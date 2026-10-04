use std::time::{Duration, Instant};

use super::*;
use crate::{
    config::RouteConfig,
    hostname::issue_hostname_record_at,
    membership::{
        MembershipRecordIssueOptions, MembershipRecordSubject,
        checkpoint::{MembershipChange, MembershipSyncState, NetworkAnchor},
        issue_named_membership_record_for_subject_at,
    },
};

const NOW: u64 = 1_000;

fn capability() -> NetworkCapability {
    NetworkCapability::from_secret(NetworkAnchor::new([7; 32]).unwrap(), Some(&[9; 32])).unwrap()
}

struct Fixture {
    creator: NodeIdentity,
    removed: NodeIdentity,
    survivor: NodeIdentity,
    records: Vec<SignedMembershipRecord>,
    names: Vec<SignedHostnameRecord>,
}

fn record(
    issuer: &NodeIdentity,
    member: &NodeIdentity,
    sequence: u64,
    revoked: bool,
    hostname: &str,
    route: &str,
) -> SignedMembershipRecord {
    issue_named_membership_record_for_subject_at(
        issuer,
        MembershipRecordIssueOptions {
            network_name: "lab".into(),
            member: MembershipRecordSubject::from_identity(member).unwrap(),
            membership_epoch: 1,
            sequence,
            revoked,
            roles: if revoked {
                vec![]
            } else {
                vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ]
            },
            route_grants: if revoked {
                vec![]
            } else {
                vec![RouteConfig {
                    prefix: route.into(),
                    metric: 17,
                }]
            },
            expires_at_unix_seconds: None,
        },
        (!revoked).then_some(hostname),
        100 + sequence,
    )
    .unwrap()
}

fn fixture() -> Fixture {
    let creator = NodeIdentity::generate_ed25519().unwrap();
    let removed = NodeIdentity::generate_ed25519().unwrap();
    let survivor = NodeIdentity::generate_ed25519().unwrap();
    let records = vec![
        record(&creator, &creator, 1, false, "creator", "10.42.0.1/32"),
        record(&creator, &removed, 1, false, "removed", "10.43.0.0/24"),
        record(
            &removed,
            &survivor,
            1,
            false,
            "old-survivor",
            "10.77.0.9/24",
        ),
        record(&creator, &removed, 2, true, "removed", "10.43.0.0/24"),
    ];
    let names = vec![
        issue_hostname_record_at(&survivor, "lab", "old-survivor", 1, 200).unwrap(),
        issue_hostname_record_at(&survivor, "lab", "current-survivor", 2, 201).unwrap(),
        issue_hostname_record_at(&removed, "lab", "removed-device", 3, 202).unwrap(),
    ];
    Fixture {
        creator,
        removed,
        survivor,
        records,
        names,
    }
}

fn prepare(fixture: &Fixture) -> SignedLegacyMigrationSeed {
    SignedLegacyMigrationSeed::prepare_at(
        capability(),
        &fixture.creator,
        LegacyMigrationOptions {
            network_name: "lab",
            records: &fixture.records,
            hostname_records: &fixture.names,
            policy: SnapshotPolicy {
                max_active_members: 16,
                route_grants_enabled: true,
            },
        },
        NOW,
    )
    .unwrap()
}

#[test]
fn migration_seed_retains_only_current_members_names_routes_and_policy() {
    let fixture = fixture();
    let original = serde_json::to_vec(&(&fixture.records, &fixture.names)).unwrap();
    let seed = prepare(&fixture);
    let snapshot = &seed.payload.snapshot.payload;
    assert_eq!(snapshot.members.len(), 2);
    assert_eq!(snapshot.policy.max_active_members, 16);
    let survivor = snapshot.member(&fixture.survivor.peer_id).unwrap();
    assert_eq!(survivor.route_grants[0].prefix, "10.77.0.0/24");
    assert_eq!(survivor.route_grants[0].metric, 17);
    assert_eq!(survivor.roles, fixture.records[2].payload.roles);
    assert!(snapshot.member(&fixture.removed.peer_id).is_none());
    let encoded = serde_json::to_string(&seed).unwrap();
    for obsolete in [&fixture.removed.peer_id, "removed-device", "old-survivor"] {
        assert!(!encoded.contains(obsolete));
    }
    for obsolete in ["issuer_peer", "revoked", "admitted_at", "membership_epoch"] {
        assert!(!encoded.contains(obsolete));
    }
    assert_eq!(seed.payload.hostnames.len(), 2);
    assert!(encoded.contains("current-survivor"));
    assert_eq!(
        serde_json::to_vec(&(&fixture.records, &fixture.names)).unwrap(),
        original
    );
}

