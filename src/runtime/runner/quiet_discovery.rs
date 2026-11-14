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

pub(super) struct WorkContext<'a> {
    pub paths: &'a PathSet,
    pub peers: &'a PeerCapabilities,
    pub udp: Option<&'a PacketPlaneRuntime>,
    pub quic: Option<&'a PacketPlaneQuicRuntime>,
    pub queues: &'a PeerQueues,
    pub in_flight: &'a PacketInFlight,
}

/// Scheduling inputs refreshed before each Swarm poll, not a retry ledger.
/// Tickets and native DATA ownership remain exclusively in RecoveryQueries.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct OfflineGate {
    pub(super) activated: bool,
    enabled: bool,
    underlay: u64,
    anchors: Vec<(Libp2pPeerId, ConnectionId)>,
    busy: HashSet<Libp2pPeerId>,
}

impl OfflineGate {
    pub(super) fn budget_enabled(&self) -> bool {
        self.enabled && !self.anchors.is_empty()
    }

    pub(super) fn has_work_or_path(&self, peer: Libp2pPeerId) -> bool {
        self.busy.contains(&peer)
    }
}

pub(super) fn reconcile_data_budgets(
    forwarder: &Forwarder,
    local: &ControlCapabilities,
    epochs: &ConnectionEpochs,
    discovered: &mut DiscoveredPeerAddresses,
    now: Instant,
) {
    discovered.offline.activated = local.demand_lifecycle_epoch().is_some();
    if !discovered.offline.activated || discovered.offline.underlay != epochs.current {
        discovered.offline.enabled = false;
    }
    if !discovered.offline.activated {
        return;
    }
    for peer in forwarder.configured_transport_peers() {
        discovered.recovery_queries.reconcile_data_budget(
            peer,
            forwarder
                .application_demand(PeerId::from_libp2p(peer))
                .and_then(|demand| demand.version),
            epochs.current,
            now,
        );
    }
}

fn refresh_offline_gate(context: &mut Context<'_>, incoming: bool, work: Option<&WorkContext<'_>>) {
    let activated = context.local.demand_lifecycle_epoch().is_some();
    let enabled = activated
        && incoming
        && work.is_some()
        && !context.node.startup.relay_server_enabled
        && !context.pairing.has_active_open()
        && context.pairing.active_join_locator().is_none();
    let mut gate = std::mem::take(&mut context.discovered.offline);
    gate.activated = activated;
    gate.enabled = enabled;
    gate.underlay = context.epochs.current;
    gate.anchors.clear();
    gate.busy.clear();
    if !enabled {
        context.discovered.offline = gate;
        return;
    }
    for (peer, addresses) in &context.readiness.relayed_listen_addresses {
        if !context.readiness.relay_ready(*peer) {
            continue;
        }
        let automatic = context
            .relay
            .reservation_listeners
            .get(peer)
            .filter(|owner| {
                context.relay.accepted_reservation_peers.contains(peer)
                    && addresses.iter().any(|(listener, _)| *listener == owner.id)
            });
        let configured = addresses.iter().any(|(listener, _)| {
            context
                .node
                .configured_relay_reservation_listeners
                .contains(listener)
        });
        for id in context
            .node
            .swarm
            .behaviour()
            .relay
            .confirmed_reservation_connections(*peer)
        {
            if (configured || automatic.is_some_and(|owner| owner.connection == id))
                && incoming_anchor::confirmed_connection(
                    &context.node.swarm,
                    context.epochs,
                    context.connections,
                    *peer,
                    id,
                )
                .is_some()
            {
                gate.anchors.push((*peer, id));
            }
        }
    }
    let work = work.expect("offline suppression requires application-work evidence");
    // PathSet's per-peer lookup scans its candidates. Index owners once, so
    // pathless offline members and connected members need no such scan.
    gate.busy.extend(
        work.paths
            .peer_ids()
            .filter_map(|peer| context.forwarder.transport_peer_for_overlay(peer)),
    );
    for peer in context.forwarder.configured_transport_peers() {
        let has_path = gate.busy.remove(&peer);
        let overlay = PeerId::from_libp2p(peer);
        if context.node.swarm.is_connected(&peer)
            || work.queues.peer_stats(overlay).queued_packets > 0
            || work.in_flight.has_application_work(overlay)
            || (has_path
                && peer_has_healthy_supported_path(
                    work.paths, work.peers, work.udp, work.quic, overlay,
                ))
        {
            gate.busy.insert(peer);
        }
    }
    context.discovered.offline = gate;
}

/// Callback inputs may have changed since the pre-poll snapshot. Fail closed
/// on anchor-loss events, and validate current listener/connection ownership.
pub(super) struct EventContext<'a> {
    pub gate: &'a mut OfflineGate,
    pub local: &'a ControlCapabilities,
    pub pairing: &'a CodePairingSessions,
    pub relay_server: bool,
    pub epochs: &'a ConnectionEpochs,
    pub connections: &'a HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    pub readiness: &'a RelayReadiness,
    pub relay: &'a AutoRelayState,
    pub configured_listeners: &'a HashSet<ListenerId>,
}

