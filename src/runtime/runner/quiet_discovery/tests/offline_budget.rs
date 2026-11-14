use super::*;

mod hint_window;

struct OfflineFixture {
    f: Fixture,
    _server: P2pNode,
    readiness: RelayReadiness,
    epochs: ConnectionEpochs,
    connections: HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    paths: PathSet,
    peers: PeerCapabilities,
    queues: PeerQueues,
    in_flight: PacketInFlight,
    members: Vec<Libp2pPeerId>,
}

impl OfflineFixture {
    async fn new(count: usize) -> Self {
        Self::new_at(count, Instant::now()).await
    }

    async fn new_at(count: usize, now: Instant) -> Self {
        let mut f = Fixture::new();
        f.now = now;
        let mut config = f.forwarder.config().clone();
        let mut members = vec![f.peer];
        let mut occupied = HashSet::from([
            crate::route::builtin_ipv4(config.local_peer_id().unwrap()),
            crate::route::builtin_ipv4(PeerId::from_libp2p(f.peer)),
        ]);
        while members.len() < count {
            let peer = Libp2pPeerId::random();
            if !occupied.insert(crate::route::builtin_ipv4(PeerId::from_libp2p(peer))) {
                continue;
            }
            members.push(peer);
            config.peers.push(crate::config::PeerConfig {
                id: peer.to_string(),
                name: None,
                ip: None,
                vpn_ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            });
        }
        f.forwarder = Forwarder::from_config(&config).unwrap();
        f.node.discovery.kademlia = true;
        let server_identity = std::iter::repeat_with(|| NodeIdentity::generate_ed25519().unwrap())
            .find(|identity| {
                !occupied.contains(&crate::route::builtin_ipv4(PeerId::from_libp2p(
                    identity.peer_id.parse().unwrap(),
                )))
            })
            .unwrap();
        let mut server = membership_sync_test_node(server_identity);
        server.swarm.behaviour_mut().relay_server = Some(libp2p::relay::Behaviour::new(
            server.local_peer_id,
            libp2p::relay::Config::default(),
        ))
        .into();
        server
            .swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        let address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = server.swarm.select_next_some().await
            {
                server.swarm.add_external_address(address.clone());
                break address.with(Protocol::P2p(server.local_peer_id));
            }
        };
        let listener = f
            .node
            .swarm
            .listen_on(address.with(Protocol::P2pCircuit))
            .unwrap();
        f.node
            .configured_relay_reservation_listeners
            .insert(listener);
        let mut fixture = Self {
            f,
            _server: server,
            readiness: RelayReadiness::default(),
            epochs: ConnectionEpochs::default(),
            connections: HashMap::new(),
            paths: PathSet::new(),
            peers: PeerCapabilities::default(),
            queues: PeerQueues::new(4, 4096),
            in_flight: PacketInFlight::new(4),
            members,
        };
        tokio::time::timeout(Duration::from_secs(15), async {
            while !incoming_recovery::has_usable_path(
                &fixture.f.node, &AutoRelayState::default(), &fixture.readiness,
                &fixture.epochs, &fixture.connections,
            ) {
                tokio::select! {
                    _ = fixture._server.swarm.select_next_some() => {},
                    event = fixture.f.node.swarm.select_next_some() => match event {
                        SwarmEvent::ConnectionEstablished { peer_id, connection_id, endpoint, .. } => {
                            assert!(fixture.epochs.record_established(connection_id));
                            fixture.connections.insert((peer_id, connection_id), endpoint);
                        },
                        SwarmEvent::Behaviour(BehaviourEvent::Relay(
                            relay::client::Event::ReservationReqAccepted { relay_peer_id, .. }
                        )) => { fixture.readiness.record_reservation_accepted(relay_peer_id); },
                        SwarmEvent::NewListenAddr { listener_id, address } => {
                            if let Some(peer) = relayed_address_relay_peer(&address)
                                && let Some(base) = relay_base_address_from_relayed_listen_address(&address)
                            {
                                fixture.readiness.record_relay_listen_address(peer, listener_id, base);
                            }
                        },
                        _ => {},
                    },
                }
            }
        }).await.expect("native incoming reservation");
        fixture.reconcile();
        fixture
    }

    fn reconcile(&mut self) {
        reconcile_with_work(
            Context {
                node: &mut self.f.node,
                forwarder: &self.f.forwarder,
                local: &self.f.local,
                relay: &AutoRelayState::default(),
                readiness: &self.readiness,
                epochs: &self.epochs,
                connections: &self.connections,
                pairing: &self.f.pairing,
                discovered: &mut self.f.discovered,
                now: self.f.now,
            },
            Some(&WorkContext {
                paths: &self.paths,
                peers: &self.peers,
                udp: None,
                quic: None,
                queues: &self.queues,
                in_flight: &self.in_flight,
            }),
        );
    }

    fn suppressed(&self, peer: Libp2pPeerId) -> bool {
        offline_suppressed(
            &self.f.node.swarm,
            &self.epochs,
            &self.f.forwarder,
            &self.f.discovered,
            peer,
            self.f.now,
        )
    }

    fn dial_suppressed(&self, peer: Libp2pPeerId) -> bool {
        offline_dials_suppressed(
            &self.f.node.swarm,
            &self.epochs,
            &self.f.forwarder,
            &self.f.discovered,
            peer,
            self.f.now,
        )
    }

    fn expire_window(&mut self) {
        self.f.now += Duration::from_secs(600);
        self.reconcile();
    }

    fn public_quiet(&self) -> bool {
        public_discovery_quiet_mode_with_offline(
            &self.f.forwarder,
            &self.paths,
            &self.peers,
            None,
            None,
            |peer| {
                self.f
                    .forwarder
                    .transport_peer_for_overlay(peer)
                    .is_some_and(|peer| self.suppressed(peer))
            },
        )
    }

    fn spend(&mut self, peer: Libp2pPeerId) -> kad::QueryId {
        assert!(
            self.f
                .discovered
                .should_query_recovery_discovery_at(peer, self.f.now)
        );
        let id = self
            .f
            .node
            .swarm
            .behaviour_mut()
            .kad
            .get_closest_peers(peer);
        self.f
            .discovered
            .recovery_queries
            .record(peer, [id], self.f.now);
        id
    }

    fn finish(&mut self, id: kad::QueryId) {
        self.f.discovered.recovery_queries.finished(id, self.f.now);
        self.f.node.swarm.behaviour_mut().kad.cancel_query(&id);
    }

    fn revalidate(&mut self, event: &SwarmEvent<BehaviourEvent>) {
        revalidate_event(
            &self.f.node.swarm,
            EventContext {
                gate: &mut self.f.discovered.offline,
                local: &self.f.local,
                pairing: &self.f.pairing,
                relay_server: self.f.node.startup.relay_server_enabled,
                epochs: &self.epochs,
                connections: &self.connections,
                readiness: &self.readiness,
                relay: &AutoRelayState::default(),
                configured_listeners: &self.f.node.configured_relay_reservation_listeners,
            },
            event,
        );
    }

    fn redial(&mut self, quiet: bool) {
        handle_redial_tick(
            &mut self.f.node,
            &mut self.epochs,
            &self.f.forwarder,
            &mut KademliaMaintenance::new(self.f.now),
            &mut self.f.discovered,
            &self.paths,
            &mut self.readiness,
            &mut ConfiguredRelayReservationRetries::default(),
            &PublicDiscoveryBackoff::default(),
            quiet,
            &RuntimeMetrics::default(),
        );
    }

    fn deliver(&mut self, id: kad::QueryId, result: kad::QueryResult) {
        let mut membership = OverlayMembership::from_config(self.f.forwarder.config()).unwrap();
        let mut tun = TunRuntimeConfig::from_config(self.f.forwarder.config()).unwrap();
        handle_kademlia_event_at(
            &mut self.f.node.swarm,
            KademliaEventContext {
                connection_epochs: &mut self.epochs,
                forwarder: &mut self.f.forwarder,
                membership_record_syncs: &mut MembershipRecordSyncs::default(),
                membership: &mut membership,
                tun_runtime: &mut tun,
                route_controller: &mut PreconfiguredTunRoutes,
                infrastructure_peers: &mut InfrastructurePeers::default(),
                auto_relay: &mut AutoRelayState::default(),
                discovered_peer_addresses: &mut self.f.discovered,
                paths: &self.paths,
                local_capabilities: &mut self.f.local,
                previous_membership_tags: &[],
                metrics: &RuntimeMetrics::default(),
                discovery: &self.f.node.discovery,
                identity: &self.f.node.identity,
                code_pairing_sessions: &mut self.f.pairing,
                public_discovery_quiet: false,
                kademlia_maintenance: &mut KademliaMaintenance::new(self.f.now),
            },
            kad::Event::OutboundQueryProgressed {
                id,
                result,
                stats: kad::QueryStats::empty(),
                step: kad::ProgressStep {
                    count: std::num::NonZeroUsize::MIN,
                    last: true,
                },
            },
            self.f.now,
        );
        self.f.node.swarm.behaviour_mut().kad.cancel_query(&id);
    }
}

