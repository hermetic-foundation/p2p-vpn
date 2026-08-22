use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore as _};
use sha2_010::Sha256;

pub const MAX_PAIRING_STATE_BYTES: usize = 512 * 1024;
pub const PAIRING_MEMBERSHIP_KEY_FILE: &str = "membership.key";
const PAIRING_STATE_MAGIC: &[u8] = b"P2PVPN-PAIR-STATE";
const PAIRING_STATE_ENVELOPE_VERSION: u8 = 1;
const PAIRING_STATE_NONCE_BYTES: usize = 24;
const PAIRING_STATE_TAG_BYTES: usize = 16;
const PAIRING_STATE_KEY_BYTES: usize = 32;
const PAIRING_STATE_KDF_SALT: &[u8] = b"p2p-vpn pairing state encryption v1";
const MAX_PAIRING_STATE_FILE_BYTES: usize = MAX_PAIRING_STATE_BYTES
    + PAIRING_STATE_MAGIC.len()
    + 1
    + PAIRING_STATE_NONCE_BYTES
    + PAIRING_STATE_TAG_BYTES;

pub struct PairingStateStore {
    path: PathBuf,
    encryption_key: Option<[u8; PAIRING_STATE_KEY_BYTES]>,
}

impl std::fmt::Debug for PairingStateStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairingStateStore")
            .field("path", &self.path)
            .field("encrypted", &self.encryption_key.is_some())
            .finish()
    }
}

impl Drop for PairingStateStore {
    fn drop(&mut self) {
        if let Some(key) = self.encryption_key.as_mut() {
            key.fill(0);
        }
    }
}

