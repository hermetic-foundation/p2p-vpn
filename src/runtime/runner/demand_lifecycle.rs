//! Runtime ownership of lifecycle exchanges. Maintenance never creates demand.

use super::*;
use crate::runtime::lifecycle::{
    Epoch, IDLE_AFTER, IdleAck, PeerLifecycle, Phase, Readiness, Rejection,
    activity::DemandVersion,
    protocol::{
        self, LifecycleCodec, RejectCode, Reply, Request, Response, Session, ValidatedRequest,
        ValidatedResponse,
    },
};

const PEER_REQUESTS_PER_SECOND: u32 = 4;
const NETWORK_REQUESTS_PER_SECOND: u32 = 64;
const RETRY_DELAY: Duration = IDLE_AFTER;

type WireBehaviour = request_response::Behaviour<LifecycleCodec>;
type WireEvent = request_response::Event<Request, Response>;

struct PendingRequest {
    id: request_response::OutboundRequestId,
    started: Instant,
}

struct Flight {
    peer: Libp2pPeerId,
    request: Request,
    underlay_epoch: u64,
    session_generation: u64,
}

struct PendingReply {
    id: request_response::InboundRequestId,
    connection: ConnectionId,
    ack: IdleAck,
    request: Request,
    started: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreparationSnapshot {
    phase: Phase,
    eligible: Result<(), Rejection>,
    readiness: Readiness,
    application_work: bool,
}

struct Peer {
    remote_epoch: Option<Epoch>,
    policy: Option<PeerLifecycle>,
    rate: PacketRateBucket,
    outbound: Option<PendingRequest>,
    inbound: Option<PendingReply>,
    next_offer: Instant,
    wake_after: Option<Instant>,
    observed_demand: Option<DemandVersion>,
    preparation_log: Option<(PreparationSnapshot, Instant)>,
    capabilities_suspended: bool,
    session_generation: u64,
}

impl Peer {
    fn suspend_capabilities(&mut self, context: &LifecycleContext<'_>, peer: Libp2pPeerId) {
        if !self.capabilities_suspended {
            self.capabilities_suspended = true;
            self.session_generation = self.session_generation.saturating_add(1);
            self.outbound = None;
            self.inbound = None;
            self.wake_after = None;
            if let Some(policy) = &mut self.policy {
                if self.session_generation == u64::MAX {
                    policy.use_legacy();
                } else {
                    policy.suspend_session();
                }
            }
        }
        context
            .forwarder
            .revoke_data_path_quiet(PeerId::from_libp2p(peer));
    }

    fn record_preparation(&mut self, snapshot: PreparationSnapshot, now: Instant) -> bool {
        if self.preparation_log.is_some_and(|(previous, last)| {
            previous == snapshot || now.saturating_duration_since(last) < IDLE_AFTER
        }) {
            return false;
        }
        self.preparation_log = Some((snapshot, now));
        true
    }

    fn use_legacy(&mut self, context: &LifecycleContext<'_>, peer: Libp2pPeerId) {
        if let Some(policy) = &mut self.policy {
            policy.use_legacy();
        }
        self.outbound = None;
        self.inbound = None;
        self.wake_after = None;
        context
            .forwarder
            .revoke_data_path_quiet(PeerId::from_libp2p(peer));
    }

    fn refresh(&mut self, local: Epoch, remote: Option<Epoch>, now: Instant) -> bool {
        self.capabilities_suspended = false;
        if self.remote_epoch == remote {
            return false;
        }
        self.remote_epoch = remote;
        self.session_generation = self.session_generation.saturating_add(1);
        self.policy = remote.map(|epoch| {
            PeerLifecycle::new(local, epoch, self.session_generation != u64::MAX, now)
        });
        self.outbound = None;
        self.inbound = None;
        self.next_offer = now + IDLE_AFTER;
        self.wake_after = None;
        self.observed_demand = None;
        true
    }

