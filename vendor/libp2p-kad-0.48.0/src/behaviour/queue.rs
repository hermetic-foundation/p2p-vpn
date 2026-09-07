mod payload;

use std::{collections::VecDeque, num::NonZeroUsize};

use libp2p_identity::PeerId;
use libp2p_swarm::{dial_opts::DialOpts, ToSwarm};

use super::Event;
use crate::{
    addresses::{AddressLimits, Reservations, RoutingBudget},
    handler::HandlerIn,
};

/// Aggregate unsent actions and intermediate results for one behaviour.
#[derive(Clone, Copy, Debug)]
pub struct BehaviourQueueLimits {
    events: usize,
    bytes: usize,
}

impl BehaviourQueueLimits {
    pub fn new(events: NonZeroUsize, bytes: NonZeroUsize) -> Self {
        Self {
            events: events.get(),
            bytes: bytes.get(),
        }
    }
}

/// Payload includes vector capacities where available, not allocator overhead.
/// RoutingUpdated snapshots retain their separate routing reservations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BehaviourQueueUsage {
    pub event_limit: Option<usize>,
    pub byte_limit: Option<usize>,
    pub events: usize,
    pub bytes: usize,
    pub rejected: u64,
}

pub(super) struct QueuedEvents {
    events: VecDeque<(ToSwarm<Event, HandlerIn>, Option<Reservations>, usize)>,
    limits: Option<BehaviourQueueLimits>,
    usage: BehaviourQueueUsage,
    routing_budget: Option<RoutingBudget>,
    address_limits: AddressLimits,
}

impl QueuedEvents {
    pub(super) fn new(
        capacity: usize,
        routing_budget: Option<RoutingBudget>,
        address_limits: AddressLimits,
        limits: Option<BehaviourQueueLimits>,
    ) -> Self {
        Self {
            events: VecDeque::with_capacity(limits.map_or(capacity, |limits| limits.events)),
            limits,
            usage: BehaviourQueueUsage::default(),
            routing_budget,
            address_limits,
        }
    }

    pub(super) fn push_dial(&mut self, peer: PeerId) -> bool {
        self.insert(
            ToSwarm::Dial {
                opts: DialOpts::peer_id(peer).build(),
            },
            0,
        )
    }

    pub(super) fn usage(&self) -> BehaviourQueueUsage {
        BehaviourQueueUsage {
            events: self.events.len(),
            event_limit: self.limits.map(|limits| limits.events),
            byte_limit: self.limits.map(|limits| limits.bytes),
            ..self.usage
        }
    }

    pub(super) fn is_bounded(&self) -> bool {
        self.limits.is_some()
    }

    pub(super) fn pop_query_progress(
        &mut self,
        id: &crate::QueryId,
    ) -> Option<ToSwarm<Event, HandlerIn>> {
        let index = self.events.iter().position(|(event, _, _)| matches!(
            event, ToSwarm::GenerateEvent(Event::OutboundQueryProgressed { id: pending, .. }) if pending == id
        ))?;
        let (event, _reservation, bytes) = self.events.remove(index)?;
        self.usage.bytes -= bytes;
        Some(event)
    }

    pub(super) fn push_back(&mut self, event: ToSwarm<Event, HandlerIn>) -> bool {
        let bytes = match &event {
            ToSwarm::GenerateEvent(event) => {
                let Some(bytes) = payload::event_bytes(event) else {
                    self.usage.rejected = self.usage.rejected.saturating_add(1);
                    return false;
                };
                bytes
            }
            ToSwarm::NotifyHandler { event, .. } => payload::handler_bytes(event),
            ToSwarm::NewExternalAddrOfPeer { address, .. } => address.len(),
            // Addressless dials use push_dial; other swarm commands are not queued by Kademlia.
            _ => {
                self.usage.rejected = self.usage.rejected.saturating_add(1);
                return false;
            }
        };
        self.insert(event, bytes)
    }

    fn insert(&mut self, mut event: ToSwarm<Event, HandlerIn>, bytes: usize) -> bool {
        if self.limits.is_some_and(|limits| {
            self.events.len() >= limits.events
                || bytes > limits.bytes.saturating_sub(self.usage.bytes)
        }) {
            self.usage.rejected = self.usage.rejected.saturating_add(1);
            return false;
        }
        if self.limits.is_some() {
            match &mut event {
                ToSwarm::GenerateEvent(event) => payload::normalize_event(event),
                ToSwarm::NotifyHandler { event, .. } => payload::normalize_handler(event),
                ToSwarm::NewExternalAddrOfPeer { address, .. } => {
                    *address = crate::addresses::normalized_address(address);
                }
                _ => {}
            }
        } else if self.routing_budget.is_some() {
            match &mut event {
                ToSwarm::NewExternalAddrOfPeer { address, .. }
                | ToSwarm::GenerateEvent(
                    Event::RoutablePeer { address, .. }
                    | Event::PendingRoutablePeer { address, .. },
                ) => {
                    *address = crate::addresses::normalized_address(address);
                }
                _ => {}
            }
        }
        let reservation = if let Some(budget) = &self.routing_budget {
            let address = match &event {
                ToSwarm::NewExternalAddrOfPeer { address, .. }
                | ToSwarm::GenerateEvent(
                    Event::RoutablePeer { address, .. }
                    | Event::PendingRoutablePeer { address, .. },
                ) => Some(Some(address)),
                ToSwarm::GenerateEvent(Event::UnroutablePeer { .. }) => Some(None),
                // RoutingUpdated already owns charged Addresses snapshots.
                _ => None,
            };
            if let Some(address) = address {
                if address.is_some_and(|address| !self.address_limits.accepts(address)) {
                    self.usage.rejected = self.usage.rejected.saturating_add(1);
                    return false;
                }
                let Some(reservation) = Reservations::notification(budget, address) else {
                    self.usage.rejected = self.usage.rejected.saturating_add(1);
                    return false;
                };
                Some(reservation)
            } else {
                None
            }
        } else {
            None
        };
        self.usage.bytes = self.usage.bytes.saturating_add(bytes);
        self.events.push_back((event, reservation, bytes));
        true
    }

    pub(super) fn pop_front(&mut self) -> Option<ToSwarm<Event, HandlerIn>> {
        self.events.pop_front().map(|(event, _reservation, bytes)| {
            self.usage.bytes -= bytes;
            event
        })
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &ToSwarm<Event, HandlerIn>> {
        self.events.iter().map(|(event, _, _)| event)
    }

    pub(super) fn retain(&mut self, mut keep: impl FnMut(&ToSwarm<Event, HandlerIn>) -> bool) {
        self.events.retain(|(event, _, bytes)| {
            if keep(event) {
                true
            } else {
                self.usage.bytes -= bytes;
                false
            }
        });
    }

    pub(super) fn len(&self) -> usize {
        self.events.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}
