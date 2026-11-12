use super::*;
use libp2p::SwarmBuilder;

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
        let identity = NodeIdentity::generate_ed25519().unwrap();
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
        }
    }

    fn extra_peer(&mut self) -> Libp2pPeerId {
        let peer = Libp2pPeerId::random();
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
                [(
                    libp2p::StreamProtocol::new(name),
                    request_response::ProtocolSupport::Full,
                )],
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
    let mut a = wire_swarm();
    let mut b = wire_swarm_with_protocol(receiver_protocol);
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
        a_owner
            .offer_idle(a.behaviour_mut(), &a_context.context(), b_peer, idle)
            .unwrap();
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
        let wake = idle + Duration::from_secs(1);
        b_owner
            .wake(b.behaviour_mut(), &b_context.context(), a_peer, wake)
            .unwrap();
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
        fixture.readiness = lifecycle_not_ready;
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
