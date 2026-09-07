use std::{
    collections::HashSet,
    num::NonZeroUsize,
    task::{Context, Poll},
};

use libp2p::{
    PeerId, StreamProtocol,
    core::ConnectedPoint,
    kad,
    kad::store::RecordStore,
    swarm::{ConnectionHandler, ConnectionId, NetworkBehaviour, NotifyHandler, ToSwarm},
};

type Dht = kad::Behaviour<kad::store::MemoryStore>;
type HandlerEvent =
    <<Dht as NetworkBehaviour>::ConnectionHandler as ConnectionHandler>::ToBehaviour;
type HandlerInput =
    <<Dht as NetworkBehaviour>::ConnectionHandler as ConnectionHandler>::FromBehaviour;

fn limited_dht(events: usize, bytes: usize) -> Dht {
    configured_dht(events, bytes, kad::StoreInserts::Unfiltered)
}

fn configured_dht(events: usize, bytes: usize, filtering: kad::StoreInserts) -> Dht {
    configured_dht_with_timeout(events, bytes, filtering, std::time::Duration::from_secs(10))
}

fn configured_dht_with_timeout(
    events: usize,
    bytes: usize,
    filtering: kad::StoreInserts,
    timeout: std::time::Duration,
) -> Dht {
    let local = PeerId::random();
    let mut config = super::controlled_kademlia_config(StreamProtocol::new(
        crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
    ));
    config.set_behaviour_queue_limits(kad::BehaviourQueueLimits::new(
        NonZeroUsize::new(events).unwrap(),
        NonZeroUsize::new(bytes).unwrap(),
    ));
    config.set_periodic_bootstrap_interval(None);
    config.set_automatic_bootstrap_throttle(None);
    config.set_record_filtering(filtering);
    config.set_query_timeout(timeout);
    Dht::with_config(local, kad::store::MemoryStore::new(local), config)
}

#[test]
fn queue_continuous_inbound_work_cannot_strand_query_retirement() {
    let mut kad = configured_dht_with_timeout(
        4,
        64,
        kad::StoreInserts::FilterBoth,
        std::time::Duration::ZERO,
    );
    kad.store_mut()
        .put(kad::Record::new(vec![1], vec![2]))
        .unwrap();
    let finished = kad.try_get_record(kad::RecordKey::new(&[1])).unwrap();
    kad.query_mut(&finished).unwrap().finish();
    let expired = kad.try_get_record(kad::RecordKey::new(&[1])).unwrap();
    let peer = PeerId::random();
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut found = HashSet::new();
    let mut terminal = HashSet::new();
    for _ in 0..16 {
        for _ in 0..8 {
            kad.on_connection_handler_event(
                peer,
                ConnectionId::new_unchecked(1),
                HandlerEvent::AddProvider {
                    key: kad::RecordKey::new(&[1]),
                    provider: kad::KadPeer {
                        node_id: peer,
                        multiaddrs: Vec::new(),
                        connection_ty: kad::ConnectionType::Connected,
                    },
                },
            );
        }
        assert_eq!(kad.behaviour_queue_usage().events, 4);
        if let Poll::Ready(ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
            id,
            step,
            ..
        })) = kad.poll(&mut cx)
        {
            if step.last {
                assert!(
                    found.contains(&id),
                    "terminal overtook previously admitted progress"
                );
                assert!(terminal.insert(id));
            } else {
                assert!(found.insert(id));
            }
        }
        // A late response must not replenish progress after graceful finish or expiry.
        for id in [finished, expired] {
            let before = kad.behaviour_queue_usage().events;
            kad.on_connection_handler_event(
                peer,
                ConnectionId::new_unchecked(1),
                HandlerEvent::GetRecordRes {
                    record: Some(kad::Record::new(vec![1], vec![2])),
                    closer_peers: Vec::new(),
                    query_id: id,
                },
            );
            assert_eq!(kad.behaviour_queue_usage().events, before);
        }
    }
    assert_eq!(terminal, HashSet::from([finished, expired]));
    assert_eq!(kad.query_pool_usage().retained, 0);
    assert_eq!(kad.query_metadata_usage().payload_bytes, 0);
    assert!(kad.behaviour_queue_usage().rejected > 0);
    assert!(kad.try_get_record(kad::RecordKey::new(&[1])).is_ok());
}

