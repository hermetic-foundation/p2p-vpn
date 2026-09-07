use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use libp2p::{PeerId, kad::QueryId};

use crate::membership::MAX_MEMBERSHIP_RECORDS;

pub(super) const QUERY_TIMEOUT: Duration = Duration::from_mins(1);
pub(super) const BACKOFF_BASE: Duration = Duration::from_secs(30);
const BACKOFF_MAX: Duration = Duration::from_hours(1);
const STATE_TTL: Duration = Duration::from_secs(5 * 60 + 10);
const CONNECTED_RETRY: Duration = Duration::from_secs(10);
const MAX_CONCURRENT: usize = 1;

/// Owns targeted overlay query accounting, never public infrastructure queries.
/// Cancellation decisions remove ownership before the runner applies effects.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct RecoveryQueries {
    peers: HashMap<PeerId, QueryState>,
    queries: HashMap<QueryId, PeerId>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct QueryState {
    pub(super) last_query: Instant,
    pub(super) pending_queries: usize,
    pub(super) attempt_count: u8,
    retry_after: Instant,
}

impl RecoveryQueries {
    pub(super) fn retain_peers(
        &mut self,
        mut authorized: impl FnMut(PeerId) -> bool,
    ) -> Vec<QueryId> {
        self.peers.retain(|peer, _| authorized(*peer));
        let mut cancelled = Vec::new();
        self.queries.retain(|query, peer| {
            if self.peers.contains_key(peer) {
                true
            } else {
                cancelled.push(*query);
                false
            }
        });
        cancelled
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.queries.is_empty()
    }

    pub(super) fn connected(&mut self, peer: PeerId, now: Instant) {
        if let Some(state) = self.peers.get_mut(&peer) {
            state.attempt_count = 0;
            state.retry_after = now + CONNECTED_RETRY;
        }
    }

    pub(super) fn should_query(&mut self, peer: PeerId, now: Instant) -> bool {
        self.peers.retain(|_, state| {
            // Idle expiry must not erase a long failure cooldown or reset its
            // attempt count just as the next retry becomes eligible.
            state.pending_queries > 0
                || now.saturating_duration_since(state.last_query.max(state.retry_after))
                    < STATE_TTL
        });
        if let Some(state) = self.peers.get(&peer) {
            if state.pending_queries > 0 || now < state.retry_after {
                return false;
            }
        } else {
            if self.peers.len() >= MAX_MEMBERSHIP_RECORDS {
                return false;
            }
            self.peers.insert(
                peer,
                QueryState {
                    last_query: now,
                    pending_queries: 0,
                    attempt_count: 0,
                    retry_after: now,
                },
            );
        }
        self.queries.len() < MAX_CONCURRENT
    }

    pub(super) fn record(
        &mut self,
        peer: PeerId,
        queries: impl IntoIterator<Item = QueryId>,
        now: Instant,
    ) {
        let Some(state) = self.peers.get_mut(&peer) else {
            return;
        };
        state.last_query = now;
        state.attempt_count = state.attempt_count.saturating_add(1);
        state.retry_after = now + failure_backoff(state.attempt_count);
        for query in queries {
            if self.queries.insert(query, peer).is_none() {
                state.pending_queries += 1;
            }
        }
    }

    pub(super) fn finished(&mut self, query: QueryId, now: Instant) {
        let Some(peer) = self.queries.remove(&query) else {
            return;
        };
        if let Some(state) = self.peers.get_mut(&peer) {
            state.pending_queries = state.pending_queries.saturating_sub(1);
            if state.pending_queries == 0 {
                state.retry_after = now + failure_backoff(state.attempt_count);
            }
        }
    }

    pub(super) fn expire(&mut self, now: Instant) -> Vec<QueryId> {
        let expired_peers = self
            .peers
            .iter()
            .filter_map(|(peer, state)| {
                (state.pending_queries > 0
                    && now.saturating_duration_since(state.last_query) >= QUERY_TIMEOUT)
                    .then_some(*peer)
            })
            .collect::<HashSet<_>>();
        let expired = self
            .queries
            .iter()
            .filter_map(|(query, peer)| expired_peers.contains(peer).then_some(*query))
            .collect::<Vec<_>>();
        for query in &expired {
            self.finished(*query, now);
        }
        expired
    }

