//! Runtime ownership of lifecycle exchanges. Maintenance never creates demand.

use super::*;
use crate::runtime::lifecycle::{
    Epoch, IDLE_AFTER, IdleAck, PeerLifecycle, Phase, Readiness, Rejection,
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
    request: Request,
    started: Instant,
}

struct PendingReply {
    id: request_response::InboundRequestId,
    connection: ConnectionId,
    ack: IdleAck,
    request: Request,
    started: Instant,
}

struct Peer {
    remote_epoch: Option<Epoch>,
    policy: Option<PeerLifecycle>,
    rate: PacketRateBucket,
    outbound: Option<PendingRequest>,
    inbound: Option<PendingReply>,
    next_offer: Instant,
}

impl Peer {
    fn refresh(&mut self, local: Epoch, remote: Option<Epoch>, now: Instant) {
        if self.remote_epoch == remote {
            return;
        }
        self.remote_epoch = remote;
        self.policy = remote.map(|epoch| PeerLifecycle::new(local, epoch, true, now));
        self.outbound = None;
        self.inbound = None;
        self.next_offer = now + IDLE_AFTER;
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
}

impl LifecycleContext<'_> {
    fn authorized(&self, peer: Libp2pPeerId) -> bool {
        self.forwarder.is_configured_transport_peer(peer)
            && self.peers.contains(PeerId::from_libp2p(peer))
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
    flights: HashMap<request_response::OutboundRequestId, Libp2pPeerId>,
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
        self.peers.retain(|peer, state| {
            if !context.authorized(*peer) {
                return false;
            }
            let remote = context
                .session(*peer, self.epoch)
                .map(|session| session.remote_epoch);
            state.refresh(self.epoch, remote, now);
            if changed {
                state.outbound = None;
                state.inbound = None;
                if let Some(policy) = &mut state.policy {
                    policy.network_changed(now, (context.readiness)(*peer).outstanding_work);
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
        let state = self.peers.entry(peer).or_insert_with(|| Peer {
            remote_epoch: remote,
            policy: remote.map(|remote| PeerLifecycle::new(self.epoch, remote, true, now)),
            rate: PacketRateBucket {
                tokens: PEER_REQUESTS_PER_SECOND,
                refilled_at: now,
            },
            outbound: None,
            inbound: None,
            next_offer: now + IDLE_AFTER,
        });
        state.refresh(self.epoch, remote, now);
        state.expire(now);
        true
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
    pub(super) fn demand(
        &mut self,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        now: Instant,
    ) {
        if !self.admit_peer(context, peer, now) {
            if !context.authorized(peer) {
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

    pub(super) fn offer_idle(
        &mut self,
        wire: &mut WireBehaviour,
        context: &LifecycleContext<'_>,
        peer: Libp2pPeerId,
        now: Instant,
    ) -> Result<(), Rejection> {
        self.reconcile(context, now);
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
            || self.flights.values().any(|pending| *pending == peer)
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
        let id = wire.send_request(&peer, request.clone());
        self.flights.insert(id, peer);
        self.peers
            .get_mut(&peer)
            .expect("admitted lifecycle peer")
            .outbound = Some(PendingRequest {
            id,
            request,
            started: now,
        });
        Ok(())
    }

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
            || self.flights.values().any(|pending| *pending == peer)
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
        let id = wire.send_request(&peer, request.clone());
        state.outbound = Some(PendingRequest {
            id,
            request,
            started: now,
        });
        self.flights.insert(id, peer);
        Ok(())
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
        if let Some((peer, id)) = completed
            && self.flights.get(&id) == Some(&peer)
        {
            self.flights.remove(&id);
        }
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
                    } => self.receive_response(context, peer, request_id, &response, now),
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
                    let _ = policy.idle_reply_sent(pending.ack, now, (context.readiness)(peer));
                }
            }
            request_response::Event::OutboundFailure {
                peer,
                request_id,
                error,
                ..
            } => {
                if let Some(state) = self.peers.get_mut(&peer)
                    && state
                        .outbound
                        .as_ref()
                        .is_some_and(|pending| pending.id == request_id)
                {
                    state.outbound = None;
                    if matches!(
                        error,
                        request_response::OutboundFailure::UnsupportedProtocols
                    ) && let Some(policy) = &mut state.policy
                    {
                        policy.use_legacy();
                        state.inbound = None;
                    }
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
                        state.inbound = None;
                        state.outbound = None;
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
        now: Instant,
    ) {
        let Some(state) = self.peers.get_mut(&peer) else {
            return;
        };
        if !state
            .outbound
            .as_ref()
            .is_some_and(|pending| pending.id == request_id)
        {
            return;
        }
        let pending = state.outbound.take().expect("matched lifecycle request");
        let Some(session) = context.session(peer, self.epoch) else {
            return;
        };
        let Ok(response) = session.validate_response(
            peer,
            |peer| context.authorized(peer),
            &pending.request,
            response,
        ) else {
            return;
        };
        let Some(policy) = &mut state.policy else {
            return;
        };
        match response {
            ValidatedResponse::IdleAccepted(ack) => {
                let _ = policy.receive_idle_ack(ack, now, (context.readiness)(peer));
            }
            ValidatedResponse::Rejected(RejectCode::Unsupported) => {
                policy.use_legacy();
                state.inbound = None;
            }
            ValidatedResponse::WakeAccepted | ValidatedResponse::Rejected(_) => {}
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