#[tokio::test]
async fn hint_window_spent_lookup_keeps_cached_address_recovery_open_until_deadline() {
    let mut t = OfflineFixture::new(1).await;
    let peer = t.f.peer;
    let id = t.spend(peer);
    t.finish(id);
    t.reconcile();
    assert!(
        t.f.discovered.recovery_queries.data_budget_exhausted(
            peer,
            t.f.forwarder
                .application_demand(PeerId::from_libp2p(peer))
                .unwrap()
                .version,
            t.epochs.current,
            t.f.now,
        )
    );
    assert!(
        !t.suppressed(peer),
        "lookup completion must not consume the cached-address window"
    );
    assert!(!t.public_quiet());
    assert!(!t.f.node.swarm.behaviour().kad.background_jobs_paused());
    t.f.now += Duration::from_secs(599);
    t.reconcile();
    assert!(!t.suppressed(peer));
    assert!(
        !t.f.discovered
            .should_query_recovery_discovery_at(peer, t.f.now)
    );
    t.f.now += Duration::from_secs(1);
    t.reconcile();
    assert!(t.suppressed(peer));
    assert!(t.public_quiet());
    assert!(t.f.node.swarm.behaviour().kad.background_jobs_paused());
}

#[tokio::test]
async fn hint_window_keeps_admitted_wan_hint_during_lan_grace_without_dialing() {
    let mut t = OfflineFixture::new(1).await;
    let peer = t.f.peer;
    let id = t.spend(peer);
    assert!(t.f.discovered.start_lan_first_recovery(peer, t.f.now));
    let address: Multiaddr = "/ip4/11.252.0.2/tcp/4001".parse().unwrap();
    t.deliver(
        id,
        kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk {
            key: peer.to_bytes(),
            peers: vec![kad::PeerInfo {
                peer_id: peer,
                addrs: vec![address.clone()],
            }],
        })),
    );
    assert!(
        t.f.discovered.as_vec().contains(&(peer, address)),
        "an admitted lookup's WAN hint must survive LAN grace"
    );
    assert!(t.f.discovered.recovery_dial_attempts.is_empty());
    assert_eq!(
        t.f.node
            .swarm
            .network_info()
            .connection_counters()
            .num_pending_outgoing(),
        0
    );
    assert!(!t.f.discovered.recovery_queries.has_pending());
}

