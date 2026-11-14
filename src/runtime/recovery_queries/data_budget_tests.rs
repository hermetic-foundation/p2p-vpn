use super::*;
use crate::runtime::lifecycle::activity::ApplicationDemand;
use libp2p::kad;

fn demand_for(peers: &[PeerId], now: Instant) -> ApplicationDemand {
    ApplicationDemand::new(peers.iter().copied().map(crate::PeerId::from_libp2p), now)
}

fn version(demand: &ApplicationDemand, peer: PeerId) -> Option<DemandVersion> {
    demand
        .snapshot(crate::PeerId::from_libp2p(peer))
        .expect("authorized peer")
        .version
}

fn native_queries() -> (QueryId, QueryId) {
    let local = PeerId::random();
    let mut kad = kad::Behaviour::new(local, kad::store::MemoryStore::new(local));
    (kad.get_closest_peers(local), kad.get_closest_peers(local))
}

#[test]
fn initial_allowance_is_not_quiet_and_exhaustion_evidence_is_read_only() {
    let now = Instant::now();
    let peer = PeerId::random();
    let demand = demand_for(&[peer], now);
    let ticket = version(&demand, peer);
    let mut owner = RecoveryQueries::default();
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now + DATA_BUDGET_WINDOW));
    assert!(owner.peers.is_empty());
    owner.reconcile_data_budget(peer, ticket, 1, now);
    let mut expected = RecoveryQueries::default();
    expected.reconcile_data_budget(peer, ticket, 1, now);

    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    assert!(owner.should_query_data(peer, now));
    assert!(owner.should_query_data(peer, now));
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    assert!(owner.data_budget_exhausted(peer, ticket, 1, now + DATA_BUDGET_WINDOW));
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    assert_eq!(owner, expected);
}

#[test]
fn successful_new_data_ownership_spends_the_allowance_immediately() {
    let now = Instant::now();
    let peer = PeerId::random();
    let demand = demand_for(&[peer], now);
    let ticket = version(&demand, peer);
    let (query, _) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(peer, ticket, 1, now);
    assert!(owner.should_query_data(peer, now));
    owner.record(peer, [query], now);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, now));
    assert!(!owner.should_query_data(peer, now));
    assert_eq!(owner.state(&peer).unwrap().pending_queries, 1);
    assert_eq!(owner.state(&peer).unwrap().attempt_count, 1);
    owner.finished(query, now);
    assert!(!owner.should_query_data(peer, now + BACKOFF_BASE));
    assert!(owner.should_query(peer, now + BACKOFF_BASE));
}

#[test]
fn spent_budget_is_distinct_from_pending_data_and_late_owned_result_delivery() {
    let now = Instant::now();
    let peer = PeerId::random();
    let other = PeerId::random();
    let demand = demand_for(&[peer], now);
    let ticket = version(&demand, peer);
    let (data, control) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(peer, ticket, 1, now);
    owner.record(peer, [data], now);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, now));
    assert!(owner.has_data_query(peer));
    assert!(!owner.has_data_query(other));
    assert!(owner.owns_data_query(peer, &data));
    assert!(!owner.owns_data_query(other, &data));
    assert!(!owner.owns_data_query(peer, &control));

    let late = now + QUERY_TIMEOUT;
    // Read-only budget/ownership checks cannot retire the native lookup.
    assert!(owner.data_budget_exhausted(peer, ticket, 1, late));
    assert!(!owner.should_query_data(peer, late));
    assert!(owner.has_data_query(peer));
    let deliver_owned_result = owner.owns_data_query(peer, &data);
    owner.finished(data, late);
    assert!(deliver_owned_result);
    assert!(!owner.has_data_query(peer));
    assert!(!owner.owns_data_query(peer, &data));
    assert!(owner.data_budget_exhausted(peer, ticket, 1, late));
    assert!(!owner.should_query_data(peer, late));

    assert!(owner.can_continue_control(peer));
    let scope = ControlQueryScope {
        peer,
        generation: 2,
        deadline: late + CONTROL_QUERY_TIMEOUT,
    };
    owner.record_control(control, scope, late);
    assert!(owner.has_pending());
    assert!(!owner.has_data_query(peer));
    assert!(!owner.owns_data_query(peer, &control));
    owner.finished(data, late);
    assert_eq!(owner.control_query(&control).unwrap().scope, scope);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, late));
    assert_eq!(owner.expire(scope.deadline), vec![control]);
    assert!(!owner.has_pending());
    assert!(owner.data_budget_exhausted(peer, ticket, 1, scope.deadline));
}

