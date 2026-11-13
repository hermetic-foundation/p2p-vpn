use super::*;
use libp2p::SwarmBuilder;

#[path = "tests/legacy.rs"]
mod legacy;

fn epoch(value: u8) -> Epoch {
    Epoch::new([value; 16]).unwrap()
}

fn ready(_: Libp2pPeerId) -> Readiness {
    Readiness {
        incoming_path_available: true,
        outstanding_work: false,
    }
}

fn usable(_: Libp2pPeerId) -> bool {
    true
}

struct Fixture {
    forwarder: Forwarder,
    local: ControlCapabilities,
    peers: PeerCapabilities,
    connections: ConnectionEpochs,
    readiness: fn(Libp2pPeerId) -> Readiness,
}

impl Fixture {
    fn new(peer: Libp2pPeerId, local_epoch: u8, remote_epoch: u8) -> Self {
        let address = crate::route::builtin_ipv4(PeerId::from_libp2p(peer));
        let identity = std::iter::repeat_with(|| NodeIdentity::generate_ed25519().unwrap())
            .take(1024)
            .find(|identity| {
                crate::route::builtin_ipv4(PeerId::from_libp2p(identity.peer_id.parse().unwrap()))
                    != address
            })
            .expect("fixture local address must differ from the remote address");
        let config: Config = serde_json::from_value(serde_json::json!({
            "network": {"name":"demand-test", "private_key":identity.private_key},
            "peers":[{"id":peer.to_string()}],
        }))
        .unwrap();
        let mut peers = PeerCapabilities::default();
        peers.record(
            PeerId::from_libp2p(peer),
            ControlCapabilities::local("demand-test", None, 1280)
                .with_demand_lifecycle_epoch(epoch(remote_epoch)),
        );
        Self {
            forwarder: Forwarder::from_config(&config).unwrap(),
            local: ControlCapabilities::local("demand-test", None, 1280)
                .with_demand_lifecycle_epoch(epoch(local_epoch)),
            peers,
            connections: ConnectionEpochs::default(),
            readiness: ready,
        }
    }

    fn context(&self) -> LifecycleContext<'_> {
        LifecycleContext {
            forwarder: &self.forwarder,
            local: &self.local,
            peers: &self.peers,
            previous_tags: &[],
            connections: &self.connections,
            readiness: &self.readiness,
            usable_path: &usable,
            application_work: &|_| false,
        }
    }

    fn extra_peer(&mut self) -> Libp2pPeerId {
        self.extra_peer_from(std::iter::repeat_with(Libp2pPeerId::random).take(1024))
    }

    fn extra_peer_from(
        &mut self,
        candidates: impl IntoIterator<Item = Libp2pPeerId>,
    ) -> Libp2pPeerId {
        let occupied = std::iter::once(self.forwarder.config().local_peer_id().unwrap())
            .chain(self.forwarder.configured_overlay_peers())
            .map(crate::route::builtin_ipv4)
            .collect::<HashSet<_>>();
        let peer = candidates
            .into_iter()
            .find(|peer| {
                !occupied.contains(&crate::route::builtin_ipv4(PeerId::from_libp2p(*peer)))
            })
            .expect("fixture needs an unused derived IPv4 address");
        let mut config = serde_json::to_value(self.forwarder.config()).unwrap();
        config["peers"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"id":peer.to_string()}));
        self.forwarder = Forwarder::from_config(&serde_json::from_value(config).unwrap()).unwrap();
        self.peers.record(
            PeerId::from_libp2p(peer),
            ControlCapabilities::local("demand-test", None, 1280)
                .with_demand_lifecycle_epoch(epoch(2)),
        );
        peer
    }
}

fn wire_swarm() -> Swarm<WireBehaviour> {
    wire_swarm_with_protocol(protocol::PROTOCOL)
}

fn wire_swarm_with_protocol(name: &'static str) -> Swarm<WireBehaviour> {
    wire_swarm_with_support(name, request_response::ProtocolSupport::Full)
}

fn wire_swarm_with_support(
    name: &'static str,
    support: request_response::ProtocolSupport,
) -> Swarm<WireBehaviour> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .unwrap()
        .with_quic()
        .with_behaviour(move |_| {
            request_response::Behaviour::with_codec(
                LifecycleCodec,
                [(libp2p::StreamProtocol::new(name), support)],
                request_response::Config::default()
                    .with_request_timeout(protocol::REQUEST_TIMEOUT)
                    .with_max_concurrent_streams(8),
            )
        })
        .unwrap()
        .build()
}

async fn connected(listen: &str) -> (Swarm<WireBehaviour>, Fixture, Swarm<WireBehaviour>, Fixture) {
    connected_protocol(listen, protocol::PROTOCOL).await
}

async fn connected_protocol(
    listen: &str,
    receiver_protocol: &'static str,
) -> (Swarm<WireBehaviour>, Fixture, Swarm<WireBehaviour>, Fixture) {
    connected_with_support(
        listen,
        receiver_protocol,
        request_response::ProtocolSupport::Full,
    )
    .await
}

