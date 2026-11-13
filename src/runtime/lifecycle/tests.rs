use super::*;

const READY: Readiness = Readiness {
    incoming_path_available: true,
    outstanding_work: false,
};

fn pair(now: Instant) -> (PeerLifecycle, PeerLifecycle) {
    let a = Epoch::new([1; 16]).unwrap();
    let b = Epoch::new([2; 16]).unwrap();
    (
        PeerLifecycle::new(a, b, true, now),
        PeerLifecycle::new(b, a, true, now),
    )
}

fn dormant_pair(now: Instant) -> (PeerLifecycle, PeerLifecycle) {
    let (mut a, mut b) = pair(now);
    let idle = now + IDLE_AFTER;
    let offer = a.offer_idle(idle, READY).unwrap();
    let ack = b.receive_idle(offer, idle, READY).unwrap();
    b.idle_reply_sent(ack, idle, READY).unwrap();
    a.receive_idle_ack(ack, idle, READY).unwrap();
    (a, b)
}

#[test]
fn epoch_rejects_the_zero_sentinel() {
    assert_eq!(Epoch::new([0; 16]), None);
    assert_eq!(Epoch::new([1; 16]).unwrap().as_bytes(), [1; 16]);
}

#[test]
fn initial_grace_is_not_fabricated_payload_activity() {
    let now = Instant::now();
    let (mut a, _) = pair(now);
    assert_eq!(a.last_demand(), None);
    assert_eq!(a.offer_idle(now, READY), Err(Rejection::RecentlyActive));
    assert!(a.offer_idle(now + IDLE_AFTER, READY).is_ok());
}

#[test]
fn session_suspension_invalidates_tickets_without_restarting_application_idle_age() {
    let now = Instant::now();
    let (mut a, mut b) = pair(now);
    let due = now + IDLE_AFTER;
    let old = a.offer_idle(due, READY).unwrap();
    let ack = b.receive_idle(old, due, READY).unwrap();
    a.suspend_session();
    assert_eq!(a.last_demand(), None);
    assert_eq!(a.phase(), Phase::Active);
    assert_eq!(
        a.receive_idle_ack(ack, due, READY),
        Err(Rejection::StaleAcknowledgement)
    );
    assert_eq!(a.idle_preparation(due, true, false), Ok(()));
    let fresh = a.offer_idle(due, READY).unwrap();
    assert!(fresh.revision > old.revision);
    assert_eq!(fresh.sender_epoch, old.sender_epoch);
    assert_eq!(fresh.receiver_epoch, old.receiver_epoch);
}

#[test]
fn maintenance_does_not_extend_the_activity_window() {
    let now = Instant::now();
    let (mut a, _) = pair(now);
    for _ in 0..100 {
        a.observe_maintenance();
    }
    assert_eq!(a.last_demand(), None);
    assert!(a.offer_idle(now + IDLE_AFTER, READY).is_ok());
}

#[test]
fn revoking_quiet_permission_is_a_bounded_grace_not_application_demand() {
    let now = Instant::now();
    let (mut a, _) = dormant_pair(now);
    let changed = now + IDLE_AFTER + Duration::from_secs(1);
    a.revoke_quiet_permission(changed);
    assert_eq!(a.phase(), Phase::Active);
    assert_eq!(a.last_demand(), None);
    assert_eq!(a.offer_idle(changed, READY), Err(Rejection::RecentlyActive));
    for _ in 0..100 {
        a.observe_maintenance();
    }
    assert!(a.offer_idle(changed + IDLE_AFTER, READY).is_ok());
}

#[test]
fn receiver_waits_for_reply_delivery_before_becoming_dormant() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let offer = a.offer_idle(idle, READY).unwrap();
    let ack = b.receive_idle(offer, idle, READY).unwrap();
    assert_eq!(a.phase(), Phase::Quiescing);
    assert_eq!(b.phase(), Phase::Quiescing);
    b.idle_reply_sent(ack, idle, READY).unwrap();
    a.receive_idle_ack(ack, idle, READY).unwrap();
    assert_eq!(a.phase(), Phase::Dormant);
    assert_eq!(b.phase(), Phase::Dormant);
}

#[test]
fn queued_or_in_flight_work_blocks_both_sides_of_idle_exchange() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let busy = Readiness {
        outstanding_work: true,
        ..READY
    };
    let (mut a, mut b) = pair(now);
    assert_eq!(a.offer_idle(idle, busy), Err(Rejection::OutstandingWork));
    let offer = a.offer_idle(idle, READY).unwrap();
    assert_eq!(
        b.receive_idle(offer, idle, busy),
        Err(Rejection::OutstandingWork)
    );
}