#[test]
fn same_seed_installs_same_authority_and_only_self_signed_names_behind_resync_gate() {
    let fixture = fixture();
    let seed = prepare(&fixture);
    let mut creator = seed
        .instantiate_at(capability(), &fixture.creator, "lab", NOW)
        .unwrap();
    let mut survivor = seed
        .instantiate_at(capability(), &fixture.survivor, "lab", NOW)
        .unwrap();
    assert_eq!(creator.snapshot(), survivor.snapshot());
    for (state, identity, hostname) in [
        (&mut creator, &fixture.creator, "creator"),
        (&mut survivor, &fixture.survivor, "current-survivor"),
    ] {
        assert_eq!(state.sync_state(), MembershipSyncState::ResyncRequired);
        assert_eq!(state.hostname_claims().len(), 1);
        assert_eq!(
            state.hostname_claims()[0].payload.subject.peer_id,
            identity.peer_id
        );
        assert_eq!(state.hostname_claims()[0].payload.hostname, hostname);
        assert!(matches!(
            state.sign_mutation_at(
                identity,
                MembershipChange::RemoveMember(fixture.removed.peer_id.clone()),
                NOW,
            ),
            Err(CheckpointError::NoParticipation)
        ));
        let now = Instant::now();
        state.begin_resync(now, Duration::from_millis(1)).unwrap();
        state
            .finish_resync(now + Duration::from_millis(1), NOW)
            .unwrap();
        assert_eq!(state.sync_state(), MembershipSyncState::Participating);
    }
    let now = Instant::now();
    let challenge = creator.begin_resync(now, Duration::from_millis(1)).unwrap();
    let offer = survivor
        .make_offer_at(challenge, &fixture.survivor, NOW)
        .unwrap();
    creator
        .collect_offer(&offer, fixture.survivor.peer_id.parse().unwrap(), now)
        .unwrap();
    creator
        .finish_resync(now + Duration::from_millis(1), NOW)
        .unwrap();
    assert_eq!(creator.hostname_claims().len(), 2);
    let boundary = creator.snapshot().payload.boundary().unwrap();
    let removal = creator
        .sign_mutation_at(
            &fixture.creator,
            MembershipChange::RemoveMember(fixture.survivor.peer_id.clone()),
            NOW,
        )
        .unwrap();
    creator.apply_mutation_at(&removal, NOW).unwrap();
    assert_eq!(
        creator
            .snapshot()
            .payload
            .boundary()
            .unwrap()
            .authority_revision,
        boundary.authority_revision + 1,
    );
    assert_eq!(creator.hostname_claims().len(), 1);
    let encoded = serde_json::to_string(&creator.retained()).unwrap();
    assert!(!encoded.contains(&fixture.survivor.peer_id));
    assert!(!encoded.contains("current-survivor"));
}

#[test]
fn migration_seed_is_scoped_authenticated_and_time_bounded() {
    let fixture = fixture();
    let seed = prepare(&fixture);
    let bytes = serde_json::to_vec(&seed).unwrap();
    assert_eq!(
        SignedLegacyMigrationSeed::decode_at(&bytes, &capability(), "lab", NOW).unwrap(),
        seed,
    );
    for now in [NOW - 1, NOW + MIGRATION_SEED_LIFETIME_SECONDS] {
        assert!(SignedLegacyMigrationSeed::decode_at(&bytes, &capability(), "lab", now).is_err());
    }
    assert!(SignedLegacyMigrationSeed::decode_at(&bytes, &capability(), "other", NOW).is_err());
    let wrong_secret =
        NetworkCapability::from_secret(capability().anchor().clone(), Some(&[8; 32])).unwrap();
    assert!(seed.verify_at(&wrong_secret, "lab", NOW).is_err());
    let wrong_scope =
        NetworkCapability::from_secret(NetworkAnchor::new([6; 32]).unwrap(), Some(&[9; 32]))
            .unwrap();
    assert!(seed.verify_at(&wrong_scope, "lab", NOW).is_err());
    let mut tampered = seed.clone();
    tampered.payload.hostnames[0].hostname = "forged".into();
    assert!(matches!(
        tampered.verify_at(&capability(), "lab", NOW),
        Err(CheckpointError::InvalidSignature)
    ));
    let mut tampered = seed;
    tampered.payload.snapshot.payload.policy.max_active_members = 17;
    assert!(matches!(
        tampered.verify_at(&capability(), "lab", NOW),
        Err(CheckpointError::InvalidMac)
    ));
}

