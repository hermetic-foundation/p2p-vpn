//! Serialized checkpoint owner. Persist the selected authority before exposing it.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use libp2p::{
    PeerId, Swarm,
    request_response::{self, Message},
    swarm::ConnectionId,
};

use crate::{
    identity::NodeIdentity,
    membership::checkpoint::{
        BranchSelection, CheckpointError, CheckpointMember, CooperativeMembershipState,
        MAX_CAPABILITY_BYTES, MembershipChange, MembershipSyncState, NetworkAnchor, OfferOutcome,
        SignedHostnameClaim, SignedMembershipMutation, SignedSnapshotOffer, SnapshotChallenge,
        SnapshotPolicy, SnapshotRank,
    },
    pairing::{PairingCheckpointGrant, PairingOffer, PairingResponse},
};

use super::{
    checkpoint_handoff::{DeliveryOutcome, HandoffReport, MutationHandoff},
    control::{
        ControlCapabilities, PeerCapabilities,
        checkpoint::{
            CheckpointCapabilities, CheckpointPageRequest, CheckpointPageResponse,
            CheckpointProgress, CheckpointRejection, CheckpointSync, CheckpointSyncError,
            CheckpointSyncLimits, MAX_TRANSFER_SESSIONS,
        },
        checkpoint_mutation::{
            CheckpointMutationRequest, CheckpointMutationResponse, MAX_MUTATION_STREAMS,
            MUTATION_REQUEST_TIMEOUT, MutationOutcome, MutationRejection,
        },
    },
    forward::{ForwardError, Forwarder},
    membership_store::{
        MembershipStateStore, MembershipStateStoreError,
        checkpoint::{CheckpointCredentials, LoadedCheckpointAuthority, PersistedAuthority},
    },
    p2p::Behaviour,
    runner::RunnerError,
};

pub(crate) const RESYNC_WINDOW: Duration = Duration::from_secs(15);

#[derive(Debug)]
struct PendingResync {
    candidate: CooperativeMembershipState,
    challenge: SnapshotChallenge,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug)]
struct PendingMutationResponse {
    peer: PeerId,
    connection: ConnectionId,
    deadline: Instant,
}

pub(crate) struct CheckpointRuntime {
    network_name: String,
    credentials: CheckpointCredentials,
    state: CooperativeMembershipState,
    pending: Option<PendingResync>,
    last_selection: Option<BranchSelection>,
    transfer: CheckpointSync,
    requests: HashMap<request_response::OutboundRequestId, (PeerId, CheckpointPageRequest)>,
    responses: HashMap<request_response::InboundRequestId, (PeerId, SnapshotChallenge)>,
    attempted: HashSet<PeerId>,
    next_resync: Instant,
    offers_accepted: u64,
    transfer_failures: u64,
    candidates: HashMap<PeerId, (CheckpointCapabilities, Instant)>,
    handoff: Option<MutationHandoff>,
    mutation_requests: HashMap<request_response::OutboundRequestId, PeerId>,
    mutation_responses: HashMap<request_response::InboundRequestId, PendingMutationResponse>,
    last_handoff: HandoffReport,
    route_cleanup_pending: bool,
    enrollment_floor: Option<SnapshotRank>,
    // Reconstructed from the protected pairing transaction before any resync can install grants.
    pairing_activation_blocked: bool,
}

impl std::fmt::Debug for CheckpointRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckpointRuntime")
            .field("network", &self.network_name)
            .field("sync_state", &self.state.sync_state())
            .field("pending_requests", &self.requests.len())
            .finish_non_exhaustive()
    }
}

impl CheckpointRuntime {
    pub(crate) fn fresh_solo_eligible(
        config: &crate::config::Config,
        identity: &NodeIdentity,
        store: &MembershipStateStore,
        wall_now: u64,
    ) -> Result<bool, RunnerError> {
        validate_local_checkpoint_identity(config, identity)?;
        let secret = configured_checkpoint_secret(config)?;
        let authority = store.load_authority(
            &config.network.name,
            &identity.peer_id,
            None,
            secret.as_deref(),
        )?;
        match authority {
            Some(PersistedAuthority::Checkpoint(_)) => Ok(true),
            Some(PersistedAuthority::Legacy(legacy)) => {
                Ok(
                    validate_fresh_solo_authority(config, identity, Some(&legacy), wall_now)
                        .is_ok(),
                )
            }
            None => Ok(validate_fresh_solo_authority(config, identity, None, wall_now).is_ok()),
        }
    }

    pub(crate) fn set_pairing_activation_blocked(
        &mut self,
        blocked: bool,
        forwarder: &mut Forwarder,
        wall_now: u64,
    ) -> Result<(), RunnerError> {
        if !blocked && self.enrollment_pending() {
            return Err(core_error(CheckpointError::NoParticipation));
        }
        let projected = if blocked {
            CooperativeMembershipState::restore(
                self.credentials.capability()?,
                self.state.local_peer().to_owned(),
                self.state.retained(),
            )
            .map_err(core_error)?
        } else {
            self.state.clone()
        };
        let update = forwarder.prepare_checkpoint_update(&projected, self.anchor(), wall_now)?;
        forwarder.commit_checkpoint_update(update)?;
        if blocked && !self.pairing_activation_blocked {
            self.last_selection = None;
        }
        self.pairing_activation_blocked = blocked;
        Ok(())
    }

    pub(crate) fn pairing_activation_blocked(&self) -> bool {
        self.pairing_activation_blocked
    }

    pub(crate) fn pairing_remote_ready(&self) -> bool {
        self.last_selection
            .as_ref()
            .is_some_and(|selection| selection.observed_remote_offer)
    }

    /// Only explicit PairOpen authorization may create a scope from fresh solo authority.
    /// Saved checkpoints are restored without regenerating credentials or bypassing resync.
    pub(crate) fn form_new_network(
        config: &crate::config::Config,
        identity: &NodeIdentity,
        store: &MembershipStateStore,
        wall_now: u64,
    ) -> Result<Self, RunnerError> {
        validate_local_checkpoint_identity(config, identity)?;
        let configured_secret = configured_checkpoint_secret(config)?;
        let previous = store.load_authority(
            &config.network.name,
            &identity.peer_id,
            None,
            configured_secret.as_deref(),
        )?;
        let legacy = match previous {
            Some(PersistedAuthority::Checkpoint(loaded)) => {
                return Self::restore(config.network.name.clone(), &identity.peer_id, *loaded);
            }
            Some(PersistedAuthority::Legacy(legacy)) => Some(legacy),
            None => None,
        };
        validate_fresh_solo_authority(config, identity, legacy.as_ref(), wall_now)?;
        config.validate_runtime()?;

        let generated = CheckpointCredentials::generate()?;
        let credentials = match configured_secret {
            Some(secret) => CheckpointCredentials::new(generated.anchor().clone(), secret)?,
            None => generated,
        };
        let mut member = CheckpointMember::new(identity).map_err(core_error)?;
        member.route_grants = config.network.routes.clone();
        if let Some(vpn_ip) = &config.network.vpn_ip {
            member.route_grants.push(crate::config::RouteConfig {
                prefix: crate::config::vpn_ip_host_route(vpn_ip)?.to_string(),
                metric: 0,
            });
        }
        for route in &mut member.route_grants {
            route.prefix = route.prefix()?.to_string();
        }
        member
            .route_grants
            .sort_by(|left, right| (&left.prefix, left.metric).cmp(&(&right.prefix, right.metric)));
        member.route_grants.dedup();
        if !member.route_grants.is_empty() {
            member
                .roles
                .push(crate::membership::MembershipRole::RouteAuthority);
        }
        let incarnation = member.incarnation;
        let mut state = CooperativeMembershipState::bootstrap_at(
            credentials.capability()?,
            identity.peer_id.clone(),
            vec![member],
            SnapshotPolicy::default(),
            wall_now,
        )
        .map_err(core_error)?;
        if let Some(hostname) = config.network.dns.hostname.as_deref() {
            let claim = SignedHostnameClaim::issue(
                credentials.anchor().clone(),
                identity,
                incarnation,
                1,
                hostname,
            )
            .map_err(core_error)?;
            state.merge_hostname_claims(&[claim]).map_err(core_error)?;
        }
        // Validate local grants while participating, before making the new scope durable.
        Forwarder::from_checkpoint_config(config, &state, credentials.anchor(), wall_now)?;
        let retained = state.retained();
        let owner = Self::restore(
            config.network.name.clone(),
            &identity.peer_id,
            LoadedCheckpointAuthority {
                credentials,
                retained,
                enrollment_floor: None,
            },
        )?;
        store.save_checkpoint(
            &config.network.name,
            &identity.peer_id,
            &owner.credentials,
            &owner.state.retained(),
        )?;
        Ok(owner)
    }

    /// Caller-approved signed enrollment installs credentials, never a partial roster's grants.
    pub(crate) fn stage_pairing_enrollment(
        config: &crate::config::Config,
        identity: &NodeIdentity,
        offer: &PairingOffer,
        response: &PairingResponse,
        store: &MembershipStateStore,
        wall_now: u64,
    ) -> Result<Self, RunnerError> {
        Self::stage_pairing_enrollment_inner(
            config, identity, offer, response, store, wall_now, false,
        )
    }

    /// Explicit accepted PairJoin may replace only validated fresh solo legacy state.
    /// Existing checkpoint scope and minimum-rank pins remain authoritative.
    pub(crate) fn stage_pairing_enrollment_from_solo(
        config: &crate::config::Config,
        identity: &NodeIdentity,
        offer: &PairingOffer,
        response: &PairingResponse,
        store: &MembershipStateStore,
        wall_now: u64,
    ) -> Result<Self, RunnerError> {
        Self::stage_pairing_enrollment_inner(
            config, identity, offer, response, store, wall_now, true,
        )
    }

    fn stage_pairing_enrollment_inner(
        config: &crate::config::Config,
        identity: &NodeIdentity,
        offer: &PairingOffer,
        response: &PairingResponse,
        store: &MembershipStateStore,
        wall_now: u64,
        allow_fresh_solo: bool,
    ) -> Result<Self, RunnerError> {
        let network_name = config.network.name.clone();
        validate_local_checkpoint_identity(config, identity)?;
        response.verify_for_offer_at(offer, identity, wall_now)?;
        if response.payload.network_name != network_name {
            return Err(crate::pairing::PairingError::OfferConfigMismatch.into());
        }
        let grant = response.payload.checkpoint.as_ref().ok_or_else(|| {
            core_error(CheckpointError::Invalid(
                "checkpoint enrollment grant required",
            ))
        })?;
        let capability = grant
            .validate_for(
                &response.payload.inviter_peer,
                &response.payload.inviter_public_key,
                &identity.peer_id,
                wall_now,
            )
            .map_err(crate::pairing::PairingError::from)?;
        let credentials = CheckpointCredentials::new(
            grant.anchor.clone(),
            grant
                .secret_bytes()
                .map_err(crate::pairing::PairingError::from)?,
        )?;
        if configured_checkpoint_secret(config)?
            .as_deref()
            .is_some_and(|configured| configured != credentials.secret())
        {
            return Err(MembershipStateStoreError::CapabilityMismatch.into());
        }
        let previous = store.load_authority(
            &network_name,
            &identity.peer_id,
            Some(credentials.anchor()),
            Some(credentials.secret()),
        )?;
        let previous = match previous {
            Some(PersistedAuthority::Legacy(legacy)) => {
                if !allow_fresh_solo {
                    return Err(core_error(CheckpointError::Invalid(
                        "legacy authority requires explicit checkpoint migration",
                    )));
                }
                validate_fresh_solo_authority(config, identity, Some(&legacy), wall_now)?;
                None
            }
            Some(PersistedAuthority::Checkpoint(loaded)) => Some(loaded),
            None => {
                if allow_fresh_solo {
                    validate_fresh_solo_authority(config, identity, None, wall_now)?;
                }
                None
            }
        };
        let (seed, floor) = match previous {
            Some(loaded) => {
                let floor = loaded
                    .enrollment_floor
                    .map_or(grant.minimum, |old| old.max(grant.minimum));
                if loaded
                    .retained
                    .snapshot
                    .payload
                    .rank()
                    .map_err(core_error)?
                    >= floor
                {
                    // An old approval cannot reinstall a removed member or an old roster.
                    return Self::restore(network_name, &identity.peer_id, *loaded);
                }
                (loaded.retained, floor)
            }
            None => {
                let seed = CooperativeMembershipState::bootstrap_at(
                    capability,
                    identity.peer_id.clone(),
                    vec![grant.inviter.clone(), grant.joiner.clone()],
                    SnapshotPolicy::default(),
                    wall_now,
                )
                .map_err(core_error)?
                .retained();
                (seed, grant.minimum)
            }
        };
        store.save_pending_checkpoint_enrollment(
            &network_name,
            &identity.peer_id,
            &credentials,
            &seed,
            floor,
        )?;
        Self::restore(
            network_name,
            &identity.peer_id,
            LoadedCheckpointAuthority {
                credentials,
                retained: seed,
                enrollment_floor: Some(floor),
            },
        )
    }

    pub(crate) fn restore(
        network_name: String,
        local_peer: &str,
        loaded: LoadedCheckpointAuthority,
    ) -> Result<Self, RunnerError> {
        let credentials = loaded.credentials.clone();
        let enrollment_floor = loaded.enrollment_floor;
        let state = if enrollment_floor.is_some() {
            CooperativeMembershipState::restore(
                credentials.capability()?,
                local_peer.to_owned(),
                loaded.retained,
            )
            .map_err(core_error)?
        } else {
            loaded.restore(local_peer)?
        };
        let transfer = CheckpointSync::new(
            local_peer
                .parse()
                .map_err(crate::config::ConfigError::Libp2pPeerId)?,
            credentials.anchor().clone(),
            CheckpointSyncLimits::default(),
        )
        .map_err(wire_error)?;
        Ok(Self {
            network_name,
            credentials,
            state,
            pending: None,
            last_selection: None,
            transfer,
            requests: HashMap::new(),
            responses: HashMap::new(),
            attempted: HashSet::new(),
            next_resync: Instant::now(),
            offers_accepted: 0,
            transfer_failures: 0,
            candidates: HashMap::new(),
            handoff: None,
            mutation_requests: HashMap::new(),
            mutation_responses: HashMap::new(),
            last_handoff: HandoffReport::default(),
            route_cleanup_pending: false,
            enrollment_floor,
            pairing_activation_blocked: false,
        })
    }

    pub(crate) fn state(&self) -> &CooperativeMembershipState {
        &self.state
    }

    pub(crate) fn anchor(&self) -> &NetworkAnchor {
        self.credentials.anchor()
    }

    pub(crate) fn can_accept_pairing(&self) -> bool {
        self.state.sync_state() == MembershipSyncState::Participating
            && self.pending.is_none()
            && self.handoff.is_none()
            && !self.enrollment_pending()
            && !self.pairing_activation_blocked
    }

    pub(crate) fn enrollment_pending(&self) -> bool {
        self.enrollment_floor.is_some()
    }

    pub(crate) fn pairing_grant_for(
        &self,
        candidate: &CooperativeMembershipState,
        joiner: &str,
        wall_now: u64,
    ) -> Result<PairingCheckpointGrant, RunnerError> {
        if !self.can_accept_pairing()
            || &candidate.snapshot().payload.anchor != self.anchor()
            || candidate.local_peer() != self.state.local_peer()
        {
            return Err(core_error(CheckpointError::Invalid(
                "checkpoint pairing scope or participation",
            )));
        }
        PairingCheckpointGrant::from_state_at(
            candidate,
            self.credentials.secret(),
            joiner,
            wall_now,
        )
        .map_err(crate::pairing::PairingError::from)
        .map_err(RunnerError::from)
    }

    #[cfg(test)]
    pub(crate) fn challenge(&self) -> Option<&SnapshotChallenge> {
        self.pending.as_ref().map(|pending| &pending.challenge)
    }

    pub(crate) fn last_selection(&self) -> Option<&BranchSelection> {
        self.last_selection.as_ref()
    }

    pub(crate) fn extend_status_lines(&self, lines: &mut Vec<String>) {
        lines.push(format!(
            "checkpoint_pairing_activation_blocked {}",
            usize::from(self.pairing_activation_blocked)
        ));
        lines.push(format!(
            "checkpoint_enrollment_pending {}",
            usize::from(self.enrollment_floor.is_some())
        ));
        if let Some(floor) = self.enrollment_floor {
            lines.push(format!(
                "checkpoint_enrollment_minimum_revision {}",
                floor.authority_revision
            ));
        }
        lines.push(format!(
            "checkpoint_route_cleanup_pending {}",
            usize::from(self.route_cleanup_pending)
        ));
        let buffers = self.transfer.stats();
        let state = if self.pending.is_some() {
            "resyncing"
        } else {
            match self.state.sync_state() {
                MembershipSyncState::ResyncRequired => "resync_required",
                MembershipSyncState::Resyncing => "resyncing",
                MembershipSyncState::Participating => "participating",
                MembershipSyncState::Excluded => "excluded",
            }
        };
        lines.push(format!("checkpoint_sync_state {state}"));
        lines.push(format!(
            "checkpoint_mutation_pending_responses {}",
            self.mutation_responses.len()
        ));
        for (name, value) in [
            (
                "authority_revision",
                self.state.snapshot().payload.authority_revision,
            ),
            (
                "active_members",
                self.state.snapshot().payload.members.len() as u64,
            ),
            ("pending_requests", self.requests.len() as u64),
            ("buffered_bytes", buffers.buffered_bytes as u64),
            ("retired_transfer_slots", buffers.retired_sessions as u64),
            ("offers_accepted", self.offers_accepted),
            ("transfer_failures", self.transfer_failures),
            (
                "decisions_may_have_been_discarded",
                u64::from(
                    self.last_selection()
                        .is_some_and(|selection| selection.decisions_may_have_been_discarded),
                ),
            ),
        ] {
            lines.push(format!("checkpoint_{name} {value}"));
        }
        let handoff = self
            .handoff
            .as_ref()
            .map_or(self.last_handoff, MutationHandoff::report);
        for (name, value) in [
            ("active", usize::from(self.handoff.is_some())),
            ("pending_requests", self.mutation_requests.len()),
            ("recipients", handoff.recipients),
            ("acknowledged", handoff.acknowledged),
            ("failed", handoff.failed),
            ("pending", handoff.pending),
            ("attempts", handoff.attempts),
        ] {
            lines.push(format!("checkpoint_handoff_{name} {value}"));
        }
    }

    /// Catch-up gates a restored node. A live node keeps its already installed
    /// packet authority until the selected replacement is durable.
    pub(crate) fn begin_resync(&mut self, now: Instant) -> Result<(), RunnerError> {
        if self.pending.is_some() {
            return Ok(());
        }
        let mut candidate = self.state.clone();
        let challenge = candidate
            .begin_resync(now, RESYNC_WINDOW)
            .map_err(core_error)?;
        self.pending = Some(PendingResync {
            candidate,
            challenge,
            deadline: now + RESYNC_WINDOW,
        });
        self.attempted.clear();
        Ok(())
    }

    pub(crate) fn decorate_capabilities(
        &self,
        capabilities: &mut ControlCapabilities,
    ) -> Result<(), RunnerError> {
        let mut descriptor = CheckpointCapabilities::from_state(&self.state).map_err(wire_error)?;
        if self.pending.is_some() {
            descriptor.sync = super::control::checkpoint::CheckpointSyncIndicator::Resyncing;
        }
        capabilities.checkpoint = Some(descriptor);
        capabilities.membership_tag = Some(crate::config::membership_tag(
            &self.network_name,
            self.credentials.secret(),
        ));
        Ok(())
    }

