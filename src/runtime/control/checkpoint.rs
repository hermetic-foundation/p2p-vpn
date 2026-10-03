//! Bounded, capability-authenticated transfer of frozen cooperative snapshot offers.
//!
//! Transport authentication alone does not authorize fetching a roster. Request proofs
//! bind a separately derived secret key to the transport requester and complete request.
//! Proof-valid excluded/stale members may fetch their exclusion without packet authority.
//! Complete offers are only decoded here: the caller MUST pass them to core `collect_offer`
//! before accepting authority. This protocol supplies no quorum or global-freshness proof.
//!
//! A transfer uses one fixed monotonic deadline. Completion/failure releases its buffers;
//! bounded nonce-only retirement slots prevent reopening it before that deadline. Request
//! transport failures must cancel the transfer; retries use a fresh core resync challenge.

use std::{
    collections::HashMap,
    fmt, io,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use libp2p::{PeerId, StreamProtocol, request_response};
use serde::{Deserialize, Deserializer, Serialize};
use sha2_010::{Digest as _, Sha256};

use crate::{
    identity::NodeIdentity,
    membership::{
        MAX_MEMBERSHIP_RECORD_INTEGER,
        checkpoint::{
            CheckpointBoundary, CheckpointError, CooperativeMembershipState, MAX_CAPABILITY_BYTES,
            MAX_CHECKPOINT_MEMBERS, MAX_RESYNC_WINDOW, MAX_SNAPSHOT_OFFER_BYTES,
            MembershipSyncState, NetworkAnchor, SignedSnapshotOffer, SnapshotChallenge,
            SnapshotRank,
        },
    },
};

pub const CHECKPOINT_PROTOCOL: &str = "/p2p-vpn/checkpoint-sync/1";
pub const CHECKPOINT_TRANSFER_VERSION: u8 = 1;
pub const MAX_PAGE_BYTES: usize = 8 * 1024;
pub const MAX_PAGE_MESSAGE_BYTES: usize = 12 * 1024;
pub const MAX_TRANSFER_SESSIONS: usize = 32;
pub const MAX_TRANSFERS_PER_PEER: usize = 4;
pub const MAX_TRANSFER_BUFFER_BYTES: usize = 16 * 1024 * 1024;
const REQUEST_KEY_DOMAIN: &[u8] = b"p2p-vpn checkpoint transfer request key v1\n";
const REQUEST_PROOF_DOMAIN: &[u8] = b"p2p-vpn checkpoint transfer request proof v1\n";
const OFFER_DIGEST_DOMAIN: &[u8] = b"p2p-vpn checkpoint transfer offer bytes v1\n";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointSyncIndicator {
    ResyncRequired,
    Resyncing,
    Participating,
    Excluded,
}

impl From<MembershipSyncState> for CheckpointSyncIndicator {
    fn from(state: MembershipSyncState) -> Self {
        match state {
            MembershipSyncState::ResyncRequired => Self::ResyncRequired,
            MembershipSyncState::Resyncing => Self::Resyncing,
            MembershipSyncState::Participating => Self::Participating,
            MembershipSyncState::Excluded => Self::Excluded,
        }
    }
}

/// An authenticated peer's advertisement, never proof of admission or freshness.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointCapabilities {
    pub version: u8,
    pub anchor: NetworkAnchor,
    pub boundary: CheckpointBoundary,
    pub active_member_count: u16,
    pub sync: CheckpointSyncIndicator,
}

