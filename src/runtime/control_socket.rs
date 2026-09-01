use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
};

use crate::{
    network_peer::{NETWORK_PEER_LIST_SCHEMA_VERSION, NetworkPeerList, NetworkPeerSnapshot},
    runtime::dns::{DnsLookupType, MAX_DNS_CONTROL_LIST_LIMIT},
};

const STATUS_REQUEST: &[u8] = b"status\n";
const STATE_REQUEST: &[u8] = b"state\n";
const PEERS_REQUEST: &[u8] = b"peers\n";
const ROUTES_REQUEST: &[u8] = b"routes\n";
const PATHS_REQUEST: &[u8] = b"paths\n";
const MTU_REQUEST: &[u8] = b"mtu\n";
const CAPABILITIES_REQUEST: &[u8] = b"capabilities\n";
const NETWORK_PEERS_REQUEST: &[u8] = b"network-peers-v1\n";
const SHUTDOWN_REQUEST: &[u8] = b"shutdown\n";
const PAIR_RPC_FRAME_PREFIX: &str = "rpc-v1 ";
const MAX_REQUEST_LEN: usize = 512;
const MAX_RESPONSE_LEN: usize = 256 * 1024;
const REQUEST_CHANNEL: usize = 16;

pub const PAIR_RPC_VERSION: u8 = 1;
pub const MAX_PAIR_RPC_REQUEST_LEN: usize = 64 * 1024;
pub const MAX_PAIR_RPC_RESPONSE_LEN: usize = MAX_RESPONSE_LEN;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcRequestEnvelope {
    pub version: u8,
    pub request: PairRpcRequest,
}

#[derive(Serialize)]
struct PairRpcRequestEnvelopeRef<'a> {
    version: u8,
    request: &'a PairRpcRequest,
}

