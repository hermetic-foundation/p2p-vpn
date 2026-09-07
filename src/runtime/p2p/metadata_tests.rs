use std::{num::NonZeroUsize, task::Context};

use libp2p::{
    Multiaddr, PeerId, StreamProtocol, kad,
    kad::store::RecordStore,
    swarm::{ConnectionHandler, ConnectionId, NetworkBehaviour, ToSwarm},
};

type Dht = kad::Behaviour<kad::store::MemoryStore>;
type HandlerEvent =
    <<Dht as NetworkBehaviour>::ConnectionHandler as ConnectionHandler>::ToBehaviour;

fn limited_dht(payload_bytes: usize, peers: usize) -> Dht {
    let local = PeerId::random();
    let mut config = super::controlled_kademlia_config(StreamProtocol::new(
        crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
    ));
    config.set_query_metadata_limits(Some(kad::QueryMetadataLimits::new(
        NonZeroUsize::new(payload_bytes).unwrap(),
        NonZeroUsize::new(peers).unwrap(),
        kad::AddressLimits::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(16).unwrap(),
        ),
    )));
    Dht::with_config(local, kad::store::MemoryStore::new(local), config)
}

fn unbounded_dht() -> Dht {
    let local = PeerId::random();
    let mut config = kad::Config::new(StreamProtocol::new(
        crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
    ));
    config
        .set_periodic_bootstrap_interval(None)
        .set_automatic_bootstrap_throttle(None);
    Dht::with_config(local, kad::store::MemoryStore::new(local), config)
}

fn overallocated_address(address: Multiaddr) -> Multiaddr {
    let mut bytes = Vec::with_capacity(256 * 1024);
    bytes.extend_from_slice(address.as_ref());
    Multiaddr::try_from(bytes).unwrap()
}

fn assert_address_backing(actual: &Multiaddr, original: &Multiaddr, detached: bool) {
    assert_eq!(actual, original);
    let actual_bytes: &[u8] = actual.as_ref();
    let original_bytes: &[u8] = original.as_ref();
    assert_eq!(
        actual_bytes.as_ptr() != original_bytes.as_ptr(),
        detached,
        "compare backing buffers while the original allocation is still alive"
    );
}

#[test]
fn metadata_fixed_peer_backing_is_normalized_and_released() {
    let mut kad = limited_dht(8, 2);
    let mut queries = Vec::new();
    let mut slots = 0;
    for count in [0_usize, 1, 255, 300] {
        let mut peers = Vec::with_capacity(16_384);
        peers.extend((0..count).map(|_| PeerId::random()));
        let query = kad
            .try_put_record_to(
                kad::Record::new(vec![1], vec![2]),
                peers.into_iter(),
                kad::Quorum::All,
            )
            .unwrap();
        let retained = count.min(super::KADEMLIA_QUERY_CANDIDATES);
        slots += retained;
        assert_eq!(kad.query_metadata_usage().fixed_peer_slots, slots);
        queries.push((query, retained));
    }
    for (query, retained) in queries {
        kad.query_mut(&query).unwrap().finish();
        assert_eq!(kad.query_metadata_usage().fixed_peer_slots, slots);
        assert!(kad.cancel_query(&query));
        slots -= retained;
        assert_eq!(kad.query_metadata_usage().fixed_peer_slots, slots);
    }
    let query = kad
        .try_put_record_to(
            kad::Record::new(vec![1], vec![2]),
            [PeerId::random()].into_iter(),
            kad::Quorum::One,
        )
        .unwrap();
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    assert!(kad.poll(&mut cx).is_ready());
    assert_eq!(kad.query_metadata_usage().fixed_peer_slots, 1);
    kad.query_mut(&query).unwrap().finish();
    assert!(kad.poll(&mut cx).is_ready());
    assert_eq!(kad.query_metadata_usage().fixed_peer_slots, 0);
    assert_eq!(kad.pending_rpc_usage().requests, 0);

    let mut kad = unbounded_dht();
    let query = kad
        .try_put_record_to(
            kad::Record::new(vec![1], vec![2]),
            (0..300).map(|_| PeerId::random()),
            kad::Quorum::All,
        )
        .unwrap();
    assert!(kad.query_metadata_usage().fixed_peer_slots >= 300);
    assert!(kad.query(&query).unwrap().resource_usage().is_none());
    assert!(kad.cancel_query(&query));
}

