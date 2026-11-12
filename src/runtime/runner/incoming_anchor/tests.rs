use super::*;

fn address(peer: Libp2pPeerId, transport: &str) -> Multiaddr {
    format!("/ip4/127.0.0.1/{transport}/p2p/{peer}")
        .parse()
        .unwrap()
}

fn endpoint(address: Multiaddr) -> ConnectedPoint {
    ConnectedPoint::Dialer {
        address,
        role_override: libp2p::core::Endpoint::Dialer,
        port_use: libp2p::core::transport::PortUse::New,
    }
}

#[test]
fn connection_selection_requires_current_established_direct_host_and_transport() {
    let peer = Libp2pPeerId::random();
    let tcp = address(peer, "tcp/4001");
    let pending = PendingAutoRelayReservation {
        address: tcp.clone(),
        expires_at: Instant::now(),
        owned_dial: None,
    };
    let mut epochs = ConnectionEpochs::default();
    let mut connections = HashMap::new();
    for (index, scenario) in [
        "stale",
        "retiring",
        "quic",
        "relayed",
        "wrong_peer",
        "unestablished",
        "wrong_host",
        "usable",
    ]
    .into_iter()
    .enumerate()
    {
        let id = ConnectionId::new_unchecked(index);
        epochs.record_started(id);
        if scenario != "unestablished" {
            assert!(epochs.record_established(id));
        }
        if scenario == "stale" {
            epochs.advance();
        }
        if scenario == "retiring" {
            epochs.mark_retiring(id);
        }
        let address = match scenario {
            "quic" => address(peer, "udp/4001/quic-v1"),
            "relayed" => tcp
                .clone()
                .with(Protocol::P2pCircuit)
                .with(Protocol::P2p(Libp2pPeerId::random())),
            "wrong_host" => format!("/ip4/127.0.0.2/tcp/4001/p2p/{peer}")
                .parse()
                .unwrap(),
            _ => tcp.clone(),
        };
        let owner = if scenario == "wrong_peer" {
            Libp2pPeerId::random()
        } else {
            peer
        };
        connections.insert((owner, id), endpoint(address));
        let selected = matching_connection(peer, &pending, &epochs, &connections);
        assert_eq!(selected, (scenario == "usable").then_some(id), "{scenario}");
    }
}

#[test]
fn candidate_ranking_keeps_lan_before_wan_and_tcp_before_quic_within_locality() {
    let peer = Libp2pPeerId::random();
    let candidates = [
        format!("/ip4/8.8.8.8/udp/4001/quic-v1/p2p/{peer}"),
        format!("/ip4/192.168.51.2/udp/4001/quic-v1/p2p/{peer}"),
        format!("/ip4/8.8.8.8/tcp/4001/p2p/{peer}"),
        format!("/ip4/192.168.51.2/tcp/4001/p2p/{peer}"),
    ]
    .map(|address| address.parse::<Multiaddr>().unwrap());
    let networks = [LocalInterfaceNetwork {
        ip: "192.168.51.1".parse().unwrap(),
        netmask: "255.255.255.0".parse().unwrap(),
    }];
    assert_eq!(
        ranked_auto_relay_candidate_addresses(
            peer,
            &candidates,
            &networks,
            DiscoveredPeerAddressSource::LocalDiscovery
        ),
        [
            candidates[3].clone(),
            candidates[1].clone(),
            candidates[2].clone(),
            candidates[0].clone()
        ]
    );
}

