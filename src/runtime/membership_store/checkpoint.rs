//! Versioned active-only storage. Ordinary provisioning/migration is a separate step.
//! Credentials and the selected snapshot share one owner-only atomic replacement.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};

use crate::membership::checkpoint::{
    CooperativeMembershipState, MAX_CAPABILITY_BYTES, MAX_SNAPSHOT_OFFER_BYTES, NetworkAnchor,
    NetworkCapability, RetainedCheckpointState, SnapshotRank,
};

use super::{
    LEGACY_MEMBERSHIP_STATE_VERSION, MEMBERSHIP_STATE_ENVELOPE_BYTES, MEMBERSHIP_STATE_VERSION,
    MembershipStateStore, MembershipStateStoreError, PersistedMembershipStateData, decode_legacy,
    state_version, validate_scope,
};

pub(crate) const CHECKPOINT_STATE_VERSION: u8 = 3;
pub(crate) const MAX_CHECKPOINT_STATE_BYTES: usize =
    MAX_SNAPSHOT_OFFER_BYTES + MEMBERSHIP_STATE_ENVELOPE_BYTES;
const MAX_ENCODED_CAPABILITY_BYTES: usize = MAX_CAPABILITY_BYTES.div_ceil(3) * 4;

/// Not serializable or printable by public diagnostics/config export.
#[derive(Clone)]
pub(crate) struct CheckpointCredentials {
    anchor: NetworkAnchor,
    secret: Vec<u8>,
}

impl fmt::Debug for CheckpointCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckpointCredentials")
            .field("anchor", &self.anchor)
            .finish_non_exhaustive()
    }
}

impl CheckpointCredentials {
    pub(crate) fn new(
        anchor: NetworkAnchor,
        secret: Vec<u8>,
    ) -> Result<Self, MembershipStateStoreError> {
        NetworkCapability::from_secret(anchor.clone(), Some(&secret))?;
        Ok(Self { anchor, secret })
    }

    /// Initial network formation only. Pairing must transfer this same scope.
    pub(crate) fn generate() -> Result<Self, MembershipStateStoreError> {
        let mut network_id = [0; 32];
        let mut secret = vec![0; 32];
        OsRng.fill_bytes(&mut network_id);
        OsRng.fill_bytes(&mut secret);
        Self::new(NetworkAnchor::new(network_id)?, secret)
    }

    pub(crate) fn anchor(&self) -> &NetworkAnchor {
        &self.anchor
    }

    pub(crate) fn secret(&self) -> &[u8] {
        &self.secret
    }

    pub(crate) fn capability(&self) -> Result<NetworkCapability, MembershipStateStoreError> {
        Ok(NetworkCapability::from_secret(
            self.anchor.clone(),
            Some(&self.secret),
        )?)
    }
}

#[derive(Debug)]
pub(crate) struct LoadedCheckpointAuthority {
    pub(crate) credentials: CheckpointCredentials,
    pub(crate) retained: RetainedCheckpointState,
    pub(crate) enrollment_floor: Option<SnapshotRank>,
}

impl LoadedCheckpointAuthority {
    pub(crate) fn restore(
        self,
        local_peer: &str,
    ) -> Result<CooperativeMembershipState, MembershipStateStoreError> {
        if self.enrollment_floor.is_some() {
            return Err(MembershipStateStoreError::CheckpointEnrollmentIncomplete);
        }
        Ok(CooperativeMembershipState::restore(
            self.credentials.capability()?,
            local_peer.to_owned(),
            self.retained,
        )?)
    }
}

#[derive(Debug)]
pub(crate) enum PersistedAuthority {
    Legacy(PersistedMembershipStateData),
    Checkpoint(Box<LoadedCheckpointAuthority>),
}

// This DTO is private: its secret may only be encoded into the protected state file.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CheckpointEnvelope {
    version: u8,
    network_name: String,
    local_peer: String,
    anchor: NetworkAnchor,
    capability_secret: String,
    retained: RetainedCheckpointState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enrollment_floor: Option<SnapshotRank>,
}

impl MembershipStateStore {
    /// Decode either format without falling back to static peers on checkpoint errors.
    /// Explicit configuration/pairing pins are never silently replaced by saved keys.
    pub(crate) fn load_authority(
        &self,
        network_name: &str,
        local_peer: &str,
        expected_anchor: Option<&NetworkAnchor>,
        configured_secret: Option<&[u8]>,
    ) -> Result<Option<PersistedAuthority>, MembershipStateStoreError> {
        self.read_bytes()?
            .map(|bytes| match state_version(&bytes)? {
                LEGACY_MEMBERSHIP_STATE_VERSION | MEMBERSHIP_STATE_VERSION => Ok(
                    PersistedAuthority::Legacy(decode_legacy(&bytes, network_name, local_peer)?),
                ),
                CHECKPOINT_STATE_VERSION => {
                    self.validate_checkpoint_parent()?;
                    Ok(PersistedAuthority::Checkpoint(Box::new(decode_checkpoint(
                        &bytes,
                        network_name,
                        local_peer,
                        expected_anchor,
                        configured_secret,
                    )?)))
                }
                version => Err(MembershipStateStoreError::UnsupportedVersion(version)),
            })
            .transpose()
    }