#[test]
fn failed_admission_empty_ids_and_missing_owners_do_not_spend_an_allowance() {
    let now = Instant::now();
    let peer = PeerId::random();
    let absent = PeerId::random();
    let demand = demand_for(&[peer, absent], now);
    let ticket = version(&demand, peer);
    let (query, _) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.record(absent, [query], now);
    assert!(!owner.has_pending());
    assert!(owner.state(&absent).is_none());
    assert!(!owner.data_budget_exhausted(absent, version(&demand, absent), 1, now));

    owner.reconcile_data_budget(peer, ticket, 1, now);
    // Native admission fails after the eligibility check: no IDs to own.
    assert!(owner.should_query_data(peer, now));
    assert!(owner.should_query_data(peer, now));
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    owner.record(peer, [], now);
    assert!(!owner.has_pending());
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    // Preserve legacy failure accounting, without delaying an unspent ticket.
    assert_eq!(owner.state(&peer).unwrap().attempt_count, 1);
    assert!(!owner.should_query(peer, now));
    assert!(owner.should_query_data(peer, now));
    owner.record(peer, [query], now);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, now));
}

#[test]
fn data_completion_cancellation_expiry_and_connection_do_not_renew_a_ticket() {
    for termination in 0..3 {
        let now = Instant::now();
        let peer = PeerId::random();
        let other = PeerId::random();
        let demand = demand_for(&[peer], now);
        let ticket = version(&demand, peer);
        let (query, _) = native_queries();
        let mut owner = RecoveryQueries::default();
        owner.reconcile_data_budget(peer, ticket, 1, now);
        owner.record(peer, [query], now);
        assert!(owner.has_data_query(peer));
        assert!(owner.owns_data_query(peer, &query));
        assert!(owner.data_budget_exhausted(peer, ticket, 1, now));
        let ended = now + QUERY_TIMEOUT;
        match termination {
            0 => owner.finished(query, ended),
            1 => assert_eq!(owner.cancel_data_queries(|_| false, ended), vec![query]),
            _ => {
                assert!(owner.expire(ended - Duration::from_nanos(1)).is_empty());
                assert_eq!(owner.expire(ended), vec![query]);
            }
        }
        assert!(!owner.has_data_query(peer));
        assert!(!owner.owns_data_query(peer, &query));
        assert!(owner.data_budget_exhausted(peer, ticket, 1, ended));
        assert!(!owner.should_query(peer, ended));
        assert!(owner.should_query(peer, ended + BACKOFF_BASE));
        owner.connected(peer, ended);
        owner.finished(query, ended + Duration::from_secs(1));
        assert!(!owner.should_query(peer, ended));
        assert!(owner.should_query(peer, ended + CONNECTED_RETRY));
        let later = now + Duration::from_hours(24);
        assert!(owner.should_query(other, later));
        owner.reconcile_data_budget(peer, ticket, 1, later);
        assert!(owner.data_budget_exhausted(peer, ticket, 1, later));
        assert!(!owner.should_query_data(peer, later));
        assert!(owner.state(&peer).is_some());
        assert!(!owner.has_pending());
    }
}

#[test]
fn unspent_deadline_survives_ttl_and_repeated_reconciliation_without_a_timer() {
    assert_eq!(DATA_BUDGET_WINDOW, Duration::from_secs(600));
    let now = Instant::now();
    let peer = PeerId::random();
    let other = PeerId::random();
    let demand = demand_for(&[peer], now);
    let ticket = version(&demand, peer);
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(peer, ticket, 1, now);
    assert!(owner.should_query(other, now + STATE_TTL));
    assert!(owner.state(&peer).is_some());
    let before = now + DATA_BUDGET_WINDOW - Duration::from_nanos(1);
    owner.reconcile_data_budget(peer, ticket, 1, before);
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, before));
    assert!(owner.should_query_data(peer, before));
    let deadline = now + DATA_BUDGET_WINDOW;
    assert!(owner.data_budget_exhausted(peer, ticket, 1, deadline));
    assert!(!owner.should_query_data(peer, deadline));
    let later = deadline + Duration::from_hours(24);
    assert!(owner.should_query(other, later));
    owner.reconcile_data_budget(peer, ticket, 1, later);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, later));
    assert!(!owner.should_query_data(peer, later));
    assert_eq!(owner.state(&peer).unwrap().attempt_count, 0);
}