impl PairingStateStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            encryption_key: None,
        }
    }

    pub fn encrypted(
        path: impl Into<PathBuf>,
        identity_private_key: &str,
        network_name: &str,
        local_peer: &str,
    ) -> Result<Self, PairingStateStoreError> {
        let hkdf = Hkdf::<Sha256>::new(
            Some(PAIRING_STATE_KDF_SALT),
            identity_private_key.as_bytes(),
        );
        let mut context = Vec::with_capacity(network_name.len() + local_peer.len() + 16);
        context.extend_from_slice(b"p2p-vpn-state\0");
        context.extend_from_slice(network_name.as_bytes());
        context.push(0);
        context.extend_from_slice(local_peer.as_bytes());
        let mut encryption_key = [0_u8; PAIRING_STATE_KEY_BYTES];
        hkdf.expand(&context, &mut encryption_key)
            .map_err(|_| PairingStateStoreError::InvalidEncryptionContext)?;
        Ok(Self {
            path: path.into(),
            encryption_key: Some(encryption_key),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<Vec<u8>>, PairingStateStoreError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(PairingStateStoreError::Io(error)),
        };
        validate_state_file(&self.path, &metadata)?;
        let length = usize::try_from(metadata.len())
            .map_err(|_| PairingStateStoreError::TooLarge { actual: usize::MAX })?;
        validate_stored_length(length)?;
        Ok(Some(self.open(&fs::read(&self.path)?)?))
    }

    pub fn save(&self, bytes: &[u8]) -> Result<(), PairingStateStoreError> {
        self.save_with_parent_sync(bytes, sync_parent_directory)
    }

    pub fn save_membership_key(
        &self,
        membership_key: &str,
    ) -> Result<PathBuf, PairingStateStoreError> {
        let parent = self
            .path
            .parent()
            .ok_or(PairingStateStoreError::MissingParent)?;
        let path = parent.join(PAIRING_MEMBERSHIP_KEY_FILE);
        let contents = format!("{membership_key}\n");
        let membership_store = Self::new(&path);
        if let Some(existing) = membership_store.load()? {
            if existing != contents.as_bytes() {
                return Err(PairingStateStoreError::MembershipKeyConflict);
            }
            return Ok(path);
        }
        membership_store.save(contents.as_bytes())?;
        Ok(path)
    }

    fn save_with_parent_sync(
        &self,
        bytes: &[u8],
        sync_parent: impl FnOnce(&Path) -> io::Result<()>,
    ) -> Result<(), PairingStateStoreError> {
        validate_length(bytes.len())?;
        let stored = self.seal(bytes)?;
        let parent = self
            .path
            .parent()
            .ok_or(PairingStateStoreError::MissingParent)?;
        let parent_metadata = fs::symlink_metadata(parent)?;
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(PairingStateStoreError::UnsafeParent(parent.to_path_buf()));
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.path) {
            validate_state_file(&self.path, &metadata)?;
        }

        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(PairingStateStoreError::InvalidFileName)?;
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
            .and_then(|()| temporary.write_all(&stored))
            .and_then(|()| temporary.sync_all())
        {
            drop(temporary);
            let _ = fs::remove_file(&temporary_path);
            return Err(PairingStateStoreError::Io(error));
        }
        drop(temporary);
        if let Err(error) = fs::rename(&temporary_path, &self.path) {
            let _ = fs::remove_file(&temporary_path);
            return Err(PairingStateStoreError::Io(error));
        }
        sync_parent(parent)?;
        Ok(())
    }

    fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, PairingStateStoreError> {
        let Some(key) = self.encryption_key.as_ref() else {
            return Ok(plaintext.to_vec());
        };
        let mut nonce = [0_u8; PAIRING_STATE_NONCE_BYTES];
        OsRng.fill_bytes(&mut nonce);
        let mut header = Vec::with_capacity(PAIRING_STATE_MAGIC.len() + 1);
        header.extend_from_slice(PAIRING_STATE_MAGIC);
        header.push(PAIRING_STATE_ENVELOPE_VERSION);
        let ciphertext = XChaCha20Poly1305::new(key.into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &header,
                },
            )
            .map_err(|_| PairingStateStoreError::EncryptionFailed)?;
        let mut stored = Vec::with_capacity(header.len() + nonce.len() + ciphertext.len());
        stored.extend_from_slice(&header);
        stored.extend_from_slice(&nonce);
        stored.extend_from_slice(&ciphertext);
        Ok(stored)
    }

    fn open(&self, stored: &[u8]) -> Result<Vec<u8>, PairingStateStoreError> {
        if !stored.starts_with(PAIRING_STATE_MAGIC) {
            validate_length(stored.len())?;
            return Ok(stored.to_vec());
        }
        let key = self
            .encryption_key
            .as_ref()
            .ok_or(PairingStateStoreError::EncryptionKeyRequired)?;
        let header_len = PAIRING_STATE_MAGIC.len() + 1;
        let minimum_len = header_len + PAIRING_STATE_NONCE_BYTES + PAIRING_STATE_TAG_BYTES;
        if stored.len() < minimum_len {
            return Err(PairingStateStoreError::InvalidEncryptedState);
        }
        if stored[PAIRING_STATE_MAGIC.len()] != PAIRING_STATE_ENVELOPE_VERSION {
            return Err(PairingStateStoreError::UnsupportedEncryptionVersion(
                stored[PAIRING_STATE_MAGIC.len()],
            ));
        }
        let nonce_end = header_len + PAIRING_STATE_NONCE_BYTES;
        let plaintext = XChaCha20Poly1305::new(key.into())
            .decrypt(
                XNonce::from_slice(&stored[header_len..nonce_end]),
                Payload {
                    msg: &stored[nonce_end..],
                    aad: &stored[..header_len],
                },
            )
            .map_err(|_| PairingStateStoreError::DecryptionFailed)?;
        validate_length(plaintext.len())?;
        Ok(plaintext)
    }

    pub fn remove(&self) -> Result<(), PairingStateStoreError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PairingStateStoreError::Io(error)),
        }
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
    // Some Unix filesystems reject directory fsync with EINVAL rather than ENOTSUP.
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
    ) {
        Ok(())
    } else {
        Err(error)
    }
}

fn validate_state_file(path: &Path, metadata: &fs::Metadata) -> Result<(), PairingStateStoreError> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(PairingStateStoreError::UnsafeFile(path.to_path_buf()));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(PairingStateStoreError::PermissiveMode { mode });
    }
    Ok(())
}

fn validate_length(length: usize) -> Result<(), PairingStateStoreError> {
    if length > MAX_PAIRING_STATE_BYTES {
        Err(PairingStateStoreError::TooLarge { actual: length })
    } else {
        Ok(())
    }
}