    pub(crate) fn save_checkpoint(
        &self,
        network_name: &str,
        local_peer: &str,
        credentials: &CheckpointCredentials,
        retained: &RetainedCheckpointState,
    ) -> Result<(), MembershipStateStoreError> {
        self.save_checkpoint_with_parent_sync(
            network_name,
            local_peer,
            credentials,
            retained,
            sync_checkpoint_parent,
        )
    }

    fn save_checkpoint_with_parent_sync(
        &self,
        network_name: &str,
        local_peer: &str,
        credentials: &CheckpointCredentials,
        retained: &RetainedCheckpointState,
        sync_parent: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    ) -> Result<(), MembershipStateStoreError> {
        self.save_checkpoint_with_floor_and_sync(
            network_name,
            local_peer,
            credentials,
            retained,
            None,
            sync_parent,
        )
    }

    pub(crate) fn save_pending_checkpoint_enrollment(
        &self,
        network_name: &str,
        local_peer: &str,
        credentials: &CheckpointCredentials,
        seed: &RetainedCheckpointState,
        floor: SnapshotRank,
    ) -> Result<(), MembershipStateStoreError> {
        floor.validate()?;
        if floor.authority_revision == 0
            || floor.active_member_count < 2
            || seed.snapshot.payload.rank()? >= floor
        {
            return Err(MembershipStateStoreError::CheckpointEnrollmentIncomplete);
        }
        self.save_checkpoint_with_floor_and_sync(
            network_name,
            local_peer,
            credentials,
            seed,
            Some(floor),
            sync_checkpoint_parent,
        )
    }

    fn save_checkpoint_with_floor_and_sync(
        &self,
        network_name: &str,
        local_peer: &str,
        credentials: &CheckpointCredentials,
        retained: &RetainedCheckpointState,
        enrollment_floor: Option<SnapshotRank>,
        sync_parent: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    ) -> Result<(), MembershipStateStoreError> {
        self.validate_checkpoint_parent()?;
        validate_retained(retained, local_peer, &credentials.capability()?)?;
        let bytes = serde_json::to_vec(&CheckpointEnvelope {
            version: CHECKPOINT_STATE_VERSION,
            network_name: network_name.to_owned(),
            local_peer: local_peer.to_owned(),
            anchor: credentials.anchor.clone(),
            capability_secret: STANDARD.encode(&credentials.secret),
            retained: retained.clone(),
            enrollment_floor,
        })?;
        validate_checkpoint_length(bytes.len())?;

        if let Some(PersistedAuthority::Checkpoint(previous)) = self.load_authority(
            network_name,
            local_peer,
            Some(&credentials.anchor),
            Some(&credentials.secret),
        )? {
            if let Some(previous_floor) = previous.enrollment_floor {
                let lowered = match enrollment_floor {
                    Some(next_floor) => next_floor < previous_floor,
                    None => retained.snapshot.payload.rank()? < previous_floor,
                };
                if lowered {
                    return Err(MembershipStateStoreError::CheckpointEnrollmentIncomplete);
                }
            }
            if retained.snapshot.payload.rank()? < previous.retained.snapshot.payload.rank()? {
                return Err(MembershipStateStoreError::CheckpointRollback);
            }
            validate_hostname_progress(&previous.retained, retained)?;
        }
        let mut replacement_visible = false;
        let result = self.save_with_parent_sync(&bytes, |parent| {
            replacement_visible = true;
            sync_parent(parent)
        });
        match result {
            Err(MembershipStateStoreError::Io(error)) if replacement_visible => Err(
                MembershipStateStoreError::CheckpointDurabilityUncertain(error),
            ),
            result => result,
        }
    }

    fn validate_checkpoint_parent(&self) -> Result<(), MembershipStateStoreError> {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = self
            .path
            .parent()
            .ok_or(MembershipStateStoreError::MissingParent)?;
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(MembershipStateStoreError::UnsafeParent(
                parent.to_path_buf(),
            ));
        }
        Ok(())
    }
}

