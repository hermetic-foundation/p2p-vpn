//! Quiet changes future maintenance scheduling, never native request ownership.
use super::*;

pub(super) struct Context<'a> {
    pub node: &'a mut P2pNode,
    pub forwarder: &'a Forwarder,
    pub local: &'a ControlCapabilities,
    pub relay: &'a AutoRelayState,
    pub readiness: &'a RelayReadiness,
    pub epochs: &'a ConnectionEpochs,
    pub connections: &'a HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    pub pairing: &'a CodePairingSessions,
    pub discovered: &'a mut DiscoveredPeerAddresses,
    pub now: Instant,
}

fn eligible(
    forwarder: &Forwarder,
    local: &ControlCapabilities,
    incoming: bool,
    pairing: &CodePairingSessions,
    relay_server: bool,
) -> bool {
    local.demand_lifecycle_epoch().is_some()
        && incoming
        && !relay_server
        && !pairing.has_active_open()
        && pairing.active_join_locator().is_none()
        && forwarder
            .configured_overlay_peers()
            .all(|peer| forwarder.data_path_maintenance_is_paused(peer))
}

pub(super) fn reconcile(context: Context<'_>) {
    let paused = eligible(
        context.forwarder,
        context.local,
        incoming_recovery::has_usable_path(
            context.node,
            context.relay,
            context.readiness,
            context.epochs,
            context.connections,
        ),
        context.pairing,
        context.node.startup.relay_server_enabled,
    );
    let behaviour = context.node.swarm.behaviour_mut();
    let mut changed = behaviour
        .kad
        .set_background_jobs_paused(paused && behaviour.kad.mode() == kad::Mode::Client);
    if let Some(kad) = behaviour.pairing_kad.as_mut() {
        changed |= kad.set_background_jobs_paused(paused && kad.mode() == kad::Mode::Client);
    }
    if let Some(autonat) = behaviour.autonat.as_mut() {
        changed |= autonat.set_client_probes_paused(paused);
    }
    let cancelled = context.discovered.recovery_queries.cancel_data_queries(
        |peer| {
            context.forwarder.is_configured_transport_peer(peer)
                && !context
                    .forwarder
                    .data_path_maintenance_is_paused(PeerId::from_libp2p(peer))
        },
        context.now,
    );
    let cancelled = finish_targeted_recovery_queries(&mut behaviour.kad, cancelled);
    if changed || cancelled > 0 {
        log_runtime_event(
            LogLevel::Info,
            "quiet_discovery_policy_changed",
            &[
                ("periodic_paused", &paused.to_string()),
                ("cancelled_data_queries", &cancelled.to_string()),
            ],
        );
    }
}

#[cfg(test)]
mod tests;
