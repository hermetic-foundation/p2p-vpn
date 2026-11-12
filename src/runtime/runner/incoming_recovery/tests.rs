use super::*;

struct Fixture {
    node: P2pNode,
    state: AutoRelayState,
    maintenance: KademliaMaintenance,
    readiness: RelayReadiness,
    epochs: ConnectionEpochs,
    connections: HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    backoff: PublicDiscoveryBackoff,
    holdoff_until: Option<Instant>,
    metrics: RuntimeMetrics,
}

impl Fixture {
    fn new(now: Instant, protocol: &str, capacity: usize) -> Self {
        let (_, node) = super::super::tests::autonat_query_test_node(
            protocol,
            Duration::from_secs(600),
            capacity,
        );
        Self {
            node,
            state: AutoRelayState::default(),
            maintenance: KademliaMaintenance::new(now),
            readiness: RelayReadiness::default(),
            epochs: ConnectionEpochs::default(),
            connections: HashMap::new(),
            backoff: PublicDiscoveryBackoff::default(),
            holdoff_until: None,
            metrics: RuntimeMetrics::default(),
        }
    }

    fn drive(&mut self, now: Instant) {
        drive(
            &mut self.node,
            RecoveryContext {
                state: &mut self.state,
                maintenance: &mut self.maintenance,
                readiness: &self.readiness,
                epochs: &self.epochs,
                connections: &self.connections,
                backoff: &self.backoff,
                holdoff_until: self.holdoff_until,
                metrics: &self.metrics,
            },
            now,
        );
    }
}

#[tokio::test]
async fn quiet_incoming_recovery_obeys_burst_cooldown_and_deadlines_for_twenty_four_hours() {
    use libp2p::swarm::NetworkBehaviour;
    use std::task::{Context, Poll};
    let start = Instant::now();
    for protocol in [
        PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        crate::config::PRIVATE_KADEMLIA_PROTOCOL,
    ] {
        for fast_completion in [false, true] {
            let mut fixture = Fixture::new(start, protocol, 1);
            let mut starts = Vec::new();
            let mut last_query = None;
            for elapsed in (0..86_400).step_by(10) {
                let now = start + Duration::from_secs(elapsed);
                fixture.drive(now);
                let incoming = &mut fixture.maintenance.incoming;
                if let Some((id, started)) = incoming.query {
                    assert!(now < started + KADEMLIA_MAINTENANCE_QUERY_TIMEOUT);
                    if last_query != Some(id) {
                        starts.push(elapsed);
                        last_query = Some(id);
                    }
                    // This is ordinary maintenance suppression, not incoming-path loss.
                    assert_eq!(
                        fixture
                            .maintenance
                            .cancel_queries(&mut fixture.node.swarm.behaviour_mut().kad),
                        0
                    );
                    assert!(fixture.maintenance.incoming.owns(id));
                    if fast_completion {
                        let kad = &mut fixture.node.swarm.behaviour_mut().kad;
                        let mut context = Context::from_waker(futures::task::noop_waker_ref());
                        let mut completed = false;
                        for _ in 0..16 {
                            match kad.poll(&mut context) {
                                Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(
                                    kad::Event::OutboundQueryProgressed {
                                        id: result, step, ..
                                    },
                                )) if result == id && step.last => {
                                    completed = true;
                                    break;
                                }
                                Poll::Ready(_) => {}
                                Poll::Pending => break,
                            }
                        }
                        assert!(completed, "empty native query must terminate");
                        assert!(fixture.maintenance.finish_query(id));
                    }
                }
                let usage = fixture.node.swarm.behaviour().kad.query_pool_usage();
                assert!(usage.retained <= 1);
                assert_eq!(usage.rejected, 0);
                assert!(fixture.state.candidates.is_empty());
                assert!(fixture.connections.is_empty());
            }
            assert_eq!(starts.len(), 309, "{protocol}, fast={fast_completion}");
            for (index, pair) in starts.windows(2).enumerate() {
                assert_eq!(pair[1] - pair[0], if index % 3 == 2 { 600 } else { 120 });
            }
            assert_eq!(
                fixture
                    .metrics
                    .snapshot(crate::queue::QueueStats::default())
                    .auto_relay_discovery_queries,
                309
            );
            assert_eq!(
                fixture.node.swarm.behaviour().kad.protocol_names()[0].as_ref(),
                protocol
            );
            if let Some(pairing) = fixture.node.swarm.behaviour().pairing_kad.as_ref() {
                assert_eq!(pairing.query_pool_usage().retained, 0);
            }
        }
    }
}

