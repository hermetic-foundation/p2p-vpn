//! Protected pairing transactions own activation until snapshot selection and finalization.

use super::*;
use crate::membership::checkpoint::{MembershipChange, MembershipSyncState};
use crate::runtime::pairing_sessions::{CheckpointJoinerOwnership, PairingEnrollment};

pub(super) fn pending_join(sessions: &CodePairingSessions) -> Option<PairingEnrollment> {
    sessions
        .enrollments()
        .find(|entry| {
            entry.role == PairingEnrollmentRole::Joiner
                && entry.response.payload.checkpoint.is_some()
                && entry.state != PairingEnrollmentState::Applied
        })
        .cloned()
}

pub(super) fn release_idle_barrier(
    owner: &mut CheckpointRuntime,
    sessions: &CodePairingSessions,
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    now: u64,
) -> Result<(), RunnerError> {
    if owner.pairing_activation_blocked() && pending_join(sessions).is_none() {
        install_owner(owner, forwarder, membership, false, now)?;
    }
    Ok(())
}

fn install_owner(
    owner: &mut CheckpointRuntime,
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    blocked: bool,
    now: u64,
) -> Result<(), RunnerError> {
    owner.set_pairing_activation_blocked(blocked, forwarder, now)?;
    membership.replace_from_forwarder(forwarder)?;
    membership.replace_checkpoint_sync_peers(owner)?;
    Ok(())
}

