use std::collections::{HashMap, HashSet};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use libp2p::{PeerId as Libp2pPeerId, identity::PublicKey};
use serde::{Deserialize, Serialize};

use crate::{PeerId, config::RouteConfig, identity::NodeIdentity};

pub const MEMBERSHIP_RECORD_VERSION: u8 = 1;

const SIGNING_DOMAIN: &[u8] = b"p2p-vpn membership record v1\n";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SignedMembershipRecord {
    pub payload: MembershipRecordPayload,
    pub signature: String,
}

impl SignedMembershipRecord {
    pub fn verify_at(&self, now_unix_seconds: u64) -> Result<(), MembershipRecordError> {
        validate_payload(&self.payload, now_unix_seconds)?;
        let issuer_public_key = decode_public_key(&self.payload.issuer_public_key)?;
        let issuer_peer = self.payload.issuer_peer.parse::<Libp2pPeerId>()?;
        if issuer_public_key.to_peer_id() != issuer_peer {
            return Err(MembershipRecordError::PublicKeyPeerMismatch {
                field: "issuer_public_key",
                expected: self.payload.issuer_peer.clone(),
                actual: issuer_public_key.to_peer_id().to_string(),
            });
        }
        let member_public_key = decode_public_key(&self.payload.member_public_key)?;
        let member_peer = self.payload.member_peer.parse::<Libp2pPeerId>()?;
        if member_public_key.to_peer_id() != member_peer {
            return Err(MembershipRecordError::PublicKeyPeerMismatch {
                field: "member_public_key",
                expected: self.payload.member_peer.clone(),
                actual: member_public_key.to_peer_id().to_string(),
            });
        }

        let signature = STANDARD.decode(&self.signature)?;
        if !issuer_public_key.verify(&signing_message(&self.payload)?, &signature) {
            return Err(MembershipRecordError::InvalidSignature);
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MembershipRecordPayload {
    pub version: u8,
    pub network_name: String,
    pub member_peer: String,
    pub member_public_key: String,
    pub issuer_peer: String,
    pub issuer_public_key: String,
    #[serde(default = "default_membership_epoch")]
    pub membership_epoch: u64,
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub roles: Vec<MembershipRole>,
    #[serde(default)]
    pub route_grants: Vec<RouteConfig>,
    pub issued_at_unix_seconds: u64,
    #[serde(default)]
    pub expires_at_unix_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipRole {
    OverlayMember,
    RouteAuthority,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipRecordOptions {
    pub network_name: String,
    pub member: NodeIdentity,
    pub membership_epoch: u64,
    pub sequence: u64,
    pub roles: Vec<MembershipRole>,
    pub route_grants: Vec<RouteConfig>,
    pub expires_at_unix_seconds: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MembershipRecordSubject {
    pub peer_id: String,
    pub public_key: String,
}

impl MembershipRecordSubject {
    pub fn from_identity(identity: &NodeIdentity) -> Result<Self, MembershipRecordError> {
        Ok(Self {
            peer_id: identity.peer_id.clone(),
            public_key: STANDARD.encode(identity.public_key_protobuf()?),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipRecordIssueOptions {
    pub network_name: String,
    pub member: MembershipRecordSubject,
    pub membership_epoch: u64,
    pub sequence: u64,
    pub roles: Vec<MembershipRole>,
    pub route_grants: Vec<RouteConfig>,
    pub expires_at_unix_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MembershipRecordMergeStats {
    pub accepted: usize,
    pub ignored_stale_or_equal: usize,
    pub removed_expired: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrustedMembershipIssuers {
    issuers: HashSet<String>,
}

impl TrustedMembershipIssuers {
    #[must_use]
    pub fn contains(&self, issuer_peer: &str) -> bool {
        self.issuers.contains(issuer_peer)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.issuers.is_empty()
    }
}

pub fn issue_membership_record_at(
    issuer: &NodeIdentity,
    options: MembershipRecordOptions,
    issued_at_unix_seconds: u64,
) -> Result<SignedMembershipRecord, MembershipRecordError> {
    let member = MembershipRecordSubject::from_identity(&options.member)?;
    issue_membership_record_for_subject_at(
        issuer,
        MembershipRecordIssueOptions {
            network_name: options.network_name,
            member,
            membership_epoch: options.membership_epoch,
            sequence: options.sequence,
            roles: options.roles,
            route_grants: options.route_grants,
            expires_at_unix_seconds: options.expires_at_unix_seconds,
        },
        issued_at_unix_seconds,
    )
}

pub fn issue_membership_record_for_subject_at(
    issuer: &NodeIdentity,
    options: MembershipRecordIssueOptions,
    issued_at_unix_seconds: u64,
) -> Result<SignedMembershipRecord, MembershipRecordError> {
    if let Some(expires_at) = options.expires_at_unix_seconds
        && expires_at <= issued_at_unix_seconds
    {
        return Err(MembershipRecordError::ExpiredBeforeIssued);
    }
    let payload = MembershipRecordPayload {
        version: MEMBERSHIP_RECORD_VERSION,
        network_name: options.network_name,
        member_peer: options.member.peer_id,
        member_public_key: options.member.public_key,
        issuer_peer: issuer.peer_id.clone(),
        issuer_public_key: STANDARD.encode(issuer.public_key_protobuf()?),
        membership_epoch: options.membership_epoch.max(1),
        sequence: options.sequence,
        roles: options.roles,
        route_grants: options.route_grants,
        issued_at_unix_seconds,
        expires_at_unix_seconds: options.expires_at_unix_seconds,
    };
    validate_payload(&payload, issued_at_unix_seconds)?;
    let signature = STANDARD.encode(issuer.sign(&signing_message(&payload)?)?);
    let record = SignedMembershipRecord { payload, signature };
    record.verify_at(issued_at_unix_seconds)?;
    Ok(record)
}

pub fn merge_membership_records_at(
    records: &mut Vec<SignedMembershipRecord>,
    incoming: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
    trusted_issuers: &TrustedMembershipIssuers,
    max_records: usize,
) -> Result<MembershipRecordMergeStats, MembershipRecordError> {
    let mut stats = MembershipRecordMergeStats::default();
    for record in incoming {
        record.verify_at(now_unix_seconds)?;
        if record.payload.network_name != network_name {
            return Err(MembershipRecordError::NetworkMismatch {
                expected: network_name.to_owned(),
                actual: record.payload.network_name.clone(),
            });
        }
        if !trusted_issuers.contains(&record.payload.issuer_peer) {
            return Err(MembershipRecordError::UntrustedIssuer {
                issuer: record.payload.issuer_peer.clone(),
            });
        }
    }

    let before_retain = records.len();
    records.retain(|record| {
        record.verify_at(now_unix_seconds).is_ok()
            && record.payload.network_name == network_name
            && trusted_issuers.contains(&record.payload.issuer_peer)
    });
    stats.removed_expired = before_retain - records.len();

    for incoming_record in incoming {
        let existing_index = records
            .iter()
            .position(|record| record.payload.member_peer == incoming_record.payload.member_peer);
        if let Some(index) = existing_index {
            let existing = &records[index];
            if (
                incoming_record.payload.membership_epoch,
                incoming_record.payload.sequence,
            ) > (existing.payload.membership_epoch, existing.payload.sequence)
            {
                records[index] = incoming_record.clone();
                stats.accepted += 1;
            } else {
                stats.ignored_stale_or_equal += 1;
            }
        } else {
            if records.len() >= max_records {
                return Err(MembershipRecordError::TooManyRecords {
                    max: max_records,
                    actual: records.len() + 1,
                });
            }
            records.push(incoming_record.clone());
            stats.accepted += 1;
        }
    }

    Ok(stats)
}

pub fn validate_membership_records_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<(), MembershipRecordError> {
    for record in records {
        record.verify_at(now_unix_seconds)?;
        if record.payload.network_name != network_name {
            return Err(MembershipRecordError::NetworkMismatch {
                expected: network_name.to_owned(),
                actual: record.payload.network_name.clone(),
            });
        }
    }

    Ok(())
}

pub fn trusted_membership_issuers_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<TrustedMembershipIssuers, MembershipRecordError> {
    let mut issuers = HashSet::new();
    for record in records {
        if let Err(error) = record.verify_at(now_unix_seconds) {
            if matches!(error, MembershipRecordError::Expired { .. })
                && record.payload.network_name == network_name
            {
                continue;
            }
            return Err(error);
        }
        if record.payload.network_name != network_name {
            return Err(MembershipRecordError::NetworkMismatch {
                expected: network_name.to_owned(),
                actual: record.payload.network_name.clone(),
            });
        }
        issuers.insert(record.payload.issuer_peer.clone());
    }
    Ok(TrustedMembershipIssuers { issuers })
}

pub fn effective_membership_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<EffectiveMembership, MembershipRecordError> {
    let mut members = HashMap::new();

    for record in records {
        record.verify_at(now_unix_seconds)?;
        if record.payload.network_name != network_name {
            return Err(MembershipRecordError::NetworkMismatch {
                expected: network_name.to_owned(),
                actual: record.payload.network_name.clone(),
            });
        }

        let candidate = EffectiveMember::try_from_payload(&record.payload)?;
        let replace = members
            .get(&candidate.peer)
            .is_none_or(|existing: &EffectiveMember| {
                (candidate.membership_epoch, candidate.sequence)
                    > (existing.membership_epoch, existing.sequence)
            });
        if replace {
            members.insert(candidate.peer, candidate);
        }
    }

    Ok(EffectiveMembership { members })
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectiveMembership {
    members: HashMap<PeerId, EffectiveMember>,
}

impl EffectiveMembership {
    pub fn overlay_members(&self) -> impl Iterator<Item = &EffectiveMember> {
        self.members
            .values()
            .filter(|member| member.has_role(MembershipRole::OverlayMember))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveMember {
    pub peer: PeerId,
    pub transport_peer: Libp2pPeerId,
    pub membership_epoch: u64,
    pub sequence: u64,
    pub roles: Vec<MembershipRole>,
    pub route_grants: Vec<RouteConfig>,
}

impl EffectiveMember {
    fn try_from_payload(payload: &MembershipRecordPayload) -> Result<Self, MembershipRecordError> {
        let transport_peer = payload.member_peer.parse::<Libp2pPeerId>()?;
        Ok(Self {
            peer: PeerId::from_libp2p(transport_peer),
            transport_peer,
            membership_epoch: payload.membership_epoch,
            sequence: payload.sequence,
            roles: payload.roles.clone(),
            route_grants: payload.route_grants.clone(),
        })
    }

    #[must_use]
    pub fn has_role(&self, role: MembershipRole) -> bool {
        self.roles.contains(&role)
    }
}

fn validate_payload(
    payload: &MembershipRecordPayload,
    now_unix_seconds: u64,
) -> Result<(), MembershipRecordError> {
    if payload.version != MEMBERSHIP_RECORD_VERSION {
        return Err(MembershipRecordError::UnsupportedVersion(payload.version));
    }
    if payload.network_name.is_empty() {
        return Err(MembershipRecordError::EmptyNetworkName);
    }
    if payload.membership_epoch == 0 {
        return Err(MembershipRecordError::InvalidMembershipEpoch);
    }
    if payload.roles.is_empty() {
        return Err(MembershipRecordError::MissingRoles);
    }
    if let Some(expires_at) = payload.expires_at_unix_seconds {
        if expires_at <= payload.issued_at_unix_seconds {
            return Err(MembershipRecordError::ExpiredBeforeIssued);
        }
        if now_unix_seconds > expires_at {
            return Err(MembershipRecordError::Expired {
                expired_at: expires_at,
                now: now_unix_seconds,
            });
        }
    }
    for route in &payload.route_grants {
        route.prefix().map_err(|error| match error {
            crate::config::ConfigError::RoutePrefix(error) => {
                MembershipRecordError::RoutePrefix(error)
            }
            _ => unreachable!("RouteConfig::prefix only returns route prefix errors"),
        })?;
    }

    Ok(())
}

fn signing_message(payload: &MembershipRecordPayload) -> Result<Vec<u8>, MembershipRecordError> {
    let payload = serde_json::to_vec(payload)?;
    let mut message = Vec::with_capacity(SIGNING_DOMAIN.len() + payload.len());
    message.extend_from_slice(SIGNING_DOMAIN);
    message.extend_from_slice(&payload);
    Ok(message)
}

fn decode_public_key(encoded: &str) -> Result<PublicKey, MembershipRecordError> {
    let bytes = STANDARD.decode(encoded)?;
    Ok(PublicKey::try_decode_protobuf(&bytes)?)
}

const fn default_membership_epoch() -> u64 {
    1
}

#[derive(Debug)]
pub enum MembershipRecordError {
    Identity(crate::identity::IdentityError),
    Json(serde_json::Error),
    Base64(base64::DecodeError),
    Libp2pIdentity(libp2p::identity::DecodingError),
    Libp2pPeerId(libp2p::identity::ParseError),
    RoutePrefix(crate::config::RoutePrefixError),
    UnsupportedVersion(u8),
    EmptyNetworkName,
    InvalidMembershipEpoch,
    MissingRoles,
    NetworkMismatch {
        expected: String,
        actual: String,
    },
    PublicKeyPeerMismatch {
        field: &'static str,
        expected: String,
        actual: String,
    },
    InvalidSignature,
    UntrustedIssuer {
        issuer: String,
    },
    TooManyRecords {
        max: usize,
        actual: usize,
    },
    Expired {
        expired_at: u64,
        now: u64,
    },
    ExpiredBeforeIssued,
}

impl From<crate::identity::IdentityError> for MembershipRecordError {
    fn from(error: crate::identity::IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<serde_json::Error> for MembershipRecordError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<base64::DecodeError> for MembershipRecordError {
    fn from(error: base64::DecodeError) -> Self {
        Self::Base64(error)
    }
}

impl From<libp2p::identity::DecodingError> for MembershipRecordError {
    fn from(error: libp2p::identity::DecodingError) -> Self {
        Self::Libp2pIdentity(error)
    }
}

impl From<libp2p::identity::ParseError> for MembershipRecordError {
    fn from(error: libp2p::identity::ParseError) -> Self {
        Self::Libp2pPeerId(error)
    }
}

impl From<crate::config::RoutePrefixError> for MembershipRecordError {
    fn from(error: crate::config::RoutePrefixError) -> Self {
        Self::RoutePrefix(error)
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::*;

    fn test_record() -> (NodeIdentity, NodeIdentity, SignedMembershipRecord) {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let record = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: member.clone(),
                membership_epoch: 7,
                sequence: 42,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 10,
                }],
                expires_at_unix_seconds: Some(2_000),
            },
            1_000,
        )
        .expect("record");

        (issuer, member, record)
    }

    #[test]
    fn signed_membership_record_round_trips() {
        let (_issuer, _member, record) = test_record();

        record.verify_at(1_500).expect("verified record");
        validate_membership_records_at(&[record], "lab", 1_500).expect("validated records");
    }

    #[test]
    fn membership_record_rejects_tampered_payload() {
        let (_issuer, _member, mut record) = test_record();
        record.payload.sequence += 1;

        assert!(matches!(
            record.verify_at(1_500),
            Err(MembershipRecordError::InvalidSignature)
        ));
    }

    #[test]
    fn membership_record_rejects_wrong_member_public_key_binding() {
        let (issuer, _member, mut record) = test_record();
        let other = NodeIdentity::generate_ed25519().expect("other");
        record.payload.member_public_key =
            STANDARD.encode(other.public_key_protobuf().expect("other public key"));
        record.signature = STANDARD.encode(
            issuer
                .sign(&signing_message(&record.payload).expect("message"))
                .expect("signature"),
        );

        assert!(matches!(
            record.verify_at(1_500),
            Err(MembershipRecordError::PublicKeyPeerMismatch {
                field: "member_public_key",
                ..
            })
        ));
    }

    #[test]
    fn membership_record_rejects_expired_or_wrong_network_records() {
        let (_issuer, _member, record) = test_record();

        assert!(matches!(
            record.verify_at(2_001),
            Err(MembershipRecordError::Expired {
                expired_at: 2_000,
                now: 2_001
            })
        ));
        assert!(matches!(
            validate_membership_records_at(&[record], "other", 1_500),
            Err(MembershipRecordError::NetworkMismatch { .. })
        ));
    }

    #[test]
    fn membership_record_rejects_invalid_route_grants() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let record = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::RouteAuthority],
                route_grants: vec![RouteConfig {
                    prefix: "not-a-prefix".to_owned(),
                    metric: 0,
                }],
                expires_at_unix_seconds: None,
            },
            1_000,
        );

        assert!(matches!(record, Err(MembershipRecordError::RoutePrefix(_))));
    }

    #[test]
    fn membership_record_rejects_expiry_before_issue_time() {
        let (_issuer, _member, mut record) = test_record();
        record.payload.issued_at_unix_seconds = 2_000;
        record.payload.expires_at_unix_seconds = Some(1_999);

        assert!(matches!(
            record.verify_at(1_500),
            Err(MembershipRecordError::ExpiredBeforeIssued)
        ));
    }

    #[test]
    fn effective_membership_uses_latest_epoch_sequence() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let older = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: member.clone(),
                membership_epoch: 1,
                sequence: 1,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 10,
                }],
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("older");
        let newer = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 2,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("newer");

        let effective = effective_membership_at(&[older, newer], "lab", 1_500).expect("effective");
        let members = effective.overlay_members().collect::<Vec<_>>();

        assert_eq!(members.len(), 1);
        assert_eq!(members[0].sequence, 2);
        assert!(!members[0].has_role(MembershipRole::RouteAuthority));
        assert!(members[0].route_grants.is_empty());
    }

    #[test]
    fn merge_membership_records_accepts_newer_and_discards_stale_or_expired() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let old = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: member.clone(),
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("old");
        let stale = old.clone();
        let newer = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 2,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("newer");
        let expired_member = NodeIdentity::generate_ed25519().expect("expired member");
        let expired = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: expired_member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: Some(1_050),
            },
            1_000,
        )
        .expect("expired");
        let mut records = vec![old, expired];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_000).expect("trusted issuers");

        let stats = merge_membership_records_at(
            &mut records,
            &[stale, newer],
            "lab",
            1_100,
            &trusted_issuers,
            8,
        )
        .expect("merge");

        assert_eq!(
            stats,
            MembershipRecordMergeStats {
                accepted: 1,
                ignored_stale_or_equal: 1,
                removed_expired: 1,
            }
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].payload.sequence, 2);
    }

    #[test]
    fn merge_membership_records_rejects_untrusted_issuers() {
        let trusted_issuer = NodeIdentity::generate_ed25519().expect("trusted issuer");
        let untrusted_issuer = NodeIdentity::generate_ed25519().expect("untrusted issuer");
        let trusted_member = NodeIdentity::generate_ed25519().expect("trusted member");
        let new_member = NodeIdentity::generate_ed25519().expect("new member");
        let trusted_record = issue_membership_record_at(
            &trusted_issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: trusted_member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("trusted record");
        let incoming = issue_membership_record_at(
            &untrusted_issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: new_member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("incoming");
        let mut records = vec![trusted_record];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted issuers");

        assert!(matches!(
            merge_membership_records_at(
                &mut records,
                &[incoming],
                "lab",
                1_100,
                &trusted_issuers,
                8,
            ),
            Err(MembershipRecordError::UntrustedIssuer { .. })
        ));
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn merge_membership_records_enforces_total_record_cap() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let first_member = NodeIdentity::generate_ed25519().expect("first member");
        let second_member = NodeIdentity::generate_ed25519().expect("second member");
        let first = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: first_member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("first");
        let second = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: second_member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("second");
        let mut records = vec![first];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted issuers");

        assert!(matches!(
            merge_membership_records_at(&mut records, &[second], "lab", 1_100, &trusted_issuers, 1),
            Err(MembershipRecordError::TooManyRecords { max: 1, actual: 2 })
        ));
        assert_eq!(records.len(), 1);
    }
}
