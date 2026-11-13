//! Coalesced control updates share the membership page owner's native slots.

use super::*;

pub(super) const DELIVERY_WINDOW: Duration = Duration::from_secs(45);
const RETRY_DELAY: Duration = Duration::from_secs(5);
const MAX_ATTEMPTS: u8 = 3;

#[derive(Debug)]
pub(super) enum PendingControlSync {
    Records(Box<PendingMembershipRecordSync>),
    Capabilities {
        peer: Libp2pPeerId,
        generation: u64,
        deadline: Instant,
        cancellation: futures::future::AbortHandle,
    },
    Retired {
        peer: Libp2pPeerId,
    },
}

impl PendingControlSync {
    pub(super) fn cancel(&self) {
        match self {
            Self::Records(work) => {
                if let Some(cancellation) = &work.cancellation {
                    cancellation.abort();
                }
            }
            Self::Capabilities { cancellation, .. } => cancellation.abort(),
            Self::Retired { .. } => (),
        }
    }
    pub(super) fn peer(&self) -> Libp2pPeerId {
        match self {
            Self::Records(work) => work.peer,
            Self::Capabilities { peer, .. } | Self::Retired { peer } => *peer,
        }
    }

    pub(super) fn records(&self) -> Option<&PendingMembershipRecordSync> {
        if let Self::Records(work) = self {
            Some(work)
        } else {
            None
        }
    }
}

#[derive(Debug)]
struct Round {
    deadline: Instant,
    lan_until: Instant,
    attempts: u8,
    lan_attempted: bool,
}

#[derive(Debug, Default)]
pub(super) struct Recipient {
    acknowledged: u64,
    exhausted: bool,
    retry_at: Option<Instant>,
    last_connection: Option<ConnectionId>,
    last_requested_demand: Option<crate::runtime::lifecycle::activity::DemandVersion>,
}

#[derive(Debug, Default)]
pub(super) struct CapabilityPublication {
    current: Option<ControlCapabilities>,
    generation: u64,
    exhausted: bool,
    pub peers: HashMap<Libp2pPeerId, Recipient>,
    rounds: HashMap<Libp2pPeerId, Round>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CapabilityTicket {
    peer: Libp2pPeerId,
    generation: u64,
    deadline: Instant,
}

pub(super) enum Completion {
    Unowned,
    Discard,
    Failed,
    Reply(CapabilityTicket),
}

impl CapabilityPublication {
    fn observe(&mut self, current: ControlCapabilities, forwarder: &Forwarder) {
        if self.exhausted {
            return;
        }
        if self.current.as_ref() != Some(&current) {
            let Some(next) = self.generation.checked_add(1) else {
                self.exhausted = true;
                log_runtime_event(LogLevel::Warn, "control_delivery_generation_exhausted", &[]);
                return;
            };
            self.current = Some(current);
            self.generation = next;
            self.rounds.clear();
            for recipient in self.peers.values_mut() {
                recipient.exhausted = false;
            }
        }
        self.retain_authorization(forwarder);
        let limit = MAX_MEMBERSHIP_RECORDS.max(forwarder.config().peers.len());
        for peer in forwarder.control_capability_peers() {
            if self.peers.len() >= limit && !self.peers.contains_key(&peer) {
                continue;
            }
            self.peers.entry(peer).or_default();
        }
    }

    pub(super) fn retain_authorization(&mut self, forwarder: &Forwarder) {
        self.peers
            .retain(|peer, _| forwarder.authorizes_control_capabilities(*peer));
        self.rounds
            .retain(|peer, _| forwarder.authorizes_control_capabilities(*peer));
    }
}

impl MembershipRecordSyncs {
    pub(super) fn occupied_slots(&self) -> usize {
        self.pending.len()
            + self
                .capabilities
                .rounds
                .keys()
                .filter(|peer| !self.pending.values().any(|work| work.peer() == **peer))
                .count()
    }