#[tokio::test]
async fn four_idle_two_offline_settle_both_gates_without_negotiating_offline_dormancy() {
    let mut t = OfflineFixture::new(6).await;
    for peer in &t.members[..4] {
        let overlay = PeerId::from_libp2p(*peer);
        let version =
            t.f.forwarder
                .application_demand(overlay)
                .unwrap()
                .version
                .unwrap();
        assert!(t.f.forwarder.grant_data_path_quiet(overlay, version));
        t.paths
            .record_established(overlay, PathKind::DirectTcpStream);
    }
    t.reconcile();
    assert!(!t.public_quiet());
    assert!(!t.f.node.swarm.behaviour().kad.background_jobs_paused());
    t.f.now += Duration::from_secs(600);
    t.reconcile();
    assert!(t.public_quiet());
    assert!(t.f.node.swarm.behaviour().kad.background_jobs_paused());
    assert!(
        t.f.node
            .swarm
            .behaviour()
            .autonat
            .as_ref()
            .unwrap()
            .client_probes_paused()
    );
    let capacity = (
        t.f.discovered.offline.anchors.capacity(),
        t.f.discovered.offline.busy.capacity(),
    );
    t.reconcile();
    assert_eq!(
        capacity,
        (
            t.f.discovered.offline.anchors.capacity(),
            t.f.discovered.offline.busy.capacity()
        )
    );
    for peer in &t.members[4..] {
        assert!(t.suppressed(*peer));
        assert!(
            !t.f.forwarder
                .data_path_is_dormant(PeerId::from_libp2p(*peer))
        );
        assert!(
            !t.f.forwarder
                .data_path_maintenance_is_paused(PeerId::from_libp2p(*peer))
        );
    }
}