#[test]
fn control_ownership_deadlines_and_cooldowns_remain_separate_from_data_budget() {
    let now = Instant::now();
    let peer = PeerId::random();
    let demand = demand_for(&[peer], now);
    let ticket = version(&demand, peer);
    let (first, second) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(peer, ticket, 1, now);
    assert!(owner.should_query(peer, now));
    let scope = ControlQueryScope {
        peer,
        generation: 7,
        deadline: now + Duration::from_secs(5),
    };
    owner.record_control(first, scope, now);
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    assert!(!owner.should_query_data(peer, now));
    owner.reconcile_data_budget(peer, ticket, 2, now);
    assert_eq!(owner.control_query(&first).unwrap().scope, scope);
    assert_eq!(
        owner.control_query(&first).unwrap().expires_at,
        scope.deadline
    );
    assert!(owner.cancel_data_queries(|_| false, now).is_empty());
    assert_eq!(owner.expire(scope.deadline), vec![first]);
    let retry = owner.state(&peer).unwrap().retry_after;
    assert_eq!(retry, scope.deadline + BACKOFF_BASE);
    assert!(!owner.should_query(peer, scope.deadline));
    assert!(owner.should_query_data(peer, scope.deadline));
    assert_eq!(owner.state(&peer).unwrap().retry_after, retry);
    assert_eq!(owner.state(&peer).unwrap().attempt_count, 1);
    assert!(owner.can_continue_control(peer));
    let continuation = ControlQueryScope {
        deadline: now + Duration::from_secs(45),
        ..scope
    };
    owner.record_control(second, continuation, scope.deadline);
    assert_eq!(owner.control_query(&second).unwrap().scope, continuation);
    let expires = scope.deadline + CONTROL_QUERY_TIMEOUT;
    assert_eq!(owner.control_query(&second).unwrap().expires_at, expires);
    assert_eq!(owner.expire(expires), vec![second]);
    assert_eq!(owner.state(&peer).unwrap().attempt_count, 2);
    assert!(!owner.data_budget_exhausted(peer, ticket, 2, expires));
    assert!(owner.should_query_data(peer, expires));
    assert!(!owner.should_query(peer, expires + BACKOFF_BASE));
    assert!(owner.should_query(peer, expires + BACKOFF_BASE * 2));
}

#[test]
fn exhausted_data_budget_does_not_suppress_control_queries() {
    let now = Instant::now();
    let peer = PeerId::random();
    let demand = demand_for(&[peer], now);
    let ticket = version(&demand, peer);
    let (data, control) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(peer, ticket, 1, now);
    owner.record(peer, [data], now);
    owner.finished(data, now);
    let retry = now + BACKOFF_BASE;
    assert!(owner.should_query(peer, retry));
    assert!(!owner.should_query_data(peer, retry));
    let scope = ControlQueryScope {
        peer,
        generation: 3,
        deadline: retry + Duration::from_secs(45),
    };
    owner.record_control(control, scope, retry);
    assert_eq!(owner.control_queries().count(), 1);
    assert_eq!(owner.control_query(&control).unwrap().scope, scope);
    assert_eq!(owner.expire(retry + CONTROL_QUERY_TIMEOUT), vec![control]);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, retry));
    assert!(!owner.should_query_data(peer, retry + Duration::from_hours(1)));
}