    pub(crate) fn record_route_cleanup(&mut self, pending: bool) -> bool {
        std::mem::replace(&mut self.route_cleanup_pending, pending) != pending
    }

    /// Matching advertisements permit bounded catch-up only, never packet access.
    pub(crate) fn observe_capabilities(
        &mut self,
        peer: PeerId,
        capabilities: &ControlCapabilities,
        expected_tag: Option<&str>,
        previous_tags: &[String],
        now: Instant,
    ) -> bool {
        self.candidates.retain(|_, (_, deadline)| now < *deadline);
        if super::control::validate_capabilities(
            capabilities,
            &self.network_name,
            expected_tag,
            previous_tags,
        )
        .is_some()
        {
            return false;
        }
        let Some(descriptor) = &capabilities.checkpoint else {
            return false;
        };
        if descriptor.validate_for(self.anchor()).is_err()
            || (!self.candidates.contains_key(&peer)
                && self.candidates.len() >= MAX_TRANSFER_SESSIONS)
        {
            return false;
        }
        self.candidates
            .insert(peer, (descriptor.clone(), now + RESYNC_WINDOW));
        true
    }

    pub(crate) fn sync_candidates(&self, now: Instant) -> impl Iterator<Item = PeerId> + '_ {
        self.candidates
            .iter()
            .filter_map(move |(peer, (_, deadline))| (now < *deadline).then_some(*peer))
            .chain(
                self.mutation_responses
                    .values()
                    .filter_map(move |response| (now < response.deadline).then_some(response.peer)),
            )
            .chain(
                self.mutation_requests
                    .values()
                    .copied()
                    .filter(move |peer| {
                        self.handoff.as_ref().is_some_and(|handoff| {
                            now < handoff.deadline() && handoff.is_in_flight(*peer)
                        })
                    }),
            )
    }

    pub(crate) fn drive_sync(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        peers: &PeerCapabilities,
        now: Instant,
    ) -> Result<(), RunnerError> {
        self.transfer.cleanup(now);
        self.cleanup_mutation_responses(now);
        self.candidates.retain(|_, (_, deadline)| now < *deadline);
        let connected = swarm
            .connected_peers()
            .copied()
            .filter(|peer| {
                self.state
                    .snapshot()
                    .payload
                    .member(&peer.to_string())
                    .is_some()
                    || self.candidates.contains_key(peer)
            })
            .collect::<Vec<_>>();
        let higher_advertisement = connected.iter().any(|peer| {
            self.candidates
                .get(peer)
                .map(|(descriptor, _)| descriptor)
                .or_else(|| {
                    peers
                        .get(crate::PeerId::from_libp2p(*peer))
                        .and_then(|capabilities| capabilities.checkpoint.as_ref())
                })
                .is_some_and(|descriptor| {
                    descriptor.validate_for(self.anchor()).is_ok()
                        && self
                            .state
                            .snapshot()
                            .payload
                            .rank()
                            .is_ok_and(|rank| descriptor.rank() > rank)
                })
        });
        if self.pending.is_none()
            && (self.state.sync_state() == MembershipSyncState::ResyncRequired
                || (!connected.is_empty()
                    && (higher_advertisement
                        || now >= self.next_resync
                        || (self.pairing_activation_blocked && !self.pairing_remote_ready()))))
        {
            self.begin_resync(now)?;
        }
        let Some(pending) = &self.pending else {
            return Ok(());
        };
        if now >= pending.deadline {
            return Ok(());
        }
        let challenge = pending.challenge.clone();
        let deadline = pending.deadline;
        for peer in connected {
            if self.requests.len() >= MAX_TRANSFER_SESSIONS {
                break;
            }
            if self.attempted.contains(&peer) {
                continue;
            }
            if !self.candidates.contains_key(&peer)
                && let Some(capabilities) = peers.get(crate::PeerId::from_libp2p(peer))
                && capabilities
                    .checkpoint
                    .as_ref()
                    .is_none_or(|descriptor| descriptor.validate_for(self.anchor()).is_err())
            {
                continue;
            }
            let request = match self.transfer.start_request(
                peer,
                challenge.clone(),
                self.credentials.secret(),
                now,
                deadline,
            ) {
                Ok(request) => request,
                Err(CheckpointSyncError::ResourceLimit) => break,
                Err(_) => {
                    self.attempted.insert(peer);
                    continue;
                }
            };
            self.attempted.insert(peer);
            let request_id = swarm
                .behaviour_mut()
                .checkpoint
                .send_request(&peer, request.clone());
            self.requests.insert(request_id, (peer, request));
        }
        Ok(())
    }

    pub(crate) fn handle_wire_event(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        event: request_response::Event<CheckpointPageRequest, CheckpointPageResponse>,
        identity: &NodeIdentity,
        now: Instant,
        wall_now: u64,
    ) {
        match event {
            request_response::Event::Message {
                peer,
                message:
                    Message::Request {
                        request_id,
                        request,
                        channel,
                    },
                ..
            } => {
                if self.responses.len() >= MAX_TRANSFER_SESSIONS {
                    let _ = swarm.behaviour_mut().checkpoint.send_response(
                        channel,
                        CheckpointPageResponse::rejected(&request, CheckpointRejection::Busy),
                    );
                    return;
                }
                let response = self
                    .transfer
                    .respond_at(
                        peer,
                        &request,
                        &self.state,
                        identity,
                        self.credentials.secret(),
                        now,
                        wall_now,
                    )
                    .unwrap_or_else(|error| {
                        CheckpointPageResponse::rejected(
                            &request,
                            match error {
                                CheckpointSyncError::Unauthorized => {
                                    CheckpointRejection::Unauthorized
                                }
                                CheckpointSyncError::ResourceLimit => CheckpointRejection::Busy,
                                CheckpointSyncError::Expired => CheckpointRejection::Expired,
                                _ => CheckpointRejection::InvalidTransfer,
                            },
                        )
                    });
                let published = matches!(response, CheckpointPageResponse::Page(_));
                let sent = swarm
                    .behaviour_mut()
                    .checkpoint
                    .send_response(channel, response);
                if published {
                    if sent.is_ok() {
                        self.responses.insert(request_id, (peer, request.challenge));
                    } else {
                        self.transfer.cancel(peer, &request.challenge, now);
                    }
                }
            }
            request_response::Event::Message {
                peer,
                message:
                    Message::Response {
                        request_id,
                        response,
                    },
                ..
            } => {
                let Some((expected_peer, request)) = self.requests.remove(&request_id) else {
                    return;
                };
                if peer != expected_peer {
                    self.transfer.cancel(expected_peer, &request.challenge, now);
                    return;
                }
                match self.transfer.accept_response(
                    peer,
                    &request,
                    response,
                    self.credentials.secret(),
                    now,
                ) {
                    Ok(CheckpointProgress::More(next)) => {
                        let id = swarm
                            .behaviour_mut()
                            .checkpoint
                            .send_request(&peer, next.clone());
                        self.requests.insert(id, (peer, next));
                    }
                    Ok(CheckpointProgress::Complete(offer)) => {
                        // Decoding and request proof are not membership authority.
                        if self.collect_offer(&offer, peer, now).is_ok() {
                            self.offers_accepted = self.offers_accepted.saturating_add(1);
                        } else {
                            self.transfer_failures = self.transfer_failures.saturating_add(1);
                        }
                    }
                    Err(_) => {
                        self.transfer_failures = self.transfer_failures.saturating_add(1);
                        self.transfer.cancel(peer, &request.challenge, now);
                    }
                }
            }
            request_response::Event::OutboundFailure { request_id, .. } => {
                self.retire_outbound(request_id, now);
            }
            request_response::Event::InboundFailure { request_id, .. } => {
                if let Some((peer, challenge)) = self.responses.remove(&request_id) {
                    self.transfer_failures = self.transfer_failures.saturating_add(1);
                    self.transfer.cancel(peer, &challenge, now);
                }
            }
            request_response::Event::ResponseSent { request_id, .. } => {
                self.responses.remove(&request_id);
            }
        }
    }

    pub(crate) fn discard_wire_event(
        &mut self,
        event: request_response::Event<CheckpointPageRequest, CheckpointPageResponse>,
        now: Instant,
    ) {
        if let request_response::Event::Message {
            message: Message::Response { request_id, .. },
            ..
        } = event
        {
            self.retire_outbound(request_id, now);
        }
    }

    fn retire_outbound(&mut self, request_id: request_response::OutboundRequestId, now: Instant) {
        if let Some((peer, request)) = self.requests.remove(&request_id) {
            self.transfer_failures = self.transfer_failures.saturating_add(1);
            self.transfer.cancel(peer, &request.challenge, now);
        }
    }

    fn retire_resync(&mut self, now: Instant) {
        for (_, (peer, request)) in self.requests.drain() {
            self.transfer.cancel(peer, &request.challenge, now);
        }
        self.attempted.clear();
        self.next_resync = now + Duration::from_mins(1);
    }

    pub(crate) fn collect_offer(
        &mut self,
        offer: &SignedSnapshotOffer,
        transport_peer: PeerId,
        now: Instant,
    ) -> Result<OfferOutcome, RunnerError> {
        self.pending
            .as_mut()
            .ok_or_else(|| core_error(CheckpointError::NoSyncRound))?
            .candidate
            .collect_offer(offer, transport_peer, now)
            .map_err(core_error)
    }

    pub(crate) fn finish_due(
        &mut self,
        store: &MembershipStateStore,
        forwarder: &mut Forwarder,
        now: Instant,
        wall_now: u64,
    ) -> Result<Option<BranchSelection>, RunnerError> {
        let Some(pending) = &self.pending else {
            return Ok(None);
        };
        if now < pending.deadline {
            return Ok(None);
        }
        let mut candidate = pending.candidate.clone();
        let selection = candidate.finish_resync(now, wall_now).map_err(core_error)?;
        if let Some(floor) = self.enrollment_floor
            && (!selection.observed_remote_offer
                || candidate.snapshot().payload.rank().map_err(core_error)? < floor)
        {
            self.pending = None;
            self.retire_resync(now);
            return Ok(None);
        }
        self.persist_then_install(candidate, store, forwarder, wall_now)?;
        self.pending = None;
        self.last_selection = Some(selection.clone());
        self.retire_resync(now);
        Ok(Some(selection))
    }

    pub(crate) fn apply_change(
        &mut self,
        change: MembershipChange,
        identity: &NodeIdentity,
        store: &MembershipStateStore,
        forwarder: &mut Forwarder,
        wall_now: u64,
    ) -> Result<SignedMembershipMutation, RunnerError> {
        if self.pending.is_some() {
            return Err(core_error(CheckpointError::NoParticipation));
        }
        let mutation = self
            .state
            .sign_mutation_at(identity, change, wall_now)
            .map_err(core_error)?;
        let mut candidate = self.state.clone();
        candidate
            .apply_mutation_at(&mutation, wall_now)
            .map_err(core_error)?;
        self.persist_then_install(candidate, store, forwarder, wall_now)?;
        Ok(mutation)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_change_with_handoff(
        &mut self,
        change: MembershipChange,
        identity: &NodeIdentity,
        store: &MembershipStateStore,
        forwarder: &mut Forwarder,
        recipients: &[PeerId],
        now: Instant,
        wall_now: u64,
    ) -> Result<(), RunnerError> {
        self.finish_handoff_if_due(now);
        if self.pending.is_some() || self.handoff.is_some() {
            return Err(std::io::Error::other(
                "checkpoint synchronization or mutation handoff is still pending",
            )
            .into());
        }
        let mutation = self
            .state
            .sign_mutation_at(identity, change, wall_now)
            .map_err(core_error)?;
        CheckpointMutationRequest::new(mutation.clone())
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let authorized = recipients
            .iter()
            .copied()
            .filter(|peer| {
                peer.to_string() != identity.peer_id
                    && self
                        .state
                        .snapshot()
                        .payload
                        .member(&peer.to_string())
                        .is_some_and(|member| member.active_at(wall_now))
            })
            .collect::<Vec<_>>();
        let mut candidate = self.state.clone();
        candidate
            .apply_mutation_at(&mutation, wall_now)
            .map_err(core_error)?;
        let handoff = MutationHandoff::new(
            mutation,
            candidate
                .snapshot()
                .payload
                .boundary()
                .map_err(core_error)?,
            authorized,
            now,
        )
        .map_err(std::io::Error::other)?;
        self.persist_then_install(candidate, store, forwarder, wall_now)?;
        self.handoff = Some(handoff);
        self.finish_handoff_if_due(now);
        Ok(())
    }

    fn finish_handoff_if_due(&mut self, now: Instant) {
        let Some(handoff) = &mut self.handoff else {
            return;
        };
        handoff.advance_clock(now);
        self.mutation_requests
            .retain(|_, peer| handoff.is_in_flight(*peer));
        let report = handoff.report();
        if report.pending == 0 {
            self.last_handoff = report;
            self.handoff = None;
            self.mutation_requests.clear();
        }
    }

    pub(crate) fn drive_mutations(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        now: Instant,
    ) -> Result<(), RunnerError> {
        self.cleanup_mutation_responses(now);
        self.finish_handoff_if_due(now);
        let Some(handoff) = &mut self.handoff else {
            return Ok(());
        };
        while let Some(peer) = handoff.next_ready(now) {
            if !swarm.is_connected(&peer) {
                handoff.resolve(peer, DeliveryOutcome::RetryableFailure, now);
                continue;
            }
            let request = CheckpointMutationRequest::new(handoff.mutation().clone())
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .with_deadline(handoff.deadline());
            let id = swarm
                .behaviour_mut()
                .checkpoint_mutation
                .send_request(&peer, request);
            self.mutation_requests.insert(id, peer);
        }
        Ok(())
    }

    fn incoming_mutation_outcome(
        &mut self,
        request: &CheckpointMutationRequest,
        peer: PeerId,
        store: &MembershipStateStore,
        forwarder: &mut Forwarder,
        wall_now: u64,
    ) -> MutationOutcome {
        let reject = |reason, current| MutationOutcome::Rejected { reason, current };
        let Ok(mutation) = request.validate_for(peer, self.anchor()) else {
            return reject(MutationRejection::Unauthorized, None);
        };
        if self.state.sync_state() != MembershipSyncState::Participating {
            return reject(MutationRejection::ResyncRequired, None);
        }
        let mut candidate = self.state.clone();
        if let Err(error) = candidate.apply_mutation_at(mutation, wall_now) {
            return match error {
                CheckpointError::StaleMutation => reject(
                    MutationRejection::StaleBase,
                    self.state.snapshot().payload.boundary().ok(),
                ),
                CheckpointError::NoParticipation => reject(MutationRejection::ResyncRequired, None),
                _ => reject(MutationRejection::Invalid, None),
            };
        }
        let rebased = if let Some(pending) = &self.pending {
            let mut refresh = pending.candidate.clone();
            if refresh.rebase_live_resync_on_installed(&candidate).is_err() {
                return reject(MutationRejection::Invalid, None);
            }
            Some(refresh)
        } else {
            None
        };
        if self
            .persist_then_install(candidate, store, forwarder, wall_now)
            .is_err()
        {
            return reject(MutationRejection::PersistenceFailed, None);
        }
        if let Some(refresh) = rebased
            && let Some(pending) = &mut self.pending
        {
            pending.candidate = refresh;
        }
        match self.state.snapshot().payload.boundary() {
            Ok(boundary) => MutationOutcome::Applied(boundary),
            Err(_) => reject(MutationRejection::Invalid, None),
        }
    }

    pub(crate) fn handle_mutation_event(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        event: request_response::Event<CheckpointMutationRequest, CheckpointMutationResponse>,
        store: &MembershipStateStore,
        forwarder: &mut Forwarder,
        now: Instant,
        wall_now: u64,
    ) -> bool {
        self.cleanup_mutation_responses(now);
        self.finish_handoff_if_due(now);
        match event {
            request_response::Event::Message {
                peer,
                connection_id,
                message:
                    Message::Request {
                        request_id,
                        request,
                        channel,
                    },
            } => {
                let revision = forwarder.membership_revision();
                // Do not durably apply a command when its ACK has no bounded owner.
                let outcome = if self.mutation_responses.len() >= MAX_MUTATION_STREAMS {
                    MutationOutcome::Rejected {
                        reason: MutationRejection::Busy,
                        current: None,
                    }
                } else {
                    self.incoming_mutation_outcome(&request, peer, store, forwarder, wall_now)
                };
                if let Ok(response) = CheckpointMutationResponse::for_request(&request, outcome) {
                    let owned = self.track_mutation_response(
                        request_id,
                        peer,
                        connection_id,
                        &request,
                        outcome,
                        now,
                    );
                    if swarm
                        .behaviour_mut()
                        .checkpoint_mutation
                        .send_response(channel, response)
                        .is_err()
                        && owned
                    {
                        self.retire_mutation_response(request_id, peer, connection_id);
                    }
                }
                return forwarder.membership_revision() != revision;
            }
            request_response::Event::Message {
                peer,
                message:
                    Message::Response {
                        request_id,
                        response,
                    },
                ..
            } => {
                if let Some(expected_peer) = self.mutation_requests.remove(&request_id)
                    && let Some(handoff) = &mut self.handoff
                {
                    let outcome = CheckpointMutationRequest::new(handoff.mutation().clone())
                        .and_then(|request| response.validate_for(&request));
                    let delivery = if peer != expected_peer {
                        DeliveryOutcome::DefiniteFailure
                    } else {
                        match outcome {
                            Ok(MutationOutcome::Applied(boundary))
                                if handoff.acknowledges(boundary) =>
                            {
                                DeliveryOutcome::Acknowledged
                            }
                            Ok(MutationOutcome::Rejected {
                                reason: MutationRejection::StaleBase,
                                current: Some(boundary),
                            }) if handoff.acknowledges(boundary) => DeliveryOutcome::Acknowledged,
                            Ok(MutationOutcome::Rejected {
                                reason:
                                    MutationRejection::Busy
                                    | MutationRejection::ResyncRequired
                                    | MutationRejection::PersistenceFailed,
                                ..
                            }) => DeliveryOutcome::RetryableFailure,
                            _ => DeliveryOutcome::DefiniteFailure,
                        }
                    };
                    handoff.resolve(expected_peer, delivery, now);
                }
            }
            request_response::Event::OutboundFailure {
                request_id, error, ..
            } => {
                if matches!(
                    error,
                    request_response::OutboundFailure::UnsupportedProtocols
                ) {
                    if let Some(peer) = self.mutation_requests.remove(&request_id)
                        && let Some(handoff) = &mut self.handoff
                    {
                        handoff.resolve(peer, DeliveryOutcome::DefiniteFailure, now);
                    }
                } else {
                    self.fail_mutation_request(request_id, now);
                }
            }
            request_response::Event::InboundFailure {
                peer,
                connection_id,
                request_id,
                ..
            }
            | request_response::Event::ResponseSent {
                peer,
                connection_id,
                request_id,
            } => self.retire_mutation_response(request_id, peer, connection_id),
        }
        self.finish_handoff_if_due(now);
        false
    }

    pub(crate) fn discard_mutation_event(
        &mut self,
        event: request_response::Event<CheckpointMutationRequest, CheckpointMutationResponse>,
        now: Instant,
    ) {
        self.cleanup_mutation_responses(now);
        match event {
            request_response::Event::Message {
                message: Message::Response { request_id, .. },
                ..
            } => self.fail_mutation_request(request_id, now),
            request_response::Event::InboundFailure {
                peer,
                connection_id,
                request_id,
                ..
            }
            | request_response::Event::ResponseSent {
                peer,
                connection_id,
                request_id,
            } => self.retire_mutation_response(request_id, peer, connection_id),
            _ => (),
        }
        self.finish_handoff_if_due(now);
    }

    fn cleanup_mutation_responses(&mut self, now: Instant) {
        self.mutation_responses
            .retain(|_, response| now < response.deadline);
    }

    pub(crate) fn owns_mutation_response(
        &self,
        peer: PeerId,
        connection: ConnectionId,
        now: Instant,
    ) -> bool {
        self.mutation_responses.values().any(|response| {
            response.peer == peer && response.connection == connection && now < response.deadline
        })
    }

    /// This exception keeps only an authenticated command's bounded ACK transport.
    /// It never changes the roster, packet grants, or snapshot publication rights.
    fn track_mutation_response(
        &mut self,
        request_id: request_response::InboundRequestId,
        peer: PeerId,
        connection: ConnectionId,
        request: &CheckpointMutationRequest,
        outcome: MutationOutcome,
        now: Instant,
    ) -> bool {
        self.cleanup_mutation_responses(now);
        if self.mutation_responses.len() >= MAX_MUTATION_STREAMS
            || request.validate_for(peer, self.anchor()).is_err()
            || !matches!(
                outcome,
                MutationOutcome::Applied(_)
                    | MutationOutcome::Rejected {
                        reason: MutationRejection::StaleBase,
                        current: Some(_),
                    }
            )
        {
            return false;
        }
        if let Some(response) = self.mutation_responses.get(&request_id) {
            return response.peer == peer
                && response.connection == connection
                && self.owns_mutation_response(peer, connection, now);
        }
        self.mutation_responses.insert(
            request_id,
            PendingMutationResponse {
                peer,
                connection,
                deadline: now + MUTATION_REQUEST_TIMEOUT,
            },
        );
        true
    }

    fn retire_mutation_response(
        &mut self,
        request_id: request_response::InboundRequestId,
        peer: PeerId,
        connection: ConnectionId,
    ) {
        if self
            .mutation_responses
            .get(&request_id)
            .is_some_and(|response| response.peer == peer && response.connection == connection)
        {
            self.mutation_responses.remove(&request_id);
        }
    }

    fn fail_mutation_request(
        &mut self,
        request_id: request_response::OutboundRequestId,
        now: Instant,
    ) {
        if let Some(peer) = self.mutation_requests.remove(&request_id)
            && let Some(handoff) = &mut self.handoff
        {
            handoff.resolve(peer, DeliveryOutcome::RetryableFailure, now);
        }
    }

    pub(crate) fn reconcile_local_hostname(
        &mut self,
        identity: &NodeIdentity,
        store: &MembershipStateStore,
        forwarder: &mut Forwarder,
        wall_now: u64,
    ) -> Result<bool, RunnerError> {
        if self.pending.is_some() || self.state.sync_state() != MembershipSyncState::Participating {
            return Ok(false);
        }
        let Some(hostname) = forwarder.config().network.dns.hostname.as_deref() else {
            return Ok(false);
        };
        let hostname = crate::dns::canonical_dns_label(hostname)
            .map_err(crate::membership::MembershipRecordError::InvalidHostname)
            .map_err(ForwardError::MembershipRecord)?;
        let previous = self
            .state
            .hostname_claims()
            .iter()
            .find(|claim| claim.payload.subject.peer_id == identity.peer_id);
        if previous.is_some_and(|claim| claim.payload.hostname == hostname) {
            return Ok(false);
        }
        let sequence = previous
            .map_or(Some(1), |claim| claim.payload.sequence.checked_add(1))
            .ok_or_else(|| core_error(CheckpointError::Invalid("hostname sequence exhausted")))?;
        let incarnation = self
            .state
            .snapshot()
            .payload
            .member(&identity.peer_id)
            .ok_or_else(|| core_error(CheckpointError::NoParticipation))?
            .incarnation;
        let claim = SignedHostnameClaim::issue(
            self.anchor().clone(),
            identity,
            incarnation,
            sequence,
            &hostname,
        )
        .map_err(core_error)?;
        let mut candidate = self.state.clone();
        candidate
            .merge_hostname_claims(&[claim])
            .map_err(core_error)?;
        self.persist_then_install(candidate, store, forwarder, wall_now)?;
        Ok(true)
    }

    fn persist_then_install(
        &mut self,
        candidate: CooperativeMembershipState,
        store: &MembershipStateStore,
        forwarder: &mut Forwarder,
        wall_now: u64,
    ) -> Result<(), RunnerError> {
        let projection = if self.pairing_activation_blocked {
            CooperativeMembershipState::restore(
                self.credentials.capability()?,
                candidate.local_peer().to_owned(),
                candidate.retained(),
            )
            .map_err(core_error)?
        } else {
            candidate.clone()
        };
        let update = forwarder.prepare_checkpoint_update(&projection, self.anchor(), wall_now)?;
        let result = store.save_checkpoint(
            &self.network_name,
            candidate.local_peer(),
            &self.credentials,
            &candidate.retained(),
        );
        if let Err(error) = result {
            if matches!(
                error,
                MembershipStateStoreError::CheckpointDurabilityUncertain(_)
            ) {
                self.install_gated(candidate, forwarder, wall_now)?;
            }
            return Err(error.into());
        }
        if let Err(error) = forwarder.commit_checkpoint_update(update) {
            // The new disk state is already visible. Never restore older grants.
            self.install_gated(candidate, forwarder, wall_now)?;
            return Err(error.into());
        }
        self.state = candidate;
        self.enrollment_floor = None;
        if self.handoff.as_ref().is_some_and(|handoff| {
            self.state
                .snapshot()
                .payload
                .boundary()
                .map_or(true, |boundary| !handoff.acknowledges(boundary))
        }) {
            self.cancel_handoff();
        }
        Ok(())
    }

    fn install_gated(
        &mut self,
        candidate: CooperativeMembershipState,
        forwarder: &mut Forwarder,
        wall_now: u64,
    ) -> Result<(), RunnerError> {
        if self.enrollment_floor.is_some_and(|floor| {
            candidate
                .snapshot()
                .payload
                .rank()
                .is_ok_and(|rank| rank >= floor)
        }) {
            // A qualifying replacement is already visible on disk; retry it, not the seed.
            self.enrollment_floor = None;
        }
        self.state = CooperativeMembershipState::restore(
            self.credentials.capability()?,
            candidate.local_peer().to_owned(),
            candidate.retained(),
        )
        .map_err(core_error)?;
        self.pending = None;
        self.retire_resync(Instant::now());
        self.cancel_handoff();
        let update = forwarder.prepare_checkpoint_update(&self.state, self.anchor(), wall_now)?;
        forwarder.commit_checkpoint_update(update)?;
        Ok(())
    }

    fn cancel_handoff(&mut self) {
        if let Some(handoff) = self.handoff.take() {
            self.last_handoff = handoff.report();
            self.last_handoff.failed += self.last_handoff.pending;
            self.last_handoff.pending = 0;
        }
        self.mutation_requests.clear();
    }

    pub(crate) fn prune_expired(
        &mut self,
        identity: &NodeIdentity,
        store: &MembershipStateStore,
        forwarder: &mut Forwarder,
        wall_now: u64,
    ) -> Result<bool, RunnerError> {
        if self.pending.is_some()
            || self.handoff.is_some()
            || self.state.sync_state() != MembershipSyncState::Participating
            || self
                .state
                .snapshot()
                .payload
                .members
                .iter()
                .all(|member| member.active_at(wall_now))
        {
            return Ok(false);
        }
        // An expired local member cannot authorize cleanup; active survivors do it.
        if self
            .state
            .snapshot()
            .payload
            .member(&identity.peer_id)
            .is_none_or(|member| !member.active_at(wall_now))
        {
            return Ok(false);
        }
        self.apply_change(
            MembershipChange::PruneExpired,
            identity,
            store,
            forwarder,
            wall_now,
        )?;
        Ok(true)
    }
}