#[test]
fn metadata_query_addresses_detach_on_learning_and_replacement() {
    for bounded in [false, true] {
        let mut kad = if bounded {
            limited_dht(8, 2)
        } else {
            unbounded_dht()
        };
        let source = PeerId::random();
        let peer = PeerId::random();
        let original = overallocated_address(format!("/memory/1/p2p/{peer}").parse().unwrap());
        let replacement = overallocated_address(format!("/memory/2/p2p/{peer}").parse().unwrap());
        kad.add_address(&source, "/memory/100".parse().unwrap());
        let query = kad.try_get_closest_peers(vec![1]).unwrap();
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        while kad.poll(&mut cx).is_ready() {}
        kad.on_connection_handler_event(
            source,
            ConnectionId::new_unchecked(1),
            HandlerEvent::FindNodeRes {
                closer_peers: vec![kad::KadPeer {
                    node_id: peer,
                    multiaddrs: vec![original.clone()],
                    connection_ty: kad::ConnectionType::NotConnected,
                }],
                query_id: query,
            },
        );
        let addresses = kad
            .handle_pending_outbound_connection(
                ConnectionId::new_unchecked(2),
                Some(peer),
                &[],
                libp2p::core::Endpoint::Dialer,
            )
            .unwrap();
        assert_eq!(addresses.len(), 1);
        assert_address_backing(&addresses[0], &original, bounded);
        let endpoint = |address| libp2p::core::ConnectedPoint::Dialer {
            address,
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        kad.on_swarm_event(libp2p::swarm::FromSwarm::AddressChange(
            libp2p::swarm::behaviour::AddressChange {
                peer_id: peer,
                connection_id: ConnectionId::new_unchecked(2),
                old: &endpoint(original.clone()),
                new: &endpoint(replacement.clone()),
            },
        ));
        let current = kad
            .handle_pending_outbound_connection(
                ConnectionId::new_unchecked(2),
                Some(peer),
                &[],
                libp2p::core::Endpoint::Dialer,
            )
            .unwrap();
        assert_eq!(current.len(), 1);
        assert_address_backing(&current[0], &replacement, bounded);
        if bounded {
            assert_eq!(
                kad.query(&query)
                    .unwrap()
                    .resource_usage()
                    .unwrap()
                    .address_bytes,
                replacement.len()
            );
        }
        assert!(kad.cancel_query(&query));
        assert!(
            kad.handle_pending_outbound_connection(
                ConnectionId::new_unchecked(2),
                Some(peer),
                &[],
                libp2p::core::Endpoint::Dialer,
            )
            .unwrap()
            .is_empty()
        );
    }
}

#[test]
fn metadata_routing_addresses_detach_and_preserve_snapshot_sharing() {
    for (per_peer, aggregate) in [(false, false), (true, false), (false, true), (true, true)] {
        let local = PeerId::random();
        let peer = PeerId::random();
        let mut config = kad::Config::new(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config
            .set_periodic_bootstrap_interval(None)
            .set_automatic_bootstrap_throttle(None);
        if per_peer {
            config.set_address_limits(kad::AddressLimits::new(
                NonZeroUsize::new(4).unwrap(),
                NonZeroUsize::new(2048).unwrap(),
            ));
        }
        if aggregate {
            config.set_routing_limits(kad::RoutingLimits::new(
                NonZeroUsize::new(8).unwrap(),
                NonZeroUsize::new(2048).unwrap(),
            ));
        }
        let mut kad = Dht::with_config(local, kad::store::MemoryStore::new(local), config);
        let originals: Vec<_> = (1..=4)
            .map(|port| {
                overallocated_address(format!("/memory/{port}/p2p/{peer}").parse().unwrap())
            })
            .collect();
        assert_eq!(
            kad.add_protected_address(&peer, originals[0].clone()),
            kad::RoutingUpdate::Success
        );
        let waker = futures::task::noop_waker();
        while kad.poll(&mut Context::from_waker(&waker)).is_ready() {}
        let mut addresses = kad.remove_peer(&peer).unwrap().node.value;
        assert_address_backing(addresses.first(), &originals[0], per_peer || aggregate);
        let snapshot = addresses.clone();
        assert_address_backing(snapshot.first(), addresses.first(), false);
        assert!(addresses.insert(originals[1].clone()));
        assert_address_backing(
            addresses.iter().find(|a| *a == &originals[1]).unwrap(),
            &originals[1],
            per_peer || aggregate,
        );
        assert!(addresses.replace(&originals[0], &originals[2]));
        assert!(addresses.replace(&originals[1], &originals[3]));
        assert_eq!(addresses.len(), 3);
        for original in [&originals[0], &originals[2], &originals[3]] {
            assert_address_backing(
                addresses.iter().find(|a| *a == original).unwrap(),
                original,
                per_peer || aggregate,
            );
        }
        assert_address_backing(snapshot.first(), addresses.first(), false);
        if aggregate {
            assert_eq!(
                kad.routing_resource_usage().address_bytes,
                originals[0].len() + originals[2].len() + originals[3].len()
            );
        }
        drop(snapshot);
        drop(addresses);
        assert_eq!(kad.routing_resource_usage().entries, 0);
        assert_eq!(kad.routing_resource_usage().address_bytes, 0);
    }
}

#[test]
fn metadata_input_rejection_has_no_query_or_store_side_effects() {
    let mut kad = limited_dht(8, 2);
    let key = kad::RecordKey::new(&[1; 9]);
    let record = kad::Record::new(key.clone(), vec![2]);
    for error in [
        kad.try_get_closest_peers(vec![1; 9]).unwrap_err(),
        kad.try_get_n_closest_peers(vec![1; 9], NonZeroUsize::MIN)
            .unwrap_err(),
        kad.try_get_record(key.clone()).unwrap_err(),
        kad.try_get_providers(key.clone()).unwrap_err(),
        kad.try_start_providing(key.clone()).unwrap_err(),
        kad.try_put_record(record.clone(), kad::Quorum::One)
            .unwrap_err(),
        kad.try_put_record_to(
            record.clone(),
            [PeerId::random()].into_iter(),
            kad::Quorum::One,
        )
        .unwrap_err(),
    ] {
        assert!(matches!(
            error,
            kad::QueryStartError::InputTooLarge(kad::QueryInputTooLarge { limit: 8, .. })
        ));
    }
    assert_eq!(kad.query_pool_usage().retained, 0);
    assert_eq!(kad.query_metadata_usage().rejected_inputs, 7);
    assert!(kad.store_mut().get(&key).is_none());
    assert!(kad.store_mut().providers(&key).is_empty());
    for id in [
        kad.get_closest_peers(vec![1; 9]),
        kad.get_record(key.clone()),
        kad.get_providers(key.clone()),
        kad.start_providing(key.clone()).unwrap(),
        kad.put_record(record.clone(), kad::Quorum::One).unwrap(),
        kad.put_record_to(record, [PeerId::random()].into_iter(), kad::Quorum::One),
    ] {
        assert!(!kad.query_is_retained(&id));
    }
    assert!(kad.store_mut().get(&key).is_none());
    assert!(kad.store_mut().providers(&key).is_empty());
    let waker = futures::task::noop_waker();
    assert!(kad.poll(&mut Context::from_waker(&waker)).is_pending());
}

#[test]
fn metadata_is_aggregate_across_finished_entries_and_normalizes_spare_capacity() {
    let mut kad = limited_dht(8, 2);
    let mut queries = Vec::new();
    for _ in 0..32 {
        let mut key = Vec::with_capacity(1024);
        key.push(1);
        let mut value = Vec::with_capacity(4096);
        value.resize(7, 2);
        let id = kad
            .try_put_record_to(
                kad::Record::new(key, value),
                [PeerId::random()].into_iter(),
                kad::Quorum::One,
            )
            .unwrap();
        let state = kad.query(&id).unwrap();
        let kad::QueryInfo::PutRecord { record, .. } = state.info() else {
            panic!("wrong query")
        };
        assert_eq!(record.value.capacity(), 7);
        queries.push(id);
    }
    assert_eq!(kad.query_metadata_usage().payload_bytes, 32 * 8);
    kad.query_mut(&queries[0]).unwrap().finish();
    assert_eq!(kad.query_metadata_usage().payload_bytes, 32 * 8);
    assert!(matches!(
        kad.try_get_record(kad::RecordKey::new(&[1])),
        Err(kad::QueryStartError::Capacity(_))
    ));
    assert!(kad.cancel_query(&queries.remove(0)));
    assert_eq!(kad.query_metadata_usage().payload_bytes, 31 * 8);
    let next = kad.try_get_record(kad::RecordKey::new(&[1; 8])).unwrap();
    assert_eq!(kad.query_metadata_usage().payload_bytes, 32 * 8);
    for id in queries.into_iter().chain([next]) {
        assert!(kad.cancel_query(&id));
    }
    assert_eq!(kad.query_metadata_usage().payload_bytes, 0);
}

#[test]
fn metadata_result_lists_bound_duplicates_cache_candidates_and_quorum() {
    let mut kad = limited_dht(8, 2);
    let peers = [PeerId::random(), PeerId::random(), PeerId::random()];
    let query = kad
        .try_put_record_to(
            kad::Record::new(vec![1], vec![2]),
            peers.into_iter(),
            kad::Quorum::All,
        )
        .unwrap();
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    while kad.poll(&mut cx).is_ready() {}
    for peer in std::iter::repeat_n(peers[0], 64).chain([PeerId::random(), peers[1]]) {
        kad.on_connection_handler_event(
            peer,
            ConnectionId::new_unchecked(1),
            HandlerEvent::PutRecordRes {
                key: kad::RecordKey::new(&[1]),
                value: vec![2],
                query_id: query,
            },
        );
        assert!(kad.query_metadata_usage().result_peers <= 2);
    }
    assert_eq!(kad.query(&query).unwrap().stats().num_successes(), 2);
    assert!(
        kad.poll(&mut cx).is_pending(),
        "duplicates must not satisfy quorum"
    );
    kad.on_connection_handler_event(
        peers[2],
        ConnectionId::new_unchecked(1),
        HandlerEvent::PutRecordRes {
            key: kad::RecordKey::new(&[1]),
            value: vec![2],
            query_id: query,
        },
    );
    assert_eq!(kad.query_metadata_usage().result_peers, 2);
    let std::task::Poll::Ready(ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
        id,
        result: kad::QueryResult::PutRecord(Ok(_)),
        stats,
        ..
    })) = kad.poll(&mut cx)
    else {
        panic!("three unique acknowledgements must satisfy the original quorum")
    };
    assert_eq!(id, query);
    assert_eq!(stats.num_successes(), 3);

    let mut config = super::controlled_kademlia_config(StreamProtocol::new(
        crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
    ));
    config.set_caching(kad::Caching::Enabled { max_peers: 64 });
    config.set_query_metadata_limits(Some(kad::QueryMetadataLimits::new(
        NonZeroUsize::new(8).unwrap(),
        NonZeroUsize::new(2).unwrap(),
        kad::AddressLimits::default(),
    )));
    let local = PeerId::random();
    let mut kad = Dht::with_config(local, kad::store::MemoryStore::new(local), config);
    let query = kad.try_get_record(kad::RecordKey::new(&[1])).unwrap();
    for _ in 0..64 {
        kad.on_connection_handler_event(
            PeerId::random(),
            ConnectionId::new_unchecked(1),
            HandlerEvent::GetRecordRes {
                record: None,
                closer_peers: Vec::new(),
                query_id: query,
            },
        );
        assert!(kad.query_metadata_usage().result_peers <= 2);
    }
    assert_eq!(kad.query_metadata_usage().result_peers, 2);
    assert!(kad.cancel_query(&query));
    assert_eq!(kad.query_metadata_usage().result_peers, 0);
}

