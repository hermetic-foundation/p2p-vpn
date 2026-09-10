//! Stateful production dispatch regressions, not physical network race measurements.

use super::tests::{config_with_peer, membership_sync_test_node};
use super::*;

#[test]
fn tun_worker_drop_releases_reader_and_metrics_with_a_full_channel() {
    use crate::runtime::tun::{PacketIo, PacketRead, PacketWrite};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc as sync_channel,
    };

    struct Reader {
        reads: usize,
        full: sync_channel::Sender<()>,
        dropped: Arc<AtomicBool>,
    }
    impl Drop for Reader {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    impl PacketRead for Reader {
        fn read_packet(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            if self.reads == TUN_READ_CHANNEL + 1 {
                self.full.send(()).unwrap();
            }
            buffer[0] = 1;
            Ok(1)
        }
        fn cancellation(&self) -> Option<Box<dyn FnOnce() + Send>> {
            Some(Box::new(|| {}))
        }
    }
    struct Writer;
    impl PacketWrite for Writer {
        fn write_packet(&mut self, _: &[u8]) -> std::io::Result<usize> {
            unreachable!()
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let (full, filled) = sync_channel::channel();
    let (reader, _) = PacketIo::new(
        Reader {
            reads: 0,
            full,
            dropped: Arc::clone(&dropped),
        },
        Writer,
    )
    .split();
    let metrics = Arc::new(RuntimeMetrics::default());
    let worker = spawn_tun_reader(reader, Arc::clone(&metrics), 1280);
    filled.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(worker);
    assert!(dropped.load(Ordering::Acquire));
    assert_eq!(Arc::strong_count(&metrics), 1);
}

#[tokio::test]
async fn failed_packet_connection_cannot_veto_fresh_opposite_role_connection() {
    let peer = NodeIdentity::generate_ed25519()
        .unwrap()
        .peer_id
        .parse()
        .unwrap();
    let mut fixture = EventFixture::new(peer);
    let overlay = PeerId::from_libp2p(peer);
    let old = ConnectionId::new_unchecked(30);
    let fresh = ConnectionId::new_unchecked(43);
    let local = *fixture.node.swarm.local_peer_id();
    let outbound = ConnectedPoint::Dialer {
        address: "/ip4/127.0.0.1/tcp/42001".parse().unwrap(),
        role_override: libp2p::core::Endpoint::Dialer,
        port_use: libp2p::core::transport::PortUse::New,
    };
    let inbound = ConnectedPoint::Listener {
        local_addr: "/ip4/127.0.0.1/tcp/42002".parse().unwrap(),
        send_back_addr: "/ip4/127.0.0.1/tcp/42003".parse().unwrap(),
    };
    let (preferred, opposite) = if local.to_bytes() < peer.to_bytes() {
        (outbound, inbound)
    } else {
        (inbound, outbound)
    };
    fixture.epochs.record_established(old);
    fixture.active_connections.insert((peer, old), preferred);
    fixture.paths.record_established_with_details(
        overlay,
        PathKind::DirectTcpStream,
        None,
        Some(1280),
        PathOrigin::Identify,
        PathConnectionRole::Unknown,
        false,
        Some(old),
        None,
    );
    let path = fixture.paths.best_for(overlay).unwrap();
    let id = fixture
        .node
        .swarm
        .behaviour_mut()
        .pinned_packet_stream
        .send_request_on_connection(peer, old, Frame::packet(1, 1, vec![0x45; 20]).unwrap());
    fixture.packet_in_flight.record_path_probe(
        overlay,
        PacketInFlightId::PinnedPacketStream(id),
        path,
    );
    fixture
        .dispatch(SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
            pinned_packet_stream::Event::OutboundFailure {
                peer,
                connection_id: old,
                request_id: id,
                error: pinned_packet_stream::Failure::StreamUpgrade("Timeout".to_owned()),
            },
        )))
        .await;
    assert!(fixture.paths.best_for(overlay).is_none());
    assert_eq!(fixture.packet_in_flight.stats().packets, 0);
    fixture.epochs.record_established(fresh);
    fixture.active_connections.insert((peer, fresh), opposite);
    let redundant =
        redundant_direct_connection_ids(local, peer, &fixture.active_connections, &fixture.epochs);
    assert!(
        !redundant.contains(&fresh),
        "failed preferred connection vetoed fresh replacement"
    );
    assert!(
        !fixture.epochs.is_usable(old),
        "failed connection must be retired until its close event"
    );
    assert!(fixture.epochs.is_usable(fresh));
}

struct UnusedPacketIo;

#[tokio::test]
async fn packet_failure_retirement_is_owned_current_and_preserves_replacements() {
    for scenario in [
        "io",
        "capacity",
        "unowned",
        "stale_epoch",
        "untracked",
        "wrong_peer",
        "wrong_path",
        "non_transport",
        "already_retiring",
    ] {
        let peer = NodeIdentity::generate_ed25519()
            .unwrap()
            .peer_id
            .parse()
            .unwrap();
        let mut fixture = EventFixture::new(peer);
        let overlay = PeerId::from_libp2p(peer);
        let old = ConnectionId::new_unchecked(7);
        let replacement = ConnectionId::new_unchecked(8);
        let endpoint = ConnectedPoint::Dialer {
            address: if scenario == "wrong_path" {
                "/ip4/127.0.0.1/udp/42001/quic-v1"
            } else {
                "/ip4/127.0.0.1/tcp/42001"
            }
            .parse()
            .unwrap(),
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::New,
        };
        fixture.epochs.record_established(old);
        if scenario != "untracked" {
            fixture.active_connections.insert((peer, old), endpoint);
        }
        if scenario == "stale_epoch" {
            fixture.epochs.advance();
        }
        if scenario == "already_retiring" {
            fixture.epochs.mark_retiring(old);
        }
        fixture.epochs.record_started(replacement);
        fixture.epochs.record_established(replacement);
        fixture.active_connections.insert(
            (peer, replacement),
            ConnectedPoint::Dialer {
                address: "/ip4/127.0.0.1/tcp/42002".parse().unwrap(),
                role_override: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::New,
            },
        );
        for id in [old, replacement] {
            fixture.paths.record_established_with_details(
                overlay,
                PathKind::DirectTcpStream,
                None,
                Some(1280),
                PathOrigin::Identify,
                PathConnectionRole::Dialer,
                false,
                Some(id),
                None,
            );
        }
        let path = fixture.paths.best_for(overlay).unwrap();
        let id = fixture
            .node
            .swarm
            .behaviour_mut()
            .pinned_packet_stream
            .send_request_on_connection(peer, old, Frame::packet(1, 1, vec![0x45; 20]).unwrap());
        if scenario != "unowned" {
            fixture.packet_in_flight.record_path_probe(
                overlay,
                PacketInFlightId::PinnedPacketStream(id),
                path,
            );
        }
        let error = match scenario {
            "capacity" => pinned_packet_stream::Failure::capacity_exhausted(),
            "non_transport" => pinned_packet_stream::Failure::MissingInboundRequest(id),
            _ => pinned_packet_stream::Failure::Io("connection reset".to_owned()),
        };
        let event_peer = if scenario == "wrong_peer" {
            NodeIdentity::generate_ed25519()
                .unwrap()
                .peer_id
                .parse()
                .unwrap()
        } else {
            peer
        };
        for _ in 0..2 {
            fixture
                .dispatch(SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
                    pinned_packet_stream::Event::OutboundFailure {
                        peer: event_peer,
                        connection_id: old,
                        request_id: id,
                        error: error.clone(),
                    },
                )))
                .await;
            assert_eq!(
                fixture.epochs.retiring.contains(&old),
                matches!(scenario, "io" | "already_retiring"),
                "{scenario}"
            );
            assert!(fixture.epochs.is_usable(replacement), "{scenario}");
            assert_eq!(
                fixture
                    .paths
                    .best_for(overlay)
                    .unwrap()
                    .latest_connection_id,
                Some(replacement),
                "{scenario}"
            );
            assert_eq!(fixture.packet_in_flight.stats().packets, 0, "{scenario}");
        }
    }
}

