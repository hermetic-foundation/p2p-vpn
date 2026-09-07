use super::*;

use std::task::{Context, Poll};

use libp2p::{
    core::{Endpoint, transport::PortUse},
    swarm::{
        ConnectionHandler, ConnectionHandlerEvent, ConnectionId, DialError, FromSwarm, ToSwarm,
        handler::{ConnectionEvent, DialUpgradeError},
    },
};

type Dht = kad::Behaviour<kad::store::MemoryStore>;
type Handler = <Dht as NetworkBehaviour>::ConnectionHandler;
type HandlerEvent = <Handler as ConnectionHandler>::ToBehaviour;
type HandlerInput = <Handler as ConnectionHandler>::FromBehaviour;

fn resource_config() -> kad::Config {
    controlled_kademlia_config(StreamProtocol::new(PUBLIC_IPFS_KADEMLIA_PROTOCOL))
}

fn poll_resource_dht(kad: &mut Dht) -> Poll<ToSwarm<kad::Event, HandlerInput>> {
    let waker = futures::task::noop_waker();
    kad.poll(&mut Context::from_waker(&waker))
}

#[test]
fn query_resource_snapshot_includes_finished_caches_and_releases_current_maxima() {
    let local = PeerId::random();
    let remote = PeerId::random();
    let mut config = resource_config();
    config.set_query_limits(kad::QueryLimits::new(
        NonZeroUsize::new(2).unwrap(),
        kad::AddressLimits::new(NonZeroUsize::MIN, NonZeroUsize::new(128).unwrap()),
        NonZeroUsize::new(128).unwrap(),
    ));
    let mut kad = Dht::with_config(local, kad::store::MemoryStore::new(local), config);
    kad.add_address(&remote, "/memory/1".parse().unwrap());
    drop(drain_routing_snapshots(&mut kad));
    let rich = kad.get_closest_peers(PeerId::random());
    assert!(matches!(
        poll_resource_dht(&mut kad),
        Poll::Ready(ToSwarm::Dial { .. })
    ));
    let learned: Multiaddr = "/memory/2".parse().unwrap();
    kad.on_connection_handler_event(
        remote,
        ConnectionId::new_unchecked(1),
        HandlerEvent::FindNodeRes {
            query_id: rich,
            closer_peers: vec![
                kad::KadPeer {
                    node_id: PeerId::random(),
                    multiaddrs: vec![learned.clone(), "/memory/3".parse().unwrap()],
                    connection_ty: kad::ConnectionType::NotConnected,
                },
                kad::KadPeer {
                    node_id: PeerId::random(),
                    multiaddrs: vec!["/memory/4".parse().unwrap()],
                    connection_ty: kad::ConnectionType::NotConnected,
                },
            ],
        },
    );
    let small = kad.get_closest_peers(PeerId::random());
    let expected = kad::QueryResourceSnapshot {
        bounded_queries: 2,
        candidates: 3,
        address_bytes: learned.len(),
        max_candidates: 2,
        max_address_bytes: learned.len(),
        rejected_reports: 2,
    };
    assert_eq!(kad.query_resource_snapshot(), expected);
    kad.query_mut(&rich).unwrap().finish();
    assert!(kad.query(&rich).is_none());
    assert_eq!(kad.iter_queries().count(), 1);
    assert!(kad.query_is_retained(&rich));
    assert_eq!(kad.query_resource_snapshot(), expected);
    assert_eq!(kad.query_lifecycle_usage().retired_phases, 0);
    assert!(kad.cancel_query(&rich));
    assert_eq!(
        kad.query_resource_snapshot(),
        kad::QueryResourceSnapshot {
            bounded_queries: 1,
            candidates: 1,
            max_candidates: 1,
            ..Default::default()
        }
    );
    assert!(kad.cancel_query(&small));
    assert_eq!(kad.query_resource_snapshot(), Default::default());
    assert_eq!(
        kad.query_lifecycle_usage(),
        kad::QueryLifecycleUsage {
            admitted_phases: 2,
            retired_phases: 2,
            cancelled_phases: 2,
            requests: 1,
            successes: 1,
            ..Default::default()
        }
    );
}

