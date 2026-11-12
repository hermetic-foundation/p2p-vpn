//! Incoming reachability has its own bounded query owner, not payload demand.
use super::*;

const STARTS_PER_BURST: u8 = 3;

#[derive(Debug)]
pub(super) struct IncomingRecovery {
    query: Option<(kad::QueryId, Instant)>,
    next_due: Instant,
    starts_in_burst: u8,
}

impl IncomingRecovery {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            query: None,
            next_due: now,
            starts_in_burst: 0,
        }
    }

    pub(super) fn owns(&self, id: kad::QueryId) -> bool {
        self.query.is_some_and(|(query, _)| query == id)
    }

    pub(super) fn pending(&self) -> bool {
        self.query.is_some()
    }

    pub(super) fn finish(&mut self, id: kad::QueryId) -> bool {
        if !self.owns(id) {
            return false;
        }
        self.query = None;
        true
    }

    fn cancel(&mut self, kad: &mut kad::Behaviour<kad::store::MemoryStore>) -> usize {
        let Some((query, _)) = self.query.take() else {
            return 0;
        };
        kad.cancel_query(&query);
        1
    }

    pub(super) fn reset(
        &mut self,
        kad: &mut kad::Behaviour<kad::store::MemoryStore>,
        now: Instant,
    ) -> usize {
        let cancelled = self.cancel(kad);
        self.next_due = now;
        self.starts_in_burst = 0;
        cancelled
    }

    fn expire(&mut self, kad: &mut kad::Behaviour<kad::store::MemoryStore>, now: Instant) {
        if let Some((query, started)) = self.query
            && now.saturating_duration_since(started) >= KADEMLIA_MAINTENANCE_QUERY_TIMEOUT
        {
            self.cancel(kad);
            log_runtime_event(
                LogLevel::Warn,
                "incoming_relay_recovery_expired",
                &[("query_id", &format!("{query:?}"))],
            );
        }
    }

    fn started(&mut self, query: kad::QueryId, now: Instant) {
        debug_assert!(!self.pending());
        self.query = Some((query, now));
        self.starts_in_burst += 1;
        let delay = if self.starts_in_burst == STARTS_PER_BURST {
            self.starts_in_burst = 0;
            PUBLIC_DISCOVERY_BACKOFF_MAX
        } else {
            KADEMLIA_MAINTENANCE_INTERVAL
        };
        self.next_due = now + delay;
    }
}

pub(super) struct RecoveryContext<'a> {
    pub state: &'a mut AutoRelayState,
    pub maintenance: &'a mut KademliaMaintenance,
    pub readiness: &'a RelayReadiness,
    pub epochs: &'a ConnectionEpochs,
    pub connections: &'a HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    pub backoff: &'a PublicDiscoveryBackoff,
    pub holdoff_until: Option<Instant>,
    pub metrics: &'a RuntimeMetrics,
}

pub(super) fn has_usable_path(
    node: &P2pNode,
    state: &AutoRelayState,
    readiness: &RelayReadiness,
    epochs: &ConnectionEpochs,
    connections: &HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
) -> bool {
    readiness
        .relayed_listen_addresses
        .iter()
        .any(|(peer, addresses)| {
            if !readiness.relay_ready(*peer) {
                return false;
            }
            if let Some(owner) = state.reservation_listeners.get(peer)
                && state.accepted_reservation_peers.contains(peer)
                && addresses.iter().any(|(listener, _)| *listener == owner.id)
                && incoming_anchor::confirmed_connection(
                    &node.swarm,
                    epochs,
                    connections,
                    *peer,
                    owner.connection,
                )
                .is_some()
            {
                return true;
            }
            addresses.iter().any(|(listener, _)| {
                node.configured_relay_reservation_listeners
                    .contains(listener)
            }) && node
                .swarm
                .behaviour()
                .relay
                .confirmed_reservation_connections(*peer)
                .any(|id| {
                    incoming_anchor::confirmed_connection(
                        &node.swarm,
                        epochs,
                        connections,
                        *peer,
                        id,
                    )
                    .is_some()
                })
        })
}

pub(super) fn drive(node: &mut P2pNode, context: RecoveryContext<'_>, now: Instant) {
    let RecoveryContext {
        state,
        maintenance,
        readiness,
        epochs,
        connections,
        backoff,
        holdoff_until,
        metrics,
    } = context;
    state.exclude_configured_relays(&node.relay_peer_addresses);
    let enabled = (node.discovery.kademlia || node.discovery.autonat)
        && state.policy.max_candidates > 0
        && state.policy.max_reservations > 0;
    if !enabled || has_usable_path(node, state, readiness, epochs, connections) {
        maintenance
            .incoming
            .reset(&mut node.swarm.behaviour_mut().kad, now);
        return;
    }
    maintenance
        .incoming
        .expire(&mut node.swarm.behaviour_mut().kad, now);
    if holdoff_until.is_some_and(|until| now < until) || backoff.suppresses(now) {
        maintenance
            .incoming
            .cancel(&mut node.swarm.behaviour_mut().kad);
        return;
    }
    // Candidates prevent new lookups, not completion of the current one:
    // a newly connected bootstrap may still prove to be routing-only.
    if !state.candidates.is_empty() || !state.pending_reservations.is_empty() {
        return;
    }
    if maintenance.incoming.pending() || now < maintenance.incoming.next_due {
        return;
    }
    let usage = node.swarm.behaviour().kad.query_pool_usage();
    if usage
        .capacity
        .is_some_and(|capacity| usage.retained >= capacity)
    {
        return;
    }
    for (peer, address) in &node.bootstrap_peer_addresses {
        node.swarm
            .behaviour_mut()
            .kad
            .add_protected_address(peer, address.clone());
    }
    if let Some(query) =
        query_auto_relay_infrastructure(&mut node.swarm, metrics, "incoming_path_recovery")
    {
        maintenance.incoming.started(query, now);
    }
}

#[cfg(test)]
mod tests;