#[tokio::test]
async fn all_six_receive_one_native_attempt_and_admitted_data_drains_before_quiet() {
    let mut t = OfflineFixture::new(6).await;
    let mut seen = HashSet::new();
    for _ in 0..6 {
        t.redial(false);
        let owners = t
            .members
            .iter()
            .copied()
            .filter(|peer| t.f.discovered.recovery_queries.has_data_query(*peer))
            .collect::<Vec<_>>();
        assert_eq!(owners.len(), 1);
        let peer = owners[0];
        assert!(
            seen.insert(peer),
            "spent member cannot steal another member's opportunity"
        );
        let id =
            t.f.node
                .swarm
                .behaviour()
                .kad
                .iter_queries()
                .find(|query| {
                    t.f.discovered
                        .recovery_queries
                        .owns_data_query(peer, &query.id())
                })
                .unwrap()
                .id();
        t.reconcile();
        assert!(!t.suppressed(peer));
        assert!(!t.public_quiet());
        assert!(!t.f.node.swarm.behaviour().kad.background_jobs_paused());
        t.redial(true);
        assert!(t.f.node.swarm.behaviour().kad.query_is_retained(&id));
        assert!(t.f.discovered.recovery_queries.owns_data_query(peer, &id));
        t.deliver(
            id,
            kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk {
                key: peer.to_bytes(),
                peers: Vec::new(),
            })),
        );
        t.reconcile();
    }
    assert_eq!(seen.len(), 6);
    assert!(
        !t.public_quiet(),
        "cached-address recovery keeps its original window"
    );
    t.expire_window();
    assert!(t.public_quiet());
    assert!(t.f.node.swarm.behaviour().kad.background_jobs_paused());
    for elapsed in [600, 7200, 86400] {
        t.f.now += Duration::from_secs(elapsed);
        t.reconcile();
        t.redial(true);
        assert!(!t.f.discovered.recovery_queries.has_pending());
        assert!(t.members.iter().all(|peer| t.suppressed(*peer)));
    }
}

#[tokio::test]
async fn payload_and_underlay_invalidate_old_snapshot_and_preserve_lan_first() {
    let mut t = OfflineFixture::new(1).await;
    let peer = t.f.peer;
    let id = t.spend(peer);
    t.finish(id);
    t.expire_window();
    assert!(t.suppressed(peer));
    assert!(
        t.f.forwarder
            .record_application_demand(PeerId::from_libp2p(peer), t.f.now)
    );
    assert!(
        !t.suppressed(peer),
        "live demand is checked without refreshing the snapshot"
    );
    t.reconcile();
    assert!(!t.public_quiet());
    assert!(t.f.discovered.start_lan_first_recovery(peer, t.f.now));
    assert!(
        !t.f.discovered
            .should_query_recovery_discovery_at(peer, t.f.now)
    );
    let after_grace = t.f.now + PEER_RECOVERY_LAN_FIRST_GRACE;
    assert!(
        t.f.discovered
            .should_query_recovery_discovery_at(peer, after_grace)
    );
    t.f.now = after_grace;
    let id = t.spend(peer);
    t.finish(id);
    t.expire_window();
    assert!(t.suppressed(peer));
    t.epochs.advance();
    assert!(!t.suppressed(peer));
    t.reconcile();
    assert!(!t.f.node.swarm.behaviour().kad.background_jobs_paused());
    assert!(
        t.f.discovered
            .should_query_recovery_discovery_at(peer, t.f.now),
        "underlay change must not recreate a cooldown that elapsed during the old window"
    );
}