#[test]
fn query_request_totals_are_monotonic_through_natural_retirement() {
    for kind in ["fixed", "closest", "disjoint"] {
        let local = PeerId::random();
        let peers = [PeerId::random(), PeerId::random()];
        let mut config = resource_config();
        config.disjoint_query_paths(kind == "disjoint");
        let mut kad = Dht::with_config(local, kad::store::MemoryStore::new(local), config);
        for round in 1..=2 {
            for peer in peers {
                kad.add_address(&peer, "/memory/1".parse().unwrap());
            }
            drop(drain_routing_snapshots(&mut kad));
            let key = kad::RecordKey::new(&[u8::try_from(round).unwrap()]);
            let query = if kind == "fixed" {
                kad.put_record_to(
                    kad::Record::new(key.clone(), vec![1]),
                    peers.into_iter(),
                    kad::Quorum::All,
                )
            } else {
                kad.get_closest_peers(PeerId::random())
            };
            let mut completed = false;
            let mut previous = kad.query_lifecycle_usage();
            for _ in 0..16 {
                match poll_resource_dht(&mut kad) {
                    Poll::Ready(ToSwarm::Dial { opts }) => {
                        let peer = opts.get_peer_id().unwrap();
                        if peer == peers[0] {
                            let event = if kind == "fixed" {
                                HandlerEvent::PutRecordRes {
                                    key: key.clone(),
                                    value: vec![1],
                                    query_id: query,
                                }
                            } else {
                                HandlerEvent::FindNodeRes {
                                    closer_peers: vec![],
                                    query_id: query,
                                }
                            };
                            kad.on_connection_handler_event(peer, opts.connection_id(), event);
                        } else {
                            assert_eq!(peer, peers[1]);
                            kad.on_swarm_event(FromSwarm::DialFailure(
                                libp2p::swarm::behaviour::DialFailure {
                                    peer_id: Some(peer),
                                    connection_id: opts.connection_id(),
                                    error: &DialError::NoAddresses,
                                },
                            ));
                        }
                    }
                    Poll::Ready(ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
                        id,
                        result,
                        stats,
                        step,
                    })) => {
                        assert_eq!(id, query);
                        assert!(step.last);
                        assert_eq!(stats.num_requests(), 2);
                        assert_eq!(stats.num_successes(), 1);
                        assert_eq!(stats.num_failures(), 1);
                        match result {
                            kad::QueryResult::PutRecord(result) => assert!(result.is_err()),
                            kad::QueryResult::GetClosestPeers(result) => assert!(result.is_ok()),
                            other => panic!("unexpected result: {other:?}"),
                        }
                        completed = true;
                    }
                    other => panic!("unexpected query action: {other:?}"),
                }
                let current = kad.query_lifecycle_usage();
                assert!(current.requests >= previous.requests);
                assert!(current.successes >= previous.successes);
                assert!(current.failures >= previous.failures);
                previous = current;
                if completed {
                    break;
                }
            }
            assert!(completed, "{kind} query did not retire");
            assert_eq!(kad.query_pool_usage().retained, 0);
            assert_eq!(kad.query_resource_snapshot(), Default::default());
            assert_eq!(
                kad.query_lifecycle_usage(),
                kad::QueryLifecycleUsage {
                    admitted_phases: round,
                    retired_phases: round,
                    completed_phases: round,
                    requests: 2 * round,
                    successes: round,
                    failures: round,
                    ..Default::default()
                },
                "{kind}: a failed application result still completes its phase"
            );
            assert!(poll_resource_dht(&mut kad).is_pending());
            assert_eq!(kad.query_lifecycle_usage(), previous);
            assert!(!kad.cancel_query(&query));
            assert_eq!(kad.query_lifecycle_usage(), previous);
        }
    }
}

