#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::{
    fs::{self, File},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore as _};
use sha2_010::{Digest as _, Sha256};

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
    last_durable: Mutex<Option<DurablePairingState>>,
}

struct DurablePairingState {
    plaintext_digest: [u8; 32],
    stored_digest: [u8; 32],
    metadata: fs::Metadata,
    directory: fs::Metadata,
    #[cfg(windows)]
    file_identity: same_file::Handle,
    #[cfg(windows)]
    directory_identity: same_file::Handle,
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
            last_durable: Mutex::new(None),
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
            last_durable: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<Vec<u8>>, PairingStateStoreError> {
        let _write_owner = super::state_write_cleanup::lock_and_retire(&self.path)?;
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
        sync_parent: impl FnOnce(&Path) -> io::Result<bool>,
    ) -> Result<(), PairingStateStoreError> {
        let mut last_durable = self.lock_durable_state();
        let previous = last_durable.take();
        validate_length(bytes.len())?;
        let parent = self
            .path
            .parent()
            .ok_or(PairingStateStoreError::MissingParent)?;
        let parent_metadata = fs::symlink_metadata(parent)?;
        if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
            return Err(PairingStateStoreError::UnsafeParent(parent.to_path_buf()));
        }
        let write_owner = super::state_write_cleanup::lock_and_retire(&self.path)?;
        #[cfg(unix)]
        let directory = write_owner.as_ref().and_then(|owner| owner.metadata().ok());
        #[cfg(windows)]
        let directory = {
            let _ = &write_owner;
            fs::metadata(parent).ok()
        };
        if let Ok(metadata) = fs::symlink_metadata(&self.path) {
            validate_state_file(&self.path, &metadata)?;
        }

        let plaintext_digest = Sha256::digest(bytes).into();
        // A read proves visibility, not durability. Only a completed save can seed this hint.
        if previous.as_ref().is_some_and(|durable| {
            directory
                .as_ref()
                .is_some_and(|metadata| same_directory_owner(metadata, &durable.directory))
                && durable.plaintext_digest == plaintext_digest
                && self.matches_durable_file(durable, parent).unwrap_or(false)
        }) {
            *last_durable = previous;
            return Ok(());
        }
        let stored = self.seal(bytes)?;

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
        let mut temporary = crate::private_fs::create_private_file(&temporary_path)?;
        if let Err(error) = temporary
            .write_all(&stored)
            .and_then(|()| temporary.sync_all())
        {
            drop(temporary);
            let _ = fs::remove_file(&temporary_path);
            return Err(PairingStateStoreError::Io(error));
        }
        if let Err(error) = fs::rename(&temporary_path, &self.path) {
            drop(temporary);
            let _ = fs::remove_file(&temporary_path);
            return Err(PairingStateStoreError::Io(error));
        }
        let parent_synced = sync_parent(parent)?;
        if parent_synced && let (Some(directory), Ok(metadata)) = (directory, temporary.metadata())
        {
            let captured = (|| -> io::Result<DurablePairingState> {
                Ok(DurablePairingState {
                    plaintext_digest,
                    stored_digest: Sha256::digest(&stored).into(),
                    metadata,
                    directory,
                    #[cfg(windows)]
                    file_identity: same_file::Handle::from_file(temporary.try_clone()?)?,
                    #[cfg(windows)]
                    directory_identity: same_file::Handle::from_path(parent)?,
                })
            })();
            *last_durable = captured.ok();
        }
        Ok(())
    }

    fn lock_durable_state(&self) -> MutexGuard<'_, Option<DurablePairingState>> {
        self.last_durable.lock().unwrap_or_else(|error| {
            let mut state = error.into_inner();
            *state = None;
            self.last_durable.clear_poison();
            state
        })
    }

    fn matches_durable_file(
        &self,
        durable: &DurablePairingState,
        parent: &Path,
    ) -> io::Result<bool> {
        #[cfg(unix)]
        let file = File::from(rustix::fs::open(
            &self.path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?);
        #[cfg(windows)]
        let file = File::open(&self.path)?;
        #[cfg(windows)]
        if same_file::Handle::from_file(file.try_clone()?)? != durable.file_identity
            || same_file::Handle::from_path(parent)? != durable.directory_identity
        {
            return Ok(false);
        }
        if !same_file_version(&file.metadata()?, &durable.metadata) {
            return Ok(false);
        }
        // Also check ciphertext, including on filesystems with coarse timestamps.
        let mut reader = file.take(durable.metadata.len().saturating_add(1));
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 4096];
        let mut length = 0_u64;
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            length += read as u64;
            digest.update(&buffer[..read]);
        }
        let stored_digest: [u8; 32] = digest.finalize().into();
        let directory = fs::symlink_metadata(parent)?;
        Ok(length == durable.metadata.len()
            && stored_digest == durable.stored_digest
            && same_file_version(&reader.get_ref().metadata()?, &durable.metadata)
            && same_file_version(&fs::symlink_metadata(&self.path)?, &durable.metadata)
            && same_directory_owner(&directory, &durable.directory))
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
        let mut last_durable = self.lock_durable_state();
        *last_durable = None;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PairingStateStoreError::Io(error)),
        }
    }
}