    pub(super) fn cancel(&mut self, now: Instant) -> Vec<QueryId> {
        let cancelled = self.queries.keys().copied().collect::<Vec<_>>();
        for query in &cancelled {
            self.finished(*query, now);
        }
        cancelled
    }

    pub(super) fn reset(&mut self) -> Vec<QueryId> {
        self.peers.clear();
        self.queries.drain().map(|(query, _)| query).collect()
    }

    #[cfg(test)]
    pub(super) fn state(&self, peer: &PeerId) -> Option<&QueryState> {
        self.peers.get(peer)
    }

    #[cfg(test)]
    pub(super) fn query_ids(&self) -> Vec<QueryId> {
        self.queries.keys().copied().collect()
    }
}

fn failure_backoff(failure_count: u8) -> Duration {
    let exponent = u32::from(failure_count.saturating_sub(1)).min(8);
    BACKOFF_BASE
        .saturating_mul(1_u32 << exponent)
        .min(BACKOFF_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::kad;

    fn query_ids() -> (QueryId, QueryId) {
        let local = PeerId::random();
        let mut kad = kad::Behaviour::new(local, kad::store::MemoryStore::new(local));
        (
            kad.get_record(kad::RecordKey::new(&b"one")),
            kad.get_record(kad::RecordKey::new(&b"two")),
        )
    }

    #[test]
    fn timeout_releases_capacity_at_deadline_and_ignores_late_completion() {
        let now = Instant::now();
        let peer = PeerId::random();
        let other = PeerId::random();
        let (old, next) = query_ids();
        let mut owner = RecoveryQueries::default();
        assert!(owner.should_query(peer, now));
        owner.record(peer, [old], now);
        assert!(!owner.should_query(other, now));
        assert!(
            owner
                .expire(now + QUERY_TIMEOUT.checked_sub(Duration::from_nanos(1)).unwrap())
                .is_empty()
        );
        let deadline = now + QUERY_TIMEOUT;
        assert_eq!(owner.expire(deadline), vec![old]);
        assert!(owner.expire(deadline).is_empty());
        assert!(owner.should_query(other, deadline));
        owner.record(other, [next], deadline);
        owner.finished(old, deadline + Duration::from_secs(1));
        assert_eq!(owner.query_ids(), vec![next]);
        owner.finished(next, deadline);
        assert!(!owner.should_query(
            peer,
            deadline + BACKOFF_BASE.checked_sub(Duration::from_nanos(1)).unwrap()
        ));
        assert!(owner.should_query(peer, deadline + BACKOFF_BASE));
    }

    #[test]
    fn suppression_preserves_backoff_but_network_reset_starts_fresh() {
        let now = Instant::now();
        let peer = PeerId::random();
        let (old, next) = query_ids();
        let mut owner = RecoveryQueries::default();
        assert!(owner.should_query(peer, now));
        owner.record(peer, [old], now);
        assert_eq!(owner.cancel(now), vec![old]);
        assert!(owner.cancel(now).is_empty());
        assert!(!owner.should_query(peer, now));
        assert!(owner.reset().is_empty());
        assert!(owner.should_query(peer, now));
        assert_eq!(owner.state(&peer).unwrap().attempt_count, 0);
        owner.record(peer, [next], now);
        owner.finished(old, now);
        assert_eq!(owner.reset(), vec![next]);
        assert!(!owner.has_pending());
        owner.finished(next, now);
        assert!(owner.state(&peer).is_none());
    }

    #[test]
    fn revocation_discards_pending_and_cooldown_state_without_resurrection() {
        let now = Instant::now();
        let allowed = PeerId::random();
        let revoked = PeerId::random();
        let (old, next) = query_ids();
        let mut owner = RecoveryQueries::default();
        assert!(owner.should_query(allowed, now));
        owner.record(allowed, [old], now);
        owner.finished(old, now);
        assert!(owner.should_query(revoked, now));
        owner.record(revoked, [next], now);
        assert_eq!(owner.retain_peers(|peer| peer == allowed), vec![next]);
        owner.finished(next, now);
        assert!(owner.state(&revoked).is_none());
        assert!(!owner.should_query(allowed, now));
        assert!(owner.should_query(revoked, now));
        assert_eq!(owner.state(&revoked).unwrap().attempt_count, 0);
    }

    #[test]
    fn stale_state_expires_but_pending_queries_require_explicit_cancellation() {
        let now = Instant::now();
        let peer = PeerId::random();
        let idle = PeerId::random();
        let (query, _) = query_ids();
        let mut owner = RecoveryQueries::default();
        assert!(owner.should_query(idle, now));
        assert!(owner.should_query(peer, now));
        owner.record(peer, [query], now);
        assert!(!owner.should_query(peer, now + STATE_TTL));
        assert!(owner.state(&idle).is_none());
        assert_eq!(owner.query_ids(), vec![query]);
        assert_eq!(owner.expire(now + STATE_TTL), vec![query]);
    }

    #[test]
    fn state_limit_reclaims_only_stale_nonpending_peers() {
        let now = Instant::now();
        let mut owner = RecoveryQueries::default();
        for _ in 0..MAX_MEMBERSHIP_RECORDS {
            assert!(owner.should_query(PeerId::random(), now));
        }
        let extra = PeerId::random();
        assert!(!owner.should_query(
            extra,
            now + STATE_TTL.checked_sub(Duration::from_nanos(1)).unwrap()
        ));
        assert_eq!(owner.peers.len(), MAX_MEMBERSHIP_RECORDS);
        assert!(owner.should_query(extra, now + STATE_TTL));
        assert_eq!(owner.peers.len(), 1);
    }

    #[test]
    fn connection_resets_failure_count_and_backoff_is_bounded() {
        assert_eq!(failure_backoff(0), BACKOFF_BASE);
        assert_eq!(failure_backoff(1), BACKOFF_BASE);
        assert_eq!(failure_backoff(2), BACKOFF_BASE * 2);
        assert_eq!(failure_backoff(u8::MAX), BACKOFF_MAX);
        let now = Instant::now();
        let peer = PeerId::random();
        let (query, _) = query_ids();
        let mut owner = RecoveryQueries::default();
        assert!(owner.should_query(peer, now));
        owner.record(peer, [query], now);
        owner.finished(query, now);
        owner.connected(peer, now);
        assert_eq!(owner.state(&peer).unwrap().attempt_count, 0);
        assert!(
            !owner.should_query(
                peer,
                now + CONNECTED_RETRY
                    .checked_sub(Duration::from_nanos(1))
                    .unwrap()
            )
        );
        assert!(owner.should_query(peer, now + CONNECTED_RETRY));
    }

    #[test]
    fn sustained_failures_preserve_backoff_past_state_ttl() {
        let peer = PeerId::random();
        let other = PeerId::random();
        let (query, _) = query_ids();
        let mut owner = RecoveryQueries::default();
        let mut now = Instant::now();
        for attempt in 1..=12 {
            assert!(owner.should_query(peer, now));
            owner.record(peer, [query], now);
            owner.finished(query, now);
            assert_eq!(owner.state(&peer).unwrap().attempt_count, attempt);
            let retry = now + failure_backoff(attempt);
            let before_retry = retry - Duration::from_nanos(1);
            assert!(owner.should_query(other, before_retry));
            assert!(
                !owner.should_query(peer, before_retry),
                "attempt {attempt}: state expiry must not shorten retry backoff"
            );
            now = retry;
        }
        assert_eq!(failure_backoff(12), BACKOFF_MAX);
        assert!(owner.should_query(other, now + STATE_TTL));
        assert!(
            owner.state(&peer).is_none(),
            "abandoned state still expires"
        );
    }
}
