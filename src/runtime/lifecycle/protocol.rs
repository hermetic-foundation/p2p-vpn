//! Bounded lifecycle exchange over an authenticated libp2p stream.
//!
//! A decoded frame is not permission to change peer policy. The dispatcher must
//! validate it against a current `Session` and authorization before applying it.
//! This module does not register or advertise support in the production host.

use std::{fmt, io, time::Duration};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{PeerId, StreamProtocol, request_response};
use serde::{Deserialize, Serialize};

use super::{Epoch, IdleAck, IdleOffer, Rejection, Wake};
use crate::runtime::control::membership_tag_matches;

pub const PROTOCOL: &str = "/p2p-vpn/lifecycle/1";
pub const VERSION: u8 = 1;
pub const MAX_MESSAGE_BYTES: usize = 2048;
pub const MAX_NETWORK_NAME_BYTES: usize = 128;
pub const MAX_MEMBERSHIP_TAG_BYTES: usize = 64;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub network_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership_tag: Option<String>,
}

impl Scope {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !supported_scope(&self.network_name, self.membership_tag.as_deref()) {
            return Err(ValidationError::InvalidScope);
        }
        Ok(())
    }
}

#[must_use]
pub fn supported_scope(network_name: &str, membership_tag: Option<&str>) -> bool {
    !network_name.is_empty()
        && network_name.len() <= MAX_NETWORK_NAME_BYTES
        && membership_tag.is_none_or(|tag| !tag.is_empty() && tag.len() <= MAX_MEMBERSHIP_TAG_BYTES)
}

/// The intent is echoed verbatim in a response, not reversed or reissued.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub sender_epoch: [u8; 16],
    pub receiver_epoch: [u8; 16],
    pub revision: u64,
}

impl From<IdleOffer> for Intent {
    fn from(value: IdleOffer) -> Self {
        Self {
            sender_epoch: value.sender_epoch.as_bytes(),
            receiver_epoch: value.receiver_epoch.as_bytes(),
            revision: value.revision,
        }
    }
}

impl From<Wake> for Intent {
    fn from(value: Wake) -> Self {
        Self {
            sender_epoch: value.sender_epoch.as_bytes(),
            receiver_epoch: value.receiver_epoch.as_bytes(),
            revision: value.revision,
        }
    }
}

impl Intent {
    fn offer(self) -> Result<IdleOffer, ValidationError> {
        let sender_epoch = Epoch::new(self.sender_epoch).ok_or(ValidationError::InvalidIntent)?;
        let receiver_epoch =
            Epoch::new(self.receiver_epoch).ok_or(ValidationError::InvalidIntent)?;
        if self.revision == 0 {
            return Err(ValidationError::InvalidIntent);
        }
        Ok(IdleOffer {
            sender_epoch,
            receiver_epoch,
            revision: self.revision,
        })
    }