async fn connected_with_support(
    listen: &str,
    receiver_protocol: &'static str,
    support: request_response::ProtocolSupport,
) -> (Swarm<WireBehaviour>, Fixture, Swarm<WireBehaviour>, Fixture) {
    let mut a = wire_swarm();
    let mut b = wire_swarm_with_support(receiver_protocol, support);
    let mut a_context = Fixture::new(*b.local_peer_id(), 1, 2);
    let mut b_context = Fixture::new(*a.local_peer_id(), 2, 1);
    b.listen_on(listen.parse().unwrap()).unwrap();
    let address = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = b.select_next_some().await {
            break address;
        }
    };
    a.dial(address).unwrap();
    let mut a_connected = false;
    let mut b_connected = false;
    while !a_connected || !b_connected {
        tokio::select! {
            event = a.select_next_some() => if let SwarmEvent::ConnectionEstablished { peer_id, connection_id, .. } = event {
                assert_eq!(peer_id, *b.local_peer_id());
                a_context.connections.record_started(connection_id);
                a_context.connections.record_established(connection_id);
                a_connected = true;
            },
            event = b.select_next_some() => if let SwarmEvent::ConnectionEstablished { peer_id, connection_id, .. } = event {
                assert_eq!(peer_id, *a.local_peer_id());
                b_context.connections.record_started(connection_id);
                b_context.connections.record_established(connection_id);
                b_connected = true;
            },
        }
    }
    (a, a_context, b, b_context)
}

async fn idle_then_wake(listen: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, a_context, mut b, b_context) = connected(listen).await;
        let a_peer = *a.local_peer_id();
        let b_peer = *b.local_peer_id();
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut a_owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        let mut b_owner = DemandLifecycle::new(epoch(2), 2, 2, now);
        a_owner.observe_peer(&a_context.context(), b_peer, now);
        b_owner.observe_peer(&b_context.context(), a_peer, now);
        a_owner.drive(a.behaviour_mut(), &a_context.context(), idle);
        while a_owner.phase(b_peer) != Phase::Dormant || b_owner.phase(a_peer) != Phase::Dormant {
            tokio::select! {
                event = a.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    a_owner.handle_event(a.behaviour_mut(), &a_context.context(), event, idle);
                },
                event = b.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    b_owner.handle_event(b.behaviour_mut(), &b_context.context(), event, idle);
                },
            }
        }
        assert_eq!(a_owner.snapshot().outbound, 0);
        assert_eq!(b_owner.snapshot().inbound, 0);
        assert!(
            a_context
                .forwarder
                .data_path_is_dormant(PeerId::from_libp2p(b_peer))
        );
        assert!(
            b_context
                .forwarder
                .data_path_is_dormant(PeerId::from_libp2p(a_peer))
        );
        for step in 0..1000 {
            let tick = idle + Duration::from_secs(step * 5);
            a_owner.drive(a.behaviour_mut(), &a_context.context(), tick);
            b_owner.drive(b.behaviour_mut(), &b_context.context(), tick);
        }
        assert_eq!(a_owner.snapshot().outbound, 0);
        assert_eq!(b_owner.snapshot().outbound, 0);
        assert_eq!(
            a_owner.peers[&b_peer]
                .policy
                .as_ref()
                .unwrap()
                .last_demand(),
            None
        );
        assert_eq!(
            b_owner.peers[&a_peer]
                .policy
                .as_ref()
                .unwrap()
                .last_demand(),
            None
        );
        let wake = idle + Duration::from_secs(5001);
        b_context
            .forwarder
            .record_application_demand(PeerId::from_libp2p(a_peer), wake);
        let demand = b_context
            .forwarder
            .application_demand(PeerId::from_libp2p(a_peer))
            .unwrap();
        b_owner.drive(b.behaviour_mut(), &b_context.context(), wake);
        assert_eq!(
            b_context
                .forwarder
                .application_demand(PeerId::from_libp2p(a_peer)),
            Some(demand)
        );
        while a_owner.phase(b_peer) != Phase::Active || b_owner.snapshot().outbound != 0 {
            tokio::select! {
                event = a.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    a_owner.handle_event(a.behaviour_mut(), &a_context.context(), event, wake);
                },
                event = b.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    b_owner.handle_event(b.behaviour_mut(), &b_context.context(), event, wake);
                },
            }
        }
        assert_eq!(b_owner.phase(a_peer), Phase::Active);
        assert!(
            !a_context
                .forwarder
                .data_path_is_dormant(PeerId::from_libp2p(b_peer))
        );
        assert!(
            !b_context
                .forwarder
                .data_path_is_dormant(PeerId::from_libp2p(a_peer))
        );
    })
    .await
    .expect("bounded dispatcher exchange");
}

#[tokio::test]
async fn dispatcher_commits_and_wakes_over_authenticated_tcp() {
    idle_then_wake("/ip4/127.0.0.1/tcp/0").await;
}

#[tokio::test]
async fn dispatcher_commits_and_wakes_over_authenticated_quic() {
    idle_then_wake("/ip4/127.0.0.1/udp/0/quic-v1").await;
}

#[test]
fn unknown_peers_and_table_overflow_allocate_no_extra_records() {
    let peer = Libp2pPeerId::random();
    let mut fixture = Fixture::new(peer, 1, 2);
    let now = Instant::now();
    let mut owner = DemandLifecycle::new(epoch(1), 2, 1, now);
    for _ in 0..10_000 {
        assert!(!owner.admit_peer(&fixture.context(), Libp2pPeerId::random(), now));
    }
    assert_eq!(owner.snapshot().peers, 0);
    assert!(owner.admit_peer(&fixture.context(), peer, now));
    let second = fixture.extra_peer();
    assert!(owner.admit_peer(&fixture.context(), second, now));
    let third = fixture.extra_peer();
    assert!(!owner.admit_peer(&fixture.context(), third, now));
    assert_eq!(owner.snapshot().peers, 2);
}

