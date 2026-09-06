use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use libp2p::PeerId;

pub(super) const CAPACITY: usize = 1024;
pub(super) const RETRY_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct CompletedSnapshot {
    snapshot: String,
    refreshed_at: Instant,
}

#[derive(Debug, Default)]
pub(super) struct MembershipSyncHistory {
    completed: HashMap<PeerId, CompletedSnapshot>,
    retry_after: HashMap<PeerId, Instant>,
    overflow_retry_after: Option<Instant>,
}

impl MembershipSyncHistory {
    pub(super) fn allows(&self, peer: PeerId, snapshot: &str, now: Instant) -> bool {
        self.completed
            .get(&peer)
            .is_none_or(|completed| completed.snapshot != snapshot)
            && self
                .retry_after
                .get(&peer)
                .is_none_or(|deadline| now >= *deadline)
            && self
                .overflow_retry_after
                .is_none_or(|deadline| now >= deadline)
    }

    pub(super) fn mark_completed(&mut self, peer: PeerId, snapshot: String, now: Instant) {
        if !self.completed.contains_key(&peer) && self.completed.len() >= CAPACITY {
            let oldest = self
                .completed
                .iter()
                .min_by_key(|(_, entry)| entry.refreshed_at)
                .map(|(peer, _)| *peer);
            if let Some(oldest) = oldest {
                self.completed.remove(&oldest);
            }
        }
        self.completed.insert(
            peer,
            CompletedSnapshot {
                snapshot,
                refreshed_at: now,
            },
        );
        self.retry_after.remove(&peer);
    }

    pub(super) fn mark_failed(&mut self, peer: PeerId, now: Instant) -> Option<Instant> {
        let mut pressure_deadline = None;
        if !self.retry_after.contains_key(&peer) && self.retry_after.len() >= CAPACITY {
            self.retry_after.retain(|_, deadline| now < *deadline);
            if self.retry_after.len() >= CAPACITY {
                let oldest = self
                    .retry_after
                    .iter()
                    .min_by_key(|(_, deadline)| **deadline)
                    .map(|(peer, deadline)| (*peer, *deadline));
                if let Some((oldest, deadline)) = oldest {
                    self.retry_after.remove(&oldest);
                    // Eviction must never turn a live backoff into immediate retry permission.
                    let deadline = self
                        .overflow_retry_after
                        .map_or(deadline, |old| old.max(deadline));
                    self.overflow_retry_after = Some(deadline);
                    pressure_deadline = Some(deadline);
                }
            }
        }
        self.retry_after.insert(peer, now + RETRY_DELAY);
        pressure_deadline
    }

    pub(super) fn retain_peers(&mut self, mut authorized: impl FnMut(PeerId) -> bool) {
        self.completed.retain(|peer, _| authorized(*peer));
        self.retry_after.retain(|peer, _| authorized(*peer));
        // The shared deadline may also cover an evicted peer that remains authorized.
    }

    #[cfg(test)]
    pub(super) fn completed_snapshot(&self, peer: PeerId) -> Option<&str> {
        self.completed
            .get(&peer)
            .map(|entry| entry.snapshot.as_str())
    }

    #[cfg(test)]
    pub(super) fn retry_deadline(&self, peer: PeerId) -> Option<Instant> {
        self.retry_after.get(&peer).copied()
    }

