//! Authorized per-peer demand, independent of transport maintenance.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use crate::PeerId;

const QUIET: u64 = 1;
const EXHAUSTED: u64 = u64::MAX;
const LAST_GENERATION: u64 = u64::MAX - 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DemandVersion {
    record: u64,
    generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DemandSnapshot {
    pub version: Option<DemandVersion>,
    pub last_demand: Option<Instant>,
}

#[derive(Debug)]
struct Record {
    id: u64,
    state: AtomicU64,
    timestamp: AtomicU64,
    paused_generation: AtomicU64,
}

#[derive(Debug)]
pub(crate) struct ApplicationDemand {
    origin: Instant,
    peers: HashMap<PeerId, Record>,
    next_record: Option<u64>,
}

impl ApplicationDemand {
    pub fn new(peers: impl IntoIterator<Item = PeerId>, now: Instant) -> Self {
        let mut this = Self {
            origin: now,
            peers: HashMap::new(),
            next_record: Some(0),
        };
        this.admit(peers);
        this
    }

    pub fn admit(&mut self, peers: impl IntoIterator<Item = PeerId>) {
        for peer in peers {
            if let std::collections::hash_map::Entry::Vacant(entry) = self.peers.entry(peer) {
                self.next_record = self.next_record.and_then(|id| id.checked_add(1));
                entry.insert(Record {
                    id: self.next_record.unwrap_or(0),
                    state: AtomicU64::new(if self.next_record.is_some() {
                        0
                    } else {
                        EXHAUSTED
                    }),
                    timestamp: AtomicU64::new(0),
                    paused_generation: AtomicU64::new(EXHAUSTED),
                });
            }
        }
    }

    pub fn retain(&mut self, mut allowed: impl FnMut(PeerId) -> bool) {
        self.peers.retain(|peer, _| allowed(*peer));
    }

    pub fn record(&self, peer: PeerId, now: Instant) -> bool {
        let Some(record) = self.peers.get(&peer) else {
            return false;
        };
        let stamp = u64::try_from(now.saturating_duration_since(self.origin).as_nanos())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1);
        // A reader acquiring a new generation must also observe its timestamp.
        record.timestamp.fetch_max(stamp, Ordering::Relaxed);
        let _ = record
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                Some(if state == EXHAUSTED || state & !QUIET >= LAST_GENERATION {
                    EXHAUSTED
                } else {
                    (state & !QUIET) + 2
                })
            });
        true
    }

    pub fn snapshot(&self, peer: PeerId) -> Option<DemandSnapshot> {
        let record = self.peers.get(&peer)?;
        let state = record.state.load(Ordering::Acquire);
        let stamp = record.timestamp.load(Ordering::Relaxed);
        let last_demand = if stamp == 0 {
            None
        } else {
            let Some(time) = self.origin.checked_add(Duration::from_nanos(stamp - 1)) else {
                return Some(DemandSnapshot {
                    version: None,
                    last_demand: None,
                });
            };
            Some(time)
        };
        Some(DemandSnapshot {
            version: (state != EXHAUSTED).then_some(DemandVersion {
                record: record.id,
                generation: state & !QUIET,
            }),
            last_demand,
        })
    }

    pub fn grant_quiet(&self, peer: PeerId, version: DemandVersion) -> bool {
        self.peers.get(&peer).is_some_and(|record| {
            if record.id != version.record {
                return false;
            }
            match record.state.compare_exchange(
                version.generation,
                version.generation | QUIET,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => true,
                Err(state) => state == (version.generation | QUIET),
            }
        })
    }

    pub fn is_quiet(&self, peer: PeerId) -> bool {
        self.peers.get(&peer).is_some_and(|record| {
            let state = record.state.load(Ordering::Acquire);
            state != EXHAUSTED && state & QUIET != 0
        })
    }

    pub fn pause_maintenance(&self, peer: PeerId, version: DemandVersion) -> bool {
        self.peers.get(&peer).is_some_and(|record| {
            let current = record.state.load(Ordering::Acquire);
            if record.id != version.record
                || current == EXHAUSTED
                || current & !QUIET != version.generation
            {
                return false;
            }
            record
                .paused_generation
                .store(version.generation, Ordering::Release);
            let current = record.state.load(Ordering::Acquire);
            current != EXHAUSTED && current & !QUIET == version.generation
        })
    }

    pub fn maintenance_is_paused(&self, peer: PeerId) -> bool {
        self.peers.get(&peer).is_some_and(|record| {
            let current = record.state.load(Ordering::Acquire);
            current != EXHAUSTED
                && (current & QUIET != 0
                    || record.paused_generation.load(Ordering::Acquire) == current & !QUIET)
        })
    }

    pub fn resume_maintenance(&self, peer: PeerId) {
        if let Some(record) = self.peers.get(&peer) {
            record.paused_generation.store(EXHAUSTED, Ordering::Release);
        }
    }

    pub fn revoke_quiet(&self, peer: PeerId) {
        if let Some(record) = self.peers.get(&peer) {
            let _ = record
                .state
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                    Some(if state == EXHAUSTED {
                        EXHAUSTED
                    } else {
                        state & !QUIET
                    })
                });
            record.paused_generation.store(EXHAUSTED, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(value: u8) -> PeerId {
        PeerId::from_bytes([value; 32])
    }

    #[test]
    fn new_records_are_inactive_and_cannot_admit_unknown_peers() {
        let now = Instant::now();
        let demand = ApplicationDemand::new([peer(1)], now);
        assert_eq!(demand.snapshot(peer(1)).unwrap().last_demand, None);
        assert!(!demand.record(peer(2), now));
        assert_eq!(demand.snapshot(peer(2)), None);
        assert_eq!(demand.peers.len(), 1);
    }

    #[test]
    fn every_packet_changes_version_even_with_the_same_timestamp() {
        let now = Instant::now();
        let demand = ApplicationDemand::new([peer(1)], now);
        let before = demand.snapshot(peer(1)).unwrap();
        assert!(demand.grant_quiet(peer(1), before.version.unwrap()));
        assert!(demand.record(peer(1), now));
        let first = demand.snapshot(peer(1)).unwrap();
        assert!(!demand.is_quiet(peer(1)));
        assert!(!demand.grant_quiet(peer(1), before.version.unwrap()));
        assert!(demand.record(peer(1), now));
        let second = demand.snapshot(peer(1)).unwrap();
        assert_eq!(first.last_demand, second.last_demand);
        assert_ne!(first.version, second.version);
        assert!(!demand.grant_quiet(peer(1), first.version.unwrap()));
    }

    #[test]
    fn preparation_is_not_quiet_and_payload_immediately_invalidates_it() {
        let now = Instant::now();
        let demand = ApplicationDemand::new([peer(1), peer(2)], now);
        let before = demand.snapshot(peer(1)).unwrap();
        assert!(!demand.maintenance_is_paused(peer(1)));
        assert!(demand.pause_maintenance(peer(1), before.version.unwrap()));
        assert!(demand.maintenance_is_paused(peer(1)));
        assert!(!demand.maintenance_is_paused(peer(2)));
        assert!(!demand.is_quiet(peer(1)));
        assert_eq!(demand.snapshot(peer(1)), Some(before));
        demand.record(peer(1), now);
        assert!(!demand.maintenance_is_paused(peer(1)));
        assert!(!demand.pause_maintenance(peer(1), before.version.unwrap()));
        let current = demand.snapshot(peer(1)).unwrap().version.unwrap();
        assert!(demand.pause_maintenance(peer(1), current));
        demand.resume_maintenance(peer(1));
        assert!(!demand.maintenance_is_paused(peer(1)));
        assert!(demand.grant_quiet(peer(1), current));
        demand.resume_maintenance(peer(1));
        assert!(
            demand.maintenance_is_paused(peer(1)),
            "resuming preparation cannot cancel agreed quiet"
        );
        demand.revoke_quiet(peer(1));
        assert!(!demand.maintenance_is_paused(peer(1)));
    }

    #[test]
    fn preparation_cannot_cross_record_replacement_or_exhaustion() {
        let now = Instant::now();
        let mut demand = ApplicationDemand::new([peer(1)], now);
        let old = demand.snapshot(peer(1)).unwrap().version.unwrap();
        assert!(demand.pause_maintenance(peer(1), old));
        demand.retain(|_| false);
        demand.admit([peer(1)]);
        assert!(!demand.maintenance_is_paused(peer(1)));
        assert!(!demand.pause_maintenance(peer(1), old));
        demand.peers[&peer(1)]
            .state
            .store(LAST_GENERATION, Ordering::Relaxed);
        let last = demand.snapshot(peer(1)).unwrap().version.unwrap();
        assert!(demand.pause_maintenance(peer(1), last));
        demand.record(peer(1), now);
        assert!(!demand.maintenance_is_paused(peer(1)));
        assert!(!demand.pause_maintenance(peer(1), last));
        assert_eq!(demand.snapshot(peer(1)).unwrap().version, None);
    }

    #[test]
    fn stale_preparation_cannot_survive_concurrent_payload() {
        let now = Instant::now();
        let demand = std::sync::Arc::new(ApplicationDemand::new([peer(1)], now));
        let old = demand.snapshot(peer(1)).unwrap().version.unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let data = {
            let demand = std::sync::Arc::clone(&demand);
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..10_000 {
                    demand.record(peer(1), now);
                }
            })
        };
        barrier.wait();
        for _ in 0..10_000 {
            demand.pause_maintenance(peer(1), old);
        }
        data.join().unwrap();
        assert!(!demand.maintenance_is_paused(peer(1)));
        assert!(!demand.is_quiet(peer(1)));
    }

    #[test]
    fn timestamps_do_not_go_backward_and_instances_do_not_share_activity() {
        let now = Instant::now();
        let a = ApplicationDemand::new([peer(1)], now);
        let b = ApplicationDemand::new([peer(1)], now);
        a.record(peer(1), now + Duration::from_secs(2));
        a.record(peer(1), now + Duration::from_secs(1));
        assert_eq!(
            a.snapshot(peer(1)).unwrap().last_demand,
            Some(now + Duration::from_secs(2))
        );
        assert_eq!(b.snapshot(peer(1)).unwrap().last_demand, None);
    }

    #[test]
    fn removal_and_readdition_do_not_inherit_activity_or_permission() {
        let now = Instant::now();
        let mut demand = ApplicationDemand::new([peer(1), peer(2)], now);
        demand.record(peer(1), now);
        let stamp = demand.snapshot(peer(1)).unwrap();
        assert!(demand.grant_quiet(peer(1), stamp.version.unwrap()));
        demand.retain(|candidate| candidate != peer(1));
        demand.admit([peer(1)]);
        assert!(!demand.is_quiet(peer(1)));
        assert_eq!(demand.snapshot(peer(1)).unwrap().last_demand, None);
        assert!(!demand.grant_quiet(peer(1), stamp.version.unwrap()));
    }

    #[test]
    fn an_unused_readded_peer_cannot_reuse_a_zero_generation_ticket() {
        let now = Instant::now();
        let mut demand = ApplicationDemand::new([peer(1)], now);
        let old = demand.snapshot(peer(1)).unwrap().version.unwrap();
        demand.retain(|_| false);
        demand.admit([peer(1)]);
        let new = demand.snapshot(peer(1)).unwrap().version.unwrap();
        assert_ne!(old, new);
        assert!(!demand.grant_quiet(peer(1), old));
        assert!(demand.grant_quiet(peer(1), new));
    }

    #[test]
    fn exhaustion_is_sticky_and_never_authorizes_quiet() {
        let now = Instant::now();
        let demand = ApplicationDemand::new([peer(1)], now);
        demand.peers[&peer(1)]
            .state
            .store(LAST_GENERATION, Ordering::Relaxed);
        let old = demand.snapshot(peer(1)).unwrap().version.unwrap();
        demand.record(peer(1), now);
        assert_eq!(demand.snapshot(peer(1)).unwrap().version, None);
        demand.revoke_quiet(peer(1));
        demand.record(peer(1), now);
        assert_eq!(demand.snapshot(peer(1)).unwrap().version, None);
        assert!(!demand.is_quiet(peer(1)));
        assert!(!demand.grant_quiet(peer(1), old));
    }

    #[test]
    fn a_concurrent_old_grant_cannot_survive_accepted_data() {
        let now = Instant::now();
        let demand = std::sync::Arc::new(ApplicationDemand::new([peer(1)], now));
        let old = demand.snapshot(peer(1)).unwrap().version.unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let data = {
            let demand = std::sync::Arc::clone(&demand);
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..10_000 {
                    demand.record(peer(1), now);
                }
            })
        };
        barrier.wait();
        for _ in 0..10_000 {
            demand.grant_quiet(peer(1), old);
        }
        data.join().unwrap();
        assert!(!demand.is_quiet(peer(1)));
        assert!(!demand.grant_quiet(peer(1), old));
        assert_eq!(demand.snapshot(peer(1)).unwrap().last_demand, Some(now));
    }
}