fn dial_queue_dht(events: usize) -> (Dht, PeerId, PeerId) {
    let peer_for = |seed| {
        Keypair::ed25519_from_bytes([seed; 32])
            .unwrap()
            .public()
            .to_peer_id()
    };
    let local = peer_for(0);
    let first = peer_for(1);
    let mut config = resource_config();
    config
        .set_kbucket_size(NonZeroUsize::MIN)
        .set_behaviour_queue_limits(kad::BehaviourQueueLimits::new(
            NonZeroUsize::new(events).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
        ));
    let mut kad = Dht::with_config(local, kad::store::MemoryStore::new(local), config);
    let range = kad.kbucket(first).unwrap().range();
    let candidate = (2..=255)
        .map(peer_for)
        .find(|peer| kad.kbucket(*peer).unwrap().range() == range)
        .unwrap();
    assert_eq!(
        kad.add_address(&first, "/memory/1".parse().unwrap()),
        kad::RoutingUpdate::Success
    );
    // Only connected newcomers can cause a full bucket to probe its incumbent.
    let endpoint = libp2p::core::ConnectedPoint::Dialer {
        address: "/memory/2".parse().unwrap(),
        role_override: Endpoint::Dialer,
        port_use: PortUse::Reuse,
    };
    kad.on_swarm_event(FromSwarm::ConnectionEstablished(
        libp2p::swarm::behaviour::ConnectionEstablished {
            peer_id: candidate,
            connection_id: ConnectionId::new_unchecked(1),
            endpoint: &endpoint,
            failed_addresses: &[],
            other_established: 0,
        },
    ));
    (kad, first, candidate)
}

#[test]
fn local_dial_queue_counts_admission_rejection_and_dispatch_separately() {
    for events in [1, 2] {
        let (mut kad, first, candidate) = dial_queue_dht(events);
        assert_eq!(kad.dial_queue_usage(), Default::default());
        assert_eq!(
            kad.add_address(&candidate, "/memory/2".parse().unwrap()),
            kad::RoutingUpdate::Pending
        );
        let admitted = u64::from(events == 2);
        assert_eq!(
            kad.dial_queue_usage(),
            kad::DialQueueUsage {
                attempted: 1,
                admitted,
                dispatched: 0,
                discarded: 0
            }
        );
        assert_eq!(kad.behaviour_queue_usage().rejected, 1 - admitted);
        assert!(matches!(
            poll_resource_dht(&mut kad),
            Poll::Ready(ToSwarm::GenerateEvent(kad::Event::RoutingUpdated { .. }))
        ));
        if admitted == 1 {
            let Poll::Ready(ToSwarm::Dial { opts }) = poll_resource_dht(&mut kad) else {
                panic!("admitted local dial was not dispatched");
            };
            assert_eq!(opts.get_peer_id(), Some(first));
        }
        assert!(poll_resource_dht(&mut kad).is_pending());
        assert_eq!(
            kad.dial_queue_usage(),
            kad::DialQueueUsage {
                attempted: 1,
                admitted,
                dispatched: admitted,
                discarded: 0,
            }
        );
        assert_eq!(kad.query_lifecycle_usage(), Default::default());
    }
}

#[test]
fn local_dial_cancellation_discards_only_queued_unneeded_intents() {
    let (mut kad, first, candidate) = dial_queue_dht(2);
    drop(drain_routing_snapshots(&mut kad));
    let queries = [0_u8, 1].map(|key| {
        kad.put_record_to(
            kad::Record::new(vec![key], vec![1]),
            [first].into_iter(),
            kad::Quorum::One,
        )
    });
    for _ in 0..2 {
        assert!(matches!(
            poll_resource_dht(&mut kad),
            Poll::Ready(ToSwarm::Dial { .. })
        ));
    }
    assert_eq!(kad.pending_rpc_usage().requests, 2);
    assert_eq!(
        kad.add_address(&candidate, "/memory/2".parse().unwrap()),
        kad::RoutingUpdate::Pending
    );
    let queued = kad::DialQueueUsage {
        attempted: 3,
        admitted: 3,
        dispatched: 2,
        discarded: 0,
    };
    assert_eq!(kad.dial_queue_usage(), queued);
    assert!(kad.cancel_query(&queries[0]));
    assert_eq!(
        kad.dial_queue_usage(),
        queued,
        "another query still needs the queued dial"
    );
    assert!(kad.cancel_query(&queries[1]));
    let discarded = kad::DialQueueUsage {
        discarded: 1,
        ..queued
    };
    assert_eq!(kad.dial_queue_usage(), discarded);
    assert_eq!(kad.behaviour_queue_usage().events, 0);
    assert!(poll_resource_dht(&mut kad).is_pending());
    assert!(!kad.cancel_query(&queries[1]));
    assert_eq!(kad.dial_queue_usage(), discarded);
}

