use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};

use crate::hostname::{
    HostnameRecordError, MAX_HOSTNAME_RECORD_ENCODED_LEN, MAX_HOSTNAME_RECORDS,
    SignedHostnameRecord, validate_hostname_record_history,
};
use crate::membership::{
    MAX_MEMBERSHIP_RECORD_ENCODED_LEN, MAX_MEMBERSHIP_RECORDS, MembershipRecordError,
    SignedMembershipRecord, validate_membership_record_history,
};

const MEMBERSHIP_STATE_VERSION: u8 = 2;
const LEGACY_MEMBERSHIP_STATE_VERSION: u8 = 1;
const MEMBERSHIP_STATE_ENVELOPE_BYTES: usize = 64 * 1024;
pub const MAX_MEMBERSHIP_STATE_BYTES: usize = MAX_MEMBERSHIP_RECORDS
    * (MAX_MEMBERSHIP_RECORD_ENCODED_LEN + 1)
    + MAX_HOSTNAME_RECORDS * (MAX_HOSTNAME_RECORD_ENCODED_LEN + 1)
    + MEMBERSHIP_STATE_ENVELOPE_BYTES;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PersistedMembershipStateData {
    pub(crate) records: Vec<SignedMembershipRecord>,
    pub(crate) hostname_records: Vec<SignedHostnameRecord>,
}

#[derive(Debug)]
pub(crate) struct MembershipStateStore {
    path: PathBuf,
}

impl MembershipStateStore {
    #[must_use]
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(crate) fn load(
        &self,
        expected_network_name: &str,
        expected_local_peer: &str,
    ) -> Result<Option<PersistedMembershipStateData>, MembershipStateStoreError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(MembershipStateStoreError::Io(error)),
        };
        validate_state_file(&self.path, &metadata)?;
        let length = usize::try_from(metadata.len())
            .map_err(|_| MembershipStateStoreError::TooLarge { actual: usize::MAX })?;
        validate_length(length)?;

        let file = File::open(&self.path)?;
        let opened_metadata = file.metadata()?;
        validate_state_file(&self.path, &opened_metadata)?;
        if metadata.dev() != opened_metadata.dev() || metadata.ino() != opened_metadata.ino() {
            return Err(MembershipStateStoreError::FileChanged(self.path.clone()));
        }
        let opened_length = usize::try_from(opened_metadata.len())
            .map_err(|_| MembershipStateStoreError::TooLarge { actual: usize::MAX })?;
        validate_length(opened_length)?;
        let mut bytes = Vec::with_capacity(opened_length);
        file.take((MAX_MEMBERSHIP_STATE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        validate_length(bytes.len())?;
        let state: OwnedPersistedMembershipState = serde_json::from_slice(&bytes)?;
        if state.version != MEMBERSHIP_STATE_VERSION
            && state.version != LEGACY_MEMBERSHIP_STATE_VERSION
        {
            return Err(MembershipStateStoreError::UnsupportedVersion(state.version));
        }
        if state.network_name != expected_network_name {
            return Err(MembershipStateStoreError::NetworkMismatch {
                expected: expected_network_name.to_owned(),
                actual: state.network_name,
            });
        }
        if state.local_peer != expected_local_peer {
            return Err(MembershipStateStoreError::LocalPeerMismatch {
                expected: expected_local_peer.to_owned(),
                actual: state.local_peer,
            });
        }
        validate_membership_record_history(&state.records, expected_network_name)?;
        validate_hostname_record_history(&state.hostname_records, expected_network_name)?;
        Ok(Some(PersistedMembershipStateData {
            records: state.records,
            hostname_records: state.hostname_records,
        }))
    }

    pub(crate) fn save(
        &self,
        network_name: &str,
        local_peer: &str,
        records: &[SignedMembershipRecord],
        hostname_records: &[SignedHostnameRecord],
    ) -> Result<(), MembershipStateStoreError> {
        validate_membership_record_history(records, network_name)?;
        validate_hostname_record_history(hostname_records, network_name)?;
        let bytes = serde_json::to_vec(&PersistedMembershipState {
            version: MEMBERSHIP_STATE_VERSION,
            network_name,
            local_peer,
            records,
            hostname_records,
        })?;
        validate_length(bytes.len())?;
        self.save_with_parent_sync(&bytes, sync_parent_directory)
    }

    fn save_with_parent_sync(
        &self,
        bytes: &[u8],
        sync_parent: impl FnOnce(&Path) -> io::Result<()>,
    ) -> Result<(), MembershipStateStoreError> {
        let parent = self
            .path
            .parent()
            .ok_or(MembershipStateStoreError::MissingParent)?;
        let parent_metadata = fs::symlink_metadata(parent)?;
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(MembershipStateStoreError::UnsafeParent(
                parent.to_path_buf(),
            ));
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.path) {
            validate_state_file(&self.path, &metadata)?;
        }

        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(MembershipStateStoreError::InvalidFileName)?;
        let mut nonce = [0_u8; 8];
        OsRng.fill_bytes(&mut nonce);
        let temporary_path = parent.join(format!(
            ".{file_name}.{}.{}",
            std::process::id(),
            u64::from_be_bytes(nonce)
        ));
        let mut temporary = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary_path)?;
        if let Err(error) = temporary
            .set_permissions(fs::Permissions::from_mode(0o600))
            .and_then(|()| temporary.write_all(bytes))
            .and_then(|()| temporary.sync_all())
        {
            drop(temporary);
            let _ = fs::remove_file(&temporary_path);
            return Err(MembershipStateStoreError::Io(error));
        }
        drop(temporary);
        if let Err(error) = fs::rename(&temporary_path, &self.path) {
            let _ = fs::remove_file(&temporary_path);
            return Err(MembershipStateStoreError::Io(error));
        }
        sync_parent(parent)?;
        Ok(())
    }
}