#[tokio::test]
async fn spent_budget_never_suppresses_ordinary_admission_without_all_offline_gates() {
    for scenario in [
        "anchor",
        "pairing",
        "server",
        "missing_work",
        "underlay",
        "queued",
        "in_flight",
    ] {
        let mut t = OfflineFixture::new(1).await;
        let peer = t.f.peer;
        let id = t.spend(peer);
        t.finish(id);
        t.expire_window();
        // A foreground failure can establish its own cooldown after the
        // background window ends; disabling a gate must not erase it.
        let control = t.f.node.swarm.behaviour_mut().kad.get_closest_peers(peer);
        t.f.discovered.recovery_queries.record_control(
            control,
            crate::runtime::recovery_queries::ControlQueryScope {
                peer,
                generation: 1,
                deadline: t.f.now + Duration::from_secs(10),
            },
            t.f.now,
        );
        t.finish(control);
        let after_cooldown = t.f.now + Duration::from_secs(61);
        assert!(
            !t.f.discovered
                .should_query_recovery_discovery_at(peer, after_cooldown)
        );
        let event = SwarmEvent::OutgoingConnectionError {
            peer_id: None,
            connection_id: ConnectionId::new_unchecked(999),
            error: DialError::NoAddresses,
        };
        match scenario {
            "anchor" => {
                t.readiness.reset();
                t.revalidate(&event);
            }
            "pairing" => {
                t.f.pairing
                    .open("lab", 60, current_unix_seconds_lossy(), t.f.now)
                    .unwrap();
                t.revalidate(&event);
            }
            "server" => {
                t.f.node.startup.relay_server_enabled = true;
                t.revalidate(&event);
            }
            "missing_work" => reconcile(Context {
                node: &mut t.f.node,
                forwarder: &t.f.forwarder,
                local: &t.f.local,
                relay: &AutoRelayState::default(),
                readiness: &t.readiness,
                epochs: &t.epochs,
                connections: &t.connections,
                pairing: &t.f.pairing,
                discovered: &mut t.f.discovered,
                now: t.f.now,
            }),
            "underlay" => {
                t.epochs.advance();
                // Network-change admission runs before the next full work
                // reconciliation; its old derived gate must already be invalid.
                reconcile_data_budgets(
                    &t.f.forwarder,
                    &t.f.local,
                    &t.epochs,
                    &mut t.f.discovered,
                    t.f.now,
                );
            }
            "queued" => {
                t.queues
                    .enqueue(crate::queue::Packet::new(
                        PeerId::from_libp2p(peer),
                        1,
                        vec![1],
                    ))
                    .unwrap();
                t.reconcile();
            }
            "in_flight" => {
                t.in_flight
                    .peers
                    .entry(PeerId::from_libp2p(peer))
                    .or_default()
                    .record(0, true);
                t.reconcile();
            }
            _ => unreachable!(),
        }
        assert!(!t.suppressed(peer), "{scenario}");
        assert!(
            !t.f.discovered
                .should_query_recovery_discovery_at(peer, t.f.now),
            "legacy cooldown remains intact: {scenario}"
        );
        assert!(
            t.f.discovered
                .should_query_recovery_discovery_at(peer, after_cooldown),
            "raw spent allowance cannot suppress legacy admission: {scenario}"
        );
        if scenario == "queued" {
            assert!(
                t.f.forwarder
                    .record_application_demand(PeerId::from_libp2p(peer), t.f.now)
            );
            t.reconcile();
            assert!(
                t.f.discovered
                    .should_query_recovery_discovery_at(peer, t.f.now),
                "fresh payload allowance bypasses cooldown even with its first packet queued"
            );
        }
    }
}

