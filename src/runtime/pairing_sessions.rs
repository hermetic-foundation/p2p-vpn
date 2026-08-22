use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use libp2p::{Multiaddr, PeerId as Libp2pPeerId, kad, request_response::OutboundRequestId};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use sha2_010::{Digest as _, Sha256};

use crate::{
    config::RouteConfig,
    pairing::{
        PAIRING_OFFER_VERSION, PairingAcceptanceMode, PairingOffer, PairingRequest, PairingResponse,
    },
    pairing_code::{PairingCode, PairingCodeSession, PendingPairingCodeHello},
    runtime::pairing_code::{PairingCodeRejectionReason, PairingCodeResponse},
};

pub const DEFAULT_CODE_PAIRING_EXPIRES_IN_SECONDS: u64 = 10 * 60;
pub const MAX_CODE_PAIRING_EXPIRES_IN_SECONDS: u64 = 60 * 60;
pub const CODE_PAIRING_LAN_GRACE: Duration = Duration::from_secs(3);
pub const CODE_PAIRING_TICK: Duration = Duration::from_secs(1);
pub const CODE_PAIRING_LAN_CANDIDATE_TTL: Duration = Duration::from_mins(2);
pub const CODE_PAIRING_PUBLIC_LOOKUP_INTERVAL: Duration = Duration::from_secs(10);
pub const MAX_CODE_PAIRING_LAN_CANDIDATES: usize = 128;
pub const MAX_CODE_PAIRING_LAN_ADDRESSES_PER_PEER: usize = 8;
pub const MAX_CODE_PAIRING_PEER_ATTEMPTS: usize = 128;
pub const MAX_PENDING_CODE_HELLOS: usize = 32;
pub const MAX_INBOUND_CODE_SESSIONS: usize = 8;
pub const CODE_PAIRING_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub const MAX_CODE_PAIRING_ENROLLMENTS: usize = 256;
pub const MAX_RETAINED_PAIRING_TICKETS: usize = 32;

