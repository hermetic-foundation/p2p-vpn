//! Explicit-time owner/policy tests, not a simulated swarm or packet-delivery soak.
//! Numeric schedules are independent of the production functions under test.

use super::*;

fn bootstrap_failure() -> DialError {
    DialError::Transport(vec![(
        "/ip4/203.0.113.1/tcp/4001".parse().unwrap(),
        TransportError::Other(io::Error::other("controlled unreachable bootstrap")),
    )])
}

#[test]
fn public_backoff_caps_retries_and_ignores_bursts_for_twenty_four_hours() {
    assert_eq!(PUBLIC_DISCOVERY_BACKOFF_BASE, Duration::from_secs(30));
    assert_eq!(PUBLIC_DISCOVERY_BACKOFF_MAX, Duration::from_secs(600));
    let start = Instant::now();
    let mut backoff = PublicDiscoveryBackoff::from_bootstrap_defaults(true);
    let mut public_peers = backoff.bootstrap_peers.iter().copied();
    let public = public_peers.next().unwrap();
    let alternate = public_peers.next().unwrap();
    let unrelated = Libp2pPeerId::random();
    assert!(!backoff.bootstrap_peers.contains(&unrelated));
    let error = bootstrap_failure();
    let mut failures = 0;

    for elapsed in 0..=86_400 {
        let now = start + Duration::from_secs(elapsed);
        // 30+60+120+240+480 = 930; all subsequent failures wait 600s.
        let expected_attempt = [0, 30, 90, 210, 450].contains(&elapsed)
            || (elapsed >= 930 && (elapsed - 930) % 600 == 0);
        assert_eq!(
            backoff.should_dial_bootstrap_peer(public, now),
            expected_attempt,
            "t={elapsed}"
        );
        assert_eq!(
            backoff.should_dial_bootstrap_peer(alternate, now),
            expected_attempt,
            "global bootstrap cooldown, t={elapsed}"
        );
        assert!(backoff.should_dial_bootstrap_peer(unrelated, now));
        if expected_attempt {
            let delay_seconds = match elapsed {
                0 => 30,
                30 => 60,
                90 => 120,
                210 => 240,
                450 => 480,
                _ => 600,
            };
            let delay = Duration::from_secs(delay_seconds);
            assert_eq!(
                backoff.record_outgoing_error(Some(public), &error, now),
                Some(delay)
            );
            assert_eq!(backoff.suppressed_until, Some(now + delay));
            assert!(backoff.suppresses(now + delay - Duration::from_nanos(1)));
            assert!(!backoff.suppresses(now + delay));
            failures += 1;
        }
        let deadline = backoff.suppressed_until;
        for peer in [Some(public), Some(alternate), Some(unrelated), None] {
            assert_eq!(backoff.record_outgoing_error(peer, &error, now), None);
        }
        assert_eq!(
            backoff.record_outgoing_error(Some(public), &DialError::Aborted, now),
            None
        );
        backoff.record_connection_established(unrelated);
        assert_eq!(backoff.suppressed_until, deadline);
        assert_eq!(backoff.consecutive_no_route_failures, failures);
    }
    assert_eq!(failures, 148);
    assert_eq!(
        backoff.suppressed_until,
        Some(start + Duration::from_secs(86_730))
    );
    let now = start + Duration::from_secs(86_400);
    backoff.record_connection_established(alternate);
    assert_eq!(backoff.consecutive_no_route_failures, 0);
    assert_eq!(backoff.suppressed_until, None);
    assert!(backoff.should_dial_bootstrap_peer(public, now));
    assert_eq!(
        backoff.record_outgoing_error(Some(public), &error, now),
        Some(Duration::from_secs(30))
    );
}