#[tokio::test]
async fn missing_ticket_work_supported_path_and_connected_legacy_are_conservative() {
    let mut t = OfflineFixture::new(1).await;
    let peer = t.f.peer;
    let overlay = PeerId::from_libp2p(peer);
    assert!(!t.suppressed(peer));
    let id = t.spend(peer);
    t.finish(id);
    t.expire_window();
    assert!(t.suppressed(peer));
    let version = t.f.forwarder.application_demand(overlay).unwrap().version;
    t.f.discovered
        .recovery_queries
        .reconcile_data_budget(peer, None, t.epochs.current, t.f.now);
    assert!(!t.suppressed(peer));
    t.reconcile();
    assert!(t.suppressed(peer));
    let packet = crate::queue::Packet::new(overlay, 1, vec![1]);
    t.queues.enqueue(packet).unwrap();
    t.reconcile();
    assert!(!t.suppressed(peer));
    assert!(!t.public_quiet());
    assert!(!t.f.node.swarm.behaviour().kad.background_jobs_paused());
    t.queues = PeerQueues::new(4, 4096);
    t.in_flight
        .peers
        .entry(overlay)
        .or_default()
        .record(0, true);
    t.reconcile();
    assert!(!t.suppressed(peer));
    assert!(!t.f.node.swarm.behaviour().kad.background_jobs_paused());
    t.in_flight = PacketInFlight::new(4);
    t.paths
        .record_established(overlay, PathKind::DirectTcpStream);
    t.reconcile();
    assert!(!t.suppressed(peer));
    assert!(!t.f.forwarder.data_path_is_dormant(overlay));
    assert!(!t.f.node.swarm.behaviour().kad.background_jobs_paused());
    assert_eq!(
        t.f.forwarder.application_demand(overlay).unwrap().version,
        version
    );
    t.paths = PathSet::new();
    t.reconcile();
    assert!(t.suppressed(peer));
    t.f.discovered.recovery_queries.retain_peers(|_| false);
    assert!(!t.suppressed(peer));

    // A connected legacy member remains live without a packet-path observation.
    let connected = t._server.local_peer_id;
    let mut config = t.f.forwarder.config().clone();
    config.peers.push(crate::config::PeerConfig {
        id: connected.to_string(),
        name: None,
        ip: None,
        vpn_ip: None,
        addresses: Vec::new(),
        routes: Vec::new(),
    });
    t.f.forwarder = Forwarder::from_config(&config).unwrap();
    t.reconcile();
    let id = t.spend(connected);
    t.finish(id);
    assert!(t.f.node.swarm.is_connected(&connected));
    assert!(!t.suppressed(connected));
    assert!(
        !t.f.forwarder
            .data_path_is_dormant(PeerId::from_libp2p(connected))
    );
}