#[test]
fn production_handlers_observe_preload_mutation_and_drop_without_losing_history() {
    let local = PeerId::random();
    let peers = [PeerId::random(), PeerId::random()];
    let mut config = resource_config();
    config.set_handler_queue_limits(kad::HandlerQueueLimits::new(
        NonZeroUsize::MIN,
        NonZeroUsize::new(8).unwrap(),
    ));
    let mut kad = Dht::with_config(local, kad::store::MemoryStore::new(local), config);
    let queries = peers.map(|peer| {
        kad.put_record_to(
            kad::Record::new(vec![1], vec![1]),
            [peer].into_iter(),
            kad::Quorum::One,
        )
    });
    drop(drain_routing_snapshots(&mut kad));
    assert_eq!(kad.pending_rpc_usage().requests, 2);
    assert_eq!(kad.handler_resource_usage(), Default::default());
    let address: Multiaddr = "/memory/1".parse().unwrap();
    let mut outbound = kad
        .handle_established_outbound_connection(
            ConnectionId::new_unchecked(1),
            peers[0],
            &address,
            Endpoint::Dialer,
            PortUse::Reuse,
        )
        .unwrap();
    assert_eq!(kad.handler_resource_usage().handlers, 1);
    assert_eq!(
        kad.handler_resource_usage().usage,
        outbound.pending_request_usage()
    );
    let mut inbound = kad
        .handle_established_inbound_connection(
            ConnectionId::new_unchecked(2),
            peers[1],
            &address,
            &address,
        )
        .unwrap();
    assert_eq!(kad.pending_rpc_usage().requests, 0);
    assert_eq!(kad.handler_resource_usage().handlers, 2);
    assert_eq!(kad.handler_resource_usage().usage.requests, 2);
    assert_eq!(kad.handler_resource_usage().usage.bytes, 4);
    assert_eq!(
        kad.handler_resource_usage()
            .peak_pending_requests_per_handler,
        1
    );
    assert_eq!(
        kad.handler_resource_usage().peak_pending_bytes_per_handler,
        2
    );
    for query_id in queries {
        inbound.on_behaviour_event(HandlerInput::GetRecord {
            key: kad::RecordKey::new(&[1]),
            query_id,
        });
    }
    assert_eq!(kad.handler_resource_usage().usage.rejected, 2);
    assert_eq!(kad.handler_resource_usage().usage.unreported_rejections, 1);
    assert_eq!(kad.handler_resource_usage().usage.queued_rejections, 1);
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    assert!(matches!(
        outbound.poll(&mut cx),
        Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest { .. })
    ));
    assert_eq!(kad.handler_resource_usage().usage.requests, 1);
    assert_eq!(kad.handler_resource_usage().usage.bytes, 2);
    assert_eq!(kad.handler_resource_usage().usage.pending_negotiations, 1);
    assert_eq!(
        kad.handler_resource_usage().usage.active_outbound_streams,
        1
    );
    outbound.on_connection_event(ConnectionEvent::DialUpgradeError(DialUpgradeError {
        info: (),
        error: libp2p::swarm::StreamUpgradeError::Timeout,
    }));
    assert_eq!(kad.handler_resource_usage().usage.pending_negotiations, 0);
    drop(inbound);
    assert_eq!(kad.handler_resource_usage().handlers, 1);
    assert_eq!(kad.handler_resource_usage().usage.requests, 0);
    assert_eq!(kad.handler_resource_usage().usage.bytes, 0);
    assert_eq!(kad.handler_resource_usage().usage.queued_rejections, 0);
    drop(outbound);
    let retired = kad::HandlerResourceUsage {
        usage: kad::HandlerQueueUsage {
            rejected: 2,
            unreported_rejections: 1,
            ..Default::default()
        },
        peak_pending_requests_per_handler: 1,
        peak_pending_bytes_per_handler: 2,
        peak_pending_negotiations_per_handler: 1,
        peak_outbound_streams_per_handler: 1,
        peak_queued_rejections_per_handler: 1,
        ..Default::default()
    };
    assert_eq!(kad.handler_resource_usage(), retired);
    for query in queries {
        assert!(kad.cancel_query(&query));
    }
    for index in 3..11 {
        let handler = kad
            .handle_established_inbound_connection(
                ConnectionId::new_unchecked(index),
                peers[1],
                &address,
                &address,
            )
            .unwrap();
        assert_eq!(
            kad.handler_resource_usage(),
            kad::HandlerResourceUsage {
                handlers: 1,
                ..retired
            }
        );
        drop(handler);
        assert_eq!(kad.handler_resource_usage(), retired);
    }
}

