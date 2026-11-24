#![cfg(windows)]

use p2p_vpn::private_fs;
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn cli_help_private_config_and_windows_dry_run() {
    use std::process::Command;
    let executable = env!("CARGO_BIN_EXE_p2p-vpn");
    assert!(
        Command::new(executable)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success()
    );
    let directory = std::env::temp_dir().join(format!(
        "p2p-vpn-windows-cli-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    private_fs::ensure_private_directory(&directory).unwrap();
    let config = directory.join("config.json");
    let generated = Command::new(executable)
        .args(["init-config", "--network", "windows-test", "--output"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    private_fs::validate_private(&config).unwrap();
    let status = Command::new(executable)
        .args(["status", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let preview = Command::new(executable)
        .args(["up", "--dry-run", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let stdout = String::from_utf8_lossy(&preview.stdout);
    assert!(stdout.contains("Windows addr replace"));
    assert!(!stdout.contains("sysctl -w"));
    fs::remove_dir_all(directory).unwrap();
}
