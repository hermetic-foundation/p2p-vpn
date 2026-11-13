//! Stateful production dispatch regressions, not physical network race measurements.

use super::tests::{config_with_peer, membership_sync_test_node, test_pairing_state_path};
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
    let redundant = redundant_direct_connection_ids(
        local,
        peer,
        &fixture.active_connections,
        &fixture.epochs,
        &[],
    );
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

async fn incoming_anchor_test_relay() -> (P2pNode, Multiaddr, Multiaddr) {
    incoming_anchor_test_host(true, &DiscoveryConfig::default().kademlia_protocol).await
}

async fn incoming_anchor_test_host(
    relay_server: bool,
    protocol: &str,
) -> (P2pNode, Multiaddr, Multiaddr) {
    let mut node = build_node(&HostConfig {
        identity: NodeIdentity::generate_ed25519().unwrap(),
        network_name: "lab".into(),
        membership_tag: None,
        mtu: 1280,
        max_concurrent_control_streams: 8,
        max_concurrent_packet_streams: 8,
        listen_addresses: vec![
            "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
            "/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap(),
        ],
        external_addresses: Vec::new(),
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: Vec::new(),
        relay_server,
        relay_resources: crate::config::RelayResourceConfig::default(),
        resources: crate::config::ResourceConfig::default(),
        discovery: DiscoveryConfig {
            kademlia_protocol: protocol.to_owned(),
            mdns: false,
            kademlia: false,
            autonat: false,
            dcutr: false,
            ..DiscoveryConfig::default()
        },
    })
    .unwrap();
    let (mut tcp, mut quic) = (None, None);
    while tcp.is_none() || quic.is_none() {
        if let SwarmEvent::NewListenAddr { address, .. } = node.swarm.select_next_some().await {
            node.swarm.add_external_address(address.clone());
            let address = address.with(Protocol::P2p(node.local_peer_id));
            if address.iter().any(|p| matches!(p, Protocol::Tcp(_))) {
                tcp = Some(address);
            } else if address.iter().any(|p| p == Protocol::QuicV1) {
                quic = Some(address);
            }
        }
    }
    (node, tcp.unwrap(), quic.unwrap())
}

#[tokio::test]
async fn automatic_incoming_anchor_uses_tcp_after_quic_and_accepts_fresh_relay_control() {
    automatic_incoming_anchor_scenario(AnchorSetup::DialTcp).await;
}

#[tokio::test]
async fn automatic_incoming_anchor_reuses_tcp_without_another_transport_dial() {
    automatic_incoming_anchor_scenario(AnchorSetup::ReuseTcp).await;
}

#[tokio::test]
async fn automatic_incoming_anchor_keeps_quic_fallback_and_accepts_fresh_relay_control() {
    automatic_incoming_anchor_scenario(AnchorSetup::ReuseQuic).await;
}

#[tokio::test]
async fn automatic_incoming_anchor_recovers_over_quic_after_real_tcp_failure() {
    automatic_incoming_anchor_scenario(AnchorSetup::RecoverQuic).await;
}

#[tokio::test]
async fn automatic_incoming_anchor_cold_tcp_failure_uses_quic_and_accepts_fresh_control() {
    automatic_incoming_anchor_scenario(AnchorSetup::ColdQuic).await;
}

#[tokio::test]
async fn automatic_incoming_recovery_replenishes_an_empty_pool_via_native_discovery() {
    automatic_incoming_anchor_scenario(AnchorSetup::DiscoverTcp).await;
}

#[tokio::test]
async fn automatic_incoming_recovery_replaces_a_lost_anchor_without_payload_demand() {
    automatic_incoming_anchor_scenario(AnchorSetup::DiscoverAfterLoss).await;
}

#[tokio::test]
async fn automatic_incoming_recovery_uses_a_private_routing_only_bootstrap_and_separate_relay() {
    automatic_incoming_anchor_scenario(AnchorSetup::PrivateRouter).await;
}

#[tokio::test]
async fn idle_infrastructure_prunes_spare_quic_and_preserves_incoming_after_sparse_tcp_ping() {
    automatic_incoming_anchor_scenario(AnchorSetup::PruneIdle).await;
}

#[tokio::test]
async fn automatic_incoming_anchor_preserves_an_explicit_quic_reservation() {
    tokio::time::timeout(
        Duration::from_secs(10),
        Box::pin(async {
            let (mut relay, tcp, quic) = incoming_anchor_test_relay().await;
            let peer = relay.local_peer_id;
            let mut fixture = EventFixture::new(Libp2pPeerId::random());
            fixture.infrastructure.insert(peer, quic.clone());
            fixture.relay_addresses.push((peer, quic.clone()));
            fixture.auto_relay.record_candidate(peer, tcp);
            let listener = fixture
                .node
                .swarm
                .listen_on(quic.with(Protocol::P2pCircuit))
                .unwrap();
            fixture.configured_listeners.insert(listener);
            while !fixture.readiness.relay_ready(peer) {
                tokio::select! {
                    _ = relay.swarm.select_next_some() => {},
                    event = fixture.node.swarm.select_next_some() => fixture.dispatch(event).await,
                }
            }
            for _ in 0..10 {
                attempt_auto_relay_reservations(
                    &mut fixture.node.swarm,
                    &mut fixture.auto_relay,
                    &fixture.metrics,
                    &mut fixture.epochs,
                    &fixture.active_connections,
                    &fixture.relay_addresses,
                );
            }
            assert!(fixture.configured_listeners.contains(&listener));
            assert!(fixture.auto_relay.pending_reservations.is_empty());
            assert!(fixture.auto_relay.reservation_listeners.is_empty());
            assert_eq!(
                fixture
                    .metrics
                    .snapshot(crate::queue::QueueStats::default())
                    .auto_relay_reservation_attempts,
                0
            );
            let id = fixture
                .node
                .swarm
                .behaviour()
                .relay
                .confirmed_reservation_connections(peer)
                .next()
                .unwrap();
            assert_eq!(
                incoming_anchor::confirmed_connection(
                    &fixture.node.swarm,
                    &fixture.epochs,
                    &fixture.active_connections,
                    peer,
                    id
                ),
                Some((id, PathKind::DirectQuicStream))
            );
            fixture.node.relay_peer_addresses = fixture.relay_addresses.clone();
            fixture
                .node
                .configured_relay_reservation_listeners
                .insert(listener);
            fixture.node.discovery.autonat = true;
            assert!(has_incoming_fixture(&fixture));
            let readiness = std::mem::take(&mut fixture.readiness);
            drive_incoming_fixture(&mut fixture, Instant::now());
            assert!(fixture.maintenance.incoming.pending());
            fixture.readiness = readiness;
            let unrelated = fixture
                .node
                .swarm
                .behaviour_mut()
                .kad
                .get_closest_peers(Libp2pPeerId::random());
            drive_incoming_fixture(&mut fixture, Instant::now());
            assert!(!fixture.maintenance.incoming.pending());
            assert!(
                fixture
                    .node
                    .swarm
                    .behaviour()
                    .kad
                    .query_is_retained(&unrelated)
            );
            fixture
                .node
                .configured_relay_reservation_listeners
                .remove(&listener);
            assert!(
                !has_incoming_fixture(&fixture),
                "cached acceptance cannot replace a live configured listener"
            );
            fixture
                .node
                .configured_relay_reservation_listeners
                .insert(listener);
            fixture.epochs.mark_retiring(id);
            assert!(
                !has_incoming_fixture(&fixture),
                "a retiring configured transport is not incoming readiness"
            );
        }),
    )
    .await
    .expect("explicit reservation was replaced by automatic setup");
}