#[test]
fn queue_filtered_provider_payloads_are_aggregate_and_release_on_dispatch() {
    let mut kad = configured_dht(8, 64, kad::StoreInserts::FilterBoth);
    let peer = PeerId::random();
    for _ in 0..32 {
        kad.on_connection_handler_event(
            peer,
            ConnectionId::new_unchecked(1),
            HandlerEvent::AddProvider {
                key: kad::RecordKey::new(&[1; 32]),
                provider: kad::KadPeer {
                    node_id: peer,
                    multiaddrs: Vec::new(),
                    connection_ty: kad::ConnectionType::Connected,
                },
            },
        );
        assert!(kad.behaviour_queue_usage().bytes <= 64);
    }
    assert_eq!(kad.behaviour_queue_usage().events, 2);
    assert_eq!(kad.behaviour_queue_usage().bytes, 64);
    assert_eq!(kad.behaviour_queue_usage().rejected, 30);
    assert!(
        kad.store_mut()
            .providers(&kad::RecordKey::new(&[1; 32]))
            .is_empty()
    );
    assert_eq!(drain(&mut kad).len(), 2);
    assert_eq!(kad.behaviour_queue_usage().bytes, 0);
    // Empty address lists still own their vector allocation and must be charged.
    kad.on_connection_handler_event(
        peer,
        ConnectionId::new_unchecked(1),
        HandlerEvent::AddProvider {
            key: kad::RecordKey::new(&[1]),
            provider: kad::KadPeer {
                node_id: peer,
                multiaddrs: Vec::with_capacity(128),
                connection_ty: kad::ConnectionType::Connected,
            },
        },
    );
    assert_eq!(kad.behaviour_queue_usage().events, 0);
    assert_eq!(kad.behaviour_queue_usage().rejected, 31);
}

#[test]
fn queue_provider_dispatch_rejection_never_reports_success() {
    let mut kad = limited_dht(8, 8);
    let peer = PeerId::random();
    let connection = ConnectionId::new_unchecked(1);
    connect(&mut kad, connection, peer);
    kad.add_address(&peer, "/memory/1".parse().unwrap());
    let external = "/ip4/192.0.2.1/tcp/1234".parse().unwrap();
    kad.on_swarm_event(libp2p::swarm::FromSwarm::ExternalAddrConfirmed(
        libp2p::swarm::behaviour::ExternalAddrConfirmed { addr: &external },
    ));
    drain(&mut kad);
    let id = kad
        .try_start_providing(kad::RecordKey::new(&[1]))
        .unwrap()
        .unwrap();
    assert!(
        matches!(&drain(&mut kad)[..], [ToSwarm::NotifyHandler { event: HandlerInput::FindNodeReq { query_id, .. }, .. }] if *query_id == id)
    );
    kad.on_connection_handler_event(
        peer,
        connection,
        HandlerEvent::FindNodeRes {
            closer_peers: Vec::new(),
            query_id: id,
        },
    );
    assert!(
        matches!(&drain(&mut kad)[..], [ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
        id: finished, result: kad::QueryResult::StartProviding(Err(kad::AddProviderError::NoPeersReached { .. })), step, ..
    })] if *finished == id && step.last)
    );
    assert_eq!(kad.query_pool_usage().retained, 0);
    assert_eq!(kad.pending_rpc_usage().requests, 0);
    assert_eq!(kad.behaviour_queue_usage().bytes, 0);
    assert!(kad.behaviour_queue_usage().rejected >= 1);
}