#[test]
fn healthy_quiet_paths_suppress_due_work_for_twenty_four_hours_then_release_it() {
    assert_eq!(PUBLIC_DISCOVERY_LAN_FIRST_GRACE, Duration::from_secs(60));
    let identity = NodeIdentity::generate_ed25519().unwrap();
    let direct = Libp2pPeerId::random();
    let relay = Libp2pPeerId::random();
    let config: Config = serde_json::from_value(serde_json::json!({
        "network": {"name": "settling-timeline", "private_key": identity.private_key},
        "peers": [{"id": direct.to_string()}, {"id": relay.to_string()}]
    }))
    .unwrap();
    let forwarder = Forwarder::from_config(&config).unwrap();
    let capabilities = PeerCapabilities::default();
    let direct = PeerId::from_libp2p(direct);
    let relay = PeerId::from_libp2p(relay);
    let mut paths = PathSet::new();
    paths.record_established(direct, PathKind::DirectTcpStream);
    paths.record_established(relay, PathKind::CircuitRelay);
    let start = Instant::now();
    let holdoff = Some(start + Duration::from_secs(60));
    let mut maintenance = KademliaMaintenance::new(start);
    let mut backoff = PublicDiscoveryBackoff::from_bootstrap_defaults(true);
    let public = *backoff.bootstrap_peers.iter().next().unwrap();
    assert_eq!(
        backoff.record_outgoing_error(Some(public), &bootstrap_failure(), start),
        Some(Duration::from_secs(30))
    );
    let mut eligible_dials = 0;
    let mut eligible_queries = 0;
    for elapsed in 0..=86_400 {
        let now = start + Duration::from_secs(elapsed);
        assert_eq!(public_discovery_holdoff_active(holdoff, now), elapsed < 60);
        assert_eq!(backoff.suppresses(now), elapsed < 30);
        let quiet = public_discovery_quiet_mode(&forwarder, &paths, &capabilities, None, None);
        assert!(quiet, "healthy direct and relay paths, t={elapsed}");
        let suppressed = public_discovery_holdoff_active(holdoff, now) || quiet;
        eligible_dials +=
            usize::from(!suppressed && backoff.should_dial_bootstrap_peer(public, now));
        eligible_queries += usize::from(maintenance.should_start(now, suppressed));
        assert_eq!(maintenance.pending_queries(), 0);
        assert!(maintenance.started_at.is_none());
        assert_eq!(maintenance.next_due, start);
        assert_eq!(backoff.consecutive_no_route_failures, 1);
    }
    assert_eq!(eligible_dials, 0);
    assert_eq!(eligible_queries, 0);

    // No reset or timer/configuration rewrite: losing just one intended peer
    // ends quiet mode even though the other peer still has a usable path.
    let now = start + Duration::from_secs(86_400);
    paths.record_closed(relay, PathKind::CircuitRelay);
    let quiet = public_discovery_quiet_mode(&forwarder, &paths, &capabilities, None, None);
    assert!(!quiet);
    assert!(!public_discovery_holdoff_active(holdoff, now));
    assert!(backoff.should_dial_bootstrap_peer(public, now));
    assert!(maintenance.should_start(now, quiet));
    let local = Libp2pPeerId::random();
    let mut kad = kad::Behaviour::new(local, kad::store::MemoryStore::new(local));
    let query = kad.get_closest_peers(public);
    maintenance.record_started(query, now);
    assert_eq!(maintenance.pending_queries(), 1);
    paths.record_established(relay, PathKind::CircuitRelay);
    assert!(public_discovery_quiet_mode(
        &forwarder,
        &paths,
        &capabilities,
        None,
        None
    ));
    assert_eq!(maintenance.cancel_queries(&mut kad), 1);
    assert_eq!(kad.query_pool_usage().retained, 0);
    assert_eq!(maintenance.pending_queries(), 0);
}