    fn observe_demand(&mut self, context: &LifecycleContext<'_>, peer: Libp2pPeerId, now: Instant) {
        let overlay = PeerId::from_libp2p(peer);
        let Some(activity) = context.forwarder.application_demand(overlay) else {
            return;
        };
        if activity.version.is_none() || activity.version != self.observed_demand {
            let was_observed = self.observed_demand.is_some();
            self.observed_demand = activity.version;
            self.outbound = None;
            self.inbound = None;
            if let Some(last) = activity.last_demand {
                if let Some(policy) = &mut self.policy {
                    let usable = (context.usable_path)(peer);
                    if self.wake_after.is_none()
                        && (matches!(policy.phase(), Phase::Dormant | Phase::Quiescing) || !usable)
                    {
                        self.wake_after = Some(now);
                    }
                    policy.demand(last, usable);
                }
            } else if was_observed && let Some(policy) = &mut self.policy {
                self.wake_after = None;
                policy.revoke_quiet_permission(now);
            }
        }
        if let Some(policy) = &mut self.policy {
            if (context.usable_path)(peer) {
                policy.path_established();
            }
            if policy.phase() == Phase::Dormant && !context.forwarder.data_path_is_dormant(overlay)
            {
                policy.revoke_quiet_permission(now);
            }
        }
    }

    fn commit_quiet(&mut self, context: &LifecycleContext<'_>, peer: Libp2pPeerId, now: Instant) {
        let overlay = PeerId::from_libp2p(peer);
        let was_quiet = context.forwarder.data_path_is_dormant(overlay);
        if !self
            .observed_demand
            .is_some_and(|version| context.forwarder.grant_data_path_quiet(overlay, version))
        {
            self.observe_demand(context, peer, now);
            if let Some(policy) = &mut self.policy
                && policy.phase() == Phase::Dormant
            {
                policy.revoke_quiet_permission(now);
            }
        } else {
            self.wake_after = None;
            if !was_quiet {
                log_runtime_event(
                    LogLevel::Info,
                    "peer_lifecycle_dormant",
                    &[("peer", &peer.to_string())],
                );
            }
        }
    }

    fn expire(&mut self, now: Instant) {
        if let Some(policy) = &mut self.policy {
            policy.expire_exchanges(now);
        }
        if self
            .outbound
            .as_ref()
            .is_some_and(|pending| expired(pending.started, now))
        {
            self.outbound = None;
        }
        if self
            .inbound
            .as_ref()
            .is_some_and(|pending| expired(pending.started, now))
        {
            self.inbound = None;
        }
    }
}

pub(super) struct LifecycleContext<'a> {
    pub forwarder: &'a Forwarder,
    pub local: &'a ControlCapabilities,
    pub peers: &'a PeerCapabilities,
    pub previous_tags: &'a [String],
    pub connections: &'a ConnectionEpochs,
    pub readiness: &'a dyn Fn(Libp2pPeerId) -> Readiness,
    pub usable_path: &'a dyn Fn(Libp2pPeerId) -> bool,
    pub application_work: &'a dyn Fn(Libp2pPeerId) -> bool,
}

impl LifecycleContext<'_> {
    fn member_authorized(&self, peer: Libp2pPeerId) -> bool {
        self.forwarder
            .transport_peer_for_overlay(PeerId::from_libp2p(peer))
            == Some(peer)
    }

    fn authorized(&self, peer: Libp2pPeerId) -> bool {
        self.member_authorized(peer) && self.peers.contains(PeerId::from_libp2p(peer))
    }

    fn session(&self, peer: Libp2pPeerId, epoch: Epoch) -> Option<Session<'_>> {
        if !self.authorized(peer) || self.local.demand_lifecycle_epoch() != Some(epoch) {
            return None;
        }
        let remote = self.peers.get(PeerId::from_libp2p(peer))?;
        if remote.network_name != self.local.network_name
            || !super::super::control::membership_tag_matches(
                remote.membership_tag.as_deref(),
                self.local.membership_tag.as_deref(),
                self.previous_tags,
            )
        {
            return None;
        }
        Some(Session {
            peer,
            local_epoch: epoch,
            remote_epoch: remote.demand_lifecycle_epoch()?,
            network_name: &self.local.network_name,
            membership_tag: self.local.membership_tag.as_deref(),
            previous_membership_tags: self.previous_tags,
        })
    }
}