fn drain(kad: &mut Dht) -> Vec<ToSwarm<kad::Event, HandlerInput>> {
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut events = Vec::new();
    for _ in 0..4096 {
        match kad.poll(&mut cx) {
            Poll::Ready(event) => events.push(event),
            Poll::Pending => return events,
        }
    }
    panic!("behaviour did not settle");
}

fn record_found(kad: &Dht, id: &kad::QueryId) -> bool {
    let query = kad.query(id).unwrap();
    let kad::QueryInfo::GetRecord { found_a_record, .. } = query.info() else {
        panic!("expected record query")
    };
    *found_a_record
}

#[test]
fn queue_count_cancellation_and_churn_preserve_unrelated_results() {
    let mut kad = limited_dht(2, 100);
    kad.store_mut()
        .put(kad::Record::new(vec![1], vec![2; 4]))
        .unwrap();
    for _ in 0..16 {
        let first = kad.try_get_record(kad::RecordKey::new(&[1])).unwrap();
        let second = kad.try_get_record(kad::RecordKey::new(&[1])).unwrap();
        let rejected = kad.try_get_record(kad::RecordKey::new(&[1])).unwrap();
        assert_eq!(kad.behaviour_queue_usage().events, 2);
        assert_eq!(kad.behaviour_queue_usage().bytes, 10);
        assert!(record_found(&kad, &first));
        assert!(record_found(&kad, &second));
        assert!(!record_found(&kad, &rejected));
        kad.query_mut(&first).unwrap().finish();
        assert_eq!(kad.behaviour_queue_usage().bytes, 10);
        assert!(kad.cancel_query(&first));
        assert_eq!(kad.behaviour_queue_usage().bytes, 5);
        let next = kad.try_get_record(kad::RecordKey::new(&[1])).unwrap();
        assert_eq!(kad.behaviour_queue_usage().events, 2);
        let mut finished = HashSet::new();
        let mut found = HashSet::new();
        for event in drain(&mut kad) {
            let ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
                id,
                result,
                step,
                ..
            }) = event
            else {
                panic!("unexpected event")
            };
            assert_ne!(id, first, "canceled progress survived");
            if step.last {
                assert!(finished.insert(id));
                if id == rejected {
                    assert!(matches!(
                        result,
                        kad::QueryResult::GetRecord(Err(kad::GetRecordError::NotFound { .. }))
                    ));
                } else {
                    assert!(found.contains(&id), "terminal result overtook progress");
                }
            } else {
                assert!(matches!(
                    result,
                    kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(_)))
                ));
                assert!(found.insert(id));
            }
        }
        assert_eq!(found, HashSet::from([second, next]));
        assert_eq!(finished, HashSet::from([second, rejected, next]));
        assert_eq!(kad.behaviour_queue_usage().events, 0);
        assert_eq!(kad.behaviour_queue_usage().bytes, 0);
        assert_eq!(kad.query_pool_usage().retained, 0);
        assert_eq!(kad.query_metadata_usage().payload_bytes, 0);
    }
    assert_eq!(kad.behaviour_queue_usage().rejected, 16);
}

#[test]
fn queue_bytes_reject_large_or_spare_capacity_record_progress_without_false_success() {
    let mut kad = limited_dht(8, 10);
    let source = PeerId::random();
    let mut ids = Vec::new();
    for capacity in [128, 4, 4, 4] {
        let id = kad.try_get_record(kad::RecordKey::new(&[1])).unwrap();
        let mut value = Vec::with_capacity(capacity);
        value.resize(4, 2);
        kad.on_connection_handler_event(
            source,
            ConnectionId::new_unchecked(1),
            HandlerEvent::GetRecordRes {
                record: Some(kad::Record::new(vec![1], value)),
                closer_peers: Vec::new(),
                query_id: id,
            },
        );
        ids.push(id);
    }
    assert_eq!(kad.behaviour_queue_usage().events, 2);
    assert_eq!(kad.behaviour_queue_usage().bytes, 10);
    assert_eq!(kad.behaviour_queue_usage().rejected, 2);
    assert!(!record_found(&kad, &ids[0]));
    assert!(!record_found(&kad, &ids[3]));
    for id in ids {
        assert!(kad.cancel_query(&id));
    }
    assert_eq!(kad.behaviour_queue_usage().bytes, 0);
    assert!(drain(&mut kad).is_empty());
}

