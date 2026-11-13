//! Cached routing addresses do not require permanent transport connections.
use super::*;

const INITIAL_GRACE: Duration = Duration::from_secs(60);

pub(super) struct RetirementContext<'a> {
    pub node: &'a mut P2pNode,
    pub forwarder: &'a Forwarder,
    pub infrastructure: &'a InfrastructurePeers,
    pub routing: &'a RoutingInfrastructurePeers,
    pub queues: &'a PeerQueues,
    pub in_flight: &'a PacketInFlight,
    pub epochs: &'a mut ConnectionEpochs,
    pub connections: &'a HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    pub readiness: &'a RelayReadiness,
    pub auto_relay: &'a AutoRelayState,
    pub pairing: &'a CodePairingSessions,
    pub probes: &'a MembershipProbeConnections,
    pub discovery_quiet: bool,
    pub now: Instant,
}

pub(super) fn wire_busy(node: &P2pNode) -> bool {
    let behaviour = node.swarm.behaviour();
    behaviour.has_pending_work()
        || behaviour.has_pending_routing_work()
        || node
            .swarm
            .network_info()
            .connection_counters()
            .num_pending()
            > 0
}

fn eligible(context: &RetirementContext<'_>, peer: Libp2pPeerId, id: ConnectionId) -> bool {
    let known = context.infrastructure.contains(peer)
        || context.routing.contains(peer)
        || context
            .node
            .bootstrap_peer_addresses
            .iter()
            .any(|(owner, _)| *owner == peer);
    known
        && !context.node.startup.relay_server_enabled
        && !context.forwarder.is_configured_transport_peer(peer)
        && context.discovery_quiet
        && context.epochs.is_usable(id)
        && context
            .epochs
            .established
            .get(&id)
            .is_some_and(|opened| context.now.saturating_duration_since(*opened) >= INITIAL_GRACE)
        && context.connections.contains_key(&(peer, id))
        && !context
            .node
            .relay_peer_addresses
            .iter()
            .any(|(owner, _)| *owner == peer)
        && !context.auto_relay.pending_reservations.contains_key(&peer)
        && !context
            .node
            .swarm
            .behaviour()
            .relay
            .confirmed_reservation_connections(peer)
            .any(|owner| owner == id)
        && !context.pairing.uses_pairing_relay(peer)
        && !context.pairing.allows_pairing_probe(peer)
        && !context
            .probes
            .by_peer
            .get(&peer)
            .is_some_and(|probe| context.now < probe.expires_at)
        && !context
            .connections
            .values()
            .any(|endpoint| relay_peer_for_endpoint(endpoint) == Some(PeerId::from_libp2p(peer)))
}

pub(super) fn retire(context: &mut RetirementContext<'_>) -> usize {
    if !context.discovery_quiet
        || context.node.startup.relay_server_enabled
        || context.queues.total_stats().queued_packets > 0
        || context.in_flight.stats().packets > 0
        || wire_busy(context.node)
        || !incoming_recovery::has_usable_path(
            context.node,
            context.auto_relay,
            context.readiness,
            context.epochs,
            context.connections,
        )
    {
        return 0;
    }
    let mut candidates = context
        .connections
        .keys()
        .copied()
        .filter(|(peer, id)| eligible(context, *peer, *id))
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|(_, id)| *id);
    let mut retired = 0;
    for (peer, id) in candidates {
        if !eligible(context, peer, id) || !context.epochs.mark_retiring(id) {
            continue;
        }
        let closed = context.node.swarm.close_connection(id);
        retired += usize::from(closed);
        log_runtime_event(
            LogLevel::Info,
            "idle_infrastructure_connection_retired",
            &[
                ("peer", &peer.to_string()),
                ("connection_id", &id.to_string()),
                ("close_requested", &closed.to_string()),
            ],
        );
    }
    retired
}

#[cfg(test)]
mod tests;
