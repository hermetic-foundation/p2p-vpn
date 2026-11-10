//! Windows serializes state writers with a private lock file.
//! Abandoned copies are never treated as authority and are left for inspection.
use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

pub(super) fn lock_and_retire(path: &Path) -> io::Result<Option<File>> {
    let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(None);
    };
    if !parent.try_exists()? {
        return Ok(None);
    }
    crate::private_fs::validate_private(parent)?;
    let lock_path = parent.join(".p2p-vpn-state.lock");
    match crate::private_fs::create_private_file(&lock_path) {
        Ok(file) => drop(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    crate::private_fs::validate_private(&lock_path)?;
    let lock = OpenOptions::new().read(true).write(true).open(lock_path)?;
    lock.lock()?;
    Ok(Some(lock))
}