#[test]
fn control_fixture_rejects_distinct_identities_with_colliding_derived_addresses() {
    let mut seen = HashMap::new();
    let (first, colliding) = (0..=65_536_u32)
        .find_map(|seed| {
            let mut secret = [0; 32];
            secret[..4].copy_from_slice(&seed.to_be_bytes());
            let peer = libp2p::identity::Keypair::ed25519_from_bytes(secret)
                .unwrap()
                .public()
                .to_peer_id();
            let address = crate::route::builtin_ipv4(PeerId::from_libp2p(peer));
            seen.insert(address, peer).map(|previous| (previous, peer))
        })
        .expect("more identities than available derived IPv4 addresses");
    assert_ne!(first, colliding);
    assert_eq!(
        crate::route::builtin_ipv4(PeerId::from_libp2p(first)),
        crate::route::builtin_ipv4(PeerId::from_libp2p(colliding))
    );
    let mut fixture = Fixture::new(first, 1, 2);
    let added = fixture.extra_peer_from(
        std::iter::once(colliding).chain(std::iter::repeat_with(Libp2pPeerId::random).take(1024)),
    );
    assert_ne!(added, colliding);
    assert_eq!(fixture.forwarder.config().peers.len(), 2);
    assert!(
        fixture
            .forwarder
            .config()
            .peers
            .iter()
            .all(|peer| peer.vpn_ip.is_none())
    );
}

#[test]
fn peer_and_network_budgets_share_both_directions_and_refill_monotonically() {
    let first = Libp2pPeerId::random();
    let mut fixture = Fixture::new(first, 1, 2);
    let now = Instant::now();
    let mut owner = DemandLifecycle::new(epoch(1), 20, 2, now);
    let mut peers = vec![first];
    for _ in 0..16 {
        peers.push(fixture.extra_peer());
    }
    let mut accepted = 0;
    for peer in &peers {
        assert!(owner.admit_peer(&fixture.context(), *peer, now));
        for _ in 0..PEER_REQUESTS_PER_SECOND {
            accepted += u32::from(owner.allow_request(*peer, now));
        }
        assert!(!owner.allow_request(*peer, now));
    }
    assert_eq!(accepted, NETWORK_REQUESTS_PER_SECOND);
    assert!(!owner.allow_request(first, now - Duration::from_secs(1)));
    assert!(owner.allow_request(first, now + Duration::from_secs(1)));
    assert_eq!(owner.snapshot().inbound, 0);
    assert_eq!(owner.snapshot().outbound, 0);
}

#[test]
fn epoch_refresh_does_not_refill_an_exhausted_peer_budget() {
    let peer = Libp2pPeerId::random();
    let mut fixture = Fixture::new(peer, 1, 2);
    let now = Instant::now();
    let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
    owner.observe_peer(&fixture.context(), peer, now);
    for _ in 0..PEER_REQUESTS_PER_SECOND {
        assert!(owner.allow_request(peer, now));
    }
    fixture.peers.record(
        PeerId::from_libp2p(peer),
        ControlCapabilities::local("demand-test", None, 1280).with_demand_lifecycle_epoch(epoch(3)),
    );
    owner.observe_peer(&fixture.context(), peer, now);
    assert!(!owner.allow_request(peer, now));
    assert_eq!(owner.peers[&peer].remote_epoch, Some(epoch(3)));
}

#[tokio::test]
async fn a_missing_control_path_never_creates_automatic_dial_work() {
    let peer = Libp2pPeerId::random();
    let fixture = Fixture::new(peer, 1, 2);
    let now = Instant::now();
    let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
    let mut swarm = wire_swarm();
    owner.observe_peer(&fixture.context(), peer, now);
    for _ in 0..1_000 {
        assert_eq!(
            owner.wake(swarm.behaviour_mut(), &fixture.context(), peer, now),
            Err(Rejection::IncomingPathUnavailable)
        );
    }
    assert_eq!(owner.snapshot().outbound, 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), swarm.select_next_some())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn semantic_cancellation_expiry_and_revocation_cannot_reuse_transport_slots() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, mut fixture, b, _) = connected("/ip4/127.0.0.1/tcp/0").await;
        let peer = *b.local_peer_id();
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut owner = DemandLifecycle::new(epoch(1), 2, 1, now);
        owner.observe_peer(&fixture.context(), peer, now);
        owner
            .offer_idle(a.behaviour_mut(), &fixture.context(), peer, idle)
            .unwrap();
        for _ in 0..1_000 {
            assert_eq!(
                owner.wake(a.behaviour_mut(), &fixture.context(), peer, idle),
                Err(Rejection::OutstandingWork)
            );
            assert_eq!(owner.snapshot().outbound, 1);
        }
        let later = idle + Duration::from_secs(100);
        owner.reconcile(&fixture.context(), later);
        assert!(owner.peers[&peer].outbound.is_none());
        assert_eq!(owner.snapshot().outbound, 1);
        fixture.peers.remove(PeerId::from_libp2p(peer));
        owner.reconcile(&fixture.context(), later);
        assert_eq!(owner.snapshot().peers, 0);
        assert_eq!(owner.snapshot().outbound, 1);
        drop(b);
        while owner.snapshot().outbound != 0 {
            if let SwarmEvent::Behaviour(event) = a.select_next_some().await {
                owner.handle_event(a.behaviour_mut(), &fixture.context(), event, later);
            }
        }
        assert_eq!(owner.snapshot().peers, 0);
    })
    .await
    .expect("terminal event releases retained transport slot");
}