#[tokio::test]
async fn incoming_recovery_waits_for_lan_backoff_native_capacity_and_existing_candidates() {
    let start = Instant::now();
    let mut fixture = Fixture::new(start, PUBLIC_IPFS_KADEMLIA_PROTOCOL, 1);
    fixture.holdoff_until = Some(start + PEER_RECOVERY_LAN_FIRST_GRACE);
    fixture.drive(start);
    fixture.drive(start + PEER_RECOVERY_LAN_FIRST_GRACE - Duration::from_millis(1));
    assert!(!fixture.maintenance.incoming.pending());
    assert_eq!(fixture.maintenance.incoming.starts_in_burst, 0);
    let now = start + PEER_RECOVERY_LAN_FIRST_GRACE;
    fixture.backoff.suppressed_until = Some(now + Duration::from_secs(30));
    fixture.drive(now);
    assert!(!fixture.maintenance.incoming.pending());
    fixture.backoff.reset();
    let unrelated = fixture
        .node
        .swarm
        .behaviour_mut()
        .kad
        .get_closest_peers(Libp2pPeerId::random());
    for offset in [0, 120, 600] {
        fixture.drive(now + Duration::from_secs(offset));
        assert!(!fixture.maintenance.incoming.pending());
        assert_eq!(fixture.maintenance.incoming.starts_in_burst, 0);
        assert_eq!(
            fixture
                .node
                .swarm
                .behaviour()
                .kad
                .query_pool_usage()
                .rejected,
            0
        );
        assert!(
            fixture
                .node
                .swarm
                .behaviour()
                .kad
                .query_is_retained(&unrelated)
        );
    }
    fixture
        .node
        .swarm
        .behaviour_mut()
        .kad
        .cancel_query(&unrelated);
    let peer = Libp2pPeerId::random();
    fixture.state.record_candidate(
        peer,
        format!("/ip4/8.8.8.8/tcp/4001/p2p/{peer}").parse().unwrap(),
    );
    fixture
        .state
        .retry_after
        .insert(peer, now + Duration::from_secs(900));
    fixture.drive(now + Duration::from_secs(600));
    assert!(!fixture.maintenance.incoming.pending());
    fixture.state.remove_candidate(peer);
    fixture.drive(now + Duration::from_secs(600));
    assert!(fixture.maintenance.incoming.pending());
    assert_eq!(fixture.maintenance.incoming.starts_in_burst, 1);
}

#[tokio::test]
async fn provisional_candidate_does_not_cancel_the_owned_lookup_or_start_another() {
    let now = Instant::now();
    let mut fixture = Fixture::new(now, PUBLIC_IPFS_KADEMLIA_PROTOCOL, 1);
    fixture.drive(now);
    let (query, _) = fixture.maintenance.incoming.query.unwrap();
    let peer = Libp2pPeerId::random();
    fixture.state.record_candidate(
        peer,
        format!("/ip4/8.8.8.8/tcp/4001/p2p/{peer}").parse().unwrap(),
    );
    fixture.drive(now + Duration::from_secs(10));
    assert!(fixture.maintenance.incoming.owns(query));
    assert!(fixture.node.swarm.behaviour().kad.query_is_retained(&query));
    assert_eq!(
        fixture
            .metrics
            .snapshot(crate::queue::QueueStats::default())
            .auto_relay_discovery_queries,
        1
    );
    fixture.drive(now + KADEMLIA_MAINTENANCE_QUERY_TIMEOUT);
    assert!(!fixture.maintenance.incoming.pending());
    fixture.drive(now + KADEMLIA_MAINTENANCE_INTERVAL);
    assert!(!fixture.maintenance.incoming.pending());
    assert_eq!(
        fixture
            .metrics
            .snapshot(crate::queue::QueueStats::default())
            .auto_relay_discovery_queries,
        1
    );
}