#[derive(Serialize)]
struct PersistedMembershipState<'a> {
    version: u8,
    network_name: &'a str,
    local_peer: &'a str,
    records: &'a [SignedMembershipRecord],
    hostname_records: &'a [SignedHostnameRecord],
}

#[derive(Deserialize)]
struct OwnedPersistedMembershipState {
    version: u8,
    network_name: String,
    local_peer: String,
    records: Vec<SignedMembershipRecord>,
    #[serde(default)]
    hostname_records: Vec<SignedHostnameRecord>,
}

fn validate_state_file(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MembershipStateStoreError> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MembershipStateStoreError::UnsafeFile(path.to_path_buf()));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(MembershipStateStoreError::PermissiveMode { mode });
    }
    Ok(())
}

fn validate_length(length: usize) -> Result<(), MembershipStateStoreError> {
    if length > MAX_MEMBERSHIP_STATE_BYTES {
        Err(MembershipStateStoreError::TooLarge { actual: length })
    } else {
        Ok(())
    }
}

fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    let directory = match File::open(parent) {
        Ok(directory) => directory,
        Err(error) => return handle_directory_sync_error(error),
    };
    match directory.sync_all() {
        Ok(()) => Ok(()),
        Err(error) => handle_directory_sync_error(error),
    }
}

fn handle_directory_sync_error(error: io::Error) -> io::Result<()> {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
    ) {
        Ok(())
    } else {
        Err(error)
    }
}

#[derive(Debug)]
pub enum MembershipStateStoreError {
    Io(io::Error),
    Json(serde_json::Error),
    Membership(MembershipRecordError),
    Hostname(HostnameRecordError),
    MissingParent,
    UnsafeParent(PathBuf),
    UnsafeFile(PathBuf),
    FileChanged(PathBuf),
    PermissiveMode { mode: u32 },
    InvalidFileName,
    TooLarge { actual: usize },
    UnsupportedVersion(u8),
    NetworkMismatch { expected: String, actual: String },
    LocalPeerMismatch { expected: String, actual: String },
}

impl std::fmt::Display for MembershipStateStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "membership state I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "membership state JSON is invalid: {error}"),
            Self::Membership(error) => {
                write!(
                    formatter,
                    "membership state contains an invalid record: {error:?}"
                )
            }
            Self::Hostname(error) => {
                write!(
                    formatter,
                    "membership state contains an invalid hostname record: {error:?}"
                )
            }
            Self::MissingParent => formatter.write_str("membership state path has no parent"),
            Self::UnsafeParent(path) => write!(
                formatter,
                "membership state parent must be a real directory: {}",
                path.display()
            ),
            Self::UnsafeFile(path) => write!(
                formatter,
                "membership state must be a regular file: {}",
                path.display()
            ),
            Self::FileChanged(path) => write!(
                formatter,
                "membership state changed while it was being opened: {}",
                path.display()
            ),
            Self::PermissiveMode { mode } => write!(
                formatter,
                "membership state has mode {mode:04o}; expected owner-only permissions"
            ),
            Self::InvalidFileName => {
                formatter.write_str("membership state path has no UTF-8 file name")
            }
            Self::TooLarge { actual } => write!(
                formatter,
                "membership state size {actual} exceeds limit {MAX_MEMBERSHIP_STATE_BYTES}"
            ),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported membership state version {version}")
            }
            Self::NetworkMismatch { expected, actual } => write!(
                formatter,
                "membership state belongs to network {actual:?}, expected {expected:?}"
            ),
            Self::LocalPeerMismatch { expected, actual } => write!(
                formatter,
                "membership state belongs to local peer {actual}, expected {expected}"
            ),
        }
    }
}