    pub(super) fn queue_capabilities(
        &mut self,
        capabilities: ControlCapabilities,
        forwarder: &Forwarder,
    ) {
        let old = self.capabilities.generation;
        self.capabilities.observe(capabilities, forwarder);
        if old != self.capabilities.generation || self.capabilities.exhausted {
            for (id, work) in &mut self.pending {
                if matches!(work, PendingControlSync::Capabilities { .. }) {
                    let peer = work.peer();
                    work.cancel();
                    *work = PendingControlSync::Retired { peer };
                    if self.active_by_peer.get(&peer) == Some(id) {
                        self.active_by_peer.remove(&peer);
                    }
                }
            }
        }
    }

    pub(super) fn retry_capabilities_after_network_change(&mut self) {
        self.capabilities.rounds.clear();
        for recipient in self.capabilities.peers.values_mut() {
            recipient.exhausted = false;
        }
    }

    pub(super) fn control_connection_available(&mut self, peer: Libp2pPeerId, id: ConnectionId) {
        if let Some(recipient) = self.capabilities.peers.get_mut(&peer)
            && recipient.last_connection.is_none_or(|last| id > last)
        {
            recipient.last_connection = Some(id);
            recipient.acknowledged = 0;
            if recipient.exhausted {
                self.capabilities.rounds.remove(&peer);
                recipient.exhausted = false;
            }
        }
    }

    pub(super) fn request_capabilities(&mut self, peer: Libp2pPeerId, forwarder: &Forwarder) {
        let demand = forwarder
            .application_demand(PeerId::from_libp2p(peer))
            .filter(|snapshot| snapshot.last_demand.is_some())
            .and_then(|snapshot| snapshot.version);
        if let Some(recipient) = self.capabilities.peers.get_mut(&peer) {
            if demand.is_some() && recipient.last_requested_demand != demand {
                recipient.last_requested_demand = demand;
                recipient.exhausted = false;
                self.capabilities.rounds.remove(&peer);
            }
            recipient.acknowledged = 0;
        }
    }

    pub(super) fn control_delivery_deadline(
        &self,
        peer: Libp2pPeerId,
        forwarder: &Forwarder,
        now: Instant,
    ) -> Option<Instant> {
        self.pending
            .values()
            .filter_map(|work| match work {
                PendingControlSync::Capabilities {
                    peer: expected,
                    generation,
                    deadline,
                    cancellation,
                } if *expected == peer
                    && *generation == self.capabilities.generation
                    && forwarder.authorizes_control_capabilities(peer)
                    && !cancellation.is_aborted()
                    && now < *deadline =>
                {
                    Some(*deadline)
                }
                PendingControlSync::Records(work)
                    if work.peer == peer
                        && forwarder.authorizes_membership_sync(peer)
                        && now < work.deadline
                        && work
                            .cancellation
                            .as_ref()
                            .is_some_and(|cancellation| !cancellation.is_aborted()) =>
                {
                    Some(work.deadline)
                }
                _ => None,
            })
            .max()
    }

    pub(super) fn capability_response_may_advertise(
        &self,
        peer: Libp2pPeerId,
        id: request_response::OutboundRequestId,
        forwarder: &Forwarder,
        now: Instant,
    ) -> bool {
        match self.pending.get(&id) {
            Some(PendingControlSync::Capabilities {
                peer: expected,
                generation,
                deadline,
                ..
            }) => {
                *expected == peer
                    && *generation == self.capabilities.generation
                    && now < *deadline
                    && forwarder.authorizes_control_capabilities(peer)
            }
            Some(_) => false,
            None => true,
        }
    }