fn core_error(error: CheckpointError) -> RunnerError {
    ForwardError::Checkpoint(error).into()
}

fn validate_local_checkpoint_identity(
    config: &crate::config::Config,
    identity: &NodeIdentity,
) -> Result<(), RunnerError> {
    if config.network.name.is_empty()
        || config.identity()?.peer_id != identity.peer_id
        || identity
            .public_key()
            .map_err(crate::pairing::PairingError::from)?
            .to_peer_id()
            .to_string()
            != identity.peer_id
    {
        return Err(crate::pairing::PairingError::OfferConfigMismatch.into());
    }
    Ok(())
}

fn configured_checkpoint_secret(
    config: &crate::config::Config,
) -> Result<Option<Vec<u8>>, RunnerError> {
    if config
        .network
        .membership_key
        .as_ref()
        .is_some_and(|secret| secret.len() > MAX_CAPABILITY_BYTES.div_ceil(3) * 4)
    {
        return Err(core_error(CheckpointError::Invalid(
            "checkpoint capability exceeds limit",
        )));
    }
    let secret = config.membership_key_bytes()?;
    if secret
        .as_ref()
        .is_some_and(|secret| secret.len() > MAX_CAPABILITY_BYTES)
    {
        return Err(core_error(CheckpointError::Invalid(
            "checkpoint capability exceeds limit",
        )));
    }
    Ok(secret)
}

fn validate_fresh_solo_authority(
    config: &crate::config::Config,
    identity: &NodeIdentity,
    legacy: Option<&super::membership_store::PersistedMembershipStateData>,
    wall_now: u64,
) -> Result<(), RunnerError> {
    if !config.peers.is_empty() {
        return Err(core_error(CheckpointError::Invalid(
            "configured peers require explicit migration",
        )));
    }
    crate::membership::validate_membership_record_history(
        &config.network.member_records,
        &config.network.name,
    )
    .map_err(MembershipStateStoreError::from)?;
    let mut records = config.network.member_records.clone();
    if let Some(legacy) = legacy {
        for record in &legacy.records {
            if !records.contains(record) {
                records.push(record.clone());
            }
        }
        crate::hostname::validate_hostname_record_history(
            &legacy.hostname_records,
            &config.network.name,
        )
        .map_err(MembershipStateStoreError::from)?;
        if legacy.hostname_records.iter().any(|record| {
            record.payload.peer != identity.peer_id
                || record.payload.issued_at_unix_seconds > wall_now
        }) {
            return Err(core_error(CheckpointError::Invalid(
                "foreign or future solo hostname history",
            )));
        }
    }
    crate::membership::validate_membership_record_history(&records, &config.network.name)
        .map_err(MembershipStateStoreError::from)?;
    for record in &records {
        record
            .verify_at(wall_now)
            .map_err(MembershipStateStoreError::from)?;
        if record.payload.issuer_peer != identity.peer_id
            || record.payload.member_peer != identity.peer_id
            || record.payload.revoked
            || record.payload.issued_at_unix_seconds > wall_now
            || record.is_expired_at(wall_now)
        {
            return Err(core_error(CheckpointError::Invalid(
                "non-fresh solo membership history",
            )));
        }
    }
    Ok(())
}

