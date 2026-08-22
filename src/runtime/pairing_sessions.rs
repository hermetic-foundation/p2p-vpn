use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use libp2p::{
    Multiaddr, PeerId as Libp2pPeerId,
    request_response::{OutboundRequestId, ResponseChannel},
    swarm::ConnectionId,
};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use sha2_010::{Digest as _, Sha256};

use crate::{
    config::RouteConfig,
    pairing::{PairingOffer, PairingRequest, PairingResponse},
    pairing_code::{PairingCode, PairingCodeSession, PendingPairingCodeHello},
};

pub const DEFAULT_CODE_PAIRING_EXPIRES_IN_SECONDS: u64 = 10 * 60;
pub const MAX_CODE_PAIRING_EXPIRES_IN_SECONDS: u64 = 60 * 60;
pub const CODE_PAIRING_LAN_GRACE: Duration = Duration::from_secs(3);
pub const CODE_PAIRING_TICK: Duration = Duration::from_secs(1);
pub const CODE_PAIRING_LAN_CANDIDATE_TTL: Duration = Duration::from_mins(2);
pub const MAX_CODE_PAIRING_LAN_CANDIDATES: usize = 128;
pub const MAX_CODE_PAIRING_LAN_ADDRESSES_PER_PEER: usize = 8;
pub const MAX_CODE_PAIRING_PEER_ATTEMPTS: usize = 128;
pub const MAX_PENDING_CODE_HELLOS: usize = 32;
pub const MAX_INBOUND_CODE_SESSIONS: usize = 8;

const OPERATION_ID_BYTES: usize = 16;

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

pub struct CodePairingSessions {
    open: Option<OpenOperation>,
    join: Option<JoinOperation>,
    lan_candidates: HashMap<Libp2pPeerId, LanCandidate>,
    outbound_hellos: HashMap<OutboundRequestId, OutboundHello>,
    inbound_sessions: HashMap<(Libp2pPeerId, String), InboundSession>,
    outbound_pairing: HashMap<OutboundRequestId, OutboundPairing>,
    pending_approval: Option<PendingApproval>,
}

struct OpenOperation {
    id: String,
    code: Option<PairingCode>,
    locator: String,
    opened_at: Instant,
    expires_at_unix_seconds: u64,
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
    public_lookup_started: bool,
    attempted_peers: HashSet<Libp2pPeerId>,
    selected_inviter: Option<Libp2pPeerId>,
    requested_vpn_ip: Option<String>,
    requested_routes: Vec<RouteConfig>,
    completed: Option<(PairingOffer, PairingResponse)>,
    terminal: Option<TerminalStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
}

