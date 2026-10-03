//! Serialized checkpoint owner. Persist the selected authority before exposing it.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use libp2p::{
    PeerId, Swarm,
    request_response::{self, Message},
};

use crate::{
    identity::NodeIdentity,
    membership::checkpoint::{
        BranchSelection, CheckpointError, CooperativeMembershipState, MembershipChange,
        MembershipSyncState, NetworkAnchor, OfferOutcome, SignedHostnameClaim,
        SignedMembershipMutation, SignedSnapshotOffer, SnapshotChallenge,
    },
};

use super::{
    control::{
        ControlCapabilities, PeerCapabilities,
        checkpoint::{
            CheckpointCapabilities, CheckpointPageRequest, CheckpointPageResponse,
            CheckpointProgress, CheckpointRejection, CheckpointSync, CheckpointSyncError,
            CheckpointSyncLimits, MAX_TRANSFER_SESSIONS,
        },
    },
    forward::{ForwardError, Forwarder},
    membership_store::{
        MembershipStateStore, MembershipStateStoreError,
        checkpoint::{CheckpointCredentials, LoadedCheckpointAuthority},
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
    pub(crate) fn restore(
        network_name: String,
        local_peer: &str,
        loaded: LoadedCheckpointAuthority,
    ) -> Result<Self, RunnerError> {
        let credentials = loaded.credentials.clone();
        let state = loaded.restore(local_peer)?;
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
        })
    }

    pub(crate) fn state(&self) -> &CooperativeMembershipState {
        &self.state
    }

    pub(crate) fn anchor(&self) -> &NetworkAnchor {
        self.credentials.anchor()
    }

    #[cfg(test)]
    fn challenge(&self) -> Option<&SnapshotChallenge> {
        self.pending.as_ref().map(|pending| &pending.challenge)
    }

    pub(crate) fn last_selection(&self) -> Option<&BranchSelection> {
        self.last_selection.as_ref()
    }

    pub(crate) fn extend_status_lines(&self, lines: &mut Vec<String>) {
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
        Ok(())
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
    }

    pub(crate) fn drive_sync(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        peers: &PeerCapabilities,
        now: Instant,
    ) -> Result<(), RunnerError> {
        self.transfer.cleanup(now);
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
                || (!connected.is_empty() && (higher_advertisement || now >= self.next_resync)))
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
        let update = forwarder.prepare_checkpoint_update(&candidate, self.anchor(), wall_now)?;
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
        Ok(())
    }

    fn install_gated(
        &mut self,
        candidate: CooperativeMembershipState,
        forwarder: &mut Forwarder,
        wall_now: u64,
    ) -> Result<(), RunnerError> {
        self.state = CooperativeMembershipState::restore(
            self.credentials.capability()?,
            candidate.local_peer().to_owned(),
            candidate.retained(),
        )
        .map_err(core_error)?;
        self.pending = None;
        self.retire_resync(Instant::now());
        let update = forwarder.prepare_checkpoint_update(&self.state, self.anchor(), wall_now)?;
        forwarder.commit_checkpoint_update(update)?;
        Ok(())
    }

    pub(crate) fn prune_expired(
        &mut self,
        identity: &NodeIdentity,
        store: &MembershipStateStore,
        forwarder: &mut Forwarder,
        wall_now: u64,
    ) -> Result<bool, RunnerError> {
        if self.pending.is_some()
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
            let member = NodeIdentity::generate_ed25519().unwrap();
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
                CheckpointRuntime::restore("lab".into(), &self.local.peer_id, loaded).unwrap();
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
            CheckpointRuntime::restore("lab".into(), &publisher.peer_id, loaded).unwrap();
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