#[derive(Clone, Copy)]
enum AnchorSetup {
    DialTcp,
    ReuseTcp,
    ReuseQuic,
    RecoverQuic,
    ColdQuic,
    DiscoverTcp,
    DiscoverAfterLoss,
    PrivateRouter,
    PruneIdle,
}

fn retire_infrastructure_fixture(
    receiver: &mut EventFixture,
    queues: &PeerQueues,
    now: Instant,
) -> usize {
    idle_infrastructure::retire(&mut idle_infrastructure::RetirementContext {
        node: &mut receiver.node,
        forwarder: &receiver.forwarder,
        infrastructure: &receiver.infrastructure,
        routing: &receiver.routing,
        queues,
        in_flight: &receiver.packet_in_flight,
        epochs: &mut receiver.epochs,
        connections: &receiver.active_connections,
        readiness: &receiver.readiness,
        auto_relay: &receiver.auto_relay,
        pairing: &receiver.pairing,
        probes: &MembershipProbeConnections::default(),
        discovery_quiet: true,
        now,
    })
}

#[tokio::test]
async fn incoming_routing_identify_preserves_public_pairing_addresses_without_overlay_discovery() {
    for protocol in [
        PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        crate::config::PRIVATE_KADEMLIA_PROTOCOL,
    ] {
        let mut fixture = EventFixture::new(Libp2pPeerId::random());
        fixture.node.discovery.kademlia = false;
        fixture.node.discovery.autonat = true;
        fixture.node.discovery.kademlia_protocol = protocol.to_owned();
        let local = fixture.node.local_peer_id;
        let primary_config = crate::runtime::p2p::controlled_kademlia_config(
            libp2p::StreamProtocol::try_from_owned(protocol.to_owned()).unwrap(),
        );
        fixture.node.swarm.behaviour_mut().kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), primary_config);
        if protocol != PUBLIC_IPFS_KADEMLIA_PROTOCOL {
            let public_config = crate::runtime::p2p::controlled_kademlia_config(
                libp2p::StreamProtocol::new(PUBLIC_IPFS_KADEMLIA_PROTOCOL),
            );
            fixture.node.swarm.behaviour_mut().pairing_kad = Some(kad::Behaviour::with_config(
                local,
                kad::store::MemoryStore::new(local),
                public_config,
            ))
            .into();
        } else {
            fixture.node.swarm.behaviour_mut().pairing_kad = None.into();
        }
        let identity = libp2p::identity::Keypair::generate_ed25519();
        let peer = identity.public().to_peer_id();
        let address: Multiaddr = "/ip4/8.8.8.8/tcp/4001".parse().unwrap();
        let connection_id = ConnectionId::new_unchecked(1);
        fixture.epochs.record_started(connection_id);
        assert!(fixture.epochs.record_established(connection_id));
        let mut protocols = vec![libp2p::StreamProtocol::new(PUBLIC_IPFS_KADEMLIA_PROTOCOL)];
        if protocol != PUBLIC_IPFS_KADEMLIA_PROTOCOL {
            protocols.push(libp2p::StreamProtocol::new(
                crate::config::PRIVATE_KADEMLIA_PROTOCOL,
            ));
        }
        fixture
            .dispatch(SwarmEvent::Behaviour(BehaviourEvent::Identify(
                identify::Event::Received {
                    connection_id,
                    peer_id: peer,
                    info: identify::Info {
                        public_key: identity.public(),
                        protocol_version: "test/1".to_owned(),
                        agent_version: "pairing-retention-test".to_owned(),
                        listen_addrs: vec![address.clone()],
                        protocols,
                        observed_addr: "/ip4/127.0.0.1/tcp/4002".parse().unwrap(),
                        signed_peer_record: None,
                    },
                },
            )))
            .await;
        let addresses =
            public_pairing_kad_mut(fixture.node.swarm.behaviour_mut()).addresses_of_peer(peer);
        assert_eq!(addresses.len(), 1, "{protocol}");
        assert_eq!(
            first_ip_in_multiaddr(&addresses[0]),
            first_ip_in_multiaddr(&address)
        );
        assert!(
            fixture.discovered.addresses.is_empty(),
            "routing peers must not enter mesh redial queues"
        );
        assert!(
            !fixture
                .discovered
                .retention
                .overlay_peers()
                .any(|entry| entry == peer)
        );
        assert_eq!(
            fixture
                .discovered
                .retention
                .retain_peers(|entry| entry != peer),
            vec![(peer, peer_dial_address(peer, address.clone()))]
        );
        if protocol != PUBLIC_IPFS_KADEMLIA_PROTOCOL {
            assert!(
                fixture
                    .node
                    .swarm
                    .behaviour_mut()
                    .kad
                    .addresses_of_peer(peer)
                    .is_empty()
            );
        }
        assert!(fixture.routing.contains(peer));
        assert!(!fixture.membership.allows(peer));
        assert!(!fixture.node.discovery.kademlia);
        assert!(fixture.node.kademlia_rendezvous_key.is_none());
        assert_eq!(
            fixture
                .node
                .swarm
                .behaviour()
                .kad
                .query_pool_usage()
                .retained,
            0
        );
    }
}

fn drive_incoming_fixture(receiver: &mut EventFixture, now: Instant) {
    incoming_recovery::drive(
        &mut receiver.node,
        incoming_recovery::RecoveryContext {
            state: &mut receiver.auto_relay,
            maintenance: &mut receiver.maintenance,
            readiness: &receiver.readiness,
            epochs: &receiver.epochs,
            connections: &receiver.active_connections,
            backoff: &receiver.backoff,
            holdoff_until: None,
            metrics: &receiver.metrics,
        },
        now,
    );
}

fn has_incoming_fixture(receiver: &EventFixture) -> bool {
    incoming_recovery::has_usable_path(
        &receiver.node,
        &receiver.auto_relay,
        &receiver.readiness,
        &receiver.epochs,
        &receiver.active_connections,
    )
}