#[test]
fn public_identify_cannot_replace_known_lan_with_wan_or_inject_private_addresses() {
    let peer = Libp2pPeerId::random();
    let known: Multiaddr = format!("/ip4/192.168.51.2/udp/4001/quic-v1/p2p/{peer}")
        .parse()
        .unwrap();
    let unverified: Multiaddr = format!("/ip4/192.168.51.3/tcp/4001/p2p/{peer}")
        .parse()
        .unwrap();
    let public: Multiaddr = format!("/ip4/8.8.8.8/tcp/4001/p2p/{peer}").parse().unwrap();
    let networks = [LocalInterfaceNetwork {
        ip: "192.168.51.1".parse().unwrap(),
        netmask: "255.255.255.0".parse().unwrap(),
    }];
    assert_eq!(
        candidate_addresses(
            peer,
            &[public.clone(), unverified],
            Some(&known),
            &networks,
            DiscoveredPeerAddressSource::PublicDiscovery
        ),
        [known]
    );
    let old_public: Multiaddr = format!("/ip4/8.8.8.8/udp/4001/quic-v1/p2p/{peer}")
        .parse()
        .unwrap();
    assert_eq!(
        candidate_addresses(
            peer,
            std::slice::from_ref(&public),
            Some(&old_public),
            &networks,
            DiscoveredPeerAddressSource::PublicDiscovery
        ),
        [public]
    );
}

#[tokio::test]
async fn transport_setup_has_one_owned_dial_deadline_backoff_and_late_event_fence() {
    let mut node =
        super::super::tests::membership_sync_test_node(NodeIdentity::generate_ed25519().unwrap());
    let peer = Libp2pPeerId::random();
    let mut state = AutoRelayState::new(AutoRelayConfig {
        max_candidates: 1,
        max_reservations: 1,
        retry_interval_seconds: 7,
    });
    let mut epochs = ConnectionEpochs::default();
    let metrics = RuntimeMetrics::default();
    let connections = HashMap::new();
    let now = Instant::now();
    state.record_candidate(peer, address(peer, "tcp/4001"));
    attempt_at(
        &mut node.swarm,
        &mut state,
        &metrics,
        &mut epochs,
        &connections,
        now,
    );
    let id = state.pending_reservations[&peer].owned_dial.unwrap();
    let expires = state.pending_reservations[&peer].expires_at;
    for offset in 0..100 {
        attempt_at(
            &mut node.swarm,
            &mut state,
            &metrics,
            &mut epochs,
            &connections,
            now + Duration::from_millis(offset),
        );
        assert_eq!(state.pending_reservations[&peer].owned_dial, Some(id));
        assert_eq!(state.pending_reservations[&peer].expires_at, expires);
        assert!(state.reservation_listeners.is_empty());
        assert_eq!(epochs.connections.len(), 1);
    }
    attempt_at(
        &mut node.swarm,
        &mut state,
        &metrics,
        &mut epochs,
        &connections,
        expires,
    );
    assert!(state.pending_reservations.is_empty());
    assert!(epochs.retiring.contains(&id));
    epochs.record_started(id);
    assert!(!epochs.record_established(id));
    let retry = expires + Duration::from_secs(7);
    attempt_at(
        &mut node.swarm,
        &mut state,
        &metrics,
        &mut epochs,
        &connections,
        retry.checked_sub(Duration::from_millis(1)).unwrap(),
    );
    assert!(state.pending_reservations.is_empty());
    attempt_at(
        &mut node.swarm,
        &mut state,
        &metrics,
        &mut epochs,
        &connections,
        retry,
    );
    let new = state.pending_reservations[&peer].owned_dial.unwrap();
    assert_ne!(id, new);
    assert!(epochs.is_usable(new));
    epochs.advance();
    state.reset_for_network_change();
    epochs.record_started(new);
    assert!(!epochs.record_established(new));
    let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
    assert_eq!(snapshot.auto_relay_reservation_attempts, 2);
    assert_eq!(snapshot.auto_relay_reservation_failures, 1);
    assert_eq!(snapshot.auto_relay_discovery_queries, 0);
}