impl PairRpcRequestEnvelope {
    #[must_use]
    pub const fn new(request: PairRpcRequest) -> Self {
        Self {
            version: PAIR_RPC_VERSION,
            request,
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum PairRpcRequest {
    PairOpen {
        operation_id: String,
        expires_in_seconds: u64,
    },
    PairJoin {
        operation_id: String,
        code: String,
        timeout_seconds: u64,
        #[serde(default)]
        requested_vpn_ip: Option<String>,
        #[serde(default)]
        requested_routes: Option<Vec<PairRpcRoute>>,
    },
    PairStatus {
        operation_id: String,
    },
    PairApprove {
        operation_id: String,
        approval_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assigned_hostname: Option<String>,
        #[serde(default)]
        assigned_vpn_ip: Option<String>,
        #[serde(default)]
        granted_routes: Vec<PairRpcRoute>,
    },
    PairReject {
        operation_id: String,
        approval_id: String,
        reason: PairRpcRejectionReason,
    },
    PairCancel {
        operation_id: String,
    },
    PairArtifacts {
        operation_id: String,
    },
    PairAcknowledge {
        operation_id: String,
        transcript_sha256: String,
    },
}

impl fmt::Debug for PairRpcRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PairOpen {
                operation_id,
                expires_in_seconds,
            } => formatter
                .debug_struct("PairOpen")
                .field("operation_id", operation_id)
                .field("expires_in_seconds", expires_in_seconds)
                .finish(),
            Self::PairJoin {
                operation_id,
                code: _,
                timeout_seconds,
                requested_vpn_ip,
                requested_routes,
            } => formatter
                .debug_struct("PairJoin")
                .field("operation_id", operation_id)
                .field("code", &"[REDACTED]")
                .field("timeout_seconds", timeout_seconds)
                .field("requested_vpn_ip", requested_vpn_ip)
                .field("requested_routes", requested_routes)
                .finish(),
            Self::PairStatus { operation_id } => formatter
                .debug_struct("PairStatus")
                .field("operation_id", operation_id)
                .finish(),
            Self::PairApprove {
                operation_id,
                approval_id,
                assigned_hostname,
                assigned_vpn_ip,
                granted_routes,
            } => formatter
                .debug_struct("PairApprove")
                .field("operation_id", operation_id)
                .field("approval_id", approval_id)
                .field("assigned_hostname", assigned_hostname)
                .field("assigned_vpn_ip", assigned_vpn_ip)
                .field("granted_routes", granted_routes)
                .finish(),
            Self::PairReject {
                operation_id,
                approval_id,
                reason,
            } => formatter
                .debug_struct("PairReject")
                .field("operation_id", operation_id)
                .field("approval_id", approval_id)
                .field("reason", reason)
                .finish(),
            Self::PairCancel { operation_id } => formatter
                .debug_struct("PairCancel")
                .field("operation_id", operation_id)
                .finish(),
            Self::PairArtifacts { operation_id } => formatter
                .debug_struct("PairArtifacts")
                .field("operation_id", operation_id)
                .finish(),
            Self::PairAcknowledge {
                operation_id,
                transcript_sha256,
            } => formatter
                .debug_struct("PairAcknowledge")
                .field("operation_id", operation_id)
                .field("transcript_sha256", transcript_sha256)
                .finish(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcRoute {
    pub prefix: String,
    #[serde(default)]
    pub metric: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRpcRejectionReason {
    Declined,
    IdentityMismatch,
    AddressConflict,
    RouteRequestDenied,
    Policy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcResponseEnvelope {
    pub version: u8,
    pub outcome: PairRpcOutcome,
}

impl PairRpcResponseEnvelope {
    #[must_use]
    pub const fn ok(result: PairRpcResult) -> Self {
        Self {
            version: PAIR_RPC_VERSION,
            outcome: PairRpcOutcome::Ok { result },
        }
    }

    #[must_use]
    pub fn error(code: PairRpcErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            version: PAIR_RPC_VERSION,
            outcome: PairRpcOutcome::Error {
                error: PairRpcError {
                    code,
                    message: message.into(),
                    retryable,
                },
            },
        }
    }

    pub(crate) fn encoded_len(&self) -> Result<usize, serde_json::Error> {
        serde_json::to_vec(self).map(|body| body.len())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PairRpcOutcome {
    Ok { result: PairRpcResult },
    Error { error: PairRpcError },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PairRpcResult {
    OpenStarted(PairRpcOpenStarted),
    JoinStarted(PairRpcJoinStarted),
    OperationStatus(Box<PairRpcOperationStatus>),
    ActionAccepted(Box<PairRpcOperationStatus>),
    Artifacts(Box<PairRpcCompletionArtifacts>),
    Acknowledged(PairRpcReceipt),
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcOpenStarted {
    pub operation_id: String,
    pub code: String,
    pub network_name: String,
    pub local_peer: String,
    pub expires_at_unix_seconds: u64,
}

impl fmt::Debug for PairRpcOpenStarted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairRpcOpenStarted")
            .field("operation_id", &self.operation_id)
            .field("code", &"[REDACTED]")
            .field("network_name", &self.network_name)
            .field("local_peer", &self.local_peer)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcJoinStarted {
    pub operation_id: String,
    pub network_name: String,
    pub local_peer: String,
    pub expires_at_unix_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcOperationStatus {
    pub operation_id: String,
    pub network_name: String,
    pub local_peer: String,
    pub role: PairRpcRole,
    pub phase: PairRpcPhase,
    pub revision: u64,
    #[serde(default)]
    pub discovery: Option<PairRpcDiscoveryStage>,
    #[serde(default)]
    pub diagnostics: PairRpcDiagnostics,
    pub expires_at_unix_seconds: u64,
    #[serde(default)]
    pub candidate: Option<PairRpcCandidate>,
    pub artifacts_ready: bool,
    #[serde(default)]
    pub failure: Option<PairRpcFailure>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct PairRpcDiagnostics {
    pub lan_candidates: u16,
    pub handshake_attempts: u16,
    pub handshake_retries: u16,
    pub public_provider_attempts: u16,
    pub public_lookups: u16,
    pub public_providers_found: u16,
    pub poll_transport_failures: u16,
    pub route_recovery_active: bool,
    #[serde(default)]
    pub selected_transport: Option<PairRpcTransport>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRpcTransport {
    Direct,
    Relay,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRpcRole {
    Inviter,
    Joiner,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRpcPhase {
    WaitingForPeer,
    Discovering,
    Authenticating,
    AwaitingApproval,
    Finalizing,
    Completed,
    Rejected,
    Cancelled,
    Expired,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRpcDiscoveryStage {
    Lan,
    PublicDht,
    Relay,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcCandidate {
    pub approval_id: String,
    pub peer_id: String,
    pub public_key_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_hostname: Option<String>,
    #[serde(default)]
    pub requested_vpn_ip: Option<String>,
    #[serde(default)]
    pub requested_routes: Vec<PairRpcRoute>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcFailure {
    pub code: PairRpcFailureCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRpcFailureCode {
    Unavailable,
    Rejected,
    Expired,
    Transport,
    Protocol,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcCompletionArtifacts {
    pub receipt: PairRpcReceipt,
    pub nix: PairRpcNixPlan,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcReceipt {
    pub network_name: String,
    pub local_peer: String,
    pub remote_peer: String,
    pub role: PairRpcRole,
    pub transcript_sha256: String,
    pub completed_at_unix_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcNixPlan {
    pub instance_name: String,
    pub network_name: String,
    pub local_peer: String,
    #[serde(default)]
    pub assigned_vpn_ip: Option<String>,
    #[serde(default)]
    pub additional_local_routes: Vec<PairRpcRoute>,
    pub peer: PairRpcPeer,
    #[serde(default)]
    pub member_records: Vec<PairRpcSignedMembershipRecord>,
    #[serde(default)]
    pub membership_key_file: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcPeer {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub vpn_ip: Option<String>,
    #[serde(default)]
    pub routes: Vec<PairRpcRoute>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcSignedMembershipRecord {
    pub payload: PairRpcMembershipRecordPayload,
    pub signature: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcMembershipRecordPayload {
    pub version: u8,
    pub network_name: String,
    pub member_peer: String,
    pub member_public_key: String,
    pub issuer_peer: String,
    pub issuer_public_key: String,
    pub membership_epoch: u64,
    pub sequence: u64,
    pub revoked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default)]
    pub roles: Vec<PairRpcMembershipRole>,
    #[serde(default)]
    pub route_grants: Vec<PairRpcRoute>,
    pub issued_at_unix_seconds: u64,
    #[serde(default)]
    pub expires_at_unix_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRpcMembershipRole {
    OverlayMember,
    RouteAuthority,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairRpcError {
    pub code: PairRpcErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRpcErrorCode {
    InvalidRequest,
    UnsupportedVersion,
    MessageTooLarge,
    ResponseTooLarge,
    NotFound,
    Conflict,
    InvalidState,
    Expired,
    RateLimited,
    Busy,
    Unavailable,
    Internal,
}

#[derive(Debug)]
pub enum RuntimeControlRequest {
    Status {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    State {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Peers {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Routes {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Paths {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Mtu {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    Capabilities {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    NetworkPeers {
        respond_to: oneshot::Sender<Result<NetworkPeerList, String>>,
    },
    PeerSnapshot {
        respond_to: oneshot::Sender<Result<NetworkPeerSnapshot, String>>,
    },
    Dns {
        request: DnsControlRequest,
        respond_to: oneshot::Sender<Vec<String>>,
    },
    NetworkChanged {
        respond_to: oneshot::Sender<RuntimeNetworkChange>,
    },
    Shutdown {
        respond_to: oneshot::Sender<Vec<String>>,
    },
    PairRpc {
        request: PairRpcRequest,
        respond_to: oneshot::Sender<PairRpcResponseEnvelope>,
    },
}

/// Coarse result of invalidating transport state after the host network changes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RuntimeNetworkChange {
    pub disconnected_peers: usize,
    pub invalidated_paths: usize,
    pub invalidated_packet_plane_sessions: usize,
    pub cleared_in_flight_packets: usize,
}

/// Sends control requests directly to a running p2p-vpn runtime.
///
/// This is the platform-neutral equivalent of connecting to [`ControlSocket`].
#[derive(Clone, Debug)]
pub struct RuntimeControlHandle {
    tx: mpsc::Sender<RuntimeControlRequest>,
}

/// Runtime-owned side of an in-process control channel.
#[derive(Debug)]
pub struct RuntimeControlReceiver {
    handle: RuntimeControlHandle,
    rx: mpsc::Receiver<RuntimeControlRequest>,
}

/// Creates a bounded in-process control channel.
#[must_use]
pub fn runtime_control_channel() -> (RuntimeControlHandle, RuntimeControlReceiver) {
    let (tx, rx) = mpsc::channel(REQUEST_CHANNEL);
    let handle = RuntimeControlHandle { tx };
    let receiver = RuntimeControlReceiver {
        handle: handle.clone(),
        rx,
    };
    (handle, receiver)
}

impl RuntimeControlHandle {
    pub async fn status(&self) -> io::Result<Vec<String>> {
        self.request_lines(|respond_to| RuntimeControlRequest::Status { respond_to })
            .await
    }

    pub async fn state(&self) -> io::Result<Vec<String>> {
        self.request_lines(|respond_to| RuntimeControlRequest::State { respond_to })
            .await
    }

    pub async fn peers(&self) -> io::Result<Vec<String>> {
        self.request_lines(|respond_to| RuntimeControlRequest::Peers { respond_to })
            .await
    }

    pub async fn routes(&self) -> io::Result<Vec<String>> {
        self.request_lines(|respond_to| RuntimeControlRequest::Routes { respond_to })
            .await
    }

    pub async fn paths(&self) -> io::Result<Vec<String>> {
        self.request_lines(|respond_to| RuntimeControlRequest::Paths { respond_to })
            .await
    }

    pub async fn mtu(&self) -> io::Result<Vec<String>> {
        self.request_lines(|respond_to| RuntimeControlRequest::Mtu { respond_to })
            .await
    }

    pub async fn capabilities(&self) -> io::Result<Vec<String>> {
        self.request_lines(|respond_to| RuntimeControlRequest::Capabilities { respond_to })
            .await
    }

    pub async fn network_peers(&self) -> io::Result<NetworkPeerList> {
        let (respond_to, response) = oneshot::channel();
        self.send(RuntimeControlRequest::NetworkPeers { respond_to })
            .await?;
        response
            .await
            .map_err(|_| runtime_response_dropped())?
            .map_err(io::Error::other)
    }

    pub async fn peer_snapshot(&self) -> io::Result<NetworkPeerSnapshot> {
        let (respond_to, response) = oneshot::channel();
        self.send(RuntimeControlRequest::PeerSnapshot { respond_to })
            .await?;
        response
            .await
            .map_err(|_| runtime_response_dropped())?
            .map_err(io::Error::other)
    }

    pub async fn dns(&self, request: DnsControlRequest) -> io::Result<Vec<String>> {
        self.request_lines(|respond_to| RuntimeControlRequest::Dns {
            request,
            respond_to,
        })
        .await
    }

    pub async fn network_changed(&self) -> io::Result<RuntimeNetworkChange> {
        let (respond_to, response) = oneshot::channel();
        self.send(RuntimeControlRequest::NetworkChanged { respond_to })
            .await?;
        response.await.map_err(|_| runtime_response_dropped())
    }

    pub async fn shutdown(&self) -> io::Result<Vec<String>> {
        self.request_lines(|respond_to| RuntimeControlRequest::Shutdown { respond_to })
            .await
    }

    pub async fn pair_rpc(&self, request: PairRpcRequest) -> io::Result<PairRpcResponseEnvelope> {
        let encoded_len = serde_json::to_vec(&PairRpcRequestEnvelopeRef {
            version: PAIR_RPC_VERSION,
            request: &request,
        })
        .map_err(invalid_data)?
        .len();
        if encoded_len > MAX_PAIR_RPC_REQUEST_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pair RPC request exceeds the 64 KiB limit",
            ));
        }

        let (respond_to, response) = oneshot::channel();
        self.send(RuntimeControlRequest::PairRpc {
            request,
            respond_to,
        })
        .await?;
        let response = response.await.map_err(|_| runtime_response_dropped())?;
        if response.encoded_len().map_err(invalid_data)? > MAX_PAIR_RPC_RESPONSE_LEN {
            return Ok(PairRpcResponseEnvelope::error(
                PairRpcErrorCode::ResponseTooLarge,
                "pair RPC response exceeds the 256 KiB limit",
                false,
            ));
        }
        Ok(response)
    }

    async fn request_lines(
        &self,
        request: impl FnOnce(oneshot::Sender<Vec<String>>) -> RuntimeControlRequest,
    ) -> io::Result<Vec<String>> {
        let (respond_to, response) = oneshot::channel();
        self.send(request(respond_to)).await?;
        response.await.map_err(|_| runtime_response_dropped())
    }

    async fn send(&self, request: RuntimeControlRequest) -> io::Result<()> {
        self.tx.send(request).await.map_err(|_| runtime_stopped())
    }
}

impl RuntimeControlReceiver {
    #[must_use]
    pub fn handle(&self) -> RuntimeControlHandle {
        self.handle.clone()
    }

    pub(crate) async fn recv(&mut self) -> Option<RuntimeControlRequest> {
        self.rx.recv().await
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DnsControlRequest {
    Status,
    List {
        offset: usize,
        limit: usize,
    },
    Resolve {
        input: String,
        lookup_type: DnsLookupType,
    },
}

pub struct ControlSocket {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl ControlSocket {
    pub fn bind(
        path: impl Into<PathBuf>,
    ) -> io::Result<(Self, mpsc::Receiver<RuntimeControlRequest>)> {
        let path = path.into();
        let listener = UnixListener::bind(&path)?;
        let (tx, rx) = mpsc::channel(REQUEST_CHANNEL);
        let task = tokio::spawn(serve(listener, tx));

        Ok((Self { path, task }, rx))
    }

    /// Binds a Unix control socket to an existing in-process runtime channel.
    pub fn bind_with_handle(
        path: impl Into<PathBuf>,
        handle: &RuntimeControlHandle,
    ) -> io::Result<Self> {
        let path = path.into();
        let listener = UnixListener::bind(&path)?;
        let task = tokio::spawn(serve(listener, handle.tx.clone()));

        Ok(Self { path, task })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        self.task.abort();
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!(
                "failed to remove control socket {}: {error}",
                self.path.display()
            );
        }
    }
}

async fn serve(listener: UnixListener, tx: mpsc::Sender<RuntimeControlRequest>) {
    loop {
        match listener.accept().await {
            Ok((stream, _address)) => {
                let tx = tx.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, tx).await {
                        eprintln!("control socket request failed: {error}");
                    }
                });
            }
            Err(error) => {
                eprintln!("control socket accept failed: {error}");
                break;
            }
        }
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    tx: mpsc::Sender<RuntimeControlRequest>,
) -> io::Result<()> {
    let header = read_bounded_request(&mut stream).await?;
    if header == NETWORK_PEERS_REQUEST {
        return handle_network_peers_connection(&mut stream, &tx).await;
    }
    let request = match header.as_slice() {
        STATUS_REQUEST => RequestKind::Status,
        STATE_REQUEST => RequestKind::State,
        PEERS_REQUEST => RequestKind::Peers,
        ROUTES_REQUEST => RequestKind::Routes,
        PATHS_REQUEST => RequestKind::Paths,
        MTU_REQUEST => RequestKind::Mtu,
        CAPABILITIES_REQUEST => RequestKind::Capabilities,
        SHUTDOWN_REQUEST => RequestKind::Shutdown,
        header if header.starts_with(b"dns ") => {
            let Some(request) = parse_dns_control_request(header) else {
                stream.write_all(b"error invalid dns request\n").await?;
                return Ok(());
            };
            RequestKind::Dns(request)
        }
        header if header.starts_with(b"rpc-v1") => {
            return handle_pair_rpc_connection(&mut stream, &tx, header).await;
        }
        _ => {
            stream.write_all(b"error unsupported request\n").await?;
            return Ok(());
        }
    };

    handle_legacy_connection(&mut stream, &tx, request).await
}

async fn handle_network_peers_connection(
    stream: &mut UnixStream,
    tx: &mpsc::Sender<RuntimeControlRequest>,
) -> io::Result<()> {
    let (respond_to, response) = oneshot::channel();
    tx.send(RuntimeControlRequest::NetworkPeers { respond_to })
        .await
        .map_err(|_| runtime_stopped())?;
    let response = response
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "runtime response dropped"))?;

    match response {
        Ok(peers) => {
            let body = serde_json::to_string(&peers).map_err(invalid_data)?;
            if body.len() > MAX_RESPONSE_LEN.saturating_sub(4) {
                stream
                    .write_all(b"error network peer response exceeds size limit\n")
                    .await
            } else {
                stream
                    .write_all(encode_line_response(&[body]).as_bytes())
                    .await
            }
        }
        Err(error) => {
            let error = error.split_whitespace().collect::<Vec<_>>().join(" ");
            let error = if error.is_empty() {
                "network peer inventory unavailable"
            } else {
                error.as_str()
            };
            stream
                .write_all(format!("error {error}\n").as_bytes())
                .await
        }
    }
}

async fn handle_legacy_connection(
    stream: &mut UnixStream,
    tx: &mpsc::Sender<RuntimeControlRequest>,
    request: RequestKind,
) -> io::Result<()> {
    let (respond_to, response) = oneshot::channel();
    match request {
        RequestKind::Status => {
            tx.send(RuntimeControlRequest::Status { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::State => {
            tx.send(RuntimeControlRequest::State { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Peers => {
            tx.send(RuntimeControlRequest::Peers { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Routes => {
            tx.send(RuntimeControlRequest::Routes { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Paths => {
            tx.send(RuntimeControlRequest::Paths { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Mtu => {
            tx.send(RuntimeControlRequest::Mtu { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Capabilities => {
            tx.send(RuntimeControlRequest::Capabilities { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
        RequestKind::Dns(request) => {
            tx.send(RuntimeControlRequest::Dns {
                request,
                respond_to,
            })
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
            })?;
        }
        RequestKind::Shutdown => {
            tx.send(RuntimeControlRequest::Shutdown { respond_to })
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
                })?;
        }
    }
    let lines = response
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "runtime response dropped"))?;

    stream
        .write_all(encode_line_response(&lines).as_bytes())
        .await
}

async fn handle_pair_rpc_connection(
    stream: &mut UnixStream,
    tx: &mpsc::Sender<RuntimeControlRequest>,
    header: &[u8],
) -> io::Result<()> {
    let length = match parse_pair_rpc_frame_length(header, MAX_PAIR_RPC_REQUEST_LEN) {
        Ok(length) => length,
        Err(PairRpcFrameError::Malformed) => {
            return write_pair_rpc_response(
                stream,
                &PairRpcResponseEnvelope::error(
                    PairRpcErrorCode::InvalidRequest,
                    "malformed rpc-v1 frame header",
                    false,
                ),
            )
            .await;
        }
        Err(PairRpcFrameError::TooLarge) => {
            return write_pair_rpc_response(
                stream,
                &PairRpcResponseEnvelope::error(
                    PairRpcErrorCode::MessageTooLarge,
                    "pair RPC request exceeds the 64 KiB limit",
                    false,
                ),
            )
            .await;
        }
    };

    let mut body = vec![0; length];
    if stream.read_exact(&mut body).await.is_err() {
        return write_pair_rpc_response(
            stream,
            &PairRpcResponseEnvelope::error(
                PairRpcErrorCode::InvalidRequest,
                "truncated pair RPC request body",
                false,
            ),
        )
        .await;
    }
    let envelope: PairRpcRequestEnvelope = match serde_json::from_slice(&body) {
        Ok(envelope) => envelope,
        Err(_) => {
            return write_pair_rpc_response(
                stream,
                &PairRpcResponseEnvelope::error(
                    PairRpcErrorCode::InvalidRequest,
                    "malformed pair RPC request body",
                    false,
                ),
            )
            .await;
        }
    };
    if envelope.version != PAIR_RPC_VERSION {
        return write_pair_rpc_response(
            stream,
            &PairRpcResponseEnvelope::error(
                PairRpcErrorCode::UnsupportedVersion,
                "unsupported pair RPC API version",
                false,
            ),
        )
        .await;
    }

    let (respond_to, response) = oneshot::channel();
    tx.send(RuntimeControlRequest::PairRpc {
        request: envelope.request,
        respond_to,
    })
    .await
    .map_err(|_| runtime_stopped())?;
    let response = response
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "runtime response dropped"))?;

    write_pair_rpc_response(stream, &response).await
}

async fn write_pair_rpc_response(
    stream: &mut UnixStream,
    response: &PairRpcResponseEnvelope,
) -> io::Result<()> {
    let mut body = serde_json::to_vec(response).map_err(invalid_data)?;
    if body.len() > MAX_PAIR_RPC_RESPONSE_LEN {
        body = serde_json::to_vec(&PairRpcResponseEnvelope::error(
            PairRpcErrorCode::ResponseTooLarge,
            "pair RPC response exceeds the 256 KiB limit",
            false,
        ))
        .map_err(invalid_data)?;
    }
    write_pair_rpc_frame(stream, &body).await
}

async fn write_pair_rpc_frame(stream: &mut UnixStream, body: &[u8]) -> io::Result<()> {
    let header = format!("{PAIR_RPC_FRAME_PREFIX}{}\n", body.len());
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairRpcFrameError {
    Malformed,
    TooLarge,
}

fn parse_pair_rpc_frame_length(header: &[u8], limit: usize) -> Result<usize, PairRpcFrameError> {
    let header = std::str::from_utf8(header).map_err(|_| PairRpcFrameError::Malformed)?;
    let encoded = header
        .strip_prefix(PAIR_RPC_FRAME_PREFIX)
        .and_then(|header| header.strip_suffix('\n'))
        .ok_or(PairRpcFrameError::Malformed)?;
    if encoded.is_empty() || !encoded.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(PairRpcFrameError::Malformed);
    }
    let length = encoded
        .parse::<u64>()
        .map_err(|_| PairRpcFrameError::Malformed)?;
    if length > u64::try_from(limit).expect("pair RPC size limit fits u64") {
        return Err(PairRpcFrameError::TooLarge);
    }
    usize::try_from(length).map_err(|_| PairRpcFrameError::TooLarge)
}

fn runtime_stopped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "runtime control loop stopped")
}

fn runtime_response_dropped() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "runtime response dropped")
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RequestKind {
    Status,
    State,
    Peers,
    Routes,
    Paths,
    Mtu,
    Capabilities,
    Dns(DnsControlRequest),
    Shutdown,
}

fn parse_dns_control_request(header: &[u8]) -> Option<DnsControlRequest> {
    let header = std::str::from_utf8(header).ok()?.strip_suffix('\n')?;
    let fields = header.split_ascii_whitespace().collect::<Vec<_>>();
    match fields.as_slice() {
        ["dns", "status"] => Some(DnsControlRequest::Status),
        ["dns", "list", offset, limit] => {
            let offset = offset.parse().ok()?;
            let limit = limit.parse().ok()?;
            (limit > 0 && limit <= MAX_DNS_CONTROL_LIST_LIMIT)
                .then_some(DnsControlRequest::List { offset, limit })
        }
        ["dns", "resolve", input, lookup_type] => Some(DnsControlRequest::Resolve {
            input: (*input).to_owned(),
            lookup_type: parse_dns_lookup_type(lookup_type)?,
        }),
        _ => None,
    }
}

fn parse_dns_lookup_type(input: &str) -> Option<DnsLookupType> {
    match input.to_ascii_uppercase().as_str() {
        "AUTO" => Some(DnsLookupType::Auto),
        "A" => Some(DnsLookupType::A),
        "AAAA" => Some(DnsLookupType::Aaaa),
        "PTR" => Some(DnsLookupType::Ptr),
        "ANY" => Some(DnsLookupType::Any),
        _ => None,
    }
}

async fn read_bounded_request(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut byte = [0; 1];
    while request.len() < MAX_REQUEST_LEN {
        let read = stream.read(&mut byte).await?;
        if read == 0 {
            break;
        }
        request.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(request);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "control request is missing newline or exceeds maximum length",
    ))
}

fn encode_line_response(lines: &[String]) -> String {
    let mut response = String::from("ok\n");
    for line in lines {
        response.push_str(line);
        response.push('\n');
    }
    response
}

pub async fn query_status(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, STATUS_REQUEST).await
}

pub async fn query_state(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, STATE_REQUEST).await
}

pub async fn query_peers(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, PEERS_REQUEST).await
}

pub async fn query_routes(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, ROUTES_REQUEST).await
}

pub async fn query_paths(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, PATHS_REQUEST).await
}

pub async fn query_mtu(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, MTU_REQUEST).await
}

pub async fn query_capabilities(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, CAPABILITIES_REQUEST).await
}

pub async fn query_network_peers(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<NetworkPeerList, QueryError> {
    let mut lines = query_lines(path, timeout, NETWORK_PEERS_REQUEST).await?;
    if lines.len() != 1 {
        return Err(QueryError::InvalidResponse(format!(
            "network peer response contains {} lines, expected one",
            lines.len()
        )));
    }
    let peers: NetworkPeerList = serde_json::from_str(&lines.remove(0))
        .map_err(|error| QueryError::InvalidResponse(error.to_string()))?;
    if peers.schema_version != NETWORK_PEER_LIST_SCHEMA_VERSION {
        return Err(QueryError::InvalidResponse(format!(
            "unsupported network peer schema version {}",
            peers.schema_version
        )));
    }
    Ok(peers)
}

pub async fn query_dns_status(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, b"dns status\n").await
}

pub async fn query_dns_list(
    path: &Path,
    timeout: std::time::Duration,
    offset: usize,
    limit: usize,
) -> Result<Vec<String>, QueryError> {
    if limit == 0 || limit > MAX_DNS_CONTROL_LIST_LIMIT {
        return Err(QueryError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("DNS list limit must be between 1 and {MAX_DNS_CONTROL_LIST_LIMIT}"),
        )));
    }
    let request = format!("dns list {offset} {limit}\n");
    query_lines(path, timeout, request.as_bytes()).await
}

pub async fn query_dns_resolve(
    path: &Path,
    timeout: std::time::Duration,
    input: &str,
    lookup_type: DnsLookupType,
) -> Result<Vec<String>, QueryError> {
    if input.is_empty() || input.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(QueryError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "DNS lookup input must be one name or IP address",
        )));
    }
    let request = format!("dns resolve {input} {}\n", lookup_type.as_str());
    if request.len() > MAX_REQUEST_LEN {
        return Err(QueryError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("DNS lookup request exceeds the {MAX_REQUEST_LEN}-byte limit"),
        )));
    }
    query_lines(path, timeout, request.as_bytes()).await
}

pub async fn query_shutdown(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<Vec<String>, QueryError> {
    query_lines(path, timeout, SHUTDOWN_REQUEST).await
}

pub async fn query_pair_rpc(
    path: &Path,
    timeout: std::time::Duration,
    request: &PairRpcRequestEnvelope,
) -> Result<PairRpcResponseEnvelope, PairRpcQueryError> {
    if request.version != PAIR_RPC_VERSION {
        return Err(PairRpcQueryError::InvalidRequest(
            "unsupported pair RPC API version".to_owned(),
        ));
    }
    let body = serde_json::to_vec(request)
        .map_err(|error| PairRpcQueryError::InvalidRequest(error.to_string()))?;
    if body.len() > MAX_PAIR_RPC_REQUEST_LEN {
        return Err(PairRpcQueryError::InvalidRequest(format!(
            "pair RPC request exceeds the {MAX_PAIR_RPC_REQUEST_LEN}-byte limit"
        )));
    }

    let mut stream = tokio::time::timeout(timeout, UnixStream::connect(path))
        .await
        .map_err(|_| PairRpcQueryError::TimedOut)??;
    let header = format!("{PAIR_RPC_FRAME_PREFIX}{}\n", body.len());
    tokio::time::timeout(timeout, async {
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(&body).await
    })
    .await
    .map_err(|_| PairRpcQueryError::TimedOut)??;

    let response_header = tokio::time::timeout(timeout, read_bounded_request(&mut stream))
        .await
        .map_err(|_| PairRpcQueryError::TimedOut)??;
    let response_length = parse_pair_rpc_frame_length(&response_header, MAX_PAIR_RPC_RESPONSE_LEN)
        .map_err(|error| match error {
            PairRpcFrameError::Malformed => {
                PairRpcQueryError::InvalidResponse("malformed rpc-v1 response header".to_owned())
            }
            PairRpcFrameError::TooLarge => PairRpcQueryError::InvalidResponse(format!(
                "pair RPC response exceeds the {MAX_PAIR_RPC_RESPONSE_LEN}-byte limit"
            )),
        })?;
    let mut response_body = vec![0; response_length];
    tokio::time::timeout(timeout, stream.read_exact(&mut response_body))
        .await
        .map_err(|_| PairRpcQueryError::TimedOut)??;
    let response: PairRpcResponseEnvelope = serde_json::from_slice(&response_body)
        .map_err(|error| PairRpcQueryError::InvalidResponse(error.to_string()))?;
    if response.version != PAIR_RPC_VERSION {
        return Err(PairRpcQueryError::InvalidResponse(
            "unsupported pair RPC response version".to_owned(),
        ));
    }

    Ok(response)
}

async fn query_lines(
    path: &Path,
    timeout: std::time::Duration,
    request: &[u8],
) -> Result<Vec<String>, QueryError> {
    let mut stream = tokio::time::timeout(timeout, UnixStream::connect(path))
        .await
        .map_err(|_| QueryError::TimedOut)??;
    tokio::time::timeout(timeout, stream.write_all(request))
        .await
        .map_err(|_| QueryError::TimedOut)??;

    let mut response = String::new();
    tokio::time::timeout(
        timeout,
        stream
            .take(u64::try_from(MAX_RESPONSE_LEN).expect("response limit fits u64"))
            .read_to_string(&mut response),
    )
    .await
    .map_err(|_| QueryError::TimedOut)??;

    decode_line_response(&response)
}

fn decode_line_response(response: &str) -> Result<Vec<String>, QueryError> {
    let mut lines = response.lines();
    match lines.next() {
        Some("ok") => Ok(lines.map(ToOwned::to_owned).collect()),
        Some(error) if error.starts_with("error ") => Err(QueryError::Remote(error.to_owned())),
        Some(other) => Err(QueryError::InvalidResponse(other.to_owned())),
        None => Err(QueryError::InvalidResponse("empty response".to_owned())),
    }
}

#[derive(Debug)]
pub enum QueryError {
    Io(io::Error),
    TimedOut,
    Remote(String),
    InvalidResponse(String),
}

impl From<io::Error> for QueryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
pub enum PairRpcQueryError {
    Io(io::Error),
    TimedOut,
    InvalidRequest(String),
    InvalidResponse(String),
}

impl From<io::Error> for PairRpcQueryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    fn test_socket_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "p2p-vpn-control-{}-{label}.sock",
            std::process::id()
        ))
    }

    fn pair_rpc_requests() -> Vec<PairRpcRequest> {
        vec![
            PairRpcRequest::PairOpen {
                operation_id: "open-operation".to_owned(),
                expires_in_seconds: 600,
            },
            PairRpcRequest::PairJoin {
                operation_id: "join-operation".to_owned(),
                code: "ABCD-EFGH-JKLM-NPQR".to_owned(),
                timeout_seconds: 600,
                requested_vpn_ip: Some("10.42.0.2".to_owned()),
                requested_routes: Some(vec![PairRpcRoute {
                    prefix: "10.42.0.2/32".to_owned(),
                    metric: 10,
                }]),
            },
            PairRpcRequest::PairStatus {
                operation_id: "status-operation".to_owned(),
            },
            PairRpcRequest::PairApprove {
                operation_id: "approve-operation".to_owned(),
                approval_id: "approval-transcript".to_owned(),
                assigned_hostname: Some("worker-2".to_owned()),
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                granted_routes: vec![PairRpcRoute {
                    prefix: "10.42.0.2/32".to_owned(),
                    metric: 0,
                }],
            },
            PairRpcRequest::PairReject {
                operation_id: "reject-operation".to_owned(),
                approval_id: "approval-transcript".to_owned(),
                reason: PairRpcRejectionReason::Declined,
            },
            PairRpcRequest::PairCancel {
                operation_id: "cancel-operation".to_owned(),
            },
            PairRpcRequest::PairArtifacts {
                operation_id: "artifacts-operation".to_owned(),
            },
            PairRpcRequest::PairAcknowledge {
                operation_id: "acknowledge-operation".to_owned(),
                transcript_sha256: "transcript-digest".to_owned(),
            },
        ]
    }

    fn operation_status(operation_id: &str) -> PairRpcOperationStatus {
        PairRpcOperationStatus {
            operation_id: operation_id.to_owned(),
            network_name: "runners".to_owned(),
            local_peer: "12D3KooWLocal".to_owned(),
            role: PairRpcRole::Joiner,
            phase: PairRpcPhase::AwaitingApproval,
            revision: 3,
            discovery: Some(PairRpcDiscoveryStage::PublicDht),
            diagnostics: PairRpcDiagnostics::default(),
            expires_at_unix_seconds: 1_700_000_600,
            candidate: Some(PairRpcCandidate {
                approval_id: "approval-transcript".to_owned(),
                peer_id: "12D3KooWRemote".to_owned(),
                public_key_fingerprint: "sha256:public-key".to_owned(),
                requested_hostname: Some("worker-2".to_owned()),
                requested_vpn_ip: Some("10.42.0.2".to_owned()),
                requested_routes: Vec::new(),
            }),
            artifacts_ready: false,
            failure: None,
        }
    }

    fn completion_artifacts() -> PairRpcCompletionArtifacts {
        PairRpcCompletionArtifacts {
            receipt: PairRpcReceipt {
                network_name: "runners".to_owned(),
                local_peer: "12D3KooWLocal".to_owned(),
                remote_peer: "12D3KooWRemote".to_owned(),
                role: PairRpcRole::Joiner,
                transcript_sha256: "transcript-digest".to_owned(),
                completed_at_unix_seconds: 1_700_000_100,
            },
            nix: PairRpcNixPlan {
                instance_name: "runners".to_owned(),
                network_name: "runners".to_owned(),
                local_peer: "12D3KooWLocal".to_owned(),
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                additional_local_routes: Vec::new(),
                peer: PairRpcPeer {
                    id: "12D3KooWRemote".to_owned(),
                    name: Some("runner-a".to_owned()),
                    vpn_ip: Some("10.42.0.1".to_owned()),
                    routes: Vec::new(),
                },
                member_records: vec![PairRpcSignedMembershipRecord {
                    payload: PairRpcMembershipRecordPayload {
                        version: 1,
                        network_name: "runners".to_owned(),
                        member_peer: "12D3KooWLocal".to_owned(),
                        member_public_key: "member-public-key".to_owned(),
                        issuer_peer: "12D3KooWRemote".to_owned(),
                        issuer_public_key: "issuer-public-key".to_owned(),
                        membership_epoch: 1,
                        sequence: 7,
                        revoked: false,
                        hostname: None,
                        roles: vec![PairRpcMembershipRole::OverlayMember],
                        route_grants: Vec::new(),
                        issued_at_unix_seconds: 1_700_000_100,
                        expires_at_unix_seconds: None,
                    },
                    signature: "signed-record".to_owned(),
                }],
                membership_key_file: Some("/var/lib/p2p-vpn/runners/membership.key".to_owned()),
            },
        }
    }

    async fn raw_exchange(path: &Path, request: &[u8]) -> Vec<u8> {
        let mut stream = UnixStream::connect(path).await.expect("connect");
        stream.write_all(request).await.expect("write request");
        stream.shutdown().await.expect("shutdown request side");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read response");
        response
    }

    fn framed_request(request: &PairRpcRequestEnvelope) -> Vec<u8> {
        let body = serde_json::to_vec(request).expect("serialize request");
        let mut framed = format!("{PAIR_RPC_FRAME_PREFIX}{}\n", body.len()).into_bytes();
        framed.extend_from_slice(&body);
        framed
    }

    fn decode_framed_response(response: &[u8]) -> PairRpcResponseEnvelope {
        let newline = response
            .iter()
            .position(|byte| *byte == b'\n')
            .expect("response header newline");
        let (header, body) = response.split_at(newline + 1);
        let length = parse_pair_rpc_frame_length(header, MAX_PAIR_RPC_RESPONSE_LEN)
            .expect("response frame length");
        assert_eq!(body.len(), length);
        serde_json::from_slice(body).expect("response body")
    }

    fn error_code(response: &PairRpcResponseEnvelope) -> PairRpcErrorCode {
        let PairRpcOutcome::Error { error } = &response.outcome else {
            panic!("expected RPC error response, got {response:?}");
        };
        error.code
    }

    #[test]
    fn line_response_round_trips_lines() {
        let response = encode_line_response(&[
            "network lab".to_owned(),
            "outbound_path_probes_sent 1".to_owned(),
        ]);

        assert_eq!(
            decode_line_response(&response).expect("line response"),
            vec![
                "network lab".to_owned(),
                "outbound_path_probes_sent 1".to_owned()
            ]
        );
    }

    #[test]
    fn line_response_rejects_remote_errors() {
        assert!(matches!(
            decode_line_response("error unsupported request\n"),
            Err(QueryError::Remote(_))
        ));
    }

    #[test]
    fn pair_rpc_request_envelopes_round_trip_every_method() {
        for request in pair_rpc_requests() {
            let envelope = PairRpcRequestEnvelope::new(request);
            let encoded = serde_json::to_vec(&envelope).expect("encode request envelope");
            let decoded: PairRpcRequestEnvelope =
                serde_json::from_slice(&encoded).expect("decode request envelope");
            assert_eq!(decoded, envelope);
        }
    }

    #[test]
    fn pair_rpc_response_envelopes_round_trip_neutral_payloads() {
        let responses = [
            PairRpcResponseEnvelope::ok(PairRpcResult::OpenStarted(PairRpcOpenStarted {
                operation_id: "open-operation".to_owned(),
                code: "ABCD-EFGH-JKLM-NPQR".to_owned(),
                network_name: "runners".to_owned(),
                local_peer: "12D3KooWLocal".to_owned(),
                expires_at_unix_seconds: 1_700_000_600,
            })),
            PairRpcResponseEnvelope::ok(PairRpcResult::JoinStarted(PairRpcJoinStarted {
                operation_id: "join-operation".to_owned(),
                network_name: "runners".to_owned(),
                local_peer: "12D3KooWLocal".to_owned(),
                expires_at_unix_seconds: 1_700_000_600,
            })),
            PairRpcResponseEnvelope::ok(PairRpcResult::OperationStatus(Box::new(
                operation_status("status-operation"),
            ))),
            PairRpcResponseEnvelope::ok(PairRpcResult::ActionAccepted(Box::new(operation_status(
                "approve-operation",
            )))),
            PairRpcResponseEnvelope::ok(PairRpcResult::Artifacts(Box::new(completion_artifacts()))),
            PairRpcResponseEnvelope::ok(PairRpcResult::Acknowledged(
                completion_artifacts().receipt,
            )),
            PairRpcResponseEnvelope::error(
                PairRpcErrorCode::InvalidState,
                "operation is not awaiting approval",
                false,
            ),
        ];

        for response in responses {
            let encoded = serde_json::to_vec(&response).expect("encode response envelope");
            let decoded: PairRpcResponseEnvelope =
                serde_json::from_slice(&encoded).expect("decode response envelope");
            assert_eq!(decoded, response);
        }
    }

    #[test]
    fn pair_rpc_status_accepts_legacy_and_partial_diagnostics() {
        let mut legacy =
            serde_json::to_value(operation_status("legacy-status")).expect("encode legacy status");
        legacy
            .as_object_mut()
            .expect("status object")
            .remove("diagnostics");
        let decoded: PairRpcOperationStatus =
            serde_json::from_value(legacy).expect("decode status without diagnostics");
        assert_eq!(decoded.diagnostics, PairRpcDiagnostics::default());

        let mut partial = serde_json::to_value(operation_status("partial-status"))
            .expect("encode partial status");
        partial["diagnostics"] = serde_json::json!({ "selected_transport": "relay" });
        let decoded: PairRpcOperationStatus =
            serde_json::from_value(partial).expect("decode partial diagnostics");
        assert_eq!(
            decoded.diagnostics,
            PairRpcDiagnostics {
                selected_transport: Some(PairRpcTransport::Relay),
                ..PairRpcDiagnostics::default()
            }
        );
    }

    #[test]
    fn pair_rpc_approve_accepts_legacy_request_without_hostname() {
        let legacy = serde_json::json!({
            "version": PAIR_RPC_VERSION,
            "request": {
                "method": "pair_approve",
                "params": {
                    "operation_id": "operation",
                    "approval_id": "approval",
                    "assigned_vpn_ip": "10.42.0.2",
                    "granted_routes": []
                }
            }
        });

        let decoded: PairRpcRequestEnvelope =
            serde_json::from_value(legacy).expect("legacy approve request");
        assert!(matches!(
            decoded.request,
            PairRpcRequest::PairApprove {
                assigned_hostname: None,
                ..
            }
        ));
    }

    #[test]
    fn pair_rpc_debug_redacts_codes_but_wire_retains_them() {
        let code = "ABCD-EFGH-JKLM-NPQR";
        let request = PairRpcRequestEnvelope::new(PairRpcRequest::PairJoin {
            operation_id: "join-operation".to_owned(),
            code: code.to_owned(),
            timeout_seconds: 600,
            requested_vpn_ip: None,
            requested_routes: None,
        });
        let response =
            PairRpcResponseEnvelope::ok(PairRpcResult::OpenStarted(PairRpcOpenStarted {
                operation_id: "open-operation".to_owned(),
                code: code.to_owned(),
                network_name: "runners".to_owned(),
                local_peer: "12D3KooWLocal".to_owned(),
                expires_at_unix_seconds: 1_700_000_600,
            }));

        assert!(!format!("{request:?}").contains(code));
        assert!(!format!("{response:?}").contains(code));
        assert!(
            serde_json::to_string(&request)
                .expect("request JSON")
                .contains(code)
        );
        assert!(
            serde_json::to_string(&response)
                .expect("response JSON")
                .contains(code)
        );
    }

    #[tokio::test]
    async fn in_process_control_channel_round_trips_runtime_requests() {
        let (handle, mut receiver) = runtime_control_channel();
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::Status { respond_to }) = receiver.recv().await else {
                panic!("expected status request");
            };
            respond_to
                .send(vec!["network lab".to_owned()])
                .expect("status response accepted");

            let Some(RuntimeControlRequest::PairRpc {
                request,
                respond_to,
            }) = receiver.recv().await
            else {
                panic!("expected pair RPC request");
            };
            assert_eq!(
                request,
                PairRpcRequest::PairStatus {
                    operation_id: "pair-operation".to_owned(),
                }
            );
            respond_to
                .send(PairRpcResponseEnvelope::error(
                    PairRpcErrorCode::NotFound,
                    "operation not found",
                    false,
                ))
                .expect("pair response accepted");

            let Some(RuntimeControlRequest::NetworkChanged { respond_to }) = receiver.recv().await
            else {
                panic!("expected network change request");
            };
            respond_to
                .send(RuntimeNetworkChange {
                    disconnected_peers: 2,
                    invalidated_paths: 1,
                    invalidated_packet_plane_sessions: 1,
                    cleared_in_flight_packets: 3,
                })
                .expect("network change response accepted");

            let Some(RuntimeControlRequest::Shutdown { respond_to }) = receiver.recv().await else {
                panic!("expected shutdown request");
            };
            respond_to
                .send(vec!["shutdown accepted".to_owned()])
                .expect("shutdown response accepted");
        });

        assert_eq!(
            handle.status().await.expect("status"),
            vec!["network lab".to_owned()]
        );
        let response = handle
            .pair_rpc(PairRpcRequest::PairStatus {
                operation_id: "pair-operation".to_owned(),
            })
            .await
            .expect("pair response");
        assert!(matches!(
            response.outcome,
            PairRpcOutcome::Error {
                error: PairRpcError {
                    code: PairRpcErrorCode::NotFound,
                    ..
                }
            }
        ));
        assert_eq!(
            handle.network_changed().await.expect("network changed"),
            RuntimeNetworkChange {
                disconnected_peers: 2,
                invalidated_paths: 1,
                invalidated_packet_plane_sessions: 1,
                cleared_in_flight_packets: 3,
            }
        );
        assert_eq!(
            handle.shutdown().await.expect("shutdown"),
            vec!["shutdown accepted".to_owned()]
        );
        responder.await.expect("responder");
    }

    #[tokio::test]
    async fn control_socket_can_share_an_in_process_channel() {
        let path = test_socket_path("shared-channel");
        let _ = std::fs::remove_file(&path);
        let (handle, mut receiver) = runtime_control_channel();
        let socket = ControlSocket::bind_with_handle(&path, &handle).expect("control socket");
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::Status { respond_to }) = receiver.recv().await else {
                panic!("expected status request");
            };
            respond_to
                .send(vec!["network shared".to_owned()])
                .expect("status response accepted");
        });

        assert_eq!(
            query_status(&path, std::time::Duration::from_secs(1))
                .await
                .expect("query"),
            vec!["network shared".to_owned()]
        );
        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn in_process_control_channel_rejects_oversized_pair_requests() {
        let (handle, _receiver) = runtime_control_channel();
        let error = handle
            .pair_rpc(PairRpcRequest::PairJoin {
                operation_id: "join-operation".to_owned(),
                code: "X".repeat(MAX_PAIR_RPC_REQUEST_LEN),
                timeout_seconds: 600,
                requested_vpn_ip: None,
                requested_routes: None,
            })
            .await
            .expect_err("oversized request rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn control_socket_serves_status_lines_and_cleans_up() {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-control-{}-{}.sock",
            std::process::id(),
            "status"
        ));
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::Status { respond_to }) = rx.recv().await else {
                panic!("expected status request");
            };
            respond_to
                .send(vec!["network lab".to_owned(), "peers 2".to_owned()])
                .expect("status response accepted");
        });

        let lines = query_status(&path, std::time::Duration::from_secs(1))
            .await
            .expect("query");

        assert_eq!(lines, vec!["network lab".to_owned(), "peers 2".to_owned()]);
        responder.await.expect("responder");
        drop(socket);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn control_socket_serves_state_lines() {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-control-{}-{}.sock",
            std::process::id(),
            "state"
        ));
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::State { respond_to }) = rx.recv().await else {
                panic!("expected state request");
            };
            respond_to
                .send(vec![
                    "daemon state: running".to_owned(),
                    "configured peers: 1".to_owned(),
                ])
                .expect("state response accepted");
        });

        let lines = query_state(&path, std::time::Duration::from_secs(1))
            .await
            .expect("query");

        assert_eq!(
            lines,
            vec![
                "daemon state: running".to_owned(),
                "configured peers: 1".to_owned()
            ]
        );
        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn control_socket_serves_shutdown_request() {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-control-{}-{}.sock",
            std::process::id(),
            "shutdown"
        ));
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::Shutdown { respond_to }) = rx.recv().await else {
                panic!("expected shutdown request");
            };
            respond_to
                .send(vec!["shutdown accepted".to_owned()])
                .expect("shutdown response accepted");
        });

        let lines = query_shutdown(&path, std::time::Duration::from_secs(1))
            .await
            .expect("query");

        assert_eq!(lines, vec!["shutdown accepted".to_owned()]);
        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn control_socket_serves_daemon_view_requests() {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-control-{}-{}.sock",
            std::process::id(),
            "views"
        ));
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            for expected in ["peers", "routes", "paths", "mtu", "capabilities"] {
                match (expected, rx.recv().await) {
                    ("peers", Some(RuntimeControlRequest::Peers { respond_to })) => {
                        respond_to
                            .send(vec!["peers: 1".to_owned()])
                            .expect("peers response accepted");
                    }
                    ("routes", Some(RuntimeControlRequest::Routes { respond_to })) => {
                        respond_to
                            .send(vec!["remote advertised routes: 1".to_owned()])
                            .expect("routes response accepted");
                    }
                    ("paths", Some(RuntimeControlRequest::Paths { respond_to })) => {
                        respond_to
                            .send(vec!["peer selected path: abc direct_tcp_stream".to_owned()])
                            .expect("paths response accepted");
                    }
                    ("mtu", Some(RuntimeControlRequest::Mtu { respond_to })) => {
                        respond_to
                            .send(vec!["peer mtu: abc selected_path_mtu 1200".to_owned()])
                            .expect("mtu response accepted");
                    }
                    ("capabilities", Some(RuntimeControlRequest::Capabilities { respond_to })) => {
                        respond_to
                            .send(vec!["validated peers: 1".to_owned()])
                            .expect("capabilities response accepted");
                    }
                    (kind, other) => panic!("expected {kind} request, got {other:?}"),
                }
            }
        });

        assert_eq!(
            query_peers(&path, std::time::Duration::from_secs(1))
                .await
                .expect("peers"),
            vec!["peers: 1".to_owned()]
        );
        assert_eq!(
            query_routes(&path, std::time::Duration::from_secs(1))
                .await
                .expect("routes"),
            vec!["remote advertised routes: 1".to_owned()]
        );
        assert_eq!(
            query_paths(&path, std::time::Duration::from_secs(1))
                .await
                .expect("paths"),
            vec!["peer selected path: abc direct_tcp_stream".to_owned()]
        );
        assert_eq!(
            query_mtu(&path, std::time::Duration::from_secs(1))
                .await
                .expect("mtu"),
            vec!["peer mtu: abc selected_path_mtu 1200".to_owned()]
        );
        assert_eq!(
            query_capabilities(&path, std::time::Duration::from_secs(1))
                .await
                .expect("capabilities"),
            vec!["validated peers: 1".to_owned()]
        );
        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn control_socket_serves_structured_network_peer_inventory() {
        let path = test_socket_path("network-peers");
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let expected = NetworkPeerList {
            schema_version: NETWORK_PEER_LIST_SCHEMA_VERSION,
            network: "runners".to_owned(),
            peers: vec![crate::network_peer::NetworkPeer {
                peer_id: "12D3KooWPeer".to_owned(),
                hostnames: vec!["worker-1".to_owned()],
                ipv4: vec!["100.64.0.1".parse().expect("IPv4")],
                ipv6: vec!["fd00::1".parse().expect("IPv6")],
                local: true,
            }],
        };
        let response = expected.clone();
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::NetworkPeers { respond_to }) = rx.recv().await else {
                panic!("expected network peers request");
            };
            respond_to
                .send(Ok(response))
                .expect("network peers response accepted");
        });

        assert_eq!(
            query_network_peers(&path, std::time::Duration::from_secs(1))
                .await
                .expect("network peers"),
            expected
        );
        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn control_socket_surfaces_network_peer_inventory_errors() {
        let path = test_socket_path("network-peers-error");
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::NetworkPeers { respond_to }) = rx.recv().await else {
                panic!("expected network peers request");
            };
            respond_to
                .send(Err("inventory unavailable".to_owned()))
                .expect("network peers error accepted");
        });

        assert!(matches!(
            query_network_peers(&path, std::time::Duration::from_secs(1)).await,
            Err(QueryError::Remote(error)) if error == "error inventory unavailable"
        ));
        responder.await.expect("responder");
        drop(socket);
    }

    #[test]
    fn dns_control_request_parser_accepts_bounded_operations() {
        assert_eq!(
            parse_dns_control_request(b"dns status\n"),
            Some(DnsControlRequest::Status)
        );
        assert_eq!(
            parse_dns_control_request(b"dns list 32 64\n"),
            Some(DnsControlRequest::List {
                offset: 32,
                limit: 64,
            })
        );
        assert_eq!(
            parse_dns_control_request(b"dns resolve worker-1 AAAA\n"),
            Some(DnsControlRequest::Resolve {
                input: "worker-1".to_owned(),
                lookup_type: DnsLookupType::Aaaa,
            })
        );
        assert!(parse_dns_control_request(b"dns list 0 0\n").is_none());
        assert!(parse_dns_control_request(b"dns list 0 257\n").is_none());
        assert!(parse_dns_control_request(b"dns resolve host MX\n").is_none());
    }

    #[tokio::test]
    async fn control_socket_round_trips_dns_requests() {
        let path = test_socket_path("dns");
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            let expected = [
                DnsControlRequest::Status,
                DnsControlRequest::List {
                    offset: 2,
                    limit: 4,
                },
                DnsControlRequest::Resolve {
                    input: "worker-1".to_owned(),
                    lookup_type: DnsLookupType::Auto,
                },
            ];
            for request in expected {
                let Some(RuntimeControlRequest::Dns {
                    request: actual,
                    respond_to,
                }) = rx.recv().await
                else {
                    panic!("expected DNS request");
                };
                assert_eq!(actual, request);
                respond_to
                    .send(vec!["dns test=true".to_owned()])
                    .expect("DNS response accepted");
            }
        });

        assert_eq!(
            query_dns_status(&path, std::time::Duration::from_secs(1))
                .await
                .expect("DNS status"),
            vec!["dns test=true".to_owned()]
        );
        assert_eq!(
            query_dns_list(&path, std::time::Duration::from_secs(1), 2, 4)
                .await
                .expect("DNS list"),
            vec!["dns test=true".to_owned()]
        );
        assert_eq!(
            query_dns_resolve(
                &path,
                std::time::Duration::from_secs(1),
                "worker-1",
                DnsLookupType::Auto,
            )
            .await
            .expect("DNS resolve"),
            vec!["dns test=true".to_owned()]
        );
        assert!(matches!(
            query_dns_list(&path, std::time::Duration::from_secs(1), 0, 0).await,
            Err(QueryError::Io(error)) if error.kind() == io::ErrorKind::InvalidInput
        ));
        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn legacy_commands_remain_byte_for_byte_compatible() {
        let path = test_socket_path("legacy-byte-compatibility");
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            for index in 0..8 {
                let request = rx.recv().await.expect("legacy request");
                let respond_to = match (index, request) {
                    (0, RuntimeControlRequest::Status { respond_to })
                    | (1, RuntimeControlRequest::State { respond_to })
                    | (2, RuntimeControlRequest::Peers { respond_to })
                    | (3, RuntimeControlRequest::Routes { respond_to })
                    | (4, RuntimeControlRequest::Paths { respond_to })
                    | (5, RuntimeControlRequest::Mtu { respond_to })
                    | (6, RuntimeControlRequest::Capabilities { respond_to })
                    | (7, RuntimeControlRequest::Shutdown { respond_to }) => respond_to,
                    (_, other) => panic!("unexpected legacy request: {other:?}"),
                };
                respond_to
                    .send(vec![format!("legacy-{index}")])
                    .expect("legacy response accepted");
            }
        });
        let requests: [&[u8]; 8] = [
            STATUS_REQUEST,
            STATE_REQUEST,
            PEERS_REQUEST,
            ROUTES_REQUEST,
            PATHS_REQUEST,
            MTU_REQUEST,
            CAPABILITIES_REQUEST,
            SHUTDOWN_REQUEST,
        ];

        for (index, request) in requests.into_iter().enumerate() {
            assert_eq!(
                raw_exchange(&path, request).await,
                format!("ok\nlegacy-{index}\n").as_bytes()
            );
        }
        assert_eq!(
            raw_exchange(&path, b"unknown\n").await,
            b"error unsupported request\n"
        );

        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn pair_rpc_socket_round_trips_every_method() {
        let path = test_socket_path("rpc-method-roundtrip");
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let requests = pair_rpc_requests();
        let expected = requests.clone();
        let response = PairRpcResponseEnvelope::ok(PairRpcResult::OperationStatus(Box::new(
            operation_status("roundtrip-operation"),
        )));
        let expected_response = response.clone();
        let responder = tokio::spawn(async move {
            for expected_request in expected {
                let Some(RuntimeControlRequest::PairRpc {
                    request,
                    respond_to,
                }) = rx.recv().await
                else {
                    panic!("expected pair RPC request");
                };
                assert_eq!(request, expected_request);
                respond_to
                    .send(response.clone())
                    .expect("pair RPC response accepted");
            }
        });

        for request in requests {
            let received = query_pair_rpc(
                &path,
                std::time::Duration::from_secs(1),
                &PairRpcRequestEnvelope::new(request),
            )
            .await
            .expect("pair RPC query");
            assert_eq!(received, expected_response);
        }

        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn pair_rpc_rejects_malformed_and_oversized_requests() {
        let path = test_socket_path("rpc-invalid-requests");
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");

        let malformed_header =
            decode_framed_response(&raw_exchange(&path, b"rpc-v1 not-a-length\n").await);
        assert_eq!(
            error_code(&malformed_header),
            PairRpcErrorCode::InvalidRequest
        );

        let malformed_body =
            decode_framed_response(&raw_exchange(&path, b"rpc-v1 8\nnot-json").await);
        assert_eq!(
            error_code(&malformed_body),
            PairRpcErrorCode::InvalidRequest
        );

        let truncated_body = decode_framed_response(&raw_exchange(&path, b"rpc-v1 20\n{}").await);
        assert_eq!(
            error_code(&truncated_body),
            PairRpcErrorCode::InvalidRequest
        );

        let oversized_header = format!("rpc-v1 {}\n", MAX_PAIR_RPC_REQUEST_LEN + 1);
        let oversized =
            decode_framed_response(&raw_exchange(&path, oversized_header.as_bytes()).await);
        assert_eq!(error_code(&oversized), PairRpcErrorCode::MessageTooLarge);
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        drop(socket);
    }

    #[tokio::test]
    async fn pair_rpc_rejects_unsupported_body_version() {
        let path = test_socket_path("rpc-version");
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let mut request = PairRpcRequestEnvelope::new(PairRpcRequest::PairStatus {
            operation_id: "status-operation".to_owned(),
        });
        request.version = PAIR_RPC_VERSION + 1;

        let response =
            decode_framed_response(&raw_exchange(&path, &framed_request(&request)).await);
        assert_eq!(error_code(&response), PairRpcErrorCode::UnsupportedVersion);
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        drop(socket);
    }

    #[tokio::test]
    async fn pair_rpc_bounds_client_requests_before_connecting() {
        let request = PairRpcRequestEnvelope::new(PairRpcRequest::PairJoin {
            operation_id: "join-operation".to_owned(),
            code: "X".repeat(MAX_PAIR_RPC_REQUEST_LEN),
            timeout_seconds: 600,
            requested_vpn_ip: None,
            requested_routes: None,
        });

        assert!(matches!(
            query_pair_rpc(
                Path::new("/no/such/p2p-vpn-control.sock"),
                std::time::Duration::from_secs(1),
                &request,
            )
            .await,
            Err(PairRpcQueryError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn pair_rpc_replaces_oversized_runtime_responses_with_bounded_error() {
        let path = test_socket_path("rpc-oversized-response");
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) = ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            let Some(RuntimeControlRequest::PairRpc { respond_to, .. }) = rx.recv().await else {
                panic!("expected pair RPC request");
            };
            respond_to
                .send(PairRpcResponseEnvelope::error(
                    PairRpcErrorCode::Internal,
                    "X".repeat(MAX_PAIR_RPC_RESPONSE_LEN),
                    false,
                ))
                .expect("oversized response accepted");
        });

        let response = query_pair_rpc(
            &path,
            std::time::Duration::from_secs(1),
            &PairRpcRequestEnvelope::new(PairRpcRequest::PairStatus {
                operation_id: "status-operation".to_owned(),
            }),
        )
        .await
        .expect("bounded pair RPC response");
        assert_eq!(error_code(&response), PairRpcErrorCode::ResponseTooLarge);

        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn pair_rpc_client_rejects_oversized_declared_response_before_reading_body() {
        let path = test_socket_path("rpc-oversized-declared-response");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("test listener");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept query");
            let header = read_bounded_request(&mut stream)
                .await
                .expect("request header");
            let length = parse_pair_rpc_frame_length(&header, MAX_PAIR_RPC_REQUEST_LEN)
                .expect("request length");
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await.expect("request body");
            stream
                .write_all(
                    format!("{PAIR_RPC_FRAME_PREFIX}{}\n", MAX_PAIR_RPC_RESPONSE_LEN + 1)
                        .as_bytes(),
                )
                .await
                .expect("oversized response header");
        });

        let error = query_pair_rpc(
            &path,
            std::time::Duration::from_secs(1),
            &PairRpcRequestEnvelope::new(PairRpcRequest::PairStatus {
                operation_id: "status-operation".to_owned(),
            }),
        )
        .await
        .expect_err("oversized declared response must fail");
        assert!(matches!(error, PairRpcQueryError::InvalidResponse(_)));

        server.await.expect("server");
        std::fs::remove_file(path).expect("remove test socket");
    }
}