async fn lose_anchor_and_exhaust_candidates(receiver: &mut EventFixture) {
    let (mut relay, tcp, _) = incoming_anchor_test_relay().await;
    let peer = relay.local_peer_id;
    receiver.infrastructure.insert(peer, tcp.clone());
    connect_anchor_fixture(receiver, &mut relay, tcp.clone()).await;
    receiver.auto_relay.record_candidate(peer, tcp);
    attempt_auto_relay_reservations(
        &mut receiver.node.swarm,
        &mut receiver.auto_relay,
        &receiver.metrics,
        &mut receiver.epochs,
        &receiver.active_connections,
        &[],
    );
    while !receiver
        .auto_relay
        .accepted_reservation_peers
        .contains(&peer)
    {
        tokio::select! {
            _ = relay.swarm.select_next_some() => {},
            event = receiver.node.swarm.select_next_some() => receiver.dispatch(event).await,
        }
    }
    assert!(incoming_recovery::has_usable_path(
        &receiver.node,
        &receiver.auto_relay,
        &receiver.readiness,
        &receiver.epochs,
        &receiver.active_connections
    ));
    drop(relay);
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    while receiver
        .auto_relay
        .candidates
        .iter()
        .any(|(owner, _)| *owner == peer)
        || receiver
            .active_connections
            .keys()
            .any(|(owner, _)| *owner == peer)
    {
        tokio::select! {
            _ = tick.tick() => {
                let now = receiver.auto_relay.retry_after.get(&peer).copied().unwrap_or_else(Instant::now);
                attempt_auto_relay_reservations_at(&mut receiver.node.swarm, &mut receiver.auto_relay,
                    &receiver.metrics, &mut receiver.epochs, &receiver.active_connections, now);
            },
            event = receiver.node.swarm.select_next_some() => receiver.dispatch(event).await,
        }
    }
    assert!(receiver.auto_relay.pending_reservations.is_empty());
    assert!(!incoming_recovery::has_usable_path(
        &receiver.node,
        &receiver.auto_relay,
        &receiver.readiness,
        &receiver.epochs,
        &receiver.active_connections
    ));
}

async fn connect_anchor_fixture(
    receiver: &mut EventFixture,
    relay: &mut P2pNode,
    address: Multiaddr,
) -> ConnectionId {
    let peer = relay.local_peer_id;
    let options = DialOpts::peer_id(peer)
        .condition(PeerCondition::Always)
        .addresses(vec![address])
        .build();
    let id = options.connection_id();
    receiver
        .epochs
        .dial(&mut receiver.node.swarm, options)
        .unwrap();
    while !receiver.active_connections.contains_key(&(peer, id)) {
        tokio::select! {
            _ = relay.swarm.select_next_some() => {},
            event = receiver.node.swarm.select_next_some() => receiver.dispatch(event).await,
        }
    }
    id
}

fn assert_anchor_readiness_fences(
    receiver: &mut EventFixture,
    relay: Libp2pPeerId,
    id: ConnectionId,
) {
    let listener = receiver.auto_relay.reservation_listeners[&relay];
    receiver.auto_relay.release_reservation_for_retry(relay);
    receiver.auto_relay.next_reservation_targets(Instant::now());
    assert!(receiver.auto_relay.record_reservation_listener(
        relay,
        listener.id,
        listener.connection
    ));
    receiver
        .auto_relay
        .pending_reservations
        .get_mut(&relay)
        .unwrap()
        .expires_at = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .unwrap();
    incoming_anchor::confirm_if_ready(
        &receiver.node.swarm,
        &receiver.epochs,
        &receiver.active_connections,
        &mut receiver.auto_relay,
        &receiver.readiness,
        relay,
    );
    assert!(
        !receiver
            .auto_relay
            .accepted_reservation_peers
            .contains(&relay),
        "expired attempt cannot become ready"
    );
    receiver
        .auto_relay
        .pending_reservations
        .get_mut(&relay)
        .unwrap()
        .expires_at = Instant::now() + AUTO_RELAY_RESERVATION_PENDING_TIMEOUT;
    receiver
        .auto_relay
        .reservation_listeners
        .get_mut(&relay)
        .unwrap()
        .id = ListenerId::next();
    incoming_anchor::confirm_if_ready(
        &receiver.node.swarm,
        &receiver.epochs,
        &receiver.active_connections,
        &mut receiver.auto_relay,
        &receiver.readiness,
        relay,
    );
    assert!(
        !receiver
            .auto_relay
            .accepted_reservation_peers
            .contains(&relay),
        "unowned listener cannot become ready"
    );
    receiver
        .auto_relay
        .reservation_listeners
        .insert(relay, listener);
    let unconfirmed = receiver
        .active_connections
        .keys()
        .find_map(|(peer, candidate)| (*peer == relay && *candidate != id).then_some(*candidate))
        .unwrap_or(ConnectionId::new_unchecked(999_999));
    receiver
        .auto_relay
        .reservation_listeners
        .get_mut(&relay)
        .unwrap()
        .connection = unconfirmed;
    incoming_anchor::confirm_if_ready(
        &receiver.node.swarm,
        &receiver.epochs,
        &receiver.active_connections,
        &mut receiver.auto_relay,
        &receiver.readiness,
        relay,
    );
    assert!(
        !receiver
            .auto_relay
            .accepted_reservation_peers
            .contains(&relay),
        "another connection to the same relay is not this reservation's owner"
    );
    receiver
        .auto_relay
        .reservation_listeners
        .insert(relay, listener);
    incoming_anchor::confirm_if_ready(
        &receiver.node.swarm,
        &receiver.epochs,
        &receiver.active_connections,
        &mut receiver.auto_relay,
        &receiver.readiness,
        relay,
    );
    assert!(
        receiver
            .auto_relay
            .accepted_reservation_peers
            .contains(&relay)
    );
    assert!(has_incoming_fixture(receiver));
    assert!(receiver.lifecycle_readiness().incoming_path_available);
    receiver.epochs.mark_retiring(id);
    assert!(!has_incoming_fixture(receiver));
    assert!(!receiver.lifecycle_readiness().incoming_path_available);
    assert!(
        incoming_anchor::confirmed_connection(
            &receiver.node.swarm,
            &receiver.epochs,
            &receiver.active_connections,
            relay,
            id
        )
        .is_none()
    );
    receiver.epochs.retiring.remove(&id);
    receiver.epochs.advance();
    assert!(!has_incoming_fixture(receiver));
    assert!(!receiver.lifecycle_readiness().incoming_path_available);
    assert!(
        incoming_anchor::confirmed_connection(
            &receiver.node.swarm,
            &receiver.epochs,
            &receiver.active_connections,
            relay,
            id
        )
        .is_none()
    );
}

