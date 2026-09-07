use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    task::Context,
    time::{Duration, Instant},
};

use futures::StreamExt;
use libp2p::{
    PeerId, StreamProtocol, kad,
    kad::store::RecordStore,
    multiaddr::Protocol,
    swarm::{NetworkBehaviour, SwarmEvent},
};

#[derive(Default)]
struct OrderedStore {
    records: BTreeMap<Vec<u8>, kad::Record>,
    providers: BTreeMap<Vec<u8>, kad::ProviderRecord>,
}

impl RecordStore for OrderedStore {
    type RecordsIter<'a> = std::iter::Map<
        std::collections::btree_map::Values<'a, Vec<u8>, kad::Record>,
        fn(&'a kad::Record) -> Cow<'a, kad::Record>,
    >;
    type ProvidedIter<'a> = std::iter::Map<
        std::collections::btree_map::Values<'a, Vec<u8>, kad::ProviderRecord>,
        fn(&'a kad::ProviderRecord) -> Cow<'a, kad::ProviderRecord>,
    >;

    fn get(&self, key: &kad::RecordKey) -> Option<Cow<'_, kad::Record>> {
        self.records.get(key.as_ref()).map(Cow::Borrowed)
    }

    fn put(&mut self, record: kad::Record) -> kad::store::Result<()> {
        self.records.insert(record.key.to_vec(), record);
        Ok(())
    }

    fn remove(&mut self, key: &kad::RecordKey) {
        self.records.remove(key.as_ref());
    }

    fn records(&self) -> Self::RecordsIter<'_> {
        self.records.values().map(Cow::Borrowed)
    }

    fn add_provider(&mut self, record: kad::ProviderRecord) -> kad::store::Result<()> {
        self.providers.insert(record.key.to_vec(), record);
        Ok(())
    }

    fn providers(&self, key: &kad::RecordKey) -> Vec<kad::ProviderRecord> {
        self.providers
            .get(key.as_ref())
            .cloned()
            .into_iter()
            .collect()
    }

    fn provided(&self) -> Self::ProvidedIter<'_> {
        self.providers.values().map(Cow::Borrowed)
    }

    fn remove_provider(&mut self, key: &kad::RecordKey, peer: &PeerId) {
        if self
            .providers
            .get(key.as_ref())
            .is_some_and(|record| record.provider == *peer)
        {
            self.providers.remove(key.as_ref());
        }
    }
}

fn job_config(limits: Option<kad::BackgroundJobLimits>) -> kad::Config {
    let mut config = kad::Config::new(StreamProtocol::new("/p2p-vpn/test/jobs/1"));
    config
        .set_periodic_bootstrap_interval(None)
        .set_automatic_bootstrap_throttle(None)
        .set_query_pool_capacity(NonZeroUsize::new(2).unwrap())
        .set_background_query_limits(NonZeroUsize::new(2).unwrap(), NonZeroUsize::MIN)
        .set_provider_publication_interval(Some(Duration::from_millis(100)))
        .set_replication_interval(Some(Duration::from_millis(100)))
        .set_publication_interval(Some(Duration::from_millis(100)));
    if let Some(limits) = limits {
        config.set_background_job_limits(limits);
    }
    config
}

fn limits(keys: usize, bytes: usize, input: usize) -> kad::BackgroundJobLimits {
    kad::BackgroundJobLimits::new(
        NonZeroUsize::new(keys).unwrap(),
        NonZeroUsize::new(bytes).unwrap(),
        NonZeroUsize::new(input).unwrap(),
    )
}

fn poll_jobs(
    kad: &mut kad::Behaviour<OrderedStore>,
    foreground: kad::QueryId,
) -> Vec<kad::QueryInfo> {
    let waker = futures::task::noop_waker();
    let _ = kad.poll(&mut Context::from_waker(&waker));
    assert!(kad.query_is_retained(&foreground));
    let queries = kad
        .iter_queries()
        .filter(|query| query.id() != foreground)
        .map(|query| (query.id(), query.info().clone()))
        .collect::<Vec<_>>();
    assert!(queries.len() <= 1);
    for (id, _) in &queries {
        assert!(kad.cancel_query(id));
    }
    queries.into_iter().map(|(_, info)| info).collect()
}