impl crate::runtime::tun::PacketRead for UnusedPacketIo {
    fn read_packet(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("event dispatch must not read packets");
    }
}

impl crate::runtime::tun::PacketWrite for UnusedPacketIo {
    fn write_packet(&mut self, _: &[u8]) -> io::Result<usize> {
        panic!("event dispatch must not write packets");
    }
}

struct EventFixture {
    node: P2pNode,
    forwarder: Forwarder,
    membership: OverlayMembership,
    tun: TunRuntimeConfig,
    paths: PathSet,
    capabilities: PeerCapabilities,
    readiness: RelayReadiness,
    auto_relay: AutoRelayState,
    backoff: PublicDiscoveryBackoff,
    discovered: DiscoveredPeerAddresses,
    metrics: RuntimeMetrics,
    epochs: ConnectionEpochs,
    active_connections: HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    maintenance: KademliaMaintenance,
    configured_listeners: HashSet<ListenerId>,
    retired_listeners: HashSet<ListenerId>,
    pairing: CodePairingSessions,
    pairing_tokens: PairingReplayTokens,
    packet_in_flight: PacketInFlight,
}

impl EventFixture {
    fn new(peer: Libp2pPeerId) -> Self {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let config = config_with_peer(&identity, peer);
        Self {
            node: membership_sync_test_node(identity),
            forwarder: Forwarder::from_config(&config).unwrap(),
            membership: OverlayMembership::from_config(&config).unwrap(),
            tun: TunRuntimeConfig::from_config(&config).unwrap(),
            paths: PathSet::new(),
            capabilities: PeerCapabilities::default(),
            readiness: RelayReadiness::default(),
            auto_relay: AutoRelayState::default(),
            backoff: PublicDiscoveryBackoff::from_bootstrap_defaults(true),
            discovered: DiscoveredPeerAddresses::default(),
            metrics: RuntimeMetrics::default(),
            epochs: ConnectionEpochs::default(),
            active_connections: HashMap::new(),
            maintenance: KademliaMaintenance::new(Instant::now()),
            configured_listeners: HashSet::new(),
            retired_listeners: HashSet::new(),
            pairing: CodePairingSessions::new(),
            pairing_tokens: PairingReplayTokens::default(),
            packet_in_flight: PacketInFlight::new(1),
        }
    }

    async fn dispatch(&mut self, event: SwarmEvent<BehaviourEvent>) {
        self.dispatch_with_routes(event, &mut PreconfiguredTunRoutes)
            .await
            .unwrap();
    }

    async fn dispatch_with_routes(
        &mut self,
        event: SwarmEvent<BehaviourEvent>,
        routes: &mut dyn TunRouteController,
    ) -> Result<(), RunnerError> {
        let (_, mut writer) = PacketIo::new(UnusedPacketIo, UnusedPacketIo).split();
        handle_swarm_event(
            &mut self.node.swarm,
            SwarmEventContext {
                forwarder: &mut self.forwarder,
                membership: &mut self.membership,
                tun_runtime: &mut self.tun,
                route_controller: routes,
                infrastructure_peers: &mut InfrastructurePeers::default(),
                routing_infrastructure_peers: &mut RoutingInfrastructurePeers::default(),
                writer: &mut writer,
                paths: &mut self.paths,
                peer_capabilities: &mut self.capabilities,
                relay_readiness: &mut self.readiness,
                auto_relay: &mut self.auto_relay,
                public_discovery_backoff: &mut self.backoff,
                public_discovery_holdoff_active: false,
                relay_addresses: &[],
                configured_peer_addresses: &[],
                configured_relay_reservation_listeners: &mut self.configured_listeners,
                retiring_configured_relay_reservation_listeners: &mut self.retired_listeners,
                relay_server_enabled: false,
                discovered_peer_addresses: &mut self.discovered,
                packet_in_flight: &mut self.packet_in_flight,
                inbound_packet_rate_limiters: &mut PeerRateLimiters::new(1),
                pairing_request_rate_limiters: &mut PeerRateLimiters::new(1),
                membership_page_rate_limiters: &mut PeerRateLimiters::new(1),
                membership_record_syncs: &mut MembershipRecordSyncs::default(),
                pairing_handshake_rate_limiter: &mut GlobalRateLimiter::new(1, Instant::now()),
                metrics: &self.metrics,
                local_capabilities: &mut ControlCapabilities::local("lab", None, 1280),
                persistent_packet_endpoint_candidates: &[],
                persistent_packet_plane_quic_endpoint_candidates: &[],
                previous_membership_tags: &[],
                discovery: &self.node.discovery,
                identity: &self.node.identity,
                packet_plane: &mut PacketPlaneRuntime::disabled(),
                packet_plane_quic: None,
                packet_plane_negotiator: &mut PacketPlaneNegotiator::default(),
                path_probe_tracker: &mut PathProbeTracker::default(),
                packet_plane_session_ttl: Duration::from_secs(60),
                packet_plane_replay_windows_per_session: 1,
                pairing_replay_tokens: &mut self.pairing_tokens,
                code_pairing_sessions: &mut self.pairing,
                pairing_state_store: None,
                active_connections: &mut self.active_connections,
                connection_epochs: &mut self.epochs,
                membership_probe_connections: &mut MembershipProbeConnections::default(),
                kademlia_maintenance: &mut self.maintenance,
            },
            event,
        )
        .await
    }