async fn automatic_incoming_anchor_scenario(setup: AnchorSetup) {
    let prune = matches!(setup, AnchorSetup::PruneIdle);
    tokio::time::timeout(Duration::from_secs(if prune {90} else {15}), Box::pin(async {
        let private_router = matches!(setup, AnchorSetup::PrivateRouter);
        let protocol = if private_router { crate::config::PRIVATE_KADEMLIA_PROTOCOL.to_owned() }
            else { DiscoveryConfig::default().kademlia_protocol };
        let (mut relay, tcp, quic) = incoming_anchor_test_host(true, &protocol).await;
        let relay_peer = relay.local_peer_id;
        let mut sender = membership_sync_test_node(NodeIdentity::generate_ed25519().unwrap());
        let mut receiver = EventFixture::new(sender.local_peer_id);
        receiver.auto_relay.policy.max_reservations = 1;
        let discover = matches!(setup, AnchorSetup::DiscoverTcp | AnchorSetup::DiscoverAfterLoss | AnchorSetup::PrivateRouter);
        let mut routing_bootstrap = None;
        if matches!(setup, AnchorSetup::DiscoverAfterLoss) {
            lose_anchor_and_exhaust_candidates(&mut receiver).await;
        }
        if discover {
            receiver.node.discovery.autonat = true;
            receiver.node.discovery.kademlia_protocol = protocol.clone();
            receiver.node.bootstrap_peer_addresses = vec![(relay_peer, tcp.clone())];
            // Replace the fixture's inherited public seeds as well as its config.
            // The production recovery driver must be the sole source of this seed.
            let config = crate::runtime::p2p::controlled_kademlia_config(
                libp2p::StreamProtocol::try_from_owned(protocol.clone()).unwrap());
            let local = receiver.node.local_peer_id;
            receiver.node.swarm.behaviour_mut().kad = kad::Behaviour::with_config(
                local, kad::store::MemoryStore::new(local), config);
            relay.swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));
            if private_router {
                let (mut bootstrap, address, _) = incoming_anchor_test_host(false, &protocol).await;
                bootstrap.swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));
                bootstrap.swarm.behaviour_mut().kad.add_protected_address(&relay_peer, tcp.clone());
                receiver.node.bootstrap_peer_addresses = vec![(bootstrap.local_peer_id, address)];
                routing_bootstrap = Some(bootstrap);
            }
        } else {
            receiver.infrastructure.insert(relay_peer, tcp.clone());
        }
        let quic_id = if matches!(setup, AnchorSetup::ColdQuic) || discover {
            assert!(receiver.active_connections.is_empty());
            None
        } else {
            Some(connect_anchor_fixture(&mut receiver, &mut relay, quic.clone()).await)
        };
        let reuse = match setup {
            AnchorSetup::ReuseTcp => Some(connect_anchor_fixture(&mut receiver, &mut relay, tcp.clone()).await),
            AnchorSetup::ReuseQuic | AnchorSetup::RecoverQuic => quic_id,
            AnchorSetup::DialTcp | AnchorSetup::ColdQuic | AnchorSetup::DiscoverTcp | AnchorSetup::DiscoverAfterLoss | AnchorSetup::PrivateRouter | AnchorSetup::PruneIdle => None,
        };
        let use_quic = matches!(setup, AnchorSetup::ReuseQuic | AnchorSetup::RecoverQuic | AnchorSetup::ColdQuic);
        let address = if use_quic {quic} else {tcp};
        let candidate = if matches!(setup, AnchorSetup::RecoverQuic | AnchorSetup::ColdQuic) {
            let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = socket.local_addr().unwrap().port();
            drop(socket);
            format!("/ip4/127.0.0.1/tcp/{port}/p2p/{relay_peer}").parse().unwrap()
        } else {address.clone()};
        let refused_route = candidate.clone();
        if discover {
            assert!(receiver.auto_relay.candidates.is_empty());
            drive_incoming_fixture(&mut receiver, Instant::now());
            assert!(receiver.maintenance.incoming.pending());
        } else if matches!(setup, AnchorSetup::ColdQuic) {
            receiver.auto_relay.record_candidate_addresses(relay_peer, [candidate, address.clone()]);
        } else {
            receiver.auto_relay.record_candidate(relay_peer, candidate);
        }
        attempt_auto_relay_reservations(&mut receiver.node.swarm, &mut receiver.auto_relay,
            &receiver.metrics, &mut receiver.epochs, &receiver.active_connections, &[]);
        let owned = receiver.auto_relay.pending_reservations.get(&relay_peer).and_then(|pending| pending.owned_dial);
        let expected_owner = reuse.or(owned);
        assert_eq!(owned.is_some(), matches!(setup, AnchorSetup::DialTcp | AnchorSetup::RecoverQuic | AnchorSetup::ColdQuic | AnchorSetup::PruneIdle));
        assert_eq!(receiver.auto_relay.reservation_listeners.is_empty(), owned.is_some() || discover);
        let kind = if use_quic {PathKind::DirectQuicStream} else {PathKind::DirectTcpStream};
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        while !receiver.auto_relay.accepted_reservation_peers.contains(&relay_peer) {
            tokio::select! {
                _ = relay.swarm.select_next_some() => {},
                _ = async {
                    if let Some(bootstrap) = &mut routing_bootstrap { bootstrap.swarm.select_next_some().await; }
                    else { std::future::pending::<()>().await; }
                } => {},
                _ = tick.tick() => {
                    // Exercise the configured retry boundary without a 30-second test sleep.
                    let now = receiver.auto_relay.retry_after.get(&relay_peer).copied().unwrap_or_else(Instant::now);
                    attempt_auto_relay_reservations_at(&mut receiver.node.swarm,
                        &mut receiver.auto_relay, &receiver.metrics, &mut receiver.epochs, &receiver.active_connections, now);
                    if discover { drive_incoming_fixture(&mut receiver, now); }
                },
                event = receiver.node.swarm.select_next_some() => receiver.dispatch(event).await,
            }
        }
        if let Some(bootstrap) = &mut routing_bootstrap {
            let peer = bootstrap.local_peer_id;
            while !receiver.routing.contains(peer) {
                tokio::select! {
                    _ = relay.swarm.select_next_some() => {},
                    _ = bootstrap.swarm.select_next_some() => {},
                    event = receiver.node.swarm.select_next_some() => receiver.dispatch(event).await,
                }
            }
            assert!(!receiver.infrastructure.contains(peer));
            assert!(!receiver.membership.allows(peer));
            assert!(!receiver.node.discovery.kademlia);
            assert!(receiver.node.kademlia_rendezvous_key.is_none());
            assert!(receiver.node.swarm.is_connected(&peer));
        }
        let owner = receiver.auto_relay.reservation_listeners[&relay_peer].connection;
        if let Some(expected) = expected_owner { assert_eq!(owner, expected); }
        assert_eq!(incoming_anchor::confirmed_connection(&receiver.node.swarm, &receiver.epochs,
            &receiver.active_connections, relay_peer, owner), Some((owner, kind)));
        if matches!(setup, AnchorSetup::DialTcp) {
            assert!(matches!(receiver.active_connections[&(relay_peer, owner)],
                ConnectedPoint::Dialer {port_use: libp2p::core::transport::PortUse::New, ..}));
        }
        if let Some(quic_id) = quic_id {
            assert!(receiver.epochs.is_usable(quic_id), "TCP promotion must not destroy the old connection");
        }
        if prune {
            while idle_infrastructure::wire_busy(&receiver.node) {
                tokio::select! {
                    _ = relay.swarm.select_next_some() => {},
                    event = receiver.node.swarm.select_next_some() => receiver.dispatch(event).await,
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {},
                }
            }
            let quic_id = quic_id.unwrap();
            let now = Instant::now() + Duration::from_secs(61);
            let mut queues = PeerQueues::new(4, 4096);
            queues.enqueue(crate::queue::Packet::new(PeerId::from_libp2p(sender.local_peer_id), 1, vec![0x45;20])).unwrap();
            assert_eq!(retire_infrastructure_fixture(&mut receiver, &queues, now), 0);
            queues.dequeue().unwrap();
            let query = receiver.node.swarm.behaviour_mut().kad.get_closest_peers(Libp2pPeerId::random());
            assert_eq!(retire_infrastructure_fixture(&mut receiver, &queues, now), 0);
            receiver.node.swarm.behaviour_mut().kad.cancel_query(&query);
            assert_eq!(retire_infrastructure_fixture(&mut receiver, &queues, now), 1);
            assert!(receiver.epochs.retiring.contains(&quic_id));
            assert!(receiver.epochs.is_usable(owner));
            assert_eq!(retire_infrastructure_fixture(&mut receiver, &queues, now), 0);
            while receiver.active_connections.contains_key(&(relay_peer, quic_id)) {
                tokio::select! {
                    _ = relay.swarm.select_next_some() => {},
                    event = receiver.node.swarm.select_next_some() => receiver.dispatch(event).await,
                }
            }
            assert!(has_incoming_fixture(&receiver));
            assert!(receiver.infrastructure.contains(relay_peer), "retirement must preserve the active anchor's peer metadata");
            let deadline = tokio::time::sleep(Duration::from_secs(62));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    _ = relay.swarm.select_next_some() => {},
                    event = receiver.node.swarm.select_next_some() => {
                        assert!(!matches!(&event, SwarmEvent::ConnectionClosed {connection_id, ..} if *connection_id == owner), "TCP anchor must survive sparse native ping interval");
                        receiver.dispatch(event).await;
                    },
                }
            }
            assert!(has_incoming_fixture(&receiver));
            assert_eq!(receiver.metrics.snapshot(crate::queue::QueueStats::default()).auto_relay_reservation_attempts, 1);
        }
        if discover {
            assert!(incoming_recovery::has_usable_path(&receiver.node, &receiver.auto_relay,
                &receiver.readiness, &receiver.epochs, &receiver.active_connections));
            drive_incoming_fixture(&mut receiver, Instant::now());
            assert!(!receiver.maintenance.incoming.pending());
            assert_eq!(receiver.packet_in_flight.stats().packets, 0);
            assert_eq!(receiver.metrics.snapshot(crate::queue::QueueStats::default()).auto_relay_discovery_queries, 1);
        }
        if matches!(setup, AnchorSetup::ColdQuic) {
            assert_eq!(receiver.metrics.snapshot(crate::queue::QueueStats::default()).auto_relay_reservation_attempts, 1);
            let peer = receiver.node.local_peer_id;
            let refused = refused_route.with(Protocol::P2pCircuit).with(Protocol::P2p(peer));
            let successful = address.clone().with(Protocol::P2pCircuit).with(Protocol::P2p(peer));
            assert!(!receiver.node.swarm.external_addresses().any(|route| *route == refused), "failed primary route must not be advertised");
            assert!(receiver.node.swarm.external_addresses().any(|route| *route == successful), "advertise the authenticated fallback route");
        }
        assert_eq!(receiver.auto_relay.reservation_slots(), 1);
        let peer = receiver.node.local_peer_id;
        sender.swarm.dial(address.with(Protocol::P2pCircuit).with(Protocol::P2p(peer))).unwrap();
        let (mut request, mut relayed) = (None, false);
        loop {
            tokio::select! {
                _ = relay.swarm.select_next_some() => {},
                _ = async {
                    if let Some(bootstrap) = &mut routing_bootstrap { bootstrap.swarm.select_next_some().await; }
                    else { std::future::pending::<()>().await; }
                } => {},
                event = receiver.node.swarm.select_next_some() => receiver.dispatch(event).await,
                event = sender.swarm.select_next_some() => match event {
                    SwarmEvent::ConnectionEstablished {peer_id, endpoint, ..} if peer_id == peer => {
                        assert!(endpoint.is_relayed());
                        relayed = true;
                        request = Some(sender.swarm.behaviour_mut().control.send_request(&peer,
                            ControlRequest::Capabilities(ControlCapabilities::local("lab", None, 1280))));
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                        peer: owner, message: Message::Response {request_id, response}, ..
                    })) if Some(request_id) == request => {
                        assert_eq!(owner, peer);
                        assert!(matches!(response, ControlResponse::CapabilitiesAccepted(_)));
                        assert!(relayed);
                        break;
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                        message: Message::Request {channel, ..}, ..
                    })) => {
                        sender.swarm.behaviour_mut().control.send_response(channel,
                            ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("lab", None, 1280))).unwrap();
                    }
                    SwarmEvent::OutgoingConnectionError {error, ..} => panic!("fresh incoming circuit failed: {error}"),
                    _ => {},
                }
            }
        }
        assert_anchor_readiness_fences(&mut receiver, relay_peer, owner);
    })).await.expect("automatic anchor and fresh inbound control deadline");
}