#[test]
fn unavailable_incoming_path_blocks_quiet_even_with_a_live_direct_path() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let unavailable = Readiness {
        incoming_path_available: false,
        ..READY
    };
    let (mut a, mut b) = pair(now);
    assert_eq!(
        a.offer_idle(idle, unavailable),
        Err(Rejection::IncomingPathUnavailable)
    );
    let offer = a.offer_idle(idle, READY).unwrap();
    assert_eq!(
        b.receive_idle(offer, idle, unavailable),
        Err(Rejection::IncomingPathUnavailable)
    );
}

#[test]
fn local_payload_invalidates_an_in_flight_idle_ack() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let offer = a.offer_idle(idle, READY).unwrap();
    let ack = b.receive_idle(offer, idle, READY).unwrap();
    a.demand(idle, true);
    assert_eq!(
        a.receive_idle_ack(ack, idle, READY),
        Err(Rejection::StaleAcknowledgement)
    );
    assert_eq!(a.phase(), Phase::Active);
}

#[test]
fn receiver_payload_invalidates_pending_reply_delivery() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let offer = a.offer_idle(idle, READY).unwrap();
    let ack = b.receive_idle(offer, idle, READY).unwrap();
    b.demand(idle, true);
    assert_eq!(
        b.idle_reply_sent(ack, idle, READY),
        Err(Rejection::StaleAcknowledgement)
    );
    assert_eq!(b.phase(), Phase::Active);
}

#[test]
fn work_and_reachability_are_rechecked_when_committing_an_ack() {
    for readiness in [
        Readiness {
            outstanding_work: true,
            ..READY
        },
        Readiness {
            incoming_path_available: false,
            ..READY
        },
    ] {
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let (mut a, mut b) = pair(now);
        let ack = b
            .receive_idle(a.offer_idle(idle, READY).unwrap(), idle, READY)
            .unwrap();
        assert!(a.receive_idle_ack(ack, idle, readiness).is_err());
        assert!(b.idle_reply_sent(ack, idle, readiness).is_err());
        assert_ne!(a.phase(), Phase::Dormant);
        assert_ne!(b.phase(), Phase::Dormant);
    }
}

#[test]
fn crossed_offers_can_both_finish() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let from_a = a.offer_idle(idle, READY).unwrap();
    let from_b = b.offer_idle(idle, READY).unwrap();
    let ack_a = b.receive_idle(from_a, idle, READY).unwrap();
    let ack_b = a.receive_idle(from_b, idle, READY).unwrap();
    a.idle_reply_sent(ack_b, idle, READY).unwrap();
    b.idle_reply_sent(ack_a, idle, READY).unwrap();
    a.receive_idle_ack(ack_a, idle, READY).unwrap();
    b.receive_idle_ack(ack_b, idle, READY).unwrap();
    assert_eq!(a.phase(), Phase::Dormant);
    assert_eq!(b.phase(), Phase::Dormant);
}

#[test]
fn duplicates_do_not_extend_reply_deadlines() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let offer = a.offer_idle(idle, READY).unwrap();
    let ack = b.receive_idle(offer, idle, READY).unwrap();
    assert_eq!(
        b.receive_idle(offer, idle + Duration::from_secs(9), READY),
        Ok(ack)
    );
    assert_eq!(
        b.idle_reply_sent(ack, idle + IDLE_EXCHANGE_TIMEOUT, READY),
        Err(Rejection::ExchangeExpired)
    );
    assert_eq!(
        a.receive_idle_ack(ack, idle + IDLE_EXCHANGE_TIMEOUT, READY),
        Err(Rejection::ExchangeExpired)
    );
    assert_eq!(a.phase(), Phase::Active);
    assert_eq!(b.phase(), Phase::Active);
}

#[test]
fn stale_requests_cannot_return_after_new_demand_and_another_idle_period() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let offer = a.offer_idle(idle, READY).unwrap();
    b.receive_idle(offer, idle, READY).unwrap();
    b.demand(idle, true);
    assert_eq!(
        b.receive_idle(offer, idle + IDLE_AFTER, READY),
        Err(Rejection::StaleRevision)
    );
}

#[test]
fn wrong_epoch_request_does_not_poison_the_remote_revision() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let valid = a.offer_idle(idle, READY).unwrap();
    let wrong = IdleOffer {
        sender_epoch: Epoch::new([3; 16]).unwrap(),
        revision: u64::MAX,
        ..valid
    };
    assert_eq!(
        b.receive_idle(wrong, idle, READY),
        Err(Rejection::WrongEpoch)
    );
    assert!(b.receive_idle(valid, idle, READY).is_ok());
}

