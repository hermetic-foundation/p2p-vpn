//! Small, signed-response enrollment credentials, not an authoritative snapshot.
//! Recipients must fetch a remote authenticated snapshot at or above `minimum`
//! before participating. Fork choice remains cooperative, not irreversible.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::{
    identity::NodeIdentity,
    membership::checkpoint::{
        CheckpointError, CheckpointMember, CooperativeMembershipState, MAX_CAPABILITY_BYTES,
        MAX_CHECKPOINT_MEMBERS, MembershipSyncState, NetworkAnchor, NetworkCapability,
        SnapshotRank,
    },
};

pub const PAIRING_CHECKPOINT_GRANT_VERSION: u8 = 1;
pub const MAX_PAIRING_CHECKPOINT_GRANT_BYTES: usize = 16 * 1024;
const MAX_ENCODED_CAPABILITY_BYTES: usize = MAX_CAPABILITY_BYTES.div_ceil(3) * 4;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingCheckpointGrant {
    pub version: u8,
    pub anchor: NetworkAnchor,
    pub capability_secret: String,
    pub minimum: SnapshotRank,
    pub inviter: CheckpointMember,
    pub joiner: CheckpointMember,
}

impl fmt::Debug for PairingCheckpointGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingCheckpointGrant")
            .field("version", &self.version)
            .field("anchor", &self.anchor)
            .field("capability_secret", &"[REDACTED]")
            .field("minimum", &self.minimum)
            .field("inviter", &self.inviter)
            .field("joiner", &self.joiner)
            .finish()
    }
}

impl PairingCheckpointGrant {
    /// The caller must durably admit the joiner before issuing enrollment.
    pub fn from_state_at(
        state: &CooperativeMembershipState,
        capability_secret: &[u8],
        joiner_peer: &str,
        now_unix_seconds: u64,
    ) -> Result<Self, CheckpointGrantError> {
        if state.sync_state() != MembershipSyncState::Participating {
            return Err(CheckpointGrantError::Invalid(
                "inviter must be participating",
            ));
        }
        let snapshot = state.snapshot();
        NetworkCapability::from_secret(snapshot.payload.anchor.clone(), Some(capability_secret))?
            .verify(snapshot)?;
        let inviter = snapshot
            .payload
            .member(state.local_peer())
            .ok_or(CheckpointGrantError::Invalid("inviter is not admitted"))?
            .clone();
        let joiner = snapshot
            .payload
            .member(joiner_peer)
            .ok_or(CheckpointGrantError::Invalid("joiner is not admitted"))?
            .clone();
        let grant = Self {
            version: PAIRING_CHECKPOINT_GRANT_VERSION,
            anchor: snapshot.payload.anchor.clone(),
            capability_secret: STANDARD.encode(capability_secret),
            minimum: snapshot.payload.rank()?,
            inviter,
            joiner,
        };
        grant.validate_at(now_unix_seconds)?;
        Ok(grant)
    }

    /// Validates descriptors and derives a capability, not snapshot/packet grants.
    /// The complete response signature must be checked before using credentials.
    pub fn validate_for(
        &self,
        inviter_peer: &str,
        inviter_public_key: &str,
        joiner_peer: &str,
        now_unix_seconds: u64,
    ) -> Result<NetworkCapability, CheckpointGrantError> {
        let capability = self.validate_at(now_unix_seconds)?;
        if self.inviter.subject.peer_id != inviter_peer
            || self.inviter.subject.public_key != inviter_public_key
            || self.joiner.subject.peer_id != joiner_peer
        {
            return Err(CheckpointGrantError::Invalid(
                "response participant/key mismatch",
            ));
        }
        Ok(capability)
    }

    pub fn validate_joiner_identity(
        &self,
        identity: &NodeIdentity,
    ) -> Result<(), CheckpointGrantError> {
        self.joiner.validate()?;
        if identity.peer_id != self.joiner.subject.peer_id
            || STANDARD.encode(identity.public_key_protobuf()?) != self.joiner.subject.public_key
        {
            return Err(CheckpointGrantError::Invalid(
                "joiner identity/key mismatch",
            ));
        }
        Ok(())
    }