fn validate_stored_length(length: usize) -> Result<(), PairingStateStoreError> {
    if length > MAX_PAIRING_STATE_FILE_BYTES {
        Err(PairingStateStoreError::TooLarge { actual: length })
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum PairingStateStoreError {
    Io(io::Error),
    MissingParent,
    UnsafeParent(PathBuf),
    UnsafeFile(PathBuf),
    PermissiveMode { mode: u32 },
    InvalidFileName,
    TooLarge { actual: usize },
    InvalidEncryptionContext,
    EncryptionFailed,
    EncryptionKeyRequired,
    InvalidEncryptedState,
    UnsupportedEncryptionVersion(u8),
    DecryptionFailed,
    MembershipKeyConflict,
}

impl std::fmt::Display for PairingStateStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "pairing state I/O failed: {error}"),
            Self::MissingParent => formatter.write_str("pairing state path has no parent"),
            Self::UnsafeParent(path) => write!(
                formatter,
                "pairing state parent must be a real directory: {}",
                path.display()
            ),
            Self::UnsafeFile(path) => write!(
                formatter,
                "pairing state must be a regular file: {}",
                path.display()
            ),
            Self::PermissiveMode { mode } => write!(
                formatter,
                "pairing state has mode {mode:04o}; expected owner-only permissions"
            ),
            Self::InvalidFileName => {
                formatter.write_str("pairing state path has no UTF-8 file name")
            }
            Self::TooLarge { actual } => write!(
                formatter,
                "pairing state size {actual} exceeds limit {MAX_PAIRING_STATE_BYTES}"
            ),
            Self::InvalidEncryptionContext => {
                formatter.write_str("pairing state encryption context is invalid")
            }
            Self::EncryptionFailed => formatter.write_str("pairing state encryption failed"),
            Self::EncryptionKeyRequired => {
                formatter.write_str("encrypted pairing state requires the matching identity key")
            }
            Self::InvalidEncryptedState => {
                formatter.write_str("encrypted pairing state envelope is truncated")
            }
            Self::UnsupportedEncryptionVersion(version) => write!(
                formatter,
                "unsupported pairing state encryption version {version}"
            ),
            Self::DecryptionFailed => formatter
                .write_str("pairing state authentication failed for this identity and network"),
            Self::MembershipKeyConflict => formatter
                .write_str("managed pairing membership key already contains different material"),
        }
    }
}

impl std::error::Error for PairingStateStoreError {}