    fn start_auto_listener(&mut self, relay: Libp2pPeerId, address: &Multiaddr) -> ListenerId {
        self.auto_relay.record_candidate(relay, address.clone());
        assert_eq!(
            self.auto_relay.next_reservation_targets(Instant::now()),
            vec![(relay, address.clone())]
        );
        let listener = ListenerId::next();
        assert!(self.auto_relay.record_reservation_listener(relay, listener));
        self.auto_relay.record_reservation_accepted(relay);
        self.readiness.record_reservation_accepted(relay);
        listener
    }
}

fn relay_address(relay: Libp2pPeerId) -> Multiaddr {
    format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
        .parse()
        .unwrap()
}

#[tokio::test]
async fn file_pairing_response_waits_for_route_commit_and_retry_succeeds() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut remote = membership_sync_test_node(NodeIdentity::generate_ed25519().unwrap());
        let peer = *remote.swarm.local_peer_id();
        let mut fixture = EventFixture::new(peer);
        let mut config = fixture.forwarder.config().clone();
        config.peers.clear();
        fixture.forwarder = Forwarder::from_config(&config).unwrap();
        fixture.membership = OverlayMembership::from_config(&config).unwrap();
        fixture.tun = TunRuntimeConfig::from_config(&config).unwrap();
        let unix = current_unix_seconds_lossy();
        let offer = crate::pairing::export_pairing_offer_at(
            &config, crate::pairing::PairingOfferOptions::default(), unix,
        ).unwrap();
        let request = crate::pairing::build_pairing_request_at(
            &offer, PairingRequestOptions {
                identity: remote.identity.clone(),
                requested_vpn_ip: Some("10.42.0.2".to_owned()),
                requested_routes: Vec::new(),
            }, unix,
        ).unwrap();
        fixture.node.swarm.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
        let address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = fixture.node.swarm.select_next_some().await {
                break address;
            }
        };
        remote.swarm.dial(address).unwrap();
        loop {
            tokio::select! {
                event = fixture.node.swarm.select_next_some() => {
                    if let SwarmEvent::ConnectionEstablished { connection_id, .. } = event {
                        assert!(fixture.epochs.record_established(connection_id));
                        break;
                    }
                }
                _ = remote.swarm.select_next_some() => {}
            }
        }
        struct Routes { fail: bool, calls: usize }
        impl TunRouteController for Routes {
            fn reconcile(&mut self, _: &TunRuntimeConfig, _: &TunRuntimeConfig,
                _: &TunRouteUpdate) -> Result<(), RunnerError> {
                self.calls += 1;
                if self.fail { Err(io::Error::other("injected route failure").into()) }
                else { Ok(()) }
            }
        }
        let mut routes = Routes { fail: true, calls: 0 };
        for fail in [true, false] {
            routes.fail = fail;
            let id = remote.swarm.behaviour_mut().pairing.send_request(
                fixture.node.swarm.local_peer_id(), request.clone(),
            );
            let mut handled = false;
            loop {
                tokio::select! {
                    event = fixture.node.swarm.select_next_some() => {
                        if matches!(&event, SwarmEvent::Behaviour(BehaviourEvent::Pairing(
                            request_response::Event::Message { message: Message::Request { .. }, .. }
                        ))) {
                            assert!(!handled);
                            let result = fixture.dispatch_with_routes(event, &mut routes).await;
                            assert_eq!(result.is_err(), fail);
                            assert_eq!(fixture.forwarder.is_configured_transport_peer(peer), !fail);
                            assert_eq!(fixture.pairing_tokens.file_bearer.contains(
                                &request.payload.rendezvous_token), !fail);
                            handled = true;
                        }
                    }
                    event = remote.swarm.select_next_some() => {
                        match event {
                            SwarmEvent::Behaviour(BehaviourEvent::Pairing(request_response::Event::OutboundFailure {
                                request_id, ..
                            })) if request_id == id => {
                                assert!(handled && fail);
                                break;
                            }
                            SwarmEvent::Behaviour(BehaviourEvent::Pairing(request_response::Event::Message {
                                message: Message::Response { request_id, response }, ..
                            })) if request_id == id => {
                                assert!(handled && !fail, "acceptance must not escape a failed commit");
                                response.verify_for_offer_at(&offer, &remote.identity,
                                    current_unix_seconds_lossy()).unwrap();
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        assert_eq!(routes.calls, 2);
        let offer = crate::pairing::export_pairing_offer_at(
            &config, crate::pairing::PairingOfferOptions::default(), unix,
        ).unwrap();
        let request = crate::pairing::build_pairing_request_at(
            &offer, PairingRequestOptions {
                identity: remote.identity.clone(),
                requested_vpn_ip: Some("10.42.0.2".to_owned()),
                requested_routes: Vec::new(),
            }, unix,
        ).unwrap();
        remote.swarm.behaviour_mut().pairing.send_request(
            fixture.node.swarm.local_peer_id(), request.clone(),
        );
        let held = loop {
            tokio::select! {
                event = fixture.node.swarm.select_next_some() => {
                    if matches!(&event, SwarmEvent::Behaviour(BehaviourEvent::Pairing(
                        request_response::Event::Message { message: Message::Request { .. }, .. }
                    ))) { break event; }
                }
                _ = remote.swarm.select_next_some() => {}
            }
        };
        remote.swarm.disconnect_peer_id(*fixture.node.swarm.local_peer_id()).unwrap();
        loop {
            let SwarmEvent::Behaviour(BehaviourEvent::Pairing(request_response::Event::Message {
                message: Message::Request { channel, .. }, ..
            })) = &held else { unreachable!() };
            if !channel.is_open() { break; }
            tokio::select! {
                _ = fixture.node.swarm.select_next_some() => {}
                _ = remote.swarm.select_next_some() => {}
            }
        }
        let before_records = fixture.forwarder.member_records().to_vec();
        let before_membership = fixture.membership.clone();
        let before_tun = fixture.tun.clone();
        // Keep the admitted epoch to exercise the closed channel, not stale-event filtering.
        fixture.dispatch_with_routes(held, &mut routes).await.unwrap();
        assert_eq!(routes.calls, 2);
        assert_eq!(fixture.forwarder.member_records(), before_records);
        assert_eq!(fixture.membership, before_membership);
        assert_eq!(fixture.tun, before_tun);
        assert!(!fixture.pairing_tokens.file_bearer.contains(&request.payload.rendezvous_token));
    }).await.expect("bounded file-pairing commit and retry");
}

#[tokio::test]
async fn pairing_hello_terminal_stale_reply_releases_retry_ownership() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut remote = membership_sync_test_node(NodeIdentity::generate_ed25519().unwrap());
        let peer = *remote.swarm.local_peer_id();
        let mut fixture = EventFixture::new(peer);
        remote.swarm.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
        let address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = remote.swarm.select_next_some().await {
                break address;
            }
        };
        for _ in 0..2 {
            fixture.node.swarm.dial(DialOpts::peer_id(peer).condition(PeerCondition::Always)
                .addresses(vec![address.clone()]).build()).unwrap();
            loop {
                tokio::select! {
                    event = fixture.node.swarm.select_next_some() => {
                        if let SwarmEvent::ConnectionEstablished { connection_id, .. } = event {
                            assert!(fixture.epochs.record_established(connection_id));
                            break;
                        }
                    }
                    _ = remote.swarm.select_next_some() => {}
                }
            }
        }
        let now = Instant::now();
        fixture.pairing.join("lab", crate::pairing_code::PairingCode::generate(), None,
            vec![], 600, current_unix_seconds_lossy(), now).unwrap();
        send_pairing_code_hello(&mut fixture.node.swarm, &mut fixture.pairing,
            &fixture.node.identity, "lab", peer, PairingDiscoveryStage::Lan,
            &fixture.metrics, now);
        let response_event = loop {
            tokio::select! {
                event = fixture.node.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::PairingCode(event @ request_response::Event::Message {
                        message: Message::Response { .. }, ..
                    })) = event { break event; }
                }
                event = remote.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::PairingCode(request_response::Event::Message {
                        message: Message::Request { request, channel, .. }, ..
                    })) = event {
                        assert!(matches!(request, PairingCodeRequest::Hello { .. }));
                        remote.swarm.behaviour_mut().pairing_code.send_response(channel,
                            PairingCodeResponse::Rejected { reason: PairingCodeRejectionReason::Unavailable }).unwrap();
                    }
                }
            }
        };
        let request_response::Event::Message { connection_id, message: Message::Response { request_id, .. }, .. } = &response_event
            else { panic!("expected response"); };
        assert!(!fixture.node.swarm.behaviour().pairing_code.is_pending_outbound(&peer, request_id));
        let (old_connection, old_request) = (*connection_id, *request_id);
        assert!(fixture.epochs.mark_retiring(old_connection));
        assert!(fixture.node.swarm.close_connection(old_connection));
        fixture.dispatch(SwarmEvent::Behaviour(BehaviourEvent::PairingCode(response_event))).await;
        assert!(fixture.pairing.mark_peer_attempted(peer, Instant::now()).is_none(), "retry must retain backoff");
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        let retried = loop {
            tokio::select! {
                _ = tick.tick() => send_pairing_code_hello(&mut fixture.node.swarm, &mut fixture.pairing,
                    &fixture.node.identity, "lab", peer, PairingDiscoveryStage::Lan,
                    &fixture.metrics, Instant::now()),
                event = fixture.node.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::PairingCode(event @ request_response::Event::Message {
                        message: Message::Response { .. }, ..
                    })) = event { break event; }
                }
                event = remote.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::PairingCode(request_response::Event::Message {
                        message: Message::Request { request, channel, .. }, ..
                    })) = event {
                        assert!(matches!(request, PairingCodeRequest::Hello { .. }));
                        remote.swarm.behaviour_mut().pairing_code.send_response(channel,
                            PairingCodeResponse::Rejected { reason: PairingCodeRejectionReason::Unavailable }).unwrap();
                    }
                }
            }
        };
        let request_response::Event::Message { connection_id, message: Message::Response { request_id, .. }, .. } = &retried
            else { panic!("expected retry response"); };
        assert_ne!(*connection_id, old_connection);
        assert_ne!(*request_id, old_request);
        let new_request = *request_id;
        for (sender, request_id) in [(peer, old_request), (Libp2pPeerId::random(), new_request)] {
            fixture.dispatch(SwarmEvent::Behaviour(BehaviourEvent::PairingCode(request_response::Event::Message {
                peer: sender, connection_id: old_connection,
                message: Message::Response { request_id,
                    response: PairingCodeResponse::Rejected { reason: PairingCodeRejectionReason::Unavailable } },
            }))).await;
            assert!(fixture.pairing.mark_peer_attempted(peer, Instant::now() + Duration::from_secs(60)).is_none(),
                "unrelated stale event released the replacement attempt");
        }
        fixture.dispatch(SwarmEvent::Behaviour(BehaviourEvent::PairingCode(retried))).await;
        assert!(fixture.pairing.take_outbound_request(new_request).is_none());
        assert!(fixture.pairing.mark_peer_attempted(peer, Instant::now()).is_none());
        assert!(fixture.pairing.mark_peer_attempted(peer, Instant::now() + Duration::from_secs(60)).is_some(),
            "retry response must release the replacement attempt");
    }).await.expect("loopback pairing response deadline");
}