#[test]
fn metadata_result_limit_does_not_lower_required_quorum() {
    let mut kad = limited_dht(8, 1);
    let peers = [PeerId::random(), PeerId::random(), PeerId::random()];
    let query = kad
        .try_put_record_to(
            kad::Record::new(vec![1], vec![2]),
            peers.into_iter(),
            kad::Quorum::All,
        )
        .unwrap();
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    while kad.poll(&mut cx).is_ready() {}
    for peer in &peers[..2] {
        kad.on_connection_handler_event(
            *peer,
            ConnectionId::new_unchecked(1),
            HandlerEvent::PutRecordRes {
                key: kad::RecordKey::new(&[1]),
                value: vec![2],
                query_id: query,
            },
        );
    }
    assert_eq!(kad.query_metadata_usage().result_peers, 1);
    assert_eq!(kad.query(&query).unwrap().stats().num_successes(), 2);
    kad.query_mut(&query).unwrap().finish();
    let std::task::Poll::Ready(ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
        id,
        result:
            kad::QueryResult::PutRecord(Err(kad::PutRecordError::QuorumFailed {
                success, quorum, ..
            })),
        stats,
        ..
    })) = kad.poll(&mut cx)
    else {
        panic!("partial completion must preserve the required quorum")
    };
    assert_eq!(id, query);
    assert_eq!(success, vec![peers[0]]);
    assert_eq!(quorum.get(), 3);
    assert_eq!(stats.num_successes(), 2);
    assert_eq!(kad.query_metadata_usage().result_peers, 0);
}