    pub(super) fn expire_control_work(&mut self, now: Instant, metrics: &RuntimeMetrics) {
        let expired_pages = self
            .pending
            .iter()
            .filter_map(|(id, work)| {
                work.records()
                    .filter(|work| now >= work.deadline)
                    .map(|_| *id)
            })
            .collect::<Vec<_>>();
        for id in expired_pages {
            let work = self.pending.get_mut(&id).expect("owned expired page");
            let peer = work.peer();
            work.cancel();
            *work = PendingControlSync::Retired { peer };
            if self.active_by_peer.get(&peer) == Some(&id) {
                self.active_by_peer.remove(&peer);
            }
            fail_membership_record_sync_at(self, metrics, peer, "operation_expired", now);
        }
        for (id, work) in &mut self.pending {
            if let PendingControlSync::Capabilities { peer, deadline, .. } = work
                && now >= *deadline
            {
                let peer = *peer;
                work.cancel();
                *work = PendingControlSync::Retired { peer };
                if self.active_by_peer.get(&peer) == Some(id) {
                    self.active_by_peer.remove(&peer);
                }
            }
        }
        let publication = &mut self.capabilities;
        publication.rounds.retain(|peer, round| {
            if now >= round.deadline
                || (round.attempts >= MAX_ATTEMPTS && !self.active_by_peer.contains_key(peer))
            {
                if let Some(recipient) = publication.peers.get_mut(peer) {
                    recipient.exhausted = true;
                }
                log_runtime_event(
                    LogLevel::Info,
                    "control_delivery_round_exhausted",
                    &[("peer", &peer.to_string())],
                );
                false
            } else {
                true
            }
        });
    }

    pub(super) fn drive_capabilities(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        forwarder: &Forwarder,
        metrics: &RuntimeMetrics,
        now: Instant,
    ) {
        self.expire_control_work(now, metrics);
        if self.capabilities.exhausted {
            return;
        }
        let generation = self.capabilities.generation;
        if self.capabilities.current.is_none() {
            return;
        }
        if !self
            .capabilities
            .peers
            .values()
            .any(|recipient| !recipient.exhausted && recipient.acknowledged != generation)
        {
            return;
        }
        let mut peers = self.capabilities.peers.keys().copied().collect::<Vec<_>>();
        peers.sort_by_key(|peer| !swarm.behaviour().control.is_connected(peer));
        for peer in peers {
            if !forwarder.authorizes_control_capabilities(peer)
                || self.pending.values().any(|work| work.peer() == peer)
            {
                continue;
            }
            let recipient = &self.capabilities.peers[&peer];
            if recipient.exhausted
                || recipient.acknowledged == generation
                || recipient.retry_at.is_some_and(|retry| now < retry)
            {
                continue;
            }
            if !self.capabilities.rounds.contains_key(&peer)
                && self.occupied_slots() >= MAX_CONCURRENT_MEMBERSHIP_SYNCS
            {
                continue;
            }
            let connected = swarm.behaviour().control.is_connected(&peer);
            let recipient = self.capabilities.peers.get_mut(&peer).unwrap();
            let round = self
                .capabilities
                .rounds
                .entry(peer)
                .or_insert_with(|| Round {
                    deadline: now + DELIVERY_WINDOW,
                    lan_until: if connected {
                        now
                    } else {
                        now + PEER_RECOVERY_LAN_FIRST_GRACE
                    },
                    attempts: 0,
                    lan_attempted: false,
                });
            if !connected && now < round.lan_until {
                continue;
            }
            let deadline = round.deadline;
            if self.pending.len() >= MAX_CONCURRENT_MEMBERSHIP_SYNCS {
                continue;
            }
            round.attempts += 1;
            recipient.retry_at = Some(now + RETRY_DELAY);
            let (id, cancellation) = swarm.behaviour_mut().control.send_request_cancellable(
                &peer,
                ControlRequest::Capabilities(self.capabilities.current.as_ref().unwrap().clone()),
                deadline,
            );
            self.pending.insert(
                id,
                PendingControlSync::Capabilities {
                    peer,
                    generation,
                    deadline,
                    cancellation,
                },
            );
            self.active_by_peer.insert(peer, id);
            metrics.record_control_request_sent();
            log_runtime_event(
                LogLevel::Info,
                "control_delivery_requested",
                &[
                    ("peer", &peer.to_string()),
                    ("generation", &generation.to_string()),
                ],
            );
        }
    }