#[tokio::test]
async fn late_start_and_success_events_cannot_admit_stale_or_retired_paths() {
    for scenario in ["old_epoch", "retired_pending", "retired_established"] {
        let peer = Libp2pPeerId::random();
        let overlay = PeerId::from_libp2p(peer);
        let mut fixture = EventFixture::new(peer);
        let connection_id = ConnectionId::new_unchecked(71);
        fixture.epochs.record_started(connection_id);
        if scenario == "retired_established" {
            assert!(fixture.epochs.record_established(connection_id));
        }
        if scenario == "old_epoch" {
            fixture.epochs.advance();
        } else {
            fixture.epochs.mark_retiring(connection_id);
        }
        fixture
            .dispatch(SwarmEvent::Dialing {
                peer_id: Some(peer),
                connection_id,
            })
            .await;
        assert!(!fixture.epochs.is_usable(connection_id), "{scenario}");
        fixture
            .dispatch(SwarmEvent::ConnectionEstablished {
                peer_id: peer,
                connection_id,
                endpoint: ConnectedPoint::Dialer {
                    address: "/ip4/127.0.0.1/tcp/42001".parse().unwrap(),
                    role_override: libp2p::core::Endpoint::Dialer,
                    port_use: libp2p::core::transport::PortUse::New,
                },
                num_established: std::num::NonZeroU32::new(1).unwrap(),
                concurrent_dial_errors: None,
                established_in: Duration::ZERO,
            })
            .await;
        assert!(fixture.epochs.retiring.contains(&connection_id));
        assert!(!fixture.epochs.is_usable(connection_id), "{scenario}");
        assert!(fixture.active_connections.is_empty(), "{scenario}");
        assert_eq!(
            fixture.paths.candidates_for(overlay).count(),
            0,
            "{scenario}"
        );
    }
}

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
    infrastructure: InfrastructurePeers,
    routing: RoutingInfrastructurePeers,
    tun: TunRuntimeConfig,
    paths: PathSet,
    capabilities: PeerCapabilities,
    readiness: RelayReadiness,
    relay_addresses: Vec<(Libp2pPeerId, Multiaddr)>,
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
    pairing_store: Option<PairingStateStore>,
    pairing_tokens: PairingReplayTokens,
    packet_in_flight: PacketInFlight,
    membership_record_syncs: MembershipRecordSyncs,
}