#[derive(Clone, Copy, Debug)]
enum Interrupt {
    NewDemand,
    Revoked,
    RestartedPeer,
    NetworkChange,
    RetiringConnection,
    IncomingPathLost,
    NewOutstandingWork,
    MembershipRotation,
    AcceptedPayload,
}

async fn interrupted_reply(interrupt: Interrupt) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, a_fixture, mut b, mut b_fixture) = connected("/ip4/127.0.0.1/tcp/0").await;
        let a_peer = *a.local_peer_id();
        let b_peer = *b.local_peer_id();
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut a_owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        let mut b_owner = DemandLifecycle::new(epoch(2), 2, 2, now);
        a_owner.observe_peer(&a_fixture.context(), b_peer, now);
        b_owner.observe_peer(&b_fixture.context(), a_peer, now);
        a_owner.offer_idle(a.behaviour_mut(), &a_fixture.context(), b_peer, idle).unwrap();
        let mut interrupted = false;
        let mut sent = false;
        while !sent || a_owner.snapshot().outbound != 0 {
            tokio::select! {
                event = a.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    a_owner.handle_event(a.behaviour_mut(), &a_fixture.context(), event, idle);
                },
                event = b.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    let prepared = matches!(&event, WireEvent::Message { message: Message::Request { .. }, .. });
                    let delivery = matches!(&event, WireEvent::ResponseSent { .. });
                    b_owner.handle_event(b.behaviour_mut(), &b_fixture.context(), event, idle);
                    if prepared {
                        assert!(!interrupted);
                        interrupted = true;
                        assert_eq!(b_owner.phase(a_peer), Phase::Quiescing);
                        match interrupt {
                            Interrupt::NewDemand => b_owner.demand(&b_fixture.context(), a_peer, idle),
                            Interrupt::Revoked => {
                                let mut config = b_fixture.forwarder.config().clone();
                                config.peers.clear();
                                b_fixture.forwarder = Forwarder::from_config(&config).unwrap();
                            }
                            Interrupt::RestartedPeer => b_fixture.peers.record(PeerId::from_libp2p(a_peer), ControlCapabilities::local("demand-test", None, 1280).with_demand_lifecycle_epoch(epoch(3))),
                            Interrupt::NetworkChange => { b_fixture.connections.advance(); }
                            Interrupt::RetiringConnection => {
                                let connection = b_owner.peers[&a_peer].inbound.as_ref().unwrap().connection;
                                b_fixture.connections.mark_retiring(connection);
                            }
                            Interrupt::IncomingPathLost => b_fixture.readiness = |_| Readiness { incoming_path_available: false, outstanding_work: false },
                            Interrupt::NewOutstandingWork => b_fixture.readiness = |_| Readiness { incoming_path_available: true, outstanding_work: true },
                            Interrupt::MembershipRotation => {
                                b_fixture.local.membership_tag = Some("rotated".into());
                                b_fixture.peers.record(PeerId::from_libp2p(a_peer), ControlCapabilities::local("demand-test", Some("rotated".into()), 1280).with_demand_lifecycle_epoch(epoch(1)));
                            }
                            Interrupt::AcceptedPayload => {
                                let source = crate::route::builtin_ipv4(PeerId::from_libp2p(a_peer));
                                let target = crate::route::builtin_ipv4(b_fixture.forwarder.config().local_peer_id().unwrap());
                                let mut bytes = vec![0; 20];
                                bytes[0] = 0x45;
                                bytes[12..16].copy_from_slice(&source.octets());
                                bytes[16..20].copy_from_slice(&target.octets());
                                let frame = Frame::packet(100, 1, bytes).unwrap();
                                b_fixture.forwarder.accept_inbound_stream_packet(a_peer, &frame).unwrap();
                            }
                        }
                    }
                    if delivery { sent = true; }
                },
            }
        }
        assert!(interrupted);
        assert_ne!(b_owner.phase(a_peer), Phase::Dormant, "{interrupt:?}");
        assert_eq!(b_owner.snapshot().inbound, 0);
        assert!(!b_fixture.forwarder.data_path_is_dormant(PeerId::from_libp2p(a_peer)));
    }).await.expect("interrupted reply finishes without committing quiet");
}

#[tokio::test]
async fn every_destructive_commit_rechecks_current_authority_epoch_work_and_connection() {
    for interrupt in [
        Interrupt::NewDemand,
        Interrupt::Revoked,
        Interrupt::RestartedPeer,
        Interrupt::NetworkChange,
        Interrupt::RetiringConnection,
        Interrupt::IncomingPathLost,
        Interrupt::NewOutstandingWork,
        Interrupt::MembershipRotation,
        Interrupt::AcceptedPayload,
    ] {
        interrupted_reply(interrupt).await;
    }
}