#[tokio::test]
async fn setup_expiry_does_not_retire_established_or_caller_owned_connections() {
    let mut node =
        super::super::tests::membership_sync_test_node(NodeIdentity::generate_ed25519().unwrap());
    let peer = Libp2pPeerId::random();
    let mut state = AutoRelayState::new(AutoRelayConfig {
        max_reservations: 2,
        ..AutoRelayConfig::default()
    });
    let mut epochs = ConnectionEpochs::default();
    let now = Instant::now();
    state.record_candidate(peer, address(peer, "tcp/4001"));
    state.next_reservation_targets(now);
    let owned = ConnectionId::new_unchecked(901);
    let borrowed = ConnectionId::new_unchecked(902);
    state
        .pending_reservations
        .get_mut(&peer)
        .unwrap()
        .owned_dial = Some(owned);
    for id in [owned, borrowed] {
        assert!(epochs.record_established(id));
    }
    let terminal_peer = Libp2pPeerId::random();
    state.record_candidate(terminal_peer, address(terminal_peer, "tcp/4001"));
    state.next_reservation_targets(now);
    let terminal = ConnectionId::new_unchecked(904);
    state
        .pending_reservations
        .get_mut(&terminal_peer)
        .unwrap()
        .owned_dial = Some(terminal);
    epochs.record_started(terminal);
    epochs.remove(terminal);
    expire_pending_auto_relay_reservations(
        &mut node.swarm,
        &mut state,
        &RuntimeMetrics::default(),
        &mut epochs,
        now + AUTO_RELAY_RESERVATION_PENDING_TIMEOUT,
    );
    assert!(state.pending_reservations.is_empty());
    assert!(epochs.is_usable(owned));
    assert!(epochs.is_usable(borrowed));
    assert!(
        !epochs.retiring.contains(&terminal),
        "terminal attempts must not leave retirement tombstones"
    );
}

#[tokio::test]
async fn cached_readiness_and_an_owned_listener_are_not_native_reservation_evidence() {
    let node =
        super::super::tests::membership_sync_test_node(NodeIdentity::generate_ed25519().unwrap());
    let peer = Libp2pPeerId::random();
    let id = ConnectionId::new_unchecked(903);
    let tcp = address(peer, "tcp/4001");
    let mut epochs = ConnectionEpochs::default();
    assert!(epochs.record_established(id));
    let connections = HashMap::from([((peer, id), endpoint(tcp.clone()))]);
    let mut state = AutoRelayState::default();
    let now = Instant::now();
    state.record_candidate(peer, tcp.clone());
    state.next_reservation_targets(now);
    let listener = ListenerId::next();
    assert!(state.record_reservation_listener(peer, listener, id));
    let mut readiness = RelayReadiness::default();
    readiness.record_reservation_accepted(peer);
    readiness.record_relay_listen_address(peer, listener, tcp);
    confirm_if_ready(
        &node.swarm,
        &epochs,
        &connections,
        &mut state,
        &readiness,
        peer,
    );
    assert!(state.pending_reservations.contains_key(&peer));
    assert!(state.accepted_reservation_peers.is_empty());
    assert!(confirmed_connection(&node.swarm, &epochs, &connections, peer, id).is_none());
}

#[tokio::test]
async fn explicit_relay_override_is_excluded_even_before_its_reservation_is_ready() {
    let mut node =
        super::super::tests::membership_sync_test_node(NodeIdentity::generate_ed25519().unwrap());
    let configured = Libp2pPeerId::random();
    let alternative = Libp2pPeerId::random();
    let configured_relays = [(configured, address(configured, "udp/4001/quic-v1"))];
    let mut state = AutoRelayState::new(AutoRelayConfig {
        max_reservations: 1,
        ..AutoRelayConfig::default()
    });
    state.record_candidate(configured, address(configured, "tcp/4001"));
    state.record_candidate(alternative, address(alternative, "tcp/4002"));
    let mut epochs = ConnectionEpochs::default();
    let metrics = RuntimeMetrics::default();
    attempt_with_configured_at(
        &mut node.swarm,
        &mut state,
        &metrics,
        &mut epochs,
        &HashMap::new(),
        &configured_relays,
        Instant::now(),
    );
    assert!(!state.pending_reservations.contains_key(&configured));
    assert!(
        state.pending_reservations[&alternative]
            .owned_dial
            .is_some()
    );
    assert_eq!(epochs.connections.len(), 1);
    assert_eq!(
        metrics
            .snapshot(crate::queue::QueueStats::default())
            .auto_relay_reservation_attempts,
        1
    );
    assert!(state.reservation_listeners.is_empty());
}