// An uncertain rename may have installed a newer scope. Never keep legacy/older grants live.
fn restore_visible(
    runtime: &mut Option<CheckpointRuntime>,
    store: &MembershipStateStore,
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    identity: &NodeIdentity,
    blocked: bool,
    now: u64,
) -> Result<(), RunnerError> {
    let config = forwarder.config();
    let secret = config.membership_key_bytes()?;
    if let Some(PersistedAuthority::Checkpoint(loaded)) = store.load_authority(
        &config.network.name,
        &identity.peer_id,
        None,
        secret.as_deref(),
    )? {
        let mut owner =
            CheckpointRuntime::restore(config.network.name.clone(), &identity.peer_id, *loaded)?;
        install_owner(&mut owner, forwarder, membership, blocked, now)?;
        *runtime = Some(owner);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn form_after_open(
    runtime: &mut Option<CheckpointRuntime>,
    store: &MembershipStateStore,
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    tun: &mut TunRuntimeConfig,
    routes: &mut dyn TunRouteController,
    capabilities: &mut ControlCapabilities,
    identity: &NodeIdentity,
) -> Result<(), RunnerError> {
    if runtime.is_some() {
        return Ok(());
    }
    let now = current_unix_seconds_lossy();
    if !CheckpointRuntime::fresh_solo_eligible(forwarder.config(), identity, store, now)? {
        return Ok(()); // Existing legacy networks retain their existing pairing contract.
    }
    let result = CheckpointRuntime::form_new_network(forwarder.config(), identity, store, now);
    let mut owner = match result {
        Ok(owner) => owner,
        Err(error) => {
            restore_visible(runtime, store, forwarder, membership, identity, false, now)?;
            return Err(error);
        }
    };
    install_owner(&mut owner, forwarder, membership, false, now)?;
    *capabilities = refreshed_local_capabilities(capabilities, forwarder);
    owner.decorate_capabilities(capabilities)?;
    retry_checkpoint_tun_routes(&mut owner, forwarder, tun, routes);
    *runtime = Some(owner);
    Ok(())
}

pub(super) fn accept_joiner(
    context: &mut SwarmEventContext<'_>,
    swarm: &mut Swarm<Behaviour>,
    outbound: OutboundPairing,
    response: PairingResponse,
    wall_now: u64,
) -> Result<(), RunnerError> {
    let checkpoints = context.checkpoint_pairing.as_mut().ok_or_else(|| {
        io::Error::other("checkpoint pairing requires protected membership state")
    })?;
    let pairing_store = context
        .pairing_state_store
        .ok_or_else(|| io::Error::other("checkpoint pairing requires protected pairing state"))?;
    response.verify_for_offer_at(&outbound.offer, context.identity, wall_now)?;
    let grant = response
        .payload
        .checkpoint
        .as_ref()
        .expect("checkpoint acceptance");
    let network = context.forwarder.config().network.name.clone();
    if let Some(saved) = context
        .code_pairing_sessions
        .enrollment(&outbound.operation_id)
    {
        if saved.state == PairingEnrollmentState::Aborting
            || saved.response != response
            || saved.offer.as_ref() != Some(&outbound.offer)
        {
            return Err(CodePairingSessionError::Conflict.into());
        }
        if saved.state == PairingEnrollmentState::Applied {
            return Ok(());
        }
    } else {
        let secret = grant
            .secret_bytes()
            .map_err(crate::pairing::PairingError::from)?;
        let previous = checkpoints.store.load_authority(
            &network,
            &context.identity.peer_id,
            Some(&grant.anchor),
            Some(&secret),
        )?;
        if !matches!(&previous, Some(PersistedAuthority::Checkpoint(_)))
            && !CheckpointRuntime::fresh_solo_eligible(
                context.forwarder.config(),
                context.identity,
                checkpoints.store,
                wall_now,
            )?
        {
            return Err(io::Error::other(
                "joining an established legacy network requires explicit migration",
            )
            .into());
        }
        let ownership = match previous {
            Some(PersistedAuthority::Checkpoint(loaded))
                if loaded.enrollment_floor.is_none()
                    && loaded
                        .retained
                        .snapshot
                        .payload
                        .member(&context.identity.peer_id)
                        .is_some_and(|member| {
                            member.incarnation == grant.joiner.incarnation
                                && member.active_at(wall_now)
                        }) =>
            {
                CheckpointJoinerOwnership::Repair
            }
            _ => CheckpointJoinerOwnership::Fresh,
        };
        let mut anticipated = context.forwarder.config().clone();
        anticipated.network.routes = grant.joiner.route_grants.clone();
        anticipated.network.vpn_ip = response.payload.assigned_vpn_ip.clone();
        let anticipated_tun = TunRuntimeConfig::from_config_with_routes(&anticipated, &[])?;
        let cleanup =
            super::super::tun::PairingTunCleanup::capture(context.tun_runtime, &anticipated_tun)?;
        context.code_pairing_sessions.prepare_enrollment(
            &network,
            PairingEnrollmentPreparation {
                operation_id: outbound.operation_id.clone(),
                role: PairingEnrollmentRole::Joiner,
                approval_id: None,
                offer: Some(outbound.offer),
                response: response.clone(),
                transcript_sha256: outbound.transcript_sha256,
                membership_key_preconfigured: Some(
                    context.forwarder.config().network.membership_key.is_some(),
                ),
            },
        )?;
        context
            .code_pairing_sessions
            .record_checkpoint_joiner_ownership(&outbound.operation_id, ownership)?;
        context
            .code_pairing_sessions
            .record_tun_cleanup(&outbound.operation_id, cleanup)?;
    }
    persist_code_pairing_sessions(Some(pairing_store), context.code_pairing_sessions, &network)?;
    stage_prepared(
        checkpoints.runtime,
        checkpoints.store,
        context.code_pairing_sessions,
        context.forwarder,
        context.membership,
        context.identity,
        wall_now,
    )?;
    if let Some(owner) = checkpoints.runtime.as_mut() {
        *context.local_capabilities =
            refreshed_local_capabilities(context.local_capabilities, context.forwarder);
        owner.decorate_capabilities(context.local_capabilities)?;
        retry_checkpoint_tun_routes(
            owner,
            context.forwarder,
            context.tun_runtime,
            context.route_controller,
        );
        send_sync_capabilities(swarm, owner, context.local_capabilities, context.metrics);
        owner.drive_sync(swarm, context.peer_capabilities, Instant::now())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn stage_prepared(
    runtime: &mut Option<CheckpointRuntime>,
    store: &MembershipStateStore,
    sessions: &CodePairingSessions,
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    identity: &NodeIdentity,
    wall_now: u64,
) -> Result<(), RunnerError> {
    let Some(entry) = pending_join(sessions) else {
        return Ok(());
    };
    let ownership = entry
        .checkpoint_joiner_ownership
        .ok_or_else(|| io::Error::other("checkpoint joiner is missing installation ownership"))?;
    let offer = entry
        .offer
        .as_ref()
        .ok_or(CodePairingSessionError::Conflict)?;
    let accepted_at = entry.response.payload.issued_at_unix_seconds;
    // Only a previously protected Prepared transcript may bypass current handshake expiry.
    entry
        .response
        .verify_for_offer_at(offer, identity, accepted_at)?;
    if entry.state == PairingEnrollmentState::Prepared {
        sessions.validate_prepared_recovery(&forwarder.config().network.name, &entry)?;
    }
    let grant = entry
        .response
        .payload
        .checkpoint
        .as_ref()
        .expect("checkpoint transcript");
    if let Some(owner) = runtime.as_mut() {
        if owner.anchor() != &grant.anchor {
            return Err(io::Error::other("checkpoint pairing anchor changed").into());
        }
        let secret = grant
            .secret_bytes()
            .map_err(crate::pairing::PairingError::from)?;
        let Some(PersistedAuthority::Checkpoint(loaded)) = store.load_authority(
            &forwarder.config().network.name,
            &identity.peer_id,
            Some(&grant.anchor),
            Some(&secret),
        )?
        else {
            return Err(
                io::Error::other("checkpoint pairing requires its durable authority").into(),
            );
        };
        install_owner(owner, forwarder, membership, true, wall_now)?;
        if ownership == CheckpointJoinerOwnership::Released {
            return Ok(());
        }
        let covers_grant = match loaded.enrollment_floor {
            Some(floor) => floor >= grant.minimum,
            None => {
                owner
                    .state()
                    .snapshot()
                    .payload
                    .rank()
                    .map_err(ForwardError::Checkpoint)?
                    >= grant.minimum
            }
        };
        if covers_grant {
            return Ok(());
        }
    }
    if ownership == CheckpointJoinerOwnership::Released {
        return Err(io::Error::other(
            "released checkpoint cleanup requires its restored authority",
        )
        .into());
    }
    let result = CheckpointRuntime::stage_pairing_enrollment_from_solo(
        forwarder.config(),
        identity,
        offer,
        &entry.response,
        store,
        accepted_at,
    );
    let mut owner = match result {
        Ok(owner) => owner,
        Err(error) => {
            restore_visible(
                runtime, store, forwarder, membership, identity, true, wall_now,
            )?;
            return Err(error);
        }
    };
    install_owner(&mut owner, forwarder, membership, true, wall_now)?;
    *runtime = Some(owner);
    Ok(())
}

pub(super) fn send_sync_capabilities(
    swarm: &mut Swarm<Behaviour>,
    owner: &CheckpointRuntime,
    capabilities: &ControlCapabilities,
    metrics: &RuntimeMetrics,
) {
    for peer in swarm.connected_peers().copied().collect::<Vec<_>>() {
        if owner
            .state()
            .snapshot()
            .payload
            .member(&peer.to_string())
            .is_some()
        {
            swarm
                .behaviour_mut()
                .control
                .send_request(&peer, ControlRequest::Capabilities(capabilities.clone()));
            metrics.record_control_request_sent();
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_joiner(
    owner: &mut CheckpointRuntime,
    sessions: &mut CodePairingSessions,
    store: Option<&PairingStateStore>,
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    tun: &mut TunRuntimeConfig,
    routes: &mut dyn TunRouteController,
    identity: &NodeIdentity,
    wall_now: u64,
) -> Result<bool, RunnerError> {
    if pending_join(sessions).is_none() && !sessions.checkpoint_completion_uncertain() {
        return Ok(false);
    }
    let network = forwarder.config().network.name.clone();
    let protected =
        store.ok_or_else(|| io::Error::other("checkpoint completion requires protected state"))?;
    sessions.reconcile_checkpoint_completion_with::<RunnerError>(
        &network,
        wall_now,
        || protected.load().map_err(Into::into),
        |bytes| protected.save(bytes).map_err(Into::into),
    )?;
    if pending_join(sessions).is_none() {
        let was_blocked = owner.pairing_activation_blocked();
        release_idle_barrier(owner, sessions, forwarder, membership, wall_now)?;
        return Ok(was_blocked);
    }
    let Some(entry) = pending_join(sessions) else {
        return Ok(false);
    };
    if entry.state != PairingEnrollmentState::Prepared
        || owner.enrollment_pending()
        || !owner.pairing_remote_ready()
        || !matches!(
            owner.state().sync_state(),
            MembershipSyncState::Participating | MembershipSyncState::Excluded
        )
    {
        return Ok(false);
    }
    let grant = entry
        .response
        .payload
        .checkpoint
        .as_ref()
        .expect("checkpoint transcript");
    if owner
        .state()
        .snapshot()
        .payload
        .rank()
        .map_err(ForwardError::Checkpoint)?
        < grant.minimum
    {
        return Ok(false);
    }
    let selected = owner.state().snapshot().payload.member(&identity.peer_id);
    if owner.anchor() != &grant.anchor
        || selected.is_none_or(|member| {
            member.incarnation != grant.joiner.incarnation
                || member.subject != grant.joiner.subject
                || !member.active_at(wall_now)
        })
    {
        sessions.begin_enrollment_abort(&entry.operation_id)?;
        sessions.fail_join(
            &entry.operation_id,
            "checkpoint enrollment was removed or superseded",
        );
        persist_code_pairing_sessions(store, sessions, &network)?;
        return Err(io::Error::other(
            "checkpoint enrollment was removed or superseded; pairing remains gated",
        )
        .into());
    }
    // Names remain local configuration and are reconciled through self-signed claims.
    let config = forwarder.config().clone();
    let projection = forwarder.prepare_checkpoint_reconfigure(
        config,
        owner.state(),
        owner.anchor(),
        wall_now,
    )?;
    let next_tun = TunRuntimeConfig::from_config_with_routes(
        projection.config(),
        projection.authorized_routes(),
    )?;
    // Persist cleanup before the first command, never amend it after Aborting.
    sessions.record_tun_cleanup(
        &entry.operation_id,
        super::super::tun::PairingTunCleanup::capture(tun, &next_tun)?,
    )?;
    persist_code_pairing_sessions(store, sessions, &network)?;
    let update = next_tun.pairing_reconciliation_from(tun)?;
    routes.reconcile(tun, &next_tun, &update)?;
    // Completion is prepared separately, so failed final writes preserve the cancelable transaction.
    sessions.finish_checkpoint_join_with::<RunnerError>(
        &entry.operation_id,
        &network,
        wall_now,
        |bytes| protected.save(bytes).map_err(Into::into),
        || protected.load().map_err(Into::into),
    )?;
    forwarder.commit_checkpoint_update(projection)?;
    owner.set_pairing_activation_blocked(false, forwarder, wall_now)?;
    membership.replace_from_forwarder(forwarder)?;
    membership.replace_checkpoint_sync_peers(owner)?;
    *tun = next_tun;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cleanup_joiner_abort(
    owner: Option<&mut CheckpointRuntime>,
    entry: &PairingEnrollment,
    checkpoint_store: Option<&MembershipStateStore>,
    sessions: &mut CodePairingSessions,
    pairing_store: Option<&PairingStateStore>,
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    identity: &NodeIdentity,
    recipients: &[Libp2pPeerId],
) -> Result<(), RunnerError> {
    let ownership = entry
        .checkpoint_joiner_ownership
        .ok_or_else(|| io::Error::other("checkpoint joiner abort lacks installation ownership"))?;
    if ownership == CheckpointJoinerOwnership::Released {
        return Ok(());
    }
    let owner = owner
        .ok_or_else(|| io::Error::other("checkpoint joiner abort requires restored authority"))?;
    let now = current_unix_seconds_lossy();
    install_owner(owner, forwarder, membership, true, now)?;
    if owner.enrollment_pending()
        || !matches!(
            owner.state().sync_state(),
            MembershipSyncState::Participating | MembershipSyncState::Excluded
        )
    {
        return Err(io::Error::other("checkpoint abort requires durable catch-up").into());
    }
    let grant = entry
        .response
        .payload
        .checkpoint
        .as_ref()
        .expect("checkpoint abort");
    if owner.anchor() != &grant.anchor || entry.response.payload.joiner_peer != identity.peer_id {
        return Err(io::Error::other("checkpoint joiner abort scope mismatch").into());
    }
    if ownership == CheckpointJoinerOwnership::Fresh
        && owner
            .state()
            .snapshot()
            .payload
            .member(&identity.peer_id)
            .is_some_and(|member| member.incarnation == grant.joiner.incarnation)
    {
        owner.apply_change_with_handoff(
            MembershipChange::RemoveMember(identity.peer_id.clone()),
            identity,
            checkpoint_store
                .ok_or_else(|| io::Error::other("checkpoint abort requires durable authority"))?,
            forwarder,
            recipients,
            Instant::now(),
            now,
        )?;
    }
    sessions.record_checkpoint_joiner_ownership(
        &entry.operation_id,
        CheckpointJoinerOwnership::Released,
    )?;
    persist_code_pairing_sessions(pairing_store, sessions, &forwarder.config().network.name)?;
    Ok(())
}