#[test]
fn background_job_batches_bound_both_jobs_without_skipping_large_earlier_keys() {
    let local = PeerId::random();
    let mut kad = kad::Behaviour::with_config(
        local,
        OrderedStore::default(),
        job_config(Some(limits(3, 8, 128))),
    );
    kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
    let foreground = kad.try_get_closest_peers(PeerId::random()).unwrap();
    let held = kad.try_get_closest_peers(PeerId::random()).unwrap();
    let expected = [b"AAAAAAA".to_vec(), b"BB".to_vec(), b"C".to_vec()]
        .into_iter()
        .chain((4..40).map(|index| vec![b'D' + index, 1]))
        .collect::<BTreeSet<_>>();
    for key in &expected {
        kad.store_mut()
            .put(kad::Record::new(key.clone(), vec![7; 64]))
            .unwrap();
        kad.store_mut()
            .add_provider(kad::ProviderRecord::new(key.clone(), local, Vec::new()))
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(110));
    let waker = futures::task::noop_waker();
    for _ in 0..8 {
        let _ = kad.poll(&mut Context::from_waker(&waker));
        assert_eq!(kad.background_job_usage().pending_keys, 0);
        assert_eq!(kad.query_pool_usage().retained, 2);
    }
    assert!(kad.cancel_query(&held));
    let mut records = BTreeSet::new();
    let mut providers = BTreeSet::new();
    let mut saw_retained_batch = false;
    for _ in 0..1000 {
        for info in poll_jobs(&mut kad, foreground) {
            match info {
                kad::QueryInfo::PutRecord { record, .. } => {
                    records.insert(record.key.to_vec());
                }
                kad::QueryInfo::AddProvider { key, .. } => {
                    providers.insert(key.to_vec());
                }
                _ => panic!("unexpected background query"),
            }
        }
        let usage = kad.background_job_usage();
        assert_eq!(usage.bounded_jobs, 2);
        assert!(usage.pending_keys <= 6);
        assert!(usage.pending_key_bytes <= 16);
        assert!(usage.cursor_bytes <= 4 * 8);
        saw_retained_batch |= usage.pending_keys > 0;
        if records == expected && providers == expected && usage.cursor_bytes == 0 {
            break;
        }
    }
    assert!(saw_retained_batch);
    assert_eq!(records, expected);
    assert_eq!(providers, expected);
    let usage = kad.background_job_usage();
    assert_eq!(usage.pending_key_bytes, 0);
    assert_eq!(usage.cursor_bytes, 0);
    assert_eq!(kad.query_pool_usage().rejected, 0);
}

#[test]
fn background_job_batches_read_fresh_values_and_discard_removed_or_expired_work() {
    for bounded in [false, true] {
        let local = PeerId::random();
        let config = job_config(bounded.then(|| limits(8, 128, 128)));
        let mut kad = kad::Behaviour::with_config(local, OrderedStore::default(), config);
        kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
        let foreground = kad.try_get_closest_peers(PeerId::random()).unwrap();
        for byte in 0..4 {
            let mut record = kad::Record::new(vec![byte], vec![1]);
            record.publisher = Some(local);
            kad.store_mut().put(record).unwrap();
        }
        std::thread::sleep(Duration::from_millis(110));
        let mut first = None;
        for _ in 0..10 {
            first = poll_jobs(&mut kad, foreground).into_iter().next();
            if first.is_some() {
                break;
            }
        }
        let Some(kad::QueryInfo::PutRecord { record, .. }) = first else {
            panic!("first job query")
        };
        assert_eq!(record.key.to_vec(), vec![0]);
        let mut changed = kad
            .store_mut()
            .get(&kad::RecordKey::new(&[1]))
            .unwrap()
            .into_owned();
        changed.value = vec![42];
        kad.store_mut().put(changed).unwrap();
        kad.remove_record(&kad::RecordKey::new(&[2]));
        let mut expired = kad
            .store_mut()
            .get(&kad::RecordKey::new(&[3]))
            .unwrap()
            .into_owned();
        expired.expires = Some(Instant::now() - Duration::from_secs(1));
        kad.store_mut().put(expired).unwrap();
        let mut found = BTreeMap::new();
        for _ in 0..50 {
            for info in poll_jobs(&mut kad, foreground) {
                if let kad::QueryInfo::PutRecord { record, .. } = info {
                    found.insert(record.key.to_vec(), record.value);
                }
            }
        }
        assert_eq!(
            found.get(&vec![1]),
            Some(&vec![if bounded { 42 } else { 1 }])
        );
        assert!(!found.contains_key(&vec![2]));
        assert_eq!(found.contains_key(&vec![3]), !bounded);
        if bounded {
            assert!(kad.store_mut().get(&kad::RecordKey::new(&[3])).is_none());
            assert_eq!(kad.background_job_usage().pending_keys, 0);
            assert_eq!(kad.background_job_usage().cursor_bytes, 0);
        }
    }
}

