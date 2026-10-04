use super::*;
use crate::membership::checkpoint::{
    CheckpointMember, NetworkAnchor, SnapshotPublisher, SnapshotRank,
};
use crate::pairing::checkpoint_grant::PairingCheckpointGrant;
use crate::runtime::pairing_store::PairingStateStore;
use std::{cell::RefCell, fs, os::unix::fs::PermissionsExt as _, path::PathBuf};

fn retire(sessions: &mut CodePairingSessions, now: u64) -> bool {
    sessions
        .retire_checkpoint_artifacts_with::<CodePairingSessionError>(
            "runners",
            &[],
            now,
            |_| Ok(()),
            || Ok(None),
        )
        .unwrap()
}

fn applied(role: PairingEnrollmentRole) -> (CodePairingSessions, PairingEnrollment) {
    let (mut sessions, entry) = match role {
        PairingEnrollmentRole::Inviter => {
            let (sessions, entry, _, _, _) = prepared_open_fixture(600);
            (sessions, entry)
        }
        PairingEnrollmentRole::Joiner => {
            let (sessions, entry, _) = prepared_join_fixture(600);
            (sessions, entry)
        }
    };
    match role {
        PairingEnrollmentRole::Inviter => {
            sessions.recover_prepared_open("runners", &entry).unwrap();
        }
        PairingEnrollmentRole::Joiner => sessions.recover_prepared_join("runners", &entry).unwrap(),
    }
    sessions
        .mark_enrollment_applied_at(&entry.operation_id, 1_010)
        .unwrap();
    let entry = sessions.enrollment(&entry.operation_id).unwrap().clone();
    (sessions, entry)
}

#[test]
fn installed_authority_retires_all_completed_legacy_proof_copies() {
    for role in [
        PairingEnrollmentRole::Inviter,
        PairingEnrollmentRole::Joiner,
    ] {
        let (mut sessions, entry) = applied(role);
        assert!(retire(&mut sessions, 1_020));
        let bytes = sessions.encode_persisted("runners").unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        for removed in [
            &entry.response.payload.inviter_peer,
            &entry.response.payload.joiner_peer,
            &entry.response.signature,
            &entry.response.payload.inviter_public_key,
            &entry.operation_id,
        ] {
            assert!(!text.contains(removed), "old profile or proof retained");
        }
        assert_eq!(
            sessions.active_replay_tokens(1_020).collect::<Vec<_>>(),
            vec![entry.response.payload.rendezvous_token.as_str()]
        );
        let mut restarted =
            CodePairingSessions::restore_persisted(&bytes, "runners", 1_030, Instant::now())
                .unwrap();
        assert!(!retire(&mut restarted, 1_030));
        assert!(retire(&mut restarted, 1_601));
        assert!(restarted.active_replay_tokens(1_601).next().is_none());
        assert!(restarted.encode_persisted("runners").unwrap().len() < 512);
    }
}

#[test]
fn acknowledged_inviter_polling_ticket_is_not_a_hidden_history_archive() {
    let (mut sessions, entry) = applied(PairingEnrollmentRole::Inviter);
    sessions
        .acknowledge_enrollment(&entry.operation_id, &entry.transcript_sha256, 1_020)
        .unwrap();
    assert_eq!(sessions.retained_inbound_tickets.len(), 1);
    assert!(retire(&mut sessions, 1_030));
    assert!(sessions.retained_inbound_tickets.is_empty());
    assert!(sessions.receipts.is_empty());
    let text = String::from_utf8(sessions.encode_persisted("runners").unwrap()).unwrap();
    assert!(!text.contains(&entry.response.signature));
    assert!(!text.contains(&entry.response.payload.joiner_peer));
}

#[test]
fn unfinished_and_aborting_ownership_survives_retirement_even_after_expiry() {
    for state in [
        PairingEnrollmentState::Prepared,
        PairingEnrollmentState::Aborting,
    ] {
        for role in [
            PairingEnrollmentRole::Inviter,
            PairingEnrollmentRole::Joiner,
        ] {
            let (mut sessions, entry) = match role {
                PairingEnrollmentRole::Inviter => {
                    let (sessions, entry, _, _, _) = prepared_open_fixture(600);
                    (sessions, entry)
                }
                PairingEnrollmentRole::Joiner => {
                    let (sessions, entry, _) = prepared_join_fixture(600);
                    (sessions, entry)
                }
            };
            sessions.enrollments[0].state = state;
            let before = sessions.encode_persisted("runners").unwrap();
            assert!(!retire(&mut sessions, 5_000));
            assert_eq!(before, sessions.encode_persisted("runners").unwrap());
            assert_eq!(
                sessions.enrollment(&entry.operation_id).unwrap().state,
                state
            );
        }
    }
}

