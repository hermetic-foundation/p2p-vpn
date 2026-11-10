//! Same bounded control protocol over a local, access-controlled Windows pipe.
use interprocess::{
    local_socket::{
        GenericNamespaced, ListenerOptions, ToNsName,
        tokio::{Listener, Stream, prelude::*},
    },
    os::windows::{local_socket::ListenerOptionsExt as _, security_descriptor::SecurityDescriptor},
};
use std::{io, path::Path};
use widestring::U16CString;

pub(super) type LocalStream = Stream;
pub(super) struct LocalListener(Listener);

fn name(path: &Path) -> io::Result<interprocess::local_socket::Name<'static>> {
    use sha2_010::{Digest as _, Sha256};
    // A filesystem-like CLI path is a stable identifier; no endpoint file is written.
    let absolute = std::path::absolute(path)?;
    let normalized = absolute.to_string_lossy().replace('/', "\\").to_lowercase();
    let digest = Sha256::digest(normalized.as_bytes());
    let hash: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("p2p-vpn-{hash}")
        .to_ns_name::<GenericNamespaced>()
        .map(|name| name.into_owned())
}

impl LocalListener {
    pub(super) fn bind(path: &Path) -> io::Result<Self> {
        let output = crate::private_fs::windows::powershell(
            "[Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
            &[],
        )?;
        let sid = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !sid.starts_with("S-1-")
            || !sid
                .chars()
                .all(|c| c.is_ascii_digit() || c == 'S' || c == '-')
        {
            return Err(io::Error::other("invalid current user SID"));
        }
        // Deny network logons; grant only this account, SYSTEM and administrators.
        let sddl = U16CString::from_str(format!(
            "D:P(D;;GA;;;NU)(A;;GA;;;{sid})(A;;GA;;;SY)(A;;GA;;;BA)"
        ))
        .map_err(io::Error::other)?;
        let sd = SecurityDescriptor::deserialize(&sddl)?;
        ListenerOptions::new()
            .name(name(path)?)
            .security_descriptor(sd)
            .create_tokio()
            .map(Self)
    }

    pub(super) async fn accept(&self) -> io::Result<(LocalStream, ())> {
        self.0.accept().await.map(|stream| (stream, ()))
    }
}

pub(super) async fn connect(path: &Path) -> io::Result<LocalStream> {
    Stream::connect(name(path)?).await
}