#[test]
fn revoked_or_unadmitted_identity_cannot_install_or_publish_seed() {
    let fixture = fixture();
    let seed = prepare(&fixture);
    for identity in [&fixture.removed, &NodeIdentity::generate_ed25519().unwrap()] {
        assert!(matches!(
            seed.instantiate_at(capability(), identity, "lab", NOW),
            Err(CheckpointError::NoParticipation)
        ));
        assert!(
            SignedLegacyMigrationSeed::prepare_at(
                capability(),
                identity,
                LegacyMigrationOptions {
                    network_name: "lab",
                    records: &fixture.records,
                    hostname_records: &fixture.names,
                    policy: SnapshotPolicy::default(),
                },
                NOW,
            )
            .is_err()
        );
    }
}

#[test]
fn invalid_or_future_hostname_history_is_not_silently_discarded() {
    let mut fixture = fixture();
    fixture.names[0].signature = "invalid".into();
    assert!(
        SignedLegacyMigrationSeed::prepare_at(
            capability(),
            &fixture.creator,
            LegacyMigrationOptions {
                network_name: "lab",
                records: &fixture.records,
                hostname_records: &fixture.names,
                policy: SnapshotPolicy::default(),
            },
            NOW,
        )
        .is_err()
    );
    fixture.names =
        vec![issue_hostname_record_at(&fixture.survivor, "lab", "future", 3, NOW + 1).unwrap()];
    assert!(
        SignedLegacyMigrationSeed::prepare_at(
            capability(),
            &fixture.creator,
            LegacyMigrationOptions {
                network_name: "lab",
                records: &fixture.records,
                hostname_records: &fixture.names,
                policy: SnapshotPolicy::default(),
            },
            NOW,
        )
        .is_err()
    );
}

#[test]
fn migration_does_not_silently_drop_unsupported_route_only_authority() {
    let mut fixture = fixture();
    let options = MembershipRecordIssueOptions {
        network_name: "lab".into(),
        member: MembershipRecordSubject::from_identity(&fixture.survivor).unwrap(),
        membership_epoch: 1,
        sequence: 2,
        revoked: false,
        roles: vec![MembershipRole::RouteAuthority],
        route_grants: vec![RouteConfig {
            prefix: "10.77.0.0/24".into(),
            metric: 17,
        }],
        expires_at_unix_seconds: None,
    };
    fixture.records = vec![
        fixture.records[0].clone(),
        issue_named_membership_record_for_subject_at(&fixture.creator, options, None, NOW).unwrap(),
    ];
    let result = SignedLegacyMigrationSeed::prepare_at(
        capability(),
        &fixture.creator,
        LegacyMigrationOptions {
            network_name: "lab",
            records: &fixture.records,
            hostname_records: &fixture.names,
            policy: SnapshotPolicy::default(),
        },
        NOW,
    );
    assert!(
        matches!(
            &result,
            Err(CheckpointError::Invalid(
                "migration cannot represent route-only membership"
            ))
        ),
        "unexpected migration result: {result:?}"
    );
}

#[test]
fn migration_encoding_and_policy_are_bounded_and_unknown_fields_rejected() {
    let fixture = fixture();
    assert!(
        SignedLegacyMigrationSeed::prepare_at(
            capability(),
            &fixture.creator,
            LegacyMigrationOptions {
                network_name: "lab",
                records: &fixture.records,
                hostname_records: &fixture.names,
                policy: SnapshotPolicy {
                    max_active_members: 1,
                    route_grants_enabled: true
                },
            },
            NOW,
        )
        .is_err()
    );
    let mut value = serde_json::to_value(prepare(&fixture)).unwrap();
    value["old_records"] = serde_json::json!([]);
    assert!(
        SignedLegacyMigrationSeed::decode_at(
            &serde_json::to_vec(&value).unwrap(),
            &capability(),
            "lab",
            NOW
        )
        .is_err()
    );
    assert!(
        SignedLegacyMigrationSeed::decode_at(
            &vec![b' '; MAX_SNAPSHOT_OFFER_BYTES + 1],
            &capability(),
            "lab",
            NOW
        )
        .is_err()
    );
}

