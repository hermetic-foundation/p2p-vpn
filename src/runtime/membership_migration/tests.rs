use super::super::{
    control_socket::{RuntimeControlHandle, runtime_control_channel},
    pairing_sessions::{
        CodePairingSessions, PairingEnrollment, PairingEnrollmentPreparation,
        PairingEnrollmentRole, PendingApproval,
    },
    pairing_store::PairingStateStore,
    runner::{PreconfiguredTunRoutes, RuntimePlatform, run_config_until_with_runtime_platform},
    tun::{PacketIo, PacketRead, PacketWrite},
};
use super::*;
use crate::{
    config::Config,
    membership::checkpoint::{MembershipChange, MembershipSyncState},
    membership::{
        MembershipRecordIssueOptions, MembershipRecordSubject,
        issue_named_membership_record_for_subject_at,
    },
};
use std::{fs, os::unix::fs::PermissionsExt as _, path::PathBuf};

const NOW: u64 = 1_000;

struct Fixture {
    directory: PathBuf,
    store: MembershipStateStore,
    identity: NodeIdentity,
    peer: NodeIdentity,
    forwarder: Forwarder,
    runtime: Option<CheckpointRuntime>,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let local = PeerId::from_libp2p(identity.peer_id.parse().unwrap());
        let peer = loop {
            let candidate = NodeIdentity::generate_ed25519().unwrap();
            let peer = PeerId::from_libp2p(candidate.peer_id.parse().unwrap());
            if builtin_ipv4(local) != builtin_ipv4(peer) {
                break candidate;
            }
        };
        Self::with_identity(label, identity, peer)
    }

    fn with_identity(label: &str, identity: NodeIdentity, peer: NodeIdentity) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "p2p-vpn-migration-{}-{label}-{}",
            std::process::id(),
            identity.peer_id
        ));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let store = MembershipStateStore::new(directory.join("membership-state.json"));
        let records = [&identity, &peer]
            .iter()
            .map(|member| {
                let public = STANDARD.encode(member.public_key_protobuf().unwrap());
                issue_named_membership_record_for_subject_at(
                    &identity,
                    MembershipRecordIssueOptions {
                        network_name: "lab".into(),
                        member: MembershipRecordSubject {
                            peer_id: member.peer_id.clone(),
                            public_key: public,
                        },
                        membership_epoch: 1,
                        sequence: 1,
                        revoked: false,
                        roles: vec![MembershipRole::OverlayMember],
                        route_grants: vec![],
                        expires_at_unix_seconds: None,
                    },
                    Some(if member.peer_id == identity.peer_id {
                        "local-host"
                    } else {
                        "remote-host"
                    }),
                    NOW,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let config: Config = serde_json::from_value(serde_json::json!({
            "network": {"name": "lab", "local_peer": identity.peer_id,
                "private_key": identity.private_key, "member_records": records,
                "vpn_ip": "10.42.0.1", "dns": {"hostname": "local-host"},
                "routes": [{"prefix": "10.99.12.5/16", "metric": 7}]},
            "peers": [{"id": peer.peer_id, "vpn_ip": "10.42.0.2"}],
        }))
        .unwrap();
        let forwarder = Forwarder::from_config(&config).unwrap();
        store
            .save("lab", &identity.peer_id, forwarder.member_records(), &[])
            .unwrap();
        Self {
            directory,
            store,
            identity,
            peer,
            forwarder,
            runtime: None,
        }
    }
    fn bytes(&self) -> Vec<u8> {
        fs::read(self.directory.join("membership-state.json")).unwrap()
    }
    fn prepare(&self) -> MembershipMigrationStatus {
        prepare(&self.store, &self.forwarder, &self.identity, NOW).unwrap()
    }
    fn install(&mut self, id: &str) -> Result<MembershipMigrationStatus, RunnerError> {
        install(
            &mut self.runtime,
            &self.store,
            &mut self.forwarder,
            &self.identity,
            id,
            NOW,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn preparation_is_idempotent_protected_and_does_not_change_authority() {
    let fixture = Fixture::new("prepare");
    let before = fixture.bytes();
    let first = fixture.prepare();
    assert_eq!(first.active_members, 2);
    assert_eq!(first.route_grants, 3);
    assert_eq!(first.current_hostnames, 2);
    assert_eq!(first, fixture.prepare());
    assert_eq!(before, fixture.bytes());
    assert!(fixture.forwarder.checkpoint_sync_state().is_none());
    let encoded = serde_json::to_string(&first).unwrap();
    assert!(!encoded.contains("capability_secret"));
    assert!(!encoded.contains(&fixture.identity.private_key));
    assert!(encoded.len() < 2_048);
    let artifact = MigrationArtifactStore::for_authority(&fixture.store)
        .load("lab", None)
        .unwrap()
        .unwrap();
    let route = &artifact
        .seed
        .payload
        .snapshot
        .payload
        .member(&fixture.identity.peer_id)
        .unwrap()
        .route_grants;
    assert!(
        route
            .iter()
            .any(|route| route.prefix == "10.99.0.0/16" && route.metric == 7)
    );
}

#[test]
fn install_erases_legacy_history_and_gates_restart_without_losing_grants() {
    let mut fixture = Fixture::new("install");
    let prepared = fixture.prepare();
    let id = prepared.migration_id.unwrap();
    let artifacts = MigrationArtifactStore::for_authority(&fixture.store);
    assert_eq!(
        fixture.install(&id).unwrap().phase,
        MembershipMigrationPhase::Installed
    );
    assert!(artifacts.load("lab", None).unwrap().is_none());
    assert_eq!(
        fixture.forwarder.checkpoint_sync_state(),
        Some(MembershipSyncState::ResyncRequired)
    );
    assert!(fixture.forwarder.member_records().is_empty());
    assert!(fixture.forwarder.hostname_records().is_empty());
    assert!(fixture.forwarder.config().network.member_records.is_empty());
    let bytes = fixture.bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["version"], 3);
    for old in [
        "issuer_peer",
        "issuer_public_key",
        "inviter",
        "revoked",
        "records",
    ] {
        assert!(!String::from_utf8_lossy(&bytes).contains(&format!("\"{old}\"")));
    }
    let Some(PersistedAuthority::Checkpoint(loaded)) = fixture
        .store
        .load_authority("lab", &fixture.identity.peer_id, None, None)
        .unwrap()
    else {
        panic!("checkpoint")
    };
    let mut restored = loaded.restore(&fixture.identity.peer_id).unwrap();
    assert_eq!(restored.sync_state(), MembershipSyncState::ResyncRequired);
    assert!(
        restored
            .sign_mutation_at(
                &fixture.identity,
                MembershipChange::RemoveMember(fixture.peer.peer_id.clone()),
                NOW
            )
            .is_err()
    );
    let instant = std::time::Instant::now();
    restored
        .begin_resync(instant, std::time::Duration::from_secs(1))
        .unwrap();
    restored
        .finish_resync(instant + std::time::Duration::from_secs(1), NOW)
        .unwrap();
    let activated = Forwarder::from_checkpoint_config(
        fixture.forwarder.config(),
        &restored,
        &restored.snapshot().payload.anchor,
        NOW,
    )
    .unwrap();
    assert!(
        activated
            .authorized_routes()
            .iter()
            .any(|route| route.prefix.to_string() == "10.42.0.2/32")
    );
    assert!(
        fixture
            .store
            .save("lab", &fixture.identity.peer_id, &[], &[])
            .is_err()
    );
    assert!(fixture.prepare_failure());
}

impl Fixture {
    fn prepare_failure(&self) -> bool {
        prepare(&self.store, &self.forwarder, &self.identity, NOW).is_err()
    }
}

#[test]
fn rejection_before_commit_keeps_legacy_and_requires_exact_fingerprint() {
    let mut fixture = Fixture::new("reject");
    let prepared = fixture.prepare();
    let before = fixture.bytes();
    assert!(fixture.install("wrong-fingerprint").is_err());
    assert_eq!(before, fixture.bytes());
    let network = fixture.forwarder.config().network.name.clone();
    let id = prepared.migration_id.unwrap();
    let result = install_with(
        &mut fixture.runtime,
        &fixture.store,
        &mut fixture.forwarder,
        &fixture.identity,
        &id,
        NOW,
        |_, _| {
            Err(
                super::super::membership_store::MembershipStateStoreError::Io(io::Error::other(
                    "injected failure before rename",
                )),
            )
        },
    );
    assert!(result.is_err());
    assert!(fixture.runtime.is_none());
    assert!(fixture.forwarder.checkpoint_sync_state().is_none());
    assert_eq!(before, fixture.bytes());
    let artifact = MigrationArtifactStore::for_authority(&fixture.store)
        .load(&network, None)
        .unwrap()
        .unwrap();
    assert_eq!(artifact.seed.migration_id().unwrap(), id);
}

#[test]
fn uncertain_visible_write_installs_gate_and_retry_reconfirms_selected_authority() {
    let mut fixture = Fixture::new("uncertain");
    let prepared = fixture.prepare();
    let id = prepared.migration_id.unwrap();
    let store = &fixture.store;
    let identity = &fixture.identity;
    let result = install_with(
        &mut fixture.runtime,
        store,
        &mut fixture.forwarder,
        identity,
        &id,
        NOW,
        |credentials, retained| {
            store.save_checkpoint_with_parent_sync_failure_for_test(
                "lab",
                &identity.peer_id,
                credentials,
                retained,
                None,
            )
        },
    );
    assert!(result.is_err());
    assert!(fixture.runtime.is_some());
    assert_eq!(
        fixture.forwarder.checkpoint_sync_state(),
        Some(MembershipSyncState::ResyncRequired)
    );
    assert!(fixture.forwarder.member_records().is_empty());
    assert_eq!(
        fixture.install(&id).unwrap().phase,
        MembershipMigrationPhase::Installed
    );
    assert!(
        MigrationArtifactStore::for_authority(&fixture.store)
            .load("lab", None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn cancel_and_expiry_retire_only_handoff_never_the_authority() {
    let fixture = Fixture::new("cancel");
    let before = fixture.bytes();
    let id = fixture.prepare().migration_id.unwrap();
    assert!(cancel(&fixture.store, &fixture.forwarder, "wrong").is_err());
    assert_eq!(
        cancel(&fixture.store, &fixture.forwarder, &id)
            .unwrap()
            .phase,
        MembershipMigrationPhase::Absent
    );
    assert_eq!(before, fixture.bytes());
    let prepared = fixture.prepare();
    assert_eq!(
        inspect(
            &fixture.store,
            &fixture.forwarder,
            &fixture.identity,
            prepared.expires_at_unix_seconds.unwrap()
        )
        .unwrap()
        .phase,
        MembershipMigrationPhase::Expired
    );
    assert!(
        retire_due(
            &fixture.store,
            &fixture.forwarder,
            &fixture.identity,
            prepared.expires_at_unix_seconds.unwrap()
        )
        .unwrap()
    );
    assert_eq!(before, fixture.bytes());
    assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
}

#[test]
fn cohort_installs_one_scope_and_each_subject_signs_only_its_own_name() {
    let mut source = Fixture::new("cohort-source");
    let mut recipient = Fixture::with_identity(
        "cohort-recipient",
        source.peer.clone(),
        source.identity.clone(),
    );
    let prepared = source.prepare();
    let artifact = MigrationArtifactStore::for_authority(&source.store)
        .load("lab", None)
        .unwrap()
        .unwrap();
    let config: Config = serde_json::from_value(serde_json::json!({
        "network": {"name": "lab", "local_peer": recipient.identity.peer_id,
            "private_key": recipient.identity.private_key, "vpn_ip": "10.42.0.2",
            "member_records": source.forwarder.member_records(), "dns": {"hostname": "remote-host"}},
        "peers": [{"id": source.identity.peer_id, "vpn_ip": "10.42.0.1",
            "routes": [{"prefix": "10.99.0.0/16", "metric": 7}]}],
    })).unwrap();
    recipient.forwarder = Forwarder::from_config(&config).unwrap();
    recipient
        .store
        .save(
            "lab",
            &recipient.identity.peer_id,
            recipient.forwarder.member_records(),
            &[],
        )
        .unwrap();
    MigrationArtifactStore::for_authority(&recipient.store)
        .save(&artifact, "lab", NOW)
        .unwrap();
    let id = prepared.migration_id.unwrap();
    source.install(&id).unwrap();
    recipient.install(&id).unwrap();
    let a = source.runtime.as_ref().unwrap();
    let b = recipient.runtime.as_ref().unwrap();
    assert_eq!(a.anchor(), b.anchor());
    assert_eq!(a.state().snapshot(), b.state().snapshot());
    assert_eq!(a.state().hostname_claims().len(), 1);
    assert_eq!(b.state().hostname_claims().len(), 1);
    assert_eq!(
        a.state().hostname_claims()[0].payload.subject.peer_id,
        source.identity.peer_id
    );
    assert_eq!(
        b.state().hostname_claims()[0].payload.subject.peer_id,
        recipient.identity.peer_id
    );
    assert_eq!(fs::read_dir(&source.directory).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&recipient.directory).unwrap().count(), 1);
}

#[test]
fn stale_plan_cannot_restore_a_locally_revoked_member_or_drop_new_grants() {
    let mut fixture = Fixture::new("drift");
    let id = fixture.prepare().migration_id.unwrap();
    let before = fixture.bytes();
    let mut changed = fixture.forwarder.config().clone();
    changed.network.routes.push(RouteConfig {
        prefix: "10.98.0.0/16".into(),
        metric: 9,
    });
    fixture.forwarder = Forwarder::from_config(&changed).unwrap();
    assert!(fixture.prepare_failure());
    assert!(fixture.install(&id).is_err());
    changed.network.routes.pop();
    let revoked = issue_named_membership_record_for_subject_at(
        &fixture.identity,
        MembershipRecordIssueOptions {
            network_name: "lab".into(),
            member: MembershipRecordSubject::from_identity(&fixture.peer).unwrap(),
            membership_epoch: 1,
            sequence: 2,
            revoked: true,
            roles: vec![],
            route_grants: vec![],
            expires_at_unix_seconds: None,
        },
        None,
        NOW,
    )
    .unwrap();
    changed.network.member_records.push(revoked);
    fixture.forwarder = Forwarder::from_config(&changed).unwrap();
    let error = fixture.install(&id).unwrap_err();
    assert!(format!("{error:?}").contains("revoked or expired"));
    assert_eq!(before, fixture.bytes());
    assert!(fixture.runtime.is_none());
}

#[test]
fn replayed_handoff_never_rolls_back_newer_installed_removal() {
    let mut fixture = Fixture::new("replay");
    let id = fixture.prepare().migration_id.unwrap();
    let artifacts = MigrationArtifactStore::for_authority(&fixture.store);
    let artifact = artifacts.load("lab", None).unwrap().unwrap();
    fixture.install(&id).unwrap();
    let mut selected = fixture.runtime.as_ref().unwrap().state().clone();
    let instant = std::time::Instant::now();
    selected
        .begin_resync(instant, std::time::Duration::from_secs(1))
        .unwrap();
    selected
        .finish_resync(instant + std::time::Duration::from_secs(1), NOW)
        .unwrap();
    let removal = selected
        .sign_mutation_at(
            &fixture.identity,
            MembershipChange::RemoveMember(fixture.peer.peer_id.clone()),
            NOW,
        )
        .unwrap();
    selected.apply_mutation_at(&removal, NOW).unwrap();
    fixture
        .store
        .save_checkpoint(
            "lab",
            &fixture.identity.peer_id,
            &artifact.credentials,
            &selected.retained(),
        )
        .unwrap();
    artifacts.save(&artifact, "lab", NOW).unwrap();
    let installed = fixture.install(&id).unwrap();
    assert_eq!(installed.active_members, 1);
    assert!(
        fixture
            .runtime
            .as_ref()
            .unwrap()
            .state()
            .snapshot()
            .payload
            .member(&fixture.peer.peer_id)
            .is_none()
    );
    assert!(!String::from_utf8_lossy(&fixture.bytes()).contains(&fixture.peer.peer_id));
}

#[tokio::test]
async fn daemon_migrates_via_control_then_resyncs_and_compacts_removed_member() {
    let fixture_owner = Fixture::new("daemon");
    let fixture = &fixture_owner;
    let (pairing_store, pairing, enrollment) = prepared_migration_pairing(fixture);
    let pairing_store = &pairing_store;
    let enrollment = &enrollment;
    with_migration_daemon(fixture, |control| async move {
        let prepared = control
            .membership_migration(MembershipMigrationRequest::Prepare {})
            .await
            .unwrap();
        assert_eq!(prepared.active_members, 2);
        let installed = control
            .membership_migration(MembershipMigrationRequest::Install {
                accept_id: prepared.migration_id.unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(installed.phase, MembershipMigrationPhase::Installed);
        let compacted = pairing_store.load().unwrap().unwrap();
        let compacted_text = String::from_utf8_lossy(&compacted);
        assert!(!compacted_text.contains(&fixture.peer.peer_id));
        assert!(!compacted_text.contains(&enrollment.response.signature));
        assert!(!compacted_text.contains("issuer_peer"));
        assert!(control.network_peers().await.unwrap().peers.is_empty());
        assert!(
            control
                .revoke_member(Some(fixture.peer.peer_id.clone()))
                .await
                .is_err()
        );
        wait_for_migration_participation(&control).await;
        assert_eq!(control.network_peers().await.unwrap().peers.len(), 2);
        control
            .revoke_member(Some(fixture.peer.peer_id.clone()))
            .await
            .unwrap();
        assert_eq!(control.network_peers().await.unwrap().peers.len(), 1);
        assert!(!String::from_utf8_lossy(&fixture.bytes()).contains(&fixture.peer.peer_id));
        let status = control
            .membership_migration(MembershipMigrationRequest::Inspect {})
            .await
            .unwrap();
        assert_eq!(status.phase, MembershipMigrationPhase::Installed);
        assert_eq!(status.active_members, 1);
        assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 2);
    })
    .await;
    // A stale sidecar restored by an interrupted deployment must not restore legacy
    // authority, and its obsolete proof must disappear before readiness on restart.
    pairing_store
        .save(&pairing.encode_persisted("lab").unwrap())
        .unwrap();
    assert!(
        String::from_utf8_lossy(&pairing_store.load().unwrap().unwrap()).contains("issuer_peer")
    );
    with_migration_daemon(fixture, |control| async move {
        control.state().await.unwrap();
        let compacted = pairing_store.load().unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&compacted).contains(&fixture.peer.peer_id));
        wait_for_migration_participation(&control).await;
        assert_eq!(control.network_peers().await.unwrap().peers.len(), 1);
        assert!(!String::from_utf8_lossy(&fixture.bytes()).contains(&fixture.peer.peer_id));
        assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 2);
    })
    .await;
}

async fn wait_for_migration_participation(control: &RuntimeControlHandle) {
    while !control
        .state()
        .await
        .unwrap()
        .contains(&"checkpoint_sync_state participating".to_owned())
    {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn prepared_migration_pairing(
    fixture: &Fixture,
) -> (PairingStateStore, CodePairingSessions, PairingEnrollment) {
    let store = PairingStateStore::encrypted(
        fixture.directory.join("pairing-state"),
        &fixture.identity.private_key,
        "lab",
        &fixture.identity.peer_id,
    )
    .unwrap();
    let offer = crate::pairing::export_code_pairing_offer_at(
        fixture.forwarder.config(),
        crate::pairing::PairingOfferOptions::default(),
        NOW,
    )
    .unwrap();
    let response = crate::pairing::build_pairing_response_at(
        fixture.forwarder.config(),
        &offer,
        crate::pairing::PairingResponseOptions {
            joiner_peer: fixture.peer.peer_id.clone(),
            assigned_vpn_ip: None,
            membership_key: None,
            member_records: fixture.forwarder.member_records().to_vec(),
            expires_in_seconds: 600,
        },
        NOW,
    )
    .unwrap();
    let mut pairing = CodePairingSessions::new();
    let opened = pairing
        .open("lab", 600, NOW, std::time::Instant::now())
        .unwrap();
    let operation_id = opened.operation_id;
    let mut request = crate::pairing::build_pairing_request_at(
        &offer,
        crate::pairing::PairingRequestOptions {
            identity: fixture.peer.clone(),
            requested_vpn_ip: None,
            requested_routes: vec![],
        },
        NOW,
    )
    .unwrap();
    request.code_authentication = Some(crate::pairing::PairingCodeAuthentication {
        locator: opened
            .code
            .parse::<crate::pairing_code::PairingCode>()
            .unwrap()
            .locator("lab")
            .unwrap(),
        confirmation: STANDARD.encode([7; 32]),
    });
    let approval = PendingApproval::new(
        operation_id.clone(),
        fixture.peer.peer_id.parse().unwrap(),
        NOW + 600,
        request,
    )
    .unwrap();
    let approval_id = approval.approval_id.clone();
    let transcript_sha256 = approval.transcript_sha256.clone();
    pairing.set_pending_approval(approval).unwrap();
    let enrollment = pairing
        .prepare_enrollment(
            "lab",
            PairingEnrollmentPreparation {
                operation_id: operation_id.clone(),
                role: PairingEnrollmentRole::Inviter,
                approval_id: Some(approval_id),
                offer: Some(offer),
                response,
                transcript_sha256,
                membership_key_preconfigured: None,
            },
        )
        .unwrap()
        .clone();
    pairing.recover_prepared_open("lab", &enrollment).unwrap();
    pairing.mark_enrollment_applied(&operation_id).unwrap();
    store
        .save(&pairing.encode_persisted("lab").unwrap())
        .unwrap();
    (store, pairing, enrollment)
}

async fn with_migration_daemon<F, Fut>(fixture: &Fixture, assertion: F)
where
    F: FnOnce(RuntimeControlHandle) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    struct EmptyPackets;
    impl PacketRead for EmptyPackets {
        fn read_packet(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }
    impl PacketWrite for EmptyPackets {
        fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
            Ok(packet.len())
        }
    }
    let mut config = fixture.forwarder.config().clone();
    config.network.listen_addresses.clear();
    config.network.discovery = crate::config::DiscoveryConfig {
        mdns: false,
        kademlia: false,
        autonat: false,
        dcutr: false,
        kademlia_provider_advertisement: false,
        ..crate::config::DiscoveryConfig::default()
    };
    config.network.relay.auto.max_reservations = 0;
    config.network.packet_plane.listen.clear();
    config.network.packet_plane.quic_listen.clear();
    let (control, receiver) = runtime_control_channel();
    let platform = RuntimePlatform::new(
        PacketIo::new(EmptyPackets, EmptyPackets),
        PreconfiguredTunRoutes,
    )
    .with_control(receiver);
    let daemon = tokio::spawn(run_config_until_with_runtime_platform(
        config,
        platform,
        None,
        None,
        Some(fixture.directory.join("pairing-state")),
        Some(fixture.directory.join("membership-state.json")),
        std::future::pending(),
    ));
    let deadline = tokio::time::timeout(
        std::time::Duration::from_secs(35),
        assertion(control.clone()),
    )
    .await;
    let _ = control.shutdown().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    deadline.expect("migration/restart, resync and retirement finish autonomously");
}