#[tokio::test]
async fn background_job_batches_reject_oversized_input_and_resume_after_store_update() {
    let local = PeerId::random();
    let mut kad = kad::Behaviour::with_config(
        local,
        OrderedStore::default(),
        job_config(Some(limits(2, 8, 128))),
    );
    kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
    let foreground = kad.try_get_closest_peers(PeerId::random()).unwrap();
    let oversized_key = kad::RecordKey::new(&[255; 129]);
    kad.store_mut()
        .put(kad::Record::new(oversized_key.clone(), vec![1]))
        .unwrap();
    kad.store_mut()
        .add_provider(kad::ProviderRecord::new(
            oversized_key.clone(),
            local,
            Vec::new(),
        ))
        .unwrap();
    kad.store_mut()
        .put(kad::Record::new(vec![2], vec![1; 128]))
        .unwrap();
    let mut expired = kad::ProviderRecord::new(vec![2], local, Vec::new());
    expired.expires = Some(Instant::now() - Duration::from_secs(1));
    kad.store_mut().add_provider(expired).unwrap();
    tokio::time::sleep(Duration::from_millis(110)).await;
    for _ in 0..20 {
        assert!(poll_jobs(&mut kad, foreground).is_empty());
    }
    assert!(kad.background_job_usage().rejected_inputs >= 3);
    assert_eq!(kad.background_job_usage().pending_key_bytes, 0);
    assert_eq!(kad.background_job_usage().cursor_bytes, 0);
    assert!(
        kad.store_mut()
            .providers(&kad::RecordKey::new(&[2]))
            .is_empty()
    );
    assert_eq!(kad.query_pool_usage().rejected, 0);
    kad.store_mut().remove(&oversized_key);
    kad.store_mut().remove_provider(&oversized_key, &local);
    kad.store_mut()
        .put(kad::Record::new(vec![2], vec![42]))
        .unwrap();
    kad.store_mut()
        .add_provider(kad::ProviderRecord::new(vec![2], local, Vec::new()))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(110)).await;
    let mut record_seen = false;
    let mut provider_seen = false;
    for _ in 0..20 {
        for info in poll_jobs(&mut kad, foreground) {
            match info {
                kad::QueryInfo::PutRecord { record, .. } => {
                    assert_eq!(record.key.to_vec(), vec![2]);
                    assert_eq!(record.value, vec![42]);
                    record_seen = true;
                }
                kad::QueryInfo::AddProvider { key, .. } => {
                    assert_eq!(key.to_vec(), vec![2]);
                    provider_seen = true;
                }
                _ => panic!("unexpected background query"),
            }
        }
        assert!(kad.background_job_usage().pending_key_bytes <= 16);
    }
    assert!(record_seen && provider_seen);
    assert_eq!(kad.background_job_usage().cursor_bytes, 0);
}

#[tokio::test]
async fn background_job_skip_bookkeeping_is_bounded_under_inbound_churn() {
    for (count, bytes, accepted) in [(3, 128, 3), (128, 8, 2)] {
        let mut receiver =
            super::build_node(&super::tests::retention_diagnostic_config(false)).unwrap();
        let mut sender =
            super::build_node(&super::tests::retention_diagnostic_config(false)).unwrap();
        for node in [&mut receiver, &mut sender] {
            let mut config = super::controlled_kademlia_config(StreamProtocol::new(
                crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            ));
            config.set_background_job_limits(limits(count, bytes, 128));
            node.swarm.behaviour_mut().kad = kad::Behaviour::with_config(
                node.local_peer_id,
                kad::store::MemoryStore::new(node.local_peer_id),
                config,
            );
            node.swarm
                .behaviour_mut()
                .kad
                .set_mode(Some(kad::Mode::Server));
        }
        receiver
            .swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        let address = super::tests::next_listen_address(&mut receiver.swarm).await;
        sender
            .swarm
            .dial(address.with(Protocol::P2p(receiver.local_peer_id)))
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            super::tests::next_connection_to_peer(
                &mut receiver.swarm,
                &mut sender.swarm,
                receiver.local_peer_id,
            ),
        )
        .await
        .unwrap();
        for index in 0..25 {
            if index == 24 {
                let key = kad::RecordKey::new(&[0, 0, 0]);
                let mut record = receiver
                    .swarm
                    .behaviour_mut()
                    .kad
                    .store_mut()
                    .get(&key)
                    .unwrap()
                    .into_owned();
                record.publisher = Some(receiver.local_peer_id);
                receiver
                    .swarm
                    .behaviour_mut()
                    .kad
                    .store_mut()
                    .put(record)
                    .unwrap();
                receiver.swarm.behaviour_mut().kad.remove_record(&key);
                assert_eq!(
                    receiver
                        .swarm
                        .behaviour()
                        .kad
                        .background_job_usage()
                        .skipped_keys,
                    accepted - 1
                );
            }
            let query = sender
                .swarm
                .behaviour_mut()
                .kad
                .try_put_record_to(
                    kad::Record::new(vec![index, 0, 0], vec![1]),
                    [receiver.local_peer_id].into_iter(),
                    kad::Quorum::One,
                )
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = tokio::select! {
                        _ = receiver.swarm.select_next_some() => continue,
                        event = sender.swarm.select_next_some() => event,
                    };
                    if let SwarmEvent::Behaviour(super::BehaviourEvent::Kad(
                        kad::Event::OutboundQueryProgressed {
                            id,
                            result: kad::QueryResult::PutRecord(result),
                            ..
                        },
                    )) = event
                    {
                        assert_eq!(id, query);
                        assert!(result.is_ok());
                        break;
                    }
                }
            })
            .await
            .unwrap();
            let usage = receiver.swarm.behaviour().kad.background_job_usage();
            assert!(usage.skipped_keys <= accepted);
            assert!(usage.skipped_key_bytes <= bytes);
        }
        let usage = receiver.swarm.behaviour().kad.background_job_usage();
        assert_eq!(usage.skipped_keys, accepted);
        assert_eq!(usage.skipped_key_bytes, accepted * 3);
        assert_eq!(usage.rejected_skips, 24 - accepted as u64);
    }
}