#[test]
fn fresh_demand_and_underlay_bypass_hour_backoff_for_data_only() {
    let peer = PeerId::random();
    let other = PeerId::random();
    let mut now = Instant::now();
    let demand = demand_for(&[peer], now);
    let old_ticket = version(&demand, peer);
    let (query, control) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(peer, old_ticket, 1, now);
    for attempt in 1..=9 {
        assert!(owner.should_query(peer, now));
        owner.record(peer, [query], now);
        owner.finished(query, now);
        if attempt < 9 {
            now += failure_backoff(attempt);
        }
    }
    let retry = now + BACKOFF_MAX;
    assert_eq!(owner.state(&peer).unwrap().retry_after, retry);
    // The caller has already applied its LAN-first grace.
    now += Duration::from_secs(30);
    assert!(demand.record(crate::PeerId::from_libp2p(peer), now));
    let ticket = version(&demand, peer);
    assert_ne!(ticket, old_ticket);
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    owner.reconcile_data_budget(peer, ticket, 1, now);
    assert!(owner.should_query_data(peer, now));
    assert!(!owner.should_query(peer, now));
    assert_eq!(owner.state(&peer).unwrap().attempt_count, 9);
    assert_eq!(owner.state(&peer).unwrap().retry_after, retry);

    assert!(owner.should_query(other, now));
    owner.record_control(
        control,
        ControlQueryScope {
            peer: other,
            generation: 1,
            deadline: now + Duration::from_secs(45),
        },
        now,
    );
    assert!(!owner.should_query_data(peer, now));
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    owner.finished(control, now);
    assert!(owner.should_query_data(peer, now));
    owner.record(peer, [query], now);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, now));
    assert!(!owner.should_query_data(peer, now));
    owner.finished(query, now);
    let next_retry = owner.state(&peer).unwrap().retry_after;
    owner.reconcile_data_budget(peer, ticket, 2, now);
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    assert!(!owner.data_budget_exhausted(peer, ticket, 2, now));
    assert!(owner.should_query_data(peer, now));
    assert!(!owner.should_query(peer, now));
    assert_eq!(owner.state(&peer).unwrap().attempt_count, 10);
    assert_eq!(owner.state(&peer).unwrap().retry_after, next_retry);
}

#[test]
fn all_six_offline_members_get_one_lookup_through_the_shared_native_slot() {
    let start = Instant::now();
    let peers: [PeerId; 6] = std::array::from_fn(|_| PeerId::random());
    let demand = demand_for(&peers, start);
    let local = PeerId::random();
    let mut kad = kad::Behaviour::new(local, kad::store::MemoryStore::new(local));
    let mut owner = RecoveryQueries::default();
    for peer in peers {
        owner.reconcile_data_budget(peer, version(&demand, peer), 1, start);
        assert!(!owner.data_budget_exhausted(peer, version(&demand, peer), 1, start));
    }
    let mut now = start;
    let mut admitted = HashSet::new();
    for (index, peer) in peers.into_iter().enumerate() {
        assert!(owner.should_query_data(peer, now));
        let query = kad.get_closest_peers(peer);
        assert!(admitted.insert(query));
        owner.record(peer, [query], now);
        assert_eq!(owner.snapshot(now).queries, 1);
        assert_eq!(kad.query_pool_usage().retained, 1);
        for waiting in peers.iter().skip(index + 1) {
            assert!(!owner.should_query_data(*waiting, now));
            assert!(!owner.data_budget_exhausted(*waiting, version(&demand, *waiting), 1, now));
            assert_eq!(owner.state(waiting).unwrap().attempt_count, 0);
        }
        now += QUERY_TIMEOUT;
        assert_eq!(owner.expire(now), vec![query]);
        assert!(kad.cancel_query(&query));
        assert_eq!(kad.query_pool_usage().retained, 0);
    }
    assert_eq!(admitted.len(), 6);
    for peer in peers {
        owner.reconcile_data_budget(peer, version(&demand, peer), 1, now);
        assert!(owner.data_budget_exhausted(peer, version(&demand, peer), 1, now));
        assert!(!owner.should_query_data(peer, now));
        assert_eq!(owner.state(&peer).unwrap().attempt_count, 1);
    }
    assert_eq!(owner.snapshot(now).queries, 0);
}

