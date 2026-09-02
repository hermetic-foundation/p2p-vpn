use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use libp2p::{PeerId as Libp2pPeerId, identity::PublicKey};
use serde::{Deserialize, Serialize};

use crate::{
    PeerId,
    dns::{DnsNameError, canonical_dns_label},
    identity::{IdentityError, NodeIdentity},
};

pub const HOSTNAME_RECORD_VERSION: u8 = 1;
pub const MAX_HOSTNAME_RECORD_INTEGER: u64 = i64::MAX as u64;
pub const MAX_HOSTNAME_RECORDS: usize = 256;
pub const MAX_HOSTNAME_RECORD_ENCODED_LEN: usize = 2 * 1024;

const SIGNING_DOMAIN: &[u8] = b"p2p-vpn hostname record v1\n";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SignedHostnameRecord {
    pub payload: HostnameRecordPayload,
    pub signature: String,
}

impl SignedHostnameRecord {
    pub fn verify(&self) -> Result<(), HostnameRecordError> {
        validate_encoded_record_len(self)?;
        validate_payload(&self.payload)?;

        let public_key = decode_public_key(&self.payload.public_key)?;
        let peer = self.payload.peer.parse::<Libp2pPeerId>()?;
        if public_key.to_peer_id() != peer {
            return Err(HostnameRecordError::PublicKeyPeerMismatch {
                expected: self.payload.peer.clone(),
                actual: public_key.to_peer_id().to_string(),
            });
        }

        let signature = STANDARD.decode(&self.signature)?;
        if !public_key.verify(&signing_message(&self.payload)?, &signature) {
            return Err(HostnameRecordError::InvalidSignature);
        }
        Ok(())
    }

