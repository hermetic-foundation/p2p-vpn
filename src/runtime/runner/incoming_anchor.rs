//! Automatic relay acquisition uses actual authenticated connections, not advertised transports.
use super::*;

pub(super) fn candidate_addresses(
    peer: Libp2pPeerId,
    advertised: &[Multiaddr],
    known: Option<&Multiaddr>,
    networks: &[LocalInterfaceNetwork],
    source: DiscoveredPeerAddressSource,
) -> Vec<Multiaddr> {
    // Validate remote advertisements before adding the already retained route.
    // A public Identify response cannot introduce a private address this way.
    let validated = ranked_auto_relay_candidate_addresses(peer, advertised, networks, source);
    ranked_auto_relay_candidate_addresses(
        peer,
        validated.iter().chain(known),
        networks,
        DiscoveredPeerAddressSource::LocalDiscovery,
    )
    .into_iter()
    .take(1)
    .collect()
}

#[cfg(test)]
pub(super) fn attempt_at(
    swarm: &mut Swarm<Behaviour>,
    state: &mut AutoRelayState,
    metrics: &RuntimeMetrics,
    epochs: &mut ConnectionEpochs,
    connections: &HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    now: Instant,
) {
    attempt_with_configured_at(swarm, state, metrics, epochs, connections, &[], now);
}

pub(super) fn attempt_with_configured_at(
    swarm: &mut Swarm<Behaviour>,
    state: &mut AutoRelayState,
    metrics: &RuntimeMetrics,
    epochs: &mut ConnectionEpochs,
    connections: &HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    configured_relays: &[(Libp2pPeerId, Multiaddr)],
    now: Instant,
) {
    expire_pending_auto_relay_reservations(swarm, state, metrics, epochs, now);
    for (peer, address) in state.next_reservation_targets_excluding(now, |peer| {
        configured_relays
            .iter()
            .any(|(configured, _)| *configured == peer)
    }) {
        metrics.record_auto_relay_reservation_attempt();
        log_runtime_event(
            LogLevel::Info,
            "auto_relay_reservation_attempt",
            &[
                ("relay", &peer.to_string()),
                ("address", &address.to_string()),
            ],
        );
    }
    let pending = state
        .pending_reservations
        .iter()
        .filter(|(peer, _)| !state.reservation_listeners.contains_key(peer))
        .map(|(peer, pending)| (*peer, pending.clone()))
        .collect::<Vec<_>>();
    for (peer, mut pending) in pending {
        if pending.owned_dial.is_none()
            && state
                .reservation_failures
                .get(&peer)
                .is_some_and(|failures| *failures > 0)
            && transport_kind(&pending.address) == Some(PathKind::DirectTcpStream)
            && let Some(address) = quic_fallback(peer, epochs, connections)
        {
            pending.address = address.clone();
            state
                .pending_reservations
                .get_mut(&peer)
                .expect("pending attempt")
                .address = address;
            log_runtime_event(
                LogLevel::Info,
                "auto_relay_reservation_quic_fallback",
                &[("relay", &peer.to_string())],
            );
        }
        if auto_relay_candidate_address(peer, &pending.address).is_none() {
            failed(
                state,
                metrics,
                peer,
                now,
                "unsupported_or_mismatched_address",
            );
            continue;
        }
        if let Some(id) = matching_connection(peer, &pending, epochs, connections) {
            if !swarm
                .behaviour_mut()
                .relay
                .prefer_relay_connection(peer, id)
            {
                failed(state, metrics, peer, now, "connection_no_longer_owned");
                continue;
            }
            if let Some(unused) = pending.owned_dial
                && unused != id
                && !epochs.established.contains_key(&unused)
            {
                epochs.mark_retiring(unused);
                swarm.close_connection(unused);
            }
            let address = pending.address.with(Protocol::P2pCircuit);
            match swarm.listen_on(address) {
                Ok(listener) if state.record_reservation_listener(peer, listener, id) => {
                    log_runtime_event(
                        LogLevel::Info,
                        "auto_relay_reservation_connection_selected",
                        &[
                            ("relay", &peer.to_string()),
                            ("connection_id", &id.to_string()),
                        ],
                    );
                }
                Ok(listener) => {
                    swarm.remove_listener(listener);
                    failed(state, metrics, peer, now, "listener_ownership_conflict");
                }
                Err(error) => failed(state, metrics, peer, now, &format!("{error:?}")),
            }
        } else if let Some(id) = pending.owned_dial {
            // Admission has one token and deadline, including repeated driver ticks.
            if !epochs.is_usable(id) {
                failed(state, metrics, peer, now, "setup_connection_failed");
            }
        } else {
            let opts = known_peer_dial_options(peer, [pending.address], PeerCondition::Always);
            let id = opts.connection_id();
            match epochs.dial(swarm, opts) {
                Ok(()) => {
                    state
                        .pending_reservations
                        .get_mut(&peer)
                        .expect("pending attempt")
                        .owned_dial = Some(id);
                    log_runtime_event(
                        LogLevel::Info,
                        "auto_relay_reservation_transport_connecting",
                        &[
                            ("relay", &peer.to_string()),
                            ("connection_id", &id.to_string()),
                        ],
                    );
                }
                Err(error) => failed(state, metrics, peer, now, &error.to_string()),
            }
        }
    }
}