pub struct PendingApproval {
    pub operation_id: String,
    pub approval_id: String,
    pub peer: Libp2pPeerId,
    pub connection_id: ConnectionId,
    pub request: PairingRequest,
    pub channel: ResponseChannel<PairingResponse>,
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
        connection_id: ConnectionId,
        request: PairingRequest,
        channel: ResponseChannel<PairingResponse>,
    ) -> Result<Self, CodePairingSessionError> {
        let approval_id = pairing_approval_id(&request)?;
        Ok(Self {
            operation_id,
            approval_id,
            peer,
            connection_id,
            request,
            channel,
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
            lan_candidates: HashMap::new(),
            outbound_hellos: HashMap::new(),
            inbound_sessions: HashMap::new(),
            outbound_pairing: HashMap::new(),
            pending_approval: None,
        }
    }

    pub fn open(
        &mut self,
        network_name: &str,
        expires_in_seconds: u64,
        now_unix_seconds: u64,
        now: Instant,
    ) -> Result<PairingOpenStarted, CodePairingSessionError> {
        self.ensure_idle()?;
        validate_expiry(expires_in_seconds)?;
        let expires_at_unix_seconds = now_unix_seconds
            .checked_add(expires_in_seconds)
            .ok_or(CodePairingSessionError::ExpiryOverflow)?;
        let code = PairingCode::generate();
        let locator = code.locator(network_name)?;
        let operation_id = fresh_operation_id();
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
        self.ensure_idle()?;
        validate_expiry(expires_in_seconds)?;
        let expires_at_unix_seconds = now_unix_seconds
            .checked_add(expires_in_seconds)
            .ok_or(CodePairingSessionError::ExpiryOverflow)?;
        let locator = code.locator(network_name)?;
        let operation_id = fresh_operation_id();
        self.join = Some(JoinOperation {
            id: operation_id.clone(),
            code: Some(code),
            locator,
            started_at: now,
            expires_at_unix_seconds,
            public_lookup_started: false,
            attempted_peers: HashSet::new(),
            selected_inviter: None,
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
        self.pending_approval.take();
        let locator = self.deactivate_open(TerminalStatus::Rejected);
        Ok(PairingExpiryActions {
            stop_providing_locator: locator,
        })
    }

    pub fn take_approval(
        &mut self,
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
        self.pending_approval
            .take()
            .ok_or(CodePairingSessionError::NotAwaitingApproval)
    }

    pub fn restore_approval(&mut self, approval: PendingApproval) {
        self.pending_approval = Some(approval);
    }

    pub fn complete_open(
        &mut self,
        operation_id: &str,
        response: PairingResponse,
    ) -> Result<PairingExpiryActions, CodePairingSessionError> {
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
        operation.completed = Some((offer, response));
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

    pub fn fail_join(&mut self, operation_id: &str, reason: impl Into<String>) {
        if self
            .join
            .as_ref()
            .is_some_and(|operation| operation.id == operation_id)
        {
            self.deactivate_join(TerminalStatus::Failed(reason.into()));
        }
    }

    pub fn expire(&mut self, now_unix_seconds: u64, now: Instant) -> PairingExpiryActions {
        self.prune_lan_candidates(now);
        self.inbound_sessions
            .retain(|_, inbound| inbound.session.expires_at_unix_seconds() >= now_unix_seconds);

        let mut actions = PairingExpiryActions::default();
        if self.open.as_ref().is_some_and(|operation| {
            operation.terminal.is_none()
                && operation.completed.is_none()
                && now_unix_seconds > operation.expires_at_unix_seconds
        }) {
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
        (!operation.public_lookup_started
            && now.saturating_duration_since(operation.started_at) >= CODE_PAIRING_LAN_GRACE)
            .then_some(operation.locator.as_str())
    }

    pub fn mark_join_lookup_started(&mut self, locator: &str) {
        if let Some(operation) = self.join.as_mut()
            && operation.locator == locator
            && operation.terminal.is_none()
            && operation.completed.is_none()
        {
            operation.public_lookup_started = true;
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
        self.active_join().is_some_and(|join| {
            join.attempted_peers.contains(&peer) || join.selected_inviter == Some(peer)
        }) || self
            .inbound_sessions
            .keys()
            .any(|(session_peer, _)| *session_peer == peer)
            || self
                .pending_approval
                .as_ref()
                .is_some_and(|approval| approval.peer == peer)
    }

    pub fn insert_outbound_hello(
        &mut self,
        request_id: OutboundRequestId,
        hello: OutboundHello,
    ) -> Result<(), CodePairingSessionError> {
        if self.outbound_hellos.len() >= MAX_PENDING_CODE_HELLOS {
            return Err(CodePairingSessionError::Capacity);
        }
        self.outbound_hellos.insert(request_id, hello);
        Ok(())
    }

    pub fn take_outbound_hello(&mut self, request_id: OutboundRequestId) -> Option<OutboundHello> {
        self.outbound_hellos.remove(&request_id)
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
    ) -> Result<(), CodePairingSessionError> {
        if self.pending_approval.is_some() {
            return Err(CodePairingSessionError::Capacity);
        }
        self.pending_approval = Some(approval);
        Ok(())
    }

    pub fn insert_outbound_pairing(
        &mut self,
        request_id: OutboundRequestId,
        pairing: OutboundPairing,
    ) {
        self.outbound_pairing.insert(request_id, pairing);
    }

    pub fn take_outbound_pairing(
        &mut self,
        request_id: OutboundRequestId,
    ) -> Option<OutboundPairing> {
        self.outbound_pairing.remove(&request_id)
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
            operation.terminal = Some(terminal);
        }
        self.clear_transient_handshakes();
    }

    fn clear_transient_handshakes(&mut self) {
        self.outbound_hellos.clear();
        self.inbound_sessions.clear();
        self.outbound_pairing.clear();
        self.pending_approval.take();
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

fn validate_expiry(expires_in_seconds: u64) -> Result<(), CodePairingSessionError> {
    if expires_in_seconds == 0 || expires_in_seconds > MAX_CODE_PAIRING_EXPIRES_IN_SECONDS {
        Err(CodePairingSessionError::InvalidExpiry)
    } else {
        Ok(())
    }
}

fn fresh_operation_id() -> String {
    let mut bytes = [0_u8; OPERATION_ID_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
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
        sessions.mark_join_lookup_started(&locator);
        assert_eq!(
            sessions.join_status(&started.operation_id).expect("status"),
            PairingJoinStatus::Searching {
                discovery: PairingDiscoveryStage::Public,
                expires_at_unix_seconds: 1_600,
            }
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
}