#[cfg(unix)]
fn same_file_version(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.len() == right.len()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.nlink() == right.nlink()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}

#[cfg(unix)]
fn same_directory_owner(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<bool> {
    let directory = match File::open(parent) {
        Ok(directory) => directory,
        Err(error) => return handle_directory_sync_error(error),
    };
    match directory.sync_all() {
        Ok(()) => Ok(true),
        Err(error) => handle_directory_sync_error(error),
    }
}

#[cfg(unix)]
fn handle_directory_sync_error(error: io::Error) -> io::Result<bool> {
    // Some Unix filesystems reject directory fsync with EINVAL rather than ENOTSUP.
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
    ) {
        Ok(false)
    } else {
        Err(error)
    }
}

fn validate_state_file(path: &Path, metadata: &fs::Metadata) -> Result<(), PairingStateStoreError> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(PairingStateStoreError::UnsafeFile(path.to_path_buf()));
    }
    #[cfg(windows)]
    crate::private_fs::validate_private(path)?;
    #[cfg(unix)]
    {
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(PairingStateStoreError::PermissiveMode { mode });
        }
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

#[cfg(all(test, unix))]
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

    fn encrypted_store(directory: &Path) -> PairingStateStore {
        PairingStateStore::encrypted(
            directory.join("pairing.json"),
            "private-identity-material",
            "runner-mesh",
            "local-peer",
        )
        .unwrap()
    }

    #[test]
    fn unchanged_saves_retain_the_durable_file_without_another_sync() {
        for encrypted in [false, true] {
            let directory = test_directory(if encrypted {
                "same-encrypted"
            } else {
                "same-plain"
            });
            let store = if encrypted {
                encrypted_store(&directory)
            } else {
                PairingStateStore::new(directory.join("pairing.json"))
            };
            store.save(b"durable state").unwrap();
            let initial = fs::read(store.path()).unwrap();
            let metadata = fs::metadata(store.path()).unwrap();
            let syncs = Cell::new(0);
            for _ in 0..32 {
                store
                    .save_with_parent_sync(b"durable state", |parent| {
                        syncs.set(syncs.get() + 1);
                        sync_parent_directory(parent)
                    })
                    .unwrap();
            }
            assert_eq!(syncs.get(), 0);
            assert_eq!(fs::read(store.path()).unwrap(), initial);
            assert!(same_file_version(
                &fs::metadata(store.path()).unwrap(),
                &metadata
            ));
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn changed_payloads_and_rollbacks_each_commit_before_becoming_reusable() {
        let directory = test_directory("changed-payloads");
        let store = encrypted_store(&directory);
        let syncs = Cell::new(0);
        for state in [b"state A", b"state A", b"state B", b"state B", b"state A"] {
            store
                .save_with_parent_sync(state, |parent| {
                    syncs.set(syncs.get() + 1);
                    sync_parent_directory(parent)
                })
                .unwrap();
            assert_eq!(store.load().unwrap().unwrap(), state);
        }
        assert_eq!(syncs.get(), 3);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn visible_state_after_failed_sync_requires_a_successful_retry() {
        let directory = test_directory("visible-not-durable");
        let store = encrypted_store(&directory);
        store.save(b"state A").unwrap();
        assert!(
            store
                .save_with_parent_sync(b"state B", |_| {
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                })
                .is_err()
        );
        assert_eq!(store.load().unwrap().unwrap(), b"state B");
        assert!(store.lock_durable_state().is_none());
        let syncs = Cell::new(0);
        for state in [b"state B", b"state B", b"state A"] {
            store
                .save_with_parent_sync(state, |parent| {
                    syncs.set(syncs.get() + 1);
                    sync_parent_directory(parent)
                })
                .unwrap();
        }
        assert_eq!(syncs.get(), 2);
        assert_eq!(store.load().unwrap().unwrap(), b"state A");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn loads_and_new_store_owners_do_not_establish_durability() {
        let directory = test_directory("new-owner");
        let first = encrypted_store(&directory);
        first.save(b"state").unwrap();
        let second = encrypted_store(&directory);
        assert_eq!(second.load().unwrap().unwrap(), b"state");
        assert!(second.lock_durable_state().is_none());
        let syncs = Cell::new(0);
        for store in [&second, &second, &first] {
            store
                .save_with_parent_sync(b"state", |parent| {
                    syncs.set(syncs.get() + 1);
                    sync_parent_directory(parent)
                })
                .unwrap();
        }
        assert_eq!(syncs.get(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn ciphertext_is_checked_even_when_file_metadata_matches() {
        let directory = test_directory("coarse-metadata");
        let store = encrypted_store(&directory);
        store.save(b"state").unwrap();
        let mut damaged = fs::read(store.path()).unwrap();
        *damaged.last_mut().unwrap() ^= 1;
        fs::write(store.path(), damaged).unwrap();
        // Model a filesystem where an in-place change has indistinguishable timestamps.
        store.lock_durable_state().as_mut().unwrap().metadata = fs::metadata(store.path()).unwrap();
        let syncs = Cell::new(0);
        store
            .save_with_parent_sync(b"state", |parent| {
                syncs.set(syncs.get() + 1);
                sync_parent_directory(parent)
            })
            .unwrap();
        assert_eq!(syncs.get(), 1);
        assert_eq!(store.load().unwrap().unwrap(), b"state");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn durable_read_hint_does_not_follow_symbolic_links() {
        let directory = test_directory("hint-no-follow");
        let store = encrypted_store(&directory);
        store.save(b"state").unwrap();
        let target = directory.join("target");
        fs::rename(store.path(), &target).unwrap();
        symlink(&target, store.path()).unwrap();
        {
            let hint = store.lock_durable_state();
            assert!(
                store
                    .matches_durable_file(hint.as_ref().unwrap(), &directory)
                    .is_err()
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_replaced_parent_directory_requires_a_new_commit() {
        let directory = test_directory("changed-parent");
        let displaced = directory.with_extension("old");
        let store = encrypted_store(&directory);
        store.save(b"state").unwrap();
        fs::rename(&directory, &displaced).unwrap();
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::rename(displaced.join("pairing.json"), store.path()).unwrap();
        // Isolate directory identity from the inode's separately checked timestamps.
        store.lock_durable_state().as_mut().unwrap().metadata = fs::metadata(store.path()).unwrap();
        let syncs = Cell::new(0);
        store
            .save_with_parent_sync(b"state", |parent| {
                syncs.set(syncs.get() + 1);
                sync_parent_directory(parent)
            })
            .unwrap();
        assert_eq!(syncs.get(), 1);
        fs::remove_dir_all(directory).unwrap();
        fs::remove_dir(displaced).unwrap();
    }

    #[test]
    fn parent_permission_changes_invalidate_the_durable_hint() {
        let directory = test_directory("parent-permissions");
        let store = encrypted_store(&directory);
        store.save(b"state").unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
        {
            let hint = store.lock_durable_state();
            assert!(
                !store
                    .matches_durable_file(hint.as_ref().unwrap(), &directory)
                    .unwrap()
            );
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn externally_replaced_or_deleted_files_are_committed_again() {
        let directory = test_directory("external-replacement");
        let store = encrypted_store(&directory);
        store.save(b"state").unwrap();
        let replacement = directory.join("replacement");
        fs::write(&replacement, fs::read(store.path()).unwrap()).unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
        fs::rename(replacement, store.path()).unwrap();
        let syncs = Cell::new(0);
        store
            .save_with_parent_sync(b"state", |parent| {
                syncs.set(syncs.get() + 1);
                sync_parent_directory(parent)
            })
            .unwrap();
        fs::remove_file(store.path()).unwrap();
        store
            .save_with_parent_sync(b"state", |parent| {
                syncs.set(syncs.get() + 1);
                sync_parent_directory(parent)
            })
            .unwrap();
        assert_eq!(syncs.get(), 2);
        assert_eq!(store.load().unwrap().unwrap(), b"state");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reusable_state_does_not_bypass_permissions_or_symlink_checks() {
        let directory = test_directory("cached-safety");
        let store = encrypted_store(&directory);
        store.save(b"state").unwrap();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            store.save(b"state"),
            Err(PairingStateStoreError::PermissiveMode { .. })
        ));
        assert!(store.lock_durable_state().is_none());
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
        store.save(b"state").unwrap();
        let target = directory.join("target");
        fs::rename(store.path(), &target).unwrap();
        symlink(&target, store.path()).unwrap();
        assert!(matches!(
            store.save(b"state"),
            Err(PairingStateStoreError::UnsafeFile(_))
        ));
        assert!(store.lock_durable_state().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn owner_only_permission_changes_are_repaired_before_reuse() {
        let directory = test_directory("owner-only-changed-mode");
        let store = encrypted_store(&directory);
        for mode in [0o400, 0o200] {
            store.save(b"state").unwrap();
            fs::set_permissions(store.path(), fs::Permissions::from_mode(mode)).unwrap();
            let syncs = Cell::new(0);
            store
                .save_with_parent_sync(b"state", |parent| {
                    syncs.set(syncs.get() + 1);
                    sync_parent_directory(parent)
                })
                .unwrap();
            assert_eq!(syncs.get(), 1);
            assert_eq!(fs::metadata(store.path()).unwrap().mode() & 0o777, 0o600);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_poisoned_hint_is_discarded_instead_of_acknowledging_an_old_commit() {
        let directory = test_directory("poisoned-hint");
        let store = encrypted_store(&directory);
        store.save(b"state").unwrap();
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _hint = store.lock_durable_state();
                panic!("interrupted save owner");
            }))
            .is_err()
        );
        let syncs = Cell::new(0);
        for _ in 0..2 {
            store
                .save_with_parent_sync(b"state", |parent| {
                    syncs.set(syncs.get() + 1);
                    sync_parent_directory(parent)
                })
                .unwrap();
        }
        assert_eq!(syncs.get(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_identical_saves_share_one_completed_commit() {
        use std::sync::{
            Arc, Barrier,
            atomic::{AtomicUsize, Ordering},
        };

        let directory = test_directory("concurrent-same-state");
        let store = Arc::new(encrypted_store(&directory));
        let ready = Arc::new(Barrier::new(8));
        let syncs = Arc::new(AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let store = Arc::clone(&store);
                let ready = Arc::clone(&ready);
                let syncs = Arc::clone(&syncs);
                scope.spawn(move || {
                    ready.wait();
                    store
                        .save_with_parent_sync(b"state", |parent| {
                            syncs.fetch_add(1, Ordering::SeqCst);
                            sync_parent_directory(parent)
                        })
                        .unwrap();
                });
            }
        });
        assert_eq!(syncs.load(Ordering::SeqCst), 1);
        assert_eq!(store.load().unwrap().unwrap(), b"state");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unprotected_legacy_directories_never_skip_a_save() {
        let directory = test_directory("unprotected-directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        let store = encrypted_store(&directory);
        let syncs = Cell::new(0);
        for _ in 0..2 {
            store
                .save_with_parent_sync(b"state", |parent| {
                    syncs.set(syncs.get() + 1);
                    sync_parent_directory(parent)
                })
                .unwrap();
        }
        assert_eq!(syncs.get(), 2);
        assert!(store.lock_durable_state().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unsupported_parent_sync_remains_successful_but_never_seeds_a_durable_hint() {
        let directory = test_directory("unsupported-parent-sync");
        let store = encrypted_store(&directory);
        let syncs = Cell::new(0);
        for _ in 0..2 {
            store
                .save_with_parent_sync(b"state", |_| {
                    syncs.set(syncs.get() + 1);
                    Ok(false)
                })
                .unwrap();
            assert!(store.lock_durable_state().is_none());
            assert_eq!(store.load().unwrap().unwrap(), b"state");
        }
        assert_eq!(syncs.get(), 2);
        store.save(b"state").unwrap();
        store
            .save_with_parent_sync(b"state", |_| {
                syncs.set(syncs.get() + 1);
                Ok(false)
            })
            .unwrap();
        assert_eq!(syncs.get(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn removing_state_invalidates_the_previous_commit() {
        let directory = test_directory("remove-invalidates");
        let store = encrypted_store(&directory);
        store.save(b"state").unwrap();
        store.remove().unwrap();
        assert!(store.lock_durable_state().is_none());
        store.save(b"state").unwrap();
        assert_eq!(store.load().unwrap().unwrap(), b"state");
        fs::remove_dir_all(directory).unwrap();
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
                Ok(true)
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
        assert!(!handle_directory_sync_error(io::Error::from(io::ErrorKind::Unsupported)).unwrap());
        assert!(
            !handle_directory_sync_error(io::Error::from(io::ErrorKind::InvalidInput)).unwrap()
        );

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

#[cfg(windows)]
fn same_file_version(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    left.len() == right.len()
        && left.file_attributes() == right.file_attributes()
        && left.creation_time() == right.creation_time()
        && left.last_write_time() == right.last_write_time()
}
#[cfg(windows)]
fn same_directory_owner(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    left.file_attributes() == right.file_attributes()
        && left.creation_time() == right.creation_time()
}
#[cfg(windows)]
fn sync_parent_directory(parent: &Path) -> io::Result<bool> {
    crate::private_fs::windows::sync_parent_volume(parent)?;
    Ok(true)
}