fn failed(
    state: &mut AutoRelayState,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    now: Instant,
    reason: &str,
) {
    state.release_reservation_for_retry_after(peer, now);
    let evicted = state.record_reservation_failure(peer);
    metrics.record_auto_relay_reservation_failure();
    log_runtime_event(
        LogLevel::Warn,
        "auto_relay_reservation_failed",
        &[
            ("relay", &peer.to_string()),
            ("error", reason),
            ("evicted", &evicted.to_string()),
        ],
    );
}

fn quic_fallback(
    peer: Libp2pPeerId,
    epochs: &ConnectionEpochs,
    connections: &HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
) -> Option<Multiaddr> {
    let networks = local_interface_networks("");
    connections
        .iter()
        .filter_map(|((owner, id), endpoint)| {
            if *owner != peer
                || !epochs.is_usable(*id)
                || !epochs.established.contains_key(id)
                || endpoint.is_relayed()
                || transport_kind(endpoint.get_remote_address()) != Some(PathKind::DirectQuicStream)
            {
                return None;
            }
            let address = auto_relay_candidate_address(peer, endpoint.get_remote_address())?;
            Some((
                auto_relay_candidate_address_rank(&address, &networks),
                *id,
                address,
            ))
        })
        .min_by_key(|(rank, id, _)| (*rank, *id))
        .map(|(_, _, address)| address)
}

fn matching_connection(
    peer: Libp2pPeerId,
    pending: &PendingAutoRelayReservation,
    epochs: &ConnectionEpochs,
    connections: &HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
) -> Option<ConnectionId> {
    let desired = transport_kind(&pending.address)?;
    connections
        .iter()
        .filter_map(|((owner, id), endpoint)| {
            let address = endpoint.get_remote_address();
            (*owner == peer
                && epochs.is_usable(*id)
                && epochs.established.contains_key(id)
                && !endpoint.is_relayed()
                && transport_kind(address) == Some(desired)
                && (pending.owned_dial == Some(*id)
                    || first_ip_in_multiaddr(address)
                        .is_some_and(|ip| first_ip_in_multiaddr(&pending.address) == Some(ip))
                    || address
                        .iter()
                        .filter(|p| !matches!(p, Protocol::P2p(_)))
                        .eq(pending
                            .address
                            .iter()
                            .filter(|p| !matches!(p, Protocol::P2p(_))))))
            .then_some(*id)
        })
        .min()
}

fn transport_kind(address: &Multiaddr) -> Option<PathKind> {
    if !supports_auto_relay_candidate_transport(address)
        || address.iter().any(|p| p == Protocol::P2pCircuit)
    {
        return None;
    }
    if address.iter().any(|p| matches!(p, Protocol::Tcp(_))) {
        Some(PathKind::DirectTcpStream)
    } else if address
        .iter()
        .any(|p| matches!(p, Protocol::Quic | Protocol::QuicV1))
    {
        Some(PathKind::DirectQuicStream)
    } else {
        None
    }
}

pub(super) fn confirmed_connection(
    swarm: &Swarm<Behaviour>,
    epochs: &ConnectionEpochs,
    connections: &HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    peer: Libp2pPeerId,
    expected: ConnectionId,
) -> Option<(ConnectionId, PathKind)> {
    swarm
        .behaviour()
        .relay
        .confirmed_reservation_connections(peer)
        .filter(|id| *id == expected)
        .filter_map(|id| {
            if !epochs.is_usable(id) || !epochs.established.contains_key(&id) {
                return None;
            }
            let endpoint = connections.get(&(peer, id))?;
            if endpoint.is_relayed() {
                return None;
            }
            Some((id, transport_kind(endpoint.get_remote_address())?))
        })
        .min_by_key(|(id, _)| *id)
}

pub(super) fn confirm_if_ready(
    swarm: &Swarm<Behaviour>,
    epochs: &ConnectionEpochs,
    connections: &HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    state: &mut AutoRelayState,
    readiness: &RelayReadiness,
    peer: Libp2pPeerId,
) {
    if state
        .pending_reservations
        .get(&peer)
        .is_none_or(|pending| Instant::now() >= pending.expires_at)
        || !readiness.relay_ready(peer)
    {
        return;
    }
    let Some(listener) = state.reservation_listeners.get(&peer) else {
        return;
    };
    if !readiness
        .relayed_listen_addresses
        .get(&peer)
        .is_some_and(|addresses| addresses.iter().any(|(owner, _)| *owner == listener.id))
    {
        return;
    }
    let Some((id, kind)) =
        confirmed_connection(swarm, epochs, connections, peer, listener.connection)
    else {
        return;
    };
    state.record_reservation_accepted(peer);
    log_runtime_event(
        LogLevel::Info,
        "auto_relay_incoming_anchor_ready",
        &[
            ("relay", &peer.to_string()),
            ("connection_id", &id.to_string()),
            ("transport", &format!("{kind:?}")),
        ],
    );
}

#[cfg(test)]
mod tests;