#[tokio::test]
async fn stale_derived_gate_cannot_survive_listener_owner_loss_pairing_or_serving() {
    let mut t = OfflineFixture::new(1).await;
    let id = t.spend(t.f.peer);
    t.finish(id);
    t.expire_window();
    let event = SwarmEvent::OutgoingConnectionError {
        peer_id: None,
        connection_id: ConnectionId::new_unchecked(999),
        error: DialError::NoAddresses,
    };
    assert!(t.suppressed(t.f.peer));
    let listeners = t.f.node.configured_relay_reservation_listeners.clone();
    t.f.node.configured_relay_reservation_listeners.clear();
    t.revalidate(&event);
    assert!(!t.suppressed(t.f.peer));
    t.f.node.configured_relay_reservation_listeners = listeners;
    t.reconcile();
    assert!(t.suppressed(t.f.peer));
    let active = t.f.local.clone();
    t.f.local = ControlCapabilities::local("lab", None, 1280);
    t.revalidate(&event);
    assert!(!t.suppressed(t.f.peer));
    t.f.local = active;
    t.reconcile();
    let invitation =
        t.f.pairing
            .open("lab", 60, current_unix_seconds_lossy(), t.f.now)
            .unwrap();
    t.revalidate(&event);
    assert!(!t.suppressed(t.f.peer));
    t.f.pairing = CodePairingSessions::new();
    t.reconcile();
    t.f.pairing
        .join(
            "lab",
            invitation.code.parse().unwrap(),
            None,
            Vec::new(),
            60,
            current_unix_seconds_lossy(),
            t.f.now,
        )
        .unwrap();
    t.revalidate(&event);
    assert!(!t.suppressed(t.f.peer));
    t.f.pairing = CodePairingSessions::new();
    t.reconcile();
    t.f.node.startup.relay_server_enabled = true;
    t.revalidate(&event);
    assert!(!t.suppressed(t.f.peer));
    t.f.node.startup.relay_server_enabled = false;
    t.reconcile();
    let listener =
        *t.f.node
            .configured_relay_reservation_listeners
            .iter()
            .next()
            .unwrap();
    let address = t.f.node.swarm.listeners().next().unwrap().clone();
    t.revalidate(&SwarmEvent::ExpiredListenAddr {
        listener_id: listener,
        address,
    });
    assert!(!t.suppressed(t.f.peer));
    t.reconcile();
    let (_, anchor) = t.f.discovered.offline.anchors[0];
    t.epochs.mark_retiring(anchor);
    assert!(!t.suppressed(t.f.peer));
    t.reconcile();
    assert!(!t.f.node.swarm.behaviour().kad.background_jobs_paused());
}

#[tokio::test]
async fn late_offline_provider_and_configured_cached_redials_stay_suppressed_with_an_active_peer() {
    let mut t = OfflineFixture::new_at(2, Instant::now() - Duration::from_secs(600)).await;
    let peer = t.f.peer;
    let id = t.spend(peer);
    t.finish(id);
    let other = t.members[1];
    let pending = t.spend(other);
    t.expire_window();
    assert!(
        t.f.forwarder
            .record_application_demand(PeerId::from_libp2p(other), t.f.now)
    );
    t.reconcile();
    let address: Multiaddr = "/ip6/2001:4860::2/tcp/4001".parse().unwrap();
    t.f.node
        .swarm
        .behaviour_mut()
        .kad
        .add_address(&peer, address.clone());
    t.f.discovered.insert(peer, address.clone());
    t.f.node
        .configured_peer_addresses
        .push((peer, address.clone()));
    let key = crate::runtime::p2p::kademlia_rendezvous_key("lab", None);
    t.deliver(
        id,
        kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
            key,
            providers: HashSet::from([peer]),
        })),
    );
    assert!(
        t.f.discovered
            .recovery_queries
            .owns_data_query(other, &pending)
    );
    assert!(t.suppressed(peer));
    assert_eq!(
        t.f.node
            .swarm
            .network_info()
            .connection_counters()
            .num_pending_outgoing(),
        0
    );
    t.redial(false);
    assert_eq!(
        t.f.node
            .swarm
            .network_info()
            .connection_counters()
            .num_pending_outgoing(),
        1,
        "the other active member retains its relay-ready recovery dial"
    );
    assert!(
        t.f.discovered
            .recovery_dial_attempts
            .keys()
            .all(|(attempted, _)| *attempted == other)
    );
    assert!(t.f.discovered.as_vec().contains(&(peer, address)));
    assert!(
        !t.f.forwarder
            .data_path_is_dormant(PeerId::from_libp2p(peer))
    );
}