#[test]
fn queue_provider_progress_counts_only_admitted_events() {
    let mut kad = limited_dht(1, 1024);
    let key = kad::RecordKey::new(&[1]);
    let provider = PeerId::random();
    kad.store_mut()
        .add_provider(kad::ProviderRecord::new(key.clone(), provider, Vec::new()))
        .unwrap();
    let first = kad.try_get_providers(key.clone()).unwrap();
    let rejected = kad.try_get_providers(key).unwrap();
    for (id, count) in [(first, 1), (rejected, 0)] {
        let query = kad.query(&id).unwrap();
        let kad::QueryInfo::GetProviders {
            providers_found, ..
        } = query.info()
        else {
            panic!("provider query")
        };
        assert_eq!(*providers_found, count);
    }
    let bytes = kad.behaviour_queue_usage().bytes;
    assert!(bytes > 1 && bytes <= 1024);
    assert!(kad.cancel_query(&rejected));
    assert_eq!(kad.behaviour_queue_usage().bytes, bytes);
    let events = drain(&mut kad);
    assert_eq!(events.len(), 2);
    assert!(
        matches!(&events[0], ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
        id, result: kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders { providers, .. })), step, ..
    }) if *id == first && providers.contains(&provider) && !step.last)
    );
    assert!(
        matches!(&events[1], ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed { id, step, .. }) if *id == first && step.last)
    );
    assert_eq!(kad.behaviour_queue_usage().bytes, 0);
}

fn connect(kad: &mut Dht, connection: ConnectionId, peer: PeerId) {
    let address = "/memory/1".parse().unwrap();
    let _handler = kad
        .handle_established_inbound_connection(connection, peer, &address, &address)
        .unwrap();
    kad.on_swarm_event(libp2p::swarm::FromSwarm::ConnectionEstablished(
        libp2p::swarm::behaviour::ConnectionEstablished {
            peer_id: peer,
            connection_id: connection,
            endpoint: &ConnectedPoint::Listener {
                local_addr: address.clone(),
                send_back_addr: address,
            },
            failed_addresses: &[],
            other_established: 0,
        },
    ));
}

#[test]
fn routing_only_budget_detaches_raw_notification_backing() {
    let local = PeerId::random();
    let peer = PeerId::random();
    let mut config = kad::Config::new(StreamProtocol::new(
        crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
    ));
    config
        .set_periodic_bootstrap_interval(None)
        .set_automatic_bootstrap_throttle(None)
        .set_kbucket_inserts(kad::BucketInserts::Manual)
        .set_routing_limits(kad::RoutingLimits::new(
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(8192).unwrap(),
        ));
    let mut kad = Dht::with_config(local, kad::store::MemoryStore::new(local), config);
    let connection = ConnectionId::new_unchecked(1);
    connect(&mut kad, connection, peer);
    drain(&mut kad);
    let encoded: libp2p::Multiaddr = format!("/memory/1/p2p/{peer}").parse().unwrap();
    let mut bytes = Vec::with_capacity(256 * 1024);
    bytes.extend_from_slice(encoded.as_ref());
    let original = libp2p::Multiaddr::try_from(bytes).unwrap();
    kad.on_connection_handler_event(
        peer,
        connection,
        HandlerEvent::ProtocolConfirmed {
            endpoint: ConnectedPoint::Dialer {
                address: original.clone(),
                role_override: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::Reuse,
            },
        },
    );
    assert_eq!(kad.behaviour_queue_usage().event_limit, None);
    assert_eq!(kad.routing_resource_usage().address_bytes, original.len());
    let events = drain(&mut kad);
    let [ToSwarm::GenerateEvent(kad::Event::RoutablePeer { address, .. })] = &events[..] else {
        panic!("raw routing notification")
    };
    assert_eq!(address, &original);
    let retained: &[u8] = address.as_ref();
    let source: &[u8] = original.as_ref();
    assert_ne!(retained.as_ptr(), source.as_ptr());
    assert_eq!(kad.routing_resource_usage().address_bytes, 0);
}

