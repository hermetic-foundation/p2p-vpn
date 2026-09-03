use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use libp2p::{PeerId as Libp2pPeerId, identity::PublicKey};
use serde::{Deserialize, Serialize};

use crate::dns::{DnsNameError, canonical_dns_label};
use crate::{PeerId, config::RouteConfig, identity::NodeIdentity};

pub const MEMBERSHIP_RECORD_VERSION: u8 = 1;
pub const MAX_MEMBERSHIP_RECORD_INTEGER: u64 = i64::MAX as u64;
pub const MAX_MEMBERSHIP_RECORDS: usize = 256;
pub const MAX_MEMBERSHIP_RECORD_ENCODED_LEN: usize = 12 * 1024;

const SIGNING_DOMAIN: &[u8] = b"p2p-vpn membership record v1\n";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SignedMembershipRecord {
    pub payload: MembershipRecordPayload,
    pub signature: String,
}

impl SignedMembershipRecord {
    pub fn verify(&self) -> Result<(), MembershipRecordError> {
        self.verify_with_time(None)
    }

    pub fn verify_at(&self, now_unix_seconds: u64) -> Result<(), MembershipRecordError> {
        self.verify_with_time(Some(now_unix_seconds))
    }

    #[must_use]
    pub fn is_expired_at(&self, now_unix_seconds: u64) -> bool {
        self.payload
            .expires_at_unix_seconds
            .is_some_and(|expires_at| now_unix_seconds >= expires_at)
    }

    fn verify_with_time(&self, now_unix_seconds: Option<u64>) -> Result<(), MembershipRecordError> {
        validate_encoded_record_len(self)?;
        validate_payload(&self.payload)?;
        if let Some(now_unix_seconds) = now_unix_seconds {
            validate_payload_time(&self.payload, now_unix_seconds)?;
        }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
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
    issue_named_membership_record_for_subject_at(issuer, options, None, issued_at_unix_seconds)
}

pub fn issue_named_membership_record_for_subject_at(
    issuer: &NodeIdentity,
    options: MembershipRecordIssueOptions,
    hostname: Option<&str>,
    issued_at_unix_seconds: u64,
) -> Result<SignedMembershipRecord, MembershipRecordError> {
    let hostname = hostname
        .map(canonical_dns_label)
        .transpose()
        .map_err(MembershipRecordError::InvalidHostname)?;
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
        hostname,
        roles: options.roles,
        route_grants: options.route_grants,
        issued_at_unix_seconds,
        expires_at_unix_seconds: options.expires_at_unix_seconds,
    };
    validate_payload(&payload)?;
    validate_payload_time(&payload, issued_at_unix_seconds)?;
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
    let mut added_incoming = Vec::new();
    for record in records.iter() {
        ensure_record_network(record, network_name)?;
        record.verify()?;
        merged.push(record.clone());
    }
    validate_membership_record_versions(&merged)?;

    for record in incoming {
        ensure_record_network(record, network_name)?;
        record.verify()?;
    }

    for incoming_record in incoming {
        if merged.contains(incoming_record) {
            stats.ignored_stale_or_equal += 1;
            continue;
        }
        if let Some(existing) = merged.iter().find(|record| {
            record.payload.issuer_peer == incoming_record.payload.issuer_peer
                && record.payload.member_peer == incoming_record.payload.member_peer
                && record.payload.membership_epoch == incoming_record.payload.membership_epoch
                && record.payload.sequence == incoming_record.payload.sequence
        }) {
            if existing != incoming_record {
                return Err(conflicting_record_version(incoming_record));
            }
            stats.ignored_stale_or_equal += 1;
            continue;
        }
        if merged.len() >= max_records {
            return Err(MembershipRecordError::TooManyRecords {
                max: max_records,
                actual: merged.len() + 1,
            });
        }
        merged.push(incoming_record.clone());
        added_incoming.push(merged.len() - 1);
        stats.accepted += 1;
    }

    if merged.len() > max_records {
        return Err(MembershipRecordError::TooManyRecords {
            max: max_records,
            actual: merged.len(),
        });
    }

    let evaluation = evaluate_membership_ledger_at(&merged, trusted_issuers, now_unix_seconds)?;
    for index in added_incoming {
        let record = &merged[index];
        if !evaluation.accepted.contains(&index) {
            return Err(MembershipRecordError::UntrustedIssuer {
                issuer: record.payload.issuer_peer.clone(),
            });
        }
    }
    *records = merged;

    Ok(stats)
}

pub fn validate_membership_records_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<(), MembershipRecordError> {
    validate_membership_record_history(records, network_name)?;
    for record in records {
        validate_payload_time(&record.payload, now_unix_seconds)?;
    }

    Ok(())
}

pub fn validate_membership_record_history(
    records: &[SignedMembershipRecord],
    network_name: &str,
) -> Result<(), MembershipRecordError> {
    if records.len() > MAX_MEMBERSHIP_RECORDS {
        return Err(MembershipRecordError::TooManyRecords {
            max: MAX_MEMBERSHIP_RECORDS,
            actual: records.len(),
        });
    }
    for record in records {
        ensure_record_network(record, network_name)?;
        record.verify()?;
    }
    latest_membership_record_indices(records)?;

    Ok(())
}

pub fn trusted_membership_issuers_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<TrustedMembershipIssuers, MembershipRecordError> {
    let anchors = membership_trust_anchors(records, network_name)?;
    let evaluation = evaluate_membership_ledger_at(records, &anchors, now_unix_seconds)?;
    let issuers = anchors
        .issuers
        .into_iter()
        .filter(|peer| evaluation.states.get(peer).is_none_or(|state| state.active))
        .collect();
    Ok(TrustedMembershipIssuers { issuers })
}