#[test]
fn migration_preserves_expiry_and_disabled_route_policy_without_reviving_expired_members() {
    let mut fixture = fixture();
    let expiring = issue_named_membership_record_for_subject_at(
        &fixture.creator,
        MembershipRecordIssueOptions {
            network_name: "lab".into(),
            member: MembershipRecordSubject::from_identity(&fixture.survivor).unwrap(),
            membership_epoch: 1,
            sequence: 2,
            revoked: false,
            roles: vec![
                MembershipRole::OverlayMember,
                MembershipRole::RouteAuthority,
            ],
            route_grants: vec![RouteConfig {
                prefix: "10.77.0.0/24".into(),
                metric: 17,
            }],
            expires_at_unix_seconds: Some(NOW + 10),
        },
        Some("survivor"),
        NOW,
    )
    .unwrap();
    fixture.records.push(expiring);
    let seed = SignedLegacyMigrationSeed::prepare_at(
        capability(),
        &fixture.creator,
        LegacyMigrationOptions {
            network_name: "lab",
            records: &fixture.records,
            hostname_records: &fixture.names,
            policy: SnapshotPolicy {
                max_active_members: 16,
                route_grants_enabled: false,
            },
        },
        NOW,
    )
    .unwrap();
    assert!(!seed.payload.snapshot.payload.policy.route_grants_enabled);
    assert_eq!(
        seed.payload
            .snapshot
            .payload
            .member(&fixture.survivor.peer_id)
            .unwrap()
            .expires_at_unix_seconds,
        Some(NOW + 10)
    );
    assert!(matches!(
        seed.instantiate_at(capability(), &fixture.survivor, "lab", NOW + 10),
        Err(CheckpointError::NoParticipation)
    ));
    let expired = SignedLegacyMigrationSeed::prepare_at(
        capability(),
        &fixture.creator,
        LegacyMigrationOptions {
            network_name: "lab",
            records: &fixture.records,
            hostname_records: &fixture.names,
            policy: SnapshotPolicy::default(),
        },
        NOW + 10,
    )
    .unwrap();
    assert_eq!(expired.payload.snapshot.payload.members.len(), 1);
    let bytes = serde_json::to_string(&expired).unwrap();
    assert!(!bytes.contains(&fixture.survivor.peer_id));
    assert!(!bytes.contains("current-survivor"));
}

#[test]
fn installed_authority_restarts_after_seed_expiry_without_retaining_migration_proof() {
    let fixture = fixture();
    let seed = prepare(&fixture);
    let installed = seed
        .instantiate_at(capability(), &fixture.survivor, "lab", NOW)
        .unwrap();
    let retained = installed.retained();
    let bytes = serde_json::to_string(&retained).unwrap();
    for staging_field in [
        "publisher",
        "issued_at_unix_seconds",
        "network_name",
        "hostnames",
        "migration",
    ] {
        assert!(!bytes.contains(staging_field));
    }
    assert!(
        seed.verify_at(&capability(), "lab", NOW + MIGRATION_SEED_LIFETIME_SECONDS)
            .is_err()
    );
    let mut restored = CooperativeMembershipState::restore(
        capability(),
        fixture.survivor.peer_id.clone(),
        retained,
    )
    .unwrap();
    assert_eq!(restored.sync_state(), MembershipSyncState::ResyncRequired);
    let now = Instant::now();
    restored
        .begin_resync(now, Duration::from_millis(1))
        .unwrap();
    restored
        .finish_resync(
            now + Duration::from_millis(1),
            NOW + MIGRATION_SEED_LIFETIME_SECONDS,
        )
        .unwrap();
    assert_eq!(restored.sync_state(), MembershipSyncState::Participating);
    assert_eq!(
        restored.hostname_claims()[0].payload.hostname,
        "current-survivor"
    );
}