fn pending_pairing_request(
    fixture: &mut EventFixture,
    poll: bool,
    offer: &PairingOffer,
    request: &PairingRequest,
) -> (String, request_response::OutboundRequestId) {
    let peer = offer.payload.inviter_peer.parse().unwrap();
    let now = Instant::now();
    let operation = fixture
        .pairing
        .join(
            "lab",
            crate::pairing_code::PairingCode::generate(),
            None,
            vec![],
            600,
            current_unix_seconds_lossy(),
            now,
        )
        .unwrap()
        .operation_id;
    let transcript = pairing_request_transcript_sha256(request).unwrap();
    let outbound = OutboundPairing {
        operation_id: operation.clone(),
        peer,
        offer: offer.clone(),
        transcript_sha256: transcript.clone(),
    };
    let id = if poll {
        let ticket = URL_SAFE_NO_PAD.encode([0xab; 16]);
        fixture
            .pairing
            .set_remote_pending(
                &operation,
                peer,
                offer.clone(),
                transcript,
                ticket.clone(),
                now,
            )
            .unwrap();
        assert!(
            fixture
                .pairing
                .due_remote_poll(now + Duration::from_secs(2))
                .is_some()
        );
        let id = fixture
            .node
            .swarm
            .behaviour_mut()
            .pairing_code
            .send_request(&peer, PairingCodeRequest::Poll { ticket });
        fixture.pairing.insert_outbound_poll(id, outbound).unwrap();
        id
    } else {
        fixture
            .pairing
            .set_pending_submission(
                &operation,
                peer,
                request.clone(),
                offer.clone(),
                transcript,
                now,
            )
            .unwrap();
        assert!(fixture.pairing.due_pending_submission(now).is_some());
        let id = fixture
            .node
            .swarm
            .behaviour_mut()
            .pairing_code
            .send_request(
                &peer,
                PairingCodeRequest::Submit {
                    request: Box::new(request.clone()),
                },
            );
        fixture
            .pairing
            .insert_outbound_submit(id, outbound)
            .unwrap();
        id
    };
    (operation, id)
}

