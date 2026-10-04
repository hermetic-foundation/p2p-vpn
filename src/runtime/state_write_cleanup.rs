//! Retire abandoned atomic-write copies, never recover authority from them.

use std::{
    fs::{self, File},
    io,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

/// Hold the protected directory's advisory lock through the caller's read/write.
/// Current stores share this owner; no extra lock file or history is retained.
pub(super) fn lock_and_retire(path: &Path) -> io::Result<Option<File>> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    let metadata = match fs::symlink_metadata(parent) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    // Legacy stores may live in shared directories. Never sweep those directories.
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Ok(None);
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let directory = File::open(parent)?;
    directory.lock()?;
    let opened = directory.metadata()?;
    if metadata.dev() != opened.dev()
        || metadata.ino() != opened.ino()
        || opened.permissions().mode() & 0o022 != 0
    {
        return Err(io::Error::other("state directory changed"));
    }
    let prefix = format!(".{name}.");
    let mut retired = false;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let filename = entry.file_name();
        let Some(suffix) = filename
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
        else {
            continue;
        };
        if !writer_suffix(suffix) {
            continue;
        }
        let candidate = fs::symlink_metadata(entry.path())?;
        if !candidate.is_file()
            || candidate.file_type().is_symlink()
            || candidate.nlink() != 1
            || candidate.uid() != opened.uid()
            || candidate.permissions().mode() & 0o7177 != 0
        {
            return Err(io::Error::other(
                "unsafe abandoned state write; inspect protected directory",
            ));
        }
        fs::remove_file(entry.path())?;
        retired = true;
    }
    if retired {
        directory.sync_all()?;
    }
    Ok(Some(directory))
}

fn writer_suffix(suffix: &str) -> bool {
    let Some((pid, nonce)) = suffix.split_once('.') else {
        return false;
    };
    pid.parse::<u32>()
        .is_ok_and(|value| value != 0 && value.to_string() == pid)
        && nonce
            .parse::<u64>()
            .is_ok_and(|value| value.to_string() == nonce)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write as _,
        os::unix::fs::{OpenOptionsExt as _, symlink},
        path::PathBuf,
        process::Command,
    };

    const CHILD: &str = "P2P_VPN_STATE_WRITE_CRASH_CHILD";
    const TEST: &str = "runtime::state_write_cleanup::tests::crashed_writes_retire_without_recovering_uncommitted_authority";

    fn directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-write-cleanup-{}-{label}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn private(path: &Path, bytes: &[u8]) {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    #[test]
    fn crashed_writes_retire_without_recovering_uncommitted_authority() {
        if let Ok(stage) = std::env::var(CHILD) {
            let path = PathBuf::from(std::env::var("P2P_VPN_STATE_WRITE_PATH").unwrap());
            let _owner = lock_and_retire(&path).unwrap();
            let temporary = path
                .parent()
                .unwrap()
                .join(format!(".state.json.{}.1", std::process::id()));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
                .unwrap();
            if stage != "created" {
                file.write_all(b"replacement authority").unwrap();
            }
            if stage == "synced" || stage == "renamed" {
                file.sync_all().unwrap();
            }
            if stage == "renamed" {
                fs::rename(temporary, &path).unwrap();
            }
            // Process exit skips Rust destructors; the OS closes descriptors and locks.
            std::process::exit(17);
        }
        for stage in ["created", "written", "synced", "renamed"] {
            let directory = directory(stage);
            let path = directory.join("state.json");
            private(&path, b"selected authority");
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST])
                .env(CHILD, stage)
                .env("P2P_VPN_STATE_WRITE_PATH", &path)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(17));
            let owner = lock_and_retire(&path).unwrap();
            let expected: &[u8] = if stage == "renamed" {
                b"replacement authority"
            } else {
                b"selected authority"
            };
            assert_eq!(fs::read(&path).unwrap(), expected);
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
            drop(owner);
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn cleanup_leaves_unrelated_and_malformed_names_untouched() {
        let directory = directory("scope");
        let path = directory.join("state.json");
        for name in [
            "state.json",
            ".state.json.backup",
            ".state.json.0.1",
            ".state.json.01.1",
            ".state.json.1.1.extra",
            ".other.json.1.1",
        ] {
            private(&directory.join(name), b"unrelated");
        }
        private(&directory.join(".state.json.123.456"), b"abandoned");
        let owner = lock_and_retire(&path).unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 6);
        assert!(!directory.join(".state.json.123.456").exists());
        drop(owner);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn repeated_private_residue_retirement_does_not_accumulate_files() {
        let directory = directory("repeated");
        let path = directory.join("state.json");
        private(&path, b"selected");
        for index in 0..64 {
            let residue = directory.join(format!(".state.json.123.{index}"));
            private(&residue, b"obsolete profile");
            fs::set_permissions(
                &residue,
                fs::Permissions::from_mode([0, 0o200, 0o400, 0o600][index % 4]),
            )
            .unwrap();
            let owner = lock_and_retire(&path).unwrap();
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
            assert_eq!(fs::read(&path).unwrap(), b"selected");
            drop(owner);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn live_writer_keeps_its_temporary_copy_until_atomic_replacement() {
        use std::{sync::mpsc, thread, time::Duration};

        let directory = directory("live-writer");
        let path = directory.join("state.json");
        private(&path, b"old selected authority");
        let owner = lock_and_retire(&path).unwrap().unwrap();
        let other = File::open(&directory).unwrap();
        assert!(other.try_lock().is_err());
        let temporary = directory.join(".state.json.123.456");
        private(&temporary, b"new selected authority");
        let (done, result) = mpsc::channel();
        let next_path = path.clone();
        let reader = thread::spawn(move || {
            let _owner = lock_and_retire(&next_path).unwrap();
            done.send(fs::read(next_path).unwrap()).unwrap();
        });
        assert!(result.recv_timeout(Duration::from_millis(20)).is_err());
        assert!(temporary.exists());
        fs::rename(temporary, &path).unwrap();
        owner.sync_all().unwrap();
        drop(owner);
        assert_eq!(
            result.recv_timeout(Duration::from_secs(2)).unwrap(),
            b"new selected authority"
        );
        reader.join().unwrap();
        other.try_lock().unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        drop(other);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cleanup_refuses_symlinks_hardlinks_directories_and_permissive_files() {
        for kind in ["symlink", "hardlink", "directory", "permissive"] {
            let directory = directory(kind);
            let path = directory.join("state.json");
            private(&path, b"selected");
            let candidate = directory.join(".state.json.123.456");
            match kind {
                "symlink" => symlink(&path, &candidate).unwrap(),
                "hardlink" => fs::hard_link(&path, &candidate).unwrap(),
                "directory" => fs::create_dir(&candidate).unwrap(),
                _ => {
                    private(&candidate, b"untrusted");
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644)).unwrap();
                }
            }
            assert!(lock_and_retire(&path).is_err());
            assert!(candidate.exists());
            assert_eq!(fs::read(&path).unwrap(), b"selected");
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn cleanup_does_not_sweep_shared_or_symlinked_parents() {
        let directory = directory("parent");
        let candidate = directory.join(".state.json.123.456");
        private(&candidate, b"untouched");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            lock_and_retire(&directory.join("state.json"))
                .unwrap()
                .is_none()
        );
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let link = directory.with_extension("symlink");
        symlink(&directory, &link).unwrap();
        assert!(lock_and_retire(&link.join("state.json")).unwrap().is_none());
        assert!(candidate.exists());
        fs::remove_file(link).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}
