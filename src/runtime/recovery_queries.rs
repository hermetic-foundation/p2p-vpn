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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct RecoveryQueriesSnapshot {
    pub(super) peers_retained: usize,
    pub(super) queries: usize,
    pub(super) oldest_pending_age_millis: u64,
    pub(super) cooldown_peers: usize,
    pub(super) next_retry_in_millis: u64,
}

impl RecoveryQueries {
    pub(super) fn snapshot(&self, now: Instant) -> RecoveryQueriesSnapshot {
        let mut snapshot = RecoveryQueriesSnapshot {
            peers_retained: self.peers.len(),
            queries: self.queries.len(),
            ..RecoveryQueriesSnapshot::default()
        };
        let mut next_retry = None;
        for state in self.peers.values() {
            if state.pending_queries > 0 {
                snapshot.oldest_pending_age_millis = snapshot.oldest_pending_age_millis.max(
                    u64::try_from(now.saturating_duration_since(state.last_query).as_millis())
                        .unwrap_or(u64::MAX),
                );
            } else if state.retry_after > now {
                snapshot.cooldown_peers += 1;
                next_retry = Some(next_retry.map_or(state.retry_after, |retry: Instant| {
                    retry.min(state.retry_after)
                }));
            }
        }
        snapshot.next_retry_in_millis = next_retry.map_or(0, |retry| {
            u64::try_from(retry.saturating_duration_since(now).as_millis()).unwrap_or(u64::MAX)
        });
        snapshot
    }

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
    fn snapshot_separates_pending_owners_from_retained_connected_cooldown() {
        let start = Instant::now();
        let peer = PeerId::random();
        let (first, second) = query_ids();
        let mut owner = RecoveryQueries::default();
        assert_eq!(owner.snapshot(start), RecoveryQueriesSnapshot::default());
        assert!(owner.should_query(peer, start));
        owner.record(peer, [first, second], start);
        let now = start + Duration::from_secs(3);
        assert_eq!(
            owner.snapshot(now),
            RecoveryQueriesSnapshot {
                peers_retained: 1,
                queries: 2,
                oldest_pending_age_millis: 3_000,
                cooldown_peers: 0,
                next_retry_in_millis: 0,
            }
        );
        owner.finished(first, now);
        assert_eq!(owner.snapshot(now).queries, 1);
        owner.finished(second, now);
        owner.connected(peer, now);
        let healthy = RecoveryQueriesSnapshot {
            peers_retained: 1,
            queries: 0,
            oldest_pending_age_millis: 0,
            cooldown_peers: 1,
            next_retry_in_millis: 10_000,
        };
        assert_eq!(owner.snapshot(now), healthy);
        owner.finished(first, now);
        assert_eq!(owner.snapshot(now), healthy);
        assert_eq!(
            owner.snapshot(now + CONNECTED_RETRY),
            RecoveryQueriesSnapshot {
                cooldown_peers: 0,
                next_retry_in_millis: 0,
                ..healthy
            }
        );
        // Observing a past-TTL snapshot must not prune retained state.
        assert_eq!(owner.snapshot(now + STATE_TTL).peers_retained, 1);
        assert!(owner.retain_peers(|_| false).is_empty());
        assert_eq!(owner.snapshot(now), RecoveryQueriesSnapshot::default());
    }