fn decode_checkpoint(
    bytes: &[u8],
    network_name: &str,
    local_peer: &str,
    expected_anchor: Option<&NetworkAnchor>,
    configured_secret: Option<&[u8]>,
) -> Result<LoadedCheckpointAuthority, MembershipStateStoreError> {
    validate_checkpoint_length(bytes.len())?;
    let envelope: CheckpointEnvelope = serde_json::from_slice(bytes)?;
    if envelope.version != CHECKPOINT_STATE_VERSION {
        return Err(MembershipStateStoreError::UnsupportedVersion(
            envelope.version,
        ));
    }
    validate_scope(
        &envelope.network_name,
        &envelope.local_peer,
        network_name,
        local_peer,
    )?;
    if expected_anchor.is_some_and(|anchor| *anchor != envelope.anchor) {
        return Err(MembershipStateStoreError::CapabilityMismatch);
    }
    if envelope.capability_secret.len() > MAX_ENCODED_CAPABILITY_BYTES {
        return Err(MembershipStateStoreError::InvalidCapability);
    }
    let secret = STANDARD
        .decode(&envelope.capability_secret)
        .map_err(|_| MembershipStateStoreError::InvalidCapability)?;
    if STANDARD.encode(&secret) != envelope.capability_secret {
        return Err(MembershipStateStoreError::InvalidCapability);
    }
    let credentials = CheckpointCredentials::new(envelope.anchor, secret)?;
    let capability = credentials.capability()?;
    validate_retained(&envelope.retained, local_peer, &capability)?;
    if let Some(floor) = envelope.enrollment_floor {
        floor.validate()?;
        if floor.authority_revision == 0
            || floor.active_member_count < 2
            || envelope.retained.snapshot.payload.rank()? >= floor
        {
            return Err(MembershipStateStoreError::CheckpointEnrollmentIncomplete);
        }
    }
    if let Some(configured) = configured_secret {
        let configured =
            NetworkCapability::from_secret(credentials.anchor.clone(), Some(configured))?;
        configured
            .verify(&envelope.retained.snapshot)
            .map_err(|_| MembershipStateStoreError::CapabilityMismatch)?;
    }
    Ok(LoadedCheckpointAuthority {
        credentials,
        retained: envelope.retained,
        enrollment_floor: envelope.enrollment_floor,
    })
}

fn validate_retained(
    retained: &RetainedCheckpointState,
    local_peer: &str,
    capability: &NetworkCapability,
) -> Result<(), MembershipStateStoreError> {
    let bytes = serde_json::to_vec(retained)?;
    RetainedCheckpointState::decode(&bytes, capability)?;
    // Local identity need not remain a member after resignation, but must be valid.
    CooperativeMembershipState::restore(
        capability.clone(),
        local_peer.to_owned(),
        retained.clone(),
    )?;
    Ok(())
}

fn validate_hostname_progress(
    previous: &RetainedCheckpointState,
    next: &RetainedCheckpointState,
) -> Result<(), MembershipStateStoreError> {
    for old in &previous.hostname_claims {
        let peer = &old.payload.subject.peer_id;
        if next.snapshot.payload.member(peer).is_none_or(|member| {
            member.subject != old.payload.subject || member.incarnation != old.payload.incarnation
        }) {
            continue;
        }
        let found = next
            .hostname_claims
            .binary_search_by(|name| name.payload.subject.peer_id.cmp(peer));
        let Ok(index) = found else {
            return Err(MembershipStateStoreError::HostnameRollback);
        };
        let name = &next.hostname_claims[index].payload;
        if (name.sequence, &name.hostname) < (old.payload.sequence, &old.payload.hostname) {
            return Err(MembershipStateStoreError::HostnameRollback);
        }
    }
    Ok(())
}

fn validate_checkpoint_length(length: usize) -> Result<(), MembershipStateStoreError> {
    if length > MAX_CHECKPOINT_STATE_BYTES {
        Err(MembershipStateStoreError::TooLarge { actual: length })
    } else {
        Ok(())
    }
}