impl EventFixture {
    fn new(peer: Libp2pPeerId) -> Self {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let config = config_with_peer(&identity, peer);
        Self {
            node: membership_sync_test_node(identity),
            forwarder: Forwarder::from_config(&config).unwrap(),
            membership: OverlayMembership::from_config(&config).unwrap(),
            infrastructure: InfrastructurePeers::default(),
            routing: RoutingInfrastructurePeers::default(),
            tun: TunRuntimeConfig::from_config(&config).unwrap(),
            paths: PathSet::new(),
            capabilities: PeerCapabilities::default(),
            readiness: RelayReadiness::default(),
            relay_addresses: Vec::new(),
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
            pairing_store: None,
            pairing_tokens: PairingReplayTokens::default(),
            packet_in_flight: PacketInFlight::new(1),
            membership_record_syncs: MembershipRecordSyncs::default(),
        }
    }

    fn lifecycle_readiness(&mut self) -> super::super::lifecycle::Readiness {
        let peer = self.forwarder.configured_transport_peers().next().unwrap();
        demand_lifecycle::RuntimeInputs {
            forwarder: &self.forwarder,
            local: &ControlCapabilities::local("lab", None, 1280),
            peers: &self.capabilities,
            previous_tags: &[],
            epochs: &self.epochs,
            connections: &self.active_connections,
            relay_readiness: &self.readiness,
            auto_relay: &self.auto_relay,
            queues: &PeerQueues::new(4, 4096),
            in_flight: &self.packet_in_flight,
            paths: &self.paths,
            udp: None,
            quic: None,
        }
        .with(&mut self.node, |_, context| (context.readiness)(peer))
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
                infrastructure_peers: &mut self.infrastructure,
                routing_infrastructure_peers: &mut self.routing,
                writer: &mut writer,
                paths: &mut self.paths,
                peer_capabilities: &mut self.capabilities,
                relay_readiness: &mut self.readiness,
                auto_relay: &mut self.auto_relay,
                public_discovery_backoff: &mut self.backoff,
                public_discovery_holdoff_active: false,
                relay_addresses: &self.relay_addresses,
                configured_peer_addresses: &[],
                configured_relay_reservation_listeners: &mut self.configured_listeners,
                retiring_configured_relay_reservation_listeners: &mut self.retired_listeners,
                relay_server_enabled: false,
                discovered_peer_addresses: &mut self.discovered,
                packet_in_flight: &mut self.packet_in_flight,
                inbound_packet_rate_limiters: &mut PeerRateLimiters::new(1),
                pairing_request_rate_limiters: &mut PeerRateLimiters::new(1),
                membership_page_rate_limiters: &mut PeerRateLimiters::new(1),
                membership_record_syncs: &mut self.membership_record_syncs,
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
                pairing_state_store: self.pairing_store.as_ref(),
                checkpoint_pairing: None,
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
        assert!(self.auto_relay.record_reservation_listener(
            relay,
            listener,
            ConnectionId::new_unchecked(0)
        ));
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
async fn pairing_relay_establishment_bypasses_full_routing_pool_only_while_owned() {
    for owned in [false, true] {
        let relay = Libp2pPeerId::random();
        let inviter = Libp2pPeerId::random();
        let mut fixture = EventFixture::new(inviter);
        for _ in 0..fixture.auto_relay.policy.max_reservations {
            fixture
                .auto_relay
                .accepted_reservation_peers
                .insert(Libp2pPeerId::random());
        }
        assert!(!fixture.auto_relay.should_discover_candidates());
        let now = Instant::now();
        let operation = fixture
            .pairing
            .join(
                "lab",
                crate::pairing_code::PairingCode::generate(),
                None,
                Vec::new(),
                600,
                current_unix_seconds_lossy(),
                now,
            )
            .unwrap();
        fixture.pairing.mark_peer_attempted(inviter, now).unwrap();
        fixture.pairing.record_pairing_relays(inviter, [relay]);
        if !owned {
            fixture
                .pairing
                .release_peer_attempt(&operation.operation_id, inviter, now);
        }
        for _ in 0..KADEMLIA_ROUTING_PEER_CAPACITY {
            assert_eq!(
                fixture.routing.admit(Libp2pPeerId::random()),
                RoutingInfrastructureAdmission::Admitted
            );
        }
        fixture.backoff.bootstrap_peers.insert(relay);
        fixture.backoff.suppressed_until = Some(now + Duration::from_mins(1));
        let connection_id = ConnectionId::new_unchecked(700);
        fixture.epochs.record_started(connection_id);
        fixture
            .dispatch(SwarmEvent::ConnectionEstablished {
                peer_id: relay,
                connection_id,
                endpoint: ConnectedPoint::Dialer {
                    address: "/ip4/135.181.230.175/tcp/4001".parse().unwrap(),
                    role_override: libp2p::core::Endpoint::Dialer,
                    port_use: libp2p::core::transport::PortUse::New,
                },
                num_established: std::num::NonZeroU32::new(1).unwrap(),
                concurrent_dial_errors: None,
                established_in: Duration::ZERO,
            })
            .await;
        assert_eq!(fixture.backoff.suppresses(now), !owned);
        assert_eq!(fixture.routing.len(), KADEMLIA_ROUTING_PEER_CAPACITY);
        assert!(!fixture.membership.allows(relay));
        assert!(!fixture.forwarder.is_configured_transport_peer(relay));
        assert_eq!(
            fixture
                .paths
                .candidates_for(PeerId::from_libp2p(relay))
                .count(),
            0
        );
    }
}

#[tokio::test]
async fn pairing_relay_completes_code_pairing_with_a_full_routing_pool() {
    tokio::time::timeout(Duration::from_secs(15), Box::pin(async {
        let mut relay = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().unwrap(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig { mdns: false, dcutr: false, autonat: false,
                ..DiscoveryConfig::default() },
        }).unwrap();
        let relay_address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = relay.swarm.select_next_some().await {
                break address;
            }
        };
        relay.swarm.add_external_address(relay_address.clone());
        let relay_peer = relay.local_peer_id;
        let circuit = relay_address.with(Protocol::P2p(relay_peer)).with(Protocol::P2pCircuit);
        let mut inviter = EventFixture::new(Libp2pPeerId::random());
        // The inviter owns a reservation, but the joiner has no configured relay.
        inviter.node.swarm.listen_on(circuit.clone()).unwrap();
        loop {
            tokio::select! {
                _ = relay.swarm.select_next_some() => {}
                event = inviter.node.swarm.select_next_some() => {
                    if matches!(event, SwarmEvent::NewListenAddr { ref address, .. }
                        if address.iter().any(|p| matches!(p, Protocol::P2pCircuit))) {
                        break;
                    }
                }
            }
        }
        let inviter_peer = inviter.node.local_peer_id;
        let mut joiner = EventFixture::new(Libp2pPeerId::random());
        let joiner_peer = joiner.node.local_peer_id;
        for (fixture, name) in [
            (&mut inviter, "relay-code-inviter"),
            (&mut joiner, "relay-code-joiner"),
        ] {
            let mut config = fixture.forwarder.config().clone();
            config.peers.clear();
            fixture.forwarder = Forwarder::from_config(&config).unwrap();
            fixture.membership = OverlayMembership::from_config(&config).unwrap();
            fixture.tun = TunRuntimeConfig::from_config(&config).unwrap();
            fixture.pairing_store = Some(PairingStateStore::encrypted(
                test_pairing_state_path(name),
                &fixture.node.identity.private_key,
                "lab",
                &fixture.node.identity.peer_id,
            ).unwrap());
        }
        let now = Instant::now();
        let unix = current_unix_seconds_lossy();
        let open = inviter.pairing.open("lab", 600, unix, now).unwrap();
        for _ in 0..joiner.auto_relay.policy.max_reservations {
            joiner.auto_relay.accepted_reservation_peers.insert(Libp2pPeerId::random());
        }
        assert!(!joiner.auto_relay.should_discover_candidates());
        let join = joiner.pairing.join("lab", open.code.parse().unwrap(), None,
            Vec::new(), 600, unix, now).unwrap();
        public_pairing_kad_mut(joiner.node.swarm.behaviour_mut())
            .add_address(&inviter_peer, circuit.with(Protocol::P2p(inviter_peer)));
        for _ in 0..KADEMLIA_ROUTING_PEER_CAPACITY {
            assert_eq!(joiner.routing.admit(Libp2pPeerId::random()),
                RoutingInfrastructureAdmission::Admitted);
        }
        send_pairing_code_hello(&mut joiner.node.swarm, &mut joiner.pairing,
            &joiner.node.identity, "lab", inviter_peer, PairingDiscoveryStage::Public,
            &joiner.metrics, now);
        assert!(joiner.pairing.uses_pairing_relay(relay_peer));
        let mut relayed_connection = false;
        let mut pending_responses = 0;
        let mut pending_poll_seen = false;
        let mut approved = false;
        let mut ticks = tokio::time::interval(Duration::from_millis(20));
        loop {
            tokio::select! {
                _ = ticks.tick() => {
                    drive_code_pairing_discovery(&mut joiner.node.swarm, &mut joiner.pairing,
                        &joiner.node.identity, "lab", &joiner.node.discovery, &joiner.metrics);
                }
                _ = relay.swarm.select_next_some() => {}
                event = inviter.node.swarm.select_next_some() => {
                    if matches!(&event,
                        SwarmEvent::Behaviour(BehaviourEvent::PairingCode(request_response::Event::Message {
                            message: Message::Request { request: PairingCodeRequest::Poll { .. }, .. }, ..
                        }))) && !approved {
                        pending_poll_seen = true;
                    }
                    inviter.dispatch(event).await;
                }
                event = joiner.node.swarm.select_next_some() => {
                    if let SwarmEvent::ConnectionEstablished { peer_id, ref endpoint, .. } = event
                        && peer_id == inviter_peer {
                        assert!(endpoint.is_relayed());
                        relayed_connection = true;
                    }
                    if matches!(&event,
                        SwarmEvent::Behaviour(BehaviourEvent::PairingCode(request_response::Event::Message {
                            peer, message: Message::Response { response: PairingCodeResponse::Pending { .. }, .. }, ..
                        })) if *peer == inviter_peer) {
                        pending_responses += 1;
                    }
                    if let SwarmEvent::Behaviour(BehaviourEvent::PairingCode(request_response::Event::OutboundFailure { error, .. })) = &event {
                        panic!("relay-backed code pairing failed: {error}");
                    }
                    joiner.dispatch(event).await;
                }
            }
            if !approved && pending_responses >= 2 {
                assert!(pending_poll_seen, "must poll while approval is still pending");
                assert!(joiner.pairing.uses_pairing_relay(relay_peer));
                assert!(!joiner.membership.allows(inviter_peer));
                assert!(!inviter.membership.allows(joiner_peer));
                let PairingOpenStatus::AwaitingApproval { approval_id, joiner_peer: candidate, .. } =
                    inviter.pairing.open_status(&open.operation_id).unwrap() else {
                        panic!("inviter did not reach approval");
                    };
                assert_eq!(candidate, joiner_peer.to_string());
                let mut promoted = None;
                let response = handle_pair_rpc_request(
                    &mut inviter.node.swarm,
                    PairRpcRequest::PairApprove {
                        operation_id: open.operation_id.clone(), approval_id,
                        assigned_hostname: None, assigned_vpn_ip: None, granted_routes: Vec::new(),
                    },
                    &mut inviter.pairing, inviter.pairing_store.as_ref(),
                    &mut inviter.forwarder, &mut inviter.membership, &mut inviter.tun,
                    &mut PreconfiguredTunRoutes, &mut ControlCapabilities::local("lab", None, 1280),
                    &inviter.node.identity, &mut inviter.pairing_tokens.code_approval,
                    &mut promoted, &inviter.metrics, None,
                );
                let super::super::control_socket::PairRpcOutcome::Ok {
                    result: PairRpcResult::ActionAccepted(status),
                } = response.outcome else { panic!("approval failed: {response:?}"); };
                assert_eq!(status.phase, PairRpcPhase::Completed);
                assert_eq!(promoted, Some(joiner_peer));
                approved = true;
            }
            if matches!(joiner.pairing.join_status(&join.operation_id).unwrap(), PairingJoinStatus::Completed) {
                break;
            }
        }
        assert!(relayed_connection);
        assert!(approved);
        assert!(matches!(inviter.pairing.open_status(&open.operation_id).unwrap(), PairingOpenStatus::Completed));
        assert!(joiner.membership.allows(inviter_peer));
        assert!(inviter.membership.allows(joiner_peer));
        assert_eq!(joiner.routing.len(), KADEMLIA_ROUTING_PEER_CAPACITY);
        assert!(!joiner.membership.allows(relay_peer));
        assert!(!inviter.membership.allows(relay_peer));
        assert!(!joiner.forwarder.is_configured_transport_peer(relay_peer));
        assert!(!inviter.forwarder.is_configured_transport_peer(relay_peer));
        assert!(!joiner.pairing.uses_pairing_relay(relay_peer));
        for fixture in [&inviter, &joiner] {
            let store = fixture.pairing_store.as_ref().unwrap();
            let restored = CodePairingSessions::restore_persisted(&store.load().unwrap().unwrap(),
                "lab", current_unix_seconds_lossy(), Instant::now()).unwrap();
            assert!(!restored.uses_pairing_relay(relay_peer));
            if fixture.node.local_peer_id == inviter_peer {
                assert!(matches!(restored.open_status(&open.operation_id).unwrap(), PairingOpenStatus::Completed));
            } else {
                assert!(matches!(restored.join_status(&join.operation_id).unwrap(), PairingJoinStatus::Completed));
            }
            std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
        }
    })).await.expect("relay-backed code pairing timed out");
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
            fixture
                .auto_relay
                .reservation_listeners
                .get(&relay)
                .map(|owner| owner.id),
            Some(current)
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
    assert!(fixture.auto_relay.record_reservation_listener(
        relay,
        current,
        ConnectionId::new_unchecked(0)
    ));
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
        !fixture
            .auto_relay
            .accepted_reservation_peers
            .contains(&relay),
        "a cached listener event still lacks the native reservation owner"
    );
    assert_eq!(
        fixture.auto_relay.pending_reservations[&relay].expires_at,
        deadline
    );
    assert!(fixture.readiness.relay_ready(relay));
    assert!(
        incoming_anchor::confirmed_connection(
            &fixture.node.swarm,
            &fixture.epochs,
            &fixture.active_connections,
            relay,
            ConnectionId::new_unchecked(0)
        )
        .is_none()
    );
    expire_pending_auto_relay_reservations(
        &mut fixture.node.swarm,
        &mut fixture.auto_relay,
        &fixture.metrics,
        &mut fixture.epochs,
        deadline,
    );
    assert!(fixture.auto_relay.pending_reservations.is_empty());
    assert!(
        !fixture
            .auto_relay
            .accepted_reservation_peers
            .contains(&relay)
    );
    assert_eq!(
        fixture.auto_relay.retry_after[&relay],
        deadline + fixture.auto_relay.policy.retry_interval()
    );
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
async fn checkpoint_control_only_connection_callback_does_not_install_a_packet_path() {
    use crate::membership::checkpoint::{
        CheckpointMember, CooperativeMembershipState, NetworkAnchor, NetworkCapability,
        SnapshotPolicy,
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut fixture = EventFixture::new(Libp2pPeerId::random());
        let local = &fixture.node.identity;
        let remote = loop {
            let remote = NodeIdentity::generate_ed25519().unwrap();
            if crate::route::builtin_ipv4(PeerId::from_libp2p(remote.peer_id.parse().unwrap()))
                != crate::route::builtin_ipv4(PeerId::from_libp2p(local.peer_id.parse().unwrap())) {
                break remote;
            }
        };
        let peer = remote.peer_id.parse().unwrap();
        let anchor = NetworkAnchor::new([92; 32]).unwrap();
        let capability = NetworkCapability::from_secret(anchor.clone(), Some(&[7; 32])).unwrap();
        let wall = current_unix_seconds_lossy();
        let state = CooperativeMembershipState::bootstrap_at(
            capability.clone(), local.peer_id.clone(),
            vec![CheckpointMember::new(local).unwrap(), CheckpointMember::new(&remote).unwrap()],
            SnapshotPolicy::default(), wall,
        ).unwrap();
        let returning = CooperativeMembershipState::restore(capability, local.peer_id.clone(), state.retained()).unwrap();
        fixture.forwarder = Forwarder::from_checkpoint_config(&config_with_peer(local, peer), &returning, &anchor, wall).unwrap();
        fixture.membership = OverlayMembership::from_transport_peers(fixture.forwarder.config(),
            fixture.forwarder.configured_transport_peers()).unwrap();
        fixture.membership.checkpoint_sync_peers.insert(peer);
        let mut receiver = membership_sync_test_node(remote);
        receiver.swarm.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
        let address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = receiver.swarm.select_next_some().await { break address; }
        };
        fixture.epochs.dial(&mut fixture.node.swarm,
            DialOpts::peer_id(peer).addresses(vec![address]).build()).unwrap();
        let mut received = 0;
        loop {
            tokio::select! {
                event = fixture.node.swarm.select_next_some() => fixture.dispatch(event).await,
                event = receiver.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                        message: Message::Request { request: ControlRequest::Capabilities(_), channel, .. }, ..
                    })) = event {
                        received += 1;
                        receiver.swarm.behaviour_mut().control.send_response(channel,
                            ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("lab", None, 1280))).unwrap();
                    }
                },
            }
            if received > 0 && fixture.membership_record_syncs.pending.is_empty() { break; }
        }
        assert_eq!(received, 1);
        assert_eq!(fixture.metrics.snapshot(crate::queue::QueueStats::default()).control_requests_sent, 1);
        assert!(!fixture.capabilities.contains(PeerId::from_libp2p(peer)));
        assert!(fixture.paths.best_for(PeerId::from_libp2p(peer)).is_none());
        assert!(fixture.forwarder.authorized_routes().is_empty());
        assert!(fixture.forwarder.application_demand(PeerId::from_libp2p(peer)).is_none());
    }).await.expect("root control-only connection callback");
}