#[tokio::test]
async fn unadvertised_support_rejects_quiet_and_disables_retry_for_the_epoch() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, a_fixture, mut b, mut b_fixture) = connected("/ip4/127.0.0.1/tcp/0").await;
        b_fixture.local = ControlCapabilities::local("demand-test", None, 1280);
        let a_peer = *a.local_peer_id();
        let b_peer = *b.local_peer_id();
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut a_owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        let mut b_owner = DemandLifecycle::new(epoch(2), 2, 2, now);
        a_owner.observe_peer(&a_fixture.context(), b_peer, now);
        a_owner
            .offer_idle(a.behaviour_mut(), &a_fixture.context(), b_peer, idle)
            .unwrap();
        while a_owner.snapshot().outbound != 0 {
            tokio::select! {
                event = a.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    a_owner.handle_event(a.behaviour_mut(), &a_fixture.context(), event, idle);
                },
                event = b.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    b_owner.handle_event(b.behaviour_mut(), &b_fixture.context(), event, idle);
                },
            }
        }
        assert_eq!(a_owner.phase(b_peer), Phase::Legacy);
        assert_eq!(b_owner.phase(a_peer), Phase::Legacy);
        for tick in 1..=100 {
            assert_eq!(
                a_owner.offer_idle(
                    a.behaviour_mut(),
                    &a_fixture.context(),
                    b_peer,
                    idle + Duration::from_secs(tick)
                ),
                Err(Rejection::Unsupported)
            );
        }
        assert_eq!(a_owner.snapshot().outbound, 0);
    })
    .await
    .expect("unadvertised runtime keeps compatible behavior");
}

#[tokio::test]
async fn actual_unsupported_protocol_is_not_retried_by_capability_refresh() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, fixture, mut b, _) = connected_protocol("/ip4/127.0.0.1/tcp/0", "/test/not-lifecycle/1").await;
        let peer = *b.local_peer_id();
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        owner.observe_peer(&fixture.context(), peer, now);
        owner.offer_idle(a.behaviour_mut(), &fixture.context(), peer, idle).unwrap();
        let mut unsupported = false;
        while owner.snapshot().outbound != 0 {
            tokio::select! {
                event = a.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    unsupported |= matches!(&event, WireEvent::OutboundFailure { error: request_response::OutboundFailure::UnsupportedProtocols, .. });
                    owner.handle_event(a.behaviour_mut(), &fixture.context(), event, idle);
                },
                _ = b.select_next_some() => {},
            }
        }
        assert!(unsupported);
        for tick in 1..=100 {
            let later = idle + Duration::from_secs(tick);
            owner.observe_peer(&fixture.context(), peer, later);
            assert_eq!(owner.offer_idle(a.behaviour_mut(), &fixture.context(), peer, later), Err(Rejection::Unsupported));
        }
        assert_eq!(owner.phase(peer), Phase::Legacy);
        assert_eq!(owner.snapshot().outbound, 0);
    }).await.expect("bounded unsupported-protocol dispatcher fallback");
}

fn raw_idle(revision: u64) -> Request {
    Request::idle(
        protocol::Scope {
            network_name: "demand-test".into(),
            membership_tag: None,
        },
        crate::runtime::lifecycle::IdleOffer {
            sender_epoch: epoch(1),
            receiver_epoch: epoch(2),
            revision,
        },
    )
}

#[tokio::test]
async fn excess_requests_are_dropped_without_response_amplification() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, _, mut b, mut fixture) = connected("/ip4/127.0.0.1/tcp/0").await;
        let peer = *a.local_peer_id();
        fixture.readiness = |_| Readiness {
            incoming_path_available: false,
            outstanding_work: true,
        };
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut owner = DemandLifecycle::new(epoch(2), 2, 2, now);
        owner.observe_peer(&fixture.context(), peer, now);
        for revision in 1..=u64::from(PEER_REQUESTS_PER_SECOND) + 1 {
            a.behaviour_mut().send_request(b.local_peer_id(), raw_idle(revision));
        }
        let mut responses = 0;
        let mut failures = 0;
        while responses + failures < PEER_REQUESTS_PER_SECOND + 1 {
            tokio::select! {
                event = b.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    owner.handle_event(b.behaviour_mut(), &fixture.context(), event, idle);
                },
                event = a.select_next_some() => match event {
                    SwarmEvent::Behaviour(WireEvent::Message { message: Message::Response { response, .. }, .. }) => {
                        assert!(matches!(response.reply, Reply::Rejected { .. }));
                        responses += 1;
                    }
                    SwarmEvent::Behaviour(WireEvent::OutboundFailure { .. }) => failures += 1,
                    _ => {},
                },
            }
        }
        assert_eq!(responses, PEER_REQUESTS_PER_SECOND);
        assert_eq!(failures, 1);
        assert_eq!(owner.snapshot().rate_limited, 1);
        assert_eq!(owner.snapshot().peers, 1);
        assert_eq!(owner.snapshot().inbound, 0);
        assert_ne!(owner.phase(peer), Phase::Dormant);
    }).await.expect("excess request receives no extra reply");
}

