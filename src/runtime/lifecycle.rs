//! Per-peer policy for the negotiated demand-driven lifecycle.
//!
//! This model performs no I/O. The dispatcher must bind each instance to one
//! currently authorized peer and validate the protocol before applying events.
//! Transport integration must confirm that an accepted idle reply was sent
//! before acting on the receiver's permission to retire a data path.

use std::time::{Duration, Instant};

pub(crate) mod activity;
pub mod protocol;

pub const IDLE_AFTER: Duration = Duration::from_secs(45);
pub const IDLE_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);

/// A fresh runtime identifier, never a durable identity or authorization token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Epoch([u8; 16]);

impl Epoch {
    #[must_use]
    pub const fn new(bytes: [u8; 16]) -> Option<Self> {
        if u128::from_be_bytes(bytes) == 0 {
            None
        } else {
            Some(Self(bytes))
        }
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Legacy,
    Active,
    Quiescing,
    Dormant,
    Establishing,
    Recovering,
}

/// Facts refreshed by the owner, not assertions accepted from the remote peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Readiness {
    pub incoming_path_available: bool,
    /// Includes queued payloads, in-flight delivery, and unfinished service work.
    pub outstanding_work: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdleOffer {
    pub sender_epoch: Epoch,
    pub receiver_epoch: Epoch,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdleAck {
    pub offer: IdleOffer,
    pub responder_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Wake {
    pub sender_epoch: Epoch,
    pub receiver_epoch: Epoch,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rejection {
    Unsupported,
    WrongEpoch,
    StaleRevision,
    OutstandingWork,
    IncomingPathUnavailable,
    RecentlyActive,
    AlreadyDormant,
    ExchangePending,
    RevisionExhausted,
    StaleAcknowledgement,
    ExchangeExpired,
}

#[derive(Clone, Copy, Debug)]
struct Pending<T> {
    value: T,
    started_at: Instant,
}

/// Fixed-size state; ownership, authorization, and table bounds live above it.
#[derive(Clone, Debug)]
pub struct PeerLifecycle {
    local_epoch: Epoch,
    remote_epoch: Epoch,
    supported: bool,
    phase: Phase,
    idle_since: Instant,
    last_demand: Option<Instant>,
    local_revision: u64,
    remote_revision: Option<u64>,
    pending_offer: Option<Pending<IdleOffer>>,
    pending_reply: Option<Pending<IdleAck>>,
}

impl PeerLifecycle {
    /// Start a fresh authenticated epoch pair with a conservative idle grace.
    #[must_use]
    pub const fn new(
        local_epoch: Epoch,
        remote_epoch: Epoch,
        supported: bool,
        now: Instant,
    ) -> Self {
        Self {
            local_epoch,
            remote_epoch,
            supported,
            phase: if supported {
                Phase::Active
            } else {
                Phase::Legacy
            },
            idle_since: now,
            last_demand: None,
            local_revision: 0,
            remote_revision: None,
            pending_offer: None,
            pending_reply: None,
        }
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    #[must_use]
    pub const fn last_demand(&self) -> Option<Instant> {
        self.last_demand
    }

    pub(crate) fn can_prepare_idle(
        &self,
        now: Instant,
        incoming: bool,
        application_work: bool,
    ) -> bool {
        self.idle_eligible(
            now,
            Readiness {
                incoming_path_available: incoming,
                outstanding_work: application_work,
            },
        )
        .is_ok()
    }

    /// A dispatcher-confirmed unsupported protocol disables negotiation for the
    /// epoch. Only a fresh validated connection may retry capability admission.
    pub fn use_legacy(&mut self) {
        self.supported = false;
        self.invalidate_exchange();
        self.phase = Phase::Legacy;
    }

    /// Record authorized payload or explicit application work, never maintenance.
    /// A delayed local observation cannot move the monotonic activity clock back.
    pub fn demand(&mut self, now: Instant, usable_path: bool) {
        self.idle_since = self.idle_since.max(now);
        self.last_demand = Some(self.last_demand.map_or(now, |last| last.max(now)));
        self.invalidate_exchange();
        self.phase = if !self.supported {
            Phase::Legacy
        } else if usable_path {
            Phase::Active
        } else {
            Phase::Establishing
        };
    }

    /// Receiving/sending maintenance can inform transport diagnostics, not demand.
    pub const fn observe_maintenance(&mut self) {}

    /// A known established path does not itself extend application activity.
    pub fn path_established(&mut self) {
        if matches!(self.phase, Phase::Establishing | Phase::Recovering) {
            self.phase = Phase::Active;
        }
    }

    /// Dormant path expiry is expected. Only current work permits path repair.
    pub fn path_lost(&mut self, now: Instant, outstanding_work: bool) {
        if !self.supported || (self.phase == Phase::Dormant && !outstanding_work) {
            return;
        }
        if self.phase == Phase::Dormant {
            self.demand(now, false);
        }
        self.invalidate_exchange();
        self.phase = if self.has_demand(now, outstanding_work) {
            Phase::Recovering
        } else {
            Phase::Active
        };
    }

    /// The network owner repairs incoming reachability separately from peers.
    /// A changed underlay must invalidate late idle acknowledgements.
    pub fn network_changed(&mut self, now: Instant, outstanding_work: bool) {
        if self.supported && self.phase == Phase::Dormant && outstanding_work {
            self.demand(now, false);
        }
        self.invalidate_exchange();
        if self.supported && self.phase != Phase::Dormant {
            self.phase = if self.has_demand(now, outstanding_work) {
                Phase::Recovering
            } else {
                Phase::Active
            };
        }
    }

    /// Authority/readiness invalidation is not application activity. Start a
    /// bounded grace without inventing a new last-demand observation.
    pub fn revoke_quiet_permission(&mut self, now: Instant) {
        self.invalidate_exchange();
        if self.supported {
            self.phase = Phase::Active;
            self.idle_since = self.idle_since.max(now);
        }
    }

    pub fn offer_idle(
        &mut self,
        now: Instant,
        readiness: Readiness,
    ) -> Result<IdleOffer, Rejection> {
        self.expire_exchanges(now);
        if self.phase == Phase::Dormant {
            return Err(Rejection::AlreadyDormant);
        }
        if self.pending_offer.is_some() || self.pending_reply.is_some() {
            return Err(Rejection::ExchangePending);
        }
        self.idle_eligible(now, readiness)?;
        self.advance_revision()?;
        let offer = IdleOffer {
            sender_epoch: self.local_epoch,
            receiver_epoch: self.remote_epoch,
            revision: self.local_revision,
        };
        self.pending_offer = Some(Pending {
            value: offer,
            started_at: now,
        });
        self.phase = Phase::Quiescing;
        Ok(offer)
    }

    /// Prepare a reply without assuming delivery. Call `idle_reply_sent` only
    /// after the transport confirms this exact acknowledgement was sent.
    pub fn receive_idle(
        &mut self,
        offer: IdleOffer,
        now: Instant,
        readiness: Readiness,
    ) -> Result<IdleAck, Rejection> {
        self.expire_exchanges(now);
        if let Some(reply) = self.pending_reply
            && reply.value.offer == offer
        {
            self.idle_eligible(now, readiness)?;
            return Ok(reply.value);
        }
        self.receive_revision(offer.sender_epoch, offer.receiver_epoch, offer.revision)?;
        if self.pending_reply.is_some() {
            return Err(Rejection::ExchangePending);
        }
        self.idle_eligible(now, readiness)?;
        let ack = IdleAck {
            offer,
            responder_revision: self.local_revision,
        };
        self.pending_reply = Some(Pending {
            value: ack,
            started_at: now,
        });
        self.phase = Phase::Quiescing;
        Ok(ack)
    }

    pub fn idle_reply_sent(
        &mut self,
        ack: IdleAck,
        now: Instant,
        readiness: Readiness,
    ) -> Result<(), Rejection> {
        let pending = self.pending_reply.ok_or(Rejection::StaleAcknowledgement)?;
        if pending.value != ack || ack.responder_revision != self.local_revision {
            return Err(Rejection::StaleAcknowledgement);
        }
        if exchange_expired(pending.started_at, now) {
            self.expire_exchanges(now);
            return Err(Rejection::ExchangeExpired);
        }
        self.idle_eligible(now, readiness)?;
        self.pending_reply = None;
        self.phase = Phase::Dormant;
        Ok(())
    }

    pub fn receive_idle_ack(
        &mut self,
        ack: IdleAck,
        now: Instant,
        readiness: Readiness,
    ) -> Result<(), Rejection> {
        let pending = self.pending_offer.ok_or(Rejection::StaleAcknowledgement)?;
        if pending.value != ack.offer || ack.offer.revision != self.local_revision {
            return Err(Rejection::StaleAcknowledgement);
        }
        if exchange_expired(pending.started_at, now) {
            self.expire_exchanges(now);
            return Err(Rejection::ExchangeExpired);
        }
        if self
            .remote_revision
            .is_some_and(|revision| ack.responder_revision < revision)
        {
            return Err(Rejection::StaleRevision);
        }
        self.idle_eligible(now, readiness)?;
        self.remote_revision = Some(ack.responder_revision);
        self.pending_offer = None;
        self.phase = Phase::Dormant;
        Ok(())
    }

    /// Issue a wake only for application demand; control-only work uses its own
    /// bounded exchange and must not keep the data activity lease alive.
    pub fn wake(&mut self, now: Instant, usable_path: bool) -> Result<Wake, Rejection> {
        self.demand(now, usable_path);
        self.announce_wake()
    }

    /// Announce demand already observed by the dispatcher. Retrying this signal
    /// must not turn protocol maintenance into a fresh application activity lease.
    pub(crate) fn announce_wake(&mut self) -> Result<Wake, Rejection> {
        if !self.supported {
            return Err(Rejection::Unsupported);
        }
        // Demand has already invalidated the old exchange, even on exhaustion.
        self.advance_revision()?;
        Ok(Wake {
            sender_epoch: self.local_epoch,
            receiver_epoch: self.remote_epoch,
            revision: self.local_revision,
        })
    }

    pub fn receive_wake(
        &mut self,
        wake: Wake,
        now: Instant,
        usable_path: bool,
    ) -> Result<(), Rejection> {
        self.receive_revision(wake.sender_epoch, wake.receiver_epoch, wake.revision)?;
        self.demand(now, usable_path);
        Ok(())
    }

    /// Timeout cleanup is local. It does not authorize another dial or offer.
    pub fn expire_exchanges(&mut self, now: Instant) {
        if self
            .pending_offer
            .is_some_and(|pending| exchange_expired(pending.started_at, now))
        {
            self.pending_offer = None;
        }
        if self
            .pending_reply
            .is_some_and(|pending| exchange_expired(pending.started_at, now))
        {
            self.pending_reply = None;
        }
        if self.phase == Phase::Quiescing
            && self.pending_offer.is_none()
            && self.pending_reply.is_none()
        {
            self.phase = Phase::Active;
        }
    }

    fn idle_eligible(&self, now: Instant, readiness: Readiness) -> Result<(), Rejection> {
        if !self.supported {
            return Err(Rejection::Unsupported);
        }
        if readiness.outstanding_work {
            return Err(Rejection::OutstandingWork);
        }
        if !readiness.incoming_path_available {
            return Err(Rejection::IncomingPathUnavailable);
        }
        if now.saturating_duration_since(self.idle_since) < IDLE_AFTER {
            return Err(Rejection::RecentlyActive);
        }
        Ok(())
    }

    fn has_demand(&self, now: Instant, outstanding_work: bool) -> bool {
        outstanding_work
            || self
                .last_demand
                .is_some_and(|last| now.saturating_duration_since(last) < IDLE_AFTER)
    }

    fn advance_revision(&mut self) -> Result<(), Rejection> {
        self.local_revision = self
            .local_revision
            .checked_add(1)
            .ok_or(Rejection::RevisionExhausted)?;
        Ok(())
    }

    fn invalidate_exchange(&mut self) {
        self.pending_offer = None;
        self.pending_reply = None;
        // Exhaustion must not wrap and accidentally validate an old ticket.
        let _ = self.advance_revision();
    }

    fn receive_revision(
        &mut self,
        sender: Epoch,
        receiver: Epoch,
        revision: u64,
    ) -> Result<(), Rejection> {
        if !self.supported {
            return Err(Rejection::Unsupported);
        }
        if sender != self.remote_epoch || receiver != self.local_epoch {
            return Err(Rejection::WrongEpoch);
        }
        if revision == 0 || self.remote_revision.is_some_and(|last| revision <= last) {
            return Err(Rejection::StaleRevision);
        }
        self.remote_revision = Some(revision);
        Ok(())
    }
}

fn exchange_expired(started_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(started_at) >= IDLE_EXCHANGE_TIMEOUT
}

#[cfg(test)]
#[path = "lifecycle/tests.rs"]
mod tests;
