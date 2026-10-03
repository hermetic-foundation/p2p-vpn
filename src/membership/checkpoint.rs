//! Cooperative, active-only membership snapshots. Not yet integrated into storage/runtime.
//!
//! Any participating survivor can publish a genuine roster/policy change. A bounded
//! resync window selects `(authority_revision, member_count, canonical_digest)`;
//! losing-branch revocations may be discarded. No quorum, global-freshness proof,
//! permanent revocation, or Byzantine fork protection is provided. Former holders
//! of the network capability can manufacture competing branches and their scores.
//! An isolated survivor can resume without remote offers; this is availability,
//! not evidence that its snapshot is globally current. Hostnames do not affect rank.
//!
//! Provisioning a shared secret and pinning the same anchor are caller responsibilities.
//! These authority objects require a new storage/protocol version, not extra fields
//! in the legacy ledger envelope. Publisher identities and offers are not durable.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use libp2p::{PeerId as TransportPeer, identity::PublicKey};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2_010::{Digest as _, Sha256};

use super::{
    EffectiveMember, EffectiveMembership, MAX_MEMBERSHIP_RECORD_INTEGER, MembershipRecordError,
    MembershipRole, SignedMembershipRecord, decode_public_key, evaluate_membership_ledger_at,
    membership_trust_anchors, validate_membership_record_history,
};
use crate::{PeerId, config::RouteConfig, dns::canonical_dns_label, identity::NodeIdentity};

pub const COOPERATIVE_CHECKPOINT_VERSION: u8 = 2;
pub const MAX_CHECKPOINT_MEMBERS: usize = 256;
pub const MAX_CHECKPOINT_ROUTES: usize = 32;
pub const MAX_CHECKPOINT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SNAPSHOT_OFFER_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CAPABILITY_BYTES: usize = 4096;
pub const MAX_RESYNC_WINDOW: Duration = Duration::from_mins(1);
const MAX_KEY_BYTES: usize = 4096;
const MAX_SIGNATURE_BYTES: usize = 4096;
const MAX_MUTATION_BYTES: usize = 32 * 1024;
const SNAPSHOT_DOMAIN: &[u8] = b"p2p-vpn cooperative checkpoint v2\n";
const ANCHOR_DOMAIN: &[u8] = b"p2p-vpn checkpoint anchor v1\n";
const CAPABILITY_DOMAIN: &[u8] = b"p2p-vpn checkpoint MAC key v1\n";
const MUTATION_DOMAIN: &[u8] = b"p2p-vpn checkpoint mutation v1\n";
const HOSTNAME_DOMAIN: &[u8] = b"p2p-vpn checkpoint hostname v1\n";
const OFFER_DOMAIN: &[u8] = b"p2p-vpn checkpoint offer v1\n";
type SnapshotMac = Hmac<Sha256>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkAnchor {
    pub version: u8,
    pub network_id: [u8; 32],
    pub policy_version: u8,
    pub rank_version: u8,
}

impl NetworkAnchor {
    pub fn new(network_id: [u8; 32]) -> Result<Self, CheckpointError> {
        let anchor = Self {
            version: 1,
            network_id,
            policy_version: 1,
            rank_version: 1,
        };
        anchor.validate()?;
        Ok(anchor)
    }

    fn validate(&self) -> Result<(), CheckpointError> {
        if self.version != 1 || self.policy_version != 1 || self.rank_version != 1 {
            return Err(CheckpointError::Invalid(
                "unsupported anchor/policy/rank version",
            ));
        }
        if self.network_id == [0; 32] {
            return Err(CheckpointError::Invalid("empty network identity"));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct NetworkCapability {
    anchor: NetworkAnchor,
    mac_key: [u8; 32],
}

impl fmt::Debug for NetworkCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkCapability")
            .field("anchor", &self.anchor)
            .finish_non_exhaustive()
    }
}

impl NetworkCapability {
    /// No public network name/tag fallback is permitted. The secret is not retained.
    pub fn from_secret(
        anchor: NetworkAnchor,
        secret: Option<&[u8]>,
    ) -> Result<Self, CheckpointError> {
        anchor.validate()?;
        let secret = secret.ok_or(CheckpointError::MissingCapability)?;
        if !(32..=MAX_CAPABILITY_BYTES).contains(&secret.len()) {
            return Err(CheckpointError::Invalid(
                "capability must contain 32..=4096 bytes",
            ));
        }
        let salt = digest(ANCHOR_DOMAIN, &anchor)?;
        let mut mac_key = [0; 32];
        Hkdf::<Sha256>::new(Some(&salt), secret)
            .expand(CAPABILITY_DOMAIN, &mut mac_key)
            .map_err(|_| CheckpointError::Invalid("capability derivation failed"))?;
        Ok(Self { anchor, mac_key })
    }

    #[must_use]
    pub fn anchor(&self) -> &NetworkAnchor {
        &self.anchor
    }

    /// Authenticates content, not the historical truth of a holder's claimed score.
    pub fn authenticate(
        &self,
        payload: CheckpointSnapshot,
    ) -> Result<AuthenticatedSnapshot, CheckpointError> {
        self.validate_scope(&payload)?;
        let mut mac = self.mac();
        mac.update(&message(SNAPSHOT_DOMAIN, &payload)?);
        Ok(AuthenticatedSnapshot {
            payload,
            mac: mac.finalize().into_bytes().into(),
        })
    }

    pub fn verify(&self, snapshot: &AuthenticatedSnapshot) -> Result<(), CheckpointError> {
        self.validate_scope(&snapshot.payload)?;
        let mut mac = self.mac();
        mac.update(&message(SNAPSHOT_DOMAIN, &snapshot.payload)?);
        mac.verify_slice(&snapshot.mac)
            .map_err(|_| CheckpointError::InvalidMac)
    }

    fn validate_scope(&self, snapshot: &CheckpointSnapshot) -> Result<(), CheckpointError> {
        if snapshot.anchor != self.anchor {
            return Err(CheckpointError::WrongAnchor);
        }
        snapshot.validate()
    }

