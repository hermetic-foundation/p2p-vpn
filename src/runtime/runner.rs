use std::{
    collections::{HashMap, HashSet},
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs, UdpSocket as StdUdpSocket},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt as _;
use libp2p::{
    Multiaddr, PeerId as Libp2pPeerId, Swarm, autonat,
    core::ConnectedPoint,
    dcutr, identify, kad, mdns,
    multiaddr::Protocol,
    relay,
    request_response::{self, Message},
    swarm::SwarmEvent,
};
use rand_core::{OsRng, RngCore as _};
use rustls::pki_types::CertificateDer;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{
    OVERLAY_FRAGMENTATION_POLICY_LINE, PathKind, PeerId, SessionId,
    config::{AutoRelayConfig, Config, ConfigError, DiscoveryConfig, QueueConfig, ResourceConfig},
    identity::NodeIdentity,
    membership::{SignedMembershipRecord, effective_membership_at},
    metrics::{
        AutoNatReachability, PacketDropReason, PacketPlaneDropReason, RuntimeMetrics,
        RuntimeSnapshot,
    },
    path::{PathSet, PathTransportSupport},
    queue::{EnqueueError, FlowShard, PeerQueues},
    route::RouteError,
    runtime::{
        control::{
            ControlCapabilities, ControlRejectionReason, ControlRequest, ControlResponse,
            MAX_CONTROL_MEMBERSHIP_RECORDS, PeerCapabilities, accepted_capabilities_response,
            rejected_capabilities_response, validate_capabilities,
        },
        control_socket::{ControlSocket, RuntimeControlRequest},
        forward::{ForwardError, Forwarder, packet_destination, packet_source},
        p2p::{Behaviour, BehaviourEvent, HostConfig, P2pBuildError, P2pNode, build_node},
        packet::{PacketRejectionReason, PacketResponse},
        packet_plane::{
            PacketPlaneEphemeralSecret, PacketPlaneHandshake, PacketPlaneHandshakeError,
            PacketPlaneHandshakeKind, PacketPlaneHandshakeParams, PacketPlaneIoError,
            PacketPlaneQuicError, PacketPlaneQuicRuntime, PacketPlaneQuicSnapshot,
            PacketPlaneReceivedFrame, PacketPlaneRuntime, PacketPlaneSessionError,
            PacketPlaneSessionRole, PacketPlaneSessionSnapshot, PacketPlaneSnapshot,
            VerifiedPacketPlaneHandshake,
        },
        service::{
            ServiceRejectionReason, ServiceRequest, ServiceResponse, ServiceStatusRequest,
            ServiceStatusResponse, validate_status_request, validate_status_response,
        },
        tun::{TunDevice, TunReader, TunRuntimeError, TunWriter, packet_too_big},
    },
    wire::{Frame, PayloadType},
};

const TUN_READ_CHANNEL: usize = 1024;
const REDIAL_INTERVAL: Duration = Duration::from_secs(10);
const BLOCKED_QUEUE_REDIAL_INTERVAL: Duration = Duration::from_secs(2);
const KADEMLIA_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const PATH_PROBE_INTERVAL: Duration = Duration::from_secs(30);
const DISCOVERED_ADDRESS_TTL: Duration = Duration::from_mins(60);
const MIN_QUEUE_EXPIRY_INTERVAL: Duration = Duration::from_millis(10);
const SERVICE_STATUS_NONCE: u64 = 1;
const PATH_PROBE_PAYLOAD: &[u8] = b"path-probe-v1";
const PATH_PROBE_ACK_PAYLOAD: &[u8] = b"path-probe-ack-v1";
const PATH_PROBE_MTU_STEP: u16 = 64;
const PATH_PROBE_TOKEN_LEN: usize = 8;
const PATH_PROBE_TIMEOUT: Duration = Duration::from_secs(45);
const PATH_PROBE_RTT_TTL: Duration = Duration::from_mins(2);
const MAX_PENDING_PATH_PROBES: usize = 4096;
const PACKET_PLANE_QUIC_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PACKET_PLANE_QUIC_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);
const AUTO_RELAY_RESERVATION_PENDING_TIMEOUT: Duration = Duration::from_secs(30);
const AUTO_RELAY_CANDIDATE_FAILURE_EVICTION_THRESHOLD: u8 = 2;
const MAX_KADEMLIA_MEMBERSHIP_RECORD_BYTES: usize = 64 * 1024;
const MAX_KADEMLIA_PEER_ADDRESS_RECORD_BYTES: usize = 64 * 1024;
const MAX_KADEMLIA_PEER_ADDRESS_RECORD_ADDRESSES: usize = 32;
const KADEMLIA_PEER_ADDRESS_RECORD_TTL: u64 = 30 * 60;
const KADEMLIA_PEER_ADDRESS_RECORD_STALE_GRACE: u64 = 60 * 60;
const AUTO_RELAY_MAX_INFRASTRUCTURE_PEERS: usize = 64;
const AUTO_RELAY_DISCOVERY_QUERY_FANOUT: usize = 4;

const LOCAL_PACKET_DATA_PLANE: LocalPacketDataPlane =
    LocalPacketDataPlane::identity_keyed_streams();
const NATIVE_LIBP2P_QUIC_DATAGRAMS: NativeLibp2pQuicDatagramCapability =
    NativeLibp2pQuicDatagramCapability::unavailable(
        "libp2p-quic 0.13.1 disables Quinn datagram receive buffers and Swarm exposes no application datagram handle",
    );

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeLibp2pQuicDatagramCapability {
    application_datagram_handle: bool,
    reason: &'static str,
}

impl NativeLibp2pQuicDatagramCapability {
    const fn unavailable(reason: &'static str) -> Self {
        Self {
            application_datagram_handle: false,
            reason,
        }
    }

    const fn can_advertise(self) -> bool {
        self.application_datagram_handle
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalPacketDataPlane {
    native_quic_datagrams: bool,
    owned_udp_packet_plane: bool,
    owned_quic_packet_plane: bool,
}

impl LocalPacketDataPlane {
    const fn identity_keyed_streams() -> Self {
        Self {
            native_quic_datagrams: NATIVE_LIBP2P_QUIC_DATAGRAMS.can_advertise(),
            owned_udp_packet_plane: false,
            owned_quic_packet_plane: false,
        }
    }
}

fn local_packet_data_plane() -> LocalPacketDataPlane {
    LOCAL_PACKET_DATA_PLANE
}

#[derive(Debug, Default)]
struct PacketPlaneNegotiator {
    pending: HashMap<PeerId, PendingPacketPlaneHello>,
}

#[derive(Debug)]
struct PendingPacketPlaneHello {
    secret: PacketPlaneEphemeralSecret,
    hello: VerifiedPacketPlaneHandshake,
    backend: PacketDatagramBackend,
    quic_connect_attempted: bool,
    quic_connect_defer_events: u8,
}

impl PacketPlaneNegotiator {
    fn insert(
        &mut self,
        peer: PeerId,
        secret: PacketPlaneEphemeralSecret,
        hello: VerifiedPacketPlaneHandshake,
        backend: PacketDatagramBackend,
    ) {
        self.pending.insert(
            peer,
            PendingPacketPlaneHello {
                secret,
                hello,
                backend,
                quic_connect_attempted: false,
                quic_connect_defer_events: u8::from(backend == PacketDatagramBackend::OwnedQuic),
            },
        );
    }

    fn remove(&mut self, peer: PeerId) -> Option<PendingPacketPlaneHello> {
        self.pending.remove(&peer)
    }

    fn has_pending(&self, peer: PeerId) -> bool {
        self.pending.contains_key(&peer)
    }

    fn remove_peer(&mut self, peer: PeerId) {
        self.pending.remove(&peer);
    }

    fn pending_quic_connect_peers(&mut self) -> Vec<PeerId> {
        let mut peers = Vec::new();
        for (peer, pending) in &mut self.pending {
            if pending.backend != PacketDatagramBackend::OwnedQuic || pending.quic_connect_attempted
            {
                continue;
            }
            if pending.quic_connect_defer_events > 0 {
                pending.quic_connect_defer_events -= 1;
                continue;
            }
            peers.push(*peer);
        }
        peers
    }

    fn mark_quic_connect_attempted(&mut self, peer: PeerId) {
        if let Some(pending) = self.pending.get_mut(&peer) {
            pending.quic_connect_attempted = true;
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PacketRateBucket {
    tokens: u32,
    refilled_at: Instant,
}

#[derive(Debug)]
struct PeerPacketRateLimiters {
    limit_per_second: u32,
    buckets: HashMap<Libp2pPeerId, PacketRateBucket>,
}

impl PeerPacketRateLimiters {
    fn new(limit_per_second: u32) -> Self {
        Self {
            limit_per_second: limit_per_second.max(1),
            buckets: HashMap::new(),
        }
    }

    const fn limit_per_second(&self) -> u32 {
        self.limit_per_second
    }

    fn allow(&mut self, peer: Libp2pPeerId, now: Instant) -> bool {
        let limit = self.limit_per_second;
        let bucket = self.buckets.entry(peer).or_insert(PacketRateBucket {
            tokens: limit,
            refilled_at: now,
        });

        let elapsed = now.saturating_duration_since(bucket.refilled_at);
        let refill = elapsed
            .as_secs()
            .saturating_mul(u64::from(limit))
            .saturating_add(
                u64::from(elapsed.subsec_nanos()).saturating_mul(u64::from(limit)) / 1_000_000_000,
            );
        if refill > 0 {
            let refill = u32::try_from(refill).unwrap_or(u32::MAX);
            bucket.tokens = bucket.tokens.saturating_add(refill).min(limit);
            bucket.refilled_at = now;
        }

        if bucket.tokens == 0 {
            return false;
        }

        bucket.tokens -= 1;
        true
    }

    fn remove(&mut self, peer: Libp2pPeerId) {
        self.buckets.remove(&peer);
    }
}

pub async fn run_config(
    config: Config,
    device: TunDevice,
    metrics_interval: Option<Duration>,
) -> Result<(), RunnerError> {
    run_config_until(
        config,
        device,
        metrics_interval,
        None,
        std::future::pending::<ShutdownReason>(),
    )
    .await
}

pub async fn run_config_until<Shutdown>(
    config: Config,
    device: TunDevice,
    metrics_interval: Option<Duration>,
    control_socket: Option<PathBuf>,
    shutdown: Shutdown,
) -> Result<(), RunnerError>
where
    Shutdown: Future<Output = ShutdownReason> + Send,
{
    let identity = config.identity()?;
    let mut node = build_node(&HostConfig {
        identity,
        network_name: config.network.name.clone(),
        membership_tag: config.membership_tag()?,
        mtu: config.effective_packet_mtu(),
        max_concurrent_control_streams: config.resources.control_stream_limit(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
        listen_addresses: config.listen_multiaddrs()?,
        external_addresses: config.external_multiaddrs()?,
        bootstrap_peers: config.effective_bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
        relay_reservations: config.relay_reservation_multiaddrs()?,
        relay_server: config.network.relay.server,
        relay_resources: config.network.relay.resources,
        resources: config.resources,
        discovery: config.network.discovery.clone(),
    })?;
    node.packet_endpoint_candidates = config.packet_plane_endpoint_candidates()?;
    let packet_plane = PacketPlaneRuntime::bind_with_replay_window_limit(
        config.packet_plane_listen_addrs()?,
        config.network.packet_plane.replay_window_limit(),
    )
    .await
    .map_err(RunnerError::PacketPlane)?;
    let packet_plane_quic = match config.packet_plane_quic_listen_addrs()?.as_slice() {
        [] => None,
        [listen_addr] => Some(
            PacketPlaneQuicRuntime::bind_with_replay_window_limit(
                *listen_addr,
                config.network.packet_plane.replay_window_limit(),
            )
            .map_err(RunnerError::PacketPlaneQuic)?,
        ),
        _ => {
            return Err(RunnerError::Config(ConfigError::PacketPlane(
                crate::config::PacketPlaneValidationError::TooManyQuicListeners {
                    actual: config.network.packet_plane.quic_listen.len(),
                    max: 1,
                },
            )));
        }
    };
    let forwarder = Forwarder::from_config(&config)?;
    let membership = OverlayMembership::from_config(&config)?;
    let previous_membership_tags = config.previous_membership_tags()?;

    Box::pin(run_node_until(
        node,
        forwarder,
        membership,
        previous_membership_tags,
        device,
        config.effective_packet_mtu(),
        config.queue,
        config.resources,
        metrics_interval,
        control_socket,
        packet_plane,
        packet_plane_quic,
        config.packet_plane_quic_endpoint_candidates()?,
        config.network.packet_plane.session_ttl(),
        config.network.packet_plane.replay_window_limit(),
        config.network.relay.auto,
        shutdown,
    ))
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownReason {
    Interrupt,
    Terminate,
    ControlSocket,
}

impl ShutdownReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Terminate => "terminate",
            Self::ControlSocket => "control_socket",
        }
    }
}

pub async fn run_node(
    node: P2pNode,
    forwarder: Forwarder,
    membership: OverlayMembership,
    device: TunDevice,
    options: RuntimeNodeOptions,
) -> Result<(), RunnerError> {
    Box::pin(run_node_until(
        node,
        forwarder,
        membership,
        Vec::new(),
        device,
        options.mtu,
        options.queue,
        options.resources,
        options.metrics_interval,
        None,
        PacketPlaneRuntime::disabled(),
        None,
        Vec::new(),
        options.packet_plane_session_ttl,
        options.packet_plane_replay_windows_per_session,
        options.auto_relay,
        std::future::pending::<ShutdownReason>(),
    ))
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeNodeOptions {
    pub mtu: u16,
    pub queue: QueueConfig,
    pub resources: ResourceConfig,
    pub metrics_interval: Option<Duration>,
    pub packet_plane_session_ttl: Duration,
    pub packet_plane_replay_windows_per_session: usize,
    pub auto_relay: AutoRelayConfig,
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub async fn run_node_until<Shutdown>(
    mut node: P2pNode,
    mut forwarder: Forwarder,
    mut membership: OverlayMembership,
    previous_membership_tags: Vec<String>,
    device: TunDevice,
    mtu: u16,
    queue_config: QueueConfig,
    resources: ResourceConfig,
    metrics_interval: Option<Duration>,
    control_socket: Option<PathBuf>,
    mut packet_plane: PacketPlaneRuntime,
    mut packet_plane_quic: Option<PacketPlaneQuicRuntime>,
    packet_plane_quic_external_endpoints: Vec<String>,
    packet_plane_session_ttl: Duration,
    packet_plane_replay_windows_per_session: usize,
    auto_relay_config: AutoRelayConfig,
    shutdown: Shutdown,
) -> Result<(), RunnerError>
where
    Shutdown: Future<Output = ShutdownReason> + Send,
{
    let (reader, mut writer) = device.split();
    let metrics = Arc::new(RuntimeMetrics::default());
    let mut tun_rx = spawn_tun_reader(reader, Arc::clone(&metrics), mtu);
    let mut queues = PeerQueues::with_packet_ttl(
        queue_config.max_packets_per_peer,
        queue_config.max_bytes_per_peer,
        queue_config.max_packet_age(),
    );
    let mut paths = PathSet::new();
    let mut peer_capabilities = PeerCapabilities::default();
    let mut packet_plane_negotiator = PacketPlaneNegotiator::default();
    let mut path_probe_tracker = PathProbeTracker::default();
    let mut relay_readiness = RelayReadiness::default();
    let mut configured_relay_reservation_retries =
        ConfiguredRelayReservationRetries::from_startup_attempts(
            &node.relay_reservation_addresses,
            Instant::now(),
        );
    let mut auto_relay = AutoRelayState::new(auto_relay_config);
    let mut infrastructure_peers = InfrastructurePeers::default();
    let mut queue_runtime = QueueRuntimeState::new(resources.packet_stream_limit());
    let mut inbound_packet_rate_limiters =
        PeerPacketRateLimiters::new(resources.inbound_packet_rate_limit());
    let kademlia_rendezvous_key = node.kademlia_rendezvous_key.clone();
    let kademlia_lookup_keys = kademlia_lookup_keys(
        &node.network_name,
        kademlia_rendezvous_key.as_ref(),
        &previous_membership_tags,
    );
    let kademlia_membership_records_key = node.kademlia_membership_records_key.clone();
    let kademlia_membership_record_lookup_keys = kademlia_membership_record_lookup_keys(
        &node.network_name,
        node.membership_tag.as_deref(),
        kademlia_membership_records_key.as_ref(),
        &previous_membership_tags,
    );
    let mut timers = RuntimeTimers::new(
        metrics_interval,
        !kademlia_lookup_keys.is_empty(),
        queue_config,
    );
    let mut packet_endpoint_candidates = node.packet_endpoint_candidates.clone();
    for listener in packet_plane.snapshot().listeners {
        if !listener.ip().is_unspecified() {
            let listener = listener.to_string();
            if !packet_endpoint_candidates.contains(&listener) {
                packet_endpoint_candidates.push(listener);
            }
        }
    }
    let packet_plane_quic_snapshot = packet_plane_quic.as_ref().map_or_else(
        PacketPlaneQuicRuntime::disabled_snapshot,
        PacketPlaneQuicRuntime::snapshot,
    );
    let mut packet_plane_quic_endpoint_candidates = packet_plane_quic_external_endpoints;
    if let Some(listener) = packet_plane_quic_snapshot.listener
        && !listener.ip().is_unspecified()
    {
        let listener = listener.to_string();
        if !packet_plane_quic_endpoint_candidates.contains(&listener) {
            packet_plane_quic_endpoint_candidates.push(listener);
        }
    }
    let mut local_capabilities =
        ControlCapabilities::local(&node.network_name, node.membership_tag.clone(), mtu)
            .with_packet_endpoint_candidates(packet_endpoint_candidates)
            .with_owned_quic_packet_endpoint_candidates(packet_plane_quic_endpoint_candidates)
            .with_advertised_routes(forwarder.local_advertised_routes())
            .with_member_records(advertised_member_records(&forwarder));
    let owned_udp_packet_plane = packet_plane.primary_listener().is_some()
        && !local_capabilities.packet_endpoint_candidates.is_empty();
    let local_data_plane = local_packet_data_plane();
    local_capabilities = local_capabilities
        .with_native_quic_datagrams(local_data_plane.native_quic_datagrams)
        .with_owned_udp_packet_plane(owned_udp_packet_plane);
    if let Some(certificate) = packet_plane_quic_snapshot.certificate_der.clone()
        && !local_capabilities
            .owned_quic_packet_endpoint_candidates
            .is_empty()
    {
        local_capabilities =
            local_capabilities.with_owned_quic_packet_plane_certificate(certificate);
    } else {
        local_capabilities = local_capabilities
            .with_owned_quic_packet_plane(local_data_plane.owned_quic_packet_plane);
    }
    timers.prime().await;
    let discovery = node.discovery.clone();
    let (control_socket_guard, mut control_rx) = match control_socket {
        Some(path) => {
            let (socket, rx) = ControlSocket::bind(path)?;
            log_runtime_event(
                LogLevel::Info,
                "control_socket_listening",
                &[("path", &socket.path().display().to_string())],
            );
            (Some(socket), Some(rx))
        }
        None => (None, None),
    };
    if let Some(socket) = control_socket_guard {
        // The socket accept task must live for the daemon lifetime.  The runtime
        // command removes stale socket paths before binding in managed runs.
        std::mem::forget(socket);
    }

    log_startup_status(node.startup);
    log_packet_plane_status(packet_plane.snapshot());
    log_packet_plane_quic_status(&packet_plane_quic_snapshot);
    log_runtime_event(
        LogLevel::Info,
        "runtime_started",
        &[("network", &node.network_name)],
    );
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            reason = &mut shutdown => {
                log_runtime_event(
                    LogLevel::Info,
                    "runtime_shutdown",
                    &[("reason", reason.as_str())],
                );
                print_metrics(
                    &metrics,
                    queues.total_stats(),
                    runtime_path_stats(&forwarder, &paths, &peer_capabilities),
                );
                return Ok(());
            }
            Some(packet) = tun_rx.recv() => {
                if let Err(error) = forwarder.enqueue_tun_packet(&mut queues, packet) {
                    metrics.record_outbound_drop(outbound_drop_reason(&error));
                    eprintln!("dropping outbound packet: {error:?}");
                }
                drain_runtime_outbound_queue(RuntimeOutboundDrain {
                    node: &mut node,
                    forwarder: &forwarder,
                    queues: &mut queues,
                    paths: &mut paths,
                    peer_capabilities: &peer_capabilities,
                    queue_runtime: &mut queue_runtime,
                    writer: &mut writer,
                    packet_plane: &packet_plane,
                    packet_plane_quic: packet_plane_quic.as_ref(),
                    metrics: &metrics,
                })
                .await;
            }
            event = node.swarm.select_next_some() => {
                handle_swarm_event(
                    &mut node.swarm,
                    SwarmEventContext {
                        forwarder: &mut forwarder,
                        membership: &mut membership,
                        infrastructure_peers: &mut infrastructure_peers,
                        writer: &mut writer,
                        paths: &mut paths,
                        peer_capabilities: &mut peer_capabilities,
                        relay_readiness: &mut relay_readiness,
                        auto_relay: &mut auto_relay,
                        configured_peer_addresses: &node.configured_peer_addresses,
                        discovered_peer_addresses: &mut queue_runtime.discovered_peer_addresses,
                        packet_in_flight: &mut queue_runtime.packet_in_flight,
                        inbound_packet_rate_limiters: &mut inbound_packet_rate_limiters,
                        metrics: &metrics,
                        local_capabilities: &mut local_capabilities,
                        previous_membership_tags: &previous_membership_tags,
                        discovery: &discovery,
                        identity: &node.identity,
                        packet_plane: &mut packet_plane,
                        packet_plane_quic: packet_plane_quic.as_mut(),
                        packet_plane_negotiator: &mut packet_plane_negotiator,
                        packet_plane_session_ttl,
                        packet_plane_replay_windows_per_session,
                    },
                    event,
                ).await?;
                drive_pending_packet_plane_quic_connects(
                    packet_plane_quic.as_mut(),
                    &mut packet_plane_negotiator,
                    &peer_capabilities,
                    &metrics,
                )
                .await;
                drain_runtime_outbound_queue(RuntimeOutboundDrain {
                    node: &mut node,
                    forwarder: &forwarder,
                    queues: &mut queues,
                    paths: &mut paths,
                    peer_capabilities: &peer_capabilities,
                    queue_runtime: &mut queue_runtime,
                    writer: &mut writer,
                    packet_plane: &packet_plane,
                    packet_plane_quic: packet_plane_quic.as_ref(),
                    metrics: &metrics,
                })
                .await;
            }
            received = packet_plane.recv_frame_from_session(), if packet_plane.can_receive() => {
                match received {
                    Ok(received) => {
                        let mut packet_plane_context = PacketPlaneInboundContext {
                            forwarder: &mut forwarder,
                            writer: &mut writer,
                            paths: &mut paths,
                            peer_capabilities: &peer_capabilities,
                            inbound_packet_rate_limiters: &mut inbound_packet_rate_limiters,
                            packet_plane: Some(&packet_plane),
                            packet_plane_quic: packet_plane_quic.as_ref(),
                            backend: PacketDatagramBackend::OwnedUdp,
                            path_probe_tracker: &mut path_probe_tracker,
                            metrics: &metrics,
                        };
                        handle_packet_plane_received(&mut packet_plane_context, &received).await?;
                    }
                    Err(error) => handle_packet_plane_receive_error(&metrics, &error),
                }
            }
            received = async {
                packet_plane_quic
                    .as_mut()
                    .expect("packet-plane QUIC runtime is present")
                    .recv_frame_from_session()
                    .await
            }, if packet_plane_quic.as_ref().is_some_and(PacketPlaneQuicRuntime::can_receive) => {
                match received {
                    Ok(received) => {
                        let mut packet_plane_context = PacketPlaneInboundContext {
                            forwarder: &mut forwarder,
                            writer: &mut writer,
                            paths: &mut paths,
                            peer_capabilities: &peer_capabilities,
                            inbound_packet_rate_limiters: &mut inbound_packet_rate_limiters,
                            packet_plane: Some(&packet_plane),
                            packet_plane_quic: packet_plane_quic.as_ref(),
                            backend: PacketDatagramBackend::OwnedQuic,
                            path_probe_tracker: &mut path_probe_tracker,
                            metrics: &metrics,
                        };
                        handle_packet_plane_received(&mut packet_plane_context, &received).await?;
                    }
                    Err(error) => {
                        handle_packet_plane_quic_receive_error(&mut paths, &metrics, &error);
                    }
                }
            }
            _ = timers.redial.tick() => {
                expire_pending_auto_relay_reservations(
                    &mut auto_relay,
                    &metrics,
                    Instant::now(),
                );
                attempt_auto_relay_reservations(&mut node.swarm, &mut auto_relay, &metrics);
                handle_redial_tick(
                    &mut node,
                    &mut queue_runtime.discovered_peer_addresses,
                    &paths,
                    &relay_readiness,
                    &mut configured_relay_reservation_retries,
                    &metrics,
                );
            }
            () = async {
                timers.kademlia_refresh
                    .as_mut()
                    .expect("kademlia refresh interval is present")
                    .tick()
                    .await;
            }, if timers.kademlia_refresh.is_some() => {
                refresh_kademlia_rendezvous(
                    &mut node.swarm,
                    &KademliaRefreshContext {
                        advertise_key: kademlia_rendezvous_key
                            .as_ref()
                            .expect("kademlia rendezvous key is present"),
                        lookup_keys: &kademlia_lookup_keys,
                        membership_record_advertise_key: kademlia_membership_records_key.as_ref(),
                        membership_record_lookup_keys: &kademlia_membership_record_lookup_keys,
                        network_name: &node.network_name,
                        membership_tag: node.membership_tag.as_deref(),
                        forwarder: &forwarder,
                        identity: &node.identity,
                        advertise_provider: discovery.kademlia_provider_advertisement,
                        auto_relay: &auto_relay,
                        metrics: &metrics,
                    },
                );
            }
            _ = timers.queue_expiry.tick() => {
                drive_pending_packet_plane_quic_connects(
                    packet_plane_quic.as_mut(),
                    &mut packet_plane_negotiator,
                    &peer_capabilities,
                    &metrics,
                )
                .await;
                expire_outbound_queue(&mut queues, &metrics);
                let expired_replay_sessions = forwarder.expire_replay_sessions();
                if expired_replay_sessions > 0 {
                    let count = expired_replay_sessions.to_string();
                    log_runtime_event(LogLevel::Info, "replay_sessions_expired", &[("count", &count)]);
                }
                prune_expired_membership_records(
                    &mut forwarder,
                    &mut membership,
                    &mut local_capabilities,
                )?;
                let mut expiry_context = PacketPlaneExpiryContext {
                    swarm: &mut node.swarm,
                    forwarder: &forwarder,
                    paths: &mut paths,
                    peer_capabilities: &peer_capabilities,
                    packet_plane: &mut packet_plane,
                    packet_plane_quic: packet_plane_quic.as_mut(),
                    negotiator: &mut packet_plane_negotiator,
                    identity: &node.identity,
                    local_capabilities: &local_capabilities,
                    metrics: &metrics,
                    session_ttl: packet_plane_session_ttl,
                };
                expire_packet_plane_sessions(&mut expiry_context);
            }
            _ = timers.path_probe.tick() => {
                let discovered_addresses = queue_runtime.discovered_peer_addresses.as_vec();
                expire_unconfirmed_path_probes(
                    &mut node.swarm,
                    &forwarder,
                    &mut paths,
                    &node.configured_peer_addresses,
                    &discovered_addresses,
                    &mut path_probe_tracker,
                    &metrics,
                    Instant::now(),
                );
                send_path_probes(
                    &mut node.swarm,
                    &mut forwarder,
                    &paths,
                    &peer_capabilities,
                    Some(&packet_plane),
                    packet_plane_quic.as_ref(),
                    &mut path_probe_tracker,
                    &metrics,
                )
                .await;
            }
            Some(request) = async {
                control_rx
                    .as_mut()
                    .expect("control socket receiver is present")
                    .recv()
                    .await
            }, if control_rx.is_some() => {
                let control_context = RuntimeControlContext {
                    forwarder: &forwarder,
                    paths: &paths,
                    peer_capabilities: &peer_capabilities,
                    local_capabilities: &local_capabilities,
                    metrics: &metrics,
                    queue: queues.total_stats(),
                    path_stats: runtime_path_stats(&forwarder, &paths, &peer_capabilities),
                    packet_in_flight: queue_runtime.packet_in_flight.stats(),
                    auto_relay: auto_relay.snapshot(Instant::now()),
                    relay_infrastructure: infrastructure_peers.snapshot(&node.swarm),
                    packet_plane: packet_plane.snapshot(),
                    packet_plane_quic: current_packet_plane_quic_snapshot(
                        packet_plane_quic.as_ref(),
                    ),
                    packet_plane_session_ttl,
                    packet_plane_replay_windows_per_session,
                };
                if let Some(reason) = handle_runtime_control_request(request, &control_context) {
                    log_runtime_event(
                        LogLevel::Info,
                        "runtime_shutdown",
                        &[("reason", reason.as_str())],
                    );
                    print_metrics(
                        &metrics,
                        queues.total_stats(),
                        runtime_path_stats(&forwarder, &paths, &peer_capabilities),
                    );
                    return Ok(());
                }
            }
            () = async {
                timers.metrics
                    .as_mut()
                    .expect("metrics interval is present")
                    .tick()
                    .await;
            }, if timers.metrics.is_some() => {
                print_metrics(
                    &metrics,
                    queues.total_stats(),
                    runtime_path_stats(&forwarder, &paths, &peer_capabilities),
                );
            }
        }
    }
}

struct RuntimeTimers {
    metrics: Option<tokio::time::Interval>,
    redial: tokio::time::Interval,
    kademlia_refresh: Option<tokio::time::Interval>,
    queue_expiry: tokio::time::Interval,
    path_probe: tokio::time::Interval,
}

impl RuntimeTimers {
    fn new(
        metrics_interval: Option<Duration>,
        kademlia_enabled: bool,
        queue_config: QueueConfig,
    ) -> Self {
        Self {
            metrics: metrics_interval.map(tokio::time::interval),
            redial: tokio::time::interval(REDIAL_INTERVAL),
            kademlia_refresh: kademlia_enabled
                .then(|| tokio::time::interval(KADEMLIA_REFRESH_INTERVAL)),
            queue_expiry: tokio::time::interval(queue_expiry_interval(
                queue_config.max_packet_age(),
            )),
            path_probe: tokio::time::interval(PATH_PROBE_INTERVAL),
        }
    }

    async fn prime(&mut self) {
        self.redial.tick().await;
        if let Some(tick) = &mut self.kademlia_refresh {
            tick.tick().await;
        }
        self.queue_expiry.tick().await;
        self.path_probe.tick().await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_path_probes(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &mut Forwarder,
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    path_probe_tracker: &mut PathProbeTracker,
    metrics: &RuntimeMetrics,
) {
    let peers = forwarder.configured_overlay_peers().collect::<Vec<_>>();
    let local_mtu = u16::try_from(forwarder.mtu()).unwrap_or(u16::MAX);

    for peer in peers {
        if !peer_capabilities.contains(peer) {
            continue;
        }
        let datagram_backend =
            local_packet_datagram_backend(peer_capabilities, packet_plane, packet_plane_quic, peer);
        let support =
            packet_transport_support_for_backend(peer_capabilities, peer, datagram_backend);
        if !paths.has_supported_path(peer, support) {
            continue;
        }

        let peer_mtu = selected_path_mtu(
            paths,
            peer_capabilities,
            packet_plane,
            packet_plane_quic,
            peer,
            local_mtu,
        );
        match packet_transport_decision(
            paths,
            peer_capabilities,
            packet_plane,
            packet_plane_quic,
            peer,
        ) {
            PacketTransportDecision::PacketPlaneDatagram { backend, .. } => {
                let probe_mtu = selected_path_probe_mtu(
                    paths,
                    peer_capabilities,
                    packet_plane,
                    packet_plane_quic,
                    backend,
                    peer,
                    local_mtu,
                );
                let token = fresh_path_probe_token();
                let payload = mtu_sized_path_probe_payload(probe_mtu, Some(token));
                match forwarder.path_probe_frame_with_mtu(probe_mtu, &payload) {
                    Ok(frame) => match send_packet_plane_frame(
                        packet_plane,
                        packet_plane_quic,
                        backend,
                        peer,
                        &frame,
                    )
                    .await
                    {
                        Ok(_) => {
                            path_probe_tracker.record(
                                peer,
                                packet_datagram_backend_path_kind(backend),
                                token,
                                Instant::now(),
                            );
                            metrics.record_outbound_path_probe_sent();
                        }
                        Err(error) => {
                            metrics.record_outbound_path_probe_failure();
                            eprintln!("packet-plane path probe to {peer} failed: {error:?}");
                        }
                    },
                    Err(error) => {
                        metrics.record_outbound_path_probe_failure();
                        eprintln!("packet-plane path probe to {peer} failed: {error:?}");
                    }
                }
            }
            PacketTransportDecision::StreamFallback { .. } => {
                match forwarder.send_path_probe_with_mtu(swarm, peer, peer_mtu, PATH_PROBE_PAYLOAD)
                {
                    Ok(_) => metrics.record_outbound_path_probe_sent(),
                    Err(error) => {
                        metrics.record_outbound_path_probe_failure();
                        eprintln!("path probe to {peer} failed: {error:?}");
                    }
                }
            }
            PacketTransportDecision::Blocked { .. } => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn expire_unconfirmed_path_probes(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    paths: &mut PathSet,
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    path_probe_tracker: &mut PathProbeTracker,
    metrics: &RuntimeMetrics,
    now: Instant,
) {
    for probe in path_probe_tracker.expire_unconfirmed(now, PATH_PROBE_TIMEOUT) {
        metrics.record_outbound_path_probe_failure();
        if !demote_packet_plane_path_probe_timeout(paths, metrics, probe.peer, probe.path) {
            continue;
        }
        if let Some(peer) = forwarder.transport_peer_for_overlay(probe.peer) {
            redial_packet_plane_recovery_addresses(
                swarm,
                peer,
                configured_peer_addresses,
                discovered_peer_addresses,
                metrics,
            );
        }
    }
}

fn mtu_sized_path_probe_payload(peer_mtu: u16, token: Option<u64>) -> Vec<u8> {
    let len = usize::from(peer_mtu).max(PATH_PROBE_PAYLOAD.len());
    let mut payload = vec![0; len];
    payload[..PATH_PROBE_PAYLOAD.len()].copy_from_slice(PATH_PROBE_PAYLOAD);
    let mtu_offset = PATH_PROBE_PAYLOAD.len();
    if payload.len() >= mtu_offset + 2 {
        payload[mtu_offset..mtu_offset + 2].copy_from_slice(&peer_mtu.to_be_bytes());
    }
    let token_offset = mtu_offset + 2;
    if let Some(token) = token
        && payload.len() >= token_offset + PATH_PROBE_TOKEN_LEN
    {
        payload[token_offset..token_offset + PATH_PROBE_TOKEN_LEN]
            .copy_from_slice(&token.to_be_bytes());
    }
    payload
}

fn path_probe_ack_payload(probed_mtu: u16, token: Option<u64>) -> Vec<u8> {
    let mut payload = Vec::with_capacity(PATH_PROBE_ACK_PAYLOAD.len() + 2 + PATH_PROBE_TOKEN_LEN);
    payload.extend_from_slice(PATH_PROBE_ACK_PAYLOAD);
    payload.extend_from_slice(&probed_mtu.to_be_bytes());
    if let Some(token) = token {
        payload.extend_from_slice(&token.to_be_bytes());
    }
    payload
}

fn path_probe_request_mtu(payload: &[u8]) -> Option<u16> {
    let mtu_offset = PATH_PROBE_PAYLOAD.len();
    if payload.len() < mtu_offset || !payload.starts_with(PATH_PROBE_PAYLOAD) {
        return None;
    }
    payload
        .get(mtu_offset..mtu_offset + 2)
        .map(|mtu| u16::from_be_bytes([mtu[0], mtu[1]]))
        .or_else(|| u16::try_from(payload.len()).ok())
}

fn path_probe_ack_mtu(payload: &[u8]) -> Option<u16> {
    let mtu_offset = PATH_PROBE_ACK_PAYLOAD.len();
    if payload.len() < mtu_offset + 2 || !payload.starts_with(PATH_PROBE_ACK_PAYLOAD) {
        return None;
    }
    Some(u16::from_be_bytes([
        payload[mtu_offset],
        payload[mtu_offset + 1],
    ]))
}

fn path_probe_request_token(payload: &[u8]) -> Option<u64> {
    path_probe_token(payload, PATH_PROBE_PAYLOAD.len() + 2, PATH_PROBE_PAYLOAD)
}

fn path_probe_ack_token(payload: &[u8]) -> Option<u64> {
    path_probe_token(
        payload,
        PATH_PROBE_ACK_PAYLOAD.len() + 2,
        PATH_PROBE_ACK_PAYLOAD,
    )
}

fn path_probe_token(payload: &[u8], offset: usize, prefix: &[u8]) -> Option<u64> {
    if !payload.starts_with(prefix) {
        return None;
    }
    let token = payload.get(offset..offset + PATH_PROBE_TOKEN_LEN)?;
    Some(u64::from_be_bytes([
        token[0], token[1], token[2], token[3], token[4], token[5], token[6], token[7],
    ]))
}

fn fresh_path_probe_token() -> u64 {
    loop {
        let token = OsRng.next_u64();
        if token != 0 {
            return token;
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingPathProbe {
    peer: PeerId,
    path: PathKind,
    sent_at: Instant,
}

#[derive(Debug, Default)]
struct PathProbeTracker {
    pending: HashMap<u64, PendingPathProbe>,
}

impl PathProbeTracker {
    fn record(&mut self, peer: PeerId, path: PathKind, token: u64, now: Instant) {
        self.drop_rtt_expired(now);
        if self.pending.len() >= MAX_PENDING_PATH_PROBES
            && let Some(oldest) = self
                .pending
                .iter()
                .min_by_key(|(_, probe)| probe.sent_at)
                .map(|(token, _)| *token)
        {
            self.pending.remove(&oldest);
        }
        self.pending.insert(
            token,
            PendingPathProbe {
                peer,
                path,
                sent_at: now,
            },
        );
    }

    fn confirm(&mut self, peer: PeerId, token: u64, now: Instant) -> Option<(PathKind, u16)> {
        self.drop_rtt_expired(now);
        let probe = self.pending.remove(&token)?;
        if probe.peer != peer {
            return None;
        }
        let rtt = now.saturating_duration_since(probe.sent_at).as_millis();
        let rtt_ms = u16::try_from(rtt).unwrap_or(u16::MAX);
        Some((probe.path, rtt_ms))
    }

    fn expire_unconfirmed(&mut self, now: Instant, timeout: Duration) -> Vec<PendingPathProbe> {
        let expired_tokens = self
            .pending
            .iter()
            .filter_map(|(token, probe)| {
                (now.saturating_duration_since(probe.sent_at) > timeout).then_some(*token)
            })
            .collect::<Vec<_>>();
        let mut expired = Vec::with_capacity(expired_tokens.len());
        for token in expired_tokens {
            if let Some(probe) = self.pending.remove(&token) {
                expired.push(probe);
            }
        }
        expired
    }

    fn drop_rtt_expired(&mut self, now: Instant) {
        self.pending
            .retain(|_, probe| now.saturating_duration_since(probe.sent_at) <= PATH_PROBE_RTT_TTL);
    }
}

struct RuntimeControlContext<'a> {
    forwarder: &'a Forwarder,
    paths: &'a PathSet,
    peer_capabilities: &'a PeerCapabilities,
    local_capabilities: &'a ControlCapabilities,
    metrics: &'a RuntimeMetrics,
    queue: crate::queue::QueueStats,
    path_stats: crate::path::PathRuntimeStats,
    packet_in_flight: PacketInFlightStats,
    auto_relay: AutoRelaySnapshot,
    relay_infrastructure: RelayInfrastructureSnapshot,
    packet_plane: PacketPlaneSnapshot,
    packet_plane_quic: PacketPlaneQuicSnapshot,
    packet_plane_session_ttl: Duration,
    packet_plane_replay_windows_per_session: usize,
}

fn handle_runtime_control_request(
    request: RuntimeControlRequest,
    context: &RuntimeControlContext<'_>,
) -> Option<ShutdownReason> {
    match request {
        RuntimeControlRequest::Status { respond_to } => {
            let lines = runtime_status_lines(RuntimeStatusView {
                metrics: context.metrics,
                queue: context.queue,
                path_stats: context.path_stats,
                auto_relay: context.auto_relay,
                packet_plane: &context.packet_plane,
                packet_plane_quic: &context.packet_plane_quic,
                packet_plane_session_ttl: context.packet_plane_session_ttl,
                packet_plane_replay_windows_per_session: context
                    .packet_plane_replay_windows_per_session,
            });
            if respond_to.send(lines).is_err() {
                eprintln!("control socket status response receiver dropped");
            }
            None
        }
        RuntimeControlRequest::State { respond_to } => {
            let lines = runtime_state_lines(&RuntimeStateView {
                forwarder: context.forwarder,
                paths: context.paths,
                peer_capabilities: context.peer_capabilities,
                metrics: context.metrics,
                queue: context.queue,
                path_stats: context.path_stats,
                packet_in_flight: context.packet_in_flight,
                auto_relay: context.auto_relay,
                relay_infrastructure: &context.relay_infrastructure,
                packet_plane: &context.packet_plane,
                packet_plane_quic: &context.packet_plane_quic,
                packet_plane_session_ttl: context.packet_plane_session_ttl,
                packet_plane_replay_windows_per_session: context
                    .packet_plane_replay_windows_per_session,
            });
            if respond_to.send(lines).is_err() {
                eprintln!("control socket state response receiver dropped");
            }
            None
        }
        RuntimeControlRequest::Peers { respond_to } => {
            let lines =
                runtime_peer_lines(context.forwarder, context.paths, context.peer_capabilities);
            if respond_to.send(lines).is_err() {
                eprintln!("control socket peers response receiver dropped");
            }
            None
        }
        RuntimeControlRequest::Routes { respond_to } => {
            let lines = runtime_route_lines(context.forwarder, context.peer_capabilities);
            if respond_to.send(lines).is_err() {
                eprintln!("control socket routes response receiver dropped");
            }
            None
        }
        RuntimeControlRequest::Paths { respond_to } => {
            let lines =
                runtime_path_lines(context.forwarder, context.paths, context.peer_capabilities);
            if respond_to.send(lines).is_err() {
                eprintln!("control socket paths response receiver dropped");
            }
            None
        }
        RuntimeControlRequest::Mtu { respond_to } => {
            let lines =
                runtime_mtu_lines(context.forwarder, context.paths, context.peer_capabilities);
            if respond_to.send(lines).is_err() {
                eprintln!("control socket mtu response receiver dropped");
            }
            None
        }
        RuntimeControlRequest::Capabilities { respond_to } => {
            let lines = runtime_capability_lines(
                context.forwarder,
                context.peer_capabilities,
                context.local_capabilities,
                &context.packet_plane,
            );
            if respond_to.send(lines).is_err() {
                eprintln!("control socket capabilities response receiver dropped");
            }
            None
        }
        RuntimeControlRequest::Shutdown { respond_to } => {
            if respond_to
                .send(vec!["shutdown accepted".to_owned()])
                .is_err()
            {
                eprintln!("control socket shutdown response receiver dropped");
            }
            Some(ShutdownReason::ControlSocket)
        }
    }
}

fn runtime_peer_lines(
    forwarder: &Forwarder,
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
) -> Vec<String> {
    let local_mtu = u16::try_from(forwarder.mtu()).unwrap_or(u16::MAX);
    let mut peers = sorted_configured_peers(forwarder);
    let mut lines = vec![format!("peers: {}", peers.len())];

    for peer in peers.drain(..) {
        let transport = forwarder
            .transport_peer_for_overlay(peer)
            .map_or_else(|| "none".to_owned(), |peer| peer.to_string());
        let support = packet_transport_support(peer_capabilities, peer);
        let selected_path = paths.best_supported_for(peer, support);
        let candidates = paths.candidates_for(peer).collect::<Vec<_>>();
        let healthy_paths = candidates
            .iter()
            .filter(|candidate| candidate.healthy)
            .count();

        lines.push(format!(
            "peer: {peer} transport {transport} validated {} effective_mtu {} quic_datagrams {} native_quic_datagrams {} owned_udp_packet_plane {} owned_quic_packet_plane {} healthy_paths {healthy_paths} selected_path {}",
            peer_capabilities.contains(peer),
            peer_capabilities.effective_mtu_for(peer, local_mtu),
            support.quic_datagrams,
            peer_capabilities.supports_native_quic_datagrams_for(peer),
            peer_capabilities.supports_owned_udp_packet_plane_for(peer),
            peer_capabilities.supports_owned_quic_packet_plane_for(peer),
            selected_path.map_or("none", |path| path.kind.wire_name()),
        ));
    }

    lines
}

fn runtime_route_lines(forwarder: &Forwarder, peer_capabilities: &PeerCapabilities) -> Vec<String> {
    let local_routes = forwarder.local_advertised_routes();
    let mut peers = sorted_configured_peers(forwarder);
    let remote_route_count = peers
        .iter()
        .filter_map(|peer| peer_capabilities.get(*peer))
        .map(|capabilities| capabilities.advertised_routes.len())
        .sum::<usize>();
    let mut lines = vec![
        format!("local advertised routes: {}", local_routes.len()),
        format!("remote advertised routes: {remote_route_count}"),
    ];

    for route in local_routes {
        lines.push(format!(
            "local advertised route: {} metric {}",
            route.prefix, route.metric
        ));
    }
    for peer in peers.drain(..) {
        let Some(capabilities) = peer_capabilities.get(peer) else {
            lines.push(format!("peer advertised routes: {peer} unvalidated"));
            continue;
        };
        lines.push(format!(
            "peer advertised routes: {peer} {}",
            capabilities.advertised_routes.len()
        ));
        for route in &capabilities.advertised_routes {
            lines.push(format!(
                "peer advertised route: {peer} {} metric {}",
                route.prefix, route.metric
            ));
        }
    }

    lines
}

fn runtime_path_lines(
    forwarder: &Forwarder,
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
) -> Vec<String> {
    let local_mtu = u16::try_from(forwarder.mtu()).unwrap_or(u16::MAX);
    let mut peers = sorted_configured_peers(forwarder);
    let mut lines = vec![format!("peers: {}", peers.len())];

    for peer in peers.drain(..) {
        let support = packet_transport_support(peer_capabilities, peer);
        let selected_path = paths.best_supported_for(peer, support);
        let candidates = paths.candidates_for(peer).collect::<Vec<_>>();
        let peer_mtu = peer_capabilities.effective_mtu_for(peer, local_mtu);
        lines.push(format!(
            "peer selected path: {peer} {} score {} mtu {}",
            selected_path.map_or("none", |path| path.kind.wire_name()),
            selected_path.map_or_else(|| "none".to_owned(), |path| path.score().to_string()),
            selected_path.map_or_else(
                || "none".to_owned(),
                |path| path.effective_mtu(peer_mtu).to_string()
            )
        ));
        lines.push(format!("peer path candidates: {peer} {}", candidates.len()));
        for candidate in candidates {
            lines.push(format!(
                "peer path: {peer} {} healthy {} relay {} direct {} established_connections {} score {} estimated_mtu {} effective_mtu {} observed_rtt_ms {}",
                candidate.kind.wire_name(),
                candidate.healthy,
                candidate.relay,
                !candidate.relay,
                candidate.established_connections,
                candidate.score(),
                candidate
                    .estimated_mtu
                    .map_or_else(|| "unknown".to_owned(), |mtu| mtu.to_string()),
                candidate.effective_mtu(peer_mtu),
                path_rtt_value(candidate)
            ));
        }
    }

    lines
}

fn path_rtt_value(path: crate::path::PathCandidate) -> String {
    path.observed_rtt_ms
        .map_or_else(|| "unknown".to_owned(), |rtt| rtt.to_string())
}

fn runtime_mtu_lines(
    forwarder: &Forwarder,
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
) -> Vec<String> {
    let local_mtu = u16::try_from(forwarder.mtu()).unwrap_or(u16::MAX);
    let mut peers = sorted_configured_peers(forwarder);
    let mut lines = vec![
        format!("local effective packet mtu: {local_mtu}"),
        OVERLAY_FRAGMENTATION_POLICY_LINE.to_owned(),
        format!("peers: {}", peers.len()),
    ];

    for peer in peers.drain(..) {
        let support = packet_transport_support(peer_capabilities, peer);
        let selected_path = paths.best_supported_for(peer, support);
        let peer_mtu = peer_capabilities.effective_mtu_for(peer, local_mtu);
        let selected_path_mtu = selected_path.map(|path| path.effective_mtu(peer_mtu));
        lines.push(format!(
            "peer mtu: {peer} validated {} effective_mtu {} selected_path {} selected_path_mtu {}",
            peer_capabilities.contains(peer),
            peer_mtu,
            selected_path.map_or("none", |path| path.kind.wire_name()),
            selected_path_mtu.map_or_else(|| "none".to_owned(), |mtu| mtu.to_string())
        ));

        for candidate in paths.candidates_for(peer) {
            lines.push(format!(
                "peer path mtu: {peer} {} healthy {} estimated_mtu {} effective_mtu {}",
                candidate.kind.wire_name(),
                candidate.healthy,
                candidate
                    .estimated_mtu
                    .map_or_else(|| "unknown".to_owned(), |mtu| mtu.to_string()),
                candidate.effective_mtu(peer_mtu)
            ));
        }
    }

    lines
}

fn runtime_capability_lines(
    forwarder: &Forwarder,
    peer_capabilities: &PeerCapabilities,
    local_capabilities: &ControlCapabilities,
    packet_plane: &PacketPlaneSnapshot,
) -> Vec<String> {
    let mut peers = sorted_configured_peers(forwarder);
    let mut lines = local_capability_lines(local_capabilities, packet_plane);
    lines.push(format!("validated peers: {}", peer_capabilities.len()));
    for peer in peers.drain(..) {
        let Some(capabilities) = peer_capabilities.get(peer) else {
            lines.push(format!("remote capability peer: {peer} unvalidated"));
            continue;
        };
        extend_remote_capability_lines(&mut lines, peer, capabilities);
    }

    lines
}

fn local_capability_lines(
    local_capabilities: &ControlCapabilities,
    packet_plane: &PacketPlaneSnapshot,
) -> Vec<String> {
    let mut lines = vec![
        format!(
            "local capability network: {}",
            local_capabilities.network_name
        ),
        format!(
            "local capability membership key matched: {}",
            local_capabilities.membership_tag.is_some()
        ),
        format!(
            "local capability wire version: {}",
            local_capabilities.wire_version
        ),
        format!(
            "local capability packet protocol: {}",
            local_capabilities.packet_protocol
        ),
        format!(
            "local capability packet header length: {}",
            local_capabilities.packet_header_len
        ),
        format!("local capability mtu: {}", local_capabilities.effective_mtu),
        format!(
            "local capability preferred path: {}",
            local_capabilities.preferred_path
        ),
        format!(
            "local capability supports quic datagrams: {}",
            local_capabilities.supports_quic_datagrams
        ),
        format!(
            "local capability supports native quic datagrams: {}",
            local_capabilities.supports_native_quic_datagrams
        ),
        format!(
            "local capability supports owned udp packet plane: {}",
            local_capabilities.supports_owned_udp_packet_plane
        ),
        format!(
            "local capability supports owned quic packet plane: {}",
            local_capabilities.supports_owned_quic_packet_plane
        ),
        format!(
            "local capability owned quic packet plane certificate bytes: {}",
            local_capabilities
                .owned_quic_packet_plane_certificate_der
                .as_ref()
                .map_or(0, Vec::len)
        ),
        format!(
            "local capability packet endpoint candidates: {}",
            local_capabilities.packet_endpoint_candidates.len()
        ),
        format!(
            "local capability owned quic packet endpoint candidates: {}",
            local_capabilities
                .owned_quic_packet_endpoint_candidates
                .len()
        ),
        format!("packet plane listeners: {}", packet_plane.listeners.len()),
        format!("packet plane sessions: {}", packet_plane.sessions.len()),
        format!(
            "local capability advertised routes: {}",
            local_capabilities.advertised_routes.len()
        ),
    ];
    extend_local_capability_detail_lines(&mut lines, local_capabilities, packet_plane);
    lines
}

fn extend_local_capability_detail_lines(
    lines: &mut Vec<String>,
    local_capabilities: &ControlCapabilities,
    packet_plane: &PacketPlaneSnapshot,
) {
    for route in &local_capabilities.advertised_routes {
        lines.push(format!(
            "local capability advertised route: {} metric {}",
            route.prefix, route.metric
        ));
    }
    for endpoint in &local_capabilities.packet_endpoint_candidates {
        lines.push(format!(
            "local capability packet endpoint candidate: {endpoint}"
        ));
    }
    for endpoint in &local_capabilities.owned_quic_packet_endpoint_candidates {
        lines.push(format!(
            "local capability owned quic packet endpoint candidate: {endpoint}"
        ));
    }
    for listener in &packet_plane.listeners {
        lines.push(format!("packet plane listener: {listener}"));
    }
    for session in &packet_plane.sessions {
        lines.push(format!(
            "packet plane session: {} endpoint {} mtu {} role {} local_session {} remote_session {}",
            session.peer,
            session.endpoint,
            session.mtu,
            packet_plane_session_role_name(session.role),
            session.local_session_id,
            session.remote_session_id
        ));
    }
}

fn extend_remote_capability_lines(
    lines: &mut Vec<String>,
    peer: PeerId,
    capabilities: &ControlCapabilities,
) {
    lines.push(format!("remote capability peer: {peer}"));
    lines.push(format!(
        "remote capability network: {peer} {}",
        capabilities.network_name
    ));
    lines.push(format!(
        "remote capability membership key matched: {peer} {}",
        capabilities.membership_tag.is_some()
    ));
    lines.push(format!(
        "remote capability wire version: {peer} {}",
        capabilities.wire_version
    ));
    lines.push(format!(
        "remote capability packet protocol: {peer} {}",
        capabilities.packet_protocol
    ));
    lines.push(format!(
        "remote capability packet header length: {peer} {}",
        capabilities.packet_header_len
    ));
    lines.push(format!(
        "remote capability mtu: {peer} {}",
        capabilities.effective_mtu
    ));
    lines.push(format!(
        "remote capability preferred path: {peer} {}",
        capabilities.preferred_path
    ));
    lines.push(format!(
        "remote capability supports quic datagrams: {peer} {}",
        capabilities.supports_quic_datagrams
    ));
    lines.push(format!(
        "remote capability supports native quic datagrams: {peer} {}",
        capabilities.supports_native_quic_datagrams
    ));
    lines.push(format!(
        "remote capability supports owned udp packet plane: {peer} {}",
        capabilities.supports_owned_udp_packet_plane
    ));
    lines.push(format!(
        "remote capability supports owned quic packet plane: {peer} {}",
        capabilities.supports_owned_quic_packet_plane
    ));
    lines.push(format!(
        "remote capability owned quic packet plane certificate bytes: {peer} {}",
        capabilities
            .owned_quic_packet_plane_certificate_der
            .as_ref()
            .map_or(0, Vec::len)
    ));
    extend_remote_packet_endpoint_lines(lines, peer, capabilities);
    lines.push(format!(
        "remote capability advertised routes: {peer} {}",
        capabilities.advertised_routes.len()
    ));
}

fn extend_remote_packet_endpoint_lines(
    lines: &mut Vec<String>,
    peer: PeerId,
    capabilities: &ControlCapabilities,
) {
    lines.push(format!(
        "remote capability packet endpoint candidates: {peer} {}",
        capabilities.packet_endpoint_candidates.len()
    ));
    lines.push(format!(
        "remote capability owned quic packet endpoint candidates: {peer} {}",
        capabilities.owned_quic_packet_endpoint_candidates.len()
    ));
    for endpoint in &capabilities.packet_endpoint_candidates {
        lines.push(format!(
            "remote capability packet endpoint candidate: {peer} {endpoint}"
        ));
    }
    for endpoint in &capabilities.owned_quic_packet_endpoint_candidates {
        lines.push(format!(
            "remote capability owned quic packet endpoint candidate: {peer} {endpoint}"
        ));
    }
}

fn sorted_configured_peers(forwarder: &Forwarder) -> Vec<PeerId> {
    let mut peers = forwarder.configured_overlay_peers().collect::<Vec<_>>();
    peers.sort_by_key(ToString::to_string);
    peers
}

#[derive(Clone, Copy)]
struct RuntimeStatusView<'a> {
    metrics: &'a RuntimeMetrics,
    queue: crate::queue::QueueStats,
    path_stats: crate::path::PathRuntimeStats,
    auto_relay: AutoRelaySnapshot,
    packet_plane: &'a PacketPlaneSnapshot,
    packet_plane_quic: &'a PacketPlaneQuicSnapshot,
    packet_plane_session_ttl: Duration,
    packet_plane_replay_windows_per_session: usize,
}

fn runtime_status_lines(view: RuntimeStatusView<'_>) -> Vec<String> {
    let mut lines = view
        .metrics
        .snapshot_with_paths(view.queue, view.path_stats)
        .lines();
    extend_auto_relay_summary_lines(&mut lines, view.auto_relay);
    lines.push(format!(
        "packet_plane_session_ttl_seconds {}",
        view.packet_plane_session_ttl.as_secs()
    ));
    lines.push(format!(
        "packet_plane_replay_windows_per_session {}",
        view.packet_plane_replay_windows_per_session
    ));
    lines.push(format!(
        "packet_plane_listeners {}",
        view.packet_plane.listeners.len()
    ));
    lines.push(format!(
        "packet_plane_sessions {}",
        view.packet_plane.sessions.len()
    ));
    extend_runtime_packet_plane_quic_summary_lines(&mut lines, view.packet_plane_quic);
    lines
}

#[derive(Clone, Copy)]
struct RuntimeStateView<'a> {
    forwarder: &'a Forwarder,
    paths: &'a PathSet,
    peer_capabilities: &'a PeerCapabilities,
    metrics: &'a RuntimeMetrics,
    queue: crate::queue::QueueStats,
    path_stats: crate::path::PathRuntimeStats,
    packet_in_flight: PacketInFlightStats,
    auto_relay: AutoRelaySnapshot,
    relay_infrastructure: &'a RelayInfrastructureSnapshot,
    packet_plane: &'a PacketPlaneSnapshot,
    packet_plane_quic: &'a PacketPlaneQuicSnapshot,
    packet_plane_session_ttl: Duration,
    packet_plane_replay_windows_per_session: usize,
}

fn runtime_state_lines(view: &RuntimeStateView<'_>) -> Vec<String> {
    let snapshot = view
        .metrics
        .snapshot_with_paths(view.queue, view.path_stats);
    let mut peers = view
        .forwarder
        .configured_overlay_peers()
        .collect::<Vec<_>>();
    peers.sort_by_key(ToString::to_string);

    let mut lines = runtime_state_summary_lines(RuntimeStateSummaryView {
        snapshot: &snapshot,
        configured_peers: peers.len(),
        validated_peers: view.peer_capabilities.len(),
        replay_windows: view.forwarder.replay_window_count(),
        packet_in_flight: view.packet_in_flight,
        auto_relay: view.auto_relay,
        relay_infrastructure: view.relay_infrastructure,
        packet_plane: view.packet_plane,
        packet_plane_quic: view.packet_plane_quic,
        packet_plane_session_ttl: view.packet_plane_session_ttl,
        packet_plane_replay_windows_per_session: view.packet_plane_replay_windows_per_session,
    });

    let local_mtu = u16::try_from(view.forwarder.mtu()).unwrap_or(u16::MAX);
    for peer in peers {
        extend_runtime_peer_state_lines(
            &mut lines,
            view.forwarder,
            view.paths,
            view.peer_capabilities,
            peer,
            local_mtu,
        );
    }

    lines
}

#[derive(Clone, Copy)]
struct RuntimeStateSummaryView<'a> {
    snapshot: &'a RuntimeSnapshot,
    configured_peers: usize,
    validated_peers: usize,
    replay_windows: usize,
    packet_in_flight: PacketInFlightStats,
    auto_relay: AutoRelaySnapshot,
    relay_infrastructure: &'a RelayInfrastructureSnapshot,
    packet_plane: &'a PacketPlaneSnapshot,
    packet_plane_quic: &'a PacketPlaneQuicSnapshot,
    packet_plane_session_ttl: Duration,
    packet_plane_replay_windows_per_session: usize,
}

fn runtime_state_summary_lines(view: RuntimeStateSummaryView<'_>) -> Vec<String> {
    let snapshot = view.snapshot;
    let packet_in_flight = view.packet_in_flight;
    let mut lines = vec![
        "daemon state: running".to_owned(),
        format!("configured peers: {}", view.configured_peers),
        format!("validated peers: {}", view.validated_peers),
        format!("replay_windows {}", view.replay_windows),
        format!(
            "packet_plane_session_ttl_seconds {}",
            view.packet_plane_session_ttl.as_secs()
        ),
        format!(
            "packet_plane_replay_windows_per_session {}",
            view.packet_plane_replay_windows_per_session
        ),
        format!(
            "outbound_stream_fallback_packets {}",
            snapshot.outbound_stream_fallback_packets
        ),
        format!(
            "outbound_quic_datagram_packets {}",
            snapshot.outbound_quic_datagram_packets
        ),
        format!(
            "outbound_quic_datagram_unavailable_packets {}",
            snapshot.outbound_quic_datagram_unavailable_packets
        ),
        format!(
            "packet_stream_fallback_in_flight {}",
            packet_in_flight.packets
        ),
        format!(
            "packet_stream_fallback_in_flight_peers {}",
            packet_in_flight.peers
        ),
        format!(
            "packet_stream_fallback_in_flight_shards {}",
            packet_in_flight.shards
        ),
        format!(
            "packet_stream_fallback_limit_per_peer {}",
            packet_in_flight.limit_per_peer
        ),
    ];
    extend_runtime_discovery_summary_lines(&mut lines, snapshot);
    extend_auto_relay_summary_lines(&mut lines, view.auto_relay);
    extend_runtime_path_summary_lines(&mut lines, snapshot);
    extend_runtime_relay_infrastructure_lines(&mut lines, view.relay_infrastructure);
    extend_runtime_packet_plane_summary_lines(&mut lines, view.packet_plane);
    extend_runtime_packet_plane_quic_summary_lines(&mut lines, view.packet_plane_quic);
    lines
}

fn extend_auto_relay_summary_lines(lines: &mut Vec<String>, snapshot: AutoRelaySnapshot) {
    lines.extend([
        format!("auto_relay_policy_candidates {}", snapshot.max_candidates),
        format!(
            "auto_relay_policy_reservations {}",
            snapshot.max_reservations
        ),
        format!(
            "auto_relay_policy_retry_seconds {}",
            snapshot.retry_interval_seconds
        ),
        format!(
            "auto_relay_private_reachability {}",
            snapshot.private_reachability
        ),
        format!("auto_relay_current_candidates {}", snapshot.candidates),
        format!("auto_relay_active_reservations {}", snapshot.reservations),
        format!("auto_relay_pending_retries {}", snapshot.pending_retries),
    ]);
}

fn extend_runtime_discovery_summary_lines(lines: &mut Vec<String>, snapshot: &RuntimeSnapshot) {
    lines.extend([
        format!(
            "path_promotions_to_direct {}",
            snapshot.path_promotions_to_direct
        ),
        format!(
            "path_fallbacks_to_relay {}",
            snapshot.path_fallbacks_to_relay
        ),
        format!(
            "outbound_path_mtu_updates {}",
            snapshot.outbound_path_mtu_updates
        ),
        format!(
            "outbound_path_mtu_probe_confirmations {}",
            snapshot.outbound_path_mtu_probe_confirmations
        ),
        format!("dcutr_successes {}", snapshot.dcutr_successes),
        format!("dcutr_failures {}", snapshot.dcutr_failures),
        format!(
            "observed_packet_plane_external_addresses {}",
            snapshot.observed_packet_plane_external_addresses
        ),
        format!(
            "observed_packet_plane_external_addresses_rejected {}",
            snapshot.observed_packet_plane_external_addresses_rejected
        ),
        format!(
            "observed_packet_plane_udp_endpoint_candidates {}",
            snapshot.observed_packet_plane_udp_endpoint_candidates
        ),
        format!(
            "observed_packet_plane_quic_endpoint_candidates {}",
            snapshot.observed_packet_plane_quic_endpoint_candidates
        ),
        format!(
            "autonat_probes_scheduled {}",
            snapshot.autonat_probes_scheduled
        ),
        format!("autonat_status_unknown {}", snapshot.autonat_status_unknown),
        format!("autonat_status_public {}", snapshot.autonat_status_public),
        format!("autonat_status_private {}", snapshot.autonat_status_private),
        format!("auto_relay_candidates {}", snapshot.auto_relay_candidates),
        format!(
            "auto_relay_reservation_attempts {}",
            snapshot.auto_relay_reservation_attempts
        ),
        format!(
            "auto_relay_reservation_failures {}",
            snapshot.auto_relay_reservation_failures
        ),
        format!(
            "autonat_status_changes_to_public {}",
            snapshot.autonat_status_changes_to_public
        ),
        format!(
            "autonat_status_changes_to_private {}",
            snapshot.autonat_status_changes_to_private
        ),
        format!(
            "outbound_path_probes_sent {}",
            snapshot.outbound_path_probes_sent
        ),
        format!(
            "outbound_path_probe_acks_sent {}",
            snapshot.outbound_path_probe_acks_sent
        ),
        format!(
            "outbound_path_probe_failures {}",
            snapshot.outbound_path_probe_failures
        ),
        format!(
            "inbound_path_probes_accepted {}",
            snapshot.inbound_path_probes_accepted
        ),
        format!(
            "outbound_queue_blocked_no_supported_path_events {}",
            snapshot.outbound_queue_blocked_no_supported_path_events
        ),
        format!(
            "outbound_queue_blocked_packet_window_events {}",
            snapshot.outbound_queue_blocked_packet_window_events
        ),
    ]);
}

fn extend_runtime_path_summary_lines(lines: &mut Vec<String>, snapshot: &RuntimeSnapshot) {
    lines.extend([
        format!(
            "peers_with_supported_path {}",
            snapshot.path.peers_with_supported_path
        ),
        format!(
            "peers_without_supported_path {}",
            snapshot.path.peers_without_supported_path
        ),
        format!(
            "healthy_direct_udp_datagram_paths {}",
            snapshot.path.healthy_direct_udp_datagram_paths
        ),
        format!(
            "healthy_direct_quic_datagram_paths {}",
            snapshot.path.healthy_direct_quic_datagram_paths
        ),
        format!(
            "healthy_direct_quic_stream_paths {}",
            snapshot.path.healthy_direct_quic_stream_paths
        ),
        format!(
            "healthy_direct_tcp_stream_paths {}",
            snapshot.path.healthy_direct_tcp_stream_paths
        ),
        format!("healthy_relay_paths {}", snapshot.path.healthy_relay_paths),
    ]);
}

fn extend_runtime_relay_infrastructure_lines(
    lines: &mut Vec<String>,
    relay_infrastructure: &RelayInfrastructureSnapshot,
) {
    lines.push(format!(
        "relay_infrastructure_peers {}",
        relay_infrastructure.peers.len()
    ));
    for peer in &relay_infrastructure.peers {
        lines.push(format!(
            "relay_infrastructure_peer {} address {} connected {}",
            peer.peer, peer.address, peer.connected
        ));
    }
}

fn extend_runtime_packet_plane_summary_lines(
    lines: &mut Vec<String>,
    packet_plane: &PacketPlaneSnapshot,
) {
    lines.push(format!(
        "packet_plane_listeners {}",
        packet_plane.listeners.len()
    ));
    lines.push(format!(
        "packet_plane_sessions {}",
        packet_plane.sessions.len()
    ));
    for listener in &packet_plane.listeners {
        lines.push(format!("packet_plane_listener {listener}"));
    }
    for session in &packet_plane.sessions {
        lines.push(format!(
            "packet_plane_session {} endpoint {} mtu {} role {} local_session {} remote_session {}",
            session.peer,
            session.endpoint,
            session.mtu,
            packet_plane_session_role_name(session.role),
            session.local_session_id,
            session.remote_session_id
        ));
    }
}

fn extend_runtime_packet_plane_quic_summary_lines(
    lines: &mut Vec<String>,
    packet_plane_quic: &PacketPlaneQuicSnapshot,
) {
    lines.push(format!(
        "packet_plane_quic_listeners {}",
        usize::from(packet_plane_quic.listener.is_some())
    ));
    lines.push(format!(
        "packet_plane_quic_sessions {}",
        packet_plane_quic.sessions.len()
    ));
    lines.push(format!(
        "packet_plane_quic_certificate_bytes {}",
        packet_plane_quic
            .certificate_der
            .as_ref()
            .map_or(0, Vec::len)
    ));
    if let Some(listener) = packet_plane_quic.listener {
        lines.push(format!("packet_plane_quic_listener {listener}"));
    }
    for session in &packet_plane_quic.sessions {
        lines.push(format!(
            "packet_plane_quic_session {} endpoint {} mtu {} role {} local_session {} remote_session {}",
            session.peer,
            session.endpoint,
            session.mtu,
            packet_plane_session_role_name(session.role),
            session.local_session_id,
            session.remote_session_id
        ));
    }
}

const fn packet_plane_session_role_name(role: PacketPlaneSessionRole) -> &'static str {
    match role {
        PacketPlaneSessionRole::Initiator => "initiator",
        PacketPlaneSessionRole::Responder => "responder",
    }
}

fn extend_runtime_peer_state_lines(
    lines: &mut Vec<String>,
    forwarder: &Forwarder,
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
    peer: PeerId,
    local_mtu: u16,
) {
    let transport = forwarder
        .transport_peer_for_overlay(peer)
        .map_or_else(|| "none".to_owned(), |peer| peer.to_string());
    let support = packet_transport_support(peer_capabilities, peer);
    let selected_path = paths.best_supported_for(peer, support);
    let candidates = paths.candidates_for(peer).collect::<Vec<_>>();
    let healthy_paths = candidates
        .iter()
        .filter(|candidate| candidate.healthy)
        .count();
    let direct_paths = candidates
        .iter()
        .filter(|candidate| candidate.healthy && !candidate.relay)
        .count();
    let relay_paths = candidates
        .iter()
        .filter(|candidate| candidate.healthy && candidate.relay)
        .count();

    lines.push(format!(
        "peer state: {peer} transport {transport} validated {} effective_mtu {} quic_datagrams {} native_quic_datagrams {} owned_udp_packet_plane {} owned_quic_packet_plane {} selected_path {} selected_path_score {} selected_path_mtu {} selected_path_rtt_ms {} healthy_paths {healthy_paths} direct_paths {direct_paths} relay_paths {relay_paths}",
        peer_capabilities.contains(peer),
        peer_capabilities.effective_mtu_for(peer, local_mtu),
        support.quic_datagrams,
        peer_capabilities.supports_native_quic_datagrams_for(peer),
        peer_capabilities.supports_owned_udp_packet_plane_for(peer),
        peer_capabilities.supports_owned_quic_packet_plane_for(peer),
        selected_path.map_or("none", |path| path.kind.wire_name()),
        selected_path.map_or_else(|| "none".to_owned(), |path| path.score().to_string()),
        selected_path.map_or_else(
            || "none".to_owned(),
            |path| path
                .effective_mtu(peer_capabilities.effective_mtu_for(peer, local_mtu))
                .to_string()
        ),
        selected_path.map_or_else(|| "unknown".to_owned(), path_rtt_value),
    ));

    if let Some(capabilities) = peer_capabilities.get(peer) {
        lines.push(format!(
            "peer capability state: {peer} preferred_path {} advertised_routes {}",
            capabilities.preferred_path,
            capabilities.advertised_routes.len()
        ));
    }

    for candidate in candidates {
        let peer_mtu = peer_capabilities.effective_mtu_for(peer, local_mtu);
        lines.push(format!(
            "peer path state: {peer} {} healthy {} relay {} established_connections {} score {} estimated_mtu {} effective_mtu {} observed_rtt_ms {}",
            candidate.kind.wire_name(),
            candidate.healthy,
            candidate.relay,
            candidate.established_connections,
            candidate.score(),
            candidate
                .estimated_mtu
                .map_or_else(|| "unknown".to_owned(), |mtu| mtu.to_string()),
            candidate.effective_mtu(peer_mtu),
            path_rtt_value(candidate)
        ));
    }
}

fn handle_redial_tick(
    node: &mut P2pNode,
    discovered_peer_addresses: &mut DiscoveredPeerAddresses,
    paths: &PathSet,
    relay_readiness: &RelayReadiness,
    configured_relay_reservation_retries: &mut ConfiguredRelayReservationRetries,
    metrics: &RuntimeMetrics,
) {
    expire_discovered_peer_addresses(discovered_peer_addresses, metrics);
    retry_configured_relay_reservations(
        &mut node.swarm,
        &node.relay_reservation_addresses,
        relay_readiness,
        configured_relay_reservation_retries,
    );
    let discovered_addresses = discovered_peer_addresses.as_vec();
    redial_known_addresses(
        &mut node.swarm,
        &node.bootstrap_peer_addresses,
        &node.relay_peer_addresses,
        &node.configured_peer_addresses,
        &discovered_addresses,
        paths,
        metrics,
        |relay| relay_readiness.relay_ready(relay),
    );
}

fn retry_configured_relay_reservations(
    swarm: &mut Swarm<Behaviour>,
    relay_reservation_addresses: &[Multiaddr],
    relay_readiness: &RelayReadiness,
    retries: &mut ConfiguredRelayReservationRetries,
) {
    let local_peer = *swarm.local_peer_id();
    let now = Instant::now();
    for address in relay_reservation_addresses {
        let Some(relay) = relayed_address_relay_peer(address) else {
            continue;
        };
        if relay_readiness.relay_ready(relay) || !retries.should_retry(address, now) {
            continue;
        }

        let listen_address = peer_dial_address(local_peer, address.clone());
        log_runtime_event(
            LogLevel::Info,
            "configured_relay_reservation_retry",
            &[
                ("relay", &relay.to_string()),
                ("address", &listen_address.to_string()),
            ],
        );
        if let Err(error) = swarm.listen_on(listen_address.clone()) {
            log_runtime_event(
                LogLevel::Warn,
                "configured_relay_reservation_retry_failed",
                &[
                    ("relay", &relay.to_string()),
                    ("address", &listen_address.to_string()),
                    ("error", &error.to_string()),
                ],
            );
        }
    }
}

fn log_startup_status(startup: crate::runtime::p2p::StartupStatus) {
    if startup.mdns_enabled {
        log_runtime_event(LogLevel::Info, "mdns_enabled", &[]);
    }
    if startup.external_addresses_configured > 0 {
        log_runtime_event(
            LogLevel::Info,
            "external_addresses_configured",
            &[("count", &startup.external_addresses_configured.to_string())],
        );
    }
    if startup.dcutr_enabled {
        log_runtime_event(LogLevel::Info, "dcutr_enabled", &[]);
    }
    if startup.autonat_enabled {
        log_runtime_event(LogLevel::Info, "autonat_enabled", &[]);
    }
    if startup.autonat_servers_registered > 0 {
        log_runtime_event(
            LogLevel::Info,
            "autonat_servers_registered",
            &[("count", &startup.autonat_servers_registered.to_string())],
        );
    }
    if startup.kademlia.bootstrap_started {
        log_runtime_event(LogLevel::Info, "kademlia_bootstrap_started", &[]);
    }
    if startup.kademlia.rendezvous_advertise_started {
        log_runtime_event(LogLevel::Info, "kademlia_provider_advertise_started", &[]);
    }
    if startup.kademlia.rendezvous_lookup_started {
        log_runtime_event(LogLevel::Info, "kademlia_provider_lookup_started", &[]);
    }
    if startup.relay_reservations_started > 0 {
        log_runtime_event(
            LogLevel::Info,
            "relay_reservation_listeners_started",
            &[("count", &startup.relay_reservations_started.to_string())],
        );
    }
    if startup.relay_server_enabled {
        log_runtime_event(LogLevel::Info, "relay_server_enabled", &[]);
    }
}

fn log_packet_plane_status(packet_plane: PacketPlaneSnapshot) {
    if packet_plane.listeners.is_empty() {
        return;
    }
    log_runtime_event(
        LogLevel::Info,
        "packet_plane_listening",
        &[("count", &packet_plane.listeners.len().to_string())],
    );
    for listener in packet_plane.listeners {
        log_runtime_event(
            LogLevel::Info,
            "packet_plane_listener",
            &[("address", &listener.to_string())],
        );
    }
}

fn log_packet_plane_quic_status(packet_plane_quic: &PacketPlaneQuicSnapshot) {
    let Some(listener) = packet_plane_quic.listener else {
        return;
    };
    log_runtime_event(
        LogLevel::Info,
        "packet_plane_quic_listening",
        &[
            ("address", &listener.to_string()),
            (
                "certificate_bytes",
                &packet_plane_quic
                    .certificate_der
                    .as_ref()
                    .map_or(0, Vec::len)
                    .to_string(),
            ),
        ],
    );
}

fn current_packet_plane_quic_snapshot(
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
) -> PacketPlaneQuicSnapshot {
    packet_plane_quic.map_or_else(
        PacketPlaneQuicRuntime::disabled_snapshot,
        PacketPlaneQuicRuntime::snapshot,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

fn log_runtime_event(level: LogLevel, event: &str, fields: &[(&str, &str)]) {
    eprintln!("{}", runtime_log_line(level, event, fields));
}

fn runtime_log_line(level: LogLevel, event: &str, fields: &[(&str, &str)]) -> String {
    let mut line = format!("level={} event={}", level.as_str(), event);
    for (key, value) in fields {
        line.push(' ');
        line.push_str(key);
        line.push('=');
        push_log_value(&mut line, value);
    }
    line
}

fn push_log_value(line: &mut String, value: &str) {
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':' | b'/')
    }) {
        line.push_str(value);
        return;
    }

    line.push('"');
    for character in value.chars() {
        match character {
            '"' => line.push_str("\\\""),
            '\\' => line.push_str("\\\\"),
            '\n' => line.push_str("\\n"),
            '\r' => line.push_str("\\r"),
            '\t' => line.push_str("\\t"),
            other => line.push(other),
        }
    }
    line.push('"');
}

fn queue_expiry_interval(max_packet_age: Duration) -> Duration {
    max_packet_age.clamp(MIN_QUEUE_EXPIRY_INTERVAL, REDIAL_INTERVAL)
}

fn expire_outbound_queue(queues: &mut PeerQueues, metrics: &RuntimeMetrics) {
    let expired_before = queues.total_stats().expired_packets;
    queues.drop_expired(std::time::Instant::now());
    let expired_after = queues.total_stats().expired_packets;
    metrics.record_outbound_queue_expired(expired_after.saturating_sub(expired_before));
}

fn expire_discovered_peer_addresses(
    discovered_peer_addresses: &mut DiscoveredPeerAddresses,
    metrics: &RuntimeMetrics,
) {
    let expired = discovered_peer_addresses.drop_expired(Instant::now(), DISCOVERED_ADDRESS_TTL);
    metrics.record_discovered_address_expired(expired);
}

struct KademliaRefreshContext<'a> {
    advertise_key: &'a kad::RecordKey,
    lookup_keys: &'a [kad::RecordKey],
    membership_record_advertise_key: Option<&'a kad::RecordKey>,
    membership_record_lookup_keys: &'a [kad::RecordKey],
    network_name: &'a str,
    membership_tag: Option<&'a str>,
    forwarder: &'a Forwarder,
    identity: &'a NodeIdentity,
    advertise_provider: bool,
    auto_relay: &'a AutoRelayState,
    metrics: &'a RuntimeMetrics,
}

fn refresh_kademlia_rendezvous(swarm: &mut Swarm<Behaviour>, context: &KademliaRefreshContext<'_>) {
    if context.advertise_provider {
        match swarm
            .behaviour_mut()
            .kad
            .start_providing(context.advertise_key.clone())
        {
            Ok(_) => context.metrics.record_kademlia_provider_advertisement(),
            Err(error) => {
                context
                    .metrics
                    .record_kademlia_provider_advertisement_failure();
                log_runtime_event(
                    LogLevel::Warn,
                    "kademlia_provider_advertisement_failed",
                    &[("error", &format!("{error:?}"))],
                );
            }
        }
    }

    for lookup_key in context.lookup_keys {
        swarm.behaviour_mut().kad.get_providers(lookup_key.clone());
        context.metrics.record_kademlia_provider_lookup();
    }

    if context.advertise_provider
        && let Some(record_key) = context.membership_record_advertise_key
    {
        publish_kademlia_membership_records(
            swarm,
            record_key,
            context.network_name,
            context.membership_tag,
            context.forwarder,
            context.metrics,
        );
    }

    for lookup_key in context.membership_record_lookup_keys {
        swarm.behaviour_mut().kad.get_record(lookup_key.clone());
        context.metrics.record_kademlia_membership_record_lookup();
    }

    publish_kademlia_peer_address_record(
        swarm,
        context.network_name,
        context.membership_tag,
        context.identity,
    );

    for peer in context.forwarder.configured_transport_peers() {
        if peer != *swarm.local_peer_id() {
            swarm
                .behaviour_mut()
                .kad
                .get_record(crate::runtime::p2p::kademlia_peer_addresses_key(
                    context.network_name,
                    context.membership_tag,
                    peer,
                ));
            swarm.behaviour_mut().kad.get_closest_peers(peer);
        }
    }

    if context.auto_relay.should_discover_candidates() {
        query_auto_relay_infrastructure(swarm, context.metrics, "kademlia_refresh");
    }

    match swarm.behaviour_mut().kad.bootstrap() {
        Ok(_) => context.metrics.record_kademlia_bootstrap_refresh(),
        Err(error) => {
            context.metrics.record_kademlia_bootstrap_failure();
            log_runtime_event(
                LogLevel::Warn,
                "kademlia_bootstrap_failed",
                &[("error", &format!("{error:?}"))],
            );
        }
    }
}

fn publish_kademlia_membership_records(
    swarm: &mut Swarm<Behaviour>,
    record_key: &kad::RecordKey,
    network_name: &str,
    membership_tag: Option<&str>,
    forwarder: &Forwarder,
    metrics: &RuntimeMetrics,
) {
    let records = advertised_member_records(forwarder);
    if records.is_empty() {
        return;
    }

    let Ok(value) = encode_kademlia_membership_records(network_name, membership_tag, records)
    else {
        metrics.record_kademlia_membership_record_publication_failure();
        log_runtime_event(
            LogLevel::Warn,
            "kademlia_membership_record_publication_failed",
            &[("reason", "encode_failed")],
        );
        return;
    };
    let record = kad::Record {
        key: record_key.clone(),
        value,
        publisher: Some(*swarm.local_peer_id()),
        expires: None,
    };

    match swarm
        .behaviour_mut()
        .kad
        .put_record(record, kad::Quorum::One)
    {
        Ok(_) => metrics.record_kademlia_membership_record_publication(),
        Err(error) => {
            metrics.record_kademlia_membership_record_publication_failure();
            log_runtime_event(
                LogLevel::Warn,
                "kademlia_membership_record_publication_failed",
                &[("error", &format!("{error:?}"))],
            );
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct KademliaPeerAddressRecordPayload {
    version: u8,
    network_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    membership_tag: Option<String>,
    peer_id: String,
    public_key_protobuf: Vec<u8>,
    sequence: u64,
    expires_at_unix_seconds: u64,
    addresses: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct KademliaPeerAddressRecord {
    payload: KademliaPeerAddressRecordPayload,
    signature: Vec<u8>,
}

#[derive(Debug)]
enum KademliaPeerAddressRecordError {
    NoAddresses,
    TooLarge,
    Decode,
    Encode,
    UnsupportedVersion,
    WrongNetwork,
    WrongMembershipScope,
    WrongPeer,
    InvalidPeerId,
    InvalidPublicKey,
    InvalidSignature,
    Expired,
    TooManyAddresses,
}

impl std::fmt::Display for KademliaPeerAddressRecordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAddresses => formatter.write_str("no_addresses"),
            Self::TooLarge => formatter.write_str("too_large"),
            Self::Decode => formatter.write_str("decode_failed"),
            Self::Encode => formatter.write_str("encode_failed"),
            Self::UnsupportedVersion => formatter.write_str("unsupported_version"),
            Self::WrongNetwork => formatter.write_str("wrong_network"),
            Self::WrongMembershipScope => formatter.write_str("wrong_membership_scope"),
            Self::WrongPeer => formatter.write_str("wrong_peer"),
            Self::InvalidPeerId => formatter.write_str("invalid_peer_id"),
            Self::InvalidPublicKey => formatter.write_str("invalid_public_key"),
            Self::InvalidSignature => formatter.write_str("invalid_signature"),
            Self::Expired => formatter.write_str("expired"),
            Self::TooManyAddresses => formatter.write_str("too_many_addresses"),
        }
    }
}

fn publish_kademlia_peer_address_record(
    swarm: &mut Swarm<Behaviour>,
    network_name: &str,
    membership_tag: Option<&str>,
    identity: &NodeIdentity,
) {
    let addresses = local_advertisable_addresses(swarm);
    let key = crate::runtime::p2p::kademlia_peer_addresses_key(
        network_name,
        membership_tag,
        *swarm.local_peer_id(),
    );
    let value = match encode_kademlia_peer_address_record(
        network_name,
        membership_tag,
        identity,
        addresses,
        current_unix_seconds_lossy(),
    ) {
        Ok(value) => value,
        Err(error) => {
            if !matches!(error, KademliaPeerAddressRecordError::NoAddresses) {
                log_runtime_event(
                    LogLevel::Warn,
                    "kademlia_peer_address_record_publication_failed",
                    &[("reason", &error.to_string())],
                );
            }
            return;
        }
    };
    let record = kad::Record {
        key,
        value,
        publisher: Some(*swarm.local_peer_id()),
        expires: None,
    };

    if let Err(error) = swarm
        .behaviour_mut()
        .kad
        .put_record(record, kad::Quorum::One)
    {
        log_runtime_event(
            LogLevel::Warn,
            "kademlia_peer_address_record_publication_failed",
            &[("error", &format!("{error:?}"))],
        );
    }
}

fn publish_kademlia_peer_address_record_for_capabilities(
    swarm: &mut Swarm<Behaviour>,
    discovery: &DiscoveryConfig,
    local_capabilities: &ControlCapabilities,
    identity: &NodeIdentity,
) {
    if !discovery.kademlia {
        return;
    }

    publish_kademlia_peer_address_record(
        swarm,
        &local_capabilities.network_name,
        local_capabilities.membership_tag.as_deref(),
        identity,
    );
}

fn local_advertisable_addresses(swarm: &Swarm<Behaviour>) -> Vec<Multiaddr> {
    let mut addresses = Vec::new();
    let confirmed_external_addresses = swarm
        .external_addresses()
        .cloned()
        .collect::<HashSet<Multiaddr>>();
    for address in swarm.listeners().chain(swarm.external_addresses()) {
        if addresses.len() >= MAX_KADEMLIA_PEER_ADDRESS_RECORD_ADDRESSES {
            break;
        }
        if !kademlia_peer_address_is_confirmed_for_publication(
            address,
            &confirmed_external_addresses,
        ) {
            continue;
        }
        if kademlia_peer_address_is_advertisable(address) && !addresses.contains(address) {
            addresses.push(address.clone());
        }
    }
    addresses
}

fn kademlia_peer_address_is_confirmed_for_publication(
    address: &Multiaddr,
    confirmed_external_addresses: &HashSet<Multiaddr>,
) -> bool {
    relayed_address_relay_peer(address).is_some() || confirmed_external_addresses.contains(address)
}

fn kademlia_peer_address_is_advertisable(address: &Multiaddr) -> bool {
    if address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
    {
        return supports_relayed_peer_dial_transport(address);
    }

    address.iter().any(|protocol| match protocol {
        Protocol::Dns(_) | Protocol::Dns4(_) | Protocol::Dns6(_) | Protocol::Dnsaddr(_) => true,
        Protocol::Ip4(address) => ipv4_address_is_advertisable(address),
        Protocol::Ip6(address) => ipv6_address_is_advertisable(address),
        _ => false,
    })
}

fn ipv4_address_is_advertisable(address: Ipv4Addr) -> bool {
    let [first, second, _, _] = address.octets();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_documentation()
        || address.is_multicast()
        || (first == 10 && second == 42)
        || (first == 100 && (64..=127).contains(&second)))
}

fn ipv6_address_is_advertisable(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    let first = segments[0];
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first == 0x2001 && segments[1] == 0x0db8))
}

fn encode_kademlia_peer_address_record(
    network_name: &str,
    membership_tag: Option<&str>,
    identity: &NodeIdentity,
    addresses: Vec<Multiaddr>,
    now_unix_seconds: u64,
) -> Result<Vec<u8>, KademliaPeerAddressRecordError> {
    if addresses.is_empty() {
        return Err(KademliaPeerAddressRecordError::NoAddresses);
    }
    let payload = KademliaPeerAddressRecordPayload {
        version: 1,
        network_name: network_name.to_owned(),
        membership_tag: membership_tag.map(str::to_owned),
        peer_id: identity.peer_id.clone(),
        public_key_protobuf: identity
            .public_key_protobuf()
            .map_err(|_| KademliaPeerAddressRecordError::InvalidPublicKey)?,
        sequence: now_unix_seconds,
        expires_at_unix_seconds: now_unix_seconds + KADEMLIA_PEER_ADDRESS_RECORD_TTL,
        addresses: addresses
            .into_iter()
            .map(|address| address.to_string())
            .collect(),
    };
    let payload_bytes =
        serde_json::to_vec(&payload).map_err(|_| KademliaPeerAddressRecordError::Encode)?;
    let signature = identity
        .sign(&payload_bytes)
        .map_err(|_| KademliaPeerAddressRecordError::InvalidSignature)?;
    serde_json::to_vec(&KademliaPeerAddressRecord { payload, signature })
        .map_err(|_| KademliaPeerAddressRecordError::Encode)
}

fn learn_peer_addresses_from_kademlia_value(
    forwarder: &Forwarder,
    expected_network_name: &str,
    current_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
    value: &[u8],
) -> Result<(Libp2pPeerId, Vec<Multiaddr>), KademliaPeerAddressRecordError> {
    if value.len() > MAX_KADEMLIA_PEER_ADDRESS_RECORD_BYTES {
        return Err(KademliaPeerAddressRecordError::TooLarge);
    }
    let record: KademliaPeerAddressRecord =
        serde_json::from_slice(value).map_err(|_| KademliaPeerAddressRecordError::Decode)?;
    let payload_bytes =
        serde_json::to_vec(&record.payload).map_err(|_| KademliaPeerAddressRecordError::Encode)?;
    if record.payload.version != 1 {
        return Err(KademliaPeerAddressRecordError::UnsupportedVersion);
    }
    if record.payload.network_name != expected_network_name {
        return Err(KademliaPeerAddressRecordError::WrongNetwork);
    }
    if !membership_tag_allowed(
        record.payload.membership_tag.as_deref(),
        current_membership_tag,
        previous_membership_tags,
    ) {
        return Err(KademliaPeerAddressRecordError::WrongMembershipScope);
    }
    if record
        .payload
        .expires_at_unix_seconds
        .saturating_add(KADEMLIA_PEER_ADDRESS_RECORD_STALE_GRACE)
        < current_unix_seconds_lossy()
    {
        return Err(KademliaPeerAddressRecordError::Expired);
    }
    if record.payload.addresses.len() > MAX_KADEMLIA_PEER_ADDRESS_RECORD_ADDRESSES {
        return Err(KademliaPeerAddressRecordError::TooManyAddresses);
    }
    let peer = record
        .payload
        .peer_id
        .parse::<Libp2pPeerId>()
        .map_err(|_| KademliaPeerAddressRecordError::InvalidPeerId)?;
    if !forwarder.is_configured_transport_peer(peer) {
        return Err(KademliaPeerAddressRecordError::WrongPeer);
    }
    let public_key =
        libp2p::identity::PublicKey::try_decode_protobuf(&record.payload.public_key_protobuf)
            .map_err(|_| KademliaPeerAddressRecordError::InvalidPublicKey)?;
    if Libp2pPeerId::from_public_key(&public_key) != peer {
        return Err(KademliaPeerAddressRecordError::InvalidPublicKey);
    }
    if !public_key.verify(&payload_bytes, &record.signature) {
        return Err(KademliaPeerAddressRecordError::InvalidSignature);
    }
    let addresses = record
        .payload
        .addresses
        .iter()
        .filter_map(|address| address.parse::<Multiaddr>().ok())
        .filter(|address| address_targets_peer(peer, address))
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(KademliaPeerAddressRecordError::NoAddresses);
    }
    Ok((peer, addresses))
}

fn kademlia_query_result_key(result: &kad::QueryResult) -> Option<&kad::RecordKey> {
    match result {
        kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(peer_record))) => {
            Some(&peer_record.record.key)
        }
        kad::QueryResult::GetRecord(Err(kad::GetRecordError::NotFound { key, .. }))
        | kad::QueryResult::GetRecord(Err(kad::GetRecordError::QuorumFailed { key, .. }))
        | kad::QueryResult::GetRecord(Err(kad::GetRecordError::Timeout { key, .. })) => Some(key),
        kad::QueryResult::PutRecord(Ok(kad::PutRecordOk { key }))
        | kad::QueryResult::PutRecord(Err(kad::PutRecordError::QuorumFailed { key, .. }))
        | kad::QueryResult::PutRecord(Err(kad::PutRecordError::Timeout { key, .. })) => Some(key),
        _ => None,
    }
}

fn kademlia_key_is_peer_address_record(key: &kad::RecordKey) -> bool {
    std::str::from_utf8(key.as_ref()).is_ok_and(|key| key.contains("/peer-addresses/"))
}

fn kademlia_lookup_keys(
    network_name: &str,
    current_key: Option<&kad::RecordKey>,
    previous_membership_tags: &[String],
) -> Vec<kad::RecordKey> {
    let Some(current_key) = current_key else {
        return Vec::new();
    };
    let mut keys = vec![current_key.clone()];
    for tag in previous_membership_tags {
        let key = crate::runtime::p2p::kademlia_rendezvous_key(network_name, Some(tag));
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

fn kademlia_membership_record_lookup_keys(
    network_name: &str,
    current_membership_tag: Option<&str>,
    current_key: Option<&kad::RecordKey>,
    previous_membership_tags: &[String],
) -> Vec<kad::RecordKey> {
    let Some(current_key) = current_key else {
        return Vec::new();
    };
    let mut keys = vec![current_key.clone()];
    for tag in previous_membership_tags {
        let key = crate::runtime::p2p::kademlia_membership_records_key(network_name, Some(tag));
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    if current_membership_tag.is_none() {
        return keys;
    }
    let untagged_key = crate::runtime::p2p::kademlia_membership_records_key(network_name, None);
    if !keys.contains(&untagged_key) {
        keys.push(untagged_key);
    }
    keys
}

fn redial_known_addresses(
    swarm: &mut Swarm<Behaviour>,
    bootstrap_addresses: &[(Libp2pPeerId, Multiaddr)],
    relay_addresses: &[(Libp2pPeerId, Multiaddr)],
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    paths: &PathSet,
    metrics: &RuntimeMetrics,
    relay_ready: impl FnMut(Libp2pPeerId) -> bool,
) {
    let local_peer = *swarm.local_peer_id();
    let targets = pending_redial_targets(
        local_peer,
        bootstrap_addresses,
        relay_addresses,
        configured_peer_addresses,
        discovered_peer_addresses,
        |peer| redial_connection_state(paths, *peer, swarm.is_connected(peer)),
        relay_ready,
    );

    for _ in 0..targets.skipped_connected {
        metrics.record_redial_skipped_connected();
    }

    for (peer, address) in targets.addresses {
        metrics.record_redial_attempt();
        let dial_address = peer_dial_address(peer, address);
        if let Err(error) = swarm.dial(dial_address.clone()) {
            metrics.record_redial_failure();
            log_runtime_event(
                LogLevel::Warn,
                "redial_failed",
                &[
                    ("peer", &peer.to_string()),
                    ("address", &dial_address.to_string()),
                    ("error", &error.to_string()),
                ],
            );
        }
    }
}

#[derive(Debug, Default)]
struct RelayReadiness {
    accepted_reservations: HashSet<Libp2pPeerId>,
    relayed_listen_addresses: HashSet<Libp2pPeerId>,
    attempted_ready_dials: HashSet<(Libp2pPeerId, Libp2pPeerId, Multiaddr)>,
}

#[derive(Debug, Default)]
struct ConfiguredRelayReservationRetries {
    last_attempts: HashMap<Multiaddr, Instant>,
}

impl ConfiguredRelayReservationRetries {
    fn from_startup_attempts(addresses: &[Multiaddr], now: Instant) -> Self {
        Self {
            last_attempts: addresses
                .iter()
                .cloned()
                .map(|address| (address, now))
                .collect(),
        }
    }

    fn should_retry(&mut self, address: &Multiaddr, now: Instant) -> bool {
        if self.last_attempts.get(address).is_some_and(|last_attempt| {
            now.saturating_duration_since(*last_attempt) < REDIAL_INTERVAL
        }) {
            return false;
        }
        self.last_attempts.insert(address.clone(), now);
        true
    }
}

#[derive(Debug)]
struct AutoRelayState {
    policy: AutoRelayConfig,
    reachability: AutoNatReachability,
    candidates: Vec<(Libp2pPeerId, Multiaddr)>,
    attempted_reservations: HashSet<(Libp2pPeerId, Multiaddr)>,
    pending_reservations: HashMap<Libp2pPeerId, PendingAutoRelayReservation>,
    accepted_reservation_peers: HashSet<Libp2pPeerId>,
    retry_after: HashMap<Libp2pPeerId, Instant>,
    reservation_failures: HashMap<Libp2pPeerId, u8>,
}

#[derive(Clone, Debug)]
struct PendingAutoRelayReservation {
    address: Multiaddr,
    expires_at: Instant,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AutoRelaySnapshot {
    max_candidates: usize,
    max_reservations: usize,
    retry_interval_seconds: u64,
    private_reachability: bool,
    candidates: usize,
    reservations: usize,
    pending_retries: usize,
}

impl Default for AutoRelayState {
    fn default() -> Self {
        Self::new(AutoRelayConfig::default())
    }
}

impl AutoRelayState {
    fn new(policy: AutoRelayConfig) -> Self {
        Self {
            policy,
            reachability: AutoNatReachability::Unknown,
            candidates: Vec::new(),
            attempted_reservations: HashSet::new(),
            pending_reservations: HashMap::new(),
            accepted_reservation_peers: HashSet::new(),
            retry_after: HashMap::new(),
            reservation_failures: HashMap::new(),
        }
    }

    fn record_reachability(&mut self, reachability: AutoNatReachability) {
        self.reachability = reachability;
    }

    const fn private_reachability(&self) -> bool {
        matches!(self.reachability, AutoNatReachability::Private)
    }

    const fn should_attempt_reservations(&self) -> bool {
        !matches!(self.reachability, AutoNatReachability::Public)
    }

    fn should_discover_candidates(&self) -> bool {
        self.should_attempt_reservations()
            && self.policy.max_candidates > self.candidates.len()
            && self.policy.max_reservations > 0
            && self.accepted_reservation_peers.len() < self.policy.max_reservations
    }

    fn snapshot(&self, now: Instant) -> AutoRelaySnapshot {
        AutoRelaySnapshot {
            max_candidates: self.policy.max_candidates,
            max_reservations: self.policy.max_reservations,
            retry_interval_seconds: self.policy.retry_interval_seconds,
            private_reachability: self.private_reachability(),
            candidates: self.candidates.len(),
            reservations: self.accepted_reservation_peers.len(),
            pending_retries: self
                .retry_after
                .values()
                .filter(|retry_after| now < **retry_after)
                .count(),
        }
    }

    fn record_candidate(&mut self, peer: Libp2pPeerId, address: Multiaddr) -> bool {
        if self.policy.max_candidates == 0 {
            return false;
        }
        if self
            .candidates
            .iter()
            .any(|(candidate_peer, _)| *candidate_peer == peer)
        {
            return false;
        }
        if self.candidates.len() >= self.policy.max_candidates {
            return false;
        }

        self.candidates.push((peer, address));
        true
    }

    fn remove_candidate(&mut self, peer: Libp2pPeerId) -> bool {
        let original_len = self.candidates.len();
        self.candidates
            .retain(|(candidate_peer, _)| *candidate_peer != peer);
        self.attempted_reservations
            .retain(|(attempted_peer, _)| *attempted_peer != peer);
        self.pending_reservations.remove(&peer);
        self.accepted_reservation_peers.remove(&peer);
        self.retry_after.remove(&peer);
        self.reservation_failures.remove(&peer);
        self.candidates.len() != original_len
    }

    fn record_reservation_failure(&mut self, peer: Libp2pPeerId) -> bool {
        let failures = self.reservation_failures.entry(peer).or_default();
        *failures = failures.saturating_add(1);
        if *failures < AUTO_RELAY_CANDIDATE_FAILURE_EVICTION_THRESHOLD {
            return false;
        }
        self.remove_candidate(peer)
    }

    fn next_reservation_targets(&mut self, now: Instant) -> Vec<(Libp2pPeerId, Multiaddr)> {
        if !self.should_attempt_reservations()
            || self.policy.max_reservations == 0
            || self.reservation_slots() >= self.policy.max_reservations
        {
            return Vec::new();
        }

        let mut targets = Vec::new();
        for (peer, address) in &self.candidates {
            if self.accepted_reservation_peers.contains(peer)
                || self.pending_reservations.contains_key(peer)
            {
                continue;
            }
            if self
                .retry_after
                .get(peer)
                .is_some_and(|retry_after| now < *retry_after)
            {
                continue;
            }
            if !self.attempted_reservations.insert((*peer, address.clone())) {
                continue;
            }
            self.pending_reservations.insert(
                *peer,
                PendingAutoRelayReservation {
                    address: address.clone(),
                    expires_at: now + AUTO_RELAY_RESERVATION_PENDING_TIMEOUT,
                },
            );
            targets.push((*peer, address.clone()));
            if self.reservation_slots() >= self.policy.max_reservations {
                break;
            }
        }

        targets
    }

    fn reservation_slots(&self) -> usize {
        self.accepted_reservation_peers.len() + self.pending_reservations.len()
    }

    fn record_reservation_accepted(&mut self, peer: Libp2pPeerId) {
        self.pending_reservations.remove(&peer);
        self.accepted_reservation_peers.insert(peer);
        self.retry_after.remove(&peer);
        self.reservation_failures.remove(&peer);
    }

    fn release_reservation_peer(&mut self, peer: Libp2pPeerId) -> bool {
        self.pending_reservations.remove(&peer).is_some()
            | self.accepted_reservation_peers.remove(&peer)
    }

    fn release_reservation_for_retry(&mut self, peer: Libp2pPeerId) -> bool {
        let released = self.release_reservation_peer(peer);
        self.attempted_reservations
            .retain(|(attempted_peer, _)| *attempted_peer != peer);
        released
    }

    fn release_reservation_for_retry_after(&mut self, peer: Libp2pPeerId, now: Instant) -> bool {
        let released = self.release_reservation_for_retry(peer);
        self.retry_after
            .insert(peer, now + self.policy.retry_interval());
        released
    }

    fn expire_pending_reservations(&mut self, now: Instant) -> Vec<(Libp2pPeerId, Multiaddr)> {
        let expired_peers = self
            .pending_reservations
            .iter()
            .filter_map(|(peer, pending)| (now >= pending.expires_at).then_some(*peer))
            .collect::<Vec<_>>();
        let mut expired = Vec::with_capacity(expired_peers.len());
        for peer in expired_peers {
            if let Some(pending) = self.pending_reservations.remove(&peer) {
                self.attempted_reservations
                    .remove(&(peer, pending.address.clone()));
                self.retry_after
                    .insert(peer, now + self.policy.retry_interval());
                expired.push((peer, pending.address));
            }
        }
        expired
    }
}

#[derive(Debug, Default)]
struct InfrastructurePeers {
    peers: HashMap<Libp2pPeerId, Multiaddr>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RelayInfrastructureSnapshot {
    peers: Vec<RelayInfrastructurePeerSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RelayInfrastructurePeerSnapshot {
    peer: Libp2pPeerId,
    address: Multiaddr,
    connected: bool,
}

impl InfrastructurePeers {
    fn insert(&mut self, peer: Libp2pPeerId, address: Multiaddr) -> bool {
        if self.peers.contains_key(&peer) || self.peers.len() >= AUTO_RELAY_MAX_INFRASTRUCTURE_PEERS
        {
            return false;
        }

        self.peers.insert(peer, address);
        true
    }

    fn contains(&self, peer: Libp2pPeerId) -> bool {
        self.peers.contains_key(&peer)
    }

    fn remove(&mut self, peer: Libp2pPeerId) -> bool {
        self.peers.remove(&peer).is_some()
    }

    fn snapshot(&self, swarm: &Swarm<Behaviour>) -> RelayInfrastructureSnapshot {
        let mut peers = self
            .peers
            .iter()
            .map(|(peer, address)| RelayInfrastructurePeerSnapshot {
                peer: *peer,
                address: address.clone(),
                connected: swarm.is_connected(peer),
            })
            .collect::<Vec<_>>();
        peers.sort_by_key(|peer| peer.peer.to_string());
        RelayInfrastructureSnapshot { peers }
    }
}

fn admit_kademlia_relay_infrastructure_peer<'a>(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    infrastructure_peers: &mut InfrastructurePeers,
    auto_relay: &mut AutoRelayState,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    mut addresses: impl Iterator<Item = &'a Multiaddr>,
) {
    if peer == *swarm.local_peer_id()
        || forwarder.is_configured_transport_peer(peer)
        || infrastructure_peers.contains(peer)
    {
        return;
    }

    let Some(address) = addresses.find_map(|address| auto_relay_candidate_address(peer, address))
    else {
        return;
    };

    if !infrastructure_peers.insert(peer, address.clone()) {
        return;
    }
    record_auto_relay_candidates(auto_relay, metrics, peer, std::iter::once(address.clone()));

    metrics.record_auto_relay_infrastructure_candidate();
    log_runtime_event(
        LogLevel::Info,
        "auto_relay_infrastructure_candidate",
        &[
            ("peer", &peer.to_string()),
            ("address", &address.to_string()),
        ],
    );

    if swarm.is_connected(&peer) {
        return;
    }

    metrics.record_auto_relay_infrastructure_dial_attempt();
    if let Err(error) = swarm.dial(address.clone()) {
        metrics.record_auto_relay_infrastructure_dial_failure();
        infrastructure_peers.remove(peer);
        auto_relay.remove_candidate(peer);
        log_runtime_event(
            LogLevel::Warn,
            "auto_relay_infrastructure_dial_failed",
            &[
                ("peer", &peer.to_string()),
                ("address", &address.to_string()),
                ("error", &error.to_string()),
            ],
        );
    }
}

fn reject_unconfirmed_infrastructure_peer(
    swarm: &mut Swarm<Behaviour>,
    infrastructure_peers: &mut InfrastructurePeers,
    auto_relay: &mut AutoRelayState,
    peer: Libp2pPeerId,
    reason: &str,
) {
    if !infrastructure_peers.remove(peer) {
        return;
    }
    auto_relay.remove_candidate(peer);

    log_runtime_event(
        LogLevel::Warn,
        "auto_relay_infrastructure_rejected",
        &[("peer", &peer.to_string()), ("reason", reason)],
    );
    if swarm.disconnect_peer_id(peer).is_err() {
        log_runtime_event(
            LogLevel::Warn,
            "auto_relay_infrastructure_already_disconnected",
            &[("peer", &peer.to_string())],
        );
    }
}

fn record_auto_relay_candidates(
    auto_relay: &mut AutoRelayState,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    addresses: impl IntoIterator<Item = Multiaddr>,
) {
    for address in addresses {
        if auto_relay.record_candidate(peer, address.clone()) {
            metrics.record_auto_relay_candidate();
            log_runtime_event(
                LogLevel::Info,
                "auto_relay_candidate",
                &[
                    ("peer", &peer.to_string()),
                    ("address", &address.to_string()),
                ],
            );
        }
    }
}

fn attempt_auto_relay_reservations(
    swarm: &mut Swarm<Behaviour>,
    auto_relay: &mut AutoRelayState,
    metrics: &RuntimeMetrics,
) {
    let local_peer = *swarm.local_peer_id();
    let now = Instant::now();
    for (relay_peer, relay_address) in auto_relay.next_reservation_targets(now) {
        let reservation_address =
            peer_dial_address(local_peer, relay_address.clone().with(Protocol::P2pCircuit));
        metrics.record_auto_relay_reservation_attempt();
        log_runtime_event(
            LogLevel::Info,
            "auto_relay_reservation_attempt",
            &[
                ("relay", &relay_peer.to_string()),
                ("address", &reservation_address.to_string()),
            ],
        );
        if let Err(error) = swarm.listen_on(reservation_address.clone()) {
            auto_relay.release_reservation_for_retry_after(relay_peer, now);
            metrics.record_auto_relay_reservation_failure();
            log_runtime_event(
                LogLevel::Warn,
                "auto_relay_reservation_failed",
                &[
                    ("relay", &relay_peer.to_string()),
                    ("address", &reservation_address.to_string()),
                    ("error", &error.to_string()),
                ],
            );
        }
    }
}

fn auto_relay_candidate_addresses(peer: Libp2pPeerId, info: &identify::Info) -> Vec<Multiaddr> {
    if !identify_protocols_include_relay_hop(&info.protocols) {
        return Vec::new();
    }

    info.listen_addrs
        .iter()
        .filter_map(|address| auto_relay_candidate_address(peer, address))
        .fold(Vec::new(), |mut addresses, address| {
            if !addresses.contains(&address) {
                addresses.push(address);
            }
            addresses
        })
}

fn identify_protocols_include_relay_hop(protocols: &[libp2p::StreamProtocol]) -> bool {
    protocols
        .iter()
        .any(|protocol| protocol.as_ref() == relay::HOP_PROTOCOL_NAME.as_ref())
}

fn auto_relay_candidate_address(peer: Libp2pPeerId, address: &Multiaddr) -> Option<Multiaddr> {
    if relayed_address_relay_peer(address).is_some() {
        return None;
    }
    if !supports_auto_relay_candidate_transport(address) {
        return None;
    }
    if discovered_address_target(address).is_some_and(|target| target != peer) {
        return None;
    }

    Some(peer_dial_address(peer, address.clone()))
}

fn supports_auto_relay_candidate_transport(address: &Multiaddr) -> bool {
    let mut supported = false;
    for protocol in address {
        match protocol {
            Protocol::Tcp(_) | Protocol::Quic | Protocol::QuicV1 => supported = true,
            Protocol::Http
            | Protocol::Https
            | Protocol::P2pWebRtcDirect
            | Protocol::P2pWebRtcStar
            | Protocol::P2pWebSocketStar
            | Protocol::Tls
            | Protocol::WebRTC
            | Protocol::WebRTCDirect
            | Protocol::WebTransport
            | Protocol::Ws(_)
            | Protocol::Wss(_) => return false,
            _ => {}
        }
    }

    supported
}

fn supports_relayed_peer_dial_transport(address: &Multiaddr) -> bool {
    let mut relay_transport = Multiaddr::empty();
    for protocol in address {
        if matches!(protocol, Protocol::P2pCircuit) {
            return supports_auto_relay_candidate_transport(&relay_transport);
        }
        relay_transport.push(protocol);
    }

    false
}

impl RelayReadiness {
    fn record_reservation_accepted(&mut self, relay: Libp2pPeerId) {
        self.accepted_reservations.insert(relay);
    }

    fn record_relay_listen_address(&mut self, relay: Libp2pPeerId) {
        self.relayed_listen_addresses.insert(relay);
    }

    fn record_relay_listen_address_lost(&mut self, relay: Libp2pPeerId) -> bool {
        self.relayed_listen_addresses.remove(&relay)
    }

    fn record_relay_reservation_lost(&mut self, relay: Libp2pPeerId) -> bool {
        let removed_reservation = self.accepted_reservations.remove(&relay);
        let removed_listen_address = self.relayed_listen_addresses.remove(&relay);
        if removed_reservation || removed_listen_address {
            self.attempted_ready_dials
                .retain(|(attempted_relay, _, _)| *attempted_relay != relay);
            true
        } else {
            false
        }
    }

    fn relay_ready(&self, relay: Libp2pPeerId) -> bool {
        self.accepted_reservations.contains(&relay)
            && self.relayed_listen_addresses.contains(&relay)
    }

    fn should_attempt_ready_dial(
        &mut self,
        relay: Libp2pPeerId,
        peer: Libp2pPeerId,
        address: &Multiaddr,
    ) -> bool {
        self.attempted_ready_dials
            .insert((relay, peer, address.clone()))
    }
}

fn dial_relay_ready_configured_peers(
    swarm: &mut Swarm<Behaviour>,
    relay_readiness: &mut RelayReadiness,
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &DiscoveredPeerAddresses,
    metrics: &RuntimeMetrics,
    relay: Libp2pPeerId,
) {
    if !relay_readiness.relay_ready(relay) {
        return;
    }

    for (peer, address) in relay_ready_configured_peer_targets(
        *swarm.local_peer_id(),
        relay,
        configured_peer_addresses,
        discovered_peer_addresses,
        |peer| swarm.is_connected(peer),
    ) {
        if !relay_readiness.should_attempt_ready_dial(relay, peer, &address) {
            continue;
        }
        metrics.record_redial_attempt();
        let dial_address = peer_dial_address(peer, address.clone());
        log_runtime_event(
            LogLevel::Info,
            "relay_ready_peer_dial",
            &[
                ("relay", &relay.to_string()),
                ("peer", &peer.to_string()),
                ("address", &dial_address.to_string()),
            ],
        );
        if let Err(error) = swarm.dial(dial_address.clone()) {
            metrics.record_redial_failure();
            log_runtime_event(
                LogLevel::Warn,
                "relay_ready_peer_dial_failed",
                &[
                    ("relay", &relay.to_string()),
                    ("peer", &peer.to_string()),
                    ("error", &error.to_string()),
                ],
            );
        }
    }
}

fn relay_ready_configured_peer_targets(
    local_peer: Libp2pPeerId,
    relay: Libp2pPeerId,
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &DiscoveredPeerAddresses,
    mut is_connected: impl FnMut(&Libp2pPeerId) -> bool,
) -> Vec<(Libp2pPeerId, Multiaddr)> {
    let mut seen = HashSet::new();
    let mut addresses = Vec::new();
    let discovered = discovered_peer_addresses
        .addresses
        .iter()
        .map(|entry| (&entry.peer, &entry.address));
    for (peer, address) in configured_peer_addresses
        .iter()
        .map(|(peer, address)| (peer, address))
        .chain(discovered)
    {
        if *peer == local_peer || is_connected(peer) {
            continue;
        }
        if relayed_address_relay_peer(address) != Some(relay) {
            continue;
        }
        if !seen.insert((*peer, address.clone())) {
            continue;
        }
        addresses.push((*peer, address.clone()));
    }
    addresses
}

#[allow(clippy::too_many_arguments)]
fn redial_selected_addresses(
    swarm: &mut Swarm<Behaviour>,
    selected_peers: &HashSet<Libp2pPeerId>,
    bootstrap_addresses: &[(Libp2pPeerId, Multiaddr)],
    relay_addresses: &[(Libp2pPeerId, Multiaddr)],
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    paths: &PathSet,
    metrics: &RuntimeMetrics,
) {
    let local_peer = *swarm.local_peer_id();
    let targets = pending_redial_targets(
        local_peer,
        bootstrap_addresses,
        relay_addresses,
        configured_peer_addresses,
        discovered_peer_addresses,
        |peer| redial_connection_state(paths, *peer, swarm.is_connected(peer)),
        |_| true,
    );

    for (peer, address) in targets.addresses {
        if !selected_peers.contains(&peer) {
            continue;
        }
        metrics.record_redial_attempt();
        let dial_address = peer_dial_address(peer, address);
        if let Err(error) = swarm.dial(dial_address.clone()) {
            metrics.record_redial_failure();
            log_runtime_event(
                LogLevel::Warn,
                "redial_failed",
                &[
                    ("peer", &peer.to_string()),
                    ("address", &dial_address.to_string()),
                    ("error", &error.to_string()),
                ],
            );
        }
    }
}

fn redial_packet_plane_recovery_addresses(
    swarm: &mut Swarm<Behaviour>,
    peer: Libp2pPeerId,
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    metrics: &RuntimeMetrics,
) {
    let local_peer = *swarm.local_peer_id();
    for (peer, address) in packet_plane_recovery_targets(
        local_peer,
        peer,
        configured_peer_addresses,
        discovered_peer_addresses,
    ) {
        metrics.record_packet_plane_path_recovery_dial_attempt();
        let dial_address = peer_dial_address(peer, address);
        if let Err(error) = swarm.dial(dial_address.clone()) {
            metrics.record_packet_plane_path_recovery_dial_failure();
            log_runtime_event(
                LogLevel::Warn,
                "packet_plane_path_recovery_dial_failed",
                &[
                    ("peer", &peer.to_string()),
                    ("address", &dial_address.to_string()),
                    ("error", &error.to_string()),
                ],
            );
        }
    }
}

fn packet_plane_recovery_targets(
    local_peer: Libp2pPeerId,
    peer: Libp2pPeerId,
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
) -> Vec<(Libp2pPeerId, Multiaddr)> {
    if peer == local_peer {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut addresses = Vec::new();
    for (candidate_peer, address) in configured_peer_addresses
        .iter()
        .chain(discovered_peer_addresses.iter())
    {
        if *candidate_peer != peer {
            continue;
        }
        if !seen.insert((*candidate_peer, address.clone())) {
            continue;
        }
        addresses.push((*candidate_peer, address.clone()));
    }
    addresses
}

fn pending_redial_targets(
    local_peer: Libp2pPeerId,
    bootstrap_addresses: &[(Libp2pPeerId, Multiaddr)],
    relay_addresses: &[(Libp2pPeerId, Multiaddr)],
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    mut connection_state: impl FnMut(&Libp2pPeerId) -> RedialConnectionState,
    mut relay_ready: impl FnMut(Libp2pPeerId) -> bool,
) -> RedialTargets {
    let mut addresses = Vec::new();
    let mut skipped_connected = 0;
    let mut seen = HashSet::new();
    let configured_relay_reservations = relay_addresses
        .iter()
        .map(|(peer, _)| *peer)
        .collect::<HashSet<_>>();
    for (peer, address) in bootstrap_addresses.iter().chain(relay_addresses.iter()) {
        if *peer == local_peer {
            continue;
        }
        if connection_state(peer).is_connected() {
            skipped_connected += 1;
            continue;
        }
        if !seen.insert((*peer, address.clone())) {
            continue;
        }
        addresses.push((*peer, address.clone()));
    }
    for (peer, address) in configured_peer_addresses
        .iter()
        .chain(discovered_peer_addresses.iter())
    {
        if *peer == local_peer {
            continue;
        }
        match connection_state(peer) {
            RedialConnectionState::Disconnected | RedialConnectionState::ConnectedNoUsablePath => {}
            RedialConnectionState::RelayOnly if relayed_address_relay_peer(address).is_none() => {}
            RedialConnectionState::DirectOnly if relayed_address_relay_peer(address).is_some() => {}
            RedialConnectionState::RelayOnly
            | RedialConnectionState::DirectOnly
            | RedialConnectionState::DirectAndRelay => {
                skipped_connected += 1;
                continue;
            }
        }
        if let Some(relay) = relayed_address_relay_peer(address)
            && configured_relay_reservations.contains(&relay)
            && !relay_ready(relay)
        {
            continue;
        }
        if !seen.insert((*peer, address.clone())) {
            continue;
        }
        addresses.push((*peer, address.clone()));
    }
    RedialTargets {
        addresses,
        skipped_connected,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RedialConnectionState {
    Disconnected,
    ConnectedNoUsablePath,
    RelayOnly,
    DirectOnly,
    DirectAndRelay,
}

impl RedialConnectionState {
    const fn is_connected(self) -> bool {
        !matches!(self, Self::Disconnected | Self::ConnectedNoUsablePath)
    }
}

fn redial_connection_state(
    paths: &PathSet,
    peer: Libp2pPeerId,
    connected: bool,
) -> RedialConnectionState {
    if !connected {
        return RedialConnectionState::Disconnected;
    }

    let overlay_peer = PeerId::from_libp2p(peer);
    let mut has_direct = false;
    let mut has_relay = false;
    let mut has_candidate = false;
    for candidate in paths.candidates_for(overlay_peer) {
        has_candidate = true;
        if !candidate.healthy || candidate.established_connections == 0 {
            continue;
        }
        if candidate.is_relay() {
            has_relay = true;
        } else {
            has_direct = true;
        }
    }

    if has_direct && has_relay {
        RedialConnectionState::DirectAndRelay
    } else if has_direct {
        RedialConnectionState::DirectOnly
    } else if has_relay {
        RedialConnectionState::RelayOnly
    } else if has_candidate {
        RedialConnectionState::ConnectedNoUsablePath
    } else {
        RedialConnectionState::RelayOnly
    }
}

fn has_healthy_relay_path(paths: &PathSet, peer: Libp2pPeerId) -> bool {
    let overlay_peer = PeerId::from_libp2p(peer);
    paths
        .candidates_for(overlay_peer)
        .any(|candidate| candidate.healthy && candidate.is_relay())
}

fn should_dial_discovered_address(
    paths: &PathSet,
    peer: Libp2pPeerId,
    connected: bool,
    address: &Multiaddr,
) -> bool {
    match redial_connection_state(paths, peer, connected) {
        RedialConnectionState::DirectOnly | RedialConnectionState::DirectAndRelay => false,
        RedialConnectionState::RelayOnly
            if relayed_address_relay_peer(address).is_some()
                && has_healthy_relay_path(paths, peer) =>
        {
            false
        }
        RedialConnectionState::Disconnected
        | RedialConnectionState::ConnectedNoUsablePath
        | RedialConnectionState::RelayOnly => true,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RedialTargets {
    addresses: Vec<(Libp2pPeerId, Multiaddr)>,
    skipped_connected: usize,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct DiscoveredPeerAddresses {
    addresses: Vec<DiscoveredPeerAddress>,
}

#[derive(Debug, Eq, PartialEq)]
struct DiscoveredPeerAddress {
    peer: Libp2pPeerId,
    address: Multiaddr,
    last_seen: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiscoveredPeerAddressSource {
    AuthenticatedPeerRecord,
    UnauthenticatedDiscovery,
}

impl DiscoveredPeerAddressSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AuthenticatedPeerRecord => "authenticated_peer_record",
            Self::UnauthenticatedDiscovery => "unauthenticated_discovery",
        }
    }
}

impl DiscoveredPeerAddresses {
    fn insert(&mut self, peer: Libp2pPeerId, address: Multiaddr) {
        self.insert_at(peer, address, Instant::now());
    }

    fn insert_at(&mut self, peer: Libp2pPeerId, address: Multiaddr, now: Instant) {
        if let Some(entry) = self
            .addresses
            .iter_mut()
            .find(|entry| entry.peer == peer && entry.address == address)
        {
            entry.last_seen = now;
            return;
        }

        self.addresses.push(DiscoveredPeerAddress {
            peer,
            address,
            last_seen: now,
        });
    }

    fn remove(&mut self, peer: Libp2pPeerId, address: &Multiaddr) {
        self.addresses
            .retain(|entry| entry.peer != peer || &entry.address != address);
    }

    fn drop_expired(&mut self, now: Instant, max_age: Duration) -> u64 {
        let mut expired = 0_u64;
        self.addresses.retain(|entry| {
            let keep = now.saturating_duration_since(entry.last_seen) <= max_age;
            if !keep {
                expired = expired.saturating_add(1);
            }
            keep
        });
        expired
    }

    fn as_vec(&self) -> Vec<(Libp2pPeerId, Multiaddr)> {
        self.addresses
            .iter()
            .map(|entry| (entry.peer, entry.address.clone()))
            .collect()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OverlayMembership {
    peers: HashSet<Libp2pPeerId>,
    configured_infrastructure_peers: HashSet<Libp2pPeerId>,
}

impl OverlayMembership {
    pub fn from_config(config: &Config) -> Result<Self, ConfigError> {
        Self::from_config_with_member_records(
            config,
            &config.network.member_records,
            current_unix_seconds_lossy(),
        )
    }

    fn from_config_with_member_records(
        config: &Config,
        member_records: &[SignedMembershipRecord],
        now_unix_seconds: u64,
    ) -> Result<Self, ConfigError> {
        let mut peers = HashSet::new();
        let mut configured_infrastructure_peers = HashSet::new();
        peers.insert(
            config
                .network
                .local_peer
                .parse()
                .map_err(ConfigError::Libp2pPeerId)?,
        );

        for peer in &config.peers {
            peers.insert(peer.id.parse().map_err(ConfigError::Libp2pPeerId)?);
        }

        for member in
            effective_membership_at(member_records, &config.network.name, now_unix_seconds)?
                .overlay_members()
        {
            peers.insert(member.transport_peer);
        }

        for peer in &config.network.bootstrap_peers {
            configured_infrastructure_peers
                .insert(peer.id.parse().map_err(ConfigError::Libp2pPeerId)?);
        }

        for (_, address) in config.peer_multiaddrs()? {
            if let Some(peer) = relay_peer_from_relayed_address(&address) {
                configured_infrastructure_peers.insert(peer);
            }
        }

        for address in config.relay_reservation_multiaddrs()? {
            if let Some(peer) = relay_peer_from_relayed_address(&address) {
                configured_infrastructure_peers.insert(peer);
            }
        }

        Ok(Self {
            peers,
            configured_infrastructure_peers,
        })
    }

    #[must_use]
    pub fn allows(&self, peer: Libp2pPeerId) -> bool {
        self.peers.contains(&peer)
    }

    pub fn replace_record_members(
        &mut self,
        config: &Config,
        member_records: &[SignedMembershipRecord],
        now_unix_seconds: u64,
    ) -> Result<(), ConfigError> {
        let next = Self::from_config_with_member_records(config, member_records, now_unix_seconds)?;
        self.peers = next.peers;
        self.configured_infrastructure_peers = next.configured_infrastructure_peers;
        Ok(())
    }

    #[must_use]
    fn allows_configured_infrastructure(&self, peer: Libp2pPeerId) -> bool {
        self.configured_infrastructure_peers.contains(&peer)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    #[must_use]
    #[cfg(test)]
    fn configured_infrastructure_len(&self) -> usize {
        self.configured_infrastructure_peers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

fn relay_peer_from_relayed_address(address: &Multiaddr) -> Option<Libp2pPeerId> {
    let mut relay_peer = None;
    for protocol in address {
        match protocol {
            Protocol::P2p(peer) => relay_peer = Some(peer),
            Protocol::P2pCircuit => return relay_peer,
            _ => {}
        }
    }

    None
}

fn spawn_tun_reader(
    mut reader: TunReader,
    metrics: Arc<RuntimeMetrics>,
    mtu: u16,
) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel(TUN_READ_CHANNEL);
    std::thread::spawn(move || {
        let mut buffer = vec![0; usize::from(mtu)];
        loop {
            match reader.read_packet(&mut buffer) {
                Ok(length) => {
                    metrics.record_tun_read(length);
                    if tx.blocking_send(buffer[..length].to_vec()).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    log_runtime_event(
                        LogLevel::Error,
                        "tun_read_failed",
                        &[("error", &format!("{error:?}"))],
                    );
                    return;
                }
            }
        }
    });
    rx
}

struct RuntimeOutboundDrain<'a> {
    node: &'a mut P2pNode,
    forwarder: &'a Forwarder,
    queues: &'a mut PeerQueues,
    paths: &'a mut PathSet,
    peer_capabilities: &'a PeerCapabilities,
    queue_runtime: &'a mut QueueRuntimeState,
    writer: &'a mut TunWriter,
    packet_plane: &'a PacketPlaneRuntime,
    packet_plane_quic: Option<&'a PacketPlaneQuicRuntime>,
    metrics: &'a RuntimeMetrics,
}

async fn drain_runtime_outbound_queue(drain: RuntimeOutboundDrain<'_>) {
    let RuntimeOutboundDrain {
        node,
        forwarder,
        queues,
        paths,
        peer_capabilities,
        queue_runtime,
        writer,
        packet_plane,
        packet_plane_quic,
        metrics,
    } = drain;
    let discovered_addresses = queue_runtime.discovered_peer_addresses.as_vec();
    let mut context = QueueDrainContext {
        paths,
        peer_capabilities,
        bootstrap_addresses: &node.bootstrap_peer_addresses,
        relay_addresses: &node.relay_peer_addresses,
        configured_peer_addresses: &node.configured_peer_addresses,
        discovered_peer_addresses: &discovered_addresses,
        packet_in_flight: &mut queue_runtime.packet_in_flight,
        last_blocked_queue_redial: &mut queue_runtime.last_blocked_queue_redial,
        writer: Some(writer),
        packet_plane: Some(packet_plane),
        packet_plane_quic,
        metrics,
    };
    drain_outbound_queue(&mut node.swarm, forwarder, queues, &mut context).await;
}

async fn drain_outbound_queue(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    queues: &mut PeerQueues,
    context: &mut QueueDrainContext<'_>,
) {
    expire_outbound_queue(queues, context.metrics);
    while let Some(packet) = queues.dequeue_ready_packet(|peer, packet| {
        let decision = packet_transport_decision(
            context.paths,
            context.peer_capabilities,
            context.packet_plane,
            context.packet_plane_quic,
            peer,
        );
        packet_in_flight_allows(context.packet_in_flight, packet, decision) && decision.can_send()
    }) {
        let peer_mtu = selected_path_mtu(
            context.paths,
            context.peer_capabilities,
            context.packet_plane,
            context.packet_plane_quic,
            packet.peer(),
            u16::try_from(forwarder.mtu()).unwrap_or(u16::MAX),
        );
        match packet_transport_decision(
            context.paths,
            context.peer_capabilities,
            context.packet_plane,
            context.packet_plane_quic,
            packet.peer(),
        ) {
            PacketTransportDecision::PacketPlaneDatagram { path, backend } => {
                send_dequeued_packet_plane_datagram(
                    swarm, forwarder, &packet, peer_mtu, path, backend, context,
                )
                .await;
            }
            PacketTransportDecision::StreamFallback { path } => {
                send_dequeued_stream_fallback(swarm, forwarder, &packet, peer_mtu, path, context);
            }
            PacketTransportDecision::Blocked { reason, .. } => {
                if reason == PacketTransportBlockReason::LocalQuicDatagramsUnavailable {
                    context.metrics.record_outbound_quic_datagram_unavailable();
                }
            }
        }
    }
    if queues.total_stats().queued_packets > 0 {
        let mut blocked_by_window = false;
        let mut blocked_by_path = false;
        for peer in queues.queued_peers() {
            let decision = packet_transport_decision(
                context.paths,
                context.peer_capabilities,
                context.packet_plane,
                context.packet_plane_quic,
                peer,
            );
            if decision.can_send()
                && !queues.peer_has_ready_packet(peer, |packet| {
                    packet_in_flight_allows(context.packet_in_flight, packet, decision)
                })
            {
                blocked_by_window = true;
                continue;
            }
            blocked_by_path = true;
            if let PacketTransportDecision::Blocked {
                reason: PacketTransportBlockReason::LocalQuicDatagramsUnavailable,
                ..
            } = decision
            {
                context.metrics.record_outbound_quic_datagram_unavailable();
            }
        }
        if blocked_by_window {
            context
                .metrics
                .record_outbound_queue_blocked_packet_window();
        }
        if blocked_by_path {
            context
                .metrics
                .record_outbound_queue_blocked_no_supported_path();
            if should_redial_blocked_queue(context.last_blocked_queue_redial, Instant::now()) {
                dial_blocked_queue_peers(swarm, forwarder, queues, context);
            }
        }
    }
}

fn packet_in_flight_allows(
    packet_in_flight: &PacketInFlight,
    packet: &crate::queue::Packet,
    decision: PacketTransportDecision,
) -> bool {
    if matches!(decision, PacketTransportDecision::StreamFallback { .. })
        && !packet.requires_ordered_delivery()
    {
        return packet_in_flight.can_send(packet.peer());
    }

    packet_in_flight.can_send_packet(packet)
}

async fn send_dequeued_packet_plane_datagram(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    packet: &crate::queue::Packet,
    peer_mtu: u16,
    path: PathKind,
    backend: PacketDatagramBackend,
    context: &mut QueueDrainContext<'_>,
) {
    match forwarder.queued_packet_frame_with_mtu(packet, peer_mtu) {
        Ok(frame) => match send_packet_plane_frame(
            context.packet_plane,
            context.packet_plane_quic,
            backend,
            packet.peer(),
            &frame,
        )
        .await
        {
            Ok(_) => {
                context.metrics.record_outbound_sent();
                context.metrics.record_outbound_quic_datagram();
            }
            Err(error) => {
                if send_dequeued_packet_plane_fallback(
                    swarm,
                    forwarder,
                    PacketPlaneFallbackAttempt {
                        packet,
                        peer_mtu,
                        path,
                        failed_backend: backend,
                        frame: &frame,
                    },
                    context,
                )
                .await
                {
                    return;
                }
                if maybe_demote_packet_plane_send_path(
                    context.paths,
                    context.metrics,
                    packet.peer(),
                    path,
                    &error,
                ) && let Some(peer) = forwarder.transport_peer_for_overlay(packet.peer())
                {
                    redial_packet_plane_recovery_addresses(
                        swarm,
                        peer,
                        context.configured_peer_addresses,
                        context.discovered_peer_addresses,
                        context.metrics,
                    );
                }
                context
                    .metrics
                    .record_outbound_drop(packet_plane_send_drop_reason(&error));
                eprintln!("dropping queued packet-plane outbound packet: {error:?}");
            }
        },
        Err(error) => {
            maybe_learn_path_mtu(context, packet.peer(), path, &error);
            maybe_write_packet_too_big(context, packet.payload(), &error);
            context
                .metrics
                .record_outbound_drop(outbound_drop_reason(&error));
            eprintln!("dropping queued packet-plane outbound packet: {error:?}");
        }
    }
}

struct PacketPlaneFallbackAttempt<'a> {
    packet: &'a crate::queue::Packet,
    peer_mtu: u16,
    path: PathKind,
    failed_backend: PacketDatagramBackend,
    frame: &'a Frame,
}

async fn send_dequeued_packet_plane_fallback(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    attempt: PacketPlaneFallbackAttempt<'_>,
    context: &mut QueueDrainContext<'_>,
) -> bool {
    if let Some(backend) = packet_plane_send_fallback_backend(
        attempt.failed_backend,
        context.peer_capabilities,
        context.packet_plane,
        attempt.packet.peer(),
    ) {
        match send_packet_plane_frame(
            context.packet_plane,
            context.packet_plane_quic,
            backend,
            attempt.packet.peer(),
            attempt.frame,
        )
        .await
        {
            Ok(_) => {
                context.metrics.record_outbound_sent();
                context.metrics.record_outbound_quic_datagram();
                log_runtime_event(
                    LogLevel::Warn,
                    "packet_plane_backend_fallback",
                    &[
                        ("peer", &attempt.packet.peer().to_string()),
                        ("from", packet_datagram_backend_name(attempt.failed_backend)),
                        ("to", packet_datagram_backend_name(backend)),
                    ],
                );
                return true;
            }
            Err(error) => {
                if maybe_demote_packet_plane_send_path(
                    context.paths,
                    context.metrics,
                    attempt.packet.peer(),
                    attempt.path,
                    &error,
                ) && let Some(peer) = forwarder.transport_peer_for_overlay(attempt.packet.peer())
                {
                    redial_packet_plane_recovery_addresses(
                        swarm,
                        peer,
                        context.configured_peer_addresses,
                        context.discovered_peer_addresses,
                        context.metrics,
                    );
                }
                context
                    .metrics
                    .record_outbound_drop(packet_plane_send_drop_reason(&error));
                eprintln!("dropping queued packet-plane fallback outbound packet: {error:?}");
                return true;
            }
        }
    }

    if let Some(path) = packet_plane_send_stream_fallback_path(
        context.paths,
        context.peer_capabilities,
        attempt.packet.peer(),
    ) {
        log_runtime_event(
            LogLevel::Warn,
            "packet_plane_backend_fallback",
            &[
                ("peer", &attempt.packet.peer().to_string()),
                ("from", packet_datagram_backend_name(attempt.failed_backend)),
                ("to", path.wire_name()),
            ],
        );
        send_dequeued_stream_fallback(
            swarm,
            forwarder,
            attempt.packet,
            attempt.peer_mtu,
            path,
            context,
        );
        return true;
    }

    false
}

async fn send_packet_plane_frame(
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    backend: PacketDatagramBackend,
    peer: PeerId,
    frame: &Frame,
) -> Result<usize, PacketPlaneSendError> {
    match backend {
        PacketDatagramBackend::OwnedUdp => packet_plane
            .ok_or(PacketPlaneSendError::MissingUdpRuntime)?
            .send_frame_to_peer(peer, frame)
            .await
            .map_err(PacketPlaneSendError::Udp),
        PacketDatagramBackend::OwnedQuic => packet_plane_quic
            .ok_or(PacketPlaneSendError::MissingQuicRuntime)?
            .send_frame_to_peer(peer, frame)
            .map_err(PacketPlaneSendError::Quic),
    }
}

fn send_dequeued_stream_fallback(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    packet: &crate::queue::Packet,
    peer_mtu: u16,
    path: PathKind,
    context: &mut QueueDrainContext<'_>,
) {
    match forwarder.send_queued_packet_with_mtu(swarm, packet, peer_mtu) {
        Ok(request_id) => {
            context.packet_in_flight.record(packet, request_id, path);
            context.metrics.record_outbound_sent();
            context.metrics.record_outbound_stream_fallback();
        }
        Err(error) => {
            maybe_learn_path_mtu(context, packet.peer(), path, &error);
            maybe_write_packet_too_big(context, packet.payload(), &error);
            context
                .metrics
                .record_outbound_drop(outbound_drop_reason(&error));
            eprintln!("dropping queued outbound packet: {error:?}");
        }
    }
}

struct QueueDrainContext<'a> {
    paths: &'a mut PathSet,
    peer_capabilities: &'a PeerCapabilities,
    bootstrap_addresses: &'a [(Libp2pPeerId, Multiaddr)],
    relay_addresses: &'a [(Libp2pPeerId, Multiaddr)],
    configured_peer_addresses: &'a [(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &'a [(Libp2pPeerId, Multiaddr)],
    packet_in_flight: &'a mut PacketInFlight,
    last_blocked_queue_redial: &'a mut Option<Instant>,
    writer: Option<&'a mut TunWriter>,
    packet_plane: Option<&'a PacketPlaneRuntime>,
    packet_plane_quic: Option<&'a PacketPlaneQuicRuntime>,
    metrics: &'a RuntimeMetrics,
}

fn maybe_learn_path_mtu(
    context: &mut QueueDrainContext<'_>,
    peer: PeerId,
    path: PathKind,
    error: &ForwardError,
) {
    let ForwardError::PacketTooLarge { max, .. } = error else {
        return;
    };
    let mtu = u16::try_from(*max).unwrap_or(u16::MAX);
    if context.paths.lower_path_mtu(peer, path, mtu) {
        context.metrics.record_outbound_path_mtu_update();
        log_runtime_event(
            LogLevel::Info,
            "path_mtu_updated",
            &[
                ("peer", &peer.to_string()),
                ("path", path.wire_name()),
                ("mtu", &mtu.to_string()),
            ],
        );
    }
}

fn maybe_write_packet_too_big(
    context: &mut QueueDrainContext<'_>,
    original: &[u8],
    error: &ForwardError,
) {
    let ForwardError::PacketTooLarge { max, .. } = error else {
        return;
    };
    let mtu = u16::try_from(*max).unwrap_or(u16::MAX);
    let Some(notification) = packet_too_big(original, mtu) else {
        context.metrics.record_outbound_packet_too_big_unparseable();
        return;
    };
    let Some(writer) = context.writer.as_deref_mut() else {
        context.metrics.record_outbound_packet_too_big_no_writer();
        return;
    };
    match writer.write_packet(&notification) {
        Ok(length) => {
            context.metrics.record_tun_write(length);
            context
                .metrics
                .record_outbound_packet_too_big_notification();
        }
        Err(error) => {
            context
                .metrics
                .record_outbound_packet_too_big_write_failure();
            log_runtime_event(
                LogLevel::Warn,
                "packet_too_big_write_failed",
                &[("error", &format!("{error:?}"))],
            );
        }
    }
}

#[derive(Debug)]
struct QueueRuntimeState {
    discovered_peer_addresses: DiscoveredPeerAddresses,
    packet_in_flight: PacketInFlight,
    last_blocked_queue_redial: Option<Instant>,
}

impl QueueRuntimeState {
    fn new(packet_in_flight_limit_per_peer: usize) -> Self {
        Self {
            discovered_peer_addresses: DiscoveredPeerAddresses::default(),
            packet_in_flight: PacketInFlight::new(packet_in_flight_limit_per_peer),
            last_blocked_queue_redial: None,
        }
    }
}

#[derive(Debug)]
struct PacketInFlight {
    limit_per_peer: usize,
    requests: HashMap<request_response::OutboundRequestId, PacketInFlightRequest>,
    peers: HashMap<PeerId, PeerInFlight>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PacketInFlightRequest {
    peer: PeerId,
    shard: FlowShard,
    path: PathKind,
    sent_at: Instant,
}

impl PacketInFlight {
    fn new(limit_per_peer: usize) -> Self {
        Self {
            limit_per_peer: limit_per_peer.max(1),
            requests: HashMap::new(),
            peers: HashMap::new(),
        }
    }

    fn can_send(&self, peer: PeerId) -> bool {
        self.peers.get(&peer).map_or(0, |state| state.total) < self.limit_per_peer
    }

    fn can_send_packet(&self, packet: &crate::queue::Packet) -> bool {
        self.can_send(packet.peer())
            && self
                .peers
                .get(&packet.peer())
                .is_none_or(|state| state.in_flight_for_shard(packet.flow_shard()) == 0)
    }

    fn record(
        &mut self,
        packet: &crate::queue::Packet,
        request_id: request_response::OutboundRequestId,
        path: PathKind,
    ) {
        self.requests.insert(
            request_id,
            PacketInFlightRequest {
                peer: packet.peer(),
                shard: packet.flow_shard(),
                path,
                sent_at: Instant::now(),
            },
        );
        self.peers
            .entry(packet.peer())
            .or_default()
            .record(packet.flow_shard());
    }

    fn complete(
        &mut self,
        request_id: request_response::OutboundRequestId,
    ) -> Option<PacketInFlightRequest> {
        let request = self.requests.remove(&request_id)?;
        if let Some(state) = self.peers.get_mut(&request.peer) {
            state.complete(request.shard);
            if state.total == 0 {
                self.peers.remove(&request.peer);
            }
        }
        Some(request)
    }

    fn stats(&self) -> PacketInFlightStats {
        PacketInFlightStats {
            packets: self.requests.len(),
            peers: self.peers.len(),
            shards: self.peers.values().map(PeerInFlight::active_shards).sum(),
            limit_per_peer: self.limit_per_peer,
        }
    }

    #[cfg(test)]
    fn in_flight_for(&self, peer: PeerId) -> usize {
        self.peers.get(&peer).map_or(0, |state| state.total)
    }
}

#[derive(Clone, Debug, Default)]
struct PeerInFlight {
    total: usize,
    shards: HashMap<FlowShard, usize>,
}

impl PeerInFlight {
    fn record(&mut self, shard: FlowShard) {
        self.total += 1;
        *self.shards.entry(shard).or_default() += 1;
    }

    fn complete(&mut self, shard: FlowShard) {
        self.total = self.total.saturating_sub(1);
        if let Some(count) = self.shards.get_mut(&shard) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.shards.remove(&shard);
            }
        }
    }

    fn in_flight_for_shard(&self, shard: FlowShard) -> usize {
        self.shards.get(&shard).copied().unwrap_or(0)
    }

    fn active_shards(&self) -> usize {
        self.shards.len()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PacketInFlightStats {
    packets: usize,
    peers: usize,
    shards: usize,
    limit_per_peer: usize,
}

fn dial_blocked_queue_peers(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    queues: &PeerQueues,
    context: &QueueDrainContext<'_>,
) {
    let blocked_transport_peers = queues
        .queued_peers()
        .filter_map(|peer| forwarder.transport_peer_for_overlay(peer))
        .collect::<HashSet<_>>();
    if blocked_transport_peers.is_empty() {
        return;
    }

    redial_selected_addresses(
        swarm,
        &blocked_transport_peers,
        context.bootstrap_addresses,
        context.relay_addresses,
        context.configured_peer_addresses,
        context.discovered_peer_addresses,
        context.paths,
        context.metrics,
    );
}

fn should_redial_blocked_queue(last_redial: &mut Option<Instant>, now: Instant) -> bool {
    if last_redial.is_some_and(|last| now.duration_since(last) < BLOCKED_QUEUE_REDIAL_INTERVAL) {
        return false;
    }

    *last_redial = Some(now);
    true
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PacketTransportDecision {
    PacketPlaneDatagram {
        path: PathKind,
        backend: PacketDatagramBackend,
    },
    StreamFallback {
        path: PathKind,
    },
    Blocked {
        reason: PacketTransportBlockReason,
        best_path: Option<PathKind>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PacketDatagramBackend {
    OwnedQuic,
    OwnedUdp,
}

const fn packet_datagram_backend_name(backend: PacketDatagramBackend) -> &'static str {
    match backend {
        PacketDatagramBackend::OwnedQuic => "owned_quic",
        PacketDatagramBackend::OwnedUdp => "owned_udp",
    }
}

const fn packet_datagram_backend_path_kind(backend: PacketDatagramBackend) -> PathKind {
    match backend {
        PacketDatagramBackend::OwnedQuic => PathKind::DirectQuicDatagram,
        PacketDatagramBackend::OwnedUdp => PathKind::DirectUdpDatagram,
    }
}

#[derive(Debug)]
enum PacketPlaneSendError {
    MissingUdpRuntime,
    MissingQuicRuntime,
    Udp(PacketPlaneIoError),
    Quic(PacketPlaneQuicError),
}

impl PacketTransportDecision {
    const fn can_send(self) -> bool {
        matches!(
            self,
            Self::PacketPlaneDatagram { .. } | Self::StreamFallback { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PacketTransportBlockReason {
    MissingCapabilities,
    NoHealthyPath,
    LocalQuicDatagramsUnavailable,
}

fn packet_transport_decision(
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    peer: PeerId,
) -> PacketTransportDecision {
    if !peer_capabilities.contains(peer) {
        return PacketTransportDecision::Blocked {
            reason: PacketTransportBlockReason::MissingCapabilities,
            best_path: paths.best_for(peer).map(|path| path.kind),
        };
    }

    let datagram_backend =
        local_packet_datagram_backend(peer_capabilities, packet_plane, packet_plane_quic, peer);
    let local_datagrams = datagram_backend.is_some();
    let support = packet_transport_support_for_backend(peer_capabilities, peer, datagram_backend);
    if let Some(path) = best_packet_transport_path(paths, peer, support) {
        return if path.kind.requires_quic_datagrams() {
            PacketTransportDecision::PacketPlaneDatagram {
                path: path.kind,
                backend: datagram_backend.expect("supported datagram path requires backend"),
            }
        } else {
            PacketTransportDecision::StreamFallback { path: path.kind }
        };
    }

    let best_path = paths.best_for(peer).map(|path| path.kind);
    let reason = if best_path.is_some_and(PathKind::requires_quic_datagrams)
        && peer_capabilities.supports_datagram_packet_path_for(peer)
        && !local_datagrams
    {
        PacketTransportBlockReason::LocalQuicDatagramsUnavailable
    } else {
        PacketTransportBlockReason::NoHealthyPath
    };
    PacketTransportDecision::Blocked { reason, best_path }
}

fn best_packet_transport_path(
    paths: &PathSet,
    peer: PeerId,
    support: PathTransportSupport,
) -> Option<crate::path::PathCandidate> {
    let selected = paths.best_supported_for(peer, support)?;
    if selected.kind.requires_quic_datagrams()
        && let Some(stream_path) = paths.best_supported_for(
            peer,
            PathTransportSupport {
                udp_datagrams: false,
                quic_datagrams: false,
            },
        )
        && (selected.observed_rtt_ms.is_none() || stream_path.is_relay())
    {
        return Some(stream_path);
    }
    Some(selected)
}

fn local_packet_datagram_backend(
    peer_capabilities: &PeerCapabilities,
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    peer: PeerId,
) -> Option<PacketDatagramBackend> {
    if packet_plane_quic.is_some_and(|packet_plane| packet_plane.has_session(peer))
        && peer_capabilities.supports_owned_quic_packet_plane_for(peer)
    {
        return Some(PacketDatagramBackend::OwnedQuic);
    }
    if packet_plane.is_some_and(|packet_plane| packet_plane.has_session(peer))
        && peer_capabilities.supports_owned_udp_packet_plane_for(peer)
    {
        return Some(PacketDatagramBackend::OwnedUdp);
    }
    None
}

fn packet_plane_send_fallback_backend(
    failed_backend: PacketDatagramBackend,
    peer_capabilities: &PeerCapabilities,
    packet_plane: Option<&PacketPlaneRuntime>,
    peer: PeerId,
) -> Option<PacketDatagramBackend> {
    if failed_backend == PacketDatagramBackend::OwnedQuic
        && packet_plane.is_some_and(|packet_plane| packet_plane.has_session(peer))
        && peer_capabilities.supports_owned_udp_packet_plane_for(peer)
    {
        return Some(PacketDatagramBackend::OwnedUdp);
    }
    None
}

fn packet_plane_send_stream_fallback_path(
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
    peer: PeerId,
) -> Option<PathKind> {
    paths
        .best_supported_for(
            peer,
            packet_transport_support_for_backend(peer_capabilities, peer, None),
        )
        .and_then(|path| (!path.kind.requires_quic_datagrams()).then_some(path.kind))
}

fn packet_transport_support(
    peer_capabilities: &PeerCapabilities,
    peer: PeerId,
) -> PathTransportSupport {
    PathTransportSupport {
        udp_datagrams: false,
        quic_datagrams: local_packet_data_plane().native_quic_datagrams
            && peer_capabilities.supports_datagram_packet_path_for(peer),
    }
}

fn packet_transport_support_for_backend(
    peer_capabilities: &PeerCapabilities,
    peer: PeerId,
    backend: Option<PacketDatagramBackend>,
) -> PathTransportSupport {
    PathTransportSupport {
        udp_datagrams: backend == Some(PacketDatagramBackend::OwnedUdp)
            && peer_capabilities.supports_owned_udp_packet_plane_for(peer),
        quic_datagrams: backend == Some(PacketDatagramBackend::OwnedQuic)
            && peer_capabilities.supports_owned_quic_packet_plane_for(peer),
    }
}

struct SwarmEventContext<'a> {
    forwarder: &'a mut Forwarder,
    membership: &'a mut OverlayMembership,
    infrastructure_peers: &'a mut InfrastructurePeers,
    writer: &'a mut TunWriter,
    paths: &'a mut PathSet,
    peer_capabilities: &'a mut PeerCapabilities,
    relay_readiness: &'a mut RelayReadiness,
    auto_relay: &'a mut AutoRelayState,
    configured_peer_addresses: &'a [(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &'a mut DiscoveredPeerAddresses,
    packet_in_flight: &'a mut PacketInFlight,
    inbound_packet_rate_limiters: &'a mut PeerPacketRateLimiters,
    metrics: &'a RuntimeMetrics,
    local_capabilities: &'a mut ControlCapabilities,
    previous_membership_tags: &'a [String],
    discovery: &'a DiscoveryConfig,
    identity: &'a NodeIdentity,
    packet_plane: &'a mut PacketPlaneRuntime,
    packet_plane_quic: Option<&'a mut PacketPlaneQuicRuntime>,
    packet_plane_negotiator: &'a mut PacketPlaneNegotiator,
    packet_plane_session_ttl: Duration,
    packet_plane_replay_windows_per_session: usize,
}

#[derive(Clone, Copy)]
struct MembershipValidationScope<'a> {
    network: &'a str,
    current_tag: Option<&'a str>,
    previous_tags: &'a [String],
}

impl<'a> MembershipValidationScope<'a> {
    fn from_capabilities(
        capabilities: &'a ControlCapabilities,
        previous_tags: &'a [String],
    ) -> Self {
        Self {
            network: &capabilities.network_name,
            current_tag: capabilities.membership_tag.as_deref(),
            previous_tags,
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn handle_swarm_event(
    swarm: &mut Swarm<Behaviour>,
    mut context: SwarmEventContext<'_>,
    event: SwarmEvent<BehaviourEvent>,
) -> Result<(), RunnerError> {
    match event {
        SwarmEvent::Behaviour(BehaviourEvent::Control(event)) => {
            handle_control_event(swarm, &mut context, event).await?;
        }
        SwarmEvent::Behaviour(BehaviourEvent::Packet(event)) => {
            handle_packet_event(swarm, &mut context, event)?;
        }
        SwarmEvent::Behaviour(BehaviourEvent::Service(event)) => {
            handle_service_event(swarm, &mut context, event)?;
        }
        SwarmEvent::Behaviour(event) => {
            let mut behaviour_context = BehaviourEventContext {
                forwarder: context.forwarder,
                membership: context.membership,
                infrastructure_peers: context.infrastructure_peers,
                relay_readiness: context.relay_readiness,
                auto_relay: context.auto_relay,
                paths: context.paths,
                configured_peer_addresses: context.configured_peer_addresses,
                discovered_peer_addresses: context.discovered_peer_addresses,
                metrics: context.metrics,
                discovery: context.discovery,
                local_capabilities: context.local_capabilities,
                previous_membership_tags: context.previous_membership_tags,
                identity: context.identity,
            };
            handle_behaviour_event(swarm, &mut behaviour_context, event);
        }
        SwarmEvent::ConnectionEstablished {
            peer_id,
            endpoint,
            num_established,
            ..
        } => {
            match authorize_established_connection(
                context.membership,
                context.infrastructure_peers,
                context.metrics,
                peer_id,
                context.auto_relay.should_discover_candidates(),
            ) {
                EstablishedConnectionAuthorization::OverlayPeer => {}
                EstablishedConnectionAuthorization::InfrastructurePeer => {
                    log_runtime_event(
                        LogLevel::Info,
                        "relay_infrastructure_connection_established",
                        &[
                            ("peer", &peer_id.to_string()),
                            ("relayed", &endpoint.is_relayed().to_string()),
                        ],
                    );
                    return Ok(());
                }
                EstablishedConnectionAuthorization::InfrastructureProbe => {
                    log_runtime_event(
                        LogLevel::Info,
                        "auto_relay_infrastructure_probe_connection_established",
                        &[
                            ("peer", &peer_id.to_string()),
                            ("relayed", &endpoint.is_relayed().to_string()),
                        ],
                    );
                    return Ok(());
                }
                EstablishedConnectionAuthorization::Rejected => {
                    log_runtime_event(
                        LogLevel::Warn,
                        "unauthorized_peer_disconnected",
                        &[("peer", &peer_id.to_string())],
                    );
                    if swarm.disconnect_peer_id(peer_id).is_err() {
                        log_runtime_event(
                            LogLevel::Warn,
                            "unauthorized_peer_already_disconnected",
                            &[("peer", &peer_id.to_string())],
                        );
                    }
                    return Ok(());
                }
            }
            invalidate_peer_capabilities_on_first_connection(
                context.forwarder,
                context.peer_capabilities,
                peer_id,
                num_established.get(),
            );
            advertise_direct_packet_plane_endpoint_from_path(
                context.local_capabilities,
                context.packet_plane.primary_listener(),
                &endpoint,
                context.metrics,
            );
            record_path_established_and_maybe_send_packet_plane_hello(
                swarm,
                context.paths,
                context.forwarder,
                context.peer_capabilities,
                context.metrics,
                context.local_capabilities,
                context.identity,
                context.packet_plane,
                context.packet_plane_quic.as_deref(),
                context.packet_plane_negotiator,
                peer_id,
                &endpoint,
            );
            send_control_capabilities(
                swarm,
                context.forwarder,
                peer_id,
                context.local_capabilities,
                context.metrics,
            );
            send_service_status_request(
                swarm,
                context.forwarder,
                peer_id,
                context.local_capabilities,
                context.metrics,
            );
            context
                .metrics
                .record_connection_established(endpoint.is_relayed());
            log_runtime_event(
                LogLevel::Info,
                "connection_established",
                &[
                    ("peer", &peer_id.to_string()),
                    ("relayed", &endpoint.is_relayed().to_string()),
                    ("endpoint", &format!("{endpoint:?}")),
                ],
            );
        }
        SwarmEvent::ConnectionClosed {
            peer_id,
            endpoint,
            num_established,
            ..
        } => {
            record_path_closed(
                context.paths,
                context.forwarder,
                context.metrics,
                peer_id,
                &endpoint,
            );
            invalidate_peer_capabilities_when_disconnected(
                context.forwarder,
                context.peer_capabilities,
                peer_id,
                num_established,
            );
            if num_established == 0 {
                context.inbound_packet_rate_limiters.remove(peer_id);
                context
                    .packet_plane_negotiator
                    .remove_peer(PeerId::from_libp2p(peer_id));
                if context
                    .relay_readiness
                    .record_relay_reservation_lost(peer_id)
                {
                    context
                        .auto_relay
                        .release_reservation_for_retry_after(peer_id, Instant::now());
                    context.metrics.record_relay_reservation_lost();
                    log_runtime_event(
                        LogLevel::Warn,
                        "relay_reservation_readiness_lost",
                        &[("relay", &peer_id.to_string())],
                    );
                }
            }
            log_runtime_event(
                LogLevel::Info,
                "connection_closed",
                &[
                    ("peer", &peer_id.to_string()),
                    ("relayed", &endpoint.is_relayed().to_string()),
                    ("endpoint", &format!("{endpoint:?}")),
                ],
            );
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            let peer_connected = peer_id.is_some_and(|peer_id| swarm.is_connected(&peer_id));
            handle_relay_infrastructure_outgoing_connection_error(
                context.infrastructure_peers,
                context.auto_relay,
                context.membership,
                context.metrics,
                peer_id,
                peer_connected,
                &error,
            );
        }
        SwarmEvent::NewExternalAddrCandidate { address } => {
            context.metrics.record_external_address_candidate();
            log_runtime_event(
                LogLevel::Info,
                "external_address_candidate",
                &[("address", &address.to_string())],
            );
        }
        SwarmEvent::ExternalAddrConfirmed { address } => {
            context.metrics.record_external_address_confirmed();
            let packet_plane_quic_snapshot = context
                .packet_plane_quic
                .as_deref()
                .map(PacketPlaneQuicRuntime::snapshot);
            let observed_endpoint_update = update_observed_packet_plane_endpoints(
                context.local_capabilities,
                &address,
                context.packet_plane.primary_listener(),
                packet_plane_quic_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.listener),
                packet_plane_quic_snapshot.and_then(|snapshot| snapshot.certificate_der),
            );
            if observed_endpoint_update.public_address_accepted {
                context
                    .metrics
                    .record_observed_packet_plane_external_address();
            } else {
                context
                    .metrics
                    .record_observed_packet_plane_external_address_rejected();
            }
            if observed_endpoint_update.udp_candidate_added {
                context
                    .metrics
                    .record_observed_packet_plane_udp_endpoint_candidate();
            }
            if observed_endpoint_update.quic_candidate_added {
                context
                    .metrics
                    .record_observed_packet_plane_quic_endpoint_candidate();
            }
            if observed_endpoint_update.changed() {
                log_runtime_event(
                    LogLevel::Info,
                    "observed_packet_plane_endpoints_advertised",
                    &[
                        ("address", &address.to_string()),
                        (
                            "udp_candidates",
                            &context
                                .local_capabilities
                                .packet_endpoint_candidates
                                .len()
                                .to_string(),
                        ),
                        (
                            "quic_candidates",
                            &context
                                .local_capabilities
                                .owned_quic_packet_endpoint_candidates
                                .len()
                                .to_string(),
                        ),
                    ],
                );
                send_control_capabilities_to_connected_peers(
                    swarm,
                    context.forwarder,
                    context.local_capabilities,
                    context.metrics,
                );
            }
            log_runtime_event(
                LogLevel::Info,
                "external_address_confirmed",
                &[("address", &address.to_string())],
            );
        }
        SwarmEvent::ExternalAddrExpired { address } => {
            context.metrics.record_external_address_expired();
            log_runtime_event(
                LogLevel::Info,
                "external_address_expired",
                &[("address", &address.to_string())],
            );
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            if let Some(relay) = relayed_address_relay_peer(&address) {
                context.relay_readiness.record_relay_listen_address(relay);
                dial_relay_ready_configured_peers(
                    swarm,
                    context.relay_readiness,
                    context.configured_peer_addresses,
                    context.discovered_peer_addresses,
                    context.metrics,
                    relay,
                );
                log_runtime_event(
                    LogLevel::Info,
                    "relay_listen_address_ready",
                    &[
                        ("relay", &relay.to_string()),
                        ("address", &address.to_string()),
                    ],
                );
                publish_kademlia_peer_address_record_for_capabilities(
                    swarm,
                    context.discovery,
                    context.local_capabilities,
                    context.identity,
                );
            }
        }
        SwarmEvent::ExpiredListenAddr { address, .. } => {
            if let Some(relay) = relayed_address_relay_peer(&address)
                && context
                    .relay_readiness
                    .record_relay_listen_address_lost(relay)
            {
                log_runtime_event(
                    LogLevel::Warn,
                    "relay_listen_address_lost",
                    &[
                        ("relay", &relay.to_string()),
                        ("address", &address.to_string()),
                    ],
                );
                publish_kademlia_peer_address_record_for_capabilities(
                    swarm,
                    context.discovery,
                    context.local_capabilities,
                    context.identity,
                );
            }
        }
        SwarmEvent::ListenerClosed { addresses, .. } => {
            for address in addresses {
                if let Some(relay) = relayed_address_relay_peer(&address)
                    && context
                        .relay_readiness
                        .record_relay_listen_address_lost(relay)
                {
                    log_runtime_event(
                        LogLevel::Warn,
                        "relay_listen_address_lost",
                        &[
                            ("relay", &relay.to_string()),
                            ("address", &address.to_string()),
                        ],
                    );
                    publish_kademlia_peer_address_record_for_capabilities(
                        swarm,
                        context.discovery,
                        context.local_capabilities,
                        context.identity,
                    );
                }
            }
        }
        _ => {}
    }

    Ok(())
}

fn handle_outgoing_connection_error(
    metrics: &RuntimeMetrics,
    peer_id: Option<Libp2pPeerId>,
    error: &impl std::fmt::Display,
) {
    metrics.record_outgoing_connection_error();
    match peer_id {
        Some(peer_id) => eprintln!("outgoing connection to {peer_id} failed: {error}"),
        None => eprintln!("outgoing connection failed: {error}"),
    }
}

fn handle_relay_infrastructure_outgoing_connection_error(
    infrastructure_peers: &mut InfrastructurePeers,
    auto_relay: &mut AutoRelayState,
    membership: &OverlayMembership,
    metrics: &RuntimeMetrics,
    peer_id: Option<Libp2pPeerId>,
    peer_connected: bool,
    error: &impl std::fmt::Display,
) {
    handle_outgoing_connection_error(metrics, peer_id, error);
    let Some(peer_id) = peer_id else {
        return;
    };
    if peer_connected || membership.allows(peer_id) || !infrastructure_peers.remove(peer_id) {
        return;
    }
    auto_relay.remove_candidate(peer_id);

    metrics.record_auto_relay_infrastructure_dial_failure();
    log_runtime_event(
        LogLevel::Warn,
        "auto_relay_infrastructure_dial_failed_async",
        &[
            ("peer", &peer_id.to_string()),
            ("error", &error.to_string()),
        ],
    );
}

async fn handle_control_event(
    swarm: &mut Swarm<Behaviour>,
    context: &mut SwarmEventContext<'_>,
    event: request_response::Event<ControlRequest, ControlResponse>,
) -> Result<(), RunnerError> {
    match event {
        request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        } => {
            handle_control_request(swarm, context, peer, request, channel).await?;
        }
        request_response::Event::Message {
            peer,
            message: Message::Response { response, .. },
            ..
        } => {
            let validation_scope = MembershipValidationScope::from_capabilities(
                context.local_capabilities,
                context.previous_membership_tags,
            );
            handle_control_response_event(
                swarm,
                context.forwarder,
                context.membership,
                context.peer_capabilities,
                context.metrics,
                peer,
                response,
                validation_scope,
                context.local_capabilities,
                context.packet_plane,
                context.packet_plane_quic.as_deref_mut(),
                context.packet_plane_negotiator,
                context.identity,
                context.paths,
            );
        }
        request_response::Event::OutboundFailure { peer, error, .. } => {
            context.metrics.record_control_failure();
            eprintln!("control request to {peer} failed: {error}");
        }
        request_response::Event::InboundFailure { peer, error, .. } => {
            context.metrics.record_control_failure();
            eprintln!("control request from {peer} failed: {error}");
        }
        request_response::Event::ResponseSent { .. } => {}
    }

    Ok(())
}

fn handle_packet_event(
    swarm: &mut Swarm<Behaviour>,
    context: &mut SwarmEventContext<'_>,
    event: request_response::Event<crate::wire::Frame, crate::runtime::packet::PacketResponse>,
) -> Result<(), RunnerError> {
    match event {
        request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        } => handle_packet_request(swarm, context, peer, &request, channel)?,
        request_response::Event::Message {
            peer,
            message:
                Message::Response {
                    request_id,
                    response,
                },
            ..
        } => {
            let in_flight = context.packet_in_flight.complete(request_id);
            match response {
                PacketResponse::Accepted => {
                    if let Some(request) = in_flight {
                        let rtt_ms = request
                            .sent_at
                            .elapsed()
                            .as_millis()
                            .min(u128::from(u16::MAX)) as u16;
                        let change = context.paths.record_rtt(request.peer, request.path, rtt_ms);
                        record_path_selection_change(context.metrics, change);
                    }
                }
                PacketResponse::Rejected(reason) => {
                    context.metrics.record_outbound_failure();
                    context
                        .metrics
                        .record_outbound_drop(packet_rejection_drop_reason(reason));
                    audit_packet_response_rejection(peer, reason);
                }
            }
        }
        request_response::Event::OutboundFailure {
            peer,
            request_id,
            error,
            ..
        } => {
            let in_flight = context.packet_in_flight.complete(request_id);
            context.metrics.record_outbound_failure();
            if let Some(request) = in_flight {
                maybe_demote_stream_fallback_path(
                    context.paths,
                    context.metrics,
                    request.peer,
                    request.path,
                    &error,
                );
                if let Some(peer) = context.forwarder.transport_peer_for_overlay(request.peer) {
                    let discovered_addresses = context.discovered_peer_addresses.as_vec();
                    redial_packet_plane_recovery_addresses(
                        swarm,
                        peer,
                        context.configured_peer_addresses,
                        &discovered_addresses,
                        context.metrics,
                    );
                }
            }
            eprintln!("packet request to {peer} failed: {error}");
        }
        request_response::Event::InboundFailure { peer, error, .. } => {
            context.metrics.record_inbound_failure();
            eprintln!("packet request from {peer} failed: {error}");
        }
        request_response::Event::ResponseSent { .. } => {}
    }

    Ok(())
}

fn handle_packet_request(
    swarm: &mut Swarm<Behaviour>,
    context: &mut SwarmEventContext<'_>,
    peer: Libp2pPeerId,
    request: &crate::wire::Frame,
    channel: request_response::ResponseChannel<PacketResponse>,
) -> Result<(), RunnerError> {
    if !context
        .inbound_packet_rate_limiters
        .allow(peer, Instant::now())
    {
        context
            .metrics
            .record_inbound_drop(PacketDropReason::RateLimited);
        audit_packet_rate_limit_rejection(
            peer,
            request,
            context.inbound_packet_rate_limiters.limit_per_second(),
        );
        return Forwarder::send_packet_response(
            swarm,
            channel,
            PacketResponse::Rejected(PacketRejectionReason::RateLimited),
        )
        .map_err(|_| RunnerError::PacketResponseDropped);
    }

    let result = match request.header.payload_type {
        PayloadType::IpPacket => match context
            .forwarder
            .accept_inbound_stream_packet(peer, request)
        {
            Ok(packet) => {
                context.writer.write_packet(packet)?;
                context.metrics.record_tun_write(packet.len());
                context.metrics.record_inbound_accepted();
                Ok(())
            }
            Err(error) => Err(error),
        },
        PayloadType::Keepalive => context
            .forwarder
            .accept_inbound_control_frame(peer, request, PayloadType::Keepalive)
            .map(|()| context.metrics.record_inbound_keepalive_accepted()),
        PayloadType::PathProbe => context
            .forwarder
            .accept_inbound_control_frame(peer, request, PayloadType::PathProbe)
            .map(|()| context.metrics.record_inbound_path_probe_accepted()),
    };

    match result {
        Ok(()) => Forwarder::send_packet_response(swarm, channel, PacketResponse::Accepted)
            .map_err(|_| RunnerError::PacketResponseDropped),
        Err(error) => {
            let drop_reason = inbound_drop_reason(&error);
            context.metrics.record_inbound_drop(drop_reason);
            audit_packet_request_rejection(peer, request, &error);
            Forwarder::send_packet_response(
                swarm,
                channel,
                PacketResponse::Rejected(packet_rejection_reason(drop_reason)),
            )
            .map_err(|_| RunnerError::PacketResponseDropped)
        }
    }
}

struct PacketPlaneInboundContext<'a> {
    forwarder: &'a mut Forwarder,
    writer: &'a mut TunWriter,
    paths: &'a mut PathSet,
    peer_capabilities: &'a PeerCapabilities,
    inbound_packet_rate_limiters: &'a mut PeerPacketRateLimiters,
    packet_plane: Option<&'a PacketPlaneRuntime>,
    packet_plane_quic: Option<&'a PacketPlaneQuicRuntime>,
    backend: PacketDatagramBackend,
    path_probe_tracker: &'a mut PathProbeTracker,
    metrics: &'a RuntimeMetrics,
}

async fn handle_packet_plane_received(
    context: &mut PacketPlaneInboundContext<'_>,
    received: &PacketPlaneReceivedFrame,
) -> Result<(), RunnerError> {
    let Some(overlay_peer) = received.peer else {
        context
            .metrics
            .record_inbound_drop(PacketDropReason::UnauthorizedPeer);
        log_runtime_event(
            LogLevel::Warn,
            "packet_plane_rejected",
            &[("reason", "missing_peer")],
        );
        return Ok(());
    };
    let Some(transport_peer) = context.forwarder.transport_peer_for_overlay(overlay_peer) else {
        context
            .metrics
            .record_inbound_drop(PacketDropReason::UnauthorizedPeer);
        log_runtime_event(
            LogLevel::Warn,
            "packet_plane_rejected",
            &[
                ("peer", &overlay_peer.to_string()),
                ("reason", "unknown_overlay_peer"),
            ],
        );
        return Ok(());
    };
    if !context
        .inbound_packet_rate_limiters
        .allow(transport_peer, Instant::now())
    {
        context
            .metrics
            .record_inbound_drop(PacketDropReason::RateLimited);
        audit_packet_rate_limit_rejection(
            transport_peer,
            &received.frame,
            context.inbound_packet_rate_limiters.limit_per_second(),
        );
        return Ok(());
    }

    let result = match received.frame.header.payload_type {
        PayloadType::IpPacket => match context
            .forwarder
            .accept_inbound_packet(transport_peer, &received.frame)
        {
            Ok(packet) => {
                context.writer.write_packet(packet)?;
                context.metrics.record_tun_write(packet.len());
                context.metrics.record_inbound_accepted();
                Ok(())
            }
            Err(error) => Err(error),
        },
        PayloadType::Keepalive => context
            .forwarder
            .accept_inbound_control_frame(transport_peer, &received.frame, PayloadType::Keepalive)
            .map(|()| context.metrics.record_inbound_keepalive_accepted()),
        PayloadType::PathProbe => context
            .forwarder
            .accept_inbound_control_frame(transport_peer, &received.frame, PayloadType::PathProbe)
            .map(|()| context.metrics.record_inbound_path_probe_accepted()),
    };

    if let Err(error) = result {
        let drop_reason = inbound_drop_reason(&error);
        context.metrics.record_inbound_drop(drop_reason);
        audit_packet_request_rejection(transport_peer, &received.frame, &error);
    } else if received.frame.header.payload_type == PayloadType::PathProbe {
        handle_packet_plane_path_probe(
            context.forwarder,
            context.paths,
            context.peer_capabilities,
            context.packet_plane,
            context.packet_plane_quic,
            context.backend,
            context.path_probe_tracker,
            context.metrics,
            overlay_peer,
            received,
        )
        .await;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_packet_plane_path_probe(
    forwarder: &mut Forwarder,
    paths: &mut PathSet,
    peer_capabilities: &PeerCapabilities,
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    backend: PacketDatagramBackend,
    path_probe_tracker: &mut PathProbeTracker,
    metrics: &RuntimeMetrics,
    overlay_peer: PeerId,
    received: &PacketPlaneReceivedFrame,
) {
    if let Some(confirmed_mtu) = path_probe_ack_mtu(&received.frame.payload) {
        if let Some(token) = path_probe_ack_token(&received.frame.payload)
            && let Some((path, rtt_ms)) =
                path_probe_tracker.confirm(overlay_peer, token, Instant::now())
        {
            let change = paths.record_rtt(overlay_peer, path, rtt_ms);
            record_path_selection_change(metrics, change);
            log_runtime_event(
                LogLevel::Info,
                "path_probe_rtt_confirmed",
                &[
                    ("peer", &overlay_peer.to_string()),
                    ("path", path.wire_name()),
                    ("rtt_ms", &rtt_ms.to_string()),
                ],
            );
        }
        maybe_confirm_path_mtu_probe(
            paths,
            peer_capabilities,
            packet_plane,
            packet_plane_quic,
            backend,
            metrics,
            overlay_peer,
            confirmed_mtu,
            u16::try_from(forwarder.mtu()).unwrap_or(u16::MAX),
        );
        return;
    }

    let Some(probed_mtu) = path_probe_request_mtu(&received.frame.payload) else {
        return;
    };
    let ack_payload = path_probe_ack_payload(
        probed_mtu,
        path_probe_request_token(&received.frame.payload),
    );
    let peer_mtu = selected_path_mtu(
        paths,
        peer_capabilities,
        packet_plane,
        packet_plane_quic,
        overlay_peer,
        u16::try_from(forwarder.mtu()).unwrap_or(u16::MAX),
    );
    match forwarder.path_probe_frame_with_mtu(peer_mtu, &ack_payload) {
        Ok(frame) => {
            if let Err(error) = send_packet_plane_frame(
                packet_plane,
                packet_plane_quic,
                backend,
                overlay_peer,
                &frame,
            )
            .await
            {
                metrics.record_outbound_path_probe_failure();
                eprintln!("packet-plane path probe ack to {overlay_peer} failed: {error:?}");
            } else {
                metrics.record_outbound_path_probe_ack_sent();
            }
        }
        Err(error) => {
            metrics.record_outbound_path_probe_failure();
            eprintln!("packet-plane path probe ack to {overlay_peer} failed: {error:?}");
        }
    }
}

fn handle_packet_plane_receive_error(metrics: &RuntimeMetrics, error: &PacketPlaneIoError) {
    if let PacketPlaneIoError::Io(error) = error {
        metrics.record_inbound_failure();
        log_runtime_event(
            LogLevel::Warn,
            "packet_plane_receive_failed",
            &[("error", &format!("{error:?}"))],
        );
    } else {
        metrics.record_inbound_drop(packet_plane_inbound_drop_reason(error));
        metrics.record_packet_plane_inbound_drop(packet_plane_inbound_metric_reason(error));
        log_runtime_event(
            LogLevel::Warn,
            "packet_plane_rejected",
            &[("reason", packet_plane_io_error_name(error))],
        );
    }
}

fn handle_packet_plane_quic_receive_error(
    paths: &mut PathSet,
    metrics: &RuntimeMetrics,
    error: &PacketPlaneQuicError,
) {
    maybe_demote_packet_plane_quic_receive_path(paths, metrics, error);

    match error {
        PacketPlaneQuicError::Io(_)
        | PacketPlaneQuicError::Connection(_)
        | PacketPlaneQuicError::PeerConnection { .. }
        | PacketPlaneQuicError::EndpointClosed => {
            metrics.record_inbound_failure();
            log_runtime_event(
                LogLevel::Warn,
                "packet_plane_quic_receive_failed",
                &[("reason", packet_plane_quic_error_name(error))],
            );
        }
        PacketPlaneQuicError::Datagram(error) => {
            metrics.record_inbound_drop(packet_plane_datagram_inbound_drop_reason(error));
            metrics.record_packet_plane_inbound_drop(packet_plane_datagram_metric_reason(error));
            log_runtime_event(
                LogLevel::Warn,
                "packet_plane_quic_rejected",
                &[("reason", packet_plane_datagram_error_name(error))],
            );
        }
        PacketPlaneQuicError::NoConnection { .. } | PacketPlaneQuicError::NoSessions => {
            metrics.record_inbound_drop(PacketDropReason::UnauthorizedPeer);
            log_runtime_event(
                LogLevel::Warn,
                "packet_plane_quic_rejected",
                &[("reason", packet_plane_quic_error_name(error))],
            );
        }
        PacketPlaneQuicError::Connect(_)
        | PacketPlaneQuicError::Certificate(_)
        | PacketPlaneQuicError::Rustls(_)
        | PacketPlaneQuicError::ClientVerifier(_)
        | PacketPlaneQuicError::SendDatagram(_)
        | PacketPlaneQuicError::Session(_) => {
            metrics.record_inbound_drop(PacketDropReason::MalformedPacket);
            log_runtime_event(
                LogLevel::Warn,
                "packet_plane_quic_rejected",
                &[("reason", packet_plane_quic_error_name(error))],
            );
        }
    }
}

fn handle_service_event(
    swarm: &mut Swarm<Behaviour>,
    context: &mut SwarmEventContext<'_>,
    event: request_response::Event<ServiceRequest, ServiceResponse>,
) -> Result<(), RunnerError> {
    match event {
        request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        } => {
            handle_service_request(swarm, context, peer, request, channel)?;
        }
        request_response::Event::Message {
            peer,
            message: Message::Response { response, .. },
            ..
        } => {
            handle_service_response(
                context.metrics,
                peer,
                response,
                &context.local_capabilities.network_name,
                context.local_capabilities.membership_tag.as_deref(),
                context.previous_membership_tags,
            );
        }
        request_response::Event::OutboundFailure { peer, error, .. } => {
            context.metrics.record_service_failure();
            eprintln!("service request to {peer} failed: {error}");
        }
        request_response::Event::InboundFailure { peer, error, .. } => {
            context.metrics.record_service_failure();
            eprintln!("service request from {peer} failed: {error}");
        }
        request_response::Event::ResponseSent { .. } => {}
    }

    Ok(())
}

async fn handle_control_request(
    swarm: &mut Swarm<Behaviour>,
    context: &mut SwarmEventContext<'_>,
    peer: Libp2pPeerId,
    request: ControlRequest,
    channel: request_response::ResponseChannel<ControlResponse>,
) -> Result<(), RunnerError> {
    match request {
        ControlRequest::Capabilities(capabilities) => {
            context.metrics.record_control_request_received();
            eprintln!("control capabilities from {peer}: {capabilities:?}");
            let response = capability_response_for_peer_with_membership_records(
                context.forwarder,
                context.membership,
                peer,
                &capabilities,
                context.local_capabilities,
                context.previous_membership_tags,
            );
            match &response {
                ControlResponse::CapabilitiesAccepted(_) => {
                    record_peer_capabilities(
                        context.forwarder,
                        context.peer_capabilities,
                        peer,
                        capabilities.clone(),
                    );
                    context.metrics.record_control_capability_accept();
                }
                ControlResponse::CapabilitiesRejected(reason) => {
                    context.metrics.record_control_capability_rejection(*reason);
                    context.metrics.record_control_failure();
                    audit_control_capabilities_rejection(peer, *reason);
                }
                ControlResponse::PacketPlaneAccepted(_)
                | ControlResponse::PacketPlaneRejected(_) => {}
            }
            swarm
                .behaviour_mut()
                .control
                .send_response(channel, response)
                .map_err(|_| RunnerError::ControlResponseDropped)?;
        }
        ControlRequest::PacketPlaneHello(handshake) => {
            context.metrics.record_control_request_received();
            let response = packet_plane_accept_response_for_peer(
                PacketPlaneAcceptContext {
                    forwarder: context.forwarder,
                    peer_capabilities: context.peer_capabilities,
                    paths: context.paths,
                    metrics: context.metrics,
                    packet_plane: context.packet_plane,
                    packet_plane_quic: context.packet_plane_quic.as_deref_mut(),
                    identity: context.identity,
                    local_capabilities: context.local_capabilities,
                },
                peer,
                &handshake,
            )
            .await;
            if matches!(response, ControlResponse::PacketPlaneRejected(_)) {
                context.metrics.record_control_failure();
            }
            swarm
                .behaviour_mut()
                .control
                .send_response(channel, response)
                .map_err(|_| RunnerError::ControlResponseDropped)?;
        }
    }

    Ok(())
}

fn handle_service_request(
    swarm: &mut Swarm<Behaviour>,
    context: &mut SwarmEventContext<'_>,
    peer: Libp2pPeerId,
    request: ServiceRequest,
    channel: request_response::ResponseChannel<ServiceResponse>,
) -> Result<(), RunnerError> {
    match request {
        ServiceRequest::Status(request) => {
            context.metrics.record_service_request_received();
            eprintln!("service status request from {peer}: {request:?}");
            let response = service_status_response_for_peer(
                context.forwarder,
                context.paths,
                context.peer_capabilities,
                peer,
                &request,
                context.local_capabilities,
                context.previous_membership_tags,
                context.packet_plane_session_ttl,
                context.packet_plane_replay_windows_per_session,
            );
            match &response {
                ServiceResponse::Status(_) => context.metrics.record_service_status_accept(),
                ServiceResponse::Rejected(reason) => {
                    context.metrics.record_service_status_rejection(*reason);
                    context.metrics.record_service_failure();
                    audit_service_status_rejection(peer, *reason);
                }
            }
            swarm
                .behaviour_mut()
                .service
                .send_response(channel, response)
                .map_err(|_| RunnerError::ServiceResponseDropped)?;
        }
    }

    Ok(())
}

fn handle_service_response(
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    response: ServiceResponse,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) {
    metrics.record_service_response_received();
    match response {
        ServiceResponse::Status(status) => {
            if let Some(reason) = validate_status_response(
                &status,
                expected_network,
                expected_membership_tag,
                previous_membership_tags,
            ) {
                metrics.record_service_status_rejection(reason);
                metrics.record_service_failure();
                eprintln!("ignoring incompatible service status response from {peer}: {reason:?}");
            } else {
                metrics.record_service_status_accept();
                eprintln!("service status response from {peer}: {status:?}");
            }
        }
        ServiceResponse::Rejected(reason) => {
            metrics.record_service_status_rejection(reason);
            metrics.record_service_failure();
            eprintln!("service status request rejected by {peer}: {reason:?}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_control_response_event(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    peer_capabilities: &mut PeerCapabilities,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    response: ControlResponse,
    validation: MembershipValidationScope<'_>,
    local_capabilities: &ControlCapabilities,
    packet_plane: &mut PacketPlaneRuntime,
    packet_plane_quic: Option<&mut PacketPlaneQuicRuntime>,
    packet_plane_negotiator: &mut PacketPlaneNegotiator,
    identity: &NodeIdentity,
    paths: &mut PathSet,
) {
    metrics.record_control_response_received();
    match response {
        ControlResponse::CapabilitiesAccepted(capabilities) => {
            if let Some(reason) = validate_peer_capabilities(
                forwarder,
                membership,
                peer,
                &capabilities,
                validation.network,
                validation.current_tag,
                validation.previous_tags,
            ) {
                metrics.record_control_capability_rejection(reason);
                metrics.record_control_failure();
                eprintln!("ignoring incompatible control acceptance from {peer}: {reason:?}");
            } else {
                record_peer_capabilities(forwarder, peer_capabilities, peer, capabilities.clone());
                metrics.record_control_capability_accept();
                eprintln!("control capabilities accepted by {peer}: {capabilities:?}");
                maybe_send_packet_plane_hello(
                    swarm,
                    forwarder,
                    paths,
                    packet_plane,
                    packet_plane_quic.as_deref(),
                    packet_plane_negotiator,
                    identity,
                    local_capabilities,
                    peer,
                    &capabilities,
                    metrics,
                );
            }
        }
        ControlResponse::CapabilitiesRejected(reason) => {
            metrics.record_control_capability_rejection(reason);
            metrics.record_control_failure();
            eprintln!("control capabilities rejected by {peer}: {reason:?}");
        }
        ControlResponse::PacketPlaneAccepted(handshake) => {
            if let Err(error) = complete_packet_plane_hello(
                &mut PacketPlaneCompleteContext {
                    forwarder,
                    peer_capabilities,
                    packet_plane,
                    packet_plane_quic,
                    negotiator: packet_plane_negotiator,
                    paths,
                    metrics,
                    network_name: validation.network,
                },
                peer,
                &handshake,
            ) {
                packet_plane_negotiator.remove_peer(PeerId::from_libp2p(peer));
                metrics.record_control_failure();
                eprintln!(
                    "packet-plane accept from {peer} failed: {}",
                    error.describe()
                );
            }
        }
        ControlResponse::PacketPlaneRejected(reason) => {
            packet_plane_negotiator.remove_peer(PeerId::from_libp2p(peer));
            metrics.record_control_capability_rejection(reason);
            metrics.record_control_failure();
            eprintln!("packet-plane hello rejected by {peer}: {reason:?}");
        }
    }
}

#[cfg(test)]
fn handle_control_response(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    response: ControlResponse,
    validation: MembershipValidationScope<'_>,
) {
    metrics.record_control_response_received();
    match response {
        ControlResponse::CapabilitiesAccepted(capabilities) => {
            let reason = validate_capabilities(
                &capabilities,
                validation.network,
                validation.current_tag,
                validation.previous_tags,
            )
            .or_else(|| {
                (!forwarder.authorizes_advertised_routes(peer, &capabilities.advertised_routes))
                    .then_some(ControlRejectionReason::UnauthorizedRouteAdvertisement)
            });
            if let Some(reason) = reason {
                metrics.record_control_capability_rejection(reason);
                metrics.record_control_failure();
                eprintln!("ignoring incompatible control acceptance from {peer}: {reason:?}");
            } else {
                record_peer_capabilities(forwarder, peer_capabilities, peer, capabilities.clone());
                metrics.record_control_capability_accept();
                eprintln!("control capabilities accepted by {peer}: {capabilities:?}");
            }
        }
        ControlResponse::CapabilitiesRejected(reason) => {
            metrics.record_control_capability_rejection(reason);
            metrics.record_control_failure();
            eprintln!("control capabilities rejected by {peer}: {reason:?}");
        }
        ControlResponse::PacketPlaneAccepted(_) => {
            metrics.record_control_failure();
            eprintln!("unexpected packet-plane accept response from {peer}");
        }
        ControlResponse::PacketPlaneRejected(reason) => {
            metrics.record_control_capability_rejection(reason);
            metrics.record_control_failure();
            eprintln!("packet-plane hello rejected by {peer}: {reason:?}");
        }
    }
}

fn capability_response_for_peer_with_membership_records(
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    peer: Libp2pPeerId,
    capabilities: &ControlCapabilities,
    local_capabilities: &ControlCapabilities,
    previous_membership_tags: &[String],
) -> ControlResponse {
    if !forwarder.is_configured_transport_peer(peer) {
        return rejected_capabilities_response(ControlRejectionReason::UnauthorizedPeer);
    }

    if let Some(reason) = validate_peer_capabilities(
        forwarder,
        membership,
        peer,
        capabilities,
        &local_capabilities.network_name,
        local_capabilities.membership_tag.as_deref(),
        previous_membership_tags,
    ) {
        return rejected_capabilities_response(reason);
    }

    accepted_capabilities_response(&refreshed_local_capabilities(local_capabilities, forwarder))
}

#[cfg(test)]
fn capability_response_for_peer(
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    capabilities: &ControlCapabilities,
    local_capabilities: &ControlCapabilities,
    previous_membership_tags: &[String],
) -> ControlResponse {
    if !forwarder.is_configured_transport_peer(peer) {
        return rejected_capabilities_response(ControlRejectionReason::UnauthorizedPeer);
    }

    if let Some(reason) = validate_capabilities(
        capabilities,
        &local_capabilities.network_name,
        local_capabilities.membership_tag.as_deref(),
        previous_membership_tags,
    ) {
        return rejected_capabilities_response(reason);
    }

    if !forwarder.authorizes_advertised_routes(peer, &capabilities.advertised_routes) {
        return rejected_capabilities_response(
            ControlRejectionReason::UnauthorizedRouteAdvertisement,
        );
    }

    accepted_capabilities_response(local_capabilities)
}

fn learn_membership_records_from_capabilities(
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    capabilities: &ControlCapabilities,
) -> Result<(), ForwardError> {
    let now_unix_seconds = current_unix_seconds_lossy();
    let stats =
        forwarder.merge_membership_records(&capabilities.member_records, now_unix_seconds)?;
    if stats.accepted > 0 || stats.removed_expired > 0 {
        membership.replace_record_members(
            forwarder.config(),
            forwarder.member_records(),
            now_unix_seconds,
        )?;
        let accepted = stats.accepted.to_string();
        let ignored = stats.ignored_stale_or_equal.to_string();
        let removed_expired = stats.removed_expired.to_string();
        log_runtime_event(
            LogLevel::Info,
            "membership_records_merged",
            &[
                ("accepted", &accepted),
                ("ignored_stale_or_equal", &ignored),
                ("removed_expired", &removed_expired),
            ],
        );
    }
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct KademliaMembershipRecordBundle {
    version: u8,
    network_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    membership_tag: Option<String>,
    records: Vec<SignedMembershipRecord>,
}

#[derive(Debug)]
enum KademliaMembershipRecordError {
    TooLarge,
    Decode,
    UnsupportedVersion,
    WrongNetwork,
    WrongMembershipScope,
    TooManyRecords,
    InvalidRecord(String),
}

impl std::fmt::Display for KademliaMembershipRecordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => formatter.write_str("too_large"),
            Self::Decode => formatter.write_str("decode_failed"),
            Self::UnsupportedVersion => formatter.write_str("unsupported_version"),
            Self::WrongNetwork => formatter.write_str("wrong_network"),
            Self::WrongMembershipScope => formatter.write_str("wrong_membership_scope"),
            Self::TooManyRecords => formatter.write_str("too_many_records"),
            Self::InvalidRecord(error) => write!(formatter, "invalid_record:{error}"),
        }
    }
}

fn encode_kademlia_membership_records(
    network_name: &str,
    membership_tag: Option<&str>,
    records: Vec<SignedMembershipRecord>,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&KademliaMembershipRecordBundle {
        version: 1,
        network_name: network_name.to_owned(),
        membership_tag: membership_tag.map(str::to_owned),
        records,
    })
}

fn learn_membership_records_from_kademlia_value(
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    local_capabilities: &mut ControlCapabilities,
    expected_network_name: &str,
    current_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
    value: &[u8],
) -> Result<usize, KademliaMembershipRecordError> {
    if value.len() > MAX_KADEMLIA_MEMBERSHIP_RECORD_BYTES {
        return Err(KademliaMembershipRecordError::TooLarge);
    }
    let bundle: KademliaMembershipRecordBundle =
        serde_json::from_slice(value).map_err(|_| KademliaMembershipRecordError::Decode)?;
    if bundle.version != 1 {
        return Err(KademliaMembershipRecordError::UnsupportedVersion);
    }
    if bundle.network_name != expected_network_name {
        return Err(KademliaMembershipRecordError::WrongNetwork);
    }
    if !membership_tag_allowed(
        bundle.membership_tag.as_deref(),
        current_membership_tag,
        previous_membership_tags,
    ) {
        return Err(KademliaMembershipRecordError::WrongMembershipScope);
    }
    if bundle.records.len() > MAX_CONTROL_MEMBERSHIP_RECORDS {
        return Err(KademliaMembershipRecordError::TooManyRecords);
    }

    let now_unix_seconds = current_unix_seconds_lossy();
    let stats = forwarder
        .merge_membership_records(&bundle.records, now_unix_seconds)
        .map_err(|error| KademliaMembershipRecordError::InvalidRecord(format!("{error:?}")))?;
    if stats.accepted > 0 || stats.removed_expired > 0 {
        membership
            .replace_record_members(
                forwarder.config(),
                forwarder.member_records(),
                now_unix_seconds,
            )
            .map_err(ForwardError::from)
            .map_err(|error| KademliaMembershipRecordError::InvalidRecord(format!("{error:?}")))?;
        *local_capabilities = refreshed_local_capabilities(local_capabilities, forwarder);
        let accepted = stats.accepted.to_string();
        let ignored = stats.ignored_stale_or_equal.to_string();
        let removed_expired = stats.removed_expired.to_string();
        log_runtime_event(
            LogLevel::Info,
            "kademlia_membership_records_merged",
            &[
                ("accepted", &accepted),
                ("ignored_stale_or_equal", &ignored),
                ("removed_expired", &removed_expired),
            ],
        );
    }
    Ok(stats.accepted)
}

fn membership_tag_allowed(
    tag: Option<&str>,
    current_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) -> bool {
    tag == current_membership_tag
        || tag.is_none()
        || tag.is_some_and(|tag| {
            previous_membership_tags
                .iter()
                .any(|previous| previous == tag)
        })
}

fn prune_expired_membership_records(
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    local_capabilities: &mut ControlCapabilities,
) -> Result<(), ForwardError> {
    let now_unix_seconds = current_unix_seconds_lossy();
    let stats = forwarder.prune_membership_records(now_unix_seconds)?;
    if stats.removed_expired == 0 {
        return Ok(());
    }

    membership.replace_record_members(
        forwarder.config(),
        forwarder.member_records(),
        now_unix_seconds,
    )?;
    *local_capabilities = refreshed_local_capabilities(local_capabilities, forwarder);
    let removed_expired = stats.removed_expired.to_string();
    log_runtime_event(
        LogLevel::Info,
        "membership_records_pruned",
        &[("removed_expired", &removed_expired)],
    );

    Ok(())
}

fn advertised_member_records(forwarder: &Forwarder) -> Vec<SignedMembershipRecord> {
    forwarder
        .member_records()
        .iter()
        .take(MAX_CONTROL_MEMBERSHIP_RECORDS)
        .cloned()
        .collect()
}

fn refreshed_local_capabilities(
    local_capabilities: &ControlCapabilities,
    forwarder: &Forwarder,
) -> ControlCapabilities {
    let mut capabilities = local_capabilities.clone();
    capabilities.advertised_routes = forwarder.local_advertised_routes();
    capabilities.member_records = advertised_member_records(forwarder);
    capabilities
}

fn current_unix_seconds_lossy() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[allow(clippy::too_many_arguments)]
fn service_status_response_for_peer(
    forwarder: &Forwarder,
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
    peer: Libp2pPeerId,
    request: &ServiceStatusRequest,
    local_capabilities: &ControlCapabilities,
    previous_membership_tags: &[String],
    packet_plane_session_ttl: Duration,
    packet_plane_replay_windows_per_session: usize,
) -> ServiceResponse {
    if !forwarder.is_configured_transport_peer(peer) {
        return ServiceResponse::Rejected(ServiceRejectionReason::UnauthorizedPeer);
    }

    if let Some(reason) = validate_status_request(
        request,
        &local_capabilities.network_name,
        local_capabilities.membership_tag.as_deref(),
        previous_membership_tags,
    ) {
        return ServiceResponse::Rejected(reason);
    }

    let mut response = ServiceStatusResponse::local(
        &local_capabilities.network_name,
        local_capabilities.membership_tag.clone(),
        request.nonce,
        local_capabilities.effective_mtu,
    )
    .with_packet_data_plane_capabilities(local_capabilities)
    .with_packet_plane_session_ttl_seconds(packet_plane_session_ttl.as_secs())
    .with_packet_plane_replay_windows_per_session(packet_plane_replay_windows_per_session);
    let overlay_peer = PeerId::from_libp2p(peer);
    let support = packet_transport_support(peer_capabilities, overlay_peer);
    if let Some(path) = paths.best_supported_for(overlay_peer, support) {
        response = response.with_selected_path(
            path.kind.wire_name().to_owned(),
            path.score(),
            path.effective_mtu(local_capabilities.effective_mtu),
            path.observed_rtt_ms,
        );
    }

    ServiceResponse::Status(response)
}

fn validate_peer_capabilities(
    forwarder: &mut Forwarder,
    membership: &mut OverlayMembership,
    peer: Libp2pPeerId,
    capabilities: &ControlCapabilities,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) -> Option<ControlRejectionReason> {
    if let Some(reason) = validate_capabilities(
        capabilities,
        expected_network,
        expected_membership_tag,
        previous_membership_tags,
    ) {
        return Some(reason);
    }

    if learn_membership_records_from_capabilities(forwarder, membership, capabilities).is_err() {
        return Some(ControlRejectionReason::InvalidMembershipRecord);
    }

    if !forwarder.authorizes_advertised_routes(peer, &capabilities.advertised_routes) {
        return Some(ControlRejectionReason::UnauthorizedRouteAdvertisement);
    }

    None
}

fn record_peer_capabilities(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    peer: Libp2pPeerId,
    capabilities: ControlCapabilities,
) {
    if forwarder.is_configured_transport_peer(peer) {
        peer_capabilities.record(PeerId::from_libp2p(peer), capabilities);
    }
}

#[derive(Debug)]
enum PacketPlaneNegotiationError {
    UnauthorizedPeer,
    MissingLocalEndpoint,
    MissingRemoteCapabilities,
    MissingRemoteEndpoint,
    EndpointNotAdvertised,
    Decode(PacketPlaneHandshakeError),
    Verify(PacketPlaneHandshakeError),
    Encode(PacketPlaneHandshakeError),
    Session(PacketPlaneSessionError),
    Quic(String),
    NoPendingHello,
    NoDirectNegotiationPath,
}

impl PacketPlaneNegotiationError {
    fn describe(&self) -> String {
        match self {
            Self::UnauthorizedPeer => "unauthorized_peer".to_owned(),
            Self::MissingLocalEndpoint => "missing_local_endpoint".to_owned(),
            Self::MissingRemoteCapabilities => "missing_remote_capabilities".to_owned(),
            Self::MissingRemoteEndpoint => "missing_remote_endpoint".to_owned(),
            Self::EndpointNotAdvertised => "endpoint_not_advertised".to_owned(),
            Self::Decode(error) => format!("decode: {error:?}"),
            Self::Verify(error) => format!("verify: {error:?}"),
            Self::Encode(error) => format!("encode: {error:?}"),
            Self::Session(error) => format!("session: {error:?}"),
            Self::Quic(error) => format!("quic: {error}"),
            Self::NoPendingHello => "no_pending_hello".to_owned(),
            Self::NoDirectNegotiationPath => "no_direct_negotiation_path".to_owned(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn maybe_send_packet_plane_hello(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    paths: &PathSet,
    packet_plane: &PacketPlaneRuntime,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    negotiator: &mut PacketPlaneNegotiator,
    identity: &NodeIdentity,
    local_capabilities: &ControlCapabilities,
    peer: Libp2pPeerId,
    remote_capabilities: &ControlCapabilities,
    metrics: &RuntimeMetrics,
) {
    let local_overlay = PeerId::from_libp2p(*swarm.local_peer_id());
    let remote_overlay = PeerId::from_libp2p(peer);
    if !forwarder.is_configured_transport_peer(peer)
        || !remote_capabilities.supports_datagram_packet_path()
        || !local_capabilities.supports_datagram_packet_path()
        || !has_direct_packet_plane_negotiation_path(paths, remote_overlay)
        || negotiator.has_pending(remote_overlay)
        || local_overlay.as_bytes() > remote_overlay.as_bytes()
    {
        return;
    }
    let backend = packet_plane_negotiation_backend(
        local_capabilities,
        remote_capabilities,
        packet_plane,
        packet_plane_quic,
        remote_overlay,
    );
    let Some(backend) = backend else {
        return;
    };

    match signed_packet_plane_handshake(
        PacketPlaneHandshakeKind::Hello,
        identity,
        local_capabilities,
        backend,
    ) {
        Ok((secret, handshake, verified)) => match handshake.encode() {
            Ok(encoded) => {
                negotiator.insert(remote_overlay, secret, verified, backend);
                swarm
                    .behaviour_mut()
                    .control
                    .send_request(&peer, ControlRequest::PacketPlaneHello(encoded));
                metrics.record_control_request_sent();
                log_runtime_event(
                    LogLevel::Info,
                    "packet_plane_hello_sent",
                    &[("peer", &remote_overlay.to_string())],
                );
            }
            Err(error) => {
                metrics.record_control_failure();
                eprintln!("packet-plane hello encode failed for {peer}: {error:?}");
            }
        },
        Err(error) => {
            metrics.record_control_failure();
            eprintln!("packet-plane hello signing failed for {peer}: {error:?}");
        }
    }
}

fn has_direct_packet_plane_negotiation_path(paths: &PathSet, peer: PeerId) -> bool {
    paths
        .candidates_for(peer)
        .any(|path| path.healthy && path.is_direct() && !path.kind.requires_quic_datagrams())
}

fn packet_plane_negotiation_backend(
    local_capabilities: &ControlCapabilities,
    remote_capabilities: &ControlCapabilities,
    packet_plane: &PacketPlaneRuntime,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    peer: PeerId,
) -> Option<PacketDatagramBackend> {
    if local_capabilities.supports_owned_quic_packet_plane
        && remote_capabilities.supports_owned_quic_packet_plane
        && packet_plane_quic.is_some_and(|packet_plane| !packet_plane.has_session(peer))
        && first_packet_plane_quic_endpoint(local_capabilities).is_some()
        && first_packet_plane_quic_endpoint(remote_capabilities).is_some()
        && remote_capabilities
            .owned_quic_packet_plane_certificate_der
            .as_ref()
            .is_some_and(|certificate| !certificate.is_empty())
    {
        return Some(PacketDatagramBackend::OwnedQuic);
    }
    if local_capabilities.supports_owned_udp_packet_plane
        && remote_capabilities.supports_owned_udp_packet_plane
        && !packet_plane.has_session(peer)
        && first_packet_plane_endpoint(local_capabilities).is_some()
        && first_packet_plane_endpoint(remote_capabilities).is_some()
    {
        return Some(PacketDatagramBackend::OwnedUdp);
    }
    None
}

async fn connect_packet_plane_quic_peer(
    packet_plane_quic: &mut PacketPlaneQuicRuntime,
    peer: PeerId,
    remote_capabilities: &ControlCapabilities,
) -> Result<(), PacketPlaneNegotiationError> {
    if packet_plane_quic.has_session(peer) {
        return Ok(());
    }
    let endpoint = first_packet_plane_quic_endpoint(remote_capabilities)
        .ok_or(PacketPlaneNegotiationError::MissingRemoteEndpoint)?;
    let certificate = remote_capabilities
        .owned_quic_packet_plane_certificate_der
        .clone()
        .filter(|certificate| !certificate.is_empty())
        .ok_or(PacketPlaneNegotiationError::MissingRemoteEndpoint)?;
    tokio::time::timeout(
        PACKET_PLANE_QUIC_CONNECT_TIMEOUT,
        packet_plane_quic.connect_peer(peer, endpoint, CertificateDer::from(certificate)),
    )
    .await
    .map_err(|_| PacketPlaneNegotiationError::Quic("connect_timeout".to_owned()))?
    .map_err(|error| PacketPlaneNegotiationError::Quic(packet_plane_quic_error_detail(&error)))
}

async fn drive_pending_packet_plane_quic_connects(
    packet_plane_quic: Option<&mut PacketPlaneQuicRuntime>,
    negotiator: &mut PacketPlaneNegotiator,
    peer_capabilities: &PeerCapabilities,
    metrics: &RuntimeMetrics,
) {
    let Some(packet_plane_quic) = packet_plane_quic else {
        return;
    };
    for peer in negotiator.pending_quic_connect_peers() {
        let Some(remote_capabilities) = peer_capabilities.get(peer) else {
            negotiator.remove_peer(peer);
            metrics.record_control_failure();
            continue;
        };
        negotiator.mark_quic_connect_attempted(peer);
        if let Err(error) =
            connect_packet_plane_quic_peer(packet_plane_quic, peer, remote_capabilities).await
        {
            negotiator.remove_peer(peer);
            metrics.record_control_failure();
            eprintln!(
                "packet-plane QUIC connect to {peer} failed: {}",
                error.describe()
            );
        }
    }
}

struct PacketPlaneAcceptContext<'a> {
    forwarder: &'a Forwarder,
    peer_capabilities: &'a PeerCapabilities,
    paths: &'a mut PathSet,
    metrics: &'a RuntimeMetrics,
    packet_plane: &'a mut PacketPlaneRuntime,
    packet_plane_quic: Option<&'a mut PacketPlaneQuicRuntime>,
    identity: &'a NodeIdentity,
    local_capabilities: &'a ControlCapabilities,
}

async fn packet_plane_accept_response_for_peer(
    mut context: PacketPlaneAcceptContext<'_>,
    peer: Libp2pPeerId,
    handshake: &[u8],
) -> ControlResponse {
    match accept_packet_plane_hello(&mut context, peer, handshake).await {
        Ok(encoded) => ControlResponse::PacketPlaneAccepted(encoded),
        Err(error) => {
            eprintln!(
                "packet-plane hello from {peer} rejected: {}",
                error.describe()
            );
            ControlResponse::PacketPlaneRejected(packet_plane_negotiation_rejection(&error))
        }
    }
}

async fn accept_packet_plane_hello(
    context: &mut PacketPlaneAcceptContext<'_>,
    peer: Libp2pPeerId,
    handshake: &[u8],
) -> Result<Vec<u8>, PacketPlaneNegotiationError> {
    if !context.forwarder.is_configured_transport_peer(peer) {
        return Err(PacketPlaneNegotiationError::UnauthorizedPeer);
    }
    let remote_overlay = PeerId::from_libp2p(peer);
    if !has_direct_packet_plane_negotiation_path(context.paths, remote_overlay) {
        return Err(PacketPlaneNegotiationError::NoDirectNegotiationPath);
    }
    let remote_capabilities = context
        .peer_capabilities
        .get(remote_overlay)
        .ok_or(PacketPlaneNegotiationError::MissingRemoteCapabilities)?;
    let backend = packet_plane_negotiation_backend(
        context.local_capabilities,
        remote_capabilities,
        context.packet_plane,
        context.packet_plane_quic.as_deref(),
        remote_overlay,
    )
    .ok_or(PacketPlaneNegotiationError::MissingRemoteEndpoint)?;
    if backend == PacketDatagramBackend::OwnedQuic {
        let packet_plane_quic = context
            .packet_plane_quic
            .as_deref_mut()
            .ok_or(PacketPlaneNegotiationError::MissingLocalEndpoint)?;
        tokio::time::timeout(
            PACKET_PLANE_QUIC_ACCEPT_TIMEOUT,
            packet_plane_quic.accept_peer(remote_overlay),
        )
        .await
        .map_err(|_| PacketPlaneNegotiationError::Quic("accept_timeout".to_owned()))?
        .map_err(|error| {
            PacketPlaneNegotiationError::Quic(packet_plane_quic_error_detail(&error))
        })?;
    }
    if !remote_capabilities.supports_datagram_packet_path() {
        return Err(PacketPlaneNegotiationError::MissingRemoteEndpoint);
    }
    let hello = PacketPlaneHandshake::decode(handshake)
        .map_err(PacketPlaneNegotiationError::Decode)?
        .verify(
            &context.local_capabilities.network_name,
            Some(remote_overlay),
        )
        .map_err(PacketPlaneNegotiationError::Verify)?;
    if hello.kind != PacketPlaneHandshakeKind::Hello
        || !endpoint_is_advertised_for_backend(remote_capabilities, hello.endpoint, backend)
    {
        return Err(PacketPlaneNegotiationError::EndpointNotAdvertised);
    }
    let (secret, accept, verified_accept) = signed_packet_plane_handshake(
        PacketPlaneHandshakeKind::Accept,
        context.identity,
        context.local_capabilities,
        backend,
    )?;
    let session = match backend {
        PacketDatagramBackend::OwnedUdp => context
            .packet_plane
            .establish_session(
                PacketPlaneSessionRole::Responder,
                &secret,
                &verified_accept,
                &hello,
            )
            .map_err(PacketPlaneNegotiationError::Session)?,
        PacketDatagramBackend::OwnedQuic => context
            .packet_plane_quic
            .as_deref_mut()
            .ok_or(PacketPlaneNegotiationError::MissingLocalEndpoint)?
            .establish_session(
                PacketPlaneSessionRole::Responder,
                &secret,
                &verified_accept,
                &hello,
            )
            .map_err(|error| {
                PacketPlaneNegotiationError::Quic(packet_plane_quic_error_detail(&error))
            })?,
    };
    record_packet_plane_path_established(
        context.paths,
        context.metrics,
        remote_overlay,
        backend,
        session.mtu,
    );
    log_runtime_event(
        LogLevel::Info,
        "packet_plane_session_established",
        &[
            ("peer", &remote_overlay.to_string()),
            ("role", "responder"),
            ("backend", packet_datagram_backend_name(backend)),
            ("endpoint", &hello.endpoint.to_string()),
        ],
    );
    accept.encode().map_err(PacketPlaneNegotiationError::Encode)
}

struct PacketPlaneCompleteContext<'a> {
    forwarder: &'a Forwarder,
    peer_capabilities: &'a PeerCapabilities,
    packet_plane: &'a mut PacketPlaneRuntime,
    packet_plane_quic: Option<&'a mut PacketPlaneQuicRuntime>,
    negotiator: &'a mut PacketPlaneNegotiator,
    paths: &'a mut PathSet,
    metrics: &'a RuntimeMetrics,
    network_name: &'a str,
}

fn complete_packet_plane_hello(
    context: &mut PacketPlaneCompleteContext<'_>,
    peer: Libp2pPeerId,
    handshake: &[u8],
) -> Result<(), PacketPlaneNegotiationError> {
    if !context.forwarder.is_configured_transport_peer(peer) {
        return Err(PacketPlaneNegotiationError::UnauthorizedPeer);
    }
    let remote_overlay = PeerId::from_libp2p(peer);
    if !has_direct_packet_plane_negotiation_path(context.paths, remote_overlay) {
        return Err(PacketPlaneNegotiationError::NoDirectNegotiationPath);
    }
    let pending = context
        .negotiator
        .remove(remote_overlay)
        .ok_or(PacketPlaneNegotiationError::NoPendingHello)?;
    let remote_capabilities = context
        .peer_capabilities
        .get(remote_overlay)
        .ok_or(PacketPlaneNegotiationError::MissingRemoteCapabilities)?;
    let accept = PacketPlaneHandshake::decode(handshake)
        .map_err(PacketPlaneNegotiationError::Decode)?
        .verify(context.network_name, Some(remote_overlay))
        .map_err(PacketPlaneNegotiationError::Verify)?;
    if accept.kind != PacketPlaneHandshakeKind::Accept
        || !endpoint_is_advertised_for_backend(
            remote_capabilities,
            accept.endpoint,
            pending.backend,
        )
    {
        return Err(PacketPlaneNegotiationError::EndpointNotAdvertised);
    }
    let session = match pending.backend {
        PacketDatagramBackend::OwnedUdp => context
            .packet_plane
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                &pending.secret,
                &pending.hello,
                &accept,
            )
            .map_err(PacketPlaneNegotiationError::Session)?,
        PacketDatagramBackend::OwnedQuic => context
            .packet_plane_quic
            .as_deref_mut()
            .ok_or(PacketPlaneNegotiationError::MissingLocalEndpoint)?
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                &pending.secret,
                &pending.hello,
                &accept,
            )
            .map_err(|error| {
                PacketPlaneNegotiationError::Quic(packet_plane_quic_error_detail(&error))
            })?,
    };
    record_packet_plane_path_established(
        context.paths,
        context.metrics,
        remote_overlay,
        pending.backend,
        session.mtu,
    );
    log_runtime_event(
        LogLevel::Info,
        "packet_plane_session_established",
        &[
            ("peer", &remote_overlay.to_string()),
            ("role", "initiator"),
            ("backend", packet_datagram_backend_name(pending.backend)),
            ("endpoint", &accept.endpoint.to_string()),
        ],
    );
    Ok(())
}

struct PacketPlaneExpiryContext<'a> {
    swarm: &'a mut Swarm<Behaviour>,
    forwarder: &'a Forwarder,
    paths: &'a mut PathSet,
    peer_capabilities: &'a PeerCapabilities,
    packet_plane: &'a mut PacketPlaneRuntime,
    packet_plane_quic: Option<&'a mut PacketPlaneQuicRuntime>,
    negotiator: &'a mut PacketPlaneNegotiator,
    identity: &'a NodeIdentity,
    local_capabilities: &'a ControlCapabilities,
    metrics: &'a RuntimeMetrics,
    session_ttl: Duration,
}

fn expire_packet_plane_sessions(context: &mut PacketPlaneExpiryContext<'_>) {
    let expired = context.packet_plane.expire_sessions(context.session_ttl);
    for session in expired {
        handle_expired_packet_plane_session(context, &session, PacketDatagramBackend::OwnedUdp);
    }

    let expired = context
        .packet_plane_quic
        .as_deref_mut()
        .map_or_else(Vec::new, |packet_plane_quic| {
            packet_plane_quic.expire_sessions(context.session_ttl)
        });
    for session in expired {
        handle_expired_packet_plane_session(context, &session, PacketDatagramBackend::OwnedQuic);
    }
}

fn handle_expired_packet_plane_session(
    context: &mut PacketPlaneExpiryContext<'_>,
    session: &PacketPlaneSessionSnapshot,
    backend: PacketDatagramBackend,
) {
    context.metrics.record_packet_plane_session_expired();
    let path = packet_datagram_backend_path_kind(backend);
    let change = context.paths.mark_unhealthy(session.peer, path);
    record_path_selection_change(context.metrics, change);
    log_runtime_event(
        LogLevel::Info,
        "packet_plane_session_expired",
        &[
            ("peer", &session.peer.to_string()),
            ("endpoint", &session.endpoint.to_string()),
            ("role", packet_plane_session_role_name(session.role)),
            ("backend", packet_datagram_backend_name(backend)),
        ],
    );

    if let Some(peer) = context.forwarder.transport_peer_for_overlay(session.peer)
        && let Some(capabilities) = context.peer_capabilities.get(session.peer)
    {
        maybe_send_packet_plane_hello(
            context.swarm,
            context.forwarder,
            context.paths,
            context.packet_plane,
            context.packet_plane_quic.as_deref(),
            context.negotiator,
            context.identity,
            context.local_capabilities,
            peer,
            capabilities,
            context.metrics,
        );
    }
}

fn record_packet_plane_path_established(
    paths: &mut PathSet,
    metrics: &RuntimeMetrics,
    peer: PeerId,
    backend: PacketDatagramBackend,
    mtu: u16,
) {
    let change = paths.record_established_with_mtu(
        peer,
        packet_datagram_backend_path_kind(backend),
        Some(mtu),
    );
    record_path_selection_change(metrics, change);
}

fn signed_packet_plane_handshake(
    kind: PacketPlaneHandshakeKind,
    identity: &NodeIdentity,
    local_capabilities: &ControlCapabilities,
    backend: PacketDatagramBackend,
) -> Result<
    (
        PacketPlaneEphemeralSecret,
        PacketPlaneHandshake,
        VerifiedPacketPlaneHandshake,
    ),
    PacketPlaneNegotiationError,
> {
    let endpoint = first_packet_plane_endpoint_for_backend(local_capabilities, backend)
        .ok_or(PacketPlaneNegotiationError::MissingLocalEndpoint)?;
    let secret = PacketPlaneEphemeralSecret::generate();
    let handshake = PacketPlaneHandshake::signed(
        kind,
        identity,
        PacketPlaneHandshakeParams {
            network_name: local_capabilities.network_name.clone(),
            session_id: random_nonzero_session_id(),
            nonce: OsRng.next_u64(),
            mtu: local_capabilities.effective_mtu,
            ephemeral_public_key: secret.public_key(),
            endpoint,
        },
    )
    .map_err(PacketPlaneNegotiationError::Encode)?;
    let local_overlay = identity
        .peer_id
        .parse::<PeerId>()
        .map_err(|_| PacketPlaneNegotiationError::UnauthorizedPeer)?;
    let verified = handshake
        .verify(&local_capabilities.network_name, Some(local_overlay))
        .map_err(PacketPlaneNegotiationError::Verify)?;
    Ok((secret, handshake, verified))
}

fn random_nonzero_session_id() -> SessionId {
    loop {
        let id = OsRng.next_u32();
        if id != 0 {
            return id;
        }
    }
}

fn first_packet_plane_endpoint(capabilities: &ControlCapabilities) -> Option<SocketAddr> {
    capabilities
        .packet_endpoint_candidates
        .iter()
        .flat_map(|endpoint| resolve_packet_plane_endpoint_candidate(endpoint))
        .min_by_key(|endpoint| packet_endpoint_priority(*endpoint))
}

fn first_packet_plane_quic_endpoint(capabilities: &ControlCapabilities) -> Option<SocketAddr> {
    capabilities
        .owned_quic_packet_endpoint_candidates
        .iter()
        .flat_map(|endpoint| resolve_packet_plane_endpoint_candidate(endpoint))
        .min_by_key(|endpoint| packet_endpoint_priority(*endpoint))
}

fn first_packet_plane_endpoint_for_backend(
    capabilities: &ControlCapabilities,
    backend: PacketDatagramBackend,
) -> Option<SocketAddr> {
    match backend {
        PacketDatagramBackend::OwnedUdp => first_packet_plane_endpoint(capabilities),
        PacketDatagramBackend::OwnedQuic => first_packet_plane_quic_endpoint(capabilities),
    }
}

fn endpoint_is_advertised(capabilities: &ControlCapabilities, endpoint: SocketAddr) -> bool {
    capabilities
        .packet_endpoint_candidates
        .iter()
        .any(|candidate| {
            candidate.parse::<SocketAddr>() == Ok(endpoint)
                || resolve_packet_plane_endpoint_candidate(candidate)
                    .into_iter()
                    .any(|resolved| resolved == endpoint)
        })
}

fn endpoint_is_quic_advertised(capabilities: &ControlCapabilities, endpoint: SocketAddr) -> bool {
    capabilities
        .owned_quic_packet_endpoint_candidates
        .iter()
        .any(|candidate| {
            candidate.parse::<SocketAddr>() == Ok(endpoint)
                || resolve_packet_plane_endpoint_candidate(candidate)
                    .into_iter()
                    .any(|resolved| resolved == endpoint)
        })
}

fn endpoint_is_advertised_for_backend(
    capabilities: &ControlCapabilities,
    endpoint: SocketAddr,
    backend: PacketDatagramBackend,
) -> bool {
    match backend {
        PacketDatagramBackend::OwnedUdp => endpoint_is_advertised(capabilities, endpoint),
        PacketDatagramBackend::OwnedQuic => endpoint_is_quic_advertised(capabilities, endpoint),
    }
}

fn send_control_capabilities_to_connected_peers(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    local_capabilities: &ControlCapabilities,
    metrics: &RuntimeMetrics,
) {
    let peers = swarm.connected_peers().copied().collect::<Vec<_>>();
    for peer in peers {
        send_control_capabilities(swarm, forwarder, peer, local_capabilities, metrics);
    }
}

fn update_observed_packet_plane_endpoints(
    capabilities: &mut ControlCapabilities,
    external_address: &Multiaddr,
    udp_listener: Option<SocketAddr>,
    quic_listener: Option<SocketAddr>,
    quic_certificate_der: Option<Vec<u8>>,
) -> ObservedPacketPlaneEndpointUpdate {
    let endpoints = observed_packet_plane_endpoints(external_address, udp_listener, quic_listener);
    let mut update = ObservedPacketPlaneEndpointUpdate {
        public_address_accepted: endpoints.public_address_accepted,
        ..ObservedPacketPlaneEndpointUpdate::default()
    };

    if let Some(endpoint) = endpoints.udp
        && !capabilities.packet_endpoint_candidates.contains(&endpoint)
    {
        capabilities.packet_endpoint_candidates.push(endpoint);
        capabilities.supports_owned_udp_packet_plane = true;
        capabilities.supports_quic_datagrams = true;
        update.udp_candidate_added = true;
    }

    if let (Some(endpoint), Some(certificate_der)) = (endpoints.quic, quic_certificate_der)
        && !capabilities
            .owned_quic_packet_endpoint_candidates
            .contains(&endpoint)
    {
        capabilities
            .owned_quic_packet_endpoint_candidates
            .push(endpoint);
        capabilities.owned_quic_packet_plane_certificate_der = Some(certificate_der);
        capabilities.supports_owned_quic_packet_plane = true;
        capabilities.supports_quic_datagrams = true;
        update.quic_candidate_added = true;
    }

    update
}

fn advertise_direct_packet_plane_endpoint_from_path(
    capabilities: &mut ControlCapabilities,
    udp_listener: Option<SocketAddr>,
    endpoint: &ConnectedPoint,
    metrics: &RuntimeMetrics,
) -> bool {
    let Some(candidate) = direct_packet_plane_endpoint_from_path(udp_listener, endpoint) else {
        return false;
    };
    if capabilities
        .packet_endpoint_candidates
        .first()
        .is_some_and(|existing| existing == &candidate)
    {
        return false;
    }

    replace_packet_plane_endpoint_for_listener(
        &mut capabilities.packet_endpoint_candidates,
        &candidate,
    );
    capabilities.supports_owned_udp_packet_plane = true;
    capabilities.supports_quic_datagrams = true;
    metrics.record_observed_packet_plane_udp_endpoint_candidate();
    log_runtime_event(
        LogLevel::Info,
        "direct_packet_plane_endpoint_advertised",
        &[("endpoint", &candidate)],
    );
    true
}

fn replace_packet_plane_endpoint_for_listener(candidates: &mut Vec<String>, candidate: &str) {
    let Ok(endpoint) = candidate.parse::<SocketAddr>() else {
        candidates.insert(0, candidate.to_owned());
        return;
    };
    candidates.retain(|existing| {
        existing.parse::<SocketAddr>().map_or(true, |existing| {
            existing == endpoint || existing.port() != endpoint.port()
        })
    });
    candidates.retain(|existing| existing != candidate);
    candidates.insert(0, candidate.to_owned());
}

fn direct_packet_plane_endpoint_from_path(
    udp_listener: Option<SocketAddr>,
    endpoint: &ConnectedPoint,
) -> Option<String> {
    let listener = udp_listener?;
    if listener.port() == 0 || endpoint.is_relayed() {
        return None;
    }
    if !listener.ip().is_unspecified() {
        return is_usable_direct_packet_endpoint_ip(listener.ip()).then(|| listener.to_string());
    }

    let remote_ip = direct_endpoint_remote_ip(endpoint)?;
    let local_ip = local_ip_for_remote(remote_ip)?;
    is_usable_direct_packet_endpoint_ip(local_ip)
        .then(|| SocketAddr::new(local_ip, listener.port()).to_string())
}

fn is_usable_direct_packet_endpoint_ip(address: IpAddr) -> bool {
    is_routable_packet_endpoint_ip(address) || address.is_loopback()
}

fn direct_endpoint_remote_ip(endpoint: &ConnectedPoint) -> Option<IpAddr> {
    let address = match endpoint {
        ConnectedPoint::Dialer { address, .. } => address,
        ConnectedPoint::Listener { send_back_addr, .. } => send_back_addr,
    };
    address.iter().find_map(|protocol| match protocol {
        Protocol::Ip4(address) => Some(IpAddr::V4(address)),
        Protocol::Ip6(address) => Some(IpAddr::V6(address)),
        _ => None,
    })
}

fn local_ip_for_remote(remote_ip: IpAddr) -> Option<IpAddr> {
    let bind_addr = match remote_ip {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = StdUdpSocket::bind(bind_addr).ok()?;
    socket.connect(SocketAddr::new(remote_ip, 9)).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ObservedPacketPlaneEndpointUpdate {
    public_address_accepted: bool,
    udp_candidate_added: bool,
    quic_candidate_added: bool,
}

impl ObservedPacketPlaneEndpointUpdate {
    fn changed(self) -> bool {
        self.udp_candidate_added || self.quic_candidate_added
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct ObservedPacketPlaneEndpoints {
    public_address_accepted: bool,
    udp: Option<String>,
    quic: Option<String>,
}

fn observed_packet_plane_endpoints(
    external_address: &Multiaddr,
    udp_listener: Option<SocketAddr>,
    quic_listener: Option<SocketAddr>,
) -> ObservedPacketPlaneEndpoints {
    let Some(ip) = routable_packet_endpoint_ip_from_external_address(external_address) else {
        return ObservedPacketPlaneEndpoints::default();
    };

    ObservedPacketPlaneEndpoints {
        public_address_accepted: true,
        udp: udp_listener
            .filter(|listener| listener.port() != 0)
            .map(|listener| SocketAddr::new(ip, listener.port()).to_string()),
        quic: quic_listener
            .filter(|listener| listener.port() != 0)
            .map(|listener| SocketAddr::new(ip, listener.port()).to_string()),
    }
}

fn routable_packet_endpoint_ip_from_external_address(address: &Multiaddr) -> Option<IpAddr> {
    if address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
    {
        return None;
    }

    for protocol in address {
        let ip = match protocol {
            Protocol::Ip4(address) => IpAddr::V4(address),
            Protocol::Ip6(address) => IpAddr::V6(address),
            _ => continue,
        };
        if is_routable_packet_endpoint_ip(ip) {
            return Some(ip);
        }
    }

    None
}

fn is_routable_packet_endpoint_ip(address: IpAddr) -> bool {
    matches!(packet_endpoint_reachability_rank(address), 0 | 1) && !address.is_multicast()
}

fn resolve_packet_plane_endpoint_candidate(candidate: &str) -> Vec<SocketAddr> {
    if let Ok(endpoint) = candidate.parse() {
        return vec![endpoint];
    }
    candidate
        .to_socket_addrs()
        .map(Iterator::collect)
        .unwrap_or_default()
}

fn packet_endpoint_priority(endpoint: SocketAddr) -> (u8, u16) {
    (
        packet_endpoint_reachability_rank(endpoint.ip()),
        endpoint.port(),
    )
}

fn packet_endpoint_reachability_rank(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(address) => packet_ipv4_reachability_rank(address.octets()),
        IpAddr::V6(address) => packet_ipv6_reachability_rank(address.octets()),
    }
}

fn packet_ipv4_reachability_rank(octets: [u8; 4]) -> u8 {
    if octets[0] == 0 {
        return 3;
    }
    if octets[0] == 127 || octets[0] == 169 && octets[1] == 254 {
        return 2;
    }
    if octets[0] == 10
        || octets[0] == 172 && octets[1] >= 16 && octets[1] <= 31
        || octets[0] == 192 && octets[1] == 168
    {
        return 1;
    }

    0
}

fn packet_ipv6_reachability_rank(octets: [u8; 16]) -> u8 {
    if octets == [0; 16] {
        return 3;
    }
    if octets[15] == 1
        && octets[0] == 0
        && octets[1] == 0
        && octets[2] == 0
        && octets[3] == 0
        && octets[4] == 0
        && octets[5] == 0
        && octets[6] == 0
        && octets[7] == 0
        && octets[8] == 0
        && octets[9] == 0
        && octets[10] == 0
        && octets[11] == 0
        && octets[12] == 0
        && octets[13] == 0
        && octets[14] == 0
        || octets[0] == 0xfe && octets[1] & 0xc0 == 0x80
    {
        return 2;
    }
    if octets[0] & 0xfe == 0xfc {
        return 1;
    }

    0
}

const fn packet_plane_negotiation_rejection(
    error: &PacketPlaneNegotiationError,
) -> ControlRejectionReason {
    match error {
        PacketPlaneNegotiationError::UnauthorizedPeer => ControlRejectionReason::UnauthorizedPeer,
        PacketPlaneNegotiationError::Decode(_)
        | PacketPlaneNegotiationError::Verify(_)
        | PacketPlaneNegotiationError::EndpointNotAdvertised
        | PacketPlaneNegotiationError::MissingLocalEndpoint
        | PacketPlaneNegotiationError::MissingRemoteCapabilities
        | PacketPlaneNegotiationError::MissingRemoteEndpoint
        | PacketPlaneNegotiationError::Encode(_)
        | PacketPlaneNegotiationError::Session(_)
        | PacketPlaneNegotiationError::Quic(_)
        | PacketPlaneNegotiationError::NoPendingHello
        | PacketPlaneNegotiationError::NoDirectNegotiationPath => {
            ControlRejectionReason::UnsupportedPreferredPath
        }
    }
}

fn invalidate_peer_capabilities_on_first_connection(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    peer: Libp2pPeerId,
    established_connections: u32,
) {
    if established_connections == 1 {
        invalidate_peer_capabilities(forwarder, peer_capabilities, peer);
    }
}

fn invalidate_peer_capabilities_when_disconnected(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    peer: Libp2pPeerId,
    remaining_connections: u32,
) {
    let _ = (forwarder, peer_capabilities, peer, remaining_connections);
}

fn invalidate_peer_capabilities(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    peer: Libp2pPeerId,
) {
    if forwarder.is_configured_transport_peer(peer) {
        peer_capabilities.remove(PeerId::from_libp2p(peer));
    }
}

fn send_control_capabilities(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    local_capabilities: &ControlCapabilities,
    metrics: &RuntimeMetrics,
) {
    if !forwarder.is_configured_transport_peer(peer) {
        return;
    }

    swarm.behaviour_mut().control.send_request(
        &peer,
        ControlRequest::Capabilities(refreshed_local_capabilities(local_capabilities, forwarder)),
    );
    metrics.record_control_request_sent();
}

fn send_service_status_request(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    local_capabilities: &ControlCapabilities,
    metrics: &RuntimeMetrics,
) {
    if !forwarder.is_configured_transport_peer(peer) {
        return;
    }

    swarm.behaviour_mut().service.send_request(
        &peer,
        ServiceRequest::Status(ServiceStatusRequest::local(
            &local_capabilities.network_name,
            local_capabilities.membership_tag.clone(),
            SERVICE_STATUS_NONCE,
        )),
    );
    metrics.record_service_request_sent();
}

#[allow(clippy::too_many_arguments)]
fn record_path_established_and_maybe_send_packet_plane_hello(
    swarm: &mut Swarm<Behaviour>,
    paths: &mut PathSet,
    forwarder: &Forwarder,
    peer_capabilities: &PeerCapabilities,
    metrics: &RuntimeMetrics,
    local_capabilities: &ControlCapabilities,
    identity: &NodeIdentity,
    packet_plane: &PacketPlaneRuntime,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    packet_plane_negotiator: &mut PacketPlaneNegotiator,
    peer: Libp2pPeerId,
    endpoint: &ConnectedPoint,
) {
    let Some(change) = record_path_established(paths, forwarder, metrics, peer, endpoint) else {
        return;
    };
    let kind = path_kind_for_endpoint(endpoint);
    let remote_overlay = PeerId::from_libp2p(peer);
    let selected_direct_negotiation_path = !kind.requires_quic_datagrams()
        && change
            .current
            .is_some_and(|current| current.peer == remote_overlay && current.is_direct());
    if !(change.promoted_to_direct() || selected_direct_negotiation_path) {
        return;
    }

    let Some(remote_capabilities) = peer_capabilities.get(remote_overlay).cloned() else {
        return;
    };
    maybe_send_packet_plane_hello(
        swarm,
        forwarder,
        paths,
        packet_plane,
        packet_plane_quic,
        packet_plane_negotiator,
        identity,
        local_capabilities,
        peer,
        &remote_capabilities,
        metrics,
    );
}

fn record_path_established(
    paths: &mut PathSet,
    forwarder: &Forwarder,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    endpoint: &ConnectedPoint,
) -> Option<crate::path::PathSelectionChange> {
    if !forwarder.is_configured_transport_peer(peer) {
        return None;
    }

    let kind = path_kind_for_endpoint(endpoint);
    let change = paths.record_established_with_mtu(
        PeerId::from_libp2p(peer),
        kind,
        Some(initial_path_mtu(
            kind,
            u16::try_from(forwarder.mtu()).unwrap_or(u16::MAX),
        )),
    );
    record_path_selection_change(metrics, change);
    change
}

fn record_path_closed(
    paths: &mut PathSet,
    forwarder: &Forwarder,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    endpoint: &ConnectedPoint,
) {
    if !forwarder.is_configured_transport_peer(peer) {
        return;
    }

    let change = paths.record_closed(PeerId::from_libp2p(peer), path_kind_for_endpoint(endpoint));
    record_path_selection_change(metrics, change);
}

fn record_path_selection_change(
    metrics: &RuntimeMetrics,
    change: Option<crate::path::PathSelectionChange>,
) {
    let Some(change) = change else {
        return;
    };
    if change.promoted_to_direct() {
        metrics.record_path_promotion_to_direct();
        log_path_selection_change("path_promoted_to_direct", change);
    } else if change.fell_back_to_relay() {
        metrics.record_path_fallback_to_relay();
        log_path_selection_change("path_fell_back_to_relay", change);
    }
}

#[cfg(test)]
fn maybe_demote_packet_plane_path(
    paths: &mut PathSet,
    metrics: &RuntimeMetrics,
    peer: PeerId,
    path: PathKind,
    error: &PacketPlaneIoError,
) -> bool {
    if !packet_plane_send_failure_demotes_path(error) {
        return false;
    }

    let change = paths.mark_unhealthy(peer, path);
    metrics.record_packet_plane_path_demotion();
    record_path_selection_change(metrics, change);

    let peer = peer.to_string();
    log_runtime_event(
        LogLevel::Warn,
        "packet_plane_path_demoted",
        &[
            ("peer", &peer),
            ("path", path.wire_name()),
            ("reason", packet_plane_io_error_name(error)),
        ],
    );
    true
}

fn maybe_demote_packet_plane_send_path(
    paths: &mut PathSet,
    metrics: &RuntimeMetrics,
    peer: PeerId,
    path: PathKind,
    error: &PacketPlaneSendError,
) -> bool {
    if !packet_plane_send_error_demotes_path(error) {
        return false;
    }

    let change = paths.mark_unhealthy(peer, path);
    metrics.record_packet_plane_path_demotion();
    record_path_selection_change(metrics, change);

    let peer = peer.to_string();
    log_runtime_event(
        LogLevel::Warn,
        "packet_plane_path_demoted",
        &[
            ("peer", &peer),
            ("path", path.wire_name()),
            ("reason", packet_plane_send_error_name(error)),
        ],
    );
    true
}

fn demote_packet_plane_path_probe_timeout(
    paths: &mut PathSet,
    metrics: &RuntimeMetrics,
    peer: PeerId,
    path: PathKind,
) -> bool {
    if !paths
        .candidates_for(peer)
        .any(|candidate| candidate.kind == path && candidate.healthy)
    {
        return false;
    }

    let change = paths.mark_unhealthy(peer, path);
    metrics.record_packet_plane_path_demotion();
    record_path_selection_change(metrics, change);

    let peer = peer.to_string();
    log_runtime_event(
        LogLevel::Warn,
        "packet_plane_path_demoted",
        &[
            ("peer", &peer),
            ("path", path.wire_name()),
            ("reason", "path_probe_timeout"),
        ],
    );
    true
}

fn maybe_demote_stream_fallback_path(
    paths: &mut PathSet,
    metrics: &RuntimeMetrics,
    peer: PeerId,
    path: PathKind,
    error: &request_response::OutboundFailure,
) -> bool {
    if path == PathKind::CircuitRelay {
        log_runtime_event(
            LogLevel::Warn,
            "relay_stream_fallback_failure",
            &[
                ("peer", &peer.to_string()),
                ("reason", stream_fallback_failure_name(error)),
            ],
        );
        return false;
    }

    let change = paths.mark_unhealthy(peer, path);
    metrics.record_stream_fallback_path_demotion();
    record_path_selection_change(metrics, change);

    let peer = peer.to_string();
    log_runtime_event(
        LogLevel::Warn,
        "stream_fallback_path_demoted",
        &[
            ("peer", &peer),
            ("path", path.wire_name()),
            ("reason", stream_fallback_failure_name(error)),
        ],
    );
    true
}

const fn stream_fallback_failure_name(error: &request_response::OutboundFailure) -> &'static str {
    match error {
        request_response::OutboundFailure::DialFailure => "dial_failure",
        request_response::OutboundFailure::Timeout => "timeout",
        request_response::OutboundFailure::ConnectionClosed => "connection_closed",
        request_response::OutboundFailure::UnsupportedProtocols => "unsupported_protocols",
        request_response::OutboundFailure::Io(_) => "io",
    }
}

fn maybe_demote_packet_plane_quic_receive_path(
    paths: &mut PathSet,
    metrics: &RuntimeMetrics,
    error: &PacketPlaneQuicError,
) -> bool {
    let Some(peer) = packet_plane_quic_receive_error_peer(error) else {
        return false;
    };

    let path = PathKind::DirectQuicDatagram;
    let change = paths.mark_unhealthy(peer, path);
    metrics.record_packet_plane_path_demotion();
    record_path_selection_change(metrics, change);

    let peer = peer.to_string();
    log_runtime_event(
        LogLevel::Warn,
        "packet_plane_path_demoted",
        &[
            ("peer", &peer),
            ("path", path.wire_name()),
            ("reason", packet_plane_quic_error_name(error)),
        ],
    );
    true
}

fn packet_plane_quic_receive_error_peer(error: &PacketPlaneQuicError) -> Option<PeerId> {
    match error {
        PacketPlaneQuicError::PeerConnection { peer, .. }
        | PacketPlaneQuicError::NoConnection { peer } => Some(*peer),
        _ => None,
    }
}

fn log_path_selection_change(event: &str, change: crate::path::PathSelectionChange) {
    let peer = change.peer.to_string();
    let previous = change
        .previous
        .map_or("none", |candidate| candidate.kind.wire_name());
    let current = change
        .current
        .map_or("none", |candidate| candidate.kind.wire_name());
    log_runtime_event(
        LogLevel::Info,
        event,
        &[
            ("peer", &peer),
            ("previous_path", previous),
            ("current_path", current),
        ],
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EstablishedConnectionAuthorization {
    OverlayPeer,
    InfrastructurePeer,
    InfrastructureProbe,
    Rejected,
}

fn authorize_established_connection(
    membership: &OverlayMembership,
    infrastructure_peers: &InfrastructurePeers,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    allow_infrastructure_probe: bool,
) -> EstablishedConnectionAuthorization {
    if membership.allows(peer) {
        return EstablishedConnectionAuthorization::OverlayPeer;
    }

    if membership.allows_configured_infrastructure(peer) || infrastructure_peers.contains(peer) {
        return EstablishedConnectionAuthorization::InfrastructurePeer;
    }

    if allow_infrastructure_probe {
        return EstablishedConnectionAuthorization::InfrastructureProbe;
    }

    metrics.record_unauthorized_connection_dropped();
    EstablishedConnectionAuthorization::Rejected
}

fn path_kind_for_endpoint(endpoint: &ConnectedPoint) -> PathKind {
    if endpoint.is_relayed() {
        return PathKind::CircuitRelay;
    }

    let address = match endpoint {
        ConnectedPoint::Dialer { address, .. } => address,
        ConnectedPoint::Listener { local_addr, .. } => local_addr,
    };

    if address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::Quic | Protocol::QuicV1))
    {
        PathKind::DirectQuicStream
    } else {
        PathKind::DirectTcpStream
    }
}

struct BehaviourEventContext<'a> {
    forwarder: &'a mut Forwarder,
    membership: &'a mut OverlayMembership,
    infrastructure_peers: &'a mut InfrastructurePeers,
    relay_readiness: &'a mut RelayReadiness,
    auto_relay: &'a mut AutoRelayState,
    paths: &'a PathSet,
    configured_peer_addresses: &'a [(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &'a mut DiscoveredPeerAddresses,
    metrics: &'a RuntimeMetrics,
    discovery: &'a DiscoveryConfig,
    local_capabilities: &'a mut ControlCapabilities,
    previous_membership_tags: &'a [String],
    identity: &'a NodeIdentity,
}

fn handle_behaviour_event(
    swarm: &mut Swarm<Behaviour>,
    context: &mut BehaviourEventContext<'_>,
    event: BehaviourEvent,
) {
    match event {
        BehaviourEvent::Mdns(mdns::Event::Discovered(peers)) if context.discovery.mdns => {
            for (peer, address) in peers {
                learn_peer_address(
                    swarm,
                    context.forwarder,
                    context.discovered_peer_addresses,
                    context.paths,
                    context.metrics,
                    peer,
                    address,
                    context.discovery,
                    DiscoveredPeerAddressSource::UnauthenticatedDiscovery,
                );
            }
        }
        BehaviourEvent::Mdns(mdns::Event::Expired(peers)) if context.discovery.mdns => {
            for (peer, address) in peers {
                context.discovered_peer_addresses.remove(peer, &address);
                if context.discovery.kademlia {
                    swarm.behaviour_mut().kad.remove_address(&peer, &address);
                }
            }
        }
        BehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. }) => {
            handle_identify_received(swarm, context, peer_id, info);
        }
        BehaviourEvent::Identify(identify::Event::Error { peer_id, error, .. }) => {
            if context.infrastructure_peers.contains(peer_id) && !context.membership.allows(peer_id)
            {
                reject_unconfirmed_infrastructure_peer(
                    swarm,
                    context.infrastructure_peers,
                    context.auto_relay,
                    peer_id,
                    "identify_failed",
                );
            }
            eprintln!("identify with {peer_id} failed: {error}");
        }
        BehaviourEvent::Kad(event) if context.discovery.kademlia => {
            handle_kademlia_event(
                swarm,
                KademliaEventContext {
                    forwarder: context.forwarder,
                    membership: context.membership,
                    infrastructure_peers: context.infrastructure_peers,
                    auto_relay: context.auto_relay,
                    discovered_peer_addresses: context.discovered_peer_addresses,
                    paths: context.paths,
                    local_capabilities: context.local_capabilities,
                    previous_membership_tags: context.previous_membership_tags,
                    metrics: context.metrics,
                    discovery: context.discovery,
                },
                event,
            );
        }
        BehaviourEvent::Relay(event) => handle_relay_event(
            swarm,
            context.relay_readiness,
            context.auto_relay,
            context.configured_peer_addresses,
            context.discovered_peer_addresses,
            context.metrics,
            context.discovery,
            context.local_capabilities,
            context.identity,
            &event,
        ),
        BehaviourEvent::RelayServer(event) => handle_relay_server_event(context.metrics, &event),
        BehaviourEvent::Dcutr(dcutr::Event {
            remote_peer_id,
            result,
        }) if context.discovery.dcutr => {
            context.metrics.record_dcutr_result(result.is_ok());
            log_runtime_event(
                if result.is_ok() {
                    LogLevel::Info
                } else {
                    LogLevel::Warn
                },
                "dcutr_hole_punch_result",
                &[
                    ("peer", &remote_peer_id.to_string()),
                    ("success", &result.is_ok().to_string()),
                    ("result", &format!("{result:?}")),
                ],
            );
        }
        BehaviourEvent::Autonat(event) if context.discovery.autonat => {
            handle_autonat_event(swarm, context.auto_relay, context.metrics, event);
        }
        _ => {}
    }
}

fn handle_identify_received(
    swarm: &mut Swarm<Behaviour>,
    context: &mut BehaviourEventContext<'_>,
    peer_id: Libp2pPeerId,
    info: identify::Info,
) {
    let observed_addr = info.observed_addr.clone();
    let relay_hop = identify_protocols_include_relay_hop(&info.protocols);
    let auto_relay_candidates = auto_relay_candidate_addresses(peer_id, &info);
    for address in info.listen_addrs {
        learn_peer_address(
            swarm,
            context.forwarder,
            context.discovered_peer_addresses,
            context.paths,
            context.metrics,
            peer_id,
            address,
            context.discovery,
            DiscoveredPeerAddressSource::UnauthenticatedDiscovery,
        );
    }
    if context.infrastructure_peers.contains(peer_id)
        && !context.membership.allows(peer_id)
        && !relay_hop
    {
        reject_unconfirmed_infrastructure_peer(
            swarm,
            context.infrastructure_peers,
            context.auto_relay,
            peer_id,
            "missing_relay_hop",
        );
        return;
    }
    if !context.membership.allows(peer_id) && !relay_hop {
        log_runtime_event(
            LogLevel::Warn,
            "auto_relay_infrastructure_rejected",
            &[
                ("peer", &peer_id.to_string()),
                ("reason", "identified_non_relay_peer"),
            ],
        );
        if swarm.disconnect_peer_id(peer_id).is_err() {
            log_runtime_event(
                LogLevel::Warn,
                "auto_relay_infrastructure_already_disconnected",
                &[("peer", &peer_id.to_string())],
            );
        }
        return;
    }
    if !context.membership.allows(peer_id)
        && relay_hop
        && let Some(address) = auto_relay_candidates.first().cloned()
        && !context.infrastructure_peers.contains(peer_id)
        && context
            .infrastructure_peers
            .insert(peer_id, address.clone())
    {
        context.metrics.record_auto_relay_infrastructure_candidate();
        log_runtime_event(
            LogLevel::Info,
            "auto_relay_infrastructure_candidate",
            &[
                ("peer", &peer_id.to_string()),
                ("address", &address.to_string()),
            ],
        );
    }
    record_auto_relay_candidates(
        context.auto_relay,
        context.metrics,
        peer_id,
        auto_relay_candidates,
    );
    attempt_auto_relay_reservations(swarm, context.auto_relay, context.metrics);
    if context.discovery.autonat && observed_addr.iter().next().is_some() {
        schedule_autonat_probe(swarm, context.metrics, &observed_addr);
    }
}

fn schedule_autonat_probe(
    swarm: &mut Swarm<Behaviour>,
    metrics: &RuntimeMetrics,
    address: &Multiaddr,
) -> bool {
    let Some(autonat) = swarm.behaviour_mut().autonat.as_mut() else {
        return false;
    };
    autonat.probe_address(address.clone());
    metrics.record_autonat_probe_scheduled();
    eprintln!("autonat probe scheduled for observed address: {address}");
    true
}

fn handle_autonat_event(
    swarm: &mut Swarm<Behaviour>,
    auto_relay: &mut AutoRelayState,
    metrics: &RuntimeMetrics,
    event: autonat::Event,
) {
    match event {
        autonat::Event::StatusChanged { old, new } => {
            if let autonat::NatStatus::Public(address) = &new {
                swarm.add_external_address(address.clone());
            }
            let reachability = autonat_reachability(&new);
            auto_relay.record_reachability(reachability);
            metrics.record_autonat_status(reachability);
            if auto_relay.private_reachability() {
                query_auto_relay_infrastructure(swarm, metrics, "autonat_private");
            }
            attempt_auto_relay_reservations(swarm, auto_relay, metrics);
            log_runtime_event(
                LogLevel::Info,
                "autonat_status_changed",
                &[
                    ("old", &format!("{old:?}")),
                    ("new", &format!("{new:?}")),
                    ("reachability", reachability.as_str()),
                ],
            );
        }
        autonat::Event::OutboundProbe(event) => {
            log_runtime_event(
                LogLevel::Info,
                "autonat_outbound_probe",
                &[("event", &format!("{event:?}"))],
            );
        }
        autonat::Event::InboundProbe(event) => {
            log_runtime_event(
                LogLevel::Info,
                "autonat_inbound_probe",
                &[("event", &format!("{event:?}"))],
            );
        }
    }
}

fn autonat_reachability(status: &autonat::NatStatus) -> AutoNatReachability {
    match status {
        autonat::NatStatus::Unknown => AutoNatReachability::Unknown,
        autonat::NatStatus::Public(_) => AutoNatReachability::Public,
        autonat::NatStatus::Private => AutoNatReachability::Private,
    }
}

impl AutoNatReachability {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

struct KademliaEventContext<'a> {
    forwarder: &'a mut Forwarder,
    membership: &'a mut OverlayMembership,
    infrastructure_peers: &'a mut InfrastructurePeers,
    auto_relay: &'a mut AutoRelayState,
    discovered_peer_addresses: &'a mut DiscoveredPeerAddresses,
    paths: &'a PathSet,
    local_capabilities: &'a mut ControlCapabilities,
    previous_membership_tags: &'a [String],
    metrics: &'a RuntimeMetrics,
    discovery: &'a DiscoveryConfig,
}

fn handle_kademlia_event(
    swarm: &mut Swarm<Behaviour>,
    mut context: KademliaEventContext<'_>,
    event: kad::Event,
) {
    match event {
        kad::Event::OutboundQueryProgressed { result, .. } => {
            if let kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                providers,
                ..
            })) = &result
            {
                dial_kademlia_providers(swarm, context.forwarder, context.metrics, providers);
            }
            handle_kademlia_membership_record_result(&result, &mut context);
            handle_kademlia_peer_address_record_result(swarm, &mut context, &result);
            handle_kademlia_closest_peer_result(
                swarm,
                context.forwarder,
                context.infrastructure_peers,
                context.auto_relay,
                context.discovered_peer_addresses,
                context.paths,
                context.metrics,
                context.discovery,
                &result,
            );
            eprintln!("kademlia query progressed: {result:?}");
        }
        kad::Event::RoutingUpdated {
            peer, addresses, ..
        } => {
            admit_kademlia_relay_infrastructure_peer(
                swarm,
                context.forwarder,
                context.infrastructure_peers,
                context.auto_relay,
                context.metrics,
                peer,
                addresses.iter(),
            );
            eprintln!("kademlia routing updated: {peer}");
        }
        other => {
            eprintln!("kademlia event: {other:?}");
        }
    }
}

fn handle_kademlia_membership_record_result(
    result: &kad::QueryResult,
    context: &mut KademliaEventContext<'_>,
) {
    if kademlia_query_result_key(result).is_some_and(kademlia_key_is_peer_address_record) {
        return;
    }
    if let kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(peer_record))) = result {
        let value = peer_record.record.value.as_slice();
        let expected_network_name = context.local_capabilities.network_name.clone();
        let current_membership_tag = context.local_capabilities.membership_tag.clone();
        match learn_membership_records_from_kademlia_value(
            context.forwarder,
            context.membership,
            context.local_capabilities,
            &expected_network_name,
            current_membership_tag.as_deref(),
            context.previous_membership_tags,
            value,
        ) {
            Ok(accepted) => {
                context.metrics.record_kademlia_membership_records_found();
                context
                    .metrics
                    .record_kademlia_membership_records_accepted(accepted);
            }
            Err(error) => {
                context.metrics.record_kademlia_membership_record_invalid();
                log_runtime_event(
                    LogLevel::Warn,
                    "kademlia_membership_record_rejected",
                    &[("reason", &error.to_string())],
                );
            }
        }
    }
    if let kad::QueryResult::GetRecord(Err(error)) = result {
        log_runtime_event(
            LogLevel::Warn,
            "kademlia_membership_record_lookup_failed",
            &[("error", &format!("{error:?}"))],
        );
    }
    handle_kademlia_put_record_result(context.metrics, result);
}

fn handle_kademlia_peer_address_record_result(
    swarm: &mut Swarm<Behaviour>,
    context: &mut KademliaEventContext<'_>,
    result: &kad::QueryResult,
) {
    let kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(peer_record))) = result else {
        return;
    };
    if !kademlia_key_is_peer_address_record(&peer_record.record.key) {
        return;
    }
    let expected_network_name = context.local_capabilities.network_name.clone();
    let current_membership_tag = context.local_capabilities.membership_tag.clone();
    match learn_peer_addresses_from_kademlia_value(
        context.forwarder,
        &expected_network_name,
        current_membership_tag.as_deref(),
        context.previous_membership_tags,
        peer_record.record.value.as_slice(),
    ) {
        Ok((peer, addresses)) => {
            for address in addresses {
                learn_peer_address(
                    swarm,
                    context.forwarder,
                    context.discovered_peer_addresses,
                    context.paths,
                    context.metrics,
                    peer,
                    address,
                    context.discovery,
                    DiscoveredPeerAddressSource::AuthenticatedPeerRecord,
                );
            }
        }
        Err(error) => {
            log_runtime_event(
                LogLevel::Warn,
                "kademlia_peer_address_record_rejected",
                &[("reason", &error.to_string())],
            );
        }
    }
}

fn query_auto_relay_infrastructure(
    swarm: &mut Swarm<Behaviour>,
    metrics: &RuntimeMetrics,
    reason: &'static str,
) {
    let local_peer = *swarm.local_peer_id();
    let mut targets = vec![local_peer];
    for _ in 1..AUTO_RELAY_DISCOVERY_QUERY_FANOUT {
        targets.push(
            libp2p::identity::Keypair::generate_ed25519()
                .public()
                .to_peer_id(),
        );
    }
    for target in targets {
        swarm.behaviour_mut().kad.get_closest_peers(target);
        metrics.record_auto_relay_discovery_query();
        log_runtime_event(
            LogLevel::Info,
            "auto_relay_discovery_query",
            &[("reason", reason), ("target", &target.to_string())],
        );
    }
}

fn handle_kademlia_put_record_result(metrics: &RuntimeMetrics, result: &kad::QueryResult) {
    if kademlia_query_result_key(result).is_some_and(kademlia_key_is_peer_address_record) {
        return;
    }
    if let kad::QueryResult::PutRecord(Ok(kad::PutRecordOk { key })) = result {
        log_runtime_event(
            LogLevel::Info,
            "kademlia_membership_record_published",
            &[("key", &format!("{key:?}"))],
        );
    }
    if let kad::QueryResult::PutRecord(Err(error)) = result {
        metrics.record_kademlia_membership_record_publication_failure();
        log_runtime_event(
            LogLevel::Warn,
            "kademlia_membership_record_publication_failed",
            &[("error", &format!("{error:?}"))],
        );
    }
}

fn handle_kademlia_closest_peer_result(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    infrastructure_peers: &mut InfrastructurePeers,
    auto_relay: &mut AutoRelayState,
    discovered_peer_addresses: &mut DiscoveredPeerAddresses,
    paths: &PathSet,
    metrics: &RuntimeMetrics,
    discovery: &DiscoveryConfig,
    result: &kad::QueryResult,
) {
    if let kad::QueryResult::GetClosestPeers(
        Ok(kad::GetClosestPeersOk { peers, .. })
        | Err(kad::GetClosestPeersError::Timeout { peers, .. }),
    ) = result
    {
        for peer in peers {
            for address in &peer.addrs {
                learn_peer_address(
                    swarm,
                    forwarder,
                    discovered_peer_addresses,
                    paths,
                    metrics,
                    peer.peer_id,
                    address.clone(),
                    discovery,
                    DiscoveredPeerAddressSource::UnauthenticatedDiscovery,
                );
            }
            admit_kademlia_relay_infrastructure_peer(
                swarm,
                forwarder,
                infrastructure_peers,
                auto_relay,
                metrics,
                peer.peer_id,
                peer.addrs.iter(),
            );
        }
    }
}

fn dial_kademlia_providers(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    metrics: &RuntimeMetrics,
    providers: &HashSet<Libp2pPeerId>,
) {
    metrics.record_kademlia_providers_found(providers.len());
    for provider in providers {
        if *provider == *swarm.local_peer_id()
            || !forwarder.is_configured_transport_peer(*provider)
            || swarm.is_connected(provider)
        {
            metrics.record_kademlia_provider_ignored();
            continue;
        }
        dial_configured_peer(swarm, forwarder, metrics, *provider);
    }
}

fn handle_relay_server_event(metrics: &RuntimeMetrics, event: &relay::Event) {
    match event {
        relay::Event::ReservationReqAccepted {
            src_peer_id,
            renewed,
        } => {
            metrics.record_relay_server_reservation_accepted();
            eprintln!("relay server accepted reservation from {src_peer_id} renewed={renewed}");
        }
        relay::Event::ReservationReqDenied {
            src_peer_id,
            status,
        } => {
            metrics.record_relay_server_reservation_denied();
            eprintln!("relay server denied reservation from {src_peer_id}: {status:?}");
        }
        relay::Event::ReservationClosed { src_peer_id } => {
            metrics.record_relay_server_reservation_closed();
            eprintln!("relay server reservation closed for {src_peer_id}");
        }
        relay::Event::ReservationTimedOut { src_peer_id } => {
            metrics.record_relay_server_reservation_timed_out();
            eprintln!("relay server reservation timed out for {src_peer_id}");
        }
        relay::Event::CircuitReqDenied {
            src_peer_id,
            dst_peer_id,
            status,
        } => {
            metrics.record_relay_server_circuit_denied();
            eprintln!("relay server denied circuit {src_peer_id} -> {dst_peer_id}: {status:?}");
        }
        relay::Event::CircuitReqAccepted {
            src_peer_id,
            dst_peer_id,
        } => {
            metrics.record_relay_server_circuit_accepted();
            eprintln!("relay server accepted circuit {src_peer_id} -> {dst_peer_id}");
        }
        relay::Event::CircuitClosed {
            src_peer_id,
            dst_peer_id,
            error,
        } => {
            metrics.record_relay_server_circuit_closed();
            eprintln!("relay server circuit closed {src_peer_id} -> {dst_peer_id}: {error:?}");
        }
        _ => {}
    }
}

fn handle_relay_event(
    swarm: &mut Swarm<Behaviour>,
    relay_readiness: &mut RelayReadiness,
    auto_relay: &mut AutoRelayState,
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &DiscoveredPeerAddresses,
    metrics: &RuntimeMetrics,
    discovery: &DiscoveryConfig,
    local_capabilities: &ControlCapabilities,
    identity: &NodeIdentity,
    event: &relay::client::Event,
) {
    let accepted_relay = record_relay_client_event(metrics, event);
    if let Some(relay_peer_id) = accepted_relay {
        auto_relay.record_reservation_accepted(relay_peer_id);
        relay_readiness.record_reservation_accepted(relay_peer_id);
        dial_relay_ready_configured_peers(
            swarm,
            relay_readiness,
            configured_peer_addresses,
            discovered_peer_addresses,
            metrics,
            relay_peer_id,
        );
        if discovery.kademlia {
            publish_kademlia_peer_address_record(
                swarm,
                &local_capabilities.network_name,
                local_capabilities.membership_tag.as_deref(),
                identity,
            );
        }
    }
}

fn expire_pending_auto_relay_reservations(
    auto_relay: &mut AutoRelayState,
    metrics: &RuntimeMetrics,
    now: Instant,
) {
    for (relay_peer, relay_address) in auto_relay.expire_pending_reservations(now) {
        metrics.record_auto_relay_reservation_failure();
        let evicted = auto_relay.record_reservation_failure(relay_peer);
        log_runtime_event(
            LogLevel::Warn,
            "auto_relay_reservation_timeout",
            &[
                ("relay", &relay_peer.to_string()),
                ("address", &relay_address.to_string()),
                ("evicted", &evicted.to_string()),
            ],
        );
    }
}

fn record_relay_client_event(
    metrics: &RuntimeMetrics,
    event: &relay::client::Event,
) -> Option<Libp2pPeerId> {
    match event {
        relay::client::Event::ReservationReqAccepted {
            relay_peer_id,
            renewal,
            ..
        } => {
            metrics.record_relay_reservation_accepted();
            eprintln!("relay reservation accepted by {relay_peer_id} renewal={renewal}");
            Some(*relay_peer_id)
        }
        relay::client::Event::OutboundCircuitEstablished { relay_peer_id, .. } => {
            metrics.record_relay_outbound_circuit_established();
            eprintln!("outbound relay circuit established via {relay_peer_id}");
            None
        }
        relay::client::Event::InboundCircuitEstablished { src_peer_id, .. } => {
            metrics.record_relay_inbound_circuit_established();
            eprintln!("inbound relay circuit established from {src_peer_id}");
            None
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn learn_peer_address(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    discovered_peer_addresses: &mut DiscoveredPeerAddresses,
    paths: &PathSet,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    address: Multiaddr,
    discovery: &DiscoveryConfig,
    source: DiscoveredPeerAddressSource,
) {
    if peer == *swarm.local_peer_id() {
        return;
    }
    if !forwarder.is_configured_transport_peer(peer) {
        if discovery.kademlia && address_targets_peer(peer, &address) {
            swarm
                .behaviour_mut()
                .kad
                .add_address(&peer, address.clone());
        }
        return;
    }
    if !address_targets_peer(peer, &address) {
        metrics.record_discovered_address_rejected();
        eprintln!("rejecting discovered address for {peer} with mismatched target: {address}");
        return;
    }
    if relayed_address_relay_peer(&address).is_some() {
        if !supports_relayed_peer_dial_transport(&address) {
            metrics.record_discovered_address_rejected();
            log_runtime_event(
                LogLevel::Info,
                "discovered_relayed_address_rejected",
                &[
                    ("peer", &peer.to_string()),
                    ("address", &address.to_string()),
                    ("reason", "unsupported_transport"),
                    ("source", source.as_str()),
                ],
            );
            return;
        }
        log_runtime_event(
            LogLevel::Info,
            "discovered_relayed_address_accepted",
            &[
                ("peer", &peer.to_string()),
                ("address", &address.to_string()),
                ("source", source.as_str()),
            ],
        );
    }

    if discovery.kademlia {
        swarm
            .behaviour_mut()
            .kad
            .add_address(&peer, address.clone());
    }

    metrics.record_discovered_address_accepted();
    discovered_peer_addresses.insert(peer, address.clone());

    if discovery.autonat
        && let Some(autonat) = swarm.behaviour_mut().autonat.as_mut()
    {
        autonat.add_server(peer, Some(address.clone()));
    }

    if !should_dial_discovered_address(paths, peer, swarm.is_connected(&peer), &address) {
        return;
    }

    let dial_address = peer_dial_address(peer, address);
    metrics.record_discovered_address_dial_attempt();
    if let Err(error) = swarm.dial(dial_address) {
        metrics.record_discovered_address_dial_failure();
        eprintln!("dial discovered peer {peer} failed: {error}");
    }
}

fn dial_configured_peer(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
) {
    if peer == *swarm.local_peer_id()
        || !forwarder.is_configured_transport_peer(peer)
        || swarm.is_connected(&peer)
    {
        return;
    }

    metrics.record_kademlia_provider_dial_attempt();
    if let Err(error) = swarm.dial(peer) {
        metrics.record_kademlia_provider_dial_failure();
        eprintln!("dial discovered provider {peer} failed: {error}");
    }
}

fn peer_dial_address(peer: Libp2pPeerId, address: Multiaddr) -> Multiaddr {
    address.with_p2p(peer).unwrap_or_else(|address| address)
}

fn address_targets_peer(peer: Libp2pPeerId, address: &Multiaddr) -> bool {
    discovered_address_target(address).is_none_or(|target| target == peer)
}

fn discovered_address_target(address: &Multiaddr) -> Option<Libp2pPeerId> {
    let mut direct_target = None;
    let mut relayed_target = None;
    let mut after_circuit = false;

    for protocol in address {
        match protocol {
            Protocol::P2p(peer) if after_circuit => relayed_target = Some(peer),
            Protocol::P2p(peer) => direct_target = Some(peer),
            Protocol::P2pCircuit => after_circuit = true,
            _ => {}
        }
    }

    if after_circuit {
        relayed_target
    } else {
        direct_target
    }
}

fn relayed_address_relay_peer(address: &Multiaddr) -> Option<Libp2pPeerId> {
    let mut relay_peer = None;

    for protocol in address {
        match protocol {
            Protocol::P2pCircuit => return relay_peer,
            Protocol::P2p(peer) => relay_peer = Some(peer),
            _ => {}
        }
    }

    None
}

fn print_metrics(
    metrics: &RuntimeMetrics,
    queue: crate::queue::QueueStats,
    path: crate::path::PathRuntimeStats,
) {
    eprintln!("metrics:");
    for line in metrics.snapshot_with_paths(queue, path).lines() {
        eprintln!("  {line}");
    }
}

fn runtime_path_stats(
    forwarder: &Forwarder,
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
) -> crate::path::PathRuntimeStats {
    paths.runtime_stats_for_peers(forwarder.configured_overlay_peers(), |peer| {
        packet_transport_support(peer_capabilities, peer)
    })
}

fn selected_path_mtu(
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    peer: PeerId,
    local_mtu: u16,
) -> u16 {
    let peer_mtu = peer_capabilities.effective_mtu_for(peer, local_mtu);
    let datagram_backend =
        local_packet_datagram_backend(peer_capabilities, packet_plane, packet_plane_quic, peer);
    let support = packet_transport_support_for_backend(peer_capabilities, peer, datagram_backend);
    let path_mtu = best_packet_transport_path(paths, peer, support)
        .map_or(peer_mtu, |path| path.effective_mtu(peer_mtu));
    selected_datagram_session_mtu(packet_plane, packet_plane_quic, datagram_backend, peer)
        .map_or(path_mtu, |session_mtu| path_mtu.min(session_mtu))
}

fn selected_path_probe_mtu(
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    backend: PacketDatagramBackend,
    peer: PeerId,
    local_mtu: u16,
) -> u16 {
    let current = selected_path_mtu(
        paths,
        peer_capabilities,
        packet_plane,
        packet_plane_quic,
        peer,
        local_mtu,
    );
    let ceiling = selected_path_mtu_ceiling(
        peer_capabilities,
        packet_plane,
        packet_plane_quic,
        backend,
        peer,
        local_mtu,
    );
    current.saturating_add(PATH_PROBE_MTU_STEP).min(ceiling)
}

fn selected_path_mtu_ceiling(
    peer_capabilities: &PeerCapabilities,
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    backend: PacketDatagramBackend,
    peer: PeerId,
    local_mtu: u16,
) -> u16 {
    let peer_mtu = peer_capabilities.effective_mtu_for(peer, local_mtu);
    selected_datagram_session_mtu(packet_plane, packet_plane_quic, Some(backend), peer)
        .map_or(peer_mtu, |session_mtu| peer_mtu.min(session_mtu))
}

fn selected_datagram_session_mtu(
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    backend: Option<PacketDatagramBackend>,
    peer: PeerId,
) -> Option<u16> {
    match backend {
        Some(PacketDatagramBackend::OwnedUdp) => {
            packet_plane.and_then(|packet_plane| packet_plane.session_mtu_for(peer))
        }
        Some(PacketDatagramBackend::OwnedQuic) => {
            packet_plane_quic.and_then(|packet_plane| packet_plane.session_mtu_for(peer))
        }
        None => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn maybe_confirm_path_mtu_probe(
    paths: &mut PathSet,
    peer_capabilities: &PeerCapabilities,
    packet_plane: Option<&PacketPlaneRuntime>,
    packet_plane_quic: Option<&PacketPlaneQuicRuntime>,
    backend: PacketDatagramBackend,
    metrics: &RuntimeMetrics,
    peer: PeerId,
    confirmed_mtu: u16,
    local_mtu: u16,
) {
    let support = packet_transport_support_for_backend(peer_capabilities, peer, Some(backend));
    let Some(path) = paths.best_supported_for(peer, support) else {
        return;
    };
    if path.kind != packet_datagram_backend_path_kind(backend) {
        return;
    }
    let ceiling = selected_path_mtu_ceiling(
        peer_capabilities,
        packet_plane,
        packet_plane_quic,
        backend,
        peer,
        local_mtu,
    );
    if paths.raise_path_mtu(peer, path.kind, confirmed_mtu, ceiling) {
        metrics.record_outbound_path_mtu_update();
        metrics.record_outbound_path_mtu_probe_confirmation();
        let mtu = confirmed_mtu.min(ceiling);
        log_runtime_event(
            LogLevel::Info,
            "path_mtu_probe_confirmed",
            &[
                ("peer", &peer.to_string()),
                ("path", path.kind.wire_name()),
                ("mtu", &mtu.to_string()),
            ],
        );
    }
}

const fn initial_path_mtu(kind: PathKind, local_mtu: u16) -> u16 {
    match kind {
        PathKind::CircuitRelay => {
            if local_mtu < 1_200 {
                local_mtu
            } else {
                1_200
            }
        }
        PathKind::DirectUdpDatagram
        | PathKind::DirectQuicDatagram
        | PathKind::DirectQuicStream
        | PathKind::DirectTcpStream => local_mtu,
    }
}

fn outbound_drop_reason(error: &ForwardError) -> PacketDropReason {
    match error {
        ForwardError::NoRoute(_) => PacketDropReason::NoRoute,
        ForwardError::NoTransportPeer(_) => PacketDropReason::NoTransportPeer,
        ForwardError::PacketTooLarge { .. } => PacketDropReason::PacketTooLarge,
        ForwardError::UnauthorizedLocalSource { .. } => PacketDropReason::UnauthorizedSource,
        ForwardError::TruncatedIpPacket { .. }
        | ForwardError::UnsupportedIpVersion(_)
        | ForwardError::PayloadLengthMismatch { .. }
        | ForwardError::UnexpectedPayload(_)
        | ForwardError::Frame(_)
        | ForwardError::Config(_)
        | ForwardError::MembershipRecord(_)
        | ForwardError::Route(_)
        | ForwardError::UnauthorizedPeer(_)
        | ForwardError::UnauthorizedLocalDestination { .. }
        | ForwardError::ReplayedPacket { .. }
        | ForwardError::PacketOutsideReplayWindow { .. } => PacketDropReason::MalformedPacket,
        ForwardError::Enqueue(crate::queue::EnqueueError::PacketTooLarge { .. }) => {
            PacketDropReason::PacketTooLarge
        }
        ForwardError::Enqueue(crate::queue::EnqueueError::QueueFull { .. }) => {
            PacketDropReason::QueueFull
        }
    }
}

fn packet_plane_drop_reason(error: &PacketPlaneIoError) -> PacketDropReason {
    match error {
        PacketPlaneIoError::NoSession { .. }
        | PacketPlaneIoError::NoSessions
        | PacketPlaneIoError::UnknownEndpoint { .. } => PacketDropReason::NoTransportPeer,
        PacketPlaneIoError::Datagram(
            crate::runtime::packet_plane::PacketPlaneDatagramError::PayloadTooLarge { .. },
        ) => PacketDropReason::PacketTooLarge,
        PacketPlaneIoError::Datagram(
            crate::runtime::packet_plane::PacketPlaneDatagramError::ReplayedDatagram { .. }
            | crate::runtime::packet_plane::PacketPlaneDatagramError::DatagramOutsideReplayWindow {
                ..
            },
        ) => PacketDropReason::Replay,
        PacketPlaneIoError::Datagram(_) | PacketPlaneIoError::UnexpectedEndpoint { .. } => {
            PacketDropReason::MalformedPacket
        }
        PacketPlaneIoError::NoListener { .. } | PacketPlaneIoError::Io(_) => {
            PacketDropReason::NoTransportPeer
        }
    }
}

fn packet_plane_send_drop_reason(error: &PacketPlaneSendError) -> PacketDropReason {
    match error {
        PacketPlaneSendError::Udp(error) => packet_plane_drop_reason(error),
        PacketPlaneSendError::Quic(PacketPlaneQuicError::Datagram(
            crate::runtime::packet_plane::PacketPlaneDatagramError::PayloadTooLarge { .. },
        )) => PacketDropReason::PacketTooLarge,
        PacketPlaneSendError::Quic(PacketPlaneQuicError::Datagram(
            crate::runtime::packet_plane::PacketPlaneDatagramError::ReplayedDatagram { .. }
            | crate::runtime::packet_plane::PacketPlaneDatagramError::DatagramOutsideReplayWindow {
                ..
            },
        )) => PacketDropReason::Replay,
        PacketPlaneSendError::Quic(
            PacketPlaneQuicError::Datagram(_)
            | PacketPlaneQuicError::Session(_)
            | PacketPlaneQuicError::Certificate(_)
            | PacketPlaneQuicError::Rustls(_)
            | PacketPlaneQuicError::ClientVerifier(_),
        ) => PacketDropReason::MalformedPacket,
        PacketPlaneSendError::MissingUdpRuntime
        | PacketPlaneSendError::MissingQuicRuntime
        | PacketPlaneSendError::Quic(
            PacketPlaneQuicError::NoConnection { .. }
            | PacketPlaneQuicError::NoSessions
            | PacketPlaneQuicError::EndpointClosed
            | PacketPlaneQuicError::Connect(_)
            | PacketPlaneQuicError::Connection(_)
            | PacketPlaneQuicError::PeerConnection { .. }
            | PacketPlaneQuicError::SendDatagram(_)
            | PacketPlaneQuicError::Io(_),
        ) => PacketDropReason::NoTransportPeer,
    }
}

fn packet_plane_inbound_drop_reason(error: &PacketPlaneIoError) -> PacketDropReason {
    match error {
        PacketPlaneIoError::NoSession { .. }
        | PacketPlaneIoError::NoSessions
        | PacketPlaneIoError::UnknownEndpoint { .. }
        | PacketPlaneIoError::UnexpectedEndpoint { .. } => PacketDropReason::UnauthorizedPeer,
        PacketPlaneIoError::Datagram(error) => packet_plane_datagram_inbound_drop_reason(error),
        PacketPlaneIoError::NoListener { .. } | PacketPlaneIoError::Io(_) => {
            PacketDropReason::MalformedPacket
        }
    }
}

fn packet_plane_datagram_inbound_drop_reason(
    error: &crate::runtime::packet_plane::PacketPlaneDatagramError,
) -> PacketDropReason {
    match error {
        crate::runtime::packet_plane::PacketPlaneDatagramError::PayloadTooLarge { .. } => {
            PacketDropReason::PacketTooLarge
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::ReplayedDatagram { .. }
        | crate::runtime::packet_plane::PacketPlaneDatagramError::DatagramOutsideReplayWindow {
            ..
        } => PacketDropReason::Replay,
        _ => PacketDropReason::MalformedPacket,
    }
}

fn packet_plane_inbound_metric_reason(error: &PacketPlaneIoError) -> PacketPlaneDropReason {
    match error {
        PacketPlaneIoError::NoListener { .. } => PacketPlaneDropReason::NoListener,
        PacketPlaneIoError::NoSession { .. } => PacketPlaneDropReason::NoSession,
        PacketPlaneIoError::NoSessions => PacketPlaneDropReason::NoSessions,
        PacketPlaneIoError::UnknownEndpoint { .. } => PacketPlaneDropReason::UnknownEndpoint,
        PacketPlaneIoError::UnexpectedEndpoint { .. } => PacketPlaneDropReason::UnexpectedEndpoint,
        PacketPlaneIoError::Io(_) => PacketPlaneDropReason::IoError,
        PacketPlaneIoError::Datagram(error) => packet_plane_datagram_metric_reason(error),
    }
}

fn packet_plane_datagram_metric_reason(
    error: &crate::runtime::packet_plane::PacketPlaneDatagramError,
) -> PacketPlaneDropReason {
    match error {
        crate::runtime::packet_plane::PacketPlaneDatagramError::Encrypt => {
            PacketPlaneDropReason::Encrypt
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::Decrypt => {
            PacketPlaneDropReason::Decrypt
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::InvalidMagic => {
            PacketPlaneDropReason::InvalidMagic
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::Truncated { .. } => {
            PacketPlaneDropReason::Truncated
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::UnsupportedVersion(_) => {
            PacketPlaneDropReason::UnsupportedVersion
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::CiphertextTooLarge { .. } => {
            PacketPlaneDropReason::CiphertextTooLarge
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::PayloadTooLarge { .. } => {
            PacketPlaneDropReason::PayloadTooLarge
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::FrameDecode(_) => {
            PacketPlaneDropReason::FrameDecode
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::FrameLengthMismatch { .. } => {
            PacketPlaneDropReason::FrameLengthMismatch
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::HeaderMismatch { .. } => {
            PacketPlaneDropReason::HeaderMismatch
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::ReplayedDatagram { .. } => {
            PacketPlaneDropReason::ReplayedDatagram
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::DatagramOutsideReplayWindow {
            ..
        } => PacketPlaneDropReason::DatagramOutsideReplayWindow,
        crate::runtime::packet_plane::PacketPlaneDatagramError::TrailingBytes { .. } => {
            PacketPlaneDropReason::TrailingBytes
        }
    }
}

fn packet_plane_send_failure_demotes_path(error: &PacketPlaneIoError) -> bool {
    matches!(
        error,
        PacketPlaneIoError::NoListener { .. }
            | PacketPlaneIoError::NoSession { .. }
            | PacketPlaneIoError::NoSessions
            | PacketPlaneIoError::Io(_)
    )
}

fn packet_plane_send_error_demotes_path(error: &PacketPlaneSendError) -> bool {
    match error {
        PacketPlaneSendError::MissingUdpRuntime | PacketPlaneSendError::MissingQuicRuntime => true,
        PacketPlaneSendError::Udp(error) => packet_plane_send_failure_demotes_path(error),
        PacketPlaneSendError::Quic(error) => matches!(
            error,
            PacketPlaneQuicError::NoConnection { .. }
                | PacketPlaneQuicError::EndpointClosed
                | PacketPlaneQuicError::NoSessions
                | PacketPlaneQuicError::Connect(_)
                | PacketPlaneQuicError::Connection(_)
                | PacketPlaneQuicError::PeerConnection { .. }
                | PacketPlaneQuicError::SendDatagram(_)
                | PacketPlaneQuicError::Io(_)
        ),
    }
}

fn inbound_drop_reason(error: &ForwardError) -> PacketDropReason {
    match error {
        ForwardError::UnauthorizedPeer(_) => PacketDropReason::UnauthorizedPeer,
        ForwardError::Route(RouteError::UnauthorizedSource { .. }) => {
            PacketDropReason::UnauthorizedSource
        }
        ForwardError::UnauthorizedLocalDestination { .. } => {
            PacketDropReason::UnauthorizedDestination
        }
        ForwardError::PacketTooLarge { .. } => PacketDropReason::PacketTooLarge,
        ForwardError::UnexpectedPayload(_) => PacketDropReason::UnexpectedPayload,
        ForwardError::ReplayedPacket { .. } | ForwardError::PacketOutsideReplayWindow { .. } => {
            PacketDropReason::Replay
        }
        ForwardError::PayloadLengthMismatch { .. }
        | ForwardError::TruncatedIpPacket { .. }
        | ForwardError::UnsupportedIpVersion(_)
        | ForwardError::Frame(_)
        | ForwardError::Config(_)
        | ForwardError::MembershipRecord(_)
        | ForwardError::Route(_)
        | ForwardError::NoRoute(_)
        | ForwardError::NoTransportPeer(_)
        | ForwardError::Enqueue(_)
        | ForwardError::UnauthorizedLocalSource { .. } => PacketDropReason::MalformedPacket,
    }
}

type AuditFields = Vec<(&'static str, String)>;

fn audit_packet_request_rejection(peer: Libp2pPeerId, frame: &Frame, error: &ForwardError) {
    log_runtime_event_owned(
        LogLevel::Warn,
        "packet_rejected",
        &packet_rejection_audit_fields(peer, frame, error),
    );
}

fn audit_packet_rate_limit_rejection(peer: Libp2pPeerId, frame: &Frame, limit_per_second: u32) {
    let mut fields = packet_base_audit_fields(peer, frame, "rate_limited");
    fields.push(("limit_per_second", limit_per_second.to_string()));
    log_runtime_event_owned(LogLevel::Warn, "packet_rejected", &fields);
}

fn audit_packet_response_rejection(peer: Libp2pPeerId, reason: PacketRejectionReason) {
    log_runtime_event_owned(
        LogLevel::Warn,
        "packet_response_rejected",
        &vec![
            ("peer", peer.to_string()),
            ("reason", packet_response_rejection_name(reason).to_owned()),
        ],
    );
}

fn audit_control_capabilities_rejection(peer: Libp2pPeerId, reason: ControlRejectionReason) {
    log_runtime_event_owned(
        LogLevel::Warn,
        "control_capabilities_rejected",
        &vec![
            ("peer", peer.to_string()),
            ("reason", control_rejection_name(reason).to_owned()),
        ],
    );
}

fn audit_service_status_rejection(peer: Libp2pPeerId, reason: ServiceRejectionReason) {
    log_runtime_event_owned(
        LogLevel::Warn,
        "service_status_rejected",
        &vec![
            ("peer", peer.to_string()),
            ("reason", service_rejection_name(reason).to_owned()),
        ],
    );
}

fn log_runtime_event_owned(level: LogLevel, event: &str, fields: &AuditFields) {
    let fields = fields
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect::<Vec<_>>();
    log_runtime_event(level, event, &fields);
}

fn packet_rejection_audit_fields(
    peer: Libp2pPeerId,
    frame: &Frame,
    error: &ForwardError,
) -> AuditFields {
    packet_base_audit_fields(peer, frame, packet_rejection_error_name(error))
}

fn packet_base_audit_fields(peer: Libp2pPeerId, frame: &Frame, reason: &str) -> AuditFields {
    let mut fields = vec![
        ("peer", peer.to_string()),
        ("reason", reason.to_owned()),
        (
            "payload_type",
            payload_type_name(frame.header.payload_type).to_owned(),
        ),
        ("session_id", frame.header.session_id.to_string()),
        ("sequence", frame.header.sequence.to_string()),
        ("payload_len", frame.payload.len().to_string()),
    ];

    if let Ok(source) = packet_source(&frame.payload) {
        fields.push(("source", source.to_string()));
    }
    if let Ok(destination) = packet_destination(&frame.payload) {
        fields.push(("destination", destination.to_string()));
    }

    fields
}

fn packet_rejection_error_name(error: &ForwardError) -> &'static str {
    match error {
        ForwardError::UnauthorizedPeer(_) => "unauthorized_peer",
        ForwardError::Route(RouteError::UnauthorizedSource { .. })
        | ForwardError::UnauthorizedLocalSource { .. } => "unauthorized_source",
        ForwardError::UnauthorizedLocalDestination { .. } => "unauthorized_destination",
        ForwardError::ReplayedPacket { .. } => "replayed_packet",
        ForwardError::PacketOutsideReplayWindow { .. } => "packet_outside_replay_window",
        ForwardError::PacketTooLarge { .. }
        | ForwardError::Enqueue(EnqueueError::PacketTooLarge { .. }) => "packet_too_large",
        ForwardError::UnexpectedPayload(_) => "unexpected_payload",
        ForwardError::PayloadLengthMismatch { .. } => "payload_length_mismatch",
        ForwardError::TruncatedIpPacket { .. } => "truncated_ip_packet",
        ForwardError::UnsupportedIpVersion(_) => "unsupported_ip_version",
        ForwardError::Frame(_) => "malformed_frame",
        ForwardError::Route(_) => "route_authorization_failed",
        ForwardError::NoRoute(_) => "no_route",
        ForwardError::NoTransportPeer(_) => "no_transport_peer",
        ForwardError::Config(_) => "configuration_error",
        ForwardError::MembershipRecord(_) => "membership_record_error",
        ForwardError::Enqueue(EnqueueError::QueueFull { .. }) => "queue_full",
    }
}

fn packet_plane_io_error_name(error: &PacketPlaneIoError) -> &'static str {
    match error {
        PacketPlaneIoError::NoListener { .. } => "no_listener",
        PacketPlaneIoError::NoSession { .. } => "no_session",
        PacketPlaneIoError::NoSessions => "no_sessions",
        PacketPlaneIoError::UnknownEndpoint { .. } => "unknown_endpoint",
        PacketPlaneIoError::UnexpectedEndpoint { .. } => "unexpected_endpoint",
        PacketPlaneIoError::Io(_) => "io_error",
        PacketPlaneIoError::Datagram(error) => packet_plane_datagram_error_name(error),
    }
}

fn packet_plane_send_error_name(error: &PacketPlaneSendError) -> &'static str {
    match error {
        PacketPlaneSendError::MissingUdpRuntime => "missing_udp_runtime",
        PacketPlaneSendError::MissingQuicRuntime => "missing_quic_runtime",
        PacketPlaneSendError::Udp(error) => packet_plane_io_error_name(error),
        PacketPlaneSendError::Quic(error) => packet_plane_quic_error_name(error),
    }
}

fn packet_plane_quic_error_name(error: &PacketPlaneQuicError) -> &'static str {
    match error {
        PacketPlaneQuicError::Io(_) => "io_error",
        PacketPlaneQuicError::Certificate(_) => "certificate_error",
        PacketPlaneQuicError::Rustls(_) => "rustls_error",
        PacketPlaneQuicError::ClientVerifier(_) => "client_verifier_error",
        PacketPlaneQuicError::Connect(_) => "connect_error",
        PacketPlaneQuicError::Connection(_) => "connection_error",
        PacketPlaneQuicError::PeerConnection { .. } => "peer_connection_error",
        PacketPlaneQuicError::EndpointClosed => "endpoint_closed",
        PacketPlaneQuicError::NoSessions => "no_sessions",
        PacketPlaneQuicError::NoConnection { .. } => "no_connection",
        PacketPlaneQuicError::SendDatagram(_) => "send_datagram",
        PacketPlaneQuicError::Datagram(error) => packet_plane_datagram_error_name(error),
        PacketPlaneQuicError::Session(_) => "session_error",
    }
}

fn packet_plane_quic_error_detail(error: &PacketPlaneQuicError) -> String {
    format!("{}: {error:?}", packet_plane_quic_error_name(error))
}

fn packet_plane_datagram_error_name(
    error: &crate::runtime::packet_plane::PacketPlaneDatagramError,
) -> &'static str {
    match error {
        crate::runtime::packet_plane::PacketPlaneDatagramError::Encrypt => "encrypt",
        crate::runtime::packet_plane::PacketPlaneDatagramError::Decrypt => "decrypt",
        crate::runtime::packet_plane::PacketPlaneDatagramError::InvalidMagic => "invalid_magic",
        crate::runtime::packet_plane::PacketPlaneDatagramError::Truncated { .. } => "truncated",
        crate::runtime::packet_plane::PacketPlaneDatagramError::UnsupportedVersion(_) => {
            "unsupported_version"
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::CiphertextTooLarge { .. } => {
            "ciphertext_too_large"
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::PayloadTooLarge { .. } => {
            "payload_too_large"
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::FrameDecode(_) => "frame_decode",
        crate::runtime::packet_plane::PacketPlaneDatagramError::FrameLengthMismatch { .. } => {
            "frame_length_mismatch"
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::HeaderMismatch { .. } => {
            "header_mismatch"
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::ReplayedDatagram { .. } => {
            "replayed_datagram"
        }
        crate::runtime::packet_plane::PacketPlaneDatagramError::DatagramOutsideReplayWindow {
            ..
        } => "datagram_outside_replay_window",
        crate::runtime::packet_plane::PacketPlaneDatagramError::TrailingBytes { .. } => {
            "trailing_bytes"
        }
    }
}

fn packet_response_rejection_name(reason: PacketRejectionReason) -> &'static str {
    match reason {
        PacketRejectionReason::MalformedPacket => "malformed_packet",
        PacketRejectionReason::PacketTooLarge => "packet_too_large",
        PacketRejectionReason::Replay => "replay",
        PacketRejectionReason::UnauthorizedPeer => "unauthorized_peer",
        PacketRejectionReason::UnauthorizedSource => "unauthorized_source",
        PacketRejectionReason::UnauthorizedDestination => "unauthorized_destination",
        PacketRejectionReason::UnexpectedPayload => "unexpected_payload",
        PacketRejectionReason::RateLimited => "rate_limited",
    }
}

fn control_rejection_name(reason: ControlRejectionReason) -> &'static str {
    match reason {
        ControlRejectionReason::UnauthorizedPeer => "unauthorized_peer",
        ControlRejectionReason::WrongNetwork => "wrong_network",
        ControlRejectionReason::MembershipMismatch => "membership_mismatch",
        ControlRejectionReason::UnsupportedWireVersion => "unsupported_wire_version",
        ControlRejectionReason::UnsupportedPacketProtocol => "unsupported_packet_protocol",
        ControlRejectionReason::UnsupportedPacketHeaderLength => "unsupported_packet_header_length",
        ControlRejectionReason::InvalidEffectiveMtu => "invalid_effective_mtu",
        ControlRejectionReason::UnsupportedPreferredPath => "unsupported_preferred_path",
        ControlRejectionReason::UnauthorizedRouteAdvertisement => {
            "unauthorized_route_advertisement"
        }
        ControlRejectionReason::InvalidOwnedQuicCertificate => "invalid_owned_quic_certificate",
        ControlRejectionReason::InvalidMembershipRecord => "invalid_membership_record",
    }
}

fn service_rejection_name(reason: ServiceRejectionReason) -> &'static str {
    match reason {
        ServiceRejectionReason::UnauthorizedPeer => "unauthorized_peer",
        ServiceRejectionReason::WrongNetwork => "wrong_network",
        ServiceRejectionReason::MembershipMismatch => "membership_mismatch",
    }
}

fn payload_type_name(payload_type: PayloadType) -> &'static str {
    match payload_type {
        PayloadType::IpPacket => "ip_packet",
        PayloadType::Keepalive => "keepalive",
        PayloadType::PathProbe => "path_probe",
    }
}

fn packet_rejection_reason(reason: PacketDropReason) -> PacketRejectionReason {
    match reason {
        PacketDropReason::PacketTooLarge | PacketDropReason::QueueFull => {
            PacketRejectionReason::PacketTooLarge
        }
        PacketDropReason::Replay => PacketRejectionReason::Replay,
        PacketDropReason::UnauthorizedPeer | PacketDropReason::NoTransportPeer => {
            PacketRejectionReason::UnauthorizedPeer
        }
        PacketDropReason::UnauthorizedSource => PacketRejectionReason::UnauthorizedSource,
        PacketDropReason::UnauthorizedDestination => PacketRejectionReason::UnauthorizedDestination,
        PacketDropReason::UnexpectedPayload => PacketRejectionReason::UnexpectedPayload,
        PacketDropReason::RateLimited => PacketRejectionReason::RateLimited,
        PacketDropReason::MalformedPacket | PacketDropReason::NoRoute => {
            PacketRejectionReason::MalformedPacket
        }
    }
}

fn packet_rejection_drop_reason(reason: PacketRejectionReason) -> PacketDropReason {
    match reason {
        PacketRejectionReason::MalformedPacket => PacketDropReason::MalformedPacket,
        PacketRejectionReason::PacketTooLarge => PacketDropReason::PacketTooLarge,
        PacketRejectionReason::Replay => PacketDropReason::Replay,
        PacketRejectionReason::UnauthorizedPeer => PacketDropReason::UnauthorizedPeer,
        PacketRejectionReason::UnauthorizedSource => PacketDropReason::UnauthorizedSource,
        PacketRejectionReason::UnauthorizedDestination => PacketDropReason::UnauthorizedDestination,
        PacketRejectionReason::UnexpectedPayload => PacketDropReason::UnexpectedPayload,
        PacketRejectionReason::RateLimited => PacketDropReason::RateLimited,
    }
}

#[derive(Debug)]
pub enum RunnerError {
    Config(crate::config::ConfigError),
    P2p(P2pBuildError),
    Forward(ForwardError),
    Tun(TunRuntimeError),
    ControlSocket(io::Error),
    PacketPlane(io::Error),
    PacketPlaneQuic(PacketPlaneQuicError),
    PacketResponseDropped,
    ControlResponseDropped,
    ServiceResponseDropped,
}

impl From<crate::config::ConfigError> for RunnerError {
    fn from(error: crate::config::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<P2pBuildError> for RunnerError {
    fn from(error: P2pBuildError) -> Self {
        Self::P2p(error)
    }
}

impl From<ForwardError> for RunnerError {
    fn from(error: ForwardError) -> Self {
        Self::Forward(error)
    }
}

impl From<TunRuntimeError> for RunnerError {
    fn from(error: TunRuntimeError) -> Self {
        Self::Tun(error)
    }
}

impl From<io::Error> for RunnerError {
    fn from(error: io::Error) -> Self {
        Self::ControlSocket(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        net::{Ipv4Addr, SocketAddr},
    };

    use libp2p::{
        core::{Endpoint, transport::PortUse},
        identity::Keypair,
    };
    use tokio::time::{Duration as TokioDuration, timeout};

    use crate::{
        config::{
            BootstrapPeerConfig, Config, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig,
            RelayConfig, ResourceConfig, RouteConfig,
        },
        identity::NodeIdentity,
        membership::{MembershipRecordOptions, MembershipRole, issue_membership_record_at},
        route::builtin_ipv4,
        runtime::control::ControlRoute,
        runtime::packet_plane::{
            PacketPlaneEphemeralSecret, PacketPlaneHandshake, PacketPlaneHandshakeKind,
            PacketPlaneHandshakeParams, PacketPlaneSessionSnapshot, VerifiedPacketPlaneHandshake,
        },
    };

    use super::*;

    fn peer_id() -> Libp2pPeerId {
        Keypair::generate_ed25519().public().to_peer_id()
    }

    fn ipv4_packet(source: Ipv4Addr, destination: Ipv4Addr) -> Vec<u8> {
        let mut packet = vec![0; 20];
        packet[0] = 0x45;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        packet
    }

    fn ipv4_tcp_packet(
        source: Ipv4Addr,
        destination: Ipv4Addr,
        source_port: u16,
        destination_port: u16,
    ) -> Vec<u8> {
        let mut packet = vec![0; 24];
        packet[0] = 0x45;
        packet[9] = 6;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        packet[20..22].copy_from_slice(&source_port.to_be_bytes());
        packet[22..24].copy_from_slice(&destination_port.to_be_bytes());
        packet
    }

    fn queue_drain_context<'a>(
        paths: &'a mut PathSet,
        peer_capabilities: &'a PeerCapabilities,
        packet_in_flight: &'a mut PacketInFlight,
        metrics: &'a RuntimeMetrics,
    ) -> QueueDrainContext<'a> {
        let last_blocked_queue_redial = Box::leak(Box::new(None));
        QueueDrainContext {
            paths,
            peer_capabilities,
            bootstrap_addresses: &[],
            relay_addresses: &[],
            configured_peer_addresses: &[],
            discovered_peer_addresses: &[],
            packet_in_flight,
            last_blocked_queue_redial,
            writer: None,
            packet_plane: None,
            packet_plane_quic: None,
            metrics,
        }
    }

    fn test_packet_plane_secret(byte: u8) -> PacketPlaneEphemeralSecret {
        PacketPlaneEphemeralSecret::from_bytes(
            [byte; crate::runtime::packet_plane::PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN],
        )
    }

    fn verified_test_packet_plane_handshake(
        kind: PacketPlaneHandshakeKind,
        identity: &crate::identity::NodeIdentity,
        secret: &PacketPlaneEphemeralSecret,
        mtu: u16,
        endpoint: std::net::SocketAddr,
    ) -> VerifiedPacketPlaneHandshake {
        let (session_id, nonce) = match kind {
            PacketPlaneHandshakeKind::Hello => (11, 101),
            PacketPlaneHandshakeKind::Accept => (13, 103),
        };
        PacketPlaneHandshake::signed(
            kind,
            identity,
            PacketPlaneHandshakeParams {
                network_name: "lab".to_owned(),
                session_id,
                nonce,
                mtu,
                ephemeral_public_key: secret.public_key(),
                endpoint,
            },
        )
        .expect("signed handshake")
        .verify("lab", None)
        .expect("verified handshake")
    }

    fn packet_plane_test_capabilities(endpoint: std::net::SocketAddr) -> ControlCapabilities {
        let mut capabilities =
            ControlCapabilities::local("lab", None, 1280).with_owned_udp_packet_plane(true);
        capabilities.preferred_path = PathKind::DirectUdpDatagram.wire_name().to_owned();
        capabilities.packet_endpoint_candidates = vec![endpoint.to_string()];
        capabilities
    }

    fn test_relay_infrastructure_snapshot() -> (Libp2pPeerId, Multiaddr, RelayInfrastructureSnapshot)
    {
        let peer = peer_id();
        let address: Multiaddr = format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer}")
            .parse()
            .expect("relay infrastructure address");
        let snapshot = RelayInfrastructureSnapshot {
            peers: vec![RelayInfrastructurePeerSnapshot {
                peer,
                address: address.clone(),
                connected: true,
            }],
        };
        (peer, address, snapshot)
    }

    fn test_packet_plane_snapshot(peer: PeerId) -> PacketPlaneSnapshot {
        PacketPlaneSnapshot {
            listeners: vec!["127.0.0.1:51820".parse::<SocketAddr>().expect("listener")],
            sessions: vec![PacketPlaneSessionSnapshot {
                peer,
                endpoint: "127.0.0.1:51821".parse().expect("endpoint"),
                mtu: 1200,
                role: PacketPlaneSessionRole::Responder,
                local_session_id: 13,
                remote_session_id: 11,
            }],
        }
    }

    fn assert_auto_relay_lines(lines: &[String], snapshot: AutoRelaySnapshot) {
        assert!(lines.contains(&format!(
            "auto_relay_policy_candidates {}",
            snapshot.max_candidates
        )));
        assert!(lines.contains(&format!(
            "auto_relay_policy_reservations {}",
            snapshot.max_reservations
        )));
        assert!(lines.contains(&format!(
            "auto_relay_policy_retry_seconds {}",
            snapshot.retry_interval_seconds
        )));
        assert!(lines.contains(&format!(
            "auto_relay_private_reachability {}",
            snapshot.private_reachability
        )));
        assert!(lines.contains(&format!(
            "auto_relay_current_candidates {}",
            snapshot.candidates
        )));
        assert!(lines.contains(&format!(
            "auto_relay_active_reservations {}",
            snapshot.reservations
        )));
        assert!(lines.contains(&format!(
            "auto_relay_pending_retries {}",
            snapshot.pending_retries
        )));
    }

    fn assert_lines_contain(lines: &[String], expected: &[&str]) {
        for line in expected {
            assert!(lines.contains(&(*line).to_owned()), "missing line: {line}");
        }
    }

    struct RuntimeStateLinesFixture {
        lines: Vec<String>,
        remote: Libp2pPeerId,
        remote_overlay: PeerId,
        infrastructure: Libp2pPeerId,
        infrastructure_address: Multiaddr,
        auto_relay: AutoRelaySnapshot,
    }

    fn runtime_state_lines_fixture() -> RuntimeStateLinesFixture {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_with_peer(&local_identity, remote);
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1200)
                .with_advertised_routes(vec![ControlRoute::new("10.0.0.2/32", 100)]),
        );
        let metrics = RuntimeMetrics::default();
        metrics.record_outbound_path_probe_sent();
        metrics.record_outbound_path_probe_ack_sent();
        metrics.record_outbound_path_mtu_probe_confirmation();
        metrics.record_dcutr_result(true);
        metrics.record_dcutr_result(false);
        metrics.record_autonat_probe_scheduled();
        metrics.record_autonat_status(AutoNatReachability::Public);
        let (infrastructure, infrastructure_address, relay_infrastructure) =
            test_relay_infrastructure_snapshot();
        let packet_plane = test_packet_plane_snapshot(remote_overlay);
        let auto_relay = AutoRelaySnapshot {
            max_candidates: 16,
            max_reservations: 4,
            retry_interval_seconds: 60,
            private_reachability: true,
            candidates: 3,
            reservations: 2,
            pending_retries: 1,
        };
        let lines = runtime_state_lines(&RuntimeStateView {
            forwarder: &forwarder,
            paths: &paths,
            peer_capabilities: &peer_capabilities,
            metrics: &metrics,
            queue: crate::queue::QueueStats::default(),
            path_stats: runtime_path_stats(&forwarder, &paths, &peer_capabilities),
            packet_in_flight: PacketInFlightStats {
                packets: 2,
                peers: 1,
                shards: 2,
                limit_per_peer: 256,
            },
            auto_relay,
            relay_infrastructure: &relay_infrastructure,
            packet_plane: &packet_plane,
            packet_plane_quic: &PacketPlaneQuicSnapshot::default(),
            packet_plane_session_ttl: Duration::from_secs(90),
            packet_plane_replay_windows_per_session: 256,
        });

        RuntimeStateLinesFixture {
            lines,
            remote,
            remote_overlay,
            infrastructure,
            infrastructure_address,
            auto_relay,
        }
    }

    fn config_with_peer(
        local_identity: &crate::identity::NodeIdentity,
        peer: Libp2pPeerId,
    ) -> Config {
        Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: peer.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        }
    }

    fn establish_test_packet_plane_sessions(
        sender: &mut PacketPlaneRuntime,
        receiver: &mut PacketPlaneRuntime,
        sender_identity: &crate::identity::NodeIdentity,
        receiver_identity: &crate::identity::NodeIdentity,
        sender_secret: &PacketPlaneEphemeralSecret,
        receiver_secret: &PacketPlaneEphemeralSecret,
        mtu: u16,
    ) {
        let sender_addr = sender.snapshot().listeners[0];
        let receiver_addr = receiver.snapshot().listeners[0];
        let hello = verified_test_packet_plane_handshake(
            PacketPlaneHandshakeKind::Hello,
            sender_identity,
            sender_secret,
            mtu,
            sender_addr,
        );
        let accept = verified_test_packet_plane_handshake(
            PacketPlaneHandshakeKind::Accept,
            receiver_identity,
            receiver_secret,
            mtu,
            receiver_addr,
        );

        sender
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                sender_secret,
                &hello,
                &accept,
            )
            .expect("sender packet-plane session");
        receiver
            .establish_session(
                PacketPlaneSessionRole::Responder,
                receiver_secret,
                &accept,
                &hello,
            )
            .expect("receiver packet-plane session");
    }

    async fn establish_test_packet_plane_quic_sessions(
        sender: &mut PacketPlaneQuicRuntime,
        receiver: &mut PacketPlaneQuicRuntime,
        sender_identity: &crate::identity::NodeIdentity,
        receiver_identity: &crate::identity::NodeIdentity,
        sender_secret: &PacketPlaneEphemeralSecret,
        receiver_secret: &PacketPlaneEphemeralSecret,
        mtu: u16,
    ) {
        let sender_addr = sender.local_addr();
        let receiver_addr = receiver.local_addr();
        let receiver_certificate = receiver.server_certificate();
        let hello = verified_test_packet_plane_handshake(
            PacketPlaneHandshakeKind::Hello,
            sender_identity,
            sender_secret,
            mtu,
            sender_addr,
        );
        let accept = verified_test_packet_plane_handshake(
            PacketPlaneHandshakeKind::Accept,
            receiver_identity,
            receiver_secret,
            mtu,
            receiver_addr,
        );

        let (connect, accept_connection) = tokio::join!(
            sender.connect_peer(accept.peer, receiver_addr, receiver_certificate),
            receiver.accept_peer(hello.peer)
        );
        connect.expect("sender QUIC connection");
        accept_connection.expect("receiver QUIC connection");

        sender
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                sender_secret,
                &hello,
                &accept,
            )
            .expect("sender QUIC packet-plane session");
        receiver
            .establish_session(
                PacketPlaneSessionRole::Responder,
                receiver_secret,
                &accept,
                &hello,
            )
            .expect("receiver QUIC packet-plane session");
    }

    #[tokio::test]
    async fn current_packet_plane_quic_snapshot_reports_runtime_sessions() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let mut local = PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("local quic"))
            .expect("local quic");
        let mut remote = PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("remote quic"))
            .expect("remote quic");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_quic_sessions(
            &mut local,
            &mut remote,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1_200,
        )
        .await;
        let remote_overlay = remote_identity
            .peer_id
            .parse::<PeerId>()
            .expect("remote overlay");

        let snapshot = current_packet_plane_quic_snapshot(Some(&local));

        assert_eq!(snapshot.listener, Some(local.local_addr()));
        assert!(snapshot.certificate_der.is_some());
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].peer, remote_overlay);
        assert_eq!(snapshot.sessions[0].mtu, 1_200);
        assert_eq!(snapshot.sessions[0].role, PacketPlaneSessionRole::Initiator);
        assert_eq!(
            current_packet_plane_quic_snapshot(None),
            PacketPlaneQuicRuntime::disabled_snapshot()
        );
    }

    #[test]
    fn shutdown_reason_has_stable_log_values() {
        assert_eq!(ShutdownReason::Interrupt.as_str(), "interrupt");
        assert_eq!(ShutdownReason::Terminate.as_str(), "terminate");
        assert_eq!(ShutdownReason::ControlSocket.as_str(), "control_socket");
    }

    #[test]
    fn runtime_control_shutdown_acknowledges_and_requests_stop() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = config_with_peer(&local_identity, remote);
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let paths = PathSet::new();
        let peer_capabilities = PeerCapabilities::default();
        let metrics = RuntimeMetrics::default();
        let local_capabilities = ControlCapabilities::local("lab", None, 1280);
        let (respond_to, mut response) = tokio::sync::oneshot::channel();

        let reason = handle_runtime_control_request(
            RuntimeControlRequest::Shutdown { respond_to },
            &RuntimeControlContext {
                forwarder: &forwarder,
                paths: &paths,
                peer_capabilities: &peer_capabilities,
                local_capabilities: &local_capabilities,
                metrics: &metrics,
                queue: crate::queue::QueueStats::default(),
                path_stats: crate::path::PathRuntimeStats::default(),
                packet_in_flight: PacketInFlightStats::default(),
                auto_relay: AutoRelaySnapshot::default(),
                relay_infrastructure: RelayInfrastructureSnapshot::default(),
                packet_plane: PacketPlaneSnapshot::default(),
                packet_plane_quic: PacketPlaneQuicSnapshot::default(),
                packet_plane_session_ttl: Duration::from_secs(42),
                packet_plane_replay_windows_per_session: 512,
            },
        );

        assert_eq!(reason, Some(ShutdownReason::ControlSocket));
        assert_eq!(
            response.try_recv().expect("shutdown response"),
            vec!["shutdown accepted".to_owned()]
        );
    }

    #[test]
    fn runtime_control_view_lines_report_peers_routes_paths_and_capabilities() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut config = config_with_peer(&local_identity, remote);
        config.network.routes = vec![RouteConfig {
            prefix: "10.10.0.0/24".to_owned(),
            metric: 90,
        }];
        config.peers[0].routes = vec![RouteConfig {
            prefix: "10.20.0.0/24".to_owned(),
            metric: 70,
        }];
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let local_capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_advertised_routes(forwarder.local_advertised_routes());
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1200)
                .with_advertised_routes(vec![ControlRoute::new("10.20.0.0/24", 70)]),
        );
        let mut paths = PathSet::new();
        paths.record_established_with_mtu(remote_overlay, PathKind::DirectTcpStream, Some(1180));
        paths.record_established_with_mtu(remote_overlay, PathKind::CircuitRelay, Some(1000));

        let peer_lines = runtime_peer_lines(&forwarder, &paths, &peer_capabilities);
        assert!(peer_lines.contains(&"peers: 1".to_owned()));
        assert!(peer_lines.iter().any(|line| {
            line == &format!(
                "peer: {remote_overlay} transport {remote} validated true effective_mtu 1200 quic_datagrams false native_quic_datagrams false owned_udp_packet_plane false owned_quic_packet_plane false healthy_paths 2 selected_path direct_tcp_stream"
            )
        }));

        let route_lines = runtime_route_lines(&forwarder, &peer_capabilities);
        assert!(route_lines.contains(&"local advertised routes: 3".to_owned()));
        assert!(route_lines.contains(&"remote advertised routes: 1".to_owned()));
        assert!(route_lines.contains(&format!(
            "peer advertised route: {remote_overlay} 10.20.0.0/24 metric 70"
        )));

        let path_lines = runtime_path_lines(&forwarder, &paths, &peer_capabilities);
        assert!(path_lines.contains(&format!(
            "peer selected path: {remote_overlay} direct_tcp_stream score 60 mtu 1180"
        )));
        assert!(path_lines.contains(&format!(
            "peer path: {remote_overlay} circuit_relay healthy true relay true direct false established_connections 1 score 30 estimated_mtu 1000 effective_mtu 1000 observed_rtt_ms unknown"
        )));

        let mtu_lines = runtime_mtu_lines(&forwarder, &paths, &peer_capabilities);
        assert!(mtu_lines.contains(&"local effective packet mtu: 1280".to_owned()));
        assert!(mtu_lines.contains(&"peers: 1".to_owned()));
        assert!(mtu_lines.contains(&format!(
            "peer mtu: {remote_overlay} validated true effective_mtu 1200 selected_path direct_tcp_stream selected_path_mtu 1180"
        )));
        assert!(mtu_lines.contains(&format!(
            "peer path mtu: {remote_overlay} direct_tcp_stream healthy true estimated_mtu 1180 effective_mtu 1180"
        )));
        assert!(mtu_lines.contains(&format!(
            "peer path mtu: {remote_overlay} circuit_relay healthy true estimated_mtu 1000 effective_mtu 1000"
        )));

        let packet_plane = PacketPlaneSnapshot {
            listeners: vec!["127.0.0.1:51820".parse().expect("listener")],
            sessions: Vec::new(),
        };
        let capability_lines = runtime_capability_lines(
            &forwarder,
            &peer_capabilities,
            &local_capabilities,
            &packet_plane,
        );
        assert!(capability_lines.contains(&"local capability advertised routes: 3".to_owned()));
        assert!(capability_lines.contains(&"validated peers: 1".to_owned()));
        assert!(capability_lines.contains(&format!(
            "remote capability preferred path: {remote_overlay} direct_quic_stream"
        )));
    }

    #[test]
    fn runtime_mtu_lines_report_overlay_fragmentation_policy() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let forwarder = Forwarder::from_config(&config_with_peer(&local_identity, peer_id()))
            .expect("forwarder");

        let mtu_lines =
            runtime_mtu_lines(&forwarder, &PathSet::new(), &PeerCapabilities::default());

        assert!(mtu_lines.contains(&OVERLAY_FRAGMENTATION_POLICY_LINE.to_owned()));
    }

    #[test]
    fn local_capability_lines_report_packet_plane_listeners() {
        let local_capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_owned_quic_packet_plane_certificate(vec![0x30, 0x01, 0x02])
            .with_owned_quic_packet_endpoint_candidates(vec!["127.0.0.1:51822".to_owned()]);
        let packet_plane = PacketPlaneSnapshot {
            listeners: vec!["127.0.0.1:51820".parse().expect("listener")],
            sessions: vec![PacketPlaneSessionSnapshot {
                peer: PeerId::from_bytes([7; 32]),
                endpoint: "127.0.0.1:51821".parse().expect("endpoint"),
                mtu: 1200,
                role: PacketPlaneSessionRole::Initiator,
                local_session_id: 11,
                remote_session_id: 13,
            }],
        };

        let lines = local_capability_lines(&local_capabilities, &packet_plane);

        assert!(lines.contains(&"packet plane listeners: 1".to_owned()));
        assert!(
            lines.contains(&"local capability supports owned quic packet plane: true".to_owned())
        );
        assert!(
            lines.contains(
                &"local capability owned quic packet plane certificate bytes: 3".to_owned()
            )
        );
        assert!(
            lines.contains(&"local capability owned quic packet endpoint candidates: 1".to_owned())
        );
        assert!(lines.contains(
            &"local capability owned quic packet endpoint candidate: 127.0.0.1:51822".to_owned()
        ));
        assert!(lines.contains(&"packet plane listener: 127.0.0.1:51820".to_owned()));
        assert!(lines.contains(&"packet plane sessions: 1".to_owned()));
        assert!(lines.contains(&format!(
            "packet plane session: {} endpoint 127.0.0.1:51821 mtu 1200 role initiator local_session 11 remote_session 13",
            PeerId::from_bytes([7; 32])
        )));
    }

    #[test]
    fn runtime_status_lines_include_packet_plane_session_ttl() {
        let packet_plane = PacketPlaneSnapshot {
            listeners: vec!["127.0.0.1:51820".parse().expect("listener")],
            sessions: Vec::new(),
        };
        let packet_plane_quic = PacketPlaneQuicSnapshot {
            listener: Some("127.0.0.1:51821".parse().expect("quic listener")),
            certificate_der: Some(vec![0x30, 0x01]),
            sessions: vec![PacketPlaneSessionSnapshot {
                peer: PeerId::from_bytes([8; 32]),
                endpoint: "127.0.0.1:51822".parse().expect("quic endpoint"),
                mtu: 1180,
                role: PacketPlaneSessionRole::Initiator,
                local_session_id: 17,
                remote_session_id: 19,
            }],
        };
        let auto_relay = AutoRelaySnapshot {
            max_candidates: 12,
            max_reservations: 3,
            retry_interval_seconds: 45,
            private_reachability: true,
            candidates: 5,
            reservations: 2,
            pending_retries: 1,
        };
        let lines = runtime_status_lines(RuntimeStatusView {
            metrics: &RuntimeMetrics::default(),
            queue: crate::queue::QueueStats::default(),
            path_stats: crate::path::PathRuntimeStats::default(),
            auto_relay,
            packet_plane: &packet_plane,
            packet_plane_quic: &packet_plane_quic,
            packet_plane_session_ttl: Duration::from_secs(75),
            packet_plane_replay_windows_per_session: 512,
        });

        assert_auto_relay_lines(&lines, auto_relay);
        assert!(lines.contains(&"packet_plane_session_ttl_seconds 75".to_owned()));
        assert!(lines.contains(&"packet_plane_replay_windows_per_session 512".to_owned()));
        assert!(lines.contains(&"packet_plane_listeners 1".to_owned()));
        assert!(lines.contains(&"packet_plane_sessions 0".to_owned()));
        assert!(lines.contains(&"packet_plane_quic_listeners 1".to_owned()));
        assert!(lines.contains(&"packet_plane_quic_sessions 1".to_owned()));
        assert!(lines.contains(&"packet_plane_quic_certificate_bytes 2".to_owned()));
        assert!(lines.contains(&"packet_plane_quic_listener 127.0.0.1:51821".to_owned()));
        assert!(lines.contains(&format!(
            "packet_plane_quic_session {} endpoint 127.0.0.1:51822 mtu 1180 role initiator local_session 17 remote_session 19",
            PeerId::from_bytes([8; 32])
        )));
    }

    #[test]
    fn runtime_state_lines_include_peer_capabilities_paths_and_probes() {
        let fixture = runtime_state_lines_fixture();
        let lines = fixture.lines;

        assert_lines_contain(
            &lines,
            &[
                "daemon state: running",
                "configured peers: 1",
                "validated peers: 1",
                "replay_windows 0",
                "packet_plane_session_ttl_seconds 90",
                "packet_plane_replay_windows_per_session 256",
                "outbound_stream_fallback_packets 0",
                "outbound_quic_datagram_packets 0",
                "outbound_quic_datagram_unavailable_packets 0",
                "path_promotions_to_direct 0",
                "path_fallbacks_to_relay 0",
                "dcutr_successes 1",
                "dcutr_failures 1",
                "autonat_probes_scheduled 1",
                "autonat_status_unknown 0",
                "autonat_status_public 1",
                "autonat_status_private 0",
            ],
        );
        assert_auto_relay_lines(&lines, fixture.auto_relay);
        assert_lines_contain(
            &lines,
            &[
                "autonat_status_changes_to_public 1",
                "autonat_status_changes_to_private 0",
                "outbound_path_probes_sent 1",
                "outbound_path_probe_acks_sent 1",
                "outbound_path_mtu_probe_confirmations 1",
                "outbound_queue_blocked_no_supported_path_events 0",
                "outbound_queue_blocked_packet_window_events 0",
                "relay_infrastructure_peers 1",
                "packet_stream_fallback_in_flight 2",
                "packet_stream_fallback_in_flight_peers 1",
                "packet_stream_fallback_in_flight_shards 2",
                "packet_stream_fallback_limit_per_peer 256",
                "packet_plane_listeners 1",
                "packet_plane_listener 127.0.0.1:51820",
                "packet_plane_sessions 1",
            ],
        );
        assert!(lines.contains(&format!(
            "relay_infrastructure_peer {} address {} connected true",
            fixture.infrastructure, fixture.infrastructure_address
        )));
        assert!(lines.contains(&format!(
            "packet_plane_session {} endpoint 127.0.0.1:51821 mtu 1200 role responder local_session 13 remote_session 11",
            fixture.remote_overlay
        )));
        assert!(lines.iter().any(|line| {
            line == &format!(
                "peer state: {} transport {} validated true effective_mtu 1200 quic_datagrams false native_quic_datagrams false owned_udp_packet_plane false owned_quic_packet_plane false selected_path direct_tcp_stream selected_path_score 60 selected_path_mtu 1200 selected_path_rtt_ms unknown healthy_paths 1 direct_paths 1 relay_paths 0",
                fixture.remote_overlay, fixture.remote
            )
        }));
        assert!(lines.iter().any(|line| {
            line == &format!(
                "peer capability state: {} preferred_path direct_quic_stream advertised_routes 1",
                fixture.remote_overlay
            )
        }));
        assert!(lines.iter().any(|line| {
            line == &format!(
                "peer path state: {} direct_tcp_stream healthy true relay false established_connections 1 score 60 estimated_mtu unknown effective_mtu 1200 observed_rtt_ms unknown",
                fixture.remote_overlay
            )
        }));
    }

    #[test]
    fn runtime_log_line_is_key_value_structured_and_quoted() {
        assert_eq!(
            runtime_log_line(
                LogLevel::Warn,
                "connection_closed",
                &[("peer", "12D3KooWPeer"), ("error", "dial failed \"hard\"")],
            ),
            "level=warn event=connection_closed peer=12D3KooWPeer error=\"dial failed \\\"hard\\\"\""
        );
    }

    #[test]
    fn packet_rejection_audit_fields_include_safe_packet_metadata() {
        let peer = peer_id();
        let source = Ipv4Addr::new(100, 64, 1, 10);
        let destination = Ipv4Addr::new(100, 64, 2, 20);
        let frame = Frame::new(
            PayloadType::IpPacket,
            42,
            9,
            ipv4_packet(source, destination),
        )
        .expect("valid frame");
        let error = ForwardError::Route(RouteError::UnauthorizedSource {
            peer: PeerId::from_libp2p(peer),
            source: source.into(),
        });

        let fields = packet_rejection_audit_fields(peer, &frame, &error);

        assert!(fields.contains(&("peer", peer.to_string())));
        assert!(fields.contains(&("reason", "unauthorized_source".to_owned())));
        assert!(fields.contains(&("payload_type", "ip_packet".to_owned())));
        assert!(fields.contains(&("session_id", "42".to_owned())));
        assert!(fields.contains(&("sequence", "9".to_owned())));
        assert!(fields.contains(&("payload_len", "20".to_owned())));
        assert!(fields.contains(&("source", source.to_string())));
        assert!(fields.contains(&("destination", destination.to_string())));
    }

    #[test]
    fn audit_rejection_reason_names_are_stable() {
        assert_eq!(
            packet_response_rejection_name(PacketRejectionReason::Replay),
            "replay"
        );
        assert_eq!(
            packet_response_rejection_name(PacketRejectionReason::RateLimited),
            "rate_limited"
        );
        assert_eq!(
            control_rejection_name(ControlRejectionReason::UnauthorizedRouteAdvertisement),
            "unauthorized_route_advertisement"
        );
        assert_eq!(
            service_rejection_name(ServiceRejectionReason::MembershipMismatch),
            "membership_mismatch"
        );
        assert_eq!(payload_type_name(PayloadType::PathProbe), "path_probe");
    }

    #[test]
    fn peer_packet_rate_limiter_caps_each_peer_independently() {
        let peer_a = peer_id();
        let peer_b = peer_id();
        let mut limiters = PeerPacketRateLimiters::new(2);
        let now = Instant::now();

        assert!(limiters.allow(peer_a, now));
        assert!(limiters.allow(peer_a, now));
        assert!(!limiters.allow(peer_a, now));
        assert!(limiters.allow(peer_b, now));
    }

    #[test]
    fn peer_packet_rate_limiter_refills_over_time_and_can_forget_peers() {
        let peer = peer_id();
        let mut limiters = PeerPacketRateLimiters::new(1);
        let now = Instant::now();

        assert!(limiters.allow(peer, now));
        assert!(!limiters.allow(peer, now));
        assert!(limiters.allow(peer, now + Duration::from_secs(1)));

        assert!(!limiters.allow(peer, now + Duration::from_secs(1)));
        limiters.remove(peer);
        assert!(limiters.allow(peer, now + Duration::from_secs(1)));
    }

    #[test]
    fn queue_expiry_interval_is_bounded_by_ttl_and_redial_interval() {
        assert_eq!(
            queue_expiry_interval(Duration::from_millis(1)),
            MIN_QUEUE_EXPIRY_INTERVAL
        );
        assert_eq!(
            queue_expiry_interval(Duration::from_millis(250)),
            Duration::from_millis(250)
        );
        assert_eq!(
            queue_expiry_interval(Duration::from_secs(11)),
            REDIAL_INTERVAL
        );
    }

    #[test]
    fn expire_outbound_queue_records_expired_drops() {
        let now = std::time::Instant::now();
        let expired_at = now
            .checked_sub(Duration::from_millis(101))
            .expect("test instant should allow subtraction");
        let mut queues = PeerQueues::with_packet_ttl(4, 4096, Duration::from_millis(100));
        queues
            .enqueue(crate::queue::Packet::new_at(
                PeerId::from_bytes([1; 32]),
                1,
                vec![1],
                expired_at,
            ))
            .expect("expired packet");
        queues
            .enqueue(crate::queue::Packet::new_at(
                PeerId::from_bytes([2; 32]),
                2,
                vec![2],
                now,
            ))
            .expect("fresh packet");
        let metrics = RuntimeMetrics::default();

        expire_outbound_queue(&mut queues, &metrics);

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_dropped_packets, 1);
        assert_eq!(snapshot.outbound_drop_queue_expired_packets, 1);
        assert_eq!(snapshot.queue.queued_packets, 1);
        assert_eq!(snapshot.queue.expired_packets, 1);
    }

    #[tokio::test]
    async fn kademlia_refresh_records_lookup_advertisement_and_bootstrap_result() {
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: true,
            kademlia_provider_advertisement: true,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        };
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery,
        })
        .expect("node");
        let metrics = RuntimeMetrics::default();
        let key = node.kademlia_rendezvous_key.clone().expect("kademlia key");
        let keys = vec![key.clone()];
        let forwarder = Forwarder::from_config(&config_with_peer(&node.identity, peer_id()))
            .expect("forwarder");

        let mut auto_relay = AutoRelayState::default();
        auto_relay.record_reachability(AutoNatReachability::Private);

        refresh_kademlia_rendezvous(
            &mut node.swarm,
            &KademliaRefreshContext {
                advertise_key: &key,
                lookup_keys: &keys,
                membership_record_advertise_key: node.kademlia_membership_records_key.as_ref(),
                membership_record_lookup_keys: &[],
                network_name: &node.network_name,
                membership_tag: node.membership_tag.as_deref(),
                forwarder: &forwarder,
                identity: &node.identity,
                advertise_provider: true,
                auto_relay: &auto_relay,
                metrics: &metrics,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.kademlia_provider_lookups, 1);
        assert_eq!(snapshot.kademlia_provider_advertisements, 1);
        assert_eq!(snapshot.kademlia_provider_advertisement_failures, 0);
        assert_eq!(
            snapshot.auto_relay_discovery_queries,
            AUTO_RELAY_DISCOVERY_QUERY_FANOUT as u64
        );
        assert_eq!(snapshot.kademlia_bootstrap_refreshes, 0);
        assert_eq!(snapshot.kademlia_bootstrap_failures, 1);
    }

    #[tokio::test]
    async fn kademlia_refresh_can_lookup_without_provider_advertisement() {
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: true,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        };
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery,
        })
        .expect("node");
        let metrics = RuntimeMetrics::default();
        let key = node.kademlia_rendezvous_key.clone().expect("kademlia key");
        let keys = vec![key.clone()];
        let forwarder = Forwarder::from_config(&config_with_peer(&node.identity, peer_id()))
            .expect("forwarder");

        assert!(!node.startup.kademlia.rendezvous_advertise_started);
        assert!(node.startup.kademlia.rendezvous_lookup_started);

        let auto_relay = AutoRelayState::default();

        refresh_kademlia_rendezvous(
            &mut node.swarm,
            &KademliaRefreshContext {
                advertise_key: &key,
                lookup_keys: &keys,
                membership_record_advertise_key: node.kademlia_membership_records_key.as_ref(),
                membership_record_lookup_keys: &[],
                network_name: &node.network_name,
                membership_tag: node.membership_tag.as_deref(),
                forwarder: &forwarder,
                identity: &node.identity,
                advertise_provider: false,
                auto_relay: &auto_relay,
                metrics: &metrics,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.kademlia_provider_lookups, 1);
        assert_eq!(snapshot.kademlia_provider_advertisements, 0);
        assert_eq!(snapshot.kademlia_provider_advertisement_failures, 0);
    }

    #[tokio::test]
    async fn kademlia_refresh_queries_previous_membership_rendezvous_keys() {
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: true,
            kademlia_provider_advertisement: true,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        };
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: Some("current".to_owned()),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery,
        })
        .expect("node");
        let metrics = RuntimeMetrics::default();
        let current_key = node.kademlia_rendezvous_key.clone().expect("kademlia key");
        let keys = kademlia_lookup_keys(
            &node.network_name,
            Some(&current_key),
            &[
                "previous-a".to_owned(),
                "previous-b".to_owned(),
                "previous-a".to_owned(),
            ],
        );

        assert_eq!(
            keys,
            vec![
                current_key.clone(),
                crate::runtime::p2p::kademlia_rendezvous_key("lab", Some("previous-a")),
                crate::runtime::p2p::kademlia_rendezvous_key("lab", Some("previous-b")),
            ]
        );

        let auto_relay = AutoRelayState::default();
        let forwarder = Forwarder::from_config(&config_with_peer(&node.identity, peer_id()))
            .expect("forwarder");

        refresh_kademlia_rendezvous(
            &mut node.swarm,
            &KademliaRefreshContext {
                advertise_key: &current_key,
                lookup_keys: &keys,
                membership_record_advertise_key: node.kademlia_membership_records_key.as_ref(),
                membership_record_lookup_keys: &[],
                network_name: &node.network_name,
                membership_tag: node.membership_tag.as_deref(),
                forwarder: &forwarder,
                identity: &node.identity,
                advertise_provider: true,
                auto_relay: &auto_relay,
                metrics: &metrics,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.kademlia_provider_lookups, 3);
        assert_eq!(snapshot.kademlia_provider_advertisements, 1);
    }

    #[test]
    fn kademlia_membership_record_lookup_keys_include_previous_and_untagged_scope() {
        let current_key =
            crate::runtime::p2p::kademlia_membership_records_key("lab", Some("current"));

        let keys = kademlia_membership_record_lookup_keys(
            "lab",
            Some("current"),
            Some(&current_key),
            &[
                "previous-a".to_owned(),
                "previous-b".to_owned(),
                "previous-a".to_owned(),
            ],
        );

        assert_eq!(
            keys,
            vec![
                current_key,
                crate::runtime::p2p::kademlia_membership_records_key("lab", Some("previous-a")),
                crate::runtime::p2p::kademlia_membership_records_key("lab", Some("previous-b")),
                crate::runtime::p2p::kademlia_membership_records_key("lab", None),
            ]
        );
    }

    #[test]
    fn kademlia_membership_record_bundle_merges_verified_records() {
        let local_identity = NodeIdentity::generate_ed25519().expect("local identity");
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let mut config = config_with_peer(&local_identity, member_peer);
        config.peers.clear();
        config.network.member_records = vec![
            issue_membership_record_at(
                &issuer,
                MembershipRecordOptions {
                    network_name: "lab".to_owned(),
                    member: issuer.clone(),
                    membership_epoch: 1,
                    sequence: 1,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1_000,
            )
            .expect("issuer record"),
        ];
        let member_record = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_001,
        )
        .expect("member record");
        let value = encode_kademlia_membership_records("lab", Some("current"), vec![member_record])
            .expect("encoded bundle");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut membership = OverlayMembership::from_config(&config).expect("membership");
        let mut capabilities = ControlCapabilities::local("lab", Some("current".to_owned()), 1280);

        let accepted = learn_membership_records_from_kademlia_value(
            &mut forwarder,
            &mut membership,
            &mut capabilities,
            "lab",
            Some("current"),
            &[],
            &value,
        )
        .expect("trusted bundle");

        assert_eq!(accepted, 1);
        assert!(membership.allows(member_peer));
        assert_eq!(capabilities.member_records.len(), 2);
    }

    #[test]
    fn kademlia_membership_record_bundle_rejects_wrong_scope() {
        let local_identity = NodeIdentity::generate_ed25519().expect("local identity");
        let member = peer_id();
        let config = config_with_peer(&local_identity, member);
        let value =
            encode_kademlia_membership_records("lab", Some("other"), Vec::new()).expect("bundle");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut membership = OverlayMembership::from_config(&config).expect("membership");
        let mut capabilities = ControlCapabilities::local("lab", Some("current".to_owned()), 1280);

        let result = learn_membership_records_from_kademlia_value(
            &mut forwarder,
            &mut membership,
            &mut capabilities,
            "lab",
            Some("current"),
            &["previous".to_owned()],
            &value,
        );

        assert!(matches!(
            result,
            Err(KademliaMembershipRecordError::WrongMembershipScope)
        ));
    }

    #[tokio::test]
    async fn kademlia_provider_results_dial_configured_peers_and_count_failures() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let unconfigured = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: configured.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity {
                peer_id: local_identity.peer_id,
                private_key: local_identity.private_key,
            },
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let metrics = RuntimeMetrics::default();
        let providers = HashSet::from([configured, unconfigured]);

        dial_kademlia_providers(&mut node.swarm, &forwarder, &metrics, &providers);

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.kademlia_providers_found, 2);
        assert_eq!(snapshot.kademlia_providers_ignored, 1);
        assert_eq!(snapshot.kademlia_provider_dial_attempts, 1);
        assert_eq!(snapshot.kademlia_provider_dial_failures, 1);
    }

    #[tokio::test]
    async fn kademlia_closest_peer_results_learn_configured_peer_addresses() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let unconfigured = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut infrastructure_peers = InfrastructurePeers::default();
        let mut auto_relay = AutoRelayState::default();
        let mut discovered = DiscoveredPeerAddresses::default();
        let paths = PathSet::new();
        let metrics = RuntimeMetrics::default();
        let configured_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let unconfigured_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");
        let result = kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk {
            key: configured.to_bytes(),
            peers: vec![
                kad::PeerInfo {
                    peer_id: configured,
                    addrs: vec![configured_address.clone()],
                },
                kad::PeerInfo {
                    peer_id: unconfigured,
                    addrs: vec![unconfigured_address],
                },
            ],
        }));

        handle_kademlia_closest_peer_result(
            &mut node.swarm,
            &forwarder,
            &mut infrastructure_peers,
            &mut auto_relay,
            &mut discovered,
            &paths,
            &metrics,
            &DiscoveryConfig::default(),
            &result,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 1);
        assert_eq!(snapshot.discovered_address_dial_attempts, 1);
        assert_eq!(snapshot.discovered_address_dial_failures, 0);
        assert_eq!(snapshot.discovered_addresses_rejected, 0);
        assert_eq!(discovered.as_vec(), vec![(configured, configured_address)]);
    }

    #[tokio::test]
    async fn kademlia_closest_peer_results_accept_unauthenticated_relayed_dial_hints() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let relay = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut infrastructure_peers = InfrastructurePeers::default();
        let mut auto_relay = AutoRelayState::default();
        let mut discovered = DiscoveredPeerAddresses::default();
        let paths = PathSet::new();
        let metrics = RuntimeMetrics::default();
        let relayed_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{configured}")
                .parse()
                .expect("relayed address");
        let result = kad::QueryResult::GetClosestPeers(Ok(kad::GetClosestPeersOk {
            key: configured.to_bytes(),
            peers: vec![kad::PeerInfo {
                peer_id: configured,
                addrs: vec![relayed_address.clone()],
            }],
        }));

        handle_kademlia_closest_peer_result(
            &mut node.swarm,
            &forwarder,
            &mut infrastructure_peers,
            &mut auto_relay,
            &mut discovered,
            &paths,
            &metrics,
            &DiscoveryConfig::default(),
            &result,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 1);
        assert_eq!(snapshot.discovered_address_dial_attempts, 1);
        assert_eq!(snapshot.discovered_addresses_rejected, 0);
        assert_eq!(discovered.as_vec(), vec![(configured, relayed_address)]);
    }

    #[test]
    fn kademlia_peer_address_records_accept_signed_configured_relay_addresses() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_peer = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote peer id");
        let relay = peer_id();
        let config = config_with_peer(&local_identity, remote_peer);
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let relayed_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit")
            .parse()
            .expect("relayed address");
        let value = encode_kademlia_peer_address_record(
            "lab",
            None,
            &remote_identity,
            vec![relayed_address.clone()],
            current_unix_seconds_lossy(),
        )
        .expect("address record");

        let (peer, addresses) =
            learn_peer_addresses_from_kademlia_value(&forwarder, "lab", None, &[], &value)
                .expect("trusted address record");

        assert_eq!(peer, remote_peer);
        assert_eq!(addresses, vec![relayed_address]);
    }

    #[test]
    fn kademlia_peer_address_records_accept_recently_stale_signed_addresses() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_peer = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote peer id");
        let relay = peer_id();
        let config = config_with_peer(&local_identity, remote_peer);
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let relayed_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit")
            .parse()
            .expect("relayed address");
        let current = current_unix_seconds_lossy();
        let recently_stale = current
            .saturating_sub(KADEMLIA_PEER_ADDRESS_RECORD_TTL)
            .saturating_sub(KADEMLIA_PEER_ADDRESS_RECORD_STALE_GRACE / 2);
        let too_stale = current
            .saturating_sub(KADEMLIA_PEER_ADDRESS_RECORD_TTL)
            .saturating_sub(KADEMLIA_PEER_ADDRESS_RECORD_STALE_GRACE)
            .saturating_sub(1);

        let recently_stale_value = encode_kademlia_peer_address_record(
            "lab",
            None,
            &remote_identity,
            vec![relayed_address.clone()],
            recently_stale,
        )
        .expect("recently stale address record");
        let (peer, addresses) = learn_peer_addresses_from_kademlia_value(
            &forwarder,
            "lab",
            None,
            &[],
            &recently_stale_value,
        )
        .expect("recently stale signed address record");
        assert_eq!(peer, remote_peer);
        assert_eq!(addresses, vec![relayed_address.clone()]);

        let too_stale_value = encode_kademlia_peer_address_record(
            "lab",
            None,
            &remote_identity,
            vec![relayed_address],
            too_stale,
        )
        .expect("too stale address record");
        let result = learn_peer_addresses_from_kademlia_value(
            &forwarder,
            "lab",
            None,
            &[],
            &too_stale_value,
        );
        assert!(matches!(
            result,
            Err(KademliaPeerAddressRecordError::Expired)
        ));
    }

    #[tokio::test]
    async fn authenticated_peer_records_can_learn_relayed_peer_addresses() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let relay = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut discovered = DiscoveredPeerAddresses::default();
        let paths = PathSet::new();
        let metrics = RuntimeMetrics::default();
        let relayed_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{configured}")
                .parse()
                .expect("relayed address");

        learn_peer_address(
            &mut node.swarm,
            &forwarder,
            &mut discovered,
            &paths,
            &metrics,
            configured,
            relayed_address.clone(),
            &DiscoveryConfig::default(),
            DiscoveredPeerAddressSource::AuthenticatedPeerRecord,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 1);
        assert_eq!(snapshot.discovered_address_dial_attempts, 1);
        assert_eq!(snapshot.discovered_addresses_rejected, 0);
        assert_eq!(discovered.as_vec(), vec![(configured, relayed_address)]);
    }

    #[tokio::test]
    async fn authenticated_peer_records_reject_unsupported_relayed_peer_addresses() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let relay = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut discovered = DiscoveredPeerAddresses::default();
        let paths = PathSet::new();
        let metrics = RuntimeMetrics::default();
        let relayed_address: Multiaddr = format!(
            "/ip4/127.0.0.1/udp/4001/quic-v1/webtransport/p2p/{relay}/p2p-circuit/p2p/{configured}"
        )
        .parse()
        .expect("relayed address");

        learn_peer_address(
            &mut node.swarm,
            &forwarder,
            &mut discovered,
            &paths,
            &metrics,
            configured,
            relayed_address,
            &DiscoveryConfig::default(),
            DiscoveredPeerAddressSource::AuthenticatedPeerRecord,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 0);
        assert_eq!(snapshot.discovered_address_dial_attempts, 0);
        assert_eq!(snapshot.discovered_addresses_rejected, 1);
        assert!(discovered.as_vec().is_empty());
    }

    #[test]
    fn kademlia_peer_address_records_reject_unconfigured_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let unconfigured_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let config = config_with_peer(&local_identity, configured);
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let value = encode_kademlia_peer_address_record(
            "lab",
            None,
            &unconfigured_identity,
            vec![address],
            current_unix_seconds_lossy(),
        )
        .expect("address record");

        let result = learn_peer_addresses_from_kademlia_value(&forwarder, "lab", None, &[], &value);

        assert!(matches!(
            result,
            Err(KademliaPeerAddressRecordError::WrongPeer)
        ));
    }

    #[test]
    fn kademlia_peer_address_publication_filters_local_only_direct_addresses() {
        let relay = peer_id();
        let rejected = [
            "/ip4/127.0.0.1/tcp/4001",
            "/ip4/10.42.0.1/tcp/4001",
            "/ip4/100.64.9.171/tcp/4001",
            "/ip6/fd00:6879:7072:7370:6163:6500:4b5b:8ec1/tcp/4001",
            "/ip6/fe80::1/tcp/4001",
            &format!("/ip4/127.0.0.1/udp/4001/quic-v1/webtransport/p2p/{relay}/p2p-circuit"),
            &format!("/ip4/127.0.0.1/udp/4001/webrtc-direct/p2p/{relay}/p2p-circuit"),
            &format!("/dns4/relay.example.net/tcp/4001/tls/ws/p2p/{relay}/p2p-circuit"),
        ];
        for address in rejected {
            let address: Multiaddr = address.parse().expect("address");
            assert!(!kademlia_peer_address_is_advertisable(&address));
        }

        let accepted = [
            "/ip4/8.8.8.8/tcp/4001",
            "/ip4/192.168.0.10/tcp/4001",
            "/ip4/172.17.0.1/tcp/4001",
            "/ip6/2606:4700:4700::1111/tcp/4001",
            "/dns4/relay.example.net/tcp/4001",
            &format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit"),
        ];
        for address in accepted {
            let address: Multiaddr = address.parse().expect("address");
            assert!(kademlia_peer_address_is_advertisable(&address));
        }
    }

    #[test]
    fn relayed_peer_dial_transport_matches_runtime_supported_transports() {
        let relay = peer_id();
        let accepted = [
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit"),
            format!("/ip4/127.0.0.1/udp/4001/quic-v1/p2p/{relay}/p2p-circuit"),
        ];
        for address in accepted {
            let address: Multiaddr = address.parse().expect("address");
            assert!(supports_relayed_peer_dial_transport(&address));
        }

        let rejected = [
            format!("/ip4/127.0.0.1/udp/4001/quic-v1/webtransport/p2p/{relay}/p2p-circuit"),
            format!("/ip4/127.0.0.1/udp/4001/webrtc-direct/p2p/{relay}/p2p-circuit"),
            format!("/dns4/relay.example.net/tcp/4001/tls/ws/p2p/{relay}/p2p-circuit"),
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}"),
        ];
        for address in rejected {
            let address: Multiaddr = address.parse().expect("address");
            assert!(!supports_relayed_peer_dial_transport(&address));
        }
    }

    #[test]
    fn kademlia_peer_address_publication_requires_confirmed_direct_or_relay_address() {
        let relay = peer_id();
        let relayed: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit")
            .parse()
            .expect("relayed address");
        let direct: Multiaddr = "/ip4/192.168.0.10/tcp/4001"
            .parse()
            .expect("direct address");
        let empty = HashSet::new();
        let confirmed = HashSet::from([direct.clone()]);

        assert!(!kademlia_peer_address_is_confirmed_for_publication(
            &direct, &empty
        ));
        assert!(kademlia_peer_address_is_confirmed_for_publication(
            &relayed, &empty
        ));
        assert!(kademlia_peer_address_is_confirmed_for_publication(
            &direct, &confirmed
        ));
    }

    #[test]
    fn redial_targets_skip_self_and_connected_peers() {
        let local = peer_id();
        let connected = peer_id();
        let disconnected = peer_id();
        let bootstrap_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let peer_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");
        let local_address: Multiaddr = "/ip4/127.0.0.1/tcp/4003".parse().expect("address");

        let targets = pending_redial_targets(
            local,
            &[(connected, bootstrap_address)],
            &[],
            &[(disconnected, peer_address.clone()), (local, local_address)],
            &[],
            |peer| {
                if *peer == connected {
                    RedialConnectionState::DirectOnly
                } else {
                    RedialConnectionState::Disconnected
                }
            },
            |_| true,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![(disconnected, peer_address)],
                skipped_connected: 1,
            }
        );
    }

    #[test]
    fn redial_targets_include_bootstrap_and_configured_peer_addresses() {
        let local = peer_id();
        let bootstrap = peer_id();
        let relay = peer_id();
        let configured = peer_id();
        let bootstrap_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let relay_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");
        let peer_address: Multiaddr = "/ip4/127.0.0.1/tcp/4003".parse().expect("address");

        let targets = pending_redial_targets(
            local,
            &[(bootstrap, bootstrap_address.clone())],
            &[(relay, relay_address.clone())],
            &[(configured, peer_address.clone())],
            &[],
            |_| RedialConnectionState::Disconnected,
            |_| true,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![
                    (bootstrap, bootstrap_address),
                    (relay, relay_address),
                    (configured, peer_address),
                ],
                skipped_connected: 0,
            }
        );
    }

    #[test]
    fn redial_targets_include_discovered_peer_addresses() {
        let local = peer_id();
        let configured = peer_id();
        let discovered = peer_id();
        let configured_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let discovered_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");

        let targets = pending_redial_targets(
            local,
            &[],
            &[],
            &[(configured, configured_address.clone())],
            &[(discovered, discovered_address.clone())],
            |_| RedialConnectionState::Disconnected,
            |_| true,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![
                    (configured, configured_address),
                    (discovered, discovered_address),
                ],
                skipped_connected: 0,
            }
        );
    }

    #[test]
    fn redial_targets_deduplicate_discovered_addresses() {
        let local = peer_id();
        let peer = peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        let targets = pending_redial_targets(
            local,
            &[(peer, address.clone())],
            &[(peer, address.clone())],
            &[],
            &[(peer, address.clone())],
            |_| RedialConnectionState::Disconnected,
            |_| true,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![(peer, address)],
                skipped_connected: 0,
            }
        );
    }

    #[test]
    fn redial_targets_wait_for_configured_relay_reservation_readiness() {
        let local = peer_id();
        let relay = peer_id();
        let peer = peer_id();
        let relay_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let relayed_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");

        let targets = pending_redial_targets(
            local,
            &[],
            &[(relay, relay_address.clone())],
            &[(peer, relayed_address.clone())],
            &[],
            |_| RedialConnectionState::Disconnected,
            |_| false,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![(relay, relay_address.clone())],
                skipped_connected: 0,
            }
        );

        let targets = pending_redial_targets(
            local,
            &[],
            &[(relay, relay_address.clone())],
            &[(peer, relayed_address.clone())],
            &[],
            |_| RedialConnectionState::Disconnected,
            |_| true,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![(relay, relay_address), (peer, relayed_address)],
                skipped_connected: 0,
            }
        );
    }

    #[test]
    fn redial_targets_keep_direct_overlay_addresses_for_relay_only_peers() {
        let local = peer_id();
        let relay = peer_id();
        let peer = peer_id();
        let direct_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("direct address");
        let relayed_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");

        let targets = pending_redial_targets(
            local,
            &[],
            &[],
            &[(peer, relayed_address)],
            &[(peer, direct_address.clone())],
            |_| RedialConnectionState::RelayOnly,
            |_| true,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![(peer, direct_address)],
                skipped_connected: 1,
            }
        );
    }

    #[test]
    fn redial_targets_prewarm_configured_relay_for_direct_only_peers() {
        let local = peer_id();
        let relay = peer_id();
        let peer = peer_id();
        let relay_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let direct_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4002/p2p/{peer}")
            .parse()
            .expect("direct address");
        let relayed_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");

        let targets = pending_redial_targets(
            local,
            &[],
            &[(relay, relay_address)],
            &[(peer, direct_address), (peer, relayed_address.clone())],
            &[],
            |candidate| {
                if *candidate == peer {
                    RedialConnectionState::DirectOnly
                } else {
                    RedialConnectionState::RelayOnly
                }
            },
            |_| true,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![(peer, relayed_address)],
                skipped_connected: 2,
            }
        );
    }

    #[test]
    fn redial_targets_skip_configured_relay_after_relay_path_exists() {
        let local = peer_id();
        let relay = peer_id();
        let peer = peer_id();
        let relay_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let relayed_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");

        let targets = pending_redial_targets(
            local,
            &[],
            &[(relay, relay_address)],
            &[(peer, relayed_address)],
            &[],
            |candidate| {
                if *candidate == peer {
                    RedialConnectionState::DirectAndRelay
                } else {
                    RedialConnectionState::RelayOnly
                }
            },
            |_| true,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: Vec::new(),
                skipped_connected: 2,
            }
        );
    }

    #[test]
    fn configured_relay_reservation_retries_wait_for_redial_interval() {
        let relay = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit")
            .parse()
            .expect("relay reservation");
        let now = Instant::now();
        let mut retries =
            ConfiguredRelayReservationRetries::from_startup_attempts(&[address.clone()], now);

        assert!(!retries.should_retry(&address, now));
        assert!(!retries.should_retry(&address, now + REDIAL_INTERVAL - Duration::from_millis(1)));
        assert!(retries.should_retry(&address, now + REDIAL_INTERVAL));
        assert!(!retries.should_retry(&address, now + REDIAL_INTERVAL));
    }

    #[test]
    fn redial_connection_state_reports_relay_only_until_direct_path_exists() {
        let peer = peer_id();
        let overlay = PeerId::from_libp2p(peer);
        let mut paths = PathSet::new();

        assert_eq!(
            redial_connection_state(&paths, peer, false),
            RedialConnectionState::Disconnected
        );
        assert_eq!(
            redial_connection_state(&paths, peer, true),
            RedialConnectionState::RelayOnly
        );

        paths.record_established(overlay, PathKind::CircuitRelay);
        assert_eq!(
            redial_connection_state(&paths, peer, true),
            RedialConnectionState::RelayOnly
        );

        paths.record_closed(overlay, PathKind::CircuitRelay);
        assert_eq!(
            redial_connection_state(&paths, peer, true),
            RedialConnectionState::ConnectedNoUsablePath
        );

        paths.record_established(overlay, PathKind::DirectTcpStream);
        assert_eq!(
            redial_connection_state(&paths, peer, true),
            RedialConnectionState::DirectOnly
        );

        paths.record_established(overlay, PathKind::CircuitRelay);
        assert_eq!(
            redial_connection_state(&paths, peer, true),
            RedialConnectionState::DirectAndRelay
        );
    }

    #[test]
    fn discovered_relay_addresses_are_dialed_when_connected_path_is_stale() {
        let peer = peer_id();
        let relay = peer_id();
        let overlay = PeerId::from_libp2p(peer);
        let relay_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relay address");
        let direct_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("direct address");
        let mut paths = PathSet::new();

        assert!(should_dial_discovered_address(
            &paths,
            peer,
            true,
            &relay_address
        ));

        paths.record_established(overlay, PathKind::DirectTcpStream);
        paths.mark_unhealthy(overlay, PathKind::DirectTcpStream);
        assert!(should_dial_discovered_address(
            &paths,
            peer,
            true,
            &relay_address
        ));
        assert!(should_dial_discovered_address(
            &paths,
            peer,
            true,
            &direct_address
        ));

        paths.record_established(overlay, PathKind::CircuitRelay);
        assert!(!should_dial_discovered_address(
            &paths,
            peer,
            true,
            &relay_address
        ));

        paths.record_closed(overlay, PathKind::CircuitRelay);
        assert!(should_dial_discovered_address(
            &paths,
            peer,
            true,
            &relay_address
        ));

        paths.record_established(overlay, PathKind::DirectTcpStream);
        assert!(!should_dial_discovered_address(
            &paths,
            peer,
            true,
            &direct_address
        ));
    }

    #[test]
    fn blocked_queue_redial_is_rate_limited() {
        let now = Instant::now();
        let mut last_redial = None;

        assert!(should_redial_blocked_queue(&mut last_redial, now));
        assert!(!should_redial_blocked_queue(
            &mut last_redial,
            now + BLOCKED_QUEUE_REDIAL_INTERVAL - Duration::from_millis(1)
        ));
        assert!(should_redial_blocked_queue(
            &mut last_redial,
            now + BLOCKED_QUEUE_REDIAL_INTERVAL
        ));
    }

    #[test]
    fn relayed_address_relay_peer_extracts_relay_before_circuit() {
        let relay = peer_id();
        let peer = peer_id();
        let direct: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("direct address");
        let relayed: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");

        assert_eq!(relayed_address_relay_peer(&direct), None);
        assert_eq!(relayed_address_relay_peer(&relayed), Some(relay));
    }

    #[test]
    fn relay_ready_configured_peer_targets_select_matching_relayed_peers() {
        let local = peer_id();
        let relay = peer_id();
        let other_relay = peer_id();
        let peer = peer_id();
        let connected = peer_id();
        let direct_peer = peer_id();
        let relayed: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");
        let connected_relayed: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{connected}")
                .parse()
                .expect("connected relayed address");
        let other_relayed: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4002/p2p/{other_relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("other relayed address");
        let direct: Multiaddr = format!("/ip4/127.0.0.1/tcp/4003/p2p/{direct_peer}")
            .parse()
            .expect("direct address");

        let targets = relay_ready_configured_peer_targets(
            local,
            relay,
            &[
                (peer, relayed.clone()),
                (peer, relayed.clone()),
                (connected, connected_relayed),
                (peer, other_relayed),
                (direct_peer, direct),
            ],
            &DiscoveredPeerAddresses::default(),
            |candidate| *candidate == connected,
        );

        assert_eq!(targets, vec![(peer, relayed)]);
    }

    #[test]
    fn relay_ready_peer_targets_include_discovered_relayed_addresses() {
        let local = peer_id();
        let relay = peer_id();
        let peer = peer_id();
        let discovered_relayed: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");
        let discovered_direct: Multiaddr = format!("/ip4/127.0.0.1/tcp/4003/p2p/{peer}")
            .parse()
            .expect("direct address");
        let mut discovered = DiscoveredPeerAddresses::default();
        discovered.insert(peer, discovered_direct);
        discovered.insert(peer, discovered_relayed.clone());

        let targets =
            relay_ready_configured_peer_targets(local, relay, &[], &discovered, |_| false);

        assert_eq!(targets, vec![(peer, discovered_relayed)]);
    }

    #[test]
    fn relay_ready_peer_targets_deduplicate_configured_and_discovered_addresses() {
        let local = peer_id();
        let relay = peer_id();
        let peer = peer_id();
        let relayed: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");
        let mut discovered = DiscoveredPeerAddresses::default();
        discovered.insert(peer, relayed.clone());

        let targets = relay_ready_configured_peer_targets(
            local,
            relay,
            &[(peer, relayed.clone())],
            &discovered,
            |_| false,
        );

        assert_eq!(targets, vec![(peer, relayed)]);
    }

    #[test]
    fn relay_readiness_requires_reservation_and_relayed_listen_address() {
        let relay = peer_id();
        let peer = peer_id();
        let address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");
        let mut readiness = RelayReadiness::default();

        readiness.record_reservation_accepted(relay);

        assert!(!readiness.relay_ready(relay));

        readiness.record_relay_listen_address(relay);

        assert!(readiness.relay_ready(relay));
        assert!(readiness.should_attempt_ready_dial(relay, peer, &address));
        assert!(!readiness.should_attempt_ready_dial(relay, peer, &address));
    }

    #[test]
    fn relay_readiness_clears_on_lost_listen_address_and_reservation() {
        let relay = peer_id();
        let peer = peer_id();
        let address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");
        let mut readiness = RelayReadiness::default();

        readiness.record_reservation_accepted(relay);
        readiness.record_relay_listen_address(relay);
        assert!(readiness.relay_ready(relay));
        assert!(readiness.should_attempt_ready_dial(relay, peer, &address));

        assert!(readiness.record_relay_listen_address_lost(relay));
        assert!(!readiness.relay_ready(relay));
        assert!(!readiness.record_relay_listen_address_lost(relay));

        readiness.record_relay_listen_address(relay);
        assert!(readiness.relay_ready(relay));
        assert!(!readiness.should_attempt_ready_dial(relay, peer, &address));

        assert!(readiness.record_relay_reservation_lost(relay));
        assert!(!readiness.relay_ready(relay));
        assert!(!readiness.record_relay_reservation_lost(relay));

        readiness.record_reservation_accepted(relay);
        readiness.record_relay_listen_address(relay);
        assert!(readiness.should_attempt_ready_dial(relay, peer, &address));
    }

    #[test]
    fn auto_relay_candidate_address_accepts_supported_direct_transports() {
        let peer = peer_id();
        let tcp: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("tcp address");
        let quic_without_peer: Multiaddr = "/ip4/127.0.0.1/udp/4001/quic-v1"
            .parse()
            .expect("quic address");

        assert_eq!(auto_relay_candidate_address(peer, &tcp), Some(tcp));
        assert_eq!(
            auto_relay_candidate_address(peer, &quic_without_peer),
            Some(peer_dial_address(peer, quic_without_peer))
        );
    }

    #[test]
    fn auto_relay_candidate_address_rejects_unsupported_or_relayed_addresses() {
        let peer = peer_id();
        let other = peer_id();
        let wrong_peer: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{other}")
            .parse()
            .expect("wrong peer address");
        let websocket: Multiaddr = format!("/dns4/relay.example.test/tcp/443/wss/p2p/{peer}")
            .parse()
            .expect("websocket address");
        let relayed: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{other}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("relayed address");

        assert_eq!(auto_relay_candidate_address(peer, &wrong_peer), None);
        assert_eq!(auto_relay_candidate_address(peer, &websocket), None);
        assert_eq!(auto_relay_candidate_address(peer, &relayed), None);
    }

    #[test]
    fn auto_relay_state_attempts_candidates_when_reachability_is_unknown_or_private() {
        let relay = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let mut state = AutoRelayState::default();
        let now = Instant::now();

        assert!(state.record_candidate(relay, address.clone()));
        assert!(!state.record_candidate(relay, address.clone()));
        assert_eq!(
            state.next_reservation_targets(now),
            vec![(relay, address.clone())]
        );
        assert!(state.next_reservation_targets(now).is_empty());

        assert!(state.release_reservation_for_retry(relay));
        state.record_reachability(AutoNatReachability::Private);

        assert_eq!(state.next_reservation_targets(now), vec![(relay, address)]);
        assert!(state.next_reservation_targets(now).is_empty());
    }

    #[test]
    fn auto_relay_state_skips_reservations_when_public() {
        let relay = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let mut state = AutoRelayState::default();
        state.record_reachability(AutoNatReachability::Public);

        assert!(state.record_candidate(relay, address));
        assert!(state.next_reservation_targets(Instant::now()).is_empty());
    }

    #[test]
    fn auto_relay_state_discovers_candidates_while_reachability_is_unknown() {
        let mut state = AutoRelayState::default();

        assert!(state.should_discover_candidates());

        state.record_reachability(AutoNatReachability::Public);
        assert!(!state.should_discover_candidates());
    }

    #[test]
    fn auto_relay_state_caps_reservation_targets() {
        let max_reservations = 3;
        let mut state = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 8,
            max_reservations,
            retry_interval_seconds: 5,
        });
        state.record_reachability(AutoNatReachability::Private);
        let candidates = (0..max_reservations + 2)
            .map(|port| {
                let relay = peer_id();
                let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/{}/p2p/{relay}", 4001 + port)
                    .parse()
                    .expect("relay address");
                (relay, address)
            })
            .collect::<Vec<_>>();
        for (relay, address) in &candidates {
            assert!(state.record_candidate(*relay, address.clone()));
        }

        let targets = state.next_reservation_targets(Instant::now());

        assert_eq!(targets.len(), max_reservations);
    }

    #[test]
    fn auto_relay_state_respects_configured_candidate_cap() {
        let mut state = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 1,
            max_reservations: 1,
            retry_interval_seconds: 5,
        });
        let relay_a = peer_id();
        let relay_b = peer_id();
        let address_a: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay_a}")
            .parse()
            .expect("relay address");
        let address_b: Multiaddr = format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay_b}")
            .parse()
            .expect("relay address");

        assert!(state.record_candidate(relay_a, address_a));
        assert!(!state.record_candidate(relay_b, address_b));
    }

    #[test]
    fn auto_relay_state_keeps_one_candidate_address_per_relay_peer() {
        let relay_a = peer_id();
        let relay_b = peer_id();
        let address_a_tcp: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay_a}")
            .parse()
            .expect("relay address");
        let address_a_quic: Multiaddr = format!("/ip4/127.0.0.1/udp/4001/quic-v1/p2p/{relay_a}")
            .parse()
            .expect("relay address");
        let address_b: Multiaddr = format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay_b}")
            .parse()
            .expect("relay address");
        let mut state = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 2,
            max_reservations: 2,
            retry_interval_seconds: 5,
        });

        assert!(state.record_candidate(relay_a, address_a_tcp.clone()));
        assert!(!state.record_candidate(relay_a, address_a_quic));
        assert!(state.record_candidate(relay_b, address_b.clone()));

        assert_eq!(
            state.next_reservation_targets(Instant::now()),
            vec![(relay_a, address_a_tcp), (relay_b, address_b)]
        );
    }

    #[test]
    fn auto_relay_state_zero_limits_disable_auto_reservation() {
        let relay = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let mut no_candidates = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 0,
            max_reservations: 1,
            retry_interval_seconds: 5,
        });
        assert!(!no_candidates.record_candidate(relay, address.clone()));

        let mut no_reservations = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 1,
            max_reservations: 0,
            retry_interval_seconds: 5,
        });
        no_reservations.record_reachability(AutoNatReachability::Private);
        assert!(no_reservations.record_candidate(relay, address));
        assert!(
            no_reservations
                .next_reservation_targets(Instant::now())
                .is_empty()
        );
    }

    #[test]
    fn auto_relay_state_releases_failed_attempt_slot_without_retrying_same_address() {
        let max_reservations = 2;
        let mut state = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 4,
            max_reservations,
            retry_interval_seconds: 5,
        });
        state.record_reachability(AutoNatReachability::Private);
        let candidates = (0..=max_reservations)
            .map(|port| {
                let relay = peer_id();
                let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/{}/p2p/{relay}", 4101 + port)
                    .parse()
                    .expect("relay address");
                (relay, address)
            })
            .collect::<Vec<_>>();
        for (relay, address) in &candidates {
            assert!(state.record_candidate(*relay, address.clone()));
        }
        let now = Instant::now();
        let initial_targets = state.next_reservation_targets(now);
        assert_eq!(initial_targets.len(), max_reservations);
        let failed_relay = initial_targets[0].0;

        assert!(state.release_reservation_peer(failed_relay));
        let retry_targets = state.next_reservation_targets(now);

        assert_eq!(retry_targets, vec![candidates[max_reservations].clone()]);
    }

    #[test]
    fn auto_relay_state_retries_lost_reservation_after_configured_delay() {
        let relay = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let mut state = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 1,
            max_reservations: 1,
            retry_interval_seconds: 7,
        });
        state.record_reachability(AutoNatReachability::Private);
        assert!(state.record_candidate(relay, address.clone()));
        let now = Instant::now();
        assert_eq!(
            state.next_reservation_targets(now),
            vec![(relay, address.clone())]
        );

        assert!(state.release_reservation_for_retry_after(relay, now));

        assert!(
            state
                .next_reservation_targets(now + Duration::from_secs(6))
                .is_empty()
        );
        assert_eq!(
            state.next_reservation_targets(now + Duration::from_secs(7)),
            vec![(relay, address)]
        );
    }

    #[test]
    fn auto_relay_state_retries_timed_out_single_candidate_after_configured_delay() {
        let relay = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let mut state = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 1,
            max_reservations: 1,
            retry_interval_seconds: 7,
        });
        state.record_reachability(AutoNatReachability::Private);
        assert!(state.record_candidate(relay, address.clone()));
        let now = Instant::now();
        assert_eq!(
            state.next_reservation_targets(now),
            vec![(relay, address.clone())]
        );

        assert_eq!(
            state.expire_pending_reservations(now + AUTO_RELAY_RESERVATION_PENDING_TIMEOUT),
            vec![(relay, address.clone())]
        );
        assert!(
            state
                .next_reservation_targets(
                    now + AUTO_RELAY_RESERVATION_PENDING_TIMEOUT + Duration::from_secs(6)
                )
                .is_empty()
        );
        assert_eq!(
            state.next_reservation_targets(
                now + AUTO_RELAY_RESERVATION_PENDING_TIMEOUT + Duration::from_secs(7)
            ),
            vec![(relay, address)]
        );
    }

    #[test]
    fn auto_relay_state_evicts_repeatedly_timed_out_candidate() {
        let relay = peer_id();
        let replacement = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let replacement_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4002/p2p/{replacement}")
            .parse()
            .expect("replacement relay address");
        let mut state = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 1,
            max_reservations: 1,
            retry_interval_seconds: 7,
        });
        state.record_reachability(AutoNatReachability::Private);
        assert!(state.record_candidate(relay, address.clone()));
        let now = Instant::now();

        assert_eq!(state.next_reservation_targets(now), vec![(relay, address)]);
        assert_eq!(
            state
                .expire_pending_reservations(now + AUTO_RELAY_RESERVATION_PENDING_TIMEOUT)
                .len(),
            1
        );
        assert!(!state.record_reservation_failure(relay));
        assert_eq!(state.snapshot(now).candidates, 1);
        assert!(!state.record_candidate(replacement, replacement_address.clone()));

        assert!(state.record_reservation_failure(relay));

        assert_eq!(state.snapshot(now).candidates, 0);
        assert!(state.record_candidate(replacement, replacement_address));
    }

    #[test]
    fn auto_relay_state_removes_identify_rejected_candidate() {
        let relay = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let mut state = AutoRelayState::default();

        assert!(state.record_candidate(relay, address));
        assert!(state.remove_candidate(relay));

        assert_eq!(state.snapshot(Instant::now()).candidates, 0);
        assert!(!state.remove_candidate(relay));
    }

    #[test]
    fn auto_relay_state_times_out_pending_reservation_and_rotates_candidate() {
        let relay_a = peer_id();
        let relay_b = peer_id();
        let address_a: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay_a}")
            .parse()
            .expect("relay address");
        let address_b: Multiaddr = format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay_b}")
            .parse()
            .expect("relay address");
        let mut state = AutoRelayState::new(AutoRelayConfig {
            max_candidates: 2,
            max_reservations: 1,
            retry_interval_seconds: 30,
        });
        let now = Instant::now();

        assert!(state.record_candidate(relay_a, address_a.clone()));
        assert!(state.record_candidate(relay_b, address_b.clone()));
        assert_eq!(
            state.next_reservation_targets(now),
            vec![(relay_a, address_a.clone())]
        );
        assert_eq!(state.snapshot(now).reservations, 0);

        assert_eq!(
            state.expire_pending_reservations(now + AUTO_RELAY_RESERVATION_PENDING_TIMEOUT),
            vec![(relay_a, address_a)]
        );
        assert_eq!(
            state.next_reservation_targets(now + AUTO_RELAY_RESERVATION_PENDING_TIMEOUT),
            vec![(relay_b, address_b)]
        );
    }

    #[test]
    fn auto_relay_state_keeps_successful_reservation_slot_occupied() {
        let relay = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let mut state = AutoRelayState::default();
        let now = Instant::now();
        state.record_reachability(AutoNatReachability::Private);
        assert!(state.record_candidate(relay, address.clone()));

        assert_eq!(state.next_reservation_targets(now), vec![(relay, address)]);
        state.record_reservation_accepted(relay);
        assert_eq!(state.snapshot(now).reservations, 1);

        assert!(state.next_reservation_targets(now).is_empty());
    }

    #[tokio::test]
    async fn kademlia_relay_infrastructure_does_not_gain_vpn_authority() {
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let relay = peer_id();
        let relay_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let mut infrastructure_peers = InfrastructurePeers::default();
        let mut auto_relay = AutoRelayState::default();
        let metrics = RuntimeMetrics::default();
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let config = config_with_peer(&local_identity, peer_id());
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        admit_kademlia_relay_infrastructure_peer(
            &mut node.swarm,
            &forwarder,
            &mut infrastructure_peers,
            &mut auto_relay,
            &metrics,
            relay,
            std::slice::from_ref(&relay_address).iter(),
        );

        assert!(infrastructure_peers.contains(relay));
        assert_eq!(auto_relay.snapshot(Instant::now()).candidates, 1);
        assert_eq!(
            authorize_established_connection(
                &OverlayMembership::default(),
                &infrastructure_peers,
                &metrics,
                relay,
                false,
            ),
            EstablishedConnectionAuthorization::InfrastructurePeer,
        );

        let local_capabilities = ControlCapabilities::local("lab", None, 1280);

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                relay,
                &ControlCapabilities::local("lab", None, 1280),
                &local_capabilities,
                &[],
            ),
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::UnauthorizedPeer)
        );
        assert_eq!(
            service_status_response_for_peer(
                &forwarder,
                &PathSet::new(),
                &PeerCapabilities::default(),
                relay,
                &ServiceStatusRequest::local("lab", None, 42),
                &local_capabilities,
                &[],
                Duration::from_secs(123),
                456,
            ),
            ServiceResponse::Rejected(ServiceRejectionReason::UnauthorizedPeer)
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.auto_relay_infrastructure_candidates, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_dial_attempts, 1);
        assert_eq!(snapshot.unauthorized_connections_dropped, 0);
    }

    #[tokio::test]
    async fn kademlia_relay_infrastructure_skips_configured_vpn_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured_peer = peer_id();
        let config = config_with_peer(&local_identity, configured_peer);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let relay_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{configured_peer}")
            .parse()
            .expect("relay address");
        let mut infrastructure_peers = InfrastructurePeers::default();
        let mut auto_relay = AutoRelayState::default();
        let metrics = RuntimeMetrics::default();

        admit_kademlia_relay_infrastructure_peer(
            &mut node.swarm,
            &forwarder,
            &mut infrastructure_peers,
            &mut auto_relay,
            &metrics,
            configured_peer,
            std::slice::from_ref(&relay_address).iter(),
        );

        assert!(!infrastructure_peers.contains(configured_peer));
        assert_eq!(auto_relay.snapshot(Instant::now()).candidates, 0);
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.auto_relay_infrastructure_candidates, 0);
        assert_eq!(snapshot.auto_relay_candidates, 0);
    }

    #[tokio::test]
    async fn unconfirmed_relay_infrastructure_is_removed() {
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let peer = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("address");
        let mut infrastructure_peers = InfrastructurePeers::default();
        let mut auto_relay = AutoRelayState::default();
        assert!(infrastructure_peers.insert(peer, address));
        assert!(
            auto_relay.record_candidate(
                peer,
                format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
                    .parse()
                    .expect("relay address")
            )
        );

        reject_unconfirmed_infrastructure_peer(
            &mut node.swarm,
            &mut infrastructure_peers,
            &mut auto_relay,
            peer,
            "missing_relay_hop",
        );

        assert!(!infrastructure_peers.contains(peer));
        assert_eq!(auto_relay.snapshot(Instant::now()).candidates, 0);
    }

    #[test]
    fn packet_plane_recovery_targets_include_configured_and_discovered_peer_addresses() {
        let local = peer_id();
        let peer = peer_id();
        let other = peer_id();
        let configured_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let discovered_address: Multiaddr =
            "/ip4/127.0.0.1/udp/4002/quic-v1".parse().expect("address");
        let other_address: Multiaddr = "/ip4/127.0.0.1/tcp/4003".parse().expect("address");

        let targets = packet_plane_recovery_targets(
            local,
            peer,
            &[(peer, configured_address.clone()), (other, other_address)],
            &[
                (peer, discovered_address.clone()),
                (peer, configured_address.clone()),
            ],
        );

        assert_eq!(
            targets,
            vec![(peer, configured_address), (peer, discovered_address)]
        );
    }

    #[test]
    fn packet_plane_recovery_targets_skip_local_peer() {
        let local = peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        assert!(packet_plane_recovery_targets(local, local, &[(local, address)], &[]).is_empty());
    }

    #[test]
    fn discovered_peer_addresses_are_unique_and_expirable() {
        let peer = peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let other_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");
        let now = Instant::now();
        let old = now.checked_sub(Duration::from_secs(30)).expect("old time");
        let mut discovered = DiscoveredPeerAddresses::default();

        discovered.insert_at(peer, address.clone(), old);
        discovered.insert_at(peer, address.clone(), now);
        discovered.insert_at(peer, other_address.clone(), old);

        assert_eq!(
            discovered.as_vec(),
            vec![(peer, address.clone()), (peer, other_address.clone())]
        );

        assert_eq!(discovered.drop_expired(now, Duration::from_secs(10)), 1);
        assert_eq!(discovered.as_vec(), vec![(peer, address.clone())]);

        discovered.remove(peer, &address);

        assert_eq!(discovered.as_vec(), Vec::new());
    }

    #[test]
    fn discovered_address_expiry_records_metrics() {
        let peer = peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let now = Instant::now();
        let mut discovered = DiscoveredPeerAddresses::default();
        let metrics = RuntimeMetrics::default();

        discovered.insert_at(
            peer,
            address,
            now.checked_sub(DISCOVERED_ADDRESS_TTL + Duration::from_secs(1))
                .expect("expired time"),
        );

        let expired = discovered.drop_expired(now, DISCOVERED_ADDRESS_TTL);
        metrics.record_discovered_address_expired(expired);

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_expired, 1);
    }

    #[test]
    fn overlay_membership_separates_vpn_peers_from_configured_infrastructure() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let local = local_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("local peer");
        let configured = peer_id();
        let bootstrap = peer_id();
        let relay = peer_id();
        let peer_address_relay = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: bootstrap.to_string(),
                    address: "/ip4/127.0.0.1/tcp/4001".to_owned(),
                }],
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig {
                    server: false,
                    reservations: vec![format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay}/p2p-circuit")],
                    auto: crate::config::AutoRelayConfig::default(),
                    resources: crate::config::RelayResourceConfig::default(),
                },
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: configured.to_string(),
                name: None,
                ip: None,
                addresses: vec![format!(
                    "/ip4/127.0.0.1/tcp/4003/p2p/{peer_address_relay}/p2p-circuit/p2p/{configured}"
                )],
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };

        let membership = OverlayMembership::from_config(&config).expect("membership");

        assert_eq!(membership.len(), 2);
        assert_eq!(membership.configured_infrastructure_len(), 3);
        assert!(membership.allows(local));
        assert!(membership.allows(configured));
        assert!(!membership.allows(bootstrap));
        assert!(!membership.allows(relay));
        assert!(!membership.allows(peer_address_relay));
        assert!(membership.allows_configured_infrastructure(bootstrap));
        assert!(membership.allows_configured_infrastructure(relay));
        assert!(membership.allows_configured_infrastructure(peer_address_relay));
        assert!(!membership.allows(peer_id()));
    }

    #[test]
    fn overlay_membership_accepts_member_record_peer() {
        let local_identity = NodeIdentity::generate_ed25519().expect("local identity");
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let mut config = config_with_peer(&local_identity, member_peer);
        config.peers.clear();
        config.network.member_records = vec![
            issue_membership_record_at(
                &issuer,
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
            .expect("member record"),
        ];

        let membership = OverlayMembership::from_config(&config).expect("membership");

        assert_eq!(membership.len(), 2);
        assert!(
            membership.allows(
                local_identity
                    .peer_id
                    .parse::<Libp2pPeerId>()
                    .expect("local peer")
            )
        );
        assert!(membership.allows(member_peer));
    }

    #[test]
    fn relay_peer_is_extracted_from_reservation_address() {
        let relay = peer_id();
        let target = peer_id();
        let reservation: Multiaddr = format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay}/p2p-circuit")
            .parse()
            .expect("reservation");
        let target_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay}/p2p-circuit/p2p/{target}")
                .parse()
                .expect("target address");

        assert_eq!(relay_peer_from_relayed_address(&reservation), Some(relay));
        assert_eq!(
            relay_peer_from_relayed_address(&target_address),
            Some(relay)
        );
    }

    #[test]
    fn discovered_address_target_uses_direct_or_relayed_target_peer() {
        let peer = peer_id();
        let relay = peer_id();
        let other = peer_id();
        let direct_without_target: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let direct_target: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("address");
        let relayed_without_target: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit")
                .parse()
                .expect("address");
        let relayed_target: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("address");
        let relayed_other_target: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{other}")
                .parse()
                .expect("address");

        assert!(address_targets_peer(peer, &direct_without_target));
        assert!(address_targets_peer(peer, &direct_target));
        assert!(address_targets_peer(peer, &relayed_without_target));
        assert!(address_targets_peer(peer, &relayed_target));
        assert!(!address_targets_peer(peer, &relayed_other_target));
    }

    #[test]
    fn unauthorized_connections_are_rejected_and_counted() {
        let allowed = peer_id();
        let infrastructure = peer_id();
        let rejected = peer_id();
        let membership = OverlayMembership {
            peers: HashSet::from([allowed]),
            configured_infrastructure_peers: HashSet::new(),
        };
        let mut infrastructure_peers = InfrastructurePeers::default();
        assert!(
            infrastructure_peers.insert(
                infrastructure,
                format!("/ip4/127.0.0.1/tcp/4001/p2p/{infrastructure}")
                    .parse()
                    .expect("infrastructure address"),
            )
        );
        let metrics = RuntimeMetrics::default();

        assert_eq!(
            authorize_established_connection(
                &membership,
                &infrastructure_peers,
                &metrics,
                allowed,
                false,
            ),
            EstablishedConnectionAuthorization::OverlayPeer,
        );
        assert_eq!(
            authorize_established_connection(
                &membership,
                &infrastructure_peers,
                &metrics,
                infrastructure,
                false,
            ),
            EstablishedConnectionAuthorization::InfrastructurePeer,
        );
        assert_eq!(
            authorize_established_connection(
                &membership,
                &infrastructure_peers,
                &metrics,
                rejected,
                false,
            ),
            EstablishedConnectionAuthorization::Rejected,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.unauthorized_connections_dropped, 1);
    }

    #[test]
    fn configured_infrastructure_connections_are_admitted_without_overlay_membership() {
        let infrastructure = peer_id();
        let rejected = peer_id();
        let membership = OverlayMembership {
            peers: HashSet::new(),
            configured_infrastructure_peers: HashSet::from([infrastructure]),
        };
        let infrastructure_peers = InfrastructurePeers::default();
        let metrics = RuntimeMetrics::default();

        assert_eq!(
            authorize_established_connection(
                &membership,
                &infrastructure_peers,
                &metrics,
                infrastructure,
                false,
            ),
            EstablishedConnectionAuthorization::InfrastructurePeer,
        );
        assert_eq!(
            authorize_established_connection(
                &membership,
                &infrastructure_peers,
                &metrics,
                rejected,
                false,
            ),
            EstablishedConnectionAuthorization::Rejected,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.unauthorized_connections_dropped, 1);
    }

    #[test]
    fn infrastructure_probe_connections_are_temporarily_admitted_without_counting_rejections() {
        let probe = peer_id();
        let membership = OverlayMembership {
            peers: HashSet::new(),
            configured_infrastructure_peers: HashSet::new(),
        };
        let infrastructure_peers = InfrastructurePeers::default();
        let metrics = RuntimeMetrics::default();

        assert_eq!(
            authorize_established_connection(
                &membership,
                &infrastructure_peers,
                &metrics,
                probe,
                true,
            ),
            EstablishedConnectionAuthorization::InfrastructureProbe,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.unauthorized_connections_dropped, 0);
    }

    #[test]
    fn outgoing_connection_errors_are_counted() {
        let metrics = RuntimeMetrics::default();

        handle_outgoing_connection_error(&metrics, Some(peer_id()), &"dial failed");
        handle_outgoing_connection_error(&metrics, None, &"peer id unavailable");

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outgoing_connection_errors, 2);
    }

    #[test]
    fn failed_relay_infrastructure_dials_are_removed_and_counted() {
        let peer = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("infrastructure address");
        let mut infrastructure_peers = InfrastructurePeers::default();
        let mut auto_relay = AutoRelayState::default();
        assert!(infrastructure_peers.insert(peer, address.clone()));
        assert!(auto_relay.record_candidate(peer, address));
        let metrics = RuntimeMetrics::default();

        handle_relay_infrastructure_outgoing_connection_error(
            &mut infrastructure_peers,
            &mut auto_relay,
            &OverlayMembership::default(),
            &metrics,
            Some(peer),
            false,
            &"dial failed",
        );

        assert!(!infrastructure_peers.contains(peer));
        assert_eq!(auto_relay.snapshot(Instant::now()).candidates, 0);
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outgoing_connection_errors, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_dial_failures, 1);
    }

    #[test]
    fn failed_overlay_peer_dials_do_not_remove_infrastructure_records() {
        let peer = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("infrastructure address");
        let mut infrastructure_peers = InfrastructurePeers::default();
        let mut auto_relay = AutoRelayState::default();
        assert!(infrastructure_peers.insert(peer, address.clone()));
        assert!(auto_relay.record_candidate(peer, address));
        let membership = OverlayMembership {
            peers: HashSet::from([peer]),
            configured_infrastructure_peers: HashSet::new(),
        };
        let metrics = RuntimeMetrics::default();

        handle_relay_infrastructure_outgoing_connection_error(
            &mut infrastructure_peers,
            &mut auto_relay,
            &membership,
            &metrics,
            Some(peer),
            false,
            &"dial failed",
        );

        assert!(infrastructure_peers.contains(peer));
        assert_eq!(auto_relay.snapshot(Instant::now()).candidates, 1);
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outgoing_connection_errors, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_dial_failures, 0);
    }

    #[test]
    fn failed_connected_infrastructure_dials_do_not_remove_relay_records() {
        let peer = peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("infrastructure address");
        let mut infrastructure_peers = InfrastructurePeers::default();
        let mut auto_relay = AutoRelayState::default();
        assert!(infrastructure_peers.insert(peer, address.clone()));
        assert!(auto_relay.record_candidate(peer, address));
        let metrics = RuntimeMetrics::default();

        handle_relay_infrastructure_outgoing_connection_error(
            &mut infrastructure_peers,
            &mut auto_relay,
            &OverlayMembership::default(),
            &metrics,
            Some(peer),
            true,
            &"dial failed",
        );

        assert!(infrastructure_peers.contains(peer));
        assert_eq!(auto_relay.snapshot(Instant::now()).candidates, 1);
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outgoing_connection_errors, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_dial_failures, 0);
    }

    #[tokio::test]
    async fn schedule_autonat_probe_records_enabled_probe() {
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = "/ip4/203.0.113.10/tcp/4001".parse().expect("address");

        assert!(schedule_autonat_probe(&mut node.swarm, &metrics, &address));

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.autonat_probes_scheduled, 1);
    }

    #[tokio::test]
    async fn schedule_autonat_probe_is_noop_when_disabled() {
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig {
                autonat: false,
                ..DiscoveryConfig::default()
            },
        })
        .expect("node");
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = "/ip4/203.0.113.10/tcp/4001".parse().expect("address");

        assert!(!schedule_autonat_probe(&mut node.swarm, &metrics, &address));

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.autonat_probes_scheduled, 0);
    }

    #[tokio::test]
    async fn learn_peer_address_records_accepted_address_and_dial_attempt() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut discovered = DiscoveredPeerAddresses::default();
        let paths = PathSet::new();
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        learn_peer_address(
            &mut node.swarm,
            &forwarder,
            &mut discovered,
            &paths,
            &metrics,
            configured,
            address.clone(),
            &DiscoveryConfig::default(),
            DiscoveredPeerAddressSource::UnauthenticatedDiscovery,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 1);
        assert_eq!(snapshot.discovered_address_dial_attempts, 1);
        assert_eq!(snapshot.discovered_address_dial_failures, 0);
        assert_eq!(snapshot.discovered_addresses_rejected, 0);
        assert_eq!(discovered.as_vec(), vec![(configured, address)]);
    }

    #[tokio::test]
    async fn learn_peer_address_ignores_unconfigured_peers_without_metrics() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let unconfigured = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut discovered = DiscoveredPeerAddresses::default();
        let paths = PathSet::new();
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        learn_peer_address(
            &mut node.swarm,
            &forwarder,
            &mut discovered,
            &paths,
            &metrics,
            unconfigured,
            address,
            &DiscoveryConfig::default(),
            DiscoveredPeerAddressSource::UnauthenticatedDiscovery,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 0);
        assert_eq!(snapshot.discovered_address_dial_attempts, 0);
        assert_eq!(snapshot.discovered_address_dial_failures, 0);
        assert_eq!(snapshot.discovered_addresses_rejected, 0);
        assert!(discovered.as_vec().is_empty());
    }

    #[tokio::test]
    async fn learn_peer_address_counts_mismatched_discovered_targets_as_rejected() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let other = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut discovered = DiscoveredPeerAddresses::default();
        let paths = PathSet::new();
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{other}")
            .parse()
            .expect("address");

        learn_peer_address(
            &mut node.swarm,
            &forwarder,
            &mut discovered,
            &paths,
            &metrics,
            configured,
            address,
            &DiscoveryConfig::default(),
            DiscoveredPeerAddressSource::UnauthenticatedDiscovery,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 0);
        assert_eq!(snapshot.discovered_address_dial_attempts, 0);
        assert_eq!(snapshot.discovered_address_dial_failures, 0);
        assert_eq!(snapshot.discovered_addresses_rejected, 1);
        assert!(discovered.as_vec().is_empty());
    }

    #[tokio::test]
    async fn autonat_status_changes_update_reachability_metrics() {
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig {
                mdns: false,
                dcutr: false,
                ..DiscoveryConfig::default()
            },
        })
        .expect("node");
        let metrics = RuntimeMetrics::default();
        let mut auto_relay = AutoRelayState::default();
        let public_address: Multiaddr = "/ip4/203.0.113.10/tcp/4001".parse().expect("address");

        handle_autonat_event(
            &mut node.swarm,
            &mut auto_relay,
            &metrics,
            autonat::Event::StatusChanged {
                old: autonat::NatStatus::Unknown,
                new: autonat::NatStatus::Public(public_address.clone()),
            },
        );
        assert!(
            node.swarm
                .external_addresses()
                .any(|address| address == &public_address)
        );

        handle_autonat_event(
            &mut node.swarm,
            &mut auto_relay,
            &metrics,
            autonat::Event::StatusChanged {
                old: autonat::NatStatus::Public(
                    "/ip4/203.0.113.10/tcp/4001".parse().expect("address"),
                ),
                new: autonat::NatStatus::Private,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.autonat_status_unknown, 0);
        assert_eq!(snapshot.autonat_status_public, 0);
        assert_eq!(snapshot.autonat_status_private, 1);
        assert_eq!(snapshot.autonat_status_changes_to_public, 1);
        assert_eq!(snapshot.autonat_status_changes_to_private, 1);
    }

    #[test]
    fn peer_dial_address_appends_p2p_component() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        let dial = peer_dial_address(peer, address);

        assert!(dial.to_string().ends_with(&format!("/p2p/{peer}")));
    }

    #[test]
    fn peer_dial_address_preserves_existing_p2p_address() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("address");

        assert_eq!(peer_dial_address(peer, address.clone()), address);
    }

    #[test]
    fn endpoint_path_kind_prefers_relay_then_quic_then_tcp() {
        let relay: Multiaddr =
            "/ip4/127.0.0.1/tcp/4001/p2p/12D3KooWLSJY9r3syVF7eh1b5CAJSmQkHdHu1QMUGNXk7Nzd4y6f/p2p-circuit/p2p/12D3KooWBCGXBm96czaYf6X41Hd2mD879WoF5Jyi8YUxi2Tiz3aT"
                .parse()
                .expect("relay address");
        let quic: Multiaddr = "/ip4/127.0.0.1/udp/4001/quic-v1"
            .parse()
            .expect("quic address");
        let tcp: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("tcp address");

        assert_eq!(
            path_kind_for_endpoint(&ConnectedPoint::Dialer {
                address: relay,
                role_override: Endpoint::Dialer,
                port_use: PortUse::Reuse,
            }),
            PathKind::CircuitRelay
        );
        assert_eq!(
            path_kind_for_endpoint(&ConnectedPoint::Dialer {
                address: quic,
                role_override: Endpoint::Dialer,
                port_use: PortUse::Reuse,
            }),
            PathKind::DirectQuicStream
        );
        assert_eq!(
            path_kind_for_endpoint(&ConnectedPoint::Listener {
                local_addr: tcp,
                send_back_addr: "/ip4/127.0.0.1/tcp/5000".parse().expect("send back"),
            }),
            PathKind::DirectTcpStream
        );
    }

    #[test]
    fn packet_drop_reasons_map_to_packet_rejections() {
        assert_eq!(
            packet_rejection_reason(PacketDropReason::MalformedPacket),
            PacketRejectionReason::MalformedPacket
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::PacketTooLarge),
            PacketRejectionReason::PacketTooLarge
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::QueueFull),
            PacketRejectionReason::PacketTooLarge
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::Replay),
            PacketRejectionReason::Replay
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::UnauthorizedPeer),
            PacketRejectionReason::UnauthorizedPeer
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::NoTransportPeer),
            PacketRejectionReason::UnauthorizedPeer
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::UnauthorizedSource),
            PacketRejectionReason::UnauthorizedSource
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::UnauthorizedDestination),
            PacketRejectionReason::UnauthorizedDestination
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::UnexpectedPayload),
            PacketRejectionReason::UnexpectedPayload
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::RateLimited),
            PacketRejectionReason::RateLimited
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::NoRoute),
            PacketRejectionReason::MalformedPacket
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::MalformedPacket),
            PacketDropReason::MalformedPacket
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::PacketTooLarge),
            PacketDropReason::PacketTooLarge
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::Replay),
            PacketDropReason::Replay
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::UnauthorizedPeer),
            PacketDropReason::UnauthorizedPeer
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::UnauthorizedSource),
            PacketDropReason::UnauthorizedSource
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::UnauthorizedDestination),
            PacketDropReason::UnauthorizedDestination
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::UnexpectedPayload),
            PacketDropReason::UnexpectedPayload
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::RateLimited),
            PacketDropReason::RateLimited
        );
    }

    #[test]
    fn packet_plane_receive_errors_map_to_stable_drop_reasons_and_names() {
        use crate::runtime::packet_plane::PacketPlaneDatagramError;

        let unknown_endpoint = PacketPlaneIoError::UnknownEndpoint {
            actual: "127.0.0.1:51820".parse().expect("endpoint"),
        };
        let oversized = PacketPlaneIoError::Datagram(PacketPlaneDatagramError::PayloadTooLarge {
            actual: 1_300,
            max: 1_280,
        });
        let decrypt = PacketPlaneIoError::Datagram(PacketPlaneDatagramError::Decrypt);
        let replay = PacketPlaneIoError::Datagram(PacketPlaneDatagramError::ReplayedDatagram {
            session_id: 77,
            sequence: 42,
        });

        assert_eq!(
            packet_plane_inbound_drop_reason(&unknown_endpoint),
            PacketDropReason::UnauthorizedPeer
        );
        assert_eq!(
            packet_plane_inbound_metric_reason(&unknown_endpoint),
            PacketPlaneDropReason::UnknownEndpoint
        );
        assert_eq!(
            packet_plane_io_error_name(&unknown_endpoint),
            "unknown_endpoint"
        );
        assert_eq!(
            packet_plane_inbound_drop_reason(&oversized),
            PacketDropReason::PacketTooLarge
        );
        assert_eq!(
            packet_plane_inbound_metric_reason(&oversized),
            PacketPlaneDropReason::PayloadTooLarge
        );
        assert_eq!(packet_plane_io_error_name(&oversized), "payload_too_large");
        assert_eq!(
            packet_plane_inbound_drop_reason(&decrypt),
            PacketDropReason::MalformedPacket
        );
        assert_eq!(
            packet_plane_inbound_metric_reason(&decrypt),
            PacketPlaneDropReason::Decrypt
        );
        assert_eq!(packet_plane_io_error_name(&decrypt), "decrypt");
        assert_eq!(
            packet_plane_inbound_drop_reason(&replay),
            PacketDropReason::Replay
        );
        assert_eq!(
            packet_plane_inbound_metric_reason(&replay),
            PacketPlaneDropReason::ReplayedDatagram
        );
        assert_eq!(packet_plane_io_error_name(&replay), "replayed_datagram");
    }

    #[test]
    fn packet_plane_transport_failure_demotes_datagram_path_to_relay() {
        let peer = PeerId::from_bytes([9; 32]);
        let mut paths = PathSet::new();
        let metrics = RuntimeMetrics::default();

        paths.record_established(peer, PathKind::CircuitRelay);
        paths.upsert(crate::path::PathCandidate::new(
            peer,
            PathKind::DirectUdpDatagram,
        ));

        maybe_demote_packet_plane_path(
            &mut paths,
            &metrics,
            peer,
            PathKind::DirectUdpDatagram,
            &PacketPlaneIoError::NoSession { peer },
        );

        assert_eq!(
            paths.best_for(peer).map(|candidate| candidate.kind),
            Some(PathKind::CircuitRelay)
        );
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.packet_plane_path_demotions, 1);
        assert_eq!(snapshot.path_fallbacks_to_relay, 1);
    }

    #[test]
    fn packet_plane_payload_error_does_not_demote_datagram_path() {
        use crate::runtime::packet_plane::PacketPlaneDatagramError;

        let peer = PeerId::from_bytes([10; 32]);
        let mut paths = PathSet::new();
        let metrics = RuntimeMetrics::default();

        paths.record_established(peer, PathKind::CircuitRelay);
        paths.upsert(crate::path::PathCandidate::new(
            peer,
            PathKind::DirectUdpDatagram,
        ));

        let error = PacketPlaneIoError::Datagram(PacketPlaneDatagramError::PayloadTooLarge {
            actual: 1_500,
            max: 1_280,
        });
        maybe_demote_packet_plane_path(
            &mut paths,
            &metrics,
            peer,
            PathKind::DirectUdpDatagram,
            &error,
        );

        assert_eq!(
            paths.best_for(peer).map(|candidate| candidate.kind),
            Some(PathKind::DirectUdpDatagram)
        );
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.packet_plane_path_demotions, 0);
        assert_eq!(snapshot.path_fallbacks_to_relay, 0);
    }

    #[test]
    fn quic_receive_peer_error_demotes_datagram_path_to_relay() {
        let peer = PeerId::from_bytes([11; 32]);
        let mut paths = PathSet::new();
        let metrics = RuntimeMetrics::default();

        paths.record_established(peer, PathKind::CircuitRelay);
        paths.upsert(crate::path::PathCandidate::new(
            peer,
            PathKind::DirectQuicDatagram,
        ));

        maybe_demote_packet_plane_quic_receive_path(
            &mut paths,
            &metrics,
            &PacketPlaneQuicError::NoConnection { peer },
        );

        assert_eq!(
            paths.best_for(peer).map(|candidate| candidate.kind),
            Some(PathKind::CircuitRelay)
        );
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.packet_plane_path_demotions, 1);
        assert_eq!(snapshot.path_fallbacks_to_relay, 1);
    }

    #[test]
    fn stream_fallback_timeout_demotes_direct_path_to_relay() {
        let peer = PeerId::from_bytes([12; 32]);
        let mut paths = PathSet::new();
        let metrics = RuntimeMetrics::default();

        paths.record_established(peer, PathKind::CircuitRelay);
        paths.upsert(crate::path::PathCandidate::new(
            peer,
            PathKind::DirectTcpStream,
        ));

        maybe_demote_stream_fallback_path(
            &mut paths,
            &metrics,
            peer,
            PathKind::DirectTcpStream,
            &request_response::OutboundFailure::Timeout,
        );

        assert_eq!(
            paths.best_for(peer).map(|candidate| candidate.kind),
            Some(PathKind::CircuitRelay)
        );
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.stream_fallback_path_demotions, 1);
        assert_eq!(snapshot.path_fallbacks_to_relay, 1);
    }

    #[test]
    fn stream_fallback_failure_does_not_demote_relay_path() {
        let peer = PeerId::from_bytes([12; 32]);
        let mut paths = PathSet::new();
        let metrics = RuntimeMetrics::default();

        paths.record_established(peer, PathKind::CircuitRelay);

        assert!(!maybe_demote_stream_fallback_path(
            &mut paths,
            &metrics,
            peer,
            PathKind::CircuitRelay,
            &request_response::OutboundFailure::DialFailure,
        ));

        assert_eq!(
            paths.best_for(peer).map(|candidate| candidate.kind),
            Some(PathKind::CircuitRelay)
        );
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.stream_fallback_path_demotions, 0);
        assert_eq!(snapshot.path_fallbacks_to_relay, 0);
    }

    #[tokio::test]
    async fn packet_plane_session_expiry_marks_datagram_path_unhealthy() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote_peer = remote_identity.peer_id.parse().expect("remote peer");
        let remote_overlay = PeerId::from_libp2p(remote_peer);
        let config = config_with_peer(&local_identity, remote_peer);
        let mut node = build_node(&HostConfig {
            identity: local_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectQuicStream);
        paths.record_established(remote_overlay, PathKind::DirectUdpDatagram);

        let mut packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
                .await
                .expect("packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        let local_endpoint = packet_plane.primary_listener().expect("listener");
        let remote_endpoint = "127.0.0.1:51820".parse().expect("remote endpoint");
        let hello = verified_test_packet_plane_handshake(
            PacketPlaneHandshakeKind::Hello,
            &local_identity,
            &local_secret,
            1280,
            local_endpoint,
        );
        let accept = verified_test_packet_plane_handshake(
            PacketPlaneHandshakeKind::Accept,
            &remote_identity,
            &remote_secret,
            1280,
            remote_endpoint,
        );
        let session_ttl = Duration::from_secs(3);
        packet_plane
            .establish_test_session_at(
                PacketPlaneSessionRole::Initiator,
                &local_secret,
                &hello,
                &accept,
                Instant::now()
                    .checked_sub(session_ttl)
                    .expect("established time"),
            )
            .expect("session");

        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            packet_plane_test_capabilities(remote_endpoint),
        );
        let mut negotiator = PacketPlaneNegotiator::default();
        let metrics = RuntimeMetrics::default();
        let local_capabilities = packet_plane_test_capabilities(local_endpoint);

        let mut expiry_context = PacketPlaneExpiryContext {
            swarm: &mut node.swarm,
            forwarder: &forwarder,
            paths: &mut paths,
            peer_capabilities: &peer_capabilities,
            packet_plane: &mut packet_plane,
            packet_plane_quic: None,
            negotiator: &mut negotiator,
            identity: &local_identity,
            local_capabilities: &local_capabilities,
            metrics: &metrics,
            session_ttl,
        };
        expire_packet_plane_sessions(&mut expiry_context);

        assert!(!packet_plane.has_session(remote_overlay));
        assert_eq!(
            paths
                .best_for(remote_overlay)
                .map(|candidate| candidate.kind),
            Some(PathKind::DirectQuicStream)
        );
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.packet_plane_sessions_expired, 1);
    }

    #[tokio::test]
    async fn packet_plane_quic_session_expiry_marks_datagram_path_unhealthy() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote_peer = remote_identity.peer_id.parse().expect("remote peer");
        let remote_overlay = PeerId::from_libp2p(remote_peer);
        let config = config_with_peer(&local_identity, remote_peer);
        let mut node = build_node(&HostConfig {
            identity: local_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectQuicStream);
        paths.record_established(remote_overlay, PathKind::DirectQuicDatagram);

        let mut packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
                .await
                .expect("packet plane");
        let mut packet_plane_quic =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("local quic"))
                .expect("local quic");
        let mut remote_quic =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("remote quic"))
                .expect("remote quic");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_quic_sessions(
            &mut packet_plane_quic,
            &mut remote_quic,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1_200,
        )
        .await;

        let mut peer_capabilities = PeerCapabilities::default();
        let mut remote_capabilities =
            ControlCapabilities::local("lab", None, 1_280).with_owned_quic_packet_plane(true);
        remote_capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        remote_capabilities.owned_quic_packet_endpoint_candidates =
            vec![remote_quic.local_addr().to_string()];
        remote_capabilities.owned_quic_packet_plane_certificate_der =
            Some(remote_quic.server_certificate().as_ref().to_vec());
        peer_capabilities.record(remote_overlay, remote_capabilities);
        let mut negotiator = PacketPlaneNegotiator::default();
        let metrics = RuntimeMetrics::default();
        let mut local_capabilities =
            ControlCapabilities::local("lab", None, 1_280).with_owned_quic_packet_plane(true);
        local_capabilities.owned_quic_packet_endpoint_candidates =
            vec![packet_plane_quic.local_addr().to_string()];
        local_capabilities.owned_quic_packet_plane_certificate_der =
            Some(packet_plane_quic.server_certificate().as_ref().to_vec());

        {
            let mut expiry_context = PacketPlaneExpiryContext {
                swarm: &mut node.swarm,
                forwarder: &forwarder,
                paths: &mut paths,
                peer_capabilities: &peer_capabilities,
                packet_plane: &mut packet_plane,
                packet_plane_quic: Some(&mut packet_plane_quic),
                negotiator: &mut negotiator,
                identity: &local_identity,
                local_capabilities: &local_capabilities,
                metrics: &metrics,
                session_ttl: Duration::ZERO,
            };
            expire_packet_plane_sessions(&mut expiry_context);
        }

        assert!(!packet_plane_quic.has_session(remote_overlay));
        assert_eq!(
            paths
                .best_for(remote_overlay)
                .map(|candidate| candidate.kind),
            Some(PathKind::DirectQuicStream)
        );
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.packet_plane_sessions_expired, 1);
    }

    #[test]
    fn rejected_packet_responses_update_outbound_drop_metrics() {
        let metrics = RuntimeMetrics::default();
        let reason = packet_rejection_drop_reason(PacketRejectionReason::PacketTooLarge);

        metrics.record_outbound_failure();
        metrics.record_outbound_drop(reason);

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outbound_failures, 1);
        assert_eq!(snapshot.outbound_dropped_packets, 1);
        assert_eq!(snapshot.outbound_drop_packet_too_large_packets, 1);
    }

    #[test]
    fn packet_too_big_feedback_records_no_writer_and_unparseable_outcomes() {
        let metrics = RuntimeMetrics::default();
        let mut paths = PathSet::new();
        let peer_capabilities = PeerCapabilities::default();
        let mut packet_in_flight = PacketInFlight::new(1);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );
        let error = ForwardError::PacketTooLarge {
            actual: 1_300,
            max: 1_200,
        };

        maybe_write_packet_too_big(
            &mut context,
            &ipv4_packet(Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(10, 0, 0, 2)),
            &error,
        );
        maybe_write_packet_too_big(&mut context, b"not-an-ip-packet", &error);

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outbound_packet_too_big_notifications, 0);
        assert_eq!(snapshot.outbound_packet_too_big_no_writer, 1);
        assert_eq!(snapshot.outbound_packet_too_big_unparseable, 1);
        assert_eq!(snapshot.outbound_packet_too_big_write_failures, 0);
    }

    #[test]
    fn capability_response_accepts_configured_compatible_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let local_capabilities = ControlCapabilities::local("lab", None, 1280);

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &ControlCapabilities::local("lab", None, 1200),
                &local_capabilities,
                &[],
            ),
            ControlResponse::CapabilitiesAccepted(local_capabilities)
        );
    }

    #[test]
    fn service_status_response_accepts_configured_compatible_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = config_with_peer(&local_identity, remote);
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut paths = PathSet::new();
        paths.record_established_with_mtu(remote_overlay, PathKind::DirectQuicStream, Some(1_200));
        paths.record_rtt(remote_overlay, PathKind::DirectQuicStream, 42);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1_280),
        );
        let local_capabilities = ControlCapabilities::local("lab", None, 1280);

        assert_eq!(
            service_status_response_for_peer(
                &forwarder,
                &paths,
                &peer_capabilities,
                remote,
                &ServiceStatusRequest::local("lab", None, 42),
                &local_capabilities,
                &[],
                Duration::from_secs(123),
                456,
            ),
            ServiceResponse::Status(
                ServiceStatusResponse::local("lab", None, 42, 1280)
                    .with_packet_plane_session_ttl_seconds(123)
                    .with_packet_plane_replay_windows_per_session(456)
                    .with_selected_path(
                        PathKind::DirectQuicStream.wire_name().to_owned(),
                        71,
                        1_200,
                        Some(42)
                    )
            )
        );
    }

    #[test]
    fn service_status_response_rejects_unconfigured_or_wrong_overlay_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let unconfigured = peer_id();
        let config = config_with_peer(&local_identity, remote);
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let local_capabilities =
            ControlCapabilities::local("lab", Some("expected".to_owned()), 1280);

        assert_eq!(
            service_status_response_for_peer(
                &forwarder,
                &PathSet::new(),
                &PeerCapabilities::default(),
                unconfigured,
                &ServiceStatusRequest::local("lab", Some("expected".to_owned()), 1),
                &local_capabilities,
                &[],
                Duration::from_secs(123),
                456,
            ),
            ServiceResponse::Rejected(ServiceRejectionReason::UnauthorizedPeer)
        );
        assert_eq!(
            service_status_response_for_peer(
                &forwarder,
                &PathSet::new(),
                &PeerCapabilities::default(),
                remote,
                &ServiceStatusRequest::local("prod", Some("expected".to_owned()), 1),
                &local_capabilities,
                &[],
                Duration::from_secs(123),
                456,
            ),
            ServiceResponse::Rejected(ServiceRejectionReason::WrongNetwork)
        );
        assert_eq!(
            service_status_response_for_peer(
                &forwarder,
                &PathSet::new(),
                &PeerCapabilities::default(),
                remote,
                &ServiceStatusRequest::local("lab", Some("wrong".to_owned()), 1),
                &local_capabilities,
                &[],
                Duration::from_secs(123),
                456,
            ),
            ServiceResponse::Rejected(ServiceRejectionReason::MembershipMismatch)
        );
    }

    #[test]
    fn service_responses_update_status_metrics() {
        let remote = peer_id();
        let metrics = RuntimeMetrics::default();

        handle_service_response(
            &metrics,
            remote,
            ServiceResponse::Status(ServiceStatusResponse::local("lab", None, 42, 1280)),
            "lab",
            None,
            &[],
        );
        handle_service_response(
            &metrics,
            remote,
            ServiceResponse::Status(ServiceStatusResponse::local("prod", None, 42, 1280)),
            "lab",
            None,
            &[],
        );
        handle_service_response(
            &metrics,
            remote,
            ServiceResponse::Rejected(ServiceRejectionReason::WrongNetwork),
            "lab",
            None,
            &[],
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.service_responses_received, 3);
        assert_eq!(snapshot.service_status_accepts, 1);
        assert_eq!(snapshot.service_status_rejections, 2);
        assert_eq!(snapshot.service_reject_wrong_network, 2);
        assert_eq!(snapshot.service_failures, 2);
    }

    #[test]
    fn accepted_control_response_records_capability_metrics() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut peer_capabilities = PeerCapabilities::default();
        let metrics = RuntimeMetrics::default();

        handle_control_response(
            &forwarder,
            &mut peer_capabilities,
            &metrics,
            remote,
            ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("lab", None, 1200)),
            MembershipValidationScope {
                network: "lab",
                current_tag: None,
                previous_tags: &[],
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(peer_capabilities.len(), 1);
        assert_eq!(snapshot.control_capability_accepts, 1);
        assert_eq!(snapshot.control_responses_received, 1);
        assert_eq!(snapshot.control_failures, 0);
    }

    #[test]
    fn rejected_control_response_records_rejection_reason_metrics() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut peer_capabilities = PeerCapabilities::default();
        let metrics = RuntimeMetrics::default();

        handle_control_response(
            &forwarder,
            &mut peer_capabilities,
            &metrics,
            remote,
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::WrongNetwork),
            MembershipValidationScope {
                network: "lab",
                current_tag: None,
                previous_tags: &[],
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert!(peer_capabilities.is_empty());
        assert_eq!(snapshot.control_capability_accepts, 0);
        assert_eq!(snapshot.control_capability_rejections, 1);
        assert_eq!(snapshot.control_reject_wrong_network, 1);
        assert_eq!(snapshot.control_responses_received, 1);
        assert_eq!(snapshot.control_failures, 1);
    }

    #[test]
    fn incompatible_accepted_control_response_records_response_metrics() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut peer_capabilities = PeerCapabilities::default();
        let metrics = RuntimeMetrics::default();

        handle_control_response(
            &forwarder,
            &mut peer_capabilities,
            &metrics,
            remote,
            ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("prod", None, 1200)),
            MembershipValidationScope {
                network: "lab",
                current_tag: None,
                previous_tags: &[],
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert!(peer_capabilities.is_empty());
        assert_eq!(snapshot.control_capability_accepts, 0);
        assert_eq!(snapshot.control_capability_rejections, 1);
        assert_eq!(snapshot.control_reject_wrong_network, 1);
        assert_eq!(snapshot.control_responses_received, 1);
        assert_eq!(snapshot.control_failures, 1);
    }

    #[test]
    fn first_connection_invalidates_stale_peer_capabilities() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );

        invalidate_peer_capabilities_on_first_connection(
            &forwarder,
            &mut peer_capabilities,
            remote,
            1,
        );

        assert!(!peer_capabilities.contains(remote_overlay));
    }

    #[test]
    fn peer_capabilities_survive_transient_disconnects() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );

        invalidate_peer_capabilities_when_disconnected(
            &forwarder,
            &mut peer_capabilities,
            remote,
            1,
        );
        assert!(peer_capabilities.contains(remote_overlay));

        invalidate_peer_capabilities_when_disconnected(
            &forwarder,
            &mut peer_capabilities,
            remote,
            0,
        );
        assert!(peer_capabilities.contains(remote_overlay));
    }

    #[test]
    fn capability_response_accepts_authorized_route_advertisements() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 100,
                }],
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let local_capabilities = ControlCapabilities::local("lab", None, 1280);
        let remote_capabilities = ControlCapabilities::local("lab", None, 1200)
            .with_advertised_routes(vec![
                ControlRoute::new(format!("{}/32", builtin_ipv4(remote_overlay)), 0),
                ControlRoute::new("10.42.0.0/24", 100),
            ]);

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &remote_capabilities,
                &local_capabilities,
                &[],
            ),
            ControlResponse::CapabilitiesAccepted(local_capabilities)
        );
    }

    #[test]
    fn capability_response_rejects_unauthorized_route_advertisements() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let remote_capabilities = ControlCapabilities::local("lab", None, 1200)
            .with_advertised_routes(vec![ControlRoute::new("10.42.0.0/24", 0)]);

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &remote_capabilities,
                &ControlCapabilities::local("lab", None, 1280),
                &[],
            ),
            ControlResponse::CapabilitiesRejected(
                ControlRejectionReason::UnauthorizedRouteAdvertisement
            )
        );
    }

    #[test]
    fn capability_response_learns_member_records_from_configured_peer() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let trusted = peer_id();
        let trusted_subject = NodeIdentity::generate_ed25519().expect("trusted subject");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let trust_root_record = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: trusted_subject,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("trust root");
        let member_record = issue_membership_record_at(
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
                route_grants: vec![RouteConfig {
                    prefix: "10.77.0.0/24".to_owned(),
                    metric: 100,
                }],
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("record");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: vec![trust_root_record],
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: trusted.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut membership = OverlayMembership::from_config(&config).expect("membership");
        let local_capabilities = ControlCapabilities::local("lab", None, 1280);
        let trusted_capabilities =
            ControlCapabilities::local("lab", None, 1200).with_member_records(vec![member_record]);

        let response = capability_response_for_peer_with_membership_records(
            &mut forwarder,
            &mut membership,
            trusted,
            &trusted_capabilities,
            &local_capabilities,
            &[],
        );
        let ControlResponse::CapabilitiesAccepted(accepted) = response else {
            panic!("expected accepted capabilities");
        };
        assert_eq!(accepted.member_records.len(), 2);

        assert!(membership.allows(member_peer));
        assert!(forwarder.is_configured_transport_peer(member_peer));
        assert!(
            forwarder
                .authorizes_advertised_routes(member_peer, &[ControlRoute::new("10.77.0.0/24", 1)])
        );
        assert_eq!(forwarder.member_record_count(), 2);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn capability_response_learns_revocation_and_removes_live_member_authorization() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let trusted = peer_id();
        let trusted_subject = NodeIdentity::generate_ed25519().expect("trusted subject");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let trust_root_record = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: trusted_subject,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("trust root");
        let member_record = issue_membership_record_at(
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
                    prefix: "10.77.0.0/24".to_owned(),
                    metric: 100,
                }],
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("record");
        let revocation = crate::membership::issue_membership_record_for_subject_at(
            &issuer,
            crate::membership::MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: crate::membership::MembershipRecordSubject::from_identity(&member)
                    .expect("member subject"),
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
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: vec![trust_root_record],
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: trusted.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut membership = OverlayMembership::from_config(&config).expect("membership");
        let local_capabilities = ControlCapabilities::local("lab", None, 1280);
        let trusted_capabilities = ControlCapabilities::local("lab", None, 1200)
            .with_member_records(vec![member_record, revocation]);

        let response = capability_response_for_peer_with_membership_records(
            &mut forwarder,
            &mut membership,
            trusted,
            &trusted_capabilities,
            &local_capabilities,
            &[],
        );
        let ControlResponse::CapabilitiesAccepted(accepted) = response else {
            panic!("expected accepted capabilities");
        };

        assert_eq!(accepted.member_records.len(), 2);
        assert!(
            accepted
                .member_records
                .iter()
                .any(
                    |record| record.payload.member_peer == member_peer.to_string()
                        && record.payload.revoked
                )
        );
        assert!(!membership.allows(member_peer));
        assert!(!forwarder.is_configured_transport_peer(member_peer));
        assert!(
            !forwarder
                .authorizes_advertised_routes(member_peer, &[ControlRoute::new("10.77.0.0/24", 1)])
        );
    }

    #[test]
    fn capability_response_rejects_untrusted_member_record_issuers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let trusted = peer_id();
        let member = NodeIdentity::generate_ed25519().expect("member");
        let trusted_issuer = NodeIdentity::generate_ed25519().expect("trusted issuer");
        let untrusted_issuer = NodeIdentity::generate_ed25519().expect("untrusted issuer");
        let trust_subject = NodeIdentity::generate_ed25519().expect("trust subject");
        let trust_root_record = issue_membership_record_at(
            &trusted_issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: trust_subject,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("trust root");
        let untrusted_record = issue_membership_record_at(
            &untrusted_issuer,
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
        .expect("untrusted record");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: vec![trust_root_record],
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: trusted.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut membership = OverlayMembership::from_config(&config).expect("membership");

        assert_eq!(
            capability_response_for_peer_with_membership_records(
                &mut forwarder,
                &mut membership,
                trusted,
                &ControlCapabilities::local("lab", None, 1200)
                    .with_member_records(vec![untrusted_record]),
                &ControlCapabilities::local("lab", None, 1280),
                &[],
            ),
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::InvalidMembershipRecord)
        );
        assert_eq!(forwarder.member_record_count(), 1);
    }

    #[test]
    fn forwarder_prunes_expired_member_records_from_live_authorization() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let now = current_unix_seconds_lossy();
        let member_record = issue_membership_record_at(
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
                route_grants: vec![RouteConfig {
                    prefix: "10.88.0.0/24".to_owned(),
                    metric: 100,
                }],
                expires_at_unix_seconds: Some(now + 1),
            },
            now,
        )
        .expect("member record");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: vec![member_record],
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut membership = OverlayMembership::from_config(&config).expect("membership");

        assert!(membership.allows(member_peer));
        assert!(forwarder.is_configured_transport_peer(member_peer));
        assert!(
            forwarder
                .authorizes_advertised_routes(member_peer, &[ControlRoute::new("10.88.0.0/24", 1)])
        );

        let stats = forwarder
            .prune_membership_records(now + 2)
            .expect("pruned member records");
        membership
            .replace_record_members(forwarder.config(), forwarder.member_records(), now + 2)
            .expect("membership rebuilt");

        assert_eq!(stats.removed_expired, 1);
        assert_eq!(forwarder.member_record_count(), 0);
        assert!(!membership.allows(member_peer));
        assert!(!forwarder.is_configured_transport_peer(member_peer));
        assert!(
            !forwarder
                .authorizes_advertised_routes(member_peer, &[ControlRoute::new("10.88.0.0/24", 1)])
        );
    }

    #[test]
    fn capability_response_rejects_invalid_member_records() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let trusted = peer_id();
        let member = NodeIdentity::generate_ed25519().expect("member");
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let mut member_record = issue_membership_record_at(
            &issuer,
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
        .expect("record");
        member_record.payload.sequence += 1;
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: trusted.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut membership = OverlayMembership::from_config(&config).expect("membership");

        assert_eq!(
            capability_response_for_peer_with_membership_records(
                &mut forwarder,
                &mut membership,
                trusted,
                &ControlCapabilities::local("lab", None, 1200)
                    .with_member_records(vec![member_record]),
                &ControlCapabilities::local("lab", None, 1280),
                &[],
            ),
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::InvalidMembershipRecord)
        );
        assert_eq!(forwarder.member_record_count(), 0);
    }

    #[test]
    fn capability_response_rejects_wrong_network() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &ControlCapabilities::local("prod", None, 1280),
                &ControlCapabilities::local("lab", None, 1280),
                &[],
            ),
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::WrongNetwork)
        );
    }

    #[test]
    fn capability_response_rejects_wrong_membership_tag() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &ControlCapabilities::local("lab", Some("remote-tag".to_owned()), 1280),
                &ControlCapabilities::local("lab", Some("local-tag".to_owned()), 1280),
                &[],
            ),
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::MembershipMismatch)
        );
    }

    #[test]
    fn capability_response_accepts_previous_membership_tag() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let local_capabilities =
            ControlCapabilities::local("lab", Some("local-tag".to_owned()), 1280);

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &ControlCapabilities::local("lab", Some("previous-tag".to_owned()), 1280),
                &local_capabilities,
                &[String::from("previous-tag")],
            ),
            ControlResponse::CapabilitiesAccepted(local_capabilities)
        );
    }

    #[test]
    fn capability_response_rejects_unconfigured_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let unconfigured = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: configured.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                unconfigured,
                &ControlCapabilities::local("lab", None, 1280),
                &ControlCapabilities::local("lab", None, 1280),
                &[],
            ),
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::UnauthorizedPeer)
        );
    }

    #[test]
    fn capability_response_rejects_incompatible_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.packet_protocol = "/other/packet/1".to_owned();

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &capabilities,
                &ControlCapabilities::local("lab", None, 1280),
                &[],
            ),
            ControlResponse::CapabilitiesRejected(
                ControlRejectionReason::UnsupportedPacketProtocol
            )
        );
    }

    #[tokio::test]
    async fn drain_outbound_queue_waits_for_validated_peer_capabilities() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(4, 4096);
        forwarder
            .enqueue_tun_packet(
                &mut queues,
                ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay)),
            )
            .expect("queued");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        let peer_capabilities = PeerCapabilities::default();
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 0);
        assert_eq!(snapshot.outbound_dropped_packets, 0);
        assert_eq!(snapshot.outbound_queue_blocked_no_supported_path_events, 1);
        assert_eq!(snapshot.queue.queued_packets, 1);
    }

    #[tokio::test]
    async fn drain_outbound_queue_respects_peer_capability_mtu() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(4, 4096);
        forwarder
            .enqueue_tun_packet(
                &mut queues,
                ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay)),
            )
            .expect("queued");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(remote_overlay, ControlCapabilities::local("lab", None, 19));
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 0);
        assert_eq!(snapshot.outbound_stream_fallback_packets, 0);
        assert_eq!(snapshot.outbound_dropped_packets, 1);
        assert_eq!(snapshot.outbound_drop_packet_too_large_packets, 1);
        assert_eq!(snapshot.outbound_packet_too_big_no_writer, 1);
        assert_eq!(snapshot.outbound_path_mtu_updates, 1);
        assert_eq!(snapshot.queue.queued_packets, 0);
        assert_eq!(
            paths
                .best_for(remote_overlay)
                .expect("selected path")
                .estimated_mtu,
            Some(19)
        );
    }

    #[tokio::test]
    async fn drain_outbound_queue_respects_selected_path_mtu() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(4, 4096);
        forwarder
            .enqueue_tun_packet(
                &mut queues,
                ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay)),
            )
            .expect("queued");
        let mut paths = PathSet::new();
        paths.record_established_with_mtu(remote_overlay, PathKind::DirectTcpStream, Some(19));
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 0);
        assert_eq!(snapshot.outbound_stream_fallback_packets, 0);
        assert_eq!(snapshot.outbound_dropped_packets, 1);
        assert_eq!(snapshot.outbound_drop_packet_too_large_packets, 1);
        assert_eq!(snapshot.outbound_packet_too_big_no_writer, 1);
        assert_eq!(snapshot.queue.queued_packets, 0);
    }

    #[tokio::test]
    async fn drain_outbound_queue_respects_packet_in_flight_window() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(4, 4096);
        for _ in 0..2 {
            forwarder
                .enqueue_tun_packet(
                    &mut queues,
                    ipv4_tcp_packet(
                        builtin_ipv4(local_overlay),
                        builtin_ipv4(remote_overlay),
                        10_000,
                        443,
                    ),
                )
                .expect("queued");
        }
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(1);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 1);
        assert_eq!(snapshot.outbound_stream_fallback_packets, 1);
        assert_eq!(snapshot.outbound_queue_blocked_no_supported_path_events, 0);
        assert_eq!(snapshot.outbound_queue_blocked_packet_window_events, 1);
        assert_eq!(snapshot.queue.queued_packets, 1);
        assert_eq!(packet_in_flight.in_flight_for(remote_overlay), 1);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn drain_outbound_queue_gates_stream_fallback_by_flow_shard() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let local_ipv4 = builtin_ipv4(local_overlay);
        let remote_ipv4 = builtin_ipv4(remote_overlay);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 8192,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let first_flow = ipv4_tcp_packet(local_ipv4, remote_ipv4, 10_000, 443);
        let mut different_flow = None;
        for port in 10_001..=u16::MAX {
            let candidate = ipv4_tcp_packet(local_ipv4, remote_ipv4, port, 443);
            let first_packet = crate::queue::Packet::new(remote_overlay, 0, first_flow.clone());
            let candidate_packet = crate::queue::Packet::new(remote_overlay, 1, candidate.clone());
            if first_packet.flow_shard() != candidate_packet.flow_shard() {
                different_flow = Some(candidate);
                break;
            }
        }
        let different_flow = different_flow.expect("test should find a different flow shard");
        let mut queues = PeerQueues::new(8, 8192);
        forwarder
            .enqueue_tun_packet(&mut queues, first_flow.clone())
            .expect("first flow packet");
        forwarder
            .enqueue_tun_packet(&mut queues, first_flow)
            .expect("same flow packet");
        forwarder
            .enqueue_tun_packet(&mut queues, different_flow)
            .expect("different flow packet");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 2);
        assert_eq!(snapshot.outbound_stream_fallback_packets, 2);
        assert_eq!(snapshot.outbound_queue_blocked_packet_window_events, 1);
        assert_eq!(snapshot.queue.queued_packets, 1);
        assert_eq!(packet_in_flight.in_flight_for(remote_overlay), 2);
        assert_eq!(
            packet_in_flight.stats(),
            PacketInFlightStats {
                packets: 2,
                peers: 1,
                shards: 2,
                limit_per_peer: 256
            }
        );
    }

    #[tokio::test]
    async fn stream_fallback_allows_unordered_packets_while_in_flight() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = config_with_peer(&local_identity, remote);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(4, 4096);
        for _ in 0..2 {
            forwarder
                .enqueue_tun_packet(
                    &mut queues,
                    ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay)),
                )
                .expect("queued");
        }
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 2);
        assert_eq!(snapshot.outbound_stream_fallback_packets, 2);
        assert_eq!(snapshot.outbound_queue_blocked_packet_window_events, 0);
        assert_eq!(snapshot.queue.queued_packets, 0);
        assert_eq!(packet_in_flight.in_flight_for(remote_overlay), 2);
    }

    #[tokio::test]
    async fn drain_outbound_queue_waits_when_only_datagram_path_is_unsupported() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(4, 4096);
        forwarder
            .enqueue_tun_packet(
                &mut queues,
                ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay)),
            )
            .expect("queued");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectUdpDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities = capabilities.with_owned_udp_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 0);
        assert_eq!(snapshot.outbound_dropped_packets, 0);
        assert_eq!(snapshot.outbound_quic_datagram_unavailable_packets, 1);
        assert_eq!(snapshot.outbound_queue_blocked_no_supported_path_events, 1);
        assert_eq!(snapshot.queue.queued_packets, 1);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn drain_outbound_queue_prefers_established_packet_plane_datagram_path() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote libp2p peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut node = build_node(&HostConfig {
            identity: local_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut sender_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("sender socket")])
                .await
                .expect("sender packet plane");
        let mut receiver_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("receiver socket")])
                .await
                .expect("receiver packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_sessions(
            &mut sender_packet_plane,
            &mut receiver_packet_plane,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1280,
        );
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay));
        let mut queues = PeerQueues::new(4, 4096);
        forwarder
            .enqueue_tun_packet(&mut queues, packet.clone())
            .expect("queued");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectUdpDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities = capabilities.with_owned_udp_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );
        context.packet_plane = Some(&sender_packet_plane);

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;
        let inbound = timeout(
            TokioDuration::from_secs(1),
            receiver_packet_plane.recv_frame_from_peer(local_overlay),
        )
        .await
        .expect("packet-plane receive should not time out")
        .expect("packet-plane receive");

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 1);
        assert_eq!(snapshot.outbound_quic_datagram_packets, 1);
        assert_eq!(snapshot.outbound_stream_fallback_packets, 0);
        assert_eq!(snapshot.outbound_dropped_packets, 0);
        assert_eq!(snapshot.queue.queued_packets, 0);
        assert_eq!(packet_in_flight.in_flight_for(remote_overlay), 0);
        assert_eq!(inbound.peer, Some(local_overlay));
        assert_eq!(inbound.frame.payload, packet);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn drain_outbound_queue_prefers_established_packet_plane_quic_datagram_path() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote libp2p peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = config_with_peer(&local_identity, remote);
        let mut node = build_node(&HostConfig {
            identity: local_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut sender_packet_plane =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("sender socket"))
                .expect("sender packet plane");
        let mut receiver_packet_plane =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("receiver socket"))
                .expect("receiver packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_quic_sessions(
            &mut sender_packet_plane,
            &mut receiver_packet_plane,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1280,
        )
        .await;
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay));
        let mut queues = PeerQueues::new(4, 4096);
        forwarder
            .enqueue_tun_packet(&mut queues, packet.clone())
            .expect("queued");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectUdpDatagram);
        paths.record_established(remote_overlay, PathKind::DirectQuicDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities = capabilities.with_owned_quic_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );
        context.packet_plane_quic = Some(&sender_packet_plane);

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;
        let inbound = timeout(
            TokioDuration::from_secs(1),
            receiver_packet_plane.recv_frame_from_peer(local_overlay),
        )
        .await
        .expect("packet-plane QUIC receive should not time out")
        .expect("packet-plane QUIC receive");

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 1);
        assert_eq!(snapshot.outbound_quic_datagram_packets, 1);
        assert_eq!(snapshot.outbound_stream_fallback_packets, 0);
        assert_eq!(snapshot.outbound_dropped_packets, 0);
        assert_eq!(snapshot.queue.queued_packets, 0);
        assert_eq!(packet_in_flight.in_flight_for(remote_overlay), 0);
        assert_eq!(inbound.peer, Some(local_overlay));
        assert_eq!(inbound.frame.payload, packet);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn quic_send_failure_falls_back_to_udp_packet_plane_when_session_exists() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote libp2p peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = config_with_peer(&local_identity, remote);
        let mut node = build_node(&HostConfig {
            identity: local_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut sender_udp =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("sender udp")])
                .await
                .expect("sender udp packet plane");
        let mut receiver_udp =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("receiver udp")])
                .await
                .expect("receiver udp packet plane");
        let mut sender_quic =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("sender quic"))
                .expect("sender quic packet plane");
        let mut receiver_quic =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("receiver quic"))
                .expect("receiver quic packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_sessions(
            &mut sender_udp,
            &mut receiver_udp,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1280,
        );
        establish_test_packet_plane_quic_sessions(
            &mut sender_quic,
            &mut receiver_quic,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1280,
        )
        .await;
        assert!(sender_quic.forget_connection(remote_overlay));
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay));
        let mut queues = PeerQueues::new(4, 4096);
        forwarder
            .enqueue_tun_packet(&mut queues, packet.clone())
            .expect("queued");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectQuicDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        let capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_owned_udp_packet_plane(true)
            .with_owned_quic_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );
        context.packet_plane = Some(&sender_udp);
        context.packet_plane_quic = Some(&sender_quic);

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;
        let inbound = timeout(
            TokioDuration::from_secs(1),
            receiver_udp.recv_frame_from_peer(local_overlay),
        )
        .await
        .expect("UDP fallback receive should not time out")
        .expect("UDP fallback receive");

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 1);
        assert_eq!(snapshot.outbound_quic_datagram_packets, 1);
        assert_eq!(snapshot.outbound_stream_fallback_packets, 0);
        assert_eq!(snapshot.outbound_dropped_packets, 0);
        assert_eq!(snapshot.packet_plane_path_demotions, 0);
        assert_eq!(snapshot.queue.queued_packets, 0);
        assert_eq!(
            paths.best_for(remote_overlay).expect("selected path").kind,
            PathKind::DirectQuicDatagram
        );
        assert_eq!(inbound.peer, Some(local_overlay));
        assert_eq!(inbound.frame.payload, packet);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn quic_send_failure_falls_back_to_stream_when_no_udp_session_exists() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote libp2p peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = config_with_peer(&local_identity, remote);
        let mut node = build_node(&HostConfig {
            identity: local_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut sender_quic =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("sender quic"))
                .expect("sender quic packet plane");
        let mut receiver_quic =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("receiver quic"))
                .expect("receiver quic packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_quic_sessions(
            &mut sender_quic,
            &mut receiver_quic,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1280,
        )
        .await;
        assert!(sender_quic.forget_connection(remote_overlay));
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay));
        let mut queues = PeerQueues::new(4, 4096);
        forwarder
            .enqueue_tun_packet(&mut queues, packet)
            .expect("queued");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        paths.record_established(remote_overlay, PathKind::DirectQuicDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        let capabilities =
            ControlCapabilities::local("lab", None, 1280).with_owned_quic_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);
        let metrics = RuntimeMetrics::default();
        let mut packet_in_flight = PacketInFlight::new(256);
        let mut context = queue_drain_context(
            &mut paths,
            &peer_capabilities,
            &mut packet_in_flight,
            &metrics,
        );
        context.packet_plane_quic = Some(&sender_quic);

        drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &mut context).await;

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 1);
        assert_eq!(snapshot.outbound_quic_datagram_packets, 0);
        assert_eq!(snapshot.outbound_stream_fallback_packets, 1);
        assert_eq!(snapshot.outbound_dropped_packets, 0);
        assert_eq!(snapshot.packet_plane_path_demotions, 0);
        assert_eq!(snapshot.queue.queued_packets, 0);
        assert_eq!(packet_in_flight.in_flight_for(remote_overlay), 1);
        assert_eq!(
            paths.best_for(remote_overlay).expect("selected path").kind,
            PathKind::DirectQuicDatagram
        );
    }

    #[tokio::test]
    async fn selected_path_mtu_respects_packet_plane_session_mtu() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote libp2p peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut sender_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("sender socket")])
                .await
                .expect("sender packet plane");
        let mut receiver_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("receiver socket")])
                .await
                .expect("receiver packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_sessions(
            &mut sender_packet_plane,
            &mut receiver_packet_plane,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1_000,
        );
        let mut paths = PathSet::new();
        paths.record_established_with_mtu(remote_overlay, PathKind::DirectUdpDatagram, Some(1_200));
        let mut peer_capabilities = PeerCapabilities::default();
        let mut capabilities = ControlCapabilities::local("lab", None, 1_280);
        capabilities = capabilities.with_owned_udp_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);

        assert_eq!(
            selected_path_mtu(
                &paths,
                &peer_capabilities,
                Some(&sender_packet_plane),
                None,
                remote_overlay,
                1_280,
            ),
            1_000
        );
    }

    #[tokio::test]
    async fn path_probes_use_packet_plane_for_datagram_paths() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote libp2p peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_with_peer(&local_identity, remote);
        let mut node = build_node(&HostConfig {
            identity: local_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut sender_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("sender socket")])
                .await
                .expect("sender packet plane");
        let mut receiver_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("receiver socket")])
                .await
                .expect("receiver packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_sessions(
            &mut sender_packet_plane,
            &mut receiver_packet_plane,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1_000,
        );
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut paths = PathSet::new();
        paths.record_established_with_mtu(remote_overlay, PathKind::DirectUdpDatagram, Some(1_200));
        let mut peer_capabilities = PeerCapabilities::default();
        let mut capabilities = ControlCapabilities::local("lab", None, 1_280);
        capabilities = capabilities.with_owned_udp_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);
        let metrics = RuntimeMetrics::default();
        let mut path_probe_tracker = PathProbeTracker::default();

        send_path_probes(
            &mut node.swarm,
            &mut forwarder,
            &paths,
            &peer_capabilities,
            Some(&sender_packet_plane),
            None,
            &mut path_probe_tracker,
            &metrics,
        )
        .await;

        let inbound = timeout(
            TokioDuration::from_secs(1),
            receiver_packet_plane.recv_frame_from_peer(PeerId::from_libp2p(
                local_identity
                    .peer_id
                    .parse::<Libp2pPeerId>()
                    .expect("local libp2p peer"),
            )),
        )
        .await
        .expect("packet-plane probe receive should not time out")
        .expect("packet-plane probe receive");
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());

        assert_eq!(snapshot.outbound_path_probes_sent, 1);
        assert_eq!(snapshot.outbound_path_probe_failures, 0);
        assert_eq!(
            inbound.peer,
            Some(PeerId::from_libp2p(
                local_identity
                    .peer_id
                    .parse::<Libp2pPeerId>()
                    .expect("local libp2p peer")
            ))
        );
        assert_eq!(inbound.frame.header.payload_type, PayloadType::PathProbe);
        assert_eq!(inbound.frame.payload.len(), 1_000);
        assert!(inbound.frame.payload.starts_with(PATH_PROBE_PAYLOAD));
        assert!(path_probe_request_token(&inbound.frame.payload).is_some());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn packet_plane_path_probe_ack_raises_selected_path_mtu() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let local = local_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("local libp2p peer");
        let remote = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote libp2p peer");
        let local_overlay = PeerId::from_libp2p(local);
        let remote_overlay = PeerId::from_libp2p(remote);
        let sender_config = config_with_peer(&local_identity, remote);
        let receiver_config = config_with_peer(&remote_identity, local);
        let mut node = build_node(&HostConfig {
            identity: local_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut sender_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("sender socket")])
                .await
                .expect("sender packet plane");
        let mut receiver_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("receiver socket")])
                .await
                .expect("receiver packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_sessions(
            &mut sender_packet_plane,
            &mut receiver_packet_plane,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1_280,
        );
        let mut sender_forwarder = Forwarder::from_config(&sender_config).expect("sender");
        let mut receiver_forwarder = Forwarder::from_config(&receiver_config).expect("receiver");
        let mut sender_paths = PathSet::new();
        sender_paths.record_established_with_mtu(
            remote_overlay,
            PathKind::DirectUdpDatagram,
            Some(1_000),
        );
        let mut receiver_paths = PathSet::new();
        receiver_paths.record_established_with_mtu(
            local_overlay,
            PathKind::DirectUdpDatagram,
            Some(1_280),
        );
        let mut sender_capabilities = PeerCapabilities::default();
        let mut remote_capabilities = ControlCapabilities::local("lab", None, 1_280);
        remote_capabilities = remote_capabilities.with_owned_udp_packet_plane(true);
        sender_capabilities.record(remote_overlay, remote_capabilities);
        let mut receiver_capabilities = PeerCapabilities::default();
        let mut local_capabilities = ControlCapabilities::local("lab", None, 1_280);
        local_capabilities = local_capabilities.with_owned_udp_packet_plane(true);
        receiver_capabilities.record(local_overlay, local_capabilities);
        let metrics = RuntimeMetrics::default();
        let mut sender_path_probe_tracker = PathProbeTracker::default();
        let mut receiver_path_probe_tracker = PathProbeTracker::default();

        send_path_probes(
            &mut node.swarm,
            &mut sender_forwarder,
            &sender_paths,
            &sender_capabilities,
            Some(&sender_packet_plane),
            None,
            &mut sender_path_probe_tracker,
            &metrics,
        )
        .await;

        let probe = timeout(
            TokioDuration::from_secs(1),
            receiver_packet_plane.recv_frame_from_peer(local_overlay),
        )
        .await
        .expect("probe receive should not time out")
        .expect("probe receive");
        assert_eq!(probe.frame.header.payload_type, PayloadType::PathProbe);
        assert_eq!(probe.frame.payload.len(), 1_064);
        assert_eq!(path_probe_request_mtu(&probe.frame.payload), Some(1_064));

        handle_packet_plane_path_probe(
            &mut receiver_forwarder,
            &mut receiver_paths,
            &receiver_capabilities,
            Some(&receiver_packet_plane),
            None,
            PacketDatagramBackend::OwnedUdp,
            &mut receiver_path_probe_tracker,
            &metrics,
            local_overlay,
            &probe,
        )
        .await;
        let ack = timeout(
            TokioDuration::from_secs(1),
            sender_packet_plane.recv_frame_from_peer(remote_overlay),
        )
        .await
        .expect("ack receive should not time out")
        .expect("ack receive");
        assert_eq!(path_probe_ack_mtu(&ack.frame.payload), Some(1_064));

        handle_packet_plane_path_probe(
            &mut sender_forwarder,
            &mut sender_paths,
            &sender_capabilities,
            Some(&sender_packet_plane),
            None,
            PacketDatagramBackend::OwnedUdp,
            &mut sender_path_probe_tracker,
            &metrics,
            remote_overlay,
            &ack,
        )
        .await;

        assert_eq!(
            sender_paths.path_mtu(remote_overlay, PathKind::DirectUdpDatagram),
            Some(1_064)
        );
        assert!(
            sender_paths
                .path_rtt(remote_overlay, PathKind::DirectUdpDatagram)
                .is_some()
        );
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outbound_path_probe_acks_sent, 1);
        assert_eq!(snapshot.outbound_path_mtu_updates, 1);
        assert_eq!(snapshot.outbound_path_mtu_probe_confirmations, 1);
    }

    #[tokio::test]
    async fn packet_plane_path_probe_ack_is_capped_by_negotiated_mtu() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote libp2p peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_with_peer(&local_identity, remote);
        let mut sender_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("sender socket")])
                .await
                .expect("sender packet plane");
        let mut receiver_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("receiver socket")])
                .await
                .expect("receiver packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_sessions(
            &mut sender_packet_plane,
            &mut receiver_packet_plane,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1_200,
        );
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut paths = PathSet::new();
        paths.record_established_with_mtu(remote_overlay, PathKind::DirectUdpDatagram, Some(1_000));
        let mut peer_capabilities = PeerCapabilities::default();
        let mut capabilities = ControlCapabilities::local("lab", None, 1_280);
        capabilities = capabilities.with_owned_udp_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);
        let ack = PacketPlaneReceivedFrame {
            frame: Frame::path_probe(7, 42, path_probe_ack_payload(1_400, None))
                .expect("ack frame"),
            peer: Some(remote_overlay),
            remote_addr: receiver_packet_plane
                .primary_listener()
                .expect("receiver listener"),
            local_addr: sender_packet_plane
                .primary_listener()
                .expect("sender listener"),
        };
        let metrics = RuntimeMetrics::default();
        let mut path_probe_tracker = PathProbeTracker::default();

        handle_packet_plane_path_probe(
            &mut forwarder,
            &mut paths,
            &peer_capabilities,
            Some(&sender_packet_plane),
            None,
            PacketDatagramBackend::OwnedUdp,
            &mut path_probe_tracker,
            &metrics,
            remote_overlay,
            &ack,
        )
        .await;

        assert_eq!(
            paths.path_mtu(remote_overlay, PathKind::DirectUdpDatagram),
            Some(1_200)
        );
    }

    #[test]
    fn unconfirmed_packet_plane_path_probe_demotes_direct_datagram_to_relay() {
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::CircuitRelay);
        paths.record_established(remote_overlay, PathKind::DirectUdpDatagram);
        let metrics = RuntimeMetrics::default();
        let mut path_probe_tracker = PathProbeTracker::default();
        let start = Instant::now();

        path_probe_tracker.record(remote_overlay, PathKind::DirectUdpDatagram, 7, start);
        assert!(
            path_probe_tracker
                .expire_unconfirmed(start + PATH_PROBE_TIMEOUT, PATH_PROBE_TIMEOUT)
                .is_empty()
        );
        assert_eq!(
            paths
                .best_for(remote_overlay)
                .map(|candidate| candidate.kind),
            Some(PathKind::DirectUdpDatagram)
        );

        let expired = path_probe_tracker.expire_unconfirmed(
            start + PATH_PROBE_TIMEOUT + Duration::from_millis(1),
            PATH_PROBE_TIMEOUT,
        );
        assert_eq!(expired.len(), 1);
        metrics.record_outbound_path_probe_failure();
        assert!(demote_packet_plane_path_probe_timeout(
            &mut paths,
            &metrics,
            expired[0].peer,
            expired[0].path,
        ));

        assert_eq!(
            paths
                .best_for(remote_overlay)
                .map(|candidate| candidate.kind),
            Some(PathKind::CircuitRelay)
        );
        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outbound_path_probe_failures, 1);
        assert_eq!(snapshot.packet_plane_path_demotions, 1);
        assert_eq!(snapshot.path_fallbacks_to_relay, 1);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn packet_plane_control_negotiation_establishes_sessions() {
        let initiator_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("initiator identity");
        let responder_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("responder identity");
        let initiator_peer = initiator_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("initiator libp2p peer");
        let responder_peer = responder_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("responder libp2p peer");
        let initiator_overlay = PeerId::from_libp2p(initiator_peer);
        let responder_overlay = PeerId::from_libp2p(responder_peer);
        let initiator_forwarder =
            Forwarder::from_config(&config_with_peer(&initiator_identity, responder_peer))
                .expect("initiator forwarder");
        let responder_forwarder =
            Forwarder::from_config(&config_with_peer(&responder_identity, initiator_peer))
                .expect("responder forwarder");
        let mut initiator_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("initiator socket")])
                .await
                .expect("initiator packet plane");
        let mut responder_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("responder socket")])
                .await
                .expect("responder packet plane");
        let mut initiator_capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_packet_endpoint_candidates(vec![
                initiator_packet_plane
                    .primary_listener()
                    .expect("initiator listener")
                    .to_string(),
            ]);
        initiator_capabilities = initiator_capabilities.with_owned_udp_packet_plane(true);
        let mut responder_capabilities = ControlCapabilities::local("lab", None, 1200)
            .with_packet_endpoint_candidates(vec![
                responder_packet_plane
                    .primary_listener()
                    .expect("responder listener")
                    .to_string(),
            ]);
        responder_capabilities = responder_capabilities.with_owned_udp_packet_plane(true);
        let mut responder_peer_capabilities = PeerCapabilities::default();
        responder_peer_capabilities.record(initiator_overlay, initiator_capabilities.clone());
        let mut initiator_peer_capabilities = PeerCapabilities::default();
        initiator_peer_capabilities.record(responder_overlay, responder_capabilities.clone());
        let mut responder_paths = PathSet::new();
        let mut initiator_paths = PathSet::new();
        let metrics = RuntimeMetrics::default();
        let mut negotiator = PacketPlaneNegotiator::default();
        let (secret, hello, verified_hello) = signed_packet_plane_handshake(
            PacketPlaneHandshakeKind::Hello,
            &initiator_identity,
            &initiator_capabilities,
            PacketDatagramBackend::OwnedUdp,
        )
        .expect("signed hello");
        negotiator.insert(
            responder_overlay,
            secret,
            verified_hello,
            PacketDatagramBackend::OwnedUdp,
        );
        let encoded_hello = hello.encode().expect("encoded hello");

        let rejected_response = packet_plane_accept_response_for_peer(
            PacketPlaneAcceptContext {
                forwarder: &responder_forwarder,
                peer_capabilities: &responder_peer_capabilities,
                paths: &mut responder_paths,
                metrics: &metrics,
                packet_plane: &mut responder_packet_plane,
                packet_plane_quic: None,
                identity: &responder_identity,
                local_capabilities: &responder_capabilities,
            },
            initiator_peer,
            &encoded_hello,
        )
        .await;
        assert_eq!(
            rejected_response,
            ControlResponse::PacketPlaneRejected(ControlRejectionReason::UnsupportedPreferredPath)
        );

        responder_paths.record_established(initiator_overlay, PathKind::DirectTcpStream);
        initiator_paths.record_established(responder_overlay, PathKind::DirectTcpStream);
        let response = packet_plane_accept_response_for_peer(
            PacketPlaneAcceptContext {
                forwarder: &responder_forwarder,
                peer_capabilities: &responder_peer_capabilities,
                paths: &mut responder_paths,
                metrics: &metrics,
                packet_plane: &mut responder_packet_plane,
                packet_plane_quic: None,
                identity: &responder_identity,
                local_capabilities: &responder_capabilities,
            },
            initiator_peer,
            &encoded_hello,
        )
        .await;
        let ControlResponse::PacketPlaneAccepted(encoded_accept) = response else {
            panic!("expected packet-plane accept, got {response:?}");
        };
        complete_packet_plane_hello(
            &mut PacketPlaneCompleteContext {
                forwarder: &initiator_forwarder,
                peer_capabilities: &initiator_peer_capabilities,
                packet_plane: &mut initiator_packet_plane,
                packet_plane_quic: None,
                negotiator: &mut negotiator,
                paths: &mut initiator_paths,
                metrics: &metrics,
                network_name: "lab",
            },
            responder_peer,
            &encoded_accept,
        )
        .expect("complete hello");

        let initiator_session = initiator_packet_plane
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.peer == responder_overlay)
            .expect("initiator session");
        let responder_session = responder_packet_plane
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.peer == initiator_overlay)
            .expect("responder session");
        assert_eq!(initiator_session.role, PacketPlaneSessionRole::Initiator);
        assert_eq!(responder_session.role, PacketPlaneSessionRole::Responder);
        assert_eq!(
            initiator_paths
                .best_for(responder_overlay)
                .expect("initiator path")
                .kind,
            PathKind::DirectUdpDatagram
        );
        assert_eq!(
            responder_paths
                .best_for(initiator_overlay)
                .expect("responder path")
                .kind,
            PathKind::DirectUdpDatagram
        );
        assert_eq!(initiator_session.mtu, 1200);
        assert_eq!(responder_session.mtu, 1200);
        assert_eq!(
            initiator_session.endpoint,
            responder_packet_plane
                .primary_listener()
                .expect("responder")
        );
        assert_eq!(
            responder_session.endpoint,
            initiator_packet_plane
                .primary_listener()
                .expect("initiator")
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn packet_plane_control_negotiation_establishes_quic_sessions() {
        let initiator_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("initiator identity");
        let responder_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("responder identity");
        let initiator_peer = initiator_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("initiator peer");
        let responder_peer = responder_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("responder peer");
        let initiator_overlay = PeerId::from_libp2p(initiator_peer);
        let responder_overlay = PeerId::from_libp2p(responder_peer);
        let initiator_forwarder =
            Forwarder::from_config(&config_with_peer(&initiator_identity, responder_peer))
                .expect("initiator forwarder");
        let responder_forwarder =
            Forwarder::from_config(&config_with_peer(&responder_identity, initiator_peer))
                .expect("responder forwarder");
        let mut initiator_packet_plane = PacketPlaneRuntime::disabled();
        let mut responder_packet_plane = PacketPlaneRuntime::disabled();
        let mut initiator_quic =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("initiator quic"))
                .expect("initiator quic");
        let mut responder_quic =
            PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().expect("responder quic"))
                .expect("responder quic");
        let initiator_endpoint = initiator_quic.local_addr();
        let responder_endpoint = responder_quic.local_addr();
        let mut initiator_capabilities = ControlCapabilities::local("lab", None, 1200)
            .with_owned_quic_packet_endpoint_candidates(vec![initiator_endpoint.to_string()])
            .with_owned_quic_packet_plane_certificate(
                initiator_quic.server_certificate().as_ref().to_vec(),
            );
        initiator_capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        let mut responder_capabilities = ControlCapabilities::local("lab", None, 1200)
            .with_owned_quic_packet_endpoint_candidates(vec![responder_endpoint.to_string()])
            .with_owned_quic_packet_plane_certificate(
                responder_quic.server_certificate().as_ref().to_vec(),
            );
        responder_capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        let mut responder_peer_capabilities = PeerCapabilities::default();
        responder_peer_capabilities.record(initiator_overlay, initiator_capabilities.clone());
        let mut initiator_peer_capabilities = PeerCapabilities::default();
        initiator_peer_capabilities.record(responder_overlay, responder_capabilities.clone());
        let mut responder_paths = PathSet::new();
        responder_paths.record_established(initiator_overlay, PathKind::DirectTcpStream);
        let mut initiator_paths = PathSet::new();
        initiator_paths.record_established(responder_overlay, PathKind::DirectTcpStream);
        let metrics = RuntimeMetrics::default();
        let mut negotiator = PacketPlaneNegotiator::default();
        let (secret, hello, verified_hello) = signed_packet_plane_handshake(
            PacketPlaneHandshakeKind::Hello,
            &initiator_identity,
            &initiator_capabilities,
            PacketDatagramBackend::OwnedQuic,
        )
        .expect("signed quic hello");
        negotiator.insert(
            responder_overlay,
            secret,
            verified_hello,
            PacketDatagramBackend::OwnedQuic,
        );
        let encoded_hello = hello.encode().expect("encoded quic hello");
        let responder_certificate = responder_quic.server_certificate();

        let (connect, response) = tokio::join!(
            initiator_quic.connect_peer(
                responder_overlay,
                responder_endpoint,
                responder_certificate,
            ),
            packet_plane_accept_response_for_peer(
                PacketPlaneAcceptContext {
                    forwarder: &responder_forwarder,
                    peer_capabilities: &responder_peer_capabilities,
                    paths: &mut responder_paths,
                    metrics: &metrics,
                    packet_plane: &mut responder_packet_plane,
                    packet_plane_quic: Some(&mut responder_quic),
                    identity: &responder_identity,
                    local_capabilities: &responder_capabilities,
                },
                initiator_peer,
                &encoded_hello,
            )
        );
        connect.expect("initiator quic connection");
        let ControlResponse::PacketPlaneAccepted(encoded_accept) = response else {
            panic!("expected packet-plane quic accept, got {response:?}");
        };
        complete_packet_plane_hello(
            &mut PacketPlaneCompleteContext {
                forwarder: &initiator_forwarder,
                peer_capabilities: &initiator_peer_capabilities,
                packet_plane: &mut initiator_packet_plane,
                packet_plane_quic: Some(&mut initiator_quic),
                negotiator: &mut negotiator,
                paths: &mut initiator_paths,
                metrics: &metrics,
                network_name: "lab",
            },
            responder_peer,
            &encoded_accept,
        )
        .expect("complete quic hello");

        assert!(initiator_quic.has_session(responder_overlay));
        assert!(responder_quic.has_session(initiator_overlay));
        assert_eq!(
            initiator_paths
                .best_for(responder_overlay)
                .map(|candidate| candidate.kind),
            Some(PathKind::DirectQuicDatagram)
        );
        let frame = Frame::packet(11, 7, vec![0x45; 20]).expect("frame");
        initiator_quic
            .send_frame_to_peer(responder_overlay, &frame)
            .expect("send quic frame");
        let received = timeout(
            TokioDuration::from_secs(1),
            responder_quic.recv_frame_from_peer(initiator_overlay),
        )
        .await
        .expect("quic receive should not time out")
        .expect("quic receive");
        assert_eq!(received.frame, frame);
    }

    #[test]
    fn packet_plane_endpoint_selection_prefers_reachable_candidates() {
        let capabilities = ControlCapabilities::local("lab", None, 1_280)
            .with_packet_endpoint_candidates(vec![
                "0.0.0.0:51820".to_owned(),
                "127.0.0.1:51820".to_owned(),
                "10.0.0.7:51820".to_owned(),
                "8.8.8.8:51820".to_owned(),
            ]);

        assert_eq!(
            first_packet_plane_endpoint(&capabilities),
            Some("8.8.8.8:51820".parse().expect("endpoint"))
        );
    }

    #[test]
    fn packet_plane_endpoint_selection_prefers_private_before_loopback_or_wildcard() {
        let capabilities = ControlCapabilities::local("lab", None, 1_280)
            .with_packet_endpoint_candidates(vec![
                "127.0.0.1:51820".to_owned(),
                "0.0.0.0:51820".to_owned(),
                "192.168.1.10:51820".to_owned(),
            ]);

        assert_eq!(
            first_packet_plane_endpoint(&capabilities),
            Some("192.168.1.10:51820".parse().expect("endpoint"))
        );
    }

    #[test]
    fn packet_plane_endpoint_selection_resolves_dns_candidates() {
        let capabilities = ControlCapabilities::local("lab", None, 1_280)
            .with_packet_endpoint_candidates(vec!["localhost:51820".to_owned()]);
        let endpoint = first_packet_plane_endpoint(&capabilities).expect("resolved endpoint");

        assert!(endpoint.ip().is_loopback());
        assert_eq!(endpoint.port(), 51820);
    }

    #[test]
    fn packet_plane_endpoint_advertisement_accepts_resolved_dns_candidate() {
        let capabilities = ControlCapabilities::local("lab", None, 1_280)
            .with_packet_endpoint_candidates(vec!["localhost:51820".to_owned()]);
        let endpoint = first_packet_plane_endpoint(&capabilities).expect("resolved endpoint");

        assert!(endpoint_is_advertised(&capabilities, endpoint));
        assert!(!endpoint_is_advertised(
            &capabilities,
            "127.0.0.1:51821".parse().expect("other endpoint")
        ));
    }

    #[test]
    fn observed_packet_plane_endpoints_use_confirmed_public_ip_with_listener_ports() {
        let external_address: Multiaddr = "/ip4/8.8.8.8/tcp/4001".parse().expect("multiaddr");
        let endpoints = observed_packet_plane_endpoints(
            &external_address,
            Some("0.0.0.0:51820".parse().expect("udp listener")),
            Some("0.0.0.0:51821".parse().expect("quic listener")),
        );

        assert_eq!(
            endpoints,
            ObservedPacketPlaneEndpoints {
                public_address_accepted: true,
                udp: Some("8.8.8.8:51820".to_owned()),
                quic: Some("8.8.8.8:51821".to_owned())
            }
        );
    }

    #[test]
    fn observed_packet_plane_endpoints_accept_private_lan_addresses() {
        let private_address: Multiaddr = "/ip4/192.168.1.10/tcp/4001"
            .parse()
            .expect("private multiaddr");
        let udp_listener = Some("0.0.0.0:51820".parse().expect("udp listener"));

        assert_eq!(
            observed_packet_plane_endpoints(&private_address, udp_listener, None),
            ObservedPacketPlaneEndpoints {
                public_address_accepted: true,
                udp: Some("192.168.1.10:51820".to_owned()),
                quic: None,
            }
        );
    }

    #[test]
    fn observed_packet_plane_endpoints_reject_unusable_or_relayed_external_addresses() {
        let relayed_address: Multiaddr = "/ip4/8.8.8.8/tcp/4001/p2p-circuit"
            .parse()
            .expect("relayed multiaddr");
        let loopback_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001"
            .parse()
            .expect("loopback multiaddr");
        let udp_listener = Some("0.0.0.0:51820".parse().expect("udp listener"));

        assert_eq!(
            observed_packet_plane_endpoints(&loopback_address, udp_listener, None),
            ObservedPacketPlaneEndpoints::default()
        );
        assert_eq!(
            observed_packet_plane_endpoints(&relayed_address, udp_listener, None),
            ObservedPacketPlaneEndpoints::default()
        );
    }

    #[test]
    fn direct_packet_plane_endpoint_uses_concrete_listener_address() {
        let endpoint = ConnectedPoint::Dialer {
            address: "/ip4/192.168.1.20/tcp/4001".parse().expect("address"),
            role_override: Endpoint::Dialer,
            port_use: PortUse::Reuse,
        };

        assert_eq!(
            direct_packet_plane_endpoint_from_path(
                Some("192.168.1.10:51820".parse().expect("listener")),
                &endpoint,
            ),
            Some("192.168.1.10:51820".to_owned())
        );
    }

    #[test]
    fn direct_packet_plane_endpoint_derives_loopback_source_for_wildcard_listener() {
        let endpoint = ConnectedPoint::Dialer {
            address: "/ip4/127.0.0.1/tcp/4001".parse().expect("address"),
            role_override: Endpoint::Dialer,
            port_use: PortUse::Reuse,
        };

        assert_eq!(
            direct_packet_plane_endpoint_from_path(
                Some("0.0.0.0:51820".parse().expect("listener")),
                &endpoint,
            ),
            Some("127.0.0.1:51820".to_owned())
        );
    }

    #[test]
    fn direct_packet_plane_endpoint_rejects_relay_path() {
        let endpoint = ConnectedPoint::Dialer {
            address: "/ip4/8.8.8.8/tcp/4001/p2p-circuit"
                .parse()
                .expect("address"),
            role_override: Endpoint::Dialer,
            port_use: PortUse::Reuse,
        };

        assert_eq!(
            direct_packet_plane_endpoint_from_path(
                Some("0.0.0.0:51820".parse().expect("listener")),
                &endpoint,
            ),
            None
        );
    }

    #[test]
    fn direct_packet_plane_endpoint_replaces_stale_candidate_for_listener_port() {
        let mut candidates = vec![
            "172.20.10.2:51820".to_owned(),
            "192.168.1.50:51821".to_owned(),
        ];

        replace_packet_plane_endpoint_for_listener(&mut candidates, "192.168.1.10:51820");

        assert_eq!(
            candidates,
            vec![
                "192.168.1.10:51820".to_owned(),
                "192.168.1.50:51821".to_owned(),
            ]
        );
    }

    #[test]
    fn direct_packet_plane_endpoint_moves_existing_candidate_to_front() {
        let mut candidates = vec![
            "172.20.10.2:51820".to_owned(),
            "192.168.1.10:51820".to_owned(),
            "192.168.1.50:51821".to_owned(),
        ];

        replace_packet_plane_endpoint_for_listener(&mut candidates, "192.168.1.10:51820");

        assert_eq!(
            candidates,
            vec![
                "192.168.1.10:51820".to_owned(),
                "192.168.1.50:51821".to_owned(),
            ]
        );
    }

    #[test]
    fn observed_packet_plane_endpoint_update_is_deduplicated_and_certificate_gated() {
        let external_address: Multiaddr = "/ip4/8.8.8.8/tcp/4001".parse().expect("multiaddr");
        let mut capabilities = ControlCapabilities::local("lab", None, 1_280)
            .with_packet_endpoint_candidates(vec!["8.8.8.8:51820".to_owned()]);
        capabilities = capabilities.with_owned_udp_packet_plane(true);

        assert_eq!(
            update_observed_packet_plane_endpoints(
                &mut capabilities,
                &external_address,
                Some("0.0.0.0:51820".parse().expect("udp listener")),
                Some("0.0.0.0:51821".parse().expect("quic listener")),
                None,
            ),
            ObservedPacketPlaneEndpointUpdate {
                public_address_accepted: true,
                udp_candidate_added: false,
                quic_candidate_added: false,
            }
        );
        assert_eq!(
            capabilities.packet_endpoint_candidates,
            vec!["8.8.8.8:51820".to_owned()]
        );
        assert!(
            capabilities
                .owned_quic_packet_endpoint_candidates
                .is_empty()
        );
        assert!(!capabilities.supports_owned_quic_packet_plane);

        assert_eq!(
            update_observed_packet_plane_endpoints(
                &mut capabilities,
                &external_address,
                Some("0.0.0.0:51820".parse().expect("udp listener")),
                Some("0.0.0.0:51821".parse().expect("quic listener")),
                Some(vec![0x30, 0x01, 0x02]),
            ),
            ObservedPacketPlaneEndpointUpdate {
                public_address_accepted: true,
                udp_candidate_added: false,
                quic_candidate_added: true,
            }
        );
        assert_eq!(
            capabilities.packet_endpoint_candidates,
            vec!["8.8.8.8:51820".to_owned()]
        );
        assert_eq!(
            capabilities.owned_quic_packet_endpoint_candidates,
            vec!["8.8.8.8:51821".to_owned()]
        );
        assert!(capabilities.supports_owned_quic_packet_plane);
        assert!(capabilities.supports_quic_datagrams);

        assert_eq!(
            update_observed_packet_plane_endpoints(
                &mut capabilities,
                &external_address,
                Some("0.0.0.0:51820".parse().expect("udp listener")),
                Some("0.0.0.0:51821".parse().expect("quic listener")),
                Some(vec![0x30, 0x01, 0x02]),
            ),
            ObservedPacketPlaneEndpointUpdate {
                public_address_accepted: true,
                udp_candidate_added: false,
                quic_candidate_added: false,
            }
        );
    }

    #[test]
    fn packet_transport_decision_uses_stream_fallback_for_stream_paths() {
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectQuicStream);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );

        assert_eq!(
            packet_transport_decision(&paths, &peer_capabilities, None, None, remote_overlay),
            PacketTransportDecision::StreamFallback {
                path: PathKind::DirectQuicStream
            }
        );
    }

    #[test]
    fn packet_transport_decision_blocks_datagram_only_paths_until_local_support_exists() {
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectUdpDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities = capabilities.with_owned_udp_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);

        assert_eq!(
            packet_transport_decision(&paths, &peer_capabilities, None, None, remote_overlay),
            PacketTransportDecision::Blocked {
                reason: PacketTransportBlockReason::LocalQuicDatagramsUnavailable,
                best_path: Some(PathKind::DirectUdpDatagram)
            }
        );
    }

    #[test]
    fn packet_transport_decision_blocks_native_only_datagram_claim_without_local_handle() {
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectQuicDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280).with_native_quic_datagrams(true),
        );

        assert_eq!(
            local_packet_datagram_backend(&peer_capabilities, None, None, remote_overlay),
            None
        );
        assert_eq!(
            packet_transport_support(&peer_capabilities, remote_overlay),
            PathTransportSupport {
                udp_datagrams: false,
                quic_datagrams: false
            }
        );
        assert_eq!(
            packet_transport_decision(&paths, &peer_capabilities, None, None, remote_overlay),
            PacketTransportDecision::Blocked {
                reason: PacketTransportBlockReason::LocalQuicDatagramsUnavailable,
                best_path: Some(PathKind::DirectQuicDatagram)
            }
        );
    }

    #[tokio::test]
    async fn packet_transport_decision_prefers_stream_until_datagram_path_is_confirmed() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote_identity =
            crate::identity::NodeIdentity::generate_ed25519().expect("remote identity");
        let remote = remote_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("remote libp2p peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut sender_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("sender socket")])
                .await
                .expect("sender packet plane");
        let mut receiver_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("receiver socket")])
                .await
                .expect("receiver packet plane");
        let local_secret = test_packet_plane_secret(7);
        let remote_secret = test_packet_plane_secret(9);
        establish_test_packet_plane_sessions(
            &mut sender_packet_plane,
            &mut receiver_packet_plane,
            &local_identity,
            &remote_identity,
            &local_secret,
            &remote_secret,
            1280,
        );
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        paths.record_established(remote_overlay, PathKind::DirectUdpDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        let capabilities =
            ControlCapabilities::local("lab", None, 1280).with_owned_udp_packet_plane(true);
        peer_capabilities.record(remote_overlay, capabilities);

        assert_eq!(
            packet_transport_decision(
                &paths,
                &peer_capabilities,
                Some(&sender_packet_plane),
                None,
                remote_overlay
            ),
            PacketTransportDecision::StreamFallback {
                path: PathKind::DirectTcpStream
            }
        );

        paths.record_rtt(remote_overlay, PathKind::DirectUdpDatagram, 10);

        assert_eq!(
            packet_transport_decision(
                &paths,
                &peer_capabilities,
                Some(&sender_packet_plane),
                None,
                remote_overlay
            ),
            PacketTransportDecision::PacketPlaneDatagram {
                path: PathKind::DirectUdpDatagram,
                backend: PacketDatagramBackend::OwnedUdp
            }
        );
    }

    #[test]
    fn packet_transport_selection_prefers_established_relay_over_direct_datagram() {
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectUdpDatagram);
        paths.record_rtt(remote_overlay, PathKind::DirectUdpDatagram, 10);
        paths.record_established(remote_overlay, PathKind::CircuitRelay);

        assert_eq!(
            best_packet_transport_path(
                &paths,
                remote_overlay,
                PathTransportSupport {
                    udp_datagrams: true,
                    quic_datagrams: false
                }
            )
            .map(|path| path.kind),
            Some(PathKind::CircuitRelay)
        );
    }

    #[test]
    fn packet_plane_negotiation_waits_for_direct_transport_path() {
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::CircuitRelay);
        assert!(!has_direct_packet_plane_negotiation_path(
            &paths,
            remote_overlay
        ));

        paths.record_established(remote_overlay, PathKind::DirectQuicDatagram);
        assert!(!has_direct_packet_plane_negotiation_path(
            &paths,
            remote_overlay
        ));

        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        assert!(has_direct_packet_plane_negotiation_path(
            &paths,
            remote_overlay
        ));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn direct_path_promotion_retries_packet_plane_negotiation_with_cached_capabilities() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let local_peer = local_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("local peer");
        let remote = loop {
            let candidate = peer_id();
            if PeerId::from_libp2p(local_peer).as_bytes()
                <= PeerId::from_libp2p(candidate).as_bytes()
            {
                break candidate;
            }
        };
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_with_peer(&local_identity, remote);
        let mut node = build_node(&HostConfig {
            identity: local_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let local_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("local socket")])
                .await
                .expect("local packet plane");
        let remote_packet_plane =
            PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("remote socket")])
                .await
                .expect("remote packet plane");
        let mut local_capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_packet_endpoint_candidates(vec![
                local_packet_plane
                    .primary_listener()
                    .expect("local listener")
                    .to_string(),
            ]);
        local_capabilities = local_capabilities.with_owned_udp_packet_plane(true);
        let mut remote_capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_packet_endpoint_candidates(vec![
                remote_packet_plane
                    .primary_listener()
                    .expect("remote listener")
                    .to_string(),
            ]);
        remote_capabilities = remote_capabilities.with_owned_udp_packet_plane(true);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(remote_overlay, remote_capabilities);
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let metrics = RuntimeMetrics::default();
        let mut paths = PathSet::new();
        let mut negotiator = PacketPlaneNegotiator::default();
        let relay_endpoint = ConnectedPoint::Dialer {
            address: format!(
                "/ip4/127.0.0.1/tcp/4001/p2p/{}/p2p-circuit/p2p/{remote}",
                peer_id()
            )
            .parse()
            .expect("relay endpoint"),
            role_override: Endpoint::Dialer,
            port_use: PortUse::Reuse,
        };
        let direct_endpoint = ConnectedPoint::Dialer {
            address: "/ip4/127.0.0.1/tcp/4002".parse().expect("direct endpoint"),
            role_override: Endpoint::Dialer,
            port_use: PortUse::Reuse,
        };

        record_path_established_and_maybe_send_packet_plane_hello(
            &mut node.swarm,
            &mut paths,
            &forwarder,
            &peer_capabilities,
            &metrics,
            &local_capabilities,
            &local_identity,
            &local_packet_plane,
            None,
            &mut negotiator,
            remote,
            &relay_endpoint,
        );
        assert!(!negotiator.has_pending(remote_overlay));

        record_path_established_and_maybe_send_packet_plane_hello(
            &mut node.swarm,
            &mut paths,
            &forwarder,
            &peer_capabilities,
            &metrics,
            &local_capabilities,
            &local_identity,
            &local_packet_plane,
            None,
            &mut negotiator,
            remote,
            &direct_endpoint,
        );

        assert!(negotiator.has_pending(remote_overlay));
        assert_eq!(
            metrics
                .snapshot(crate::queue::QueueStats::default())
                .control_requests_sent,
            1
        );
    }

    #[test]
    fn local_packet_data_plane_is_identity_keyed_stream_fallback_only() {
        let local_data_plane = local_packet_data_plane();
        assert_eq!(
            local_data_plane,
            LocalPacketDataPlane::identity_keyed_streams()
        );
        assert_eq!(
            NATIVE_LIBP2P_QUIC_DATAGRAMS,
            NativeLibp2pQuicDatagramCapability::unavailable(
                "libp2p-quic 0.13.1 disables Quinn datagram receive buffers and Swarm exposes no application datagram handle"
            )
        );
        assert!(!NATIVE_LIBP2P_QUIC_DATAGRAMS.can_advertise());
        assert!(!local_data_plane.native_quic_datagrams);
        assert!(!local_data_plane.owned_udp_packet_plane);
        assert!(!local_data_plane.owned_quic_packet_plane);
    }

    #[tokio::test]
    async fn path_probes_wait_for_capabilities_and_supported_path() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_with_peer(&local_identity, remote);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut paths = PathSet::new();
        let mut peer_capabilities = PeerCapabilities::default();
        let metrics = RuntimeMetrics::default();
        let mut path_probe_tracker = PathProbeTracker::default();

        send_path_probes(
            &mut node.swarm,
            &mut forwarder,
            &paths,
            &peer_capabilities,
            None,
            None,
            &mut path_probe_tracker,
            &metrics,
        )
        .await;
        assert_eq!(
            metrics
                .snapshot(crate::queue::QueueStats::default())
                .outbound_path_probes_sent,
            0
        );

        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );

        send_path_probes(
            &mut node.swarm,
            &mut forwarder,
            &paths,
            &peer_capabilities,
            None,
            None,
            &mut path_probe_tracker,
            &metrics,
        )
        .await;

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outbound_path_probes_sent, 1);
        assert_eq!(snapshot.outbound_path_probe_failures, 0);
    }

    #[tokio::test]
    async fn path_probe_failures_are_counted() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_with_peer(&local_identity, remote);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(remote_overlay, ControlCapabilities::local("lab", None, 4));
        let metrics = RuntimeMetrics::default();
        let mut path_probe_tracker = PathProbeTracker::default();

        send_path_probes(
            &mut node.swarm,
            &mut forwarder,
            &paths,
            &peer_capabilities,
            None,
            None,
            &mut path_probe_tracker,
            &metrics,
        )
        .await;

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outbound_path_probes_sent, 0);
        assert_eq!(snapshot.outbound_path_probe_failures, 1);
    }

    #[test]
    fn runtime_path_stats_report_supported_and_blocked_configured_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let stream_peer = peer_id();
        let datagram_peer = peer_id();
        let stream_overlay = PeerId::from_libp2p(stream_peer);
        let datagram_overlay = PeerId::from_libp2p(datagram_peer);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![
                PeerConfig {
                    id: stream_peer.to_string(),
                    name: None,
                    ip: None,
                    addresses: Vec::new(),
                    routes: Vec::new(),
                },
                PeerConfig {
                    id: datagram_peer.to_string(),
                    name: None,
                    ip: None,
                    addresses: Vec::new(),
                    routes: Vec::new(),
                },
            ],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut paths = PathSet::new();
        paths.record_established(stream_overlay, PathKind::DirectQuicStream);
        paths.record_established(datagram_overlay, PathKind::DirectUdpDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            stream_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );
        let mut datagram_capabilities = ControlCapabilities::local("lab", None, 1280);
        datagram_capabilities = datagram_capabilities.with_owned_udp_packet_plane(true);
        peer_capabilities.record(datagram_overlay, datagram_capabilities);

        let stats = runtime_path_stats(&forwarder, &paths, &peer_capabilities);

        assert_eq!(stats.healthy_direct_udp_datagram_paths, 1);
        assert_eq!(stats.healthy_direct_quic_datagram_paths, 0);
        assert_eq!(stats.healthy_direct_quic_stream_paths, 1);
        assert_eq!(stats.peers_with_supported_path, 1);
        assert_eq!(stats.peers_without_supported_path, 1);
    }

    #[test]
    fn relay_client_events_update_path_metrics() {
        let metrics = RuntimeMetrics::default();
        let relay_peer_id = peer_id();
        let src_peer_id = peer_id();

        record_relay_client_event(
            &metrics,
            &relay::client::Event::ReservationReqAccepted {
                relay_peer_id,
                renewal: false,
                limit: None,
            },
        );
        record_relay_client_event(
            &metrics,
            &relay::client::Event::OutboundCircuitEstablished {
                relay_peer_id,
                limit: None,
            },
        );
        record_relay_client_event(
            &metrics,
            &relay::client::Event::InboundCircuitEstablished {
                src_peer_id,
                limit: None,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.relay_reservations_accepted, 1);
        assert_eq!(snapshot.relay_outbound_circuits_established, 1);
        assert_eq!(snapshot.relay_inbound_circuits_established, 1);
    }

    #[test]
    fn relay_server_events_update_path_metrics() {
        let metrics = RuntimeMetrics::default();
        let src_peer_id = peer_id();
        let dst_peer_id = peer_id();

        handle_relay_server_event(
            &metrics,
            &relay::Event::ReservationReqAccepted {
                src_peer_id,
                renewed: false,
            },
        );
        handle_relay_server_event(
            &metrics,
            &relay::Event::ReservationReqDenied {
                src_peer_id,
                status: relay::StatusCode::ResourceLimitExceeded,
            },
        );
        handle_relay_server_event(&metrics, &relay::Event::ReservationClosed { src_peer_id });
        handle_relay_server_event(&metrics, &relay::Event::ReservationTimedOut { src_peer_id });
        handle_relay_server_event(
            &metrics,
            &relay::Event::CircuitReqDenied {
                src_peer_id,
                dst_peer_id,
                status: relay::StatusCode::NoReservation,
            },
        );
        handle_relay_server_event(
            &metrics,
            &relay::Event::CircuitReqAccepted {
                src_peer_id,
                dst_peer_id,
            },
        );
        handle_relay_server_event(
            &metrics,
            &relay::Event::CircuitClosed {
                src_peer_id,
                dst_peer_id,
                error: None,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.relay_server_reservations_accepted, 1);
        assert_eq!(snapshot.relay_server_reservations_denied, 1);
        assert_eq!(snapshot.relay_server_reservations_closed, 1);
        assert_eq!(snapshot.relay_server_reservations_timed_out, 1);
        assert_eq!(snapshot.relay_server_circuits_accepted, 1);
        assert_eq!(snapshot.relay_server_circuits_denied, 1);
        assert_eq!(snapshot.relay_server_circuits_closed, 1);
    }
}