#[test]
fn queue_overflow_does_not_discard_latest_mode_or_close_connections() {
    let mut kad = limited_dht(1, 8);
    let mut connections = HashSet::new();
    for id in 1..=3 {
        let connection = ConnectionId::new_unchecked(id);
        connect(&mut kad, connection, PeerId::random());
        connections.insert(connection);
    }
    drain(&mut kad);
    kad.store_mut()
        .put(kad::Record::new(vec![1], vec![2]))
        .unwrap();
    let query = kad.try_get_record(kad::RecordKey::new(&[1])).unwrap();
    for _ in 0..512 {
        kad.set_mode(Some(kad::Mode::Client));
        kad.set_mode(Some(kad::Mode::Server));
    }
    assert_eq!(kad.behaviour_queue_usage().events, 1);
    let mut terminal = false;
    for event in drain(&mut kad) {
        match event {
            ToSwarm::NotifyHandler {
                handler: NotifyHandler::One(id),
                event:
                    HandlerInput::ReconfigureMode {
                        new_mode: kad::Mode::Server,
                    },
                ..
            } => {
                assert!(connections.remove(&id));
            }
            ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed { id, step, .. }) => {
                assert_eq!(id, query);
                terminal |= step.last;
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }
    assert!(connections.is_empty());
    assert!(terminal);
    assert_eq!(kad.behaviour_queue_usage().bytes, 0);
    assert_eq!(kad.query_pool_usage().retained, 0);
}

#[test]
fn queue_outbound_byte_rejection_fails_query_and_readmits_after_overload() {
    let mut kad = limited_dht(2, 8);
    let peer = PeerId::random();
    let connection = ConnectionId::new_unchecked(1);
    connect(&mut kad, connection, peer);
    drain(&mut kad);
    let rejected = kad
        .try_put_record_to(
            kad::Record::new(vec![1], vec![2; 8]),
            [peer].into_iter(),
            kad::Quorum::One,
        )
        .unwrap();
    let events = drain(&mut kad);
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
        id, result: kad::QueryResult::PutRecord(Err(kad::PutRecordError::QuorumFailed { success, .. })), step, ..
    }) if *id == rejected && success.is_empty() && step.last)
    );
    assert_eq!(kad.behaviour_queue_usage().rejected, 1);
    assert_eq!(kad.pending_rpc_usage().requests, 0);
    assert_eq!(kad.query_pool_usage().retained, 0);
    let accepted = kad
        .try_put_record_to(
            kad::Record::new(vec![1], vec![2]),
            [peer].into_iter(),
            kad::Quorum::One,
        )
        .unwrap();
    assert!(
        matches!(&drain(&mut kad)[..], [ToSwarm::NotifyHandler { event: HandlerInput::PutRecord { query_id, .. }, .. }] if *query_id == accepted)
    );
    kad.on_connection_handler_event(
        peer,
        connection,
        HandlerEvent::PutRecordRes {
            key: kad::RecordKey::new(&[1]),
            value: vec![2],
            query_id: accepted,
        },
    );
    assert!(
        matches!(&drain(&mut kad)[..], [ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
        id, result: kad::QueryResult::PutRecord(Ok(_)), step, ..
    })] if *id == accepted && step.last)
    );
    assert_eq!(kad.behaviour_queue_usage().bytes, 0);
    assert_eq!(kad.query_pool_usage().retained, 0);
}
