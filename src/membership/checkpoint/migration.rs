//! Explicit, bounded migration seeds. These are not a second membership ledger.
//! Each recipient restores gated authority and signs its own current hostname.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{
    AuthenticatedSnapshot, CheckpointError, CooperativeMembershipState, MAX_CHECKPOINT_MEMBERS,
    MAX_SIGNATURE_BYTES, MAX_SNAPSHOT_OFFER_BYTES, NetworkCapability, RetainedCheckpointState,
    SignedHostnameClaim, SnapshotPolicy, SnapshotPublisher, canonical_dns_label, encoded_bound,
    evaluate_membership_ledger_at, field_bound, membership_trust_anchors, participation, portable,
    sign, verify_signature,
};
use crate::{
    hostname::{SignedHostnameRecord, merge_hostname_records, validate_hostname_record_history},
    identity::NodeIdentity,
    membership::{MembershipRole, SignedMembershipRecord, effective_membership_at},
};

const MIGRATION_DOMAIN: &[u8] = b"p2p-vpn trusted legacy migration seed v1\n";
pub const MIGRATION_SEED_LIFETIME_SECONDS: u64 = 3_600;

pub struct LegacyMigrationOptions<'a> {
    pub network_name: &'a str,
    pub records: &'a [SignedMembershipRecord],
    pub hostname_records: &'a [SignedHostnameRecord],
    pub policy: SnapshotPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationHostname {
    pub peer_id: String,
    pub hostname: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyMigrationPayload {
    pub version: u8,
    pub network_name: String,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub snapshot: AuthenticatedSnapshot,
    pub hostnames: Vec<MigrationHostname>,
    pub publisher: SnapshotPublisher,
}

/// Public active-only seed. The network capability is supplied separately over a
/// protected management path, never derived from the public network name.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedLegacyMigrationSeed {
    pub payload: LegacyMigrationPayload,
    pub signature: String,
}

impl SignedLegacyMigrationSeed {
    /// The caller must authorize the legacy trust roots and explicit conversion.
    pub fn prepare_at(
        capability: NetworkCapability,
        publisher: &NodeIdentity,
        options: LegacyMigrationOptions<'_>,
        now: u64,
    ) -> Result<Self, CheckpointError> {
        let legacy = CooperativeMembershipState::migrate_trusted_legacy_at(
            capability.clone(),
            publisher.peer_id.clone(),
            options.records,
            options.network_name,
            now,
        )?;
        let effective = effective_membership_at(options.records, options.network_name, now)?;
        validate_legacy_roles(options.records, options.network_name, now)?;
        let state = CooperativeMembershipState::bootstrap_at(
            capability,
            publisher.peer_id.clone(),
            legacy.snapshot().payload.members.clone(),
            options.policy,
            now,
        )?;
        if state.sync_state() != super::MembershipSyncState::Participating {
            return Err(CheckpointError::NoParticipation);
        }

        validate_hostname_record_history(options.hostname_records, options.network_name)
            .map_err(|_| CheckpointError::Invalid("invalid legacy hostname history"))?;
        if options
            .hostname_records
            .iter()
            .any(|record| record.payload.issued_at_unix_seconds > now)
        {
            return Err(CheckpointError::Invalid("future legacy hostname"));
        }
        let mut latest_names = Vec::new();
        merge_hostname_records(
            &mut latest_names,
            options.hostname_records,
            options.network_name,
            MAX_CHECKPOINT_MEMBERS,
        )
        .map_err(|_| CheckpointError::Invalid("invalid legacy hostname history"))?;
        let latest: HashMap<_, _> = latest_names
            .iter()
            .map(|record| {
                (
                    record.payload.peer.as_str(),
                    record.payload.hostname.as_str(),
                )
            })
            .collect();
        let effective_names: HashMap<_, _> = effective
            .overlay_members()
            .filter_map(|member| {
                member
                    .hostnames
                    .first()
                    .map(|hostname| (member.transport_peer.to_string(), hostname.as_str()))
            })
            .collect();
        let hostnames = state
            .snapshot()
            .payload
            .members
            .iter()
            .filter_map(|member| {
                let peer = &member.subject.peer_id;
                latest
                    .get(peer.as_str())
                    .copied()
                    .or_else(|| effective_names.get(peer).copied())
                    .map(|hostname| MigrationHostname {
                        peer_id: peer.clone(),
                        hostname: hostname.to_owned(),
                    })
            })
            .collect();
        let payload = LegacyMigrationPayload {
            version: 1,
            network_name: options.network_name.to_owned(),
            issued_at_unix_seconds: now,
            expires_at_unix_seconds: now
                .checked_add(MIGRATION_SEED_LIFETIME_SECONDS)
                .ok_or(CheckpointError::Invalid("migration expiry overflow"))?,
            snapshot: state.snapshot().clone(),
            hostnames,
            publisher: SnapshotPublisher::from_identity(publisher)?,
        };
        let seed = Self {
            signature: sign(publisher, MIGRATION_DOMAIN, &payload)?,
            payload,
        };
        seed.verify_at(&state.capability, options.network_name, now)?;
        Ok(seed)
    }