#[test]
fn maintenance_cleanup_and_released_capacity_span_twenty_four_hours() {
    assert_eq!(KADEMLIA_MAINTENANCE_INTERVAL, Duration::from_secs(120));
    assert_eq!(KADEMLIA_MAINTENANCE_QUERY_TIMEOUT, Duration::from_secs(90));
    assert_eq!(KADEMLIA_MAINTENANCE_POLL_INTERVAL, Duration::from_secs(5));
    assert_eq!(REDIAL_INTERVAL, Duration::from_secs(10));
    // Public-primary shares one DHT; private-primary has a separate public
    // pairing DHT. Only the primary owns ordinary maintenance in the runner.
    for protocols in [
        vec![PUBLIC_IPFS_KADEMLIA_PROTOCOL],
        vec![
            crate::config::PRIVATE_KADEMLIA_PROTOCOL,
            PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ],
    ] {
        for cleanup_poll_seconds in [5, 10] {
            let start = Instant::now();
            let local = Libp2pPeerId::random();
            let mut roles = protocols
                .iter()
                .map(|protocol| {
                    let mut config = crate::runtime::p2p::controlled_kademlia_config(
                        libp2p::StreamProtocol::new(protocol),
                    );
                    config.set_query_pool_capacity(std::num::NonZeroUsize::new(2).unwrap());
                    let mut kad = kad::Behaviour::with_config(
                        local,
                        kad::store::MemoryStore::new(local),
                        config,
                    );
                    let unrelated = kad.try_get_closest_peers(Libp2pPeerId::random()).unwrap();
                    (kad, unrelated, KademliaMaintenance::new(start))
                })
                .collect::<Vec<_>>();
            let mut expired = 0;
            let mut cancelled = 0;
            let mut resumed = 0;

            for cycle in 0..720 {
                // Starting just after a poll makes cleanup land at 95s or
                // 100s, not 90s. A fixed offset preserves the 120s cadence.
                let poll_origin = start + Duration::from_secs(cycle * 120);
                let began = poll_origin + Duration::from_nanos(1);
                for (kad, unrelated, maintenance) in roles.iter_mut().take(1) {
                    assert!(maintenance.should_start(began, false));
                    let query = kad.try_get_closest_peers(Libp2pPeerId::random()).unwrap();
                    maintenance.record_started(query, began);
                    assert_eq!(maintenance.pending_queries(), 1);
                    assert_eq!(kad.query_pool_usage().retained, 2);
                    assert!(matches!(
                        kad.try_get_closest_peers(Libp2pPeerId::random()),
                        Err(kad::QueryStartError::Capacity(_))
                    ));
                    assert_eq!(kad.query_pool_usage().retained, 2);
                    assert!(!maintenance.should_start(began + Duration::from_secs(120), false));
                    let retired_at = match cycle % 3 {
                        0 => {
                            let deadline = began + Duration::from_secs(90);
                            assert_eq!(
                                maintenance.expire_queries(kad, deadline - Duration::from_nanos(1)),
                                0
                            );
                            assert_eq!(maintenance.expire_queries(kad, deadline), 1);
                            expired += 1;
                            deadline
                        }
                        1 => {
                            let before = poll_origin + Duration::from_secs(90);
                            assert_eq!(maintenance.expire_queries(kad, before), 0);
                            assert_eq!(kad.query_pool_usage().retained, 2);
                            let poll = poll_origin + Duration::from_secs(90 + cleanup_poll_seconds);
                            assert_eq!(maintenance.expire_queries(kad, poll), 1);
                            assert!(poll.duration_since(began) < Duration::from_secs(100));
                            expired += 1;
                            poll
                        }
                        _ => {
                            let quiet = began + Duration::from_secs(35);
                            assert!(!maintenance.should_start(quiet, true));
                            assert_eq!(maintenance.cancel_queries(kad), 1);
                            cancelled += 1;
                            quiet
                        }
                    };
                    assert_eq!(maintenance.pending_queries(), 0);
                    assert!(maintenance.started_at.is_none());
                    assert!(!maintenance.finish_query(query));
                    assert!(!kad.query_is_retained(&query));
                    assert!(!kad.cancel_query(&query));
                    assert!(kad.query_is_retained(unrelated));
                    assert_eq!(kad.query_pool_usage().retained, 1);
                    assert_eq!(maintenance.expire_queries(kad, retired_at), 0);
                    assert_eq!(maintenance.cancel_queries(kad), 0);
                    assert!(!maintenance.should_start(retired_at, false));
                    assert_eq!(maintenance.next_due, began + Duration::from_secs(120));

                    let useful = kad.try_get_closest_peers(Libp2pPeerId::random()).unwrap();
                    assert!(kad.query_is_retained(&useful));
                    assert_eq!(kad.query_pool_usage().retained, 2);
                    assert!(kad.cancel_query(&useful));
                    resumed += 1;
                    let next_due = began + Duration::from_secs(120);
                    assert!(!maintenance.should_start(next_due - Duration::from_nanos(1), false));
                    assert!(maintenance.should_start(next_due, false));
                    assert!(!maintenance.should_start(next_due, true));
                    assert_eq!(kad.query_pool_usage().retained, 1);
                    assert_eq!(kad.query_pool_usage().rejected, cycle + 1);
                }
                for (pairing, unrelated, maintenance) in roles.iter().skip(1) {
                    assert!(pairing.query_is_retained(unrelated));
                    assert_eq!(pairing.query_pool_usage().retained, 1);
                    assert_eq!(pairing.query_pool_usage().rejected, 0);
                    assert_eq!(maintenance.pending_queries(), 0);
                    assert_eq!(maintenance.next_due, start);
                }
            }
            assert_eq!(expired, 480);
            assert_eq!(cancelled, 240);
            assert_eq!(resumed, 720);
            for (index, (kad, unrelated, maintenance)) in roles.iter_mut().enumerate() {
                // The unpolled sentinel represents a different query owner,
                // not a claim that real queries should live for a whole day.
                assert!(kad.cancel_query(unrelated));
                assert_eq!(kad.query_pool_usage().retained, 0);
                assert_eq!(maintenance.pending_queries(), 0);
                let end = start + Duration::from_secs(86_400);
                assert_eq!(maintenance.should_start(end, false), index != 0);
                assert!(maintenance.should_start(end + Duration::from_nanos(1), false));
            }
        }
    }
}