#[test]
fn retirement_preserves_unrelated_live_operation_timers_and_lan_candidates() {
    let (mut sessions, entry) = applied(PairingEnrollmentRole::Joiner);
    let now = Instant::now();
    let current = sessions
        .join(
            "runners",
            PairingCode::generate(),
            None,
            vec![],
            600,
            1_030,
            now,
        )
        .unwrap();
    sessions.record_lan_candidate(peer(3), "/ip4/192.0.2.3/tcp/4001".parse().unwrap(), now);
    let started_at = sessions.join.as_ref().unwrap().started_at;
    assert!(retire(&mut sessions, 1_040));
    assert_eq!(sessions.join.as_ref().unwrap().started_at, started_at);
    assert_eq!(sessions.join.as_ref().unwrap().id, current.operation_id);
    assert!(sessions.join_status(&current.operation_id).is_ok());
    assert!(sessions.enrollment(&entry.operation_id).is_none());
    assert_eq!(sessions.lan_addresses(peer(3)).len(), 1);
}

#[test]
fn prewrite_failure_preserves_state_and_visible_failure_blocks_rollback_until_resynced() {
    let (mut sessions, _) = applied(PairingEnrollmentRole::Joiner);
    let previous = sessions.encode_persisted("runners").unwrap();
    let failure = sessions.retire_checkpoint_artifacts_with::<CodePairingSessionError>(
        "runners",
        &[],
        1_020,
        |_| Err(CodePairingSessionError::Conflict),
        || Ok(Some(previous.clone())),
    );
    assert!(failure.is_err());
    assert!(!sessions.checkpoint_completion_uncertain());
    assert_eq!(previous, sessions.encode_persisted("runners").unwrap());

    let visible = RefCell::new(previous.clone());
    assert!(
        sessions
            .retire_checkpoint_artifacts_with::<CodePairingSessionError>(
                "runners",
                &[],
                1_020,
                |next| {
                    *visible.borrow_mut() = next.to_vec();
                    Err(CodePairingSessionError::Conflict)
                },
                || Ok(Some(visible.borrow().clone())),
            )
            .is_err()
    );
    assert!(sessions.checkpoint_completion_uncertain());
    assert!(
        sessions
            .open("runners", 600, 1_021, Instant::now())
            .is_err()
    );
    assert!(
        sessions
            .retire_checkpoint_artifacts_with::<CodePairingSessionError>(
                "runners",
                &[],
                1_021,
                |_| panic!("ambiguous state must not be overwritten"),
                || Ok(None),
            )
            .is_err()
    );
    assert!(
        sessions
            .reconcile_checkpoint_completion_with::<CodePairingSessionError>(
                "runners",
                1_021,
                || Ok(Some(visible.borrow().clone())),
                |_| Err(CodePairingSessionError::Conflict),
            )
            .is_err()
    );
    assert!(sessions.checkpoint_completion_uncertain());
    sessions
        .reconcile_checkpoint_completion_with::<CodePairingSessionError>(
            "runners",
            1_021,
            || Ok(Some(visible.borrow().clone())),
            |bytes| {
                assert_eq!(*visible.borrow(), bytes);
                Ok(())
            },
        )
        .unwrap();
    assert!(!sessions.checkpoint_completion_uncertain());
    assert!(sessions.enrollments.is_empty());
    assert_eq!(
        *visible.borrow(),
        sessions.encode_persisted("runners").unwrap()
    );
}

struct ProtectedState {
    directory: PathBuf,
    store: PairingStateStore,
}

impl ProtectedState {
    fn new() -> Self {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let directory = std::env::temp_dir().join(format!(
            "p2p-vpn-pairing-retirement-{}-{}",
            std::process::id(),
            identity.peer_id
        ));
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let store = PairingStateStore::encrypted(
            directory.join("pairing-state"),
            &identity.private_key,
            "runners",
            &identity.peer_id,
        )
        .unwrap();
        Self { directory, store }
    }
}