#[tokio::test]
async fn native_connection_event_uses_one_owned_capability_request() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let remote = NodeIdentity::generate_ed25519().unwrap();
        let peer = remote.peer_id.parse().unwrap();
        let mut fixture = EventFixture::new(peer);
        let before = fixture.forwarder.application_demand(PeerId::from_libp2p(peer));
        let mut receiver = membership_sync_test_node(remote);
        receiver.swarm.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
        let address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = receiver.swarm.select_next_some().await { break address; }
        };
        fixture.epochs.dial(&mut fixture.node.swarm,
            DialOpts::peer_id(peer).addresses(vec![address]).build()).unwrap();
        let mut received = 0;
        loop {
            tokio::select! {
                event = fixture.node.swarm.select_next_some() => fixture.dispatch(event).await,
                event = receiver.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                        message: Message::Request { request: ControlRequest::Capabilities(_), channel, .. }, ..
                    })) = event {
                        received += 1;
                        receiver.swarm.behaviour_mut().control.send_response(channel,
                            ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("lab", None, 1280))).unwrap();
                    }
                },
            }
            if received > 0 && fixture.capabilities.contains(PeerId::from_libp2p(peer))
                && fixture.membership_record_syncs.pending.is_empty()
            { break; }
        }
        assert_eq!(received, 1);
        assert_eq!(fixture.metrics.snapshot(crate::queue::QueueStats::default()).control_requests_sent, 1);
        assert_eq!(fixture.forwarder.application_demand(PeerId::from_libp2p(peer)), before);
        assert!(fixture.membership_record_syncs.active_by_peer.is_empty());
    }).await.expect("root connection callback must coalesce its capability request");
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
    use libp2p::swarm::{FromSwarm, NetworkBehaviour};
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
            let _handler = fixture
                .node
                .swarm
                .behaviour_mut()
                .control
                .handle_established_outbound_connection(
                    id,
                    peer,
                    "/ip4/127.0.0.1/tcp/4001"
                        .parse::<Multiaddr>()
                        .as_ref()
                        .unwrap(),
                    libp2p::core::Endpoint::Dialer,
                    libp2p::core::transport::PortUse::New,
                )
                .unwrap();
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
                    remaining_established: usize::from(scenario != "no_replacement"),
                },
            ));
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
        assert!(fixture.auto_relay.record_reservation_listener(
            relay,
            pending,
            ConnectionId::new_unchecked(0)
        ));
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
            fixture
                .auto_relay
                .reservation_listeners
                .get(&relay)
                .map(|owner| owner.id),
            Some(pending),
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
            PacketPlaneQuicBinding::from_bytes([7; 32]),
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
            PacketPlaneQuicBinding::from_bytes([9; 32]),
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
        PacketPlaneQuicBinding::from_bytes([11; 32]),
    );
    assert!(negotiator.quic_connection_task_handles[&(peer, role)].generation > generation);
    negotiator.clear();
    tokio::time::timeout(Duration::from_secs(1), async {
        while negotiator.quic_connection_tasks.join_next().await.is_some() {}
    })
    .await
    .unwrap();
}