fn wire_error(error: CheckpointSyncError) -> RunnerError {
    std::io::Error::other(error.to_string()).into()
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _, path::PathBuf};

    use crate::{
        config::Config,
        membership::checkpoint::{CheckpointMember, SnapshotPolicy},
    };

    use super::*;

    const WALL_NOW: u64 = 10_000;

    fn unused_ipv4_identity(allocated: &mut HashSet<std::net::Ipv4Addr>) -> NodeIdentity {
        (0..1_024)
            .find_map(|_| {
                let identity = NodeIdentity::generate_ed25519().unwrap();
                let peer = crate::PeerId::from_libp2p(identity.peer_id.parse().unwrap());
                allocated
                    .insert(crate::route::builtin_ipv4(peer))
                    .then_some(identity)
            })
            .expect("fixture must allocate distinct derived IPv4 addresses")
    }

    struct Fixture {
        directory: PathBuf,
        store: MembershipStateStore,
        credentials: CheckpointCredentials,
        local: NodeIdentity,
        member: NodeIdentity,
        state: CooperativeMembershipState,
        config: Config,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "p2p-vpn-checkpoint-owner-{}-{name}",
                std::process::id(),
            ));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let store = MembershipStateStore::new(directory.join("membership-state.json"));
            let credentials =
                CheckpointCredentials::new(NetworkAnchor::new([45; 32]).unwrap(), vec![89; 32])
                    .unwrap();
            let local = NodeIdentity::generate_ed25519().unwrap();
            let mut allocated = HashSet::from([crate::route::builtin_ipv4(
                crate::PeerId::from_libp2p(local.peer_id.parse().unwrap()),
            )]);
            let member = unused_ipv4_identity(&mut allocated);
            let config: Config = serde_json::from_value(serde_json::json!({
                "network": {"name": "lab", "local_peer": local.peer_id, "private_key": local.private_key},
                "peers": [{"id": member.peer_id, "name": "stale-member"}],
            })).unwrap();
            let state = CooperativeMembershipState::bootstrap_at(
                credentials.capability().unwrap(),
                local.peer_id.clone(),
                vec![
                    CheckpointMember::new(&local).unwrap(),
                    CheckpointMember::new(&member).unwrap(),
                ],
                SnapshotPolicy::default(),
                WALL_NOW,
            )
            .unwrap();
            store
                .save_checkpoint("lab", &local.peer_id, &credentials, &state.retained())
                .unwrap();
            Self {
                directory,
                store,
                credentials,
                local,
                member,
                state,
                config,
            }
        }

        fn restored(&self) -> (CheckpointRuntime, Forwarder) {
            let Some(super::super::membership_store::checkpoint::PersistedAuthority::Checkpoint(
                loaded,
            )) = self
                .store
                .load_authority(
                    "lab",
                    &self.local.peer_id,
                    Some(self.credentials.anchor()),
                    None,
                )
                .unwrap()
            else {
                panic!("checkpoint authority")
            };
            let runtime =
                CheckpointRuntime::restore("lab".into(), &self.local.peer_id, *loaded).unwrap();
            let forwarder = Forwarder::from_checkpoint_config(
                &self.config,
                runtime.state(),
                runtime.anchor(),
                WALL_NOW,
            )
            .unwrap();
            (runtime, forwarder)
        }

        fn path(&self) -> PathBuf {
            self.directory.join("membership-state.json")
        }

        fn participating(&self) -> (CheckpointRuntime, Forwarder) {
            let (mut runtime, mut forwarder) = self.restored();
            let now = Instant::now();
            runtime.begin_resync(now).unwrap();
            runtime
                .finish_due(&self.store, &mut forwarder, now + RESYNC_WINDOW, WALL_NOW)
                .unwrap();
            (runtime, forwarder)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    fn solo_config(fixture: &Fixture) -> Config {
        let mut config = fixture.config.clone();
        config.peers.clear();
        config
    }

    fn solo_legacy_record(
        issuer: &NodeIdentity,
        member: &NodeIdentity,
        revoked: bool,
        issued: u64,
        expires: Option<u64>,
    ) -> crate::membership::SignedMembershipRecord {
        use crate::membership::{
            MembershipRecordIssueOptions, MembershipRecordSubject, MembershipRole,
        };
        crate::membership::issue_membership_record_for_subject_at(
            issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".into(),
                member: MembershipRecordSubject::from_identity(member).unwrap(),
                membership_epoch: 1,
                sequence: 1,
                revoked,
                roles: if revoked {
                    vec![]
                } else {
                    vec![MembershipRole::OverlayMember]
                },
                route_grants: vec![],
                expires_at_unix_seconds: expires,
            },
            issued,
        )
        .unwrap()
    }

    fn write_solo_legacy(
        fixture: &Fixture,
        version: u8,
        records: &[crate::membership::SignedMembershipRecord],
        names: &[crate::hostname::SignedHostnameRecord],
    ) {
        let mut envelope = serde_json::json!({
            "version": version,
            "network_name": "lab",
            "local_peer": fixture.local.peer_id,
            "records": records,
        });
        if version != 1 {
            envelope["hostname_records"] = serde_json::to_value(names).unwrap();
        }
        fs::write(fixture.path(), serde_json::to_vec(&envelope).unwrap()).unwrap();
    }

    #[test]
    fn fresh_formation_accepts_empty_or_self_only_startup_history_and_returns_gated() {
        for version in [1, 2] {
            for self_only in [false, true] {
                let fixture = Fixture::new(&format!("fresh-formation-{version}-{self_only}"));
                let mut config = solo_config(&fixture);
                config.network.dns.hostname = Some("new-local-name".into());
                let records = if self_only {
                    vec![solo_legacy_record(
                        &fixture.local,
                        &fixture.local,
                        false,
                        WALL_NOW - 1,
                        None,
                    )]
                } else {
                    vec![]
                };
                config.network.member_records = records.clone();
                let names = vec![
                    crate::hostname::issue_hostname_record_at(
                        &fixture.local,
                        "lab",
                        "old-local-name",
                        1,
                        WALL_NOW - 1,
                    )
                    .unwrap(),
                ];
                write_solo_legacy(&fixture, version, &records, &names);
                let mut runtime = CheckpointRuntime::form_new_network(
                    &config,
                    &fixture.local,
                    &fixture.store,
                    WALL_NOW,
                )
                .unwrap();
                assert_eq!(
                    runtime.state().sync_state(),
                    MembershipSyncState::ResyncRequired
                );
                assert!(!runtime.can_accept_pairing());
                assert!(!runtime.enrollment_pending());
                assert_eq!(runtime.credentials.secret().len(), 32);
                assert_ne!(runtime.credentials.secret(), &[0; 32]);
                assert_eq!(runtime.state().snapshot().payload.members.len(), 1);
                assert_eq!(runtime.state().snapshot().payload.authority_revision, 0);
                assert_eq!(
                    runtime.state().snapshot().payload.policy,
                    SnapshotPolicy::default()
                );
                assert!(
                    runtime
                        .state()
                        .snapshot()
                        .payload
                        .member(&fixture.local.peer_id)
                        .is_some()
                );
                assert_eq!(
                    runtime.state().hostname_claims()[0].payload.hostname,
                    "new-local-name"
                );
                let saved: serde_json::Value =
                    serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
                assert_eq!(saved["version"], 3);
                assert!(saved.get("records").is_none());
                assert!(saved.get("hostname_records").is_none());
                assert!(
                    !serde_json::to_string(&saved)
                        .unwrap()
                        .contains("old-local-name")
                );
                assert_eq!(
                    fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
                    0o600
                );

                let mut forwarder = Forwarder::from_checkpoint_config(
                    &config,
                    runtime.state(),
                    runtime.anchor(),
                    WALL_NOW,
                )
                .unwrap();
                assert!(
                    forwarder
                        .effective_membership()
                        .overlay_members()
                        .next()
                        .is_none()
                );
                let now = Instant::now();
                runtime.begin_resync(now).unwrap();
                runtime
                    .finish_due(
                        &fixture.store,
                        &mut forwarder,
                        now + RESYNC_WINDOW,
                        WALL_NOW,
                    )
                    .unwrap();
                assert!(runtime.can_accept_pairing());
            }
        }
    }

    #[test]
    fn fresh_formation_preserves_secret_pins_and_canonical_local_route_grants() {
        use crate::{config::RouteConfig, membership::MembershipRole};
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        for size in [32, MAX_CAPABILITY_BYTES] {
            let fixture = Fixture::new(&format!("fresh-formation-grants-{size}"));
            let mut config = solo_config(&fixture);
            let secret = vec![37; size];
            config.network.membership_key = Some(STANDARD.encode(&secret));
            config.network.vpn_ip = Some("10.77.0.9".into());
            config.network.routes = vec![
                RouteConfig {
                    prefix: "172.22.3.99/24".into(),
                    metric: 11,
                },
                RouteConfig {
                    prefix: "172.22.3.0/24".into(),
                    metric: 11,
                },
                RouteConfig {
                    prefix: "fd77::4/128".into(),
                    metric: 0,
                },
            ];
            write_solo_legacy(&fixture, 2, &[], &[]);
            let mut runtime = CheckpointRuntime::form_new_network(
                &config,
                &fixture.local,
                &fixture.store,
                WALL_NOW,
            )
            .unwrap();
            assert_eq!(runtime.credentials.secret(), secret);
            let grants = vec![
                RouteConfig {
                    prefix: "10.77.0.9/32".into(),
                    metric: 0,
                },
                RouteConfig {
                    prefix: "172.22.3.0/24".into(),
                    metric: 11,
                },
                RouteConfig {
                    prefix: "fd77::4/128".into(),
                    metric: 0,
                },
            ];
            let member = runtime
                .state()
                .snapshot()
                .payload
                .member(&fixture.local.peer_id)
                .unwrap();
            assert_eq!(
                member.roles,
                [
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority
                ]
            );
            assert_eq!(member.route_grants, grants);
            let mut forwarder = Forwarder::from_checkpoint_config(
                &config,
                runtime.state(),
                runtime.anchor(),
                WALL_NOW,
            )
            .unwrap();
            let now = Instant::now();
            runtime.begin_resync(now).unwrap();
            runtime
                .finish_due(
                    &fixture.store,
                    &mut forwarder,
                    now + RESYNC_WINDOW,
                    WALL_NOW,
                )
                .unwrap();
            assert_eq!(forwarder.config().network.routes, grants);
            let Some(PersistedAuthority::Checkpoint(saved)) = fixture
                .store
                .load_authority(
                    "lab",
                    &fixture.local.peer_id,
                    Some(runtime.anchor()),
                    Some(&secret),
                )
                .unwrap()
            else {
                panic!("formed checkpoint authority")
            };
            assert_eq!(
                saved
                    .retained
                    .snapshot
                    .payload
                    .member(&fixture.local.peer_id)
                    .unwrap()
                    .route_grants,
                grants
            );
        }
    }

    #[test]
    fn solo_conversion_rejects_configured_foreign_revoked_future_or_invalid_history() {
        for case in [
            "configured-peer",
            "configured-foreign",
            "foreign-member",
            "foreign-issuer",
            "revoked",
            "future",
            "expired",
            "foreign-hostname",
            "future-hostname",
            "invalid-config-signature",
            "invalid-store-signature",
            "conflicting-self-history",
        ] {
            let fixture = Fixture::new(&format!("solo-reject-{case}"));
            let (offer, response, _) = checkpoint_pairing(&fixture);
            let mut config = solo_config(&fixture);
            let self_record =
                solo_legacy_record(&fixture.local, &fixture.local, false, WALL_NOW - 10, None);
            let mut records = vec![self_record.clone()];
            let mut names = vec![];
            match case {
                "configured-peer" => config.peers = fixture.config.peers.clone(),
                "configured-foreign" => {
                    config.network.member_records = vec![solo_legacy_record(
                        &fixture.local,
                        &fixture.member,
                        false,
                        WALL_NOW - 10,
                        None,
                    )]
                }
                "foreign-member" => {
                    records = vec![solo_legacy_record(
                        &fixture.local,
                        &fixture.member,
                        false,
                        WALL_NOW - 10,
                        None,
                    )]
                }
                "foreign-issuer" => {
                    records = vec![solo_legacy_record(
                        &fixture.member,
                        &fixture.local,
                        false,
                        WALL_NOW - 10,
                        None,
                    )]
                }
                "revoked" => {
                    records = vec![solo_legacy_record(
                        &fixture.local,
                        &fixture.local,
                        true,
                        WALL_NOW - 10,
                        None,
                    )]
                }
                "future" => {
                    records = vec![solo_legacy_record(
                        &fixture.local,
                        &fixture.local,
                        false,
                        WALL_NOW + 1,
                        None,
                    )]
                }
                "expired" => {
                    records = vec![solo_legacy_record(
                        &fixture.local,
                        &fixture.local,
                        false,
                        WALL_NOW - 10,
                        Some(WALL_NOW - 1),
                    )]
                }
                "foreign-hostname" => names.push(
                    crate::hostname::issue_hostname_record_at(
                        &fixture.member,
                        "lab",
                        "foreign",
                        1,
                        WALL_NOW - 1,
                    )
                    .unwrap(),
                ),
                "future-hostname" => names.push(
                    crate::hostname::issue_hostname_record_at(
                        &fixture.local,
                        "lab",
                        "future",
                        1,
                        WALL_NOW + 1,
                    )
                    .unwrap(),
                ),
                "invalid-config-signature" => {
                    let mut tampered = self_record.clone();
                    tampered.payload.sequence += 1;
                    config.network.member_records.push(tampered);
                }
                "invalid-store-signature" => records[0].payload.sequence += 1,
                "conflicting-self-history" => {
                    config.network.member_records = vec![solo_legacy_record(
                        &fixture.local,
                        &fixture.local,
                        false,
                        WALL_NOW - 9,
                        None,
                    )]
                }
                _ => unreachable!(),
            }
            write_solo_legacy(&fixture, 2, &records, &names);
            let before = fs::read(fixture.path()).unwrap();
            assert!(
                CheckpointRuntime::form_new_network(
                    &config,
                    &fixture.local,
                    &fixture.store,
                    WALL_NOW
                )
                .is_err(),
                "{case}"
            );
            assert_eq!(fs::read(fixture.path()).unwrap(), before, "{case}");
            assert!(
                CheckpointRuntime::stage_pairing_enrollment_from_solo(
                    &config,
                    &fixture.local,
                    &offer,
                    &response,
                    &fixture.store,
                    WALL_NOW
                )
                .is_err(),
                "{case}"
            );
            assert_eq!(fs::read(fixture.path()).unwrap(), before, "{case}");
        }
    }

    #[test]
    fn solo_conversion_rejects_wrong_actual_key_scope_or_unknown_store_version() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        for case in [
            "claimed-key",
            "configured-key",
            "network",
            "local-peer",
            "version",
            "oversized-secret",
            "decoded-secret-limit",
        ] {
            let fixture = Fixture::new(&format!("solo-identity-{case}"));
            let (offer, response, _) = checkpoint_pairing(&fixture);
            let mut config = solo_config(&fixture);
            let mut identity = fixture.local.clone();
            write_solo_legacy(&fixture, 2, &[], &[]);
            match case {
                "claimed-key" => identity.private_key = fixture.member.private_key.clone(),
                "configured-key" => {
                    config.network.private_key = Some(fixture.member.private_key.clone())
                }
                "network" => config.network.name = "different".into(),
                "local-peer" => config.network.local_peer = fixture.member.peer_id.clone(),
                "version" => write_solo_legacy(&fixture, 99, &[], &[]),
                "oversized-secret" => {
                    config.network.membership_key =
                        Some(STANDARD.encode(vec![0; MAX_CAPABILITY_BYTES + 4]))
                }
                "decoded-secret-limit" => {
                    config.network.membership_key =
                        Some(STANDARD.encode(vec![0; MAX_CAPABILITY_BYTES + 1]))
                }
                _ => unreachable!(),
            }
            let before = fs::read(fixture.path()).unwrap();
            assert!(
                CheckpointRuntime::form_new_network(&config, &identity, &fixture.store, WALL_NOW)
                    .is_err(),
                "{case}"
            );
            assert!(
                CheckpointRuntime::stage_pairing_enrollment_from_solo(
                    &config,
                    &identity,
                    &offer,
                    &response,
                    &fixture.store,
                    WALL_NOW
                )
                .is_err(),
                "{case}"
            );
            assert_eq!(fs::read(fixture.path()).unwrap(), before, "{case}");
        }
    }

    #[test]
    fn accepted_solo_enrollment_keeps_strict_legacy_rejection_and_remote_floor_gate() {
        for version in [1, 2] {
            for self_only in [false, true] {
                let fixture = Fixture::new(&format!("solo-enrollment-{version}-{self_only}"));
                let (offer, response, _) = checkpoint_pairing(&fixture);
                let mut config = solo_config(&fixture);
                let records = if self_only {
                    vec![solo_legacy_record(
                        &fixture.local,
                        &fixture.local,
                        false,
                        WALL_NOW - 1,
                        None,
                    )]
                } else {
                    vec![]
                };
                config.network.member_records = records.clone();
                write_solo_legacy(&fixture, version, &records, &[]);
                let before = fs::read(fixture.path()).unwrap();
                assert!(
                    CheckpointRuntime::stage_pairing_enrollment(
                        &config,
                        &fixture.local,
                        &offer,
                        &response,
                        &fixture.store,
                        WALL_NOW
                    )
                    .is_err()
                );
                assert_eq!(fs::read(fixture.path()).unwrap(), before);
                let mut runtime = CheckpointRuntime::stage_pairing_enrollment_from_solo(
                    &config,
                    &fixture.local,
                    &offer,
                    &response,
                    &fixture.store,
                    WALL_NOW,
                )
                .unwrap();
                assert_eq!(
                    runtime.enrollment_floor,
                    Some(response.payload.checkpoint.as_ref().unwrap().minimum)
                );
                assert_eq!(
                    runtime.state().sync_state(),
                    MembershipSyncState::ResyncRequired
                );
                assert!(!runtime.can_accept_pairing());
                let mut forwarder = Forwarder::from_checkpoint_config(
                    &config,
                    runtime.state(),
                    runtime.anchor(),
                    WALL_NOW,
                )
                .unwrap();
                assert!(
                    forwarder
                        .effective_membership()
                        .overlay_members()
                        .next()
                        .is_none()
                );
                let now = Instant::now();
                runtime.begin_resync(now).unwrap();
                assert!(
                    runtime
                        .finish_due(
                            &fixture.store,
                            &mut forwarder,
                            now + RESYNC_WINDOW,
                            WALL_NOW
                        )
                        .unwrap()
                        .is_none()
                );
                assert!(runtime.enrollment_pending());
                assert!(
                    !forwarder
                        .is_configured_transport_peer(fixture.member.peer_id.parse().unwrap())
                );
            }
        }
    }

    #[test]
    fn formation_and_accepted_solo_replay_preserve_established_scope_and_exclusion() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let fixture = Fixture::new("solo-established");
        let (offer, response, mut selected) = checkpoint_pairing(&fixture);
        let removal = selected
            .sign_mutation_at(
                &fixture.member,
                MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        selected.apply_mutation_at(&removal, WALL_NOW).unwrap();
        fixture
            .store
            .save_checkpoint(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &selected.retained(),
            )
            .unwrap();
        let before = fs::read(fixture.path()).unwrap();
        let config = solo_config(&fixture);
        let restored =
            CheckpointRuntime::form_new_network(&config, &fixture.local, &fixture.store, WALL_NOW)
                .unwrap();
        assert_eq!(restored.anchor(), fixture.credentials.anchor());
        assert_eq!(restored.credentials.secret(), fixture.credentials.secret());
        assert_eq!(restored.state().retained(), selected.retained());
        assert_eq!(
            restored.state().sync_state(),
            MembershipSyncState::ResyncRequired
        );
        let replayed = CheckpointRuntime::stage_pairing_enrollment_from_solo(
            &config,
            &fixture.local,
            &offer,
            &response,
            &fixture.store,
            WALL_NOW,
        )
        .unwrap();
        assert_eq!(replayed.state().retained(), selected.retained());
        assert!(
            replayed
                .state()
                .snapshot()
                .payload
                .member(&fixture.local.peer_id)
                .is_none()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);

        let different = CheckpointCredentials::new(
            NetworkAnchor::new([46; 32]).unwrap(),
            fixture.credentials.secret().to_vec(),
        )
        .unwrap();
        let foreign = CooperativeMembershipState::bootstrap_at(
            different.capability().unwrap(),
            fixture.local.peer_id.clone(),
            vec![CheckpointMember::new(&fixture.local).unwrap()],
            SnapshotPolicy::default(),
            WALL_NOW,
        )
        .unwrap();
        fixture
            .store
            .save_checkpoint(
                "lab",
                &fixture.local.peer_id,
                &different,
                &foreign.retained(),
            )
            .unwrap_err();
        let mut mismatched = config.clone();
        mismatched.network.membership_key = Some(STANDARD.encode([90; 32]));
        assert!(
            CheckpointRuntime::form_new_network(
                &mismatched,
                &fixture.local,
                &fixture.store,
                WALL_NOW
            )
            .is_err()
        );
        assert!(
            CheckpointRuntime::stage_pairing_enrollment_from_solo(
                &mismatched,
                &fixture.local,
                &offer,
                &response,
                &fixture.store,
                WALL_NOW
            )
            .is_err()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
    }

    #[test]
    fn solo_conversion_prewrite_failures_preserve_legacy_and_signed_approval() {
        let fixture = Fixture::new("solo-prewrite");
        let (offer, response, _) = checkpoint_pairing(&fixture);
        let config = solo_config(&fixture);
        write_solo_legacy(&fixture, 2, &[], &[]);
        let before = fs::read(fixture.path()).unwrap();
        fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            CheckpointRuntime::form_new_network(&config, &fixture.local, &fixture.store, WALL_NOW)
                .is_err()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
        assert!(
            CheckpointRuntime::stage_pairing_enrollment_from_solo(
                &config,
                &fixture.local,
                &offer,
                &response,
                &fixture.store,
                WALL_NOW
            )
            .is_err()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
        fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o700)).unwrap();
        let mut bad_response = response.clone();
        bad_response
            .payload
            .checkpoint
            .as_mut()
            .unwrap()
            .minimum
            .authority_revision += 1;
        assert!(
            CheckpointRuntime::stage_pairing_enrollment_from_solo(
                &config,
                &fixture.local,
                &offer,
                &bad_response,
                &fixture.store,
                WALL_NOW
            )
            .is_err()
        );
        assert!(
            CheckpointRuntime::stage_pairing_enrollment_from_solo(
                &config,
                &fixture.local,
                &offer,
                &response,
                &fixture.store,
                WALL_NOW + 301
            )
            .is_err()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
        assert!(
            CheckpointRuntime::stage_pairing_enrollment_from_solo(
                &config,
                &fixture.local,
                &offer,
                &response,
                &fixture.store,
                WALL_NOW
            )
            .unwrap()
            .enrollment_pending()
        );
    }

    #[test]
    fn solo_conversion_postrename_failures_preserve_visible_scope_and_pending_floor() {
        for pending in [false, true] {
            let fixture = Fixture::new(&format!("solo-postrename-{pending}"));
            let (offer, response, _) = checkpoint_pairing(&fixture);
            let config = solo_config(&fixture);
            write_solo_legacy(&fixture, 2, &[], &[]);
            let grant = response.payload.checkpoint.as_ref().unwrap();
            let credentials = if pending {
                fixture.credentials.clone()
            } else {
                CheckpointCredentials::generate().unwrap()
            };
            let seed = CooperativeMembershipState::bootstrap_at(
                credentials.capability().unwrap(),
                fixture.local.peer_id.clone(),
                if pending {
                    vec![grant.inviter.clone(), grant.joiner.clone()]
                } else {
                    vec![CheckpointMember::new(&fixture.local).unwrap()]
                },
                SnapshotPolicy::default(),
                WALL_NOW,
            )
            .unwrap();
            assert!(matches!(
                fixture
                    .store
                    .save_checkpoint_with_parent_sync_failure_for_test(
                        "lab",
                        &fixture.local.peer_id,
                        &credentials,
                        &seed.retained(),
                        pending.then_some(grant.minimum),
                    ),
                Err(MembershipStateStoreError::CheckpointDurabilityUncertain(_))
            ));
            let visible = fs::read(fixture.path()).unwrap();
            let restored = CheckpointRuntime::form_new_network(
                &config,
                &fixture.local,
                &fixture.store,
                WALL_NOW,
            )
            .unwrap();
            assert_eq!(restored.credentials.anchor(), credentials.anchor());
            assert_eq!(restored.credentials.secret(), credentials.secret());
            assert_eq!(restored.state().retained(), seed.retained());
            assert_eq!(restored.enrollment_pending(), pending);
            assert_eq!(
                restored.state().sync_state(),
                MembershipSyncState::ResyncRequired
            );
            assert_eq!(fs::read(fixture.path()).unwrap(), visible);
            if pending {
                let replay = CheckpointRuntime::stage_pairing_enrollment_from_solo(
                    &config,
                    &fixture.local,
                    &offer,
                    &response,
                    &fixture.store,
                    WALL_NOW,
                )
                .unwrap();
                assert_eq!(replay.enrollment_floor, Some(grant.minimum));
                assert_eq!(replay.state().retained(), seed.retained());
                assert_eq!(
                    replay.state().sync_state(),
                    MembershipSyncState::ResyncRequired
                );
                assert_eq!(fs::read(fixture.path()).unwrap(), visible);
            }
        }
    }

    fn surviving_owner(fixture: &Fixture) -> (CheckpointRuntime, Forwarder, MembershipStateStore) {
        let directory = fixture.directory.join("survivor");
        fs::create_dir(&directory).unwrap();
        let store = MembershipStateStore::new(directory.join("membership-state.json"));
        store
            .save_checkpoint(
                "lab",
                &fixture.member.peer_id,
                &fixture.credentials,
                &fixture.state.retained(),
            )
            .unwrap();
        let Some(super::super::membership_store::checkpoint::PersistedAuthority::Checkpoint(
            loaded,
        )) = store
            .load_authority(
                "lab",
                &fixture.member.peer_id,
                Some(fixture.credentials.anchor()),
                None,
            )
            .unwrap()
        else {
            panic!("survivor authority");
        };
        let mut runtime =
            CheckpointRuntime::restore("lab".into(), &fixture.member.peer_id, *loaded).unwrap();
        let mut config = fixture.config.clone();
        config.network.local_peer = fixture.member.peer_id.clone();
        config.network.private_key = Some(fixture.member.private_key.clone());
        config.peers.clear();
        let mut forwarder =
            Forwarder::from_checkpoint_config(&config, runtime.state(), runtime.anchor(), WALL_NOW)
                .unwrap();
        let now = Instant::now();
        runtime.begin_resync(now).unwrap();
        runtime
            .finish_due(&store, &mut forwarder, now + RESYNC_WINDOW, WALL_NOW)
            .unwrap();
        (runtime, forwarder, store)
    }

    #[test]
    fn pairing_grants_require_ready_owner_matching_identity_anchor_and_capability() {
        let fixture = Fixture::new("owner-pairing-grant");
        let (mut runtime, _) = fixture.participating();
        let joiner = NodeIdentity::generate_ed25519().unwrap();
        let mut candidate = runtime.state().clone();
        let admission = candidate
            .sign_mutation_at(
                &fixture.local,
                MembershipChange::UpsertMember(CheckpointMember::new(&joiner).unwrap()),
                WALL_NOW,
            )
            .unwrap();
        candidate.apply_mutation_at(&admission, WALL_NOW).unwrap();
        assert!(runtime.can_accept_pairing());
        let grant = runtime
            .pairing_grant_for(&candidate, &joiner.peer_id, WALL_NOW)
            .unwrap();
        assert_eq!(grant.inviter.subject.peer_id, fixture.local.peer_id);
        assert_eq!(grant.joiner.subject.peer_id, joiner.peer_id);
        assert_eq!(grant.minimum, candidate.snapshot().payload.rank().unwrap());
        let mut wrong_identity = CooperativeMembershipState::restore(
            fixture.credentials.capability().unwrap(),
            fixture.member.peer_id.clone(),
            candidate.retained(),
        )
        .unwrap();
        let now = Instant::now();
        wrong_identity.begin_resync(now, RESYNC_WINDOW).unwrap();
        wrong_identity
            .finish_resync(now + RESYNC_WINDOW, WALL_NOW)
            .unwrap();
        assert!(
            runtime
                .pairing_grant_for(&wrong_identity, &joiner.peer_id, WALL_NOW)
                .is_err()
        );
        for (anchor, secret) in [
            (NetworkAnchor::new([11; 32]).unwrap(), vec![89; 32]),
            (fixture.credentials.anchor().clone(), vec![90; 32]),
        ] {
            let capability = crate::membership::checkpoint::NetworkCapability::from_secret(
                anchor,
                Some(&secret),
            )
            .unwrap();
            let mut wrong_capability = CooperativeMembershipState::bootstrap_at(
                capability,
                fixture.local.peer_id.clone(),
                candidate.snapshot().payload.members.clone(),
                SnapshotPolicy::default(),
                WALL_NOW,
            )
            .unwrap();
            let change = wrong_capability
                .sign_mutation_at(
                    &fixture.local,
                    MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
                    WALL_NOW,
                )
                .unwrap();
            wrong_capability
                .apply_mutation_at(&change, WALL_NOW)
                .unwrap();
            assert!(
                runtime
                    .pairing_grant_for(&wrong_capability, &joiner.peer_id, WALL_NOW)
                    .is_err()
            );
        }
        runtime.begin_resync(Instant::now()).unwrap();
        assert!(!runtime.can_accept_pairing());
        assert!(
            runtime
                .pairing_grant_for(&candidate, &joiner.peer_id, WALL_NOW)
                .is_err()
        );
        let (restored, _) = fixture.restored();
        assert!(!restored.can_accept_pairing());
    }

    fn checkpoint_pairing(
        fixture: &Fixture,
    ) -> (PairingOffer, PairingResponse, CooperativeMembershipState) {
        use crate::pairing::{
            PairingCheckpointGrant, PairingOfferOptions, PairingResponseOptions,
            build_checkpoint_pairing_response_at, export_code_pairing_offer_at,
        };
        let mut config = fixture.config.clone();
        config.network.local_peer = fixture.member.peer_id.clone();
        config.network.private_key = Some(fixture.member.private_key.clone());
        config.peers.clear();
        let mut state = CooperativeMembershipState::bootstrap_at(
            fixture.credentials.capability().unwrap(),
            fixture.member.peer_id.clone(),
            vec![CheckpointMember::new(&fixture.member).unwrap()],
            SnapshotPolicy::default(),
            WALL_NOW,
        )
        .unwrap();
        let admission = state
            .sign_mutation_at(
                &fixture.member,
                MembershipChange::UpsertMember(CheckpointMember::new(&fixture.local).unwrap()),
                WALL_NOW,
            )
            .unwrap();
        state.apply_mutation_at(&admission, WALL_NOW).unwrap();
        let grant = PairingCheckpointGrant::from_state_at(
            &state,
            fixture.credentials.secret(),
            &fixture.local.peer_id,
            WALL_NOW,
        )
        .unwrap();
        let offer = export_code_pairing_offer_at(&config, PairingOfferOptions::default(), WALL_NOW)
            .unwrap();
        let response = build_checkpoint_pairing_response_at(
            &config,
            &offer,
            PairingResponseOptions {
                joiner_peer: fixture.local.peer_id.clone(),
                assigned_vpn_ip: None,
                membership_key: None,
                member_records: vec![],
                expires_in_seconds: 300,
            },
            grant,
            WALL_NOW,
        )
        .unwrap();
        (offer, response, state)
    }

    #[test]
    fn signed_enrollment_remains_gated_without_a_qualifying_offer_across_restart() {
        let fixture = Fixture::new("signed-enrollment");
        let (offer, response, selected) = checkpoint_pairing(&fixture);
        fs::remove_file(fixture.path()).unwrap();
        let runtime = CheckpointRuntime::stage_pairing_enrollment(
            &fixture.config,
            &fixture.local,
            &offer,
            &response,
            &fixture.store,
            WALL_NOW,
        )
        .unwrap();
        assert_eq!(
            runtime.enrollment_floor,
            Some(selected.snapshot().payload.rank().unwrap())
        );
        assert_eq!(
            runtime.state().sync_state(),
            MembershipSyncState::ResyncRequired
        );
        let before = fs::read(fixture.path()).unwrap();
        for _ in 0..3 {
            let (mut runtime, mut forwarder) = fixture.restored();
            let peer = fixture.member.peer_id.parse().unwrap();
            assert!(!forwarder.is_configured_transport_peer(peer));
            assert!(
                runtime
                    .apply_change(
                        MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
                        &fixture.local,
                        &fixture.store,
                        &mut forwarder,
                        WALL_NOW
                    )
                    .is_err()
            );
            let now = Instant::now();
            runtime.begin_resync(now).unwrap();
            assert!(
                runtime
                    .state()
                    .make_offer_at(
                        runtime.challenge().unwrap().clone(),
                        &fixture.local,
                        WALL_NOW
                    )
                    .is_err()
            );
            assert!(
                runtime
                    .finish_due(
                        &fixture.store,
                        &mut forwarder,
                        now + RESYNC_WINDOW,
                        WALL_NOW
                    )
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                runtime.state().sync_state(),
                MembershipSyncState::ResyncRequired
            );
            assert!(!forwarder.is_configured_transport_peer(peer));
            assert!(runtime.requests.is_empty());
            assert_eq!(runtime.transfer.stats().buffered_bytes, 0);
            let mut lines = Vec::new();
            runtime.extend_status_lines(&mut lines);
            assert!(lines.contains(&"checkpoint_enrollment_pending 1".to_owned()));
            assert!(lines.contains(&"checkpoint_enrollment_minimum_revision 1".to_owned()));
            assert_eq!(fs::read(fixture.path()).unwrap(), before);
        }
    }

    #[test]
    fn enrollment_rejects_signed_but_insufficient_snapshot_then_accepts_current_state() {
        let fixture = Fixture::new("enrollment-stale-offer");
        let (offer, response, selected) = checkpoint_pairing(&fixture);
        let mut runtime = CheckpointRuntime::stage_pairing_enrollment(
            &fixture.config,
            &fixture.local,
            &offer,
            &response,
            &fixture.store,
            WALL_NOW,
        )
        .unwrap();
        let mut forwarder = Forwarder::from_checkpoint_config(
            &fixture.config,
            runtime.state(),
            runtime.anchor(),
            WALL_NOW,
        )
        .unwrap();
        let mut stale = CooperativeMembershipState::restore(
            fixture.credentials.capability().unwrap(),
            fixture.member.peer_id.clone(),
            runtime.state().retained(),
        )
        .unwrap();
        let stale_round = Instant::now();
        stale.begin_resync(stale_round, RESYNC_WINDOW).unwrap();
        stale
            .finish_resync(stale_round + RESYNC_WINDOW, WALL_NOW)
            .unwrap();
        let before = fs::read(fixture.path()).unwrap();
        let now = Instant::now();
        runtime.begin_resync(now).unwrap();
        let stale_offer = stale
            .make_offer_at(
                runtime.challenge().unwrap().clone(),
                &fixture.member,
                WALL_NOW,
            )
            .unwrap();
        runtime
            .collect_offer(&stale_offer, fixture.member.peer_id.parse().unwrap(), now)
            .unwrap();
        assert!(
            runtime
                .finish_due(
                    &fixture.store,
                    &mut forwarder,
                    now + RESYNC_WINDOW,
                    WALL_NOW
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
        assert!(runtime.enrollment_floor.is_some());
        assert_eq!(
            runtime.state().sync_state(),
            MembershipSyncState::ResyncRequired
        );
        let later = now + Duration::from_secs(60);
        runtime.begin_resync(later).unwrap();
        let current = selected
            .make_offer_at(
                runtime.challenge().unwrap().clone(),
                &fixture.member,
                WALL_NOW,
            )
            .unwrap();
        runtime
            .collect_offer(&current, fixture.member.peer_id.parse().unwrap(), later)
            .unwrap();
        let selection = runtime
            .finish_due(
                &fixture.store,
                &mut forwarder,
                later + RESYNC_WINDOW,
                WALL_NOW,
            )
            .unwrap()
            .unwrap();
        assert!(selection.observed_remote_offer);
        assert_eq!(runtime.enrollment_floor, None);
        assert!(forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap()));
        assert_eq!(fixture.restored().0.enrollment_floor, None);
    }

    #[test]
    fn enrollment_validates_signature_scope_and_pins_before_writing() {
        let fixture = Fixture::new("enrollment-validation");
        let (offer, response, _) = checkpoint_pairing(&fixture);
        let before = fs::read(fixture.path()).unwrap();
        let mut tampered = response.clone();
        tampered
            .payload
            .checkpoint
            .as_mut()
            .unwrap()
            .minimum
            .authority_revision += 1;
        assert!(
            CheckpointRuntime::stage_pairing_enrollment(
                &fixture.config,
                &fixture.local,
                &offer,
                &tampered,
                &fixture.store,
                WALL_NOW
            )
            .is_err()
        );
        assert!(
            CheckpointRuntime::stage_pairing_enrollment(
                &crate::config::Config {
                    network: crate::config::NetworkConfig {
                        name: "other".into(),
                        ..fixture.config.network.clone()
                    },
                    ..fixture.config.clone()
                },
                &fixture.local,
                &offer,
                &response,
                &fixture.store,
                WALL_NOW
            )
            .is_err()
        );
        assert!(
            CheckpointRuntime::stage_pairing_enrollment(
                &fixture.config,
                &fixture.member,
                &offer,
                &response,
                &fixture.store,
                WALL_NOW
            )
            .is_err()
        );
        assert!(
            CheckpointRuntime::stage_pairing_enrollment(
                &fixture.config,
                &fixture.local,
                &offer,
                &response,
                &fixture.store,
                WALL_NOW + 301
            )
            .is_err()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
        let mut foreign = Fixture::new("enrollment-foreign-anchor");
        foreign.credentials =
            CheckpointCredentials::new(NetworkAnchor::new([46; 32]).unwrap(), vec![90; 32])
                .unwrap();
        let (foreign_offer, foreign_response, _) = checkpoint_pairing(&foreign);
        assert!(
            CheckpointRuntime::stage_pairing_enrollment(
                &foreign.config,
                &foreign.local,
                &foreign_offer,
                &foreign_response,
                &foreign.store,
                WALL_NOW
            )
            .is_err()
        );
    }

    #[test]
    fn enrollment_cannot_replace_legacy_authority_or_a_newer_pending_floor() {
        let fixture = Fixture::new("enrollment-preserve");
        let (offer, response, _) = checkpoint_pairing(&fixture);
        let high_floor = SnapshotRank {
            authority_revision: 10,
            ..response.payload.checkpoint.as_ref().unwrap().minimum
        };
        fixture
            .store
            .save_pending_checkpoint_enrollment(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &fixture.state.retained(),
                high_floor,
            )
            .unwrap();
        let runtime = CheckpointRuntime::stage_pairing_enrollment(
            &fixture.config,
            &fixture.local,
            &offer,
            &response,
            &fixture.store,
            WALL_NOW,
        )
        .unwrap();
        assert_eq!(runtime.enrollment_floor, Some(high_floor));
        assert_eq!(fixture.restored().0.enrollment_floor, Some(high_floor));
        let legacy = serde_json::to_vec(&serde_json::json!({
            "version": 2, "network_name": "lab", "local_peer": fixture.local.peer_id,
            "records": [], "hostname_records": [],
        }))
        .unwrap();
        fs::write(fixture.path(), &legacy).unwrap();
        assert!(
            CheckpointRuntime::stage_pairing_enrollment(
                &fixture.config,
                &fixture.local,
                &offer,
                &response,
                &fixture.store,
                WALL_NOW
            )
            .is_err()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), legacy);
    }

    #[test]
    fn fresh_enrollment_cannot_replace_an_explicit_configured_secret() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let fixture = Fixture::new("enrollment-configured-secret");
        let (offer, response, _) = checkpoint_pairing(&fixture);
        fs::remove_file(fixture.path()).unwrap();
        let mut config = fixture.config.clone();
        config.network.membership_key = Some(STANDARD.encode([90; 32]));
        assert!(matches!(
            CheckpointRuntime::stage_pairing_enrollment(
                &config,
                &fixture.local,
                &offer,
                &response,
                &fixture.store,
                WALL_NOW
            ),
            Err(RunnerError::MembershipStateStore(
                MembershipStateStoreError::CapabilityMismatch
            ))
        ));
        assert!(!fixture.path().exists());
        config.network.membership_key = Some(STANDARD.encode(fixture.credentials.secret()));
        let staged = CheckpointRuntime::stage_pairing_enrollment(
            &config,
            &fixture.local,
            &offer,
            &response,
            &fixture.store,
            WALL_NOW,
        )
        .unwrap();
        assert!(staged.enrollment_floor.is_some());
        assert_eq!(
            staged.state().sync_state(),
            MembershipSyncState::ResyncRequired
        );
    }

    #[test]
    fn replayed_approval_preserves_newer_removal_instead_of_reinstalling_joiner() {
        let fixture = Fixture::new("enrollment-replay");
        let (offer, response, mut selected) = checkpoint_pairing(&fixture);
        let remove = selected
            .sign_mutation_at(
                &fixture.member,
                MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        selected.apply_mutation_at(&remove, WALL_NOW).unwrap();
        fixture
            .store
            .save_checkpoint(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &selected.retained(),
            )
            .unwrap();
        let before = fs::read(fixture.path()).unwrap();
        let runtime = CheckpointRuntime::stage_pairing_enrollment(
            &fixture.config,
            &fixture.local,
            &offer,
            &response,
            &fixture.store,
            WALL_NOW,
        )
        .unwrap();
        assert!(runtime.enrollment_floor.is_none());
        assert!(
            runtime
                .state()
                .snapshot()
                .payload
                .member(&fixture.local.peer_id)
                .is_none()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
    }

    #[test]
    fn qualifying_visible_replacement_retries_gated_without_fetching_enrollment_again() {
        let fixture = Fixture::new("enrollment-retry");
        let (offer, response, selected) = checkpoint_pairing(&fixture);
        let mut runtime = CheckpointRuntime::stage_pairing_enrollment(
            &fixture.config,
            &fixture.local,
            &offer,
            &response,
            &fixture.store,
            WALL_NOW,
        )
        .unwrap();
        let mut forwarder = Forwarder::from_checkpoint_config(
            &fixture.config,
            runtime.state(),
            runtime.anchor(),
            WALL_NOW,
        )
        .unwrap();
        fixture
            .store
            .save_checkpoint(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &selected.retained(),
            )
            .unwrap();
        let candidate = CooperativeMembershipState::restore(
            fixture.credentials.capability().unwrap(),
            fixture.local.peer_id.clone(),
            selected.retained(),
        )
        .unwrap();
        runtime
            .install_gated(candidate, &mut forwarder, WALL_NOW)
            .unwrap();
        assert_eq!(runtime.enrollment_floor, None);
        assert_eq!(
            runtime.state().sync_state(),
            MembershipSyncState::ResyncRequired
        );
        let now = Instant::now();
        runtime.begin_resync(now).unwrap();
        runtime
            .finish_due(
                &fixture.store,
                &mut forwarder,
                now + RESYNC_WINDOW,
                WALL_NOW,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            runtime.state().sync_state(),
            MembershipSyncState::Participating
        );
        assert_eq!(
            runtime.state().snapshot().payload,
            selected.snapshot().payload
        );
    }

    #[test]
    fn final_departure_revokes_local_authority_and_survivor_persists_without_tombstones() {
        let fixture = Fixture::new("departure");
        let (mut creator, mut creator_forwarder) = fixture.participating();
        let (mut survivor, mut survivor_forwarder, survivor_store) = surviving_owner(&fixture);
        let recipient = fixture.member.peer_id.parse().unwrap();
        let now = Instant::now();
        creator
            .apply_change_with_handoff(
                MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                &fixture.local,
                &fixture.store,
                &mut creator_forwarder,
                &[recipient],
                now,
                WALL_NOW,
            )
            .unwrap();
        assert_eq!(creator.state().sync_state(), MembershipSyncState::Excluded);
        assert!(!creator_forwarder.is_configured_transport_peer(recipient));
        let request =
            CheckpointMutationRequest::new(creator.handoff.as_ref().unwrap().mutation().clone())
                .unwrap();
        assert!(
            creator
                .state()
                .make_offer_at(
                    SnapshotChallenge {
                        anchor: creator.anchor().clone(),
                        nonce: [8; 32]
                    },
                    &fixture.local,
                    WALL_NOW
                )
                .is_err()
        );
        let MutationOutcome::Applied(boundary) = survivor.incoming_mutation_outcome(
            &request,
            fixture.local.peer_id.parse().unwrap(),
            &survivor_store,
            &mut survivor_forwarder,
            WALL_NOW,
        ) else {
            panic!("active survivor must install final exact-base departure");
        };
        assert!(creator.handoff.as_ref().unwrap().acknowledges(boundary));
        assert_eq!(
            survivor.state().sync_state(),
            MembershipSyncState::Participating
        );
        assert!(
            survivor
                .state()
                .snapshot()
                .payload
                .member(&fixture.member.peer_id)
                .is_some()
        );
        assert!(
            !survivor_forwarder
                .is_configured_transport_peer(fixture.local.peer_id.parse().unwrap())
        );
        let bytes =
            fs::read_to_string(fixture.directory.join("survivor/membership-state.json")).unwrap();
        assert!(!bytes.contains(&fixture.local.peer_id));
        assert!(!bytes.contains("tombstone"));
        assert!(!bytes.contains("inviter"));
        assert_eq!(
            survivor.incoming_mutation_outcome(
                &request,
                fixture.local.peer_id.parse().unwrap(),
                &survivor_store,
                &mut survivor_forwarder,
                WALL_NOW,
            ),
            MutationOutcome::Rejected {
                reason: MutationRejection::StaleBase,
                current: Some(boundary)
            }
        );
        assert_eq!(
            fs::read_to_string(fixture.directory.join("survivor/membership-state.json")).unwrap(),
            bytes
        );
        let creator_bytes: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
        assert!(
            !creator_bytes["retained"]
                .to_string()
                .contains(&fixture.local.peer_id)
        );
        creator.finish_handoff_if_due(now + super::super::checkpoint_handoff::HANDOFF_WINDOW);
        assert!(creator.handoff.is_none());
        assert!(creator.mutation_requests.is_empty());
        assert_eq!(creator.last_handoff.failed, 1);
    }

    #[test]
    fn rejected_or_unpersisted_remote_command_cannot_acknowledge_or_change_authority() {
        let fixture = Fixture::new("departure-failure");
        let (mut sender, mut sender_forwarder) = fixture.participating();
        let (mut receiver, mut receiver_forwarder, store) = surviving_owner(&fixture);
        sender
            .apply_change_with_handoff(
                MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                &fixture.local,
                &fixture.store,
                &mut sender_forwarder,
                &[fixture.member.peer_id.parse().unwrap()],
                Instant::now(),
                WALL_NOW,
            )
            .unwrap();
        let request =
            CheckpointMutationRequest::new(sender.handoff.as_ref().unwrap().mutation().clone())
                .unwrap();
        let before = receiver.state().retained();
        let path = fixture.directory.join("survivor/membership-state.json");
        let bytes = fs::read(&path).unwrap();
        assert_eq!(
            receiver.incoming_mutation_outcome(
                &request,
                fixture.member.peer_id.parse().unwrap(),
                &store,
                &mut receiver_forwarder,
                WALL_NOW
            ),
            MutationOutcome::Rejected {
                reason: MutationRejection::Unauthorized,
                current: None
            }
        );
        let directory = fixture.directory.join("survivor");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(
            receiver.incoming_mutation_outcome(
                &request,
                fixture.local.peer_id.parse().unwrap(),
                &store,
                &mut receiver_forwarder,
                WALL_NOW
            ),
            MutationOutcome::Rejected {
                reason: MutationRejection::PersistenceFailed,
                current: None
            }
        );
        assert_eq!(receiver.state().retained(), before);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(
            receiver_forwarder.is_configured_transport_peer(fixture.local.peer_id.parse().unwrap())
        );
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn live_refresh_accepts_exact_base_departure_without_losing_higher_offer_or_extending_deadline()
    {
        for higher_offer in [false, true] {
            let fixture = Fixture::new(if higher_offer {
                "refresh-higher"
            } else {
                "refresh-departure"
            });
            let request = CheckpointMutationRequest::new(
                fixture
                    .state
                    .sign_mutation_at(
                        &fixture.local,
                        MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                        WALL_NOW,
                    )
                    .unwrap(),
            )
            .unwrap();
            let now = Instant::now();
            let sender = fixture.local.peer_id.parse().unwrap();
            let (mut gated, mut gated_forwarder) = fixture.restored();
            gated.begin_resync(now).unwrap();
            assert_eq!(
                gated.incoming_mutation_outcome(
                    &request,
                    sender,
                    &fixture.store,
                    &mut gated_forwarder,
                    WALL_NOW
                ),
                MutationOutcome::Rejected {
                    reason: MutationRejection::ResyncRequired,
                    current: None
                }
            );

            let (mut receiver, mut forwarder, store) = surviving_owner(&fixture);
            receiver.begin_resync(now).unwrap();
            let challenge = receiver.challenge().unwrap().clone();
            let mut wrong_context = receiver.pending.as_ref().unwrap().candidate.clone();
            assert!(
                wrong_context
                    .rebase_live_resync_on_installed(&fixture.state)
                    .is_err()
            );
            assert!(
                wrong_context
                    .rebase_live_resync_on_installed(receiver.state())
                    .is_err()
            );
            let mut other = fixture.state.clone();
            if higher_offer {
                for maximum in [255, 254] {
                    let mutation = other
                        .sign_mutation_at(
                            &fixture.local,
                            MembershipChange::SetPolicy(SnapshotPolicy {
                                max_active_members: maximum,
                                ..SnapshotPolicy::default()
                            }),
                            WALL_NOW,
                        )
                        .unwrap();
                    other.apply_mutation_at(&mutation, WALL_NOW).unwrap();
                }
                let offer = other
                    .make_offer_at(challenge.clone(), &fixture.local, WALL_NOW)
                    .unwrap();
                receiver.collect_offer(&offer, sender, now).unwrap();
            }
            let MutationOutcome::Applied(boundary) = receiver.incoming_mutation_outcome(
                &request,
                sender,
                &store,
                &mut forwarder,
                WALL_NOW,
            ) else {
                panic!("installed live authority must process an exact-base removal");
            };
            assert_eq!(boundary.authority_revision, 1);
            assert!(!forwarder.is_configured_transport_peer(sender));
            assert_eq!(receiver.challenge(), Some(&challenge));
            assert_eq!(
                receiver.pending.as_ref().unwrap().deadline,
                now + RESYNC_WINDOW
            );
            assert!(
                receiver
                    .finish_due(
                        &store,
                        &mut forwarder,
                        now + RESYNC_WINDOW - Duration::from_millis(1),
                        WALL_NOW
                    )
                    .unwrap()
                    .is_none()
            );
            let selection = receiver
                .finish_due(&store, &mut forwarder, now + RESYNC_WINDOW, WALL_NOW)
                .unwrap()
                .unwrap();
            if higher_offer {
                assert_eq!(
                    receiver.state().snapshot().payload,
                    other.snapshot().payload
                );
                assert!(selection.decisions_may_have_been_discarded);
            } else {
                assert_eq!(selection.selected, boundary);
                assert!(!forwarder.is_configured_transport_peer(sender));
            }
        }
    }

    #[test]
    fn concurrent_local_commands_are_rejected_without_overwriting_bounded_handoff() {
        let fixture = Fixture::new("serialized-handoff");
        let (mut runtime, mut forwarder) = fixture.participating();
        let now = Instant::now();
        let peer = fixture.member.peer_id.parse().unwrap();
        let mut policy = runtime.state().snapshot().payload.policy.clone();
        policy.route_grants_enabled = false;
        runtime
            .apply_change_with_handoff(
                MembershipChange::SetPolicy(policy),
                &fixture.local,
                &fixture.store,
                &mut forwarder,
                &[peer, PeerId::random()],
                now,
                WALL_NOW,
            )
            .unwrap();
        let retained = runtime.state().retained();
        let bytes = fs::read(fixture.path()).unwrap();
        assert_eq!(runtime.handoff.as_ref().unwrap().report().recipients, 1);
        assert!(
            runtime
                .apply_change_with_handoff(
                    MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
                    &fixture.local,
                    &fixture.store,
                    &mut forwarder,
                    &[peer],
                    now,
                    WALL_NOW
                )
                .is_err()
        );
        assert_eq!(runtime.state().retained(), retained);
        assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
        runtime.finish_handoff_if_due(now + super::super::checkpoint_handoff::HANDOFF_WINDOW);
        assert!(runtime.handoff.is_none());
        assert!(runtime.mutation_requests.is_empty());
        let mut lines = Vec::new();
        runtime.extend_status_lines(&mut lines);
        assert!(lines.contains(&"checkpoint_handoff_failed 1".to_owned()));
        assert!(
            lines
                .iter()
                .all(|line| !line.contains(&fixture.local.peer_id)
                    && !line.contains(&fixture.member.peer_id))
        );
    }

    #[test]
    fn restart_and_singleton_progress_gate_packets_and_mutations_until_deadline() {
        let fixture = Fixture::new("restart");
        let (mut runtime, mut forwarder) = fixture.restored();
        let peer = fixture.member.peer_id.parse().unwrap();
        assert!(!forwarder.is_configured_transport_peer(peer));
        let before = fs::read(fixture.path()).unwrap();
        assert!(
            runtime
                .apply_change(
                    MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
                    &fixture.local,
                    &fixture.store,
                    &mut forwarder,
                    WALL_NOW
                )
                .is_err()
        );
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
        let now = Instant::now();
        runtime.begin_resync(now).unwrap();
        let challenge = runtime.challenge().unwrap().clone();
        runtime.begin_resync(now + Duration::from_secs(1)).unwrap();
        assert_eq!(runtime.challenge(), Some(&challenge));
        assert!(
            runtime
                .finish_due(
                    &fixture.store,
                    &mut forwarder,
                    now + RESYNC_WINDOW - Duration::from_nanos(1),
                    WALL_NOW
                )
                .unwrap()
                .is_none()
        );
        assert!(!forwarder.is_configured_transport_peer(peer));
        let result = runtime
            .finish_due(
                &fixture.store,
                &mut forwarder,
                now + RESYNC_WINDOW,
                WALL_NOW,
            )
            .unwrap()
            .unwrap();
        assert!(!result.observed_remote_offer);
        assert_eq!(runtime.last_selection(), Some(&result));
        assert!(forwarder.is_configured_transport_peer(peer));
        assert!(runtime.challenge().is_none());
    }

    #[test]
    fn catchup_persists_revocation_and_erases_stale_static_peer_without_history() {
        let fixture = Fixture::new("catchup");
        let (mut runtime, mut forwarder) = fixture.restored();
        let mut remote = fixture.state.clone();
        let removal = remote
            .sign_mutation_at(
                &fixture.local,
                MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        remote.apply_mutation_at(&removal, WALL_NOW).unwrap();
        let now = Instant::now();
        runtime.begin_resync(now).unwrap();
        let offer = remote
            .make_offer_at(
                runtime.challenge().unwrap().clone(),
                &fixture.local,
                WALL_NOW,
            )
            .unwrap();
        runtime
            .collect_offer(&offer, fixture.local.peer_id.parse().unwrap(), now)
            .unwrap();
        let result = runtime
            .finish_due(
                &fixture.store,
                &mut forwarder,
                now + RESYNC_WINDOW,
                WALL_NOW,
            )
            .unwrap()
            .unwrap();
        assert_eq!(result.removed_members, 1);
        assert!(!forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap()));
        assert!(forwarder.config().peers.is_empty());
        let bytes = fs::read_to_string(fixture.path()).unwrap();
        assert!(!bytes.contains(&fixture.member.peer_id));
        assert!(!bytes.contains("stale-member"));
        assert!(
            fixture
                .restored()
                .0
                .state()
                .snapshot()
                .payload
                .member(&fixture.member.peer_id)
                .is_none()
        );
    }

    #[test]
    fn prewrite_failure_leaves_selected_authority_and_live_grants_unchanged() {
        let fixture = Fixture::new("prewrite-failure");
        let (mut runtime, mut forwarder) = fixture.participating();
        let before = runtime.state().retained();
        let disk = fs::read(fixture.path()).unwrap();
        fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            runtime
                .apply_change(
                    MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
                    &fixture.local,
                    &fixture.store,
                    &mut forwarder,
                    WALL_NOW
                )
                .is_err()
        );
        assert_eq!(runtime.state().retained(), before);
        assert_eq!(fs::read(fixture.path()).unwrap(), disk);
        assert!(forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap()));
        fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o700)).unwrap();
        runtime
            .apply_change(
                MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
                &fixture.local,
                &fixture.store,
                &mut forwarder,
                WALL_NOW,
            )
            .unwrap();
        assert!(!forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap()));
    }

    #[test]
    fn gated_or_excluded_checkpoint_has_no_local_or_static_dns_records() {
        let fixture = Fixture::new("dns-gate");
        let mut config = fixture.config.clone();
        config.network.dns.enabled = true;
        config.network.dns.hostname = Some("local-device".into());
        let (runtime, _) = fixture.restored();
        let effective = runtime.state().effective_membership_at(WALL_NOW).unwrap();
        let zone = crate::dns::DnsZone::from_config_with_effective_membership_at(
            &config,
            &[],
            &HashMap::new(),
            &effective,
            WALL_NOW,
        )
        .unwrap();
        assert_eq!(zone.records().count(), 0);
        assert_eq!(zone.reverse_records().count(), 0);
        let (mut runtime, mut forwarder) = fixture.participating();
        runtime
            .apply_change(
                MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                &fixture.local,
                &fixture.store,
                &mut forwarder,
                WALL_NOW,
            )
            .unwrap();
        assert_eq!(runtime.state().sync_state(), MembershipSyncState::Excluded);
        let zone = crate::dns::DnsZone::from_config_with_effective_membership_at(
            &config,
            &[],
            &HashMap::new(),
            forwarder.effective_membership(),
            WALL_NOW,
        )
        .unwrap();
        assert_eq!(zone.records().count(), 0);
    }

    fn test_node(identity: &NodeIdentity) -> super::super::p2p::P2pNode {
        super::super::p2p::build_node(&super::super::p2p::HostConfig {
            identity: identity.clone(),
            network_name: "lab".into(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 4,
            max_concurrent_packet_streams: 4,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
            external_addresses: vec![],
            bootstrap_peers: vec![],
            known_peers: vec![],
            relay_reservations: vec![],
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: crate::config::DiscoveryConfig {
                mdns: false,
                kademlia: false,
                autonat: false,
                dcutr: false,
                kademlia_provider_advertisement: false,
                ..crate::config::DiscoveryConfig::default()
            },
        })
        .unwrap()
    }

    async fn connected_mutation_nodes(
        fixture: &Fixture,
    ) -> (
        super::super::p2p::P2pNode,
        super::super::p2p::P2pNode,
        [ConnectionId; 2],
    ) {
        use futures::StreamExt as _;
        use libp2p::swarm::{SwarmEvent, dial_opts::DialOpts};

        let mut sender = test_node(&fixture.local);
        let mut receiver = test_node(&fixture.member);
        let connections = tokio::time::timeout(Duration::from_secs(5), async {
            let address = loop {
                if let SwarmEvent::NewListenAddr { address, .. } =
                    receiver.swarm.select_next_some().await
                {
                    break address;
                }
            };
            sender
                .swarm
                .dial(
                    DialOpts::peer_id(receiver.local_peer_id)
                        .addresses(vec![address])
                        .build(),
                )
                .unwrap();
            let mut connections = [None; 2];
            while connections.iter().any(Option::is_none) {
                tokio::select! {
                    event = sender.swarm.select_next_some() => {
                        if let SwarmEvent::ConnectionEstablished { connection_id, .. } = event {
                            connections[0] = Some(connection_id);
                        }
                    }
                    event = receiver.swarm.select_next_some() => {
                        if let SwarmEvent::ConnectionEstablished { connection_id, .. } = event {
                            connections[1] = Some(connection_id);
                        }
                    }
                }
            }
            connections.map(Option::unwrap)
        })
        .await
        .expect("real mutation connection");
        (sender, receiver, connections)
    }

    async fn receive_mutation_request(
        sender: &mut super::super::p2p::P2pNode,
        receiver: &mut super::super::p2p::P2pNode,
        request: CheckpointMutationRequest,
    ) -> request_response::Event<CheckpointMutationRequest, CheckpointMutationResponse> {
        use super::super::p2p::BehaviourEvent;
        use futures::StreamExt as _;
        use libp2p::swarm::SwarmEvent;

        sender
            .swarm
            .behaviour_mut()
            .checkpoint_mutation
            .send_request(&receiver.local_peer_id, request);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    _ = sender.swarm.select_next_some() => (),
                    event = receiver.swarm.select_next_some() => {
                        if let SwarmEvent::Behaviour(BehaviourEvent::CheckpointMutation(
                            event @ request_response::Event::Message {
                                message: Message::Request { .. }, ..
                            },
                        )) = event {
                            break event;
                        }
                    }
                }
            }
        })
        .await
        .expect("real mutation request")
    }

    fn identify_info(identity: &NodeIdentity) -> libp2p::identify::Info {
        libp2p::identify::Info {
            public_key: identity.public_key().unwrap(),
            protocol_version: "p2p-vpn-test".into(),
            agent_version: "checkpoint-ack-regression".into(),
            listen_addrs: Vec::new(),
            protocols: vec![libp2p::StreamProtocol::new(
                super::super::control::checkpoint_mutation::CHECKPOINT_MUTATION_PROTOCOL,
            )],
            observed_addr: "/ip4/127.0.0.1/tcp/4001".parse().unwrap(),
            signed_peer_record: None,
        }
    }

    #[tokio::test]
    async fn mutation_reply_owners_are_scoped_bounded_and_retired_without_packet_grants() {
        let fixture = Fixture::new("reply-owner-bounds");
        let (mut runtime, mut forwarder, store) = surviving_owner(&fixture);
        let mutation = fixture
            .state
            .sign_mutation_at(
                &fixture.local,
                MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        let request = CheckpointMutationRequest::new(mutation).unwrap();
        let peer = fixture.local.peer_id.parse().unwrap();
        let outcome =
            runtime.incoming_mutation_outcome(&request, peer, &store, &mut forwarder, WALL_NOW);
        assert!(matches!(outcome, MutationOutcome::Applied(_)));
        assert!(!forwarder.is_configured_transport_peer(peer));
        let state_path = fixture.directory.join("survivor/membership-state.json");
        let disk = fs::read(&state_path).unwrap();
        let (mut sender, mut receiver, connections) = connected_mutation_nodes(&fixture).await;
        let now = Instant::now();
        let mut owners = Vec::new();
        for index in 0..=MAX_MUTATION_STREAMS {
            let event = receive_mutation_request(&mut sender, &mut receiver, request.clone()).await;
            if index == MAX_MUTATION_STREAMS {
                use super::super::p2p::BehaviourEvent;
                use futures::StreamExt as _;
                use libp2p::swarm::SwarmEvent;

                let before = runtime.state().retained();
                assert!(!runtime.handle_mutation_event(
                    &mut receiver.swarm,
                    event,
                    &store,
                    &mut forwarder,
                    now,
                    WALL_NOW,
                ));
                assert_eq!(runtime.state().retained(), before);
                let outcome = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        tokio::select! {
                            event = sender.swarm.select_next_some() => {
                                if let SwarmEvent::Behaviour(BehaviourEvent::CheckpointMutation(
                                    request_response::Event::Message {
                                        message: Message::Response { response, .. }, ..
                                    },
                                )) = event {
                                    break response.validate_for(&request).unwrap();
                                }
                            }
                            _ = receiver.swarm.select_next_some() => (),
                        }
                    }
                })
                .await
                .expect("bounded reply owners send Busy instead of applying");
                assert_eq!(
                    outcome,
                    MutationOutcome::Rejected {
                        reason: MutationRejection::Busy,
                        current: None,
                    }
                );
                break;
            }
            let request_response::Event::Message {
                peer: transport_peer,
                connection_id,
                message:
                    Message::Request {
                        request_id,
                        channel,
                        ..
                    },
            } = event
            else {
                unreachable!("request fixture")
            };
            drop(channel);
            assert_eq!(transport_peer, peer);
            assert_eq!(connection_id, connections[1]);
            if index == 0 {
                assert!(!runtime.track_mutation_response(
                    request_id,
                    PeerId::random(),
                    connection_id,
                    &request,
                    outcome,
                    now,
                ));
                let mut wrong_scope = request.clone();
                wrong_scope.mutation.payload.anchor = NetworkAnchor::new([91; 32]).unwrap();
                assert!(!runtime.track_mutation_response(
                    request_id,
                    peer,
                    connection_id,
                    &wrong_scope,
                    outcome,
                    now,
                ));
                let mut bad_signature = request.clone();
                bad_signature.mutation.signature.push('!');
                assert!(!runtime.track_mutation_response(
                    request_id,
                    peer,
                    connection_id,
                    &bad_signature,
                    outcome,
                    now,
                ));
                assert!(!runtime.track_mutation_response(
                    request_id,
                    peer,
                    connection_id,
                    &request,
                    MutationOutcome::Rejected {
                        reason: MutationRejection::Invalid,
                        current: None
                    },
                    now,
                ));
            }
            assert!(runtime.track_mutation_response(
                request_id,
                peer,
                connection_id,
                &request,
                outcome,
                now
            ));
            owners.push(request_id);
            if index == 0 {
                let deadline = runtime.mutation_responses[&request_id].deadline;
                assert!(runtime.track_mutation_response(
                    request_id,
                    peer,
                    connection_id,
                    &request,
                    outcome,
                    now + Duration::from_secs(4),
                ));
                assert_eq!(runtime.mutation_responses[&request_id].deadline, deadline);
            }
        }
        assert_eq!(runtime.mutation_responses.len(), MAX_MUTATION_STREAMS);
        assert!(runtime.owns_mutation_response(peer, connections[1], now));
        assert!(!runtime.owns_mutation_response(PeerId::random(), connections[1], now));
        let other_connection = ConnectionId::new_unchecked(usize::MAX);
        assert!(!runtime.owns_mutation_response(peer, other_connection, now));
        runtime.retire_mutation_response(owners[0], PeerId::random(), connections[1]);
        runtime.retire_mutation_response(owners[0], peer, other_connection);
        assert_eq!(runtime.mutation_responses.len(), MAX_MUTATION_STREAMS);
        runtime.handle_mutation_event(
            &mut receiver.swarm,
            request_response::Event::ResponseSent {
                peer,
                connection_id: connections[1],
                request_id: owners[0],
            },
            &store,
            &mut forwarder,
            now,
            WALL_NOW,
        );
        assert_eq!(runtime.mutation_responses.len(), MAX_MUTATION_STREAMS - 1);
        runtime.discard_mutation_event(
            request_response::Event::InboundFailure {
                peer,
                connection_id: connections[1],
                request_id: owners[1],
                error: request_response::InboundFailure::ConnectionClosed,
            },
            now,
        );
        assert_eq!(runtime.mutation_responses.len(), MAX_MUTATION_STREAMS - 2);
        assert!(!runtime.owns_mutation_response(
            peer,
            connections[1],
            now + MUTATION_REQUEST_TIMEOUT
        ));
        assert_eq!(
            runtime
                .sync_candidates(now + MUTATION_REQUEST_TIMEOUT)
                .count(),
            0
        );
        runtime.cleanup_mutation_responses(now + MUTATION_REQUEST_TIMEOUT);
        assert!(runtime.mutation_responses.is_empty());
        assert!(!forwarder.is_configured_transport_peer(peer));
        assert_eq!(fs::read(&state_path).unwrap(), disk);
    }

    #[tokio::test]
    async fn identify_after_durable_departure_preserves_real_ack_then_retires_connection() {
        use super::super::{p2p::BehaviourEvent, runner::dispatch_checkpoint_identify_test_event};
        use futures::StreamExt as _;
        use libp2p::swarm::SwarmEvent;

        for infrastructure_candidate in [false, true] {
            let fixture = Fixture::new(&format!("identify-ack-{infrastructure_candidate}"));
            let (mut creator, mut creator_forwarder) = fixture.participating();
            let (mut survivor, mut survivor_forwarder, survivor_store) = surviving_owner(&fixture);
            let (mut sender, mut receiver, connections) = connected_mutation_nodes(&fixture).await;
            creator
                .apply_change_with_handoff(
                    MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                    &fixture.local,
                    &fixture.store,
                    &mut creator_forwarder,
                    &[receiver.local_peer_id],
                    Instant::now(),
                    WALL_NOW,
                )
                .unwrap();
            assert_eq!(creator.state().sync_state(), MembershipSyncState::Excluded);
            creator
                .drive_mutations(&mut sender.swarm, Instant::now())
                .unwrap();
            dispatch_checkpoint_identify_test_event(
                &mut sender,
                &mut creator_forwarder,
                &creator,
                receiver.local_peer_id,
                connections[0],
                identify_info(&fixture.member),
                infrastructure_candidate,
            );
            let mut queued = false;
            let mut flushed = false;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        event = sender.swarm.select_next_some() => {
                            assert!(!matches!(event, SwarmEvent::ConnectionClosed { .. }), "sender retired before ACK");
                            if let SwarmEvent::Behaviour(BehaviourEvent::CheckpointMutation(event)) = event {
                                creator.handle_mutation_event(
                                    &mut sender.swarm, event, &fixture.store,
                                    &mut creator_forwarder, Instant::now(), WALL_NOW,
                                );
                            }
                        }
                        event = receiver.swarm.select_next_some() => {
                            assert!(!matches!(event, SwarmEvent::ConnectionClosed { .. }), "receiver retired before ACK");
                            if let SwarmEvent::Behaviour(BehaviourEvent::CheckpointMutation(event)) = event {
                                let received = matches!(&event, request_response::Event::Message { message: Message::Request { .. }, .. });
                                let response_sent = matches!(&event, request_response::Event::ResponseSent { .. });
                                survivor.handle_mutation_event(
                                    &mut receiver.swarm, event, &survivor_store,
                                    &mut survivor_forwarder, Instant::now(), WALL_NOW,
                                );
                                if received {
                                    queued = true;
                                    assert!(!survivor_forwarder.is_configured_transport_peer(sender.local_peer_id));
                                    assert_eq!(survivor.mutation_responses.len(), 1);
                                    assert!(survivor.owns_mutation_response(sender.local_peer_id, connections[1], Instant::now()));
                                    dispatch_checkpoint_identify_test_event(
                                        &mut receiver, &mut survivor_forwarder, &survivor,
                                        sender.local_peer_id, connections[1], identify_info(&fixture.local),
                                        infrastructure_candidate,
                                    );
                                }
                                flushed |= response_sent;
                            }
                        }
                    }
                    if creator.last_handoff.acknowledged == 1 && flushed {
                        break;
                    }
                }
            }).await.expect("ACK must survive Identify before flush");
            assert!(queued);
            assert_eq!(creator.last_handoff.failed, 0);
            assert!(survivor.mutation_responses.is_empty());
            assert!(
                !survivor
                    .sync_candidates(Instant::now())
                    .any(|peer| peer == sender.local_peer_id)
            );
            assert!(
                !fs::read_to_string(fixture.directory.join("survivor/membership-state.json"))
                    .unwrap()
                    .contains(&fixture.local.peer_id)
            );
            dispatch_checkpoint_identify_test_event(
                &mut receiver,
                &mut survivor_forwarder,
                &survivor,
                sender.local_peer_id,
                connections[1],
                identify_info(&fixture.local),
                infrastructure_candidate,
            );
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        _ = sender.swarm.select_next_some() => (),
                        event = receiver.swarm.select_next_some() => {
                            if matches!(event, SwarmEvent::ConnectionClosed { .. }) { break; }
                        }
                    }
                }
            })
            .await
            .expect("unowned excluded peer is retired after ACK");
            assert_eq!(creator.last_handoff.acknowledged, 1);
        }
    }

    #[tokio::test]
    async fn handoff_acknowledgments_bind_request_peer_command_and_result_boundary() {
        use libp2p::swarm::ConnectionId;

        for case in [
            "applied",
            "duplicate",
            "wrong-peer",
            "wrong-command",
            "wrong-boundary",
            "late-request",
        ] {
            let fixture = Fixture::new(&format!("ack-{case}"));
            let (mut runtime, mut forwarder) = fixture.participating();
            let mut node = test_node(&fixture.local);
            let peer = fixture.member.peer_id.parse().unwrap();
            let now = Instant::now();
            runtime
                .apply_change_with_handoff(
                    MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                    &fixture.local,
                    &fixture.store,
                    &mut forwarder,
                    &[peer],
                    now,
                    WALL_NOW,
                )
                .unwrap();
            assert_eq!(
                runtime.handoff.as_mut().unwrap().next_ready(now),
                Some(peer)
            );
            let request = CheckpointMutationRequest::new(
                runtime.handoff.as_ref().unwrap().mutation().clone(),
            )
            .unwrap();
            let id = node
                .swarm
                .behaviour_mut()
                .checkpoint_mutation
                .send_request(&peer, request.clone());
            runtime.mutation_requests.insert(id, peer);
            let expected = runtime.state().snapshot().payload.boundary().unwrap();
            let outcome = if case == "duplicate" {
                MutationOutcome::Rejected {
                    reason: MutationRejection::StaleBase,
                    current: Some(expected),
                }
            } else {
                let mut boundary = expected;
                if case == "wrong-boundary" {
                    boundary.digest[0] ^= 1;
                }
                MutationOutcome::Applied(boundary)
            };
            let mut response = CheckpointMutationResponse::for_request(&request, outcome).unwrap();
            if case == "wrong-command" {
                response.mutation_digest[0] ^= 1;
            }
            let response_peer = if case == "wrong-peer" {
                PeerId::random()
            } else {
                peer
            };
            let event_now = if case == "late-request" {
                runtime
                    .finish_handoff_if_due(now + super::super::checkpoint_handoff::ATTEMPT_WINDOW);
                let retry =
                    now + super::super::checkpoint_handoff::ATTEMPT_WINDOW + Duration::from_secs(1);
                assert_eq!(
                    runtime.handoff.as_mut().unwrap().next_ready(retry),
                    Some(peer)
                );
                let fresh_id = node
                    .swarm
                    .behaviour_mut()
                    .checkpoint_mutation
                    .send_request(&peer, request.clone());
                runtime.mutation_requests.insert(fresh_id, peer);
                retry
            } else {
                now
            };
            let event = request_response::Event::Message {
                peer: response_peer,
                connection_id: ConnectionId::new_unchecked(1),
                message: Message::Response {
                    request_id: id,
                    response,
                },
            };
            assert!(!runtime.handle_mutation_event(
                &mut node.swarm,
                event,
                &fixture.store,
                &mut forwarder,
                event_now,
                WALL_NOW,
            ));
            if case == "late-request" {
                assert_eq!(runtime.handoff.as_ref().unwrap().report().acknowledged, 0);
                assert_eq!(runtime.handoff.as_ref().unwrap().report().pending, 1);
                assert_eq!(runtime.mutation_requests.len(), 1);
                runtime
                    .finish_handoff_if_due(now + super::super::checkpoint_handoff::HANDOFF_WINDOW);
            } else {
                assert!(runtime.handoff.is_none());
                assert_eq!(
                    runtime.last_handoff.acknowledged,
                    usize::from(matches!(case, "applied" | "duplicate"))
                );
                assert!(runtime.mutation_requests.is_empty());
            }
        }
    }

    async fn catch_up_over_tcp(
        fixture: &Fixture,
        publisher: &NodeIdentity,
        selected_state: &CooperativeMembershipState,
    ) -> (CheckpointRuntime, Forwarder, BranchSelection) {
        use super::super::p2p::BehaviourEvent;
        use futures::StreamExt as _;
        use libp2p::swarm::{SwarmEvent, dial_opts::DialOpts};

        let (mut local, mut local_forwarder) = fixture.restored();
        let remote_directory = fixture.directory.join("remote");
        fs::create_dir(&remote_directory).unwrap();
        let remote_store =
            MembershipStateStore::new(remote_directory.join("membership-state.json"));
        remote_store
            .save_checkpoint(
                "lab",
                &publisher.peer_id,
                &fixture.credentials,
                &selected_state.retained(),
            )
            .unwrap();
        let Some(super::super::membership_store::checkpoint::PersistedAuthority::Checkpoint(
            loaded,
        )) = remote_store
            .load_authority(
                "lab",
                &publisher.peer_id,
                Some(fixture.credentials.anchor()),
                None,
            )
            .unwrap()
        else {
            panic!("remote authority")
        };
        let mut remote =
            CheckpointRuntime::restore("lab".into(), &publisher.peer_id, *loaded).unwrap();
        let mut remote_config = fixture.config.clone();
        remote_config.network.local_peer = publisher.peer_id.clone();
        remote_config.network.private_key = Some(publisher.private_key.clone());
        remote_config.peers.clear();
        let mut remote_forwarder = Forwarder::from_checkpoint_config(
            &remote_config,
            remote.state(),
            remote.anchor(),
            WALL_NOW,
        )
        .unwrap();
        let start = Instant::now();
        remote.begin_resync(start).unwrap();
        remote
            .finish_due(
                &remote_store,
                &mut remote_forwarder,
                start + RESYNC_WINDOW,
                WALL_NOW,
            )
            .unwrap();
        let mut local_node = test_node(&fixture.local);
        let mut remote_node = test_node(publisher);
        let address = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let SwarmEvent::NewListenAddr { address, .. } =
                    remote_node.swarm.select_next_some().await
                {
                    break address;
                }
            }
        })
        .await
        .unwrap();
        local_node
            .swarm
            .dial(
                DialOpts::peer_id(remote_node.local_peer_id)
                    .addresses(vec![address])
                    .build(),
            )
            .unwrap();
        let now = Instant::now();
        local.begin_resync(now).unwrap();
        let peers = PeerCapabilities::default();
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            loop {
                tokio::select! {
                    _ = tick.tick() => {local.drive_sync(&mut local_node.swarm, &peers, Instant::now()).unwrap();}
                    event = local_node.swarm.select_next_some() => {
                        if matches!(event, SwarmEvent::ConnectionEstablished { .. }) {
                            let descriptor = ControlCapabilities::local("lab", None, 1280)
                                .with_checkpoint(remote.state()).unwrap();
                            assert!(local.observe_capabilities(remote_node.local_peer_id, &descriptor, None, &[], Instant::now()));
                        }
                        if let SwarmEvent::Behaviour(BehaviourEvent::Checkpoint(event)) = event {
                            local.handle_wire_event(&mut local_node.swarm, event, &fixture.local, Instant::now(), WALL_NOW);
                        }
                    }
                    event = remote_node.swarm.select_next_some() => {
                        if let SwarmEvent::Behaviour(BehaviourEvent::Checkpoint(event)) = event {
                            remote.handle_wire_event(&mut remote_node.swarm, event, publisher, Instant::now(), WALL_NOW);
                        }
                    }
                }
                if local.offers_accepted > 0 {break;}
            }
        }).await.expect("authenticated checkpoint must cross a real TCP/Noise connection");
        assert!(!local_forwarder.is_configured_transport_peer(remote_node.local_peer_id));
        let selected = local
            .finish_due(
                &fixture.store,
                &mut local_forwarder,
                now + RESYNC_WINDOW,
                WALL_NOW,
            )
            .unwrap()
            .unwrap();
        assert!(selected.observed_remote_offer);
        assert!(local.requests.is_empty());
        assert_eq!(local.transfer.stats().buffered_bytes, 0);
        assert_eq!(
            local.state().snapshot().payload,
            selected_state.snapshot().payload
        );
        (local, local_forwarder, selected)
    }

    #[tokio::test]
    async fn real_libp2p_transfer_catches_up_excluded_member_without_granting_packets() {
        let fixture = Fixture::new("real-wire");
        let mut selected_state = fixture.state.clone();
        let removal = selected_state
            .sign_mutation_at(
                &fixture.local,
                MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        selected_state
            .apply_mutation_at(&removal, WALL_NOW)
            .unwrap();
        let (local, local_forwarder, selected) =
            catch_up_over_tcp(&fixture, &fixture.member, &selected_state).await;
        assert_eq!(selected.sync_state, MembershipSyncState::Excluded);
        assert!(
            !local_forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap())
        );
        assert!(local.requests.is_empty());
        assert_eq!(local.transfer.stats().buffered_bytes, 0);
        let mut status = Vec::new();
        local.extend_status_lines(&mut status);
        assert!(status.contains(&"checkpoint_sync_state excluded".to_owned()));
        assert!(
            status
                .iter()
                .all(|line| !line.contains(&fixture.local.peer_id)
                    && !line.contains(&fixture.member.peer_id))
        );
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
        assert!(
            !value["retained"]
                .to_string()
                .contains(&fixture.local.peer_id)
        );
        assert_eq!(
            fixture.restored().0.state().snapshot().payload,
            local.state().snapshot().payload
        );
    }

    #[tokio::test]
    async fn signed_enrollment_fetches_large_roster_through_existing_paged_transport() {
        let fixture = Fixture::new("enrollment-paged-roster");
        let (offer, response, mut selected) = checkpoint_pairing(&fixture);
        CheckpointRuntime::stage_pairing_enrollment(
            &fixture.config,
            &fixture.local,
            &offer,
            &response,
            &fixture.store,
            WALL_NOW,
        )
        .unwrap();
        let mut allocated = selected
            .snapshot()
            .payload
            .members
            .iter()
            .map(|member| {
                crate::route::builtin_ipv4(crate::PeerId::from_libp2p(
                    member.subject.peer_id.parse().unwrap(),
                ))
            })
            .collect::<HashSet<_>>();
        for _ in 0..100 {
            let member = unused_ipv4_identity(&mut allocated);
            let admission = selected
                .sign_mutation_at(
                    &fixture.member,
                    MembershipChange::UpsertMember(CheckpointMember::new(&member).unwrap()),
                    WALL_NOW,
                )
                .unwrap();
            selected.apply_mutation_at(&admission, WALL_NOW).unwrap();
        }
        assert!(
            serde_json::to_vec(&selected.retained()).unwrap().len()
                > crate::pairing::MAX_PAIRING_MESSAGE_LEN
        );
        assert!(
            serde_json::to_vec(&response).unwrap().len() < crate::pairing::MAX_PAIRING_MESSAGE_LEN
        );
        let (local, forwarder, selection) =
            catch_up_over_tcp(&fixture, &fixture.member, &selected).await;
        assert_eq!(selection.sync_state, MembershipSyncState::Participating);
        assert_eq!(local.enrollment_floor, None);
        assert_eq!(local.state().snapshot().payload.members.len(), 102);
        assert!(forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap()));
        assert_eq!(fixture.restored().0.enrollment_floor, None);
    }

    #[tokio::test]
    async fn revoked_joiner_fetches_current_state_without_using_old_pairing_approval() {
        let fixture = Fixture::new("enrollment-removed-joiner");
        let (offer, response, mut selected) = checkpoint_pairing(&fixture);
        CheckpointRuntime::stage_pairing_enrollment(
            &fixture.config,
            &fixture.local,
            &offer,
            &response,
            &fixture.store,
            WALL_NOW,
        )
        .unwrap();
        let remove = selected
            .sign_mutation_at(
                &fixture.member,
                MembershipChange::RemoveMember(fixture.local.peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        selected.apply_mutation_at(&remove, WALL_NOW).unwrap();
        let (local, forwarder, selection) =
            catch_up_over_tcp(&fixture, &fixture.member, &selected).await;
        assert_eq!(selection.sync_state, MembershipSyncState::Excluded);
        assert_eq!(local.enrollment_floor, None);
        assert!(!forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap()));
        assert!(
            local
                .state()
                .snapshot()
                .payload
                .member(&fixture.local.peer_id)
                .is_none()
        );
        let retained: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.path()).unwrap()).unwrap();
        assert!(
            !retained["retained"]
                .to_string()
                .contains(&fixture.local.peer_id)
        );
        assert!(retained.get("enrollment_floor").is_none());
    }

    #[tokio::test]
    async fn returning_node_fetches_new_publisher_without_packet_authority_from_old_roster() {
        let fixture = Fixture::new("new-publisher");
        let publisher = NodeIdentity::generate_ed25519().unwrap();
        let mut selected_state = fixture.state.clone();
        for change in [
            MembershipChange::UpsertMember(CheckpointMember::new(&publisher).unwrap()),
            MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
        ] {
            let mutation = selected_state
                .sign_mutation_at(&fixture.local, change, WALL_NOW)
                .unwrap();
            selected_state
                .apply_mutation_at(&mutation, WALL_NOW)
                .unwrap();
        }
        assert!(
            fixture
                .state
                .snapshot()
                .payload
                .member(&publisher.peer_id)
                .is_none()
        );
        let (local, forwarder, selected) =
            catch_up_over_tcp(&fixture, &publisher, &selected_state).await;
        assert_eq!(selected.sync_state, MembershipSyncState::Participating);
        assert!(forwarder.is_configured_transport_peer(publisher.peer_id.parse().unwrap()));
        assert!(!forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap()));
        assert!(
            local
                .state()
                .snapshot()
                .payload
                .member(&fixture.member.peer_id)
                .is_none()
        );
        assert!(
            !fs::read_to_string(fixture.path())
                .unwrap()
                .contains(&fixture.member.peer_id)
        );
    }

    #[tokio::test]
    async fn isolated_gated_recovery_starts_a_round_without_any_connected_peer() {
        let fixture = Fixture::new("gated-recovery");
        let (mut runtime, mut forwarder) = fixture.participating();
        runtime
            .install_gated(runtime.state().clone(), &mut forwarder, WALL_NOW)
            .unwrap();
        assert!(runtime.pending.is_none());
        let mut node = test_node(&fixture.local);
        let now = Instant::now();
        runtime
            .drive_sync(&mut node.swarm, &PeerCapabilities::default(), now)
            .unwrap();
        assert!(runtime.pending.is_some());
        assert!(!forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap()));
        runtime
            .finish_due(
                &fixture.store,
                &mut forwarder,
                now + RESYNC_WINDOW,
                WALL_NOW,
            )
            .unwrap();
        assert_eq!(
            runtime.state().sync_state(),
            MembershipSyncState::Participating
        );
        assert!(forwarder.is_configured_transport_peer(fixture.member.peer_id.parse().unwrap()));
    }

    #[test]
    fn control_only_candidates_are_scoped_bounded_and_expire_without_packet_grants() {
        let fixture = Fixture::new("candidate-bounds");
        let (mut runtime, forwarder) = fixture.restored();
        let now = Instant::now();
        let capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_checkpoint(&fixture.state)
            .unwrap();
        let mut wrong_scope = capabilities.clone();
        wrong_scope.checkpoint.as_mut().unwrap().anchor = NetworkAnchor::new([1; 32]).unwrap();
        assert!(!runtime.observe_capabilities(PeerId::random(), &wrong_scope, None, &[], now));
        let mut wrong_network = capabilities.clone();
        wrong_network.network_name = "other".to_owned();
        assert!(!runtime.observe_capabilities(PeerId::random(), &wrong_network, None, &[], now));
        for _ in 0..MAX_TRANSFER_SESSIONS {
            let peer = PeerId::random();
            assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], now));
            assert!(!forwarder.is_configured_transport_peer(peer));
        }
        assert_eq!(runtime.sync_candidates(now).count(), MAX_TRANSFER_SESSIONS);
        assert!(!runtime.observe_capabilities(PeerId::random(), &capabilities, None, &[], now));
        assert_eq!(runtime.sync_candidates(now + RESYNC_WINDOW).count(), 0);
        assert!(runtime.observe_capabilities(
            PeerId::random(),
            &capabilities,
            None,
            &[],
            now + RESYNC_WINDOW
        ));
        assert_eq!(runtime.candidates.len(), 1);
    }

    #[tokio::test]
    async fn daemon_restarts_gated_then_revokes_through_its_existing_control_api() {
        use super::super::{
            control_socket::runtime_control_channel,
            runner::{
                PreconfiguredTunRoutes, RuntimePlatform, run_config_until_with_runtime_platform,
            },
            tun::{PacketIo, PacketRead, PacketWrite},
        };

        struct EmptyPacketDevice;
        impl PacketRead for EmptyPacketDevice {
            fn read_packet(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }
        impl PacketWrite for EmptyPacketDevice {
            fn write_packet(&mut self, packet: &[u8]) -> std::io::Result<usize> {
                Ok(packet.len())
            }
        }

        let fixture = Fixture::new("daemon");
        let mut config = fixture.config.clone();
        config.network.listen_addresses.clear();
        config.network.discovery = crate::config::DiscoveryConfig {
            mdns: false,
            kademlia: false,
            autonat: false,
            dcutr: false,
            kademlia_provider_advertisement: false,
            ..crate::config::DiscoveryConfig::default()
        };
        config.network.relay.auto.max_reservations = 0;
        config.network.packet_plane.listen.clear();
        config.network.packet_plane.quic_listen.clear();
        let (control, receiver) = runtime_control_channel();
        let platform = RuntimePlatform::new(
            PacketIo::new(EmptyPacketDevice, EmptyPacketDevice),
            PreconfiguredTunRoutes,
        )
        .with_control(receiver);
        let path = fixture.path();
        let daemon = tokio::spawn(run_config_until_with_runtime_platform(
            config,
            platform,
            None,
            None,
            None,
            Some(path),
            std::future::pending(),
        ));
        tokio::time::timeout(Duration::from_secs(25), async {
            let initial = control.state().await.unwrap();
            assert!(initial.contains(&"checkpoint_sync_state resyncing".to_owned()));
            assert!(control.network_peers().await.unwrap().peers.is_empty());
            assert!(
                control
                    .revoke_member(Some(fixture.member.peer_id.clone()))
                    .await
                    .is_err()
            );
            loop {
                if control
                    .state()
                    .await
                    .unwrap()
                    .contains(&"checkpoint_sync_state participating".to_owned())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert_eq!(control.network_peers().await.unwrap().peers.len(), 2);
            let result = control
                .revoke_member(Some(fixture.member.peer_id.clone()))
                .await
                .unwrap();
            assert_eq!(result.member_peer, fixture.member.peer_id);
            assert!(!result.resigned);
            assert_eq!(result.membership_epoch, 1);
            let peers = control.network_peers().await.unwrap();
            assert_eq!(peers.peers.len(), 1);
            assert_eq!(peers.peers[0].peer_id, fixture.local.peer_id);
            assert!(
                !fs::read_to_string(fixture.path())
                    .unwrap()
                    .contains(&fixture.member.peer_id)
            );
            control.shutdown().await.unwrap();
            daemon.await.unwrap().unwrap();
        })
        .await
        .expect("checkpoint startup must complete without operator intervention");
    }

    #[tokio::test]
    async fn three_daemons_deliver_creator_departure_and_keep_survivor_governance() {
        use super::super::{
            control_socket::runtime_control_channel,
            runner::{RuntimePlatform, run_config_until_with_runtime_platform},
            tun::{PacketIo, PacketRead, PacketWrite},
        };

        struct EmptyPacketDevice;
        impl PacketRead for EmptyPacketDevice {
            fn read_packet(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Ok(0)
            }
        }
        impl PacketWrite for EmptyPacketDevice {
            fn write_packet(&mut self, packet: &[u8]) -> std::io::Result<usize> {
                Ok(packet.len())
            }
        }

        struct RoutesWithOneFailure {
            creator: bool,
            armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
            failures: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        impl super::super::runner::TunRouteController for RoutesWithOneFailure {
            fn reconcile(
                &mut self,
                _: &super::super::tun::TunRuntimeConfig,
                _: &super::super::tun::TunRuntimeConfig,
                _: &super::super::tun::TunRouteUpdate,
            ) -> Result<(), RunnerError> {
                if self.creator && self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    self.failures
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return Err(std::io::Error::other("injected route cleanup failure").into());
                }
                Ok(())
            }
        }
        let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failures = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut fixture = Fixture::new("three-daemon-departure");
        let mut allocated = fixture
            .state
            .snapshot()
            .payload
            .members
            .iter()
            .map(|member| {
                crate::route::builtin_ipv4(crate::PeerId::from_libp2p(
                    member.subject.peer_id.parse().unwrap(),
                ))
            })
            .collect::<HashSet<_>>();
        let third = unused_ipv4_identity(&mut allocated);
        let admission = fixture
            .state
            .sign_mutation_at(
                &fixture.local,
                MembershipChange::UpsertMember(CheckpointMember::new(&third).unwrap()),
                WALL_NOW,
            )
            .unwrap();
        fixture
            .state
            .apply_mutation_at(&admission, WALL_NOW)
            .unwrap();
        let identities = [fixture.local.clone(), fixture.member.clone(), third];
        let listeners = identities
            .iter()
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
            .collect::<Vec<_>>();
        let addresses = listeners
            .iter()
            .map(|listener| {
                format!(
                    "/ip4/127.0.0.1/tcp/{}",
                    listener.local_addr().unwrap().port()
                )
            })
            .collect::<Vec<_>>();
        let mut setups = Vec::new();
        let mut controls = Vec::new();
        let mut paths = Vec::new();
        for (index, identity) in identities.iter().enumerate() {
            let directory = fixture.directory.join(format!("node-{index}"));
            fs::create_dir(&directory).unwrap();
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let path = directory.join("membership-state.json");
            MembershipStateStore::new(path.clone())
                .save_checkpoint(
                    "lab",
                    &identity.peer_id,
                    &fixture.credentials,
                    &fixture.state.retained(),
                )
                .unwrap();
            let peers = identities
                .iter()
                .enumerate()
                .filter(|(peer_index, _)| *peer_index != index)
                .map(|(peer_index, peer)| {
                    // Dial each undirected edge once; both ends still authorize the full roster.
                    let addresses = if index < peer_index {
                        vec![addresses[peer_index].clone()]
                    } else {
                        vec![]
                    };
                    serde_json::json!({"id": peer.peer_id, "addresses": addresses})
                })
                .collect::<Vec<_>>();
            let mut config: Config = serde_json::from_value(serde_json::json!({
                "network": {
                    "name": "lab", "local_peer": identity.peer_id,
                    "private_key": identity.private_key,
                    "listen_addresses": [addresses[index]],
                },
                "peers": peers,
            }))
            .unwrap();
            config.network.discovery = crate::config::DiscoveryConfig {
                mdns: false,
                kademlia: false,
                autonat: false,
                dcutr: false,
                kademlia_provider_advertisement: false,
                ..crate::config::DiscoveryConfig::default()
            };
            config.network.relay.auto.max_reservations = 0;
            config.network.packet_plane.listen.clear();
            config.network.packet_plane.quic_listen.clear();
            let (control, receiver) = runtime_control_channel();
            let platform = RuntimePlatform::new(
                PacketIo::new(EmptyPacketDevice, EmptyPacketDevice),
                RoutesWithOneFailure {
                    creator: index == 0,
                    armed: std::sync::Arc::clone(&armed),
                    failures: std::sync::Arc::clone(&failures),
                },
            )
            .with_control(receiver);
            setups.push((config, platform, path.clone()));
            controls.push(control);
            paths.push(path);
        }
        drop(listeners);
        let mut daemons = Vec::new();
        let mut shutdowns = Vec::new();
        for (config, platform, path) in setups {
            let (shutdown, receiver) = tokio::sync::oneshot::channel();
            shutdowns.push(shutdown);
            daemons.push(tokio::spawn(run_config_until_with_runtime_platform(
                config,
                platform,
                None,
                None,
                None,
                Some(path),
                async {
                    let _ = receiver.await;
                    super::super::runner::ShutdownReason::ControlSocket
                },
            )));
        }

        let mut scenario = tokio::spawn(async move {
            let started = Instant::now();
            loop {
                let mut ready = true;
                let mut diagnostics = Vec::new();
                for (index, control) in controls.iter().enumerate() {
                    let state = control.state().await.unwrap();
                    diagnostics.push(
                        state
                            .iter()
                            .filter(|line| {
                                line.starts_with("checkpoint_sync_state ")
                                    || line.starts_with("connected_overlay_peers ")
                                    || line.starts_with("peer path state: ")
                            })
                            .cloned()
                            .collect::<Vec<_>>(),
                    );
                    ready &= state.contains(&"checkpoint_sync_state participating".to_owned());
                    ready &= state.contains(&"connected_overlay_peers 2".to_owned());
                    for (peer_index, peer) in identities.iter().enumerate() {
                        if peer_index == index {
                            continue;
                        }
                        ready &= state.iter().any(|line| {
                            line.starts_with(&format!(
                                "peer path state: {} ",
                                crate::PeerId::from_libp2p(peer.peer_id.parse().unwrap())
                            )) && line.contains("healthy true ")
                                && !line.contains("established_connections 0 ")
                        });
                    }
                }
                if ready {
                    break;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(30),
                    "daemon readiness: {diagnostics:?}"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            armed.store(true, std::sync::atomic::Ordering::SeqCst);
            let departure = controls[0].revoke_member(None).await.unwrap();
            assert!(departure.resigned);
            assert_eq!(failures.load(std::sync::atomic::Ordering::SeqCst), 1);
            assert_eq!(departure.membership_epoch, 2);
            assert!(controls[0].network_peers().await.unwrap().peers.is_empty());
            loop {
                let state = controls[0].state().await.unwrap();
                if state.contains(&"checkpoint_handoff_active 0".to_owned()) {
                    assert!(
                        state.contains(&"checkpoint_handoff_recipients 2".to_owned()),
                        "{state:?}"
                    );
                    assert!(
                        state.contains(&"checkpoint_handoff_acknowledged 2".to_owned()),
                        "{state:?}"
                    );
                    assert!(state.contains(&"checkpoint_handoff_failed 0".to_owned()));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            for (index, control) in controls.iter().enumerate().skip(1) {
                let peers = control.network_peers().await.unwrap().peers;
                assert_eq!(peers.len(), 2);
                assert!(
                    peers
                        .iter()
                        .all(|peer| peer.peer_id != identities[0].peer_id)
                );
                let retained = fs::read_to_string(&paths[index]).unwrap();
                assert!(!retained.contains(&identities[0].peer_id));
                assert!(!retained.contains("revocation"));
                assert!(!retained.contains("inviter"));
            }
            loop {
                if controls[0]
                    .state()
                    .await
                    .unwrap()
                    .contains(&"checkpoint_route_cleanup_pending 0".to_owned())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            // A survivor's advertisement may have started a live refresh even
            // though the exact-base departure was durably applied and ACKed.
            let ready = Instant::now();
            loop {
                if controls[1]
                    .state()
                    .await
                    .unwrap()
                    .contains(&"checkpoint_sync_state participating".to_owned())
                {
                    break;
                }
                assert!(
                    ready.elapsed() < Duration::from_secs(20),
                    "survivor did not finish live refresh"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let removal = controls[1]
                .revoke_member(Some(identities[2].peer_id.clone()))
                .await
                .unwrap();
            assert!(!removal.resigned);
            assert_eq!(removal.membership_epoch, 3);
            assert_eq!(controls[1].network_peers().await.unwrap().peers.len(), 1);
            let retained = fs::read_to_string(&paths[1]).unwrap();
            assert!(!retained.contains(&identities[0].peer_id));
            assert!(!retained.contains(&identities[2].peer_id));
        });
        // Always stop daemons before removing fixture files, including failed assertions.
        let result = match tokio::time::timeout(Duration::from_secs(60), &mut scenario).await {
            Ok(result) => Ok(result),
            Err(error) => {
                scenario.abort();
                let _ = scenario.await;
                Err(error)
            }
        };
        for shutdown in shutdowns {
            let _ = shutdown.send(());
        }
        let mut outcomes = Vec::new();
        for mut daemon in daemons {
            match tokio::time::timeout(Duration::from_secs(5), &mut daemon).await {
                Ok(outcome) => outcomes.push(Some(outcome)),
                Err(_) => {
                    daemon.abort();
                    let _ = daemon.await;
                    outcomes.push(None);
                }
            }
        }
        for outcome in outcomes {
            outcome
                .expect("checkpoint daemon failed to shut down")
                .unwrap()
                .unwrap();
        }
        result
            .expect("three-node checkpoint handoff must converge")
            .unwrap();
    }

    #[test]
    fn stale_responses_and_transport_failures_release_owned_assembly_immediately() {
        use libp2p::swarm::ConnectionId;

        use super::super::control::checkpoint::{
            CHECKPOINT_TRANSFER_VERSION, CheckpointPage, MAX_PAGE_BYTES,
        };

        for stale in [false, true] {
            let fixture = Fixture::new(if stale {
                "stale-response"
            } else {
                "transport-failure"
            });
            let (mut runtime, _) = fixture.restored();
            let peer = fixture.member.peer_id.parse().unwrap();
            let now = Instant::now();
            runtime.begin_resync(now).unwrap();
            let request = runtime
                .transfer
                .start_request(
                    peer,
                    runtime.challenge().unwrap().clone(),
                    runtime.credentials.secret(),
                    now,
                    now + RESYNC_WINDOW,
                )
                .unwrap();
            let response = CheckpointPageResponse::Page(CheckpointPage {
                version: CHECKPOINT_TRANSFER_VERSION,
                challenge: request.challenge.clone(),
                offer_digest: [29; 32],
                cursor: 0,
                total_bytes: u32::try_from(MAX_PAGE_BYTES * 2).unwrap(),
                bytes: vec![1; MAX_PAGE_BYTES],
            });
            let CheckpointProgress::More(next) = runtime
                .transfer
                .accept_response(peer, &request, response, runtime.credentials.secret(), now)
                .unwrap()
            else {
                panic!("two-page transfer must remain pending");
            };
            let mut behaviour = super::super::control::checkpoint::behaviour(4);
            let request_id = behaviour.send_request(&peer, next.clone());
            runtime.requests.insert(request_id, (peer, next.clone()));
            assert_eq!(runtime.transfer.stats().buffered_bytes, MAX_PAGE_BYTES * 2);
            if stale {
                runtime.discard_wire_event(
                    request_response::Event::Message {
                        peer,
                        connection_id: ConnectionId::new_unchecked(1),
                        message: Message::Response {
                            request_id,
                            response: CheckpointPageResponse::rejected(
                                &next,
                                CheckpointRejection::Expired,
                            ),
                        },
                    },
                    now,
                );
            } else {
                runtime.retire_outbound(request_id, now);
            }
            assert!(runtime.requests.is_empty());
            assert_eq!(runtime.transfer.stats().buffered_bytes, 0);
            assert_eq!(runtime.transfer.stats().retired_sessions, 1);
            assert_eq!(runtime.transfer_failures, 1);
            runtime.retire_outbound(request_id, now);
            assert_eq!(runtime.transfer_failures, 1);
            runtime.transfer.cleanup(now + RESYNC_WINDOW);
            assert_eq!(runtime.transfer.stats().retired_sessions, 0);
        }
    }

    #[test]
    fn hostname_changes_are_durable_and_do_not_change_authority_revision() {
        let fixture = Fixture::new("hostname");
        let (mut runtime, mut forwarder) = fixture.participating();
        let revision = runtime.state().snapshot().payload.authority_revision;
        let mut config = forwarder.config().clone();
        config.network.dns.enabled = true;
        config.network.dns.hostname = Some("new-device".into());
        let update = forwarder.prepare_reconfigure(config, WALL_NOW).unwrap();
        forwarder.try_commit_reconfigure(update).unwrap();
        assert!(
            runtime
                .reconcile_local_hostname(&fixture.local, &fixture.store, &mut forwarder, WALL_NOW)
                .unwrap()
        );
        assert!(
            !runtime
                .reconcile_local_hostname(&fixture.local, &fixture.store, &mut forwarder, WALL_NOW)
                .unwrap()
        );
        assert_eq!(
            runtime.state().snapshot().payload.authority_revision,
            revision
        );
        let peer = crate::PeerId::from_libp2p(fixture.local.peer_id.parse().unwrap());
        let restored = fixture.restored().0;
        assert_eq!(
            restored.state().hostname_claims()[0].payload.hostname,
            "new-device"
        );
        assert!(
            forwarder
                .effective_hostname_records()
                .unwrap()
                .get(&peer)
                .is_some_and(|name| name == "new-device")
        );
        assert!(
            !fs::read_to_string(fixture.path())
                .unwrap()
                .contains("inviter")
        );
    }
}
