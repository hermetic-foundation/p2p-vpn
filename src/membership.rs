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
    #[serde(default, skip_serializing_if = "is_false")]
    pub revoked: bool,
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
    pub revoked: bool,
    pub roles: Vec<MembershipRole>,
    pub route_grants: Vec<RouteConfig>,
    pub expires_at_unix_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MembershipRecordMergeStats {
    pub accepted: usize,
    pub ignored_stale_or_equal: usize,
    pub removed_expired: usize,
    pub removed_untrusted: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrustedMembershipIssuers {
    issuers: HashSet<String>,
}

impl TrustedMembershipIssuers {
    pub fn insert(&mut self, issuer_peer: impl Into<String>) {
        self.issuers.insert(issuer_peer.into());
    }

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
            revoked: false,
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
        revoked: options.revoked,
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
    let mut merged = Vec::with_capacity(records.len().saturating_add(incoming.len()));
    for record in records.iter() {
        ensure_record_network(record, network_name)?;
        match record.verify_at(now_unix_seconds) {
            Ok(()) => merged.push(record.clone()),
            Err(MembershipRecordError::Expired { .. }) => stats.removed_expired += 1,
            Err(error) => return Err(error),
        }
    }
    merged = canonical_membership_records(&merged)?;

    for record in incoming {
        ensure_record_network(record, network_name)?;
        record.verify_at(now_unix_seconds)?;
    }

    for incoming_record in incoming {
        let existing_index = merged.iter().position(|record| {
            record.payload.issuer_peer == incoming_record.payload.issuer_peer
                && record.payload.member_peer == incoming_record.payload.member_peer
        });
        if let Some(index) = existing_index {
            let existing = &merged[index];
            let incoming_version = (
                incoming_record.payload.membership_epoch,
                incoming_record.payload.sequence,
            );
            let existing_version = (existing.payload.membership_epoch, existing.payload.sequence);
            if incoming_version > existing_version {
                merged[index] = incoming_record.clone();
                stats.accepted += 1;
            } else if incoming_version == existing_version && incoming_record != existing {
                return Err(conflicting_record_version(incoming_record));
            } else {
                stats.ignored_stale_or_equal += 1;
            }
        } else {
            if merged.len() >= max_records {
                return Err(MembershipRecordError::TooManyRecords {
                    max: max_records,
                    actual: merged.len() + 1,
                });
            }
            merged.push(incoming_record.clone());
            stats.accepted += 1;
        }
    }

    if merged.len() > max_records {
        return Err(MembershipRecordError::TooManyRecords {
            max: max_records,
            actual: merged.len(),
        });
    }

    let authorized_issuers = authorized_membership_issuers(&merged, trusted_issuers);
    for record in incoming {
        if !authorized_issuers.contains(&record.payload.issuer_peer) {
            return Err(MembershipRecordError::UntrustedIssuer {
                issuer: record.payload.issuer_peer.clone(),
            });
        }
    }

    let before_trust_filter = merged.len();
    merged.retain(|record| authorized_issuers.contains(&record.payload.issuer_peer));
    stats.removed_untrusted = before_trust_filter - merged.len();
    *records = merged;

    Ok(stats)
}

pub fn validate_membership_records_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<(), MembershipRecordError> {
    for record in records {
        ensure_record_network(record, network_name)?;
        record.verify_at(now_unix_seconds)?;
    }
    latest_membership_record_indices(records)?;

    Ok(())
}

pub fn trusted_membership_issuers_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<TrustedMembershipIssuers, MembershipRecordError> {
    let mut valid_records = Vec::with_capacity(records.len());
    let mut has_explicit_root_record = false;
    for record in records {
        ensure_record_network(record, network_name)?;
        has_explicit_root_record |= record.payload.issuer_peer == record.payload.member_peer;
        if let Err(error) = record.verify_at(now_unix_seconds) {
            if matches!(error, MembershipRecordError::Expired { .. }) {
                continue;
            }
            return Err(error);
        }
        valid_records.push(record.clone());
    }
    let valid_records = canonical_membership_records(&valid_records)?;

    let mut issuers = HashSet::new();
    if has_explicit_root_record {
        issuers.extend(
            valid_records
                .iter()
                .filter(|record| record.payload.issuer_peer == record.payload.member_peer)
                .map(|record| record.payload.issuer_peer.clone()),
        );
    } else {
        // Legacy configurations treated every explicitly configured issuer as a root.
        issuers.extend(
            valid_records
                .iter()
                .map(|record| record.payload.issuer_peer.clone()),
        );
    }
    Ok(TrustedMembershipIssuers { issuers })
}

pub fn effective_membership_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<EffectiveMembership, MembershipRecordError> {
    validate_membership_records_at(records, network_name, now_unix_seconds)?;
    let latest_records = latest_membership_record_indices(records)?
        .into_iter()
        .map(|index| records[index].clone())
        .collect::<Vec<_>>();
    let trusted_issuers = trusted_membership_issuers_at(records, network_name, now_unix_seconds)?;
    let authorized_issuers = authorized_membership_issuers(&latest_records, &trusted_issuers);

    let mut members = HashMap::new();
    for record in latest_records {
        let payload = &record.payload;
        if !authorized_issuers.contains(&payload.issuer_peer) {
            continue;
        }
        if payload.revoked {
            continue;
        }
        let candidate = EffectiveMember::try_from_payload(payload)?;
        members
            .entry(candidate.peer)
            .and_modify(|member: &mut EffectiveMember| member.merge_payload(payload))
            .or_insert(candidate);
    }

    Ok(EffectiveMembership { members })
}

fn ensure_record_network(
    record: &SignedMembershipRecord,
    network_name: &str,
) -> Result<(), MembershipRecordError> {
    if record.payload.network_name == network_name {
        return Ok(());
    }
    Err(MembershipRecordError::NetworkMismatch {
        expected: network_name.to_owned(),
        actual: record.payload.network_name.clone(),
    })
}

fn latest_membership_record_indices(
    records: &[SignedMembershipRecord],
) -> Result<Vec<usize>, MembershipRecordError> {
    let mut latest = HashMap::<(String, String), usize>::new();
    for (index, record) in records.iter().enumerate() {
        let key = (
            record.payload.issuer_peer.clone(),
            record.payload.member_peer.clone(),
        );
        let Some(existing_index) = latest.get(&key).copied() else {
            latest.insert(key, index);
            continue;
        };
        let existing = &records[existing_index];
        let version = (record.payload.membership_epoch, record.payload.sequence);
        let existing_version = (existing.payload.membership_epoch, existing.payload.sequence);
        if version > existing_version {
            latest.insert(key, index);
        } else if version == existing_version && record != existing {
            return Err(conflicting_record_version(record));
        }
    }

    let mut indices = latest.into_values().collect::<Vec<_>>();
    indices.sort_unstable();
    Ok(indices)
}

fn canonical_membership_records(
    records: &[SignedMembershipRecord],
) -> Result<Vec<SignedMembershipRecord>, MembershipRecordError> {
    Ok(latest_membership_record_indices(records)?
        .into_iter()
        .map(|index| records[index].clone())
        .collect())
}

fn authorized_membership_issuers(
    records: &[SignedMembershipRecord],
    trusted_issuers: &TrustedMembershipIssuers,
) -> HashSet<String> {
    let mut authorized = trusted_issuers.issuers.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for record in records {
            let payload = &record.payload;
            if authorized.contains(&payload.issuer_peer)
                && !payload.revoked
                && payload.roles.contains(&MembershipRole::OverlayMember)
            {
                changed |= authorized.insert(payload.member_peer.clone());
            }
        }
    }
    authorized
}