    fn check_epochs(self, sender: Epoch, receiver: Epoch) -> Result<(), ValidationError> {
        if self.sender_epoch != sender.as_bytes() || self.receiver_epoch != receiver.as_bytes() {
            return Err(ValidationError::WrongEpoch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Idle,
    Wake,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u8,
    pub scope: Scope,
    pub intent: Intent,
    pub command: Command,
}

impl Request {
    #[must_use]
    pub fn idle(scope: Scope, offer: IdleOffer) -> Self {
        Self {
            version: VERSION,
            scope,
            intent: offer.into(),
            command: Command::Idle,
        }
    }

    #[must_use]
    pub fn wake(scope: Scope, wake: Wake) -> Self {
        Self {
            version: VERSION,
            scope,
            intent: wake.into(),
            command: Command::Wake,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    Busy,
    NoIncomingPath,
    NotIdle,
    Unsupported,
    Stale,
    RateLimited,
    Unauthorized,
}

impl From<Rejection> for RejectCode {
    fn from(value: Rejection) -> Self {
        match value {
            Rejection::Unsupported | Rejection::RevisionExhausted => Self::Unsupported,
            Rejection::WrongEpoch
            | Rejection::StaleRevision
            | Rejection::StaleAcknowledgement
            | Rejection::ExchangeExpired => Self::Stale,
            Rejection::OutstandingWork | Rejection::ExchangePending => Self::Busy,
            Rejection::IncomingPathUnavailable => Self::NoIncomingPath,
            Rejection::RecentlyActive | Rejection::AlreadyDormant => Self::NotIdle,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    IdleAccepted { responder_revision: u64 },
    // A unit variant would ignore extra fields despite deny_unknown_fields.
    WakeAccepted {},
    Rejected { reason: RejectCode },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u8,
    pub scope: Scope,
    pub intent: Intent,
    pub reply: Reply,
}

impl Response {
    #[must_use]
    pub fn for_request(request: &Request, reply: Reply) -> Self {
        Self {
            version: VERSION,
            scope: request.scope.clone(),
            intent: request.intent,
            reply,
        }
    }

    pub fn idle_accepted(request: &Request, ack: IdleAck) -> Result<Self, ValidationError> {
        if request.command != Command::Idle || request.intent != Intent::from(ack.offer) {
            return Err(ValidationError::ResponseMismatch);
        }
        Ok(Self::for_request(
            request,
            Reply::IdleAccepted {
                responder_revision: ack.responder_revision,
            },
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidatedRequest {
    Idle(IdleOffer),
    Wake(Wake),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidatedResponse {
    IdleAccepted(IdleAck),
    WakeAccepted,
    Rejected(RejectCode),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    UnauthorizedPeer,
    UnsupportedVersion,
    InvalidScope,
    WrongNetwork,
    MembershipMismatch,
    InvalidIntent,
    WrongEpoch,
    ResponseMismatch,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::UnauthorizedPeer => "unauthorized lifecycle peer",
            Self::UnsupportedVersion => "unsupported lifecycle version",
            Self::InvalidScope => "invalid lifecycle scope",
            Self::WrongNetwork => "wrong lifecycle network",
            Self::MembershipMismatch => "wrong lifecycle membership tag",
            Self::InvalidIntent => "invalid lifecycle intent",
            Self::WrongEpoch => "wrong lifecycle runtime epoch",
            Self::ResponseMismatch => "lifecycle response does not match its request",
        })
    }
}

impl std::error::Error for ValidationError {}

/// Construct from the validated capability exchange, not the message's claims.
/// The authorization callback must consult current membership on every call.
pub struct Session<'a> {
    pub peer: PeerId,
    pub local_epoch: Epoch,
    pub remote_epoch: Epoch,
    pub network_name: &'a str,
    pub membership_tag: Option<&'a str>,
    pub previous_membership_tags: &'a [String],
}

impl Session<'_> {
    pub fn validate_request(
        &self,
        authenticated_peer: PeerId,
        authorized_now: impl FnOnce(PeerId) -> bool,
        request: &Request,
    ) -> Result<ValidatedRequest, ValidationError> {
        self.authorize(authenticated_peer, authorized_now)?;
        request.validate()?;
        self.validate_scope(&request.scope)?;
        request
            .intent
            .check_epochs(self.remote_epoch, self.local_epoch)?;
        let offer = request.intent.offer()?;
        Ok(match request.command {
            Command::Idle => ValidatedRequest::Idle(offer),
            Command::Wake => ValidatedRequest::Wake(Wake {
                sender_epoch: offer.sender_epoch,
                receiver_epoch: offer.receiver_epoch,
                revision: offer.revision,
            }),
        })
    }

    pub fn validate_response(
        &self,
        authenticated_peer: PeerId,
        authorized_now: impl FnOnce(PeerId) -> bool,
        request: &Request,
        response: &Response,
    ) -> Result<ValidatedResponse, ValidationError> {
        self.authorize(authenticated_peer, authorized_now)?;
        request.validate()?;
        response.validate()?;
        self.validate_scope(&request.scope)?;
        request
            .intent
            .check_epochs(self.local_epoch, self.remote_epoch)?;
        if response.scope != request.scope || response.intent != request.intent {
            return Err(ValidationError::ResponseMismatch);
        }
        match (request.command, response.reply) {
            (Command::Idle, Reply::IdleAccepted { responder_revision }) => {
                Ok(ValidatedResponse::IdleAccepted(IdleAck {
                    offer: request.intent.offer()?,
                    responder_revision,
                }))
            }
            (Command::Wake, Reply::WakeAccepted {}) => Ok(ValidatedResponse::WakeAccepted),
            (_, Reply::Rejected { reason }) => Ok(ValidatedResponse::Rejected(reason)),
            _ => Err(ValidationError::ResponseMismatch),
        }
    }

    fn authorize(
        &self,
        authenticated_peer: PeerId,
        authorized_now: impl FnOnce(PeerId) -> bool,
    ) -> Result<(), ValidationError> {
        if authenticated_peer != self.peer || !authorized_now(authenticated_peer) {
            return Err(ValidationError::UnauthorizedPeer);
        }
        Ok(())
    }

    fn validate_scope(&self, scope: &Scope) -> Result<(), ValidationError> {
        if scope.network_name != self.network_name {
            return Err(ValidationError::WrongNetwork);
        }
        if !membership_tag_matches(
            scope.membership_tag.as_deref(),
            self.membership_tag,
            self.previous_membership_tags,
        ) {
            return Err(ValidationError::MembershipMismatch);
        }
        Ok(())
    }
}

trait WireMessage: Serialize + for<'de> Deserialize<'de> {
    fn validate(&self) -> Result<(), ValidationError>;
}

fn validate_header(version: u8, scope: &Scope, intent: Intent) -> Result<(), ValidationError> {
    if version != VERSION {
        return Err(ValidationError::UnsupportedVersion);
    }
    scope.validate()?;
    intent.offer()?;
    Ok(())
}

impl WireMessage for Request {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_header(self.version, &self.scope, self.intent)
    }
}

impl WireMessage for Response {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_header(self.version, &self.scope, self.intent)
    }
}

#[derive(Clone, Debug, Default)]
pub struct LifecycleCodec;

#[async_trait]
impl request_response::Codec for LifecycleCodec {
    type Protocol = StreamProtocol;
    type Request = Request;
    type Response = Response;

    async fn read_request<T>(&mut self, _: &Self::Protocol, stream: &mut T) -> io::Result<Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(stream).await
    }

    async fn read_response<T>(&mut self, _: &Self::Protocol, stream: &mut T) -> io::Result<Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_frame(stream).await
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        stream: &mut T,
        value: Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(stream, &value).await
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        stream: &mut T,
        value: Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_frame(stream, &value).await
    }
}

#[must_use]
/// The libp2p cap is per connection, not a peer/network request-rate budget.
pub fn behaviour(
    max_concurrent_streams_per_connection: usize,
) -> request_response::Behaviour<LifecycleCodec> {
    request_response::Behaviour::with_codec(
        LifecycleCodec,
        [(
            StreamProtocol::new(PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(REQUEST_TIMEOUT)
            .with_max_concurrent_streams(max_concurrent_streams_per_connection.max(1)),
    )
}

async fn read_frame<T: AsyncRead + Unpin + Send, M: WireMessage>(stream: &mut T) -> io::Result<M> {
    let mut length = [0; 2];
    stream.read_exact(&mut length).await?;
    let length = usize::from(u16::from_be_bytes(length));
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(invalid_frame("lifecycle frame length out of bounds"));
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await?;
    let value: M = serde_json::from_slice(&payload)
        .map_err(|_| invalid_frame("invalid lifecycle JSON message"))?;
    value
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut extra = [0];
    if stream.read(&mut extra).await? != 0 {
        return Err(invalid_frame("trailing lifecycle frame bytes"));
    }
    Ok(value)
}

async fn write_frame<T: AsyncWrite + Unpin + Send, M: WireMessage>(
    stream: &mut T,
    value: &M,
) -> io::Result<()> {
    value
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let payload = serde_json::to_vec(value)
        .map_err(|_| invalid_frame("cannot encode lifecycle JSON message"))?;
    if payload.is_empty() || payload.len() > MAX_MESSAGE_BYTES {
        return Err(invalid_frame("lifecycle frame length out of bounds"));
    }
    let length = u16::try_from(payload.len())
        .map_err(|_| invalid_frame("lifecycle frame length out of bounds"))?;
    stream.write_all(&length.to_be_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.close().await
}

fn invalid_frame(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[path = "protocol/tests.rs"]
mod tests;
