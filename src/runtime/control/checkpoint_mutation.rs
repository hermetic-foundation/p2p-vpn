//! Ephemeral, bounded delivery of exact-base cooperative membership commands.
//!
//! A transport-authenticated sender must be the signed command's issuer. Scope,
//! encoding, and signature validation here DO NOT establish current authority:
//! the runtime must call core `apply_mutation_at` on a clone of its selected state,
//! persist the result, and install it before acknowledging `Applied`.
//!
//! Self-departure is a last signed command captured before local exclusion, not
//! permission for an excluded peer to publish snapshots. There is no replay log:
//! successful application advances the exact base and makes repeats stale.
//! Runtime retry/recipient buffers must be bounded and use one monotonic deadline
//! of at most 60 seconds. Attach that deadline with `with_deadline` before enqueueing:
//! the codec refuses expired queued writes and bounds the complete async write.
//! The 5-second libp2p timeout alone does not cover its pre-connection dial queue.

use std::{
    fmt, io,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{PeerId, StreamProtocol, request_response};
use serde::{Deserialize, Serialize};
use sha2_010::{Digest as _, Sha256};

use crate::membership::{
    MAX_MEMBERSHIP_RECORD_INTEGER,
    checkpoint::{CheckpointBoundary, CheckpointError, NetworkAnchor, SignedMembershipMutation},
};

pub const CHECKPOINT_MUTATION_PROTOCOL: &str = "/p2p-vpn/checkpoint-mutation/1";
pub const CHECKPOINT_MUTATION_VERSION: u8 = 1;
pub const MAX_MUTATION_MESSAGE_BYTES: usize = 16 * 1024;
pub const MAX_MUTATION_STREAMS: usize = 32;
pub const MUTATION_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_MUTATION_HANDOFF_LIFETIME: Duration = Duration::from_mins(1);
const MUTATION_DIGEST_DOMAIN: &[u8] = b"p2p-vpn checkpoint mutation handoff digest v1\n";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointMutationRequest {
    pub version: u8,
    pub mutation: SignedMembershipMutation,
    #[serde(skip)]
    outbound_deadline: Option<Instant>,
}

impl CheckpointMutationRequest {
    /// Freeze an already core-signed command, including a final self-departure.
    pub fn new(mutation: SignedMembershipMutation) -> Result<Self, CheckpointMutationError> {
        let request = Self {
            version: CHECKPOINT_MUTATION_VERSION,
            mutation,
            outbound_deadline: None,
        };
        request.validate()?;
        Ok(request)
    }

    /// Sender-local queue/write deadline; not signed, hashed, or transmitted.
    /// Reattaching cannot extend an existing deadline. Attach the handoff's
    /// original deadline, not a fresh retry budget.
    #[must_use]
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        let deadline = deadline.min(Instant::now() + MAX_MUTATION_HANDOFF_LIFETIME);
        self.outbound_deadline = Some(
            self.outbound_deadline
                .map_or(deadline, |old| old.min(deadline)),
        );
        self
    }

    fn validate(&self) -> Result<(), CheckpointMutationError> {
        if self.version != CHECKPOINT_MUTATION_VERSION {
            return Err(CheckpointMutationError::Invalid("unsupported version"));
        }
        // Bound serialization even for typed callers before core/key parsing work.
        bounded_encoding(self)?;
        let issuer = self
            .mutation
            .payload
            .issuer
            .peer_id
            .parse()
            .map_err(|_| CheckpointMutationError::Invalid("issuer peer identity"))?;
        self.mutation
            .authenticate_for(&self.mutation.payload.anchor, issuer)?;
        Ok(())
    }

    /// Authenticate the complete command without granting membership authority.
    ///
    /// The caller MUST still authorize/apply against its current exact base using
    /// core state. In particular, a valid old signature does not authorize a replay.
    pub fn authenticate_for(
        &self,
        anchor: &NetworkAnchor,
        transport_sender: PeerId,
    ) -> Result<&SignedMembershipMutation, CheckpointMutationError> {
        if self.version != CHECKPOINT_MUTATION_VERSION {
            return Err(CheckpointMutationError::Invalid("unsupported version"));
        }
        bounded_encoding(self)?;
        if &self.mutation.payload.anchor != anchor {
            return Err(CheckpointMutationError::WrongScope);
        }
        if self.mutation.payload.issuer.peer_id != transport_sender.to_string() {
            return Err(CheckpointMutationError::WrongIssuer);
        }
        self.mutation.authenticate_for(anchor, transport_sender)?;
        Ok(&self.mutation)
    }

    pub fn validate_for(
        &self,
        transport_sender: PeerId,
        anchor: &NetworkAnchor,
    ) -> Result<&SignedMembershipMutation, CheckpointMutationError> {
        self.authenticate_for(anchor, transport_sender)
    }

    pub fn digest(&self) -> Result<[u8; 32], CheckpointMutationError> {
        self.validate()?;
        let mut hash = Sha256::new();
        hash.update(MUTATION_DIGEST_DOMAIN);
        hash.update(serde_json::to_vec(&self.mutation)?);
        Ok(hash.finalize().into())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationRejection {
    Unauthorized,
    StaleBase,
    ResyncRequired,
    Invalid,
    Busy,
    PersistenceFailed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum MutationOutcome {
    /// The receiver durably committed and installed the command, not merely read it.
    Applied(CheckpointBoundary),
    Rejected {
        reason: MutationRejection,
        current: Option<CheckpointBoundary>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointMutationResponse {
    pub version: u8,
    pub mutation_digest: [u8; 32],
    pub outcome: MutationOutcome,
}

impl CheckpointMutationResponse {
    pub fn for_request(
        request: &CheckpointMutationRequest,
        outcome: MutationOutcome,
    ) -> Result<Self, CheckpointMutationError> {
        let response = Self {
            version: CHECKPOINT_MUTATION_VERSION,
            mutation_digest: request.digest()?,
            outcome,
        };
        response.validate()?;
        Ok(response)
    }

    fn validate(&self) -> Result<(), CheckpointMutationError> {
        if self.version != CHECKPOINT_MUTATION_VERSION || self.mutation_digest == [0; 32] {
            return Err(CheckpointMutationError::Invalid("mutation reply"));
        }
        let boundary = match self.outcome {
            MutationOutcome::Applied(boundary) => Some(boundary),
            MutationOutcome::Rejected { current, .. } => current,
        };
        if boundary.is_some_and(|boundary| {
            boundary.authority_revision > MAX_MEMBERSHIP_RECORD_INTEGER
                || boundary.digest == [0; 32]
        }) {
            return Err(CheckpointMutationError::Invalid("reply boundary"));
        }
        Ok(())
    }

    /// Also match the libp2p response's request ID and authenticated target peer.
    pub fn validate_for(
        &self,
        request: &CheckpointMutationRequest,
    ) -> Result<MutationOutcome, CheckpointMutationError> {
        self.validate()?;
        if self.mutation_digest != request.digest()? {
            return Err(CheckpointMutationError::ResponseMismatch);
        }
        Ok(self.outcome)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CheckpointMutationCodec;

#[async_trait]
impl request_response::Codec for CheckpointMutationCodec {
    type Protocol = StreamProtocol;
    type Request = CheckpointMutationRequest;
    type Response = CheckpointMutationResponse;

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
        ensure_before_deadline(request.outbound_deadline)?;
        request.validate().map_err(invalid_data)?;
        let bytes = bounded_encoding(&request).map_err(invalid_data)?;
        ensure_before_deadline(request.outbound_deadline)?;
        let write = write_frame(io, &bytes, request.outbound_deadline);
        if let Some(deadline) = request.outbound_deadline {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), write)
                .await
                .map_err(|_| deadline_expired())?
        } else {
            write.await
        }
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
pub fn behaviour(
    max_concurrent_streams: usize,
) -> request_response::Behaviour<CheckpointMutationCodec> {
    request_response::Behaviour::with_codec(
        CheckpointMutationCodec,
        [(
            StreamProtocol::new(CHECKPOINT_MUTATION_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(MUTATION_REQUEST_TIMEOUT)
            .with_max_concurrent_streams(stream_limit(max_concurrent_streams)),
    )
}

fn stream_limit(requested: usize) -> usize {
    requested.clamp(1, MAX_MUTATION_STREAMS)
}

fn require_protocol(protocol: &StreamProtocol) -> io::Result<()> {
    if protocol.as_ref() != CHECKPOINT_MUTATION_PROTOCOL {
        return Err(invalid_data("unsupported checkpoint mutation protocol"));
    }
    Ok(())
}

async fn read_message<T: AsyncRead + Unpin + Send, M: for<'de> Deserialize<'de>>(
    io: &mut T,
) -> io::Result<M> {
    let mut header = [0; 2];
    io.read_exact(&mut header).await?;
    let length = usize::from(u16::from_be_bytes(header));
    if length == 0 || length > MAX_MUTATION_MESSAGE_BYTES {
        return Err(invalid_data("checkpoint mutation message size"));
    }
    let mut bytes = vec![0; length];
    io.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(invalid_data)
}

async fn write_message<T: AsyncWrite + Unpin + Send, M: Serialize>(
    io: &mut T,
    message: &M,
) -> io::Result<()> {
    let bytes = bounded_encoding(message).map_err(invalid_data)?;
    write_frame(io, &bytes, None).await
}

async fn write_frame<T: AsyncWrite + Unpin + Send>(
    io: &mut T,
    bytes: &[u8],
    deadline: Option<Instant>,
) -> io::Result<()> {
    let length = u16::try_from(bytes.len()).map_err(invalid_data)?;
    ensure_before_deadline(deadline)?;
    io.write_all(&length.to_be_bytes()).await?;
    ensure_before_deadline(deadline)?;
    io.write_all(bytes).await?;
    ensure_before_deadline(deadline)?;
    io.close().await?;
    ensure_before_deadline(deadline)
}

fn ensure_before_deadline(deadline: Option<Instant>) -> io::Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(deadline_expired());
    }
    Ok(())
}

fn deadline_expired() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "checkpoint mutation handoff deadline expired",
    )
}

fn bounded_encoding(value: &impl Serialize) -> Result<Vec<u8>, CheckpointMutationError> {
    struct FrameBuffer(Vec<u8>);
    impl io::Write for FrameBuffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_MUTATION_MESSAGE_BYTES - self.0.len() {
                return Err(invalid_data("checkpoint mutation message size"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = FrameBuffer(Vec::new());
    serde_json::to_writer(&mut buffer, value)?;
    Ok(buffer.0)
}

fn invalid_data(error: impl fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[derive(Debug)]
pub enum CheckpointMutationError {
    Invalid(&'static str),
    WrongScope,
    WrongIssuer,
    ResponseMismatch,
    Core(CheckpointError),
    Encoding(serde_json::Error),
}

impl fmt::Display for CheckpointMutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid checkpoint mutation: {reason}"),
            Self::Encoding(_) => f.write_str("invalid checkpoint mutation encoding"),
            Self::Core(_) => f.write_str("checkpoint mutation rejected by core"),
            other => write!(f, "checkpoint mutation {other:?}"),
        }
    }
}

impl std::error::Error for CheckpointMutationError {}
impl From<CheckpointError> for CheckpointMutationError {
    fn from(error: CheckpointError) -> Self {
        Self::Core(error)
    }
}
impl From<serde_json::Error> for CheckpointMutationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encoding(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::RouteConfig,
        identity::NodeIdentity,
        membership::{
            MembershipRole,
            checkpoint::{
                CheckpointMember, CooperativeMembershipState, MAX_CHECKPOINT_ROUTES,
                MembershipChange, MembershipSyncState, NetworkCapability, SnapshotPolicy,
                SnapshotPublisher,
            },
        },
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use futures::{StreamExt as _, io::Cursor};
    use libp2p::{Swarm, SwarmBuilder, noise, swarm::SwarmEvent, tcp, yamux};
    use request_response::Codec as _;
    use std::time::Instant;

    const WALL_NOW: u64 = 1_000;
    const MAX_KEY_OR_SIGNATURE_BYTES: usize = 4096;

    fn anchor() -> NetworkAnchor {
        NetworkAnchor::new([7; 32]).unwrap()
    }
    fn capability() -> NetworkCapability {
        NetworkCapability::from_secret(anchor(), Some(&[9; 32])).unwrap()
    }
    fn peer(identity: &NodeIdentity) -> PeerId {
        identity.peer_id.parse().unwrap()
    }
    fn route(prefix: impl Into<String>, metric: u16) -> RouteConfig {
        RouteConfig {
            prefix: prefix.into(),
            metric,
        }
    }
    fn fixture() -> ([NodeIdentity; 3], CooperativeMembershipState) {
        let identities = std::array::from_fn(|_| NodeIdentity::generate_ed25519().unwrap());
        let mut members = identities
            .iter()
            .map(|identity| CheckpointMember::new(identity).unwrap())
            .collect::<Vec<_>>();
        members[2].expires_at_unix_seconds = Some(WALL_NOW + 1);
        let current = CooperativeMembershipState::bootstrap_at(
            capability(),
            identities[0].peer_id.clone(),
            members,
            SnapshotPolicy::default(),
            WALL_NOW,
        )
        .unwrap();
        (identities, current)
    }
    fn receiver(
        current: &CooperativeMembershipState,
        identity: &NodeIdentity,
    ) -> CooperativeMembershipState {
        let mut state = CooperativeMembershipState::restore(
            capability(),
            identity.peer_id.clone(),
            current.retained(),
        )
        .unwrap();
        let now = Instant::now();
        state.begin_resync(now, Duration::from_secs(1)).unwrap();
        state
            .finish_resync(now + Duration::from_secs(1), WALL_NOW)
            .unwrap();
        state
    }
    fn command(
        state: &CooperativeMembershipState,
        identity: &NodeIdentity,
        change: MembershipChange,
        now: u64,
    ) -> CheckpointMutationRequest {
        CheckpointMutationRequest::new(state.sign_mutation_at(identity, change, now).unwrap())
            .unwrap()
    }
    fn removal(
        current: &CooperativeMembershipState,
        identities: &[NodeIdentity; 3],
    ) -> CheckpointMutationRequest {
        command(
            current,
            &identities[0],
            MembershipChange::RemoveMember(identities[0].peer_id.clone()),
            WALL_NOW,
        )
    }
    fn resign(request: &mut CheckpointMutationRequest, identity: &NodeIdentity) {
        let mut bytes = b"p2p-vpn checkpoint mutation v1\n".to_vec();
        bytes.extend(serde_json::to_vec(&request.mutation.payload).unwrap());
        request.mutation.signature = STANDARD.encode(identity.sign(&bytes).unwrap());
    }
    fn framed(value: &impl Serialize) -> Cursor<Vec<u8>> {
        let encoded = serde_json::to_vec(value).unwrap();
        let mut bytes = u16::try_from(encoded.len()).unwrap().to_be_bytes().to_vec();
        bytes.extend(encoded);
        Cursor::new(bytes)
    }

    #[tokio::test]
    async fn all_core_mutation_variants_round_trip_and_verify_core_signatures() {
        let (ids, current) = fixture();
        let mut grant = current
            .snapshot()
            .payload
            .member(&ids[1].peer_id)
            .unwrap()
            .clone();
        grant.roles.push(MembershipRole::RouteAuthority);
        grant.route_grants = vec![route("10.0.0.0/8", 2)];
        let cases = [
            (MembershipChange::UpsertMember(grant), WALL_NOW),
            (
                MembershipChange::RemoveMember(ids[0].peer_id.clone()),
                WALL_NOW,
            ),
            (
                MembershipChange::SetPolicy(SnapshotPolicy {
                    max_active_members: 128,
                    route_grants_enabled: false,
                }),
                WALL_NOW,
            ),
            (MembershipChange::PruneExpired, WALL_NOW + 1),
        ];
        for (change, now) in cases {
            let request = command(&current, &ids[0], change, now);
            let protocol = StreamProtocol::new(CHECKPOINT_MUTATION_PROTOCOL);
            let mut codec = CheckpointMutationCodec;
            let mut bytes = Cursor::new(Vec::new());
            codec
                .write_request(&protocol, &mut bytes, request.clone())
                .await
                .unwrap();
            assert!(bytes.get_ref().len() <= MAX_MUTATION_MESSAGE_BYTES + 2);
            bytes.set_position(0);
            let decoded = codec.read_request(&protocol, &mut bytes).await.unwrap();
            assert_eq!(decoded, request);
            let mut remote = receiver(&current, &ids[1]);
            remote
                .apply_mutation_at(decoded.validate_for(peer(&ids[0]), &anchor()).unwrap(), now)
                .unwrap();
            let reply = CheckpointMutationResponse::for_request(
                &request,
                MutationOutcome::Applied(remote.snapshot().payload.boundary().unwrap()),
            )
            .unwrap();
            let mut bytes = Cursor::new(Vec::new());
            codec
                .write_response(&protocol, &mut bytes, reply.clone())
                .await
                .unwrap();
            bytes.set_position(0);
            assert_eq!(
                codec.read_response(&protocol, &mut bytes).await.unwrap(),
                reply
            );
            assert_eq!(reply.validate_for(&request).unwrap(), reply.outcome);
        }
    }

    #[test]
    fn transport_peer_scope_and_complete_payload_are_authenticated() {
        let (ids, current) = fixture();
        let request = removal(&current, &ids);
        assert!(matches!(
            request.validate_for(peer(&ids[1]), &anchor()),
            Err(CheckpointMutationError::WrongIssuer)
        ));
        assert!(matches!(
            request.validate_for(peer(&ids[0]), &NetworkAnchor::new([8; 32]).unwrap()),
            Err(CheckpointMutationError::WrongScope)
        ));
        for variant in 0..4 {
            let mut bad = request.clone();
            match variant {
                0 => bad.mutation.payload.base.digest[0] ^= 1,
                1 => bad.mutation.payload.base.authority_revision += 1,
                2 => {
                    bad.mutation.payload.change =
                        MembershipChange::RemoveMember(ids[2].peer_id.clone());
                }
                _ => {
                    let mut signature = STANDARD.decode(&bad.mutation.signature).unwrap();
                    signature[0] ^= 1;
                    bad.mutation.signature = STANDARD.encode(signature);
                }
            }
            assert!(matches!(
                bad.validate_for(peer(&ids[0]), &anchor()),
                Err(CheckpointMutationError::Core(
                    CheckpointError::InvalidSignature
                ))
            ));
        }
    }

    #[test]
    fn pinned_scope_and_key_binding_fail_even_with_valid_detached_signatures() {
        let (ids, current) = fixture();
        let mut request = removal(&current, &ids);
        request.mutation.payload.anchor = NetworkAnchor::new([8; 32]).unwrap();
        resign(&mut request, &ids[0]);
        assert!(matches!(
            request.validate_for(peer(&ids[0]), &anchor()),
            Err(CheckpointMutationError::WrongScope)
        ));
        request.mutation.payload.anchor = anchor();
        request.mutation.payload.issuer.public_key = SnapshotPublisher::from_identity(&ids[1])
            .unwrap()
            .public_key;
        resign(&mut request, &ids[0]);
        assert!(matches!(
            request.validate_for(peer(&ids[0]), &anchor()),
            Err(CheckpointMutationError::Core(CheckpointError::Invalid(
                "noncanonical or mismatched member key"
            )))
        ));
    }

    #[test]
    fn issuer_signature_does_not_grant_absent_or_restoring_member_authority() {
        let (ids, current) = fixture();
        let stranger = NodeIdentity::generate_ed25519().unwrap();
        let mut request = removal(&current, &ids);
        request.mutation.payload.issuer = SnapshotPublisher::from_identity(&stranger).unwrap();
        resign(&mut request, &stranger);
        let authenticated = request.validate_for(peer(&stranger), &anchor()).unwrap();
        let mut remote = receiver(&current, &ids[1]);
        assert!(matches!(
            remote.apply_mutation_at(authenticated, WALL_NOW),
            Err(CheckpointError::Invalid("inactive mutation issuer"))
        ));
        let mut restoring = CooperativeMembershipState::restore(
            capability(),
            ids[1].peer_id.clone(),
            current.retained(),
        )
        .unwrap();
        let authorized_request = removal(&current, &ids);
        assert!(matches!(
            restoring.apply_mutation_at(
                authorized_request
                    .validate_for(peer(&ids[0]), &anchor())
                    .unwrap(),
                WALL_NOW
            ),
            Err(CheckpointError::NoParticipation)
        ));
    }

    #[test]
    fn repeat_after_lost_ack_only_confirms_exact_expected_result_without_history() {
        let (ids, current) = fixture();
        let request = removal(&current, &ids);
        let mut remote = receiver(&current, &ids[1]);
        remote
            .apply_mutation_at(&request.mutation, WALL_NOW)
            .unwrap();
        let expected = remote.snapshot().payload.boundary().unwrap();
        let retained = serde_json::to_vec(&remote.retained()).unwrap();
        assert!(matches!(
            remote.apply_mutation_at(
                request.validate_for(peer(&ids[0]), &anchor()).unwrap(),
                WALL_NOW
            ),
            Err(CheckpointError::StaleMutation)
        ));
        let response = CheckpointMutationResponse::for_request(
            &request,
            MutationOutcome::Rejected {
                reason: MutationRejection::StaleBase,
                current: Some(remote.snapshot().payload.boundary().unwrap()),
            },
        )
        .unwrap();
        assert_eq!(
            response.validate_for(&request).unwrap(),
            MutationOutcome::Rejected {
                reason: MutationRejection::StaleBase,
                current: Some(expected)
            }
        );
        assert_eq!(serde_json::to_vec(&remote.retained()).unwrap(), retained);
        assert!(
            !std::str::from_utf8(&retained)
                .unwrap()
                .contains(&ids[0].peer_id)
        );
    }

    #[test]
    fn conflicting_same_revision_is_not_acknowledged_as_synchronized() {
        let (ids, current) = fixture();
        let request = removal(&current, &ids);
        let mut expected_state = current.clone();
        expected_state
            .apply_mutation_at(&request.mutation, WALL_NOW)
            .unwrap();
        let expected = expected_state.snapshot().payload.boundary().unwrap();
        let mut remote = receiver(&current, &ids[1]);
        let conflicting = remote
            .sign_mutation_at(
                &ids[1],
                MembershipChange::RemoveMember(ids[2].peer_id.clone()),
                WALL_NOW,
            )
            .unwrap();
        remote.apply_mutation_at(&conflicting, WALL_NOW).unwrap();
        assert!(matches!(
            remote.apply_mutation_at(&request.mutation, WALL_NOW),
            Err(CheckpointError::StaleMutation)
        ));
        let boundary = remote.snapshot().payload.boundary().unwrap();
        assert_eq!(boundary.authority_revision, expected.authority_revision);
        assert_ne!(boundary, expected);
        let reply = CheckpointMutationResponse::for_request(
            &request,
            MutationOutcome::Rejected {
                reason: MutationRejection::StaleBase,
                current: Some(boundary),
            },
        )
        .unwrap();
        assert_eq!(reply.validate_for(&request).unwrap(), reply.outcome);
        assert_ne!(boundary, expected);
    }

    #[test]
    fn acknowledgment_digest_binds_every_command_field_and_mixed_replies_fail() {
        let (ids, current) = fixture();
        let first = removal(&current, &ids);
        let second = command(
            &current,
            &ids[0],
            MembershipChange::RemoveMember(ids[2].peer_id.clone()),
            WALL_NOW,
        );
        let reply = CheckpointMutationResponse::for_request(
            &first,
            MutationOutcome::Rejected {
                reason: MutationRejection::Busy,
                current: None,
            },
        )
        .unwrap();
        assert!(matches!(
            reply.validate_for(&second),
            Err(CheckpointMutationError::ResponseMismatch)
        ));
        let mut changed = first.clone();
        changed.mutation.payload.base.authority_revision += 1;
        resign(&mut changed, &ids[0]);
        assert_ne!(changed.digest().unwrap(), first.digest().unwrap());
    }

    #[test]
    fn malformed_envelopes_key_fields_and_boundaries_fail_closed() {
        let (ids, current) = fixture();
        let request = removal(&current, &ids);
        for variant in 0..12 {
            let mut bad = request.clone();
            match variant {
                0 => bad.version = 2,
                1 => bad.mutation.payload.version = 2,
                2 => bad.mutation.payload.anchor.rank_version = 2,
                3 => bad.mutation.payload.anchor.network_id = [0; 32],
                4 => bad.mutation.payload.base.digest = [0; 32],
                5 => {
                    bad.mutation.payload.base.authority_revision =
                        MAX_MEMBERSHIP_RECORD_INTEGER + 1;
                }
                6 => bad.mutation.payload.issuer.peer_id = "x".repeat(129),
                7 => {
                    bad.mutation.payload.issuer.public_key =
                        "x".repeat(MAX_KEY_OR_SIGNATURE_BYTES + 1);
                }
                8 => bad.mutation.signature = "x".repeat(MAX_KEY_OR_SIGNATURE_BYTES + 1),
                9 => bad.mutation.signature.clear(),
                10 => {
                    bad.mutation.payload.change =
                        MembershipChange::RemoveMember("not a peer".to_owned());
                }
                _ => {
                    bad.mutation.payload.change = MembershipChange::SetPolicy(SnapshotPolicy {
                        max_active_members: 0,
                        route_grants_enabled: true,
                    });
                }
            }
            assert!(
                CheckpointMutationRequest::new(bad.mutation.clone()).is_err() || bad.version != 1
            );
            assert!(bad.validate_for(peer(&ids[0]), &anchor()).is_err());
        }
        for boundary in [
            CheckpointBoundary {
                authority_revision: 0,
                digest: [0; 32],
            },
            CheckpointBoundary {
                authority_revision: MAX_MEMBERSHIP_RECORD_INTEGER + 1,
                digest: [1; 32],
            },
        ] {
            assert!(
                CheckpointMutationResponse::for_request(
                    &request,
                    MutationOutcome::Applied(boundary)
                )
                .is_err()
            );
            assert!(
                CheckpointMutationResponse::for_request(
                    &request,
                    MutationOutcome::Rejected {
                        reason: MutationRejection::StaleBase,
                        current: Some(boundary)
                    }
                )
                .is_err()
            );
        }
    }

    #[test]
    fn oversized_and_noncanonical_upserts_never_pass_wire_shape_validation() {
        let (ids, current) = fixture();
        let member = current.snapshot().payload.member(&ids[1].peer_id).unwrap();
        for variant in 0..10 {
            let mut bad = member.clone();
            match variant {
                0 => bad.incarnation = [0; 32],
                1 => bad.roles.clear(),
                2 => {
                    bad.roles = vec![
                        MembershipRole::RouteAuthority,
                        MembershipRole::OverlayMember,
                    ];
                }
                3 => bad.route_grants = vec![route("10.0.0.0/8", 0)],
                4 => {
                    bad.roles.push(MembershipRole::RouteAuthority);
                    bad.route_grants = vec![route("10.0.0.0/8", 0); MAX_CHECKPOINT_ROUTES + 1];
                }
                5 => {
                    bad.roles.push(MembershipRole::RouteAuthority);
                    bad.route_grants = vec![route("10.1.2.3/8", 0)];
                }
                6 => {
                    bad.roles.push(MembershipRole::RouteAuthority);
                    bad.route_grants = vec![route("x".repeat(65), 0)];
                }
                7 => {
                    bad.roles.push(MembershipRole::RouteAuthority);
                    bad.route_grants = vec![route("10.0.0.0/8", 0); 2];
                }
                8 => bad.expires_at_unix_seconds = Some(MAX_MEMBERSHIP_RECORD_INTEGER + 1),
                _ => bad.subject.public_key.push(' '),
            }
            let mut request = removal(&current, &ids);
            request.mutation.payload.change = MembershipChange::UpsertMember(bad);
            resign(&mut request, &ids[0]);
            assert!(request.validate().is_err(), "case {variant}");
        }
    }

    #[tokio::test]
    async fn every_rejection_round_trips_without_freeform_history() {
        let (ids, current) = fixture();
        let request = removal(&current, &ids);
        let protocol = StreamProtocol::new(CHECKPOINT_MUTATION_PROTOCOL);
        let mut codec = CheckpointMutationCodec;
        for reason in [
            MutationRejection::Unauthorized,
            MutationRejection::StaleBase,
            MutationRejection::ResyncRequired,
            MutationRejection::Invalid,
            MutationRejection::Busy,
            MutationRejection::PersistenceFailed,
        ] {
            let reply = CheckpointMutationResponse::for_request(
                &request,
                MutationOutcome::Rejected {
                    reason,
                    current: Some(current.snapshot().payload.boundary().unwrap()),
                },
            )
            .unwrap();
            let mut bytes = Cursor::new(Vec::new());
            codec
                .write_response(&protocol, &mut bytes, reply.clone())
                .await
                .unwrap();
            bytes.set_position(0);
            assert_eq!(
                codec.read_response(&protocol, &mut bytes).await.unwrap(),
                reply
            );
        }
    }

    #[tokio::test]
    async fn codec_rejects_size_before_reading_or_allocating_body_and_truncation() {
        let mut codec = CheckpointMutationCodec;
        let protocol = StreamProtocol::new(CHECKPOINT_MUTATION_PROTOCOL);
        for size in [0, MAX_MUTATION_MESSAGE_BYTES + 1, usize::from(u16::MAX)] {
            let header = u16::try_from(size).unwrap().to_be_bytes().to_vec();
            let mut input = Cursor::new(header.clone());
            assert_eq!(
                codec
                    .read_request(&protocol, &mut input)
                    .await
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(input.position(), 2);
            let mut input = Cursor::new(header);
            assert_eq!(
                codec
                    .read_response(&protocol, &mut input)
                    .await
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(input.position(), 2);
        }
        let mut truncated = Cursor::new(vec![0, 12, b'{']);
        assert_eq!(
            codec
                .read_request(&protocol, &mut truncated)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            bounded_encoding(&"x".repeat(MAX_MUTATION_MESSAGE_BYTES - 2))
                .unwrap()
                .len(),
            MAX_MUTATION_MESSAGE_BYTES
        );
        assert!(bounded_encoding(&"x".repeat(MAX_MUTATION_MESSAGE_BYTES - 1)).is_err());
    }

    #[tokio::test]
    async fn codec_rejects_unknown_nested_fields_duplicate_fields_and_versions() {
        let (ids, current) = fixture();
        let request = removal(&current, &ids);
        let protocol = StreamProtocol::new(CHECKPOINT_MUTATION_PROTOCOL);
        let mut codec = CheckpointMutationCodec;
        for variant in 0..7 {
            let mut value = serde_json::to_value(&request).unwrap();
            match variant {
                0 => value["extra"] = true.into(),
                1 => value["mutation"]["extra"] = true.into(),
                2 => value["mutation"]["payload"]["extra"] = true.into(),
                3 => value["mutation"]["payload"]["anchor"]["extra"] = true.into(),
                4 => value["mutation"]["payload"]["issuer"]["extra"] = true.into(),
                5 => value["version"] = 2.into(),
                _ => value["mutation"]["payload"]["version"] = 2.into(),
            }
            assert_eq!(
                codec
                    .read_request(&protocol, &mut framed(&value))
                    .await
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
        let duplicate = format!(
            "{{\"version\":1,\"version\":1,\"mutation\":{}}}",
            serde_json::to_string(&request.mutation).unwrap()
        );
        let mut bytes = u16::try_from(duplicate.len())
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bytes.extend(duplicate.bytes());
        assert!(
            codec
                .read_request(&protocol, &mut Cursor::new(bytes))
                .await
                .is_err()
        );
        let reply = CheckpointMutationResponse::for_request(
            &request,
            MutationOutcome::Rejected {
                reason: MutationRejection::Invalid,
                current: None,
            },
        )
        .unwrap();
        for variant in 0..3 {
            let mut value = serde_json::to_value(&reply).unwrap();
            match variant {
                0 => value["version"] = 2.into(),
                1 => value["extra"] = true.into(),
                _ => value["outcome"]["rejected"]["reason"] = "unrecognized".into(),
            }
            assert!(
                codec
                    .read_response(&protocol, &mut framed(&value))
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn wrong_protocol_and_invalid_outbound_frames_write_nothing() {
        let (ids, current) = fixture();
        let mut request = removal(&current, &ids);
        let mut codec = CheckpointMutationCodec;
        let wrong = StreamProtocol::new("/p2p-vpn/control/1");
        let protocol = StreamProtocol::new(CHECKPOINT_MUTATION_PROTOCOL);
        let mut output = Cursor::new(Vec::new());
        assert!(
            codec
                .write_request(&wrong, &mut output, request.clone())
                .await
                .is_err()
        );
        assert!(output.get_ref().is_empty());
        request.mutation.signature = "x".repeat(MAX_MUTATION_MESSAGE_BYTES);
        assert!(
            codec
                .write_request(&protocol, &mut output, request)
                .await
                .is_err()
        );
        assert!(output.get_ref().is_empty());
        let mut input = Cursor::new(vec![0; 10]);
        assert!(codec.read_request(&wrong, &mut input).await.is_err());
        assert_eq!(input.position(), 0);
    }

    #[tokio::test]
    async fn expired_queued_request_writes_zero_bytes_before_validation() {
        let (ids, current) = fixture();
        let mut request = removal(&current, &ids).with_deadline(Instant::now());
        // Expiry wins even if a stale queued value has other invalid fields.
        request.mutation.signature.clear();
        let mut output = Cursor::new(Vec::new());
        let error = CheckpointMutationCodec
            .write_request(
                &StreamProtocol::new(CHECKPOINT_MUTATION_PROTOCOL),
                &mut output,
                request,
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(output.get_ref().is_empty());
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    enum WriteBlock {
        Header,
        Body,
        Close,
    }

    struct SlowWriter {
        block: WriteBlock,
        bytes: Vec<u8>,
    }

    impl AsyncWrite for SlowWriter {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            if self.block == WriteBlock::Header
                || (self.block == WriteBlock::Body && self.bytes.len() >= 2)
            {
                return std::task::Poll::Pending;
            }
            self.bytes.extend_from_slice(bytes);
            std::task::Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            if self.block == WriteBlock::Close {
                std::task::Poll::Pending
            } else {
                std::task::Poll::Ready(Ok(()))
            }
        }
    }

    #[tokio::test]
    async fn original_sender_deadline_bounds_header_body_and_close_writes() {
        let (ids, current) = fixture();
        let request = removal(&current, &ids);
        for block in [WriteBlock::Header, WriteBlock::Body, WriteBlock::Close] {
            let deadline = Instant::now() + Duration::from_millis(100);
            let mut writer = SlowWriter {
                block,
                bytes: vec![],
            };
            let error = tokio::time::timeout(
                Duration::from_secs(2),
                CheckpointMutationCodec.write_request(
                    &StreamProtocol::new(CHECKPOINT_MUTATION_PROTOCOL),
                    &mut writer,
                    request.clone().with_deadline(deadline),
                ),
            )
            .await
            .expect("sender write must stop at its original deadline")
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(Instant::now() >= deadline);
            match block {
                WriteBlock::Header => assert!(writer.bytes.is_empty()),
                WriteBlock::Body => assert_eq!(writer.bytes.len(), 2),
                WriteBlock::Close => assert!(writer.bytes.len() > 2),
            }
        }
    }

    #[tokio::test]
    async fn local_deadline_changes_neither_wire_signature_nor_digest_and_is_not_decoded() {
        let (ids, current) = fixture();
        let request = removal(&current, &ids);
        assert_eq!(request.outbound_deadline, None);
        let deadline = Instant::now() + Duration::from_secs(2);
        let bounded = request.clone().with_deadline(deadline);
        assert_eq!(bounded.mutation, request.mutation);
        assert_eq!(bounded.digest().unwrap(), request.digest().unwrap());
        assert_eq!(
            serde_json::to_vec(&bounded).unwrap(),
            serde_json::to_vec(&request).unwrap()
        );
        let protocol = StreamProtocol::new(CHECKPOINT_MUTATION_PROTOCOL);
        let mut codec = CheckpointMutationCodec;
        let mut output = Cursor::new(Vec::new());
        codec
            .write_request(&protocol, &mut output, bounded)
            .await
            .unwrap();
        assert_eq!(output.get_ref(), framed(&request).get_ref());
        output.set_position(0);
        let decoded = codec.read_request(&protocol, &mut output).await.unwrap();
        assert_eq!(decoded.outbound_deadline, None);
        assert_eq!(decoded, request);
        decoded.validate_for(peer(&ids[0]), &anchor()).unwrap();
        let mut injected = serde_json::to_value(&request).unwrap();
        injected["outbound_deadline"] = true.into();
        assert!(serde_json::from_value::<CheckpointMutationRequest>(injected).is_err());
    }

    #[test]
    fn reattaching_a_sender_deadline_cannot_extend_it_or_exceed_the_lifetime_cap() {
        let (ids, current) = fixture();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(2);
        let request = removal(&current, &ids)
            .with_deadline(deadline)
            .with_deadline(now + Duration::from_secs(10));
        assert_eq!(request.outbound_deadline, Some(deadline));
        let shortened = request.with_deadline(now + Duration::from_secs(1));
        assert_eq!(
            shortened.outbound_deadline,
            Some(now + Duration::from_secs(1))
        );
        let capped = removal(&current, &ids).with_deadline(now + Duration::from_hours(1));
        assert!(
            capped.outbound_deadline.unwrap() <= Instant::now() + MAX_MUTATION_HANDOFF_LIFETIME
        );
    }

    #[test]
    fn stream_count_and_request_deadline_are_hard_bounded() {
        assert_eq!(stream_limit(0), 1);
        assert_eq!(stream_limit(8), 8);
        assert_eq!(stream_limit(usize::MAX), MAX_MUTATION_STREAMS);
        assert!(MUTATION_REQUEST_TIMEOUT <= MAX_MUTATION_HANDOFF_LIFETIME);
        assert!(MAX_MUTATION_HANDOFF_LIFETIME <= Duration::from_mins(1));
    }

    fn swarm(
        identity: &NodeIdentity,
    ) -> Swarm<request_response::Behaviour<CheckpointMutationCodec>> {
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
    async fn two_swarms_deliver_final_self_departure_then_reject_exact_base_replay() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let (ids, mut local) = fixture();
            let mut remote = receiver(&local, &ids[1]);
            let request = removal(&local, &ids);
            // Capture before exclusion; only this command is sent after departure.
            local.apply_mutation_at(&request.mutation, WALL_NOW).unwrap();
            assert_eq!(local.sync_state(), MembershipSyncState::Excluded);
            let expected = local.snapshot().payload.boundary().unwrap();
            let mut sender = swarm(&ids[0]);
            let mut recipient = swarm(&ids[1]);
            recipient.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
            let address = loop {
                if let SwarmEvent::NewListenAddr { address, .. } = recipient.select_next_some().await {
                    break address;
                }
            };
            sender.add_peer_address(peer(&ids[1]), address);
            let mut pending = sender.behaviour_mut().send_request(&peer(&ids[1]), request.clone());
            let mut responses = 0;
            loop {
                tokio::select! {
                    event = recipient.select_next_some() => match event {
                        SwarmEvent::Behaviour(request_response::Event::Message { peer: authenticated_peer,
                            message: request_response::Message::Request { request: delivered, channel, .. }, .. }) => {
                            let mutation = delivered.validate_for(authenticated_peer, &anchor()).unwrap();
                            let outcome = match remote.apply_mutation_at(mutation, WALL_NOW) {
                                Ok(()) => MutationOutcome::Applied(remote.snapshot().payload.boundary().unwrap()),
                                Err(CheckpointError::StaleMutation) => MutationOutcome::Rejected {
                                    reason: MutationRejection::StaleBase,
                                    current: Some(remote.snapshot().payload.boundary().unwrap())
                                },
                                Err(error) => panic!("core application: {error:?}"),
                            };
                            let reply = CheckpointMutationResponse::for_request(&delivered, outcome).unwrap();
                            recipient.behaviour_mut().send_response(channel, reply).unwrap();
                        },
                        SwarmEvent::Behaviour(request_response::Event::InboundFailure { error, .. }) => panic!("inbound: {error:?}"),
                        _ => (),
                    },
                    event = sender.select_next_some() => match event {
                        SwarmEvent::Behaviour(request_response::Event::Message { peer: authenticated_peer,
                            message: request_response::Message::Response { request_id, response }, .. }) => {
                            assert_eq!(authenticated_peer, peer(&ids[1]));
                            assert_eq!(request_id, pending);
                            responses += 1;
                            let expected_outcome = if responses == 1 {
                                MutationOutcome::Applied(expected)
                            } else { MutationOutcome::Rejected {
                                reason: MutationRejection::StaleBase, current: Some(expected)
                            }};
                            assert_eq!(response.validate_for(&request).unwrap(), expected_outcome);
                            if responses == 2 { break; }
                            pending = sender.behaviour_mut().send_request(&authenticated_peer, request.clone());
                        },
                        SwarmEvent::Behaviour(request_response::Event::OutboundFailure { error, .. }) => panic!("outbound: {error:?}"),
                        _ => (),
                    },
                }
            }
            assert_eq!(remote.snapshot(), local.snapshot());
            assert_eq!(remote.sync_state(), MembershipSyncState::Participating);
            assert!(remote.snapshot().payload.member(&ids[0].peer_id).is_none());
            assert!(remote.snapshot().payload.member(&ids[2].peer_id).is_some());
        }).await.expect("mutation loopback protocol exceeded 15 seconds");
    }
}