#[tokio::test]
async fn owned_results_after_dial_deadline_only_cache_and_fresh_demand_reopens_window() {
    for timed_out in [false, true] {
        let mut t = OfflineFixture::new(2).await;
        let peer = t.f.peer;
        let id = t.spend(peer);
        let address: Multiaddr = "/ip4/11.252.0.2/tcp/4001".parse().unwrap();
        let result = kad::QueryResult::GetClosestPeers(if timed_out {
            Err(kad::GetClosestPeersError::Timeout {
                key: peer.to_bytes(),
                peers: vec![kad::PeerInfo {
                    peer_id: peer,
                    addrs: vec![address.clone()],
                }],
            })
        } else {
            Ok(kad::GetClosestPeersOk {
                key: peer.to_bytes(),
                peers: vec![kad::PeerInfo {
                    peer_id: peer,
                    addrs: vec![address.clone()],
                }],
            })
        });
        t.expire_window();
        assert!(
            t.dial_suppressed(peer),
            "owned lookup cannot extend the original dial deadline"
        );
        assert!(
            !t.suppressed(peer),
            "native DATA ownership still drains before quiet scheduling"
        );
        t.deliver(id, result.clone());
        assert!(t.suppressed(peer));
        assert!(t.f.discovered.as_vec().contains(&(peer, address.clone())));
        assert!(t.f.discovered.recovery_dial_attempts.is_empty());
        assert_eq!(
            t.f.node
                .swarm
                .network_info()
                .connection_counters()
                .num_pending_outgoing(),
            0
        );
        assert!(
            t.f.forwarder
                .record_application_demand(PeerId::from_libp2p(peer), t.f.now)
        );
        t.reconcile();
        let fresh = t.spend(peer);
        assert!(!t.dial_suppressed(peer));
        t.deliver(fresh, result);
        assert!(
            !t.f.discovered
                .recovery_queries
                .owns_data_query(peer, &fresh)
        );
        assert_eq!(
            t.f.node
                .swarm
                .network_info()
                .connection_counters()
                .num_pending_outgoing(),
            1
        );
    }
}

#[tokio::test]
async fn offline_pause_preserves_scoped_control_and_essential_signed_address_publication() {
    let mut t = OfflineFixture::new(1).await;
    let peer = t.f.peer;
    let data = t.spend(peer);
    t.finish(data);
    t.expire_window();
    assert!(t.f.node.swarm.behaviour().kad.background_jobs_paused());
    let control = t.f.node.swarm.behaviour_mut().kad.get_closest_peers(peer);
    let scope = crate::runtime::recovery_queries::ControlQueryScope {
        peer,
        generation: 7,
        deadline: t.f.now + Duration::from_secs(45),
    };
    t.f.discovered
        .recovery_queries
        .record_control(control, scope, t.f.now);
    t.reconcile();
    t.redial(true);
    assert_eq!(
        t.f.discovered
            .recovery_queries
            .control_query(&control)
            .unwrap()
            .scope,
        scope
    );
    assert!(t.f.node.swarm.behaviour().kad.query_is_retained(&control));
    t.finish(control);
    let local = t.f.node.local_peer_id;
    t.f.node.swarm.behaviour_mut().kad = kad::Behaviour::with_config(
        local,
        kad::store::MemoryStore::new(local),
        crate::runtime::p2p::controlled_kademlia_config(libp2p::StreamProtocol::new(
            crate::config::PRIVATE_KADEMLIA_PROTOCOL,
        )),
    );
    t.f.node
        .swarm
        .behaviour_mut()
        .kad
        .set_mode(Some(kad::Mode::Client));
    t.f.node
        .swarm
        .add_external_address("/ip4/11.252.0.9/tcp/4001".parse().unwrap());
    t.reconcile();
    let mut maintenance = KademliaMaintenance::new(t.f.now);
    request_kademlia_address_publication(&mut maintenance, &t.f.node.discovery);
    drive_kademlia_address_publication(
        false,
        &mut t.f.node.swarm,
        &mut maintenance,
        "lab",
        None,
        &t.f.node.identity,
        t.f.now,
    );
    let (id, _) = maintenance
        .address_publication
        .expect("essential signed record starts while paused");
    assert!(t.f.node.swarm.behaviour().kad.query_is_retained(&id));
    assert!(t.f.node.swarm.behaviour().kad.background_jobs_paused());
}