pub(crate) fn membership_trust_anchors(
    records: &[SignedMembershipRecord],
    network_name: &str,
) -> Result<TrustedMembershipIssuers, MembershipRecordError> {
    validate_membership_record_history(records, network_name)?;
    let latest_records = canonical_membership_records(records)?;
    let has_explicit_root_record = latest_records
        .iter()
        .any(|record| record.payload.issuer_peer == record.payload.member_peer);
    let issuers = if has_explicit_root_record {
        latest_records
            .iter()
            .filter(|record| record.payload.issuer_peer == record.payload.member_peer)
            .map(|record| record.payload.issuer_peer.clone())
            .collect()
    } else {
        latest_records
            .iter()
            .map(|record| record.payload.issuer_peer.clone())
            .collect()
    };
    Ok(TrustedMembershipIssuers { issuers })
}

pub fn effective_membership_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<EffectiveMembership, MembershipRecordError> {
    validate_membership_record_history(records, network_name)?;
    let anchors = membership_trust_anchors(records, network_name)?;
    let evaluation = evaluate_membership_ledger_at(records, &anchors, now_unix_seconds)?;

    let mut members = HashMap::new();
    for state in evaluation.states.values() {
        if !state.active {
            continue;
        }
        let payload = &records[state.record_index].payload;
        let original_admission = evaluation
            .first_admissions
            .get(&payload.member_peer)
            .map(|index| &records[*index].payload);
        let current_admission = evaluation
            .current_admissions
            .get(&payload.member_peer)
            .map_or(payload, |index| &records[*index].payload);
        let candidate =
            EffectiveMember::try_from_payload(payload, current_admission, original_admission)?;
        members.insert(candidate.peer, candidate);
    }

    Ok(EffectiveMembership { members })
}