    /// Bounded decode is available for storage/contract consumers outside pairing.
    pub fn decode_at(bytes: &[u8], now_unix_seconds: u64) -> Result<Self, CheckpointGrantError> {
        if bytes.len() > MAX_PAIRING_CHECKPOINT_GRANT_BYTES {
            return Err(CheckpointGrantError::TooLarge(bytes.len()));
        }
        let grant: Self = serde_json::from_slice(bytes)?;
        grant.validate_at(now_unix_seconds)?;
        Ok(grant)
    }

    pub fn encode_at(&self, now_unix_seconds: u64) -> Result<Vec<u8>, CheckpointGrantError> {
        self.validate_at(now_unix_seconds)?;
        Ok(serde_json::to_vec(self)?)
    }

    /// Only for callers persisting protected credentials after signed validation.
    pub fn secret_bytes(&self) -> Result<Vec<u8>, CheckpointGrantError> {
        if self.capability_secret.len() > MAX_ENCODED_CAPABILITY_BYTES {
            return Err(CheckpointGrantError::Invalid(
                "encoded capability exceeds bound",
            ));
        }
        let bytes = STANDARD.decode(&self.capability_secret)?;
        if !(32..=MAX_CAPABILITY_BYTES).contains(&bytes.len())
            || STANDARD.encode(&bytes) != self.capability_secret
        {
            return Err(CheckpointGrantError::Invalid(
                "noncanonical or invalid capability",
            ));
        }
        Ok(bytes)
    }

    fn validate_at(
        &self,
        now_unix_seconds: u64,
    ) -> Result<NetworkCapability, CheckpointGrantError> {
        if self.version != PAIRING_CHECKPOINT_GRANT_VERSION {
            return Err(CheckpointGrantError::UnsupportedVersion(self.version));
        }
        self.minimum.validate()?;
        if self.minimum.authority_revision == 0
            || !(2..=MAX_CHECKPOINT_MEMBERS).contains(&self.minimum.active_member_count)
        {
            return Err(CheckpointGrantError::Invalid("invalid enrollment floor"));
        }
        self.inviter.validate()?;
        self.joiner.validate()?;
        if self.inviter.subject.peer_id == self.joiner.subject.peer_id {
            return Err(CheckpointGrantError::Invalid(
                "participants must be distinct",
            ));
        }
        if !self.inviter.active_at(now_unix_seconds) || !self.joiner.active_at(now_unix_seconds) {
            return Err(CheckpointGrantError::Invalid("expired enrollment member"));
        }
        let capability =
            NetworkCapability::from_secret(self.anchor.clone(), Some(&self.secret_bytes()?))?;
        let length = serde_json::to_vec(self)?.len();
        if length > MAX_PAIRING_CHECKPOINT_GRANT_BYTES {
            return Err(CheckpointGrantError::TooLarge(length));
        }
        Ok(capability)
    }
}