pub(super) fn revalidate_event(
    swarm: &Swarm<Behaviour>,
    context: EventContext<'_>,
    event: &SwarmEvent<BehaviourEvent>,
) {
    let gate = context.gate;
    gate.activated = context.local.demand_lifecycle_epoch().is_some();
    gate.enabled &= gate.activated
        && !context.relay_server
        && !context.pairing.has_active_open()
        && context.pairing.active_join_locator().is_none()
        && gate.underlay == context.epochs.current;
    if matches!(event,
        SwarmEvent::ConnectionClosed { connection_id, .. }
            if gate.anchors.iter().any(|(_, id)| id == connection_id)
    ) || matches!(
        event,
        SwarmEvent::ExpiredListenAddr { .. }
            | SwarmEvent::ListenerClosed { .. }
            | SwarmEvent::ListenerError { .. }
    ) {
        gate.enabled = false;
    }
    gate.anchors.retain(|(peer, id)| {
        let Some(addresses) = context.readiness.relayed_listen_addresses.get(peer) else {
            return false;
        };
        let automatic = context
            .relay
            .reservation_listeners
            .get(peer)
            .is_some_and(|owner| {
                owner.connection == *id
                    && context.relay.accepted_reservation_peers.contains(peer)
                    && addresses.iter().any(|(listener, _)| *listener == owner.id)
            });
        let configured = addresses
            .iter()
            .any(|(listener, _)| context.configured_listeners.contains(listener));
        (automatic || configured)
            && context.readiness.relay_ready(*peer)
            && incoming_anchor::confirmed_connection(
                swarm,
                context.epochs,
                context.connections,
                *peer,
                *id,
            )
            .is_some()
    });
}

/// Read-only and ticket-exact. An owned result may use its admitted opportunity;
/// a late ID cannot borrow a newer DATA query's permission to dial.
pub(super) fn offline_suppressed(
    swarm: &Swarm<Behaviour>,
    epochs: &ConnectionEpochs,
    forwarder: &Forwarder,
    discovered: &DiscoveredPeerAddresses,
    peer: Libp2pPeerId,
    result: Option<kad::QueryId>,
    now: Instant,
) -> bool {
    let gate = &discovered.offline;
    gate.enabled
        && gate.underlay == epochs.current
        && forwarder.is_configured_transport_peer(peer)
        && !swarm.is_connected(&peer)
        && !gate.busy.contains(&peer)
        && gate.anchors.iter().any(|(relay, id)| {
            epochs.is_usable(*id)
                && epochs.established.contains_key(id)
                && swarm
                    .behaviour()
                    .relay
                    .confirmed_reservation_connections(*relay)
                    .any(|current| current == *id)
        })
        && discovered.recovery_queries.data_budget_exhausted(
            peer,
            forwarder
                .application_demand(PeerId::from_libp2p(peer))
                .and_then(|demand| demand.version),
            epochs.current,
            now,
        )
        && !result.map_or_else(
            || discovered.recovery_queries.has_data_query(peer),
            |id| discovered.recovery_queries.owns_data_query(peer, &id),
        )
}

pub(super) fn data_suppressed(
    swarm: &Swarm<Behaviour>,
    epochs: &ConnectionEpochs,
    forwarder: &Forwarder,
    discovered: &DiscoveredPeerAddresses,
    peer: Libp2pPeerId,
) -> bool {
    forwarder.data_path_maintenance_is_paused(PeerId::from_libp2p(peer))
        || offline_suppressed(
            swarm,
            epochs,
            forwarder,
            discovered,
            peer,
            None,
            Instant::now(),
        )
}

pub(super) fn draining_data_peer(
    forwarder: &Forwarder,
    epochs: &ConnectionEpochs,
    discovered: &DiscoveredPeerAddresses,
    now: Instant,
) -> Option<Libp2pPeerId> {
    if !discovered.offline.activated || !discovered.recovery_queries.has_pending() {
        return None;
    }
    // RecoveryQueries admits at most one shared native query, so no second
    // collection of DATA IDs or per-peer drain state is necessary.
    forwarder.configured_transport_peers().find(|peer| {
        discovered.recovery_queries.has_data_query(*peer)
            && discovered.recovery_queries.data_budget_exhausted(
                *peer,
                forwarder
                    .application_demand(PeerId::from_libp2p(*peer))
                    .and_then(|demand| demand.version),
                epochs.current,
                now,
            )
    })
}

fn eligible(
    forwarder: &Forwarder,
    local: &ControlCapabilities,
    incoming: bool,
    pairing: &CodePairingSessions,
    relay_server: bool,
    mut offline: impl FnMut(PeerId) -> bool,
) -> bool {
    local.demand_lifecycle_epoch().is_some()
        && incoming
        && !relay_server
        && !pairing.has_active_open()
        && pairing.active_join_locator().is_none()
        && forwarder
            .configured_overlay_peers()
            .all(|peer| forwarder.data_path_maintenance_is_paused(peer) || offline(peer))
}

#[cfg(test)]
pub(super) fn reconcile(context: Context<'_>) {
    reconcile_with_work(context, None);
}

pub(super) fn reconcile_with_work(mut context: Context<'_>, work: Option<&WorkContext<'_>>) {
    reconcile_data_budgets(
        context.forwarder,
        context.local,
        context.epochs,
        context.discovered,
        context.now,
    );
    let incoming = incoming_recovery::has_usable_path(
        context.node,
        context.relay,
        context.readiness,
        context.epochs,
        context.connections,
    );
    refresh_offline_gate(&mut context, incoming, work);
    let paused = eligible(
        context.forwarder,
        context.local,
        incoming,
        context.pairing,
        context.node.startup.relay_server_enabled,
        |peer| {
            context
                .forwarder
                .transport_peer_for_overlay(peer)
                .is_some_and(|peer| {
                    offline_suppressed(
                        &context.node.swarm,
                        context.epochs,
                        context.forwarder,
                        context.discovered,
                        peer,
                        None,
                        context.now,
                    )
                })
        },
    );
    let draining = draining_data_peer(
        context.forwarder,
        context.epochs,
        context.discovered,
        context.now,
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
                && (draining == Some(peer)
                    || !context
                        .forwarder
                        .data_path_maintenance_is_paused(PeerId::from_libp2p(peer)))
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