#[tokio::test]
async fn unauthorized_transport_peer_gets_no_reply_or_retained_record() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, _, mut b, mut fixture) = connected("/ip4/127.0.0.1/tcp/0").await;
        let mut config = fixture.forwarder.config().clone();
        config.peers.clear();
        fixture.forwarder = Forwarder::from_config(&config).unwrap();
        let now = Instant::now();
        let mut owner = DemandLifecycle::new(epoch(2), 2, 2, now);
        a.behaviour_mut().send_request(b.local_peer_id(), raw_idle(1));
        loop {
            tokio::select! {
                event = b.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    owner.handle_event(b.behaviour_mut(), &fixture.context(), event, now);
                },
                event = a.select_next_some() => match event {
                    SwarmEvent::Behaviour(WireEvent::OutboundFailure { .. }) => break,
                    SwarmEvent::Behaviour(WireEvent::Message { message: Message::Response { .. }, .. }) => panic!("unauthorized request received a reply"),
                    _ => {},
                },
            }
        }
        assert_eq!(owner.snapshot().dropped, 1);
        assert_eq!(owner.snapshot().peers, 0);
    }).await.expect("unauthorized peer is dropped before state admission");
}

#[tokio::test]
async fn automatic_driver_never_dials_or_renews_demand_without_a_control_path() {
    for application in [false, true] {
        let peer = Libp2pPeerId::random();
        let fixture = Fixture::new(peer, 1, 2);
        let mut wire = wire_swarm();
        let now = Instant::now();
        let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        let no_path = |_| false;
        let mut context = fixture.context();
        context.usable_path = &no_path;
        owner.observe_peer(&context, peer, now);
        if application {
            fixture
                .forwarder
                .record_application_demand(PeerId::from_libp2p(peer), now);
        }
        let before = fixture
            .forwarder
            .application_demand(PeerId::from_libp2p(peer));
        for step in 0..1000 {
            owner.drive(
                wire.behaviour_mut(),
                &context,
                now + Duration::from_secs(step),
            );
            assert_eq!(owner.snapshot().outbound, 0);
            assert!(!wire.behaviour().has_any_pending_requests());
            assert_eq!(wire.network_info().connection_counters().num_pending(), 0);
        }
        assert_eq!(
            fixture
                .forwarder
                .application_demand(PeerId::from_libp2p(peer)),
            before
        );
        assert_eq!(owner.peers[&peer].wake_after, None);
    }
}

#[tokio::test]
async fn preparation_drains_busy_maintenance_without_granting_quiet() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, mut fixture, mut b, remote) = connected("/ip4/127.0.0.1/tcp/0").await;
        let peer = *b.local_peer_id();
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        let mut responder = DemandLifecycle::new(epoch(2), 2, 2, now);
        owner.observe_peer(&fixture.context(), peer, now);
        responder.observe_peer(&remote.context(), *a.local_peer_id(), now);
        let overlay = PeerId::from_libp2p(peer);
        let before = fixture.forwarder.application_demand(overlay);
        fixture.readiness = |_| Readiness {
            incoming_path_available: true,
            outstanding_work: true,
        };
        for step in 0..1000 {
            owner.drive(
                a.behaviour_mut(),
                &fixture.context(),
                idle + Duration::from_secs(step * 5),
            );
            assert!(fixture.forwarder.data_path_maintenance_is_paused(overlay));
            assert!(!fixture.forwarder.data_path_is_dormant(overlay));
            assert_eq!(owner.snapshot().outbound, 0);
        }
        assert_eq!(fixture.forwarder.application_demand(overlay), before);
        fixture.readiness = ready;
        let drained = idle + Duration::from_secs(5001);
        owner.drive(a.behaviour_mut(), &fixture.context(), drained);
        assert_eq!(owner.snapshot().outbound, 1);
        while owner.phase(peer) != Phase::Dormant
            || responder.phase(*a.local_peer_id()) != Phase::Dormant
        {
            tokio::select! {
                event = a.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    owner.handle_event(a.behaviour_mut(), &fixture.context(), event, drained);
                },
                event = b.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    responder.handle_event(b.behaviour_mut(), &remote.context(), event, drained);
                },
            }
        }
        assert!(fixture.forwarder.data_path_is_dormant(overlay));
        assert_eq!(fixture.forwarder.application_demand(overlay), before);
    })
    .await
    .expect("maintenance drain followed by native idle exchange");
}

#[tokio::test]
async fn preparation_requires_support_anchor_idle_age_and_empty_application_work() {
    for blocked in ["legacy", "anchor", "age", "application"] {
        let peer = Libp2pPeerId::random();
        let mut fixture = Fixture::new(peer, 1, 2);
        let mut wire = wire_swarm();
        let now = Instant::now();
        let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        owner.observe_peer(&fixture.context(), peer, now);
        if blocked == "legacy" {
            fixture.local.supports_demand_lifecycle = false;
        }
        if blocked == "anchor" {
            fixture.readiness = |_| Readiness {
                incoming_path_available: false,
                outstanding_work: false,
            };
        }
        let work = |_| blocked == "application";
        let mut context = fixture.context();
        context.application_work = &work;
        let at = now + IDLE_AFTER - Duration::from_secs(u64::from(blocked == "age"));
        owner.drive(wire.behaviour_mut(), &context, at);
        assert!(
            !fixture
                .forwarder
                .data_path_maintenance_is_paused(PeerId::from_libp2p(peer)),
            "{blocked}"
        );
        assert!(
            !fixture
                .forwarder
                .data_path_is_dormant(PeerId::from_libp2p(peer))
        );
        assert_eq!(owner.snapshot().outbound, 0);
    }
}