fn pairing_retry_due(fixture: &mut EventFixture, poll: bool, now: Instant) -> bool {
    if poll {
        fixture.pairing.due_remote_poll(now).is_some()
    } else {
        fixture.pairing.due_pending_submission(now).is_some()
    }
}

#[tokio::test]
async fn pairing_submit_poll_stale_cleanup_preserves_replacement_and_backoff() {
    for poll in [false, true] {
        let (_, inviter, _, offer, request, _) = super::tests::code_pairing_runtime_fixture();
        let peer = inviter.peer_id.parse().unwrap();
        let mut fixture = EventFixture::new(peer);
        let connection = ConnectionId::new_unchecked(45);
        assert!(fixture.epochs.record_established(connection));
        assert!(fixture.epochs.mark_retiring(connection));
        let (old_operation, old_id) = pending_pairing_request(&mut fixture, poll, &offer, &request);
        let reply = |peer, request_id| {
            SwarmEvent::Behaviour(BehaviourEvent::PairingCode(
                request_response::Event::Message {
                    peer,
                    connection_id: connection,
                    message: Message::Response {
                        request_id,
                        response: PairingCodeResponse::Rejected {
                            reason: PairingCodeRejectionReason::UserRejected,
                        },
                    },
                },
            ))
        };
        fixture.dispatch(reply(peer, old_id)).await;
        assert!(fixture.pairing.take_outbound_request(old_id).is_none());
        assert!(!pairing_retry_due(&mut fixture, poll, Instant::now()));
        assert!(pairing_retry_due(
            &mut fixture,
            poll,
            Instant::now() + Duration::from_secs(60)
        ));
        fixture.pairing.cancel(&old_operation).unwrap();
        let (new_operation, new_id) = pending_pairing_request(&mut fixture, poll, &offer, &request);
        assert_ne!(new_operation, old_operation);
        assert_ne!(new_id, old_id);
        for (sender, id) in [(peer, old_id), (Libp2pPeerId::random(), new_id)] {
            fixture.dispatch(reply(sender, id)).await;
            assert!(
                !pairing_retry_due(&mut fixture, poll, Instant::now() + Duration::from_secs(60)),
                "old/wrong-peer reply released replacement retry ownership"
            );
        }
        fixture.dispatch(reply(peer, new_id)).await;
        assert!(fixture.pairing.take_outbound_request(new_id).is_none());
        assert!(!pairing_retry_due(&mut fixture, poll, Instant::now()));
        assert!(pairing_retry_due(
            &mut fixture,
            poll,
            Instant::now() + Duration::from_secs(60)
        ));
        assert!(fixture.pairing.join_completion(&new_operation).is_none());
        assert_eq!(fixture.pairing.enrollments().len(), 0);
    }
}

#[tokio::test]
async fn old_relay_listener_close_preserves_replacement_and_current_close_allows_retry() {
    for expiration in [false, true] {
        let relay = Libp2pPeerId::random();
        let address = relay_address(relay);
        let relayed = address.clone().with(Protocol::P2pCircuit);
        let mut fixture = EventFixture::new(Libp2pPeerId::random());
        let old = fixture.start_auto_listener(relay, &address);
        assert_eq!(fixture.auto_relay.reset_for_network_change(), vec![old]);
        fixture.readiness.reset();
        let current = fixture.start_auto_listener(relay, &address);
        fixture
            .dispatch(SwarmEvent::NewListenAddr {
                listener_id: current,
                address: relayed.clone(),
            })
            .await;
        assert!(fixture.readiness.relay_ready(relay));
        fixture
            .dispatch(if expiration {
                SwarmEvent::ExpiredListenAddr {
                    listener_id: old,
                    address: relayed.clone(),
                }
            } else {
                SwarmEvent::ListenerClosed {
                    listener_id: old,
                    addresses: vec![relayed.clone()],
                    reason: Ok(()),
                }
            })
            .await;
        assert!(
            fixture.readiness.relay_ready(relay),
            "old listener removed replacement readiness"
        );
        assert_eq!(
            fixture.auto_relay.reservation_listeners.get(&relay),
            Some(&current)
        );
        assert!(!fixture.auto_relay.retry_after.contains_key(&relay));
        fixture
            .dispatch(SwarmEvent::ListenerClosed {
                listener_id: current,
                addresses: vec![relayed],
                reason: Ok(()),
            })
            .await;
        assert!(!fixture.readiness.relay_ready(relay));
        assert!(
            !fixture
                .auto_relay
                .reservation_listeners
                .contains_key(&relay)
        );
        let retry = fixture.auto_relay.retry_after[&relay];
        assert!(
            fixture
                .auto_relay
                .next_reservation_targets(retry - Duration::from_nanos(1))
                .is_empty()
        );
        assert_eq!(
            fixture.auto_relay.next_reservation_targets(retry),
            vec![(relay, address)]
        );
    }
}

#[tokio::test]
async fn unowned_relay_listener_cannot_restore_addresses_after_reset() {
    let relay = Libp2pPeerId::random();
    let address = relay_address(relay);
    let mut fixture = EventFixture::new(Libp2pPeerId::random());
    let old = fixture.start_auto_listener(relay, &address);
    fixture.auto_relay.reset_for_network_change();
    fixture.readiness.reset();
    fixture
        .dispatch(SwarmEvent::NewListenAddr {
            listener_id: old,
            address: address.with(Protocol::P2pCircuit),
        })
        .await;
    assert!(
        !fixture
            .readiness
            .relayed_listen_addresses
            .contains_key(&relay)
    );
    assert!(fixture.auto_relay.reservation_listeners.is_empty());
}

#[tokio::test]
async fn overlapping_configured_relay_listeners_keep_readiness_until_last_close() {
    let relay = Libp2pPeerId::random();
    let relayed = relay_address(relay).with(Protocol::P2pCircuit);
    let mut fixture = EventFixture::new(Libp2pPeerId::random());
    fixture.readiness.record_reservation_accepted(relay);
    let old = ListenerId::next();
    let current = ListenerId::next();
    for listener in [old, current] {
        fixture.configured_listeners.insert(listener);
        fixture
            .dispatch(SwarmEvent::NewListenAddr {
                listener_id: listener,
                address: relayed.clone(),
            })
            .await;
    }
    fixture
        .dispatch(SwarmEvent::ListenerClosed {
            listener_id: old,
            addresses: vec![relayed.clone()],
            reason: Ok(()),
        })
        .await;
    assert!(fixture.readiness.relay_ready(relay));
    assert!(fixture.configured_listeners.contains(&current));
    fixture
        .dispatch(SwarmEvent::ListenerClosed {
            listener_id: current,
            addresses: vec![relayed],
            reason: Ok(()),
        })
        .await;
    assert!(!fixture.readiness.relay_ready(relay));
    assert!(fixture.configured_listeners.is_empty());
}