    #[test]
    fn snapshot_keeps_overdue_ownership_until_explicit_expiry() {
        let start = Instant::now();
        let peer = PeerId::random();
        let (query, _) = query_ids();
        let mut owner = RecoveryQueries::default();
        assert!(owner.should_query(peer, start));
        owner.record(peer, [query], start);
        let now = start + QUERY_TIMEOUT;
        let overdue = owner.snapshot(now);
        assert_eq!(overdue.queries, 1);
        assert_eq!(overdue.oldest_pending_age_millis, 60_000);
        assert_eq!(owner.snapshot(now), overdue);
        assert_eq!(owner.expire(now), vec![query]);
        let expired = owner.snapshot(now);
        assert_eq!(expired.queries, 0);
        assert_eq!(expired.oldest_pending_age_millis, 0);
        assert_eq!(expired.peers_retained, 1);
        assert_eq!(expired.cooldown_peers, 1);
        assert_eq!(expired.next_retry_in_millis, 30_000);
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

    #[test]
    fn targeted_recovery_expiry_and_capped_backoff_span_twenty_four_hours() {
        assert_eq!(QUERY_TIMEOUT, Duration::from_secs(60));
        assert_eq!(BACKOFF_BASE, Duration::from_secs(30));
        assert_eq!(BACKOFF_MAX, Duration::from_secs(3_600));
        assert_eq!(STATE_TTL, Duration::from_secs(310));
        assert_eq!(MAX_CONCURRENT, 1);
        // Independent schedule: each failure takes 60s, then 30/60/120/240/
        // 480/960/1920/3600s cooldown. Subsequent cooldowns stay at 3600s.
        const STARTS: [u64; 30] = [
            0, 90, 210, 390, 690, 1_230, 2_250, 4_230, 7_890, 11_550, 15_210, 18_870, 22_530,
            26_190, 29_850, 33_510, 37_170, 40_830, 44_490, 48_150, 51_810, 55_470, 59_130, 62_790,
            66_450, 70_110, 73_770, 77_430, 81_090, 84_750,
        ];
        let start = Instant::now();
        let peer = PeerId::random();
        let other = PeerId::random();
        let local = PeerId::random();
        let mut kad = kad::Behaviour::new(local, kad::store::MemoryStore::new(local));
        let mut owner = RecoveryQueries::default();
        let mut pending = None;
        let mut admitted = 0_u8;
        let mut expired_count = 0;

        for elapsed in 0..=86_400 {
            let now = start + Duration::from_secs(elapsed);
            let expected_expiry = STARTS.iter().any(|started| elapsed == started + 60);
            let expired = owner.expire(now);
            assert_eq!(expired.len(), usize::from(expected_expiry), "t={elapsed}");
            if expected_expiry {
                let query = pending.take().expect("scheduled pending query");
                assert_eq!(expired, vec![query]);
                assert!(kad.cancel_query(&query));
                expired_count += 1;
                assert!(owner.expire(now).is_empty());
            }
            let expected_start = STARTS.contains(&elapsed);
            assert_eq!(owner.should_query(peer, now), expected_start, "t={elapsed}");
            if expected_start {
                let query = kad.get_closest_peers(peer);
                owner.record(peer, [query], now);
                assert!(pending.replace(query).is_none());
                admitted += 1;
            }
            // A different peer polls through the 310s idle TTL during every
            // long cooldown. It must not erase the failing peer's history.
            assert_eq!(
                owner.should_query(other, now),
                pending.is_none(),
                "t={elapsed}"
            );
            assert_eq!(owner.state(&peer).unwrap().attempt_count, admitted);
            assert_eq!(owner.state(&other).unwrap().attempt_count, 0);
            let snapshot = owner.snapshot(now);
            assert_eq!(snapshot.peers_retained, 2);
            assert_eq!(snapshot.queries, usize::from(pending.is_some()));
            assert!(snapshot.oldest_pending_age_millis < 60_000);
            assert_eq!(snapshot.cooldown_peers, usize::from(pending.is_none()));
            assert_eq!(kad.query_pool_usage().retained, snapshot.queries);
        }
        assert_eq!(admitted, 30);
        assert_eq!(expired_count, 30);
        assert_eq!(
            owner.snapshot(start + Duration::from_secs(86_400)).queries,
            0
        );

        // The final timeout is t=84810; its retry is t=88410. Idle expiry is
        // relative to that retry, not to the last query at t=84750.
        let retry = start + Duration::from_secs(88_410);
        assert!(!owner.should_query(peer, retry - Duration::from_nanos(1)));
        assert!(owner.should_query(peer, retry));
        assert_eq!(owner.state(&peer).unwrap().attempt_count, 30);
        let idle_expiry = start + Duration::from_secs(88_720);
        assert!(owner.should_query(other, idle_expiry - Duration::from_nanos(1)));
        assert_eq!(owner.state(&peer).unwrap().attempt_count, 30);
        assert!(owner.should_query(other, idle_expiry));
        assert!(owner.state(&peer).is_none());
        assert_eq!(owner.snapshot(idle_expiry).peers_retained, 1);
    }

    #[test]
    fn targeted_recovery_quiet_cancellation_and_capacity_release_span_twenty_four_hours() {
        assert_eq!(CONNECTED_RETRY, Duration::from_secs(10));
        let start = Instant::now();
        let peer = PeerId::random();
        let other = PeerId::random();
        let local = PeerId::random();
        let mut kad = kad::Behaviour::new(local, kad::store::MemoryStore::new(local));
        let mut owner = RecoveryQueries::default();
        let mut cancelled_count = 0;
        let mut expired_count = 0;

        for hour in 0..24 {
            let hour_start = start + Duration::from_secs(hour * 3_600);
            let at = |seconds| hour_start + Duration::from_secs(seconds);
            // Quiet cancellation retires ownership, but is not a successful
            // connection and must not reset the 30/60/120s failure ladder.
            for (began, cancelled, retry, attempts) in
                [(0, 59, 89, 1), (89, 90, 150, 2), (150, 151, 271, 3)]
            {
                assert!(owner.should_query(peer, at(began)));
                let query = kad.get_closest_peers(peer);
                owner.record(peer, [query], at(began));
                assert!(!owner.should_query(other, at(began)));
                assert_eq!(owner.cancel(at(cancelled)), vec![query]);
                assert!(kad.cancel_query(&query));
                cancelled_count += 1;
                let cancelled_state = owner.snapshot(at(cancelled));
                owner.finished(query, at(cancelled) + Duration::from_nanos(1));
                assert_eq!(owner.snapshot(at(cancelled)), cancelled_state);
                for elapsed in cancelled..retry {
                    assert!(owner.cancel(at(elapsed)).is_empty());
                    assert!(!owner.should_query(peer, at(elapsed)));
                    assert_eq!(owner.state(&peer).unwrap().attempt_count, attempts);
                    assert_eq!(owner.snapshot(at(elapsed)).queries, 0);
                }
                assert!(owner.should_query(peer, at(retry)));
            }

            owner.connected(peer, at(300));
            assert_eq!(owner.state(&peer).unwrap().attempt_count, 0);
            assert!(!owner.should_query(peer, at(310) - Duration::from_nanos(1)));
            assert!(owner.should_query(peer, at(310)));
            // The connected reset does not bypass the single global slot.
            assert!(owner.should_query(other, at(310)));
            let held = kad.get_closest_peers(other);
            owner.record(other, [held], at(310));
            assert!(!owner.should_query(peer, at(310)));
            assert!(owner.expire(at(370) - Duration::from_nanos(1)).is_empty());
            assert_eq!(owner.expire(at(370)), vec![held]);
            assert!(kad.cancel_query(&held));
            expired_count += 1;
            assert!(owner.should_query(peer, at(370)));
            let resumed = kad.get_closest_peers(peer);
            owner.record(peer, [resumed], at(370));
            assert_eq!(owner.state(&peer).unwrap().attempt_count, 1);
            owner.finished(held, at(371));
            assert_eq!(owner.query_ids(), vec![resumed]);
            assert!(owner.expire(at(430) - Duration::from_nanos(1)).is_empty());
            assert_eq!(owner.expire(at(430)), vec![resumed]);
            assert!(kad.cancel_query(&resumed));
            expired_count += 1;
            assert!(!owner.should_query(peer, at(460) - Duration::from_nanos(1)));
            assert!(owner.should_query(peer, at(460)));
            owner.connected(peer, at(460));

            // A genuinely quiet period submits no new query. An unrelated
            // admission check still performs ordinary idle-state pruning.
            for elapsed in 460..3_600 {
                assert!(owner.cancel(at(elapsed)).is_empty());
                assert!(owner.should_query(other, at(elapsed)));
                assert_eq!(owner.snapshot(at(elapsed)).queries, 0);
                assert!(owner.snapshot(at(elapsed)).peers_retained <= 2);
                if elapsed == 779 {
                    assert_eq!(owner.state(&peer).unwrap().attempt_count, 0);
                } else if elapsed == 780 {
                    assert!(owner.state(&peer).is_none());
                }
            }
            assert_eq!(kad.query_pool_usage().retained, 0);
        }
        assert_eq!(cancelled_count, 72);
        assert_eq!(expired_count, 48);
        let end = start + Duration::from_secs(86_400);
        assert!(owner.cancel(end).is_empty());
        assert!(owner.expire(end).is_empty());
        assert!(owner.should_query(peer, end));
        assert_eq!(owner.state(&peer).unwrap().attempt_count, 0);
    }
}