#[tokio::test]
async fn primary_and_separate_pairing_query_budgets_and_counters_are_independent() {
    let mut node = build_node(&retention_diagnostic_config(true)).unwrap();
    let behaviour = node.swarm.behaviour_mut();
    let primary = &mut behaviour.kad;
    let pairing = behaviour.pairing_kad.as_mut().unwrap();
    let fill = |kad: &mut Dht| {
        (0..KADEMLIA_QUERY_POOL_CAPACITY.get())
            .map(|_| {
                kad.try_start_query(|kad| kad.get_closest_peers(PeerId::random()))
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    let primary_queries = fill(primary);
    assert!(primary.try_start_query(|_| ()).is_err());
    assert_eq!(pairing.query_pool_usage().retained, 0);
    assert_eq!(pairing.query_lifecycle_usage(), Default::default());
    assert_eq!(pairing.query_resource_snapshot(), Default::default());
    let primary_usage = primary.query_lifecycle_usage();
    let primary_cache = primary.query_resource_snapshot();
    let pairing_queries = fill(pairing);
    assert!(pairing.try_start_query(|_| ()).is_err());
    assert_eq!(primary.query_lifecycle_usage(), primary_usage);
    assert_eq!(primary.query_resource_snapshot(), primary_cache);
    let pairing_usage = pairing.query_lifecycle_usage();
    let pairing_cache = pairing.query_resource_snapshot();
    for query in primary_queries {
        assert!(primary.cancel_query(&query));
    }
    assert_eq!(primary.query_resource_snapshot(), Default::default());
    assert_eq!(pairing.query_lifecycle_usage(), pairing_usage);
    assert_eq!(pairing.query_resource_snapshot(), pairing_cache);
    assert!(pairing.try_start_query(|_| ()).is_err());
    let next = primary
        .try_start_query(|kad| kad.get_closest_peers(PeerId::random()))
        .unwrap();
    for query in pairing_queries {
        assert!(pairing.cancel_query(&query));
    }
    assert!(primary.query_is_retained(&next));
    assert_eq!(pairing.query_resource_snapshot(), Default::default());
    assert!(primary.cancel_query(&next));
}

fn resource_dhts(node: &P2pNode) -> [&Dht; 2] {
    let behaviour = node.swarm.behaviour();
    [&behaviour.kad, behaviour.pairing_kad.as_ref().unwrap()]
}

#[tokio::test]
async fn handler_telemetry_tracks_tcp_and_quic_loopback_churn_for_both_dhts() {
    for listen in ["/ip4/127.0.0.1/tcp/0", "/ip4/127.0.0.1/udp/0/quic-v1"] {
        let mut listener = build_node(&retention_diagnostic_config(true)).unwrap();
        let mut dialer = build_node(&retention_diagnostic_config(true)).unwrap();
        for node in [&mut listener, &mut dialer] {
            for seed in public_ipfs_bootstrap_peer_configs() {
                let (peer, _) = seed.peer_address().unwrap();
                public_pairing_kad_mut(node.swarm.behaviour_mut()).remove_peer(&peer);
            }
            public_pairing_kad_mut(node.swarm.behaviour_mut()).set_mode(Some(kad::Mode::Server));
            assert!(
                resource_dhts(node)
                    .iter()
                    .all(|kad| kad.handler_resource_usage().handlers == 0)
            );
        }
        tokio::time::timeout(Duration::from_secs(20), async {
            listener.swarm.listen_on(listen.parse().unwrap()).unwrap();
            let address = next_listen_address(&mut listener.swarm).await;
            for wave in 0_u8..2 {
                dialer.swarm.dial(address.clone().with(Protocol::P2p(listener.local_peer_id))).unwrap();
                next_connection_to_peer(&mut listener.swarm, &mut dialer.swarm, listener.local_peer_id).await;
                while [&listener, &dialer].iter().any(|node| {
                    resource_dhts(node).iter().any(|kad| kad.handler_resource_usage().handlers != 1)
                }) {
                    tokio::select! {
                        _ = listener.swarm.select_next_some() => {}
                        _ = dialer.swarm.select_next_some() => {}
                    }
                }
                for selected in 0..2 {
                    let inactive = resource_dhts(&dialer)[1 - selected];
                    let inactive_lifecycle = inactive.query_lifecycle_usage();
                    let inactive_handlers = inactive.handler_resource_usage();
                    let kad = if selected == 0 {
                        &mut dialer.swarm.behaviour_mut().kad
                    } else {
                        public_pairing_kad_mut(dialer.swarm.behaviour_mut())
                    };
                    let query = kad.put_record_to(
                        kad::Record::new(vec![wave], vec![1]),
                        [listener.local_peer_id].into_iter(),
                        kad::Quorum::One,
                    );
                    loop {
                        tokio::select! {
                            _ = listener.swarm.select_next_some() => {}
                            event = dialer.swarm.select_next_some() => {
                                let event = match event {
                                    SwarmEvent::Behaviour(BehaviourEvent::Kad(event)) if selected == 0 => Some(event),
                                    SwarmEvent::Behaviour(BehaviourEvent::PairingKad(event)) if selected == 1 => Some(event),
                                    _ => None,
                                };
                                if let Some(kad::Event::OutboundQueryProgressed {
                                    id, result: kad::QueryResult::PutRecord(result), step, ..
                                }) = event {
                                    assert_eq!(id, query);
                                    assert!(result.is_ok(), "{listen}: {result:?}");
                                    assert!(step.last);
                                    break;
                                }
                            }
                        }
                    }
                    let active = resource_dhts(&dialer)[selected];
                    assert_eq!(active.query_pool_usage().retained, 0);
                    assert_eq!(active.query_resource_snapshot(), Default::default());
                    assert_eq!(active.dial_queue_usage(), Default::default(), "swarm-initiated connections are not local Kademlia dial intents");
                    assert_eq!(active.query_lifecycle_usage(), kad::QueryLifecycleUsage {
                        admitted_phases: u64::from(wave) + 1,
                        retired_phases: u64::from(wave) + 1,
                        completed_phases: u64::from(wave) + 1,
                        requests: u64::from(wave) + 1,
                        successes: u64::from(wave) + 1,
                        ..Default::default()
                    });
                    let outgoing = active.handler_resource_usage();
                    assert_eq!(outgoing.handlers, 1);
                    assert!(outgoing.peak_pending_requests_per_handler > 0);
                    assert!(outgoing.peak_pending_bytes_per_handler > 0);
                    assert!(outgoing.peak_pending_negotiations_per_handler > 0);
                    assert!(outgoing.peak_outbound_streams_per_handler > 0);
                    assert!(resource_dhts(&listener)[selected].handler_resource_usage().peak_inbound_streams_per_handler > 0);
                    let inactive = resource_dhts(&dialer)[1 - selected];
                    assert_eq!(inactive.query_lifecycle_usage(), inactive_lifecycle);
                    assert_eq!(inactive.handler_resource_usage(), inactive_handlers);
                }
                let before = [&listener, &dialer].map(|node| resource_dhts(node).map(|kad| kad.handler_resource_usage()));
                dialer.swarm.disconnect_peer_id(listener.local_peer_id).unwrap();
                while listener.swarm.is_connected(&dialer.local_peer_id)
                    || dialer.swarm.is_connected(&listener.local_peer_id)
                    || [&listener, &dialer].iter().any(|node| {
                    resource_dhts(node).iter().any(|kad| kad.handler_resource_usage().handlers != 0)
                }) {
                    tokio::select! {
                        _ = listener.swarm.select_next_some() => {}
                        _ = dialer.swarm.select_next_some() => {}
                    }
                }
                for (node, snapshots) in [&listener, &dialer].into_iter().zip(before) {
                    for (kad, snapshot) in resource_dhts(node).into_iter().zip(snapshots) {
                        assert_eq!(kad.handler_resource_usage(), kad::HandlerResourceUsage {
                            handlers: 0,
                            usage: kad::HandlerQueueUsage::default(),
                            ..snapshot
                        });
                    }
                }
            }
        }).await.expect("loopback handler telemetry deadline");
    }
}