#[test]
fn idle_ack_must_match_the_current_offer_exactly() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let offer = a.offer_idle(idle, READY).unwrap();
    let ack = b.receive_idle(offer, idle, READY).unwrap();
    for wrong in [
        IdleOffer {
            sender_epoch: Epoch::new([3; 16]).unwrap(),
            ..offer
        },
        IdleOffer {
            receiver_epoch: Epoch::new([3; 16]).unwrap(),
            ..offer
        },
        IdleOffer {
            revision: offer.revision + 1,
            ..offer
        },
    ] {
        assert_eq!(
            a.receive_idle_ack(
                IdleAck {
                    offer: wrong,
                    ..ack
                },
                idle,
                READY
            ),
            Err(Rejection::StaleAcknowledgement)
        );
    }
    a.receive_idle_ack(ack, idle, READY).unwrap();
}

#[test]
fn dormant_timer_and_path_expiry_never_start_recovery() {
    let now = Instant::now();
    let (mut a, _) = dormant_pair(now);
    let later = now + Duration::from_hours(24);
    a.expire_exchanges(later);
    a.observe_maintenance();
    a.path_lost(later, false);
    a.path_established();
    assert_eq!(a.phase(), Phase::Dormant);
    assert_eq!(a.offer_idle(later, READY), Err(Rejection::AlreadyDormant));
}

#[test]
fn outstanding_payload_work_can_recover_even_if_demand_notification_was_delayed() {
    let now = Instant::now();
    let (mut a, mut b) = dormant_pair(now);
    let later = now + Duration::from_mins(30);
    a.path_lost(later, true);
    b.network_changed(later, true);
    assert_eq!(a.phase(), Phase::Recovering);
    assert_eq!(b.phase(), Phase::Recovering);
    assert_eq!(a.last_demand(), Some(later));
    assert_eq!(b.last_demand(), Some(later));
}

#[test]
fn either_side_can_wake_a_dormant_peer() {
    for initiator_is_a in [true, false] {
        let now = Instant::now();
        let (mut a, mut b) = dormant_pair(now);
        let (sender, receiver) = if initiator_is_a {
            (&mut a, &mut b)
        } else {
            (&mut b, &mut a)
        };
        let wake = sender.wake(now + Duration::from_mins(30), false).unwrap();
        receiver
            .receive_wake(wake, now + Duration::from_mins(30), true)
            .unwrap();
        assert_eq!(sender.phase(), Phase::Establishing);
        assert_eq!(receiver.phase(), Phase::Active);
        sender.path_established();
        assert_eq!(sender.phase(), Phase::Active);
    }
}

#[test]
fn authenticated_payload_itself_wakes_without_a_separate_wake_exchange() {
    let now = Instant::now();
    let (mut a, _) = dormant_pair(now);
    a.demand(now + Duration::from_mins(30), true);
    assert_eq!(a.phase(), Phase::Active);
}

#[test]
fn retrying_a_wake_announcement_does_not_renew_application_activity() {
    let now = Instant::now();
    let (mut a, _) = dormant_pair(now);
    let demand = now + Duration::from_mins(30);
    a.demand(demand, false);
    for _ in 0..1000 {
        a.announce_wake().unwrap();
        assert_eq!(a.last_demand(), Some(demand));
        assert_eq!(a.phase(), Phase::Establishing);
    }
    assert!(a.offer_idle(demand + IDLE_AFTER, READY).is_ok());
}

#[test]
fn stale_wake_does_not_renew_activity() {
    let now = Instant::now();
    let (mut a, mut b) = dormant_pair(now);
    let at = now + Duration::from_mins(1);
    let wake = a.wake(at, false).unwrap();
    b.receive_wake(wake, at, false).unwrap();
    assert_eq!(
        b.receive_wake(wake, at + Duration::from_secs(1), false),
        Err(Rejection::StaleRevision)
    );
    assert_eq!(b.last_demand(), Some(at));
}

#[test]
fn idle_revision_cannot_be_reused_as_a_wake() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let offer = a.offer_idle(idle, READY).unwrap();
    b.receive_idle(offer, idle, READY).unwrap();
    let wake = Wake {
        sender_epoch: offer.sender_epoch,
        receiver_epoch: offer.receiver_epoch,
        revision: offer.revision,
    };
    assert_eq!(
        b.receive_wake(wake, idle, true),
        Err(Rejection::StaleRevision)
    );
    assert_eq!(b.last_demand(), None);
}