const OPERATION_ID_BYTES: usize = 16;
const APPROVAL_ID_BYTES: usize = 32;
const PAIRING_TICKET_BYTES: usize = 16;
const PERSISTED_PAIRING_STATE_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingDiscoveryStage {
    Lan,
    Public,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PairingOpenStatus {
    Searching {
        discovery: PairingDiscoveryStage,
        expires_at_unix_seconds: u64,
    },
    AwaitingApproval {
        approval_id: String,
        joiner_peer: String,
        requested_vpn_ip: Option<String>,
        requested_routes: Vec<RouteConfig>,
        expires_at_unix_seconds: u64,
    },
    Completed,
    Rejected,
    Cancelled,
    Expired,
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PairingJoinStatus {
    Searching {
        discovery: PairingDiscoveryStage,
        expires_at_unix_seconds: u64,
    },
    AwaitingApproval {
        inviter_peer: String,
        expires_at_unix_seconds: u64,
    },
    Completed,
    Cancelled,
    Expired,
    Failed {
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingOpenStarted {
    pub operation_id: String,
    pub code: String,
    pub expires_at_unix_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingJoinStarted {
    pub operation_id: String,
    pub expires_at_unix_seconds: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingEnrollmentRole {
    Inviter,
    Joiner,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingEnrollmentState {
    Prepared,
    Applied,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingEnrollment {
    pub operation_id: String,
    pub role: PairingEnrollmentRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<PairingOffer>,
    pub response: PairingResponse,
    #[serde(default)]
    pub transcript_sha256: String,
    #[serde(default)]
    pub completed_at_unix_seconds: Option<u64>,
    pub state: PairingEnrollmentState,
}

pub struct PairingEnrollmentPreparation {
    pub operation_id: String,
    pub role: PairingEnrollmentRole,
    pub approval_id: Option<String>,
    pub offer: Option<PairingOffer>,
    pub response: PairingResponse,
    pub transcript_sha256: String,
}

pub struct CodePairingSessions {
    open: Option<OpenOperation>,
    join: Option<JoinOperation>,
    enrollments: Vec<PairingEnrollment>,
    lan_candidates: HashMap<Libp2pPeerId, LanCandidate>,
    outbound_requests: HashMap<OutboundRequestId, OutboundCodeRequest>,
    inbound_sessions: HashMap<(Libp2pPeerId, String), InboundSession>,
    pending_approval: Option<PendingApproval>,
    inbound_ticket: Option<InboundTicket>,
    retained_inbound_tickets: VecDeque<InboundTicket>,
}

struct OpenOperation {
    id: String,
    code: Option<PairingCode>,
    locator: String,
    opened_at: Instant,
    expires_at_unix_seconds: u64,
    expires_in_seconds: u64,
    provider_advertised: bool,
    completed: Option<PairingResponse>,
    terminal: Option<TerminalStatus>,
}

struct JoinOperation {
    id: String,
    code: Option<PairingCode>,
    locator: String,
    started_at: Instant,
    expires_at_unix_seconds: u64,
    expires_in_seconds: u64,
    public_lookup_started: bool,
    next_public_lookup_at: Option<Instant>,
    public_lookup_query: Option<kad::QueryId>,
    attempted_peers: HashSet<Libp2pPeerId>,
    selected_inviter: Option<Libp2pPeerId>,
    remote_approval: Option<RemoteApproval>,
    requested_vpn_ip: Option<String>,
    requested_routes: Vec<RouteConfig>,
    completed: Option<(PairingOffer, PairingResponse)>,
    terminal: Option<TerminalStatus>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
enum TerminalStatus {
    Rejected,
    Cancelled,
    Expired,
    Failed(String),
}

struct LanCandidate {
    addresses: Vec<Multiaddr>,
    last_seen: Instant,
}

pub struct OutboundHello {
    pub operation_id: String,
    pub peer: Libp2pPeerId,
    pub pending: PendingPairingCodeHello,
}

pub struct InboundSession {
    pub operation_id: String,
    pub session: PairingCodeSession,
}

pub struct OutboundPairing {
    pub operation_id: String,
    pub peer: Libp2pPeerId,
    pub offer: PairingOffer,
    pub transcript_sha256: String,
}

pub enum OutboundCodeRequest {
    Hello(OutboundHello),
    Submit(OutboundPairing),
    Poll(OutboundPairing),
}

#[derive(Clone)]
pub struct PendingApproval {
    pub operation_id: String,
    pub approval_id: String,
    pub peer: Libp2pPeerId,
    pub ticket: String,
    pub expires_at_unix_seconds: u64,
    pub request: PairingRequest,
    pub transcript_sha256: String,
}

struct InboundTicket {
    operation_id: String,
    approval_id: String,
    peer: Libp2pPeerId,
    ticket: String,
    expires_at_unix_seconds: u64,
    outcome: InboundTicketOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
enum InboundTicketOutcome {
    Pending,
    Accepted(Box<PairingResponse>),
    Rejected(PairingCodeRejectionReason),
}

struct RemoteApproval {
    peer: Libp2pPeerId,
    ticket: String,
    offer: PairingOffer,
    transcript_sha256: String,
    next_poll_at: Instant,
    poll_in_flight: bool,
}

#[derive(Clone, Debug)]
pub struct PendingRemotePoll {
    pub operation_id: String,
    pub peer: Libp2pPeerId,
    pub ticket: String,
    pub offer: PairingOffer,
    pub transcript_sha256: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PairingExpiryActions {
    pub stop_providing_locator: Option<String>,
}

#[derive(Debug)]
pub enum CodePairingSessionError {
    Busy,
    InvalidExpiry,
    ExpiryOverflow,
    NotFound,
    NotAwaitingApproval,
    ApprovalMismatch,
    Capacity,
    InvalidCode(crate::pairing_code::PairingCodeError),
    Serialization(serde_json::Error),
    InvalidOperationId,
    InvalidTicket,
    Conflict,
    PersistedNetworkMismatch { expected: String, actual: String },
    InvalidPersistedState(String),
}

impl std::fmt::Display for CodePairingSessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("another code pairing operation is active"),
            Self::InvalidExpiry => write!(
                formatter,
                "pairing expiry must be between 1 and {MAX_CODE_PAIRING_EXPIRES_IN_SECONDS} seconds"
            ),
            Self::ExpiryOverflow => formatter.write_str("pairing expiry overflowed the clock"),
            Self::NotFound => formatter.write_str("pairing operation was not found"),
            Self::NotAwaitingApproval => {
                formatter.write_str("pairing operation is not awaiting approval")
            }
            Self::ApprovalMismatch => {
                formatter.write_str("pairing approval does not match the pending request")
            }
            Self::Capacity => formatter.write_str("too many pending pairing handshakes"),
            Self::InvalidCode(error) => write!(formatter, "invalid pairing code: {error:?}"),
            Self::Serialization(error) => {
                write!(formatter, "failed to encode pairing state: {error}")
            }
            Self::InvalidOperationId => formatter.write_str("invalid pairing operation id"),
            Self::InvalidTicket => formatter.write_str("invalid pairing polling ticket"),
            Self::Conflict => {
                formatter.write_str("pairing operation id conflicts with existing state")
            }
            Self::PersistedNetworkMismatch { expected, actual } => write!(
                formatter,
                "persisted pairing network mismatch: expected {expected}, got {actual}"
            ),
            Self::InvalidPersistedState(reason) => {
                write!(formatter, "invalid persisted pairing state: {reason}")
            }
        }
    }
}

impl std::error::Error for CodePairingSessionError {}

impl From<crate::pairing_code::PairingCodeError> for CodePairingSessionError {
    fn from(error: crate::pairing_code::PairingCodeError) -> Self {
        Self::InvalidCode(error)
    }
}

impl From<serde_json::Error> for CodePairingSessionError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

impl PendingApproval {
    pub fn new(
        operation_id: String,
        peer: Libp2pPeerId,
        expires_at_unix_seconds: u64,
        request: PairingRequest,
    ) -> Result<Self, CodePairingSessionError> {
        let approval_id = pairing_approval_id(&request)?;
        let transcript_sha256 = crate::pairing_code::pairing_request_transcript_sha256(&request)?;
        Ok(Self {
            operation_id,
            approval_id,
            peer,
            ticket: fresh_pairing_ticket(),
            expires_at_unix_seconds,
            request,
            transcript_sha256,
        })
    }
}

impl Default for CodePairingSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl CodePairingSessions {
    #[must_use]
    pub fn new() -> Self {
        Self {
            open: None,
            join: None,
            enrollments: Vec::new(),
            lan_candidates: HashMap::new(),
            outbound_requests: HashMap::new(),
            inbound_sessions: HashMap::new(),
            pending_approval: None,
            inbound_ticket: None,
            retained_inbound_tickets: VecDeque::new(),
        }
    }

    pub fn open(
        &mut self,
        network_name: &str,
        expires_in_seconds: u64,
        now_unix_seconds: u64,
        now: Instant,
    ) -> Result<PairingOpenStarted, CodePairingSessionError> {
        self.open_with_id(
            fresh_pairing_operation_id(),
            network_name,
            expires_in_seconds,
            now_unix_seconds,
            now,
        )
    }

    pub fn open_with_id(
        &mut self,
        operation_id: String,
        network_name: &str,
        expires_in_seconds: u64,
        now_unix_seconds: u64,
        now: Instant,
    ) -> Result<PairingOpenStarted, CodePairingSessionError> {
        validate_pairing_operation_id(&operation_id)?;
        validate_expiry(expires_in_seconds)?;
        if let Some(existing) = self.open.as_ref().filter(|open| open.id == operation_id) {
            if existing.expires_in_seconds != expires_in_seconds {
                return Err(CodePairingSessionError::Conflict);
            }
            let code = existing
                .code
                .as_ref()
                .ok_or(CodePairingSessionError::Conflict)?;
            return Ok(PairingOpenStarted {
                operation_id,
                code: code.to_string(),
                expires_at_unix_seconds: existing.expires_at_unix_seconds,
            });
        }
        if self
            .join
            .as_ref()
            .is_some_and(|join| join.id == operation_id)
        {
            return Err(CodePairingSessionError::Conflict);
        }
        self.ensure_idle()?;
        self.archive_inbound_ticket(now_unix_seconds)?;
        self.pending_approval = None;
        let expires_at_unix_seconds = now_unix_seconds
            .checked_add(expires_in_seconds)
            .ok_or(CodePairingSessionError::ExpiryOverflow)?;
        let code = PairingCode::generate();
        let locator = code.locator(network_name)?;
        let started = PairingOpenStarted {
            operation_id: operation_id.clone(),
            code: code.to_string(),
            expires_at_unix_seconds,
        };
        self.open = Some(OpenOperation {
            id: operation_id,
            code: Some(code),
            locator,
            opened_at: now,
            expires_at_unix_seconds,
            expires_in_seconds,
            provider_advertised: false,
            completed: None,
            terminal: None,
        });
        Ok(started)
    }

    pub fn join(
        &mut self,
        network_name: &str,
        code: PairingCode,
        requested_vpn_ip: Option<String>,
        requested_routes: Vec<RouteConfig>,
        expires_in_seconds: u64,
        now_unix_seconds: u64,
        now: Instant,
    ) -> Result<PairingJoinStarted, CodePairingSessionError> {
        self.join_with_id(
            fresh_pairing_operation_id(),
            network_name,
            code,
            requested_vpn_ip,
            requested_routes,
            expires_in_seconds,
            now_unix_seconds,
            now,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn join_with_id(
        &mut self,
        operation_id: String,
        network_name: &str,
        code: PairingCode,
        requested_vpn_ip: Option<String>,
        requested_routes: Vec<RouteConfig>,
        expires_in_seconds: u64,
        now_unix_seconds: u64,
        now: Instant,
    ) -> Result<PairingJoinStarted, CodePairingSessionError> {
        validate_pairing_operation_id(&operation_id)?;
        validate_expiry(expires_in_seconds)?;
        let locator = code.locator(network_name)?;
        if let Some(existing) = self.join.as_ref().filter(|join| join.id == operation_id) {
            if existing.expires_in_seconds != expires_in_seconds
                || existing.locator != locator
                || existing.requested_vpn_ip != requested_vpn_ip
                || existing.requested_routes != requested_routes
            {
                return Err(CodePairingSessionError::Conflict);
            }
            return Ok(PairingJoinStarted {
                operation_id,
                expires_at_unix_seconds: existing.expires_at_unix_seconds,
            });
        }
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.id == operation_id)
        {
            return Err(CodePairingSessionError::Conflict);
        }
        self.ensure_idle()?;
        let expires_at_unix_seconds = now_unix_seconds
            .checked_add(expires_in_seconds)
            .ok_or(CodePairingSessionError::ExpiryOverflow)?;
        self.join = Some(JoinOperation {
            id: operation_id.clone(),
            code: Some(code),
            locator,
            started_at: now,
            expires_at_unix_seconds,
            expires_in_seconds,
            public_lookup_started: false,
            next_public_lookup_at: None,
            public_lookup_query: None,
            attempted_peers: HashSet::new(),
            selected_inviter: None,
            remote_approval: None,
            requested_vpn_ip,
            requested_routes,
            completed: None,
            terminal: None,
        });
        Ok(PairingJoinStarted {
            operation_id,
            expires_at_unix_seconds,
        })
    }

    pub fn open_status(
        &self,
        operation_id: &str,
    ) -> Result<PairingOpenStatus, CodePairingSessionError> {
        let operation = self
            .open
            .as_ref()
            .filter(|operation| operation.id == operation_id)
            .ok_or(CodePairingSessionError::NotFound)?;
        if let Some(response) = &operation.completed {
            let _ = response;
            return Ok(PairingOpenStatus::Completed);
        }
        if let Some(terminal) = &operation.terminal {
            return Ok(match terminal {
                TerminalStatus::Rejected => PairingOpenStatus::Rejected,
                TerminalStatus::Cancelled => PairingOpenStatus::Cancelled,
                TerminalStatus::Expired => PairingOpenStatus::Expired,
                TerminalStatus::Failed(reason) => PairingOpenStatus::Failed {
                    reason: reason.clone(),
                },
            });
        }
        if let Some(approval) = self
            .pending_approval
            .as_ref()
            .filter(|approval| approval.operation_id == operation_id)
        {
            return Ok(PairingOpenStatus::AwaitingApproval {
                approval_id: approval.approval_id.clone(),
                joiner_peer: approval.request.payload.joiner_peer.clone(),
                requested_vpn_ip: approval.request.payload.requested_vpn_ip.clone(),
                requested_routes: approval.request.payload.requested_routes.clone(),
                expires_at_unix_seconds: operation.expires_at_unix_seconds,
            });
        }
        Ok(PairingOpenStatus::Searching {
            discovery: operation.discovery_stage(),
            expires_at_unix_seconds: operation.expires_at_unix_seconds,
        })
    }

    pub fn join_status(
        &self,
        operation_id: &str,
    ) -> Result<PairingJoinStatus, CodePairingSessionError> {
        let operation = self
            .join
            .as_ref()
            .filter(|operation| operation.id == operation_id)
            .ok_or(CodePairingSessionError::NotFound)?;
        if let Some((offer, response)) = &operation.completed {
            let _ = (offer, response);
            return Ok(PairingJoinStatus::Completed);
        }
        if let Some(terminal) = &operation.terminal {
            return Ok(match terminal {
                TerminalStatus::Cancelled => PairingJoinStatus::Cancelled,
                TerminalStatus::Expired => PairingJoinStatus::Expired,
                TerminalStatus::Failed(reason) => PairingJoinStatus::Failed {
                    reason: reason.clone(),
                },
                TerminalStatus::Rejected => PairingJoinStatus::Failed {
                    reason: "pairing request was rejected".to_owned(),
                },
            });
        }
        if let Some(inviter_peer) = operation.selected_inviter {
            return Ok(PairingJoinStatus::AwaitingApproval {
                inviter_peer: inviter_peer.to_string(),
                expires_at_unix_seconds: operation.expires_at_unix_seconds,
            });
        }
        Ok(PairingJoinStatus::Searching {
            discovery: operation.discovery_stage(),
            expires_at_unix_seconds: operation.expires_at_unix_seconds,
        })
    }

    pub fn cancel(
        &mut self,
        operation_id: &str,
    ) -> Result<PairingExpiryActions, CodePairingSessionError> {
        if self
            .open
            .as_ref()
            .is_some_and(|operation| operation.id == operation_id)
        {
            if let Some(ticket) = self.inbound_ticket.as_mut() {
                ticket.outcome =
                    InboundTicketOutcome::Rejected(PairingCodeRejectionReason::Unavailable);
            }
            let locator = self.deactivate_open(TerminalStatus::Cancelled);
            return Ok(PairingExpiryActions {
                stop_providing_locator: locator,
            });
        }
        if self
            .join
            .as_ref()
            .is_some_and(|operation| operation.id == operation_id)
        {
            self.deactivate_join(TerminalStatus::Cancelled);
            return Ok(PairingExpiryActions::default());
        }
        Err(CodePairingSessionError::NotFound)
    }

    pub fn reject(
        &mut self,
        operation_id: &str,
        approval_id: &str,
    ) -> Result<PairingExpiryActions, CodePairingSessionError> {
        let approval = self
            .pending_approval
            .as_ref()
            .ok_or(CodePairingSessionError::NotAwaitingApproval)?;
        if approval.operation_id != operation_id {
            return Err(CodePairingSessionError::NotFound);
        }
        if approval.approval_id != approval_id {
            return Err(CodePairingSessionError::ApprovalMismatch);
        }
        if let Some(ticket) = self.inbound_ticket.as_mut() {
            ticket.outcome =
                InboundTicketOutcome::Rejected(PairingCodeRejectionReason::UserRejected);
        }
        self.pending_approval.take();
        let locator = self.deactivate_open(TerminalStatus::Rejected);
        Ok(PairingExpiryActions {
            stop_providing_locator: locator,
        })
    }

    pub fn pending_approval(
        &self,
        operation_id: &str,
        approval_id: &str,
    ) -> Result<PendingApproval, CodePairingSessionError> {
        let approval = self
            .pending_approval
            .as_ref()
            .ok_or(CodePairingSessionError::NotAwaitingApproval)?;
        if approval.operation_id != operation_id {
            return Err(CodePairingSessionError::NotFound);
        }
        if approval.approval_id != approval_id {
            return Err(CodePairingSessionError::ApprovalMismatch);
        }
        Ok(approval.clone())
    }

    pub fn complete_open(
        &mut self,
        operation_id: &str,
        approval_id: &str,
        response: PairingResponse,
    ) -> Result<PairingExpiryActions, CodePairingSessionError> {
        let approval = self
            .pending_approval
            .as_ref()
            .ok_or(CodePairingSessionError::NotAwaitingApproval)?;
        if approval.operation_id != operation_id {
            return Err(CodePairingSessionError::NotFound);
        }
        if approval.approval_id != approval_id {
            return Err(CodePairingSessionError::ApprovalMismatch);
        }
        let ticket = self
            .inbound_ticket
            .as_mut()
            .filter(|ticket| {
                ticket.operation_id == operation_id && ticket.approval_id == approval_id
            })
            .ok_or(CodePairingSessionError::InvalidPersistedState(
                "pending approval is missing its polling ticket".to_owned(),
            ))?;
        ticket.accept(response.clone());
        self.pending_approval.take();
        let operation = self
            .open
            .as_mut()
            .filter(|operation| operation.id == operation_id)
            .ok_or(CodePairingSessionError::NotFound)?;
        operation.code.take();
        operation.completed = Some(response);
        let locator = operation
            .provider_advertised
            .then(|| operation.locator.clone());
        operation.provider_advertised = false;
        self.clear_transient_handshakes();
        Ok(PairingExpiryActions {
            stop_providing_locator: locator,
        })
    }

    pub fn response_for_existing_submission(
        &self,
        peer: Libp2pPeerId,
        request: &PairingRequest,
        now_unix_seconds: u64,
    ) -> Result<Option<PairingCodeResponse>, CodePairingSessionError> {
        let approval_id = pairing_approval_id(request)?;
        Ok(self
            .find_submission_ticket(peer, &approval_id)
            .map(|ticket| ticket.response(now_unix_seconds)))
    }

    #[must_use]
    pub fn poll_response(
        &self,
        peer: Libp2pPeerId,
        ticket: &str,
        now_unix_seconds: u64,
    ) -> PairingCodeResponse {
        self.find_polling_ticket(peer, ticket).map_or_else(
            || PairingCodeResponse::Rejected {
                reason: PairingCodeRejectionReason::Unavailable,
            },
            |state| state.response(now_unix_seconds),
        )
    }

    pub fn complete_join(
        &mut self,
        operation_id: &str,
        offer: PairingOffer,
        response: PairingResponse,
    ) -> Result<(), CodePairingSessionError> {
        let operation = self
            .join
            .as_mut()
            .filter(|operation| operation.id == operation_id)
            .ok_or(CodePairingSessionError::NotFound)?;
        operation.code.take();
        operation.remote_approval = None;
        operation.completed = Some((offer, response));
        self.clear_transient_handshakes();
        Ok(())
    }

    pub fn validate_prepared_recovery(
        &self,
        network_name: &str,
        enrollment: &PairingEnrollment,
    ) -> Result<(), CodePairingSessionError> {
        match enrollment.role {
            PairingEnrollmentRole::Inviter => {
                self.validate_prepared_open_recovery(network_name, enrollment)
            }
            PairingEnrollmentRole::Joiner => {
                self.validate_prepared_join_recovery(network_name, enrollment)
            }
        }
    }

    fn validate_prepared_open_recovery(
        &self,
        network_name: &str,
        enrollment: &PairingEnrollment,
    ) -> Result<(), CodePairingSessionError> {
        self.validate_durable_prepared_enrollment(
            network_name,
            enrollment,
            PairingEnrollmentRole::Inviter,
        )?;
        let approval_id = enrollment
            .approval_id
            .as_deref()
            .expect("validated inviter enrollment has an approval ID");
        let offer = enrollment
            .offer
            .as_ref()
            .expect("validated enrollment has a signed offer");
        let joiner =
            parse_enrollment_peer(&enrollment.response.payload.joiner_peer, "response joiner")?;
        let operation = self
            .open
            .as_ref()
            .ok_or(CodePairingSessionError::NotFound)?;
        if operation.id != enrollment.operation_id {
            return Err(CodePairingSessionError::Conflict);
        }
        if let Some(completed) = &operation.completed
            && completed != &enrollment.response
        {
            return Err(CodePairingSessionError::Conflict);
        }
        if operation.completed.is_none()
            && operation
                .terminal
                .as_ref()
                .is_some_and(|terminal| !matches!(terminal, TerminalStatus::Expired))
        {
            return Err(CodePairingSessionError::Conflict);
        }

        if let Some(approval) = &self.pending_approval {
            if approval.operation_id != enrollment.operation_id
                || approval.approval_id != approval_id
                || approval.peer != joiner
                || approval.transcript_sha256 != enrollment.transcript_sha256
                || approval.request.payload.inviter_peer != enrollment.response.payload.inviter_peer
                || approval.request.payload.joiner_peer != enrollment.response.payload.joiner_peer
                || approval.request.payload.rendezvous_token
                    != enrollment.response.payload.rendezvous_token
                || approval
                    .request
                    .offer
                    .as_ref()
                    .is_some_and(|request_offer| request_offer != offer)
                || (!approval.request.payload.offer_signature.is_empty()
                    && approval.request.payload.offer_signature != offer.signature)
            {
                return Err(invalid_recovery(
                    "prepared inviter enrollment does not match its approval request",
                ));
            }
        } else if operation.completed.is_none()
            && !matches!(operation.terminal, Some(TerminalStatus::Expired))
        {
            return Err(invalid_recovery(
                "prepared inviter enrollment is missing its approval request",
            ));
        }

        let ticket = self
            .inbound_ticket
            .as_ref()
            .ok_or_else(|| invalid_recovery("prepared inviter enrollment is missing its ticket"))?;
        if ticket.operation_id != enrollment.operation_id
            || ticket.approval_id != approval_id
            || ticket.peer != joiner
            || self.pending_approval.as_ref().is_some_and(|approval| {
                approval.ticket != ticket.ticket
                    || ticket.expires_at_unix_seconds < approval.expires_at_unix_seconds
            })
        {
            return Err(invalid_recovery(
                "prepared inviter enrollment does not match its polling ticket",
            ));
        }
        match &ticket.outcome {
            InboundTicketOutcome::Accepted(response)
                if response.as_ref() != &enrollment.response =>
            {
                return Err(CodePairingSessionError::Conflict);
            }
            InboundTicketOutcome::Rejected(PairingCodeRejectionReason::Expired)
                if matches!(operation.terminal, Some(TerminalStatus::Expired)) => {}
            InboundTicketOutcome::Pending | InboundTicketOutcome::Accepted(_) => {}
            InboundTicketOutcome::Rejected(_) => {
                return Err(CodePairingSessionError::Conflict);
            }
        }

        Ok(())
    }

    pub fn recover_prepared_open(
        &mut self,
        network_name: &str,
        enrollment: &PairingEnrollment,
    ) -> Result<PairingExpiryActions, CodePairingSessionError> {
        self.validate_prepared_open_recovery(network_name, enrollment)?;
        self.inbound_ticket
            .as_mut()
            .expect("ticket was validated immediately above")
            .accept(enrollment.response.clone());
        self.pending_approval.take();
        let operation = self
            .open
            .as_mut()
            .expect("operation was validated immediately above");
        let locator = if operation.completed.is_none() && operation.provider_advertised {
            Some(operation.locator.clone())
        } else {
            None
        };
        operation.code.take();
        operation.completed = Some(enrollment.response.clone());
        operation.terminal = None;
        operation.provider_advertised = false;
        self.clear_transient_handshakes();
        Ok(PairingExpiryActions {
            stop_providing_locator: locator,
        })
    }

    fn validate_prepared_join_recovery(
        &self,
        network_name: &str,
        enrollment: &PairingEnrollment,
    ) -> Result<(), CodePairingSessionError> {
        self.validate_durable_prepared_enrollment(
            network_name,
            enrollment,
            PairingEnrollmentRole::Joiner,
        )?;
        let offer = enrollment
            .offer
            .as_ref()
            .expect("validated enrollment has a signed offer");
        let inviter = parse_enrollment_peer(
            &enrollment.response.payload.inviter_peer,
            "response inviter",
        )?;
        let operation = self
            .join
            .as_ref()
            .ok_or(CodePairingSessionError::NotFound)?;
        if operation.id != enrollment.operation_id {
            return Err(CodePairingSessionError::Conflict);
        }
        if let Some((completed_offer, completed_response)) = &operation.completed {
            if completed_offer == offer && completed_response == &enrollment.response {
                return Ok(());
            }
            return Err(CodePairingSessionError::Conflict);
        }
        if operation
            .terminal
            .as_ref()
            .is_some_and(|terminal| !matches!(terminal, TerminalStatus::Expired))
        {
            return Err(CodePairingSessionError::Conflict);
        }
        let remote = operation.remote_approval.as_ref().ok_or_else(|| {
            invalid_recovery("prepared joiner enrollment is missing its remote approval")
        })?;
        if operation.selected_inviter != Some(inviter)
            || remote.peer != inviter
            || &remote.offer != offer
            || remote.transcript_sha256 != enrollment.transcript_sha256
        {
            return Err(invalid_recovery(
                "prepared joiner enrollment does not match its remote approval",
            ));
        }
        validate_pairing_ticket(&remote.ticket)?;

        Ok(())
    }

    pub fn recover_prepared_join(
        &mut self,
        network_name: &str,
        enrollment: &PairingEnrollment,
    ) -> Result<(), CodePairingSessionError> {
        self.validate_prepared_join_recovery(network_name, enrollment)?;
        let offer = enrollment
            .offer
            .as_ref()
            .expect("validated enrollment has a signed offer");
        let operation = self
            .join
            .as_mut()
            .expect("operation was validated immediately above");
        operation.code.take();
        operation.selected_inviter = None;
        operation.remote_approval = None;
        operation.completed = Some((offer.clone(), enrollment.response.clone()));
        operation.terminal = None;
        self.clear_transient_handshakes();
        Ok(())
    }

    #[must_use]
    pub fn open_completion(&self, operation_id: &str) -> Option<&PairingResponse> {
        self.open
            .as_ref()
            .filter(|operation| operation.id == operation_id)
            .and_then(|operation| operation.completed.as_ref())
    }

    #[must_use]
    pub fn join_completion(&self, operation_id: &str) -> Option<(&PairingOffer, &PairingResponse)> {
        self.join
            .as_ref()
            .filter(|operation| operation.id == operation_id)
            .and_then(|operation| operation.completed.as_ref())
            .map(|(offer, response)| (offer, response))
    }

    #[must_use]
    pub fn operation_expires_at(&self, operation_id: &str) -> Option<u64> {
        self.open
            .as_ref()
            .filter(|operation| operation.id == operation_id)
            .map(|operation| operation.expires_at_unix_seconds)
            .or_else(|| {
                self.join
                    .as_ref()
                    .filter(|operation| operation.id == operation_id)
                    .map(|operation| operation.expires_at_unix_seconds)
            })
    }

    pub fn fail_join(&mut self, operation_id: &str, reason: impl Into<String>) {
        if self
            .join
            .as_ref()
            .is_some_and(|operation| operation.id == operation_id)
        {
            self.deactivate_join(TerminalStatus::Failed(reason.into()));
        }
    }

    pub fn prepare_enrollment(
        &mut self,
        network_name: &str,
        preparation: PairingEnrollmentPreparation,
    ) -> Result<&PairingEnrollment, CodePairingSessionError> {
        validate_transcript_sha256(&preparation.transcript_sha256, false)?;
        let enrollment = PairingEnrollment {
            operation_id: preparation.operation_id,
            role: preparation.role,
            approval_id: preparation.approval_id,
            offer: preparation.offer,
            response: preparation.response,
            transcript_sha256: preparation.transcript_sha256,
            completed_at_unix_seconds: None,
            state: PairingEnrollmentState::Prepared,
        };
        validate_pairing_enrollment(&enrollment, network_name)?;

        if let Some(index) = self
            .enrollments
            .iter()
            .position(|existing| existing.operation_id == enrollment.operation_id)
        {
            let existing = &self.enrollments[index];
            if existing.has_same_preparation(&enrollment) {
                return Ok(existing);
            }
            return Err(CodePairingSessionError::Conflict);
        }
        if self.enrollments.len() >= MAX_CODE_PAIRING_ENROLLMENTS {
            return Err(CodePairingSessionError::Capacity);
        }

        self.enrollments.push(enrollment);
        Ok(self
            .enrollments
            .last()
            .expect("the enrollment was appended immediately above"))
    }

    #[must_use]
    pub fn enrollment(&self, operation_id: &str) -> Option<&PairingEnrollment> {
        self.enrollments
            .iter()
            .find(|enrollment| enrollment.operation_id == operation_id)
    }

    pub fn enrollments(&self) -> impl ExactSizeIterator<Item = &PairingEnrollment> {
        self.enrollments.iter()
    }

    pub fn mark_enrollment_applied(
        &mut self,
        operation_id: &str,
    ) -> Result<&PairingEnrollment, CodePairingSessionError> {
        let completed_at_unix_seconds = self
            .enrollment(operation_id)
            .ok_or(CodePairingSessionError::NotFound)?
            .response
            .payload
            .issued_at_unix_seconds;
        self.mark_enrollment_applied_at(operation_id, completed_at_unix_seconds)
    }

    pub fn mark_enrollment_applied_at(
        &mut self,
        operation_id: &str,
        completed_at_unix_seconds: u64,
    ) -> Result<&PairingEnrollment, CodePairingSessionError> {
        let enrollment = self
            .enrollments
            .iter_mut()
            .find(|enrollment| enrollment.operation_id == operation_id)
            .ok_or(CodePairingSessionError::NotFound)?;
        let completed_at_unix_seconds =
            completed_at_unix_seconds.max(enrollment.response.payload.issued_at_unix_seconds);
        enrollment
            .completed_at_unix_seconds
            .get_or_insert(completed_at_unix_seconds);
        enrollment.state = PairingEnrollmentState::Applied;
        Ok(enrollment)
    }

    fn validate_durable_prepared_enrollment(
        &self,
        network_name: &str,
        enrollment: &PairingEnrollment,
        expected_role: PairingEnrollmentRole,
    ) -> Result<(), CodePairingSessionError> {
        validate_pairing_enrollment(enrollment, network_name)?;
        if enrollment.role != expected_role || enrollment.state != PairingEnrollmentState::Prepared
        {
            return Err(CodePairingSessionError::Conflict);
        }
        let durable = self
            .enrollment(&enrollment.operation_id)
            .ok_or(CodePairingSessionError::NotFound)?;
        if durable != enrollment {
            return Err(CodePairingSessionError::Conflict);
        }
        Ok(())
    }

    pub fn encode_persisted(&self, network_name: &str) -> Result<Vec<u8>, CodePairingSessionError> {
        Ok(serde_json::to_vec_pretty(
            &PersistedCodePairingSessions::from_runtime(self, network_name),
        )?)
    }

    pub fn restore_persisted(
        bytes: &[u8],
        expected_network_name: &str,
        now_unix_seconds: u64,
        now: Instant,
    ) -> Result<Self, CodePairingSessionError> {
        let persisted: PersistedCodePairingSessions = serde_json::from_slice(bytes)?;
        if persisted.version != PERSISTED_PAIRING_STATE_VERSION {
            return Err(CodePairingSessionError::InvalidPersistedState(format!(
                "unsupported version {}",
                persisted.version
            )));
        }
        if persisted.network_name != expected_network_name {
            return Err(CodePairingSessionError::PersistedNetworkMismatch {
                expected: expected_network_name.to_owned(),
                actual: persisted.network_name,
            });
        }

        let enrollments =
            validate_restored_enrollments(persisted.enrollments, expected_network_name)?;
        let preserve_join_recovery = persisted.join.as_ref().is_some_and(|join| {
            enrollments.iter().any(|enrollment| {
                enrollment.operation_id == join.id
                    && enrollment.role == PairingEnrollmentRole::Joiner
                    && enrollment.state == PairingEnrollmentState::Prepared
            })
        });
        let resumed_at = now.checked_sub(CODE_PAIRING_LAN_GRACE).unwrap_or(now);
        let open = persisted
            .open
            .map(|open| open.into_runtime(expected_network_name, now_unix_seconds, resumed_at))
            .transpose()?;
        let join = persisted
            .join
            .map(|join| {
                join.into_runtime(
                    expected_network_name,
                    now_unix_seconds,
                    resumed_at,
                    preserve_join_recovery,
                )
            })
            .transpose()?;
        let mut pending_approval = persisted
            .pending_approval
            .map(PersistedPendingApproval::into_runtime)
            .transpose()?;
        let mut inbound_ticket = persisted
            .inbound_ticket
            .map(PersistedInboundTicket::into_runtime)
            .transpose()?;
        let mut retained_inbound_tickets = persisted
            .retained_inbound_tickets
            .into_iter()
            .map(PersistedInboundTicket::into_runtime)
            .collect::<Result<VecDeque<_>, _>>()?;
        validate_restored_inbound_tickets(
            &inbound_ticket,
            &retained_inbound_tickets,
            expected_network_name,
        )?;
        retained_inbound_tickets
            .retain(|ticket| now_unix_seconds <= ticket.expires_at_unix_seconds);
        let active_open = open
            .as_ref()
            .is_some_and(|operation| operation.terminal.is_none() && operation.completed.is_none());
        let active_join = join
            .as_ref()
            .is_some_and(|operation| operation.terminal.is_none() && operation.completed.is_none());
        if active_open && active_join {
            return Err(CodePairingSessionError::InvalidPersistedState(
                "inviter and joiner operations cannot both be active".to_owned(),
            ));
        }
        validate_restored_approval_state(open.as_ref(), &pending_approval, &inbound_ticket)?;
        let preserve_pending_recovery = pending_approval.as_ref().is_some_and(|approval| {
            enrollments.iter().any(|enrollment| {
                enrollment.operation_id == approval.operation_id
                    && enrollment.role == PairingEnrollmentRole::Inviter
                    && enrollment.state == PairingEnrollmentState::Prepared
                    && enrollment.approval_id.as_deref() == Some(approval.approval_id.as_str())
            })
        });
        if !preserve_pending_recovery
            && pending_approval
                .as_ref()
                .is_some_and(|approval| now_unix_seconds > approval.expires_at_unix_seconds)
        {
            pending_approval = None;
            if let Some(ticket) = inbound_ticket.as_mut() {
                ticket.outcome =
                    InboundTicketOutcome::Rejected(PairingCodeRejectionReason::Expired);
            }
        }

        Ok(Self {
            open,
            join,
            enrollments,
            lan_candidates: HashMap::new(),
            outbound_requests: HashMap::new(),
            inbound_sessions: HashMap::new(),
            pending_approval,
            inbound_ticket,
            retained_inbound_tickets,
        })
    }

    pub fn expire(&mut self, now_unix_seconds: u64, now: Instant) -> PairingExpiryActions {
        self.prune_lan_candidates(now);
        self.prune_retained_inbound_tickets(now_unix_seconds);
        self.inbound_sessions
            .retain(|_, inbound| inbound.session.expires_at_unix_seconds() >= now_unix_seconds);

        let mut actions = PairingExpiryActions::default();
        if self.open.as_ref().is_some_and(|operation| {
            operation.terminal.is_none()
                && operation.completed.is_none()
                && now_unix_seconds > operation.expires_at_unix_seconds
        }) {
            if let Some(ticket) = self.inbound_ticket.as_mut() {
                ticket.outcome =
                    InboundTicketOutcome::Rejected(PairingCodeRejectionReason::Expired);
            }
            actions.stop_providing_locator = self.deactivate_open(TerminalStatus::Expired);
        }
        if self.join.as_ref().is_some_and(|operation| {
            operation.terminal.is_none()
                && operation.completed.is_none()
                && now_unix_seconds > operation.expires_at_unix_seconds
        }) {
            self.deactivate_join(TerminalStatus::Expired);
        }
        actions
    }

    pub fn record_lan_candidate(&mut self, peer: Libp2pPeerId, address: Multiaddr, now: Instant) {
        if let Some(candidate) = self.lan_candidates.get_mut(&peer) {
            if !candidate.addresses.contains(&address)
                && candidate.addresses.len() < MAX_CODE_PAIRING_LAN_ADDRESSES_PER_PEER
            {
                candidate.addresses.push(address);
            }
            candidate.last_seen = now;
            return;
        }
        if self.lan_candidates.len() >= MAX_CODE_PAIRING_LAN_CANDIDATES
            && let Some(oldest) = self
                .lan_candidates
                .iter()
                .min_by_key(|(_, candidate)| candidate.last_seen)
                .map(|(peer, _)| *peer)
        {
            self.lan_candidates.remove(&oldest);
        }
        self.lan_candidates.insert(
            peer,
            LanCandidate {
                addresses: vec![address],
                last_seen: now,
            },
        );
    }

    pub fn remove_lan_candidate(&mut self, peer: Libp2pPeerId, address: &Multiaddr) {
        let remove_peer = self.lan_candidates.get_mut(&peer).is_some_and(|candidate| {
            candidate.addresses.retain(|known| known != address);
            candidate.addresses.is_empty()
        });
        if remove_peer {
            self.lan_candidates.remove(&peer);
        }
    }

    #[must_use]
    pub fn pending_lan_peers(&self, local_peer: Libp2pPeerId) -> Vec<Libp2pPeerId> {
        let Some(join) = self.active_join() else {
            return Vec::new();
        };
        if join.selected_inviter.is_some() {
            return Vec::new();
        }
        self.lan_candidates
            .keys()
            .filter(|peer| **peer != local_peer && !join.attempted_peers.contains(peer))
            .copied()
            .collect()
    }

    #[must_use]
    pub fn lan_addresses(&self, peer: Libp2pPeerId) -> Vec<Multiaddr> {
        self.lan_candidates
            .get(&peer)
            .map_or_else(Vec::new, |candidate| candidate.addresses.clone())
    }

    #[must_use]
    pub fn should_start_open_provider(&self, now: Instant) -> Option<&str> {
        let operation = self.active_open()?;
        (!operation.provider_advertised
            && now.saturating_duration_since(operation.opened_at) >= CODE_PAIRING_LAN_GRACE)
            .then_some(operation.locator.as_str())
    }

    pub fn mark_open_provider_started(&mut self, locator: &str) {
        if let Some(operation) = self.open.as_mut()
            && operation.locator == locator
            && operation.terminal.is_none()
            && operation.completed.is_none()
        {
            operation.provider_advertised = true;
        }
    }

    #[must_use]
    pub fn should_start_join_lookup(&self, now: Instant) -> Option<&str> {
        let operation = self.active_join()?;
        (operation.selected_inviter.is_none()
            && now.saturating_duration_since(operation.started_at) >= CODE_PAIRING_LAN_GRACE
            && operation
                .next_public_lookup_at
                .is_none_or(|next_lookup| now >= next_lookup)
            && operation.public_lookup_query.is_none())
        .then_some(operation.locator.as_str())
    }

    pub fn mark_join_lookup_started(
        &mut self,
        locator: &str,
        query_id: kad::QueryId,
        now: Instant,
    ) {
        if let Some(operation) = self.join.as_mut()
            && operation.locator == locator
            && operation.terminal.is_none()
            && operation.completed.is_none()
        {
            operation.public_lookup_started = true;
            operation.next_public_lookup_at = Some(now + CODE_PAIRING_PUBLIC_LOOKUP_INTERVAL);
            operation.public_lookup_query = Some(query_id);
        }
    }

    pub fn finish_join_lookup(&mut self, query_id: kad::QueryId) {
        if let Some(operation) = self.active_join_mut()
            && operation.public_lookup_query == Some(query_id)
        {
            operation.public_lookup_query = None;
        }
    }

    #[must_use]
    pub fn active_open_code_for_locator(&self, locator: &str) -> Option<(&str, &PairingCode, u64)> {
        let operation = self.active_open()?;
        (operation.locator == locator).then(|| {
            (
                operation.id.as_str(),
                operation.code.as_ref().expect("active open retains code"),
                operation.expires_at_unix_seconds,
            )
        })
    }

    #[must_use]
    pub fn active_join_code(&self) -> Option<(&str, &PairingCode)> {
        let operation = self.active_join()?;
        Some((
            operation.id.as_str(),
            operation.code.as_ref().expect("active join retains code"),
        ))
    }

    #[must_use]
    pub fn active_join_locator(&self) -> Option<&str> {
        self.active_join()
            .map(|operation| operation.locator.as_str())
    }

    #[must_use]
    pub fn can_start_outbound_hello(&self) -> bool {
        self.outbound_requests.len() < MAX_PENDING_CODE_HELLOS
    }

    #[must_use]
    pub fn join_request_options(
        &self,
        operation_id: &str,
    ) -> Option<(Option<String>, Vec<RouteConfig>)> {
        let operation = self
            .active_join()
            .filter(|operation| operation.id == operation_id)?;
        Some((
            operation.requested_vpn_ip.clone(),
            operation.requested_routes.clone(),
        ))
    }

    pub fn mark_peer_attempted(&mut self, peer: Libp2pPeerId) -> bool {
        let Some(operation) = self.active_join_mut() else {
            return false;
        };
        if operation.attempted_peers.len() >= MAX_CODE_PAIRING_PEER_ATTEMPTS {
            return false;
        }
        operation.attempted_peers.insert(peer)
    }

    pub fn select_inviter(&mut self, operation_id: &str, peer: Libp2pPeerId) -> bool {
        let Some(operation) = self
            .active_join_mut()
            .filter(|operation| operation.id == operation_id)
        else {
            return false;
        };
        if operation
            .selected_inviter
            .is_some_and(|selected| selected != peer)
        {
            return false;
        }
        operation.selected_inviter = Some(peer);
        true
    }

    #[must_use]
    pub fn allows_pairing_probe(&self, peer: Libp2pPeerId) -> bool {
        self.active_open().is_some()
            || self.active_join().is_some_and(|join| {
                join.attempted_peers.contains(&peer) || join.selected_inviter == Some(peer)
            })
            || self
                .inbound_sessions
                .keys()
                .any(|(session_peer, _)| *session_peer == peer)
            || self
                .pending_approval
                .as_ref()
                .is_some_and(|approval| approval.peer == peer)
            || self.inbound_ticket.as_ref().is_some_and(|ticket| {
                ticket.peer == peer && matches!(ticket.outcome, InboundTicketOutcome::Pending)
            })
    }

    pub fn insert_outbound_hello(
        &mut self,
        request_id: OutboundRequestId,
        hello: OutboundHello,
    ) -> Result<(), CodePairingSessionError> {
        if self.outbound_requests.len() >= MAX_PENDING_CODE_HELLOS {
            return Err(CodePairingSessionError::Capacity);
        }
        self.outbound_requests
            .insert(request_id, OutboundCodeRequest::Hello(hello));
        Ok(())
    }

    pub fn take_outbound_request(
        &mut self,
        request_id: OutboundRequestId,
    ) -> Option<OutboundCodeRequest> {
        self.outbound_requests.remove(&request_id)
    }

    pub fn insert_inbound_session(
        &mut self,
        peer: Libp2pPeerId,
        session: InboundSession,
    ) -> Result<(), CodePairingSessionError> {
        if self.inbound_sessions.len() >= MAX_INBOUND_CODE_SESSIONS {
            return Err(CodePairingSessionError::Capacity);
        }
        let token = session.session.rendezvous_token().to_owned();
        self.inbound_sessions.insert((peer, token), session);
        Ok(())
    }

    pub fn take_inbound_session(
        &mut self,
        peer: Libp2pPeerId,
        rendezvous_token: &str,
    ) -> Option<InboundSession> {
        self.inbound_sessions
            .remove(&(peer, rendezvous_token.to_owned()))
    }

    pub fn set_pending_approval(
        &mut self,
        approval: PendingApproval,
    ) -> Result<PairingCodeResponse, CodePairingSessionError> {
        if self.pending_approval.is_some() {
            return Err(CodePairingSessionError::Capacity);
        }
        let response = PairingCodeResponse::Pending {
            ticket: approval.ticket.clone(),
            expires_at_unix_seconds: approval.expires_at_unix_seconds,
        };
        self.inbound_ticket = Some(InboundTicket {
            operation_id: approval.operation_id.clone(),
            approval_id: approval.approval_id.clone(),
            peer: approval.peer,
            ticket: approval.ticket.clone(),
            expires_at_unix_seconds: approval.expires_at_unix_seconds,
            outcome: InboundTicketOutcome::Pending,
        });
        self.pending_approval = Some(approval);
        Ok(response)
    }

    pub fn set_remote_pending(
        &mut self,
        operation_id: &str,
        peer: Libp2pPeerId,
        offer: PairingOffer,
        transcript_sha256: String,
        ticket: String,
        now: Instant,
    ) -> Result<(), CodePairingSessionError> {
        validate_pairing_ticket(&ticket)?;
        validate_transcript_sha256(&transcript_sha256, false)?;
        let operation = self
            .active_join_mut()
            .filter(|operation| operation.id == operation_id)
            .ok_or(CodePairingSessionError::NotFound)?;
        if operation
            .selected_inviter
            .is_some_and(|selected| selected != peer)
        {
            return Err(CodePairingSessionError::Conflict);
        }
        operation.selected_inviter = Some(peer);
        operation.remote_approval = Some(RemoteApproval {
            peer,
            ticket,
            offer,
            transcript_sha256,
            next_poll_at: now,
            poll_in_flight: false,
        });
        Ok(())
    }

    pub fn due_remote_poll(&mut self, now: Instant) -> Option<PendingRemotePoll> {
        let operation = self.active_join_mut()?;
        let remote = operation.remote_approval.as_mut()?;
        if remote.poll_in_flight || now < remote.next_poll_at {
            return None;
        }
        remote.poll_in_flight = true;
        remote.next_poll_at = now + CODE_PAIRING_POLL_INTERVAL;
        Some(PendingRemotePoll {
            operation_id: operation.id.clone(),
            peer: remote.peer,
            ticket: remote.ticket.clone(),
            offer: remote.offer.clone(),
            transcript_sha256: remote.transcript_sha256.clone(),
        })
    }

    pub fn release_remote_poll(&mut self, operation_id: &str, peer: Libp2pPeerId) {
        if let Some(remote) = self
            .active_join_mut()
            .filter(|operation| operation.id == operation_id)
            .and_then(|operation| operation.remote_approval.as_mut())
            .filter(|remote| remote.peer == peer)
        {
            remote.poll_in_flight = false;
        }
    }

    pub fn release_peer_attempt(&mut self, operation_id: &str, peer: Libp2pPeerId) {
        let Some(operation) = self
            .active_join_mut()
            .filter(|operation| operation.id == operation_id)
        else {
            return;
        };
        operation.attempted_peers.remove(&peer);
        if operation.selected_inviter == Some(peer) && operation.remote_approval.is_none() {
            operation.selected_inviter = None;
        }
    }

    pub fn insert_outbound_submit(
        &mut self,
        request_id: OutboundRequestId,
        pairing: OutboundPairing,
    ) -> Result<(), CodePairingSessionError> {
        self.insert_outbound_request(request_id, OutboundCodeRequest::Submit(pairing))
    }

    pub fn insert_outbound_poll(
        &mut self,
        request_id: OutboundRequestId,
        pairing: OutboundPairing,
    ) -> Result<(), CodePairingSessionError> {
        self.insert_outbound_request(request_id, OutboundCodeRequest::Poll(pairing))
    }

    fn insert_outbound_request(
        &mut self,
        request_id: OutboundRequestId,
        request: OutboundCodeRequest,
    ) -> Result<(), CodePairingSessionError> {
        if self.outbound_requests.len() >= MAX_PENDING_CODE_HELLOS {
            return Err(CodePairingSessionError::Capacity);
        }
        self.outbound_requests.insert(request_id, request);
        Ok(())
    }

    fn ensure_idle(&self) -> Result<(), CodePairingSessionError> {
        let open_active = self
            .open
            .as_ref()
            .is_some_and(|operation| operation.terminal.is_none() && operation.completed.is_none());
        let join_active = self
            .join
            .as_ref()
            .is_some_and(|operation| operation.terminal.is_none() && operation.completed.is_none());
        if open_active || join_active {
            Err(CodePairingSessionError::Busy)
        } else {
            Ok(())
        }
    }

    fn active_open(&self) -> Option<&OpenOperation> {
        self.open
            .as_ref()
            .filter(|operation| operation.terminal.is_none() && operation.completed.is_none())
    }

    fn active_join(&self) -> Option<&JoinOperation> {
        self.join
            .as_ref()
            .filter(|operation| operation.terminal.is_none() && operation.completed.is_none())
    }

    fn active_join_mut(&mut self) -> Option<&mut JoinOperation> {
        self.join
            .as_mut()
            .filter(|operation| operation.terminal.is_none() && operation.completed.is_none())
    }

    fn deactivate_open(&mut self, terminal: TerminalStatus) -> Option<String> {
        let operation = self.open.as_mut()?;
        operation.code.take();
        operation.terminal = Some(terminal);
        let locator = operation
            .provider_advertised
            .then(|| operation.locator.clone());
        operation.provider_advertised = false;
        self.clear_transient_handshakes();
        locator
    }

    fn deactivate_join(&mut self, terminal: TerminalStatus) {
        if let Some(operation) = &mut self.join {
            operation.code.take();
            operation.remote_approval = None;
            operation.terminal = Some(terminal);
        }
        self.clear_transient_handshakes();
    }

    fn clear_transient_handshakes(&mut self) {
        self.outbound_requests.clear();
        self.inbound_sessions.clear();
        self.pending_approval.take();
    }

    fn archive_inbound_ticket(
        &mut self,
        now_unix_seconds: u64,
    ) -> Result<(), CodePairingSessionError> {
        self.prune_retained_inbound_tickets(now_unix_seconds);
        if self
            .inbound_ticket
            .as_ref()
            .is_some_and(|ticket| matches!(ticket.outcome, InboundTicketOutcome::Pending))
        {
            return Err(CodePairingSessionError::InvalidPersistedState(
                "cannot replace a pending polling ticket".to_owned(),
            ));
        }
        let Some(ticket) = self.inbound_ticket.take() else {
            return Ok(());
        };
        if now_unix_seconds > ticket.expires_at_unix_seconds {
            return Ok(());
        }
        self.retained_inbound_tickets
            .retain(|retained| retained.ticket != ticket.ticket);
        while self.retained_inbound_tickets.len() >= MAX_RETAINED_PAIRING_TICKETS {
            self.retained_inbound_tickets.pop_front();
        }
        self.retained_inbound_tickets.push_back(ticket);
        Ok(())
    }

    fn prune_retained_inbound_tickets(&mut self, now_unix_seconds: u64) {
        self.retained_inbound_tickets
            .retain(|ticket| now_unix_seconds <= ticket.expires_at_unix_seconds);
    }

    fn find_submission_ticket(
        &self,
        peer: Libp2pPeerId,
        approval_id: &str,
    ) -> Option<&InboundTicket> {
        self.inbound_ticket
            .as_ref()
            .filter(|ticket| ticket.peer == peer && ticket.approval_id == approval_id)
            .or_else(|| {
                self.retained_inbound_tickets
                    .iter()
                    .rev()
                    .find(|ticket| ticket.peer == peer && ticket.approval_id == approval_id)
            })
    }

    fn find_polling_ticket(&self, peer: Libp2pPeerId, ticket: &str) -> Option<&InboundTicket> {
        self.inbound_ticket
            .as_ref()
            .filter(|state| state.peer == peer && state.ticket == ticket)
            .or_else(|| {
                self.retained_inbound_tickets
                    .iter()
                    .rev()
                    .find(|state| state.peer == peer && state.ticket == ticket)
            })
    }

    fn prune_lan_candidates(&mut self, now: Instant) {
        self.lan_candidates.retain(|_, candidate| {
            now.saturating_duration_since(candidate.last_seen) <= CODE_PAIRING_LAN_CANDIDATE_TTL
        });
    }
}

impl OpenOperation {
    fn discovery_stage(&self) -> PairingDiscoveryStage {
        if self.provider_advertised {
            PairingDiscoveryStage::Public
        } else {
            PairingDiscoveryStage::Lan
        }
    }
}

impl JoinOperation {
    fn discovery_stage(&self) -> PairingDiscoveryStage {
        if self.public_lookup_started {
            PairingDiscoveryStage::Public
        } else {
            PairingDiscoveryStage::Lan
        }
    }
}

impl InboundTicket {
    fn accept(&mut self, response: PairingResponse) {
        self.expires_at_unix_seconds = self
            .expires_at_unix_seconds
            .max(response.payload.expires_at_unix_seconds);
        self.outcome = InboundTicketOutcome::Accepted(Box::new(response));
    }

    fn response(&self, now_unix_seconds: u64) -> PairingCodeResponse {
        if now_unix_seconds > self.expires_at_unix_seconds {
            return PairingCodeResponse::Rejected {
                reason: PairingCodeRejectionReason::Expired,
            };
        }
        match &self.outcome {
            InboundTicketOutcome::Pending => PairingCodeResponse::Pending {
                ticket: self.ticket.clone(),
                expires_at_unix_seconds: self.expires_at_unix_seconds,
            },
            InboundTicketOutcome::Accepted(response) => PairingCodeResponse::Accepted {
                response: response.clone(),
            },
            InboundTicketOutcome::Rejected(reason) => {
                PairingCodeResponse::Rejected { reason: *reason }
            }
        }
    }
}

impl PairingEnrollment {
    fn has_same_preparation(&self, other: &Self) -> bool {
        self.operation_id == other.operation_id
            && self.role == other.role
            && self.approval_id == other.approval_id
            && self.offer == other.offer
            && self.response == other.response
            && self.transcript_sha256 == other.transcript_sha256
    }
}

fn validate_pairing_enrollment(
    enrollment: &PairingEnrollment,
    network_name: &str,
) -> Result<(), CodePairingSessionError> {
    validate_pairing_operation_id(&enrollment.operation_id)?;
    validate_transcript_sha256(&enrollment.transcript_sha256, true)?;
    if enrollment
        .completed_at_unix_seconds
        .is_some_and(|completed_at| {
            enrollment.state != PairingEnrollmentState::Applied
                || completed_at < enrollment.response.payload.issued_at_unix_seconds
        })
    {
        return Err(invalid_enrollment(
            "enrollment has an invalid completion timestamp",
        ));
    }

    let response = &enrollment.response.payload;
    if response.version != PAIRING_OFFER_VERSION {
        return Err(invalid_enrollment("response has an unsupported version"));
    }
    if response.network_name != network_name {
        return Err(invalid_enrollment(
            "response belongs to a different network",
        ));
    }
    let inviter_peer = parse_enrollment_peer(&response.inviter_peer, "response inviter")?;
    let joiner_peer = parse_enrollment_peer(&response.joiner_peer, "response joiner")?;
    if inviter_peer == joiner_peer {
        return Err(invalid_enrollment(
            "inviter and joiner peer IDs must differ",
        ));
    }
    let offer = enrollment.offer.as_ref().ok_or_else(|| {
        invalid_enrollment("enrollment is missing its signed code-approval offer")
    })?;
    if offer.signature.is_empty() {
        return Err(invalid_enrollment(
            "enrollment offer is missing its signature",
        ));
    }

    match enrollment.role {
        PairingEnrollmentRole::Inviter => {
            let approval_id = enrollment.approval_id.as_deref().ok_or_else(|| {
                invalid_enrollment("inviter enrollment is missing its approval ID")
            })?;
            validate_pairing_approval_id(approval_id)?;
        }
        PairingEnrollmentRole::Joiner => {
            if enrollment.approval_id.is_some() {
                return Err(invalid_enrollment(
                    "joiner enrollment cannot contain an approval ID",
                ));
            }
        }
    }

    if offer.payload.version != PAIRING_OFFER_VERSION {
        return Err(invalid_enrollment("offer has an unsupported version"));
    }
    if offer.payload.acceptance_mode != PairingAcceptanceMode::CodeApproval {
        return Err(invalid_enrollment(
            "enrollment offer is not a code-approval offer",
        ));
    }
    if offer.payload.network_name != network_name
        || offer.payload.network_name != response.network_name
    {
        return Err(invalid_enrollment("offer belongs to a different network"));
    }
    let offer_inviter = parse_enrollment_peer(&offer.payload.inviter_peer, "offer inviter")?;
    if offer_inviter != inviter_peer {
        return Err(invalid_enrollment(
            "offer and response inviter peer IDs do not match",
        ));
    }
    if offer.payload.inviter_public_key != response.inviter_public_key {
        return Err(invalid_enrollment(
            "offer and response inviter public keys do not match",
        ));
    }
    if offer.payload.rendezvous_token != response.rendezvous_token {
        return Err(invalid_enrollment(
            "offer and response rendezvous tokens do not match",
        ));
    }
    if offer.payload.protocols != response.protocols {
        return Err(invalid_enrollment(
            "offer and response protocol capabilities do not match",
        ));
    }

    Ok(())
}

fn validate_restored_enrollments(
    enrollments: Vec<PairingEnrollment>,
    network_name: &str,
) -> Result<Vec<PairingEnrollment>, CodePairingSessionError> {
    if enrollments.len() > MAX_CODE_PAIRING_ENROLLMENTS {
        return Err(invalid_enrollment("enrollment ledger exceeds its capacity"));
    }

    let mut operation_ids = HashSet::with_capacity(enrollments.len());
    for enrollment in &enrollments {
        validate_pairing_enrollment(enrollment, network_name)?;
        if !operation_ids.insert(enrollment.operation_id.as_str()) {
            return Err(invalid_enrollment(
                "enrollment ledger contains a duplicate operation ID",
            ));
        }
    }
    Ok(enrollments)
}

fn parse_enrollment_peer(
    peer: &str,
    relationship: &str,
) -> Result<Libp2pPeerId, CodePairingSessionError> {
    peer.parse()
        .map_err(|_| invalid_enrollment(format!("{relationship} has an invalid peer ID")))
}

fn validate_pairing_approval_id(approval_id: &str) -> Result<(), CodePairingSessionError> {
    if is_canonical_pairing_approval_id(approval_id) {
        Ok(())
    } else {
        Err(invalid_enrollment(
            "inviter enrollment has an invalid approval ID",
        ))
    }
}

fn validate_persisted_approval_id(approval_id: &str) -> Result<(), CodePairingSessionError> {
    if is_canonical_pairing_approval_id(approval_id) {
        Ok(())
    } else {
        Err(CodePairingSessionError::InvalidPersistedState(
            "polling ticket has an invalid approval ID".to_owned(),
        ))
    }
}

fn is_canonical_pairing_approval_id(approval_id: &str) -> bool {
    URL_SAFE_NO_PAD.decode(approval_id).is_ok_and(|bytes| {
        bytes.len() == APPROVAL_ID_BYTES && URL_SAFE_NO_PAD.encode(bytes) == approval_id
    })
}

fn validate_transcript_sha256(
    transcript_sha256: &str,
    allow_legacy_empty: bool,
) -> Result<(), CodePairingSessionError> {
    if allow_legacy_empty && transcript_sha256.is_empty() {
        return Ok(());
    }
    if URL_SAFE_NO_PAD
        .decode(transcript_sha256)
        .is_ok_and(|bytes| bytes.len() == 32 && URL_SAFE_NO_PAD.encode(bytes) == transcript_sha256)
    {
        Ok(())
    } else {
        Err(invalid_enrollment(
            "enrollment has an invalid transcript SHA-256 digest",
        ))
    }
}

fn invalid_enrollment(reason: impl Into<String>) -> CodePairingSessionError {
    CodePairingSessionError::InvalidPersistedState(reason.into())
}

fn invalid_recovery(reason: impl Into<String>) -> CodePairingSessionError {
    CodePairingSessionError::InvalidPersistedState(reason.into())
}

fn validate_expiry(expires_in_seconds: u64) -> Result<(), CodePairingSessionError> {
    if expires_in_seconds == 0 || expires_in_seconds > MAX_CODE_PAIRING_EXPIRES_IN_SECONDS {
        Err(CodePairingSessionError::InvalidExpiry)
    } else {
        Ok(())
    }
}

#[must_use]
pub fn fresh_pairing_operation_id() -> String {
    let mut bytes = [0_u8; OPERATION_ID_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn fresh_pairing_ticket() -> String {
    let mut bytes = [0_u8; PAIRING_TICKET_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn validate_pairing_ticket(ticket: &str) -> Result<(), CodePairingSessionError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(ticket)
        .map_err(|_| CodePairingSessionError::InvalidTicket)?;
    if bytes.len() == PAIRING_TICKET_BYTES && URL_SAFE_NO_PAD.encode(bytes) == ticket {
        Ok(())
    } else {
        Err(CodePairingSessionError::InvalidTicket)
    }
}

pub fn validate_pairing_operation_id(operation_id: &str) -> Result<(), CodePairingSessionError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(operation_id)
        .map_err(|_| CodePairingSessionError::InvalidOperationId)?;
    if bytes.len() == OPERATION_ID_BYTES && URL_SAFE_NO_PAD.encode(bytes) == operation_id {
        Ok(())
    } else {
        Err(CodePairingSessionError::InvalidOperationId)
    }
}

#[derive(Deserialize, Serialize)]
struct PersistedCodePairingSessions {
    version: u8,
    network_name: String,
    open: Option<PersistedOpenOperation>,
    join: Option<PersistedJoinOperation>,
    #[serde(default)]
    pending_approval: Option<PersistedPendingApproval>,
    #[serde(default)]
    inbound_ticket: Option<PersistedInboundTicket>,
    #[serde(default)]
    retained_inbound_tickets: Vec<PersistedInboundTicket>,
    #[serde(default)]
    enrollments: Vec<PairingEnrollment>,
}

#[derive(Deserialize, Serialize)]
struct PersistedOpenOperation {
    id: String,
    code: Option<String>,
    locator: String,
    expires_at_unix_seconds: u64,
    expires_in_seconds: u64,
    completed: Option<PairingResponse>,
    terminal: Option<TerminalStatus>,
}

#[derive(Deserialize, Serialize)]
struct PersistedJoinOperation {
    id: String,
    code: Option<String>,
    locator: String,
    expires_at_unix_seconds: u64,
    expires_in_seconds: u64,
    requested_vpn_ip: Option<String>,
    requested_routes: Vec<RouteConfig>,
    #[serde(default)]
    remote_approval: Option<PersistedRemoteApproval>,
    completed: Option<(PairingOffer, PairingResponse)>,
    terminal: Option<TerminalStatus>,
}

#[derive(Deserialize, Serialize)]
struct PersistedPendingApproval {
    operation_id: String,
    approval_id: String,
    peer: String,
    ticket: String,
    expires_at_unix_seconds: u64,
    request: PairingRequest,
}

#[derive(Deserialize, Serialize)]
struct PersistedInboundTicket {
    operation_id: String,
    approval_id: String,
    peer: String,
    ticket: String,
    expires_at_unix_seconds: u64,
    outcome: InboundTicketOutcome,
}

#[derive(Deserialize, Serialize)]
struct PersistedRemoteApproval {
    peer: String,
    ticket: String,
    offer: PairingOffer,
    #[serde(default)]
    transcript_sha256: String,
}

impl PersistedCodePairingSessions {
    fn from_runtime(sessions: &CodePairingSessions, network_name: &str) -> Self {
        Self {
            version: PERSISTED_PAIRING_STATE_VERSION,
            network_name: network_name.to_owned(),
            open: sessions
                .open
                .as_ref()
                .map(PersistedOpenOperation::from_runtime),
            join: sessions
                .join
                .as_ref()
                .map(PersistedJoinOperation::from_runtime),
            pending_approval: sessions
                .pending_approval
                .as_ref()
                .map(PersistedPendingApproval::from_runtime),
            inbound_ticket: sessions
                .inbound_ticket
                .as_ref()
                .map(PersistedInboundTicket::from_runtime),
            retained_inbound_tickets: sessions
                .retained_inbound_tickets
                .iter()
                .map(PersistedInboundTicket::from_runtime)
                .collect(),
            enrollments: sessions.enrollments.clone(),
        }
    }
}

impl PersistedOpenOperation {
    fn from_runtime(operation: &OpenOperation) -> Self {
        Self {
            id: operation.id.clone(),
            code: operation.code.as_ref().map(ToString::to_string),
            locator: operation.locator.clone(),
            expires_at_unix_seconds: operation.expires_at_unix_seconds,
            expires_in_seconds: operation.expires_in_seconds,
            completed: operation.completed.clone(),
            terminal: operation.terminal.clone(),
        }
    }

    fn into_runtime(
        self,
        network_name: &str,
        now_unix_seconds: u64,
        resumed_at: Instant,
    ) -> Result<OpenOperation, CodePairingSessionError> {
        validate_pairing_operation_id(&self.id)?;
        validate_expiry(self.expires_in_seconds)?;
        let code = self
            .code
            .map(|code| code.parse::<PairingCode>())
            .transpose()?;
        if let Some(code) = &code
            && code.locator(network_name)? != self.locator
        {
            return Err(CodePairingSessionError::InvalidPersistedState(
                "inviter code locator does not match".to_owned(),
            ));
        }
        let mut operation = OpenOperation {
            id: self.id,
            code,
            locator: self.locator,
            opened_at: resumed_at,
            expires_at_unix_seconds: self.expires_at_unix_seconds,
            expires_in_seconds: self.expires_in_seconds,
            provider_advertised: false,
            completed: self.completed,
            terminal: self.terminal,
        };
        validate_restored_operation(
            operation.code.is_some(),
            operation.completed.is_some(),
            operation.terminal.is_some(),
        )?;
        if operation.completed.is_none()
            && operation.terminal.is_none()
            && now_unix_seconds > operation.expires_at_unix_seconds
        {
            operation.code.take();
            operation.terminal = Some(TerminalStatus::Expired);
        }
        Ok(operation)
    }
}

impl PersistedJoinOperation {
    fn from_runtime(operation: &JoinOperation) -> Self {
        Self {
            id: operation.id.clone(),
            code: operation.code.as_ref().map(ToString::to_string),
            locator: operation.locator.clone(),
            expires_at_unix_seconds: operation.expires_at_unix_seconds,
            expires_in_seconds: operation.expires_in_seconds,
            requested_vpn_ip: operation.requested_vpn_ip.clone(),
            requested_routes: operation.requested_routes.clone(),
            remote_approval: operation
                .remote_approval
                .as_ref()
                .map(PersistedRemoteApproval::from_runtime),
            completed: operation.completed.clone(),
            terminal: operation.terminal.clone(),
        }
    }

    fn into_runtime(
        self,
        network_name: &str,
        now_unix_seconds: u64,
        resumed_at: Instant,
        preserve_remote_approval_for_recovery: bool,
    ) -> Result<JoinOperation, CodePairingSessionError> {
        validate_pairing_operation_id(&self.id)?;
        validate_expiry(self.expires_in_seconds)?;
        let code = self
            .code
            .map(|code| code.parse::<PairingCode>())
            .transpose()?;
        if let Some(code) = &code
            && code.locator(network_name)? != self.locator
        {
            return Err(CodePairingSessionError::InvalidPersistedState(
                "join code locator does not match".to_owned(),
            ));
        }
        let remote_approval = self
            .remote_approval
            .map(|approval| approval.into_runtime(resumed_at))
            .transpose()?;
        let selected_inviter = remote_approval.as_ref().map(|approval| approval.peer);
        let mut operation = JoinOperation {
            id: self.id,
            code,
            locator: self.locator,
            started_at: resumed_at,
            expires_at_unix_seconds: self.expires_at_unix_seconds,
            expires_in_seconds: self.expires_in_seconds,
            public_lookup_started: false,
            next_public_lookup_at: None,
            public_lookup_query: None,
            attempted_peers: HashSet::new(),
            selected_inviter,
            remote_approval,
            requested_vpn_ip: self.requested_vpn_ip,
            requested_routes: self.requested_routes,
            completed: self.completed,
            terminal: self.terminal,
        };
        validate_restored_operation(
            operation.code.is_some(),
            operation.completed.is_some(),
            operation.terminal.is_some(),
        )?;
        if operation.completed.is_none()
            && operation.terminal.is_none()
            && now_unix_seconds > operation.expires_at_unix_seconds
        {
            operation.code.take();
            operation.terminal = Some(TerminalStatus::Expired);
        }
        if operation.completed.is_some()
            || (operation.terminal.is_some() && !preserve_remote_approval_for_recovery)
        {
            operation.selected_inviter = None;
            operation.remote_approval = None;
        }
        Ok(operation)
    }
}

impl PersistedPendingApproval {
    fn from_runtime(approval: &PendingApproval) -> Self {
        Self {
            operation_id: approval.operation_id.clone(),
            approval_id: approval.approval_id.clone(),
            peer: approval.peer.to_string(),
            ticket: approval.ticket.clone(),
            expires_at_unix_seconds: approval.expires_at_unix_seconds,
            request: approval.request.clone(),
        }
    }

    fn into_runtime(self) -> Result<PendingApproval, CodePairingSessionError> {
        validate_pairing_operation_id(&self.operation_id)?;
        validate_pairing_ticket(&self.ticket)?;
        let peer = self.peer.parse().map_err(|_| {
            CodePairingSessionError::InvalidPersistedState(
                "pending approval has an invalid peer ID".to_owned(),
            )
        })?;
        if pairing_approval_id(&self.request)? != self.approval_id {
            return Err(CodePairingSessionError::InvalidPersistedState(
                "pending approval digest does not match its request".to_owned(),
            ));
        }
        let transcript_sha256 =
            crate::pairing_code::pairing_request_transcript_sha256(&self.request)?;
        Ok(PendingApproval {
            operation_id: self.operation_id,
            approval_id: self.approval_id,
            peer,
            ticket: self.ticket,
            expires_at_unix_seconds: self.expires_at_unix_seconds,
            request: self.request,
            transcript_sha256,
        })
    }
}

impl PersistedInboundTicket {
    fn from_runtime(ticket: &InboundTicket) -> Self {
        Self {
            operation_id: ticket.operation_id.clone(),
            approval_id: ticket.approval_id.clone(),
            peer: ticket.peer.to_string(),
            ticket: ticket.ticket.clone(),
            expires_at_unix_seconds: ticket.expires_at_unix_seconds,
            outcome: ticket.outcome.clone(),
        }
    }

    fn into_runtime(self) -> Result<InboundTicket, CodePairingSessionError> {
        validate_pairing_operation_id(&self.operation_id)?;
        validate_persisted_approval_id(&self.approval_id)?;
        validate_pairing_ticket(&self.ticket)?;
        let peer = self.peer.parse().map_err(|_| {
            CodePairingSessionError::InvalidPersistedState(
                "inbound polling ticket has an invalid peer ID".to_owned(),
            )
        })?;
        let expires_at_unix_seconds = match &self.outcome {
            InboundTicketOutcome::Accepted(response) => self
                .expires_at_unix_seconds
                .max(response.payload.expires_at_unix_seconds),
            InboundTicketOutcome::Pending | InboundTicketOutcome::Rejected(_) => {
                self.expires_at_unix_seconds
            }
        };
        Ok(InboundTicket {
            operation_id: self.operation_id,
            approval_id: self.approval_id,
            peer,
            ticket: self.ticket,
            expires_at_unix_seconds,
            outcome: self.outcome,
        })
    }
}

impl PersistedRemoteApproval {
    fn from_runtime(approval: &RemoteApproval) -> Self {
        Self {
            peer: approval.peer.to_string(),
            ticket: approval.ticket.clone(),
            offer: approval.offer.clone(),
            transcript_sha256: approval.transcript_sha256.clone(),
        }
    }

    fn into_runtime(self, resumed_at: Instant) -> Result<RemoteApproval, CodePairingSessionError> {
        validate_pairing_ticket(&self.ticket)?;
        validate_transcript_sha256(&self.transcript_sha256, true)?;
        let peer = self.peer.parse().map_err(|_| {
            CodePairingSessionError::InvalidPersistedState(
                "remote approval has an invalid peer ID".to_owned(),
            )
        })?;
        Ok(RemoteApproval {
            peer,
            ticket: self.ticket,
            offer: self.offer,
            transcript_sha256: self.transcript_sha256,
            next_poll_at: resumed_at,
            poll_in_flight: false,
        })
    }
}

fn validate_restored_operation(
    has_code: bool,
    completed: bool,
    terminal: bool,
) -> Result<(), CodePairingSessionError> {
    if completed && terminal {
        return Err(CodePairingSessionError::InvalidPersistedState(
            "operation cannot be both completed and terminal".to_owned(),
        ));
    }
    if !completed && !terminal && !has_code {
        return Err(CodePairingSessionError::InvalidPersistedState(
            "active operation is missing its pairing code".to_owned(),
        ));
    }
    if (completed || terminal) && has_code {
        return Err(CodePairingSessionError::InvalidPersistedState(
            "terminal operation retained its pairing code".to_owned(),
        ));
    }
    Ok(())
}

fn validate_restored_approval_state(
    open: Option<&OpenOperation>,
    approval: &Option<PendingApproval>,
    ticket: &Option<InboundTicket>,
) -> Result<(), CodePairingSessionError> {
    if let Some(approval) = approval {
        let open = open.ok_or_else(|| {
            CodePairingSessionError::InvalidPersistedState(
                "pending approval has no inviter operation".to_owned(),
            )
        })?;
        let ticket = ticket.as_ref().ok_or_else(|| {
            CodePairingSessionError::InvalidPersistedState(
                "pending approval has no polling ticket".to_owned(),
            )
        })?;
        if open.id != approval.operation_id
            || ticket.operation_id != approval.operation_id
            || ticket.approval_id != approval.approval_id
            || ticket.peer != approval.peer
            || ticket.ticket != approval.ticket
            || !matches!(ticket.outcome, InboundTicketOutcome::Pending)
        {
            return Err(CodePairingSessionError::InvalidPersistedState(
                "pending approval and polling ticket do not match".to_owned(),
            ));
        }
    } else if ticket
        .as_ref()
        .is_some_and(|ticket| matches!(ticket.outcome, InboundTicketOutcome::Pending))
    {
        return Err(CodePairingSessionError::InvalidPersistedState(
            "pending polling ticket has no approval request".to_owned(),
        ));
    }
    Ok(())
}

fn validate_restored_inbound_tickets(
    active: &Option<InboundTicket>,
    retained: &VecDeque<InboundTicket>,
    network_name: &str,
) -> Result<(), CodePairingSessionError> {
    if retained.len() > MAX_RETAINED_PAIRING_TICKETS {
        return Err(CodePairingSessionError::InvalidPersistedState(
            "retained polling tickets exceed their capacity".to_owned(),
        ));
    }

    let mut ticket_ids = HashSet::with_capacity(retained.len() + usize::from(active.is_some()));
    if let Some(ticket) = active {
        validate_restored_ticket_outcome(ticket, network_name)?;
        ticket_ids.insert(ticket.ticket.as_str());
    }
    for ticket in retained {
        if matches!(ticket.outcome, InboundTicketOutcome::Pending) {
            return Err(CodePairingSessionError::InvalidPersistedState(
                "retained polling ticket is still pending".to_owned(),
            ));
        }
        validate_restored_ticket_outcome(ticket, network_name)?;
        if !ticket_ids.insert(ticket.ticket.as_str()) {
            return Err(CodePairingSessionError::InvalidPersistedState(
                "polling ticket state contains a duplicate ticket".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_restored_ticket_outcome(
    ticket: &InboundTicket,
    network_name: &str,
) -> Result<(), CodePairingSessionError> {
    let InboundTicketOutcome::Accepted(response) = &ticket.outcome else {
        return Ok(());
    };
    if response.payload.version != PAIRING_OFFER_VERSION
        || response.payload.network_name != network_name
    {
        return Err(CodePairingSessionError::InvalidPersistedState(
            "accepted polling response belongs to a different protocol or network".to_owned(),
        ));
    }
    let joiner = response
        .payload
        .joiner_peer
        .parse::<Libp2pPeerId>()
        .map_err(|_| {
            CodePairingSessionError::InvalidPersistedState(
                "accepted polling response has an invalid joiner peer ID".to_owned(),
            )
        })?;
    if joiner != ticket.peer {
        return Err(CodePairingSessionError::InvalidPersistedState(
            "accepted polling response does not match the ticket peer".to_owned(),
        ));
    }
    if ticket.expires_at_unix_seconds < response.payload.expires_at_unix_seconds {
        return Err(CodePairingSessionError::InvalidPersistedState(
            "accepted polling ticket expires before its signed response".to_owned(),
        ));
    }
    Ok(())
}

fn pairing_approval_id(request: &PairingRequest) -> Result<String, serde_json::Error> {
    let mut hasher = Sha256::new();
    hasher.update(b"p2p-vpn pairing approval v1\n");
    hasher.update(serde_json::to_vec(request)?);
    Ok(URL_SAFE_NO_PAD.encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use crate::{
        config::DiscoveryConfig,
        pairing::{
            PairingAcceptanceMode, PairingOfferPayload, PairingProtocols, PairingRequestPayload,
            PairingResponsePayload,
        },
    };

    use super::*;

    fn peer(byte: u8) -> Libp2pPeerId {
        let mut bytes = [0_u8; 32];
        bytes.fill(byte);
        libp2p::identity::PublicKey::try_decode_protobuf(
            &libp2p::identity::Keypair::ed25519_from_bytes(bytes)
                .expect("keypair")
                .public()
                .encode_protobuf(),
        )
        .expect("public key")
        .to_peer_id()
    }

    fn test_offer(inviter: Libp2pPeerId) -> PairingOffer {
        PairingOffer {
            payload: PairingOfferPayload {
                version: 1,
                network_name: "runners".to_owned(),
                inviter_peer: inviter.to_string(),
                inviter_public_key: "public-key".to_owned(),
                rendezvous_token: "token".to_owned(),
                issued_at_unix_seconds: 1_000,
                expires_at_unix_seconds: 1_600,
                acceptance_mode: PairingAcceptanceMode::CodeApproval,
                inviter_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                relay_reservations: Vec::new(),
                discovery: DiscoveryConfig::default(),
                protocols: PairingProtocols::default(),
            },
            signature: "offer-signature".to_owned(),
        }
    }

    fn test_request(inviter: Libp2pPeerId, joiner: Libp2pPeerId) -> PairingRequest {
        PairingRequest {
            offer: Some(test_offer(inviter)),
            payload: PairingRequestPayload {
                version: 1,
                network_name: "runners".to_owned(),
                inviter_peer: inviter.to_string(),
                joiner_peer: joiner.to_string(),
                joiner_public_key: "public-key".to_owned(),
                rendezvous_token: "token".to_owned(),
                offer_issued_at_unix_seconds: 1_000,
                offer_expires_at_unix_seconds: 1_600,
                offer_signature: "offer-signature".to_owned(),
                issued_at_unix_seconds: 1_001,
                requested_vpn_ip: Some("10.42.0.2".to_owned()),
                requested_routes: Vec::new(),
            },
            signature: "request-signature".to_owned(),
            code_authentication: None,
        }
    }

    fn test_response(inviter: Libp2pPeerId, joiner: Libp2pPeerId) -> PairingResponse {
        PairingResponse {
            payload: PairingResponsePayload {
                version: 1,
                network_name: "runners".to_owned(),
                inviter_peer: inviter.to_string(),
                inviter_public_key: "public-key".to_owned(),
                joiner_peer: joiner.to_string(),
                rendezvous_token: "token".to_owned(),
                issued_at_unix_seconds: 1_002,
                expires_at_unix_seconds: 1_600,
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                membership_key: None,
                member_records: Vec::new(),
                inviter_addresses: Vec::new(),
                inviter_routes: Vec::new(),
                bootstrap_peers: Vec::new(),
                relay_reservations: Vec::new(),
                discovery: DiscoveryConfig::default(),
                protocols: PairingProtocols::default(),
            },
            signature: "response-signature".to_owned(),
        }
    }

    fn test_approval_id(inviter: Libp2pPeerId, joiner: Libp2pPeerId) -> String {
        pairing_approval_id(&test_request(inviter, joiner)).expect("approval ID")
    }

    fn test_transcript_sha256() -> String {
        URL_SAFE_NO_PAD.encode([0x42; 32])
    }

    fn test_preparation(
        operation_id: String,
        role: PairingEnrollmentRole,
        approval_id: Option<String>,
        offer: Option<PairingOffer>,
        response: PairingResponse,
        transcript_sha256: String,
    ) -> PairingEnrollmentPreparation {
        PairingEnrollmentPreparation {
            operation_id,
            role,
            approval_id,
            offer,
            response,
            transcript_sha256,
        }
    }

    fn persisted_joiner_enrollment() -> serde_json::Value {
        let inviter = peer(1);
        let joiner = peer(2);
        let mut sessions = CodePairingSessions::new();
        sessions
            .prepare_enrollment(
                "runners",
                test_preparation(
                    fresh_pairing_operation_id(),
                    PairingEnrollmentRole::Joiner,
                    None,
                    Some(test_offer(inviter)),
                    test_response(inviter, joiner),
                    test_transcript_sha256(),
                ),
            )
            .expect("prepare joiner enrollment");
        serde_json::from_slice(&sessions.encode_persisted("runners").expect("encode ledger"))
            .expect("decode persisted JSON")
    }

    fn restore_json(
        value: &serde_json::Value,
    ) -> Result<CodePairingSessions, CodePairingSessionError> {
        CodePairingSessions::restore_persisted(
            &serde_json::to_vec(value).expect("encode persisted JSON"),
            "runners",
            1_010,
            Instant::now(),
        )
    }

    fn prepared_open_fixture(
        expires_in_seconds: u64,
    ) -> (
        CodePairingSessions,
        PairingEnrollment,
        String,
        Libp2pPeerId,
        Instant,
    ) {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let inviter = peer(1);
        let joiner = peer(2);
        let started = sessions
            .open("runners", expires_in_seconds, 1_000, now)
            .expect("open");
        let request = test_request(inviter, joiner);
        let offer = request.offer.clone().expect("request offer");
        let approval = PendingApproval::new(
            started.operation_id.clone(),
            joiner,
            started.expires_at_unix_seconds,
            request,
        )
        .expect("approval");
        let ticket = approval.ticket.clone();
        let approval_id = approval.approval_id.clone();
        let transcript_sha256 = approval.transcript_sha256.clone();
        sessions
            .set_pending_approval(approval)
            .expect("pending approval");
        let enrollment = sessions
            .prepare_enrollment(
                "runners",
                test_preparation(
                    started.operation_id,
                    PairingEnrollmentRole::Inviter,
                    Some(approval_id),
                    Some(offer),
                    test_response(inviter, joiner),
                    transcript_sha256,
                ),
            )
            .expect("prepare inviter enrollment")
            .clone();
        (sessions, enrollment, ticket, joiner, now)
    }

    fn prepared_join_fixture(
        expires_in_seconds: u64,
    ) -> (CodePairingSessions, PairingEnrollment, Instant) {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let inviter = peer(1);
        let joiner = peer(2);
        let offer = test_offer(inviter);
        let started = sessions
            .join(
                "runners",
                PairingCode::generate(),
                None,
                Vec::new(),
                expires_in_seconds,
                1_000,
                now,
            )
            .expect("join");
        let transcript_sha256 = test_transcript_sha256();
        sessions
            .set_remote_pending(
                &started.operation_id,
                inviter,
                offer.clone(),
                transcript_sha256.clone(),
                fresh_pairing_ticket(),
                now,
            )
            .expect("remote approval");
        let enrollment = sessions
            .prepare_enrollment(
                "runners",
                test_preparation(
                    started.operation_id,
                    PairingEnrollmentRole::Joiner,
                    None,
                    Some(offer),
                    test_response(inviter, joiner),
                    transcript_sha256,
                ),
            )
            .expect("prepare joiner enrollment")
            .clone();
        (sessions, enrollment, now)
    }

    #[test]
    fn open_uses_human_code_without_exposing_it_in_status() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let started = sessions
            .open("runners", 600, 1_000, now)
            .expect("open pairing");

        assert_eq!(started.code.len(), 19);
        assert_eq!(started.expires_at_unix_seconds, 1_600);
        assert_eq!(
            sessions.open_status(&started.operation_id).expect("status"),
            PairingOpenStatus::Searching {
                discovery: PairingDiscoveryStage::Lan,
                expires_at_unix_seconds: 1_600,
            }
        );
        let candidate = peer(9);
        assert!(sessions.allows_pairing_probe(candidate));
        sessions.cancel(&started.operation_id).expect("cancel");
        assert!(!sessions.allows_pairing_probe(candidate));
    }

    #[test]
    fn pairing_operations_are_strictly_bounded() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        sessions
            .open("runners", 600, 1_000, now)
            .expect("open pairing");

        assert!(matches!(
            sessions.join(
                "runners",
                PairingCode::generate(),
                None,
                Vec::new(),
                600,
                1_000,
                now,
            ),
            Err(CodePairingSessionError::Busy)
        ));
    }

    #[test]
    fn discovery_promotes_from_lan_to_public_only_after_grace() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let started = sessions
            .join(
                "runners",
                PairingCode::generate(),
                None,
                Vec::new(),
                600,
                1_000,
                now,
            )
            .expect("join pairing");

        assert_eq!(sessions.should_start_join_lookup(now), None);
        let public_now = now + CODE_PAIRING_LAN_GRACE;
        let locator = sessions
            .should_start_join_lookup(public_now)
            .expect("public lookup")
            .to_owned();
        let query_peer = peer(8);
        let mut kademlia =
            kad::Behaviour::new(query_peer, kad::store::MemoryStore::new(query_peer));
        let query_id = kademlia.get_providers(kad::RecordKey::new(&locator));
        sessions.mark_join_lookup_started(&locator, query_id, public_now);
        assert_eq!(
            sessions.join_status(&started.operation_id).expect("status"),
            PairingJoinStatus::Searching {
                discovery: PairingDiscoveryStage::Public,
                expires_at_unix_seconds: 1_600,
            }
        );
        assert_eq!(
            sessions.should_start_join_lookup(
                public_now + CODE_PAIRING_PUBLIC_LOOKUP_INTERVAL - Duration::from_millis(1)
            ),
            None
        );
        assert_eq!(
            sessions.should_start_join_lookup(public_now + CODE_PAIRING_PUBLIC_LOOKUP_INTERVAL),
            None
        );
        sessions.finish_join_lookup(query_id);
        assert_eq!(
            sessions.should_start_join_lookup(public_now + CODE_PAIRING_PUBLIC_LOOKUP_INTERVAL),
            Some(locator.as_str())
        );
        assert!(sessions.select_inviter(&started.operation_id, peer(1)));
        assert_eq!(
            sessions.should_start_join_lookup(public_now + CODE_PAIRING_PUBLIC_LOOKUP_INTERVAL * 2),
            None
        );
    }

    #[test]
    fn lan_candidates_are_capped_and_expire() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        for byte in 1..=u8::try_from(MAX_CODE_PAIRING_LAN_CANDIDATES).expect("candidate cap") {
            sessions.record_lan_candidate(
                peer(byte),
                Multiaddr::empty()
                    .with(libp2p::multiaddr::Protocol::Ip4(Ipv4Addr::LOCALHOST))
                    .with(libp2p::multiaddr::Protocol::Tcp(u16::from(byte))),
                now,
            );
        }
        sessions.record_lan_candidate(
            peer(200),
            "/ip4/127.0.0.1/tcp/200".parse().expect("candidate address"),
            now + Duration::from_secs(1),
        );

        assert_eq!(
            sessions.lan_candidates.len(),
            MAX_CODE_PAIRING_LAN_CANDIDATES
        );
        sessions.expire(
            1_000,
            now + CODE_PAIRING_LAN_CANDIDATE_TTL + Duration::from_secs(2),
        );
        assert!(sessions.lan_candidates.is_empty());
    }

    #[test]
    fn lan_addresses_are_capped_per_peer() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let candidate = peer(1);
        for port in
            1..=u16::try_from(MAX_CODE_PAIRING_LAN_ADDRESSES_PER_PEER + 1).expect("address cap")
        {
            sessions.record_lan_candidate(
                candidate,
                Multiaddr::empty()
                    .with(libp2p::multiaddr::Protocol::Ip4(Ipv4Addr::LOCALHOST))
                    .with(libp2p::multiaddr::Protocol::Tcp(port)),
                now,
            );
        }

        assert_eq!(
            sessions.lan_addresses(candidate).len(),
            MAX_CODE_PAIRING_LAN_ADDRESSES_PER_PEER
        );
    }

    #[test]
    fn expiring_open_session_requests_provider_removal() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let started = sessions
            .open("runners", 10, 1_000, now)
            .expect("open pairing");
        let locator = sessions
            .should_start_open_provider(now + CODE_PAIRING_LAN_GRACE)
            .expect("provider locator")
            .to_owned();
        sessions.mark_open_provider_started(&locator);

        let actions = sessions.expire(1_011, now + Duration::from_secs(11));

        assert_eq!(
            actions.stop_providing_locator.as_deref(),
            Some(locator.as_str())
        );
        assert_eq!(
            sessions.open_status(&started.operation_id).expect("status"),
            PairingOpenStatus::Expired
        );
    }

    #[test]
    fn operation_ids_make_open_idempotent_and_detect_conflicts() {
        let mut sessions = CodePairingSessions::new();
        let operation_id = fresh_pairing_operation_id();
        let now = Instant::now();
        let first = sessions
            .open_with_id(operation_id.clone(), "runners", 600, 1_000, now)
            .expect("first open");
        let retry = sessions
            .open_with_id(operation_id.clone(), "runners", 600, 1_010, now)
            .expect("idempotent retry");

        assert_eq!(retry, first);
        assert!(matches!(
            sessions.open_with_id(operation_id, "runners", 601, 1_010, now),
            Err(CodePairingSessionError::Conflict)
        ));
    }

    #[test]
    fn active_open_restores_with_same_code_and_resumes_public_discovery() {
        let mut sessions = CodePairingSessions::new();
        let operation_id = fresh_pairing_operation_id();
        let now = Instant::now();
        let started = sessions
            .open_with_id(operation_id.clone(), "runners", 600, 1_000, now)
            .expect("open");
        let bytes = sessions.encode_persisted("runners").expect("encode");

        let restored = CodePairingSessions::restore_persisted(
            &bytes,
            "runners",
            1_010,
            now + Duration::from_secs(10),
        )
        .expect("restore");

        assert_eq!(
            restored
                .active_open_code_for_locator(
                    &started
                        .code
                        .parse::<PairingCode>()
                        .expect("code")
                        .locator("runners")
                        .expect("locator")
                )
                .map(|(_, code, _)| code.to_string()),
            Some(started.code)
        );
        assert!(
            restored
                .should_start_open_provider(now + Duration::from_secs(10))
                .is_some()
        );
    }

    #[test]
    fn persisted_pairing_state_is_network_scoped() {
        let mut sessions = CodePairingSessions::new();
        sessions
            .open("runners", 600, 1_000, Instant::now())
            .expect("open");
        let bytes = sessions.encode_persisted("runners").expect("encode");

        assert!(matches!(
            CodePairingSessions::restore_persisted(&bytes, "other", 1_010, Instant::now()),
            Err(CodePairingSessionError::PersistedNetworkMismatch { .. })
        ));
    }

    #[test]
    fn approval_ticket_survives_restart_and_returns_completed_response() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let inviter = peer(1);
        let joiner = peer(2);
        let started = sessions.open("runners", 600, 1_000, now).expect("open");
        let request = test_request(inviter, joiner);
        let approval = PendingApproval::new(
            started.operation_id.clone(),
            joiner,
            started.expires_at_unix_seconds,
            request.clone(),
        )
        .expect("approval");
        let approval_id = approval.approval_id.clone();
        let ticket = approval.ticket.clone();

        let pending = sessions
            .set_pending_approval(approval)
            .expect("pending approval");
        assert_eq!(
            pending,
            PairingCodeResponse::Pending {
                ticket: ticket.clone(),
                expires_at_unix_seconds: 1_600,
            }
        );
        assert_eq!(
            sessions
                .response_for_existing_submission(joiner, &request, 1_002)
                .expect("idempotent submit"),
            Some(pending.clone())
        );
        assert_eq!(
            sessions.poll_response(peer(3), &ticket, 1_002),
            PairingCodeResponse::Rejected {
                reason: PairingCodeRejectionReason::Unavailable,
            }
        );

        let bytes = sessions
            .encode_persisted("runners")
            .expect("encode pending");
        let mut restored = CodePairingSessions::restore_persisted(
            &bytes,
            "runners",
            1_010,
            now + Duration::from_secs(10),
        )
        .expect("restore pending");
        assert_eq!(restored.poll_response(joiner, &ticket, 1_010), pending);

        let response = test_response(inviter, joiner);
        restored
            .complete_open(&started.operation_id, &approval_id, response.clone())
            .expect("complete");
        assert_eq!(
            restored.poll_response(joiner, &ticket, 1_011),
            PairingCodeResponse::Accepted {
                response: Box::new(response.clone()),
            }
        );
        let completed_bytes = restored
            .encode_persisted("runners")
            .expect("encode completed");
        let completed = CodePairingSessions::restore_persisted(
            &completed_bytes,
            "runners",
            1_012,
            now + Duration::from_secs(12),
        )
        .expect("restore completed");
        assert_eq!(
            completed.poll_response(joiner, &ticket, 1_012),
            PairingCodeResponse::Accepted {
                response: Box::new(response),
            }
        );
    }

    #[test]
    fn approval_at_original_deadline_extends_ticket_through_response_expiry() {
        let (mut sessions, enrollment, ticket, joiner, now) = prepared_open_fixture(10);
        let operation_id = enrollment.operation_id.clone();
        let approval_id = enrollment.approval_id.clone().expect("approval ID");
        let response = enrollment.response.clone();

        sessions.expire(1_010, now + Duration::from_secs(10));
        sessions
            .complete_open(&operation_id, &approval_id, response.clone())
            .expect("approve at original deadline");

        let accepted = PairingCodeResponse::Accepted {
            response: Box::new(response),
        };
        assert_eq!(sessions.poll_response(joiner, &ticket, 1_011), accepted);
        assert_eq!(sessions.poll_response(joiner, &ticket, 1_600), accepted);
        assert_eq!(
            sessions.poll_response(joiner, &ticket, 1_601),
            PairingCodeResponse::Rejected {
                reason: PairingCodeRejectionReason::Expired,
            }
        );
    }

    #[test]
    fn completed_polling_outcome_survives_a_subsequent_open_and_restart() {
        let (mut sessions, enrollment, ticket, joiner, now) = prepared_open_fixture(10);
        let approval_id = enrollment.approval_id.clone().expect("approval ID");
        let response = enrollment.response.clone();
        sessions
            .complete_open(&enrollment.operation_id, &approval_id, response.clone())
            .expect("complete first open");

        sessions
            .open("runners", 10, 1_011, now + Duration::from_secs(11))
            .expect("subsequent open");
        let accepted = PairingCodeResponse::Accepted {
            response: Box::new(response),
        };
        assert_eq!(sessions.poll_response(joiner, &ticket, 1_011), accepted);
        assert_eq!(sessions.retained_inbound_tickets.len(), 1);

        let restored = CodePairingSessions::restore_persisted(
            &sessions.encode_persisted("runners").expect("encode"),
            "runners",
            1_012,
            now + Duration::from_secs(12),
        )
        .expect("restore");
        assert_eq!(restored.poll_response(joiner, &ticket, 1_012), accepted);
    }

    #[test]
    fn retained_polling_outcomes_are_strictly_bounded() {
        let mut sessions = CodePairingSessions::new();
        let inviter = peer(1);
        let joiner = peer(2);
        let approval_id = test_approval_id(inviter, joiner);
        let response = test_response(inviter, joiner);
        let mut oldest_ticket = String::new();
        let mut newest_ticket = String::new();

        for index in 0..=MAX_RETAINED_PAIRING_TICKETS {
            let ticket = fresh_pairing_ticket();
            if index == 0 {
                oldest_ticket.clone_from(&ticket);
            }
            newest_ticket.clone_from(&ticket);
            sessions.inbound_ticket = Some(InboundTicket {
                operation_id: fresh_pairing_operation_id(),
                approval_id: approval_id.clone(),
                peer: joiner,
                ticket,
                expires_at_unix_seconds: 1_600,
                outcome: InboundTicketOutcome::Accepted(Box::new(response.clone())),
            });
            sessions
                .archive_inbound_ticket(1_010)
                .expect("archive terminal ticket");
        }

        assert_eq!(
            sessions.retained_inbound_tickets.len(),
            MAX_RETAINED_PAIRING_TICKETS
        );
        assert_eq!(
            sessions.poll_response(joiner, &oldest_ticket, 1_010),
            PairingCodeResponse::Rejected {
                reason: PairingCodeRejectionReason::Unavailable,
            }
        );
        assert_eq!(
            sessions.poll_response(joiner, &newest_ticket, 1_010),
            PairingCodeResponse::Accepted {
                response: Box::new(response),
            }
        );
    }

    #[test]
    fn remote_pending_poll_survives_restart() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let inviter = peer(1);
        let started = sessions
            .join(
                "runners",
                PairingCode::generate(),
                None,
                Vec::new(),
                600,
                1_000,
                now,
            )
            .expect("join");
        let ticket = fresh_pairing_ticket();
        sessions
            .set_remote_pending(
                &started.operation_id,
                inviter,
                test_offer(inviter),
                test_transcript_sha256(),
                ticket.clone(),
                now,
            )
            .expect("remote pending");

        let first = sessions.due_remote_poll(now).expect("first poll");
        assert_eq!(first.ticket, ticket);
        assert!(sessions.due_remote_poll(now).is_none());
        assert!(
            sessions
                .due_remote_poll(now + CODE_PAIRING_POLL_INTERVAL)
                .is_none()
        );
        sessions.release_remote_poll(&started.operation_id, inviter);
        assert!(
            sessions
                .due_remote_poll(now + CODE_PAIRING_POLL_INTERVAL)
                .is_some()
        );

        let bytes = sessions.encode_persisted("runners").expect("encode");
        let mut restored = CodePairingSessions::restore_persisted(
            &bytes,
            "runners",
            1_010,
            now + Duration::from_secs(10),
        )
        .expect("restore");
        let resumed = restored
            .due_remote_poll(now + Duration::from_secs(10))
            .expect("resumed poll");
        assert_eq!(resumed.operation_id, started.operation_id);
        assert_eq!(resumed.peer, inviter);
        assert_eq!(resumed.ticket, first.ticket);
    }

    #[test]
    fn invalid_remote_ticket_does_not_select_inviter() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let inviter = peer(1);
        let started = sessions
            .join(
                "runners",
                PairingCode::generate(),
                None,
                Vec::new(),
                600,
                1_000,
                now,
            )
            .expect("join");

        assert!(matches!(
            sessions.set_remote_pending(
                &started.operation_id,
                inviter,
                test_offer(inviter),
                test_transcript_sha256(),
                "not-a-ticket".to_owned(),
                now,
            ),
            Err(CodePairingSessionError::InvalidTicket)
        ));
        assert!(matches!(
            sessions
                .join_status(&started.operation_id)
                .expect("join status"),
            PairingJoinStatus::Searching { .. }
        ));
        assert!(sessions.due_remote_poll(now).is_none());
    }

    #[test]
    fn expired_remote_poll_is_not_resumed_after_restart() {
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let inviter = peer(1);
        let started = sessions
            .join(
                "runners",
                PairingCode::generate(),
                None,
                Vec::new(),
                10,
                1_000,
                now,
            )
            .expect("join");
        sessions
            .set_remote_pending(
                &started.operation_id,
                inviter,
                test_offer(inviter),
                test_transcript_sha256(),
                fresh_pairing_ticket(),
                now,
            )
            .expect("remote pending");

        let bytes = sessions.encode_persisted("runners").expect("encode");
        let mut restored = CodePairingSessions::restore_persisted(
            &bytes,
            "runners",
            1_011,
            now + Duration::from_secs(11),
        )
        .expect("restore expired");

        assert!(matches!(
            restored
                .join_status(&started.operation_id)
                .expect("join status"),
            PairingJoinStatus::Expired
        ));
        assert!(
            restored
                .due_remote_poll(now + Duration::from_secs(11))
                .is_none()
        );
    }

    #[test]
    fn prepared_open_recovery_finalizes_idempotently_after_expired_restart() {
        let (sessions, enrollment, ticket, joiner, now) = prepared_open_fixture(10);
        let bytes = sessions.encode_persisted("runners").expect("encode");
        let mut restored = CodePairingSessions::restore_persisted(
            &bytes,
            "runners",
            1_011,
            now + Duration::from_secs(11),
        )
        .expect("restore after operation deadline");

        assert_eq!(
            restored
                .open_status(&enrollment.operation_id)
                .expect("expired status"),
            PairingOpenStatus::Expired
        );
        restored
            .recover_prepared_open("runners", &enrollment)
            .expect("recover prepared inviter enrollment");
        restored
            .recover_prepared_open("runners", &enrollment)
            .expect("repeat recovery idempotently");

        assert_eq!(
            restored
                .open_status(&enrollment.operation_id)
                .expect("completed status"),
            PairingOpenStatus::Completed
        );
        assert_eq!(
            restored.open_completion(&enrollment.operation_id),
            Some(&enrollment.response)
        );
        assert_eq!(
            restored.poll_response(joiner, &ticket, 1_600),
            PairingCodeResponse::Accepted {
                response: Box::new(enrollment.response.clone()),
            }
        );
        assert!(restored.pending_approval.is_none());
    }

    #[test]
    fn prepared_join_recovery_finalizes_idempotently_after_expired_restart() {
        let (sessions, enrollment, now) = prepared_join_fixture(10);
        let bytes = sessions.encode_persisted("runners").expect("encode");
        let mut restored = CodePairingSessions::restore_persisted(
            &bytes,
            "runners",
            1_011,
            now + Duration::from_secs(11),
        )
        .expect("restore after operation deadline");

        assert_eq!(
            restored
                .join_status(&enrollment.operation_id)
                .expect("expired status"),
            PairingJoinStatus::Expired
        );
        restored
            .recover_prepared_join("runners", &enrollment)
            .expect("recover prepared joiner enrollment");
        restored
            .recover_prepared_join("runners", &enrollment)
            .expect("repeat recovery idempotently");

        assert_eq!(
            restored
                .join_status(&enrollment.operation_id)
                .expect("completed status"),
            PairingJoinStatus::Completed
        );
        assert_eq!(
            restored.join_completion(&enrollment.operation_id),
            Some((
                enrollment.offer.as_ref().expect("offer"),
                &enrollment.response,
            ))
        );
    }

    #[test]
    fn prepared_open_recovery_rejects_mismatched_approval_ticket() {
        let (mut sessions, enrollment, _, _, _) = prepared_open_fixture(10);
        sessions.inbound_ticket.as_mut().expect("ticket").peer = peer(3);

        assert!(matches!(
            sessions.recover_prepared_open("runners", &enrollment),
            Err(CodePairingSessionError::InvalidPersistedState(_))
        ));
        assert!(sessions.open_completion(&enrollment.operation_id).is_none());
    }

    #[test]
    fn prepared_join_recovery_rejects_mismatched_remote_approval() {
        let (mut sessions, enrollment, _) = prepared_join_fixture(10);
        sessions
            .join
            .as_mut()
            .expect("join")
            .remote_approval
            .as_mut()
            .expect("remote approval")
            .peer = peer(3);

        assert!(matches!(
            sessions.recover_prepared_join("runners", &enrollment),
            Err(CodePairingSessionError::InvalidPersistedState(_))
        ));
        assert!(sessions.join_completion(&enrollment.operation_id).is_none());
    }

    #[test]
    fn prepared_recovery_never_overwrites_a_different_operation() {
        let (mut open_sessions, open_enrollment, _, _, now) = prepared_open_fixture(10);
        open_sessions
            .cancel(&open_enrollment.operation_id)
            .expect("cancel original open");
        let replacement_open = open_sessions
            .open("runners", 10, 1_001, now + Duration::from_secs(1))
            .expect("replacement open");
        assert!(matches!(
            open_sessions.recover_prepared_open("runners", &open_enrollment),
            Err(CodePairingSessionError::Conflict)
        ));
        assert!(matches!(
            open_sessions
                .open_status(&replacement_open.operation_id)
                .expect("replacement open status"),
            PairingOpenStatus::Searching { .. }
        ));

        let (mut join_sessions, join_enrollment, now) = prepared_join_fixture(10);
        join_sessions
            .cancel(&join_enrollment.operation_id)
            .expect("cancel original join");
        let replacement_join = join_sessions
            .join(
                "runners",
                PairingCode::generate(),
                None,
                Vec::new(),
                10,
                1_001,
                now + Duration::from_secs(1),
            )
            .expect("replacement join");
        assert!(matches!(
            join_sessions.recover_prepared_join("runners", &join_enrollment),
            Err(CodePairingSessionError::Conflict)
        ));
        assert!(matches!(
            join_sessions
                .join_status(&replacement_join.operation_id)
                .expect("replacement join status"),
            PairingJoinStatus::Searching { .. }
        ));
    }

    #[test]
    fn enrollment_ledger_round_trips_multiple_roles_and_states() {
        let inviter = peer(1);
        let joiner = peer(2);
        let inviter_operation_id = fresh_pairing_operation_id();
        let joiner_operation_id = fresh_pairing_operation_id();
        let approval_id = test_approval_id(inviter, joiner);
        let response = test_response(inviter, joiner);
        let offer = test_offer(inviter);
        let transcript_sha256 = test_transcript_sha256();
        let mut sessions = CodePairingSessions::new();

        sessions
            .prepare_enrollment(
                "runners",
                test_preparation(
                    inviter_operation_id.clone(),
                    PairingEnrollmentRole::Inviter,
                    Some(approval_id.clone()),
                    Some(offer.clone()),
                    response.clone(),
                    transcript_sha256.clone(),
                ),
            )
            .expect("prepare inviter enrollment");
        sessions
            .mark_enrollment_applied(&inviter_operation_id)
            .expect("apply inviter enrollment");
        sessions
            .mark_enrollment_applied(&inviter_operation_id)
            .expect("idempotently apply inviter enrollment");
        sessions
            .prepare_enrollment(
                "runners",
                test_preparation(
                    joiner_operation_id.clone(),
                    PairingEnrollmentRole::Joiner,
                    None,
                    Some(offer.clone()),
                    response.clone(),
                    transcript_sha256.clone(),
                ),
            )
            .expect("prepare joiner enrollment");

        let bytes = sessions.encode_persisted("runners").expect("encode ledger");
        let restored =
            CodePairingSessions::restore_persisted(&bytes, "runners", 1_010, Instant::now())
                .expect("restore ledger");

        assert_eq!(restored.enrollments().len(), 2);
        assert_eq!(
            restored.enrollment(&inviter_operation_id),
            Some(&PairingEnrollment {
                operation_id: inviter_operation_id,
                role: PairingEnrollmentRole::Inviter,
                approval_id: Some(approval_id),
                offer: Some(offer.clone()),
                response: response.clone(),
                transcript_sha256: transcript_sha256.clone(),
                completed_at_unix_seconds: Some(1_002),
                state: PairingEnrollmentState::Applied,
            })
        );
        assert_eq!(
            restored.enrollment(&joiner_operation_id),
            Some(&PairingEnrollment {
                operation_id: joiner_operation_id,
                role: PairingEnrollmentRole::Joiner,
                approval_id: None,
                offer: Some(offer),
                response,
                transcript_sha256,
                completed_at_unix_seconds: None,
                state: PairingEnrollmentState::Prepared,
            })
        );
    }

    #[test]
    fn enrollment_prepare_is_exactly_idempotent_and_rejects_conflicts() {
        let inviter = peer(1);
        let joiner = peer(2);
        let operation_id = fresh_pairing_operation_id();
        let offer = test_offer(inviter);
        let response = test_response(inviter, joiner);
        let transcript_sha256 = test_transcript_sha256();
        let mut sessions = CodePairingSessions::new();

        assert!(matches!(
            sessions.prepare_enrollment(
                "runners",
                test_preparation(
                    operation_id.clone(),
                    PairingEnrollmentRole::Joiner,
                    None,
                    Some(offer.clone()),
                    response.clone(),
                    "invalid".to_owned(),
                ),
            ),
            Err(CodePairingSessionError::InvalidPersistedState(_))
        ));

        let first = sessions
            .prepare_enrollment(
                "runners",
                test_preparation(
                    operation_id.clone(),
                    PairingEnrollmentRole::Joiner,
                    None,
                    Some(offer.clone()),
                    response.clone(),
                    transcript_sha256.clone(),
                ),
            )
            .expect("prepare enrollment")
            .clone();
        let retry = sessions
            .prepare_enrollment(
                "runners",
                test_preparation(
                    operation_id.clone(),
                    PairingEnrollmentRole::Joiner,
                    None,
                    Some(offer.clone()),
                    response.clone(),
                    transcript_sha256.clone(),
                ),
            )
            .expect("retry exact enrollment");
        assert_eq!(retry, &first);

        sessions
            .mark_enrollment_applied_at(&operation_id, 2_000)
            .expect("apply enrollment");
        sessions
            .mark_enrollment_applied_at(&operation_id, 3_000)
            .expect("idempotently apply enrollment");
        assert_eq!(
            sessions
                .enrollment(&operation_id)
                .expect("applied enrollment")
                .completed_at_unix_seconds,
            Some(2_000)
        );
        assert_eq!(
            sessions
                .prepare_enrollment(
                    "runners",
                    test_preparation(
                        operation_id.clone(),
                        PairingEnrollmentRole::Joiner,
                        None,
                        Some(offer.clone()),
                        response.clone(),
                        transcript_sha256.clone(),
                    ),
                )
                .expect("retry applied enrollment")
                .state,
            PairingEnrollmentState::Applied
        );

        let mut conflicting_response = response;
        conflicting_response.payload.assigned_vpn_ip = Some("10.42.0.3".to_owned());
        assert!(matches!(
            sessions.prepare_enrollment(
                "runners",
                test_preparation(
                    operation_id,
                    PairingEnrollmentRole::Joiner,
                    None,
                    Some(offer),
                    conflicting_response,
                    transcript_sha256,
                ),
            ),
            Err(CodePairingSessionError::Conflict)
        ));
    }

    #[test]
    fn enrollment_ledger_enforces_runtime_and_restore_capacity() {
        let inviter = peer(1);
        let joiner = peer(2);
        let offer = test_offer(inviter);
        let response = test_response(inviter, joiner);
        let transcript_sha256 = test_transcript_sha256();
        let mut sessions = CodePairingSessions::new();
        let first_operation_id = fresh_pairing_operation_id();

        for index in 0..MAX_CODE_PAIRING_ENROLLMENTS {
            let operation_id = if index == 0 {
                first_operation_id.clone()
            } else {
                fresh_pairing_operation_id()
            };
            sessions
                .prepare_enrollment(
                    "runners",
                    test_preparation(
                        operation_id,
                        PairingEnrollmentRole::Joiner,
                        None,
                        Some(offer.clone()),
                        response.clone(),
                        transcript_sha256.clone(),
                    ),
                )
                .expect("prepare bounded enrollment");
        }
        assert_eq!(sessions.enrollments().len(), MAX_CODE_PAIRING_ENROLLMENTS);
        assert!(
            sessions
                .prepare_enrollment(
                    "runners",
                    test_preparation(
                        first_operation_id,
                        PairingEnrollmentRole::Joiner,
                        None,
                        Some(offer.clone()),
                        response.clone(),
                        transcript_sha256.clone(),
                    ),
                )
                .is_ok()
        );
        assert!(matches!(
            sessions.prepare_enrollment(
                "runners",
                test_preparation(
                    fresh_pairing_operation_id(),
                    PairingEnrollmentRole::Joiner,
                    None,
                    Some(offer),
                    response,
                    transcript_sha256,
                ),
            ),
            Err(CodePairingSessionError::Capacity)
        ));

        let mut persisted: serde_json::Value = serde_json::from_slice(
            &sessions
                .encode_persisted("runners")
                .expect("encode full ledger"),
        )
        .expect("decode full ledger");
        let enrollments = persisted["enrollments"]
            .as_array_mut()
            .expect("enrollment array");
        let mut extra = enrollments[0].clone();
        extra["operation_id"] = fresh_pairing_operation_id().into();
        enrollments.push(extra);
        assert!(matches!(
            restore_json(&persisted),
            Err(CodePairingSessionError::InvalidPersistedState(_))
        ));
    }

    #[test]
    fn enrollment_restore_rejects_invalid_relationships_and_duplicates() {
        let base = persisted_joiner_enrollment();
        let mut invalid_states = Vec::new();

        let mut wrong_network = base.clone();
        wrong_network["enrollments"][0]["response"]["payload"]["network_name"] = "other".into();
        invalid_states.push(wrong_network);

        let mut wrong_role = base.clone();
        wrong_role["enrollments"][0]["role"] = "inviter".into();
        invalid_states.push(wrong_role);

        let mut invalid_peer = base.clone();
        invalid_peer["enrollments"][0]["response"]["payload"]["joiner_peer"] = "not-a-peer".into();
        invalid_states.push(invalid_peer);

        let mut mismatched_offer = base.clone();
        mismatched_offer["enrollments"][0]["offer"]["payload"]["inviter_peer"] =
            peer(3).to_string().into();
        invalid_states.push(mismatched_offer);

        let mut duplicate = base;
        let duplicate_enrollment = duplicate["enrollments"][0].clone();
        duplicate["enrollments"]
            .as_array_mut()
            .expect("enrollment array")
            .push(duplicate_enrollment);
        invalid_states.push(duplicate);

        for invalid in invalid_states {
            assert!(matches!(
                restore_json(&invalid),
                Err(CodePairingSessionError::InvalidPersistedState(_))
            ));
        }
    }

    #[test]
    fn enrollment_restore_rejects_inviter_without_offer() {
        let inviter = peer(1);
        let joiner = peer(2);
        let mut persisted = persisted_joiner_enrollment();
        persisted["enrollments"][0]["role"] = "inviter".into();
        persisted["enrollments"][0]["approval_id"] = test_approval_id(inviter, joiner).into();
        persisted["enrollments"][0]
            .as_object_mut()
            .expect("enrollment object")
            .remove("offer");

        assert!(matches!(
            restore_json(&persisted),
            Err(CodePairingSessionError::InvalidPersistedState(reason))
                if reason.contains("signed code-approval offer")
        ));
    }

    #[test]
    fn persisted_v1_without_enrollments_restores_an_empty_ledger() {
        let sessions = CodePairingSessions::new();
        let mut persisted: serde_json::Value =
            serde_json::from_slice(&sessions.encode_persisted("runners").expect("encode state"))
                .expect("decode state");
        persisted
            .as_object_mut()
            .expect("persisted state object")
            .remove("enrollments");
        persisted
            .as_object_mut()
            .expect("persisted state object")
            .remove("retained_inbound_tickets");

        let restored = restore_json(&persisted).expect("restore legacy v1 state");
        assert_eq!(restored.enrollments().len(), 0);
        assert!(restored.retained_inbound_tickets.is_empty());
    }
}