    #[cfg(test)]
    pub(super) fn entry_counts(&self) -> (usize, usize) {
        (self.completed.len(), self.retry_after.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_capacity_preserves_refreshes_without_touching_retry_history() {
        let mut history = MembershipSyncHistory::default();
        let now = Instant::now();
        let peers = (0..CAPACITY).map(|_| PeerId::random()).collect::<Vec<_>>();
        for (index, peer) in peers.iter().enumerate() {
            history.mark_completed(
                *peer,
                "old".to_owned(),
                now + Duration::from_secs(index as u64),
            );
        }
        let later = now + Duration::from_secs(CAPACITY as u64);
        history.mark_completed(peers[0], "new".to_owned(), later);
        assert_eq!(history.completed.len(), CAPACITY);
        history.mark_failed(peers[1], later);
        history.mark_completed(PeerId::random(), "other".to_owned(), later);
        assert_eq!(history.completed.len(), CAPACITY);
        assert_eq!(history.completed_snapshot(peers[0]), Some("new"));
        assert_eq!(history.completed_snapshot(peers[1]), None);
        assert!(!history.allows(peers[1], "other", later));
        assert!(history.allows(peers[1], "other", later + RETRY_DELAY));
    }

    #[test]
    fn retry_overflow_preserves_every_failed_peers_deadline() {
        let mut history = MembershipSyncHistory::default();
        let now = Instant::now();
        let peers = (0..CAPACITY + 4)
            .map(|_| PeerId::random())
            .collect::<Vec<_>>();
        for (index, peer) in peers.iter().enumerate() {
            let failed_at = now + Duration::from_millis(index as u64);
            let pressure = history.mark_failed(*peer, failed_at);
            assert_eq!(pressure.is_some(), index >= CAPACITY);
            assert!(pressure.is_none_or(|deadline| deadline <= failed_at + RETRY_DELAY));
            assert!(history.retry_after.len() <= CAPACITY);
        }
        for (index, peer) in peers.iter().enumerate() {
            let deadline = now + Duration::from_millis(index as u64) + RETRY_DELAY;
            assert!(!history.allows(*peer, "new", deadline - Duration::from_nanos(1)));
        }
        let shared_deadline = now + Duration::from_millis(3) + RETRY_DELAY;
        assert_eq!(history.overflow_retry_after, Some(shared_deadline));
        let unrelated = PeerId::random();
        assert!(!history.allows(unrelated, "new", shared_deadline - Duration::from_nanos(1)));
        assert!(history.allows(unrelated, "new", shared_deadline));
        assert!(history.allows(peers[0], "new", shared_deadline));
        assert!(!history.allows(peers[CAPACITY], "new", shared_deadline));
        let final_deadline = now + Duration::from_millis((CAPACITY + 3) as u64) + RETRY_DELAY;
        assert!(
            peers
                .iter()
                .all(|peer| history.allows(*peer, "new", final_deadline))
        );
    }

    #[test]
    fn expired_retries_make_space_without_a_shared_pause() {
        let mut history = MembershipSyncHistory::default();
        let now = Instant::now();
        for _ in 0..CAPACITY {
            assert_eq!(history.mark_failed(PeerId::random(), now), None);
        }
        let peer = PeerId::random();
        assert_eq!(history.mark_failed(peer, now + RETRY_DELAY), None);
        assert_eq!(history.retry_after.len(), 1);
        assert_eq!(history.overflow_retry_after, None);
        assert_eq!(
            history.retry_deadline(peer),
            Some(now + RETRY_DELAY + RETRY_DELAY)
        );
    }

    #[test]
    fn repeated_failure_updates_without_evicting_another_peer() {
        let mut history = MembershipSyncHistory::default();
        let now = Instant::now();
        let peer = PeerId::random();
        history.mark_failed(peer, now);
        for _ in 1..CAPACITY {
            history.mark_failed(PeerId::random(), now);
        }
        let later = now + Duration::from_secs(1);
        assert_eq!(history.mark_failed(peer, later), None);
        assert_eq!(history.retry_after.len(), CAPACITY);
        assert_eq!(history.retry_deadline(peer), Some(later + RETRY_DELAY));
    }

    #[test]
    fn completion_and_authorization_pruning_preserve_other_peers_backoff() {
        let mut history = MembershipSyncHistory::default();
        let now = Instant::now();
        let kept = PeerId::random();
        let removed = PeerId::random();
        for peer in [kept, removed] {
            history.mark_completed(peer, "old".to_owned(), now);
            history.mark_failed(peer, now);
        }
        history.retain_peers(|peer| peer == kept);
        assert_eq!(history.entry_counts(), (1, 1));
        assert!(!history.allows(kept, "new", now));
        assert!(history.allows(removed, "new", now));
        history.mark_completed(kept, "new".to_owned(), now);
        assert!(!history.allows(kept, "new", now));
        assert!(history.allows(kept, "newer", now));
        assert!(history.retry_after.is_empty());
    }

    #[test]
    fn authorization_pruning_preserves_backoff_for_an_evicted_authorized_peer() {
        let mut history = MembershipSyncHistory::default();
        let now = Instant::now();
        let peers = (0..=CAPACITY).map(|_| PeerId::random()).collect::<Vec<_>>();
        for peer in &peers {
            history.mark_failed(*peer, now);
        }
        let evicted = *peers
            .iter()
            .find(|peer| !history.retry_after.contains_key(peer))
            .unwrap();
        history.retain_peers(|peer| peer == evicted);
        assert_eq!(history.entry_counts(), (0, 0));
        assert!(!history.allows(evicted, "new", now + RETRY_DELAY - Duration::from_nanos(1)));
        assert!(history.allows(evicted, "new", now + RETRY_DELAY));
    }
}