#[test]
fn metadata_provider_addresses_are_bounded_during_phase_transition() {
    let mut kad = limited_dht(8, 2);
    let originals: Vec<_> = (0..10)
        .map(|index| overallocated_address(format!("/memory/{index}").parse().unwrap()))
        .collect();
    for address in &originals {
        kad.on_swarm_event(libp2p::swarm::FromSwarm::ExternalAddrConfirmed(
            libp2p::swarm::behaviour::ExternalAddrConfirmed { addr: address },
        ));
    }
    let oversized = format!("/dns4/{}.example/tcp/1", "a".repeat(64))
        .parse()
        .unwrap();
    kad.on_swarm_event(libp2p::swarm::FromSwarm::ExternalAddrConfirmed(
        libp2p::swarm::behaviour::ExternalAddrConfirmed { addr: &oversized },
    ));
    let remote = PeerId::random();
    kad.add_address(&remote, "/memory/100".parse().unwrap());
    let query = kad
        .try_start_providing(kad::RecordKey::new(&[1]))
        .unwrap()
        .unwrap();
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    while kad.poll(&mut cx).is_ready() {}
    kad.on_connection_handler_event(
        remote,
        ConnectionId::new_unchecked(1),
        HandlerEvent::FindNodeRes {
            closer_peers: Vec::new(),
            query_id: query,
        },
    );
    while kad.poll(&mut cx).is_ready() {}
    let state = kad.query(&query).unwrap();
    let kad::QueryInfo::AddProvider {
        phase: kad::AddProviderPhase::AddProvider {
            external_addresses, ..
        },
        ..
    } = state.info()
    else {
        panic!("provider phase")
    };
    assert_eq!(external_addresses.len(), 2);
    assert_eq!(external_addresses.capacity(), 2);
    assert!(external_addresses.iter().all(|address| address.len() <= 16));
    for address in external_addresses {
        let original = originals
            .iter()
            .find(|original| *original == address)
            .unwrap();
        assert_address_backing(address, original, true);
    }
    assert_eq!(kad.query_metadata_usage().provider_addresses, 2);
    assert!(kad.cancel_query(&query));
    assert_eq!(kad.query_metadata_usage().provider_addresses, 0);
}