impl Drop for ProtectedState {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn encrypted_retirement_survives_restart_without_a_backup_or_device_receipt() {
    let fixture = ProtectedState::new();
    let (mut sessions, entry) = applied(PairingEnrollmentRole::Inviter);
    fixture
        .store
        .save(&sessions.encode_persisted("runners").unwrap())
        .unwrap();
    sessions
        .retire_checkpoint_artifacts_with::<crate::runtime::runner::RunnerError>(
            "runners",
            &[],
            1_020,
            |bytes| fixture.store.save(bytes).map_err(Into::into),
            || fixture.store.load().map_err(Into::into),
        )
        .unwrap();
    let plaintext = fixture.store.load().unwrap().unwrap();
    assert!(!String::from_utf8_lossy(&plaintext).contains(&entry.response.payload.joiner_peer));
    assert!(
        CodePairingSessions::restore_persisted(&plaintext, "runners", 1_030, Instant::now())
            .unwrap()
            .enrollments
            .is_empty()
    );
    assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
    assert_eq!(
        fs::metadata(fixture.store.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn repeated_retirement_has_a_fixed_replay_window_and_constant_post_expiry_storage() {
    let fixture = ProtectedState::new();
    let mut sessions = CodePairingSessions::new();
    let mut largest = 0;
    for window in 0..2 {
        let now = 1_020 + window * 4_000;
        for index in 0..MAX_CODE_PAIRING_REPLAY_TOKENS {
            let (_, mut entry) = applied(PairingEnrollmentRole::Joiner);
            let removed_peer = NodeIdentity::generate_ed25519().unwrap().peer_id;
            entry.response.payload.joiner_peer.clone_from(&removed_peer);
            entry.response.payload.rendezvous_token =
                URL_SAFE_NO_PAD.encode((index as u128).to_be_bytes());
            entry.offer.as_mut().unwrap().payload.rendezvous_token =
                entry.response.payload.rendezvous_token.clone();
            entry.response.payload.issued_at_unix_seconds = now - 1;
            entry.response.payload.expires_at_unix_seconds = now + 600;
            sessions.enrollments.push(entry);
            sessions
                .retire_checkpoint_artifacts_with::<crate::runtime::runner::RunnerError>(
                    "runners",
                    &[],
                    now,
                    |bytes| fixture.store.save(bytes).map_err(Into::into),
                    || fixture.store.load().map_err(Into::into),
                )
                .unwrap();
            largest = largest.max(fs::metadata(fixture.store.path()).unwrap().len());
            assert!(sessions.enrollments.is_empty());
            assert!(sessions.receipts.is_empty());
            assert!(sessions.replay_tokens.len() <= MAX_CODE_PAIRING_REPLAY_TOKENS);
            assert!(
                !String::from_utf8_lossy(&fixture.store.load().unwrap().unwrap())
                    .contains(&removed_peer)
            );
        }
        sessions
            .retire_checkpoint_artifacts_with::<crate::runtime::runner::RunnerError>(
                "runners",
                &[],
                now + 601,
                |bytes| fixture.store.save(bytes).map_err(Into::into),
                || fixture.store.load().map_err(Into::into),
            )
            .unwrap();
        assert!(sessions.replay_tokens.is_empty());
        assert!(fs::metadata(fixture.store.path()).unwrap().len() < 512);
    }
    assert!(
        largest < 64 * 1024,
        "opaque window must not grow with historical churn"
    );
    assert_eq!(fs::read_dir(&fixture.directory).unwrap().count(), 1);
}

fn member(byte: u8) -> CheckpointMember {
    let key = libp2p::identity::Keypair::ed25519_from_bytes([byte; 32]).unwrap();
    CheckpointMember {
        subject: SnapshotPublisher {
            peer_id: key.public().to_peer_id().to_string(),
            public_key: STANDARD.encode(key.public().encode_protobuf()),
        },
        incarnation: [byte; 32],
        roles: vec![crate::membership::MembershipRole::OverlayMember],
        route_grants: vec![],
        expires_at_unix_seconds: None,
    }
}

#[test]
fn current_checkpoint_results_expire_and_removed_or_reincarnated_subjects_are_retired() {
    let (mut sessions, _) = applied(PairingEnrollmentRole::Joiner);
    let members = [member(1), member(2)];
    let grant = PairingCheckpointGrant {
        version: 1,
        anchor: NetworkAnchor::new([7; 32]).unwrap(),
        capability_secret: STANDARD.encode([8; 32]),
        minimum: SnapshotRank {
            authority_revision: 1,
            active_member_count: 2,
            digest: [9; 32],
        },
        inviter: members[0].clone(),
        joiner: members[1].clone(),
    };
    sessions.enrollments[0].response.payload.checkpoint = Some(grant.clone());
    sessions
        .join
        .as_mut()
        .unwrap()
        .completed
        .as_mut()
        .unwrap()
        .1
        .payload
        .checkpoint = Some(grant);
    assert!(
        !sessions
            .retire_checkpoint_artifacts_with::<CodePairingSessionError>(
                "runners",
                &members,
                1_020,
                |_| panic!("current export must remain"),
                || Ok(None),
            )
            .unwrap()
    );
    let response = sessions.enrollments[0].response.clone();
    assert!(super::super::checkpoint_retirement::obsolete_response(
        &response, &members, 1_601
    ));
    assert!(super::super::checkpoint_retirement::obsolete_response(
        &response,
        &members[..1],
        1_020
    ));
    let mut reincarnated = members.clone();
    reincarnated[1].incarnation = [4; 32];
    assert!(
        sessions
            .retire_checkpoint_artifacts_with::<CodePairingSessionError>(
                "runners",
                &reincarnated,
                1_020,
                |_| Ok(()),
                || Ok(None),
            )
            .unwrap()
    );
    assert!(sessions.enrollments.is_empty());
    assert!(sessions.join.is_none());
}

#[test]
fn acknowledged_metadata_and_old_long_lived_tokens_have_nonrenewing_deadlines() {
    let (mut sessions, entry) = applied(PairingEnrollmentRole::Inviter);
    sessions
        .acknowledge_enrollment(&entry.operation_id, &entry.transcript_sha256, 1_020)
        .unwrap();
    sessions.receipts[0].expires_at_unix_seconds = u64::MAX;
    sessions.replay_tokens[0].expires_at_unix_seconds = u64::MAX;
    sessions.retained_inbound_tickets.clear();
    let members = [member(1), member(2)];
    let mut compact = |now| {
        sessions
            .retire_checkpoint_artifacts_with::<CodePairingSessionError>(
                "runners",
                &members,
                now,
                |_| Ok(()),
                || Ok(None),
            )
            .unwrap()
    };
    assert!(compact(1_030));
    assert!(!compact(1_040));
    assert_eq!(sessions.receipts[0].expires_at_unix_seconds, 4_610);
    assert_eq!(sessions.replay_tokens[0].expires_at_unix_seconds, 4_630);
    let bytes = sessions.encode_persisted("runners").unwrap();
    let mut restarted =
        CodePairingSessions::restore_persisted(&bytes, "runners", 4_631, Instant::now()).unwrap();
    assert!(
        restarted
            .retire_checkpoint_artifacts_with::<CodePairingSessionError>(
                "runners",
                &members,
                4_631,
                |_| Ok(()),
                || Ok(None),
            )
            .unwrap()
    );
    assert!(restarted.receipts.is_empty());
    assert!(restarted.replay_tokens.is_empty());
}

#[test]
fn full_live_replay_window_never_evicts_a_guard_or_claims_durable_retirement() {
    let (mut sessions, _) = applied(PairingEnrollmentRole::Joiner);
    sessions.replay_tokens = (0..MAX_CODE_PAIRING_REPLAY_TOKENS)
        .map(|index| PairingReplayToken {
            token: URL_SAFE_NO_PAD.encode((index as u128).to_be_bytes()),
            expires_at_unix_seconds: 1_600,
            aborted_operation_id: None,
        })
        .collect();
    let before = sessions.encode_persisted("runners").unwrap();
    assert!(matches!(
        sessions.retire_checkpoint_artifacts_with::<CodePairingSessionError>(
            "runners",
            &[],
            1_020,
            |_| panic!("over-capacity plan must not be saved"),
            || Ok(None),
        ),
        Err(CodePairingSessionError::Capacity)
    ));
    assert_eq!(before, sessions.encode_persisted("runners").unwrap());
    assert!(retire(&mut sessions, 1_601));
    assert!(sessions.enrollments.is_empty());
    assert!(sessions.replay_tokens.is_empty());
}

#[test]
fn expiry_since_last_save_is_not_misclassified_as_an_ambiguous_failed_write() {
    let (mut sessions, _) = applied(PairingEnrollmentRole::Joiner);
    sessions.replay_tokens.push(PairingReplayToken {
        token: URL_SAFE_NO_PAD.encode([5; 16]),
        expires_at_unix_seconds: 1_015,
        aborted_operation_id: None,
    });
    let durable = sessions.encode_persisted("runners").unwrap();
    sessions.expire(1_020, Instant::now());
    let in_memory = sessions.encode_persisted("runners").unwrap();
    assert_ne!(durable, in_memory);
    assert!(
        sessions
            .retire_checkpoint_artifacts_with::<CodePairingSessionError>(
                "runners",
                &[],
                1_020,
                |_| Err(CodePairingSessionError::Conflict),
                || Ok(Some(durable.clone())),
            )
            .is_err()
    );
    assert!(!sessions.checkpoint_completion_uncertain());
    assert_eq!(in_memory, sessions.encode_persisted("runners").unwrap());

    let visible = RefCell::new(durable);
    assert!(
        sessions
            .retire_checkpoint_artifacts_with::<CodePairingSessionError>(
                "runners",
                &[],
                1_020,
                |next| {
                    *visible.borrow_mut() = next.to_vec();
                    Err(CodePairingSessionError::Conflict)
                },
                || Ok(Some(visible.borrow().clone())),
            )
            .is_err()
    );
    sessions
        .reconcile_checkpoint_completion_with::<CodePairingSessionError>(
            "runners",
            1_021,
            || Ok(Some(visible.borrow().clone())),
            |_| Ok(()),
        )
        .unwrap();
    assert!(!sessions.checkpoint_completion_uncertain());
    assert!(sessions.enrollments.is_empty());
}