#[test]
fn new_remote_wake_invalidates_pending_old_acknowledgements() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let ack = b
        .receive_idle(a.offer_idle(idle, READY).unwrap(), idle, READY)
        .unwrap();
    let wake = b.wake(idle, true).unwrap();
    a.receive_wake(wake, idle, true).unwrap();
    assert!(a.receive_idle_ack(ack, idle + IDLE_AFTER, READY).is_err());
    assert_eq!(a.phase(), Phase::Active);
}

#[test]
fn network_change_cancels_quiescence_but_does_not_rebuild_a_dormant_mesh() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let ack = b
        .receive_idle(a.offer_idle(idle, READY).unwrap(), idle, READY)
        .unwrap();
    a.network_changed(idle, false);
    assert!(a.receive_idle_ack(ack, idle, READY).is_err());
    assert_eq!(a.phase(), Phase::Active);
    let (mut dormant, _) = dormant_pair(now);
    dormant.network_changed(idle, false);
    assert_eq!(dormant.phase(), Phase::Dormant);
}

#[test]
fn path_failure_recovers_only_when_there_is_real_demand() {
    let now = Instant::now();
    let (mut a, _) = pair(now);
    a.path_lost(now, false);
    assert_eq!(a.phase(), Phase::Active);
    a.demand(now, true);
    a.path_lost(now, false);
    assert_eq!(a.phase(), Phase::Recovering);
    a.path_established();
    a.path_lost(now + IDLE_AFTER, false);
    assert_eq!(a.phase(), Phase::Active);
    a.path_lost(now + IDLE_AFTER, true);
    assert_eq!(a.phase(), Phase::Recovering);
}

#[test]
fn legacy_peers_never_silently_enter_dormancy() {
    let now = Instant::now();
    let (a, mut b) = pair(now);
    let mut old = PeerLifecycle::new(a.local_epoch, a.remote_epoch, false, now);
    assert_eq!(old.phase(), Phase::Legacy);
    assert_eq!(
        old.offer_idle(now + IDLE_AFTER, READY),
        Err(Rejection::Unsupported)
    );
    let offer = b.offer_idle(now + IDLE_AFTER, READY).unwrap();
    assert_eq!(
        old.receive_idle(offer, now + IDLE_AFTER, READY),
        Err(Rejection::Unsupported)
    );
    old.demand(now, false);
    old.network_changed(now, true);
    old.path_lost(now, true);
    assert_eq!(old.phase(), Phase::Legacy);
}

#[test]
fn unsupported_protocol_fallback_cancels_pending_idle_and_cannot_retry_itself() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let ack = b
        .receive_idle(a.offer_idle(idle, READY).unwrap(), idle, READY)
        .unwrap();
    a.use_legacy();
    a.expire_exchanges(idle + Duration::from_hours(1));
    assert_eq!(a.phase(), Phase::Legacy);
    assert_eq!(
        a.receive_idle_ack(ack, idle, READY),
        Err(Rejection::StaleAcknowledgement)
    );
    assert_eq!(
        a.offer_idle(idle + Duration::from_hours(1), READY),
        Err(Rejection::Unsupported)
    );
}

#[test]
fn state_contains_no_heap_owners_and_has_a_small_fixed_footprint() {
    assert!(!std::mem::needs_drop::<PeerLifecycle>());
    assert!(std::mem::size_of::<PeerLifecycle>() <= 512);
}

#[test]
fn revision_exhaustion_never_wraps_into_an_old_ticket() {
    let now = Instant::now();
    let (mut a, _) = pair(now);
    a.local_revision = u64::MAX;
    assert_eq!(
        a.offer_idle(now + IDLE_AFTER, READY),
        Err(Rejection::RevisionExhausted)
    );
    assert_eq!(a.wake(now, false), Err(Rejection::RevisionExhausted));
    assert_eq!(a.local_revision, u64::MAX);
    assert_eq!(a.phase(), Phase::Establishing);
}

#[test]
fn delayed_local_observations_do_not_move_activity_backwards() {
    let now = Instant::now();
    let (mut a, _) = pair(now);
    a.demand(now + Duration::from_secs(10), true);
    a.demand(now, true);
    assert_eq!(a.last_demand(), Some(now + Duration::from_secs(10)));
    assert_eq!(
        a.offer_idle(now + IDLE_AFTER, READY),
        Err(Rejection::RecentlyActive)
    );
}

