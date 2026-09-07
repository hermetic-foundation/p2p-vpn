use std::collections::VecDeque;

use libp2p_swarm::ToSwarm;

use super::Event;
use crate::{
    addresses::{AddressLimits, Reservations, RoutingBudget},
    handler::HandlerIn,
};

pub(super) struct QueuedEvents {
    events: VecDeque<(ToSwarm<Event, HandlerIn>, Option<Reservations>)>,
    routing_budget: Option<RoutingBudget>,
    address_limits: AddressLimits,
}

impl QueuedEvents {
    pub(super) fn new(
        capacity: usize,
        routing_budget: Option<RoutingBudget>,
        address_limits: AddressLimits,
    ) -> Self {
        Self {
            events: VecDeque::with_capacity(capacity),
            routing_budget,
            address_limits,
        }
    }

    pub(super) fn push_back(&mut self, event: ToSwarm<Event, HandlerIn>) {
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
                    return;
                }
                let Some(reservation) = Reservations::notification(budget, address) else {
                    return;
                };
                Some(reservation)
            } else {
                None
            }
        } else {
            None
        };
        self.events.push_back((event, reservation));
    }

    pub(super) fn pop_front(&mut self) -> Option<ToSwarm<Event, HandlerIn>> {
        self.events.pop_front().map(|(event, _reservation)| event)
    }

    pub(super) fn extend(&mut self, events: impl IntoIterator<Item = ToSwarm<Event, HandlerIn>>) {
        for event in events {
            self.push_back(event);
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &ToSwarm<Event, HandlerIn>> {
        self.events.iter().map(|(event, _)| event)
    }

    pub(super) fn retain(&mut self, mut keep: impl FnMut(&ToSwarm<Event, HandlerIn>) -> bool) {
        self.events.retain(|(event, _)| keep(event));
    }

    pub(super) fn len(&self) -> usize {
        self.events.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}