fn sync_checkpoint_parent(parent: &std::path::Path) -> std::io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt as _, symlink},
        path::PathBuf,
        time::{Duration, Instant},
    };

    use crate::{
        identity::NodeIdentity,
        membership::checkpoint::{
            CheckpointMember, MembershipChange, MembershipSyncState, SignedHostnameClaim,
            SnapshotPolicy,
        },
    };

    use super::*;

    const WALL_NOW: u64 = 10_000;

    struct Fixture {
        directory: PathBuf,
        path: PathBuf,
        store: MembershipStateStore,
        credentials: CheckpointCredentials,
        local: NodeIdentity,
        member: NodeIdentity,
        state: CooperativeMembershipState,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "p2p-vpn-checkpoint-state-{}-{name}",
                std::process::id()
            ));
            fs::create_dir(&directory).expect("unique test directory");
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
            let path = directory.join("membership-state.json");
            let store = MembershipStateStore::new(&path);
            let credentials =
                CheckpointCredentials::new(NetworkAnchor::new([43; 32]).unwrap(), vec![55; 32])
                    .unwrap();
            let local = NodeIdentity::generate_ed25519().unwrap();
            let member = NodeIdentity::generate_ed25519().unwrap();
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
            Self {
                directory,
                path,
                store,
                credentials,
                local,
                member,
                state,
            }
        }

        fn save(&self) {
            self.store
                .save_checkpoint(
                    "lab",
                    &self.local.peer_id,
                    &self.credentials,
                    &self.state.retained(),
                )
                .unwrap();
        }

        fn load(&self) -> LoadedCheckpointAuthority {
            match self
                .store
                .load_authority(
                    "lab",
                    &self.local.peer_id,
                    Some(self.credentials.anchor()),
                    Some(self.credentials.secret()),
                )
                .unwrap()
                .unwrap()
            {
                PersistedAuthority::Checkpoint(state) => *state,
                PersistedAuthority::Legacy(_) => panic!("expected checkpoint"),
            }
        }

        fn mutate(&mut self, change: MembershipChange) {
            let mutation = self
                .state
                .sign_mutation_at(&self.local, change, WALL_NOW)
                .unwrap();
            self.state.apply_mutation_at(&mutation, WALL_NOW).unwrap();
        }

        fn name(&mut self, identity: &NodeIdentity, sequence: u64, hostname: &str) {
            let incarnation = self
                .state
                .snapshot()
                .payload
                .member(&identity.peer_id)
                .unwrap()
                .incarnation;
            let claim = SignedHostnameClaim::issue(
                self.credentials.anchor.clone(),
                identity,
                incarnation,
                sequence,
                hostname,
            )
            .unwrap();
            self.state.merge_hostname_claims(&[claim]).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn checkpoint_credentials_are_private_bounded_and_never_debug_printed() {
        let first = CheckpointCredentials::generate().unwrap();
        let second = CheckpointCredentials::generate().unwrap();
        assert_ne!(first.anchor(), second.anchor());
        assert_ne!(first.secret(), second.secret());
        assert_eq!(first.secret().len(), 32);
        assert!(!format!("{first:?}").contains(&STANDARD.encode(first.secret())));
        for size in [0, 31, MAX_CAPABILITY_BYTES + 1] {
            assert!(CheckpointCredentials::new(first.anchor.clone(), vec![1; size]).is_err());
        }
        assert!(
            CheckpointCredentials::new(first.anchor.clone(), vec![1; MAX_CAPABILITY_BYTES]).is_ok()
        );
    }

    #[test]
    fn checkpoint_state_roundtrip_restores_resync_gate_not_static_authority() {
        let fixture = Fixture::new("roundtrip");
        fixture.save();
        assert_eq!(fixture.load().retained, fixture.state.retained());
        assert_eq!(
            fs::metadata(&fixture.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut restored = fixture.load().restore(&fixture.local.peer_id).unwrap();
        assert_eq!(restored.sync_state(), MembershipSyncState::ResyncRequired);
        let effective = restored.effective_membership_at(WALL_NOW).unwrap();
        assert_eq!(effective.overlay_members().count(), 0);
        assert!(!effective.authorizes_configured_peer(fixture.member.peer_id.parse().unwrap()));
        let now = Instant::now();
        restored.begin_resync(now, Duration::from_secs(1)).unwrap();
        restored
            .finish_resync(now + Duration::from_secs(1), WALL_NOW)
            .unwrap();
        assert_eq!(restored.sync_state(), MembershipSyncState::Participating);
        assert_eq!(
            restored
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .overlay_members()
                .count(),
            2
        );
        assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
    }

    #[test]
    fn pending_enrollment_pins_floor_across_restart_and_cannot_activate_seed() {
        let mut fixture = Fixture::new("enrollment-floor");
        let seed = fixture.state.retained();
        fixture.mutate(MembershipChange::RemoveMember(
            fixture.member.peer_id.clone(),
        ));
        fixture.mutate(MembershipChange::UpsertMember(
            CheckpointMember::new(&fixture.member).unwrap(),
        ));
        let floor = fixture.state.snapshot().payload.rank().unwrap();
        fixture
            .store
            .save_pending_checkpoint_enrollment(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &seed,
                floor,
            )
            .unwrap();
        let before = fs::read(&fixture.path).unwrap();
        assert_eq!(fixture.load().enrollment_floor, Some(floor));
        assert_eq!(fixture.load().retained, seed);
        assert!(matches!(
            fixture.load().restore(&fixture.local.peer_id),
            Err(MembershipStateStoreError::CheckpointEnrollmentIncomplete)
        ));
        assert_eq!(
            fs::metadata(&fixture.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(matches!(
            fixture.store.save_checkpoint(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &seed,
            ),
            Err(MembershipStateStoreError::CheckpointEnrollmentIncomplete)
        ));
        let lower = SnapshotRank {
            authority_revision: 1,
            ..floor
        };
        assert!(matches!(
            fixture.store.save_pending_checkpoint_enrollment(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &seed,
                lower,
            ),
            Err(MembershipStateStoreError::CheckpointEnrollmentIncomplete)
        ));
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
        fixture.save();
        assert_eq!(fixture.load().enrollment_floor, None);
        assert_eq!(fixture.load().retained, fixture.state.retained());
        let ready: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture.path).unwrap()).unwrap();
        assert!(ready.get("enrollment_floor").is_none());
        assert!(fixture.load().restore(&fixture.local.peer_id).is_ok());
        assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
    }

    #[test]
    fn enrollment_floor_rejects_invalid_inputs_and_corruption_without_rewriting() {
        let fixture = Fixture::new("invalid-enrollment-floor");
        fixture.save();
        let seed = fixture.state.retained();
        let valid_floor = SnapshotRank {
            authority_revision: 1,
            active_member_count: 2,
            digest: [1; 32],
        };
        let before = fs::read(&fixture.path).unwrap();
        let invalid = [
            SnapshotRank {
                authority_revision: 0,
                ..valid_floor
            },
            SnapshotRank {
                authority_revision: u64::MAX,
                ..valid_floor
            },
            SnapshotRank {
                active_member_count: 1,
                ..valid_floor
            },
            SnapshotRank {
                active_member_count: crate::membership::checkpoint::MAX_CHECKPOINT_MEMBERS + 1,
                ..valid_floor
            },
            SnapshotRank {
                digest: [0; 32],
                ..valid_floor
            },
            seed.snapshot.payload.rank().unwrap(),
        ];
        for floor in invalid {
            assert!(
                fixture
                    .store
                    .save_pending_checkpoint_enrollment(
                        "lab",
                        &fixture.local.peer_id,
                        &fixture.credentials,
                        &seed,
                        floor,
                    )
                    .is_err()
            );
            assert_eq!(fs::read(&fixture.path).unwrap(), before);
            let mut corrupt: serde_json::Value = serde_json::from_slice(&before).unwrap();
            corrupt["enrollment_floor"] = serde_json::to_value(floor).unwrap();
            let bytes = serde_json::to_vec(&corrupt).unwrap();
            fs::write(&fixture.path, &bytes).unwrap();
            assert!(
                fixture
                    .store
                    .load_authority("lab", &fixture.local.peer_id, None, None)
                    .is_err()
            );
            assert!(
                fixture
                    .store
                    .save_checkpoint("lab", &fixture.local.peer_id, &fixture.credentials, &seed)
                    .is_err()
            );
            assert_eq!(fs::read(&fixture.path).unwrap(), bytes);
            fs::write(&fixture.path, &before).unwrap();
        }
    }

    #[test]
    fn visible_qualifying_enrollment_replacement_clears_floor_even_when_sync_fails() {
        let mut fixture = Fixture::new("enrollment-visible-write");
        let seed = fixture.state.retained();
        fixture.mutate(MembershipChange::RemoveMember(
            fixture.member.peer_id.clone(),
        ));
        fixture.mutate(MembershipChange::UpsertMember(
            CheckpointMember::new(&fixture.member).unwrap(),
        ));
        let floor = fixture.state.snapshot().payload.rank().unwrap();
        fixture
            .store
            .save_pending_checkpoint_enrollment(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &seed,
                floor,
            )
            .unwrap();
        assert!(matches!(
            fixture.store.save_checkpoint_with_parent_sync(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &fixture.state.retained(),
                |_| Err(std::io::Error::other("injected directory sync failure")),
            ),
            Err(MembershipStateStoreError::CheckpointDurabilityUncertain(_))
        ));
        let loaded = fixture.load();
        assert!(loaded.enrollment_floor.is_none());
        assert_eq!(loaded.retained, fixture.state.retained());
        assert_eq!(
            loaded.restore(&fixture.local.peer_id).unwrap().sync_state(),
            MembershipSyncState::ResyncRequired
        );
        fixture.save();
        assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
    }

    #[test]
    fn initial_enrollment_write_failures_preserve_pending_gate_and_allow_retry() {
        let fixture = Fixture::new("enrollment-initial-write");
        let floor = SnapshotRank {
            authority_revision: 1,
            active_member_count: 2,
            digest: [1; 32],
        };
        fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            fixture.store.save_pending_checkpoint_enrollment(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &fixture.state.retained(),
                floor,
            ),
            Err(MembershipStateStoreError::UnsafeParent(_))
        ));
        assert!(!fixture.path.exists());
        fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            fixture.store.save_checkpoint_with_floor_and_sync(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &fixture.state.retained(),
                Some(floor),
                |_| Err(std::io::Error::other(
                    "injected initial directory sync failure"
                )),
            ),
            Err(MembershipStateStoreError::CheckpointDurabilityUncertain(_))
        ));
        assert_eq!(fixture.load().enrollment_floor, Some(floor));
        assert!(fixture.load().restore(&fixture.local.peer_id).is_err());
        fixture
            .store
            .save_pending_checkpoint_enrollment(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &fixture.state.retained(),
                floor,
            )
            .unwrap();
        assert_eq!(fixture.load().enrollment_floor, Some(floor));
        assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
    }

    #[test]
    fn checkpoint_state_dispatch_preserves_legacy_and_prevents_legacy_downgrade() {
        let fixture = Fixture::new("versions");
        for version in [1, 2] {
            let legacy = serde_json::to_vec(&serde_json::json!({
                "version": version, "network_name": "lab", "local_peer": fixture.local.peer_id,
                "records": []
            }))
            .unwrap();
            fs::write(&fixture.path, &legacy).unwrap();
            fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(
                matches!(fixture.store.load_authority("lab", &fixture.local.peer_id, None, None).unwrap(),
                Some(PersistedAuthority::Legacy(PersistedMembershipStateData { records, hostname_records }))
                if records.is_empty() && hostname_records.is_empty())
            );
            assert_eq!(fs::read(&fixture.path).unwrap(), legacy);
        }
        fixture.save();
        let committed = fs::read(&fixture.path).unwrap();
        assert!(matches!(
            fixture.store.load("lab", &fixture.local.peer_id),
            Err(MembershipStateStoreError::UnsupportedVersion(3))
        ));
        assert!(matches!(
            fixture.store.save("lab", &fixture.local.peer_id, &[], &[]),
            Err(MembershipStateStoreError::UnsupportedVersion(3))
        ));
        assert_eq!(fs::read(&fixture.path).unwrap(), committed);
        assert!(
            !String::from_utf8(committed)
                .unwrap()
                .contains("\"records\"")
        );
    }

    #[test]
    fn checkpoint_state_pins_scope_anchor_and_configured_secret_without_rewriting() {
        let fixture = Fixture::new("scope");
        fixture.save();
        let before = fs::read(&fixture.path).unwrap();
        assert!(matches!(
            fixture
                .store
                .load_authority("other", &fixture.local.peer_id, None, None),
            Err(MembershipStateStoreError::NetworkMismatch { .. })
        ));
        assert!(matches!(
            fixture
                .store
                .load_authority("lab", &fixture.member.peer_id, None, None),
            Err(MembershipStateStoreError::LocalPeerMismatch { .. })
        ));
        let wrong_anchor = NetworkAnchor::new([42; 32]).unwrap();
        assert!(matches!(
            fixture
                .store
                .load_authority("lab", &fixture.local.peer_id, Some(&wrong_anchor), None),
            Err(MembershipStateStoreError::CapabilityMismatch)
        ));
        let wrong_secret = vec![54; 32];
        let error = fixture
            .store
            .load_authority("lab", &fixture.local.peer_id, None, Some(&wrong_secret))
            .unwrap_err();
        assert!(matches!(
            error,
            MembershipStateStoreError::CapabilityMismatch
        ));
        assert!(!format!("{error:?}").contains(&STANDARD.encode(&wrong_secret)));
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
        assert!(
            !fixture
                .store
                .load_authority("lab", &fixture.local.peer_id, None, None)
                .unwrap()
                .map(|s| format!("{s:?}"))
                .unwrap()
                .contains(&STANDARD.encode(fixture.credentials.secret()))
        );
        let wrong_credentials =
            CheckpointCredentials::new(fixture.credentials.anchor.clone(), wrong_secret).unwrap();
        let wrong_state = CooperativeMembershipState::bootstrap_at(
            wrong_credentials.capability().unwrap(),
            fixture.local.peer_id.clone(),
            vec![CheckpointMember::new(&fixture.local).unwrap()],
            SnapshotPolicy::default(),
            WALL_NOW,
        )
        .unwrap();
        assert!(
            fixture
                .store
                .save_checkpoint(
                    "lab",
                    &fixture.local.peer_id,
                    &wrong_credentials,
                    &wrong_state.retained()
                )
                .is_err()
        );
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
    }

    #[test]
    fn checkpoint_state_rejects_corrupt_unknown_and_oversized_envelopes_atomically() {
        let fixture = Fixture::new("invalid");
        fixture.save();
        let valid: serde_json::Value =
            serde_json::from_slice(&fs::read(&fixture.path).unwrap()).unwrap();
        for variant in 0..8 {
            let mut value = valid.clone();
            match variant {
                0 => {
                    let byte = value["retained"]["snapshot"]["mac"][0].as_u64().unwrap();
                    value["retained"]["snapshot"]["mac"][0] = serde_json::json!(byte ^ 1);
                }
                1 => {
                    value["retained"]["snapshot"]["payload"]["authority_revision"] =
                        serde_json::json!(1)
                }
                2 => value["retained"]["old_admissions"] = serde_json::json!([]),
                3 => value["revocations"] = serde_json::json!([]),
                4 => value["capability_secret"] = serde_json::json!("not a secret!"),
                5 => {
                    value["capability_secret"] =
                        serde_json::json!("A".repeat(MAX_ENCODED_CAPABILITY_BYTES + 1))
                }
                6 => value["version"] = serde_json::json!(4),
                _ => value["retained"]["hostname_claims"] = serde_json::json!([{}]),
            }
            let before = serde_json::to_vec(&value).unwrap();
            fs::write(&fixture.path, &before).unwrap();
            assert!(
                fixture
                    .store
                    .load_authority("lab", &fixture.local.peer_id, None, None)
                    .is_err(),
                "invalid variant {variant}"
            );
            assert!(
                fixture
                    .store
                    .save_checkpoint(
                        "lab",
                        &fixture.local.peer_id,
                        &fixture.credentials,
                        &fixture.state.retained()
                    )
                    .is_err()
            );
            assert_eq!(fs::read(&fixture.path).unwrap(), before);
            assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
        }
        fs::write(&fixture.path, vec![b' '; MAX_CHECKPOINT_STATE_BYTES + 1]).unwrap();
        assert!(matches!(
            fixture
                .store
                .load_authority("lab", &fixture.local.peer_id, None, None),
            Err(MembershipStateStoreError::TooLarge { .. })
        ));
    }

    #[test]
    fn checkpoint_state_rejects_lower_rank_and_stale_hostname_without_rewriting() {
        let mut fixture = Fixture::new("rollback");
        let local = fixture.local.clone();
        fixture.name(&local, 1, "old-name");
        fixture.save();
        let stale = fixture.state.retained();
        fixture.mutate(MembershipChange::RemoveMember(
            fixture.member.peer_id.clone(),
        ));
        fixture.name(&local, 2, "new-name");
        fixture.save();
        let before = fs::read(&fixture.path).unwrap();
        assert!(matches!(
            fixture.store.save_checkpoint(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &stale
            ),
            Err(MembershipStateStoreError::CheckpointRollback)
        ));
        for claims in [stale.hostname_claims, vec![]] {
            let mut replacement = fixture.state.retained();
            replacement.hostname_claims = claims;
            assert!(matches!(
                fixture.store.save_checkpoint(
                    "lab",
                    &fixture.local.peer_id,
                    &fixture.credentials,
                    &replacement
                ),
                Err(MembershipStateStoreError::HostnameRollback)
            ));
            assert_eq!(fs::read(&fixture.path).unwrap(), before);
        }
    }

    #[test]
    fn checkpoint_state_erases_removed_names_and_accepts_fresh_readmission() {
        let mut fixture = Fixture::new("readmission");
        let member = fixture.member.clone();
        fixture.name(&member, 900, "discarded-device");
        fixture.save();
        let old_claim = fixture.state.hostname_claims()[0].clone();
        fixture.mutate(MembershipChange::RemoveMember(member.peer_id.clone()));
        fixture.save();
        let bytes = String::from_utf8(fs::read(&fixture.path).unwrap()).unwrap();
        assert!(!bytes.contains(&member.peer_id));
        assert!(!bytes.contains("discarded-device"));
        fixture.mutate(MembershipChange::UpsertMember(
            CheckpointMember::new(&member).unwrap(),
        ));
        fixture.name(&member, 1, "new-device");
        fixture.save();
        assert_ne!(
            fixture.load().retained.hostname_claims[0]
                .payload
                .incarnation,
            old_claim.payload.incarnation
        );
        let mut stale_name = fixture.state.retained();
        stale_name.hostname_claims = vec![old_claim];
        let before = fs::read(&fixture.path).unwrap();
        assert!(
            fixture
                .store
                .save_checkpoint(
                    "lab",
                    &fixture.local.peer_id,
                    &fixture.credentials,
                    &stale_name
                )
                .is_err()
        );
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
    }

    #[test]
    fn checkpoint_state_persists_cooperative_fork_choice_including_accepted_revocation_rollback() {
        let mut fixture = Fixture::new("fork-selection");
        let mut other = CooperativeMembershipState::restore(
            fixture.credentials.capability().unwrap(),
            fixture.member.peer_id.clone(),
            fixture.state.retained(),
        )
        .unwrap();
        let now = Instant::now();
        other.begin_resync(now, Duration::from_secs(1)).unwrap();
        other
            .finish_resync(now + Duration::from_secs(1), WALL_NOW)
            .unwrap();
        fixture.mutate(MembershipChange::RemoveMember(
            fixture.member.peer_id.clone(),
        ));
        fixture.save();
        let transient = NodeIdentity::generate_ed25519().unwrap();
        for change in [
            MembershipChange::UpsertMember(CheckpointMember::new(&transient).unwrap()),
            MembershipChange::RemoveMember(transient.peer_id.clone()),
        ] {
            let mutation = other
                .sign_mutation_at(&fixture.member, change, WALL_NOW)
                .unwrap();
            other.apply_mutation_at(&mutation, WALL_NOW).unwrap();
        }
        let challenge = fixture
            .state
            .begin_resync(now, Duration::from_secs(1))
            .unwrap();
        let offer = other
            .make_offer_at(challenge, &fixture.member, WALL_NOW)
            .unwrap();
        fixture
            .state
            .collect_offer(&offer, fixture.member.peer_id.parse().unwrap(), now)
            .unwrap();
        let selected = fixture
            .state
            .finish_resync(now + Duration::from_secs(1), WALL_NOW)
            .unwrap();
        assert!(selected.decisions_may_have_been_discarded);
        assert_eq!(selected.added_members, 1);
        fixture.save();
        assert_eq!(fixture.load().retained, fixture.state.retained());
        assert!(
            fixture
                .load()
                .retained
                .snapshot
                .payload
                .member(&fixture.member.peer_id)
                .is_some()
        );
        // The user accepts applying the revocation again on the selected branch.
        fixture.mutate(MembershipChange::RemoveMember(
            fixture.member.peer_id.clone(),
        ));
        fixture.save();
        let bytes = String::from_utf8(fs::read(&fixture.path).unwrap()).unwrap();
        assert!(!bytes.contains(&fixture.member.peer_id));
        assert!(!bytes.contains(&transient.peer_id));
        assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
    }

    #[test]
    fn checkpoint_state_durable_churn_has_no_device_archive_or_temporary_file_growth() {
        let mut fixture = Fixture::new("churn");
        fixture.mutate(MembershipChange::RemoveMember(
            fixture.member.peer_id.clone(),
        ));
        fixture.save();
        let baseline = fs::read(&fixture.path).unwrap().len();
        for _ in 0..128 {
            let member = NodeIdentity::generate_ed25519().unwrap();
            fixture.mutate(MembershipChange::UpsertMember(
                CheckpointMember::new(&member).unwrap(),
            ));
            fixture.name(&member, 1, "temporary-device");
            fixture.save();
            fixture.mutate(MembershipChange::RemoveMember(member.peer_id.clone()));
            fixture.save();
            let bytes = fs::read(&fixture.path).unwrap();
            assert!(bytes.len() <= baseline + 32);
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains(&member.peer_id));
            assert!(!text.contains(&STANDARD.encode(member.public_key_protobuf().unwrap())));
            for historical_field in [
                "temporary-device",
                "inviter",
                "revocation",
                "admission",
                "signature",
            ] {
                assert!(!text.contains(historical_field));
            }
            assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
        }
        assert_eq!(fixture.load().retained.snapshot.payload.members.len(), 1);
        assert!(fixture.load().retained.hostname_claims.is_empty());
    }

    #[test]
    fn checkpoint_state_self_resignation_restores_exclusion_and_preserves_survivor() {
        let mut fixture = Fixture::new("self-resignation");
        fixture.mutate(MembershipChange::RemoveMember(
            fixture.local.peer_id.clone(),
        ));
        fixture.save();
        let mut restored = fixture.load().restore(&fixture.local.peer_id).unwrap();
        assert_eq!(
            restored.snapshot().payload.members[0].subject.peer_id,
            fixture.member.peer_id
        );
        let now = Instant::now();
        restored.begin_resync(now, Duration::from_secs(1)).unwrap();
        restored
            .finish_resync(now + Duration::from_secs(1), WALL_NOW)
            .unwrap();
        assert_eq!(restored.sync_state(), MembershipSyncState::Excluded);
        assert_eq!(
            restored
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .overlay_members()
                .count(),
            0
        );
    }

    #[test]
    fn checkpoint_state_directory_sync_failure_is_visible_and_retry_is_idempotent() {
        let mut fixture = Fixture::new("sync-failure");
        fixture.save();
        fixture.mutate(MembershipChange::RemoveMember(
            fixture.member.peer_id.clone(),
        ));
        let result = fixture.store.save_checkpoint_with_parent_sync(
            "lab",
            &fixture.local.peer_id,
            &fixture.credentials,
            &fixture.state.retained(),
            |parent| {
                assert_eq!(parent, fixture.directory);
                Err(std::io::Error::other("injected sync failure"))
            },
        );
        assert!(matches!(
            result,
            Err(MembershipStateStoreError::CheckpointDurabilityUncertain(_))
        ));
        assert_eq!(fixture.load().retained, fixture.state.retained());
        assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
        fixture.save();
        assert_eq!(fixture.load().retained, fixture.state.retained());
    }

    #[test]
    fn checkpoint_state_rejects_unsafe_files_and_symlink_parents() {
        let fixture = Fixture::new("unsafe");
        fixture.save();
        fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            fixture
                .store
                .load_authority("lab", &fixture.local.peer_id, None, None),
            Err(MembershipStateStoreError::PermissiveMode { .. })
        ));
        assert!(
            fixture
                .store
                .save_checkpoint(
                    "lab",
                    &fixture.local.peer_id,
                    &fixture.credentials,
                    &fixture.state.retained()
                )
                .is_err()
        );
        fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::read(&fixture.path).unwrap();
        fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(matches!(
            fixture
                .store
                .load_authority("lab", &fixture.local.peer_id, None, None),
            Err(MembershipStateStoreError::UnsafeParent(_))
        ));
        assert!(matches!(
            fixture.store.save_checkpoint(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &fixture.state.retained()
            ),
            Err(MembershipStateStoreError::UnsafeParent(_))
        ));
        fs::set_permissions(&fixture.directory, fs::Permissions::from_mode(0o700)).unwrap();
        let link = fixture.directory.join("linked-state");
        symlink(&fixture.path, &link).unwrap();
        let store = MembershipStateStore::new(&link);
        assert!(matches!(
            store.load_authority("lab", &fixture.local.peer_id, None, None),
            Err(MembershipStateStoreError::UnsafeFile(_))
        ));
        assert!(
            store
                .save_checkpoint(
                    "lab",
                    &fixture.local.peer_id,
                    &fixture.credentials,
                    &fixture.state.retained()
                )
                .is_err()
        );
        let parent = fixture.directory.join("linked-parent");
        symlink(&fixture.directory, &parent).unwrap();
        assert!(matches!(
            MembershipStateStore::new(parent.join("other-state")).save_checkpoint(
                "lab",
                &fixture.local.peer_id,
                &fixture.credentials,
                &fixture.state.retained()
            ),
            Err(MembershipStateStoreError::UnsafeParent(_))
        ));
        assert_eq!(fs::read(&fixture.path).unwrap(), before);
    }
}