#[tokio::test]
async fn peer_only_relay_acceptance_cannot_consume_replacement_listener_deadline() {
    let relay = Libp2pPeerId::random();
    let address = relay_address(relay);
    let mut fixture = EventFixture::new(Libp2pPeerId::random());
    fixture.start_auto_listener(relay, &address);
    fixture.auto_relay.reset_for_network_change();
    fixture.readiness.reset();
    fixture.auto_relay.record_candidate(relay, address.clone());
    assert_eq!(
        fixture.auto_relay.next_reservation_targets(Instant::now()),
        vec![(relay, address.clone())]
    );
    let current = ListenerId::next();
    assert!(
        fixture
            .auto_relay
            .record_reservation_listener(relay, current)
    );
    let deadline = fixture.auto_relay.pending_reservations[&relay].expires_at;
    fixture
        .dispatch(SwarmEvent::Behaviour(BehaviourEvent::Relay(
            relay::client::Event::ReservationReqAccepted {
                relay_peer_id: relay,
                renewal: false,
                limit: None,
            },
        )))
        .await;
    assert!(
        fixture.auto_relay.pending_reservations.contains_key(&relay),
        "uncorrelated acceptance consumed the current deadline"
    );
    assert_eq!(
        fixture.auto_relay.pending_reservations[&relay].expires_at,
        deadline
    );
    assert!(
        !fixture
            .auto_relay
            .accepted_reservation_peers
            .contains(&relay)
    );
    fixture
        .dispatch(SwarmEvent::NewListenAddr {
            listener_id: current,
            address: address.with(Protocol::P2pCircuit),
        })
        .await;
    assert!(
        fixture
            .auto_relay
            .accepted_reservation_peers
            .contains(&relay)
    );
    assert!(!fixture.auto_relay.pending_reservations.contains_key(&relay));
    assert!(fixture.readiness.relay_ready(relay));
}

#[tokio::test]
async fn auto_listener_error_preserves_configured_listener_at_same_relay() {
    let relay = Libp2pPeerId::random();
    let address = relay_address(relay);
    let relayed = address.clone().with(Protocol::P2pCircuit);
    let mut fixture = EventFixture::new(Libp2pPeerId::random());
    let automatic = fixture.start_auto_listener(relay, &address);
    let configured = ListenerId::next();
    fixture.configured_listeners.insert(configured);
    for listener_id in [automatic, configured] {
        fixture
            .dispatch(SwarmEvent::NewListenAddr {
                listener_id,
                address: relayed.clone(),
            })
            .await;
    }
    assert_eq!(
        fixture.readiness.ready_relay_addresses(relay),
        vec![(relay, address)]
    );
    fixture
        .dispatch(SwarmEvent::ListenerError {
            listener_id: automatic,
            error: io::Error::other("controlled listener failure"),
        })
        .await;
    assert!(fixture.readiness.relay_ready(relay));
    assert!(fixture.configured_listeners.contains(&configured));
    fixture
        .dispatch(SwarmEvent::ListenerClosed {
            listener_id: automatic,
            addresses: vec![relayed],
            reason: Ok(()),
        })
        .await;
    assert!(fixture.readiness.relay_ready(relay));
}

#[tokio::test]
async fn retired_configured_listener_marker_survives_error_until_terminal_close() {
    let relay = Libp2pPeerId::random();
    let mut fixture = EventFixture::new(Libp2pPeerId::random());
    let retired = ListenerId::next();
    fixture.retired_listeners.insert(retired);
    fixture
        .dispatch(SwarmEvent::ListenerError {
            listener_id: retired,
            error: io::Error::other("controlled listener failure"),
        })
        .await;
    assert!(fixture.retired_listeners.contains(&retired));
    fixture
        .dispatch(SwarmEvent::ListenerClosed {
            listener_id: retired,
            addresses: vec![relay_address(relay).with(Protocol::P2pCircuit)],
            reason: Ok(()),
        })
        .await;
    assert!(fixture.retired_listeners.is_empty());
}

#[tokio::test]
async fn capability_retry_targets_survivor_after_libp2p_removes_retired_connection() {
    use libp2p::swarm::{FromSwarm, NetworkBehaviour, NotifyHandler, ToSwarm};
    use std::task::{Context, Poll};
    let peer = Libp2pPeerId::random();
    let mut fixture = EventFixture::new(peer);
    let old = ConnectionId::new_unchecked(101);
    let current = ConnectionId::new_unchecked(102);
    let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().unwrap();
    let endpoint = ConnectedPoint::Dialer {
        address: address.clone(),
        role_override: libp2p::core::Endpoint::Dialer,
        port_use: libp2p::core::transport::PortUse::New,
    };
    for id in [old, current] {
        fixture.epochs.record_started(id);
        assert!(fixture.epochs.record_established(id));
        fixture
            .active_connections
            .insert((peer, id), endpoint.clone());
        let _handler = fixture
            .node
            .swarm
            .behaviour_mut()
            .control
            .handle_established_outbound_connection(
                id,
                peer,
                &address,
                libp2p::core::Endpoint::Dialer,
                libp2p::core::transport::PortUse::New,
            )
            .unwrap();
    }
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    let mut old_request = None;
    // The pinned behaviour distributes consecutive IDs across the two connections.
    for _ in 0..2 {
        let id = fixture.node.swarm.behaviour_mut().control.send_request(
            &peer,
            ControlRequest::Capabilities(ControlCapabilities::local("lab", None, 1280)),
        );
        let event = fixture.node.swarm.behaviour_mut().control.poll(&mut cx);
        let Poll::Ready(ToSwarm::NotifyHandler {
            handler: NotifyHandler::One(target),
            ..
        }) = event
        else {
            panic!("expected connection-targeted capability request");
        };
        if target == old {
            old_request = Some(id);
        }
    }
    let old_request = old_request.expect("one request selected the retiring connection");
    fixture.epochs.mark_retiring(old);
    // Swarm notifies its behaviour before exposing ConnectionClosed to the application.
    fixture
        .node
        .swarm
        .behaviour_mut()
        .control
        .on_swarm_event(FromSwarm::ConnectionClosed(
            libp2p::swarm::behaviour::ConnectionClosed {
                peer_id: peer,
                connection_id: old,
                endpoint: &endpoint,
                cause: None,
                remaining_established: 1,
            },
        ));
    assert!(
        !fixture
            .node
            .swarm
            .behaviour()
            .control
            .is_pending_outbound(&peer, &old_request)
    );
    fixture
        .dispatch(SwarmEvent::ConnectionClosed {
            peer_id: peer,
            connection_id: old,
            endpoint,
            num_established: 1,
            cause: None,
        })
        .await;
    let Poll::Ready(ToSwarm::GenerateEvent(request_response::Event::OutboundFailure {
        request_id,
        connection_id,
        error: request_response::OutboundFailure::ConnectionClosed,
        ..
    })) = fixture.node.swarm.behaviour_mut().control.poll(&mut cx)
    else {
        panic!("retirement must fail the request on the removed connection");
    };
    assert_eq!(request_id, old_request);
    assert_eq!(connection_id, old);
    assert!(
        matches!(fixture.node.swarm.behaviour_mut().control.poll(&mut cx),
        Poll::Ready(ToSwarm::NotifyHandler { peer_id, handler: NotifyHandler::One(id), .. })
        if peer_id == peer && id == current),
        "retry must target the surviving connection"
    );
    assert!(
        fixture
            .node
            .swarm
            .behaviour_mut()
            .control
            .poll(&mut cx)
            .is_pending()
    );
}

