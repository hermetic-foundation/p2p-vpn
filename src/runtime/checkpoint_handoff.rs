//! Short-lived delivery of an exact-base command, never a retained membership ledger.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use libp2p::PeerId;

use crate::membership::checkpoint::{
    CheckpointBoundary, MAX_CHECKPOINT_MEMBERS, SignedMembershipMutation,
};

pub(crate) const HANDOFF_WINDOW: Duration = Duration::from_secs(15);
pub(crate) const ATTEMPT_WINDOW: Duration = Duration::from_secs(5);
pub(crate) const MAX_IN_FLIGHT: usize = 32;
const MAX_ATTEMPTS: u8 = 3;
const RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeliveryOutcome {
    Acknowledged,
    RetryableFailure,
    DefiniteFailure,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct HandoffReport {
    pub(crate) recipients: usize,
    pub(crate) acknowledged: usize,
    pub(crate) failed: usize,
    pub(crate) pending: usize,
    pub(crate) attempts: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeliveryState {
    Ready(Instant),
    InFlight(Instant),
    Acknowledged,
    Failed,
}

#[derive(Clone, Copy, Debug)]
struct Delivery {
    attempts: u8,
    state: DeliveryState,
}

pub(crate) struct MutationHandoff {
    mutation: SignedMembershipMutation,
    expected: CheckpointBoundary,
    recipients: BTreeMap<PeerId, Delivery>,
    deadline: Instant,
}

impl std::fmt::Debug for MutationHandoff {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MutationHandoff")
            .field("report", &self.report())
            .finish_non_exhaustive()
    }
}

impl MutationHandoff {
    pub(crate) fn new(
        mutation: SignedMembershipMutation,
        expected: CheckpointBoundary,
        recipients: impl IntoIterator<Item = PeerId>,
        now: Instant,
    ) -> Result<Self, &'static str> {
        let mut bounded = BTreeMap::new();
        for peer in recipients {
            if bounded.len() >= MAX_CHECKPOINT_MEMBERS && !bounded.contains_key(&peer) {
                return Err("too many mutation handoff recipients");
            }
            bounded.insert(
                peer,
                Delivery {
                    attempts: 0,
                    state: DeliveryState::Ready(now),
                },
            );
        }
        if mutation.payload.base.authority_revision.checked_add(1)
            != Some(expected.authority_revision)
            || expected.digest == [0; 32]
        {
            return Err("mutation handoff boundary mismatch");
        }
        Ok(Self {
            mutation,
            expected,
            recipients: bounded,
            deadline: now + HANDOFF_WINDOW,
        })
    }

    pub(crate) fn mutation(&self) -> &SignedMembershipMutation {
        &self.mutation
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) fn acknowledges(&self, boundary: CheckpointBoundary) -> bool {
        boundary == self.expected
    }

    pub(crate) fn report(&self) -> HandoffReport {
        let mut report = HandoffReport {
            recipients: self.recipients.len(),
            ..HandoffReport::default()
        };
        for delivery in self.recipients.values() {
            report.attempts += usize::from(delivery.attempts);
            match delivery.state {
                DeliveryState::Acknowledged => report.acknowledged += 1,
                DeliveryState::Failed => report.failed += 1,
                DeliveryState::Ready(_) | DeliveryState::InFlight(_) => report.pending += 1,
            }
        }
        report
    }

    pub(crate) fn is_in_flight(&self, peer: PeerId) -> bool {
        self.recipients
            .get(&peer)
            .is_some_and(|delivery| matches!(delivery.state, DeliveryState::InFlight(_)))
    }

    pub(crate) fn advance_clock(&mut self, now: Instant) {
        for delivery in self.recipients.values_mut() {
            if now >= self.deadline {
                if matches!(
                    delivery.state,
                    DeliveryState::Ready(_) | DeliveryState::InFlight(_)
                ) {
                    delivery.state = DeliveryState::Failed;
                }
            } else if matches!(delivery.state, DeliveryState::InFlight(deadline) if now >= deadline)
            {
                Self::resolve_delivery(delivery, DeliveryOutcome::RetryableFailure, now);
            }
        }
    }

    pub(crate) fn next_ready(&mut self, now: Instant) -> Option<PeerId> {
        if now >= self.deadline
            || self
                .recipients
                .values()
                .filter(|delivery| matches!(delivery.state, DeliveryState::InFlight(_)))
                .count()
                >= MAX_IN_FLIGHT
        {
            return None;
        }
        let peer = self
            .recipients
            .iter()
            .filter_map(|(peer, delivery)| {
                if let DeliveryState::Ready(ready) = delivery.state {
                    (now >= ready).then_some((*peer, ready))
                } else {
                    None
                }
            })
            .min_by_key(|(peer, ready)| (*ready, *peer))
            .map(|(peer, _)| peer)?;
        let delivery = self
            .recipients
            .get_mut(&peer)
            .expect("selected recipient exists");
        delivery.attempts += 1;
        delivery.state = DeliveryState::InFlight((now + ATTEMPT_WINDOW).min(self.deadline));
        Some(peer)
    }

    pub(crate) fn resolve(&mut self, peer: PeerId, outcome: DeliveryOutcome, now: Instant) {
        self.advance_clock(now);
        if let Some(delivery) = self.recipients.get_mut(&peer)
            && matches!(delivery.state, DeliveryState::InFlight(_))
        {
            Self::resolve_delivery(delivery, outcome, now);
        }
    }

    fn resolve_delivery(delivery: &mut Delivery, outcome: DeliveryOutcome, now: Instant) {
        delivery.state = match outcome {
            DeliveryOutcome::Acknowledged => DeliveryState::Acknowledged,
            DeliveryOutcome::RetryableFailure if delivery.attempts < MAX_ATTEMPTS => {
                DeliveryState::Ready(now + RETRY_DELAY)
            }
            DeliveryOutcome::RetryableFailure | DeliveryOutcome::DefiniteFailure => {
                DeliveryState::Failed
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        identity::NodeIdentity,
        membership::checkpoint::{
            CheckpointMember, CooperativeMembershipState, MembershipChange, NetworkAnchor,
            NetworkCapability, SnapshotPolicy,
        },
    };

    use super::*;

    fn handoff(recipients: impl IntoIterator<Item = PeerId>, now: Instant) -> MutationHandoff {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let capability =
            NetworkCapability::from_secret(NetworkAnchor::new([7; 32]).unwrap(), Some(&[9; 32]))
                .unwrap();
        let mut state = CooperativeMembershipState::bootstrap_at(
            capability,
            identity.peer_id.clone(),
            vec![CheckpointMember::new(&identity).unwrap()],
            SnapshotPolicy::default(),
            1,
        )
        .unwrap();
        let mutation = state
            .sign_mutation_at(
                &identity,
                MembershipChange::RemoveMember(identity.peer_id.clone()),
                1,
            )
            .unwrap();
        state.apply_mutation_at(&mutation, 1).unwrap();
        MutationHandoff::new(
            mutation,
            state.snapshot().payload.boundary().unwrap(),
            recipients,
            now,
        )
        .unwrap()
    }

    #[test]
    fn in_flight_slots_recipient_count_and_attempts_are_bounded() {
        let now = Instant::now();
        let peers = (0..MAX_CHECKPOINT_MEMBERS)
            .map(|_| PeerId::random())
            .collect::<Vec<_>>();
        let mut operation = handoff(peers.clone(), now);
        let mut sent = Vec::new();
        while let Some(peer) = operation.next_ready(now) {
            sent.push(peer);
        }
        assert_eq!(sent.len(), MAX_IN_FLIGHT);
        assert_eq!(operation.report().attempts, MAX_IN_FLIGHT);
        for peer in sent {
            operation.resolve(peer, DeliveryOutcome::Acknowledged, now);
        }
        assert!(operation.next_ready(now).is_some());
        let mut too_many = peers;
        too_many.push(PeerId::random());
        assert!(
            MutationHandoff::new(
                operation.mutation.clone(),
                operation.expected,
                too_many,
                now
            )
            .is_err()
        );
    }

    #[test]
    fn duplicate_recipients_and_acknowledgments_do_not_create_history_or_retries() {
        let now = Instant::now();
        let peer = PeerId::random();
        let mut operation = handoff([peer, peer], now);
        assert_eq!(operation.report().recipients, 1);
        assert!(operation.acknowledges(operation.expected));
        let mut wrong = operation.expected;
        wrong.digest[0] ^= 1;
        assert!(!operation.acknowledges(wrong));
        assert_eq!(operation.next_ready(now), Some(peer));
        operation.resolve(peer, DeliveryOutcome::Acknowledged, now);
        operation.resolve(peer, DeliveryOutcome::RetryableFailure, now);
        assert_eq!(
            operation.report(),
            HandoffReport {
                recipients: 1,
                acknowledged: 1,
                failed: 0,
                pending: 0,
                attempts: 1
            }
        );
        assert_eq!(operation.next_ready(now + HANDOFF_WINDOW), None);
        assert!(!format!("{operation:?}").contains(&peer.to_string()));
    }

    #[test]
    fn retries_do_not_extend_deadline_and_stop_after_three_attempts() {
        let now = Instant::now();
        let peer = PeerId::random();
        let mut operation = handoff([peer], now);
        for attempt in 0..3 {
            let at = now + RETRY_DELAY * attempt;
            assert_eq!(operation.next_ready(at), Some(peer));
            operation.resolve(peer, DeliveryOutcome::RetryableFailure, at);
        }
        assert_eq!(operation.report().attempts, 3);
        assert_eq!(operation.report().failed, 1);
        assert_eq!(operation.next_ready(now + Duration::from_secs(3)), None);
        let mut operation = handoff([peer], now);
        assert_eq!(operation.next_ready(now), Some(peer));
        operation.advance_clock(now + ATTEMPT_WINDOW);
        assert!(!operation.is_in_flight(peer));
        assert_eq!(operation.next_ready(now + ATTEMPT_WINDOW), None);
        assert_eq!(
            operation.next_ready(now + ATTEMPT_WINDOW + RETRY_DELAY),
            Some(peer)
        );
        operation.advance_clock(now + HANDOFF_WINDOW);
        assert_eq!(operation.report().pending, 0);
        assert_eq!(operation.report().failed, 1);
        assert_eq!(operation.next_ready(now + HANDOFF_WINDOW), None);
    }

    #[test]
    fn fresh_recipients_are_not_starved_by_failed_low_id_retries() {
        let now = Instant::now();
        let mut peers = (0..MAX_IN_FLIGHT + 1)
            .map(|_| PeerId::random())
            .collect::<Vec<_>>();
        peers.sort();
        let last = *peers.last().unwrap();
        let mut operation = handoff(peers, now);
        let first = operation.next_ready(now).unwrap();
        for _ in 1..MAX_IN_FLIGHT {
            operation.next_ready(now).unwrap();
        }
        operation.resolve(first, DeliveryOutcome::RetryableFailure, now);
        assert_eq!(operation.next_ready(now + RETRY_DELAY), Some(last));
    }

    #[test]
    fn rejected_delivery_is_terminal_and_late_ack_cannot_revive_it() {
        let now = Instant::now();
        let peer = PeerId::random();
        let mut operation = handoff([peer], now);
        assert_eq!(operation.next_ready(now), Some(peer));
        operation.resolve(peer, DeliveryOutcome::DefiniteFailure, now);
        operation.resolve(
            peer,
            DeliveryOutcome::Acknowledged,
            now + Duration::from_secs(1),
        );
        assert_eq!(operation.report().failed, 1);
        assert_eq!(operation.report().acknowledged, 0);
        assert_eq!(operation.next_ready(now + Duration::from_secs(1)), None);
    }
}