#[test]
fn missing_or_stale_ticket_cannot_suppress_or_renew_a_retained_allowance() {
    let now = Instant::now();
    let peer = PeerId::random();
    let unknown = PeerId::random();
    let demand = demand_for(&[peer], now);
    let ticket = version(&demand, peer);
    let (query, _) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(unknown, None, 1, now);
    assert!(owner.state(&unknown).is_none());
    assert!(owner.should_query_data(unknown, now));
    assert!(!owner.data_budget_exhausted(unknown, None, 1, now));
    owner.reconcile_data_budget(peer, ticket, 1, now);
    owner.record(peer, [query], now);
    owner.finished(query, now);
    assert!(!owner.data_budget_exhausted(peer, None, 1, now));
    assert!(!owner.data_budget_exhausted(peer, ticket, 2, now));
    assert!(demand.record(crate::PeerId::from_libp2p(peer), now));
    assert!(!owner.data_budget_exhausted(peer, version(&demand, peer), 1, now));
    owner.reconcile_data_budget(peer, None, 1, now);
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    let later = now + Duration::from_hours(24);
    assert!(owner.should_query_data(peer, later));
    owner.reconcile_data_budget(peer, ticket, 1, later);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, later));
    assert!(!owner.should_query_data(peer, later));
}

#[test]
fn capacity_failure_is_conservative_and_churn_does_not_renew_existing_tickets() {
    let now = Instant::now();
    let peers = (0..MAX_MEMBERSHIP_RECORDS)
        .map(|_| PeerId::random())
        .collect::<Vec<_>>();
    let extra = PeerId::random();
    let mut authorized = peers.clone();
    authorized.push(extra);
    let demand = demand_for(&authorized, now);
    let mut owner = RecoveryQueries::default();
    for peer in &peers {
        owner.reconcile_data_budget(*peer, version(&demand, *peer), 1, now);
    }
    let (query, _) = native_queries();
    let first = peers[0];
    owner.record(first, [query], now);
    owner.finished(query, now);
    let later = now + DATA_BUDGET_WINDOW;
    owner.reconcile_data_budget(extra, version(&demand, extra), 1, later);
    assert_eq!(owner.peers.len(), MAX_MEMBERSHIP_RECORDS);
    assert!(owner.state(&extra).is_none());
    assert!(!owner.data_budget_exhausted(extra, version(&demand, extra), 1, later));
    assert!(!owner.should_query_data(extra, later));
    owner.record(extra, [query], later);
    assert!(!owner.has_pending());
    assert!(owner.retain_peers(|peer| peer != peers[1]).is_empty());
    owner.reconcile_data_budget(extra, version(&demand, extra), 1, later);
    assert!(owner.should_query_data(extra, later));
    owner.reconcile_data_budget(first, version(&demand, first), 1, later);
    assert!(owner.data_budget_exhausted(first, version(&demand, first), 1, later));
    assert!(!owner.should_query_data(first, later));
    assert_eq!(owner.peers.len(), MAX_MEMBERSHIP_RECORDS);
}

#[test]
fn reconciliation_reclaims_only_ordinary_stale_owners_at_capacity() {
    let now = Instant::now();
    let extra = PeerId::random();
    let demand = demand_for(&[extra], now);
    let ticket = version(&demand, extra);
    let mut owner = RecoveryQueries::default();
    for _ in 0..MAX_MEMBERSHIP_RECORDS {
        assert!(owner.should_query(PeerId::random(), now));
    }
    owner.reconcile_data_budget(extra, ticket, 1, now);
    assert!(owner.state(&extra).is_none());
    owner.reconcile_data_budget(extra, ticket, 1, now + STATE_TTL);
    assert_eq!(owner.peers.len(), 1);
    assert!(owner.should_query_data(extra, now + STATE_TTL));
    assert!(!owner.data_budget_exhausted(extra, ticket, 1, now + STATE_TTL));
}

