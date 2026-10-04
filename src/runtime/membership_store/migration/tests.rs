use std::os::unix::fs::{PermissionsExt as _, symlink};

use super::*;
use crate::{
    hostname::issue_hostname_record_at,
    identity::NodeIdentity,
    membership::{
        MembershipRecordIssueOptions, MembershipRecordSubject, MembershipRole,
        checkpoint::{SnapshotPolicy, migration::LegacyMigrationOptions},
        issue_named_membership_record_for_subject_at,
    },
};

const NOW: u64 = 1_000;

fn make_artifact() -> MigrationArtifact {
    let identity = NodeIdentity::generate_ed25519().unwrap();
    let credentials =
        CheckpointCredentials::new(NetworkAnchor::new([7; 32]).unwrap(), vec![9; 32]).unwrap();
    let record = issue_named_membership_record_for_subject_at(
        &identity,
        MembershipRecordIssueOptions {
            network_name: "lab".into(),
            member: MembershipRecordSubject::from_identity(&identity).unwrap(),
            membership_epoch: 1,
            sequence: 1,
            revoked: false,
            roles: vec![MembershipRole::OverlayMember],
            route_grants: vec![],
            expires_at_unix_seconds: None,
        },
        Some("node"),
        NOW,
    )
    .unwrap();
    let name = issue_hostname_record_at(&identity, "lab", "node", 1, NOW).unwrap();
    let seed = SignedLegacyMigrationSeed::prepare_at(
        credentials.capability().unwrap(),
        &identity,
        LegacyMigrationOptions {
            network_name: "lab",
            records: &[record],
            hostname_records: &[name],
            policy: SnapshotPolicy::default(),
        },
        NOW,
    )
    .unwrap();
    MigrationArtifact { credentials, seed }
}

fn store(label: &str) -> (std::path::PathBuf, MigrationArtifactStore) {
    let directory = std::env::temp_dir().join(format!(
        "p2p-vpn-migration-store-{}-{label}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let authority = MembershipStateStore::new(directory.join("membership-state.json"));
    (directory, MigrationArtifactStore::for_authority(&authority))
}

#[test]
fn protected_artifact_round_trip_keeps_authority_untouched_and_debug_redacted() {
    let (directory, store) = store("round-trip");
    let artifact = make_artifact();
    let authority_path = directory.join("membership-state.json");
    fs::write(&authority_path, b"existing legacy authority").unwrap();
    store.save(&artifact, "lab", NOW).unwrap();
    assert_eq!(
        fs::read(&authority_path).unwrap(),
        b"existing legacy authority"
    );
    assert_eq!(
        fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let loaded = store.load("lab", Some(&[9; 32])).unwrap().unwrap();
    assert_eq!(loaded.credentials.anchor(), artifact.credentials.anchor());
    assert_eq!(loaded.credentials.secret(), artifact.credentials.secret());
    assert_eq!(loaded.seed, artifact.seed);
    let debug = format!("{loaded:?}");
    assert!(!debug.contains(&STANDARD.encode(loaded.credentials.secret())));
    assert!(!debug.contains("capability_secret"));
    assert!(store.load("other", None).is_err());
    assert!(matches!(
        store.load("lab", Some(&[8; 32])),
        Err(MembershipStateStoreError::CapabilityMismatch)
    ));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn preparation_retry_is_identical_and_cannot_replace_scope_or_extend_expiry() {
    let (directory, store) = store("retry");
    let artifact = make_artifact();
    store.save(&artifact, "lab", NOW).unwrap();
    let original = fs::read(store.path()).unwrap();
    store.save(&artifact, "lab", NOW + 1).unwrap();
    assert_eq!(fs::read(store.path()).unwrap(), original);
    let mut changed = make_artifact();
    changed.credentials =
        CheckpointCredentials::new(NetworkAnchor::new([6; 32]).unwrap(), vec![9; 32]).unwrap();
    assert!(store.save(&changed, "lab", NOW).is_err());
    let changed = make_artifact();
    assert!(
        matches!(store.save(&changed, "lab", NOW), Err(MembershipStateStoreError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    assert_eq!(fs::read(store.path()).unwrap(), original);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn visible_write_failure_can_be_reconfirmed_without_regenerating_scope() {
    let (directory, store) = store("visible-write");
    let artifact = make_artifact();
    let result = store.save_with_sync(&artifact, "lab", NOW, |_| {
        Err(io::Error::other("injected sync failure"))
    });
    assert!(matches!(
        result,
        Err(MembershipStateStoreError::MigrationDurabilityUncertain(_))
    ));
    let loaded = store.load("lab", None).unwrap().unwrap();
    assert_eq!(loaded.seed, artifact.seed);
    store.save(&loaded, "lab", NOW).unwrap();
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn expiry_is_checked_before_use_but_authenticated_expired_artifacts_can_be_retired() {
    let (directory, store) = store("expiry");
    let artifact = make_artifact();
    store.save(&artifact, "lab", NOW).unwrap();
    let expired = store.load("lab", None).unwrap().unwrap();
    assert!(
        expired
            .verify_at("lab", expired.seed.payload.expires_at_unix_seconds)
            .is_err()
    );
    assert!(
        store
            .save(
                &expired,
                "lab",
                expired.seed.payload.expires_at_unix_seconds
            )
            .is_err()
    );
    assert!(store.retire(&expired, "lab").unwrap());
    assert!(!store.path().exists());
    assert!(!store.retire(&expired, "lab").unwrap());
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn retirement_never_deletes_a_different_artifact_and_retries_uncertain_unlink() {
    let (directory, store) = store("retirement");
    let artifact = make_artifact();
    store.save(&artifact, "lab", NOW).unwrap();
    let unrelated = make_artifact();
    assert!(store.retire(&unrelated, "lab").is_err());
    assert!(store.path().exists());
    assert!(
        store
            .retire_with_sync(&artifact, "lab", |_| Err(io::Error::other(
                "injected unlink sync"
            )))
            .is_err()
    );
    assert!(!store.path().exists());
    assert!(!store.retire(&artifact, "lab").unwrap());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unsafe_file_or_parent_and_invalid_capability_fail_closed() {
    let (directory, store) = store("unsafe");
    let artifact = make_artifact();
    store.save(&artifact, "lab", NOW).unwrap();
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load("lab", None).is_err());
    fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(store.load("lab", None).is_err());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let bytes = fs::read(store.path()).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["capability_secret"] = serde_json::json!("invalid");
    fs::write(store.path(), serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(store.load("lab", None).is_err());
    fs::remove_file(store.path()).unwrap();
    let target = directory.join("unrelated");
    fs::write(&target, &bytes).unwrap();
    symlink(&target, store.path()).unwrap();
    assert!(store.load("lab", None).is_err());
    assert!(store.save(&artifact, "lab", NOW).is_err());
    assert_eq!(fs::read(&target).unwrap(), bytes);
    fs::remove_dir_all(directory).unwrap();
}