#[tokio::test]
async fn incoming_cancellation_expiry_and_network_reset_preserve_unrelated_query_owners() {
    let now = Instant::now();
    let mut fixture = Fixture::new(now, PUBLIC_IPFS_KADEMLIA_PROTOCOL, 4);
    let ordinary = fixture
        .node
        .swarm
        .behaviour_mut()
        .kad
        .get_closest_peers(Libp2pPeerId::random());
    fixture.maintenance.record_started(ordinary, now);
    let publication = fixture
        .node
        .swarm
        .behaviour_mut()
        .kad
        .get_closest_peers(Libp2pPeerId::random());
    fixture.maintenance.address_publication = Some((publication, now));
    let unrelated = fixture
        .node
        .swarm
        .behaviour_mut()
        .kad
        .get_closest_peers(Libp2pPeerId::random());
    fixture.drive(now);
    let (incoming, _) = fixture.maintenance.incoming.query.unwrap();
    assert!(fixture.maintenance.owns_query(incoming));
    assert!(fixture.maintenance.has_pending_query());
    assert_eq!(
        fixture
            .maintenance
            .cancel_queries(&mut fixture.node.swarm.behaviour_mut().kad),
        1
    );
    assert!(fixture.maintenance.incoming.owns(incoming));
    assert!(
        fixture
            .node
            .swarm
            .behaviour()
            .kad
            .query_is_retained(&incoming)
    );
    fixture.drive(now + KADEMLIA_MAINTENANCE_QUERY_TIMEOUT);
    assert!(!fixture.maintenance.incoming.pending());
    assert!(
        !fixture
            .node
            .swarm
            .behaviour()
            .kad
            .query_is_retained(&incoming)
    );
    assert_eq!(
        fixture.maintenance.next_due,
        now + KADEMLIA_MAINTENANCE_INTERVAL
    );
    for id in [publication, unrelated] {
        assert!(fixture.node.swarm.behaviour().kad.query_is_retained(&id));
    }
    let next = now + KADEMLIA_MAINTENANCE_INTERVAL;
    fixture.drive(next);
    let (incoming, _) = fixture.maintenance.incoming.query.unwrap();
    assert_eq!(
        fixture
            .maintenance
            .reset(&mut fixture.node.swarm.behaviour_mut().kad, next),
        1
    );
    assert!(
        !fixture
            .node
            .swarm
            .behaviour()
            .kad
            .query_is_retained(&incoming)
    );
    for id in [publication, unrelated] {
        assert!(fixture.node.swarm.behaviour().kad.query_is_retained(&id));
    }
    assert_eq!(fixture.maintenance.incoming.starts_in_burst, 0);
    assert_eq!(fixture.maintenance.incoming.next_due, next);
}

#[tokio::test]
async fn incoming_cancel_purges_native_queued_results_without_cancelling_other_work() {
    use libp2p::kad::store::RecordStore;
    use libp2p::swarm::{NetworkBehaviour, ToSwarm};
    use std::task::{Context, Poll};
    let now = Instant::now();
    let mut fixture = Fixture::new(now, PUBLIC_IPFS_KADEMLIA_PROTOCOL, 2);
    let local = fixture.node.local_peer_id;
    let kad = &mut fixture.node.swarm.behaviour_mut().kad;
    let key = kad::RecordKey::new(&b"incoming-cancel-test");
    kad.store_mut()
        .add_provider(kad::ProviderRecord::new(key.clone(), local, Vec::new()))
        .unwrap();
    let owned = kad.get_providers(key);
    fixture.maintenance.incoming.started(owned, now);
    let unrelated = kad.get_closest_peers(Libp2pPeerId::random());
    assert!(kad.behaviour_queue_usage().events > 0);
    assert_eq!(fixture.maintenance.incoming.cancel(kad), 1);
    assert!(!kad.query_is_retained(&owned));
    assert!(kad.query_is_retained(&unrelated));
    let mut context = Context::from_waker(futures::task::noop_waker_ref());
    let mut saw_unrelated = false;
    for _ in 0..16 {
        match kad.poll(&mut context) {
            Poll::Ready(ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
                id, ..
            })) => {
                assert_ne!(id, owned, "cancelled native queued result escaped");
                saw_unrelated |= id == unrelated;
            }
            Poll::Ready(_) => {}
            Poll::Pending => break,
        }
    }
    assert!(saw_unrelated);
}

#[tokio::test]
async fn incoming_recovery_respects_disabled_policy_and_explicit_roles_do_not_fill_candidates() {
    let now = Instant::now();
    for disabled in ["discovery", "candidates", "reservations"] {
        let mut fixture = Fixture::new(now, PUBLIC_IPFS_KADEMLIA_PROTOCOL, 1);
        match disabled {
            "discovery" => {
                fixture.node.discovery.kademlia = false;
                fixture.node.discovery.autonat = false;
            }
            "candidates" => fixture.state.policy.max_candidates = 0,
            _ => fixture.state.policy.max_reservations = 0,
        }
        fixture.drive(now);
        assert!(!fixture.maintenance.incoming.pending(), "{disabled}");
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
    let mut fixture = Fixture::new(now, PUBLIC_IPFS_KADEMLIA_PROTOCOL, 1);
    fixture.state.policy.max_candidates = 1;
    let explicit = Libp2pPeerId::random();
    let address: Multiaddr = format!("/ip4/8.8.8.8/tcp/4001/p2p/{explicit}")
        .parse()
        .unwrap();
    fixture.state.record_candidate(explicit, address.clone());
    fixture
        .node
        .relay_peer_addresses
        .push((explicit, address.clone()));
    fixture.drive(now);
    assert!(fixture.state.candidates.is_empty());
    assert!(fixture.maintenance.incoming.pending());
    assert!(!fixture.state.record_candidate(explicit, address));
    let automatic = Libp2pPeerId::random();
    assert!(
        fixture.state.record_candidate(
            automatic,
            format!("/ip4/1.1.1.1/tcp/4001/p2p/{automatic}")
                .parse()
                .unwrap()
        )
    );
}