impl std::error::Error for MembershipStateStoreError {}

impl From<io::Error> for MembershipStateStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for MembershipStateStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<MembershipRecordError> for MembershipStateStoreError {
    fn from(error: MembershipRecordError) -> Self {
        Self::Membership(error)
    }
}

impl From<HostnameRecordError> for MembershipStateStoreError {
    fn from(error: HostnameRecordError) -> Self {
        Self::Hostname(error)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use crate::{
        hostname::issue_hostname_record_at,
        identity::NodeIdentity,
        membership::{MembershipRecordOptions, MembershipRole, issue_membership_record_at},
    };

    use super::*;

    fn test_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-membership-state-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("test directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("directory mode");
        path
    }

    fn membership_record() -> SignedMembershipRecord {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("membership record")
    }

    #[test]
    fn membership_state_round_trips_with_owner_only_permissions() {
        let directory = test_directory("round-trip");
        let path = directory.join("membership-state.json");
        let store = MembershipStateStore::new(&path);
        let records = vec![membership_record()];

        store
            .save("lab", "local-peer", &records, &[])
            .expect("save state");
        assert_eq!(
            store.load("lab", "local-peer").expect("load state"),
            Some(PersistedMembershipStateData {
                records,
                hostname_records: Vec::new(),
            })
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("state metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn membership_state_round_trips_hostname_records_and_loads_legacy_state() {
        let directory = test_directory("hostname-round-trip");
        let path = directory.join("membership-state.json");
        let store = MembershipStateStore::new(&path);
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let hostname_record = issue_hostname_record_at(&identity, "lab", "renamed", 2, 2_000)
            .expect("hostname record");

        store
            .save(
                "lab",
                &identity.peer_id,
                &[],
                std::slice::from_ref(&hostname_record),
            )
            .expect("save state");
        assert_eq!(
            store.load("lab", &identity.peer_id).expect("load state"),
            Some(PersistedMembershipStateData {
                records: Vec::new(),
                hostname_records: vec![hostname_record],
            })
        );

        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "network_name": "lab",
                "local_peer": identity.peer_id,
                "records": []
            }))
            .expect("legacy state"),
        )
        .expect("write legacy state");
        assert_eq!(
            store.load("lab", &identity.peer_id).expect("legacy load"),
            Some(PersistedMembershipStateData::default())
        );

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn membership_state_is_bound_to_network_and_local_peer() {
        let directory = test_directory("scope");
        let store = MembershipStateStore::new(directory.join("membership-state.json"));
        store
            .save("lab", "local-peer", &[membership_record()], &[])
            .expect("save state");

        assert!(matches!(
            store.load("other", "local-peer"),
            Err(MembershipStateStoreError::NetworkMismatch { .. })
        ));
        assert!(matches!(
            store.load("lab", "other-peer"),
            Err(MembershipStateStoreError::LocalPeerMismatch { .. })
        ));

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn membership_state_rejects_tampered_records() {
        let directory = test_directory("tampered");
        let path = directory.join("membership-state.json");
        let store = MembershipStateStore::new(&path);
        store
            .save("lab", "local-peer", &[membership_record()], &[])
            .expect("save state");
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read state")).expect("decode state");
        state["records"][0]["payload"]["sequence"] = serde_json::json!(2);
        fs::write(&path, serde_json::to_vec(&state).expect("encode state")).expect("tamper state");

        assert!(matches!(
            store.load("lab", "local-peer"),
            Err(MembershipStateStoreError::Membership(_))
        ));

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn membership_state_rejects_permissive_files_and_symlinks() {
        let directory = test_directory("unsafe-files");
        let path = directory.join("membership-state.json");
        fs::write(&path, b"{}").expect("state file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("permissive mode");
        let store = MembershipStateStore::new(&path);
        assert!(matches!(
            store.load("lab", "local-peer"),
            Err(MembershipStateStoreError::PermissiveMode { mode: 0o644 })
        ));

        fs::remove_file(&path).expect("remove state file");
        let target = directory.join("target.json");
        fs::write(&target, b"{}").expect("target file");
        symlink(&target, &path).expect("state symlink");
        assert!(matches!(
            store.load("lab", "local-peer"),
            Err(MembershipStateStoreError::UnsafeFile(_))
        ));

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn membership_state_rejects_oversized_files_before_decoding() {
        let directory = test_directory("oversized");
        let path = directory.join("membership-state.json");
        fs::write(&path, vec![b' '; MAX_MEMBERSHIP_STATE_BYTES + 1]).expect("oversized state");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("state mode");
        let store = MembershipStateStore::new(&path);

        assert!(matches!(
            store.load("lab", "local-peer"),
            Err(MembershipStateStoreError::TooLarge { .. })
        ));

        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
