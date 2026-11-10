//! Creation of private files. Windows uses protected DACLs, never Unix mode emulation.
use std::{fs::File, io, path::Path};

#[cfg(unix)]
pub fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(windows)]
pub use windows::{create_private_file, ensure_private_directory, validate_private};

#[cfg(windows)]
pub(crate) mod windows {
    use super::*;
    use std::{
        ffi::OsStr,
        os::windows::process::CommandExt as _,
        process::{Command, Output},
    };

    // Pass paths as environment data, never interpolate them into PowerShell code.
    // Windows PowerShell 5.1 ships with the supported Windows versions.
    pub(crate) fn powershell(script: &str, vars: &[(&str, &OsStr)]) -> io::Result<Output> {
        let system_root = std::env::var_os("SystemRoot")
            .ok_or_else(|| io::Error::other("SystemRoot is missing"))?;
        let executable = std::path::PathBuf::from(system_root)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let output = Command::new(executable)
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!("$ErrorActionPreference='Stop'; try {{ {script} }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}"))
            .envs(vars.iter().copied()).output()?;
        if !output.status.success() {
            return Err(io::Error::other(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(output)
    }

    const SECURITY: &str = r"
        $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User;
        $acl=New-Object Security.AccessControl.FileSecurity;
        $acl.SetOwner($sid);
        $acl.SetAccessRuleProtection($true,$false);
        foreach($id in @($sid.Value,'S-1-5-18','S-1-5-32-544')) {
            $rule=New-Object Security.AccessControl.FileSystemAccessRule(
                (New-Object Security.Principal.SecurityIdentifier($id)), 'FullControl', 'Allow');
            $acl.AddAccessRule($rule);
        }
    ";

    pub fn create_private_file(path: &Path) -> io::Result<File> {
        // FileStream applies the DACL as part of CREATE_NEW, before any secret is written.
        if path.try_exists()? {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        // State directories already exclude untrusted readers. Inherit that
        // restriction at CREATE_NEW, then protect the file DACL before writing.
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && validate_private(parent).is_ok()
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_permissions::{
                LocalBox, SecurityDescriptor,
                constants::{SeObjectType::SE_FILE_OBJECT, SecurityInformation},
                wrappers::SetSecurityInfo,
            };
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .access_mode(0x40000000 | 0x00040000 | 0x00020000)
                .open(path)?;
            let sd: LocalBox<SecurityDescriptor> = format!(
                "D:P(A;;FA;;;{})(A;;FA;;;SY)(A;;FA;;;BA)",
                current_user_sid()?
            )
            .parse()?;
            SetSecurityInfo(
                &mut file,
                SE_FILE_OBJECT,
                SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
                None,
                None,
                sd.dacl(),
                None,
            )?;
            validate_private(path)?;
            return Ok(file);
        }
        powershell(
            &format!(
                r"{SECURITY}
            $f=New-Object IO.FileStream($env:P2PVPN_FS_PATH,[IO.FileMode]::CreateNew,
                [Security.AccessControl.FileSystemRights]::FullControl,[IO.FileShare]::ReadWrite,
                4096,[IO.FileOptions]::None,$acl); $f.Dispose();"
            ),
            &[("P2PVPN_FS_PATH", path.as_os_str())],
        )?;
        validate_private(path)?;
        std::fs::OpenOptions::new().write(true).open(path)
    }

    pub fn validate_private(path: &Path) -> io::Result<()> {
        use std::os::windows::fs::MetadataExt as _;
        use windows_permissions::{
            constants::{SeObjectType::SE_FILE_OBJECT, SecurityInformation},
            wrappers::{
                ConvertSecurityDescriptorToStringSecurityDescriptor, ConvertSidToStringSid,
                GetNamedSecurityInfo,
            },
        };
        if std::fs::symlink_metadata(path)?.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other(
                "reparse points are not allowed for private state",
            ));
        }
        let sd = GetNamedSecurityInfo(
            path.as_os_str(),
            SE_FILE_OBJECT,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )?;
        let allowed = [current_user_sid()?, "S-1-5-18", "S-1-5-32-544"];
        let owner = ConvertSidToStringSid(
            sd.owner()
                .ok_or_else(|| io::Error::other("private state has no owner"))?,
        )?;
        if !allowed.contains(&owner.to_string_lossy().as_ref()) || sd.dacl().is_none() {
            return Err(io::Error::other(
                "private state has an untrusted owner or a null DACL",
            ));
        }
        // Ask Windows to canonicalize the DACL. Accept only the simple allow
        // ACEs this application creates; unfamiliar ACL forms fail closed.
        let sddl =
            ConvertSecurityDescriptorToStringSecurityDescriptor(&sd, SecurityInformation::Dacl)?;
        let sddl = sddl.to_string_lossy();
        let dacl = sddl
            .strip_prefix("D:")
            .ok_or_else(|| io::Error::other("missing DACL"))?;
        let mut parts = dacl.split('(');
        if !parts.next().unwrap_or_default().contains('P') {
            return Err(io::Error::other(
                "private state requires a protected DACL; see docs/user/windows.md",
            ));
        }
        for part in parts {
            let fields: Vec<_> = part.trim_end_matches(')').split(';').collect();
            if fields.len() != 6
                || fields[0] != "A"
                || !(allowed.contains(&fields[5]) || ["SY", "BA"].contains(&fields[5]))
            {
                return Err(io::Error::other(
                    "private state has an unsupported or permissive DACL; see docs/user/windows.md",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn current_user_sid() -> io::Result<&'static str> {
        static SID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        if let Some(sid) = SID.get() {
            return Ok(sid);
        }
        let output = powershell(
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
        let _ = SID.set(sid);
        Ok(SID.get().expect("SID initialized"))
    }

    /// Windows documents volume FlushFileBuffers for flushing filesystem data.
    /// Unlike a Unix directory fsync this requires elevation and flushes the
    /// entire local volume. Never silently downgrade an access or flush failure.
    pub(crate) fn open_parent_volume(parent: &Path) -> io::Result<File> {
        use std::path::{Component, Prefix};
        let canonical = parent.canonicalize()?;
        let Some(Component::Prefix(prefix)) = canonical.components().next() else {
            return Err(io::Error::other("private state must be on a local drive"));
        };
        let drive = match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "durable Windows state requires a local drive-letter volume",
                ));
            }
        };
        File::options()
            .read(true)
            .write(true)
            .open(format!(r"\\.\{}:", char::from(drive)))
    }

    pub(crate) fn sync_parent_volume(parent: &Path) -> io::Result<()> {
        open_parent_volume(parent)?.sync_all()
    }

    pub fn ensure_private_directory(path: &Path) -> io::Result<()> {
        if path.try_exists()? {
            return validate_private(path);
        }
        powershell(
            r"
            $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User;
            $acl=New-Object Security.AccessControl.DirectorySecurity;
            $acl.SetOwner($sid); $acl.SetAccessRuleProtection($true,$false);
            foreach($id in @($sid.Value,'S-1-5-18','S-1-5-32-544')) {
                $rule=New-Object Security.AccessControl.FileSystemAccessRule(
                    (New-Object Security.Principal.SecurityIdentifier($id)), 'FullControl',
                    'ContainerInherit,ObjectInherit', 'None', 'Allow');
                $acl.AddAccessRule($rule);
            }
            [IO.Directory]::CreateDirectory($env:P2PVPN_FS_PATH,$acl) | Out-Null;
        ",
            &[("P2PVPN_FS_PATH", path.as_os_str())],
        )?;
        validate_private(path)
    }
}