impl From<io::Error> for PairingStateStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        os::unix::fs::{PermissionsExt as _, symlink},
    };

    use super::*;

    fn test_directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-pairing-state-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("test directory");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("directory mode");
        path
    }

    #[test]
    fn pairing_state_round_trips_with_owner_only_permissions() {
        let directory = test_directory("round-trip");
        let store = PairingStateStore::new(directory.join("pairing.json"));

        store.save(b"secret state").expect("save");

        assert_eq!(store.load().expect("load"), Some(b"secret state".to_vec()));
        assert_eq!(
            fs::metadata(store.path())
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn encrypted_pairing_state_round_trips_without_plaintext_at_rest() {
        let directory = test_directory("encrypted-round-trip");
        let store = PairingStateStore::encrypted(
            directory.join("pairing.json"),
            "private-identity-material",
            "runner-mesh",
            "local-peer",
        )
        .expect("encrypted store");
        let plaintext = b"one-time-code and membership-secret";

        store.save(plaintext).expect("save encrypted state");

        let stored = fs::read(store.path()).expect("stored envelope");
        assert!(stored.starts_with(PAIRING_STATE_MAGIC));
        assert!(
            !stored
                .windows(plaintext.len())
                .any(|window| window == plaintext)
        );
        assert_eq!(store.load().expect("load"), Some(plaintext.to_vec()));
        assert!(!format!("{store:?}").contains("private-identity-material"));

        let mut tampered = stored;
        *tampered.last_mut().expect("authenticated ciphertext") ^= 1;
        fs::write(store.path(), tampered).expect("tampered envelope");
        assert!(matches!(
            store.load(),
            Err(PairingStateStoreError::DecryptionFailed)
        ));
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn encrypted_pairing_state_is_bound_to_identity_and_network() {
        let directory = test_directory("encrypted-binding");
        let path = directory.join("pairing.json");
        let store = PairingStateStore::encrypted(
            &path,
            "private-identity-material",
            "runner-mesh",
            "local-peer",
        )
        .expect("encrypted store");
        store.save(b"durable state").expect("save encrypted state");

        for (identity, network, peer) in [
            ("other-identity", "runner-mesh", "local-peer"),
            ("private-identity-material", "other-network", "local-peer"),
            ("private-identity-material", "runner-mesh", "other-peer"),
        ] {
            let mismatched = PairingStateStore::encrypted(&path, identity, network, peer)
                .expect("mismatched store");
            assert!(matches!(
                mismatched.load(),
                Err(PairingStateStoreError::DecryptionFailed)
            ));
        }
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn encrypted_pairing_state_migrates_owner_only_plaintext_on_save() {
        let directory = test_directory("encrypted-migration");
        let path = directory.join("pairing.json");
        fs::write(&path, b"legacy state").expect("legacy state");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("legacy mode");
        let store = PairingStateStore::encrypted(
            &path,
            "private-identity-material",
            "runner-mesh",
            "local-peer",
        )
        .expect("encrypted store");

        assert_eq!(
            store.load().expect("legacy load"),
            Some(b"legacy state".to_vec())
        );
        store.save(b"legacy state").expect("encrypted rewrite");
        assert!(
            fs::read(&path)
                .expect("rewritten state")
                .starts_with(PAIRING_STATE_MAGIC)
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn unencrypted_pairing_store_rejects_encrypted_envelope() {
        let directory = test_directory("encrypted-key-required");
        let path = directory.join("pairing.json");
        PairingStateStore::encrypted(
            &path,
            "private-identity-material",
            "runner-mesh",
            "local-peer",
        )
        .expect("encrypted store")
        .save(b"durable state")
        .expect("save encrypted state");

        assert!(matches!(
            PairingStateStore::new(&path).load(),
            Err(PairingStateStoreError::EncryptionKeyRequired)
        ));
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn pairing_state_syncs_parent_after_owner_only_atomic_replace() {
        let directory = test_directory("sync-order");
        let path = directory.join("pairing.json");
        fs::write(&path, b"old state").expect("old state");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("old mode");
        let store = PairingStateStore::new(&path);
        let sync_calls = Cell::new(0);

        store
            .save_with_parent_sync(b"new state", |parent| {
                sync_calls.set(sync_calls.get() + 1);
                assert_eq!(parent, directory);
                assert_eq!(fs::read(&path).expect("replacement"), b"new state");
                assert_eq!(
                    fs::metadata(&path)
                        .expect("replacement metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
                let entries = fs::read_dir(parent)
                    .expect("parent entries")
                    .collect::<Result<Vec<_>, _>>()
                    .expect("read parent entries");
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].path(), path);
                Ok(())
            })
            .expect("save");

        assert_eq!(sync_calls.get(), 1);
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn pairing_store_materializes_membership_key_without_exposing_permissions() {
        let directory = test_directory("membership-key");
        let store = PairingStateStore::new(directory.join("pairing-state.json"));

        let path = store
            .save_membership_key("private-membership-key")
            .expect("save membership key");

        assert_eq!(path, directory.join(PAIRING_MEMBERSHIP_KEY_FILE));
        assert_eq!(
            fs::read_to_string(&path).expect("read membership key"),
            "private-membership-key\n"
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("membership key metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            store
                .save_membership_key("private-membership-key")
                .expect("idempotent membership key"),
            path
        );
        assert!(matches!(
            store.save_membership_key("different-membership-key"),
            Err(PairingStateStoreError::MembershipKeyConflict)
        ));
        assert_eq!(
            fs::read_to_string(&path).expect("unchanged membership key"),
            "private-membership-key\n"
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn pairing_state_ignores_only_unsupported_directory_sync_errors() {
        assert!(handle_directory_sync_error(io::Error::from(io::ErrorKind::Unsupported)).is_ok());
        assert!(handle_directory_sync_error(io::Error::from(io::ErrorKind::InvalidInput)).is_ok());

        let error = handle_directory_sync_error(io::Error::from(io::ErrorKind::PermissionDenied))
            .expect_err("permission failure must remain visible");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn pairing_state_reports_parent_sync_failure_after_replace() {
        let directory = test_directory("sync-failure");
        let path = directory.join("pairing.json");
        let store = PairingStateStore::new(&path);

        let error = store
            .save_with_parent_sync(b"new state", |_| {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            })
            .expect_err("sync failure");

        assert!(matches!(
            error,
            PairingStateStoreError::Io(error)
                if error.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(
            store.load().expect("load replacement"),
            Some(b"new state".to_vec())
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn pairing_state_rejects_permissive_existing_file() {
        let directory = test_directory("mode");
        let path = directory.join("pairing.json");
        fs::write(&path, b"state").expect("state");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("mode");
        let store = PairingStateStore::new(&path);

        assert!(matches!(
            store.load(),
            Err(PairingStateStoreError::PermissiveMode { mode: 0o644 })
        ));
        assert!(matches!(
            store.save(b"replacement"),
            Err(PairingStateStoreError::PermissiveMode { mode: 0o644 })
        ));
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn pairing_state_rejects_symlink() {
        let directory = test_directory("symlink");
        let target = directory.join("target");
        let path = directory.join("pairing.json");
        fs::write(&target, b"state").expect("target");
        symlink(&target, &path).expect("symlink");
        let store = PairingStateStore::new(&path);

        assert!(matches!(
            store.load(),
            Err(PairingStateStoreError::UnsafeFile(_))
        ));
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn pairing_state_rejects_oversized_payload() {
        let directory = test_directory("size");
        let store = PairingStateStore::new(directory.join("pairing.json"));

        assert!(matches!(
            store.save(&vec![0; MAX_PAIRING_STATE_BYTES + 1]),
            Err(PairingStateStoreError::TooLarge { .. })
        ));
        fs::remove_dir_all(directory).expect("cleanup");
    }
}