#[tokio::test]
async fn capability_invalidation_clears_local_preparation_without_activity() {
    let peer = Libp2pPeerId::random();
    let mut fixture = Fixture::new(peer, 1, 2);
    let now = Instant::now();
    let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
    let mut wire = wire_swarm();
    owner.observe_peer(&fixture.context(), peer, now);
    owner.drive(wire.behaviour_mut(), &fixture.context(), now + IDLE_AFTER);
    let overlay = PeerId::from_libp2p(peer);
    assert!(fixture.forwarder.data_path_maintenance_is_paused(overlay));
    let before = fixture.forwarder.application_demand(overlay);
    fixture.peers.record(
        overlay,
        ControlCapabilities::local("demand-test", None, 1280),
    );
    owner.drive(
        wire.behaviour_mut(),
        &fixture.context(),
        now + IDLE_AFTER + Duration::from_secs(1),
    );
    assert_eq!(owner.phase(peer), Phase::Legacy);
    assert!(!fixture.forwarder.data_path_maintenance_is_paused(overlay));
    assert_eq!(fixture.forwarder.application_demand(overlay), before);
}

#[test]
fn underlay_control_work_does_not_fabricate_demand_for_a_dormant_peer() {
    let peer = Libp2pPeerId::random();
    let mut fixture = Fixture::new(peer, 1, 2);
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
    owner.observe_peer(&fixture.context(), peer, now);
    let state = owner.peers.get_mut(&peer).unwrap();
    let policy = state.policy.as_mut().unwrap();
    let offer = policy.offer_idle(idle, ready(peer)).unwrap();
    policy
        .receive_idle_ack(
            IdleAck {
                offer,
                responder_revision: 0,
            },
            idle,
            ready(peer),
        )
        .unwrap();
    state.commit_quiet(&fixture.context(), peer, idle);
    assert_eq!(owner.phase(peer), Phase::Dormant);
    let before = fixture
        .forwarder
        .application_demand(PeerId::from_libp2p(peer));
    fixture.connections.advance();
    fixture.readiness = |_| Readiness {
        incoming_path_available: false,
        outstanding_work: true,
    };
    owner.reconcile(&fixture.context(), idle + Duration::from_secs(1));
    assert_eq!(owner.phase(peer), Phase::Dormant);
    assert!(
        fixture
            .forwarder
            .data_path_is_dormant(PeerId::from_libp2p(peer))
    );
    assert_eq!(
        fixture
            .forwarder
            .application_demand(PeerId::from_libp2p(peer)),
        before
    );
    assert_eq!(
        owner.peers[&peer].policy.as_ref().unwrap().last_demand(),
        None
    );
}

struct RuntimeFixture {
    peer: Libp2pPeerId,
    base: Fixture,
    node: P2pNode,
    queues: PeerQueues,
    in_flight: PacketInFlight,
    paths: PathSet,
    connections: HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    relay_readiness: RelayReadiness,
    auto_relay: AutoRelayState,
}

impl RuntimeFixture {
    fn new() -> Self {
        let peer = Libp2pPeerId::random();
        let base = Fixture::new(peer, 1, 2);
        let mut node = super::super::tests::membership_sync_test_node(
            base.forwarder.config().identity().unwrap(),
        );
        let local = node.local_peer_id;
        node.bootstrap_peer_addresses.clear();
        node.swarm.behaviour_mut().kad = kad::Behaviour::with_config(
            local,
            kad::store::MemoryStore::new(local),
            crate::runtime::p2p::controlled_kademlia_config(libp2p::StreamProtocol::new(
                PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            )),
        );
        node.swarm.behaviour_mut().pairing_kad = None.into();
        node.swarm.behaviour_mut().autonat = None.into();
        Self {
            peer,
            base,
            node,
            queues: PeerQueues::new(4, 4096),
            in_flight: PacketInFlight::new(4),
            paths: PathSet::new(),
            connections: HashMap::new(),
            relay_readiness: RelayReadiness::default(),
            auto_relay: AutoRelayState::default(),
        }
    }