    pub fn overlay_peer(&self) -> Result<PeerId, HostnameRecordError> {
        let peer = self.payload.peer.parse::<Libp2pPeerId>()?;
        Ok(PeerId::from_libp2p(peer))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostnameRecordPayload {
    pub version: u8,
    pub network_name: String,
    pub peer: String,
    pub public_key: String,
    pub sequence: u64,
    pub hostname: String,
    pub issued_at_unix_seconds: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostnameRecordMergeStats {
    pub accepted: usize,
    pub ignored_stale_or_equal: usize,
}

pub fn issue_hostname_record_at(
    identity: &NodeIdentity,
    network_name: &str,
    hostname: &str,
    sequence: u64,
    issued_at_unix_seconds: u64,
) -> Result<SignedHostnameRecord, HostnameRecordError> {
    let payload = HostnameRecordPayload {
        version: HOSTNAME_RECORD_VERSION,
        network_name: network_name.to_owned(),
        peer: identity.peer_id.clone(),
        public_key: STANDARD.encode(identity.public_key_protobuf()?),
        sequence,
        hostname: canonical_dns_label(hostname).map_err(HostnameRecordError::InvalidHostname)?,
        issued_at_unix_seconds,
    };
    validate_payload(&payload)?;
    let signature = STANDARD.encode(identity.sign(&signing_message(&payload)?)?);
    let record = SignedHostnameRecord { payload, signature };
    record.verify()?;
    Ok(record)
}

pub fn merge_hostname_records(
    records: &mut Vec<SignedHostnameRecord>,
    incoming: &[SignedHostnameRecord],
    network_name: &str,
    max_records: usize,
) -> Result<HostnameRecordMergeStats, HostnameRecordError> {
    validate_hostname_record_history(records, network_name)?;
    validate_hostname_record_history(incoming, network_name)?;

    let mut merged = canonical_hostname_records(records)?;
    let mut stats = HostnameRecordMergeStats::default();
    for incoming_record in incoming {
        if let Some(index) = merged
            .iter()
            .position(|record| record.payload.peer == incoming_record.payload.peer)
        {
            let existing = &merged[index];
            if incoming_record.payload.sequence > existing.payload.sequence {
                merged[index] = incoming_record.clone();
                stats.accepted += 1;
            } else if incoming_record.payload.sequence == existing.payload.sequence
                && incoming_record != existing
            {
                return Err(HostnameRecordError::ConflictingVersion {
                    peer: incoming_record.payload.peer.clone(),
                    sequence: incoming_record.payload.sequence,
                });
            } else {
                stats.ignored_stale_or_equal += 1;
            }
        } else {
            if merged.len() >= max_records {
                return Err(HostnameRecordError::TooManyRecords {
                    max: max_records,
                    actual: merged.len() + 1,
                });
            }
            merged.push(incoming_record.clone());
            stats.accepted += 1;
        }
    }
    merged.sort_unstable_by(|left, right| left.payload.peer.cmp(&right.payload.peer));
    *records = merged;
    Ok(stats)
}

pub fn validate_hostname_record_history(
    records: &[SignedHostnameRecord],
    network_name: &str,
) -> Result<(), HostnameRecordError> {
    if records.len() > MAX_HOSTNAME_RECORDS {
        return Err(HostnameRecordError::TooManyRecords {
            max: MAX_HOSTNAME_RECORDS,
            actual: records.len(),
        });
    }
    for record in records {
        if record.payload.network_name != network_name {
            return Err(HostnameRecordError::NetworkMismatch {
                expected: network_name.to_owned(),
                actual: record.payload.network_name.clone(),
            });
        }
        record.verify()?;
    }
    canonical_hostname_records(records)?;
    Ok(())
}

pub fn effective_hostname_records(
    records: &[SignedHostnameRecord],
    network_name: &str,
) -> Result<HashMap<PeerId, String>, HostnameRecordError> {
    validate_hostname_record_history(records, network_name)?;
    canonical_hostname_records(records)?
        .into_iter()
        .map(|record| Ok((record.overlay_peer()?, record.payload.hostname)))
        .collect()
}

fn canonical_hostname_records(
    records: &[SignedHostnameRecord],
) -> Result<Vec<SignedHostnameRecord>, HostnameRecordError> {
    let mut latest = HashMap::<String, usize>::new();
    for (index, record) in records.iter().enumerate() {
        let Some(existing_index) = latest.get(&record.payload.peer).copied() else {
            latest.insert(record.payload.peer.clone(), index);
            continue;
        };
        let existing = &records[existing_index];
        if record.payload.sequence > existing.payload.sequence {
            latest.insert(record.payload.peer.clone(), index);
        } else if record.payload.sequence == existing.payload.sequence && record != existing {
            return Err(HostnameRecordError::ConflictingVersion {
                peer: record.payload.peer.clone(),
                sequence: record.payload.sequence,
            });
        }
    }
    let mut records = latest
        .into_values()
        .map(|index| records[index].clone())
        .collect::<Vec<_>>();
    records.sort_unstable_by(|left, right| left.payload.peer.cmp(&right.payload.peer));
    Ok(records)
}

fn validate_payload(payload: &HostnameRecordPayload) -> Result<(), HostnameRecordError> {
    if payload.version != HOSTNAME_RECORD_VERSION {
        return Err(HostnameRecordError::UnsupportedVersion(payload.version));
    }
    if payload.network_name.is_empty() {
        return Err(HostnameRecordError::EmptyNetworkName);
    }
    if payload.sequence > MAX_HOSTNAME_RECORD_INTEGER
        || payload.issued_at_unix_seconds > MAX_HOSTNAME_RECORD_INTEGER
    {
        return Err(HostnameRecordError::IntegerOutOfRange);
    }
    let canonical =
        canonical_dns_label(&payload.hostname).map_err(HostnameRecordError::InvalidHostname)?;
    if canonical != payload.hostname {
        return Err(HostnameRecordError::NonCanonicalHostname);
    }
    Ok(())
}

fn signing_message(payload: &HostnameRecordPayload) -> Result<Vec<u8>, HostnameRecordError> {
    let encoded = serde_json::to_vec(payload)?;
    let mut message = Vec::with_capacity(SIGNING_DOMAIN.len() + encoded.len());
    message.extend_from_slice(SIGNING_DOMAIN);
    message.extend_from_slice(&encoded);
    Ok(message)
}

fn decode_public_key(encoded: &str) -> Result<PublicKey, HostnameRecordError> {
    let bytes = STANDARD.decode(encoded)?;
    Ok(PublicKey::try_decode_protobuf(&bytes)?)
}

fn validate_encoded_record_len(record: &SignedHostnameRecord) -> Result<(), HostnameRecordError> {
    let actual = serde_json::to_vec(record)?.len();
    if actual > MAX_HOSTNAME_RECORD_ENCODED_LEN {
        return Err(HostnameRecordError::RecordTooLarge {
            max: MAX_HOSTNAME_RECORD_ENCODED_LEN,
            actual,
        });
    }
    Ok(())
}

#[derive(Debug)]
pub enum HostnameRecordError {
    Base64(base64::DecodeError),
    Identity(IdentityError),
    Json(serde_json::Error),
    PeerId(libp2p::identity::ParseError),
    InvalidHostname(DnsNameError),
    UnsupportedVersion(u8),
    EmptyNetworkName,
    IntegerOutOfRange,
    NonCanonicalHostname,
    PublicKeyPeerMismatch { expected: String, actual: String },
    InvalidSignature,
    NetworkMismatch { expected: String, actual: String },
    ConflictingVersion { peer: String, sequence: u64 },
    TooManyRecords { max: usize, actual: usize },
    RecordTooLarge { max: usize, actual: usize },
}

impl From<base64::DecodeError> for HostnameRecordError {
    fn from(error: base64::DecodeError) -> Self {
        Self::Base64(error)
    }
}

impl From<IdentityError> for HostnameRecordError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<serde_json::Error> for HostnameRecordError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<libp2p::identity::ParseError> for HostnameRecordError {
    fn from(error: libp2p::identity::ParseError) -> Self {
        Self::PeerId(error)
    }
}

impl From<libp2p::identity::DecodingError> for HostnameRecordError {
    fn from(error: libp2p::identity::DecodingError) -> Self {
        Self::Identity(IdentityError::Libp2p(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_hostname_record_round_trips_and_rejects_tampering() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let mut record = issue_hostname_record_at(&identity, "personal", "Pixel-8-Pro", 1, 1_000)
            .expect("record");

        assert_eq!(record.payload.hostname, "pixel-8-pro");
        record.verify().expect("valid signature");
        record.payload.hostname = "other-phone".to_owned();
        assert!(matches!(
            record.verify(),
            Err(HostnameRecordError::InvalidSignature)
        ));
    }

    #[test]
    fn newer_self_claim_supersedes_older_name_without_changing_peer() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let old = issue_hostname_record_at(&identity, "personal", "old-phone", 1, 1_000)
            .expect("old record");
        let new = issue_hostname_record_at(&identity, "personal", "pixel-8-pro", 2, 2_000)
            .expect("new record");
        let mut records = vec![old];

        let stats = merge_hostname_records(&mut records, &[new], "personal", 8).expect("merge");
        let names = effective_hostname_records(&records, "personal").expect("effective names");

        assert_eq!(stats.accepted, 1);
        assert_eq!(records.len(), 1);
        assert_eq!(names.len(), 1);
        assert_eq!(
            names.values().next().map(String::as_str),
            Some("pixel-8-pro")
        );
        assert_eq!(records[0].payload.peer, identity.peer_id);
    }

    #[test]
    fn equal_sequence_equivocation_is_rejected() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let first = issue_hostname_record_at(&identity, "personal", "first", 7, 1_000)
            .expect("first record");
        let second = issue_hostname_record_at(&identity, "personal", "second", 7, 1_001)
            .expect("second record");
        let mut records = vec![first];

        assert!(matches!(
            merge_hostname_records(&mut records, &[second], "personal", 8),
            Err(HostnameRecordError::ConflictingVersion { sequence: 7, .. })
        ));
    }

    #[test]
    fn wrong_network_and_public_key_are_rejected() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let other = NodeIdentity::generate_ed25519().expect("other identity");
        let record =
            issue_hostname_record_at(&identity, "personal", "phone", 1, 1_000).expect("record");
        assert!(matches!(
            validate_hostname_record_history(std::slice::from_ref(&record), "work"),
            Err(HostnameRecordError::NetworkMismatch { .. })
        ));

        let mut mismatched = record;
        mismatched.payload.public_key =
            STANDARD.encode(other.public_key_protobuf().expect("public key"));
        assert!(matches!(
            mismatched.verify(),
            Err(HostnameRecordError::PublicKeyPeerMismatch { .. })
        ));
    }
}
