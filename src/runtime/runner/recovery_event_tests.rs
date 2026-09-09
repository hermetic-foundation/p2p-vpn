//! Stateful production dispatch regressions, not physical network race measurements.

use super::tests::{config_with_peer, membership_sync_test_node};
use super::*;

struct UnusedPacketIo;

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
        }
    }

    async fn dispatch(&mut self, event: SwarmEvent<BehaviourEvent>) {
        let (_, mut writer) = PacketIo::new(UnusedPacketIo, UnusedPacketIo).split();
        handle_swarm_event(
            &mut self.node.swarm,
            SwarmEventContext {
                forwarder: &mut self.forwarder,
                membership: &mut self.membership,
                tun_runtime: &mut self.tun,
                route_controller: &mut PreconfiguredTunRoutes,
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
                packet_in_flight: &mut PacketInFlight::new(1),
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
                pairing_replay_tokens: &mut PairingReplayTokens::default(),
                code_pairing_sessions: &mut CodePairingSessions::new(),
                pairing_state_store: None,
                active_connections: &mut self.active_connections,
                connection_epochs: &mut self.epochs,
                membership_probe_connections: &mut MembershipProbeConnections::default(),
                kademlia_maintenance: &mut self.maintenance,
            },
            event,
        )
        .await
        .unwrap();
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