#[test]
fn pending_exchanges_have_fixed_capacity() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let offer = a.offer_idle(idle, READY).unwrap();
    let reply = b.receive_idle(offer, idle, READY).unwrap();
    for _ in 0..1000 {
        assert_eq!(a.offer_idle(idle, READY), Err(Rejection::ExchangePending));
        assert_eq!(b.receive_idle(offer, idle, READY), Ok(reply));
    }
    assert_eq!(a.pending_offer.unwrap().value, offer);
    assert_eq!(b.pending_reply.unwrap().value, reply);
}

#[test]
fn zero_remote_revision_is_rejected_without_mutating_state() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let offer = a.offer_idle(idle, READY).unwrap();
    assert_eq!(
        b.receive_idle(
            IdleOffer {
                revision: 0,
                ..offer
            },
            idle,
            READY
        ),
        Err(Rejection::StaleRevision)
    );
    assert!(b.remote_revision.is_none());
    assert!(b.receive_idle(offer, idle, READY).is_ok());
}

#[test]
fn new_local_offer_cannot_invalidate_a_reply_waiting_for_delivery() {
    let now = Instant::now();
    let idle = now + IDLE_AFTER;
    let (mut a, mut b) = pair(now);
    let ack = b
        .receive_idle(a.offer_idle(idle, READY).unwrap(), idle, READY)
        .unwrap();
    assert_eq!(b.offer_idle(idle, READY), Err(Rejection::ExchangePending));
    b.idle_reply_sent(ack, idle, READY).unwrap();
    assert_eq!(b.phase(), Phase::Dormant);
}

#[test]
fn short_bilateral_event_traces_never_close_a_recently_active_peer() {
    #[derive(Clone)]
    struct Trace {
        a: PeerLifecycle,
        b: PeerLifecycle,
        ack_a: Option<IdleAck>,
        ack_b: Option<IdleAck>,
    }

    fn walk(trace: &Trace, now: Instant, depth: usize, visits: &mut usize) {
        *visits += 1;
        for peer in [&trace.a, &trace.b] {
            if peer.phase() == Phase::Dormant {
                assert!(
                    peer.last_demand()
                        .is_none_or(|last| now.saturating_duration_since(last) >= IDLE_AFTER)
                );
            }
            assert!(
                peer.pending_offer
                    .is_none_or(|p| p.value.revision == peer.local_revision)
            );
            assert!(
                peer.pending_reply
                    .is_none_or(|p| p.value.responder_revision == peer.local_revision)
            );
        }
        if depth == 0 {
            return;
        }
        for event in 0..10 {
            let mut next = trace.clone();
            match event {
                0 => {
                    next.a.observe_maintenance();
                    next.b.observe_maintenance();
                }
                1 => next.a.demand(now, true),
                2 => next.b.demand(now, true),
                3 => {
                    if let Ok(offer) = next.a.offer_idle(now, READY) {
                        next.ack_a = next.b.receive_idle(offer, now, READY).ok();
                    }
                }
                4 => {
                    if let Ok(offer) = next.b.offer_idle(now, READY) {
                        next.ack_b = next.a.receive_idle(offer, now, READY).ok();
                    }
                }
                5 => {
                    if let Some(ack) = next.ack_a {
                        let _ = next.b.idle_reply_sent(ack, now, READY);
                        let _ = next.a.receive_idle_ack(ack, now, READY);
                    }
                }
                6 => {
                    if let Some(ack) = next.ack_b {
                        let _ = next.a.idle_reply_sent(ack, now, READY);
                        let _ = next.b.receive_idle_ack(ack, now, READY);
                    }
                }
                7 => {
                    next.a.expire_exchanges(now);
                    next.b.expire_exchanges(now);
                }
                8 => {
                    next.a.path_lost(now, false);
                    next.b.path_lost(now, false);
                }
                9 => {
                    next.a.network_changed(now, false);
                    next.b.network_changed(now, false);
                }
                _ => unreachable!(),
            }
            // Includes prompt responses, delayed responses, and idle periods.
            let delay = if depth.is_multiple_of(2) {
                Duration::from_secs(1)
            } else {
                IDLE_AFTER
            };
            walk(&next, now + delay, depth - 1, visits);
        }
    }

    let now = Instant::now();
    let (a, b) = pair(now);
    let mut visits = 0;
    walk(
        &Trace {
            a,
            b,
            ack_a: None,
            ack_b: None,
        },
        now + IDLE_AFTER,
        5,
        &mut visits,
    );
    assert_eq!(visits, 111_111);
}