fn conflicting_record_version(record: &SignedMembershipRecord) -> MembershipRecordError {
    MembershipRecordError::ConflictingRecordVersion {
        issuer: record.payload.issuer_peer.clone(),
        member: record.payload.member_peer.clone(),
        membership_epoch: record.payload.membership_epoch,
        sequence: record.payload.sequence,
    }
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

    fn merge_payload(&mut self, payload: &MembershipRecordPayload) {
        if (payload.membership_epoch, payload.sequence) > (self.membership_epoch, self.sequence) {
            self.membership_epoch = payload.membership_epoch;
            self.sequence = payload.sequence;
        }
        for role in &payload.roles {
            if !self.roles.contains(role) {
                self.roles.push(*role);
            }
        }
        for route in &payload.route_grants {
            if !self.route_grants.contains(route) {
                self.route_grants.push(route.clone());
            }
        }
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
    if payload.revoked {
        if !payload.roles.is_empty() || !payload.route_grants.is_empty() {
            return Err(MembershipRecordError::RevocationCarriesAuthority);
        }
        if payload.expires_at_unix_seconds.is_some() {
            return Err(MembershipRecordError::RevocationExpires);
        }
    } else if payload.roles.is_empty() {
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

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
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
    RevocationCarriesAuthority,
    RevocationExpires,
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
    ConflictingRecordVersion {
        issuer: String,
        member: String,
        membership_epoch: u64,
        sequence: u64,
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

    fn overlay_record(
        issuer: &NodeIdentity,
        member: &NodeIdentity,
        sequence: u64,
        issued_at_unix_seconds: u64,
        expires_at_unix_seconds: Option<u64>,
    ) -> SignedMembershipRecord {
        issue_membership_record_at(
            issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: member.clone(),
                membership_epoch: 1,
                sequence,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds,
            },
            issued_at_unix_seconds,
        )
        .expect("overlay membership record")
    }

    #[test]
    fn signed_membership_record_round_trips() {
        let (_issuer, _member, record) = test_record();

        record.verify_at(1_500).expect("verified record");
        validate_membership_records_at(&[record], "lab", 1_500).expect("validated records");
    }

    #[test]
    fn membership_record_preserves_existing_signature_without_revoked_field() {
        let (_issuer, _member, record) = test_record();
        let mut value = serde_json::to_value(&record).expect("record json");
        value["payload"]
            .as_object_mut()
            .expect("payload object")
            .remove("revoked");
        let decoded: SignedMembershipRecord =
            serde_json::from_value(value).expect("decoded legacy record");

        assert!(!decoded.payload.revoked);
        decoded.verify_at(1_500).expect("legacy record verifies");
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
    fn effective_membership_revocation_removes_latest_member_authority() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let grant = issue_membership_record_at(
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
        .expect("grant");
        let revocation = issue_membership_record_for_subject_at(
            &issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("member subject"),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("revocation");

        revocation.verify_at(1_500).expect("verified revocation");
        let effective =
            effective_membership_at(&[grant, revocation], "lab", 1_500).expect("effective");

        assert_eq!(effective.overlay_members().count(), 0);
    }

    #[test]
    fn effective_membership_revocation_is_scoped_to_issuer() {
        let issuer_a = NodeIdentity::generate_ed25519().expect("issuer a");
        let issuer_b = NodeIdentity::generate_ed25519().expect("issuer b");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let grant_a = issue_membership_record_at(
            &issuer_a,
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
        .expect("grant a");
        let revocation_a = issue_membership_record_for_subject_at(
            &issuer_a,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("member subject"),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("revocation a");
        let grant_b = issue_membership_record_at(
            &issuer_b,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
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
        .expect("grant b");

        let effective = effective_membership_at(&[grant_a, revocation_a, grant_b], "lab", 1_500)
            .expect("effective");
        let members = effective.overlay_members().collect::<Vec<_>>();

        assert_eq!(members.len(), 1);
        assert!(members[0].has_role(MembershipRole::OverlayMember));
        assert!(members[0].has_role(MembershipRole::RouteAuthority));
        assert_eq!(
            members[0].route_grants,
            vec![RouteConfig {
                prefix: "10.42.0.0/24".to_owned(),
                metric: 10,
            }]
        );
    }

    #[test]
    fn merge_membership_records_keeps_distinct_issuer_records_for_same_member() {
        let issuer_a = NodeIdentity::generate_ed25519().expect("issuer a");
        let issuer_b = NodeIdentity::generate_ed25519().expect("issuer b");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let trusted_root = overlay_record(&issuer_a, &issuer_a, 1, 1_000, None);
        let grant_a = issue_membership_record_at(
            &issuer_a,
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
        .expect("grant a");
        let additional_root = issue_membership_record_at(
            &issuer_b,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: issuer_b.clone(),
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("issuer b root");
        let grant_b = issue_membership_record_at(
            &issuer_b,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("grant b");
        let mut records = vec![trusted_root, grant_a, additional_root];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted issuers");

        let stats = merge_membership_records_at(
            &mut records,
            std::slice::from_ref(&grant_b),
            "lab",
            1_100,
            &trusted_issuers,
            8,
        )
        .expect("merge");

        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.ignored_stale_or_equal, 0);
        assert_eq!(
            records
                .iter()
                .filter(|record| record.payload.member_peer == grant_b.payload.member_peer)
                .count(),
            2
        );
    }

    #[test]
    fn merge_membership_records_accepts_delegated_overlay_member_issuer() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let delegate_record = overlay_record(&root, &delegate, 2, 1_000, None);
        let member_record = overlay_record(&delegate, &member, 1, 1_100, None);
        let mut records = vec![root_record, delegate_record];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted roots");

        let stats = merge_membership_records_at(
            &mut records,
            &[member_record],
            "lab",
            1_100,
            &trusted_issuers,
            8,
        )
        .expect("delegated merge");

        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.removed_untrusted, 0);
        let effective = effective_membership_at(&records, "lab", 1_100).expect("effective");
        assert!(
            effective
                .overlay_members()
                .any(|candidate| candidate.transport_peer.to_string() == member.peer_id)
        );
    }

    #[test]
    fn merge_membership_records_resolves_delegation_chain_independent_of_batch_order() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let delegate_record = overlay_record(&root, &delegate, 2, 1_000, None);
        let member_record = overlay_record(&delegate, &member, 1, 1_100, None);
        let mut records = vec![root_record];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted roots");

        let stats = merge_membership_records_at(
            &mut records,
            &[member_record, delegate_record],
            "lab",
            1_100,
            &trusted_issuers,
            8,
        )
        .expect("delegated batch merge");

        assert_eq!(stats.accepted, 2);
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn merge_membership_records_cascades_delegate_revocation() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let delegate_record = overlay_record(&root, &delegate, 1, 1_000, None);
        let member_record = overlay_record(&delegate, &member, 1, 1_000, None);
        let revocation = issue_membership_record_for_subject_at(
            &root,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&delegate)
                    .expect("delegate subject"),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("delegate revocation");
        let mut records = vec![root_record, delegate_record, member_record];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_000).expect("trusted roots");

        let stats = merge_membership_records_at(
            &mut records,
            &[revocation],
            "lab",
            1_100,
            &trusted_issuers,
            8,
        )
        .expect("revocation merge");

        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.removed_untrusted, 1);
        assert!(!records.iter().any(|record| {
            record.payload.issuer_peer == delegate.peer_id
                && record.payload.member_peer == member.peer_id
        }));
    }

    #[test]
    fn merge_membership_records_cascades_delegate_expiry() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let delegate_record = overlay_record(&root, &delegate, 1, 1_000, Some(1_050));
        let member_record = overlay_record(&delegate, &member, 1, 1_000, None);
        let mut records = vec![root_record, delegate_record, member_record];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_000).expect("trusted roots");

        let stats =
            merge_membership_records_at(&mut records, &[], "lab", 1_100, &trusted_issuers, 8)
                .expect("expiry merge");

        assert_eq!(stats.removed_expired, 1);
        assert_eq!(stats.removed_untrusted, 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].payload.member_peer, root.peer_id);
    }

    #[test]
    fn effective_membership_filters_revoked_delegate_descendants_after_restart() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let delegate_record = overlay_record(&root, &delegate, 1, 1_000, None);
        let member_record = overlay_record(&delegate, &member, 1, 1_000, None);
        let revocation = issue_membership_record_for_subject_at(
            &root,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&delegate)
                    .expect("delegate subject"),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("delegate revocation");

        let effective = effective_membership_at(
            &[root_record, delegate_record, member_record, revocation],
            "lab",
            1_100,
        )
        .expect("effective membership");

        let member_peers = effective
            .overlay_members()
            .map(|candidate| candidate.transport_peer.to_string())
            .collect::<HashSet<_>>();
        assert!(member_peers.contains(&root.peer_id));
        assert!(!member_peers.contains(&delegate.peer_id));
        assert!(!member_peers.contains(&member.peer_id));
    }

    #[test]
    fn merge_membership_records_rejects_equal_version_equivocation() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let grant = overlay_record(&root, &member, 2, 1_000, None);
        let conflicting_grant = issue_membership_record_at(
            &root,
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
            1_000,
        )
        .expect("conflicting grant");
        let mut records = vec![root_record, grant];
        let original = records.clone();
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted roots");

        assert!(matches!(
            merge_membership_records_at(
                &mut records,
                &[conflicting_grant],
                "lab",
                1_100,
                &trusted_issuers,
                8,
            ),
            Err(MembershipRecordError::ConflictingRecordVersion { .. })
        ));
        assert_eq!(records, original);
    }

    #[test]
    fn merge_membership_records_rejects_unanchored_delegation_cycle() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let member_a = NodeIdentity::generate_ed25519().expect("member a");
        let member_b = NodeIdentity::generate_ed25519().expect("member b");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let a_to_b = overlay_record(&member_a, &member_b, 1, 1_000, None);
        let b_to_a = overlay_record(&member_b, &member_a, 1, 1_000, None);
        let mut records = vec![root_record];
        let original = records.clone();
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted roots");

        assert!(matches!(
            merge_membership_records_at(
                &mut records,
                &[a_to_b, b_to_a],
                "lab",
                1_100,
                &trusted_issuers,
                8,
            ),
            Err(MembershipRecordError::UntrustedIssuer { .. })
        ));
        assert_eq!(records, original);
    }

    #[test]
    fn revocation_records_cannot_carry_authority_or_expiry() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let subject = MembershipRecordSubject::from_identity(&member).expect("member subject");

        let with_role = issue_membership_record_for_subject_at(
            &issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: subject.clone(),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        );
        let with_expiry = issue_membership_record_for_subject_at(
            &issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: subject,
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: Some(2_000),
            },
            1_100,
        );

        assert!(matches!(
            with_role,
            Err(MembershipRecordError::RevocationCarriesAuthority)
        ));
        assert!(matches!(
            with_expiry,
            Err(MembershipRecordError::RevocationExpires)
        ));
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
                removed_untrusted: 0,
            }
        );
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].payload.sequence, 2);
    }

    #[test]
    fn merge_membership_records_accepts_newer_revocation_and_ignores_stale_grant() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let grant = issue_membership_record_at(
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
        .expect("grant");
        let stale_grant = grant.clone();
        let revocation = issue_membership_record_for_subject_at(
            &issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("member subject"),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("revocation");
        let mut records = vec![grant];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_000).expect("trusted issuers");

        let stats = merge_membership_records_at(
            &mut records,
            &[revocation, stale_grant],
            "lab",
            1_500,
            &trusted_issuers,
            8,
        )
        .expect("merge");

        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.ignored_stale_or_equal, 1);
        assert_eq!(records.len(), 1);
        assert!(records[0].payload.revoked);
        let effective = effective_membership_at(&records, "lab", 1_500).expect("effective");
        assert_eq!(effective.overlay_members().count(), 0);
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