    fn with<R>(
        &mut self,
        action: impl FnOnce(&mut WireBehaviour, &LifecycleContext<'_>) -> R,
    ) -> R {
        RuntimeInputs {
            forwarder: &self.base.forwarder,
            local: &self.base.local,
            peers: &self.base.peers,
            previous_tags: &[],
            epochs: &self.base.connections,
            connections: &self.connections,
            relay_readiness: &self.relay_readiness,
            auto_relay: &self.auto_relay,
            queues: &self.queues,
            in_flight: &self.in_flight,
            paths: &self.paths,
            udp: None,
            quic: None,
        }
        .with(&mut self.node, action)
    }

    fn readiness(&mut self) -> Readiness {
        let peer = self.peer;
        self.with(|_, context| (context.readiness)(peer))
    }
}

#[tokio::test]
async fn runtime_readiness_exempts_only_its_own_lifecycle_exchange() {
    let mut f = RuntimeFixture::new();
    assert_eq!(
        f.readiness(),
        Readiness {
            incoming_path_available: false,
            outstanding_work: false
        }
    );
    let peer = f.peer;
    let before = f
        .base
        .forwarder
        .application_demand(PeerId::from_libp2p(peer));
    f.with(|wire, context| {
        wire.send_request(&peer, raw_idle(1));
        assert!(!(context.readiness)(peer).outstanding_work);
    });
    assert!(
        f.node.swarm.behaviour().has_pending_peer_work(peer),
        "physical retirement must still include the lifecycle wire"
    );
    f.node
        .swarm
        .behaviour_mut()
        .control
        .send_request(&peer, ControlRequest::Capabilities(f.base.local.clone()));
    assert!(f.readiness().outstanding_work);
    assert_eq!(
        f.base
            .forwarder
            .application_demand(PeerId::from_libp2p(peer)),
        before
    );
}

#[tokio::test]
async fn runtime_readiness_separates_application_queues_and_native_transports() {
    let mut f = RuntimeFixture::new();
    let peer = f.peer;
    let overlay = PeerId::from_libp2p(peer);
    let before = f.base.forwarder.application_demand(overlay);
    f.queues
        .enqueue(crate::queue::Packet::new(overlay, 1, vec![0x45; 20]))
        .unwrap();
    f.with(|_, context| {
        assert!((context.application_work)(peer));
        assert!((context.readiness)(peer).outstanding_work);
        assert!(!(context.readiness)(peer).incoming_path_available);
    });
    f.queues.dequeue().unwrap();
    assert!(!f.readiness().outstanding_work);
    f.node
        .swarm
        .dial(
            DialOpts::peer_id(peer)
                .addresses(vec!["/ip4/127.0.0.1/tcp/9".parse().unwrap()])
                .build(),
        )
        .unwrap();
    f.with(|_, context| {
        assert!(!(context.application_work)(peer));
        assert!((context.readiness)(peer).outstanding_work);
    });
    assert_eq!(f.base.forwarder.application_demand(overlay), before);
}

#[tokio::test]
async fn runtime_readiness_counts_payload_flights_but_not_probe_flights_as_application_work() {
    let mut f = RuntimeFixture::new();
    let peer = f.peer;
    let overlay = PeerId::from_libp2p(peer);
    let before = f.base.forwarder.application_demand(overlay);
    f.paths
        .record_established(overlay, PathKind::DirectTcpStream);
    let path = f.paths.candidates_for(overlay).next().unwrap();
    let mut source = wire_swarm();
    let probe =
        PacketInFlightId::RequestResponse(source.behaviour_mut().send_request(&peer, raw_idle(1)));
    f.in_flight.record_path_probe(overlay, probe, path);
    f.with(|_, context| {
        assert!(!(context.application_work)(peer));
        assert!(!(context.readiness)(peer).outstanding_work);
        assert!((context.usable_path)(peer));
    });
    let packet = crate::queue::Packet::new(overlay, 1, vec![0x45; 20]);
    let delivery =
        PacketInFlightId::RequestResponse(source.behaviour_mut().send_request(&peer, raw_idle(2)));
    f.in_flight.record(&packet, delivery, path);
    f.with(|_, context| {
        assert!((context.application_work)(peer));
        assert!((context.readiness)(peer).outstanding_work);
    });
    f.in_flight.complete(delivery);
    assert!(!f.readiness().outstanding_work);
    f.in_flight.complete(probe);
    assert_eq!(f.base.forwarder.application_demand(overlay), before);
}

#[tokio::test]
async fn native_routing_work_protects_idle_admission_and_stream_retirement() {
    for secondary in [false, true] {
        let mut f = RuntimeFixture::new();
        let local = f.node.local_peer_id;
        if secondary {
            f.node.swarm.behaviour_mut().pairing_kad = Some(kad::Behaviour::with_config(
                local,
                kad::store::MemoryStore::new(local),
                crate::runtime::p2p::controlled_kademlia_config(libp2p::StreamProtocol::new(
                    PUBLIC_IPFS_KADEMLIA_PROTOCOL,
                )),
            ))
            .into();
        }
        let behaviour = f.node.swarm.behaviour_mut();
        let kad = if secondary {
            behaviour.pairing_kad.as_mut().unwrap()
        } else {
            &mut behaviour.kad
        };
        let query = kad.get_closest_peers(Libp2pPeerId::random());
        assert!(f.readiness().outstanding_work);
        assert!(f.node.swarm.behaviour().has_pending_peer_work(f.peer));
        let behaviour = f.node.swarm.behaviour_mut();
        let kad = if secondary {
            behaviour.pairing_kad.as_mut().unwrap()
        } else {
            &mut behaviour.kad
        };
        kad.cancel_query(&query);
        assert!(!f.readiness().outstanding_work);
        assert!(!f.node.swarm.behaviour().has_pending_peer_work(f.peer));
    }
}

#[tokio::test]
async fn queued_disconnected_work_protects_an_unselected_circuit_underlay() {
    let mut f = RuntimeFixture::new();
    let before = f
        .base
        .forwarder
        .application_demand(PeerId::from_libp2p(f.peer));
    f.node.swarm.behaviour_mut().control.send_request(
        &Libp2pPeerId::random(),
        ControlRequest::Capabilities(f.base.local.clone()),
    );
    assert_eq!(
        f.node
            .swarm
            .network_info()
            .connection_counters()
            .num_pending(),
        0
    );
    assert!(f.readiness().outstanding_work);
    assert!(f.node.swarm.behaviour().has_pending_peer_work(f.peer));
    assert_eq!(
        f.base
            .forwarder
            .application_demand(PeerId::from_libp2p(f.peer)),
        before
    );
}