pub(super) struct RuntimeInputs<'a> {
    pub forwarder: &'a Forwarder,
    pub local: &'a ControlCapabilities,
    pub peers: &'a PeerCapabilities,
    pub previous_tags: &'a [String],
    pub epochs: &'a ConnectionEpochs,
    pub connections: &'a HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    pub relay_readiness: &'a RelayReadiness,
    pub auto_relay: &'a AutoRelayState,
    pub queues: &'a PeerQueues,
    pub in_flight: &'a PacketInFlight,
    pub paths: &'a PathSet,
    pub udp: Option<&'a PacketPlaneRuntime>,
    pub quic: Option<&'a PacketPlaneQuicRuntime>,
}

impl RuntimeInputs<'_> {
    pub(super) fn with<R>(
        self,
        node: &mut P2pNode,
        action: impl FnOnce(&mut WireBehaviour, &LifecycleContext<'_>) -> R,
    ) -> R {
        let incoming = incoming_recovery::has_usable_path(
            node,
            self.auto_relay,
            self.relay_readiness,
            self.epochs,
            self.connections,
        );
        let pending_transports = node
            .swarm
            .network_info()
            .connection_counters()
            .num_pending()
            > 0;
        let (wire, native_work) = node.swarm.behaviour_mut().split_lifecycle_work();
        let application_work = |peer| {
            let peer = PeerId::from_libp2p(peer);
            self.queues.peer_stats(peer).queued_packets > 0
                || self.in_flight.has_application_work(peer)
        };
        let readiness = |peer| Readiness {
            incoming_path_available: incoming,
            outstanding_work: pending_transports || application_work(peer) || native_work(peer),
        };
        let usable_path = |peer| {
            peer_has_healthy_supported_path(
                self.paths,
                self.peers,
                self.udp,
                self.quic,
                PeerId::from_libp2p(peer),
            )
        };
        action(
            wire,
            &LifecycleContext {
                forwarder: self.forwarder,
                local: self.local,
                peers: self.peers,
                previous_tags: self.previous_tags,
                connections: self.epochs,
                readiness: &readiness,
                usable_path: &usable_path,
                application_work: &application_work,
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct LifecycleSnapshot {
    pub peers: usize,
    pub outbound: usize,
    pub inbound: usize,
    pub dropped: u64,
    pub rate_limited: u64,
}

pub(super) struct DemandLifecycle {
    epoch: Epoch,
    max_peers: usize,
    max_pending: usize,
    peers: HashMap<Libp2pPeerId, Peer>,
    // Semantic cancellation cannot cancel libp2p requests. Keep their slots until
    // a terminal transport event, even after revocation or epoch replacement.
    flights: HashMap<request_response::OutboundRequestId, Flight>,
    network_rate: GlobalRateLimiter,
    underlay_epoch: u64,
    dropped: u64,
    rate_limited: u64,
}

impl DemandLifecycle {
    pub(super) fn new(epoch: Epoch, max_peers: usize, max_pending: usize, now: Instant) -> Self {
        Self {
            epoch,
            max_peers,
            max_pending,
            peers: HashMap::new(),
            flights: HashMap::new(),
            network_rate: GlobalRateLimiter::new(NETWORK_REQUESTS_PER_SECOND, now),
            underlay_epoch: 0,
            dropped: 0,
            rate_limited: 0,
        }
    }

    pub(super) fn snapshot(&self) -> LifecycleSnapshot {
        LifecycleSnapshot {
            peers: self.peers.len(),
            outbound: self.flights.len(),
            inbound: self
                .peers
                .values()
                .filter(|peer| peer.inbound.is_some())
                .count(),
            dropped: self.dropped,
            rate_limited: self.rate_limited,
        }
    }

    pub(super) fn phase(&self, peer: Libp2pPeerId) -> Phase {
        self.peers
            .get(&peer)
            .and_then(|peer| peer.policy.as_ref())
            .map_or(Phase::Legacy, PeerLifecycle::phase)
    }

    pub(super) fn observe_peer(
        &mut self,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        now: Instant,
    ) {
        self.admit_peer(context, peer, now);
    }

    pub(super) fn reconcile(&mut self, context: &LifecycleContext<'_>, now: Instant) {
        let changed = self.underlay_epoch != context.connections.current;
        self.underlay_epoch = context.connections.current;
        // Keep native ownership, but never let re-admission resurrect an old flight.
        for flight in self.flights.values_mut() {
            if !context.member_authorized(flight.peer) {
                flight.session_generation = u64::MAX;
            }
        }
        self.peers.retain(|peer, state| {
            if !context.member_authorized(*peer) {
                context
                    .forwarder
                    .revoke_data_path_quiet(PeerId::from_libp2p(*peer));
                return false;
            }
            if !context.peers.contains(PeerId::from_libp2p(*peer)) {
                state.suspend_capabilities(context, *peer);
            } else {
                let remote = context
                    .session(*peer, self.epoch)
                    .map(|session| session.remote_epoch);
                if state.refresh(self.epoch, remote, now) {
                    context
                        .forwarder
                        .revoke_data_path_quiet(PeerId::from_libp2p(*peer));
                }
            }
            state.observe_demand(context, *peer, now);
            if changed {
                state.outbound = None;
                state.inbound = None;
                if let Some(policy) = &mut state.policy {
                    policy.network_changed(now, (context.application_work)(*peer));
                }
            }
            state.expire(now);
            true
        });
    }

    fn admit_peer(
        &mut self,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        now: Instant,
    ) -> bool {
        if !context.authorized(peer)
            || (!self.peers.contains_key(&peer) && self.peers.len() >= self.max_peers)
        {
            return false;
        }
        let remote = context
            .session(peer, self.epoch)
            .map(|session| session.remote_epoch);
        let state = self.peers.entry(peer).or_insert_with(|| {
            context
                .forwarder
                .revoke_data_path_quiet(PeerId::from_libp2p(peer));
            Peer {
                remote_epoch: remote,
                policy: remote.map(|remote| PeerLifecycle::new(self.epoch, remote, true, now)),
                rate: PacketRateBucket {
                    tokens: PEER_REQUESTS_PER_SECOND,
                    refilled_at: now,
                },
                outbound: None,
                inbound: None,
                next_offer: now + IDLE_AFTER,
                wake_after: None,
                observed_demand: None,
                preparation_log: None,
                capabilities_suspended: false,
                session_generation: 0,
            }
        });
        if state.refresh(self.epoch, remote, now) {
            context
                .forwarder
                .revoke_data_path_quiet(PeerId::from_libp2p(peer));
        }
        state.observe_demand(context, peer, now);
        state.expire(now);
        state.session_generation != u64::MAX
    }

    fn flight_is_current(&self, context: &LifecycleContext<'_>, flight: &Flight) -> bool {
        flight.session_generation != u64::MAX
            && flight.underlay_epoch == context.connections.current
            && self.peers.get(&flight.peer).is_some_and(|state| {
                !state.capabilities_suspended
                    && state.session_generation == flight.session_generation
            })
    }

    fn allow_request(&mut self, peer: Libp2pPeerId, now: Instant) -> bool {
        let state = self.peers.get_mut(&peer).expect("admitted lifecycle peer");
        if !take_rate_limit_token(&mut state.rate, PEER_REQUESTS_PER_SECOND, now)
            || !self.network_rate.allow(now)
        {
            self.rate_limited = self.rate_limited.saturating_add(1);
            return false;
        }
        true
    }

    /// Call only for accepted payload or explicit application work.
    #[cfg(test)]
    pub(super) fn demand(
        &mut self,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        now: Instant,
    ) {
        self.reconcile(context, now);
        context
            .forwarder
            .record_application_demand(PeerId::from_libp2p(peer), now);
        if !self.admit_peer(context, peer, now) {
            if !context.member_authorized(peer) {
                self.peers.remove(&peer);
            }
            return;
        }
        let state = self.peers.get_mut(&peer).expect("admitted lifecycle peer");
        state.outbound = None;
        state.inbound = None;
        if let Some(policy) = &mut state.policy {
            policy.demand(now, (context.usable_path)(peer));
        }
    }

    #[cfg(test)]
    pub(super) fn offer_idle(
        &mut self,
        wire: &mut WireBehaviour,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        now: Instant,
    ) -> Result<(), Rejection> {
        self.reconcile(context, now);
        self.offer_idle_reconciled(wire, context, peer, now)
    }

    fn offer_idle_reconciled(
        &mut self,
        wire: &mut WireBehaviour,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        now: Instant,
    ) -> Result<(), Rejection> {
        if !self.admit_peer(context, peer, now) {
            return Err(Rejection::Unsupported);
        }
        if self.phase(peer) == Phase::Legacy {
            return Err(Rejection::Unsupported);
        }
        if !wire.is_connected(&peer) {
            return Err(Rejection::IncomingPathUnavailable);
        }
        if self.flights.len() >= self.max_pending
            || self.flights.values().any(|pending| pending.peer == peer)
        {
            return Err(Rejection::OutstandingWork);
        }
        let state = self.peers.get_mut(&peer).expect("admitted lifecycle peer");
        let policy = state.policy.as_mut().ok_or(Rejection::Unsupported)?;
        if now < state.next_offer {
            return Err(Rejection::RecentlyActive);
        }
        let offer = policy.offer_idle(now, (context.readiness)(peer))?;
        state.next_offer = now + RETRY_DELAY;
        if !self.allow_request(peer, now) {
            return Err(Rejection::OutstandingWork);
        }
        let request = Request::idle(local_scope(context), offer);
        self.submit(wire, context, peer, request, now);
        Ok(())
    }

    /// Reuse the caller's existing tick; neither a quiet peer nor an absent
    /// control path creates a dial or a renewable application activity lease.
    pub(super) fn drive(
        &mut self,
        wire: &mut WireBehaviour,
        context: &LifecycleContext<'_>,
        now: Instant,
    ) {
        self.reconcile(context, now);
        for peer in context.forwarder.configured_transport_peers() {
            if !self.admit_peer(context, peer, now) {
                continue;
            }
            self.prepare_idle(context, peer, now);
            if matches!(self.phase(peer), Phase::Legacy | Phase::Dormant) {
                continue;
            }
            if let Some(after) = self.peers[&peer].wake_after {
                let recent = context
                    .forwarder
                    .application_demand(PeerId::from_libp2p(peer))
                    .and_then(|demand| demand.last_demand)
                    .is_some_and(|last| now.saturating_duration_since(last) < IDLE_AFTER);
                if !recent && !(context.application_work)(peer) {
                    self.peers.get_mut(&peer).unwrap().wake_after = None;
                } else {
                    if now >= after {
                        self.announce_wake(wire, context, peer, now);
                    }
                    continue;
                }
            }
            let _ = self.offer_idle_reconciled(wire, context, peer, now);
        }
    }

    fn prepare_idle(&mut self, context: &LifecycleContext<'_>, peer: Libp2pPeerId, now: Instant) {
        let state = self.peers.get_mut(&peer).expect("admitted lifecycle peer");
        let readiness = (context.readiness)(peer);
        let application_work = (context.application_work)(peer);
        let eligible = state
            .policy
            .as_ref()
            .filter(|_| {
                !state.capabilities_suspended && context.session(peer, self.epoch).is_some()
            })
            .map_or(Err(Rejection::Unsupported), |policy| {
                policy.idle_preparation(now, readiness.incoming_path_available, application_work)
            });
        let overlay = PeerId::from_libp2p(peer);
        if !(eligible.is_ok()
            && state.observed_demand.is_some_and(|version| {
                context
                    .forwarder
                    .pause_data_path_maintenance(overlay, version)
            }))
        {
            context.forwarder.resume_data_path_maintenance(overlay);
        }
        let snapshot = PreparationSnapshot {
            phase: state
                .policy
                .as_ref()
                .map_or(Phase::Legacy, PeerLifecycle::phase),
            eligible,
            readiness,
            application_work,
        };
        if context.local.demand_lifecycle_epoch().is_some()
            && state.record_preparation(snapshot, now)
        {
            let reason =
                eligible.map_or_else(|reason| format!("{reason:?}"), |()| "ready".to_owned());
            log_runtime_event(
                LogLevel::Info,
                "peer_lifecycle_idle_preparation",
                &[
                    ("peer", &peer.to_string()),
                    ("phase", &format!("{:?}", snapshot.phase)),
                    ("reason", &reason),
                    (
                        "incoming_path_available",
                        &readiness.incoming_path_available.to_string(),
                    ),
                    ("application_work", &application_work.to_string()),
                    ("outstanding_work", &readiness.outstanding_work.to_string()),
                    (
                        "maintenance_paused",
                        &context
                            .forwarder
                            .data_path_maintenance_is_paused(overlay)
                            .to_string(),
                    ),
                ],
            );
        }
    }

    fn announce_wake(
        &mut self,
        wire: &mut WireBehaviour,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        now: Instant,
    ) {
        if !wire.is_connected(&peer)
            || self.flights.len() >= self.max_pending
            || self.flights.values().any(|pending| pending.peer == peer)
            || !self.allow_request(peer, now)
        {
            return;
        }
        let state = self.peers.get_mut(&peer).expect("admitted lifecycle peer");
        state.wake_after = Some(now + RETRY_DELAY);
        let Some(policy) = &mut state.policy else {
            return;
        };
        let Ok(wake) = policy.announce_wake() else {
            return;
        };
        let request = Request::wake(local_scope(context), wake);
        self.submit(wire, context, peer, request, now);
        log_runtime_event(
            LogLevel::Info,
            "peer_lifecycle_wake_queued",
            &[("peer", &peer.to_string())],
        );
    }

    #[cfg(test)]
    pub(super) fn wake(
        &mut self,
        wire: &mut WireBehaviour,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        now: Instant,
    ) -> Result<(), Rejection> {
        self.reconcile(context, now);
        self.demand(context, peer, now);
        if self.phase(peer) == Phase::Legacy {
            return Err(Rejection::Unsupported);
        }
        if !wire.is_connected(&peer) {
            return Err(Rejection::IncomingPathUnavailable);
        }
        if self.flights.len() >= self.max_pending
            || self.flights.values().any(|pending| pending.peer == peer)
        {
            return Err(Rejection::OutstandingWork);
        }
        if !self.peers.contains_key(&peer) {
            return Err(Rejection::Unsupported);
        }
        if !self.allow_request(peer, now) {
            return Err(Rejection::OutstandingWork);
        }
        let state = self.peers.get_mut(&peer).ok_or(Rejection::Unsupported)?;
        let wake = state
            .policy
            .as_mut()
            .ok_or(Rejection::Unsupported)?
            .wake(now, (context.usable_path)(peer))?;
        let request = Request::wake(local_scope(context), wake);
        self.submit(wire, context, peer, request, now);
        Ok(())
    }

    fn submit(
        &mut self,
        wire: &mut WireBehaviour,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        request: Request,
        now: Instant,
    ) {
        let id = wire.send_request(&peer, request.clone());
        self.peers
            .get_mut(&peer)
            .expect("admitted lifecycle peer")
            .outbound = Some(PendingRequest { id, started: now });
        self.flights.insert(
            id,
            Flight {
                peer,
                request,
                underlay_epoch: context.connections.current,
                session_generation: self.peers[&peer].session_generation,
            },
        );
    }

    pub(super) fn handle_event(
        &mut self,
        wire: &mut WireBehaviour,
        context: &LifecycleContext<'_>,
        event: WireEvent,
        now: Instant,
    ) {
        self.reconcile(context, now);
        let completed = match &event {
            request_response::Event::Message {
                peer,
                message: Message::Response { request_id, .. },
                ..
            }
            | request_response::Event::OutboundFailure {
                peer, request_id, ..
            } => Some((*peer, *request_id)),
            _ => None,
        };
        let completed = completed.and_then(|(peer, id)| {
            self.flights
                .get(&id)
                .is_some_and(|flight| flight.peer == peer)
                .then(|| self.flights.remove(&id))
                .flatten()
        });
        match event {
            request_response::Event::Message {
                peer,
                connection_id,
                message,
            } => {
                if !context.connections.is_usable(connection_id)
                    || !self.admit_peer(context, peer, now)
                {
                    self.dropped = self.dropped.saturating_add(1);
                    return;
                }
                match message {
                    Message::Request {
                        request_id,
                        request,
                        channel,
                    } => {
                        // Excess requests are dropped, not answered with another flood.
                        if !self.allow_request(peer, now) {
                            return;
                        }
                        let reply = self.receive_request(
                            context,
                            peer,
                            request_id,
                            connection_id,
                            &request,
                            now,
                        );
                        if wire
                            .send_response(channel, Response::for_request(&request, reply))
                            .is_err()
                            && self
                                .peers
                                .get(&peer)
                                .and_then(|state| state.inbound.as_ref())
                                .is_some_and(|pending| {
                                    pending.id == request_id && pending.connection == connection_id
                                })
                        {
                            self.peers
                                .get_mut(&peer)
                                .expect("admitted lifecycle peer")
                                .inbound = None;
                        }
                    }
                    Message::Response {
                        request_id,
                        response,
                    } => self.receive_response(
                        context,
                        peer,
                        request_id,
                        &response,
                        completed.as_ref(),
                        now,
                    ),
                }
            }
            request_response::Event::ResponseSent {
                peer,
                connection_id,
                request_id,
            } => {
                let Some(state) = self.peers.get_mut(&peer) else {
                    return;
                };
                let Some(pending) = state.inbound.as_ref() else {
                    return;
                };
                if pending.id != request_id || pending.connection != connection_id {
                    return;
                }
                let pending = state.inbound.take().expect("matched lifecycle reply");
                if context.connections.is_usable(connection_id)
                    && context.session(peer, self.epoch).is_some_and(|session| {
                        session.validate_request(
                            peer,
                            |peer| context.authorized(peer),
                            &pending.request,
                        ) == Ok(ValidatedRequest::Idle(pending.ack.offer))
                    })
                    && let Some(policy) = &mut state.policy
                {
                    if policy
                        .idle_reply_sent(pending.ack, now, (context.readiness)(peer))
                        .is_ok()
                    {
                        state.commit_quiet(context, peer, now);
                    }
                }
            }
            request_response::Event::OutboundFailure {
                peer,
                connection_id,
                request_id,
                error,
                ..
            } => {
                if matches!(
                    error,
                    request_response::OutboundFailure::UnsupportedProtocols
                ) && let Some(flight) = completed.as_ref()
                    && self.flight_is_current(context, flight)
                    && context.connections.is_usable(connection_id)
                    && context.session(peer, self.epoch).is_some_and(|session| {
                        session
                            .validate_outbound_request(
                                peer,
                                |peer| context.authorized(peer),
                                &flight.request,
                            )
                            .is_ok()
                    })
                    && let Some(state) = self.peers.get_mut(&peer)
                {
                    state.use_legacy(context, peer);
                }
                if let Some(state) = self.peers.get_mut(&peer)
                    && state
                        .outbound
                        .as_ref()
                        .is_some_and(|pending| pending.id == request_id)
                {
                    state.outbound = None;
                }
            }
            request_response::Event::InboundFailure {
                peer,
                connection_id,
                request_id,
                ..
            } => {
                if let Some(state) = self.peers.get_mut(&peer)
                    && state.inbound.as_ref().is_some_and(|pending| {
                        pending.id == request_id && pending.connection == connection_id
                    })
                {
                    state.inbound = None;
                }
            }
        }
    }

    fn receive_request(
        &mut self,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        request_id: request_response::InboundRequestId,
        connection: ConnectionId,
        request: &Request,
        now: Instant,
    ) -> Reply {
        let Some(session) = context.session(peer, self.epoch) else {
            return Reply::Rejected {
                reason: RejectCode::Unsupported,
            };
        };
        let Ok(validated) =
            session.validate_request(peer, |peer| context.authorized(peer), request)
        else {
            return Reply::Rejected {
                reason: RejectCode::Unauthorized,
            };
        };
        let inbound_full = self.snapshot().inbound >= self.max_pending;
        let state = self.peers.get_mut(&peer).expect("admitted lifecycle peer");
        let Some(policy) = &mut state.policy else {
            return Reply::Rejected {
                reason: RejectCode::Unsupported,
            };
        };
        match validated {
            ValidatedRequest::Idle(offer) => {
                if state.inbound.is_some() || inbound_full {
                    return Reply::Rejected {
                        reason: RejectCode::Busy,
                    };
                }
                match policy.receive_idle(offer, now, (context.readiness)(peer)) {
                    Ok(ack) => {
                        state.inbound = Some(PendingReply {
                            id: request_id,
                            connection,
                            ack,
                            request: request.clone(),
                            started: now,
                        });
                        Reply::IdleAccepted {
                            responder_revision: ack.responder_revision,
                        }
                    }
                    Err(reason) => Reply::Rejected {
                        reason: reason.into(),
                    },
                }
            }
            ValidatedRequest::Wake(wake) => {
                match policy.receive_wake(wake, now, (context.usable_path)(peer)) {
                    Ok(()) => {
                        context
                            .forwarder
                            .record_application_demand(PeerId::from_libp2p(peer), now);
                        state.observed_demand = context
                            .forwarder
                            .application_demand(PeerId::from_libp2p(peer))
                            .and_then(|snapshot| snapshot.version);
                        state.inbound = None;
                        state.outbound = None;
                        state.wake_after = None;
                        Reply::WakeAccepted {}
                    }
                    Err(reason) => Reply::Rejected {
                        reason: reason.into(),
                    },
                }
            }
        }
    }

    fn receive_response(
        &mut self,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        request_id: request_response::OutboundRequestId,
        response: &Response,
        flight: Option<&Flight>,
        now: Instant,
    ) {
        let Some(flight) = flight.filter(|flight| self.flight_is_current(context, flight)) else {
            return;
        };
        let Some(session) = context.session(peer, self.epoch) else {
            return;
        };
        let Ok(response) = session.validate_response(
            peer,
            |peer| context.authorized(peer),
            &flight.request,
            response,
        ) else {
            return;
        };
        let Some(state) = self.peers.get_mut(&peer) else {
            return;
        };
        if response == ValidatedResponse::Rejected(RejectCode::Unsupported) {
            state.use_legacy(context, peer);
            return;
        }
        if !state
            .outbound
            .as_ref()
            .is_some_and(|pending| pending.id == request_id)
        {
            return;
        }
        state.outbound = None;
        let Some(policy) = &mut state.policy else {
            return;
        };
        match response {
            ValidatedResponse::IdleAccepted(ack) => {
                if policy
                    .receive_idle_ack(ack, now, (context.readiness)(peer))
                    .is_ok()
                {
                    state.commit_quiet(context, peer, now);
                }
            }
            ValidatedResponse::WakeAccepted => state.wake_after = None,
            ValidatedResponse::Rejected(_) => {}
        }
    }
}

fn local_scope(context: &LifecycleContext<'_>) -> protocol::Scope {
    protocol::Scope {
        network_name: context.local.network_name.clone(),
        membership_tag: context.local.membership_tag.clone(),
    }
}

fn expired(started: Instant, now: Instant) -> bool {
    now.saturating_duration_since(started) >= protocol::REQUEST_TIMEOUT
}

#[cfg(test)]
mod tests;