    fn mac(&self) -> SnapshotMac {
        SnapshotMac::new_from_slice(&self.mac_key).expect("fixed-length HMAC key")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotPolicy {
    pub max_active_members: u16,
    pub route_grants_enabled: bool,
}

impl Default for SnapshotPolicy {
    fn default() -> Self {
        Self {
            max_active_members: u16::try_from(MAX_CHECKPOINT_MEMBERS)
                .expect("bounded member limit"),
            route_grants_enabled: true,
        }
    }
}

impl SnapshotPolicy {
    fn validate(&self, count: usize) -> Result<(), CheckpointError> {
        if self.max_active_members == 0
            || usize::from(self.max_active_members) > MAX_CHECKPOINT_MEMBERS
            || count > usize::from(self.max_active_members)
        {
            return Err(CheckpointError::Invalid("invalid member limit"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotPublisher {
    pub peer_id: String,
    pub public_key: String,
}

impl SnapshotPublisher {
    pub fn from_identity(identity: &NodeIdentity) -> Result<Self, CheckpointError> {
        Ok(Self {
            peer_id: identity.peer_id.clone(),
            public_key: STANDARD.encode(
                identity
                    .public_key_protobuf()
                    .map_err(MembershipRecordError::from)?,
            ),
        })
    }

    fn validate(&self) -> Result<PublicKey, CheckpointError> {
        field_bound(&self.peer_id, 128)?;
        field_bound(&self.public_key, MAX_KEY_BYTES)?;
        let peer = self
            .peer_id
            .parse::<TransportPeer>()
            .map_err(MembershipRecordError::from)?;
        let key = decode_public_key(&self.public_key)?;
        if key.to_peer_id() != peer
            || peer.to_string() != self.peer_id
            || STANDARD.encode(key.encode_protobuf()) != self.public_key
        {
            return Err(CheckpointError::Invalid(
                "noncanonical or mismatched member key",
            ));
        }
        Ok(key)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointMember {
    pub subject: SnapshotPublisher,
    pub incarnation: [u8; 32],
    pub roles: Vec<MembershipRole>,
    pub route_grants: Vec<RouteConfig>,
    pub expires_at_unix_seconds: Option<u64>,
}

impl CheckpointMember {
    /// Initial formation descriptor. Later admissions rebind its incarnation to
    /// their exact checkpoint boundary in `sign_mutation_at`.
    pub fn new(identity: &NodeIdentity) -> Result<Self, CheckpointError> {
        Ok(Self {
            subject: SnapshotPublisher::from_identity(identity)?,
            incarnation: nonce(),
            roles: vec![MembershipRole::OverlayMember],
            route_grants: vec![],
            expires_at_unix_seconds: None,
        })
    }

    #[must_use]
    pub fn active_at(&self, now: u64) -> bool {
        self.expires_at_unix_seconds
            .is_none_or(|expiry| now < expiry)
    }

    fn validate(&self) -> Result<(), CheckpointError> {
        self.subject.validate()?;
        if self.incarnation == [0; 32] {
            return Err(CheckpointError::Invalid("empty member incarnation"));
        }
        if self.roles != [MembershipRole::OverlayMember]
            && self.roles
                != [
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ]
        {
            return Err(CheckpointError::Invalid("noncanonical membership roles"));
        }
        if self.route_grants.len() > MAX_CHECKPOINT_ROUTES {
            return Err(CheckpointError::Invalid("too many route grants"));
        }
        if !self.route_grants.is_empty() && !self.roles.contains(&MembershipRole::RouteAuthority) {
            return Err(CheckpointError::Invalid(
                "route grants without route authority",
            ));
        }
        let mut last: Option<(&str, u16)> = None;
        for route in &self.route_grants {
            field_bound(&route.prefix, 64)?;
            if canonical_route(route)? != route.prefix
                || last.is_some_and(|last| last >= (route.prefix.as_str(), route.metric))
            {
                return Err(CheckpointError::Invalid("noncanonical route grants"));
            }
            last = Some((&route.prefix, route.metric));
        }
        if let Some(expiry) = self.expires_at_unix_seconds {
            portable(expiry)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointSnapshot {
    pub version: u8,
    pub anchor: NetworkAnchor,
    pub authority_revision: u64,
    pub parent_digest: Option<[u8; 32]>,
    pub policy: SnapshotPolicy,
    pub members: Vec<CheckpointMember>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointBoundary {
    pub authority_revision: u64,
    pub digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SnapshotRank {
    pub authority_revision: u64,
    /// Canonical roster population, not locally observed online population.
    /// Expiry is enforced on access; `PruneExpired` removes expired roster entries.
    pub active_member_count: usize,
    pub digest: [u8; 32],
}

impl CheckpointSnapshot {
    pub fn validate(&self) -> Result<(), CheckpointError> {
        if self.version != COOPERATIVE_CHECKPOINT_VERSION {
            return Err(CheckpointError::Invalid("unsupported checkpoint version"));
        }
        self.anchor.validate()?;
        portable(self.authority_revision)?;
        if (self.authority_revision == 0) != self.parent_digest.is_none()
            || self.parent_digest == Some([0; 32])
        {
            return Err(CheckpointError::Invalid("invalid checkpoint boundary"));
        }
        if self.members.len() > MAX_CHECKPOINT_MEMBERS {
            return Err(CheckpointError::Invalid("too many members"));
        }
        self.policy.validate(self.members.len())?;
        let mut last = None;
        for member in &self.members {
            member.validate()?;
            if last.is_some_and(|last: &str| last >= member.subject.peer_id.as_str()) {
                return Err(CheckpointError::Invalid("noncanonical member ordering"));
            }
            last = Some(member.subject.peer_id.as_str());
        }
        encoded_bound(self, MAX_CHECKPOINT_BYTES)
    }

    pub fn boundary(&self) -> Result<CheckpointBoundary, CheckpointError> {
        self.validate()?;
        Ok(CheckpointBoundary {
            authority_revision: self.authority_revision,
            digest: digest(SNAPSHOT_DOMAIN, self)?,
        })
    }

    pub fn rank(&self) -> Result<SnapshotRank, CheckpointError> {
        Ok(SnapshotRank {
            authority_revision: self.authority_revision,
            active_member_count: self.members.len(),
            digest: self.boundary()?.digest,
        })
    }

    #[must_use]
    pub fn member(&self, peer: &str) -> Option<&CheckpointMember> {
        self.members
            .binary_search_by(|m| m.subject.peer_id.as_str().cmp(peer))
            .ok()
            .map(|index| &self.members[index])
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedSnapshot {
    pub payload: CheckpointSnapshot,
    pub mac: [u8; 32],
}

impl AuthenticatedSnapshot {
    pub fn decode(bytes: &[u8], capability: &NetworkCapability) -> Result<Self, CheckpointError> {
        bytes_bound(bytes, MAX_CHECKPOINT_BYTES)?;
        let snapshot = serde_json::from_slice(bytes)?;
        capability.verify(&snapshot)?;
        Ok(snapshot)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MembershipChange {
    UpsertMember(CheckpointMember),
    RemoveMember(String),
    SetPolicy(SnapshotPolicy),
    /// Integration must schedule this genuine expiry cleanup. Offline reachability
    /// is never an expiry condition. Projection denies expired entries immediately.
    PruneExpired,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipMutationPayload {
    pub version: u8,
    pub anchor: NetworkAnchor,
    pub base: CheckpointBoundary,
    pub issuer: SnapshotPublisher,
    pub change: MembershipChange,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Ephemeral outbound command, not retained history. For self-departure, callers
/// sign and capture the bounded current recipient set BEFORE local application,
/// then deliver this last command with a bounded deadline/retry budget even after
/// local exclusion. Recipients authorize it against its old exact base. Do not
/// grant excluded identities a general snapshot publishing privilege; offline or
/// stale recipients catch up from active survivors instead.
pub struct SignedMembershipMutation {
    pub payload: MembershipMutationPayload,
    pub signature: String,
}

impl SignedMembershipMutation {
    /// Authenticate a bounded command, not its current membership authority.
    /// Callers must still apply it against the selected exact checkpoint base.
    pub fn authenticate_for(
        &self,
        expected_anchor: &NetworkAnchor,
        transport_sender: libp2p::PeerId,
    ) -> Result<(), CheckpointError> {
        let payload = &self.payload;
        if payload.version != 1 {
            return Err(CheckpointError::Invalid("unsupported mutation version"));
        }
        payload.anchor.validate()?;
        if &payload.anchor != expected_anchor {
            return Err(CheckpointError::WrongAnchor);
        }
        portable(payload.base.authority_revision)?;
        if payload.base.digest == [0; 32] {
            return Err(CheckpointError::Invalid("empty mutation base"));
        }
        let key = payload.issuer.validate()?;
        if key.to_peer_id() != transport_sender {
            return Err(CheckpointError::Invalid(
                "mutation transport issuer mismatch",
            ));
        }
        match &payload.change {
            MembershipChange::UpsertMember(member) => member.validate()?,
            MembershipChange::RemoveMember(peer) => validate_local_peer(peer)?,
            MembershipChange::SetPolicy(policy) => policy.validate(0)?,
            MembershipChange::PruneExpired => (),
        }
        encoded_bound(self, MAX_MUTATION_BYTES)?;
        verify_signature(&payload.issuer, MUTATION_DOMAIN, payload, &self.signature)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostnameClaimPayload {
    pub version: u8,
    pub anchor: NetworkAnchor,
    pub subject: SnapshotPublisher,
    pub incarnation: [u8; 32],
    pub sequence: u64,
    pub hostname: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedHostnameClaim {
    pub payload: HostnameClaimPayload,
    pub signature: String,
}

impl SignedHostnameClaim {
    pub fn issue(
        anchor: NetworkAnchor,
        identity: &NodeIdentity,
        incarnation: [u8; 32],
        sequence: u64,
        hostname: &str,
    ) -> Result<Self, CheckpointError> {
        let payload = HostnameClaimPayload {
            version: 1,
            anchor,
            subject: SnapshotPublisher::from_identity(identity)?,
            incarnation,
            sequence,
            hostname: canonical_dns_label(hostname)
                .map_err(MembershipRecordError::InvalidHostname)?,
        };
        validate_hostname_payload(&payload)?;
        let signature = sign(identity, HOSTNAME_DOMAIN, &payload)?;
        Ok(Self { payload, signature })
    }

    fn verify(&self, snapshot: &CheckpointSnapshot) -> Result<(), CheckpointError> {
        validate_hostname_payload(&self.payload)?;
        if self.payload.anchor != snapshot.anchor {
            return Err(CheckpointError::WrongAnchor);
        }
        let member = snapshot
            .member(&self.payload.subject.peer_id)
            .ok_or(CheckpointError::Invalid("unknown hostname subject"))?;
        if self.payload.subject != member.subject || self.payload.incarnation != member.incarnation
        {
            return Err(CheckpointError::Invalid(
                "hostname subject/incarnation mismatch",
            ));
        }
        verify_signature(
            &self.payload.subject,
            HOSTNAME_DOMAIN,
            &self.payload,
            &self.signature,
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotChallenge {
    pub anchor: NetworkAnchor,
    pub nonce: [u8; 32],
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotOfferPayload {
    pub version: u8,
    pub challenge: SnapshotChallenge,
    pub snapshot: AuthenticatedSnapshot,
    pub publisher: SnapshotPublisher,
    pub hostname_claims: Vec<SignedHostnameClaim>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedSnapshotOffer {
    pub payload: SnapshotOfferPayload,
    pub signature: String,
}

impl SignedSnapshotOffer {
    pub fn decode(bytes: &[u8]) -> Result<Self, CheckpointError> {
        bytes_bound(bytes, MAX_SNAPSHOT_OFFER_BYTES)?;
        let offer: Self = serde_json::from_slice(bytes)?;
        offer.validate_shape()?;
        Ok(offer)
    }

    fn validate_shape(&self) -> Result<(), CheckpointError> {
        if self.payload.version != 1 || self.payload.challenge.nonce == [0; 32] {
            return Err(CheckpointError::Invalid("invalid offer version/challenge"));
        }
        self.payload.challenge.anchor.validate()?;
        self.payload.snapshot.payload.validate()?;
        self.payload.publisher.validate()?;
        validate_names(
            &self.payload.snapshot.payload,
            &self.payload.hostname_claims,
        )?;
        field_bound(&self.signature, MAX_SIGNATURE_BYTES)?;
        encoded_bound(self, MAX_SNAPSHOT_OFFER_BYTES)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembershipSyncState {
    ResyncRequired,
    Resyncing,
    Participating,
    Excluded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfferOutcome {
    SelectedHigherRank,
    KeptCurrentRank,
}

/// Transient, aggregate diagnostics. Never an archive of old peers or decisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchSelection {
    pub previous: CheckpointBoundary,
    pub selected: CheckpointBoundary,
    pub decisions_may_have_been_discarded: bool,
    pub added_members: usize,
    pub removed_members: usize,
    pub observed_remote_offer: bool,
    pub sync_state: MembershipSyncState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetainedCheckpointState {
    pub snapshot: AuthenticatedSnapshot,
    pub hostname_claims: Vec<SignedHostnameClaim>,
}

impl RetainedCheckpointState {
    pub fn decode(bytes: &[u8], capability: &NetworkCapability) -> Result<Self, CheckpointError> {
        bytes_bound(bytes, MAX_SNAPSHOT_OFFER_BYTES)?;
        let state: Self = serde_json::from_slice(bytes)?;
        capability.verify(&state.snapshot)?;
        validate_names(&state.snapshot.payload, &state.hostname_claims)?;
        Ok(state)
    }
}

#[derive(Clone, Debug)]
struct ResyncRound {
    challenge: SnapshotChallenge,
    deadline: Instant,
    best: AuthenticatedSnapshot,
    names: Vec<SignedHostnameClaim>,
    observed_offer: bool,
}

#[derive(Clone, Debug)]
pub struct CooperativeMembershipState {
    capability: NetworkCapability,
    local_peer: String,
    current: AuthenticatedSnapshot,
    names: Vec<SignedHostnameClaim>,
    sync_state: MembershipSyncState,
    round: Option<ResyncRound>,
}

impl CooperativeMembershipState {
    /// Only for caller-authorized initial formation/migration, not ordinary restart.
    pub fn bootstrap_at(
        capability: NetworkCapability,
        local_peer: String,
        mut members: Vec<CheckpointMember>,
        policy: SnapshotPolicy,
        now: u64,
    ) -> Result<Self, CheckpointError> {
        validate_local_peer(&local_peer)?;
        members.sort_by(|a, b| a.subject.peer_id.cmp(&b.subject.peer_id));
        let current = capability.authenticate(CheckpointSnapshot {
            version: COOPERATIVE_CHECKPOINT_VERSION,
            anchor: capability.anchor.clone(),
            authority_revision: 0,
            parent_digest: None,
            policy,
            members,
        })?;
        let sync_state = participation(&current.payload, &local_peer, now);
        Ok(Self {
            capability,
            local_peer,
            current,
            names: vec![],
            sync_state,
            round: None,
        })
    }

    /// Requires a caller-trusted legacy history and an agreed, newly pinned anchor.
    /// Legacy inviter signatures cannot be converted into self-signed hostname claims.
    pub fn migrate_trusted_legacy_at(
        capability: NetworkCapability,
        local_peer: String,
        records: &[SignedMembershipRecord],
        network_name: &str,
        now: u64,
    ) -> Result<Self, CheckpointError> {
        validate_membership_record_history(records, network_name)?;
        if records
            .iter()
            .any(|record| record.payload.issued_at_unix_seconds > now)
        {
            return Err(CheckpointError::Invalid("future legacy history"));
        }
        let roots = membership_trust_anchors(records, network_name)?;
        let evaluation = evaluate_membership_ledger_at(records, &roots, now)?;
        if evaluation.accepted.len() != records.len() {
            return Err(CheckpointError::Invalid("unaccepted legacy history"));
        }
        let mut members = Vec::new();
        for state in evaluation.states.values().filter(|state| state.active) {
            let record = &records[state.record_index];
            let payload = &record.payload;
            if !payload.roles.contains(&MembershipRole::OverlayMember) {
                continue;
            }
            let mut routes = payload.route_grants.clone();
            for route in &mut routes {
                route.prefix = canonical_route(route)?;
            }
            routes.sort_by(|a, b| (&a.prefix, a.metric).cmp(&(&b.prefix, b.metric)));
            routes.dedup();
            let mut roles = vec![MembershipRole::OverlayMember];
            if payload.roles.contains(&MembershipRole::RouteAuthority) {
                roles.push(MembershipRole::RouteAuthority);
            }
            members.push(CheckpointMember {
                subject: SnapshotPublisher {
                    peer_id: payload.member_peer.clone(),
                    public_key: payload.member_public_key.clone(),
                },
                incarnation: digest(
                    b"p2p-vpn legacy incarnation v1\n",
                    &(capability.anchor(), record),
                )?,
                roles,
                route_grants: routes,
                expires_at_unix_seconds: payload.expires_at_unix_seconds,
            });
        }
        Self::bootstrap_at(
            capability,
            local_peer,
            members,
            SnapshotPolicy::default(),
            now,
        )
    }

    /// Restarted/returning nodes must resync before mutation, publication or packets.
    pub fn restore(
        capability: NetworkCapability,
        local_peer: String,
        state: RetainedCheckpointState,
    ) -> Result<Self, CheckpointError> {
        validate_local_peer(&local_peer)?;
        capability.verify(&state.snapshot)?;
        validate_names(&state.snapshot.payload, &state.hostname_claims)?;
        Ok(Self {
            capability,
            local_peer,
            current: state.snapshot,
            names: state.hostname_claims,
            sync_state: MembershipSyncState::ResyncRequired,
            round: None,
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> &AuthenticatedSnapshot {
        &self.current
    }
    #[must_use]
    pub(crate) fn local_peer(&self) -> &str {
        &self.local_peer
    }
    #[must_use]
    pub fn hostname_claims(&self) -> &[SignedHostnameClaim] {
        &self.names
    }
    #[must_use]
    pub fn sync_state(&self) -> MembershipSyncState {
        self.sync_state
    }
    #[must_use]
    pub fn retained(&self) -> RetainedCheckpointState {
        RetainedCheckpointState {
            snapshot: self.current.clone(),
            hostname_claims: self.names.clone(),
        }
    }

    /// New admissions get a boundary-bound incarnation. Existing members retain
    /// theirs through cosmetic/grant changes; explicit remove/admit is required
    /// to begin another incarnation without a growing per-peer history archive.
    pub fn sign_mutation_at(
        &self,
        identity: &NodeIdentity,
        mut change: MembershipChange,
        now: u64,
    ) -> Result<SignedMembershipMutation, CheckpointError> {
        self.require_participation(now)?;
        if identity.peer_id != self.local_peer {
            return Err(CheckpointError::Invalid("not the local identity"));
        }
        let base = self.current.payload.boundary()?;
        if let MembershipChange::UpsertMember(member) = &mut change {
            member.validate()?;
            if self
                .current
                .payload
                .member(&member.subject.peer_id)
                .is_none()
            {
                member.incarnation =
                    admission_incarnation(self.capability.anchor(), base, &member.subject)?;
            }
        }
        let payload = MembershipMutationPayload {
            version: 1,
            anchor: self.capability.anchor.clone(),
            base,
            issuer: SnapshotPublisher::from_identity(identity)?,
            change,
        };
        self.validate_mutation(&payload, now)?;
        self.changed_snapshot(&payload.change, now)?;
        Ok(SignedMembershipMutation {
            signature: sign(identity, MUTATION_DOMAIN, &payload)?,
            payload,
        })
    }

    pub fn apply_mutation_at(
        &mut self,
        mutation: &SignedMembershipMutation,
        now: u64,
    ) -> Result<(), CheckpointError> {
        self.require_participation(now)?;
        self.validate_mutation(&mutation.payload, now)?;
        field_bound(&mutation.signature, MAX_SIGNATURE_BYTES)?;
        encoded_bound(mutation, MAX_MUTATION_BYTES)?;
        verify_signature(
            &mutation.payload.issuer,
            MUTATION_DOMAIN,
            &mutation.payload,
            &mutation.signature,
        )?;
        let next = self.changed_snapshot(&mutation.payload.change, now)?;
        let names = compatible_names(&next.payload, &self.names);
        self.current = next;
        self.names = names;
        self.sync_state = participation(&self.current.payload, &self.local_peer, now);
        Ok(())
    }

    fn validate_mutation(
        &self,
        payload: &MembershipMutationPayload,
        now: u64,
    ) -> Result<(), CheckpointError> {
        if payload.version != 1 {
            return Err(CheckpointError::Invalid("unsupported mutation version"));
        }
        if payload.anchor != self.capability.anchor {
            return Err(CheckpointError::WrongAnchor);
        }
        if payload.base != self.current.payload.boundary()? {
            return Err(CheckpointError::StaleMutation);
        }
        payload.issuer.validate()?;
        let issuer = self
            .current
            .payload
            .member(&payload.issuer.peer_id)
            .filter(|member| member.active_at(now))
            .ok_or(CheckpointError::Invalid("inactive mutation issuer"))?;
        if issuer.subject != payload.issuer {
            return Err(CheckpointError::Invalid("mutation issuer key mismatch"));
        }
        match &payload.change {
            MembershipChange::UpsertMember(member) => {
                member.validate()?;
                if !member.active_at(now) {
                    return Err(CheckpointError::Invalid("expired admission"));
                }
                let expected_incarnation =
                    match self.current.payload.member(&member.subject.peer_id) {
                        Some(current) => current.incarnation,
                        None => admission_incarnation(
                            self.capability.anchor(),
                            payload.base,
                            &member.subject,
                        )?,
                    };
                if member.incarnation != expected_incarnation {
                    return Err(CheckpointError::Invalid(
                        "admission incarnation is not checkpoint-bound",
                    ));
                }
            }
            MembershipChange::RemoveMember(peer) => validate_local_peer(peer)?,
            MembershipChange::SetPolicy(policy) => {
                policy.validate(self.current.payload.members.len())?;
            }
            MembershipChange::PruneExpired => (),
        }
        encoded_bound(payload, MAX_MUTATION_BYTES)
    }

    fn changed_snapshot(
        &self,
        change: &MembershipChange,
        now: u64,
    ) -> Result<AuthenticatedSnapshot, CheckpointError> {
        let mut next = self.current.payload.clone();
        match change {
            MembershipChange::UpsertMember(member) => {
                match next
                    .members
                    .binary_search_by(|m| m.subject.peer_id.cmp(&member.subject.peer_id))
                {
                    Ok(index) => next.members[index] = member.clone(),
                    Err(index) => next.members.insert(index, member.clone()),
                }
            }
            MembershipChange::RemoveMember(peer) => next
                .members
                .retain(|member| member.subject.peer_id != *peer),
            MembershipChange::SetPolicy(policy) => next.policy = policy.clone(),
            MembershipChange::PruneExpired => next.members.retain(|member| member.active_at(now)),
        }
        if next.members == self.current.payload.members
            && next.policy == self.current.payload.policy
        {
            return Err(CheckpointError::NoAuthorityChange);
        }
        next.authority_revision = next
            .authority_revision
            .checked_add(1)
            .filter(|revision| *revision <= MAX_MEMBERSHIP_RECORD_INTEGER)
            .ok_or(CheckpointError::RevisionExhausted)?;
        next.parent_digest = Some(self.current.payload.boundary()?.digest);
        self.capability.authenticate(next)
    }

    /// Hostname sequence is primary; equal-sequence concurrent names choose the
    /// lexicographically larger canonical label. This cosmetic tie-break cannot
    /// stall authority resync; any newer sequence supersedes either old label.
    pub fn merge_hostname_claims(
        &mut self,
        claims: &[SignedHostnameClaim],
    ) -> Result<(), CheckpointError> {
        validate_names(&self.current.payload, claims)?;
        self.names = merge_names(&self.current.payload, &self.names, claims)?;
        Ok(())
    }

    pub fn make_offer_at(
        &self,
        challenge: SnapshotChallenge,
        identity: &NodeIdentity,
        now: u64,
    ) -> Result<SignedSnapshotOffer, CheckpointError> {
        self.require_participation(now)?;
        if identity.peer_id != self.local_peer {
            return Err(CheckpointError::Invalid("not the local identity"));
        }
        if challenge.anchor != self.capability.anchor {
            return Err(CheckpointError::WrongAnchor);
        }
        if challenge.nonce == [0; 32] {
            return Err(CheckpointError::Invalid("empty challenge"));
        }
        let publisher = SnapshotPublisher::from_identity(identity)?;
        if self
            .current
            .payload
            .member(&publisher.peer_id)
            .is_none_or(|member| member.subject != publisher)
        {
            return Err(CheckpointError::Invalid("publisher key mismatch"));
        }
        let payload = SnapshotOfferPayload {
            version: 1,
            challenge,
            snapshot: self.current.clone(),
            publisher,
            hostname_claims: self.names.clone(),
        };
        let offer = SignedSnapshotOffer {
            signature: sign(identity, OFFER_DOMAIN, &payload)?,
            payload,
        };
        offer.validate_shape()?;
        Ok(offer)
    }

    pub fn begin_resync(
        &mut self,
        now: Instant,
        window: Duration,
    ) -> Result<SnapshotChallenge, CheckpointError> {
        if window.is_zero() || window > MAX_RESYNC_WINDOW {
            return Err(CheckpointError::Invalid("invalid resync window"));
        }
        let deadline = now
            .checked_add(window)
            .ok_or(CheckpointError::Invalid("resync deadline overflow"))?;
        let challenge = SnapshotChallenge {
            anchor: self.capability.anchor.clone(),
            nonce: nonce(),
        };
        self.round = Some(ResyncRound {
            challenge: challenge.clone(),
            deadline,
            best: self.current.clone(),
            names: self.names.clone(),
            observed_offer: false,
        });
        self.sync_state = MembershipSyncState::Resyncing;
        Ok(challenge)
    }

    pub fn collect_offer(
        &mut self,
        offer: &SignedSnapshotOffer,
        transport_peer: TransportPeer,
        now: Instant,
    ) -> Result<OfferOutcome, CheckpointError> {
        let round = self.round.as_ref().ok_or(CheckpointError::NoSyncRound)?;
        if now >= round.deadline {
            return Err(CheckpointError::SyncWindowClosed);
        }
        if offer.payload.challenge != round.challenge {
            return Err(CheckpointError::WrongChallenge);
        }
        offer.validate_shape()?;
        self.capability.verify(&offer.payload.snapshot)?;
        if offer.payload.publisher.peer_id != transport_peer.to_string() {
            return Err(CheckpointError::WrongPublisher);
        }
        let offered = &offer.payload.snapshot.payload;
        if offered
            .member(&offer.payload.publisher.peer_id)
            .is_none_or(|member| member.subject != offer.payload.publisher)
        {
            return Err(CheckpointError::WrongPublisher);
        }
        verify_signature(
            &offer.payload.publisher,
            OFFER_DOMAIN,
            &offer.payload,
            &offer.signature,
        )?;
        let rank = offered.rank()?;
        if rank < self.current.payload.rank()? {
            return Err(CheckpointError::StaleSnapshot);
        }
        let outcome = if rank > round.best.payload.rank()? {
            OfferOutcome::SelectedHigherRank
        } else {
            OfferOutcome::KeptCurrentRank
        };
        let best = if outcome == OfferOutcome::SelectedHigherRank {
            offer.payload.snapshot.clone()
        } else {
            round.best.clone()
        };
        let names = merge_names(
            &best.payload,
            &compatible_names(&best.payload, &round.names),
            &compatible_names(&best.payload, &offer.payload.hostname_claims),
        )?;
        let round = self.round.as_mut().expect("checked resync round");
        round.best = best;
        round.names = names;
        round.observed_offer = true;
        Ok(outcome)
    }

    /// Advance a live refresh after a separately authorized, durably installed
    /// command. Preserve collected higher branches and the original sync window.
    pub(crate) fn rebase_live_resync_on_installed(
        &mut self,
        installed: &Self,
    ) -> Result<(), CheckpointError> {
        if self.local_peer != installed.local_peer
            || self.capability.anchor != installed.capability.anchor
            || !matches!(
                installed.sync_state,
                MembershipSyncState::Participating | MembershipSyncState::Excluded
            )
        {
            return Err(CheckpointError::NoParticipation);
        }
        self.capability.verify(&installed.current)?;
        validate_names(&installed.current.payload, &installed.names)?;
        let previous = self.current.payload.boundary()?;
        if previous.authority_revision.checked_add(1)
            != Some(installed.current.payload.authority_revision)
            || installed.current.payload.parent_digest != Some(previous.digest)
        {
            return Err(CheckpointError::StaleMutation);
        }
        let round = self.round.as_ref().ok_or(CheckpointError::NoSyncRound)?;
        let best = if installed.current.payload.rank()? > round.best.payload.rank()? {
            installed.current.clone()
        } else {
            round.best.clone()
        };
        let names = merge_names(
            &best.payload,
            &compatible_names(&best.payload, &round.names),
            &compatible_names(&best.payload, &installed.names),
        )?;
        self.current = installed.current.clone();
        self.names = installed.names.clone();
        let round = self.round.as_mut().expect("checked sync round");
        round.best = best;
        round.names = names;
        Ok(())
    }

    pub fn finish_resync(
        &mut self,
        now: Instant,
        wall_now: u64,
    ) -> Result<BranchSelection, CheckpointError> {
        let round = self.round.as_ref().ok_or(CheckpointError::NoSyncRound)?;
        if now < round.deadline {
            return Err(CheckpointError::SyncWindowNotElapsed);
        }
        self.capability.verify(&round.best)?;
        let previous = self.current.payload.boundary()?;
        let selected = round.best.payload.boundary()?;
        let added_members = round
            .best
            .payload
            .members
            .iter()
            .filter(|member| {
                self.current
                    .payload
                    .member(&member.subject.peer_id)
                    .is_none()
            })
            .count();
        let removed_members = self
            .current
            .payload
            .members
            .iter()
            .filter(|member| round.best.payload.member(&member.subject.peer_id).is_none())
            .count();
        let immediate_successor = previous.authority_revision.checked_add(1)
            == Some(selected.authority_revision)
            && round.best.payload.parent_digest == Some(previous.digest);
        let sync_state = participation(&round.best.payload, &self.local_peer, wall_now);
        let result = BranchSelection {
            previous,
            selected,
            decisions_may_have_been_discarded: previous != selected && !immediate_successor,
            added_members,
            removed_members,
            observed_remote_offer: round.observed_offer,
            sync_state,
        };
        let round = self.round.take().expect("checked resync round");
        self.current = round.best;
        self.names = round.names;
        self.sync_state = sync_state;
        Ok(result)
    }

    /// Snapshot-authoritative namespace: absent static peers never get implicit grants.
    pub fn effective_membership_at(
        &self,
        now: u64,
    ) -> Result<EffectiveMembership, CheckpointError> {
        let mut effective = EffectiveMembership {
            members: HashMap::new(),
            governed_peers: HashSet::default(),
            checkpoint_authoritative: true,
        };
        if self.require_participation(now).is_err() {
            return Ok(effective);
        }
        for member in self
            .current
            .payload
            .members
            .iter()
            .filter(|member| member.active_at(now))
        {
            let transport_peer = member
                .subject
                .peer_id
                .parse::<TransportPeer>()
                .map_err(MembershipRecordError::from)?;
            let peer = PeerId::from_libp2p(transport_peer);
            let hostnames = self
                .names
                .iter()
                .filter(|claim| {
                    claim.payload.subject == member.subject
                        && claim.payload.incarnation == member.incarnation
                })
                .map(|claim| claim.payload.hostname.clone())
                .collect();
            let mut roles = member.roles.clone();
            let route_grants = if self.current.payload.policy.route_grants_enabled {
                member.route_grants.clone()
            } else {
                roles.retain(|role| *role != MembershipRole::RouteAuthority);
                vec![]
            };
            effective.members.insert(
                peer,
                EffectiveMember {
                    peer,
                    transport_peer,
                    membership_epoch: self.current.payload.authority_revision.max(1),
                    sequence: 0,
                    effective_inviter_peer: None,
                    original_inviter_peer: None,
                    admitted_at_unix_seconds: 0,
                    original_admitted_at_unix_seconds: 0,
                    hostnames,
                    roles,
                    route_grants,
                },
            );
        }
        Ok(effective)
    }

    fn require_participation(&self, now: u64) -> Result<(), CheckpointError> {
        if self.sync_state != MembershipSyncState::Participating
            || participation(&self.current.payload, &self.local_peer, now)
                != MembershipSyncState::Participating
        {
            return Err(CheckpointError::NoParticipation);
        }
        Ok(())
    }
}

fn participation(snapshot: &CheckpointSnapshot, local: &str, now: u64) -> MembershipSyncState {
    if snapshot
        .member(local)
        .is_some_and(|member| member.active_at(now))
    {
        MembershipSyncState::Participating
    } else {
        MembershipSyncState::Excluded
    }
}

fn validate_local_peer(peer: &str) -> Result<(), CheckpointError> {
    field_bound(peer, 128)?;
    if peer
        .parse::<TransportPeer>()
        .map_err(MembershipRecordError::from)?
        .to_string()
        != peer
    {
        return Err(CheckpointError::Invalid("noncanonical peer identity"));
    }
    Ok(())
}

fn validate_hostname_payload(payload: &HostnameClaimPayload) -> Result<(), CheckpointError> {
    if payload.version != 1 || payload.incarnation == [0; 32] {
        return Err(CheckpointError::Invalid(
            "invalid hostname version/incarnation",
        ));
    }
    payload.anchor.validate()?;
    payload.subject.validate()?;
    portable(payload.sequence)?;
    field_bound(&payload.hostname, 63)?;
    if canonical_dns_label(&payload.hostname).map_err(MembershipRecordError::InvalidHostname)?
        != payload.hostname
    {
        return Err(CheckpointError::Invalid("noncanonical hostname"));
    }
    Ok(())
}

fn validate_names(
    snapshot: &CheckpointSnapshot,
    names: &[SignedHostnameClaim],
) -> Result<(), CheckpointError> {
    if names.len() > MAX_CHECKPOINT_MEMBERS || names.len() > snapshot.members.len() {
        return Err(CheckpointError::Invalid("too many hostname claims"));
    }
    let mut last = None;
    for name in names {
        if last.is_some_and(|last: &str| last >= name.payload.subject.peer_id.as_str()) {
            return Err(CheckpointError::Invalid("noncanonical hostname ordering"));
        }
        name.verify(snapshot)?;
        last = Some(name.payload.subject.peer_id.as_str());
    }
    Ok(())
}

fn compatible_names(
    snapshot: &CheckpointSnapshot,
    names: &[SignedHostnameClaim],
) -> Vec<SignedHostnameClaim> {
    names
        .iter()
        .filter(|claim| {
            snapshot
                .member(&claim.payload.subject.peer_id)
                .is_some_and(|member| {
                    member.subject == claim.payload.subject
                        && member.incarnation == claim.payload.incarnation
                })
        })
        .cloned()
        .collect()
}

fn merge_names(
    snapshot: &CheckpointSnapshot,
    current: &[SignedHostnameClaim],
    incoming: &[SignedHostnameClaim],
) -> Result<Vec<SignedHostnameClaim>, CheckpointError> {
    validate_names(snapshot, current)?;
    validate_names(snapshot, incoming)?;
    let mut names = current.to_vec();
    for claim in incoming {
        match names.binary_search_by(|name| {
            name.payload
                .subject
                .peer_id
                .cmp(&claim.payload.subject.peer_id)
        }) {
            Ok(index) if names[index].payload.sequence > claim.payload.sequence => (),
            Ok(index) if names[index].payload.sequence == claim.payload.sequence => {
                if claim.payload.hostname > names[index].payload.hostname {
                    names[index] = claim.clone();
                }
            }
            Ok(index) => names[index] = claim.clone(),
            Err(index) => names.insert(index, claim.clone()),
        }
    }
    Ok(names)
}

fn portable(value: u64) -> Result<(), CheckpointError> {
    if value > MAX_MEMBERSHIP_RECORD_INTEGER {
        return Err(CheckpointError::Invalid("integer exceeds portable range"));
    }
    Ok(())
}

fn admission_incarnation(
    anchor: &NetworkAnchor,
    base: CheckpointBoundary,
    subject: &SnapshotPublisher,
) -> Result<[u8; 32], CheckpointError> {
    digest(
        b"p2p-vpn checkpoint admission incarnation v1\n",
        &(anchor, base, subject),
    )
}

fn field_bound(value: &str, max: usize) -> Result<(), CheckpointError> {
    bytes_bound(value.as_bytes(), max)
}

fn bytes_bound(bytes: &[u8], max: usize) -> Result<(), CheckpointError> {
    if bytes.len() > max {
        return Err(CheckpointError::Invalid("encoding exceeds size limit"));
    }
    Ok(())
}

fn encoded_bound<T: Serialize>(value: &T, max: usize) -> Result<(), CheckpointError> {
    bytes_bound(&serde_json::to_vec(value)?, max)
}

fn message<T: Serialize>(domain: &[u8], payload: &T) -> Result<Vec<u8>, CheckpointError> {
    let mut message = domain.to_vec();
    message.extend(serde_json::to_vec(payload)?);
    Ok(message)
}

fn digest<T: Serialize>(domain: &[u8], payload: &T) -> Result<[u8; 32], CheckpointError> {
    Ok(Sha256::digest(message(domain, payload)?).into())
}

fn sign<T: Serialize>(
    identity: &NodeIdentity,
    domain: &[u8],
    payload: &T,
) -> Result<String, CheckpointError> {
    Ok(STANDARD.encode(
        identity
            .sign(&message(domain, payload)?)
            .map_err(MembershipRecordError::from)?,
    ))
}

fn verify_signature<T: Serialize>(
    subject: &SnapshotPublisher,
    domain: &[u8],
    payload: &T,
    signature: &str,
) -> Result<(), CheckpointError> {
    field_bound(signature, MAX_SIGNATURE_BYTES)?;
    let key = subject.validate()?;
    let signature = STANDARD
        .decode(signature)
        .map_err(MembershipRecordError::from)?;
    if !key.verify(&message(domain, payload)?, &signature) {
        return Err(CheckpointError::InvalidSignature);
    }
    Ok(())
}

fn nonce() -> [u8; 32] {
    loop {
        let mut nonce = [0; 32];
        OsRng.fill_bytes(&mut nonce);
        if nonce != [0; 32] {
            return nonce;
        }
    }
}

fn canonical_route(route: &RouteConfig) -> Result<String, CheckpointError> {
    let prefix = route
        .prefix()
        .map_err(|_| CheckpointError::Invalid("invalid route prefix"))?;
    let bits = u32::from(prefix.prefix_len());
    let address = match prefix.address() {
        std::net::IpAddr::V4(address) => std::net::IpAddr::V4(
            (u32::from(address) & u32::MAX.checked_shl(32 - bits).unwrap_or(0)).into(),
        ),
        std::net::IpAddr::V6(address) => std::net::IpAddr::V6(
            (u128::from(address) & u128::MAX.checked_shl(128 - bits).unwrap_or(0)).into(),
        ),
    };
    Ok(format!("{address}/{bits}"))
}

#[derive(Debug)]
pub enum CheckpointError {
    Record(MembershipRecordError),
    Json(serde_json::Error),
    Invalid(&'static str),
    MissingCapability,
    WrongAnchor,
    InvalidMac,
    InvalidSignature,
    NoAuthorityChange,
    RevisionExhausted,
    StaleMutation,
    StaleSnapshot,
    NoParticipation,
    NoSyncRound,
    WrongChallenge,
    WrongPublisher,
    SyncWindowClosed,
    SyncWindowNotElapsed,
}

impl From<MembershipRecordError> for CheckpointError {
    fn from(error: MembershipRecordError) -> Self {
        Self::Record(error)
    }
}
impl From<serde_json::Error> for CheckpointError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Record(error) => write!(f, "checkpoint membership record: {error:?}"),
            Self::Json(error) => write!(f, "checkpoint encoding: {error}"),
            Self::Invalid(reason) => write!(f, "invalid checkpoint: {reason}"),
            _ => write!(f, "checkpoint: {self:?}"),
        }
    }
}
impl std::error::Error for CheckpointError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::{
        MembershipRecordIssueOptions, MembershipRecordSubject,
        issue_membership_record_for_subject_at, issue_named_membership_record_for_subject_at,
    };

    const WALL_NOW: u64 = 1_100;

    fn identity() -> NodeIdentity {
        NodeIdentity::generate_ed25519().unwrap()
    }
    fn capability() -> NetworkCapability {
        NetworkCapability::from_secret(NetworkAnchor::new([7; 32]).unwrap(), Some(&[9; 32]))
            .unwrap()
    }
    fn peer(identity: &NodeIdentity) -> TransportPeer {
        identity.peer_id.parse().unwrap()
    }
    fn overlay_peer(identity: &NodeIdentity) -> PeerId {
        PeerId::from_libp2p(peer(identity))
    }
    fn state(local: &NodeIdentity, members: &[&NodeIdentity]) -> CooperativeMembershipState {
        CooperativeMembershipState::bootstrap_at(
            capability(),
            local.peer_id.clone(),
            members
                .iter()
                .map(|identity| CheckpointMember::new(identity).unwrap())
                .collect(),
            SnapshotPolicy::default(),
            WALL_NOW,
        )
        .unwrap()
    }
    fn mutate(
        state: &mut CooperativeMembershipState,
        issuer: &NodeIdentity,
        change: MembershipChange,
    ) {
        let mutation = state.sign_mutation_at(issuer, change, WALL_NOW).unwrap();
        state.apply_mutation_at(&mutation, WALL_NOW).unwrap();
    }
    fn restored(
        state: &CooperativeMembershipState,
        local: &NodeIdentity,
    ) -> CooperativeMembershipState {
        CooperativeMembershipState::restore(capability(), local.peer_id.clone(), state.retained())
            .unwrap()
    }
    fn name(
        state: &CooperativeMembershipState,
        subject: &NodeIdentity,
        sequence: u64,
        hostname: &str,
    ) -> SignedHostnameClaim {
        SignedHostnameClaim::issue(
            capability().anchor.clone(),
            subject,
            state
                .snapshot()
                .payload
                .member(&subject.peer_id)
                .unwrap()
                .incarnation,
            sequence,
            hostname,
        )
        .unwrap()
    }
    fn resync(
        receiver: &mut CooperativeMembershipState,
        publisher: &CooperativeMembershipState,
        signer: &NodeIdentity,
    ) -> BranchSelection {
        let now = Instant::now();
        let challenge = receiver.begin_resync(now, Duration::from_secs(1)).unwrap();
        let offer = publisher
            .make_offer_at(challenge, signer, WALL_NOW)
            .unwrap();
        receiver.collect_offer(&offer, peer(signer), now).unwrap();
        receiver
            .finish_resync(now + Duration::from_secs(1), WALL_NOW)
            .unwrap()
    }

    #[test]
    fn capability_requires_bounded_explicit_secret_and_pinned_scope() {
        let anchor = NetworkAnchor::new([7; 32]).unwrap();
        assert!(matches!(
            NetworkCapability::from_secret(anchor.clone(), None),
            Err(CheckpointError::MissingCapability)
        ));
        for size in [0, 31, MAX_CAPABILITY_BYTES + 1] {
            assert!(NetworkCapability::from_secret(anchor.clone(), Some(&vec![1; size])).is_err());
        }
        for size in [32, MAX_CAPABILITY_BYTES] {
            assert!(NetworkCapability::from_secret(anchor.clone(), Some(&vec![1; size])).is_ok());
        }
        assert!(NetworkAnchor::new([0; 32]).is_err());
        let mut unsupported = anchor;
        unsupported.rank_version = 2;
        assert!(NetworkCapability::from_secret(unsupported, Some(&[9; 32])).is_err());
        let debug = format!("{:?}", capability());
        assert!(!debug.contains("mac_key"));
    }

    #[test]
    fn mac_is_content_and_scope_bound_and_constant_time_verifier_rejects_tampering() {
        let a = identity();
        let current = state(&a, &[&a]);
        let good = current.snapshot();
        capability().verify(good).unwrap();
        let wrong_secret =
            NetworkCapability::from_secret(capability().anchor.clone(), Some(&[1; 32])).unwrap();
        assert!(matches!(
            wrong_secret.verify(good),
            Err(CheckpointError::InvalidMac)
        ));
        let wrong_scope =
            NetworkCapability::from_secret(NetworkAnchor::new([8; 32]).unwrap(), Some(&[9; 32]))
                .unwrap();
        assert!(matches!(
            wrong_scope.verify(good),
            Err(CheckpointError::WrongAnchor)
        ));
        let mut bad = good.clone();
        bad.payload.policy.route_grants_enabled = false;
        assert!(matches!(
            capability().verify(&bad),
            Err(CheckpointError::InvalidMac)
        ));
        bad = good.clone();
        bad.mac[0] ^= 1;
        assert!(matches!(
            capability().verify(&bad),
            Err(CheckpointError::InvalidMac)
        ));
        let bytes = serde_json::to_vec(good).unwrap();
        assert_eq!(
            AuthenticatedSnapshot::decode(&bytes, &capability()).unwrap(),
            *good
        );
    }

    #[test]
    fn singleton_advances_without_acknowledgements_and_noop_changes_cannot_inflate_rank() {
        let a = identity();
        let b = identity();
        let mut current = state(&a, &[&a]);
        for change in [
            MembershipChange::PruneExpired,
            MembershipChange::SetPolicy(SnapshotPolicy::default()),
            MembershipChange::RemoveMember(b.peer_id.clone()),
            MembershipChange::UpsertMember(current.snapshot().payload.members[0].clone()),
        ] {
            assert!(matches!(
                current.sign_mutation_at(&a, change, WALL_NOW),
                Err(CheckpointError::NoAuthorityChange)
            ));
        }
        mutate(
            &mut current,
            &a,
            MembershipChange::UpsertMember(CheckpointMember::new(&b).unwrap()),
        );
        assert_eq!(current.snapshot().payload.authority_revision, 1);
        assert_eq!(current.snapshot().payload.members.len(), 2);
        let mut policy = current.snapshot().payload.policy.clone();
        policy.route_grants_enabled = false;
        mutate(&mut current, &a, MembershipChange::SetPolicy(policy));
        assert_eq!(current.snapshot().payload.authority_revision, 2);
        // B has never connected or acknowledged. Offline does not mean removed.
        assert!(current.snapshot().payload.member(&b.peer_id).is_some());
    }

    #[test]
    fn exact_base_rejects_stale_replayed_and_conflicting_mutations_atomically() {
        let a = identity();
        let b = identity();
        let c = identity();
        let mut current = state(&a, &[&a]);
        let first = current
            .sign_mutation_at(
                &a,
                MembershipChange::UpsertMember(CheckpointMember::new(&b).unwrap()),
                WALL_NOW,
            )
            .unwrap();
        let concurrent = current
            .sign_mutation_at(
                &a,
                MembershipChange::UpsertMember(CheckpointMember::new(&c).unwrap()),
                WALL_NOW,
            )
            .unwrap();
        current.apply_mutation_at(&first, WALL_NOW).unwrap();
        let saved = current.retained();
        for stale in [&first, &concurrent] {
            assert!(matches!(
                current.apply_mutation_at(stale, WALL_NOW),
                Err(CheckpointError::StaleMutation)
            ));
            assert_eq!(current.retained(), saved);
        }
    }

    #[test]
    fn mutation_signatures_and_active_issuer_are_required() {
        let a = identity();
        let b = identity();
        let c = identity();
        let mut current = state(&a, &[&a, &b]);
        let mut mutation = current
            .sign_mutation_at(
                &a,
                MembershipChange::RemoveMember(b.peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        mutation.signature = STANDARD.encode(
            c.sign(&message(MUTATION_DOMAIN, &mutation.payload).unwrap())
                .unwrap(),
        );
        assert!(matches!(
            current.apply_mutation_at(&mutation, WALL_NOW),
            Err(CheckpointError::InvalidSignature)
        ));
        mutation.payload.issuer = SnapshotPublisher::from_identity(&c).unwrap();
        assert!(current.apply_mutation_at(&mutation, WALL_NOW).is_err());
        assert_eq!(current.snapshot().payload.members.len(), 2);
    }

    #[test]
    fn restoring_blocks_packet_authority_publication_and_mutation_until_resync() {
        let a = identity();
        let b = identity();
        let original = state(&a, &[&a, &b]);
        let mut returning = restored(&original, &a);
        assert_eq!(returning.sync_state(), MembershipSyncState::ResyncRequired);
        let effective = returning.effective_membership_at(WALL_NOW).unwrap();
        assert!(!effective.authorizes_configured_peer(overlay_peer(&a)));
        assert!(
            !effective
                .authorization_for(overlay_peer(&a))
                .allows_peer(overlay_peer(&b), true)
        );
        assert!(matches!(
            returning.sign_mutation_at(
                &a,
                MembershipChange::RemoveMember(b.peer_id.clone()),
                WALL_NOW
            ),
            Err(CheckpointError::NoParticipation)
        ));
        let now = Instant::now();
        let challenge = returning.begin_resync(now, Duration::from_secs(2)).unwrap();
        assert!(matches!(
            returning.make_offer_at(challenge, &a, WALL_NOW),
            Err(CheckpointError::NoParticipation)
        ));
        assert!(matches!(
            returning.finish_resync(now, WALL_NOW),
            Err(CheckpointError::SyncWindowNotElapsed)
        ));
        let result = returning
            .finish_resync(now + Duration::from_secs(2), WALL_NOW)
            .unwrap();
        assert!(!result.observed_remote_offer); // Explicitly NOT global freshness evidence.
        assert_eq!(returning.sync_state(), MembershipSyncState::Participating);
        assert!(
            returning
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .authorizes_configured_peer(overlay_peer(&b))
        );
    }

    #[test]
    fn resync_checks_nonce_transport_scope_signature_mac_and_window() {
        let a = identity();
        let b = identity();
        let c = identity();
        let publisher = state(&a, &[&a, &b]);
        let mut returning = restored(&publisher, &b);
        let now = Instant::now();
        for window in [Duration::ZERO, MAX_RESYNC_WINDOW + Duration::from_secs(1)] {
            assert!(returning.begin_resync(now, window).is_err());
        }
        let challenge = returning.begin_resync(now, Duration::from_secs(1)).unwrap();
        let good = publisher.make_offer_at(challenge, &a, WALL_NOW).unwrap();
        assert!(matches!(
            returning.collect_offer(&good, peer(&c), now),
            Err(CheckpointError::WrongPublisher)
        ));
        let mut bad = good.clone();
        bad.signature = STANDARD.encode([1; 64]);
        assert!(matches!(
            returning.collect_offer(&bad, peer(&a), now),
            Err(CheckpointError::InvalidSignature)
        ));
        bad = good.clone();
        bad.payload.snapshot.mac[0] ^= 1;
        assert!(matches!(
            returning.collect_offer(&bad, peer(&a), now),
            Err(CheckpointError::InvalidMac)
        ));
        bad = good.clone();
        bad.payload.challenge.anchor.network_id[0] ^= 1;
        assert!(matches!(
            returning.collect_offer(&bad, peer(&a), now),
            Err(CheckpointError::WrongChallenge)
        ));
        assert!(matches!(
            returning.collect_offer(&good, peer(&a), now + Duration::from_secs(1)),
            Err(CheckpointError::SyncWindowClosed)
        ));
        returning.begin_resync(now, Duration::from_secs(1)).unwrap();
        assert!(matches!(
            returning.collect_offer(&good, peer(&a), now),
            Err(CheckpointError::WrongChallenge)
        ));
        assert!(
            !returning
                .finish_resync(now + Duration::from_secs(1), WALL_NOW)
                .unwrap()
                .observed_remote_offer
        );
    }

    #[test]
    fn publisher_not_in_offered_roster_cannot_publish_even_with_mac() {
        let a = identity();
        let b = identity();
        let publisher = state(&a, &[&a]);
        let mut returning = restored(&publisher, &a);
        let now = Instant::now();
        let challenge = returning.begin_resync(now, Duration::from_secs(1)).unwrap();
        let mut forged = publisher.make_offer_at(challenge, &a, WALL_NOW).unwrap();
        forged.payload.publisher = SnapshotPublisher::from_identity(&b).unwrap();
        forged.signature = sign(&b, OFFER_DOMAIN, &forged.payload).unwrap();
        assert!(matches!(
            returning.collect_offer(&forged, peer(&b), now),
            Err(CheckpointError::WrongPublisher)
        ));
    }

    #[test]
    fn stale_snapshots_are_rejected_without_rollback_on_the_selected_branch() {
        let a = identity();
        let b = identity();
        let initial = state(&a, &[&a]);
        let mut current = initial.clone();
        mutate(
            &mut current,
            &a,
            MembershipChange::UpsertMember(CheckpointMember::new(&b).unwrap()),
        );
        let now = Instant::now();
        let challenge = current.begin_resync(now, Duration::from_secs(1)).unwrap();
        let stale = initial.make_offer_at(challenge, &a, WALL_NOW).unwrap();
        assert!(matches!(
            current.collect_offer(&stale, peer(&a), now),
            Err(CheckpointError::StaleSnapshot)
        ));
        current
            .finish_resync(now + Duration::from_secs(1), WALL_NOW)
            .unwrap();
        assert_eq!(current.snapshot().payload.authority_revision, 1);
    }

    #[test]
    fn rank_prioritizes_real_changes_then_member_population_then_canonical_digest() {
        let a = identity();
        let b = identity();
        let c = identity();
        let initial = state(&a, &[&a]);
        let mut one = initial.clone();
        let mut two = initial.clone();
        mutate(
            &mut one,
            &a,
            MembershipChange::UpsertMember(CheckpointMember::new(&b).unwrap()),
        );
        mutate(
            &mut two,
            &a,
            MembershipChange::UpsertMember(CheckpointMember::new(&c).unwrap()),
        );
        assert_eq!(
            one.snapshot()
                .payload
                .rank()
                .unwrap()
                .cmp(&two.snapshot().payload.rank().unwrap()),
            one.snapshot()
                .payload
                .boundary()
                .unwrap()
                .digest
                .cmp(&two.snapshot().payload.boundary().unwrap().digest)
        );
        let mut larger_payload = two.snapshot().payload.clone();
        larger_payload
            .members
            .push(CheckpointMember::new(&b).unwrap());
        larger_payload
            .members
            .sort_by(|a, b| a.subject.peer_id.cmp(&b.subject.peer_id));
        let larger = capability().authenticate(larger_payload).unwrap();
        assert!(larger.payload.rank().unwrap() > one.snapshot().payload.rank().unwrap());
        mutate(
            &mut one,
            &a,
            MembershipChange::RemoveMember(b.peer_id.clone()),
        );
        assert!(one.snapshot().payload.rank().unwrap() > larger.payload.rank().unwrap());
    }

    #[test]
    fn all_offer_permutations_choose_identical_authority_and_current_hostname() {
        let a = identity();
        let b = identity();
        let c = identity();
        let d = identity();
        let initial = state(&a, &[&a]);
        let mut branches = [initial.clone(), initial.clone(), initial.clone()];
        for (branch, added) in branches.iter_mut().zip([&b, &c, &d]) {
            mutate(
                branch,
                &a,
                MembershipChange::UpsertMember(CheckpointMember::new(added).unwrap()),
            );
            branch
                .merge_hostname_claims(&[name(branch, &a, 1, "device")])
                .unwrap();
        }
        let expected = branches
            .iter()
            .map(|branch| branch.snapshot().payload.rank().unwrap())
            .max()
            .unwrap();
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut returning = restored(&initial, &a);
            let now = Instant::now();
            let challenge = returning.begin_resync(now, Duration::from_secs(1)).unwrap();
            for index in order {
                let offer = branches[index]
                    .make_offer_at(challenge.clone(), &a, WALL_NOW)
                    .unwrap();
                returning.collect_offer(&offer, peer(&a), now).unwrap();
            }
            returning
                .finish_resync(now + Duration::from_secs(1), WALL_NOW)
                .unwrap();
            assert_eq!(returning.snapshot().payload.rank().unwrap(), expected);
            assert_eq!(returning.hostname_claims()[0].payload.hostname, "device");
        }
    }

    #[test]
    fn higher_fork_can_restore_revoked_member_under_explicit_cooperative_policy() {
        let a = identity();
        let b = identity();
        let c = identity();
        let initial = state(&a, &[&a, &b]);
        let mut a_branch = initial.clone();
        let mut b_branch = CooperativeMembershipState::restore(
            capability(),
            b.peer_id.clone(),
            initial.retained(),
        )
        .unwrap();
        let now = Instant::now();
        b_branch.begin_resync(now, Duration::from_secs(1)).unwrap();
        b_branch
            .finish_resync(now + Duration::from_secs(1), WALL_NOW)
            .unwrap();
        mutate(
            &mut a_branch,
            &a,
            MembershipChange::RemoveMember(b.peer_id.clone()),
        );
        assert!(
            !a_branch
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .authorizes_configured_peer(overlay_peer(&b))
        );
        mutate(
            &mut b_branch,
            &b,
            MembershipChange::UpsertMember(CheckpointMember::new(&c).unwrap()),
        );
        mutate(
            &mut b_branch,
            &b,
            MembershipChange::RemoveMember(c.peer_id.clone()),
        );
        let result = resync(&mut a_branch, &b_branch, &b);
        assert!(result.decisions_may_have_been_discarded);
        assert_eq!(result.added_members, 1);
        assert_eq!(result.removed_members, 0);
        assert!(
            a_branch
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .authorizes_configured_peer(overlay_peer(&b))
        );
        // This is accepted rollback, not a claim that B's branch was globally newer.
    }

    #[test]
    fn creator_departure_preserves_descendants_and_erases_creator_details() {
        let a = identity();
        let b = identity();
        let c = identity();
        let original = state(&a, &[&a, &b, &c]);
        let mut a_branch = original.clone();
        let b_initial = CooperativeMembershipState::restore(
            capability(),
            b.peer_id.clone(),
            original.retained(),
        )
        .unwrap();
        let mut b_branch = b_initial;
        resync(&mut b_branch, &original, &a);
        let resignation = a_branch
            .sign_mutation_at(
                &a,
                MembershipChange::RemoveMember(a.peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        b_branch.apply_mutation_at(&resignation, WALL_NOW).unwrap();
        a_branch.apply_mutation_at(&resignation, WALL_NOW).unwrap();
        assert_eq!(a_branch.sync_state(), MembershipSyncState::Excluded);
        let effective = a_branch.effective_membership_at(WALL_NOW).unwrap();
        assert!(
            !effective
                .authorization_for(overlay_peer(&a))
                .allows_peer(overlay_peer(&b), true)
        );
        assert_eq!(
            b_branch
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .overlay_members()
                .count(),
            2
        );
        mutate(
            &mut b_branch,
            &b,
            MembershipChange::RemoveMember(c.peer_id.clone()),
        );
        let retained = serde_json::to_string(&b_branch.retained()).unwrap();
        assert!(!retained.contains(&a.peer_id));
        assert!(!retained.contains(&SnapshotPublisher::from_identity(&a).unwrap().public_key));
        assert!(!retained.contains("inviter"));
        assert!(!retained.contains("signature"));
    }

    #[test]
    fn final_bound_self_departure_handoff_converges_after_local_exclusion_and_offline_catchup() {
        let a = identity();
        let b = identity();
        let c = identity();
        let mut creator = state(&a, &[&a, &b, &c]);
        let mut survivor = restored(&creator, &b);
        let mut offline = restored(&creator, &c);
        resync(&mut survivor, &creator, &a);
        // Capture the final command and at most MAX_CHECKPOINT_MEMBERS recipients
        // before local removal. The transport owns bounded delivery, not storage.
        let departure = creator
            .sign_mutation_at(
                &a,
                MembershipChange::RemoveMember(a.peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        let recipients = creator
            .snapshot()
            .payload
            .members
            .iter()
            .filter(|member| member.subject.peer_id != a.peer_id)
            .map(|member| member.subject.peer_id.clone())
            .collect::<Vec<_>>();
        assert!(recipients.len() <= MAX_CHECKPOINT_MEMBERS);
        creator.apply_mutation_at(&departure, WALL_NOW).unwrap();
        assert_eq!(creator.sync_state(), MembershipSyncState::Excluded);
        let challenge = SnapshotChallenge {
            anchor: capability().anchor.clone(),
            nonce: nonce(),
        };
        assert!(matches!(
            creator.make_offer_at(challenge, &a, WALL_NOW),
            Err(CheckpointError::NoParticipation)
        ));
        // Local exclusion does not invalidate an already prepared exact-base
        // command for a recipient that still trusts A in its predecessor state.
        survivor.apply_mutation_at(&departure, WALL_NOW).unwrap();
        assert_eq!(survivor.snapshot(), creator.snapshot());
        assert!(
            !survivor
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .authorizes_configured_peer(overlay_peer(&a))
        );
        // C needed no A acknowledgement and does not need the old command/history.
        resync(&mut offline, &survivor, &b);
        assert_eq!(offline.snapshot(), survivor.snapshot());
        assert_eq!(offline.sync_state(), MembershipSyncState::Participating);
        for current in [&creator, &survivor, &offline] {
            assert!(
                !serde_json::to_string(&current.retained())
                    .unwrap()
                    .contains(&a.peer_id)
            );
        }
    }

    #[test]
    fn sealed_projection_denies_absent_static_peers_and_absent_local_authority() {
        let a = identity();
        let b = identity();
        let mut current = state(&a, &[&a, &b]);
        mutate(
            &mut current,
            &a,
            MembershipChange::RemoveMember(b.peer_id.clone()),
        );
        let effective = current.effective_membership_at(WALL_NOW).unwrap();
        assert!(effective.authorizes_configured_peer(overlay_peer(&a)));
        assert!(!effective.authorizes_configured_peer(overlay_peer(&b)));
        assert!(
            !effective
                .authorization_for(overlay_peer(&b))
                .allows_peer(overlay_peer(&a), true)
        );
        for member in effective.overlay_members() {
            assert_eq!(member.effective_inviter_peer, None);
            assert_eq!(member.original_inviter_peer, None);
        }
    }

    #[test]
    fn monotonic_hostname_renames_are_independent_of_snapshot_digest() {
        let a = identity();
        let mut current = state(&a, &[&a]);
        let before = current.snapshot().clone();
        let old = name(&current, &a, 1, "old-name");
        let new = name(&current, &a, 2, "New-Name");
        current
            .merge_hostname_claims(std::slice::from_ref(&new))
            .unwrap();
        current
            .merge_hostname_claims(std::slice::from_ref(&old))
            .unwrap();
        assert_eq!(current.snapshot(), &before);
        assert_eq!(current.hostname_claims(), std::slice::from_ref(&new));
        let mut old_publisher = current.clone();
        old_publisher.names = vec![old];
        resync(&mut current, &old_publisher, &a);
        assert_eq!(current.hostname_claims(), &[new]);
        assert_eq!(current.snapshot(), &before);
    }

    #[test]
    fn equal_authority_offer_can_update_hostname_without_rank_change() {
        let a = identity();
        let b = identity();
        let initial = state(&a, &[&a, &b]);
        let mut publisher = initial.clone();
        publisher
            .merge_hostname_claims(&[name(&publisher, &a, 4, "renamed")])
            .unwrap();
        let mut returning = restored(&initial, &b);
        let result = resync(&mut returning, &publisher, &a);
        assert_eq!(result.previous, result.selected);
        assert!(!result.decisions_may_have_been_discarded);
        assert_eq!(returning.hostname_claims()[0].payload.hostname, "renamed");
    }

    #[test]
    fn hostname_conflicts_converge_cosmetically_and_wrong_identity_is_rejected_atomically() {
        let a = identity();
        let b = identity();
        let mut current = state(&a, &[&a, &b]);
        let good = name(&current, &a, 1, "first");
        current.merge_hostname_claims(&[good]).unwrap();
        let conflict = name(&current, &a, 1, "second");
        current.merge_hostname_claims(&[conflict]).unwrap();
        assert_eq!(current.hostname_claims()[0].payload.hostname, "second");
        let before = current.retained();
        let mut forged = name(&current, &a, 2, "second");
        forged.signature = sign(&b, HOSTNAME_DOMAIN, &forged.payload).unwrap();
        assert!(matches!(
            current.merge_hostname_claims(&[forged]),
            Err(CheckpointError::InvalidSignature)
        ));
        assert_eq!(current.retained(), before);
    }

    #[test]
    fn conflicting_equal_sequence_names_cannot_stall_authority_fork_selection() {
        let a = identity();
        let b = identity();
        let c = identity();
        let initial = state(&a, &[&a, &b]);
        let mut one = initial.clone();
        let mut two = initial.clone();
        one.merge_hostname_claims(&[name(&one, &a, 1, "z-name")])
            .unwrap();
        two.merge_hostname_claims(&[name(&two, &a, 1, "a-name")])
            .unwrap();
        mutate(
            &mut two,
            &a,
            MembershipChange::UpsertMember(CheckpointMember::new(&c).unwrap()),
        );
        for order in [[&one, &two], [&two, &one]] {
            let mut returning = restored(&initial, &b);
            let now = Instant::now();
            let challenge = returning.begin_resync(now, Duration::from_secs(1)).unwrap();
            for publisher in order {
                let offer = publisher
                    .make_offer_at(challenge.clone(), &a, WALL_NOW)
                    .unwrap();
                returning.collect_offer(&offer, peer(&a), now).unwrap();
            }
            returning
                .finish_resync(now + Duration::from_secs(1), WALL_NOW)
                .unwrap();
            assert_eq!(returning.snapshot(), two.snapshot());
            assert_eq!(returning.hostname_claims()[0].payload.hostname, "z-name");
            let newer = name(&returning, &a, 2, "a-new-name");
            returning.merge_hostname_claims(&[newer]).unwrap();
            assert_eq!(
                returning.hostname_claims()[0].payload.hostname,
                "a-new-name"
            );
        }
    }

    #[test]
    fn readmission_has_fresh_incarnation_and_old_names_cannot_return() {
        let a = identity();
        let b = identity();
        let mut current = state(&a, &[&a, &b]);
        let old = name(&current, &b, 100, "previous");
        current
            .merge_hostname_claims(std::slice::from_ref(&old))
            .unwrap();
        mutate(
            &mut current,
            &a,
            MembershipChange::RemoveMember(b.peer_id.clone()),
        );
        assert!(current.hostname_claims().is_empty());
        mutate(
            &mut current,
            &a,
            MembershipChange::UpsertMember(CheckpointMember::new(&b).unwrap()),
        );
        assert!(
            current
                .merge_hostname_claims(std::slice::from_ref(&old))
                .is_err()
        );
        let fresh = name(&current, &b, 0, "current");
        assert_ne!(fresh.payload.incarnation, old.payload.incarnation);
        current.merge_hostname_claims(&[fresh]).unwrap();
        assert_eq!(current.hostname_claims()[0].payload.hostname, "current");
    }

    #[test]
    fn reusing_old_admission_incarnation_is_rejected_without_retaining_a_tombstone() {
        let a = identity();
        let b = identity();
        let mut current = state(&a, &[&a, &b]);
        let old_member = current
            .snapshot()
            .payload
            .member(&b.peer_id)
            .unwrap()
            .clone();
        mutate(
            &mut current,
            &a,
            MembershipChange::RemoveMember(b.peer_id.clone()),
        );
        let mut readmission = current
            .sign_mutation_at(
                &a,
                MembershipChange::UpsertMember(old_member.clone()),
                WALL_NOW,
            )
            .unwrap();
        let MembershipChange::UpsertMember(fresh) = &readmission.payload.change else {
            panic!("expected admission");
        };
        assert_ne!(fresh.incarnation, old_member.incarnation);
        readmission.payload.change = MembershipChange::UpsertMember(old_member);
        readmission.signature = sign(&a, MUTATION_DOMAIN, &readmission.payload).unwrap();
        assert!(current.apply_mutation_at(&readmission, WALL_NOW).is_err());
        assert_eq!(current.snapshot().payload.members.len(), 1);
    }

    #[test]
    fn hostname_claim_from_a_lower_ranked_fork_is_not_frozen_by_authority_digest() {
        let a = identity();
        let b = identity();
        let c = identity();
        let initial = state(&a, &[&a]);
        let mut branches = [initial.clone(), initial];
        for (branch, member) in branches.iter_mut().zip([&b, &c]) {
            mutate(
                branch,
                &a,
                MembershipChange::UpsertMember(CheckpointMember::new(member).unwrap()),
            );
        }
        branches.sort_by_key(|branch| branch.snapshot().payload.rank().unwrap());
        let latest_name = name(&branches[0], &a, 3, "new-device-name");
        let before = branches[1].snapshot().clone();
        // Cosmetic claims have their own authenticated, monotonic merge API:
        // callers need not promote the losing authority snapshot to rename A.
        branches[1].merge_hostname_claims(&[latest_name]).unwrap();
        assert_eq!(branches[1].snapshot(), &before);
        assert_eq!(
            branches[1].hostname_claims()[0].payload.hostname,
            "new-device-name"
        );
    }

    #[test]
    fn thousands_of_remove_admit_cycles_retain_constant_active_only_space() {
        let a = identity();
        let b = identity();
        let mut current = state(&a, &[&a]);
        let mut max_bytes = 0;
        for _ in 0..1_001 {
            mutate(
                &mut current,
                &a,
                MembershipChange::UpsertMember(CheckpointMember::new(&b).unwrap()),
            );
            current
                .merge_hostname_claims(&[name(&current, &b, 1, "temporary")])
                .unwrap();
            mutate(
                &mut current,
                &a,
                MembershipChange::RemoveMember(b.peer_id.clone()),
            );
            assert!(current.hostname_claims().is_empty());
            assert_eq!(current.snapshot().payload.members.len(), 1);
            let encoded = serde_json::to_string(&current.retained()).unwrap();
            max_bytes = max_bytes.max(encoded.len());
            assert!(!encoded.contains(&b.peer_id));
            assert!(!encoded.contains("temporary"));
            assert!(!encoded.contains("inviter"));
        }
        assert_eq!(current.snapshot().payload.authority_revision, 2_002);
        assert!(
            max_bytes < 2_500,
            "active-only bundle unexpectedly grew to {max_bytes}"
        );
        RetainedCheckpointState::decode(
            &serde_json::to_vec(&current.retained()).unwrap(),
            &capability(),
        )
        .unwrap();
    }

    #[test]
    fn limits_and_canonical_fields_are_checked_before_encoded_size() {
        let a = identity();
        let b = identity();
        let initial = state(&a, &[&a, &b]);
        let payload = &initial.snapshot().payload;
        let mut bad = payload.clone();
        bad.members.swap(0, 1);
        assert!(bad.validate().is_err());
        bad = payload.clone();
        bad.members[1] = bad.members[0].clone();
        assert!(bad.validate().is_err());
        bad = payload.clone();
        bad.members[0].subject.public_key = bad.members[1].subject.public_key.clone();
        assert!(bad.validate().is_err());
        bad = payload.clone();
        bad.members = vec![bad.members[0].clone(); MAX_CHECKPOINT_MEMBERS + 1];
        assert!(matches!(
            bad.validate(),
            Err(CheckpointError::Invalid("too many members"))
        ));
        bad = payload.clone();
        bad.members[0].subject.public_key = "x".repeat(MAX_KEY_BYTES + 1);
        assert!(bad.validate().is_err());
        bad = payload.clone();
        bad.members[0].roles.reverse();
        bad.members[0].roles.push(MembershipRole::OverlayMember);
        assert!(bad.validate().is_err());
        bad = payload.clone();
        bad.members[0].incarnation = [0; 32];
        assert!(bad.validate().is_err());
        bad = payload.clone();
        bad.policy.max_active_members = 1;
        assert!(bad.validate().is_err());
        bad = payload.clone();
        bad.parent_digest = Some([1; 32]);
        assert!(bad.validate().is_err());
    }

    #[test]
    fn portable_integer_limits_and_revision_overflow_fail_without_change() {
        let a = identity();
        let b = identity();
        let initial = state(&a, &[&a]);
        let mut payload = initial.snapshot().payload.clone();
        payload.authority_revision = MAX_MEMBERSHIP_RECORD_INTEGER;
        payload.parent_digest = Some([1; 32]);
        let retained = RetainedCheckpointState {
            snapshot: capability().authenticate(payload).unwrap(),
            hostname_claims: vec![],
        };
        let mut current =
            CooperativeMembershipState::restore(capability(), a.peer_id.clone(), retained).unwrap();
        let now = Instant::now();
        current.begin_resync(now, Duration::from_secs(1)).unwrap();
        current
            .finish_resync(now + Duration::from_secs(1), WALL_NOW)
            .unwrap();
        assert!(matches!(
            current.sign_mutation_at(
                &a,
                MembershipChange::UpsertMember(CheckpointMember::new(&b).unwrap()),
                WALL_NOW
            ),
            Err(CheckpointError::RevisionExhausted)
        ));
        let mut bad = current.snapshot().payload.clone();
        bad.authority_revision += 1;
        assert!(bad.validate().is_err());
        bad = current.snapshot().payload.clone();
        bad.members[0].expires_at_unix_seconds = Some(MAX_MEMBERSHIP_RECORD_INTEGER + 1);
        assert!(bad.validate().is_err());
        assert!(
            SignedHostnameClaim::issue(
                capability().anchor.clone(),
                &a,
                current.snapshot().payload.members[0].incarnation,
                MAX_MEMBERSHIP_RECORD_INTEGER + 1,
                "device"
            )
            .is_err()
        );
    }

    #[test]
    fn policy_cannot_implicitly_remove_offline_nodes_and_expiry_is_explicit() {
        let a = identity();
        let b = identity();
        let mut current = state(&a, &[&a, &b]);
        assert!(
            current
                .sign_mutation_at(
                    &a,
                    MembershipChange::SetPolicy(SnapshotPolicy {
                        max_active_members: 1,
                        route_grants_enabled: true
                    }),
                    WALL_NOW
                )
                .is_err()
        );
        let mut limited = current
            .snapshot()
            .payload
            .member(&b.peer_id)
            .unwrap()
            .clone();
        limited.expires_at_unix_seconds = Some(WALL_NOW + 1);
        mutate(&mut current, &a, MembershipChange::UpsertMember(limited));
        assert!(
            current
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .authorizes_configured_peer(overlay_peer(&b))
        );
        assert!(
            !current
                .effective_membership_at(WALL_NOW + 1)
                .unwrap()
                .authorizes_configured_peer(overlay_peer(&b))
        );
        let prune = current
            .sign_mutation_at(&a, MembershipChange::PruneExpired, WALL_NOW + 1)
            .unwrap();
        current.apply_mutation_at(&prune, WALL_NOW + 1).unwrap();
        assert!(current.snapshot().payload.member(&b.peer_id).is_none());
    }

    #[test]
    fn canonical_route_grants_and_policy_projection() {
        let a = identity();
        let b = identity();
        let mut current = state(&a, &[&a, &b]);
        let mut member = current
            .snapshot()
            .payload
            .member(&b.peer_id)
            .unwrap()
            .clone();
        member.roles.push(MembershipRole::RouteAuthority);
        member.route_grants.push(RouteConfig {
            prefix: "10.0.0.1/24".into(),
            metric: 10,
        });
        assert!(member.validate().is_err());
        member.route_grants[0].prefix = "10.0.0.0/24".into();
        mutate(&mut current, &a, MembershipChange::UpsertMember(member));
        assert_eq!(
            current
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .members
                .get(&overlay_peer(&b))
                .unwrap()
                .route_grants
                .len(),
            1
        );
        let mut policy = current.snapshot().payload.policy.clone();
        policy.route_grants_enabled = false;
        mutate(&mut current, &a, MembershipChange::SetPolicy(policy));
        let effective = current.effective_membership_at(WALL_NOW).unwrap();
        let grant = effective.members.get(&overlay_peer(&b)).unwrap();
        assert!(grant.route_grants.is_empty());
        assert!(!grant.roles.contains(&MembershipRole::RouteAuthority));
        for prefix in ["0.0.0.0/0", "::/0", "2001:db8::/32", "10.0.0.1/32"] {
            assert_eq!(
                canonical_route(&RouteConfig {
                    prefix: prefix.into(),
                    metric: 0
                })
                .unwrap(),
                prefix
            );
        }
    }

    fn legacy(
        issuer: &NodeIdentity,
        subject: &NodeIdentity,
        sequence: u64,
        revoked: bool,
        at: u64,
    ) -> SignedMembershipRecord {
        issue_named_membership_record_for_subject_at(
            issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".into(),
                member: MembershipRecordSubject::from_identity(subject).unwrap(),
                membership_epoch: 1,
                sequence,
                revoked,
                roles: if revoked {
                    vec![]
                } else {
                    vec![MembershipRole::OverlayMember]
                },
                route_grants: vec![],
                expires_at_unix_seconds: None,
            },
            (!revoked).then_some("old-host"),
            at,
        )
        .unwrap()
    }

    #[test]
    fn trusted_legacy_migration_preserves_descendants_but_drops_inviter_and_revoked_history() {
        let a = identity();
        let b = identity();
        let c = identity();
        let mut records = vec![
            legacy(&a, &a, 1, false, 1_000),
            legacy(&a, &b, 1, false, 1_000),
            legacy(&b, &c, 1, false, 1_001),
        ];
        records.push(
            issue_membership_record_for_subject_at(
                &a,
                MembershipRecordIssueOptions {
                    network_name: "lab".into(),
                    member: MembershipRecordSubject::from_identity(&b).unwrap(),
                    membership_epoch: 1,
                    sequence: 2,
                    revoked: true,
                    roles: vec![],
                    route_grants: vec![],
                    expires_at_unix_seconds: None,
                },
                WALL_NOW,
            )
            .unwrap(),
        );
        let original = serde_json::to_vec(&records).unwrap();
        let current = CooperativeMembershipState::migrate_trusted_legacy_at(
            capability(),
            a.peer_id.clone(),
            &records,
            "lab",
            WALL_NOW,
        )
        .unwrap();
        assert!(current.snapshot().payload.member(&a.peer_id).is_some());
        assert!(current.snapshot().payload.member(&b.peer_id).is_none());
        assert!(current.snapshot().payload.member(&c.peer_id).is_some());
        let encoded = serde_json::to_string(&current.retained()).unwrap();
        assert!(!encoded.contains(&b.peer_id));
        assert!(!encoded.contains("inviter"));
        assert!(!encoded.contains("old-host"));
        assert!(current.hostname_claims().is_empty());
        assert_eq!(serde_json::to_vec(&records).unwrap(), original);
        assert!(
            CooperativeMembershipState::migrate_trusted_legacy_at(
                capability(),
                a.peer_id.clone(),
                &records,
                "lab",
                1_050
            )
            .is_err()
        );
        assert!(
            CooperativeMembershipState::migrate_trusted_legacy_at(
                capability(),
                a.peer_id.clone(),
                &records,
                "other",
                WALL_NOW
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_route_only_grant_never_becomes_overlay_admission_or_local_authority() {
        let a = identity();
        let b = identity();
        let root = legacy(&a, &a, 1, false, 1_000);
        let route_only = issue_membership_record_for_subject_at(
            &a,
            MembershipRecordIssueOptions {
                network_name: "lab".into(),
                member: MembershipRecordSubject::from_identity(&b).unwrap(),
                membership_epoch: 1,
                sequence: 1,
                revoked: false,
                roles: vec![MembershipRole::RouteAuthority],
                route_grants: vec![RouteConfig {
                    prefix: "10.0.0.0/24".into(),
                    metric: 0,
                }],
                expires_at_unix_seconds: None,
            },
            1_001,
        )
        .unwrap();
        let current = CooperativeMembershipState::migrate_trusted_legacy_at(
            capability(),
            b.peer_id.clone(),
            &[root, route_only],
            "lab",
            WALL_NOW,
        )
        .unwrap();
        assert!(current.snapshot().payload.member(&b.peer_id).is_none());
        assert_eq!(current.sync_state(), MembershipSyncState::Excluded);
        assert!(
            !current
                .effective_membership_at(WALL_NOW)
                .unwrap()
                .authorization_for(overlay_peer(&b))
                .allows_peer(overlay_peer(&a), true)
        );
    }

    #[test]
    fn decoding_is_bounded_and_unknown_new_authority_fields_are_rejected() {
        let a = identity();
        let current = state(&a, &[&a]);
        assert!(
            AuthenticatedSnapshot::decode(&vec![b' '; MAX_CHECKPOINT_BYTES + 1], &capability())
                .is_err()
        );
        assert!(SignedSnapshotOffer::decode(&vec![b' '; MAX_SNAPSHOT_OFFER_BYTES + 1]).is_err());
        assert!(
            RetainedCheckpointState::decode(
                &vec![b' '; MAX_SNAPSHOT_OFFER_BYTES + 1],
                &capability()
            )
            .is_err()
        );
        let mut value = serde_json::to_value(current.retained()).unwrap();
        value["quorum"] = serde_json::json!(1);
        assert!(
            RetainedCheckpointState::decode(&serde_json::to_vec(&value).unwrap(), &capability())
                .is_err()
        );
        let mut value = serde_json::to_value(current.snapshot()).unwrap();
        value["payload"]["revocations"] = serde_json::json!([]);
        assert!(
            AuthenticatedSnapshot::decode(&serde_json::to_vec(&value).unwrap(), &capability())
                .is_err()
        );
    }
}
