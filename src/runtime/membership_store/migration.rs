//! A single protected, expiring handoff artifact. Never an authority backup.

use std::{fmt, fs, io, path::Path};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::membership::checkpoint::{
    MAX_CAPABILITY_BYTES, NetworkAnchor, migration::SignedLegacyMigrationSeed,
};

use super::{
    MembershipStateStore, MembershipStateStoreError,
    checkpoint::{CheckpointCredentials, MAX_CHECKPOINT_STATE_BYTES, sync_checkpoint_parent},
    validate_state_file,
};

pub(crate) struct MigrationArtifact {
    pub(crate) credentials: CheckpointCredentials,
    pub(crate) seed: SignedLegacyMigrationSeed,
}

impl fmt::Debug for MigrationArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MigrationArtifact")
            .field("network_name", &self.seed.payload.network_name)
            .field("anchor", self.credentials.anchor())
            .field(
                "member_count",
                &self.seed.payload.snapshot.payload.members.len(),
            )
            .finish_non_exhaustive()
    }
}

impl MigrationArtifact {
    pub(crate) fn verify_at(
        &self,
        network_name: &str,
        now: u64,
    ) -> Result<(), MembershipStateStoreError> {
        self.seed
            .verify_at(&self.credentials.capability()?, network_name, now)?;
        Ok(())
    }
}

// This DTO is never returned by diagnostics or configuration export.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MigrationEnvelope {
    version: u8,
    anchor: NetworkAnchor,
    capability_secret: String,
    seed: SignedLegacyMigrationSeed,
}

#[derive(Debug)]
pub(crate) struct MigrationArtifactStore {
    protected: MembershipStateStore,
}

impl MigrationArtifactStore {
    pub(crate) fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            protected: MembershipStateStore::new(path),
        }
    }

    pub(crate) fn for_authority(authority: &MembershipStateStore) -> Self {
        Self::new(authority.path.with_extension("migration.json"))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.protected.path
    }

    /// Authenticity and scope are checked even for an expired artifact. The owner
    /// can then retire it without extending its deadline or regenerating scope.
    pub(crate) fn load(
        &self,
        network_name: &str,
        configured_secret: Option<&[u8]>,
    ) -> Result<Option<MigrationArtifact>, MembershipStateStoreError> {
        self.protected.validate_checkpoint_parent()?;
        let Some(bytes) = self.protected.read_bytes()? else {
            return Ok(None);
        };
        if bytes.len() > MAX_CHECKPOINT_STATE_BYTES {
            return Err(MembershipStateStoreError::TooLarge {
                actual: bytes.len(),
            });
        }
        let envelope: MigrationEnvelope = serde_json::from_slice(&bytes)?;
        if envelope.version != 1 {
            return Err(MembershipStateStoreError::UnsupportedVersion(
                envelope.version,
            ));
        }
        if envelope.capability_secret.len() > MAX_CAPABILITY_BYTES.div_ceil(3) * 4 {
            return Err(MembershipStateStoreError::InvalidCapability);
        }
        let secret = STANDARD
            .decode(&envelope.capability_secret)
            .map_err(|_| MembershipStateStoreError::InvalidCapability)?;
        if STANDARD.encode(&secret) != envelope.capability_secret {
            return Err(MembershipStateStoreError::InvalidCapability);
        }
        let credentials = CheckpointCredentials::new(envelope.anchor, secret)?;
        if let Some(configured) = configured_secret {
            let pinned = crate::membership::checkpoint::NetworkCapability::from_secret(
                credentials.anchor().clone(),
                Some(configured),
            )?;
            pinned
                .verify(&envelope.seed.payload.snapshot)
                .map_err(|_| MembershipStateStoreError::CapabilityMismatch)?;
        }
        let artifact = MigrationArtifact {
            credentials,
            seed: envelope.seed,
        };
        artifact.verify_at(network_name, artifact.seed.payload.issued_at_unix_seconds)?;
        Ok(Some(artifact))
    }

    pub(crate) fn save(
        &self,
        artifact: &MigrationArtifact,
        network_name: &str,
        now: u64,
    ) -> Result<(), MembershipStateStoreError> {
        self.save_with_sync(artifact, network_name, now, sync_checkpoint_parent)
    }

    fn save_with_sync(
        &self,
        artifact: &MigrationArtifact,
        network_name: &str,
        now: u64,
        sync_parent: impl FnOnce(&Path) -> io::Result<()>,
    ) -> Result<(), MembershipStateStoreError> {
        self.protected.validate_checkpoint_parent()?;
        artifact.verify_at(network_name, now)?;
        if let Some(previous) = self.load(network_name, None)? {
            // Retrying preparation cannot silently replace a distributed scope or
            // refresh the deadline. Cancel/retire explicitly before preparing again.
            if previous.credentials.anchor() != artifact.credentials.anchor()
                || previous.credentials.secret() != artifact.credentials.secret()
                || previous.seed != artifact.seed
            {
                return Err(MembershipStateStoreError::Io(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a different migration artifact is already prepared",
                )));
            }
        }
        let bytes = serde_json::to_vec(&MigrationEnvelope {
            version: 1,
            anchor: artifact.credentials.anchor().clone(),
            capability_secret: STANDARD.encode(artifact.credentials.secret()),
            seed: artifact.seed.clone(),
        })?;
        if bytes.len() > MAX_CHECKPOINT_STATE_BYTES {
            return Err(MembershipStateStoreError::TooLarge {
                actual: bytes.len(),
            });
        }
        let mut visible = false;
        let result = self.protected.save_with_parent_sync(&bytes, |parent| {
            visible = true;
            sync_parent(parent)
        });
        match result {
            Err(MembershipStateStoreError::Io(error)) if visible => Err(
                MembershipStateStoreError::MigrationDurabilityUncertain(error),
            ),
            result => result,
        }
    }

    /// The serialized owner must compare the loaded artifact with its current
    /// operation before erasure. No extra copy or tombstone is created here.
    pub(crate) fn retire(
        &self,
        expected: &MigrationArtifact,
        network_name: &str,
    ) -> Result<bool, MembershipStateStoreError> {
        self.retire_with_sync(expected, network_name, sync_checkpoint_parent)
    }

    fn retire_with_sync(
        &self,
        expected: &MigrationArtifact,
        network_name: &str,
        sync_parent: impl FnOnce(&Path) -> io::Result<()>,
    ) -> Result<bool, MembershipStateStoreError> {
        let current = self.load(network_name, Some(expected.credentials.secret()))?;
        if let Some(current) = current {
            if current.credentials.anchor() != expected.credentials.anchor()
                || current.seed != expected.seed
            {
                return Err(MembershipStateStoreError::CapabilityMismatch);
            }
            let metadata = fs::symlink_metadata(self.path())?;
            validate_state_file(self.path(), &metadata)?;
            fs::remove_file(self.path())?;
        } else {
            // Reconfirm an unlink whose directory sync may have failed previously.
            sync_parent(
                self.path()
                    .parent()
                    .ok_or(MembershipStateStoreError::MissingParent)?,
            )?;
            return Ok(false);
        }
        sync_parent(
            self.path()
                .parent()
                .ok_or(MembershipStateStoreError::MissingParent)?,
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