#[tokio::test]
async fn retired_connection_close_retries_unvalidated_capabilities_once() {
    for scenario in [
        "retry",
        "validated",
        "stale",
        "not_retiring",
        "no_replacement",
        "replacement_retiring",
        "unconfigured",
    ] {
        let peer = Libp2pPeerId::random();
        let overlay = PeerId::from_libp2p(peer);
        let mut fixture = EventFixture::new(if scenario == "unconfigured" {
            Libp2pPeerId::random()
        } else {
            peer
        });
        let old = ConnectionId::new_unchecked(91);
        let current = ConnectionId::new_unchecked(92);
        let endpoint = ConnectedPoint::Dialer {
            address: "/ip4/127.0.0.1/tcp/4001".parse().unwrap(),
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::New,
        };
        for id in [old, current] {
            if id == current && scenario == "no_replacement" {
                continue;
            }
            if id == current && scenario == "stale" {
                fixture.epochs.advance();
            }
            fixture.epochs.record_started(id);
            assert!(fixture.epochs.record_established(id));
            fixture
                .active_connections
                .insert((peer, id), endpoint.clone());
            fixture.paths.record_established_with_details(
                overlay,
                PathKind::DirectTcpStream,
                None,
                Some(1280),
                PathOrigin::Identify,
                PathConnectionRole::Dialer,
                false,
                Some(id),
                Some(1),
            );
        }
        if scenario != "not_retiring" {
            fixture.epochs.mark_retiring(old);
        }
        if scenario == "replacement_retiring" {
            fixture.epochs.mark_retiring(current);
        }
        if scenario == "validated" {
            fixture
                .capabilities
                .record(overlay, ControlCapabilities::local("lab", None, 1280));
        }
        let close = || SwarmEvent::ConnectionClosed {
            peer_id: peer,
            connection_id: old,
            endpoint: endpoint.clone(),
            num_established: u32::from(scenario != "no_replacement"),
            cause: None,
        };
        fixture.dispatch(close()).await;
        let sent = fixture
            .metrics
            .snapshot(crate::queue::QueueStats::default())
            .control_requests_sent;
        assert_eq!(sent, u64::from(scenario == "retry"), "{scenario}");
        assert!(!fixture.epochs.connections.contains_key(&old));
        assert!(!fixture.active_connections.contains_key(&(peer, old)));
        fixture.dispatch(close()).await;
        assert_eq!(
            fixture
                .metrics
                .snapshot(crate::queue::QueueStats::default())
                .control_requests_sent,
            sent,
            "duplicate close must not retry again: {scenario}"
        );
    }
}

#[tokio::test]
async fn old_connection_close_preserves_replacement_until_last_connection_closes() {
    let peer = Libp2pPeerId::random();
    let overlay = PeerId::from_libp2p(peer);
    let mut fixture = EventFixture::new(peer);
    let old = ConnectionId::new_unchecked(81);
    let current = ConnectionId::new_unchecked(82);
    let endpoint = ConnectedPoint::Dialer {
        address: "/ip4/127.0.0.1/tcp/4001".parse().unwrap(),
        role_override: libp2p::core::Endpoint::Dialer,
        port_use: libp2p::core::transport::PortUse::New,
    };
    fixture.epochs.record_started(old);
    assert!(fixture.epochs.record_established(old));
    fixture
        .active_connections
        .insert((peer, old), endpoint.clone());
    fixture.paths.record_established_with_details(
        overlay,
        PathKind::DirectTcpStream,
        None,
        Some(1280),
        PathOrigin::Identify,
        PathConnectionRole::Dialer,
        false,
        Some(old),
        Some(1),
    );
    fixture.epochs.advance();
    assert_eq!(fixture.paths.invalidate_connections(), 1);
    fixture.epochs.record_started(current);
    assert!(fixture.epochs.record_established(current));
    fixture
        .active_connections
        .insert((peer, current), endpoint.clone());
    fixture.paths.record_established_with_details(
        overlay,
        PathKind::DirectTcpStream,
        None,
        Some(1280),
        PathOrigin::Identify,
        PathConnectionRole::Dialer,
        false,
        Some(current),
        Some(1),
    );
    fixture
        .capabilities
        .record(overlay, ControlCapabilities::local("lab", None, 1280));
    fixture
        .dispatch(SwarmEvent::ConnectionClosed {
            peer_id: peer,
            connection_id: old,
            endpoint: endpoint.clone(),
            num_established: 1,
            cause: None,
        })
        .await;
    assert!(fixture.epochs.is_usable(current));
    assert!(!fixture.epochs.connections.contains_key(&old));
    assert_eq!(fixture.active_connections.len(), 1);
    assert!(fixture.capabilities.contains(overlay));
    let selected = fixture.paths.best_for(overlay).unwrap();
    assert_eq!(selected.latest_connection_id, Some(current));
    assert_eq!(selected.established_connections, 1);
    assert!(!fixture.discovered.has_pending_recovery_discovery_query());
    assert_eq!(
        fixture
            .metrics
            .snapshot(crate::queue::QueueStats::default())
            .redial_attempts,
        0
    );
    fixture
        .dispatch(SwarmEvent::ConnectionClosed {
            peer_id: peer,
            connection_id: current,
            endpoint,
            num_established: 0,
            cause: None,
        })
        .await;
    assert!(fixture.active_connections.is_empty());
    assert!(fixture.epochs.connections.is_empty());
    assert!(!fixture.capabilities.contains(overlay));
    assert!(fixture.paths.best_for(overlay).is_none());
}