    /// Retire only the authenticated native owner. A stale semantic round keeps
    /// its slot until this terminal event, but can never acknowledge new work.
    pub(super) fn finish_capability_event(
        &mut self,
        event: &request_response::Event<ControlRequest, ControlResponse>,
        forwarder: &Forwarder,
        local: &ControlCapabilities,
        previous_tags: &[String],
        usable: bool,
        now: Instant,
    ) -> Completion {
        let (peer, id, response) = match event {
            request_response::Event::Message {
                peer,
                message:
                    Message::Response {
                        request_id,
                        response,
                    },
                ..
            } => (*peer, *request_id, Some(response)),
            request_response::Event::OutboundFailure {
                peer, request_id, ..
            } => (*peer, *request_id, None),
            _ => return Completion::Unowned,
        };
        let Some(work) = self.pending.get(&id) else {
            return Completion::Unowned;
        };
        if work.peer() != peer {
            return Completion::Discard;
        }
        if work.records().is_some() {
            return Completion::Unowned;
        }
        let work = self.pending.remove(&id).unwrap();
        if self.active_by_peer.get(&peer) == Some(&id) {
            self.active_by_peer.remove(&peer);
        }
        let PendingControlSync::Capabilities {
            generation,
            deadline,
            ..
        } = work
        else {
            return Completion::Discard;
        };
        let valid = usable && now < deadline && generation == self.capabilities.generation
            && forwarder.authorizes_control_capabilities(peer)
            && response.is_some_and(|response| matches!(response,
                ControlResponse::CapabilitiesAccepted(capabilities) if crate::runtime::control::validate_capabilities(
                    capabilities, &local.network_name, local.membership_tag.as_deref(), previous_tags,
                ).is_none()));
        if let Some(recipient) = self.capabilities.peers.get_mut(&peer) {
            recipient.retry_at = Some(now + RETRY_DELAY);
        }
        if valid {
            Completion::Reply(CapabilityTicket {
                peer,
                generation,
                deadline,
            })
        } else {
            log_runtime_event(
                LogLevel::Info,
                "control_delivery_attempt_failed",
                &[
                    ("peer", &peer.to_string()),
                    ("generation", &generation.to_string()),
                ],
            );
            Completion::Failed
        }
    }

    pub(super) fn acknowledge_capabilities(
        &mut self,
        ticket: CapabilityTicket,
        forwarder: &Forwarder,
        accepted: bool,
        now: Instant,
    ) {
        if accepted
            && now < ticket.deadline
            && ticket.generation == self.capabilities.generation
            && forwarder.authorizes_control_capabilities(ticket.peer)
            && let Some(recipient) = self.capabilities.peers.get_mut(&ticket.peer)
        {
            recipient.acknowledged = ticket.generation;
            self.capabilities.rounds.remove(&ticket.peer);
            recipient.retry_at = None;
        }
    }
}

pub(super) fn drive(
    syncs: &mut MembershipRecordSyncs,
    node: &mut P2pNode,
    forwarder: &Forwarder,
    epochs: &mut ConnectionEpochs,
    discovered: &mut DiscoveredPeerAddresses,
    metrics: &RuntimeMetrics,
    now: Instant,
) {
    syncs.drive_capabilities(&mut node.swarm, forwarder, metrics, now);
    if !syncs
        .capabilities
        .rounds
        .values()
        .any(|round| !round.lan_attempted && now < round.lan_until)
    {
        return;
    }
    let local_networks = local_interface_networks("");
    let candidates = node
        .configured_peer_addresses
        .iter()
        .cloned()
        .chain(discovered.redial_candidates_at(now))
        .collect::<Vec<_>>();
    for (peer, round) in &mut syncs.capabilities.rounds {
        if round.lan_attempted
            || now >= round.lan_until
            || now >= round.deadline
            || !forwarder.authorizes_control_capabilities(*peer)
            || node.swarm.is_connected(peer)
        {
            continue;
        }
        round.lan_attempted = true;
        start_peer_lan_first_recovery(discovered, *peer, now, "dirty_control_delivery");
        let addresses = candidates
            .iter()
            .filter(|(candidate, address)| {
                candidate == peer
                    && recovery_address_allowed_during_lan_first(
                        *peer,
                        address,
                        &node.configured_peer_addresses,
                        &local_networks,
                    )
            })
            .map(|(_, address)| address.clone())
            .collect::<Vec<_>>();
        if !addresses.is_empty() {
            let _ = dial_known_peer_addresses(
                &mut node.swarm,
                epochs,
                *peer,
                addresses,
                PeerCondition::DisconnectedAndNotDialing,
            );
        }
    }
}

#[cfg(test)]
mod tests;