    pub fn decode_at(
        bytes: &[u8],
        capability: &NetworkCapability,
        network_name: &str,
        now: u64,
    ) -> Result<Self, CheckpointError> {
        super::bytes_bound(bytes, MAX_SNAPSHOT_OFFER_BYTES)?;
        let seed: Self = serde_json::from_slice(bytes)?;
        seed.verify_at(capability, network_name, now)?;
        Ok(seed)
    }

    pub fn verify_at(
        &self,
        capability: &NetworkCapability,
        network_name: &str,
        now: u64,
    ) -> Result<(), CheckpointError> {
        field_bound(&self.payload.network_name, 255)?;
        portable(self.payload.issued_at_unix_seconds)?;
        portable(self.payload.expires_at_unix_seconds)?;
        if self.payload.version != 1
            || self.payload.network_name.is_empty()
            || self.payload.network_name != network_name
            || self.payload.issued_at_unix_seconds > now
            || self.payload.expires_at_unix_seconds <= now
            || self
                .payload
                .expires_at_unix_seconds
                .checked_sub(self.payload.issued_at_unix_seconds)
                != Some(MIGRATION_SEED_LIFETIME_SECONDS)
        {
            return Err(CheckpointError::Invalid("invalid migration scope/time"));
        }
        capability.verify(&self.payload.snapshot)?;
        let snapshot = &self.payload.snapshot.payload;
        if snapshot.authority_revision != 0 || snapshot.parent_digest.is_some() {
            return Err(CheckpointError::Invalid(
                "migration requires genesis authority",
            ));
        }
        let publisher = snapshot
            .member(&self.payload.publisher.peer_id)
            .ok_or(CheckpointError::WrongPublisher)?;
        if publisher.subject != self.payload.publisher
            || !publisher.active_at(self.payload.issued_at_unix_seconds)
        {
            return Err(CheckpointError::WrongPublisher);
        }
        if self.payload.hostnames.len() > snapshot.members.len() {
            return Err(CheckpointError::Invalid("too many migration hostnames"));
        }
        let mut previous = None;
        for name in &self.payload.hostnames {
            if previous.is_some_and(|peer: &str| peer >= name.peer_id.as_str())
                || snapshot.member(&name.peer_id).is_none()
                || canonical_dns_label(&name.hostname)
                    .map_err(crate::membership::MembershipRecordError::InvalidHostname)?
                    != name.hostname
            {
                return Err(CheckpointError::Invalid("invalid migration hostname"));
            }
            previous = Some(name.peer_id.as_str());
        }
        field_bound(&self.signature, MAX_SIGNATURE_BYTES)?;
        encoded_bound(self, MAX_SNAPSHOT_OFFER_BYTES)?;
        verify_signature(
            &self.payload.publisher,
            MIGRATION_DOMAIN,
            &self.payload,
            &self.signature,
        )
    }

    /// Installing a seed never grants immediate participation. Only this subject
    /// can sign its new name; other names arrive through ordinary resynchronization.
    pub fn instantiate_at(
        &self,
        capability: NetworkCapability,
        identity: &NodeIdentity,
        network_name: &str,
        now: u64,
    ) -> Result<CooperativeMembershipState, CheckpointError> {
        self.verify_at(&capability, network_name, now)?;
        if participation(&self.payload.snapshot.payload, &identity.peer_id, now)
            != super::MembershipSyncState::Participating
        {
            return Err(CheckpointError::NoParticipation);
        }
        let member = self
            .payload
            .snapshot
            .payload
            .member(&identity.peer_id)
            .ok_or(CheckpointError::NoParticipation)?;
        if member.subject != SnapshotPublisher::from_identity(identity)? {
            return Err(CheckpointError::NoParticipation);
        }
        let mut state = CooperativeMembershipState::restore(
            capability,
            identity.peer_id.clone(),
            RetainedCheckpointState {
                snapshot: self.payload.snapshot.clone(),
                hostname_claims: Vec::new(),
            },
        )?;
        if let Some(name) = self
            .payload
            .hostnames
            .iter()
            .find(|name| name.peer_id == identity.peer_id)
        {
            let claim = SignedHostnameClaim::issue(
                self.payload.snapshot.payload.anchor.clone(),
                identity,
                member.incarnation,
                1,
                &name.hostname,
            )?;
            state.merge_hostname_claims(&[claim])?;
        }
        Ok(state)
    }
}

fn validate_legacy_roles(
    records: &[SignedMembershipRecord],
    network_name: &str,
    now: u64,
) -> Result<(), CheckpointError> {
    let roots = membership_trust_anchors(records, network_name)?;
    let evaluation = evaluate_membership_ledger_at(records, &roots, now)?;
    if evaluation.states.values().any(|state| {
        let record = &records[state.record_index];
        !record.payload.revoked
            && !record.is_expired_at(now)
            && !record
                .payload
                .roles
                .contains(&MembershipRole::OverlayMember)
            && record
                .payload
                .roles
                .contains(&MembershipRole::RouteAuthority)
    }) {
        return Err(CheckpointError::Invalid(
            "migration cannot represent route-only membership",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