#[test]
fn duplicate_control_ids_preserve_original_ownership_and_do_not_charge_data() {
    let now = Instant::now();
    let control_peer = PeerId::random();
    let data_peer = PeerId::random();
    let demand = demand_for(&[control_peer, data_peer], now);
    let (control, data) = native_queries();
    let mut owner = RecoveryQueries::default();
    for peer in [control_peer, data_peer] {
        owner.reconcile_data_budget(peer, version(&demand, peer), 1, now);
    }
    let scope = ControlQueryScope {
        peer: control_peer,
        generation: 5,
        deadline: now + Duration::from_secs(45),
    };
    owner.record_control(control, scope, now);
    owner.record(data_peer, [control, control], now);
    owner.record_control(
        control,
        ControlQueryScope {
            peer: data_peer,
            ..scope
        },
        now,
    );
    assert_eq!(owner.control_query(&control).unwrap().scope, scope);
    assert_eq!(owner.state(&control_peer).unwrap().pending_queries, 1);
    assert_eq!(owner.state(&data_peer).unwrap().pending_queries, 0);
    for peer in [control_peer, data_peer] {
        assert!(!owner.data_budget_exhausted(peer, version(&demand, peer), 1, now));
    }
    owner.finished(control, now);
    assert!(owner.should_query_data(data_peer, now));
    owner.record(data_peer, [data, data], now);
    assert_eq!(owner.state(&data_peer).unwrap().pending_queries, 1);
    assert!(owner.data_budget_exhausted(data_peer, version(&demand, data_peer), 1, now));
    assert!(!owner.data_budget_exhausted(control_peer, version(&demand, control_peer), 1, now));
}

#[test]
fn rerecording_old_data_id_cannot_spend_a_new_ticket_or_add_pending_ownership() {
    let now = Instant::now();
    let peer = PeerId::random();
    let demand = demand_for(&[peer], now);
    let (query, next) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(peer, version(&demand, peer), 1, now);
    owner.record(peer, [query], now);
    assert!(demand.record(crate::PeerId::from_libp2p(peer), now));
    let ticket = version(&demand, peer);
    owner.reconcile_data_budget(peer, ticket, 1, now);
    owner.record(peer, [query], now);
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    assert_eq!(owner.state(&peer).unwrap().pending_queries, 1);
    assert!(!owner.should_query_data(peer, now));
    owner.finished(query, now);
    owner.finished(query, now);
    assert!(owner.should_query_data(peer, now));
    owner.record(peer, [next], now);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, now));
}

#[test]
fn readmission_changes_the_demand_record_and_revocation_removes_owned_queries() {
    let now = Instant::now();
    let peer = PeerId::random();
    let mut demand = demand_for(&[peer], now);
    let old_ticket = version(&demand, peer);
    let (old, next) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(peer, old_ticket, 1, now);
    owner.record(peer, [old], now);
    assert_eq!(owner.retain_peers(|_| false), vec![old]);
    owner.finished(old, now);
    assert!(owner.state(&peer).is_none());
    assert!(!owner.data_budget_exhausted(peer, old_ticket, 1, now));
    demand.retain(|_| false);
    demand.admit([crate::PeerId::from_libp2p(peer)]);
    let ticket = version(&demand, peer);
    assert_ne!(ticket, old_ticket);
    owner.reconcile_data_budget(peer, ticket, 1, now);
    assert!(owner.should_query_data(peer, now));
    owner.record(peer, [next], now);
    assert!(owner.data_budget_exhausted(peer, ticket, 1, now));
    owner.finished(old, now);
    assert_eq!(owner.query_ids(), vec![next]);
}

#[test]
fn reset_discards_budget_markers_and_control_owners_without_late_resurrection() {
    let now = Instant::now();
    let peer = PeerId::random();
    let demand = demand_for(&[peer], now);
    let ticket = version(&demand, peer);
    let (data, control) = native_queries();
    let mut owner = RecoveryQueries::default();
    owner.reconcile_data_budget(peer, ticket, 1, now);
    owner.record(peer, [data], now);
    owner.finished(data, now);
    owner.record_control(
        control,
        ControlQueryScope {
            peer,
            generation: 8,
            deadline: now + CONTROL_QUERY_TIMEOUT,
        },
        now,
    );
    assert_eq!(owner.reset(), vec![control]);
    assert_eq!(owner, RecoveryQueries::default());
    assert!(!owner.data_budget_exhausted(peer, ticket, 1, now));
    owner.finished(data, now);
    owner.finished(control, now);
    assert_eq!(owner, RecoveryQueries::default());
    owner.reconcile_data_budget(peer, ticket, 2, now);
    assert!(owner.should_query_data(peer, now));
}