#[tokio::test]
async fn configured_listener_loss_preserves_pending_automatic_replacement() {
    for expiration in [false, true] {
        let relay = Libp2pPeerId::random();
        let address = relay_address(relay);
        let relayed = address.clone().with(Protocol::P2pCircuit);
        let mut fixture = EventFixture::new(Libp2pPeerId::random());
        let configured = ListenerId::next();
        fixture.configured_listeners.insert(configured);
        fixture.readiness.record_reservation_accepted(relay);
        fixture
            .dispatch(SwarmEvent::NewListenAddr {
                listener_id: configured,
                address: relayed.clone(),
            })
            .await;
        fixture.auto_relay.record_candidate(relay, address.clone());
        assert_eq!(
            fixture.auto_relay.next_reservation_targets(Instant::now()),
            vec![(relay, address.clone())]
        );
        let pending = ListenerId::next();
        assert!(
            fixture
                .auto_relay
                .record_reservation_listener(relay, pending)
        );
        let deadline = fixture.auto_relay.pending_reservations[&relay].expires_at;
        fixture
            .dispatch(if expiration {
                SwarmEvent::ExpiredListenAddr {
                    listener_id: configured,
                    address: relayed,
                }
            } else {
                SwarmEvent::ListenerClosed {
                    listener_id: configured,
                    addresses: vec![relayed],
                    reason: Ok(()),
                }
            })
            .await;
        assert_eq!(
            fixture.auto_relay.reservation_listeners.get(&relay),
            Some(&pending),
            "configured listener loss cancelled automatic replacement"
        );
        assert_eq!(
            fixture.auto_relay.pending_reservations[&relay].expires_at,
            deadline
        );
        assert!(!fixture.auto_relay.retry_after.contains_key(&relay));
        fixture
            .dispatch(SwarmEvent::NewListenAddr {
                listener_id: pending,
                address: address.with(Protocol::P2pCircuit),
            })
            .await;
        assert!(fixture.readiness.relay_ready(relay));
    }
}

#[tokio::test]
async fn quic_reset_discards_cancelled_and_old_results_without_consuming_new_tasks() {
    let peer = PeerId::from_libp2p(Libp2pPeerId::random());
    let quic = PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let black_hole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let capabilities = ControlCapabilities::local("lab", None, 1200)
        .with_owned_quic_packet_endpoint_candidates(vec![
            black_hole.local_addr().unwrap().to_string(),
        ])
        .with_owned_quic_packet_plane_certificate(quic.server_certificate().as_ref().to_vec());
    let mut negotiator = PacketPlaneNegotiator::default();
    let roles = [
        PacketPlaneQuicNegotiationRole::Initiator,
        PacketPlaneQuicNegotiationRole::Responder,
    ];
    for role in roles {
        negotiator.start_quic_connection(
            quic.connector(),
            peer,
            capabilities.clone(),
            role,
            PacketPlaneQuicConnectionDirection::Connect,
        );
    }
    let old = roles.map(|role| {
        (
            role,
            negotiator.quic_connection_task_handles[&(peer, role)].generation,
        )
    });
    negotiator.clear();
    for role in roles {
        negotiator.start_quic_connection(
            quic.connector(),
            peer,
            capabilities.clone(),
            role,
            PacketPlaneQuicConnectionDirection::Connect,
        );
    }
    let current = roles.map(|role| {
        (
            role,
            negotiator.quic_connection_task_handles[&(peer, role)].generation,
        )
    });
    let mut paths = PathSet::new();
    paths.record_established(peer, PathKind::DirectTcpStream);
    let metrics = RuntimeMetrics::default();
    let mut probes = PathProbeTracker::default();
    for (role, generation) in old {
        handle_packet_plane_quic_connection_task(
            &mut PacketPlaneQuicSessionContext {
                packet_plane_quic: None,
                negotiator: &mut negotiator,
                paths: &mut paths,
                metrics: &metrics,
                path_probe_tracker: &mut probes,
            },
            Ok(PacketPlaneQuicConnectionTaskResult {
                peer,
                role,
                generation,
                result: Err(PacketPlaneNegotiationError::MissingLocalEndpoint),
            }),
        );
    }
    for _ in 0..roles.len() {
        let cancelled = tokio::time::timeout(
            Duration::from_secs(1),
            negotiator.quic_connection_tasks.join_next(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            cancelled
                .as_ref()
                .is_err_and(tokio::task::JoinError::is_cancelled)
        );
        handle_packet_plane_quic_connection_task(
            &mut PacketPlaneQuicSessionContext {
                packet_plane_quic: None,
                negotiator: &mut negotiator,
                paths: &mut paths,
                metrics: &metrics,
                path_probe_tracker: &mut probes,
            },
            cancelled,
        );
    }
    for (role, generation) in current {
        assert_eq!(
            negotiator.quic_connection_task_handles[&(peer, role)].generation,
            generation
        );
    }
    assert_eq!(negotiator.quic_connection_task_handles.len(), 2);
    assert_eq!(
        metrics
            .snapshot(crate::queue::QueueStats::default())
            .control_failures,
        0
    );
    assert!(paths.best_for(peer).is_some());
    let (role, generation) = current[0];
    handle_packet_plane_quic_connection_task(
        &mut PacketPlaneQuicSessionContext {
            packet_plane_quic: None,
            negotiator: &mut negotiator,
            paths: &mut paths,
            metrics: &metrics,
            path_probe_tracker: &mut probes,
        },
        Ok(PacketPlaneQuicConnectionTaskResult {
            peer,
            role,
            generation,
            result: Err(PacketPlaneNegotiationError::MissingLocalEndpoint),
        }),
    );
    assert!(negotiator.quic_connection_task_handles.is_empty());
    assert_eq!(
        metrics
            .snapshot(crate::queue::QueueStats::default())
            .control_failures,
        1
    );
    negotiator.start_quic_connection(
        quic.connector(),
        peer,
        capabilities,
        role,
        PacketPlaneQuicConnectionDirection::Connect,
    );
    assert!(negotiator.quic_connection_task_handles[&(peer, role)].generation > generation);
    negotiator.clear();
    tokio::time::timeout(Duration::from_secs(1), async {
        while negotiator.quic_connection_tasks.join_next().await.is_some() {}
    })
    .await
    .unwrap();
}
