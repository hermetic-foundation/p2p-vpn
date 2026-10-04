//! Explicit Linux migration, serialized by the daemon's membership owner.

use std::{collections::BTreeMap, io};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::{
    PeerId,
    config::RouteConfig,
    dns::canonical_dns_label,
    identity::NodeIdentity,
    membership::{
        MembershipRole,
        checkpoint::{
            CheckpointMember, CooperativeMembershipState, SnapshotPolicy, SnapshotPublisher,
            migration::{
                AuthorizedMigrationOptions, MigrationHostname, SignedLegacyMigrationSeed,
                validate_legacy_roles,
            },
        },
        effective_membership_at,
    },
    route::{builtin_ipv4, builtin_ipv6},
};

use super::{
    checkpoint_runtime::CheckpointRuntime,
    forward::Forwarder,
    membership_store::{
        MembershipStateStore,
        checkpoint::{CheckpointCredentials, LoadedCheckpointAuthority, PersistedAuthority},
        migration::{MigrationArtifact, MigrationArtifactStore},
    },
    runner::RunnerError,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum MembershipMigrationRequest {
    Prepare {},
    Inspect {},
    Install { accept_id: String },
    Cancel { accept_id: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipMigrationPhase {
    Absent,
    Prepared,
    Expired,
    Installed,
    CleanupPending,
}

/// Public constant-sized summary. No capability, private key, or old roster.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipMigrationStatus {
    pub schema_version: u8,
    pub network_name: String,
    pub phase: MembershipMigrationPhase,
    pub artifact_path: String,
    pub migration_id: Option<String>,
    pub expires_at_unix_seconds: Option<u64>,
    pub active_members: usize,
    pub current_hostnames: usize,
    pub route_grants: usize,
    pub publisher_peer: Option<String>,
}

fn failure(message: &str) -> RunnerError {
    io::Error::other(message).into()
}

fn checkpoint_error(error: crate::membership::checkpoint::CheckpointError) -> RunnerError {
    super::forward::ForwardError::Checkpoint(error).into()
}

pub(crate) fn inspect(
    store: &MembershipStateStore,
    forwarder: &Forwarder,
    identity: &NodeIdentity,
    now: u64,
) -> Result<MembershipMigrationStatus, RunnerError> {
    let network = &forwarder.config().network.name;
    let secret = forwarder.config().membership_key_bytes()?;
    let artifacts = MigrationArtifactStore::for_authority(store);
    let authority = store.load_authority(network, &identity.peer_id, None, secret.as_deref())?;
    let artifact = artifacts.load(network, secret.as_deref())?;
    let mut result = summary(&artifacts, network, artifact.as_ref())?;
    result.phase = match (&authority, &artifact) {
        (Some(PersistedAuthority::Checkpoint(_)), _) => MembershipMigrationPhase::Installed,
        (_, Some(artifact)) if artifact.seed.payload.expires_at_unix_seconds <= now => {
            MembershipMigrationPhase::Expired
        }
        (_, Some(_)) => MembershipMigrationPhase::Prepared,
        _ => MembershipMigrationPhase::Absent,
    };
    if let Some(PersistedAuthority::Checkpoint(loaded)) = authority {
        if let Some(artifact) = &artifact
            && artifact.credentials.anchor() != loaded.credentials.anchor()
        {
            return Err(failure(
                "migration artifact conflicts with installed authority",
            ));
        }
        result.active_members = loaded.retained.snapshot.payload.members.len();
        result.current_hostnames = loaded.retained.hostname_claims.len();
        result.route_grants = loaded
            .retained
            .snapshot
            .payload
            .members
            .iter()
            .map(|member| member.route_grants.len())
            .sum();
    }
    Ok(result)
}

fn summary(
    store: &MigrationArtifactStore,
    network: &str,
    artifact: Option<&MigrationArtifact>,
) -> Result<MembershipMigrationStatus, RunnerError> {
    Ok(MembershipMigrationStatus {
        schema_version: 1,
        network_name: network.to_owned(),
        phase: MembershipMigrationPhase::Absent,
        artifact_path: store.path().to_string_lossy().into_owned(),
        migration_id: artifact
            .map(|item| item.seed.migration_id())
            .transpose()
            .map_err(checkpoint_error)?,
        expires_at_unix_seconds: artifact.map(|item| item.seed.payload.expires_at_unix_seconds),
        active_members: artifact.map_or(0, |item| item.seed.payload.snapshot.payload.members.len()),
        current_hostnames: artifact.map_or(0, |item| item.seed.payload.hostnames.len()),
        route_grants: artifact.map_or(0, |item| {
            item.seed
                .payload
                .snapshot
                .payload
                .members
                .iter()
                .map(|member| member.route_grants.len())
                .sum()
        }),
        publisher_peer: artifact.map(|item| item.seed.payload.publisher.peer_id.clone()),
    })
}

pub(crate) fn prepare(
    store: &MembershipStateStore,
    forwarder: &Forwarder,
    identity: &NodeIdentity,
    now: u64,
) -> Result<MembershipMigrationStatus, RunnerError> {
    let network = &forwarder.config().network.name;
    let secret = forwarder.config().membership_key_bytes()?;
    if matches!(
        store.load_authority(network, &identity.peer_id, None, secret.as_deref())?,
        Some(PersistedAuthority::Checkpoint(_))
    ) || forwarder.checkpoint_sync_state().is_some()
    {
        return Err(failure(
            "checkpoint authority already installed; preparation cannot replace it",
        ));
    }
    let artifacts = MigrationArtifactStore::for_authority(store);
    let artifact = if let Some(previous) = artifacts.load(network, secret.as_deref())? {
        previous.verify_at(network, now)?;
        validate_current_projection(&previous, forwarder, identity, now, true)?;
        previous
    } else {
        let generated = CheckpointCredentials::generate()?;
        let credentials = if let Some(secret) = secret {
            CheckpointCredentials::new(generated.anchor().clone(), secret)?
        } else {
            generated
        };
        let (members, hostnames) = current_projection(forwarder, identity, &credentials, now)?;
        let seed = SignedLegacyMigrationSeed::prepare_authorized_at(
            credentials.capability()?,
            identity,
            AuthorizedMigrationOptions {
                network_name: network,
                members,
                hostnames,
                policy: SnapshotPolicy::default(),
            },
            now,
        )
        .map_err(checkpoint_error)?;
        MigrationArtifact { credentials, seed }
    };
    artifacts.save(&artifact, network, now)?;
    let mut result = summary(&artifacts, network, Some(&artifact))?;
    result.phase = MembershipMigrationPhase::Prepared;
    Ok(result)
}

pub(crate) fn cancel(
    store: &MembershipStateStore,
    forwarder: &Forwarder,
    accept_id: &str,
) -> Result<MembershipMigrationStatus, RunnerError> {
    let network = &forwarder.config().network.name;
    let artifacts = MigrationArtifactStore::for_authority(store);
    let secret = forwarder.config().membership_key_bytes()?;
    let artifact = artifacts
        .load(network, secret.as_deref())?
        .ok_or_else(|| failure("no migration artifact is prepared"))?;
    check_id(&artifact, accept_id)?;
    artifacts.retire(&artifact, network)?;
    let mut result = summary(&artifacts, network, None)?;
    if forwarder.checkpoint_sync_state().is_some() {
        result.phase = MembershipMigrationPhase::Installed;
    }
    Ok(result)
}

fn check_id(artifact: &MigrationArtifact, accept_id: &str) -> Result<(), RunnerError> {
    if accept_id != artifact.seed.migration_id().map_err(checkpoint_error)? {
        return Err(failure(
            "migration fingerprint mismatch; inspect before accepting",
        ));
    }
    Ok(())
}

/// Persist then install a gated owner. A visible uncertain write also installs
/// that gate before returning its error, never leaving legacy grants live.
pub(crate) fn install(
    runtime: &mut Option<CheckpointRuntime>,
    store: &MembershipStateStore,
    forwarder: &mut Forwarder,
    identity: &NodeIdentity,
    accept_id: &str,
    now: u64,
) -> Result<MembershipMigrationStatus, RunnerError> {
    let network = forwarder.config().network.name.clone();
    install_with(
        runtime,
        store,
        forwarder,
        identity,
        accept_id,
        now,
        |credentials, retained| {
            store.save_checkpoint(&network, &identity.peer_id, credentials, retained)
        },
    )
}

fn install_with(
    runtime: &mut Option<CheckpointRuntime>,
    store: &MembershipStateStore,
    forwarder: &mut Forwarder,
    identity: &NodeIdentity,
    accept_id: &str,
    now: u64,
    save: impl FnOnce(
        &CheckpointCredentials,
        &crate::membership::checkpoint::RetainedCheckpointState,
    ) -> Result<(), super::membership_store::MembershipStateStoreError>,
) -> Result<MembershipMigrationStatus, RunnerError> {
    let network = forwarder.config().network.name.clone();
    let secret = forwarder.config().membership_key_bytes()?;
    let artifacts = MigrationArtifactStore::for_authority(store);
    let artifact = artifacts
        .load(&network, secret.as_deref())?
        .ok_or_else(|| failure("no migration artifact is prepared"))?;
    check_id(&artifact, accept_id)?;
    let previous = store.load_authority(
        &network,
        &identity.peer_id,
        Some(artifact.credentials.anchor()),
        Some(artifact.credentials.secret()),
    )?;
    let loaded = if let Some(PersistedAuthority::Checkpoint(loaded)) = previous {
        if loaded.enrollment_floor.is_some() {
            return Err(failure("pairing activation pending"));
        }
        // Reconfirm durability of the selected state, never reinstall genesis.
        save(&loaded.credentials, &loaded.retained)?;
        *loaded
    } else {
        if runtime.is_some() || forwarder.checkpoint_sync_state().is_some() {
            return Err(failure(
                "runtime and durable authority disagree; restart safely",
            ));
        }
        artifact.verify_at(&network, now)?;
        validate_current_projection(&artifact, forwarder, identity, now, false)?;
        let state = artifact
            .seed
            .instantiate_at(artifact.credentials.capability()?, identity, &network, now)
            .map_err(checkpoint_error)?;
        // Projection validation precedes the irreversible replacement.
        forwarder.prepare_checkpoint_update(&state, artifact.credentials.anchor(), now)?;
        let retained = state.retained();
        if let Err(error) = save(&artifact.credentials, &retained) {
            if matches!(
                error,
                super::membership_store::MembershipStateStoreError::CheckpointDurabilityUncertain(
                    _
                )
            ) {
                let visible = store.load_authority(
                    &network,
                    &identity.peer_id,
                    Some(artifact.credentials.anchor()),
                    Some(artifact.credentials.secret()),
                )?;
                if let Some(PersistedAuthority::Checkpoint(loaded)) = visible {
                    install_loaded(runtime, forwarder, identity, *loaded, now)?;
                } else {
                    return Err(failure(
                        "uncertain checkpoint replacement is not recoverable",
                    ));
                }
            }
            return Err(error.into());
        }
        LoadedCheckpointAuthority {
            credentials: artifact.credentials.clone(),
            retained,
            enrollment_floor: None,
        }
    };
    install_loaded(runtime, forwarder, identity, loaded, now)?;
    let mut result = summary(&artifacts, &network, Some(&artifact))?;
    let selected = runtime.as_ref().expect("migration installed owner").state();
    result.active_members = selected.snapshot().payload.members.len();
    result.current_hostnames = selected.hostname_claims().len();
    result.route_grants = selected
        .snapshot()
        .payload
        .members
        .iter()
        .map(|member| member.route_grants.len())
        .sum();
    result.phase = match artifacts.retire(&artifact, &network) {
        Ok(_) => MembershipMigrationPhase::Installed,
        Err(_) => MembershipMigrationPhase::CleanupPending,
    };
    Ok(result)
}

fn install_loaded(
    runtime: &mut Option<CheckpointRuntime>,
    forwarder: &mut Forwarder,
    identity: &NodeIdentity,
    loaded: LoadedCheckpointAuthority,
    now: u64,
) -> Result<(), RunnerError> {
    let mut owner = CheckpointRuntime::restore(
        forwarder.config().network.name.clone(),
        &identity.peer_id,
        loaded,
    )?;
    let update = forwarder.prepare_checkpoint_update(owner.state(), owner.anchor(), now)?;
    owner.begin_resync(std::time::Instant::now())?;
    forwarder.commit_checkpoint_update(update)?;
    *runtime = Some(owner);
    Ok(())
}

/// Only the daemon-owned handoff path is retired; arbitrary user files are never deleted.
pub(crate) fn retire_due(
    store: &MembershipStateStore,
    forwarder: &Forwarder,
    identity: &NodeIdentity,
    now: u64,
) -> Result<bool, RunnerError> {
    let network = &forwarder.config().network.name;
    let secret = forwarder.config().membership_key_bytes()?;
    let artifacts = MigrationArtifactStore::for_authority(store);
    let Some(artifact) = artifacts.load(network, secret.as_deref())? else {
        return Ok(false);
    };
    let installed = matches!(
        store.load_authority(
            network,
            &identity.peer_id,
            Some(artifact.credentials.anchor()),
            Some(artifact.credentials.secret())
        )?,
        Some(PersistedAuthority::Checkpoint(_))
    );
    if installed || now >= artifact.seed.payload.expires_at_unix_seconds {
        return Ok(artifacts.retire(&artifact, network)?);
    }
    Ok(false)
}

fn validate_current_projection(
    artifact: &MigrationArtifact,
    forwarder: &Forwarder,
    identity: &NodeIdentity,
    now: u64,
    exact: bool,
) -> Result<(), RunnerError> {
    let (members, names) = current_projection(forwarder, identity, &artifact.credentials, now)?;
    let publisher = &artifact.seed.payload.publisher.peer_id;
    if !members
        .iter()
        .any(|member| &member.subject.peer_id == publisher)
    {
        return Err(failure("migration publisher is not currently authorized"));
    }
    let selected = &artifact.seed.payload.snapshot.payload;
    let effective = effective_membership_at(
        forwarder.member_records(),
        &forwarder.config().network.name,
        now,
    )
    .map_err(crate::config::ConfigError::from)?;
    for selected_member in &selected.members {
        let peer = selected_member
            .subject
            .peer_id
            .parse::<libp2p::PeerId>()
            .map_err(crate::config::ConfigError::Libp2pPeerId)?;
        if !effective.authorizes_configured_peer(PeerId::from_libp2p(peer)) {
            return Err(failure(
                "migration would restore a locally revoked or expired member",
            ));
        }
    }
    if exact && members.len() != selected.members.len() {
        return Err(failure(
            "membership changed after preparation; cancel and prepare again",
        ));
    }
    for member in &members {
        let Some(expected) = selected.member(&member.subject.peer_id) else {
            return Err(failure("migration omits a currently authorized member"));
        };
        if member.subject != expected.subject
            || member.expires_at_unix_seconds != expected.expires_at_unix_seconds
            || member
                .roles
                .iter()
                .any(|role| !expected.roles.contains(role))
            || member
                .route_grants
                .iter()
                .any(|route| !expected.route_grants.contains(route))
        {
            return Err(failure(
                "migration changes a current member identity, expiry, role, or route",
            ));
        }
        if exact && (member.roles != expected.roles || member.route_grants != expected.route_grants)
        {
            return Err(failure(
                "grants changed after preparation; cancel and prepare again",
            ));
        }
    }
    for name in &names {
        if !artifact.seed.payload.hostnames.contains(name) {
            return Err(failure(
                "current hostname differs from migration plan; prepare again",
            ));
        }
    }
    if exact && names != artifact.seed.payload.hostnames {
        return Err(failure(
            "hostnames changed after preparation; cancel and prepare again",
        ));
    }
    Ok(())
}

fn current_projection(
    forwarder: &Forwarder,
    identity: &NodeIdentity,
    credentials: &CheckpointCredentials,
    now: u64,
) -> Result<(Vec<CheckpointMember>, Vec<MigrationHostname>), RunnerError> {
    let config = forwarder.config();
    if config.identity()?.peer_id != identity.peer_id {
        return Err(failure("migration identity differs from runtime identity"));
    }
    let records = forwarder.member_records();
    validate_legacy_roles(records, &config.network.name, now).map_err(checkpoint_error)?;
    let legacy = CooperativeMembershipState::migrate_trusted_legacy_at(
        credentials.capability()?,
        identity.peer_id.clone(),
        records,
        &config.network.name,
        now,
    )
    .map_err(checkpoint_error)?;
    let effective = effective_membership_at(records, &config.network.name, now)
        .map_err(crate::config::ConfigError::from)?;
    if !effective.authorizes_configured_peer(config.local_peer_id()?) {
        return Err(failure("inactive local member cannot migrate authority"));
    }
    if forwarder
        .hostname_records()
        .iter()
        .any(|record| record.payload.issued_at_unix_seconds > now)
    {
        return Err(failure(
            "future hostname update must settle before migration",
        ));
    }
    let mut members: BTreeMap<String, CheckpointMember> = legacy
        .snapshot()
        .payload
        .members
        .iter()
        .map(|member| (member.subject.peer_id.clone(), member.clone()))
        .collect();
    members
        .entry(identity.peer_id.clone())
        .or_insert(CheckpointMember::new(identity).map_err(checkpoint_error)?);
    for peer in &config.peers {
        if !effective.authorizes_configured_peer(peer.peer_id()?) || members.contains_key(&peer.id)
        {
            continue;
        }
        let member = static_member(peer)?;
        members.insert(member.subject.peer_id.clone(), member);
    }
    let routes = config.compile_routes_with_member_records_at(records, now)?;
    for member in members.values_mut() {
        preserve_routes(member, &routes)?;
    }
    let signed_names = forwarder.effective_hostname_records()?;
    let mut names = Vec::new();
    for member in members.values() {
        let id = &member.subject.peer_id;
        let transport = id
            .parse::<libp2p::PeerId>()
            .map_err(crate::config::ConfigError::Libp2pPeerId)?;
        let label = signed_names
            .get(&PeerId::from_libp2p(transport))
            .map(String::as_str)
            .or_else(|| {
                effective
                    .overlay_members()
                    .find(|entry| entry.transport_peer.to_string() == *id)
                    .and_then(|entry| entry.hostnames.first().map(String::as_str))
            })
            .or_else(|| {
                if *id == identity.peer_id {
                    config.network.dns.hostname.as_deref()
                } else {
                    config
                        .peers
                        .iter()
                        .find(|peer| peer.id == *id)
                        .and_then(|peer| peer.name.as_deref())
                }
            });
        if let Some(label) = label {
            names.push(MigrationHostname {
                peer_id: id.clone(),
                hostname: canonical_dns_label(label)
                    .map_err(|_| failure("invalid current migration hostname"))?,
            });
        }
    }
    Ok((members.into_values().collect(), names))
}

fn static_member(peer: &crate::config::PeerConfig) -> Result<CheckpointMember, RunnerError> {
    let transport: libp2p::PeerId = peer
        .id
        .parse()
        .map_err(crate::config::ConfigError::Libp2pPeerId)?;
    // Default Ed25519 peer IDs embed their public key. Hashed static IDs need
    // a signed admission; they cannot be invented or silently dropped.
    if transport.as_ref().code() != 0 {
        return Err(failure(
            "static peer lacks an inline public key; obtain a signed admission before migration",
        ));
    }
    let public = libp2p::identity::PublicKey::try_decode_protobuf(transport.as_ref().digest())
        .map_err(|_| failure("invalid static peer public key"))?;
    if public.to_peer_id() != transport {
        return Err(failure("static peer key mismatch"));
    }
    Ok(CheckpointMember {
        subject: SnapshotPublisher {
            peer_id: transport.to_string(),
            public_key: STANDARD.encode(public.encode_protobuf()),
        },
        incarnation: [1; 32],
        roles: vec![MembershipRole::OverlayMember],
        route_grants: vec![],
        expires_at_unix_seconds: None,
    })
}

fn preserve_routes(
    member: &mut CheckpointMember,
    routes: &crate::route::RouteTable,
) -> Result<(), RunnerError> {
    let transport = member
        .subject
        .peer_id
        .parse::<libp2p::PeerId>()
        .map_err(crate::config::ConfigError::Libp2pPeerId)?;
    let peer = PeerId::from_libp2p(transport);
    member.route_grants = routes
        .routes()
        .iter()
        .filter(|route| route.owner == peer)
        .filter(|route| {
            !(route.metric == 0
                && ((route.prefix.address() == builtin_ipv4(peer)
                    && route.prefix.prefix_len() == 32)
                    || (route.prefix.address() == builtin_ipv6(peer)
                        && route.prefix.prefix_len() == 128)))
        })
        .map(|route| RouteConfig {
            prefix: route.prefix.to_string(),
            metric: route.metric,
        })
        .collect();
    for route in &mut member.route_grants {
        route.prefix =
            crate::membership::checkpoint::canonical_route(route).map_err(checkpoint_error)?;
    }
    member
        .route_grants
        .sort_by(|a, b| (&a.prefix, a.metric).cmp(&(&b.prefix, b.metric)));
    member.route_grants.dedup();
    if !member.route_grants.is_empty() && !member.roles.contains(&MembershipRole::RouteAuthority) {
        member.roles.push(MembershipRole::RouteAuthority);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