#[derive(Debug)]
pub enum CheckpointGrantError {
    Core(CheckpointError),
    Identity(crate::identity::IdentityError),
    Json(serde_json::Error),
    Base64(base64::DecodeError),
    UnsupportedVersion(u8),
    Invalid(&'static str),
    TooLarge(usize),
}

impl From<CheckpointError> for CheckpointGrantError {
    fn from(error: CheckpointError) -> Self {
        Self::Core(error)
    }
}

impl From<crate::identity::IdentityError> for CheckpointGrantError {
    fn from(error: crate::identity::IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<serde_json::Error> for CheckpointGrantError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<base64::DecodeError> for CheckpointGrantError {
    fn from(error: base64::DecodeError) -> Self {
        Self::Base64(error)
    }
}

#[cfg(test)]
mod tests {
    use crate::membership::{
        MAX_MEMBERSHIP_RECORD_INTEGER, MembershipRole,
        checkpoint::{MembershipChange, SnapshotPolicy},
    };

    use super::*;

    fn fixture_with_secret(
        secret: &[u8],
    ) -> (
        NodeIdentity,
        NodeIdentity,
        CooperativeMembershipState,
        PairingCheckpointGrant,
    ) {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let capability = NetworkCapability::from_secret(
            NetworkAnchor::new([7; 32]).expect("anchor"),
            Some(secret),
        )
        .expect("capability");
        let mut state = CooperativeMembershipState::bootstrap_at(
            capability,
            inviter.peer_id.clone(),
            vec![CheckpointMember::new(&inviter).expect("inviter member")],
            SnapshotPolicy::default(),
            1_000,
        )
        .expect("initial network");
        let admission = state
            .sign_mutation_at(
                &inviter,
                MembershipChange::UpsertMember(
                    CheckpointMember::new(&joiner).expect("joiner member"),
                ),
                1_000,
            )
            .expect("admission");
        state
            .apply_mutation_at(&admission, 1_000)
            .expect("apply admission");
        let grant = PairingCheckpointGrant::from_state_at(&state, secret, &joiner.peer_id, 1_000)
            .expect("grant");
        (inviter, joiner, state, grant)
    }

    #[test]
    fn authenticated_current_state_creates_bounded_credentials_and_floor_only() {
        let (inviter, joiner, state, grant) = fixture_with_secret(&[11; 32]);
        let capability = grant
            .validate_for(
                &inviter.peer_id,
                &STANDARD.encode(inviter.public_key_protobuf().expect("key")),
                &joiner.peer_id,
                1_001,
            )
            .expect("context");
        capability
            .verify(state.snapshot())
            .expect("same authenticated network");
        grant
            .validate_joiner_identity(&joiner)
            .expect("actual joiner key");
        assert_eq!(
            grant.minimum,
            state.snapshot().payload.rank().expect("rank")
        );
        assert_eq!(grant.minimum.authority_revision, 1);
        assert_eq!(grant.minimum.active_member_count, 2);
        assert_eq!(grant.secret_bytes().expect("secret"), [11; 32]);
        let bytes = grant.encode_at(1_001).expect("encoding");
        assert!(bytes.len() < MAX_PAIRING_CHECKPOINT_GRANT_BYTES);
        assert_eq!(
            PairingCheckpointGrant::decode_at(&bytes, 1_001).expect("decode"),
            grant
        );
        let json = serde_json::to_value(&grant).expect("JSON");
        assert!(json.get("snapshot").is_none());
        assert!(json.get("member_records").is_none());
    }

    #[test]
    fn builder_requires_matching_secret_and_participating_current_inviter() {
        let (inviter, joiner, state, _) = fixture_with_secret(&[11; 32]);
        assert!(matches!(
            PairingCheckpointGrant::from_state_at(&state, &[12; 32], &joiner.peer_id, 1_000),
            Err(CheckpointGrantError::Core(CheckpointError::InvalidMac))
        ));
        let restored = CooperativeMembershipState::restore(
            NetworkCapability::from_secret(
                state.snapshot().payload.anchor.clone(),
                Some(&[11; 32]),
            )
            .expect("capability"),
            inviter.peer_id.clone(),
            state.retained(),
        )
        .expect("restored");
        assert!(
            PairingCheckpointGrant::from_state_at(&restored, &[11; 32], &joiner.peer_id, 1_000)
                .is_err()
        );
        let mut departed = state;
        let removal = departed
            .sign_mutation_at(
                &inviter,
                MembershipChange::RemoveMember(inviter.peer_id.clone()),
                1_001,
            )
            .expect("departure");
        departed
            .apply_mutation_at(&removal, 1_001)
            .expect("apply departure");
        assert_eq!(departed.sync_state(), MembershipSyncState::Excluded);
        assert!(
            PairingCheckpointGrant::from_state_at(&departed, &[11; 32], &joiner.peer_id, 1_001)
                .is_err()
        );
    }

    #[test]
    fn builder_requires_admitted_distinct_and_unexpired_members() {
        let (inviter, joiner, mut state, _) = fixture_with_secret(&[11; 32]);
        let stranger = NodeIdentity::generate_ed25519().expect("stranger");
        assert!(
            PairingCheckpointGrant::from_state_at(&state, &[11; 32], &stranger.peer_id, 1_000)
                .is_err()
        );
        assert!(
            PairingCheckpointGrant::from_state_at(&state, &[11; 32], &inviter.peer_id, 1_000)
                .is_err()
        );
        let mut member = state
            .snapshot()
            .payload
            .member(&joiner.peer_id)
            .expect("member")
            .clone();
        member.expires_at_unix_seconds = Some(1_010);
        let update = state
            .sign_mutation_at(&inviter, MembershipChange::UpsertMember(member), 1_001)
            .expect("expiry change");
        state
            .apply_mutation_at(&update, 1_001)
            .expect("apply expiry");
        PairingCheckpointGrant::from_state_at(&state, &[11; 32], &joiner.peer_id, 1_009)
            .expect("still active");
        assert!(
            PairingCheckpointGrant::from_state_at(&state, &[11; 32], &joiner.peer_id, 1_010)
                .is_err()
        );
    }

    #[test]
    fn builder_does_not_issue_a_genesis_floor() {
        let (inviter, joiner, state, _) = fixture_with_secret(&[11; 32]);
        let genesis = CooperativeMembershipState::bootstrap_at(
            NetworkCapability::from_secret(
                state.snapshot().payload.anchor.clone(),
                Some(&[11; 32]),
            )
            .expect("capability"),
            inviter.peer_id.clone(),
            vec![
                CheckpointMember::new(&inviter).expect("member"),
                CheckpointMember::new(&joiner).expect("member"),
            ],
            SnapshotPolicy::default(),
            1_000,
        )
        .expect("genesis");
        assert!(
            PairingCheckpointGrant::from_state_at(&genesis, &[11; 32], &joiner.peer_id, 1_000)
                .is_err()
        );
    }

    #[test]
    fn grant_requires_canonical_secret_with_bounded_decoded_length() {
        let (_, _, _, mut grant) = fixture_with_secret(&[11; 32]);
        for length in [0, 31, MAX_CAPABILITY_BYTES + 1] {
            grant.capability_secret = STANDARD.encode(vec![11; length]);
            assert!(grant.secret_bytes().is_err(), "length {length}");
        }
        grant.capability_secret = STANDARD.encode([11; 32]);
        grant.capability_secret.pop();
        assert!(grant.secret_bytes().is_err(), "padding is required");
        grant.capability_secret = "A".repeat(MAX_ENCODED_CAPABILITY_BYTES + 1);
        assert!(matches!(
            grant.secret_bytes(),
            Err(CheckpointGrantError::Invalid(_))
        ));
        let (_, _, _, maximum) = fixture_with_secret(&vec![13; MAX_CAPABILITY_BYTES]);
        assert_eq!(
            maximum.secret_bytes().expect("maximum secret").len(),
            MAX_CAPABILITY_BYTES
        );
        maximum
            .encode_at(1_000)
            .expect("maximum supported secret fits");
    }

    #[test]
    fn grant_rejects_invalid_scope_versions_rank_and_member_shape() {
        let (_, _, _, grant) = fixture_with_secret(&[11; 32]);
        let cases: &[(&str, fn(&mut PairingCheckpointGrant))] = &[
            ("grant version", |grant| grant.version = 2),
            ("anchor version", |grant| grant.anchor.version = 2),
            ("policy version", |grant| grant.anchor.policy_version = 2),
            ("rank version", |grant| grant.anchor.rank_version = 2),
            ("zero network", |grant| grant.anchor.network_id = [0; 32]),
            ("zero revision", |grant| {
                grant.minimum.authority_revision = 0
            }),
            ("revision overflow", |grant| {
                grant.minimum.authority_revision = MAX_MEMBERSHIP_RECORD_INTEGER + 1
            }),
            ("too few members", |grant| {
                grant.minimum.active_member_count = 1
            }),
            ("too many members", |grant| {
                grant.minimum.active_member_count = MAX_CHECKPOINT_MEMBERS + 1
            }),
            ("zero digest", |grant| grant.minimum.digest = [0; 32]),
            ("duplicate members", |grant| {
                grant.joiner = grant.inviter.clone()
            }),
            ("zero incarnation", |grant| {
                grant.joiner.incarnation = [0; 32]
            }),
            ("route-only role", |grant| {
                grant.joiner.roles = vec![MembershipRole::RouteAuthority]
            }),
            ("duplicated roles", |grant| {
                grant.joiner.roles.push(MembershipRole::OverlayMember)
            }),
            ("invalid peer", |grant| {
                grant.joiner.subject.peer_id = "invalid".to_owned()
            }),
            ("mismatched key", |grant| {
                grant.joiner.subject.public_key = grant.inviter.subject.public_key.clone()
            }),
            ("expired joiner", |grant| {
                grant.joiner.expires_at_unix_seconds = Some(1_000)
            }),
            ("expired inviter", |grant| {
                grant.inviter.expires_at_unix_seconds = Some(1_000)
            }),
            ("expiry overflow", |grant| {
                grant.joiner.expires_at_unix_seconds = Some(MAX_MEMBERSHIP_RECORD_INTEGER + 1)
            }),
        ];
        for (name, mutate) in cases {
            let mut malformed = grant.clone();
            mutate(&mut malformed);
            assert!(malformed.encode_at(1_000).is_err(), "{name}");
            let bytes = serde_json::to_vec(&malformed).expect("serialize invalid fixture");
            assert!(
                PairingCheckpointGrant::decode_at(&bytes, 1_000).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn grant_rejects_noncanonical_or_excessive_member_routes_and_keys() {
        let (_, _, _, grant) = fixture_with_secret(&[11; 32]);
        let mut malformed = grant.clone();
        malformed.joiner.roles.push(MembershipRole::RouteAuthority);
        malformed
            .joiner
            .route_grants
            .push(crate::config::RouteConfig {
                prefix: "10.0.0.1/24".to_owned(),
                metric: 0,
            });
        assert!(malformed.encode_at(1_000).is_err());
        malformed.joiner.route_grants[0].prefix = "10.0.0.0/24".to_owned();
        malformed.encode_at(1_000).expect("canonical route");
        malformed
            .joiner
            .route_grants
            .push(malformed.joiner.route_grants[0].clone());
        assert!(malformed.encode_at(1_000).is_err(), "duplicate routes");
        malformed
            .joiner
            .route_grants
            .resize(33, malformed.joiner.route_grants[0].clone());
        assert!(malformed.encode_at(1_000).is_err(), "route count");
        malformed = grant;
        malformed.joiner.subject.public_key = "A".repeat(4097);
        assert!(
            malformed.encode_at(1_000).is_err(),
            "key length bounded before decoding"
        );
    }

    #[test]
    fn standalone_decode_rejects_oversize_before_json_and_unknown_fields() {
        assert!(matches!(
            PairingCheckpointGrant::decode_at(
                &vec![0; MAX_PAIRING_CHECKPOINT_GRANT_BYTES + 1],
                1_000
            ),
            Err(CheckpointGrantError::TooLarge(_))
        ));
        let (_, _, _, grant) = fixture_with_secret(&[11; 32]);
        let mut json = serde_json::to_value(&grant).expect("JSON");
        json["legacy_authority"] = serde_json::json!({});
        assert!(
            PairingCheckpointGrant::decode_at(&serde_json::to_vec(&json).expect("JSON"), 1_000)
                .is_err()
        );
    }

    #[test]
    fn grant_context_and_actual_joiner_key_must_match() {
        let (inviter, joiner, _, grant) = fixture_with_secret(&[11; 32]);
        let key = STANDARD.encode(inviter.public_key_protobuf().expect("key"));
        let stranger = NodeIdentity::generate_ed25519().expect("stranger");
        assert!(
            grant
                .validate_for(&stranger.peer_id, &key, &joiner.peer_id, 1_000)
                .is_err()
        );
        assert!(
            grant
                .validate_for(
                    &inviter.peer_id,
                    &grant.joiner.subject.public_key,
                    &joiner.peer_id,
                    1_000
                )
                .is_err()
        );
        assert!(
            grant
                .validate_for(&inviter.peer_id, &key, &stranger.peer_id, 1_000)
                .is_err()
        );
        assert!(grant.validate_joiner_identity(&stranger).is_err());
        let mislabeled_identity = NodeIdentity {
            peer_id: joiner.peer_id,
            private_key: stranger.private_key,
        };
        assert!(
            grant
                .validate_joiner_identity(&mislabeled_identity)
                .is_err()
        );
    }

    #[test]
    fn debug_redacts_the_capability() {
        let (_, _, _, grant) = fixture_with_secret(&[11; 32]);
        let debug = format!("{grant:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&grant.capability_secret));
    }
}
