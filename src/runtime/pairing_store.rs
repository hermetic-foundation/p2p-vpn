use std::{
    fs::{self, OpenOptions},
    io::{self, Write as _},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use rand_core::{OsRng, RngCore as _};

pub const MAX_PAIRING_STATE_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingStateStore {
    path: PathBuf,
}

impl PairingStateStore {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
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
        validate_length(length)?;
        Ok(Some(fs::read(&self.path)?))
    }

    pub fn save(&self, bytes: &[u8]) -> Result<(), PairingStateStoreError> {
        validate_length(bytes.len())?;
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
            .write_all(bytes)
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
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    pub fn remove(&self) -> Result<(), PairingStateStoreError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PairingStateStoreError::Io(error)),
        }
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

#[derive(Debug)]
pub enum PairingStateStoreError {
    Io(io::Error),
    MissingParent,
    UnsafeParent(PathBuf),
    UnsafeFile(PathBuf),
    PermissiveMode { mode: u32 },
    InvalidFileName,
    TooLarge { actual: usize },
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
    use std::os::unix::fs::{PermissionsExt as _, symlink};

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