pub fn membership_audit_at(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now_unix_seconds: u64,
) -> Result<Vec<MembershipAuditMember>, MembershipRecordError> {
    validate_membership_record_history(records, network_name)?;
    let anchors = membership_trust_anchors(records, network_name)?;
    let evaluation = evaluate_membership_ledger_at(records, &anchors, now_unix_seconds)?;
    let mut members = evaluation
        .states
        .iter()
        .map(|(member_peer, state)| {
            let state_payload = &records[state.record_index].payload;
            let current_admission = evaluation
                .current_admissions
                .get(member_peer)
                .map(|index| &records[*index].payload);
            let original_admission = evaluation
                .first_admissions
                .get(member_peer)
                .map(|index| &records[*index].payload);
            MembershipAuditMember::try_from_payloads(
                state_payload,
                current_admission,
                original_admission,
                state.active,
                now_unix_seconds,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    members.sort_unstable_by_key(|member| member.transport_peer);
    Ok(members)
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

fn validate_membership_record_versions(
    records: &[SignedMembershipRecord],
) -> Result<(), MembershipRecordError> {
    latest_membership_record_indices(records).map(|_| ())
}

#[derive(Clone, Copy, Debug)]
struct LedgerMemberState {
    record_index: usize,
    active: bool,
}

#[derive(Debug, Default)]
struct MembershipLedgerEvaluation {
    accepted: HashSet<usize>,
    states: HashMap<String, LedgerMemberState>,
    event_authorizers: HashMap<usize, Option<usize>>,
    first_admissions: HashMap<String, usize>,
    current_admissions: HashMap<String, usize>,
}

fn evaluate_membership_ledger_at(
    records: &[SignedMembershipRecord],
    trusted_roots: &TrustedMembershipIssuers,
    now_unix_seconds: u64,
) -> Result<MembershipLedgerEvaluation, MembershipRecordError> {
    validate_membership_record_versions(records)?;
    let mut ordered = (0..records.len())
        .filter(|index| records[*index].payload.issued_at_unix_seconds <= now_unix_seconds)
        .collect::<Vec<_>>();
    ordered
        .sort_unstable_by(|left, right| membership_event_order(&records[*left], &records[*right]));

    let mut evaluation = MembershipLedgerEvaluation::default();
    let mut cursor = 0;
    while cursor < ordered.len() {
        let issued_at = records[ordered[cursor]].payload.issued_at_unix_seconds;
        expire_members_at(&mut evaluation.states, records, issued_at);
        let end = ordered[cursor..]
            .iter()
            .position(|index| records[*index].payload.issued_at_unix_seconds != issued_at)
            .map_or(ordered.len(), |offset| cursor + offset);
        let group = &ordered[cursor..end];

        let mut pending = group
            .iter()
            .copied()
            .filter(|index| !records[*index].payload.revoked)
            .collect::<Vec<_>>();
        let mut made_progress = true;
        while made_progress && !pending.is_empty() {
            made_progress = false;
            pending.retain(|index| {
                let payload = &records[*index].payload;
                if !issuer_is_active_or_genesis(payload, &evaluation.states, trusted_roots) {
                    return true;
                }
                evaluation.accepted.insert(*index);
                evaluation.event_authorizers.insert(
                    *index,
                    evaluation
                        .states
                        .get(&payload.issuer_peer)
                        .map(|state| state.record_index),
                );
                let was_active = evaluation
                    .states
                    .get(&payload.member_peer)
                    .is_some_and(|state| state.active);
                if apply_membership_event(&mut evaluation.states, records, *index)
                    && records[*index]
                        .payload
                        .roles
                        .contains(&MembershipRole::OverlayMember)
                {
                    evaluation
                        .first_admissions
                        .entry(payload.member_peer.clone())
                        .or_insert(*index);
                    if !was_active {
                        evaluation
                            .current_admissions
                            .insert(payload.member_peer.clone(), *index);
                    }
                }
                made_progress = true;
                false
            });
        }

        let authorized_revocations = group
            .iter()
            .copied()
            .filter(|index| records[*index].payload.revoked)
            .filter(|index| {
                let payload = &records[*index].payload;
                issuer_is_active(payload, &evaluation.states, trusted_roots)
                    && evaluation
                        .states
                        .get(&payload.member_peer)
                        .is_some_and(|state| state.active)
            })
            .collect::<Vec<_>>();
        for index in authorized_revocations {
            evaluation.accepted.insert(index);
            let payload = &records[index].payload;
            evaluation.event_authorizers.insert(
                index,
                evaluation
                    .states
                    .get(&payload.issuer_peer)
                    .map(|state| state.record_index),
            );
            apply_membership_event(&mut evaluation.states, records, index);
        }
        cursor = end;
    }
    expire_members_at(&mut evaluation.states, records, now_unix_seconds);
    Ok(evaluation)
}

fn membership_event_order(
    left: &SignedMembershipRecord,
    right: &SignedMembershipRecord,
) -> Ordering {
    let left_payload = &left.payload;
    let right_payload = &right.payload;
    left_payload
        .issued_at_unix_seconds
        .cmp(&right_payload.issued_at_unix_seconds)
        .then_with(|| left_payload.revoked.cmp(&right_payload.revoked))
        .then_with(|| {
            let left_delegated = left_payload.issuer_peer != left_payload.member_peer;
            let right_delegated = right_payload.issuer_peer != right_payload.member_peer;
            left_delegated.cmp(&right_delegated)
        })
        .then_with(|| {
            membership_record_version(left_payload).cmp(&membership_record_version(right_payload))
        })
        .then_with(|| left_payload.issuer_peer.cmp(&right_payload.issuer_peer))
        .then_with(|| left_payload.member_peer.cmp(&right_payload.member_peer))
        .then_with(|| left.signature.cmp(&right.signature))
}

fn issuer_is_active_or_genesis(
    payload: &MembershipRecordPayload,
    states: &HashMap<String, LedgerMemberState>,
    trusted_roots: &TrustedMembershipIssuers,
) -> bool {
    if payload.issuer_peer == payload.member_peer {
        return trusted_roots.contains(&payload.issuer_peer)
            && states
                .get(&payload.issuer_peer)
                .is_none_or(|state| state.active);
    }
    issuer_is_active(payload, states, trusted_roots)
}

fn issuer_is_active(
    payload: &MembershipRecordPayload,
    states: &HashMap<String, LedgerMemberState>,
    trusted_roots: &TrustedMembershipIssuers,
) -> bool {
    states.get(&payload.issuer_peer).map_or_else(
        || trusted_roots.contains(&payload.issuer_peer),
        |state| state.active,
    )
}

fn apply_membership_event(
    states: &mut HashMap<String, LedgerMemberState>,
    records: &[SignedMembershipRecord],
    candidate_index: usize,
) -> bool {
    let candidate = &records[candidate_index];
    let payload = &candidate.payload;
    let replace = states.get(&payload.member_peer).is_none_or(|current| {
        membership_event_wins(candidate, &records[current.record_index], current.active)
    });
    if replace {
        states.insert(
            payload.member_peer.clone(),
            LedgerMemberState {
                record_index: candidate_index,
                active: !payload.revoked && payload.roles.contains(&MembershipRole::OverlayMember),
            },
        );
    }
    replace
}

fn membership_event_wins(
    candidate: &SignedMembershipRecord,
    current: &SignedMembershipRecord,
    current_active: bool,
) -> bool {
    let candidate_version = membership_record_version(&candidate.payload);
    let current_version = membership_record_version(&current.payload);
    if !current_active && !candidate.payload.revoked {
        return candidate.payload.membership_epoch > current.payload.membership_epoch;
    }
    candidate_version > current_version
        || candidate_version == current_version
            && candidate.payload.revoked
            && !current.payload.revoked
        || candidate_version == current_version
            && candidate.payload.revoked == current.payload.revoked
            && (
                candidate.payload.issuer_peer.as_str(),
                candidate.signature.as_str(),
            ) > (
                current.payload.issuer_peer.as_str(),
                current.signature.as_str(),
            )
}

const fn membership_record_version(payload: &MembershipRecordPayload) -> (u64, u64) {
    (payload.membership_epoch, payload.sequence)
}

fn expire_members_at(
    states: &mut HashMap<String, LedgerMemberState>,
    records: &[SignedMembershipRecord],
    now_unix_seconds: u64,
) {
    for state in states.values_mut() {
        if state.active && records[state.record_index].is_expired_at(now_unix_seconds) {
            state.active = false;
        }
    }
}

pub(crate) fn overlay_membership_trust_path_at(
    records: &[SignedMembershipRecord],
    member_peer: &str,
    now_unix_seconds: u64,
) -> Result<Option<Vec<SignedMembershipRecord>>, MembershipRecordError> {
    let anchors = membership_trust_anchors(
        records,
        records
            .first()
            .map_or("", |record| record.payload.network_name.as_str()),
    )?;
    let evaluation = evaluate_membership_ledger_at(records, &anchors, now_unix_seconds)?;
    let Some(state) = evaluation
        .states
        .get(member_peer)
        .filter(|state| state.active)
    else {
        return Ok(None);
    };

    let mut path = Vec::new();
    let mut current = Some(state.record_index);
    let mut visited = HashSet::new();
    while let Some(index) = current {
        if !visited.insert(index) {
            return Err(MembershipRecordError::UntrustedIssuer {
                issuer: records[index].payload.issuer_peer.clone(),
            });
        }
        path.push(records[index].clone());
        current = evaluation.event_authorizers.get(&index).copied().flatten();
    }
    path.reverse();
    Ok(Some(path))
}

pub(crate) fn overlay_membership_proof_at(
    records: &[SignedMembershipRecord],
    member_peer: &str,
    now_unix_seconds: u64,
) -> Result<Option<Vec<SignedMembershipRecord>>, MembershipRecordError> {
    let network_name = records
        .first()
        .map_or("", |record| record.payload.network_name.as_str());
    let anchors = membership_trust_anchors(records, network_name)?;
    let evaluation = evaluate_membership_ledger_at(records, &anchors, now_unix_seconds)?;
    let Some(state) = evaluation
        .states
        .get(member_peer)
        .filter(|state| state.active)
    else {
        return Ok(None);
    };

    let mut included = HashSet::new();
    let mut pending = vec![state.record_index];
    while let Some(index) = pending.pop() {
        if !included.insert(index) {
            continue;
        }
        if let Some(authorizer) = evaluation.event_authorizers.get(&index).copied().flatten() {
            pending.push(authorizer);
        }
        let subject = &records[index].payload.member_peer;
        if let Some(current) = evaluation.states.get(subject)
            && current.record_index != index
        {
            pending.push(current.record_index);
        }
    }

    let mut indices = included.into_iter().collect::<Vec<_>>();
    indices
        .sort_unstable_by(|left, right| membership_event_order(&records[*left], &records[*right]));
    Ok(Some(
        indices
            .into_iter()
            .map(|index| records[index].clone())
            .collect(),
    ))
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
    pub effective_inviter_peer: Option<Libp2pPeerId>,
    pub original_inviter_peer: Option<Libp2pPeerId>,
    pub admitted_at_unix_seconds: u64,
    pub original_admitted_at_unix_seconds: u64,
    pub hostnames: Vec<String>,
    pub roles: Vec<MembershipRole>,
    pub route_grants: Vec<RouteConfig>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembershipState {
    Active,
    Revoked,
    Expired,
    Inactive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipAuditMember {
    pub peer: PeerId,
    pub transport_peer: Libp2pPeerId,
    pub state: MembershipState,
    pub effective_inviter_peer: Option<Libp2pPeerId>,
    pub original_inviter_peer: Option<Libp2pPeerId>,
    pub admitted_at_unix_seconds: Option<u64>,
    pub original_admitted_at_unix_seconds: Option<u64>,
    pub state_changed_at_unix_seconds: u64,
    pub hostname: Option<String>,
}

impl MembershipAuditMember {
    fn try_from_payloads(
        state_payload: &MembershipRecordPayload,
        current_admission: Option<&MembershipRecordPayload>,
        original_admission: Option<&MembershipRecordPayload>,
        active: bool,
        now_unix_seconds: u64,
    ) -> Result<Self, MembershipRecordError> {
        let transport_peer = state_payload.member_peer.parse::<Libp2pPeerId>()?;
        let (state, state_changed_at_unix_seconds) = if active {
            (
                MembershipState::Active,
                state_payload.issued_at_unix_seconds,
            )
        } else if state_payload.revoked {
            (
                MembershipState::Revoked,
                state_payload.issued_at_unix_seconds,
            )
        } else if let Some(expires_at) = state_payload.expires_at_unix_seconds
            && now_unix_seconds >= expires_at
        {
            (MembershipState::Expired, expires_at)
        } else {
            (
                MembershipState::Inactive,
                state_payload.issued_at_unix_seconds,
            )
        };
        Ok(Self {
            peer: PeerId::from_libp2p(transport_peer),
            transport_peer,
            state,
            effective_inviter_peer: current_admission.map(inviter_peer).transpose()?.flatten(),
            original_inviter_peer: original_admission.map(inviter_peer).transpose()?.flatten(),
            admitted_at_unix_seconds: current_admission
                .map(|payload| payload.issued_at_unix_seconds),
            original_admitted_at_unix_seconds: original_admission
                .map(|payload| payload.issued_at_unix_seconds),
            state_changed_at_unix_seconds,
            hostname: current_admission
                .and_then(|payload| payload.hostname.as_deref())
                .map(canonical_dns_label)
                .transpose()
                .map_err(MembershipRecordError::InvalidHostname)?,
        })
    }
}

impl EffectiveMember {
    fn try_from_payload(
        payload: &MembershipRecordPayload,
        current_admission: &MembershipRecordPayload,
        original_admission: Option<&MembershipRecordPayload>,
    ) -> Result<Self, MembershipRecordError> {
        let transport_peer = payload.member_peer.parse::<Libp2pPeerId>()?;
        let original_admission = original_admission.unwrap_or(payload);
        Ok(Self {
            peer: PeerId::from_libp2p(transport_peer),
            transport_peer,
            membership_epoch: payload.membership_epoch,
            sequence: payload.sequence,
            effective_inviter_peer: inviter_peer(current_admission)?,
            original_inviter_peer: inviter_peer(original_admission)?,
            admitted_at_unix_seconds: current_admission.issued_at_unix_seconds,
            original_admitted_at_unix_seconds: original_admission.issued_at_unix_seconds,
            hostnames: payload
                .hostname
                .as_deref()
                .map(canonical_dns_label)
                .transpose()
                .map_err(MembershipRecordError::InvalidHostname)?
                .into_iter()
                .collect(),
            roles: payload.roles.clone(),
            route_grants: payload.route_grants.clone(),
        })
    }

    #[must_use]
    pub fn has_role(&self, role: MembershipRole) -> bool {
        self.roles.contains(&role)
    }
}

fn inviter_peer(
    payload: &MembershipRecordPayload,
) -> Result<Option<Libp2pPeerId>, MembershipRecordError> {
    if payload.issuer_peer == payload.member_peer {
        return Ok(None);
    }
    Ok(Some(payload.issuer_peer.parse::<Libp2pPeerId>()?))
}

fn validate_payload(payload: &MembershipRecordPayload) -> Result<(), MembershipRecordError> {
    if payload.version != MEMBERSHIP_RECORD_VERSION {
        return Err(MembershipRecordError::UnsupportedVersion(payload.version));
    }
    if payload.network_name.is_empty() {
        return Err(MembershipRecordError::EmptyNetworkName);
    }
    if payload.membership_epoch == 0 {
        return Err(MembershipRecordError::InvalidMembershipEpoch);
    }
    validate_portable_integer("membership_epoch", payload.membership_epoch)?;
    validate_portable_integer("sequence", payload.sequence)?;
    validate_portable_integer("issued_at_unix_seconds", payload.issued_at_unix_seconds)?;
    if payload.revoked {
        if payload.hostname.is_some()
            || !payload.roles.is_empty()
            || !payload.route_grants.is_empty()
        {
            return Err(MembershipRecordError::RevocationCarriesAuthority);
        }
        if payload.expires_at_unix_seconds.is_some() {
            return Err(MembershipRecordError::RevocationExpires);
        }
    } else if payload.roles.is_empty() {
        return Err(MembershipRecordError::MissingRoles);
    }
    if let Some(hostname) = payload.hostname.as_deref() {
        canonical_dns_label(hostname).map_err(MembershipRecordError::InvalidHostname)?;
    }
    if let Some(expires_at) = payload.expires_at_unix_seconds {
        validate_portable_integer("expires_at_unix_seconds", expires_at)?;
        if expires_at <= payload.issued_at_unix_seconds {
            return Err(MembershipRecordError::ExpiredBeforeIssued);
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

fn validate_payload_time(
    payload: &MembershipRecordPayload,
    now_unix_seconds: u64,
) -> Result<(), MembershipRecordError> {
    if let Some(expires_at) = payload.expires_at_unix_seconds
        && now_unix_seconds >= expires_at
    {
        return Err(MembershipRecordError::Expired {
            expired_at: expires_at,
            now: now_unix_seconds,
        });
    }
    Ok(())
}

fn validate_portable_integer(field: &'static str, value: u64) -> Result<(), MembershipRecordError> {
    if value <= MAX_MEMBERSHIP_RECORD_INTEGER {
        return Ok(());
    }
    Err(MembershipRecordError::IntegerOutOfRange {
        field,
        value,
        max: MAX_MEMBERSHIP_RECORD_INTEGER,
    })
}

fn signing_message(payload: &MembershipRecordPayload) -> Result<Vec<u8>, MembershipRecordError> {
    let payload = serde_json::to_vec(payload)?;
    let mut message = Vec::with_capacity(SIGNING_DOMAIN.len() + payload.len());
    message.extend_from_slice(SIGNING_DOMAIN);
    message.extend_from_slice(&payload);
    Ok(message)
}

fn validate_encoded_record_len(
    record: &SignedMembershipRecord,
) -> Result<(), MembershipRecordError> {
    let actual = serde_json::to_vec(record)?.len();
    if actual <= MAX_MEMBERSHIP_RECORD_ENCODED_LEN {
        return Ok(());
    }
    Err(MembershipRecordError::EncodedRecordTooLarge {
        actual,
        max: MAX_MEMBERSHIP_RECORD_ENCODED_LEN,
    })
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
    InvalidHostname(DnsNameError),
    UnsupportedVersion(u8),
    EmptyNetworkName,
    InvalidMembershipEpoch,
    IntegerOutOfRange {
        field: &'static str,
        value: u64,
        max: u64,
    },
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
    EncodedRecordTooLarge {
        actual: usize,
        max: usize,
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
    use serde::Serialize;

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

    #[derive(Serialize)]
    struct LegacyMembershipRecordPayload {
        version: u8,
        network_name: String,
        member_peer: String,
        member_public_key: String,
        issuer_peer: String,
        issuer_public_key: String,
        membership_epoch: u64,
        sequence: u64,
        #[serde(skip_serializing_if = "is_false")]
        revoked: bool,
        roles: Vec<MembershipRole>,
        route_grants: Vec<RouteConfig>,
        issued_at_unix_seconds: u64,
        expires_at_unix_seconds: Option<u64>,
    }

    #[derive(Serialize)]
    struct LegacySignedMembershipRecord {
        payload: LegacyMembershipRecordPayload,
        signature: String,
    }

    #[test]
    fn records_signed_before_hostname_claims_remain_valid() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let payload = LegacyMembershipRecordPayload {
            version: MEMBERSHIP_RECORD_VERSION,
            network_name: "lab".to_owned(),
            member_peer: member.peer_id.clone(),
            member_public_key: STANDARD.encode(member.public_key_protobuf().expect("member key")),
            issuer_peer: issuer.peer_id.clone(),
            issuer_public_key: STANDARD.encode(issuer.public_key_protobuf().expect("issuer key")),
            membership_epoch: 1,
            sequence: 7,
            revoked: false,
            roles: vec![MembershipRole::OverlayMember],
            route_grants: Vec::new(),
            issued_at_unix_seconds: 1_000,
            expires_at_unix_seconds: None,
        };
        let encoded_payload = serde_json::to_vec(&payload).expect("legacy payload");
        let mut message = SIGNING_DOMAIN.to_vec();
        message.extend_from_slice(&encoded_payload);
        let legacy = LegacySignedMembershipRecord {
            payload,
            signature: STANDARD.encode(issuer.sign(&message).expect("signature")),
        };
        let encoded = serde_json::to_vec(&legacy).expect("legacy record");

        let decoded: SignedMembershipRecord =
            serde_json::from_slice(&encoded).expect("new decoder accepts legacy record");

        assert_eq!(decoded.payload.hostname, None);
        decoded
            .verify_at(1_001)
            .expect("legacy signature remains valid");
        assert!(
            !String::from_utf8(encoded)
                .expect("JSON")
                .contains("hostname")
        );
    }

    #[test]
    fn signed_hostname_claim_is_authenticated_and_canonicalized_for_dns() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let record = issue_named_membership_record_for_subject_at(
            &issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("subject"),
                membership_epoch: 1,
                sequence: 1,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            Some("Worker-1"),
            1_000,
        )
        .expect("named membership record");

        record.verify_at(1_001).expect("signed hostname");
        assert_eq!(record.payload.hostname.as_deref(), Some("worker-1"));
        let effective =
            effective_membership_at(&[record], "lab", 1_001).expect("effective membership");
        assert_eq!(
            effective
                .overlay_members()
                .next()
                .expect("member")
                .hostnames,
            vec!["worker-1".to_owned()]
        );
    }

    #[test]
    fn invalid_or_revoked_hostname_claims_are_rejected() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let subject = MembershipRecordSubject::from_identity(&member).expect("subject");
        let options = MembershipRecordIssueOptions {
            network_name: "lab".to_owned(),
            member: subject.clone(),
            membership_epoch: 1,
            sequence: 1,
            revoked: false,
            roles: vec![MembershipRole::OverlayMember],
            route_grants: Vec::new(),
            expires_at_unix_seconds: None,
        };
        assert!(matches!(
            issue_named_membership_record_for_subject_at(
                &issuer,
                options,
                Some("invalid name"),
                1_000,
            ),
            Err(MembershipRecordError::InvalidHostname(_))
        ));
        assert!(matches!(
            issue_named_membership_record_for_subject_at(
                &issuer,
                MembershipRecordIssueOptions {
                    network_name: "lab".to_owned(),
                    member: subject,
                    membership_epoch: 1,
                    sequence: 2,
                    revoked: true,
                    roles: Vec::new(),
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                Some("worker-1"),
                1_001,
            ),
            Err(MembershipRecordError::RevocationCarriesAuthority)
        ));
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

        record.verify().expect("signed history remains valid");
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
    fn membership_record_rejects_integers_that_native_nix_cannot_represent() {
        type PayloadMutation = fn(&mut MembershipRecordPayload);

        let (issuer, _member, record) = test_record();
        let cases: [(&str, PayloadMutation); 4] = [
            (
                "membership_epoch",
                |payload: &mut MembershipRecordPayload| {
                    payload.membership_epoch = MAX_MEMBERSHIP_RECORD_INTEGER + 1;
                },
            ),
            ("sequence", |payload: &mut MembershipRecordPayload| {
                payload.sequence = MAX_MEMBERSHIP_RECORD_INTEGER + 1;
            }),
            (
                "issued_at_unix_seconds",
                |payload: &mut MembershipRecordPayload| {
                    payload.issued_at_unix_seconds = MAX_MEMBERSHIP_RECORD_INTEGER + 1;
                },
            ),
            (
                "expires_at_unix_seconds",
                |payload: &mut MembershipRecordPayload| {
                    payload.expires_at_unix_seconds = Some(MAX_MEMBERSHIP_RECORD_INTEGER + 1);
                },
            ),
        ];
        for (field, mutate) in cases {
            let mut candidate = record.clone();
            mutate(&mut candidate.payload);
            candidate.signature = STANDARD.encode(
                issuer
                    .sign(&signing_message(&candidate.payload).expect("message"))
                    .expect("signature"),
            );

            assert!(matches!(
                candidate.verify(),
                Err(MembershipRecordError::IntegerOutOfRange {
                    field: actual,
                    value,
                    max: MAX_MEMBERSHIP_RECORD_INTEGER,
                }) if actual == field && value == MAX_MEMBERSHIP_RECORD_INTEGER + 1
            ));
        }
    }

    #[test]
    fn membership_record_rejects_an_oversized_signed_encoding() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let route = RouteConfig {
            prefix: "2001:db8::/32".to_owned(),
            metric: 10,
        };

        let error = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: vec![route; 512],
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect_err("oversized record");

        assert!(matches!(
            error,
            MembershipRecordError::EncodedRecordTooLarge {
                actual,
                max: MAX_MEMBERSHIP_RECORD_ENCODED_LEN,
            } if actual > MAX_MEMBERSHIP_RECORD_ENCODED_LEN
        ));
    }

    #[test]
    fn membership_history_enforces_the_persisted_record_limit() {
        let (_issuer, _member, record) = test_record();
        let records = vec![record; MAX_MEMBERSHIP_RECORDS + 1];

        assert!(matches!(
            validate_membership_record_history(&records, "lab"),
            Err(MembershipRecordError::TooManyRecords { max, actual })
                if max == MAX_MEMBERSHIP_RECORDS && actual == MAX_MEMBERSHIP_RECORDS + 1
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
    fn membership_record_expires_at_its_deadline() {
        let (_issuer, _member, record) = test_record();

        assert!(!record.is_expired_at(1_999));
        assert!(record.is_expired_at(2_000));
        record.verify_at(1_999).expect("record before expiry");
        assert!(matches!(
            record.verify_at(2_000),
            Err(MembershipRecordError::Expired {
                expired_at: 2_000,
                now: 2_000,
            })
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
    fn effective_membership_does_not_revive_an_older_grant_after_restart() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let older = overlay_record(&root, &member, 1, 1_000, None);
        let newer = overlay_record(&root, &member, 2, 1_010, Some(1_050));
        let mut records = vec![root_record, older.clone(), newer.clone()];

        validate_membership_record_history(&records, "lab").expect("restart-safe history");
        let effective = effective_membership_at(&records, "lab", 1_100).expect("effective");
        assert!(
            !effective
                .overlay_members()
                .any(|candidate| { candidate.transport_peer.to_string() == member.peer_id })
        );

        let trusted = trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted root");
        let stats = merge_membership_records_at(&mut records, &[older], "lab", 1_100, &trusted, 8)
            .expect("stale replay");
        assert_eq!(stats.ignored_stale_or_equal, 1);
        assert!(records.iter().any(|record| record == &newer));
    }

    #[test]
    fn expired_newer_root_does_not_reactivate_an_older_root_record() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let older = overlay_record(&root, &root, 1, 1_000, None);
        let newer = overlay_record(&root, &root, 2, 1_010, Some(1_050));
        let records = vec![older, newer];

        let trusted = trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted roots");
        let effective = effective_membership_at(&records, "lab", 1_100).expect("effective");

        assert!(trusted.is_empty());
        assert_eq!(effective.overlay_members().count(), 0);
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
    fn effective_membership_revocation_applies_to_the_member_globally() {
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
        assert_eq!(effective.overlay_members().count(), 0);
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
    fn merge_membership_records_preserves_members_admitted_by_revoked_peer() {
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
        assert_eq!(stats.removed_untrusted, 0);
        assert!(records.iter().any(|record| {
            record.payload.issuer_peer == delegate.peer_id
                && record.payload.member_peer == member.peer_id
        }));
        let effective = effective_membership_at(&records, "lab", 1_100).expect("effective");
        let peers = effective
            .overlay_members()
            .map(|candidate| candidate.transport_peer.to_string())
            .collect::<HashSet<_>>();
        assert!(!peers.contains(&delegate.peer_id));
        assert!(peers.contains(&member.peer_id));
    }

    #[test]
    fn any_active_member_can_revoke_another_member() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let member_a = NodeIdentity::generate_ed25519().expect("member a");
        let member_b = NodeIdentity::generate_ed25519().expect("member b");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let grant_a = overlay_record(&root, &member_a, 2, 1_000, None);
        let grant_b = overlay_record(&root, &member_b, 3, 1_000, None);
        let revocation = issue_membership_record_for_subject_at(
            &member_b,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member_a).expect("member subject"),
                membership_epoch: 1,
                sequence: 3,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("revocation");
        let mut records = vec![root_record, grant_a, grant_b];
        let trusted = trusted_membership_issuers_at(&records, "lab", 1_000).expect("trusted roots");

        merge_membership_records_at(&mut records, &[revocation], "lab", 1_100, &trusted, 8)
            .expect("member revocation");

        let effective = effective_membership_at(&records, "lab", 1_100).expect("effective");
        let peers = effective
            .overlay_members()
            .map(|member| member.transport_peer.to_string())
            .collect::<HashSet<_>>();
        assert!(!peers.contains(&member_a.peer_id));
        assert!(peers.contains(&member_b.peer_id));
    }

    #[test]
    fn departed_member_cannot_issue_new_membership_events() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let departed = NodeIdentity::generate_ed25519().expect("departed");
        let candidate = NodeIdentity::generate_ed25519().expect("candidate");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let grant = overlay_record(&root, &departed, 2, 1_000, None);
        let revocation = issue_membership_record_for_subject_at(
            &root,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&departed).expect("subject"),
                membership_epoch: 1,
                sequence: 3,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("revocation");
        let late_grant = overlay_record(&departed, &candidate, 1, 1_200, None);
        let mut records = vec![root_record, grant, revocation];
        let original = records.clone();
        let trusted = trusted_membership_issuers_at(&records, "lab", 1_200).expect("trusted roots");

        assert!(matches!(
            merge_membership_records_at(&mut records, &[late_grant], "lab", 1_200, &trusted, 8),
            Err(MembershipRecordError::UntrustedIssuer { issuer }) if issuer == departed.peer_id
        ));
        assert_eq!(records, original);
    }

    #[test]
    fn self_resignation_requires_a_higher_epoch_for_readmission() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let grant = overlay_record(&root, &member, 2, 1_000, None);
        let resignation = issue_membership_record_for_subject_at(
            &member,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("subject"),
                membership_epoch: 1,
                sequence: 3,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("resignation");
        let stale_readmission = overlay_record(&root, &member, 4, 1_200, None);
        let readmission = issue_membership_record_at(
            &root,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: member.clone(),
                membership_epoch: 2,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_300,
        )
        .expect("readmission");
        let records = vec![root_record, grant, resignation, stale_readmission];

        let resigned = effective_membership_at(&records, "lab", 1_200).expect("resigned");
        assert!(
            !resigned
                .overlay_members()
                .any(|candidate| candidate.transport_peer.to_string() == member.peer_id)
        );
        let readmitted =
            effective_membership_at(&[records, vec![readmission]].concat(), "lab", 1_300)
                .expect("readmitted");
        assert!(
            readmitted
                .overlay_members()
                .any(|candidate| candidate.transport_peer.to_string() == member.peer_id)
        );
    }

    #[test]
    fn effective_membership_preserves_original_and_current_inviter_provenance() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let original_inviter = NodeIdentity::generate_ed25519().expect("original inviter");
        let current_inviter = NodeIdentity::generate_ed25519().expect("current inviter");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let original_inviter_record = overlay_record(&root, &original_inviter, 2, 1_001, None);
        let current_inviter_record = overlay_record(&root, &current_inviter, 3, 1_002, None);
        let original_admission = overlay_record(&original_inviter, &member, 4, 1_010, None);
        let revocation = issue_membership_record_for_subject_at(
            &current_inviter,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("member subject"),
                membership_epoch: 1,
                sequence: 5,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_020,
        )
        .expect("revocation");
        let readmission = issue_membership_record_at(
            &current_inviter,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: member.clone(),
                membership_epoch: 2,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_030,
        )
        .expect("readmission");
        let records = vec![
            root_record,
            original_inviter_record,
            current_inviter_record,
            original_admission,
            revocation,
            readmission,
        ];

        let effective = effective_membership_at(&records, "lab", 1_030).expect("effective");
        let member = effective
            .overlay_members()
            .find(|candidate| candidate.transport_peer.to_string() == member.peer_id)
            .expect("readmitted member");

        assert_eq!(
            member.original_inviter_peer.map(|peer| peer.to_string()),
            Some(original_inviter.peer_id)
        );
        assert_eq!(
            member.effective_inviter_peer.map(|peer| peer.to_string()),
            Some(current_inviter.peer_id)
        );
        assert_eq!(member.original_admitted_at_unix_seconds, 1_010);
        assert_eq!(member.admitted_at_unix_seconds, 1_030);
    }

    #[test]
    fn merge_membership_records_preserves_network_after_creator_resigns() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let delegate_record = overlay_record(&root, &delegate, 1, 1_000, None);
        let member_record = overlay_record(&delegate, &member, 1, 1_000, None);
        let root_revocation = issue_membership_record_for_subject_at(
            &root,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&root).expect("root subject"),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("root revocation");
        let mut records = vec![root_record, delegate_record, member_record];
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_000).expect("trusted root");

        let stats = merge_membership_records_at(
            &mut records,
            &[root_revocation],
            "lab",
            1_100,
            &trusted_issuers,
            8,
        )
        .expect("root revocation merge");

        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.removed_untrusted, 0);
        assert_eq!(records.len(), 4);
        let effective = effective_membership_at(&records, "lab", 1_100).expect("effective");
        let peers = effective
            .overlay_members()
            .map(|candidate| candidate.transport_peer.to_string())
            .collect::<HashSet<_>>();
        assert!(!peers.contains(&root.peer_id));
        assert!(peers.contains(&delegate.peer_id));
        assert!(peers.contains(&member.peer_id));
    }

    #[test]
    fn merge_membership_records_rejects_delegate_self_promotion() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let delegate_record = overlay_record(&root, &delegate, 1, 1_000, None);
        let delegate_self_root = overlay_record(&delegate, &delegate, 2, 1_100, None);
        let mut records = vec![root_record, delegate_record];
        let original = records.clone();
        let trusted_issuers =
            trusted_membership_issuers_at(&records, "lab", 1_100).expect("trusted root");

        assert!(matches!(
            merge_membership_records_at(
                &mut records,
                &[delegate_self_root],
                "lab",
                1_100,
                &trusted_issuers,
                8,
            ),
            Err(MembershipRecordError::UntrustedIssuer { issuer })
                if issuer == delegate.peer_id
        ));
        assert_eq!(records, original);
    }

    #[test]
    fn merge_membership_records_retains_expired_delegate_tombstone() {
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

        assert_eq!(stats.removed_expired, 0);
        assert_eq!(stats.removed_untrusted, 0);
        assert_eq!(records.len(), 3);
        assert!(records.iter().any(|record| {
            record.payload.member_peer == delegate.peer_id && record.is_expired_at(1_100)
        }));
        let effective = effective_membership_at(&records, "lab", 1_100).expect("effective");
        let peers = effective
            .overlay_members()
            .map(|candidate| candidate.transport_peer.to_string())
            .collect::<HashSet<_>>();
        assert!(peers.contains(&root.peer_id));
        assert!(!peers.contains(&delegate.peer_id));
        assert!(peers.contains(&member.peer_id));
    }

    #[test]
    fn overlay_trust_path_preserves_a_member_after_creator_resignation() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let delegate_record = overlay_record(&root, &delegate, 1, 1_000, None);
        let root_revocation = issue_membership_record_for_subject_at(
            &root,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&root).expect("root subject"),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("root revocation");

        let path = overlay_membership_trust_path_at(
            &[root_record, delegate_record, root_revocation],
            &delegate.peer_id,
            1_100,
        )
        .expect("trust graph");

        let path = path.expect("delegate remains independently admitted");
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].payload.member_peer, root.peer_id);
        assert_eq!(path[1].payload.member_peer, delegate.peer_id);
    }

    #[test]
    fn overlay_membership_proof_carries_creator_resignation() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let delegate_record = overlay_record(&root, &delegate, 1, 1_000, None);
        let root_revocation = issue_membership_record_for_subject_at(
            &root,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&root).expect("root subject"),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("root revocation");

        let proof = overlay_membership_proof_at(
            &[root_record, delegate_record, root_revocation],
            &delegate.peer_id,
            1_100,
        )
        .expect("membership proof")
        .expect("delegate remains independently admitted");

        assert_eq!(proof.len(), 3);
        assert!(proof.iter().any(|record| {
            record.payload.member_peer == root.peer_id && record.payload.revoked
        }));
        let effective = effective_membership_at(&proof, "lab", 1_100).expect("proof membership");
        let peers = effective
            .overlay_members()
            .map(|member| member.transport_peer.to_string())
            .collect::<HashSet<_>>();
        assert!(!peers.contains(&root.peer_id));
        assert!(peers.contains(&delegate.peer_id));
    }

    #[test]
    fn overlay_trust_path_selects_the_shortest_authorized_chain() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let intermediate = NodeIdentity::generate_ed25519().expect("intermediate");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let root_record = overlay_record(&root, &root, 1, 1_000, None);
        let intermediate_record = overlay_record(&root, &intermediate, 1, 1_000, None);
        let long_member_record = overlay_record(&intermediate, &member, 1, 1_000, None);
        let direct_member_record = overlay_record(&root, &member, 2, 1_000, None);

        let path = overlay_membership_trust_path_at(
            &[
                root_record,
                intermediate_record,
                long_member_record,
                direct_member_record,
            ],
            &member.peer_id,
            1_100,
        )
        .expect("trust graph")
        .expect("member trust path");

        assert_eq!(path.len(), 2);
        assert_eq!(path[0].payload.member_peer, root.peer_id);
        assert_eq!(path[1].payload.issuer_peer, root.peer_id);
        assert_eq!(path[1].payload.member_peer, member.peer_id);
    }

    #[test]
    fn effective_membership_preserves_revoked_delegate_descendants_after_restart() {
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
        assert!(member_peers.contains(&member.peer_id));
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
    fn merge_membership_records_accepts_newer_and_retains_expired_tombstone() {
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
                removed_expired: 0,
                removed_untrusted: 0,
            }
        );
        assert_eq!(records.len(), 3);
        assert!(records.iter().any(|record| record.payload.sequence == 2));
        assert!(records.iter().any(|record| record.is_expired_at(1_100)));
    }

    #[test]
    fn merge_membership_records_accepts_an_expired_newer_tombstone_from_a_peer() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let older = overlay_record(&issuer, &member, 1, 1_000, None);
        let newer = overlay_record(&issuer, &member, 2, 1_010, Some(1_050));
        let mut records = vec![older.clone()];
        let trusted =
            trusted_membership_issuers_at(&records, "lab", 1_000).expect("trusted issuer");

        let stats = merge_membership_records_at(
            &mut records,
            std::slice::from_ref(&newer),
            "lab",
            1_100,
            &trusted,
            8,
        )
        .expect("expired tombstone merged");
        let effective = effective_membership_at(&records, "lab", 1_100).expect("effective");

        assert_eq!(stats.accepted, 1);
        assert_eq!(records, vec![older, newer]);
        assert_eq!(effective.overlay_members().count(), 0);
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
        assert_eq!(records.len(), 2);
        assert!(records[1].payload.revoked);
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