impl CheckpointCapabilities {
    pub fn from_state(state: &CooperativeMembershipState) -> Result<Self, CheckpointSyncError> {
        let snapshot = &state.snapshot().payload;
        let descriptor = Self {
            version: CHECKPOINT_TRANSFER_VERSION,
            anchor: snapshot.anchor.clone(),
            boundary: snapshot.boundary()?,
            active_member_count: u16::try_from(snapshot.members.len())
                .map_err(|_| CheckpointSyncError::Invalid("member count"))?,
            sync: state.sync_state().into(),
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn validate(&self) -> Result<(), CheckpointSyncError> {
        validate_anchor(&self.anchor)?;
        if self.version != CHECKPOINT_TRANSFER_VERSION
            || self.boundary.authority_revision > MAX_MEMBERSHIP_RECORD_INTEGER
            || self.boundary.digest == [0; 32]
            || usize::from(self.active_member_count) > MAX_CHECKPOINT_MEMBERS
        {
            return Err(CheckpointSyncError::Invalid("checkpoint descriptor"));
        }
        Ok(())
    }

    pub fn validate_for(&self, expected: &NetworkAnchor) -> Result<(), CheckpointSyncError> {
        self.validate()?;
        if &self.anchor != expected {
            return Err(CheckpointSyncError::WrongScope);
        }
        Ok(())
    }

    #[must_use]
    pub fn rank(&self) -> SnapshotRank {
        SnapshotRank {
            authority_revision: self.boundary.authority_revision,
            active_member_count: usize::from(self.active_member_count),
            digest: self.boundary.digest,
        }
    }
}

pub(super) fn deserialize_capabilities<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<CheckpointCapabilities>, D::Error> {
    let descriptor = Option::<CheckpointCapabilities>::deserialize(deserializer)?;
    if let Some(value) = &descriptor {
        value.validate().map_err(serde::de::Error::custom)?;
    }
    Ok(descriptor)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointPageRequest {
    pub version: u8,
    pub challenge: SnapshotChallenge,
    pub offer_digest: Option<[u8; 32]>,
    pub cursor: u32,
    pub proof: [u8; 32],
}

impl CheckpointPageRequest {
    fn validate(&self) -> Result<(), CheckpointSyncError> {
        validate_challenge(&self.challenge)?;
        if self.version != CHECKPOINT_TRANSFER_VERSION
            || self.cursor as usize >= MAX_SNAPSHOT_OFFER_BYTES
            || !(self.cursor as usize).is_multiple_of(MAX_PAGE_BYTES)
            || (self.cursor != 0 && self.offer_digest.is_none())
            || self.offer_digest == Some([0; 32])
        {
            return Err(CheckpointSyncError::Invalid("page request"));
        }
        Ok(())
    }

    fn signed(
        challenge: SnapshotChallenge,
        offer_digest: Option<[u8; 32]>,
        cursor: u32,
        requester: PeerId,
        secret: &[u8],
    ) -> Result<Self, CheckpointSyncError> {
        let mut request = Self {
            version: CHECKPOINT_TRANSFER_VERSION,
            challenge,
            offer_digest,
            cursor,
            proof: [0; 32],
        };
        request.validate()?;
        request.proof = request_mac(&request, requester, secret)?
            .finalize()
            .into_bytes()
            .into();
        Ok(request)
    }

    fn authenticate(&self, requester: PeerId, secret: &[u8]) -> Result<(), CheckpointSyncError> {
        self.validate()?;
        request_mac(self, requester, secret)?
            .verify_slice(&self.proof)
            .map_err(|_| CheckpointSyncError::Unauthorized)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointPage {
    pub version: u8,
    pub challenge: SnapshotChallenge,
    pub offer_digest: [u8; 32],
    pub cursor: u32,
    pub total_bytes: u32,
    #[serde(with = "page_bytes")]
    pub bytes: Vec<u8>,
}

impl CheckpointPage {
    fn validate(&self) -> Result<(), CheckpointSyncError> {
        validate_challenge(&self.challenge)?;
        let total = self.total_bytes as usize;
        let cursor = self.cursor as usize;
        if self.version != CHECKPOINT_TRANSFER_VERSION
            || self.offer_digest == [0; 32]
            || total == 0
            || total > MAX_SNAPSHOT_OFFER_BYTES
            || cursor >= total
            || !cursor.is_multiple_of(MAX_PAGE_BYTES)
            || self.bytes.len() != (total - cursor).min(MAX_PAGE_BYTES)
        {
            return Err(CheckpointSyncError::Invalid("page shape"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CheckpointRejection {
    Unauthorized,
    Busy,
    Expired,
    InvalidTransfer,
    UnsupportedVersion,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRejected {
    pub version: u8,
    pub challenge: SnapshotChallenge,
    pub cursor: u32,
    pub reason: CheckpointRejection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub enum CheckpointPageResponse {
    Page(CheckpointPage),
    Rejected(CheckpointRejected),
}

impl CheckpointPageResponse {
    #[must_use]
    pub fn rejected(request: &CheckpointPageRequest, reason: CheckpointRejection) -> Self {
        Self::Rejected(CheckpointRejected {
            version: CHECKPOINT_TRANSFER_VERSION,
            challenge: request.challenge.clone(),
            cursor: request.cursor,
            reason,
        })
    }

    fn challenge(&self) -> &SnapshotChallenge {
        match self {
            Self::Page(page) => &page.challenge,
            Self::Rejected(value) => &value.challenge,
        }
    }

    fn validate(&self) -> Result<(), CheckpointSyncError> {
        match self {
            Self::Page(page) => page.validate(),
            Self::Rejected(value) => {
                validate_challenge(&value.challenge)?;
                if value.version != CHECKPOINT_TRANSFER_VERSION {
                    return Err(CheckpointSyncError::Invalid("rejection version"));
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CheckpointSyncLimits {
    /// Includes nonce-only retirement slots, reserved until their original deadline.
    pub max_sessions: usize,
    pub max_sessions_per_peer: usize,
    pub max_buffered_bytes: usize,
    pub session_timeout: Duration,
}

impl Default for CheckpointSyncLimits {
    fn default() -> Self {
        Self {
            max_sessions: 16,
            max_sessions_per_peer: 4,
            max_buffered_bytes: MAX_TRANSFER_BUFFER_BYTES,
            session_timeout: MAX_RESYNC_WINDOW,
        }
    }
}

impl CheckpointSyncLimits {
    fn validate(self) -> Result<Self, CheckpointSyncError> {
        if self.max_sessions == 0
            || self.max_sessions > MAX_TRANSFER_SESSIONS
            || self.max_sessions_per_peer == 0
            || self.max_sessions_per_peer > MAX_TRANSFERS_PER_PEER
            || self.max_buffered_bytes == 0
            || self.max_buffered_bytes > MAX_TRANSFER_BUFFER_BYTES
            || self.session_timeout.is_zero()
            || self.session_timeout > MAX_RESYNC_WINDOW
        {
            return Err(CheckpointSyncError::Invalid("transfer limits"));
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CheckpointSyncStats {
    pub publisher_sessions: usize,
    pub assembler_sessions: usize,
    pub retired_sessions: usize,
    /// Charges full assembler reservation, not only bytes already received.
    pub buffered_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Direction {
    Publish,
    Assemble,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SessionKey {
    peer: PeerId,
    nonce: [u8; 32],
    direction: Direction,
}

struct PublishedOffer {
    challenge: SnapshotChallenge,
    bytes: Vec<u8>,
    digest: [u8; 32],
    cursor: usize,
    deadline: Instant,
}

struct Assembly {
    challenge: SnapshotChallenge,
    bytes: Vec<u8>,
    digest: Option<[u8; 32]>,
    total: usize,
    deadline: Instant,
}

/// Does not retain secret credentials, historical offers, or per-device decisions.
pub struct CheckpointSync {
    local_peer: PeerId,
    anchor: NetworkAnchor,
    limits: CheckpointSyncLimits,
    publishers: HashMap<SessionKey, PublishedOffer>,
    assemblers: HashMap<SessionKey, Assembly>,
    retired: HashMap<SessionKey, Instant>,
    buffered_bytes: usize,
}

impl fmt::Debug for CheckpointSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckpointSync")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

pub enum CheckpointProgress {
    More(CheckpointPageRequest),
    /// Parsed/scoped only: core `collect_offer` still authenticates/authorizes it.
    Complete(Box<SignedSnapshotOffer>),
}

impl CheckpointSync {
    pub fn new(
        local_peer: PeerId,
        anchor: NetworkAnchor,
        limits: CheckpointSyncLimits,
    ) -> Result<Self, CheckpointSyncError> {
        validate_anchor(&anchor)?;
        Ok(Self {
            local_peer,
            anchor,
            limits: limits.validate()?,
            publishers: HashMap::new(),
            assemblers: HashMap::new(),
            retired: HashMap::new(),
            buffered_bytes: 0,
        })
    }

    pub fn start_request(
        &mut self,
        peer: PeerId,
        challenge: SnapshotChallenge,
        secret: &[u8],
        now: Instant,
        deadline: Instant,
    ) -> Result<CheckpointPageRequest, CheckpointSyncError> {
        self.cleanup(now);
        self.validate_scope(&challenge)?;
        let remaining = deadline
            .checked_duration_since(now)
            .ok_or(CheckpointSyncError::Expired)?;
        if remaining.is_zero() || remaining > self.limits.session_timeout {
            return Err(CheckpointSyncError::Invalid("transfer deadline"));
        }
        let key = SessionKey {
            peer,
            nonce: challenge.nonce,
            direction: Direction::Assemble,
        };
        self.reserve_slot(key)?;
        let request =
            CheckpointPageRequest::signed(challenge.clone(), None, 0, self.local_peer, secret)?;
        self.assemblers.insert(
            key,
            Assembly {
                challenge,
                bytes: Vec::new(),
                digest: None,
                total: 0,
                deadline,
            },
        );
        Ok(request)
    }

    /// Caller supplies the authenticated event peer and separately protected secret.
    /// A valid proof suffices for catch-up; current roster admission is NOT required.
    #[allow(clippy::too_many_arguments)] // Credentials and both clocks stay caller-owned.
    pub fn respond_at(
        &mut self,
        peer: PeerId,
        request: &CheckpointPageRequest,
        state: &CooperativeMembershipState,
        identity: &NodeIdentity,
        secret: &[u8],
        now: Instant,
        wall_now: u64,
    ) -> Result<CheckpointPageResponse, CheckpointSyncError> {
        self.cleanup(now);
        self.validate_scope(&request.challenge)?;
        // Authenticate before touching a peer's existing publication, so spoofed
        // proofs cannot cancel legitimate transfers from that authenticated peer.
        request.authenticate(peer, secret)?;
        let key = SessionKey {
            peer,
            nonce: request.challenge.nonce,
            direction: Direction::Publish,
        };
        let result = self.publish_page(key, request, state, identity, now, wall_now);
        if result.is_err() {
            self.retire(key);
        }
        result
    }

    fn publish_page(
        &mut self,
        key: SessionKey,
        request: &CheckpointPageRequest,
        state: &CooperativeMembershipState,
        identity: &NodeIdentity,
        now: Instant,
        wall_now: u64,
    ) -> Result<CheckpointPageResponse, CheckpointSyncError> {
        if !self.publishers.contains_key(&key) {
            self.reserve_slot(key)?;
            if request.cursor != 0 || request.offer_digest.is_some() {
                return Err(CheckpointSyncError::CursorMismatch);
            }
            if state.snapshot().payload.anchor != self.anchor
                || identity.peer_id != self.local_peer.to_string()
            {
                return Err(CheckpointSyncError::WrongScope);
            }
            let offer = state.make_offer_at(request.challenge.clone(), identity, wall_now)?;
            let bytes = serde_json::to_vec(&offer)?;
            if bytes.is_empty() || bytes.len() > MAX_SNAPSHOT_OFFER_BYTES {
                return Err(CheckpointSyncError::Invalid("offer size"));
            }
            self.reserve_bytes(bytes.len())?;
            let deadline = now
                .checked_add(self.limits.session_timeout)
                .ok_or(CheckpointSyncError::Invalid("deadline overflow"))?;
            let digest = offer_digest(&bytes);
            self.buffered_bytes += bytes.len();
            self.publishers.insert(
                key,
                PublishedOffer {
                    challenge: request.challenge.clone(),
                    bytes,
                    digest,
                    cursor: 0,
                    deadline,
                },
            );
        }
        let offer = self.publishers.get_mut(&key).expect("publication created");
        if offer.challenge != request.challenge {
            return Err(CheckpointSyncError::WrongChallenge);
        }
        if offer.cursor != request.cursor as usize {
            return Err(CheckpointSyncError::CursorMismatch);
        }
        if request.cursor != 0 && request.offer_digest != Some(offer.digest) {
            return Err(CheckpointSyncError::DigestMismatch);
        }
        let end = (offer.cursor + MAX_PAGE_BYTES).min(offer.bytes.len());
        let page = CheckpointPage {
            version: CHECKPOINT_TRANSFER_VERSION,
            challenge: offer.challenge.clone(),
            offer_digest: offer.digest,
            cursor: u32::try_from(offer.cursor).expect("bounded offer"),
            total_bytes: u32::try_from(offer.bytes.len()).expect("bounded offer"),
            bytes: offer.bytes[offer.cursor..end].to_vec(),
        };
        offer.cursor = end;
        if end == offer.bytes.len() {
            self.retire(key);
        }
        Ok(CheckpointPageResponse::Page(page))
    }

    /// Complete decoded offers must be passed to core `collect_offer` with this peer.
    pub fn accept_response(
        &mut self,
        peer: PeerId,
        request: &CheckpointPageRequest,
        response: CheckpointPageResponse,
        secret: &[u8],
        now: Instant,
    ) -> Result<CheckpointProgress, CheckpointSyncError> {
        self.cleanup(now);
        let key = SessionKey {
            peer,
            nonce: request.challenge.nonce,
            direction: Direction::Assemble,
        };
        let result = (|| {
            self.validate_scope(&request.challenge)?;
            request.authenticate(self.local_peer, secret)?;
            self.assemble_page(key, request, response, secret)
        })();
        if result.is_err() {
            self.retire(key);
        }
        result
    }

    fn assemble_page(
        &mut self,
        key: SessionKey,
        request: &CheckpointPageRequest,
        response: CheckpointPageResponse,
        secret: &[u8],
    ) -> Result<CheckpointProgress, CheckpointSyncError> {
        if self.retired.contains_key(&key) {
            return Err(CheckpointSyncError::Replay);
        }
        self.validate_scope(response.challenge())?;
        response.validate()?;
        let assembly = self
            .assemblers
            .get(&key)
            .ok_or(CheckpointSyncError::UnknownSession)?;
        if assembly.challenge != *response.challenge() {
            return Err(CheckpointSyncError::WrongChallenge);
        }
        if request.cursor as usize != assembly.bytes.len()
            || request.offer_digest != assembly.digest
        {
            return Err(CheckpointSyncError::CursorMismatch);
        }
        if let CheckpointPageResponse::Rejected(rejected) = &response
            && rejected.cursor != request.cursor
        {
            return Err(CheckpointSyncError::CursorMismatch);
        }
        let CheckpointPageResponse::Page(page) = response else {
            return Err(CheckpointSyncError::Rejected);
        };
        if assembly.bytes.len() != page.cursor as usize {
            return Err(CheckpointSyncError::CursorMismatch);
        }
        if assembly
            .digest
            .is_some_and(|digest| digest != page.offer_digest)
            || (assembly.total != 0 && assembly.total != page.total_bytes as usize)
        {
            return Err(CheckpointSyncError::DigestMismatch);
        }
        let total = page.total_bytes as usize;
        if assembly.digest.is_none() {
            self.reserve_bytes(total)?;
        }
        let assembly = self.assemblers.get_mut(&key).expect("checked assembly");
        if assembly.digest.is_none() {
            assembly
                .bytes
                .try_reserve_exact(total)
                .map_err(|_| CheckpointSyncError::ResourceLimit)?;
            assembly.total = total;
            assembly.digest = Some(page.offer_digest);
            self.buffered_bytes += total;
        }
        assembly.bytes.extend_from_slice(&page.bytes);
        if assembly.bytes.len() < total {
            return Ok(CheckpointProgress::More(CheckpointPageRequest::signed(
                assembly.challenge.clone(),
                assembly.digest,
                u32::try_from(assembly.bytes.len()).expect("bounded offer"),
                self.local_peer,
                secret,
            )?));
        }
        if offer_digest(&assembly.bytes) != page.offer_digest {
            return Err(CheckpointSyncError::DigestMismatch);
        }
        let offer = SignedSnapshotOffer::decode(&assembly.bytes)?;
        if offer.payload.challenge != assembly.challenge {
            return Err(CheckpointSyncError::WrongChallenge);
        }
        if offer.payload.publisher.peer_id != key.peer.to_string() {
            return Err(CheckpointSyncError::WrongPeer);
        }
        self.retire(key);
        Ok(CheckpointProgress::Complete(Box::new(offer)))
    }

    /// Cancel after transport failure/finish. No buffers survive cancellation.
    pub fn cancel(&mut self, peer: PeerId, challenge: &SnapshotChallenge, now: Instant) {
        self.cleanup(now);
        if challenge.anchor != self.anchor {
            return;
        }
        for direction in [Direction::Publish, Direction::Assemble] {
            self.retire(SessionKey {
                peer,
                nonce: challenge.nonce,
                direction,
            });
        }
    }

    /// Drop all expired sessions and retirement slots using monotonic time.
    pub fn cleanup(&mut self, now: Instant) {
        self.publishers.retain(|_, offer| {
            if now >= offer.deadline {
                self.buffered_bytes -= offer.bytes.len();
                false
            } else {
                true
            }
        });
        self.assemblers.retain(|_, assembly| {
            if now >= assembly.deadline {
                self.buffered_bytes -= assembly.total;
                false
            } else {
                true
            }
        });
        self.retired.retain(|_, deadline| now < *deadline);
    }

    #[must_use]
    pub fn stats(&self) -> CheckpointSyncStats {
        CheckpointSyncStats {
            publisher_sessions: self.publishers.len(),
            assembler_sessions: self.assemblers.len(),
            retired_sessions: self.retired.len(),
            buffered_bytes: self.buffered_bytes,
        }
    }

    fn validate_scope(&self, challenge: &SnapshotChallenge) -> Result<(), CheckpointSyncError> {
        validate_challenge(challenge)?;
        if challenge.anchor != self.anchor {
            return Err(CheckpointSyncError::WrongScope);
        }
        Ok(())
    }

    fn reserve_slot(&self, key: SessionKey) -> Result<(), CheckpointSyncError> {
        if self.retired.contains_key(&key)
            || self.publishers.contains_key(&key)
            || self.assemblers.contains_key(&key)
        {
            return Err(CheckpointSyncError::Replay);
        }
        let count = self.publishers.len() + self.assemblers.len() + self.retired.len();
        let peer_count = self
            .publishers
            .keys()
            .chain(self.assemblers.keys())
            .chain(self.retired.keys())
            .filter(|entry| entry.peer == key.peer)
            .count();
        if count >= self.limits.max_sessions || peer_count >= self.limits.max_sessions_per_peer {
            return Err(CheckpointSyncError::ResourceLimit);
        }
        Ok(())
    }

    fn reserve_bytes(&self, additional: usize) -> Result<(), CheckpointSyncError> {
        if self
            .buffered_bytes
            .checked_add(additional)
            .is_none_or(|sum| sum > self.limits.max_buffered_bytes)
        {
            return Err(CheckpointSyncError::ResourceLimit);
        }
        Ok(())
    }

    fn retire(&mut self, key: SessionKey) {
        let deadline = match key.direction {
            Direction::Publish => self.publishers.remove(&key).map(|offer| {
                self.buffered_bytes -= offer.bytes.len();
                offer.deadline
            }),
            Direction::Assemble => self.assemblers.remove(&key).map(|assembly| {
                self.buffered_bytes -= assembly.total;
                assembly.deadline
            }),
        };
        if let Some(deadline) = deadline {
            self.retired.insert(key, deadline);
        }
    }
}

fn validate_anchor(anchor: &NetworkAnchor) -> Result<(), CheckpointSyncError> {
    if *anchor != NetworkAnchor::new(anchor.network_id)? {
        return Err(CheckpointSyncError::WrongScope);
    }
    Ok(())
}

fn validate_challenge(challenge: &SnapshotChallenge) -> Result<(), CheckpointSyncError> {
    validate_anchor(&challenge.anchor)?;
    if challenge.nonce == [0; 32] {
        return Err(CheckpointSyncError::Invalid("empty nonce"));
    }
    Ok(())
}

fn request_mac(
    request: &CheckpointPageRequest,
    peer: PeerId,
    secret: &[u8],
) -> Result<Hmac<Sha256>, CheckpointSyncError> {
    if !(32..=MAX_CAPABILITY_BYTES).contains(&secret.len()) {
        return Err(CheckpointSyncError::Invalid("capability size"));
    }
    let anchor = &request.challenge.anchor;
    let mut scope = Vec::with_capacity(35);
    scope.push(anchor.version);
    scope.extend_from_slice(&anchor.network_id);
    scope.extend_from_slice(&[anchor.policy_version, anchor.rank_version]);
    let mut key = [0; 32];
    Hkdf::<Sha256>::new(Some(&scope), secret)
        .expand(REQUEST_KEY_DOMAIN, &mut key)
        .map_err(|_| CheckpointSyncError::Invalid("request key derivation"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key)
        .map_err(|_| CheckpointSyncError::Invalid("request MAC key"))?;
    mac.update(REQUEST_PROOF_DOMAIN);
    mac.update(&[request.version]);
    mac.update(&scope);
    mac.update(&request.challenge.nonce);
    let peer = peer.to_bytes();
    mac.update(
        &u16::try_from(peer.len())
            .expect("bounded peer identity")
            .to_be_bytes(),
    );
    mac.update(&peer);
    mac.update(&request.cursor.to_be_bytes());
    mac.update(&[u8::from(request.offer_digest.is_some())]);
    if let Some(digest) = request.offer_digest {
        mac.update(&digest);
    }
    Ok(mac)
}

fn offer_digest(bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(OFFER_DIGEST_DOMAIN);
    digest.update(bytes);
    digest.finalize().into()
}

mod page_bytes {
    use super::{MAX_PAGE_BYTES, STANDARD};
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        if bytes.len() > MAX_PAGE_BYTES {
            return Err(serde::ser::Error::custom("page bytes exceed limit"));
        }
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        if encoded.len() > MAX_PAGE_BYTES.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom("encoded page exceeds limit"));
        }
        let bytes = STANDARD.decode(encoded).map_err(serde::de::Error::custom)?;
        if bytes.len() > MAX_PAGE_BYTES {
            return Err(serde::de::Error::custom("page bytes exceed limit"));
        }
        Ok(bytes)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CheckpointCodec;

#[async_trait]
impl request_response::Codec for CheckpointCodec {
    type Protocol = StreamProtocol;
    type Request = CheckpointPageRequest;
    type Response = CheckpointPageResponse;

    async fn read_request<T: AsyncRead + Unpin + Send>(
        &mut self,
        protocol: &StreamProtocol,
        io: &mut T,
    ) -> io::Result<Self::Request> {
        require_protocol(protocol)?;
        let request: Self::Request = read_message(io).await?;
        request.validate().map_err(invalid_data)?;
        Ok(request)
    }
    async fn read_response<T: AsyncRead + Unpin + Send>(
        &mut self,
        protocol: &StreamProtocol,
        io: &mut T,
    ) -> io::Result<Self::Response> {
        require_protocol(protocol)?;
        let response: Self::Response = read_message(io).await?;
        response.validate().map_err(invalid_data)?;
        Ok(response)
    }
    async fn write_request<T: AsyncWrite + Unpin + Send>(
        &mut self,
        protocol: &StreamProtocol,
        io: &mut T,
        request: Self::Request,
    ) -> io::Result<()> {
        require_protocol(protocol)?;
        request.validate().map_err(invalid_data)?;
        write_message(io, &request).await
    }
    async fn write_response<T: AsyncWrite + Unpin + Send>(
        &mut self,
        protocol: &StreamProtocol,
        io: &mut T,
        response: Self::Response,
    ) -> io::Result<()> {
        require_protocol(protocol)?;
        response.validate().map_err(invalid_data)?;
        write_message(io, &response).await
    }
}

#[must_use]
pub fn behaviour(max_concurrent_streams: usize) -> request_response::Behaviour<CheckpointCodec> {
    request_response::Behaviour::with_codec(
        CheckpointCodec,
        [(
            StreamProtocol::new(CHECKPOINT_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_secs(10))
            .with_max_concurrent_streams(max_concurrent_streams.clamp(1, MAX_TRANSFER_SESSIONS)),
    )
}

fn require_protocol(protocol: &StreamProtocol) -> io::Result<()> {
    if protocol.as_ref() != CHECKPOINT_PROTOCOL {
        return Err(invalid_data("unsupported checkpoint protocol"));
    }
    Ok(())
}

async fn read_message<T: AsyncRead + Unpin + Send, M: for<'de> Deserialize<'de>>(
    io: &mut T,
) -> io::Result<M> {
    let mut header = [0; 2];
    io.read_exact(&mut header).await?;
    let length = usize::from(u16::from_be_bytes(header));
    if length == 0 || length > MAX_PAGE_MESSAGE_BYTES {
        return Err(invalid_data("checkpoint message size"));
    }
    let mut bytes = vec![0; length];
    io.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(invalid_data)
}

async fn write_message<T: AsyncWrite + Unpin + Send, M: Serialize>(
    io: &mut T,
    message: &M,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(message).map_err(invalid_data)?;
    if bytes.is_empty() || bytes.len() > MAX_PAGE_MESSAGE_BYTES {
        return Err(invalid_data("checkpoint message size"));
    }
    let length = u16::try_from(bytes.len()).map_err(invalid_data)?;
    io.write_all(&length.to_be_bytes()).await?;
    io.write_all(&bytes).await?;
    io.close().await
}

fn invalid_data(error: impl fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[derive(Debug)]
pub enum CheckpointSyncError {
    Invalid(&'static str),
    WrongScope,
    Unauthorized,
    WrongPeer,
    WrongChallenge,
    Replay,
    CursorMismatch,
    DigestMismatch,
    Expired,
    UnknownSession,
    ResourceLimit,
    Rejected,
    Core(CheckpointError),
    Encoding(serde_json::Error),
}

impl fmt::Display for CheckpointSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid checkpoint transfer: {reason}"),
            Self::Core(_) => f.write_str("checkpoint offer rejected by core"),
            Self::Encoding(_) => f.write_str("invalid checkpoint transfer encoding"),
            other => write!(f, "checkpoint transfer {other:?}"),
        }
    }
}
impl std::error::Error for CheckpointSyncError {}
impl From<CheckpointError> for CheckpointSyncError {
    fn from(error: CheckpointError) -> Self {
        Self::Core(error)
    }
}
impl From<serde_json::Error> for CheckpointSyncError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encoding(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::checkpoint::{
        CheckpointMember, MembershipChange, NetworkCapability, SnapshotPolicy,
    };
    use base64::Engine as _;
    use futures::{StreamExt as _, io::Cursor};
    use libp2p::{Swarm, SwarmBuilder, noise, swarm::SwarmEvent, tcp, yamux};

    const SECRET: [u8; 32] = [9; 32];
    const WALL_NOW: u64 = 1_000;

    fn anchor() -> NetworkAnchor {
        NetworkAnchor::new([7; 32]).unwrap()
    }
    fn peer(identity: &NodeIdentity) -> PeerId {
        identity.peer_id.parse().unwrap()
    }
    fn capability() -> NetworkCapability {
        NetworkCapability::from_secret(anchor(), Some(&SECRET)).unwrap()
    }
    fn identities() -> Vec<NodeIdentity> {
        (0..24)
            .map(|_| NodeIdentity::generate_ed25519().unwrap())
            .collect()
    }
    fn state(ids: &[NodeIdentity]) -> CooperativeMembershipState {
        CooperativeMembershipState::bootstrap_at(
            capability(),
            ids[0].peer_id.clone(),
            ids.iter()
                .map(|id| CheckpointMember::new(id).unwrap())
                .collect(),
            SnapshotPolicy::default(),
            WALL_NOW,
        )
        .unwrap()
    }
    fn challenge(nonce: u8) -> SnapshotChallenge {
        SnapshotChallenge {
            anchor: anchor(),
            nonce: [nonce; 32],
        }
    }
    fn sync(id: &NodeIdentity) -> CheckpointSync {
        CheckpointSync::new(peer(id), anchor(), CheckpointSyncLimits::default()).unwrap()
    }
    fn request(client: &mut CheckpointSync, remote: PeerId, now: Instant) -> CheckpointPageRequest {
        client
            .start_request(
                remote,
                challenge(1),
                &SECRET,
                now,
                now + Duration::from_secs(30),
            )
            .unwrap()
    }
    fn page(response: &CheckpointPageResponse) -> &CheckpointPage {
        let CheckpointPageResponse::Page(value) = response else {
            panic!("expected page")
        };
        value
    }
    fn mutate(
        current: &mut CooperativeMembershipState,
        id: &NodeIdentity,
        change: MembershipChange,
    ) {
        let mutation = current.sign_mutation_at(id, change, WALL_NOW).unwrap();
        current.apply_mutation_at(&mutation, WALL_NOW).unwrap();
    }

    #[test]
    fn request_proofs_bind_requester_challenge_scope_cursor_and_digest() {
        let ids = identities();
        let good =
            CheckpointPageRequest::signed(challenge(1), None, 0, peer(&ids[1]), &SECRET).unwrap();
        good.authenticate(peer(&ids[1]), &SECRET).unwrap();
        assert!(matches!(
            good.authenticate(peer(&ids[2]), &SECRET),
            Err(CheckpointSyncError::Unauthorized)
        ));
        assert!(matches!(
            good.authenticate(peer(&ids[1]), &[8; 32]),
            Err(CheckpointSyncError::Unauthorized)
        ));
        for variant in 0..4 {
            let mut bad = good.clone();
            match variant {
                0 => bad.challenge.nonce = [2; 32],
                1 => bad.challenge.anchor = NetworkAnchor::new([6; 32]).unwrap(),
                2 => {
                    bad.cursor = u32::try_from(MAX_PAGE_BYTES).unwrap();
                    bad.offer_digest = Some([1; 32]);
                }
                _ => bad.offer_digest = Some([1; 32]),
            }
            assert!(matches!(
                bad.authenticate(peer(&ids[1]), &SECRET),
                Err(CheckpointSyncError::Unauthorized)
            ));
        }
        for size in [0, 31, MAX_CAPABILITY_BYTES + 1] {
            assert!(
                CheckpointPageRequest::signed(challenge(1), None, 0, peer(&ids[1]), &vec![1; size])
                    .is_err()
            );
        }
        for size in [32, MAX_CAPABILITY_BYTES] {
            assert!(
                CheckpointPageRequest::signed(challenge(1), None, 0, peer(&ids[1]), &vec![1; size])
                    .is_ok()
            );
        }
    }

    #[test]
    fn unauthenticated_requests_cannot_fetch_or_cancel_frozen_rosters() {
        let ids = identities();
        let current = state(&ids);
        let now = Instant::now();
        let mut client = sync(&ids[1]);
        let mut server = sync(&ids[0]);
        let first = request(&mut client, peer(&ids[0]), now);
        let mut bad = first.clone();
        bad.proof[0] ^= 1;
        assert!(matches!(
            server.respond_at(
                peer(&ids[1]),
                &bad,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW
            ),
            Err(CheckpointSyncError::Unauthorized)
        ));
        assert_eq!(server.stats(), CheckpointSyncStats::default());
        let reply = server
            .respond_at(
                peer(&ids[1]),
                &first,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW,
            )
            .unwrap();
        assert!(page(&reply).total_bytes as usize > MAX_PAGE_BYTES);
        let before = server.stats();
        assert!(
            server
                .respond_at(
                    peer(&ids[1]),
                    &bad,
                    &current,
                    &ids[0],
                    &SECRET,
                    now,
                    WALL_NOW
                )
                .is_err()
        );
        assert_eq!(server.stats(), before);
        assert!(
            server
                .respond_at(
                    peer(&ids[2]),
                    &first,
                    &current,
                    &ids[0],
                    &SECRET,
                    now,
                    WALL_NOW
                )
                .is_err()
        );
        assert_eq!(server.stats(), before);
    }

    #[test]
    fn publication_is_frozen_and_completion_releases_all_buffers() {
        let ids = identities();
        let mut current = state(&ids);
        let original = current.snapshot().clone();
        let now = Instant::now();
        let mut client = sync(&ids[1]);
        let mut server = sync(&ids[0]);
        let mut next = request(&mut client, peer(&ids[0]), now);
        let mut pages = 0;
        let complete = loop {
            let response = server
                .respond_at(
                    peer(&ids[1]),
                    &next,
                    &current,
                    &ids[0],
                    &SECRET,
                    now,
                    WALL_NOW,
                )
                .unwrap();
            pages += 1;
            if pages == 1 {
                mutate(
                    &mut current,
                    &ids[0],
                    MembershipChange::RemoveMember(ids[2].peer_id.clone()),
                );
            }
            match client
                .accept_response(peer(&ids[0]), &next, response, &SECRET, now)
                .unwrap()
            {
                CheckpointProgress::More(request) => next = request,
                CheckpointProgress::Complete(offer) => break offer,
            }
        };
        assert!(pages > 1);
        assert_eq!(complete.payload.snapshot, original);
        assert_ne!(complete.payload.snapshot, *current.snapshot());
        assert_eq!(complete.payload.publisher.peer_id, ids[0].peer_id);
        for manager in [&client, &server] {
            assert_eq!(manager.stats().buffered_bytes, 0);
            assert_eq!(
                manager.stats().publisher_sessions + manager.stats().assembler_sessions,
                0
            );
            assert_eq!(manager.stats().retired_sessions, 1);
        }
        client.cleanup(now + Duration::from_mins(1));
        server.cleanup(now + Duration::from_mins(1));
        assert_eq!(client.stats(), CheckpointSyncStats::default());
        assert_eq!(server.stats(), CheckpointSyncStats::default());
    }

    #[test]
    fn mixed_pages_retire_the_expected_request_without_leaking_buffers() {
        let ids = identities();
        let current = state(&ids);
        let now = Instant::now();
        for variant in 0..7 {
            let mut client = sync(&ids[1]);
            let mut server = sync(&ids[0]);
            let first = request(&mut client, peer(&ids[0]), now);
            let reply = server
                .respond_at(
                    peer(&ids[1]),
                    &first,
                    &current,
                    &ids[0],
                    &SECRET,
                    now,
                    WALL_NOW,
                )
                .unwrap();
            let CheckpointProgress::More(next) = client
                .accept_response(peer(&ids[0]), &first, reply, &SECRET, now)
                .unwrap()
            else {
                panic!("more")
            };
            let response = server
                .respond_at(
                    peer(&ids[1]),
                    &next,
                    &current,
                    &ids[0],
                    &SECRET,
                    now,
                    WALL_NOW,
                )
                .unwrap();
            let CheckpointPageResponse::Page(mut bad) = response else {
                panic!("page")
            };
            match variant {
                0 => bad.challenge.nonce = [2; 32],
                1 => bad.offer_digest[0] ^= 1,
                2 => bad.cursor = 0,
                3 => bad.total_bytes += 1,
                4 => bad.bytes.pop().map(|_| ()).unwrap(),
                5 => bad.version += 1,
                _ => bad.challenge.anchor = NetworkAnchor::new([8; 32]).unwrap(),
            }
            assert!(
                client
                    .accept_response(
                        peer(&ids[0]),
                        &next,
                        CheckpointPageResponse::Page(bad),
                        &SECRET,
                        now
                    )
                    .is_err()
            );
            assert_eq!(client.stats().buffered_bytes, 0);
            assert_eq!(client.stats().assembler_sessions, 0);
            assert_eq!(client.stats().retired_sessions, 1);
        }
    }

    #[test]
    fn duplicate_pages_and_publisher_cursor_replays_fail_closed() {
        let ids = identities();
        let current = state(&ids);
        let now = Instant::now();
        let mut client = sync(&ids[1]);
        let mut server = sync(&ids[0]);
        let first = request(&mut client, peer(&ids[0]), now);
        let reply = server
            .respond_at(
                peer(&ids[1]),
                &first,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW,
            )
            .unwrap();
        assert!(matches!(
            client
                .accept_response(peer(&ids[0]), &first, reply.clone(), &SECRET, now)
                .unwrap(),
            CheckpointProgress::More(_)
        ));
        assert!(matches!(
            client.accept_response(peer(&ids[0]), &first, reply, &SECRET, now),
            Err(CheckpointSyncError::CursorMismatch)
        ));
        assert_eq!(client.stats().buffered_bytes, 0);
        assert!(matches!(
            server.respond_at(
                peer(&ids[1]),
                &first,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW
            ),
            Err(CheckpointSyncError::CursorMismatch)
        ));
        assert_eq!(server.stats().buffered_bytes, 0);
        assert!(matches!(
            server.respond_at(
                peer(&ids[1]),
                &first,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW
            ),
            Err(CheckpointSyncError::Replay)
        ));
    }

    #[test]
    fn wrong_transport_peer_cannot_complete_or_cancel_another_peers_assembly() {
        let ids = identities();
        let current = state(&ids);
        let now = Instant::now();
        let mut client = sync(&ids[1]);
        let mut server = sync(&ids[0]);
        let first = request(&mut client, peer(&ids[0]), now);
        let reply = server
            .respond_at(
                peer(&ids[1]),
                &first,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW,
            )
            .unwrap();
        assert!(matches!(
            client.accept_response(peer(&ids[2]), &first, reply.clone(), &SECRET, now),
            Err(CheckpointSyncError::UnknownSession)
        ));
        assert_eq!(client.stats().assembler_sessions, 1);
        assert!(matches!(
            client
                .accept_response(peer(&ids[0]), &first, reply, &SECRET, now)
                .unwrap(),
            CheckpointProgress::More(_)
        ));
        client.cancel(peer(&ids[2]), &first.challenge, now);
        assert!(client.stats().buffered_bytes > 0);
        client.cancel(peer(&ids[0]), &first.challenge, now);
        assert_eq!(client.stats().buffered_bytes, 0);
        assert_eq!(client.stats().assembler_sessions, 0);
    }

    #[test]
    fn deadlines_are_monotonic_not_extended_and_cleanup_is_complete() {
        let ids = identities();
        let current = state(&ids);
        let now = Instant::now();
        let mut client = sync(&ids[1]);
        let mut server = sync(&ids[0]);
        for deadline in [now, now + Duration::from_secs(61)] {
            assert!(
                client
                    .start_request(peer(&ids[0]), challenge(1), &SECRET, now, deadline)
                    .is_err()
            );
        }
        let first = request(&mut client, peer(&ids[0]), now);
        let reply = server
            .respond_at(
                peer(&ids[1]),
                &first,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW,
            )
            .unwrap();
        let CheckpointProgress::More(next) = client
            .accept_response(
                peer(&ids[0]),
                &first,
                reply,
                &SECRET,
                now + Duration::from_secs(29),
            )
            .unwrap()
        else {
            panic!("more")
        };
        let reply = server
            .respond_at(
                peer(&ids[1]),
                &next,
                &current,
                &ids[0],
                &SECRET,
                now + Duration::from_secs(29),
                WALL_NOW,
            )
            .unwrap();
        assert!(matches!(
            client.accept_response(
                peer(&ids[0]),
                &next,
                reply,
                &SECRET,
                now + Duration::from_secs(30)
            ),
            Err(CheckpointSyncError::UnknownSession)
        ));
        assert_eq!(client.stats(), CheckpointSyncStats::default());
        server.cleanup(now + Duration::from_mins(1));
        assert_eq!(server.stats(), CheckpointSyncStats::default());
        server.cleanup(now + Duration::from_mins(2));
        assert_eq!(server.stats(), CheckpointSyncStats::default());
    }

    #[test]
    fn global_per_peer_and_retired_session_limits_are_hard_bounds() {
        let ids = identities();
        let now = Instant::now();
        let limits = CheckpointSyncLimits {
            max_sessions: 2,
            max_sessions_per_peer: 1,
            ..Default::default()
        };
        let mut client = CheckpointSync::new(peer(&ids[0]), anchor(), limits).unwrap();
        let first = request(&mut client, peer(&ids[1]), now);
        assert!(matches!(
            client.start_request(
                peer(&ids[1]),
                challenge(2),
                &SECRET,
                now,
                now + Duration::from_secs(30)
            ),
            Err(CheckpointSyncError::ResourceLimit)
        ));
        client.cancel(peer(&ids[1]), &first.challenge, now);
        assert!(matches!(
            client.start_request(
                peer(&ids[1]),
                challenge(2),
                &SECRET,
                now,
                now + Duration::from_secs(30)
            ),
            Err(CheckpointSyncError::ResourceLimit)
        ));
        request(&mut client, peer(&ids[2]), now);
        assert!(matches!(
            client.start_request(
                peer(&ids[3]),
                challenge(2),
                &SECRET,
                now,
                now + Duration::from_secs(30)
            ),
            Err(CheckpointSyncError::ResourceLimit)
        ));
        assert_eq!(
            client.stats().assembler_sessions + client.stats().retired_sessions,
            2
        );
        client.cleanup(now + Duration::from_secs(30));
        assert_eq!(client.stats(), CheckpointSyncStats::default());
        request(&mut client, peer(&ids[1]), now + Duration::from_secs(30));
    }

    #[test]
    fn publisher_and_assembler_byte_budgets_reject_then_release() {
        let ids = identities();
        let current = state(&ids);
        let now = Instant::now();
        let limits = CheckpointSyncLimits {
            max_buffered_bytes: 1,
            ..Default::default()
        };
        let mut server = CheckpointSync::new(peer(&ids[0]), anchor(), limits).unwrap();
        let mut client = sync(&ids[1]);
        let first = request(&mut client, peer(&ids[0]), now);
        assert!(matches!(
            server.respond_at(
                peer(&ids[1]),
                &first,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW
            ),
            Err(CheckpointSyncError::ResourceLimit)
        ));
        assert_eq!(server.stats(), CheckpointSyncStats::default());
        let mut server = sync(&ids[0]);
        let reply = server
            .respond_at(
                peer(&ids[1]),
                &first,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW,
            )
            .unwrap();
        let mut client = CheckpointSync::new(peer(&ids[1]), anchor(), limits).unwrap();
        let first = request(&mut client, peer(&ids[0]), now);
        assert!(matches!(
            client.accept_response(peer(&ids[0]), &first, reply, &SECRET, now),
            Err(CheckpointSyncError::ResourceLimit)
        ));
        assert_eq!(client.stats().buffered_bytes, 0);
        assert_eq!(client.stats().assembler_sessions, 0);
    }

    #[test]
    fn aggregate_budget_charges_total_not_just_first_page() {
        let ids = identities();
        let now = Instant::now();
        let limits = CheckpointSyncLimits {
            max_buffered_bytes: MAX_SNAPSHOT_OFFER_BYTES,
            ..Default::default()
        };
        let mut client = CheckpointSync::new(peer(&ids[0]), anchor(), limits).unwrap();
        for (index, identity) in ids.iter().enumerate().take(3).skip(1) {
            let first = request(&mut client, peer(identity), now);
            let response = CheckpointPageResponse::Page(CheckpointPage {
                version: 1,
                challenge: first.challenge.clone(),
                offer_digest: [3; 32],
                cursor: 0,
                total_bytes: u32::try_from(MAX_SNAPSHOT_OFFER_BYTES).unwrap(),
                bytes: vec![1; MAX_PAGE_BYTES],
            });
            let result = client.accept_response(peer(identity), &first, response, &SECRET, now);
            if index == 1 {
                assert!(matches!(result, Ok(CheckpointProgress::More(_))));
            } else {
                assert!(matches!(result, Err(CheckpointSyncError::ResourceLimit)));
            }
        }
        assert_eq!(client.stats().buffered_bytes, MAX_SNAPSHOT_OFFER_BYTES);
        assert_eq!(client.stats().assembler_sessions, 1);
        client.cleanup(now + Duration::from_secs(30));
        assert_eq!(client.stats(), CheckpointSyncStats::default());
    }

    #[test]
    fn resource_limits_and_request_page_shapes_reject_unsupported_inputs() {
        let ids = identities();
        let mut limits = CheckpointSyncLimits::default();
        for variant in 0..6 {
            match variant {
                0 => limits.max_sessions = MAX_TRANSFER_SESSIONS + 1,
                1 => limits.max_sessions_per_peer = 0,
                2 => limits.max_buffered_bytes = MAX_TRANSFER_BUFFER_BYTES + 1,
                3 => limits.session_timeout = Duration::ZERO,
                4 => limits.session_timeout = Duration::from_secs(61),
                _ => limits.max_sessions = 0,
            }
            assert!(CheckpointSync::new(peer(&ids[0]), anchor(), limits).is_err());
            limits = CheckpointSyncLimits::default();
        }
        let good =
            CheckpointPageRequest::signed(challenge(1), None, 0, peer(&ids[0]), &SECRET).unwrap();
        for variant in 0..5 {
            let mut bad = good.clone();
            match variant {
                0 => bad.version = 2,
                1 => bad.cursor = u32::MAX,
                2 => bad.challenge.nonce = [0; 32],
                3 => bad.challenge.anchor.rank_version += 1,
                _ => bad.cursor = 1,
            }
            assert!(bad.validate().is_err());
        }
    }

    #[test]
    fn proof_valid_removed_peer_can_fetch_and_authenticate_its_exclusion() {
        let ids = identities();
        let mut current = state(&ids);
        let mut returning = CooperativeMembershipState::restore(
            capability(),
            ids[1].peer_id.clone(),
            current.retained(),
        )
        .unwrap();
        mutate(
            &mut current,
            &ids[0],
            MembershipChange::RemoveMember(ids[1].peer_id.clone()),
        );
        let now = Instant::now();
        let window = Duration::from_secs(1);
        let challenge = returning.begin_resync(now, window).unwrap();
        let mut client = sync(&ids[1]);
        let mut server = sync(&ids[0]);
        let mut next = client
            .start_request(peer(&ids[0]), challenge, &SECRET, now, now + window)
            .unwrap();
        let complete = loop {
            let response = server
                .respond_at(
                    peer(&ids[1]),
                    &next,
                    &current,
                    &ids[0],
                    &SECRET,
                    now,
                    WALL_NOW,
                )
                .unwrap();
            match client
                .accept_response(peer(&ids[0]), &next, response, &SECRET, now)
                .unwrap()
            {
                CheckpointProgress::More(request) => next = request,
                CheckpointProgress::Complete(offer) => break offer,
            }
        };
        returning
            .collect_offer(&complete, peer(&ids[0]), now)
            .unwrap();
        let selected = returning.finish_resync(now + window, WALL_NOW).unwrap();
        assert_eq!(selected.sync_state, MembershipSyncState::Excluded);
        assert!(
            returning
                .snapshot()
                .payload
                .member(&ids[1].peer_id)
                .is_none()
        );
        assert_eq!(
            client.stats().buffered_bytes + server.stats().buffered_bytes,
            0
        );
    }

    #[test]
    fn capability_descriptor_is_additive_bounded_and_scope_checked() {
        let ids = identities();
        let current = state(&ids);
        let legacy = super::super::ControlCapabilities::local("test", None, 1_280);
        let serialized = serde_json::to_value(&legacy).unwrap();
        assert!(serialized.get("checkpoint").is_none());
        let decoded: super::super::ControlCapabilities =
            serde_json::from_value(serialized).unwrap();
        assert!(decoded.checkpoint.is_none());
        let new = legacy.with_checkpoint(&current).unwrap();
        let decoded: super::super::ControlCapabilities =
            serde_json::from_slice(&serde_json::to_vec(&new).unwrap()).unwrap();
        assert_eq!(new, decoded);
        let descriptor = decoded.checkpoint.as_ref().unwrap();
        descriptor.validate_for(&anchor()).unwrap();
        assert_eq!(
            descriptor.rank(),
            current.snapshot().payload.rank().unwrap()
        );
        assert!(matches!(
            descriptor.validate_for(&NetworkAnchor::new([8; 32]).unwrap()),
            Err(CheckpointSyncError::WrongScope)
        ));
        for variant in 0..5 {
            let mut bad = serde_json::to_value(&decoded).unwrap();
            match variant {
                0 => bad["checkpoint"]["version"] = 2.into(),
                1 => bad["checkpoint"]["anchor"]["rank_version"] = 2.into(),
                2 => bad["checkpoint"]["active_member_count"] = 257.into(),
                3 => bad["checkpoint"]["boundary"]["authority_revision"] = u64::MAX.into(),
                _ => bad["checkpoint"]["sync"] = "unknown".into(),
            }
            assert!(serde_json::from_value::<super::super::ControlCapabilities>(bad).is_err());
        }
        let mut unsupported = new;
        unsupported.checkpoint.as_mut().unwrap().version = 2;
        assert_eq!(
            super::super::validate_capabilities(&unsupported, "test", None, &[]),
            Some(super::super::ControlRejectionReason::InvalidMembershipRecord)
        );
    }

    #[tokio::test]
    async fn codec_round_trips_max_pages_and_requests_under_independent_cap() {
        let ids = identities();
        let mut codec = CheckpointCodec;
        let protocol = StreamProtocol::new(CHECKPOINT_PROTOCOL);
        let first =
            CheckpointPageRequest::signed(challenge(255), None, 0, peer(&ids[0]), &SECRET).unwrap();
        let mut request = Cursor::new(Vec::new());
        request_response::Codec::write_request(&mut codec, &protocol, &mut request, first.clone())
            .await
            .unwrap();
        request.set_position(0);
        assert_eq!(
            request_response::Codec::read_request(&mut codec, &protocol, &mut request)
                .await
                .unwrap(),
            first
        );
        let response = CheckpointPageResponse::Page(CheckpointPage {
            version: 1,
            challenge: SnapshotChallenge {
                anchor: NetworkAnchor::new([255; 32]).unwrap(),
                nonce: [255; 32],
            },
            offer_digest: [255; 32],
            cursor: 0,
            total_bytes: u32::try_from(MAX_PAGE_BYTES).unwrap(),
            bytes: vec![255; MAX_PAGE_BYTES],
        });
        let mut bytes = Cursor::new(Vec::new());
        request_response::Codec::write_response(
            &mut codec,
            &protocol,
            &mut bytes,
            response.clone(),
        )
        .await
        .unwrap();
        assert!(bytes.get_ref().len() <= MAX_PAGE_MESSAGE_BYTES + 2);
        bytes.set_position(0);
        assert_eq!(
            request_response::Codec::read_response(&mut codec, &protocol, &mut bytes)
                .await
                .unwrap(),
            response
        );
        let rejected = CheckpointPageResponse::rejected(&first, CheckpointRejection::Busy);
        let mut bytes = Cursor::new(Vec::new());
        request_response::Codec::write_response(
            &mut codec,
            &protocol,
            &mut bytes,
            rejected.clone(),
        )
        .await
        .unwrap();
        bytes.set_position(0);
        assert_eq!(
            request_response::Codec::read_response(&mut codec, &protocol, &mut bytes)
                .await
                .unwrap(),
            rejected
        );
    }

    #[tokio::test]
    async fn codec_rejects_oversize_before_reading_body_and_unknown_versions_fields() {
        let mut codec = CheckpointCodec;
        let protocol = StreamProtocol::new(CHECKPOINT_PROTOCOL);
        for size in [0, MAX_PAGE_MESSAGE_BYTES + 1, usize::from(u16::MAX)] {
            let mut input = Cursor::new(u16::try_from(size).unwrap().to_be_bytes().to_vec());
            assert_eq!(
                request_response::Codec::read_response(&mut codec, &protocol, &mut input)
                    .await
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(input.position(), 2);
        }
        let ids = identities();
        let request =
            CheckpointPageRequest::signed(challenge(1), None, 0, peer(&ids[0]), &SECRET).unwrap();
        for variant in 0..3 {
            let mut value = serde_json::to_value(&request).unwrap();
            match variant {
                0 => value["version"] = 2.into(),
                1 => value["unexpected"] = true.into(),
                _ => value["cursor"] = u32::MAX.into(),
            }
            let encoded = serde_json::to_vec(&value).unwrap();
            let mut bytes = u16::try_from(encoded.len()).unwrap().to_be_bytes().to_vec();
            bytes.extend(encoded);
            assert_eq!(
                request_response::Codec::read_request(
                    &mut codec,
                    &protocol,
                    &mut Cursor::new(bytes)
                )
                .await
                .unwrap_err()
                .kind(),
                io::ErrorKind::InvalidData
            );
        }
        let wrong = StreamProtocol::new("/p2p-vpn/checkpoint-sync/2");
        assert!(
            request_response::Codec::write_request(
                &mut codec,
                &wrong,
                &mut Cursor::new(Vec::new()),
                request
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn malformed_completed_offer_digest_peer_and_payload_fail_and_release() {
        let ids = identities();
        let current = state(&ids);
        let now = Instant::now();
        for variant in 0..3 {
            let mut client = sync(&ids[1]);
            let remote = if variant == 2 {
                peer(&ids[2])
            } else {
                peer(&ids[0])
            };
            let first = request(&mut client, remote, now);
            let offer = current
                .make_offer_at(first.challenge.clone(), &ids[0], WALL_NOW)
                .unwrap();
            let bytes = if variant == 1 {
                b"not an offer".to_vec()
            } else {
                serde_json::to_vec(&offer).unwrap()
            };
            let mut digest = offer_digest(&bytes);
            if variant == 0 {
                digest[0] ^= 1;
            }
            let mut next = first;
            for (index, chunk) in bytes.chunks(MAX_PAGE_BYTES).enumerate() {
                let response = CheckpointPageResponse::Page(CheckpointPage {
                    version: 1,
                    challenge: next.challenge.clone(),
                    offer_digest: digest,
                    cursor: u32::try_from(index * MAX_PAGE_BYTES).unwrap(),
                    total_bytes: u32::try_from(bytes.len()).unwrap(),
                    bytes: chunk.to_vec(),
                });
                let result = client.accept_response(remote, &next, response, &SECRET, now);
                if (index + 1) * MAX_PAGE_BYTES >= bytes.len() {
                    assert!(result.is_err());
                } else {
                    let Ok(CheckpointProgress::More(request)) = result else {
                        panic!("more")
                    };
                    next = request;
                }
            }
            assert_eq!(client.stats().buffered_bytes, 0);
            assert_eq!(client.stats().assembler_sessions, 0);
        }
    }

    #[test]
    fn explicit_rejection_cancels_only_the_challenge_scoped_assembly() {
        let ids = identities();
        let now = Instant::now();
        let mut client = sync(&ids[1]);
        let first = request(&mut client, peer(&ids[0]), now);
        let rejected = CheckpointPageResponse::rejected(&first, CheckpointRejection::Busy);
        assert!(matches!(
            client.accept_response(peer(&ids[0]), &first, rejected, &SECRET, now),
            Err(CheckpointSyncError::Rejected)
        ));
        assert_eq!(client.stats().assembler_sessions, 0);
        assert_eq!(client.stats().buffered_bytes, 0);
        assert!(matches!(
            client.start_request(
                peer(&ids[0]),
                first.challenge,
                &SECRET,
                now,
                now + Duration::from_secs(30)
            ),
            Err(CheckpointSyncError::Replay)
        ));
    }

    #[test]
    fn invalid_local_request_proof_retires_reserved_assembly_buffers() {
        let ids = identities();
        let current = state(&ids);
        let now = Instant::now();
        let mut client = sync(&ids[1]);
        let mut server = sync(&ids[0]);
        let first = request(&mut client, peer(&ids[0]), now);
        let reply = server
            .respond_at(
                peer(&ids[1]),
                &first,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW,
            )
            .unwrap();
        let CheckpointProgress::More(mut next) = client
            .accept_response(peer(&ids[0]), &first, reply, &SECRET, now)
            .unwrap()
        else {
            panic!("more")
        };
        assert!(client.stats().buffered_bytes > 0);
        let reply = server
            .respond_at(
                peer(&ids[1]),
                &next,
                &current,
                &ids[0],
                &SECRET,
                now,
                WALL_NOW,
            )
            .unwrap();
        next.proof[0] ^= 1;
        assert!(matches!(
            client.accept_response(peer(&ids[0]), &next, reply, &SECRET, now),
            Err(CheckpointSyncError::Unauthorized)
        ));
        assert_eq!(client.stats().assembler_sessions, 0);
        assert_eq!(client.stats().buffered_bytes, 0);
        assert_eq!(client.stats().retired_sessions, 1);
    }

    #[tokio::test]
    async fn codec_bounds_decoded_page_bytes_even_when_base64_lengths_match() {
        let mut codec = CheckpointCodec;
        let protocol = StreamProtocol::new(CHECKPOINT_PROTOCOL);
        let response = CheckpointPageResponse::Page(CheckpointPage {
            version: 1,
            challenge: challenge(1),
            offer_digest: [3; 32],
            cursor: 0,
            total_bytes: u32::try_from(MAX_PAGE_BYTES).unwrap(),
            bytes: vec![1; MAX_PAGE_BYTES],
        });
        let mut value = serde_json::to_value(&response).unwrap();
        let oversized = STANDARD.encode(vec![1; MAX_PAGE_BYTES + 1]);
        assert_eq!(
            oversized.len(),
            STANDARD.encode(vec![1; MAX_PAGE_BYTES]).len()
        );
        value["Page"]["bytes"] = oversized.into();
        let body = serde_json::to_vec(&value).unwrap();
        assert!(body.len() < MAX_PAGE_MESSAGE_BYTES);
        let mut bytes = u16::try_from(body.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend(body);
        assert!(
            request_response::Codec::read_response(&mut codec, &protocol, &mut Cursor::new(bytes))
                .await
                .is_err()
        );
        let CheckpointPageResponse::Page(mut bad) = response else {
            panic!("page")
        };
        bad.bytes.push(1);
        let mut output = Cursor::new(Vec::new());
        assert!(
            request_response::Codec::write_response(
                &mut codec,
                &protocol,
                &mut output,
                CheckpointPageResponse::Page(bad)
            )
            .await
            .is_err()
        );
        assert!(output.get_ref().is_empty());
    }

    #[test]
    fn assembled_offer_does_not_bypass_core_mac_or_publisher_authentication() {
        let ids = identities();
        let current = state(&ids);
        let now = Instant::now();
        for variant in 0..2 {
            let mut returning = CooperativeMembershipState::restore(
                capability(),
                ids[1].peer_id.clone(),
                current.retained(),
            )
            .unwrap();
            let challenge = returning.begin_resync(now, Duration::from_secs(1)).unwrap();
            let mut client = sync(&ids[1]);
            let mut next = client
                .start_request(
                    peer(&ids[0]),
                    challenge,
                    &SECRET,
                    now,
                    now + Duration::from_secs(1),
                )
                .unwrap();
            let mut offer = current
                .make_offer_at(next.challenge.clone(), &ids[0], WALL_NOW)
                .unwrap();
            if variant == 0 {
                offer.payload.snapshot.mac[0] ^= 1;
            } else {
                offer.signature = STANDARD.encode([1; 64]);
            }
            let bytes = serde_json::to_vec(&offer).unwrap();
            let digest = offer_digest(&bytes);
            for (index, chunk) in bytes.chunks(MAX_PAGE_BYTES).enumerate() {
                let response = CheckpointPageResponse::Page(CheckpointPage {
                    version: 1,
                    challenge: next.challenge.clone(),
                    offer_digest: digest,
                    cursor: u32::try_from(index * MAX_PAGE_BYTES).unwrap(),
                    total_bytes: u32::try_from(bytes.len()).unwrap(),
                    bytes: chunk.to_vec(),
                });
                match client
                    .accept_response(peer(&ids[0]), &next, response, &SECRET, now)
                    .unwrap()
                {
                    CheckpointProgress::More(request) => next = request,
                    CheckpointProgress::Complete(decoded) => {
                        assert!(
                            returning
                                .collect_offer(&decoded, peer(&ids[0]), now)
                                .is_err()
                        );
                        assert_eq!(returning.sync_state(), MembershipSyncState::Resyncing);
                    }
                }
            }
            assert_eq!(client.stats().buffered_bytes, 0);
        }
    }

    fn swarm(identity: &NodeIdentity) -> Swarm<request_response::Behaviour<CheckpointCodec>> {
        SwarmBuilder::with_existing_identity(identity.keypair().unwrap())
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .unwrap()
            .with_behaviour(|_| behaviour(4))
            .unwrap()
            .build()
    }

    #[tokio::test]
    async fn two_swarms_transfer_multiple_pages_then_core_authenticates_offer() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let ids = identities(); let current = state(&ids);
            let mut returning = CooperativeMembershipState::restore(capability(), ids[1].peer_id.clone(), current.retained()).unwrap();
            let mut server = swarm(&ids[0]); let mut client = swarm(&ids[1]);
            server.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
            let address = loop {
                if let SwarmEvent::NewListenAddr { address, .. } = server.select_next_some().await { break address; }
            };
            client.add_peer_address(peer(&ids[0]), address);
            let mut sender = sync(&ids[0]); let mut receiver = sync(&ids[1]);
            let now = Instant::now(); let window = Duration::from_secs(10);
            let challenge = returning.begin_resync(now, window).unwrap();
            let mut pending = receiver.start_request(peer(&ids[0]), challenge, &SECRET, now, now + window).unwrap();
            let mut request_id = client.behaviour_mut().send_request(&peer(&ids[0]), pending.clone());
            let mut pages = 0;
            loop {
                tokio::select! {
                    event = server.select_next_some() => {
                        match event {
                            SwarmEvent::Behaviour(request_response::Event::Message { peer: remote, message: request_response::Message::Request { request, channel, .. }, .. }) => {
                                assert_eq!(remote, peer(&ids[1]));
                                let response = sender.respond_at(remote, &request, &current, &ids[0], &SECRET, Instant::now(), WALL_NOW).unwrap();
                                server.behaviour_mut().send_response(channel, response).unwrap();
                            },
                            SwarmEvent::Behaviour(request_response::Event::InboundFailure { error, .. }) => panic!("inbound {error:?}"),
                            _ => (),
                        }
                    },
                    event = client.select_next_some() => {
                        match event {
                            SwarmEvent::Behaviour(request_response::Event::Message { peer: remote, message: request_response::Message::Response { request_id: completed, response }, .. }) => {
                                assert_eq!(request_id, completed); assert_eq!(remote, peer(&ids[0])); pages += 1;
                                match receiver.accept_response(remote, &pending, response, &SECRET, Instant::now()).unwrap() {
                                    CheckpointProgress::More(next) => { pending = next; request_id = client.behaviour_mut().send_request(&remote, pending.clone()); },
                                    CheckpointProgress::Complete(offer) => {
                                        returning.collect_offer(&offer, remote, Instant::now()).unwrap();
                                        assert_eq!(offer.payload.snapshot, *current.snapshot()); break;
                                    },
                                }
                            },
                            SwarmEvent::Behaviour(request_response::Event::OutboundFailure { error, .. }) => panic!("outbound {error:?}"),
                            _ => (),
                        }
                    },
                }
            }
            assert!(pages > 1);
            assert_eq!(sender.stats().buffered_bytes + receiver.stats().buffered_bytes, 0);
            let selection = returning.finish_resync(now + window, WALL_NOW).unwrap();
            assert_eq!(selection.sync_state, MembershipSyncState::Participating);
        }).await.expect("checkpoint loopback protocol exceeded 15 seconds");
    }
}
