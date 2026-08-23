use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, OpenOptions},
    io::{self, Write as _},
    net::{IpAddr, UdpSocket},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use futures::StreamExt as _;
use libp2p::{
    Multiaddr, PeerId as Libp2pPeerId, kad, mdns, multiaddr::Protocol, request_response::Message,
    swarm::SwarmEvent,
};
use p2p_vpn::{
    OVERLAY_FRAGMENTATION_POLICY_LINE, PathKind, PeerId,
    config::{
        AutoRelayConfig, BootstrapPeerConfig, Config, DiscoveryConfig, InitConfigTemplate,
        InitPeer, InterfaceConfig, NetworkConfig, PRIVATE_KADEMLIA_PROTOCOL,
        PUBLIC_IPFS_BOOTSTRAP_PEERS, PUBLIC_IPFS_KADEMLIA_PROTOCOL, PacketPlaneConfig, QueueConfig,
        RelayConfig, RelayResourceConfig, ResourceConfig, RouteConfig, RuntimeDefaults,
        default_auto_relay_max_candidates, default_auto_relay_max_reservations,
        default_auto_relay_retry_interval_seconds, default_listen_addresses,
        default_max_packet_age_millis, default_packet_plane_replay_windows_per_session,
        default_packet_plane_session_ttl_seconds,
    },
    identity::NodeIdentity,
    invite::{
        InviteExportOptions, InviteImportOptions, SignedInvite, export_signed_invite,
        import_invite_config,
    },
    membership::{
        MembershipRecordIssueOptions, MembershipRecordMergeStats, MembershipRecordPayload,
        MembershipRecordSubject, MembershipRole, SignedMembershipRecord,
        issue_membership_record_for_subject_at, validate_membership_records_at,
    },
    metrics::{RuntimeMetrics, prometheus_lines_from_metric_lines},
    pairing::{
        DEFAULT_PAIRING_EXPIRES_IN_SECONDS, PairingConfigOptions, PairingError, PairingOffer,
        PairingOfferOptions, PairingRequestOptions, PairingResponse, build_pairing_request_at,
        export_discovery_only_pairing_offer, export_pairing_offer,
        import_pairing_response_config_at,
    },
    queue::QueueStats,
    route::builtin_ipv4,
    runtime::{
        bootstrap_check::{
            BootstrapCheckRequirements, BootstrapCheckThreshold, PUBLIC_RELAY_CANDIDATE_LIMIT,
            PublicDcutrListenerDescriptor, PublicDcutrReservationEvidence, PublicRelayProbeMode,
            check_config_bootstrap, check_public_dcutr_descriptor, check_public_relay_candidates,
            parse_public_relay_addresses, parse_public_relay_addresses_with_limit,
            scan_public_relay_candidates, start_public_dcutr_listener,
        },
        control_socket::{
            PairRpcCompletionArtifacts, PairRpcError, PairRpcMembershipRole,
            PairRpcOperationStatus, PairRpcOutcome, PairRpcPhase, PairRpcQueryError,
            PairRpcRejectionReason, PairRpcRequest, PairRpcRequestEnvelope,
            PairRpcResponseEnvelope, PairRpcResult, PairRpcRole, PairRpcRoute,
            PairRpcSignedMembershipRecord, query_pair_rpc,
        },
        forward::session_id_for_peer,
        p2p::{BehaviourEvent, HostConfig, build_node},
        packet_plane::{PACKET_PLANE_DATAGRAM_OVERHEAD_LEN, PACKET_PLANE_MAX_PAYLOAD_LEN},
        pairing_sessions::fresh_pairing_operation_id,
        remote::{RemotePeerStatus, query_peer_status},
        runner::{self, ShutdownReason},
        service::SERVICE_PROTOCOL,
        tun::{SysctlCommand, TunAddresses, TunDevice, TunRuntimeConfig, route_advmss},
    },
    wire::{HEADER_LEN, MAX_PAYLOAD_LEN, WIRE_VERSION},
};
use serde::{Deserialize, Serialize};

const RELAY_CHECK_CAPPED_INPUT_LIMIT: usize = 256;

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    Keygen {
        #[arg(short, long, default_value = "-")]
        output: PathBuf,
        #[arg(long)]
        force: bool,
    },
    IdentityPublic {
        #[arg(short, long)]
        config: Option<PathBuf>,
        #[arg(long)]
        private_key: Option<String>,
        #[arg(short, long, default_value = "-")]
        output: PathBuf,
        #[arg(long)]
        force: bool,
    },
    Instance {
        #[command(subcommand)]
        command: InstanceCommand,
    },
    InitConfig {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        output: PathBuf,
        #[arg(long, default_value = "lab")]
        network: String,
        #[arg(long)]
        private_key: Option<String>,
        #[arg(long)]
        membership_key: Option<String>,
        #[arg(long = "previous-membership-tag")]
        previous_membership_tags: Vec<String>,
        #[arg(long, default_value = "pv0")]
        interface: String,
        #[arg(long, default_value_t = 1_280)]
        mtu: u16,
        #[arg(long = "listen-address")]
        listen_addresses: Vec<String>,
        #[arg(long = "external-address")]
        external_addresses: Vec<String>,
        #[arg(long = "packet-listen")]
        packet_listen: Vec<String>,
        #[arg(long = "packet-endpoint")]
        packet_endpoints: Vec<String>,
        #[arg(long = "packet-quic-listen")]
        packet_quic_listen: Vec<String>,
        #[arg(long = "packet-quic-endpoint")]
        packet_quic_endpoints: Vec<String>,
        #[arg(
            long = "packet-session-ttl-seconds",
            default_value_t = default_packet_plane_session_ttl_seconds()
        )]
        packet_session_ttl_seconds: u64,
        #[arg(
            long = "packet-replay-windows-per-session",
            default_value_t = default_packet_plane_replay_windows_per_session()
        )]
        packet_replay_windows_per_session: usize,
        #[arg(long = "bootstrap-peer")]
        bootstrap_peers: Vec<EndpointArg>,
        #[arg(long)]
        ipfs_bootstrap_peers: bool,
        #[arg(long)]
        public_ipfs_profile: bool,
        #[arg(long = "peer")]
        peers: Vec<EndpointArg>,
        #[arg(long = "vpn-ip")]
        vpn_ip: Option<String>,
        #[arg(long = "local-route")]
        local_routes: Vec<LocalRouteArg>,
        #[arg(long = "peer-vpn-ip")]
        peer_vpn_ips: Vec<PeerVpnIpArg>,
        #[arg(long = "peer-route")]
        peer_routes: Vec<PeerRouteArg>,
        #[arg(long = "relay-reservation")]
        relay_reservations: Vec<String>,
        #[arg(long = "relay-peer")]
        relay_peers: Vec<EndpointArg>,
        #[arg(long)]
        relay_server: bool,
        #[arg(long, default_value_t = 128)]
        relay_max_reservations: usize,
        #[arg(long, default_value_t = 4)]
        relay_max_reservations_per_peer: usize,
        #[arg(long, default_value_t = 3_600)]
        relay_reservation_duration_secs: u64,
        #[arg(long, default_value_t = 16)]
        relay_max_circuits: usize,
        #[arg(long, default_value_t = 4)]
        relay_max_circuits_per_peer: usize,
        #[arg(long, default_value_t = 120)]
        relay_max_circuit_duration_secs: u64,
        #[arg(long, default_value_t = 131_072)]
        relay_max_circuit_bytes: u64,
        #[arg(long, default_value_t = default_auto_relay_max_candidates())]
        auto_relay_max_candidates: usize,
        #[arg(long, default_value_t = default_auto_relay_max_reservations())]
        auto_relay_max_reservations: usize,
        #[arg(long, default_value_t = default_auto_relay_retry_interval_seconds())]
        auto_relay_retry_interval_seconds: u64,
        #[arg(long, default_value_t = 256)]
        queue_max_packets_per_peer: usize,
        #[arg(long, default_value_t = 524_288)]
        queue_max_bytes_per_peer: usize,
        #[arg(long, default_value_t = default_max_packet_age_millis())]
        queue_max_packet_age_millis: u64,
        #[arg(long, default_value_t = 64)]
        max_concurrent_control_streams: usize,
        #[arg(long, default_value_t = 256)]
        max_concurrent_packet_streams: usize,
        #[arg(long, default_value_t = 4096)]
        max_inbound_packets_per_peer_per_second: u32,
        #[arg(long, default_value_t = 4)]
        max_pairing_requests_per_peer_per_second: u32,
        #[arg(long, default_value_t = 64)]
        max_pending_incoming_connections: u32,
        #[arg(long, default_value_t = 64)]
        max_pending_outgoing_connections: u32,
        #[arg(long, default_value_t = 256)]
        max_established_incoming_connections: u32,
        #[arg(long, default_value_t = 256)]
        max_established_outgoing_connections: u32,
        #[arg(long, default_value_t = 8)]
        max_established_connections_per_peer: u32,
        #[arg(long, default_value_t = 512)]
        max_established_connections: u32,
        #[arg(long)]
        disable_mdns: bool,
        #[arg(long)]
        disable_kademlia: bool,
        #[arg(long)]
        disable_kademlia_provider_advertisement: bool,
        #[arg(long, default_value = PUBLIC_IPFS_KADEMLIA_PROTOCOL)]
        kademlia_protocol: String,
        #[arg(long)]
        ipfs_kademlia: bool,
        #[arg(long)]
        disable_dcutr: bool,
        #[arg(long)]
        disable_autonat: bool,
        #[arg(long)]
        force: bool,
    },
    Status {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
    },
    Routes {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long)]
        resolve: Option<IpAddr>,
    },
    Mtu {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long)]
        live: bool,
        #[arg(long, default_value_t = 10)]
        timeout_seconds: u64,
    },
    Metrics {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long, value_enum, default_value_t = MetricsFormat::Text)]
        format: MetricsFormat,
    },
    Peers {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long)]
        live: bool,
        #[arg(long, default_value_t = 10)]
        timeout_seconds: u64,
    },
    Paths {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long)]
        live: bool,
        #[arg(long, default_value_t = 10)]
        timeout_seconds: u64,
    },
    Capabilities {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long)]
        live: bool,
        #[arg(long, default_value_t = 10)]
        timeout_seconds: u64,
    },
    BootstrapCheck {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
        #[arg(long)]
        require_all: bool,
        #[arg(long)]
        require_relay_reservations: bool,
        #[arg(long)]
        require_autonat_status: bool,
        #[arg(long)]
        require_dcutr_ready: bool,
        #[arg(long)]
        require_dcutr_success: bool,
        #[arg(long)]
        require_relayed_peer_circuits: bool,
        #[arg(long)]
        require_membership_records: bool,
        #[arg(long = "write-report")]
        write_report: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    RelayCheck {
        #[arg(short, long)]
        config: Option<PathBuf>,
        #[arg(long = "relay-candidate")]
        relay_candidates: Vec<String>,
        #[arg(long = "relay-candidates-file")]
        relay_candidates_file: Option<PathBuf>,
        #[arg(long = "require-relay-reservation")]
        require_relay_reservation: bool,
        #[arg(long)]
        require_dcutr_success: bool,
        #[arg(long, default_value_t = 45)]
        timeout_seconds: u64,
        #[arg(long)]
        max_validation_candidates: Option<usize>,
        #[arg(long = "write-report")]
        write_report: Option<PathBuf>,
        #[arg(long = "write-config")]
        write_config: Option<PathBuf>,
        #[arg(long = "write-host-a-config")]
        write_host_a_config: Option<PathBuf>,
        #[arg(long = "write-host-b-config")]
        write_host_b_config: Option<PathBuf>,
        #[arg(long = "two-host-network", default_value = "public-vpn-repro")]
        two_host_network: String,
        #[arg(long = "host-a-interface", default_value = "pv0")]
        host_a_interface: String,
        #[arg(long = "host-b-interface", default_value = "pv0")]
        host_b_interface: String,
        #[arg(long = "host-a-route", default_value = "10.42.0.1/32")]
        host_a_route: String,
        #[arg(long = "host-b-route", default_value = "10.42.0.2/32")]
        host_b_route: String,
        #[arg(long = "two-host-mtu", default_value_t = 1280)]
        two_host_mtu: u16,
        #[arg(long)]
        force: bool,
    },
    RelayDcutrListen {
        #[arg(long = "relay-candidate")]
        relay_candidate: String,
        #[arg(
            long = "write-descriptor",
            default_value = "p2p-vpn-dcutr-listener.json"
        )]
        write_descriptor: PathBuf,
        #[arg(long = "write-report")]
        write_report: Option<PathBuf>,
        #[arg(long, default_value_t = 45)]
        reservation_timeout_seconds: u64,
        #[arg(long, default_value_t = 600)]
        serve_seconds: u64,
        #[arg(long)]
        force: bool,
    },
    RelayDcutrDial {
        #[arg(long = "descriptor", default_value = "p2p-vpn-dcutr-listener.json")]
        descriptor: PathBuf,
        #[arg(long, default_value_t = 45)]
        timeout_seconds: u64,
        #[arg(long = "write-report")]
        write_report: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    RelayScan {
        #[arg(short, long)]
        config: Option<PathBuf>,
        #[arg(long = "bootstrap-peer")]
        bootstrap_peers: Vec<EndpointArg>,
        #[arg(long)]
        ipfs_bootstrap_peers: bool,
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
        #[arg(long, default_value_t = 8)]
        max_candidates: usize,
        #[arg(long)]
        check_candidates: bool,
        #[arg(long = "require-relay-reservation")]
        require_relay_reservation: bool,
        #[arg(long)]
        require_dcutr_success: bool,
        #[arg(long, default_value_t = 45)]
        candidate_timeout_seconds: u64,
        #[arg(long)]
        max_validation_candidates: Option<usize>,
        #[arg(long = "write-candidates")]
        write_candidates: Option<PathBuf>,
        #[arg(long = "write-report")]
        write_report: Option<PathBuf>,
        #[arg(long = "write-config")]
        write_config: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    InviteExport {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(short, long, default_value = "p2p-vpn-invite.json")]
        output: PathBuf,
        #[arg(long)]
        expires_at_unix_seconds: Option<u64>,
        #[arg(long, default_value_t = 1)]
        membership_epoch: u64,
        #[arg(long = "previous-membership-tag")]
        previous_membership_tags: Vec<String>,
        #[arg(long)]
        force: bool,
    },
    InviteImport {
        #[arg(short, long, default_value = "p2p-vpn-invite.json")]
        invite: PathBuf,
        #[arg(short, long, default_value = "p2p-vpn.json")]
        output: PathBuf,
        #[arg(long)]
        private_key: Option<String>,
        #[arg(long, default_value = "pv0")]
        interface: String,
        #[arg(long, default_value_t = 1_280)]
        mtu: u16,
        #[arg(long = "local-route")]
        local_routes: Vec<LocalRouteArg>,
        #[arg(long)]
        peer_name: Option<String>,
        #[arg(long)]
        force: bool,
    },
    Pair {
        #[command(subcommand)]
        command: PairCommand,
    },
    MembershipRecordIssue {
        #[arg(long = "issuer-config", default_value = "p2p-vpn.json")]
        issuer_config: PathBuf,
        #[arg(long = "member-identity")]
        member_identity: Option<PathBuf>,
        #[arg(long = "member-peer")]
        member_peer: Option<String>,
        #[arg(long = "member-public-key")]
        member_public_key: Option<String>,
        #[arg(long)]
        issuer_as_member: bool,
        #[arg(short, long, default_value = "p2p-vpn-member-record.json")]
        output: PathBuf,
        #[arg(long)]
        network: Option<String>,
        #[arg(long, default_value_t = 1)]
        membership_epoch: u64,
        #[arg(long, default_value_t = 1)]
        sequence: u64,
        #[arg(long = "role", value_enum)]
        roles: Vec<MembershipRecordRoleArg>,
        #[arg(long = "route-grant")]
        route_grants: Vec<LocalRouteArg>,
        #[arg(long)]
        revoked: bool,
        #[arg(long)]
        expires_at_unix_seconds: Option<u64>,
        #[arg(long)]
        force: bool,
    },
    MembershipRecordVerify {
        #[arg(short, long, default_value = "p2p-vpn-member-record.json")]
        input: PathBuf,
        #[arg(long)]
        network: Option<String>,
    },
    MembershipRecordInstall {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long = "record", required = true)]
        records: Vec<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
    MembershipRecordList {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
    },
    DaemonStatus {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
        #[arg(long, value_enum, default_value_t = MetricsFormat::Text)]
        format: MetricsFormat,
    },
    DaemonState {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
        #[arg(long, value_enum, default_value_t = DaemonViewFormat::Text)]
        format: DaemonViewFormat,
    },
    DaemonPeers {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
        #[arg(long, value_enum, default_value_t = DaemonViewFormat::Text)]
        format: DaemonViewFormat,
    },
    DaemonRoutes {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
        #[arg(long, value_enum, default_value_t = DaemonViewFormat::Text)]
        format: DaemonViewFormat,
    },
    DaemonPaths {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
        #[arg(long, value_enum, default_value_t = DaemonViewFormat::Text)]
        format: DaemonViewFormat,
    },
    DaemonMtu {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
        #[arg(long, value_enum, default_value_t = DaemonViewFormat::Text)]
        format: DaemonViewFormat,
    },
    DaemonCapabilities {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
        #[arg(long, value_enum, default_value_t = DaemonViewFormat::Text)]
        format: DaemonViewFormat,
    },
    DaemonDump {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long)]
        output_dir: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
        #[arg(long)]
        force: bool,
    },
    DaemonHealth {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
        #[arg(long, default_value_t = 0)]
        wait_seconds: u64,
        #[arg(long)]
        require_peers: bool,
        #[arg(long)]
        require_validated_peers: bool,
        #[arg(long)]
        require_supported_paths: bool,
        #[arg(long)]
        require_packet_plane_listener: bool,
        #[arg(long)]
        require_packet_plane_session: bool,
        #[arg(long)]
        require_packet_plane_quic_listener: bool,
        #[arg(long)]
        require_packet_plane_quic_session: bool,
        #[arg(long)]
        require_observed_packet_plane_udp_endpoint: bool,
        #[arg(long)]
        require_observed_packet_plane_quic_endpoint: bool,
        #[arg(long)]
        require_auto_relay_infrastructure_peer: bool,
        #[arg(long)]
        require_auto_relay_candidate: bool,
        #[arg(long)]
        require_auto_relay_reservation: bool,
    },
    DaemonShutdown {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
    },
    PeerStatus {
        peer: String,
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long, default_value_t = 10)]
        timeout_seconds: u64,
    },
    Up {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        metrics_interval_seconds: Option<u64>,
        #[arg(long)]
        control_socket: Option<PathBuf>,
        #[arg(long, requires = "control_socket")]
        pairing_state: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, Subcommand)]
enum InstanceCommand {
    List {
        #[arg(long, default_value = "/run")]
        runtime_root: PathBuf,
        #[arg(long, value_enum, default_value_t = InstanceFormat::Text)]
        format: InstanceFormat,
    },
    Show {
        instance: String,
        #[arg(long, default_value = "/run")]
        runtime_root: PathBuf,
        #[arg(long, value_enum, default_value_t = InstanceFormat::Text)]
        format: InstanceFormat,
    },
}

#[derive(Debug, Subcommand)]
enum PairCommand {
    /// Open a one-time pairing window on a running daemon.
    Open {
        #[command(flatten)]
        target: PairDaemonTarget,
        #[arg(long, default_value_t = DEFAULT_PAIRING_EXPIRES_IN_SECONDS)]
        expires_in_seconds: u64,
        #[arg(long, value_enum, default_value_t = PairOutputFormat::Text)]
        format: PairOutputFormat,
    },
    /// Join a running daemon using its one-time pairing code.
    Join {
        code: String,
        #[command(flatten)]
        target: PairDaemonTarget,
        #[arg(long, default_value_t = DEFAULT_PAIRING_EXPIRES_IN_SECONDS)]
        timeout_seconds: u64,
        #[arg(long = "vpn-ip")]
        requested_vpn_ip: Option<String>,
        #[arg(long = "route")]
        requested_routes: Vec<LocalRouteArg>,
        #[arg(long)]
        no_wait: bool,
        #[arg(long, value_enum, default_value_t = PairOutputFormat::Text)]
        format: PairOutputFormat,
    },
    /// Show a live pairing operation and pending approval.
    Status {
        #[arg(allow_hyphen_values = true)]
        operation_id: String,
        #[command(flatten)]
        target: PairDaemonTarget,
        #[arg(long, value_enum, default_value_t = PairOutputFormat::Text)]
        format: PairOutputFormat,
    },
    /// Approve a discovered peer and its explicit route grants.
    Approve {
        #[arg(allow_hyphen_values = true)]
        operation_id: String,
        #[arg(allow_hyphen_values = true)]
        approval_id: String,
        #[command(flatten)]
        target: PairDaemonTarget,
        #[arg(long = "vpn-ip")]
        assigned_vpn_ip: Option<String>,
        #[arg(long = "route")]
        granted_routes: Vec<LocalRouteArg>,
        #[arg(long, value_enum, default_value_t = PairOutputFormat::Text)]
        format: PairOutputFormat,
    },
    /// Reject a pending peer approval.
    Reject {
        #[arg(allow_hyphen_values = true)]
        operation_id: String,
        #[arg(allow_hyphen_values = true)]
        approval_id: String,
        #[command(flatten)]
        target: PairDaemonTarget,
        #[arg(long, value_enum, default_value_t = PairRejectionReasonArg::Declined)]
        reason: PairRejectionReasonArg,
        #[arg(long, value_enum, default_value_t = PairOutputFormat::Text)]
        format: PairOutputFormat,
    },
    /// Cancel a live pairing operation.
    Cancel {
        #[arg(allow_hyphen_values = true)]
        operation_id: String,
        #[command(flatten)]
        target: PairDaemonTarget,
        #[arg(long, value_enum, default_value_t = PairOutputFormat::Text)]
        format: PairOutputFormat,
    },
    /// Render a completed pairing as native NixOS configuration.
    Artifacts {
        #[arg(allow_hyphen_values = true)]
        operation_id: String,
        #[command(flatten)]
        target: PairDaemonTarget,
        #[arg(short, long, default_value = "-")]
        output: PathBuf,
        #[arg(long)]
        nixos_instance: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Compact a completed enrollment after its generated configuration is installed.
    Acknowledge {
        #[arg(allow_hyphen_values = true)]
        operation_id: String,
        #[command(flatten)]
        target: PairDaemonTarget,
        #[arg(long = "receipt", allow_hyphen_values = true)]
        transcript_sha256: String,
        #[arg(long, value_enum, default_value_t = PairOutputFormat::Text)]
        format: PairOutputFormat,
    },
    /// Export an offline offer for file-based pairing.
    Offer {
        #[arg(short, long, conflicts_with = "nixos_instance")]
        config: Option<PathBuf>,
        #[arg(long, conflicts_with = "config")]
        nixos_instance: Option<String>,
        #[arg(short, long, default_value = "-")]
        output: PathBuf,
        #[arg(long, default_value_t = DEFAULT_PAIRING_EXPIRES_IN_SECONDS)]
        expires_in_seconds: u64,
        #[arg(long)]
        rendezvous_token: Option<String>,
        #[arg(long)]
        discovery_only: bool,
        #[arg(long)]
        force: bool,
    },
    /// Inspect an offline pairing offer.
    Inspect {
        offer: String,
        #[arg(long)]
        show_secret: bool,
    },
    /// Accept an offline offer or import its response.
    Accept {
        offer: String,
        #[arg(long)]
        response: Option<PathBuf>,
        #[arg(short, long, default_value = "p2p-vpn.json")]
        output: PathBuf,
        #[arg(long)]
        nixos_output: Option<PathBuf>,
        #[arg(long)]
        nixos_instance: Option<String>,
        #[arg(long)]
        nixos_only: bool,
        #[arg(long)]
        nixos_state_dir: Option<PathBuf>,
        #[arg(long)]
        private_key: Option<String>,
        #[arg(long, default_value = "pv0")]
        interface: String,
        #[arg(long, default_value_t = 1_280)]
        mtu: u16,
        #[arg(long = "local-route")]
        local_routes: Vec<LocalRouteArg>,
        #[arg(long = "vpn-ip")]
        vpn_ip: Option<String>,
        #[arg(long)]
        peer_name: Option<String>,
        #[arg(long, default_value_t = 30)]
        timeout_seconds: u64,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Clone, Debug, ClapArgs)]
struct PairDaemonTarget {
    /// Running daemon control socket. Defaults to /run/p2p-vpn/control.sock.
    #[arg(long, conflicts_with = "instance")]
    socket: Option<PathBuf>,
    /// NixOS module instance, resolved to its generated control socket.
    #[arg(long, conflicts_with = "socket")]
    instance: Option<String>,
    /// Timeout for each local daemon RPC.
    #[arg(long, default_value_t = 5)]
    rpc_timeout_seconds: u64,
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<(), String> {
    let cli = Cli::parse();

    match cli.command {
        Command::Keygen { output, force } => keygen(&output, force),
        Command::IdentityPublic {
            config,
            private_key,
            output,
            force,
        } => identity_public(IdentityPublicArgs {
            config,
            private_key,
            output,
            force,
        }),
        Command::Instance { command } => instance_command(command),
        Command::InitConfig {
            output,
            network,
            private_key,
            membership_key,
            previous_membership_tags,
            interface,
            mtu,
            listen_addresses,
            external_addresses,
            packet_listen,
            packet_endpoints,
            packet_quic_listen,
            packet_quic_endpoints,
            packet_session_ttl_seconds,
            packet_replay_windows_per_session,
            bootstrap_peers,
            ipfs_bootstrap_peers,
            public_ipfs_profile,
            peers,
            vpn_ip,
            local_routes,
            peer_vpn_ips,
            peer_routes,
            relay_reservations,
            relay_peers,
            relay_server,
            relay_max_reservations,
            relay_max_reservations_per_peer,
            relay_reservation_duration_secs,
            relay_max_circuits,
            relay_max_circuits_per_peer,
            relay_max_circuit_duration_secs,
            relay_max_circuit_bytes,
            auto_relay_max_candidates,
            auto_relay_max_reservations,
            auto_relay_retry_interval_seconds,
            queue_max_packets_per_peer,
            queue_max_bytes_per_peer,
            queue_max_packet_age_millis,
            max_concurrent_control_streams,
            max_concurrent_packet_streams,
            max_inbound_packets_per_peer_per_second,
            max_pairing_requests_per_peer_per_second,
            max_pending_incoming_connections,
            max_pending_outgoing_connections,
            max_established_incoming_connections,
            max_established_outgoing_connections,
            max_established_connections_per_peer,
            max_established_connections,
            disable_mdns,
            disable_kademlia,
            disable_kademlia_provider_advertisement,
            kademlia_protocol,
            ipfs_kademlia,
            disable_dcutr,
            disable_autonat,
            force,
        } => init_config(InitConfigArgs {
            output,
            network,
            private_key,
            membership_key,
            previous_membership_tags,
            interface,
            mtu,
            listen_addresses,
            external_addresses,
            packet_plane: PacketPlaneConfig {
                listen: packet_listen,
                external_endpoints: packet_endpoints,
                quic_listen: packet_quic_listen,
                quic_external_endpoints: packet_quic_endpoints,
                session_ttl_seconds: packet_session_ttl_seconds,
                max_replay_windows_per_session: packet_replay_windows_per_session,
            },
            bootstrap_peers,
            relay_peers,
            ipfs_bootstrap_peers: ipfs_bootstrap_peers || public_ipfs_profile,
            public_ipfs_profile,
            peers,
            vpn_ip,
            local_routes,
            peer_vpn_ips,
            peer_routes,
            discovery: InitDiscoveryFlags {
                disable_mdns,
                disable_kademlia,
                disable_kademlia_provider_advertisement,
                disable_dcutr,
                disable_autonat,
            }
            .into_config(kademlia_protocol, ipfs_kademlia, public_ipfs_profile),
            relay: RelayConfig {
                server: relay_server,
                reservations: relay_reservations,
                auto: AutoRelayConfig {
                    max_candidates: auto_relay_max_candidates,
                    max_reservations: auto_relay_max_reservations,
                    retry_interval_seconds: auto_relay_retry_interval_seconds,
                },
                resources: RelayResourceConfig {
                    max_reservations: relay_max_reservations,
                    max_reservations_per_peer: relay_max_reservations_per_peer,
                    reservation_duration_secs: relay_reservation_duration_secs,
                    max_circuits: relay_max_circuits,
                    max_circuits_per_peer: relay_max_circuits_per_peer,
                    max_circuit_duration_secs: relay_max_circuit_duration_secs,
                    max_circuit_bytes: relay_max_circuit_bytes,
                },
            },
            queue: QueueConfig {
                max_packets_per_peer: queue_max_packets_per_peer,
                max_bytes_per_peer: queue_max_bytes_per_peer,
                max_packet_age_millis: queue_max_packet_age_millis,
            },
            resources: ResourceConfig {
                max_concurrent_packet_streams,
                max_concurrent_control_streams,
                max_inbound_packets_per_peer_per_second,
                max_pairing_requests_per_peer_per_second,
                max_pending_incoming_connections,
                max_pending_outgoing_connections,
                max_established_incoming_connections,
                max_established_outgoing_connections,
                max_established_connections_per_peer,
                max_established_connections,
            },
            force,
        }),
        Command::Status { config } => status(&config),
        Command::Routes { config, resolve } => routes(&config, resolve),
        Command::Mtu {
            config,
            live,
            timeout_seconds,
        } => Box::pin(mtu(&config, live, timeout_seconds)).await,
        Command::Metrics { config, format } => metrics(&config, format),
        Command::Peers {
            config,
            live,
            timeout_seconds,
        } => Box::pin(peers(&config, live, timeout_seconds)).await,
        Command::Paths {
            config,
            live,
            timeout_seconds,
        } => Box::pin(paths(&config, live, timeout_seconds)).await,
        Command::Capabilities {
            config,
            live,
            timeout_seconds,
        } => Box::pin(capabilities(&config, live, timeout_seconds)).await,
        Command::BootstrapCheck {
            config,
            timeout_seconds,
            require_all,
            require_relay_reservations,
            require_autonat_status,
            require_dcutr_ready,
            require_dcutr_success,
            require_relayed_peer_circuits,
            require_membership_records,
            write_report,
            force,
        } => {
            let threshold = if require_all {
                BootstrapCheckThreshold::All
            } else {
                BootstrapCheckThreshold::Any
            };
            Box::pin(bootstrap_check(BootstrapCheckArgs {
                config_path: config,
                timeout_seconds,
                threshold,
                requirements: BootstrapCheckRequirements {
                    relay_reservations: require_relay_reservations,
                    autonat_status: require_autonat_status,
                    dcutr_ready: require_dcutr_ready,
                    dcutr_success: require_dcutr_success,
                    relayed_peer_circuits: require_relayed_peer_circuits,
                    membership_records: require_membership_records,
                },
                write_report,
                force,
            }))
            .await
        }
        Command::RelayCheck {
            config,
            relay_candidates,
            relay_candidates_file,
            require_relay_reservation,
            require_dcutr_success,
            timeout_seconds,
            max_validation_candidates,
            write_report,
            write_config,
            write_host_a_config,
            write_host_b_config,
            two_host_network,
            host_a_interface,
            host_b_interface,
            host_a_route,
            host_b_route,
            two_host_mtu,
            force,
        } => {
            if require_relay_reservation && require_dcutr_success {
                return Err(
                    "--require-relay-reservation and --require-dcutr-success cannot be used together"
                        .to_owned(),
                );
            }
            let mode = if require_relay_reservation {
                PublicRelayProbeMode::RelayReservation
            } else if require_dcutr_success {
                PublicRelayProbeMode::DcutrSuccess
            } else {
                PublicRelayProbeMode::RelayedPeerCircuit
            };
            Box::pin(relay_check(RelayCheckArgs {
                config_path: config,
                relay_candidates,
                relay_candidates_file,
                timeout_seconds,
                mode,
                max_validation_candidates,
                write_report,
                write_config,
                write_host_a_config,
                write_host_b_config,
                two_host_network,
                host_a_interface,
                host_b_interface,
                host_a_route,
                host_b_route,
                two_host_mtu,
                force,
            }))
            .await
        }
        Command::RelayDcutrListen {
            relay_candidate,
            write_descriptor,
            write_report,
            reservation_timeout_seconds,
            serve_seconds,
            force,
        } => {
            Box::pin(relay_dcutr_listen(RelayDcutrListenArgs {
                relay_candidate,
                write_descriptor,
                write_report,
                reservation_timeout_seconds,
                serve_seconds,
                force,
            }))
            .await
        }
        Command::RelayDcutrDial {
            descriptor,
            timeout_seconds,
            write_report,
            force,
        } => {
            Box::pin(relay_dcutr_dial(RelayDcutrDialArgs {
                descriptor,
                timeout_seconds,
                write_report,
                force,
            }))
            .await
        }
        Command::RelayScan {
            config,
            bootstrap_peers,
            ipfs_bootstrap_peers,
            timeout_seconds,
            max_candidates,
            check_candidates,
            require_relay_reservation,
            require_dcutr_success,
            candidate_timeout_seconds,
            max_validation_candidates,
            write_candidates,
            write_report,
            write_config,
            force,
        } => {
            Box::pin(relay_scan(RelayScanArgs {
                config_path: config,
                bootstrap_peers,
                ipfs_bootstrap_peers,
                timeout_seconds,
                max_candidates,
                check_candidates,
                require_relay_reservation,
                require_dcutr_success,
                candidate_timeout_seconds,
                max_validation_candidates,
                write_candidates,
                write_report,
                write_config,
                force,
            }))
            .await
        }
        Command::InviteExport {
            config,
            output,
            expires_at_unix_seconds,
            membership_epoch,
            previous_membership_tags,
            force,
        } => invite_export(
            &config,
            &output,
            InviteExportOptions {
                expires_at_unix_seconds,
                membership_epoch,
                previous_membership_tags,
            },
            force,
        ),
        Command::InviteImport {
            invite,
            output,
            private_key,
            interface,
            mtu,
            local_routes,
            peer_name,
            force,
        } => invite_import(InviteImportArgs {
            invite,
            output,
            private_key,
            interface,
            mtu,
            local_routes,
            peer_name,
            force,
        }),
        Command::Pair { command } => match command {
            PairCommand::Open {
                target,
                expires_in_seconds,
                format,
            } => pair_daemon_open(&target, expires_in_seconds, format).await,
            PairCommand::Join {
                code,
                target,
                timeout_seconds,
                requested_vpn_ip,
                requested_routes,
                no_wait,
                format,
            } => {
                pair_daemon_join(
                    &target,
                    &code,
                    timeout_seconds,
                    requested_vpn_ip,
                    requested_routes,
                    !no_wait,
                    format,
                )
                .await
            }
            PairCommand::Status {
                operation_id,
                target,
                format,
            } => pair_daemon_status(&target, &operation_id, format).await,
            PairCommand::Approve {
                operation_id,
                approval_id,
                target,
                assigned_vpn_ip,
                granted_routes,
                format,
            } => {
                pair_daemon_approve(
                    &target,
                    &operation_id,
                    &approval_id,
                    assigned_vpn_ip,
                    granted_routes,
                    format,
                )
                .await
            }
            PairCommand::Reject {
                operation_id,
                approval_id,
                target,
                reason,
                format,
            } => {
                pair_daemon_action(
                    &target,
                    PairRpcRequest::PairReject {
                        operation_id,
                        approval_id,
                        reason: reason.into(),
                    },
                    format,
                )
                .await
            }
            PairCommand::Cancel {
                operation_id,
                target,
                format,
            } => {
                pair_daemon_action(&target, PairRpcRequest::PairCancel { operation_id }, format)
                    .await
            }
            PairCommand::Artifacts {
                operation_id,
                target,
                output,
                nixos_instance,
                force,
            } => {
                pair_daemon_artifacts(
                    &target,
                    &operation_id,
                    &output,
                    nixos_instance.as_deref(),
                    force,
                )
                .await
            }
            PairCommand::Acknowledge {
                operation_id,
                target,
                transcript_sha256,
                format,
            } => pair_daemon_acknowledge(&target, &operation_id, &transcript_sha256, format).await,
            PairCommand::Offer {
                config,
                nixos_instance,
                output,
                expires_in_seconds,
                rendezvous_token,
                discovery_only,
                force,
            } => pair_offer(PairOfferArgs {
                config,
                nixos_instance,
                output,
                expires_in_seconds,
                rendezvous_token,
                discovery_only,
                force,
            }),
            PairCommand::Inspect { offer, show_secret } => {
                pair_inspect(&PairInspectArgs { offer, show_secret })
            }
            PairCommand::Accept {
                offer,
                response,
                output,
                nixos_output,
                nixos_instance,
                nixos_only,
                nixos_state_dir,
                private_key,
                interface,
                mtu,
                local_routes,
                vpn_ip,
                peer_name,
                timeout_seconds,
                force,
            } => {
                pair_accept(PairAcceptArgs {
                    offer,
                    response,
                    output,
                    nixos_output,
                    nixos_instance,
                    nixos_only,
                    nixos_state_dir,
                    private_key,
                    interface,
                    mtu,
                    local_routes,
                    vpn_ip,
                    peer_name,
                    timeout_seconds,
                    force,
                })
                .await
            }
        },
        Command::MembershipRecordIssue {
            issuer_config,
            member_identity,
            member_peer,
            member_public_key,
            issuer_as_member,
            output,
            network,
            membership_epoch,
            sequence,
            roles,
            route_grants,
            revoked,
            expires_at_unix_seconds,
            force,
        } => membership_record_issue(MembershipRecordIssueArgs {
            issuer_config,
            member_identity,
            member_peer,
            member_public_key,
            issuer_as_member,
            output,
            network,
            membership_epoch,
            sequence,
            roles,
            route_grants,
            revoked,
            expires_at_unix_seconds,
            force,
        }),
        Command::MembershipRecordVerify { input, network } => {
            membership_record_verify(&input, network.as_deref())
        }
        Command::MembershipRecordInstall {
            config,
            records,
            output,
            force,
        } => membership_record_install(&config, &records, output.as_deref(), force),
        Command::MembershipRecordList { config } => membership_record_list(&config),
        Command::DaemonStatus {
            socket,
            timeout_seconds,
            format,
        } => Box::pin(daemon_status(&socket, timeout_seconds, format)).await,
        Command::DaemonState {
            socket,
            timeout_seconds,
            format,
        } => Box::pin(daemon_state(&socket, timeout_seconds, format)).await,
        Command::DaemonPeers {
            socket,
            timeout_seconds,
            format,
        } => Box::pin(daemon_peers(&socket, timeout_seconds, format)).await,
        Command::DaemonRoutes {
            socket,
            timeout_seconds,
            format,
        } => Box::pin(daemon_routes(&socket, timeout_seconds, format)).await,
        Command::DaemonPaths {
            socket,
            timeout_seconds,
            format,
        } => Box::pin(daemon_paths(&socket, timeout_seconds, format)).await,
        Command::DaemonMtu {
            socket,
            timeout_seconds,
            format,
        } => Box::pin(daemon_mtu(&socket, timeout_seconds, format)).await,
        Command::DaemonCapabilities {
            socket,
            timeout_seconds,
            format,
        } => Box::pin(daemon_capabilities(&socket, timeout_seconds, format)).await,
        Command::DaemonDump {
            socket,
            output_dir,
            timeout_seconds,
            force,
        } => Box::pin(daemon_dump(&socket, &output_dir, timeout_seconds, force)).await,
        Command::DaemonHealth {
            socket,
            timeout_seconds,
            wait_seconds,
            require_peers,
            require_validated_peers,
            require_supported_paths,
            require_packet_plane_listener,
            require_packet_plane_session,
            require_packet_plane_quic_listener,
            require_packet_plane_quic_session,
            require_observed_packet_plane_udp_endpoint,
            require_observed_packet_plane_quic_endpoint,
            require_auto_relay_infrastructure_peer,
            require_auto_relay_candidate,
            require_auto_relay_reservation,
        } => {
            Box::pin(daemon_health(
                &socket,
                timeout_seconds,
                DaemonHealthOptions {
                    wait: Duration::from_secs(wait_seconds),
                    requirements: DaemonHealthRequirements {
                        peers: require_peers,
                        validated_peers: require_validated_peers,
                        supported_paths: require_supported_paths,
                        packet_plane_listener: require_packet_plane_listener,
                        packet_plane_session: require_packet_plane_session,
                        packet_plane_quic_listener: require_packet_plane_quic_listener,
                        packet_plane_quic_session: require_packet_plane_quic_session,
                        observed_packet_plane_udp_endpoint:
                            require_observed_packet_plane_udp_endpoint,
                        observed_packet_plane_quic_endpoint:
                            require_observed_packet_plane_quic_endpoint,
                        auto_relay_infrastructure_peer: require_auto_relay_infrastructure_peer,
                        auto_relay_candidate: require_auto_relay_candidate,
                        auto_relay_reservation: require_auto_relay_reservation,
                    },
                },
            ))
            .await
        }
        Command::DaemonShutdown {
            socket,
            timeout_seconds,
        } => Box::pin(daemon_shutdown(&socket, timeout_seconds)).await,
        Command::PeerStatus {
            peer,
            config,
            timeout_seconds,
        } => Box::pin(peer_status(&config, &peer, timeout_seconds)).await,
        Command::Up {
            config,
            dry_run,
            metrics_interval_seconds,
            control_socket,
            pairing_state,
        } => {
            Box::pin(up(
                &config,
                dry_run,
                metrics_interval_seconds,
                control_socket,
                pairing_state,
            ))
            .await
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
struct InitDiscoveryFlags {
    disable_mdns: bool,
    disable_kademlia: bool,
    disable_kademlia_provider_advertisement: bool,
    disable_dcutr: bool,
    disable_autonat: bool,
}

impl InitDiscoveryFlags {
    fn into_config(
        self,
        kademlia_protocol: String,
        ipfs_kademlia: bool,
        public_ipfs_profile: bool,
    ) -> DiscoveryConfig {
        DiscoveryConfig {
            mdns: !self.disable_mdns,
            kademlia: !self.disable_kademlia,
            kademlia_provider_advertisement: !self.disable_kademlia
                && !self.disable_kademlia_provider_advertisement,
            kademlia_protocol: selected_kademlia_protocol(
                kademlia_protocol,
                ipfs_kademlia || public_ipfs_profile,
            ),
            dcutr: !self.disable_dcutr,
            autonat: !self.disable_autonat,
        }
    }
}

fn selected_kademlia_protocol(kademlia_protocol: String, ipfs_kademlia: bool) -> String {
    if ipfs_kademlia {
        PUBLIC_IPFS_KADEMLIA_PROTOCOL.to_owned()
    } else {
        kademlia_protocol
    }
}

#[derive(Clone, Debug)]
struct InitConfigArgs {
    output: PathBuf,
    network: String,
    private_key: Option<String>,
    membership_key: Option<String>,
    previous_membership_tags: Vec<String>,
    interface: String,
    mtu: u16,
    listen_addresses: Vec<String>,
    external_addresses: Vec<String>,
    packet_plane: PacketPlaneConfig,
    bootstrap_peers: Vec<EndpointArg>,
    relay_peers: Vec<EndpointArg>,
    ipfs_bootstrap_peers: bool,
    public_ipfs_profile: bool,
    peers: Vec<EndpointArg>,
    vpn_ip: Option<String>,
    local_routes: Vec<LocalRouteArg>,
    peer_vpn_ips: Vec<PeerVpnIpArg>,
    peer_routes: Vec<PeerRouteArg>,
    discovery: DiscoveryConfig,
    relay: RelayConfig,
    queue: QueueConfig,
    resources: ResourceConfig,
    force: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EndpointArg {
    id: String,
    address: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LocalRouteArg {
    route: RouteConfig,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PeerVpnIpArg {
    id: String,
    vpn_ip: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PeerRouteArg {
    id: String,
    route: RouteConfig,
}

#[derive(Clone, Debug)]
struct IdentityPublicArgs {
    config: Option<PathBuf>,
    private_key: Option<String>,
    output: PathBuf,
    force: bool,
}

#[derive(Clone, Debug)]
struct InviteImportArgs {
    invite: PathBuf,
    output: PathBuf,
    private_key: Option<String>,
    interface: String,
    mtu: u16,
    local_routes: Vec<LocalRouteArg>,
    peer_name: Option<String>,
    force: bool,
}

#[derive(Clone, Debug)]
struct PairOfferArgs {
    config: Option<PathBuf>,
    nixos_instance: Option<String>,
    output: PathBuf,
    expires_in_seconds: u64,
    rendezvous_token: Option<String>,
    discovery_only: bool,
    force: bool,
}

#[derive(Clone, Debug)]
struct PairInspectArgs {
    offer: String,
    show_secret: bool,
}

#[derive(Clone, Debug)]
struct PairAcceptArgs {
    offer: String,
    response: Option<PathBuf>,
    output: PathBuf,
    nixos_output: Option<PathBuf>,
    nixos_instance: Option<String>,
    nixos_only: bool,
    nixos_state_dir: Option<PathBuf>,
    private_key: Option<String>,
    interface: String,
    mtu: u16,
    local_routes: Vec<LocalRouteArg>,
    vpn_ip: Option<String>,
    peer_name: Option<String>,
    timeout_seconds: u64,
    force: bool,
}

#[derive(Clone, Debug)]
struct MembershipRecordIssueArgs {
    issuer_config: PathBuf,
    member_identity: Option<PathBuf>,
    member_peer: Option<String>,
    member_public_key: Option<String>,
    issuer_as_member: bool,
    output: PathBuf,
    network: Option<String>,
    membership_epoch: u64,
    sequence: u64,
    roles: Vec<MembershipRecordRoleArg>,
    route_grants: Vec<LocalRouteArg>,
    revoked: bool,
    expires_at_unix_seconds: Option<u64>,
    force: bool,
}

#[derive(Clone, Debug)]
struct MembershipRecordInstallStats {
    accepted: usize,
    ignored_stale_or_equal: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum MembershipRecordRoleArg {
    OverlayMember,
    RouteAuthority,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
struct RelayScanArgs {
    config_path: Option<PathBuf>,
    bootstrap_peers: Vec<EndpointArg>,
    ipfs_bootstrap_peers: bool,
    timeout_seconds: u64,
    max_candidates: usize,
    check_candidates: bool,
    require_relay_reservation: bool,
    require_dcutr_success: bool,
    candidate_timeout_seconds: u64,
    max_validation_candidates: Option<usize>,
    write_candidates: Option<PathBuf>,
    write_report: Option<PathBuf>,
    write_config: Option<PathBuf>,
    force: bool,
}

#[derive(Clone, Debug)]
struct RelayCheckArgs {
    config_path: Option<PathBuf>,
    relay_candidates: Vec<String>,
    relay_candidates_file: Option<PathBuf>,
    timeout_seconds: u64,
    mode: PublicRelayProbeMode,
    max_validation_candidates: Option<usize>,
    write_report: Option<PathBuf>,
    write_config: Option<PathBuf>,
    write_host_a_config: Option<PathBuf>,
    write_host_b_config: Option<PathBuf>,
    two_host_network: String,
    host_a_interface: String,
    host_b_interface: String,
    host_a_route: String,
    host_b_route: String,
    two_host_mtu: u16,
    force: bool,
}

#[derive(Clone, Debug)]
struct BootstrapCheckArgs {
    config_path: PathBuf,
    timeout_seconds: u64,
    threshold: BootstrapCheckThreshold,
    requirements: BootstrapCheckRequirements,
    write_report: Option<PathBuf>,
    force: bool,
}

#[derive(Clone, Debug)]
struct RelayDcutrListenArgs {
    relay_candidate: String,
    write_descriptor: PathBuf,
    write_report: Option<PathBuf>,
    reservation_timeout_seconds: u64,
    serve_seconds: u64,
    force: bool,
}

#[derive(Clone, Debug)]
struct RelayDcutrDialArgs {
    descriptor: PathBuf,
    timeout_seconds: u64,
    write_report: Option<PathBuf>,
    force: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RelayCandidateReachability {
    ipv4: bool,
    ipv6: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SkippedRelayCandidate {
    address: String,
    reason: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum MetricsFormat {
    Text,
    Prometheus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum DaemonViewFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum InstanceFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PairOutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PairRejectionReasonArg {
    Declined,
    IdentityMismatch,
    AddressConflict,
    RouteRequestDenied,
    Policy,
}

impl From<PairRejectionReasonArg> for PairRpcRejectionReason {
    fn from(reason: PairRejectionReasonArg) -> Self {
        match reason {
            PairRejectionReasonArg::Declined => Self::Declined,
            PairRejectionReasonArg::IdentityMismatch => Self::IdentityMismatch,
            PairRejectionReasonArg::AddressConflict => Self::AddressConflict,
            PairRejectionReasonArg::RouteRequestDenied => Self::RouteRequestDenied,
            PairRejectionReasonArg::Policy => Self::Policy,
        }
    }
}

impl From<MembershipRecordRoleArg> for MembershipRole {
    fn from(role: MembershipRecordRoleArg) -> Self {
        match role {
            MembershipRecordRoleArg::OverlayMember => Self::OverlayMember,
            MembershipRecordRoleArg::RouteAuthority => Self::RouteAuthority,
        }
    }
}

impl FromStr for EndpointArg {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (id, address) = input
            .split_once('=')
            .map_or((input, None), |(id, address)| (id, Some(address)));
        if id.is_empty() {
            return Err("peer id cannot be empty".to_owned());
        }
        if matches!(address, Some("")) {
            return Err("peer address cannot be empty".to_owned());
        }

        Ok(Self {
            id: id.to_owned(),
            address: address.map(str::to_owned),
        })
    }
}

impl FromStr for LocalRouteArg {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            route: parse_route_arg(input, "local route")?,
        })
    }
}

impl FromStr for PeerVpnIpArg {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (id, vpn_ip) = input
            .split_once('=')
            .ok_or_else(|| "peer VPN IP must be PEER_ID=IP".to_owned())?;
        if id.is_empty() {
            return Err("peer id cannot be empty".to_owned());
        }
        if vpn_ip.is_empty() {
            return Err("peer VPN IP cannot be empty".to_owned());
        }

        Ok(Self {
            id: id.to_owned(),
            vpn_ip: vpn_ip.to_owned(),
        })
    }
}

impl FromStr for PeerRouteArg {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (id, route) = input
            .split_once('=')
            .ok_or_else(|| "peer route must be PEER_ID=CIDR[,METRIC]".to_owned())?;
        if id.is_empty() {
            return Err("peer id cannot be empty".to_owned());
        }
        if route.is_empty() {
            return Err("peer route cannot be empty".to_owned());
        }

        Ok(Self {
            id: id.to_owned(),
            route: parse_route_arg(route, "peer route")?,
        })
    }
}

fn parse_route_arg(input: &str, context: &str) -> Result<RouteConfig, String> {
    let (prefix, metric) = if let Some((prefix, metric)) = input.split_once(',') {
        let metric = metric
            .parse::<u16>()
            .map_err(|_| format!("{context} metric `{metric}` is not a u16"))?;
        (prefix, metric)
    } else {
        (input, 100)
    };
    if prefix.is_empty() {
        return Err(format!("{context} prefix cannot be empty"));
    }

    Ok(RouteConfig {
        prefix: prefix.to_owned(),
        metric,
    })
}

fn keygen(output: &Path, force: bool) -> Result<(), String> {
    let identity = NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate key: {error:?}"))?;

    if output.to_string_lossy() == "-" {
        println!("peer_id: {}", identity.peer_id);
        println!("private_key: {}", identity.private_key);
        return Ok(());
    }

    if force {
        match fs::remove_file(output) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("failed to replace {}: {error}", output.display()));
            }
        }
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "{} already exists; pass --force to replace it",
                    output.display()
                )
            } else {
                format!("failed to create {}: {error}", output.display())
            }
        })?;
    writeln!(file, "{}", identity.private_key)
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
    file.sync_all()
        .map_err(|error| format!("failed to sync {}: {error}", output.display()))?;

    println!("wrote {}", output.display());
    println!("peer_id: {}", identity.peer_id);
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PublicIdentityJson {
    peer_id: String,
    public_key: String,
}

fn identity_public(args: IdentityPublicArgs) -> Result<(), String> {
    let identity = identity_from_config_or_private_key(args.config.as_deref(), args.private_key)?;
    let public = public_identity_json(&identity)?;
    write_json_output(&public, &args.output, args.force, "public identity")?;
    if args.output.to_string_lossy() != "-" {
        println!("peer_id: {}", public.peer_id);
    }
    Ok(())
}

fn public_identity_json(identity: &NodeIdentity) -> Result<PublicIdentityJson, String> {
    let subject = MembershipRecordSubject::from_identity(identity)
        .map_err(|error| format!("failed to derive public identity: {error:?}"))?;
    Ok(PublicIdentityJson {
        peer_id: subject.peer_id,
        public_key: subject.public_key,
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct InstanceInfo {
    instance: String,
    network: String,
    interface: String,
    peer_id: String,
}

fn instance_command(command: InstanceCommand) -> Result<(), String> {
    match command {
        InstanceCommand::List {
            runtime_root,
            format,
        } => {
            let instances = list_instances(&runtime_root)?;
            write_instance_list(&instances, format)
        }
        InstanceCommand::Show {
            instance,
            runtime_root,
            format,
        } => {
            validate_runtime_instance_name(&instance)?;
            let info = load_instance_info(&runtime_root, &instance)?;
            write_instance_show(&info, format)
        }
    }
}

fn validate_runtime_instance_name(instance: &str) -> Result<(), String> {
    validate_nixos_instance_name(instance).map_err(|_| {
        "instance must start with an ASCII letter or digit and contain only letters, digits, dots, underscores, or hyphens"
            .to_owned()
    })
}

fn instance_runtime_config(runtime_root: &Path, instance: &str) -> PathBuf {
    runtime_root
        .join(format!("p2p-vpn-{instance}"))
        .join("config.json")
}

fn load_instance_info(runtime_root: &Path, instance: &str) -> Result<InstanceInfo, String> {
    let path = instance_runtime_config(runtime_root, instance);
    let config = Config::load(&path).map_err(|error| {
        format!(
            "failed to inspect instance `{instance}` at {}: {error:?}; NixOS runtime configs normally require sudo",
            path.display()
        )
    })?;
    let peer_id = config.local_peer().map_err(|error| {
        format!("failed to derive peer ID for instance `{instance}`: {error:?}")
    })?;

    Ok(InstanceInfo {
        instance: instance.to_owned(),
        network: config.network.name,
        interface: config.interface.name,
        peer_id,
    })
}

fn list_instances(runtime_root: &Path) -> Result<Vec<InstanceInfo>, String> {
    let entries = fs::read_dir(runtime_root).map_err(|error| {
        format!(
            "failed to inspect runtime root {}: {error}",
            runtime_root.display()
        )
    })?;
    let mut names = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.strip_prefix("p2p-vpn-").map(str::to_owned))
        .filter(|name| {
            validate_runtime_instance_name(name).is_ok()
                && instance_runtime_config(runtime_root, name).is_file()
        })
        .collect::<Vec<_>>();
    names.sort();
    names
        .into_iter()
        .map(|instance| load_instance_info(runtime_root, &instance))
        .collect()
}

fn write_instance_list(instances: &[InstanceInfo], format: InstanceFormat) -> Result<(), String> {
    match format {
        InstanceFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(instances)
                .map_err(|error| format!("failed to encode instance list: {error}"))?
        ),
        InstanceFormat::Text if instances.is_empty() => println!("no instances found"),
        InstanceFormat::Text => {
            let instance_width = instances
                .iter()
                .map(|info| info.instance.len())
                .max()
                .unwrap_or(8)
                .max("INSTANCE".len());
            let network_width = instances
                .iter()
                .map(|info| info.network.len())
                .max()
                .unwrap_or(7)
                .max("NETWORK".len());
            let interface_width = instances
                .iter()
                .map(|info| info.interface.len())
                .max()
                .unwrap_or(9)
                .max("INTERFACE".len());
            println!(
                "{:<instance_width$}  {:<network_width$}  {:<interface_width$}  PEER_ID",
                "INSTANCE", "NETWORK", "INTERFACE"
            );
            for info in instances {
                println!(
                    "{:<instance_width$}  {:<network_width$}  {:<interface_width$}  {}",
                    info.instance, info.network, info.interface, info.peer_id
                );
            }
        }
    }
    Ok(())
}

fn write_instance_show(info: &InstanceInfo, format: InstanceFormat) -> Result<(), String> {
    match format {
        InstanceFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(info)
                .map_err(|error| format!("failed to encode instance: {error}"))?
        ),
        InstanceFormat::Text => {
            println!("instance: {}", info.instance);
            println!("network: {}", info.network);
            println!("interface: {}", info.interface);
            println!("peer ID: {}", info.peer_id);
        }
    }
    Ok(())
}

fn identity_from_config_or_private_key(
    config_path: Option<&Path>,
    private_key: Option<String>,
) -> Result<NodeIdentity, String> {
    match (config_path, private_key) {
        (Some(_), Some(_)) => Err("pass either --config or --private-key, not both".to_owned()),
        (Some(path), None) => Config::load(path)
            .map_err(|error| format!("failed to load config: {error:?}"))?
            .identity()
            .map_err(|error| format!("failed to read identity from config: {error:?}")),
        (None, Some(private_key)) => NodeIdentity::from_private_key(&private_key)
            .map_err(|error| format!("failed to decode private key: {error:?}")),
        (None, None) => Err("pass --config or --private-key".to_owned()),
    }
}

fn membership_record_issue(args: MembershipRecordIssueArgs) -> Result<(), String> {
    let issuer_config = Config::load(&args.issuer_config)
        .map_err(|error| format!("failed to load issuer config: {error:?}"))?;
    let issuer = issuer_config
        .identity()
        .map_err(|error| format!("failed to read issuer identity: {error:?}"))?;
    let member = membership_record_subject_from_args(&args, &issuer)?;
    let route_grants = args
        .route_grants
        .into_iter()
        .map(|route| route.route)
        .collect::<Vec<_>>();
    let roles = if args.revoked {
        if !args.roles.is_empty()
            || !route_grants.is_empty()
            || args.expires_at_unix_seconds.is_some()
        {
            return Err(
                "revoked membership records cannot carry roles, route grants, or expiry".to_owned(),
            );
        }
        Vec::new()
    } else {
        membership_record_roles(args.roles, !route_grants.is_empty())
    };
    let network_name = args
        .network
        .unwrap_or_else(|| issuer_config.network.name.clone());
    let record = issue_membership_record_for_subject_at(
        &issuer,
        MembershipRecordIssueOptions {
            network_name,
            member,
            membership_epoch: args.membership_epoch,
            sequence: args.sequence,
            revoked: args.revoked,
            roles,
            route_grants,
            expires_at_unix_seconds: args.expires_at_unix_seconds,
        },
        current_unix_seconds_lossy(),
    )
    .map_err(|error| format!("failed to issue membership record: {error:?}"))?;

    write_json_output(&record, &args.output, args.force, "membership record")?;
    if args.output.to_string_lossy() != "-" {
        println!("member peer: {}", record.payload.member_peer);
        println!("issuer peer: {}", record.payload.issuer_peer);
        println!("membership epoch: {}", record.payload.membership_epoch);
        println!("sequence: {}", record.payload.sequence);
        println!("revoked: {}", record.payload.revoked);
    }
    Ok(())
}

fn membership_record_subject_from_args(
    args: &MembershipRecordIssueArgs,
    issuer: &NodeIdentity,
) -> Result<MembershipRecordSubject, String> {
    match (
        args.issuer_as_member,
        &args.member_identity,
        &args.member_peer,
        &args.member_public_key,
    ) {
        (true, None, None, None) => MembershipRecordSubject::from_identity(issuer)
            .map_err(|error| format!("failed to use issuer identity as member: {error:?}")),
        (true, _, _, _) => Err(
            "pass either --issuer-as-member, --member-identity, or --member-peer with --member-public-key".to_owned(),
        ),
        (false, Some(_), Some(_), _) | (false, Some(_), _, Some(_)) => Err(
            "pass either --member-identity or --member-peer with --member-public-key".to_owned(),
        ),
        (false, Some(path), None, None) => {
            read_public_identity(path).map(|identity| MembershipRecordSubject {
                peer_id: identity.peer_id,
                public_key: identity.public_key,
            })
        }
        (false, None, Some(peer_id), Some(public_key)) => Ok(MembershipRecordSubject {
            peer_id: peer_id.clone(),
            public_key: public_key.clone(),
        }),
        (false, None, Some(_), None) => Err("--member-peer requires --member-public-key".to_owned()),
        (false, None, None, Some(_)) => Err("--member-public-key requires --member-peer".to_owned()),
        (false, None, None, None) => Err(
            "pass --issuer-as-member, --member-identity, or --member-peer with --member-public-key"
                .to_owned(),
        ),
    }
}

fn read_public_identity(path: &Path) -> Result<PublicIdentityJson, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn membership_record_roles(
    roles: Vec<MembershipRecordRoleArg>,
    include_route_authority: bool,
) -> Vec<MembershipRole> {
    let mut roles = roles
        .into_iter()
        .map(MembershipRole::from)
        .collect::<Vec<_>>();
    if roles.is_empty() {
        roles.push(MembershipRole::OverlayMember);
    }
    if include_route_authority && !roles.contains(&MembershipRole::RouteAuthority) {
        roles.push(MembershipRole::RouteAuthority);
    }
    roles.dedup();
    roles
}

fn membership_record_verify(input: &Path, network: Option<&str>) -> Result<(), String> {
    let bytes =
        fs::read(input).map_err(|error| format!("failed to read {}: {error}", input.display()))?;
    let record: SignedMembershipRecord = serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse record: {error}"))?;
    let now = current_unix_seconds_lossy();
    if let Some(network) = network {
        validate_membership_records_at(std::slice::from_ref(&record), network, now)
            .map_err(|error| format!("membership record invalid: {error:?}"))?;
    } else {
        record
            .verify_at(now)
            .map_err(|error| format!("membership record invalid: {error:?}"))?;
    }

    println!("membership record: valid");
    println!("network: {}", record.payload.network_name);
    println!("member peer: {}", record.payload.member_peer);
    println!("issuer peer: {}", record.payload.issuer_peer);
    println!("membership epoch: {}", record.payload.membership_epoch);
    println!("sequence: {}", record.payload.sequence);
    println!("revoked: {}", record.payload.revoked);
    Ok(())
}

fn membership_record_install(
    config_path: &Path,
    record_paths: &[PathBuf],
    output: Option<&Path>,
    force: bool,
) -> Result<(), String> {
    let mut config =
        Config::load(config_path).map_err(|error| format!("failed to load config: {error:?}"))?;
    let records = record_paths
        .iter()
        .map(|path| read_membership_record(path))
        .collect::<Result<Vec<_>, _>>()?;
    let now = current_unix_seconds_lossy();
    validate_membership_records_at(&records, &config.network.name, now)
        .map_err(|error| format!("membership record invalid: {error:?}"))?;
    let stats = install_config_membership_records(&mut config.network.member_records, &records);
    let output_path = output.unwrap_or(config_path);
    let overwrite = force || output.is_none() || output_path == config_path;

    write_config_output(&config, output_path, overwrite)?;
    if output_path.to_string_lossy() != "-" {
        println!("membership records accepted: {}", stats.accepted);
        println!(
            "membership records ignored stale or equal: {}",
            stats.ignored_stale_or_equal
        );
        println!(
            "membership records configured: {}",
            config.network.member_records.len()
        );
    }

    Ok(())
}

fn membership_record_list(config_path: &Path) -> Result<(), String> {
    let config =
        Config::load(config_path).map_err(|error| format!("failed to load config: {error:?}"))?;
    for line in membership_record_lines(&config)? {
        println!("{line}");
    }

    Ok(())
}

fn membership_record_lines(config: &Config) -> Result<Vec<String>, String> {
    let now = current_unix_seconds_lossy();
    validate_membership_records_at(&config.network.member_records, &config.network.name, now)
        .map_err(|error| format!("membership record invalid: {error:?}"))?;
    let mut issuers = BTreeSet::new();
    for record in &config.network.member_records {
        issuers.insert(record.payload.issuer_peer.clone());
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "membership records configured: {}",
        config.network.member_records.len()
    ));
    lines.push("membership records valid: true".to_owned());
    lines.push(format!("trusted issuers: {}", issuers.len()));
    for issuer in issuers {
        lines.push(format!("trusted issuer: {issuer}"));
    }

    let mut records = config.network.member_records.iter().collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.payload
            .member_peer
            .cmp(&right.payload.member_peer)
            .then(
                left.payload
                    .membership_epoch
                    .cmp(&right.payload.membership_epoch),
            )
            .then(left.payload.sequence.cmp(&right.payload.sequence))
    });
    lines.push(format!(
        "membership record entries: {}",
        config.network.member_records.len()
    ));
    for record in records {
        push_membership_record_line(&mut lines, record);
    }

    let effective = config
        .effective_membership()
        .map_err(|error| format!("failed to compute effective membership: {error:?}"))?;
    let mut effective_members = effective.overlay_members().collect::<Vec<_>>();
    effective_members.sort_by_key(|member| member.transport_peer.to_string());
    lines.push(format!(
        "effective overlay members: {}",
        effective_members.len()
    ));
    for member in effective_members {
        lines.push(format!(
            "effective member: {} epoch {} sequence {} roles {} route_grants {}",
            member.transport_peer,
            member.membership_epoch,
            member.sequence,
            membership_roles_text(&member.roles),
            member.route_grants.len()
        ));
        for route in &member.route_grants {
            lines.push(format!(
                "effective member route grant: {} {} metric {}",
                member.transport_peer, route.prefix, route.metric
            ));
        }
    }

    Ok(lines)
}

fn push_membership_record_line(lines: &mut Vec<String>, record: &SignedMembershipRecord) {
    let payload = &record.payload;
    let state = if payload.revoked { "revoked" } else { "active" };
    let trust_root = payload.issuer_peer == payload.member_peer;
    let expires_at = payload
        .expires_at_unix_seconds
        .map_or_else(|| "never".to_owned(), |expires| expires.to_string());
    lines.push(format!(
        "membership record: member {} issuer {} epoch {} sequence {} state {} roles {} route_grants {} expires_at {} trust_root {}",
        payload.member_peer,
        payload.issuer_peer,
        payload.membership_epoch,
        payload.sequence,
        state,
        membership_roles_text(&payload.roles),
        payload.route_grants.len(),
        expires_at,
        trust_root
    ));
    for route in &payload.route_grants {
        lines.push(format!(
            "membership record route grant: member {} {} metric {}",
            payload.member_peer, route.prefix, route.metric
        ));
    }
}

fn membership_roles_text(roles: &[MembershipRole]) -> String {
    if roles.is_empty() {
        return "none".to_owned();
    }
    roles
        .iter()
        .map(|role| match role {
            MembershipRole::OverlayMember => "overlay_member",
            MembershipRole::RouteAuthority => "route_authority",
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn read_membership_record(path: &Path) -> Result<SignedMembershipRecord, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn install_config_membership_records(
    records: &mut Vec<SignedMembershipRecord>,
    incoming: &[SignedMembershipRecord],
) -> MembershipRecordInstallStats {
    let merge_stats = merge_config_membership_records(records, incoming);
    MembershipRecordInstallStats {
        accepted: merge_stats.accepted,
        ignored_stale_or_equal: merge_stats.ignored_stale_or_equal,
    }
}

fn merge_config_membership_records(
    records: &mut Vec<SignedMembershipRecord>,
    incoming: &[SignedMembershipRecord],
) -> MembershipRecordMergeStats {
    let mut stats = MembershipRecordMergeStats::default();
    for incoming_record in incoming {
        let existing_index = records.iter().position(|record| {
            record.payload.issuer_peer == incoming_record.payload.issuer_peer
                && record.payload.member_peer == incoming_record.payload.member_peer
        });
        if let Some(index) = existing_index {
            let existing = &records[index];
            if (
                incoming_record.payload.membership_epoch,
                incoming_record.payload.sequence,
            ) > (existing.payload.membership_epoch, existing.payload.sequence)
            {
                records[index] = incoming_record.clone();
                stats.accepted += 1;
            } else {
                stats.ignored_stale_or_equal += 1;
            }
        } else {
            records.push(incoming_record.clone());
            stats.accepted += 1;
        }
    }
    stats
}

fn write_json_output<T: Serialize>(
    value: &T,
    output: &Path,
    force: bool,
    label: &str,
) -> Result<(), String> {
    if !force && output.to_string_lossy() != "-" && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|error| format!("failed to render {label}: {error}"))?;
    if output.to_string_lossy() == "-" {
        println!("{rendered}");
    } else {
        fs::write(output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
        println!("wrote {}", output.display());
    }
    Ok(())
}

fn init_config(args: InitConfigArgs) -> Result<(), String> {
    if !args.force && args.output.to_string_lossy() != "-" && args.output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            args.output.display()
        ));
    }

    if let Some(peer) = args
        .bootstrap_peers
        .iter()
        .chain(args.relay_peers.iter())
        .find(|peer| peer.address.is_none())
    {
        return Err(format!(
            "bootstrap and relay infrastructure peer {} must include an address as PEER_ID=MULTIADDR",
            peer.id
        ));
    }
    if args.public_ipfs_profile && !args.discovery.kademlia {
        return Err("--public-ipfs-profile requires Kademlia discovery".to_owned());
    }
    if args.ipfs_bootstrap_peers {
        if !args.discovery.kademlia {
            return Err("--ipfs-bootstrap-peers requires Kademlia discovery".to_owned());
        }
        if args.discovery.kademlia_protocol != PUBLIC_IPFS_KADEMLIA_PROTOCOL {
            return Err(
                "--ipfs-bootstrap-peers requires --ipfs-kademlia or --kademlia-protocol /ipfs/kad/1.0.0"
                    .to_owned(),
            );
        }
    }

    let identity = match args.private_key {
        Some(private_key) => NodeIdentity::from_private_key(&private_key)
            .map_err(|error| format!("failed to decode private key: {error:?}"))?,
        None => NodeIdentity::generate_ed25519()
            .map_err(|error| format!("failed to generate key: {error:?}"))?,
    };
    let relay_reservations = init_relay_reservations(&args.relay_peers)?;
    let bootstrap_peers = init_bootstrap_peers(
        args.bootstrap_peers,
        args.relay_peers,
        args.ipfs_bootstrap_peers,
    );
    let mut relay = args.relay;
    relay.reservations.extend(relay_reservations);
    let listen_addresses = if args.listen_addresses.is_empty() {
        default_listen_addresses()
    } else {
        args.listen_addresses
    };
    let mut config = InitConfigTemplate {
        identity,
        network_name: args.network,
        membership_key: args.membership_key,
        vpn_ip: args.vpn_ip,
        local_routes: args
            .local_routes
            .into_iter()
            .map(|route| route.route)
            .collect(),
        interface_name: args.interface,
        mtu: args.mtu,
        listen_addresses,
        external_addresses: args.external_addresses,
        packet_plane: args.packet_plane,
        bootstrap_peers,
        peers: init_peers(args.peers, args.peer_vpn_ips, args.peer_routes),
        discovery: args.discovery,
        relay,
    }
    .into_config();
    config.network.previous_membership_tags = args.previous_membership_tags;
    config.queue = args.queue;
    config.resources = args.resources;
    config
        .validate_runtime()
        .map_err(|error| format!("generated config is invalid: {error:?}"))?;
    let rendered_config = compact_generated_config(config.clone());
    let rendered = serde_json::to_string_pretty(&rendered_config)
        .map_err(|error| format!("failed to render config: {error}"))?;

    if args.output.to_string_lossy() == "-" {
        println!("{rendered}");
    } else {
        fs::write(&args.output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", args.output.display()))?;
        println!("wrote {}", args.output.display());
        println!(
            "local peer: {}",
            config
                .local_peer()
                .map_err(|error| format!("failed to resolve local peer: {error:?}"))?
        );
    }

    Ok(())
}

fn compact_generated_config(mut config: Config) -> Config {
    if config.network.private_key.is_some() {
        config.network.local_peer.clear();
    }
    if config
        .local_peer_id()
        .is_ok_and(|peer| config.network.vpn_ip.as_deref() == Some(&builtin_ipv4(peer).to_string()))
    {
        config.network.vpn_ip = None;
    }

    config
}

fn invite_export(
    config_path: &Path,
    output: &Path,
    options: InviteExportOptions,
    force: bool,
) -> Result<(), String> {
    if !force && output.to_string_lossy() != "-" && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }
    let config =
        Config::load(config_path).map_err(|error| format!("failed to load config: {error:?}"))?;
    let invite = export_signed_invite(&config, options)
        .map_err(|error| format!("failed to export invite: {error:?}"))?;
    let rendered = serde_json::to_string_pretty(&invite)
        .map_err(|error| format!("failed to render invite: {error}"))?;

    if output.to_string_lossy() == "-" {
        println!("{rendered}");
    } else {
        fs::write(output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
        println!("wrote {}", output.display());
        println!("invite network: {}", invite.payload.network_name);
        println!("inviter peer: {}", invite.payload.inviter_peer);
        println!("membership epoch: {}", invite.payload.membership_epoch);
    }

    Ok(())
}

fn invite_import(args: InviteImportArgs) -> Result<(), String> {
    if !args.force && args.output.to_string_lossy() != "-" && args.output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            args.output.display()
        ));
    }
    let bytes = fs::read(&args.invite)
        .map_err(|error| format!("failed to read {}: {error}", args.invite.display()))?;
    let invite: SignedInvite = serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse invite: {error}"))?;
    let identity = match args.private_key {
        Some(private_key) => NodeIdentity::from_private_key(&private_key)
            .map_err(|error| format!("failed to decode private key: {error:?}"))?,
        None => NodeIdentity::generate_ed25519()
            .map_err(|error| format!("failed to generate identity: {error:?}"))?,
    };
    let config = import_invite_config(
        &invite,
        InviteImportOptions {
            identity,
            interface_name: args.interface,
            mtu: args.mtu,
            local_routes: args
                .local_routes
                .into_iter()
                .map(|route| route.route)
                .collect(),
            peer_name: args.peer_name,
        },
    )
    .map_err(|error| format!("failed to import invite: {error:?}"))?;
    let rendered = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("failed to render config: {error}"))?;

    if args.output.to_string_lossy() == "-" {
        println!("{rendered}");
    } else {
        fs::write(&args.output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", args.output.display()))?;
        println!("wrote {}", args.output.display());
        println!(
            "local peer: {}",
            config
                .local_peer()
                .map_err(|error| format!("failed to resolve local peer: {error:?}"))?
        );
        println!("invited by: {}", invite.payload.inviter_peer);
    }

    Ok(())
}

async fn pair_daemon_open(
    target: &PairDaemonTarget,
    expires_in_seconds: u64,
    format: PairOutputFormat,
) -> Result<(), String> {
    let result = pair_daemon_rpc(
        target,
        PairRpcRequest::PairOpen {
            operation_id: fresh_pairing_operation_id(),
            expires_in_seconds,
        },
    )
    .await?;
    let PairRpcResult::OpenStarted(started) = result else {
        return Err("daemon returned an unexpected response to pair open".to_owned());
    };
    match format {
        PairOutputFormat::Json => print_pair_json(&started, "pair open result"),
        PairOutputFormat::Text => {
            println!("operation: {}", started.operation_id);
            println!("network: {}", started.network_name);
            println!("local peer: {}", started.local_peer);
            println!("pairing code: {}", started.code);
            println!("expires at: {}", started.expires_at_unix_seconds);
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn pair_daemon_join(
    target: &PairDaemonTarget,
    code: &str,
    timeout_seconds: u64,
    requested_vpn_ip: Option<String>,
    requested_routes: Vec<LocalRouteArg>,
    wait: bool,
    format: PairOutputFormat,
) -> Result<(), String> {
    let result = pair_daemon_rpc(
        target,
        PairRpcRequest::PairJoin {
            operation_id: fresh_pairing_operation_id(),
            code: code.to_owned(),
            timeout_seconds,
            requested_vpn_ip,
            requested_routes: Some(pair_rpc_routes(requested_routes)),
        },
    )
    .await?;
    let PairRpcResult::JoinStarted(started) = result else {
        return Err("daemon returned an unexpected response to pair join".to_owned());
    };
    if !wait {
        return match format {
            PairOutputFormat::Json => print_pair_json(&started, "pair join result"),
            PairOutputFormat::Text => {
                println!("operation: {}", started.operation_id);
                println!("network: {}", started.network_name);
                println!("local peer: {}", started.local_peer);
                println!("expires at: {}", started.expires_at_unix_seconds);
                Ok(())
            }
        };
    }

    if format == PairOutputFormat::Text {
        println!("operation: {}", started.operation_id);
        println!("waiting for inviter approval");
    }
    let status = wait_for_pairing_terminal(target, &started.operation_id, timeout_seconds).await?;
    print_pair_status(&status, format)
}

async fn pair_daemon_status(
    target: &PairDaemonTarget,
    operation_id: &str,
    format: PairOutputFormat,
) -> Result<(), String> {
    let status = pair_daemon_status_result(target, operation_id).await?;
    print_pair_status(&status, format)
}

async fn pair_daemon_approve(
    target: &PairDaemonTarget,
    operation_id: &str,
    approval_id: &str,
    assigned_vpn_ip: Option<String>,
    granted_routes: Vec<LocalRouteArg>,
    format: PairOutputFormat,
) -> Result<(), String> {
    pair_daemon_action(
        target,
        PairRpcRequest::PairApprove {
            operation_id: operation_id.to_owned(),
            approval_id: approval_id.to_owned(),
            assigned_vpn_ip,
            granted_routes: pair_rpc_routes(granted_routes),
        },
        format,
    )
    .await
}

async fn pair_daemon_action(
    target: &PairDaemonTarget,
    request: PairRpcRequest,
    format: PairOutputFormat,
) -> Result<(), String> {
    let result = pair_daemon_rpc(target, request).await?;
    let status = match result {
        PairRpcResult::ActionAccepted(status) | PairRpcResult::OperationStatus(status) => *status,
        _ => return Err("daemon returned an unexpected pairing action response".to_owned()),
    };
    print_pair_status(&status, format)
}

async fn pair_daemon_artifacts(
    target: &PairDaemonTarget,
    operation_id: &str,
    output: &Path,
    nixos_instance: Option<&str>,
    force: bool,
) -> Result<(), String> {
    if !force && output.to_string_lossy() != "-" && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }
    let result = pair_daemon_rpc(
        target,
        PairRpcRequest::PairArtifacts {
            operation_id: operation_id.to_owned(),
        },
    )
    .await?;
    let PairRpcResult::Artifacts(mut artifacts) = result else {
        return Err("daemon returned an unexpected pairing artifacts response".to_owned());
    };
    if let Some(instance) = pair_artifact_nixos_instance(target, nixos_instance) {
        validate_nixos_instance_name(instance)?;
        instance.clone_into(&mut artifacts.nix.instance_name);
    }
    let rendered = render_pair_rpc_nixos_module(&artifacts)?;
    if output.to_string_lossy() == "-" {
        eprintln!("pairing operation: {operation_id}");
        eprintln!("pairing receipt: {}", artifacts.receipt.transcript_sha256);
        println!("{rendered}");
    } else {
        fs::write(output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
        println!("wrote {}", output.display());
        println!("pairing operation: {operation_id}");
        println!("pairing receipt: {}", artifacts.receipt.transcript_sha256);
    }
    Ok(())
}

async fn pair_daemon_acknowledge(
    target: &PairDaemonTarget,
    operation_id: &str,
    transcript_sha256: &str,
    format: PairOutputFormat,
) -> Result<(), String> {
    let result = pair_daemon_rpc(
        target,
        PairRpcRequest::PairAcknowledge {
            operation_id: operation_id.to_owned(),
            transcript_sha256: transcript_sha256.to_owned(),
        },
    )
    .await?;
    let PairRpcResult::Acknowledged(receipt) = result else {
        return Err("daemon returned an unexpected pairing acknowledgement response".to_owned());
    };
    if format == PairOutputFormat::Json {
        return print_pair_json(&receipt, "pairing acknowledgement");
    }
    println!("network: {}", receipt.network_name);
    println!("local peer: {}", receipt.local_peer);
    println!("remote peer: {}", receipt.remote_peer);
    println!("role: {}", pair_role_name(receipt.role));
    println!("receipt: {}", receipt.transcript_sha256);
    println!("enrollment state: compacted");
    Ok(())
}

fn pair_artifact_nixos_instance<'a>(
    target: &'a PairDaemonTarget,
    explicit_instance: Option<&'a str>,
) -> Option<&'a str> {
    explicit_instance.or(target.instance.as_deref())
}

async fn pair_daemon_rpc(
    target: &PairDaemonTarget,
    request: PairRpcRequest,
) -> Result<PairRpcResult, String> {
    let (socket, timeout) = pair_daemon_target(target)?;
    let response = query_pair_rpc(&socket, timeout, &PairRpcRequestEnvelope::new(request))
        .await
        .map_err(|error| pair_rpc_query_error(&socket, error))?;
    pair_rpc_result(response)
}

fn pair_daemon_target(target: &PairDaemonTarget) -> Result<(PathBuf, Duration), String> {
    if target.rpc_timeout_seconds == 0 || target.rpc_timeout_seconds > 300 {
        return Err("--rpc-timeout-seconds must be between 1 and 300".to_owned());
    }
    let socket = if let Some(instance) = target.instance.as_deref() {
        validate_nixos_instance_name(instance).map_err(|_| {
            "--instance must start with an ASCII letter or digit and contain only letters, digits, dots, underscores, or hyphens"
                .to_owned()
        })?;
        PathBuf::from(format!("/run/p2p-vpn-{instance}/control.sock"))
    } else {
        target
            .socket
            .clone()
            .unwrap_or_else(|| PathBuf::from("/run/p2p-vpn/control.sock"))
    };
    Ok((socket, Duration::from_secs(target.rpc_timeout_seconds)))
}

fn pair_rpc_result(response: PairRpcResponseEnvelope) -> Result<PairRpcResult, String> {
    match response.outcome {
        PairRpcOutcome::Ok { result } => Ok(result),
        PairRpcOutcome::Error { error } => Err(pair_rpc_remote_error(&error)),
    }
}

fn pair_rpc_query_error(socket: &Path, error: PairRpcQueryError) -> String {
    match error {
        PairRpcQueryError::Io(error) => {
            format!(
                "failed to query pairing daemon at {}: {error}",
                socket.display()
            )
        }
        PairRpcQueryError::TimedOut => {
            format!("pairing daemon at {} timed out", socket.display())
        }
        PairRpcQueryError::InvalidRequest(message) => {
            format!("invalid local pairing RPC request: {message}")
        }
        PairRpcQueryError::InvalidResponse(message) => {
            format!("pairing daemon returned an invalid response: {message}")
        }
    }
}

fn pair_rpc_remote_error(error: &PairRpcError) -> String {
    format!(
        "pairing daemon rejected the request ({:?}, retryable={}): {}",
        error.code, error.retryable, error.message
    )
}

async fn pair_daemon_status_result(
    target: &PairDaemonTarget,
    operation_id: &str,
) -> Result<PairRpcOperationStatus, String> {
    let result = pair_daemon_rpc(
        target,
        PairRpcRequest::PairStatus {
            operation_id: operation_id.to_owned(),
        },
    )
    .await?;
    match result {
        PairRpcResult::OperationStatus(status) | PairRpcResult::ActionAccepted(status) => {
            Ok(*status)
        }
        _ => Err("daemon returned an unexpected pair status response".to_owned()),
    }
}

async fn wait_for_pairing_terminal(
    target: &PairDaemonTarget,
    operation_id: &str,
    timeout_seconds: u64,
) -> Result<PairRpcOperationStatus, String> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(timeout_seconds.saturating_add(5)))
        .ok_or_else(|| "pairing wait timeout is too large".to_owned())?;
    loop {
        let status = pair_daemon_status_result(target, operation_id).await?;
        match status.phase {
            PairRpcPhase::Completed => return Ok(status),
            PairRpcPhase::Rejected
            | PairRpcPhase::Cancelled
            | PairRpcPhase::Expired
            | PairRpcPhase::Failed => {
                let reason = status.failure.as_ref().map_or_else(
                    || pair_phase_name(status.phase).to_owned(),
                    |failure| failure.message.clone(),
                );
                return Err(format!("pairing did not complete: {reason}"));
            }
            PairRpcPhase::WaitingForPeer
            | PairRpcPhase::Discovering
            | PairRpcPhase::Authenticating
            | PairRpcPhase::AwaitingApproval
            | PairRpcPhase::Finalizing => {}
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for pairing operation {operation_id}"
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn print_pair_status(
    status: &PairRpcOperationStatus,
    format: PairOutputFormat,
) -> Result<(), String> {
    if format == PairOutputFormat::Json {
        return print_pair_json(status, "pair status");
    }
    println!("operation: {}", status.operation_id);
    println!("network: {}", status.network_name);
    println!("local peer: {}", status.local_peer);
    println!("role: {}", pair_role_name(status.role));
    println!("phase: {}", pair_phase_name(status.phase));
    if let Some(discovery) = status.discovery {
        println!("discovery: {discovery:?}");
    }
    println!("LAN candidates: {}", status.diagnostics.lan_candidates);
    println!(
        "pairing attempts: {} (retries {})",
        status.diagnostics.handshake_attempts, status.diagnostics.handshake_retries
    );
    println!(
        "public discovery: providers {} lookups {} advertisements {}",
        status.diagnostics.public_providers_found,
        status.diagnostics.public_lookups,
        status.diagnostics.public_provider_attempts
    );
    println!(
        "route recovery: {} (transport failures {})",
        status.diagnostics.route_recovery_active, status.diagnostics.poll_transport_failures
    );
    if let Some(transport) = status.diagnostics.selected_transport {
        println!("pairing transport: {transport:?}");
    }
    if let Some(candidate) = &status.candidate {
        println!("approval: {}", candidate.approval_id);
        println!("candidate peer: {}", candidate.peer_id);
        println!(
            "candidate key fingerprint: {}",
            candidate.public_key_fingerprint
        );
        if let Some(vpn_ip) = &candidate.requested_vpn_ip {
            println!("requested VPN IP: {vpn_ip}");
        }
        for route in &candidate.requested_routes {
            println!("requested route: {} metric {}", route.prefix, route.metric);
        }
    }
    println!("artifacts ready: {}", status.artifacts_ready);
    if let Some(failure) = &status.failure {
        println!("failure: {}", failure.message);
    }
    Ok(())
}

fn print_pair_json(value: &impl Serialize, label: &str) -> Result<(), String> {
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|error| format!("failed to render {label}: {error}"))?;
    println!("{rendered}");
    Ok(())
}

const fn pair_role_name(role: PairRpcRole) -> &'static str {
    match role {
        PairRpcRole::Inviter => "inviter",
        PairRpcRole::Joiner => "joiner",
    }
}

const fn pair_phase_name(phase: PairRpcPhase) -> &'static str {
    match phase {
        PairRpcPhase::WaitingForPeer => "waiting_for_peer",
        PairRpcPhase::Discovering => "discovering",
        PairRpcPhase::Authenticating => "authenticating",
        PairRpcPhase::AwaitingApproval => "awaiting_approval",
        PairRpcPhase::Finalizing => "finalizing",
        PairRpcPhase::Completed => "completed",
        PairRpcPhase::Rejected => "rejected",
        PairRpcPhase::Cancelled => "cancelled",
        PairRpcPhase::Expired => "expired",
        PairRpcPhase::Failed => "failed",
    }
}

fn pair_rpc_routes(routes: Vec<LocalRouteArg>) -> Vec<PairRpcRoute> {
    routes
        .into_iter()
        .map(|route| PairRpcRoute {
            prefix: route.route.prefix,
            metric: route.route.metric,
        })
        .collect()
}

fn render_pair_rpc_nixos_module(artifacts: &PairRpcCompletionArtifacts) -> Result<String, String> {
    let plan = &artifacts.nix;
    validate_nixos_instance_name(&plan.instance_name)?;
    let config = pair_rpc_nixos_config(artifacts);
    render_pairing_nixos_module(
        &plan.instance_name,
        &config,
        &PairingNixosSecretPaths {
            private_key_file: None,
            membership_key_file: plan.membership_key_file.clone(),
            membership_key_file_is_default: plan.membership_key_file.is_some(),
        },
    )
}

fn pair_rpc_nixos_config(artifacts: &PairRpcCompletionArtifacts) -> Config {
    let plan = &artifacts.nix;
    let records = plan
        .member_records
        .iter()
        .map(pair_rpc_membership_record_to_config)
        .collect();
    Config {
        network: NetworkConfig {
            name: plan.network_name.clone(),
            local_peer: plan.local_peer.clone(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            member_records: records,
            vpn_ip: plan.assigned_vpn_ip.clone(),
            routes: Vec::new(),
            listen_addresses: default_listen_addresses(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
        },
        interface: InterfaceConfig {
            name: "pv0".to_owned(),
            mtu: 1_280,
        },
        peers: Vec::new(),
        queue: QueueConfig::default(),
        resources: ResourceConfig::default(),
    }
}

fn pair_rpc_membership_record_to_config(
    record: &PairRpcSignedMembershipRecord,
) -> SignedMembershipRecord {
    SignedMembershipRecord {
        payload: MembershipRecordPayload {
            version: record.payload.version,
            network_name: record.payload.network_name.clone(),
            member_peer: record.payload.member_peer.clone(),
            member_public_key: record.payload.member_public_key.clone(),
            issuer_peer: record.payload.issuer_peer.clone(),
            issuer_public_key: record.payload.issuer_public_key.clone(),
            membership_epoch: record.payload.membership_epoch,
            sequence: record.payload.sequence,
            revoked: record.payload.revoked,
            roles: record
                .payload
                .roles
                .iter()
                .map(|role| match role {
                    PairRpcMembershipRole::OverlayMember => MembershipRole::OverlayMember,
                    PairRpcMembershipRole::RouteAuthority => MembershipRole::RouteAuthority,
                })
                .collect(),
            route_grants: record
                .payload
                .route_grants
                .iter()
                .map(pair_rpc_route_to_config)
                .collect(),
            issued_at_unix_seconds: record.payload.issued_at_unix_seconds,
            expires_at_unix_seconds: record.payload.expires_at_unix_seconds,
        },
        signature: record.signature.clone(),
    }
}

fn pair_rpc_route_to_config(route: &PairRpcRoute) -> RouteConfig {
    RouteConfig {
        prefix: route.prefix.clone(),
        metric: route.metric,
    }
}

fn pair_offer(args: PairOfferArgs) -> Result<(), String> {
    if !args.force && args.output.to_string_lossy() != "-" && args.output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            args.output.display()
        ));
    }
    let config_path =
        pair_offer_config_path(args.config.as_deref(), args.nixos_instance.as_deref())?;
    let config =
        Config::load(&config_path).map_err(|error| format!("failed to load config: {error:?}"))?;
    let export_offer = if args.discovery_only {
        export_discovery_only_pairing_offer
    } else {
        export_pairing_offer
    };
    let offer = export_offer(
        &config,
        PairingOfferOptions {
            expires_in_seconds: args.expires_in_seconds,
            rendezvous_token: args.rendezvous_token,
        },
    )
    .map_err(|error| format!("failed to export pairing offer: {error:?}"))?;
    let uri = offer
        .to_uri()
        .map_err(|error| format!("failed to render pairing URI: {error:?}"))?;

    if args.output.to_string_lossy() == "-" {
        println!("{uri}");
    } else {
        fs::write(&args.output, format!("{uri}\n"))
            .map_err(|error| format!("failed to write {}: {error}", args.output.display()))?;
        println!("wrote {}", args.output.display());
        println!("pairing network: {}", offer.payload.network_name);
        println!("inviter peer: {}", offer.payload.inviter_peer);
        println!("expires at: {}", offer.payload.expires_at_unix_seconds);
    }

    Ok(())
}

fn pair_offer_config_path(
    config: Option<&Path>,
    nixos_instance: Option<&str>,
) -> Result<PathBuf, String> {
    match (config, nixos_instance) {
        (Some(_), Some(_)) => {
            Err("--config and --nixos-instance cannot be used together".to_owned())
        }
        (Some(config), None) => Ok(config.to_path_buf()),
        (None, Some(instance)) => {
            validate_nixos_instance_name(instance)?;
            Ok(PathBuf::from(format!(
                "/run/p2p-vpn-{instance}/config.json"
            )))
        }
        (None, None) => Ok(PathBuf::from("p2p-vpn.json")),
    }
}

fn pair_inspect(args: &PairInspectArgs) -> Result<(), String> {
    let input = read_pairing_offer_input(&args.offer)?;
    let offer = PairingOffer::from_uri(&input)
        .map_err(|error| format!("failed to parse pairing offer: {error:?}"))?;
    let now = current_unix_seconds_lossy();
    let verification = match offer.verify_at(now) {
        Ok(()) => "valid",
        Err(PairingError::Expired { .. }) => {
            offer
                .verify_at(offer.payload.issued_at_unix_seconds)
                .map_err(|error| format!("failed to verify expired pairing offer: {error:?}"))?;
            "expired"
        }
        Err(error) => return Err(format!("failed to verify pairing offer: {error:?}")),
    };
    let inviter_peer = offer
        .payload
        .inviter_peer
        .parse::<Libp2pPeerId>()
        .map_err(|error| format!("invalid inviter peer in offer: {error:?}"))?;
    let inviter_addresses = pairing_inviter_addresses(&offer, inviter_peer)?;
    let bootstrap_peers = pairing_bootstrap_peers(&offer)?;
    let seconds_remaining = offer
        .payload
        .expires_at_unix_seconds
        .saturating_sub(now)
        .to_string();

    println!("pairing offer: {verification}");
    println!("network: {}", offer.payload.network_name);
    println!("inviter peer: {}", offer.payload.inviter_peer);
    println!("issued at: {}", offer.payload.issued_at_unix_seconds);
    println!("expires at: {}", offer.payload.expires_at_unix_seconds);
    println!("seconds remaining: {seconds_remaining}");
    println!(
        "discovery only: {}",
        if offer.payload.inviter_addresses.is_empty() {
            "yes"
        } else {
            "no"
        }
    );
    println!("inviter address hints: {}", inviter_addresses.len());
    for (_, address) in &inviter_addresses {
        println!("inviter address: {address}");
    }
    println!(
        "relay reservation hints: {}",
        offer.payload.relay_reservations.len()
    );
    println!("bootstrap peers: {}", bootstrap_peers.len());
    println!("mdns: {}", enabled_label(offer.payload.discovery.mdns));
    println!(
        "kademlia: {}",
        enabled_label(offer.payload.discovery.kademlia)
    );
    println!(
        "kademlia protocol: {}",
        offer.payload.discovery.kademlia_protocol
    );
    println!("dcutr: {}", enabled_label(offer.payload.discovery.dcutr));
    println!("control protocol: {}", offer.payload.protocols.control);
    println!("packet protocol: {}", offer.payload.protocols.packet);
    println!("service protocol: {}", offer.payload.protocols.service);
    if args.show_secret {
        println!("rendezvous token: {}", offer.payload.rendezvous_token);
    } else {
        println!("rendezvous token: hidden");
    }

    Ok(())
}

fn read_pairing_offer_input(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.starts_with("p2pvpn:") {
        return Ok(trimmed.to_owned());
    }

    let path = Path::new(trimmed);
    if path.exists() {
        return fs::read_to_string(path)
            .map(|contents| contents.trim().to_owned())
            .map_err(|error| format!("failed to read {}: {error}", path.display()));
    }

    Ok(trimmed.to_owned())
}

fn enabled_label(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

async fn pair_accept(args: PairAcceptArgs) -> Result<(), String> {
    let input = read_pairing_offer_input(&args.offer)?;
    let offer = PairingOffer::from_uri(&input)
        .map_err(|error| format!("failed to parse pairing offer: {error:?}"))?;
    offer
        .verify_at(current_unix_seconds_lossy())
        .map_err(|error| format!("failed to verify pairing offer: {error:?}"))?;
    validate_pair_accept_outputs(
        &args.output,
        args.nixos_output.as_deref(),
        args.nixos_instance.as_deref(),
        args.nixos_state_dir.as_deref(),
        args.nixos_only,
        args.force,
    )?;
    let identity = resolve_pair_accept_identity(
        args.private_key.as_deref(),
        args.nixos_output.as_deref(),
        args.nixos_instance.as_deref(),
        args.nixos_state_dir.as_deref(),
        &offer.payload.network_name,
    )?;
    let local_routes = args
        .local_routes
        .into_iter()
        .map(|route| route.route)
        .collect::<Vec<_>>();

    if let Some(response_path) = args.response {
        let bytes = fs::read(&response_path)
            .map_err(|error| format!("failed to read {}: {error}", response_path.display()))?;
        let response: PairingResponse = serde_json::from_slice(&bytes)
            .map_err(|error| format!("failed to parse pairing response: {error}"))?;
        let config = import_pairing_response_config_at(
            &offer,
            &response,
            PairingConfigOptions {
                identity,
                interface_name: args.interface,
                mtu: args.mtu,
                local_routes,
                peer_name: args.peer_name,
            },
            current_unix_seconds_lossy(),
        )
        .map_err(|error| format!("failed to import pairing response: {error:?}"))?;
        write_pairing_config(
            &config,
            &args.output,
            PairingNixosOutputOptions {
                output: args.nixos_output.as_deref(),
                instance: args.nixos_instance.as_deref(),
                nixos_only: args.nixos_only,
                state_dir: args.nixos_state_dir.as_deref(),
                force: args.force,
            },
            response.payload.inviter_peer.as_str(),
        )?;

        return Ok(());
    }

    let requested_vpn_ip = pairing_requested_vpn_ip(&identity, args.vpn_ip.as_deref())?;
    let response = live_pair_accept(
        &offer,
        identity.clone(),
        args.mtu,
        args.timeout_seconds,
        Some(requested_vpn_ip),
        local_routes.clone(),
    )
    .await
    .map_err(|error| format!("live pairing exchange failed: {error}"))?;
    let config = import_pairing_response_config_at(
        &offer,
        &response,
        PairingConfigOptions {
            identity,
            interface_name: args.interface,
            mtu: args.mtu,
            local_routes,
            peer_name: args.peer_name,
        },
        current_unix_seconds_lossy(),
    )
    .map_err(|error| format!("failed to import pairing response: {error:?}"))?;
    write_pairing_config(
        &config,
        &args.output,
        PairingNixosOutputOptions {
            output: args.nixos_output.as_deref(),
            instance: args.nixos_instance.as_deref(),
            nixos_only: args.nixos_only,
            state_dir: args.nixos_state_dir.as_deref(),
            force: args.force,
        },
        response.payload.inviter_peer.as_str(),
    )
}

fn validate_pair_accept_outputs(
    output: &Path,
    nixos_output: Option<&Path>,
    nixos_instance: Option<&str>,
    nixos_state_dir: Option<&Path>,
    nixos_only: bool,
    force: bool,
) -> Result<(), String> {
    if !nixos_only && !force && output.to_string_lossy() != "-" && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }
    let Some(nixos_output) = nixos_output else {
        if nixos_only {
            return Err("--nixos-only requires --nixos-output".to_owned());
        }
        if nixos_instance.is_some() {
            return Err("--nixos-instance requires --nixos-output".to_owned());
        }
        if nixos_state_dir.is_some() {
            return Err("--nixos-state-dir requires --nixos-output".to_owned());
        }
        return Ok(());
    };
    if !nixos_only && output.to_string_lossy() == "-" {
        return Err("--nixos-output requires --output to be a filesystem path".to_owned());
    }
    if !nixos_only && !output.is_absolute() {
        return Err("--nixos-output requires --output to be an absolute config path".to_owned());
    }
    if !nixos_only && output == nixos_output {
        return Err("--output and --nixos-output must be different paths".to_owned());
    }
    if let Some(instance) = nixos_instance {
        validate_nixos_instance_name(instance)?;
    }
    if nixos_state_dir.is_some_and(|path| !path.is_absolute()) {
        return Err("--nixos-state-dir must be an absolute path".to_owned());
    }
    if !force && nixos_output.to_string_lossy() != "-" && nixos_output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            nixos_output.display()
        ));
    }
    Ok(())
}

fn validate_nixos_instance_name(instance: &str) -> Result<(), String> {
    let mut characters = instance.chars();
    let valid = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        });
    if !valid {
        return Err(
            "--nixos-instance must start with an ASCII letter or digit and contain only letters, digits, dots, underscores, or hyphens"
                .to_owned(),
        );
    }
    Ok(())
}

fn default_nixos_instance_name(network_name: &str) -> String {
    let mut normalized = network_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if normalized.is_empty() {
        return "vpn".to_owned();
    }
    if !normalized
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
    {
        normalized.insert_str(0, "vpn-");
    }
    normalized
}

fn pairing_nixos_state_dir(instance: &str, configured: Option<&Path>) -> Result<PathBuf, String> {
    validate_nixos_instance_name(instance)?;
    let state_dir = configured.map_or_else(
        || PathBuf::from("/var/lib/p2p-vpn").join(instance),
        Path::to_path_buf,
    );
    if !state_dir.is_absolute() {
        return Err("--nixos-state-dir must be an absolute path".to_owned());
    }
    if state_dir.starts_with("/nix/store") {
        return Err("--nixos-state-dir must be outside the Nix store".to_owned());
    }
    Ok(state_dir)
}

fn resolve_pair_accept_identity(
    private_key: Option<&str>,
    nixos_output: Option<&Path>,
    nixos_instance: Option<&str>,
    nixos_state_dir: Option<&Path>,
    network_name: &str,
) -> Result<NodeIdentity, String> {
    if let Some(private_key) = private_key {
        return NodeIdentity::from_private_key(private_key)
            .map_err(|error| format!("failed to decode private key: {error:?}"));
    }

    if nixos_output.is_some() {
        let default_instance;
        let instance = if let Some(instance) = nixos_instance {
            instance
        } else {
            default_instance = default_nixos_instance_name(network_name);
            &default_instance
        };
        let state_dir = pairing_nixos_state_dir(instance, nixos_state_dir)?;
        ensure_private_directory(&state_dir)?;
        let private_key_file = state_dir.join("private.key");
        if let Some(private_key) = read_private_secret_file(&private_key_file)? {
            return NodeIdentity::from_private_key(private_key.trim_end_matches(['\r', '\n']))
                .map_err(|error| {
                    format!(
                        "failed to decode existing NixOS identity {}: {error:?}",
                        private_key_file.display()
                    )
                });
        }
    }

    NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate identity: {error:?}"))
}

fn write_pairing_config(
    config: &Config,
    output: &Path,
    nixos: PairingNixosOutputOptions<'_>,
    inviter_peer: &str,
) -> Result<(), String> {
    let rendered_config = compact_generated_config(config.clone());
    let rendered = serde_json::to_string_pretty(&rendered_config)
        .map_err(|error| format!("failed to render config: {error}"))?;

    if nixos.nixos_only {
        println!(
            "local peer: {}",
            config
                .local_peer()
                .map_err(|error| format!("failed to resolve local peer: {error:?}"))?
        );
        println!("paired with: {inviter_peer}");
    } else if output.to_string_lossy() == "-" {
        println!("{rendered}");
    } else {
        fs::write(output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
        println!("wrote {}", output.display());
        println!(
            "local peer: {}",
            config
                .local_peer()
                .map_err(|error| format!("failed to resolve local peer: {error:?}"))?
        );
        println!("paired with: {inviter_peer}");
    }

    if let Some(nixos_output) = nixos.output {
        let default_instance;
        let instance = if let Some(instance) = nixos.instance {
            instance
        } else {
            default_instance = default_nixos_instance_name(&config.network.name);
            &default_instance
        };
        write_pairing_nixos_module(
            instance,
            &rendered_config,
            nixos_output,
            nixos.state_dir,
            nixos.force,
        )?;
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct PairingNixosOutputOptions<'a> {
    output: Option<&'a Path>,
    instance: Option<&'a str>,
    nixos_only: bool,
    state_dir: Option<&'a Path>,
    force: bool,
}

fn write_pairing_nixos_module(
    instance: &str,
    config: &Config,
    output: &Path,
    nixos_state_dir: Option<&Path>,
    force: bool,
) -> Result<(), String> {
    validate_nixos_instance_name(instance)?;
    let secret_paths = write_pairing_nixos_secret_files(config, instance, nixos_state_dir, force)?;
    let rendered = render_pairing_nixos_module(instance, config, &secret_paths)?;
    if output.to_string_lossy() == "-" {
        println!("{rendered}");
    } else {
        fs::write(output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
        println!("wrote {}", output.display());
    }
    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PairingNixosSecretPaths {
    private_key_file: Option<String>,
    membership_key_file: Option<String>,
    membership_key_file_is_default: bool,
}

fn write_pairing_nixos_secret_files(
    config: &Config,
    instance: &str,
    nixos_state_dir: Option<&Path>,
    force: bool,
) -> Result<PairingNixosSecretPaths, String> {
    let state_dir = pairing_nixos_state_dir(instance, nixos_state_dir)?;
    ensure_private_directory(&state_dir)?;

    let private_key_file = config
        .network
        .private_key
        .as_ref()
        .map(|private_key| {
            let path = state_dir.join("private.key");
            write_secret_file(&path, private_key, force)?;
            path_to_string(&path)
        })
        .transpose()?;

    let membership_key_file = config
        .network
        .membership_key
        .as_ref()
        .map(|membership_key| {
            let path = state_dir.join("membership.key");
            write_secret_file(&path, membership_key, force)?;
            path_to_string(&path)
        })
        .transpose()?;

    Ok(PairingNixosSecretPaths {
        private_key_file,
        membership_key_file,
        membership_key_file_is_default: false,
    })
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "paired-key directory must be a real directory: {}",
                    path.display()
                ));
            }
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                return Err(format!(
                    "paired-key directory {} has mode {mode:04o}; use an owner-only directory",
                    path.display()
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(path)
                .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("failed to chmod {}: {error}", path.display()))
        }
        Err(error) => Err(format!(
            "failed to inspect paired-key directory {}: {error}",
            path.display()
        )),
    }
}

fn write_secret_file(path: &Path, value: &str, force: bool) -> Result<(), String> {
    if !force {
        if let Some(existing) = read_private_secret_file(path)? {
            if existing.trim_end_matches(['\r', '\n']) == value {
                println!("kept {}", path.display());
                return Ok(());
            }
            return Err(format!(
                "{} already exists with different content; pass --force to replace it",
                path.display()
            ));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
        if let Err(error) = file.write_all(format!("{value}\n").as_bytes()) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(format!("failed to write {}: {error}", path.display()));
        }
        println!("wrote {}", path.display());
        return Ok(());
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no UTF-8 file name", path.display()))?;
    let mut temporary = None;
    for attempt in 0..100_u8 {
        let candidate =
            path.with_file_name(format!(".{file_name}.{}.{}", std::process::id(), attempt));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!("failed to create {}: {error}", candidate.display()));
            }
        }
    }
    let (temporary_path, mut file) = temporary.ok_or_else(|| {
        format!(
            "failed to create a temporary secret next to {}",
            path.display()
        )
    })?;
    if let Err(error) = file.write_all(format!("{value}\n").as_bytes()) {
        let _ = fs::remove_file(&temporary_path);
        return Err(format!("failed to write {}: {error}", path.display()));
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(format!("failed to replace {}: {error}", path.display()));
    }
    println!("wrote {}", path.display());
    Ok(())
}

fn read_private_secret_file(path: &Path) -> Result<Option<String>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to inspect secret file {}: {error}",
                path.display()
            ));
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "secret file must be a regular file: {}",
            path.display()
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "secret file {} has mode {mode:04o}; use an owner-only file",
            path.display()
        ));
    }
    fs::read_to_string(path)
        .map(Some)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))
}

fn path_to_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
}

#[allow(clippy::too_many_lines)]
fn render_pairing_nixos_module(
    instance: &str,
    config: &Config,
    secret_paths: &PairingNixosSecretPaths,
) -> Result<String, String> {
    validate_nixos_instance_name(instance)?;

    let mut lines = Vec::new();
    if secret_paths.membership_key_file_is_default {
        lines.push("{ lib, ... }:".to_owned());
    }
    lines.extend([
        "{".to_owned(),
        format!(
            "  services.p2p-vpn.instances.{} = {{",
            nix_string_literal(instance)?
        ),
        "    enable = true;".to_owned(),
    ]);
    if config.network.name != instance {
        lines.push(format!(
            "    networkName = {};",
            nix_string_literal(&config.network.name)?
        ));
    }

    let local_peer = config
        .local_peer()
        .map_err(|error| format!("failed to resolve local peer: {error:?}"))?;
    lines.push(format!(
        "    localPeer = {};",
        nix_string_literal(&local_peer)?
    ));
    let default_private_key_file = format!("/var/lib/p2p-vpn/{instance}/private.key");
    if let Some(path) = &secret_paths.private_key_file
        && path != &default_private_key_file
    {
        lines.push(format!(
            "    privateKeyFile = {};",
            nix_string_literal(path)?
        ));
    }
    if let Some(path) = &secret_paths.membership_key_file {
        let value = nix_string_literal(path)?;
        if secret_paths.membership_key_file_is_default {
            lines.push(format!("    membershipKeyFile = lib.mkDefault {value};"));
        } else {
            lines.push(format!("    membershipKeyFile = {value};"));
        }
    }
    if !config.network.previous_membership_tags.is_empty() {
        push_nixos_string_list(
            &mut lines,
            "    previousMembershipTags",
            &config.network.previous_membership_tags,
        )?;
    }
    if !config.network.member_records.is_empty() {
        push_nixos_member_records(&mut lines, &config.network.member_records)?;
    }
    if let Some(vpn_ip) = &config.network.vpn_ip {
        lines.push(format!("    vpnIp = {};", nix_string_literal(vpn_ip)?));
    }
    if !config.network.routes.is_empty() {
        push_nixos_routes(&mut lines, "    routes", &config.network.routes)?;
    }
    if config.network.listen_addresses != default_listen_addresses() {
        push_nixos_string_list(
            &mut lines,
            "    listenAddresses",
            &config.network.listen_addresses,
        )?;
    }
    if !config.network.external_addresses.is_empty() {
        push_nixos_string_list(
            &mut lines,
            "    externalAddresses",
            &config.network.external_addresses,
        )?;
    }
    if !config.network.bootstrap_peers.is_empty() {
        push_nixos_bootstrap_peers(&mut lines, &config.network.bootstrap_peers)?;
    }
    if config.network.discovery != DiscoveryConfig::default() {
        push_nixos_discovery(&mut lines, &config.network.discovery)?;
    }
    push_nixos_relay(&mut lines, &config.network.relay)?;
    if config.network.packet_plane != PacketPlaneConfig::default() {
        push_nixos_packet_plane(&mut lines, &config.network.packet_plane)?;
    }
    if config.interface.name != "pv0" {
        lines.push(format!(
            "    interfaceName = {};",
            nix_string_literal(&config.interface.name)?
        ));
    }
    if config.interface.mtu != 1280 {
        lines.push(format!("    mtu = {};", config.interface.mtu));
    }
    push_nixos_peers(&mut lines, &config.peers)?;
    if config.queue != QueueConfig::default() {
        push_nixos_queue(&mut lines, config.queue);
    }
    if config.resources != ResourceConfig::default() {
        push_nixos_resources(&mut lines, config.resources);
    }

    lines.push("  };".to_owned());
    lines.push("}".to_owned());
    Ok(lines.join("\n"))
}

fn push_nixos_member_records(
    lines: &mut Vec<String>,
    records: &[SignedMembershipRecord],
) -> Result<(), String> {
    lines.push("    memberRecords = [".to_owned());
    for record in records {
        let payload = &record.payload;
        lines.push("      {".to_owned());
        lines.push("        payload = {".to_owned());
        lines.push(format!("          version = {};", payload.version));
        lines.push(format!(
            "          networkName = {};",
            nix_string_literal(&payload.network_name)?
        ));
        lines.push(format!(
            "          memberPeer = {};",
            nix_string_literal(&payload.member_peer)?
        ));
        lines.push(format!(
            "          memberPublicKey = {};",
            nix_string_literal(&payload.member_public_key)?
        ));
        lines.push(format!(
            "          issuerPeer = {};",
            nix_string_literal(&payload.issuer_peer)?
        ));
        lines.push(format!(
            "          issuerPublicKey = {};",
            nix_string_literal(&payload.issuer_public_key)?
        ));
        lines.push(format!(
            "          membershipEpoch = {};",
            payload.membership_epoch
        ));
        lines.push(format!("          sequence = {};", payload.sequence));
        lines.push(format!(
            "          revoked = {};",
            nix_bool(payload.revoked)
        ));
        let roles = payload
            .roles
            .iter()
            .map(|role| match role {
                MembershipRole::OverlayMember => "overlay_member",
                MembershipRole::RouteAuthority => "route_authority",
            })
            .collect::<Vec<_>>();
        push_nixos_string_list(lines, "          roles", &roles)?;
        push_nixos_routes(lines, "          routeGrants", &payload.route_grants)?;
        lines.push(format!(
            "          issuedAtUnixSeconds = {};",
            payload.issued_at_unix_seconds
        ));
        if let Some(expires_at) = payload.expires_at_unix_seconds {
            lines.push(format!("          expiresAtUnixSeconds = {expires_at};"));
        }
        lines.push("        };".to_owned());
        lines.push(format!(
            "        signature = {};",
            nix_string_literal(&record.signature)?
        ));
        lines.push("      }".to_owned());
    }
    lines.push("    ];".to_owned());
    Ok(())
}

fn push_nixos_relay(lines: &mut Vec<String>, relay: &RelayConfig) -> Result<(), String> {
    if relay.server {
        lines.push("    relayServer = true;".to_owned());
    }
    if !relay.reservations.is_empty() {
        push_nixos_string_list(lines, "    relayReservations", &relay.reservations)?;
    }
    if relay.auto != AutoRelayConfig::default() {
        lines.push("    autoRelay = {".to_owned());
        lines.push(format!(
            "      maxCandidates = {};",
            relay.auto.max_candidates
        ));
        lines.push(format!(
            "      maxReservations = {};",
            relay.auto.max_reservations
        ));
        lines.push(format!(
            "      retryIntervalSeconds = {};",
            relay.auto.retry_interval_seconds
        ));
        lines.push("    };".to_owned());
    }
    if relay.resources != RelayResourceConfig::default() {
        let resources = relay.resources;
        lines.push("    relayResources = {".to_owned());
        lines.push(format!(
            "      maxReservations = {};",
            resources.max_reservations
        ));
        lines.push(format!(
            "      maxReservationsPerPeer = {};",
            resources.max_reservations_per_peer
        ));
        lines.push(format!(
            "      reservationDurationSeconds = {};",
            resources.reservation_duration_secs
        ));
        lines.push(format!("      maxCircuits = {};", resources.max_circuits));
        lines.push(format!(
            "      maxCircuitsPerPeer = {};",
            resources.max_circuits_per_peer
        ));
        lines.push(format!(
            "      maxCircuitDurationSeconds = {};",
            resources.max_circuit_duration_secs
        ));
        lines.push(format!(
            "      maxCircuitBytes = {};",
            resources.max_circuit_bytes
        ));
        lines.push("    };".to_owned());
    }
    Ok(())
}

fn push_nixos_packet_plane(
    lines: &mut Vec<String>,
    packet_plane: &PacketPlaneConfig,
) -> Result<(), String> {
    lines.push("    packetPlane = {".to_owned());
    push_nixos_string_list(lines, "      listen", &packet_plane.listen)?;
    if !packet_plane.external_endpoints.is_empty() {
        push_nixos_string_list(
            lines,
            "      externalEndpoints",
            &packet_plane.external_endpoints,
        )?;
    }
    if !packet_plane.quic_listen.is_empty() {
        push_nixos_string_list(lines, "      quicListen", &packet_plane.quic_listen)?;
    }
    if !packet_plane.quic_external_endpoints.is_empty() {
        push_nixos_string_list(
            lines,
            "      quicExternalEndpoints",
            &packet_plane.quic_external_endpoints,
        )?;
    }
    if packet_plane.session_ttl_seconds != default_packet_plane_session_ttl_seconds() {
        lines.push(format!(
            "      sessionTtlSeconds = {};",
            packet_plane.session_ttl_seconds
        ));
    }
    if packet_plane.max_replay_windows_per_session
        != default_packet_plane_replay_windows_per_session()
    {
        lines.push(format!(
            "      maxReplayWindowsPerSession = {};",
            packet_plane.max_replay_windows_per_session
        ));
    }
    lines.push("    };".to_owned());
    Ok(())
}

fn push_nixos_queue(lines: &mut Vec<String>, queue: QueueConfig) {
    lines.push("    queue = {".to_owned());
    lines.push(format!(
        "      maxPacketsPerPeer = {};",
        queue.max_packets_per_peer
    ));
    lines.push(format!(
        "      maxBytesPerPeer = {};",
        queue.max_bytes_per_peer
    ));
    lines.push(format!(
        "      maxPacketAgeMillis = {};",
        queue.max_packet_age_millis
    ));
    lines.push("    };".to_owned());
}

fn push_nixos_resources(lines: &mut Vec<String>, resources: ResourceConfig) {
    lines.push("    resources = {".to_owned());
    lines.push(format!(
        "      maxConcurrentPacketStreams = {};",
        resources.max_concurrent_packet_streams
    ));
    lines.push(format!(
        "      maxConcurrentControlStreams = {};",
        resources.max_concurrent_control_streams
    ));
    lines.push(format!(
        "      maxInboundPacketsPerPeerPerSecond = {};",
        resources.max_inbound_packets_per_peer_per_second
    ));
    lines.push(format!(
        "      maxPairingRequestsPerPeerPerSecond = {};",
        resources.max_pairing_requests_per_peer_per_second
    ));
    lines.push(format!(
        "      maxPendingIncomingConnections = {};",
        resources.max_pending_incoming_connections
    ));
    lines.push(format!(
        "      maxPendingOutgoingConnections = {};",
        resources.max_pending_outgoing_connections
    ));
    lines.push(format!(
        "      maxEstablishedIncomingConnections = {};",
        resources.max_established_incoming_connections
    ));
    lines.push(format!(
        "      maxEstablishedOutgoingConnections = {};",
        resources.max_established_outgoing_connections
    ));
    lines.push(format!(
        "      maxEstablishedConnectionsPerPeer = {};",
        resources.max_established_connections_per_peer
    ));
    lines.push(format!(
        "      maxEstablishedConnections = {};",
        resources.max_established_connections
    ));
    lines.push("    };".to_owned());
}

fn push_nixos_peers(
    lines: &mut Vec<String>,
    peers: &[p2p_vpn::config::PeerConfig],
) -> Result<(), String> {
    if peers.is_empty() {
        return Ok(());
    }
    lines.push("    peers = {".to_owned());
    for peer in peers {
        lines.push(format!("      {} = {{", nix_string_literal(&peer.id)?));
        if let Some(name) = &peer.name {
            lines.push(format!("        name = {};", nix_string_literal(name)?));
        }
        if let Some(ip) = &peer.ip {
            lines.push(format!("        ip = {};", nix_string_literal(ip)?));
        }
        if let Some(vpn_ip) = &peer.vpn_ip {
            lines.push(format!("        vpnIp = {};", nix_string_literal(vpn_ip)?));
        }
        if !peer.addresses.is_empty() {
            push_nixos_string_list(lines, "        addresses", &peer.addresses)?;
        }
        if !peer.routes.is_empty() {
            push_nixos_routes(lines, "        routes", &peer.routes)?;
        }
        lines.push("      };".to_owned());
    }
    lines.push("    };".to_owned());
    Ok(())
}

fn push_nixos_bootstrap_peers(
    lines: &mut Vec<String>,
    peers: &[BootstrapPeerConfig],
) -> Result<(), String> {
    lines.push("    bootstrapPeers = [".to_owned());
    for peer in peers {
        lines.push("      {".to_owned());
        lines.push(format!("        id = {};", nix_string_literal(&peer.id)?));
        lines.push(format!(
            "        address = {};",
            nix_string_literal(&peer.address)?
        ));
        lines.push("      }".to_owned());
    }
    lines.push("    ];".to_owned());
    Ok(())
}

fn push_nixos_discovery(
    lines: &mut Vec<String>,
    discovery: &DiscoveryConfig,
) -> Result<(), String> {
    lines.push("    discovery = {".to_owned());
    lines.push(format!("      mdns = {};", nix_bool(discovery.mdns)));
    lines.push(format!(
        "      kademlia = {};",
        nix_bool(discovery.kademlia)
    ));
    lines.push(format!(
        "      kademliaProviderAdvertisement = {};",
        nix_bool(discovery.kademlia_provider_advertisement)
    ));
    lines.push(format!(
        "      kademliaProtocol = {};",
        nix_string_literal(&discovery.kademlia_protocol)?
    ));
    lines.push(format!("      dcutr = {};", nix_bool(discovery.dcutr)));
    lines.push(format!("      autonat = {};", nix_bool(discovery.autonat)));
    lines.push("    };".to_owned());
    Ok(())
}

fn push_nixos_routes(
    lines: &mut Vec<String>,
    attr: &str,
    routes: &[RouteConfig],
) -> Result<(), String> {
    let indent = nix_attr_indent(attr);
    let item_indent = format!("{indent}  ");
    let field_indent = format!("{item_indent}  ");
    lines.push(format!("{attr} = ["));
    for route in routes {
        if route.metric == 0 {
            lines.push(format!(
                "{item_indent}{}",
                nix_string_literal(&route.prefix)?
            ));
        } else {
            lines.push(format!("{item_indent}{{"));
            lines.push(format!(
                "{field_indent}prefix = {};",
                nix_string_literal(&route.prefix)?
            ));
            lines.push(format!("{field_indent}metric = {};", route.metric));
            lines.push(format!("{item_indent}}}"));
        }
    }
    lines.push(format!("{indent}];"));
    Ok(())
}

fn push_nixos_string_list<T: AsRef<str>>(
    lines: &mut Vec<String>,
    attr: &str,
    values: &[T],
) -> Result<(), String> {
    let indent = nix_attr_indent(attr);
    let item_indent = format!("{indent}  ");
    lines.push(format!("{attr} = ["));
    for value in values {
        lines.push(format!(
            "{item_indent}{}",
            nix_string_literal(value.as_ref())?
        ));
    }
    lines.push(format!("{indent}];"));
    Ok(())
}

fn nix_attr_indent(attr: &str) -> &str {
    &attr[..attr.len() - attr.trim_start().len()]
}

const fn nix_bool(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn nix_string_literal(value: &str) -> Result<String, String> {
    let mut rendered = String::with_capacity(value.len() + 2);
    rendered.push('"');
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' => rendered.push_str("\\\""),
            '\\' => rendered.push_str("\\\\"),
            '\n' => rendered.push_str("\\n"),
            '\r' => rendered.push_str("\\r"),
            '\t' => rendered.push_str("\\t"),
            '$' if characters.peek() == Some(&'{') => rendered.push_str("\\$"),
            control if control.is_control() => {
                return Err("Nix strings cannot contain unsupported control characters".to_owned());
            }
            character => rendered.push(character),
        }
    }
    rendered.push('"');
    Ok(rendered)
}

fn pairing_requested_vpn_ip(
    identity: &NodeIdentity,
    configured: Option<&str>,
) -> Result<String, String> {
    if let Some(configured) = configured {
        configured
            .parse::<IpAddr>()
            .map_err(|error| format!("invalid requested VPN IP `{configured}`: {error}"))?;
        return Ok(configured.to_owned());
    }

    let peer = identity
        .peer_id
        .parse::<Libp2pPeerId>()
        .map_err(|error| format!("invalid local peer ID: {error:?}"))?;
    Ok(builtin_ipv4(PeerId::from_libp2p(peer)).to_string())
}

#[allow(clippy::too_many_lines)]
async fn live_pair_accept(
    offer: &PairingOffer,
    identity: NodeIdentity,
    mtu: u16,
    timeout_seconds: u64,
    requested_vpn_ip: Option<String>,
    requested_routes: Vec<RouteConfig>,
) -> Result<PairingResponse, String> {
    let inviter_peer = offer
        .payload
        .inviter_peer
        .parse::<Libp2pPeerId>()
        .map_err(|error| format!("invalid inviter peer in offer: {error:?}"))?;
    let known_peers = pairing_inviter_addresses(offer, inviter_peer)?;
    let offer_dial_addresses = known_peers
        .iter()
        .filter_map(|(_, address)| pairing_dial_address(inviter_peer, address.clone()))
        .collect::<Vec<_>>();
    let bootstrap_peers = pairing_bootstrap_peers(offer)?;
    let mut diagnostics = PairingAcceptDiagnostics::new(&known_peers, bootstrap_peers.len());
    let mut dialed_inviter_addresses = HashSet::new();
    let mut node = build_node(&HostConfig {
        identity: identity.clone(),
        network_name: offer.payload.network_name.clone(),
        membership_tag: None,
        mtu,
        max_concurrent_control_streams: 16,
        max_concurrent_packet_streams: 16,
        listen_addresses: Vec::new(),
        external_addresses: Vec::new(),
        bootstrap_peers,
        known_peers: known_peers.clone(),
        relay_reservations: Vec::new(),
        relay_server: false,
        relay_resources: RelayResourceConfig::default(),
        resources: ResourceConfig::default(),
        discovery: offer.payload.discovery.clone(),
    })
    .map_err(|error| format!("failed to start pairing libp2p node: {error:?}"))?;
    start_pairing_discovery_queries(&mut node, offer, inviter_peer, &mut diagnostics);
    dial_pairing_inviter_addresses(
        &mut node,
        inviter_peer,
        known_peers.iter().map(|(_, address)| address.clone()),
        &mut dialed_inviter_addresses,
        &mut diagnostics,
        PairingDiscoveredAddressSource::Offer,
    );
    let request = build_pairing_request_at(
        offer,
        PairingRequestOptions {
            identity,
            requested_vpn_ip,
            requested_routes,
        },
        current_unix_seconds_lossy(),
    )
    .map_err(|error| format!("failed to build pairing request: {error:?}"))?;
    diagnostics.record_request_attempt();
    let request_id = node
        .swarm
        .behaviour_mut()
        .pairing
        .send_request(&inviter_peer, request.clone());
    let timeout = Duration::from_secs(timeout_seconds.max(1));

    tokio::time::timeout(timeout, async {
        let mut request_id = request_id;
        loop {
            match node.swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Pairing(
                    libp2p::request_response::Event::Message {
                        message:
                            Message::Response {
                                request_id: received_request_id,
                                response,
                            },
                        ..
                    },
                )) if received_request_id == request_id => return Ok(response),
                SwarmEvent::Behaviour(BehaviourEvent::Pairing(
                    libp2p::request_response::Event::OutboundFailure {
                        request_id: failed_request_id,
                        error,
                        ..
                    },
                )) if failed_request_id == request_id => {
                    diagnostics.record_outbound_failure(&error);
                    eprintln!(
                        "pairing request failed, retrying: {}",
                        diagnostics
                            .last_outbound_failure
                            .as_deref()
                            .unwrap_or("unknown error")
                    );
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    redial_pairing_offer_addresses(
                        &mut node,
                        inviter_peer,
                        &offer_dial_addresses,
                        &mut dialed_inviter_addresses,
                        &mut diagnostics,
                    );
                    diagnostics.record_request_attempt();
                    request_id = node
                        .swarm
                        .behaviour_mut()
                        .pairing
                        .send_request(&inviter_peer, request.clone());
                }
                SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                    diagnostics.record_dial_error(peer_id.as_ref(), &error);
                    eprintln!(
                        "pairing dial error: {}",
                        diagnostics
                            .last_dial_error
                            .as_deref()
                            .unwrap_or("unknown error")
                    );
                }
                SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Discovered(peers)))
                    if offer.payload.discovery.mdns =>
                {
                    for (peer, address) in peers {
                        if peer == inviter_peer {
                            dial_pairing_inviter_addresses(
                                &mut node,
                                inviter_peer,
                                std::iter::once(address),
                                &mut dialed_inviter_addresses,
                                &mut diagnostics,
                                PairingDiscoveredAddressSource::Mdns,
                            );
                        }
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Kad(event))
                    if offer.payload.discovery.kademlia =>
                {
                    handle_pairing_kademlia_event(
                        &mut node,
                        offer,
                        inviter_peer,
                        event,
                        &mut dialed_inviter_addresses,
                        &mut diagnostics,
                    );
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| diagnostics.timeout_error(timeout_seconds))?
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairingDiscoveredAddressSource {
    Offer,
    Mdns,
    KademliaClosestPeer,
    KademliaPeerRecord,
}

impl PairingDiscoveredAddressSource {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Offer => "offer",
            Self::Mdns => "mdns",
            Self::KademliaClosestPeer => "kademlia_closest_peer",
            Self::KademliaPeerRecord => "kademlia_peer_record",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct PairingAcceptDiagnostics {
    inviter_address_hints: usize,
    relayed_inviter_address_hints: usize,
    bootstrap_peers: usize,
    discovery_queries: u64,
    discovery_query_failures: u64,
    discovered_inviter_addresses: u64,
    mdns_inviter_addresses: u64,
    kademlia_inviter_addresses: u64,
    kademlia_provider_results: u64,
    ignored_kademlia_providers: u64,
    request_attempts: u64,
    outbound_failures: u64,
    dial_errors: u64,
    relayed_dial_start_failures: u64,
    last_outbound_failure: Option<String>,
    last_dial_error: Option<String>,
    last_relayed_dial_error: Option<String>,
    last_discovery_failure: Option<String>,
}

impl PairingAcceptDiagnostics {
    fn new(known_peers: &[(Libp2pPeerId, Multiaddr)], bootstrap_peers: usize) -> Self {
        Self {
            inviter_address_hints: known_peers.len(),
            relayed_inviter_address_hints: known_peers
                .iter()
                .filter(|(_, address)| {
                    address
                        .iter()
                        .any(|protocol| matches!(protocol, libp2p::multiaddr::Protocol::P2pCircuit))
                })
                .count(),
            bootstrap_peers,
            ..Self::default()
        }
    }

    fn record_request_attempt(&mut self) {
        self.request_attempts = self.request_attempts.saturating_add(1);
    }

    fn record_discovery_query(&mut self) {
        self.discovery_queries = self.discovery_queries.saturating_add(1);
    }

    fn record_discovery_query_failure<E: std::fmt::Debug>(&mut self, error: &E) {
        self.discovery_query_failures = self.discovery_query_failures.saturating_add(1);
        self.last_discovery_failure = Some(short_diagnostic_error(error));
    }

    fn record_kademlia_provider_result(&mut self, provider_count: usize) {
        self.kademlia_provider_results = self
            .kademlia_provider_results
            .saturating_add(u64::try_from(provider_count).unwrap_or(u64::MAX));
    }

    fn record_ignored_kademlia_provider(&mut self) {
        self.ignored_kademlia_providers = self.ignored_kademlia_providers.saturating_add(1);
    }

    fn record_discovered_inviter_address(&mut self, source: PairingDiscoveredAddressSource) {
        self.discovered_inviter_addresses = self.discovered_inviter_addresses.saturating_add(1);
        match source {
            PairingDiscoveredAddressSource::Offer => {}
            PairingDiscoveredAddressSource::Mdns => {
                self.mdns_inviter_addresses = self.mdns_inviter_addresses.saturating_add(1);
            }
            PairingDiscoveredAddressSource::KademliaClosestPeer
            | PairingDiscoveredAddressSource::KademliaPeerRecord => {
                self.kademlia_inviter_addresses = self.kademlia_inviter_addresses.saturating_add(1);
            }
        }
    }

    fn record_outbound_failure<E: std::fmt::Debug>(&mut self, error: &E) {
        self.outbound_failures = self.outbound_failures.saturating_add(1);
        self.last_outbound_failure = Some(short_diagnostic_error(error));
    }

    fn record_dial_error<E: std::fmt::Debug>(&mut self, peer_id: Option<&Libp2pPeerId>, error: &E) {
        self.dial_errors = self.dial_errors.saturating_add(1);
        let peer = peer_id.map_or_else(|| "unknown peer".to_owned(), ToString::to_string);
        self.last_dial_error = Some(format!("{peer}: {}", short_diagnostic_error(error)));
    }

    fn record_relayed_dial_start_failure<E: std::fmt::Debug>(&mut self, error: &E) {
        self.relayed_dial_start_failures = self.relayed_dial_start_failures.saturating_add(1);
        self.last_relayed_dial_error = Some(short_diagnostic_error(error));
    }

    fn timeout_error(&self, timeout_seconds: u64) -> String {
        format!(
            "timed out after {timeout_seconds} seconds; pairing diagnostics: {}",
            self.summary()
        )
    }

    fn summary(&self) -> String {
        let mut parts = vec![
            format!("inviter_hints={}", self.inviter_address_hints),
            format!(
                "relayed_inviter_hints={}",
                self.relayed_inviter_address_hints
            ),
            format!("bootstrap_peers={}", self.bootstrap_peers),
            format!("discovery_queries={}", self.discovery_queries),
            format!("discovery_query_failures={}", self.discovery_query_failures),
            format!(
                "discovered_inviter_addresses={}",
                self.discovered_inviter_addresses
            ),
            format!("mdns_inviter_addresses={}", self.mdns_inviter_addresses),
            format!(
                "kademlia_inviter_addresses={}",
                self.kademlia_inviter_addresses
            ),
            format!(
                "kademlia_provider_results={}",
                self.kademlia_provider_results
            ),
            format!(
                "ignored_kademlia_providers={}",
                self.ignored_kademlia_providers
            ),
            format!("request_attempts={}", self.request_attempts),
            format!("outbound_failures={}", self.outbound_failures),
            format!("dial_errors={}", self.dial_errors),
            format!(
                "relayed_dial_start_failures={}",
                self.relayed_dial_start_failures
            ),
        ];
        if let Some(error) = &self.last_outbound_failure {
            parts.push(format!("last_outbound_failure={error}"));
        }
        if let Some(error) = &self.last_dial_error {
            parts.push(format!("last_dial_error={error}"));
        }
        if let Some(error) = &self.last_relayed_dial_error {
            parts.push(format!("last_relayed_dial_error={error}"));
        }
        if let Some(error) = &self.last_discovery_failure {
            parts.push(format!("last_discovery_failure={error}"));
        }

        parts.join(" ")
    }
}

fn start_pairing_discovery_queries(
    node: &mut p2p_vpn::runtime::p2p::P2pNode,
    offer: &PairingOffer,
    inviter_peer: Libp2pPeerId,
    diagnostics: &mut PairingAcceptDiagnostics,
) {
    if !offer.payload.discovery.kademlia {
        return;
    }

    if let Some(rendezvous_key) = node.kademlia_rendezvous_key.clone() {
        node.swarm.behaviour_mut().kad.get_providers(rendezvous_key);
        diagnostics.record_discovery_query();
    }

    node.swarm
        .behaviour_mut()
        .kad
        .get_record(p2p_vpn::runtime::p2p::kademlia_peer_addresses_key(
            &offer.payload.network_name,
            None,
            inviter_peer,
        ));
    diagnostics.record_discovery_query();

    node.swarm
        .behaviour_mut()
        .kad
        .get_closest_peers(inviter_peer);
    diagnostics.record_discovery_query();

    match node.swarm.behaviour_mut().kad.bootstrap() {
        Ok(_) => diagnostics.record_discovery_query(),
        Err(error) => diagnostics.record_discovery_query_failure(&error),
    }
}

fn handle_pairing_kademlia_event(
    node: &mut p2p_vpn::runtime::p2p::P2pNode,
    offer: &PairingOffer,
    inviter_peer: Libp2pPeerId,
    event: kad::Event,
    dialed_inviter_addresses: &mut HashSet<Multiaddr>,
    diagnostics: &mut PairingAcceptDiagnostics,
) {
    match event {
        kad::Event::OutboundQueryProgressed { result, .. } => {
            handle_pairing_kademlia_query_result(
                node,
                offer,
                inviter_peer,
                result,
                dialed_inviter_addresses,
                diagnostics,
            );
        }
        kad::Event::RoutingUpdated {
            peer, addresses, ..
        } if peer == inviter_peer => {
            dial_pairing_inviter_addresses(
                node,
                inviter_peer,
                addresses.into_vec(),
                dialed_inviter_addresses,
                diagnostics,
                PairingDiscoveredAddressSource::KademliaClosestPeer,
            );
        }
        _ => {}
    }
}

fn handle_pairing_kademlia_query_result(
    node: &mut p2p_vpn::runtime::p2p::P2pNode,
    offer: &PairingOffer,
    inviter_peer: Libp2pPeerId,
    result: kad::QueryResult,
    dialed_inviter_addresses: &mut HashSet<Multiaddr>,
    diagnostics: &mut PairingAcceptDiagnostics,
) {
    match result {
        kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
            providers,
            ..
        })) => {
            diagnostics.record_kademlia_provider_result(providers.len());
            for provider in providers {
                if provider == inviter_peer {
                    node.swarm.behaviour_mut().kad.get_closest_peers(provider);
                    diagnostics.record_discovery_query();
                } else {
                    diagnostics.record_ignored_kademlia_provider();
                }
            }
        }
        kad::QueryResult::GetClosestPeers(
            Ok(kad::GetClosestPeersOk { peers, .. })
            | Err(kad::GetClosestPeersError::Timeout { peers, .. }),
        ) => {
            for peer in peers {
                if peer.peer_id == inviter_peer {
                    dial_pairing_inviter_addresses(
                        node,
                        inviter_peer,
                        peer.addrs,
                        dialed_inviter_addresses,
                        diagnostics,
                        PairingDiscoveredAddressSource::KademliaClosestPeer,
                    );
                }
            }
        }
        kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(peer_record))) => {
            match pairing_inviter_addresses_from_kademlia_record(
                offer,
                inviter_peer,
                peer_record.record.value.as_slice(),
            ) {
                Ok(addresses) => {
                    dial_pairing_inviter_addresses(
                        node,
                        inviter_peer,
                        addresses,
                        dialed_inviter_addresses,
                        diagnostics,
                        PairingDiscoveredAddressSource::KademliaPeerRecord,
                    );
                }
                Err(error) => {
                    diagnostics.record_discovery_query_failure(&error);
                }
            }
        }
        kad::QueryResult::GetProviders(Err(error)) => {
            diagnostics.record_discovery_query_failure(&error);
        }
        kad::QueryResult::GetRecord(Err(error)) => {
            diagnostics.record_discovery_query_failure(&error);
        }
        _ => {}
    }
}

fn dial_pairing_inviter_addresses(
    node: &mut p2p_vpn::runtime::p2p::P2pNode,
    inviter_peer: Libp2pPeerId,
    addresses: impl IntoIterator<Item = Multiaddr>,
    dialed_inviter_addresses: &mut HashSet<Multiaddr>,
    diagnostics: &mut PairingAcceptDiagnostics,
    source: PairingDiscoveredAddressSource,
) {
    for address in addresses {
        let Some(dial_address) = pairing_dial_address(inviter_peer, address) else {
            continue;
        };
        if !dialed_inviter_addresses.insert(dial_address.clone()) {
            continue;
        }
        diagnostics.record_discovered_inviter_address(source);
        if let Err(error) = node.swarm.dial(dial_address.clone()) {
            if dial_address
                .iter()
                .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
            {
                diagnostics.record_relayed_dial_start_failure(&error);
            } else {
                diagnostics.record_dial_error(Some(&inviter_peer), &error);
            }
            eprintln!(
                "pairing dial start failed: source={} address={} error={}",
                source.as_str(),
                dial_address,
                short_diagnostic_error(&error)
            );
        }
    }
}

fn redial_pairing_offer_addresses(
    node: &mut p2p_vpn::runtime::p2p::P2pNode,
    inviter_peer: Libp2pPeerId,
    addresses: &[Multiaddr],
    dialed_inviter_addresses: &mut HashSet<Multiaddr>,
    diagnostics: &mut PairingAcceptDiagnostics,
) {
    for address in addresses {
        dialed_inviter_addresses.remove(address);
    }
    dial_pairing_inviter_addresses(
        node,
        inviter_peer,
        addresses.iter().cloned(),
        dialed_inviter_addresses,
        diagnostics,
        PairingDiscoveredAddressSource::Offer,
    );
}

fn pairing_dial_address(inviter_peer: Libp2pPeerId, address: Multiaddr) -> Option<Multiaddr> {
    let is_relayed = address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2pCircuit));
    if is_relayed {
        let mut after_circuit = false;
        let mut target_peer = None;
        for protocol in &address {
            match protocol {
                Protocol::P2pCircuit => after_circuit = true,
                Protocol::P2p(peer) if after_circuit => target_peer = Some(peer),
                _ => {}
            }
        }
        return match target_peer {
            Some(peer) if peer == inviter_peer => Some(address),
            Some(_) => None,
            None => address.with_p2p(inviter_peer).ok(),
        };
    }

    let mut direct_target_peer = None;
    for protocol in &address {
        if let Protocol::P2p(peer) = protocol {
            if direct_target_peer.is_some() {
                return None;
            }
            direct_target_peer = Some(peer);
        }
    }

    if let Some(peer) = direct_target_peer {
        if peer == inviter_peer {
            Some(address)
        } else {
            None
        }
    } else {
        address.with_p2p(inviter_peer).ok()
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PairingKademliaPeerAddressRecord {
    payload: PairingKademliaPeerAddressRecordPayload,
    signature: Vec<u8>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PairingKademliaPeerAddressRecordPayload {
    version: u8,
    network_name: String,
    #[serde(default)]
    membership_tag: Option<String>,
    peer_id: String,
    public_key_protobuf: Vec<u8>,
    sequence: u64,
    expires_at_unix_seconds: u64,
    addresses: Vec<String>,
}

fn pairing_inviter_addresses_from_kademlia_record(
    offer: &PairingOffer,
    inviter_peer: Libp2pPeerId,
    value: &[u8],
) -> Result<Vec<Multiaddr>, String> {
    let record: PairingKademliaPeerAddressRecord =
        serde_json::from_slice(value).map_err(|error| format!("decode_failed: {error}"))?;
    let payload_bytes = serde_json::to_vec(&record.payload)
        .map_err(|error| format!("payload_encode_failed: {error}"))?;
    if record.payload.version != 1 {
        return Err("unsupported_version".to_owned());
    }
    if record.payload.network_name != offer.payload.network_name {
        return Err("wrong_network".to_owned());
    }
    if record.payload.membership_tag.is_some() {
        return Err("wrong_membership_scope".to_owned());
    }
    if record.payload.peer_id != offer.payload.inviter_peer {
        return Err("wrong_peer".to_owned());
    }
    if record.payload.expires_at_unix_seconds < current_unix_seconds_lossy() {
        return Err("expired".to_owned());
    }
    let offered_public_key_bytes = STANDARD
        .decode(&offer.payload.inviter_public_key)
        .map_err(|error| format!("offer_public_key_decode_failed: {error}"))?;
    if record.payload.public_key_protobuf != offered_public_key_bytes {
        return Err("wrong_public_key".to_owned());
    }
    let public_key = libp2p::identity::PublicKey::try_decode_protobuf(&offered_public_key_bytes)
        .map_err(|error| format!("public_key_decode_failed: {error:?}"))?;
    if !public_key.verify(&payload_bytes, &record.signature) {
        return Err("invalid_signature".to_owned());
    }

    let addresses = record
        .payload
        .addresses
        .iter()
        .filter_map(|address| address.parse::<Multiaddr>().ok())
        .filter_map(|address| pairing_dial_address(inviter_peer, address))
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("no_addresses".to_owned());
    }

    Ok(addresses)
}

fn short_diagnostic_error<E: std::fmt::Debug>(error: &E) -> String {
    const MAX_LEN: usize = 240;

    let rendered = format!("{error:?}")
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if rendered.len() <= MAX_LEN {
        return rendered;
    }

    format!("{}...", rendered.chars().take(MAX_LEN).collect::<String>())
}

fn pairing_inviter_addresses(
    offer: &PairingOffer,
    inviter_peer: Libp2pPeerId,
) -> Result<Vec<(Libp2pPeerId, Multiaddr)>, String> {
    let mut addresses = Vec::new();
    for address in &offer.payload.inviter_addresses {
        addresses.push((
            inviter_peer,
            address
                .parse::<Multiaddr>()
                .map_err(|error| format!("invalid inviter address `{address}`: {error}"))?,
        ));
    }
    for address in &offer.payload.relay_reservations {
        let reservation = address
            .parse::<Multiaddr>()
            .map_err(|error| format!("invalid relay reservation `{address}`: {error}"))?;
        let dial_address = pairing_dial_address(inviter_peer, reservation)
            .ok_or_else(|| format!("invalid relay reservation target `{address}`"))?;
        addresses.push((inviter_peer, dial_address));
    }
    Ok(addresses)
}

fn pairing_bootstrap_peers(offer: &PairingOffer) -> Result<Vec<(Libp2pPeerId, Multiaddr)>, String> {
    let bootstrap_peers = if offer.payload.bootstrap_peers.is_empty()
        && offer.payload.discovery.kademlia
        && offer.payload.discovery.kademlia_protocol == PUBLIC_IPFS_KADEMLIA_PROTOCOL
    {
        p2p_vpn::config::public_ipfs_bootstrap_peer_configs()
    } else {
        offer.payload.bootstrap_peers.clone()
    };

    bootstrap_peers
        .iter()
        .map(|peer| {
            peer.peer_address()
                .map_err(|error| format!("invalid bootstrap peer in offer: {error:?}"))
        })
        .collect()
}

impl From<EndpointArg> for InitPeer {
    fn from(value: EndpointArg) -> Self {
        Self {
            id: value.id,
            address: value.address,
            vpn_ip: None,
            routes: Vec::new(),
        }
    }
}

fn init_peers(
    addresses: Vec<EndpointArg>,
    vpn_ips: Vec<PeerVpnIpArg>,
    routes: Vec<PeerRouteArg>,
) -> Vec<InitPeer> {
    addresses
        .into_iter()
        .map(EndpointArg::into)
        .chain(vpn_ips.into_iter().map(|vpn_ip| InitPeer {
            id: vpn_ip.id,
            address: None,
            vpn_ip: Some(vpn_ip.vpn_ip),
            routes: Vec::new(),
        }))
        .chain(routes.into_iter().map(|route| InitPeer {
            id: route.id,
            address: None,
            vpn_ip: None,
            routes: vec![route.route],
        }))
        .collect()
}

fn init_bootstrap_peers(
    mut peers: Vec<EndpointArg>,
    relay_peers: Vec<EndpointArg>,
    include_ipfs_defaults: bool,
) -> Vec<InitPeer> {
    for relay in relay_peers {
        if peers
            .iter()
            .any(|peer| peer.id == relay.id && peer.address == relay.address)
        {
            continue;
        }
        peers.push(relay);
    }

    if include_ipfs_defaults {
        for (id, address) in PUBLIC_IPFS_BOOTSTRAP_PEERS {
            if peers
                .iter()
                .any(|peer| peer.id == *id && peer.address.as_deref() == Some(*address))
            {
                continue;
            }
            peers.push(EndpointArg {
                id: (*id).to_owned(),
                address: Some((*address).to_owned()),
            });
        }
    }

    peers.into_iter().map(EndpointArg::into).collect()
}

fn init_relay_reservations(relays: &[EndpointArg]) -> Result<Vec<String>, String> {
    relays
        .iter()
        .map(relay_reservation_address)
        .collect::<Result<Vec<_>, _>>()
}

fn relay_reservation_address(relay: &EndpointArg) -> Result<String, String> {
    let peer = relay
        .id
        .parse::<libp2p::PeerId>()
        .map_err(|error| format!("relay peer id {} is invalid: {error}", relay.id))?;
    let Some(address) = &relay.address else {
        return Err(format!(
            "relay peer {} must include an address as PEER_ID=MULTIADDR",
            relay.id
        ));
    };
    let mut address = address
        .parse::<libp2p::Multiaddr>()
        .map_err(|error| format!("relay peer address for {} is invalid: {error:?}", relay.id))?;

    if address
        .iter()
        .any(|protocol| matches!(protocol, libp2p::multiaddr::Protocol::P2pCircuit))
    {
        return Err(format!(
            "relay peer {} address must be the relay's direct address, without /p2p-circuit",
            relay.id
        ));
    }

    match relay_peer_target(&address) {
        Some(actual) if actual == peer => {}
        Some(actual) => {
            return Err(format!(
                "relay peer address target {actual} does not match {}",
                relay.id
            ));
        }
        None => address.push(libp2p::multiaddr::Protocol::P2p(peer)),
    }
    address.push(libp2p::multiaddr::Protocol::P2pCircuit);

    Ok(address.to_string())
}

fn relay_peer_target(address: &libp2p::Multiaddr) -> Option<libp2p::PeerId> {
    address.iter().find_map(|protocol| match protocol {
        libp2p::multiaddr::Protocol::P2p(peer) => Some(peer),
        _ => None,
    })
}

fn status(path: &PathBuf) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    for line in status_lines(&config)? {
        println!("{line}");
    }

    Ok(())
}

fn status_lines(config: &Config) -> Result<Vec<String>, String> {
    config
        .validate_runtime()
        .map_err(|error| format!("config is not runtime-ready: {error:?}"))?;
    let routes = config
        .compile_routes()
        .map_err(|error| format!("failed to compile routes: {error:?}"))?;
    let defaults = RuntimeDefaults::default();
    let local_peer = config
        .local_peer_id()
        .map_err(|error| format!("failed to parse local peer id: {error:?}"))?;
    let local_peer_display = config
        .local_peer()
        .map_err(|error| format!("failed to resolve local peer: {error:?}"))?;
    let local_addresses = TunAddresses::for_peer(local_peer);
    let mut lines = Vec::new();

    lines.push(format!("network: {}", config.network.name));
    lines.push(format!(
        "membership key configured: {}",
        config.network.membership_key.is_some()
    ));
    lines.push(format!(
        "interface: {} mtu {}",
        config.interface.name, config.interface.mtu
    ));
    lines.push(format!(
        "effective packet mtu: {}",
        config.effective_packet_mtu()
    ));
    lines.push(format!("local peer: {local_peer_display}"));
    lines.push(format!("local overlay ipv4: {}", local_addresses.ipv4));
    lines.push(format!("local overlay ipv6: {}", local_addresses.ipv6));
    lines.push(format!(
        "packet session: {}",
        session_id_for_peer(local_peer)
    ));
    lines.push(format!("peers: {}", config.peers.len()));
    lines.push(format!(
        "queue: {} packets / {} bytes / {} ms per peer",
        config.queue.max_packets_per_peer,
        config.queue.max_bytes_per_peer,
        config.queue.max_packet_age().as_millis()
    ));
    lines.push(format!(
        "resources: {} concurrent control streams / {} concurrent packet streams",
        config.resources.control_stream_limit(),
        config.resources.packet_stream_limit()
    ));
    lines.push(format!(
        "connection limits: {} pending in / {} pending out / {} established in / {} established out / {} per peer / {} total",
        config.resources.max_pending_incoming_connections,
        config.resources.max_pending_outgoing_connections,
        config.resources.max_established_incoming_connections,
        config.resources.max_established_outgoing_connections,
        config.resources.max_established_connections_per_peer,
        config.resources.max_established_connections
    ));
    lines.push(format!(
        "wire: v{WIRE_VERSION}, {HEADER_LEN}-byte packet header"
    ));
    lines.push(format!(
        "protocols: control={} packet={} service={}",
        p2p_vpn::runtime::control::CONTROL_PROTOCOL,
        p2p_vpn::runtime::packet::PACKET_PROTOCOL,
        SERVICE_PROTOCOL
    ));
    lines.push(format!(
        "preferred path: {} (score {})",
        path_name(defaults.preferred_path),
        defaults.preferred_path.default_score()
    ));
    lines.push(format!("compiled routes: {}", routes.len()));
    push_discovery_status(&mut lines, config);
    push_relay_status(&mut lines, config);
    lines.push(format!(
        "configured peer addresses: {}",
        config
            .peer_address_count()
            .map_err(|error| format!("{error:?}"))?
    ));
    lines.push(format!(
        "configured local routes: {}",
        config.network.routes.len()
    ));
    lines.push(format!(
        "configured peer routes: {}",
        config
            .peers
            .iter()
            .map(|peer| peer.routes.len())
            .sum::<usize>()
    ));
    push_route_ownership_status(&mut lines, config, &routes);

    Ok(lines)
}

fn routes(path: &PathBuf, resolve: Option<IpAddr>) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    for line in route_lines(&config, resolve)? {
        println!("{line}");
    }

    Ok(())
}

fn route_lines(config: &Config, resolve: Option<IpAddr>) -> Result<Vec<String>, String> {
    config
        .validate_runtime()
        .map_err(|error| format!("config is not runtime-ready: {error:?}"))?;
    let routes = config
        .compile_routes()
        .map_err(|error| format!("failed to compile routes: {error:?}"))?;
    let mut lines = Vec::new();

    lines.push(format!("compiled routes: {}", routes.len()));
    if let Some(destination) = resolve {
        push_route_resolution_line(&mut lines, config, &routes, destination);
    }
    for route in routes.routes() {
        let (owner_kind, owner_id, owner_name, source) = route_owner_details(config, *route);
        lines.push(format!(
            "route: {} owner {} {} name {} metric {} {}",
            route.prefix, owner_kind, owner_id, owner_name, route.metric, source
        ));
    }

    Ok(lines)
}

fn push_route_resolution_line(
    lines: &mut Vec<String>,
    config: &Config,
    routes: &p2p_vpn::route::RouteTable,
    destination: IpAddr,
) {
    if let Some(route) = routes.resolve(destination) {
        let (owner_kind, owner_id, owner_name, source) = route_owner_details(config, route);
        lines.push(format!(
            "resolve: {} -> {} owner {} {} name {} metric {} {}",
            destination, route.prefix, owner_kind, owner_id, owner_name, route.metric, source
        ));
    } else {
        lines.push(format!("resolve: {destination} -> no route"));
    }
}

fn route_owner_details(
    config: &Config,
    route: p2p_vpn::route::Route,
) -> (&'static str, String, String, &'static str) {
    let local_peer = config.local_peer_id().expect("route config is valid");
    if route.owner == local_peer {
        return (
            "local",
            config.local_peer().expect("route config is valid"),
            "-".to_owned(),
            route_source(
                config.network.vpn_ip.as_deref(),
                &config.network.routes,
                route,
            ),
        );
    }

    let peer = config
        .peers
        .iter()
        .find(|peer| peer.peer_id().is_ok_and(|peer_id| peer_id == route.owner))
        .expect("compiled route owner is configured");
    (
        "peer",
        peer.id.clone(),
        peer.name.clone().unwrap_or_else(|| "-".to_owned()),
        route_source(peer.vpn_ip.as_deref(), &peer.routes, route),
    )
}

async fn mtu(path: &PathBuf, live: bool, timeout_seconds: u64) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    config
        .validate_runtime()
        .map_err(|error| format!("config is not runtime-ready: {error:?}"))?;

    let lines = if live {
        Box::pin(mtu_lines_live(
            &config,
            Duration::from_secs(timeout_seconds.max(1)),
        ))
        .await?
    } else {
        mtu_lines_configured(&config)?
    };

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

fn mtu_lines_configured(config: &Config) -> Result<Vec<String>, String> {
    let routes = config
        .compile_routes()
        .map_err(|error| format!("failed to compile routes: {error:?}"))?;
    let effective_mtu = config.effective_packet_mtu();
    let mut lines = vec![
        format!("interface: {}", config.interface.name),
        format!("configured mtu: {}", config.interface.mtu),
        format!("effective packet mtu: {effective_mtu}"),
        format!("wire max packet payload length: {MAX_PAYLOAD_LEN}"),
        format!("packet header length: {HEADER_LEN}"),
        format!("packet plane datagram overhead length: {PACKET_PLANE_DATAGRAM_OVERHEAD_LEN}"),
        format!("packet plane max payload length: {PACKET_PLANE_MAX_PAYLOAD_LEN}"),
        OVERLAY_FRAGMENTATION_POLICY_LINE.to_owned(),
    ];

    for route in routes.routes() {
        let (owner_kind, owner_id, owner_name, source) = route_owner_details(config, *route);
        lines.push(format!(
            "route mtu: {} owner {} {} name {} metric {} {} mtu {} advmss {}",
            route.prefix,
            owner_kind,
            owner_id,
            owner_name,
            route.metric,
            source,
            effective_mtu,
            advmss_text(route.prefix, effective_mtu)
        ));
    }

    push_configured_mtu_path_lines(&mut lines, config);

    Ok(lines)
}

async fn mtu_lines_live(config: &Config, timeout: Duration) -> Result<Vec<String>, String> {
    let mut lines = mtu_lines_configured(config)?;
    let local_mtu = config.effective_packet_mtu();
    for peer in &config.peers {
        let peer_id = peer
            .id
            .parse::<libp2p::PeerId>()
            .expect("mtu config is valid");
        match Box::pin(query_peer_status(config, peer_id, timeout)).await {
            Ok(status) => push_peer_live_mtu_lines(&mut lines, &status, local_mtu),
            Err(error) => lines.push(format!(
                "peer live mtu: {} unreachable error {error:?}",
                peer.id
            )),
        }
    }

    Ok(lines)
}

fn push_configured_mtu_path_lines(lines: &mut Vec<String>, config: &Config) {
    let effective_mtu = config.effective_packet_mtu();
    for peer in &config.peers {
        for address in &peer.addresses {
            match path_kind_for_multiaddr(address) {
                Ok(kind) => lines.push(format!(
                    "path mtu candidate: {} {} estimated_mtu {} address {}",
                    peer.id,
                    path_name(kind),
                    configured_path_mtu_estimate(kind, effective_mtu),
                    address
                )),
                Err(error) => lines.push(format!(
                    "path mtu candidate: {} invalid error {error}",
                    peer.id
                )),
            }
        }
        if peer.addresses.is_empty() {
            lines.push(format!(
                "path mtu discovery: {} enabled {} initial_mtu {}",
                peer.id,
                config.network.discovery.mdns || config.network.discovery.kademlia,
                effective_mtu
            ));
        }
    }
}

fn push_peer_live_mtu_lines(lines: &mut Vec<String>, status: &RemotePeerStatus, local_mtu: u16) {
    let preferred_path = PathKind::from_wire_name(&status.capabilities.preferred_path)
        .unwrap_or(PathKind::DirectQuicStream);
    let peer_mtu = status.service.effective_mtu.min(local_mtu);
    let path_mtu = configured_path_mtu_estimate(preferred_path, peer_mtu);

    lines.push(format!(
        "peer live mtu: {} effective_mtu {} negotiated_mtu {} preferred_path {} path_mtu_estimate {} wire_max_payload {} packet_plane_max_payload {} packet_plane_overhead {}",
        status.peer,
        status.service.effective_mtu,
        peer_mtu,
        path_name(preferred_path),
        path_mtu,
        optional_usize(status.service.max_packet_payload_len),
        optional_usize(status.service.packet_plane_max_payload_len),
        optional_usize(status.service.packet_plane_datagram_overhead_len)
    ));
    lines.push(format!(
        "peer live fragmentation: {} overlay disabled",
        status.peer
    ));
}

fn advmss_text(prefix: p2p_vpn::route::IpCidr, mtu: u16) -> String {
    route_advmss(prefix, mtu).map_or_else(|| "none".to_owned(), |advmss| advmss.to_string())
}

fn push_discovery_status(lines: &mut Vec<String>, config: &Config) {
    lines.push(format!(
        "listen addresses: {}",
        config.network.listen_addresses.len()
    ));
    lines.push(format!(
        "external addresses: {}",
        config.network.external_addresses.len()
    ));
    lines.push(format!(
        "bootstrap peers: {}",
        config.network.bootstrap_peers.len()
    ));
    lines.push(format!(
        "discovery: mdns={} kademlia={} kademlia_provider_advertisement={} dcutr={} autonat={}",
        config.network.discovery.mdns,
        config.network.discovery.kademlia,
        config.network.discovery.kademlia_provider_advertisement,
        config.network.discovery.dcutr,
        config.network.discovery.autonat
    ));
    lines.push(format!(
        "kademlia protocol: {}",
        config.network.discovery.kademlia_protocol
    ));
    lines.push(format!(
        "kademlia scope: {}",
        kademlia_scope(&config.network.discovery.kademlia_protocol)
    ));
}

fn kademlia_scope(protocol: &str) -> &'static str {
    if protocol == PUBLIC_IPFS_KADEMLIA_PROTOCOL {
        "ipfs-compatible public dht"
    } else if protocol == PRIVATE_KADEMLIA_PROTOCOL {
        "private overlay"
    } else {
        "custom"
    }
}

fn push_relay_status(lines: &mut Vec<String>, config: &Config) {
    lines.push(format!("relay server: {}", config.network.relay.server));
    lines.push(format!(
        "relay reservations: {}",
        config.network.relay.reservations.len()
    ));
    lines.push(format!(
        "relay auto policy: {} candidates / {} reservations / {}s retry",
        config.network.relay.auto.max_candidates,
        config.network.relay.auto.max_reservations,
        config.network.relay.auto.retry_interval_seconds
    ));
    lines.push(format!(
        "relay resources: {} reservations / {} per peer / {}s reservation / {} circuits / {} per peer / {}s circuit / {} bytes",
        config.network.relay.resources.max_reservations,
        config.network.relay.resources.max_reservations_per_peer,
        config.network.relay.resources.reservation_duration_secs,
        config.network.relay.resources.max_circuits,
        config.network.relay.resources.max_circuits_per_peer,
        config.network.relay.resources.max_circuit_duration_secs,
        config.network.relay.resources.max_circuit_bytes
    ));
}

fn push_route_ownership_status(
    lines: &mut Vec<String>,
    config: &Config,
    routes: &p2p_vpn::route::RouteTable,
) {
    let local_peer = config.local_peer_id().expect("status config is valid");
    for route in routes.routes_for(local_peer) {
        let source = route_source(
            config.network.vpn_ip.as_deref(),
            &config.network.routes,
            route,
        );
        lines.push(format!(
            "local route: {} metric {} {source}",
            route.prefix, route.metric
        ));
    }
    for peer in &config.peers {
        let owner = peer.peer_id().expect("status config is valid");
        for route in routes.routes_for(owner) {
            let source = route_source(peer.vpn_ip.as_deref(), &peer.routes, route);
            lines.push(format!(
                "peer route: {} {} metric {} {source}",
                peer.id, route.prefix, route.metric
            ));
        }
    }
}

fn route_source(
    vpn_ip: Option<&str>,
    configured_routes: &[RouteConfig],
    route: p2p_vpn::route::Route,
) -> &'static str {
    if vpn_ip.is_some_and(|vpn_ip| vpn_ip_route_matches(vpn_ip, route)) {
        return "vpn-ip";
    }

    if configured_routes.iter().any(|configured| {
        configured.metric == route.metric
            && configured
                .prefix()
                .is_ok_and(|prefix| prefix == route.prefix)
    }) {
        "configured"
    } else {
        "built-in"
    }
}

fn vpn_ip_route_matches(vpn_ip: &str, route: p2p_vpn::route::Route) -> bool {
    if route.metric != 0 {
        return false;
    }

    if vpn_ip.contains('/') {
        return RouteConfig {
            prefix: vpn_ip.to_owned(),
            metric: 0,
        }
        .prefix()
        .is_ok_and(|prefix| prefix == route.prefix);
    }

    vpn_ip.parse::<IpAddr>().is_ok_and(|address| match address {
        IpAddr::V4(_) => route.prefix.to_string() == format!("{address}/32"),
        IpAddr::V6(_) => route.prefix.to_string() == format!("{address}/128"),
    })
}

fn metrics(path: &PathBuf, format: MetricsFormat) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    let snapshot = RuntimeMetrics::default().snapshot(QueueStats::default());

    match format {
        MetricsFormat::Text => {
            println!("network: {}", config.network.name);
            println!("runtime metrics:");
            for line in snapshot.lines() {
                println!("{line}");
            }
            println!("live output: run `up --metrics-interval-seconds N`");
        }
        MetricsFormat::Prometheus => {
            for line in snapshot.prometheus_lines() {
                println!("{line}");
            }
        }
    }
    Ok(())
}

async fn peers(path: &PathBuf, live: bool, timeout_seconds: u64) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    config
        .validate_runtime()
        .map_err(|error| format!("config is not runtime-ready: {error:?}"))?;

    let lines = if live {
        Box::pin(peer_lines_live(
            &config,
            Duration::from_secs(timeout_seconds.max(1)),
        ))
        .await?
    } else {
        peer_lines_configured(&config)?
    };

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

fn peer_lines_configured(config: &Config) -> Result<Vec<String>, String> {
    let routes = config
        .compile_routes()
        .map_err(|error| format!("failed to compile routes: {error:?}"))?;
    let mut lines = vec![format!("peers: {}", config.peers.len())];

    push_peer_config_lines(&mut lines, config, &routes);

    Ok(lines)
}

fn push_peer_config_lines(
    lines: &mut Vec<String>,
    config: &Config,
    routes: &p2p_vpn::route::RouteTable,
) {
    for peer in &config.peers {
        let peer_id = peer.peer_id().expect("peer list config is valid");
        lines.push(format!("peer: {}", peer.id));
        if let Some(name) = &peer.name {
            lines.push(format!("peer name: {} {}", peer.id, name));
        }
        lines.push(format!(
            "peer addresses: {} {}",
            peer.id,
            peer.addresses.len()
        ));
        for address in &peer.addresses {
            lines.push(format!("peer address: {} {}", peer.id, address));
        }
        for route in routes.routes_for(peer_id) {
            let source = route_source(peer.vpn_ip.as_deref(), &peer.routes, route);
            lines.push(format!(
                "peer route: {} {} metric {} {source}",
                peer.id, route.prefix, route.metric
            ));
        }
    }
}

async fn peer_lines_live(config: &Config, timeout: Duration) -> Result<Vec<String>, String> {
    let routes = config
        .compile_routes()
        .map_err(|error| format!("failed to compile routes: {error:?}"))?;
    let mut lines = vec![format!("peers: {}", config.peers.len())];

    push_peer_config_lines(&mut lines, config, &routes);
    for peer in &config.peers {
        let peer_id = peer
            .id
            .parse::<libp2p::PeerId>()
            .expect("peer list config is valid");
        match Box::pin(query_peer_status(config, peer_id, timeout)).await {
            Ok(status) => push_peer_live_status_lines(&mut lines, &status),
            Err(error) => lines.push(format!(
                "peer live: {} unreachable error {error:?}",
                peer.id
            )),
        }
    }

    Ok(lines)
}

fn push_peer_live_status_lines(lines: &mut Vec<String>, status: &RemotePeerStatus) {
    let readiness = peer_operational_readiness(status);
    lines.push(format!("peer live: {} reachable", status.peer));
    lines.push(format!(
        "peer live operational: {} ready {} reason {}",
        status.peer,
        readiness.ready,
        readiness.reason.as_str()
    ));
    lines.push(format!(
        "peer live network: {} {}",
        status.peer, status.service.network_name
    ));
    lines.push(format!(
        "peer live membership key matched: {} {}",
        status.peer,
        status.service.membership_tag.is_some()
    ));
    lines.push(format!(
        "peer live mtu: {} {}",
        status.peer, status.service.effective_mtu
    ));
    lines.push(format!(
        "peer live quic datagrams: {} {}",
        status.peer, status.service.supports_quic_datagrams
    ));
    lines.push(format!(
        "peer live native quic datagrams: {} {}",
        status.peer, status.service.supports_native_quic_datagrams
    ));
    lines.push(format!(
        "peer live owned udp packet plane: {} {}",
        status.peer, status.service.supports_owned_udp_packet_plane
    ));
    lines.push(format!(
        "peer live owned quic packet plane: {} {}",
        status.peer, status.service.supports_owned_quic_packet_plane
    ));
    push_peer_live_reported_path_lines(lines, status);
    push_peer_live_packet_plane_lines(lines, status);
    lines.push(format!(
        "peer live preferred path: {} {}",
        status.peer,
        path_name(
            PathKind::from_wire_name(&status.capabilities.preferred_path)
                .unwrap_or(PathKind::DirectQuicStream)
        )
    ));
    push_peer_live_advertised_route_lines(lines, status);
}

fn push_peer_live_reported_path_lines(lines: &mut Vec<String>, status: &RemotePeerStatus) {
    lines.push(format!(
        "peer live reported selected path: {} {}",
        status.peer,
        optional_path_name(status.service.selected_path.as_deref())
    ));
    lines.push(format!(
        "peer live reported selected path score: {} {}",
        status.peer,
        optional_i32(status.service.selected_path_score)
    ));
    lines.push(format!(
        "peer live reported selected path mtu: {} {}",
        status.peer,
        optional_u16(status.service.selected_path_mtu)
    ));
    lines.push(format!(
        "peer live reported selected path rtt ms: {} {}",
        status.peer,
        optional_u16(status.service.selected_path_rtt_ms)
    ));
}

fn push_peer_live_packet_plane_lines(lines: &mut Vec<String>, status: &RemotePeerStatus) {
    lines.push(format!(
        "peer live owned quic packet plane certificate bytes: {} {}",
        status.peer,
        status
            .capabilities
            .owned_quic_packet_plane_certificate_der
            .as_ref()
            .map_or_else(
                || "none".to_owned(),
                |certificate| { certificate.len().to_string() }
            )
    ));
    lines.push(format!(
        "peer live owned quic packet endpoints: {} {}",
        status.peer,
        status
            .capabilities
            .owned_quic_packet_endpoint_candidates
            .len()
    ));
    for endpoint in &status.capabilities.owned_quic_packet_endpoint_candidates {
        lines.push(format!(
            "peer live owned quic packet endpoint: {} {}",
            status.peer, endpoint
        ));
    }
    lines.push(format!(
        "peer live packet endpoints: {} {}",
        status.peer,
        status.capabilities.packet_endpoint_candidates.len()
    ));
    for endpoint in &status.capabilities.packet_endpoint_candidates {
        lines.push(format!(
            "peer live packet endpoint: {} {}",
            status.peer, endpoint
        ));
    }
    lines.push(format!(
        "peer live packet plane session ttl seconds: {} {}",
        status.peer,
        optional_seconds(status.service.packet_plane_session_ttl_seconds)
    ));
    lines.push(format!(
        "peer live packet plane replay windows per session: {} {}",
        status.peer,
        optional_usize(status.service.packet_plane_replay_windows_per_session)
    ));
}

fn push_peer_live_advertised_route_lines(lines: &mut Vec<String>, status: &RemotePeerStatus) {
    lines.push(format!(
        "peer live advertised routes: {} {}",
        status.peer,
        status.capabilities.advertised_routes.len()
    ));
    for route in &status.capabilities.advertised_routes {
        lines.push(format!(
            "peer live advertised route: {} {} metric {}",
            status.peer, route.prefix, route.metric
        ));
    }
}

async fn paths(path: &PathBuf, live: bool, timeout_seconds: u64) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    config
        .validate_runtime()
        .map_err(|error| format!("config is not runtime-ready: {error:?}"))?;

    let lines = if live {
        Box::pin(path_lines_live(
            &config,
            Duration::from_secs(timeout_seconds.max(1)),
        ))
        .await?
    } else {
        path_lines_configured(&config)?
    };

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

fn path_lines_configured(config: &Config) -> Result<Vec<String>, String> {
    let defaults = RuntimeDefaults::default();
    let mut lines = vec![
        format!("peers: {}", config.peers.len()),
        format!(
            "preferred path: {} (score {})",
            path_name(defaults.preferred_path),
            defaults.preferred_path.default_score()
        ),
        format!(
            "discovery paths: mdns={} kademlia={} dcutr={} autonat={}",
            config.network.discovery.mdns,
            config.network.discovery.kademlia,
            config.network.discovery.dcutr,
            config.network.discovery.autonat
        ),
        format!(
            "relay reservation paths: {}",
            config.network.relay.reservations.len()
        ),
    ];

    push_configured_path_lines(&mut lines, config)?;

    Ok(lines)
}

fn push_configured_path_lines(lines: &mut Vec<String>, config: &Config) -> Result<(), String> {
    let mut candidate_count = 0usize;
    for peer in &config.peers {
        lines.push(format!(
            "peer path candidates: {} {}",
            peer.id,
            peer.addresses.len()
        ));
        for address in &peer.addresses {
            let kind = path_kind_for_multiaddr(address)?;
            candidate_count = candidate_count.saturating_add(1);
            let estimated_mtu = configured_path_mtu_estimate(kind, config.effective_packet_mtu());
            lines.push(format!(
                "peer path candidate: {} {} score {} estimated_mtu {} address {}",
                peer.id,
                path_name(kind),
                kind.default_score(),
                estimated_mtu,
                address
            ));
        }
        if peer.addresses.is_empty() {
            lines.push(format!(
                "peer path discovery: {} enabled {}",
                peer.id,
                config.network.discovery.mdns || config.network.discovery.kademlia
            ));
        }
    }
    lines.insert(1, format!("configured path candidates: {candidate_count}"));

    Ok(())
}

async fn path_lines_live(config: &Config, timeout: Duration) -> Result<Vec<String>, String> {
    let mut lines = path_lines_configured(config)?;
    for peer in &config.peers {
        let peer_id = peer
            .id
            .parse::<libp2p::PeerId>()
            .expect("path config is valid");
        match Box::pin(query_peer_status(config, peer_id, timeout)).await {
            Ok(status) => push_peer_live_path_lines(&mut lines, &status),
            Err(error) => lines.push(format!(
                "peer live path: {} unreachable error {error:?}",
                peer.id
            )),
        }
    }

    Ok(lines)
}

fn push_peer_live_path_lines(lines: &mut Vec<String>, status: &RemotePeerStatus) {
    let preferred_path = PathKind::from_wire_name(&status.capabilities.preferred_path)
        .unwrap_or(PathKind::DirectQuicStream);
    let readiness = peer_operational_readiness(status);
    let packet_datagram_ready = status.service.supports_owned_udp_packet_plane
        || status.service.supports_owned_quic_packet_plane
        || status.service.supports_quic_datagrams
        || status.service.supports_native_quic_datagrams;
    let path_probe_ready = !preferred_path.requires_quic_datagrams() || packet_datagram_ready;
    let estimated_path_mtu =
        configured_path_mtu_estimate(preferred_path, status.service.effective_mtu);

    lines.push(format!(
        "peer live path: {} reachable preferred {} score {} mtu {} path_mtu_estimate {} reported_selected_path {} reported_selected_path_score {} reported_selected_path_mtu {} reported_selected_path_rtt_ms {} quic_datagrams {} native_quic_datagrams {} owned_udp_packet_plane {} owned_quic_packet_plane {} path_probe_ready {} operational_ready {} operational_reason {}",
        status.peer,
        path_name(preferred_path),
        preferred_path.default_score(),
        status.service.effective_mtu,
        estimated_path_mtu,
        optional_path_name(status.service.selected_path.as_deref()),
        optional_i32(status.service.selected_path_score),
        optional_u16(status.service.selected_path_mtu),
        optional_u16(status.service.selected_path_rtt_ms),
        status.service.supports_quic_datagrams,
        status.service.supports_native_quic_datagrams,
        status.service.supports_owned_udp_packet_plane,
        status.service.supports_owned_quic_packet_plane,
        path_probe_ready,
        readiness.ready,
        readiness.reason.as_str()
    ));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PeerOperationalReadiness {
    ready: bool,
    reason: PeerOperationalReadinessReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PeerOperationalReadinessReason {
    Ready,
    InvalidPreferredPath,
    RemoteDatagramSupportMissing,
    DatagramCapabilityMismatch,
    OwnedQuicCertificateMissing,
    OwnedQuicEndpointMissing,
    OwnedUdpEndpointMissing,
}

impl PeerOperationalReadinessReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::InvalidPreferredPath => "invalid_preferred_path",
            Self::RemoteDatagramSupportMissing => "remote_datagram_support_missing",
            Self::DatagramCapabilityMismatch => "datagram_capability_mismatch",
            Self::OwnedQuicCertificateMissing => "owned_quic_certificate_missing",
            Self::OwnedQuicEndpointMissing => "owned_quic_endpoint_missing",
            Self::OwnedUdpEndpointMissing => "owned_udp_endpoint_missing",
        }
    }
}

fn peer_operational_readiness(status: &RemotePeerStatus) -> PeerOperationalReadiness {
    let Some(preferred_path) = PathKind::from_wire_name(&status.capabilities.preferred_path) else {
        return blocked(PeerOperationalReadinessReason::InvalidPreferredPath);
    };

    if !preferred_path.requires_quic_datagrams() {
        return ready();
    }

    if !service_supports_datagram_packet_path(&status.service) {
        return blocked(PeerOperationalReadinessReason::RemoteDatagramSupportMissing);
    }

    if !status.capabilities.supports_datagram_packet_path() {
        return blocked(PeerOperationalReadinessReason::DatagramCapabilityMismatch);
    }

    let owned_quic_claimed = status.service.supports_owned_quic_packet_plane
        || status.capabilities.supports_owned_quic_packet_plane;
    let owned_udp_claimed = status.service.supports_owned_udp_packet_plane
        || status.capabilities.supports_owned_udp_packet_plane;
    let owned_quic_ready = owned_quic_claimed
        && status
            .capabilities
            .owned_quic_packet_plane_certificate_der
            .is_some()
        && !status
            .capabilities
            .owned_quic_packet_endpoint_candidates
            .is_empty();
    let owned_udp_ready =
        owned_udp_claimed && !status.capabilities.packet_endpoint_candidates.is_empty();

    if owned_quic_ready || owned_udp_ready {
        return ready();
    }

    if owned_quic_claimed
        && status
            .capabilities
            .owned_quic_packet_plane_certificate_der
            .is_none()
    {
        return blocked(PeerOperationalReadinessReason::OwnedQuicCertificateMissing);
    }

    if owned_quic_claimed
        && status
            .capabilities
            .owned_quic_packet_endpoint_candidates
            .is_empty()
    {
        return blocked(PeerOperationalReadinessReason::OwnedQuicEndpointMissing);
    }

    if owned_udp_claimed && status.capabilities.packet_endpoint_candidates.is_empty() {
        return blocked(PeerOperationalReadinessReason::OwnedUdpEndpointMissing);
    }

    ready()
}

const fn ready() -> PeerOperationalReadiness {
    PeerOperationalReadiness {
        ready: true,
        reason: PeerOperationalReadinessReason::Ready,
    }
}

const fn blocked(reason: PeerOperationalReadinessReason) -> PeerOperationalReadiness {
    PeerOperationalReadiness {
        ready: false,
        reason,
    }
}

const fn service_supports_datagram_packet_path(
    status: &p2p_vpn::runtime::service::ServiceStatusResponse,
) -> bool {
    status.supports_quic_datagrams
        || status.supports_native_quic_datagrams
        || status.supports_owned_udp_packet_plane
        || status.supports_owned_quic_packet_plane
}

const fn configured_path_mtu_estimate(kind: PathKind, mtu: u16) -> u16 {
    match kind {
        PathKind::CircuitRelay => {
            if mtu < 1_200 {
                mtu
            } else {
                1_200
            }
        }
        PathKind::DirectUdpDatagram
        | PathKind::DirectQuicDatagram
        | PathKind::DirectQuicStream
        | PathKind::DirectTcpStream => mtu,
    }
}

fn path_kind_for_multiaddr(address: &str) -> Result<PathKind, String> {
    let address = address
        .parse::<libp2p::Multiaddr>()
        .map_err(|error| format!("failed to parse peer path address: {error:?}"))?;

    if address
        .iter()
        .any(|protocol| matches!(protocol, libp2p::multiaddr::Protocol::P2pCircuit))
    {
        return Ok(PathKind::CircuitRelay);
    }

    if address.iter().any(|protocol| {
        matches!(
            protocol,
            libp2p::multiaddr::Protocol::Quic | libp2p::multiaddr::Protocol::QuicV1
        )
    }) {
        return Ok(PathKind::DirectQuicStream);
    }

    Ok(PathKind::DirectTcpStream)
}

async fn capabilities(path: &PathBuf, live: bool, timeout_seconds: u64) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    config
        .validate_runtime()
        .map_err(|error| format!("config is not runtime-ready: {error:?}"))?;

    let lines = if live {
        Box::pin(capability_lines_live(
            &config,
            Duration::from_secs(timeout_seconds.max(1)),
        ))
        .await?
    } else {
        capability_lines_local(&config)?
    };

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

fn capability_lines_local(config: &Config) -> Result<Vec<String>, String> {
    let routes = config
        .compile_routes()
        .map_err(|error| format!("failed to compile routes: {error:?}"))?;
    let local_peer = config
        .local_peer_id()
        .map_err(|error| format!("failed to parse local peer id: {error:?}"))?;
    let local_peer_display = config
        .local_peer()
        .map_err(|error| format!("failed to resolve local peer: {error:?}"))?;
    let packet_endpoint_candidates = config
        .packet_plane_endpoint_candidates()
        .map_err(|error| format!("failed to parse packet endpoints: {error:?}"))?;
    let packet_plane_listeners = config
        .packet_plane_listen_addrs()
        .map_err(|error| format!("failed to parse packet-plane listeners: {error:?}"))?;
    let owned_udp_packet_plane =
        !packet_endpoint_candidates.is_empty() || !packet_plane_listeners.is_empty();
    let capabilities = p2p_vpn::runtime::control::ControlCapabilities::local(
        &config.network.name,
        config
            .membership_tag()
            .map_err(|error| format!("failed to compute membership tag: {error:?}"))?,
        config.effective_packet_mtu(),
    )
    .with_packet_endpoint_candidates(packet_endpoint_candidates)
    .with_owned_quic_packet_endpoint_candidates(
        config
            .packet_plane_quic_endpoint_candidates()
            .map_err(|error| format!("failed to parse packet QUIC endpoints: {error:?}"))?,
    )
    .with_advertised_routes(
        routes
            .routes_for(local_peer)
            .map(|route| {
                p2p_vpn::runtime::control::ControlRoute::new(route.prefix.to_string(), route.metric)
            })
            .collect(),
    )
    .with_owned_udp_packet_plane(owned_udp_packet_plane);
    let mut lines = vec![
        format!("local peer: {local_peer_display}"),
        format!("configured peers: {}", config.peers.len()),
    ];

    push_capability_lines(&mut lines, "local capability", &capabilities);

    Ok(lines)
}

async fn capability_lines_live(config: &Config, timeout: Duration) -> Result<Vec<String>, String> {
    let mut lines = capability_lines_local(config)?;
    for peer in &config.peers {
        let peer_id = peer
            .id
            .parse::<libp2p::PeerId>()
            .expect("capability config is valid");
        match Box::pin(query_peer_status(config, peer_id, timeout)).await {
            Ok(status) => {
                lines.push(format!("remote capability peer: {}", status.peer));
                push_capability_lines(&mut lines, "remote capability", &status.capabilities);
            }
            Err(error) => lines.push(format!(
                "remote capability peer: {} unreachable error {error:?}",
                peer.id
            )),
        }
    }

    Ok(lines)
}

fn push_capability_lines(
    lines: &mut Vec<String>,
    prefix: &str,
    capabilities: &p2p_vpn::runtime::control::ControlCapabilities,
) {
    lines.push(format!("{prefix} network: {}", capabilities.network_name));
    lines.push(format!(
        "{prefix} membership key matched: {}",
        capabilities.membership_tag.is_some()
    ));
    lines.push(format!(
        "{prefix} wire version: {}",
        capabilities.wire_version
    ));
    lines.push(format!(
        "{prefix} packet protocol: {}",
        capabilities.packet_protocol
    ));
    lines.push(format!(
        "{prefix} packet header length: {}",
        capabilities.packet_header_len
    ));
    lines.push(format!("{prefix} mtu: {}", capabilities.effective_mtu));
    lines.push(format!(
        "{prefix} preferred path: {}",
        path_name(
            PathKind::from_wire_name(&capabilities.preferred_path)
                .unwrap_or(PathKind::DirectQuicStream)
        )
    ));
    lines.push(format!(
        "{prefix} supports quic datagrams: {}",
        capabilities.supports_quic_datagrams
    ));
    lines.push(format!(
        "{prefix} supports native quic datagrams: {}",
        capabilities.supports_native_quic_datagrams
    ));
    lines.push(format!(
        "{prefix} supports owned udp packet plane: {}",
        capabilities.supports_owned_udp_packet_plane
    ));
    lines.push(format!(
        "{prefix} supports owned quic packet plane: {}",
        capabilities.supports_owned_quic_packet_plane
    ));
    lines.push(format!(
        "{prefix} owned quic packet plane certificate bytes: {}",
        capabilities
            .owned_quic_packet_plane_certificate_der
            .as_ref()
            .map_or_else(
                || "none".to_owned(),
                |certificate| certificate.len().to_string()
            )
    ));
    lines.push(format!(
        "{prefix} owned quic packet endpoint candidates: {}",
        capabilities.owned_quic_packet_endpoint_candidates.len()
    ));
    for endpoint in &capabilities.owned_quic_packet_endpoint_candidates {
        lines.push(format!(
            "{prefix} owned quic packet endpoint candidate: {endpoint}"
        ));
    }
    lines.push(format!(
        "{prefix} packet endpoint candidates: {}",
        capabilities.packet_endpoint_candidates.len()
    ));
    for endpoint in &capabilities.packet_endpoint_candidates {
        lines.push(format!("{prefix} packet endpoint candidate: {endpoint}"));
    }
    lines.push(format!(
        "{prefix} advertised routes: {}",
        capabilities.advertised_routes.len()
    ));
    for route in &capabilities.advertised_routes {
        lines.push(format!(
            "{prefix} advertised route: {} metric {}",
            route.prefix, route.metric
        ));
    }
}

async fn peer_status(path: &PathBuf, peer: &str, timeout_seconds: u64) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    config
        .validate_runtime()
        .map_err(|error| format!("config is not runtime-ready: {error:?}"))?;
    let peer = peer
        .parse::<libp2p::PeerId>()
        .map_err(|error| format!("failed to parse peer id: {error}"))?;
    let status = Box::pin(query_peer_status(
        &config,
        peer,
        Duration::from_secs(timeout_seconds.max(1)),
    ))
    .await
    .map_err(|error| format!("peer status query failed: {error:?}"))?;

    for line in peer_status_lines(&status) {
        println!("{line}");
    }

    Ok(())
}

async fn bootstrap_check(args: BootstrapCheckArgs) -> Result<(), String> {
    let config = Config::load(&args.config_path)
        .map_err(|error| format!("failed to load config: {error:?}"))?;
    let report = Box::pin(check_config_bootstrap(
        &config,
        Duration::from_secs(args.timeout_seconds.max(1)),
        args.threshold,
        args.requirements,
    ))
    .await
    .map_err(|error| format!("bootstrap check failed to start: {error:?}"))?;
    let succeeded = report.succeeded();

    for line in report.lines() {
        println!("{line}");
    }

    if let Some(output) = &args.write_report {
        write_bootstrap_check_report(&args, &report, output)?;
    }

    if succeeded {
        Ok(())
    } else {
        Err("bootstrap check did not meet success threshold".to_owned())
    }
}

fn write_bootstrap_check_report(
    args: &BootstrapCheckArgs,
    report: &p2p_vpn::runtime::bootstrap_check::BootstrapCheckReport,
    output: &Path,
) -> Result<(), String> {
    if !args.force && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }

    let rendered = serde_json::to_string_pretty(&bootstrap_check_report_file_json(args, report))
        .map_err(|error| format!("failed to encode bootstrap check report: {error}"))?;
    fs::write(output, format!("{rendered}\n"))
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
    println!("wrote {}", output.display());

    Ok(())
}

async fn relay_check(args: RelayCheckArgs) -> Result<(), String> {
    validate_relay_check_args(&args)?;
    let raw = relay_check_candidate_input(&args)?;
    let addresses = relay_check_candidate_multiaddrs(&raw, args.max_validation_candidates)?;
    let (addresses, skipped_candidates) =
        filter_relay_validation_candidates(addresses, local_relay_candidate_reachability());
    for skipped in &skipped_candidates {
        println!(
            "public relay check skipped: {} reason {}",
            skipped.address, skipped.reason
        );
    }
    if addresses.is_empty() {
        if let Some(output) = &args.write_report {
            write_public_relay_probe_report(
                &args,
                &[],
                &skipped_candidates,
                &p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport {
                    mode: args.mode,
                    candidates: Vec::new(),
                },
                output,
            )?;
        }
        return Err(
            "public relay check did not have a host-reachable candidate to validate".to_owned(),
        );
    }
    let (addresses, limit) =
        limit_relay_validation_candidates(addresses, args.max_validation_candidates);
    let host_reachable_candidates = addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if let Some(limit) = limit {
        println!(
            "public relay check limited: {} of {} host-reachable candidates",
            limit.kept, limit.total
        );
    }
    let report = check_public_relay_candidates(
        &addresses,
        args.mode,
        Duration::from_secs(args.timeout_seconds.max(1)),
    )
    .await;
    let succeeded = report.succeeded();

    for line in report.lines() {
        println!("{line}");
    }

    if let Some(output) = &args.write_report {
        write_public_relay_probe_report(
            &args,
            &host_reachable_candidates,
            &skipped_candidates,
            &report,
            output,
        )?;
    }

    if succeeded {
        if let Some(output) = &args.write_config {
            write_public_relay_config_from_relay_check(&args, &report, output)?;
        }
        if args.write_host_a_config.is_some() || args.write_host_b_config.is_some() {
            write_public_relay_two_host_configs_from_relay_check(&args, &report)?;
        }
        Ok(())
    } else {
        Err("public relay check did not find a usable candidate".to_owned())
    }
}

fn write_public_relay_config_from_relay_check(
    args: &RelayCheckArgs,
    report: &p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport,
    output: &Path,
) -> Result<(), String> {
    if let Some(config_path) = &args.config_path {
        let config = Config::load(config_path)
            .map_err(|error| format!("failed to load config: {error:?}"))?;
        write_public_relay_config_from_base(report, config, output, args.force)
    } else {
        write_public_relay_config_from_probe(report, output, args.force)
    }
}

fn validate_relay_check_args(args: &RelayCheckArgs) -> Result<(), String> {
    if args.relay_candidates.is_empty() && args.relay_candidates_file.is_none() {
        return Err(
            "relay-check needs at least one --relay-candidate or --relay-candidates-file"
                .to_owned(),
        );
    }
    if args.max_validation_candidates == Some(0) {
        return Err("--max-validation-candidates must be greater than zero".to_owned());
    }
    if args.write_host_a_config.is_some() != args.write_host_b_config.is_some() {
        return Err(
            "--write-host-a-config and --write-host-b-config must be supplied together".to_owned(),
        );
    }
    if args.mode == PublicRelayProbeMode::RelayReservation && args.write_host_a_config.is_some() {
        return Err(
            "--write-host-a-config and --write-host-b-config require relayed peer circuit validation"
                .to_owned(),
        );
    }
    if args.two_host_mtu == 0 {
        return Err("--two-host-mtu must be greater than zero".to_owned());
    }
    Ok(())
}

fn relay_check_candidate_input(args: &RelayCheckArgs) -> Result<String, String> {
    let mut sources = Vec::new();
    if !args.relay_candidates.is_empty() {
        sources.push(args.relay_candidates.join("\n"));
    }
    if let Some(path) = &args.relay_candidates_file {
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        sources.push(contents);
    }

    Ok(sources.join("\n"))
}

async fn relay_dcutr_listen(args: RelayDcutrListenArgs) -> Result<(), String> {
    let relay_candidate = args
        .relay_candidate
        .parse::<libp2p::Multiaddr>()
        .map_err(|error| {
            let message = format!("failed to parse --relay-candidate: {error}");
            if let Some(output) = &args.write_report
                && let Err(report_error) =
                    write_public_dcutr_listen_report(&args, None, None, Some(&message), output)
            {
                return format!("{message}; additionally failed to write report: {report_error}");
            }
            message
        })?;
    let listener = match start_public_dcutr_listener(
        &relay_candidate,
        Duration::from_secs(args.reservation_timeout_seconds.max(1)),
    )
    .await
    {
        Ok(listener) => listener,
        Err(error) => {
            if let Some(output) = &args.write_report {
                write_public_dcutr_listen_report(
                    &args,
                    None,
                    error.reservation_evidence.as_ref(),
                    Some(&error.message),
                    output,
                )?;
            }
            return Err(error.message);
        }
    };
    write_public_dcutr_listener_descriptor(
        listener.descriptor(),
        &args.write_descriptor,
        args.force,
    )?;
    if let Some(output) = &args.write_report {
        write_public_dcutr_listen_report(
            &args,
            Some(listener.descriptor()),
            Some(listener.reservation_evidence()),
            None,
            output,
        )?;
    }

    println!("public dcutr listener: ready");
    println!(
        "public dcutr listener peer: {}",
        listener.descriptor().listener_peer
    );
    println!(
        "public dcutr relayed address: {}",
        listener.descriptor().relayed_address
    );
    println!(
        "public dcutr listener serving_seconds: {}",
        args.serve_seconds.max(1)
    );
    Box::pin(listener.serve_for(Duration::from_secs(args.serve_seconds.max(1)))).await;
    println!("public dcutr listener: stopped");

    Ok(())
}

fn write_public_dcutr_listen_report(
    args: &RelayDcutrListenArgs,
    descriptor: Option<&PublicDcutrListenerDescriptor>,
    reservation_evidence: Option<&PublicDcutrReservationEvidence>,
    error: Option<&str>,
    output: &Path,
) -> Result<(), String> {
    if !args.force && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }

    let rendered = serde_json::to_string_pretty(&public_dcutr_listen_report_json(
        args,
        descriptor,
        reservation_evidence,
        error,
    ))
    .map_err(|error| format!("failed to encode public dcutr listen report: {error}"))?;
    fs::write(output, format!("{rendered}\n"))
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
    println!("wrote {}", output.display());

    Ok(())
}

fn write_public_dcutr_listener_descriptor(
    descriptor: &PublicDcutrListenerDescriptor,
    output: &Path,
    force: bool,
) -> Result<(), String> {
    if !force && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }

    descriptor.validate()?;
    let rendered = serde_json::to_string_pretty(descriptor)
        .map_err(|error| format!("failed to encode public dcutr listener descriptor: {error}"))?;
    fs::write(output, format!("{rendered}\n"))
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
    println!("wrote {}", output.display());

    Ok(())
}

async fn relay_dcutr_dial(args: RelayDcutrDialArgs) -> Result<(), String> {
    let descriptor = read_public_dcutr_listener_descriptor(&args.descriptor)?;
    let report = check_public_dcutr_descriptor(
        &descriptor,
        Duration::from_secs(args.timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("public dcutr dial failed to start: {error}"))?;
    let succeeded = report.succeeded();

    for line in report.lines() {
        println!("{line}");
    }

    if let Some(output) = &args.write_report {
        write_public_dcutr_dial_report(&args, &descriptor, &report, output)?;
    }

    if succeeded {
        Ok(())
    } else {
        Err("public dcutr dial did not meet success threshold".to_owned())
    }
}

fn read_public_dcutr_listener_descriptor(
    path: &Path,
) -> Result<PublicDcutrListenerDescriptor, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let descriptor: PublicDcutrListenerDescriptor = serde_json::from_slice(&bytes)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;
    descriptor.validate()?;
    Ok(descriptor)
}

fn write_public_dcutr_dial_report(
    args: &RelayDcutrDialArgs,
    descriptor: &PublicDcutrListenerDescriptor,
    report: &p2p_vpn::runtime::bootstrap_check::BootstrapCheckReport,
    output: &Path,
) -> Result<(), String> {
    if !args.force && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }

    let rendered =
        serde_json::to_string_pretty(&public_dcutr_dial_report_json(args, descriptor, report))
            .map_err(|error| format!("failed to encode public dcutr dial report: {error}"))?;
    fs::write(output, format!("{rendered}\n"))
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
    println!("wrote {}", output.display());

    Ok(())
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct PublicDcutrListenReportJson {
    schema_version: u8,
    mode: &'static str,
    succeeded: bool,
    relay_candidate: String,
    relay_peer: Option<String>,
    listener_peer: Option<String>,
    reservation_timeout_seconds: u64,
    serve_seconds: u64,
    connected_to_relay: bool,
    reservation_accepted: bool,
    relayed_listen_address_observed: bool,
    relayed_address: Option<String>,
    listen_addresses: Vec<String>,
    created_unix_seconds: u64,
    error: Option<String>,
}

fn public_dcutr_listen_report_json(
    args: &RelayDcutrListenArgs,
    descriptor: Option<&PublicDcutrListenerDescriptor>,
    reservation_evidence: Option<&PublicDcutrReservationEvidence>,
    error: Option<&str>,
) -> PublicDcutrListenReportJson {
    PublicDcutrListenReportJson {
        schema_version: 1,
        mode: "public_dcutr_listen",
        succeeded: error.is_none() && descriptor.is_some(),
        relay_candidate: descriptor.map_or_else(
            || args.relay_candidate.clone(),
            |descriptor| descriptor.relay_candidate.clone(),
        ),
        relay_peer: descriptor.map(|descriptor| descriptor.relay_peer.clone()),
        listener_peer: descriptor.map(|descriptor| descriptor.listener_peer.clone()),
        reservation_timeout_seconds: args.reservation_timeout_seconds.max(1),
        serve_seconds: args.serve_seconds.max(1),
        connected_to_relay: reservation_evidence
            .is_some_and(|evidence| evidence.connected_to_relay),
        reservation_accepted: reservation_evidence
            .is_some_and(|evidence| evidence.reservation_accepted),
        relayed_listen_address_observed: reservation_evidence
            .is_some_and(|evidence| evidence.relayed_listen_address_observed),
        relayed_address: descriptor.map(|descriptor| descriptor.relayed_address.clone()),
        listen_addresses: reservation_evidence
            .map_or_else(Vec::new, |evidence| evidence.listen_addresses.clone()),
        created_unix_seconds: descriptor.map_or_else(current_unix_seconds_lossy, |descriptor| {
            descriptor.created_unix_seconds
        }),
        error: error.map(ToOwned::to_owned),
    }
}

fn current_unix_seconds_lossy() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[derive(Serialize)]
struct PublicDcutrDialReportJson<'a> {
    schema_version: u8,
    mode: &'static str,
    succeeded: bool,
    timeout_seconds: u64,
    descriptor: &'a PublicDcutrListenerDescriptor,
    bootstrap: BootstrapCheckReportJson<'a>,
}

fn public_dcutr_dial_report_json<'a>(
    args: &RelayDcutrDialArgs,
    descriptor: &'a PublicDcutrListenerDescriptor,
    report: &'a p2p_vpn::runtime::bootstrap_check::BootstrapCheckReport,
) -> PublicDcutrDialReportJson<'a> {
    PublicDcutrDialReportJson {
        schema_version: 1,
        mode: "public_dcutr_dial",
        succeeded: report.succeeded(),
        timeout_seconds: args.timeout_seconds.max(1),
        descriptor,
        bootstrap: bootstrap_check_report_json(report),
    }
}

fn relay_check_candidate_multiaddrs(
    raw: &str,
    max_validation_candidates: Option<usize>,
) -> Result<Vec<libp2p::Multiaddr>, String> {
    let candidate_limit = if max_validation_candidates.is_some() {
        RELAY_CHECK_CAPPED_INPUT_LIMIT
    } else {
        PUBLIC_RELAY_CANDIDATE_LIMIT
    };
    let addresses = parse_public_relay_addresses_with_limit(raw, candidate_limit)
        .map_err(|error| format!("failed to parse relay candidates: {error}"))?;
    order_relay_validation_candidates(addresses, "relay-check candidate")
}

fn write_public_relay_probe_report(
    args: &RelayCheckArgs,
    host_reachable_candidates: &[String],
    skipped_candidates: &[SkippedRelayCandidate],
    report: &p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport,
    output: &Path,
) -> Result<(), String> {
    if !args.force && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }

    let rendered = serde_json::to_string_pretty(&public_relay_probe_report_json(
        args,
        host_reachable_candidates,
        skipped_candidates,
        report,
    ))
    .map_err(|error| format!("failed to encode public relay report: {error}"))?;
    fs::write(output, format!("{rendered}\n"))
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
    println!("wrote {}", output.display());

    Ok(())
}

#[derive(Serialize)]
struct PublicRelayProbeReportJson<'a> {
    schema_version: u8,
    mode: &'static str,
    succeeded: bool,
    timeout_seconds: u64,
    max_validation_candidates: Option<usize>,
    host_reachable_candidates: &'a [String],
    skipped_candidates: Vec<SkippedRelayCandidateJson<'a>>,
    candidates: Vec<PublicRelayCandidateReportJson<'a>>,
}

#[derive(Serialize)]
struct SkippedRelayCandidateJson<'a> {
    address: &'a str,
    reason: &'static str,
}

#[derive(Serialize)]
struct PublicRelayCandidateReportJson<'a> {
    address: &'a str,
    succeeded: bool,
    failure_stage: &'static str,
    diagnosis: &'static str,
    elapsed_millis: u64,
    error: Option<&'a str>,
    bootstrap: Option<BootstrapCheckReportJson<'a>>,
}

#[derive(Serialize)]
struct BootstrapCheckReportFileJson<'a> {
    schema_version: u8,
    mode: &'static str,
    succeeded: bool,
    timeout_seconds: u64,
    config_path: String,
    bootstrap: BootstrapCheckReportJson<'a>,
}

#[derive(Serialize)]
struct BootstrapCheckReportJson<'a> {
    succeeded: bool,
    threshold: &'static str,
    requirements: BootstrapRequirementsJson,
    kademlia_protocol: &'a str,
    ipfs_compatible: bool,
    dcutr: BootstrapDcutrJson<'a>,
    configured_bootstrap_peers: usize,
    connected_bootstrap_peers: usize,
    dial_failures: usize,
    configured_relay_reservations: usize,
    accepted_relay_reservations: usize,
    relayed_listen_addresses: usize,
    configured_relayed_peer_circuits: usize,
    connected_relayed_peer_circuits: usize,
    relayed_connection_addresses: &'a [String],
    direct_connection_addresses: &'a [String],
    autonat_probe_servers_registered: usize,
    autonat_status: &'static str,
    kademlia: BootstrapKademliaJson,
    membership_records: BootstrapMembershipRecordDhtJson<'a>,
    peer_results: Vec<BootstrapPeerCheckJson<'a>>,
    relay_results: Vec<RelayReservationCheckJson<'a>>,
    relayed_peer_results: Vec<RelayedPeerCircuitCheckJson<'a>>,
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct BootstrapRequirementsJson {
    relay_reservations: bool,
    autonat_status: bool,
    dcutr_ready: bool,
    dcutr_success: bool,
    relayed_peer_circuits: bool,
    membership_records: bool,
}

#[derive(Serialize)]
struct BootstrapDcutrJson<'a> {
    enabled: bool,
    ready: bool,
    successes: usize,
    direct_connections: usize,
    failures: usize,
    last_error: Option<&'a str>,
}

#[derive(Serialize)]
struct BootstrapKademliaJson {
    bootstrap_started: bool,
    rendezvous_lookup_started: bool,
    rendezvous_advertise_started: bool,
}

#[derive(Serialize)]
struct BootstrapMembershipRecordDhtJson<'a> {
    configured_records: usize,
    publish_started: bool,
    publish_succeeded: bool,
    publish_failures: usize,
    lookup_started: bool,
    found_records: usize,
    verified_records: usize,
    accepted_records: usize,
    invalid_records: usize,
    last_error: Option<&'a str>,
}

#[derive(Serialize)]
struct BootstrapPeerCheckJson<'a> {
    peer_id: String,
    address: &'a str,
    connected: bool,
    dial_failures: usize,
    last_error: Option<&'a str>,
}

#[derive(Serialize)]
struct RelayReservationCheckJson<'a> {
    relay_peer_id: String,
    address: &'a str,
    accepted: bool,
    relayed_listen_address: bool,
}

#[derive(Serialize)]
struct RelayedPeerCircuitCheckJson<'a> {
    peer_id: String,
    address: &'a str,
    connected: bool,
    outbound_circuit: bool,
    dial_failures: usize,
    last_error: Option<&'a str>,
}

fn bootstrap_check_report_file_json<'a>(
    args: &BootstrapCheckArgs,
    report: &'a p2p_vpn::runtime::bootstrap_check::BootstrapCheckReport,
) -> BootstrapCheckReportFileJson<'a> {
    BootstrapCheckReportFileJson {
        schema_version: 2,
        mode: "bootstrap_check",
        succeeded: report.succeeded(),
        timeout_seconds: args.timeout_seconds.max(1),
        config_path: args.config_path.display().to_string(),
        bootstrap: bootstrap_check_report_json(report),
    }
}

fn public_relay_probe_report_json<'a>(
    args: &RelayCheckArgs,
    host_reachable_candidates: &'a [String],
    skipped_candidates: &'a [SkippedRelayCandidate],
    report: &'a p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport,
) -> PublicRelayProbeReportJson<'a> {
    PublicRelayProbeReportJson {
        schema_version: 5,
        mode: public_relay_probe_mode_name(args.mode),
        succeeded: report.succeeded(),
        timeout_seconds: args.timeout_seconds.max(1),
        max_validation_candidates: args.max_validation_candidates,
        host_reachable_candidates,
        skipped_candidates: skipped_candidates
            .iter()
            .map(|candidate| SkippedRelayCandidateJson {
                address: &candidate.address,
                reason: candidate.reason,
            })
            .collect(),
        candidates: report
            .candidates
            .iter()
            .map(|candidate| PublicRelayCandidateReportJson {
                address: &candidate.address,
                succeeded: candidate.succeeded,
                failure_stage: public_relay_failure_stage_name(candidate.failure_stage),
                diagnosis: candidate.diagnosis().as_str(),
                elapsed_millis: candidate.elapsed_millis,
                error: candidate.error.as_deref(),
                bootstrap: candidate
                    .bootstrap
                    .as_ref()
                    .map(bootstrap_check_report_json),
            })
            .collect(),
    }
}

fn bootstrap_check_report_json(
    report: &p2p_vpn::runtime::bootstrap_check::BootstrapCheckReport,
) -> BootstrapCheckReportJson<'_> {
    BootstrapCheckReportJson {
        succeeded: report.succeeded(),
        threshold: bootstrap_threshold_name(report.threshold),
        requirements: BootstrapRequirementsJson {
            relay_reservations: report.requirements.relay_reservations,
            autonat_status: report.requirements.autonat_status,
            dcutr_ready: report.requirements.dcutr_ready,
            dcutr_success: report.requirements.dcutr_success,
            relayed_peer_circuits: report.requirements.relayed_peer_circuits,
            membership_records: report.requirements.membership_records,
        },
        kademlia_protocol: &report.kademlia_protocol,
        ipfs_compatible: report.ipfs_compatible,
        dcutr: BootstrapDcutrJson {
            enabled: report.dcutr.enabled,
            ready: report.dcutr.ready,
            successes: report.dcutr.successes,
            direct_connections: report.dcutr.direct_connections,
            failures: report.dcutr.failures,
            last_error: report.dcutr.last_error.as_deref(),
        },
        configured_bootstrap_peers: report.configured_bootstrap_peers,
        connected_bootstrap_peers: report.connected_bootstrap_peers,
        dial_failures: report.dial_failures,
        configured_relay_reservations: report.configured_relay_reservations,
        accepted_relay_reservations: report.accepted_relay_reservations,
        relayed_listen_addresses: report.relayed_listen_addresses,
        configured_relayed_peer_circuits: report.configured_relayed_peer_circuits,
        connected_relayed_peer_circuits: report.connected_relayed_peer_circuits,
        relayed_connection_addresses: &report.relayed_connection_addresses,
        direct_connection_addresses: &report.direct_connection_addresses,
        autonat_probe_servers_registered: report.autonat_probe_servers_registered,
        autonat_status: bootstrap_autonat_status_name(report.autonat_status),
        kademlia: BootstrapKademliaJson {
            bootstrap_started: report.kademlia.bootstrap_started,
            rendezvous_lookup_started: report.kademlia.rendezvous_lookup_started,
            rendezvous_advertise_started: report.kademlia.rendezvous_advertise_started,
        },
        membership_records: BootstrapMembershipRecordDhtJson {
            configured_records: report.membership_records.configured_records,
            publish_started: report.membership_records.publish_started,
            publish_succeeded: report.membership_records.publish_succeeded,
            publish_failures: report.membership_records.publish_failures,
            lookup_started: report.membership_records.lookup_started,
            found_records: report.membership_records.found_records,
            verified_records: report.membership_records.verified_records,
            accepted_records: report.membership_records.accepted_records,
            invalid_records: report.membership_records.invalid_records,
            last_error: report.membership_records.last_error.as_deref(),
        },
        peer_results: report
            .peer_results
            .iter()
            .map(|peer| BootstrapPeerCheckJson {
                peer_id: peer.peer_id.to_string(),
                address: &peer.address,
                connected: peer.connected,
                dial_failures: peer.dial_failures,
                last_error: peer.last_error.as_deref(),
            })
            .collect(),
        relay_results: report
            .relay_results
            .iter()
            .map(|relay| RelayReservationCheckJson {
                relay_peer_id: relay.relay_peer_id.to_string(),
                address: &relay.address,
                accepted: relay.accepted,
                relayed_listen_address: relay.relayed_listen_address,
            })
            .collect(),
        relayed_peer_results: report
            .relayed_peer_results
            .iter()
            .map(|peer| RelayedPeerCircuitCheckJson {
                peer_id: peer.peer_id.to_string(),
                address: &peer.address,
                connected: peer.connected,
                outbound_circuit: peer.outbound_circuit,
                dial_failures: peer.dial_failures,
                last_error: peer.last_error.as_deref(),
            })
            .collect(),
    }
}

const fn public_relay_probe_mode_name(mode: PublicRelayProbeMode) -> &'static str {
    match mode {
        PublicRelayProbeMode::RelayReservation => "relay_reservation",
        PublicRelayProbeMode::RelayedPeerCircuit => "relayed_peer_circuit",
        PublicRelayProbeMode::DcutrSuccess => "dcutr_success",
    }
}

const fn public_relay_failure_stage_name(
    stage: p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateFailureStage,
) -> &'static str {
    match stage {
        p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateFailureStage::None => "none",
        p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateFailureStage::CandidateSetup => {
            "candidate_setup"
        }
        p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateFailureStage::RelayReservation => {
            "relay_reservation"
        }
        p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateFailureStage::RelayedPeerCircuit => {
            "relayed_peer_circuit"
        }
        p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateFailureStage::DcutrSuccess => {
            "dcutr_success"
        }
    }
}

const fn bootstrap_threshold_name(threshold: BootstrapCheckThreshold) -> &'static str {
    match threshold {
        BootstrapCheckThreshold::Any => "any",
        BootstrapCheckThreshold::All => "all",
    }
}

const fn bootstrap_autonat_status_name(
    status: p2p_vpn::runtime::bootstrap_check::BootstrapAutoNatStatus,
) -> &'static str {
    match status {
        p2p_vpn::runtime::bootstrap_check::BootstrapAutoNatStatus::Unknown => "unknown",
        p2p_vpn::runtime::bootstrap_check::BootstrapAutoNatStatus::Public => "public",
        p2p_vpn::runtime::bootstrap_check::BootstrapAutoNatStatus::Private => "private",
    }
}

fn write_public_relay_config_from_probe(
    report: &p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport,
    output: &Path,
    force: bool,
) -> Result<(), String> {
    let relay = public_relay_probe_winner(report)?;
    init_config(public_relay_config_args(output.to_path_buf(), relay, force))
}

fn write_public_relay_config_from_base(
    report: &p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport,
    mut config: Config,
    output: &Path,
    force: bool,
) -> Result<(), String> {
    let relay = public_relay_probe_winner(report)?;
    add_public_relay_infrastructure(&mut config, &relay)?;
    write_config_output(&config, output, force)
}

#[allow(clippy::similar_names)]
fn write_public_relay_two_host_configs_from_relay_check(
    args: &RelayCheckArgs,
    report: &p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport,
) -> Result<(), String> {
    let Some(host_a_output) = &args.write_host_a_config else {
        return Ok(());
    };
    let Some(host_b_output) = &args.write_host_b_config else {
        return Ok(());
    };
    ensure_config_output_writable(host_a_output, args.force)?;
    ensure_config_output_writable(host_b_output, args.force)?;

    let relay = public_relay_probe_winner(report)?;
    let (host_a, host_b) = public_relay_two_host_configs(args, &relay)?;
    write_config_output(&host_a, host_a_output, args.force)?;
    write_config_output(&host_b, host_b_output, args.force)?;
    println!(
        "Host A local peer: {}",
        host_a
            .local_peer()
            .map_err(|error| format!("failed to resolve Host A local peer: {error:?}"))?
    );
    println!(
        "Host B local peer: {}",
        host_b
            .local_peer()
            .map_err(|error| format!("failed to resolve Host B local peer: {error:?}"))?
    );
    println!(
        "Host A ping target: {}",
        route_ping_target(&args.host_b_route, "Host B")?
    );
    println!(
        "Host B ping target: {}",
        route_ping_target(&args.host_a_route, "Host A")?
    );

    Ok(())
}

#[allow(clippy::similar_names)]
fn public_relay_two_host_configs(
    args: &RelayCheckArgs,
    relay: &EndpointArg,
) -> Result<(Config, Config), String> {
    let host_a_identity = NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate Host A identity: {error:?}"))?;
    let host_b_identity = NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate Host B identity: {error:?}"))?;
    let host_a_vpn_ip = route_ping_target(&args.host_a_route, "Host A")?;
    let host_b_vpn_ip = route_ping_target(&args.host_b_route, "Host B")?;

    let mut host_a = InitConfigTemplate {
        identity: host_a_identity.clone(),
        network_name: args.two_host_network.clone(),
        membership_key: None,
        vpn_ip: Some(host_a_vpn_ip),
        local_routes: Vec::new(),
        interface_name: args.host_a_interface.clone(),
        mtu: args.two_host_mtu,
        listen_addresses: default_listen_addresses(),
        external_addresses: Vec::new(),
        packet_plane: PacketPlaneConfig::default(),
        bootstrap_peers: Vec::new(),
        peers: vec![InitPeer {
            id: host_b_identity.peer_id.clone(),
            address: None,
            vpn_ip: Some(host_b_vpn_ip.clone()),
            routes: Vec::new(),
        }],
        discovery: DiscoveryConfig::default(),
        relay: RelayConfig::default(),
    }
    .into_config();
    add_public_relay_infrastructure(&mut host_a, relay)?;
    let host_a = compact_generated_config(host_a);

    let mut host_b = InitConfigTemplate {
        identity: host_b_identity,
        network_name: args.two_host_network.clone(),
        membership_key: None,
        vpn_ip: Some(host_b_vpn_ip),
        local_routes: Vec::new(),
        interface_name: args.host_b_interface.clone(),
        mtu: args.two_host_mtu,
        listen_addresses: default_listen_addresses(),
        external_addresses: Vec::new(),
        packet_plane: PacketPlaneConfig::default(),
        bootstrap_peers: Vec::new(),
        peers: vec![InitPeer {
            id: host_a_identity.peer_id.clone(),
            address: None,
            vpn_ip: host_a.network.vpn_ip.clone(),
            routes: Vec::new(),
        }],
        discovery: DiscoveryConfig::default(),
        relay: RelayConfig::default(),
    }
    .into_config();
    add_public_relay_infrastructure(&mut host_b, relay)?;
    let host_b = compact_generated_config(host_b);

    host_a
        .validate_runtime()
        .map_err(|error| format!("generated Host A config is invalid: {error:?}"))?;
    host_b
        .validate_runtime()
        .map_err(|error| format!("generated Host B config is invalid: {error:?}"))?;

    Ok((host_a, host_b))
}

fn public_relay_probe_winner(
    report: &p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport,
) -> Result<EndpointArg, String> {
    report
        .candidates
        .iter()
        .find(|candidate| candidate.succeeded)
        .ok_or_else(|| "public relay probe succeeded without a winning candidate".to_owned())
        .and_then(|candidate| relay_candidate_endpoint_arg(&candidate.address))
}

fn relay_candidate_endpoint_arg(address: &str) -> Result<EndpointArg, String> {
    let address = address
        .parse::<libp2p::Multiaddr>()
        .map_err(|error| format!("validated relay candidate is not a multiaddr: {error}"))?;
    if address
        .iter()
        .any(|protocol| matches!(protocol, libp2p::multiaddr::Protocol::P2pCircuit))
    {
        return Err("validated relay candidate must be a direct relay address".to_owned());
    }
    let relay = relay_peer_target(&address)
        .ok_or_else(|| "validated relay candidate is missing /p2p/RELAY".to_owned())?;
    Ok(EndpointArg {
        id: relay.to_string(),
        address: Some(address.to_string()),
    })
}

fn public_relay_config_args(output: PathBuf, relay: EndpointArg, force: bool) -> InitConfigArgs {
    InitConfigArgs {
        output,
        network: "lab".to_owned(),
        private_key: None,
        membership_key: None,
        previous_membership_tags: Vec::new(),
        interface: "pv0".to_owned(),
        mtu: 1280,
        listen_addresses: Vec::new(),
        external_addresses: Vec::new(),
        packet_plane: PacketPlaneConfig::default(),
        bootstrap_peers: Vec::new(),
        relay_peers: vec![relay],
        ipfs_bootstrap_peers: true,
        public_ipfs_profile: true,
        peers: Vec::new(),
        vpn_ip: None,
        peer_vpn_ips: Vec::new(),
        local_routes: Vec::new(),
        peer_routes: Vec::new(),
        discovery: InitDiscoveryFlags {
            disable_mdns: false,
            disable_kademlia: false,
            disable_kademlia_provider_advertisement: false,
            disable_dcutr: false,
            disable_autonat: false,
        }
        .into_config(PRIVATE_KADEMLIA_PROTOCOL.to_owned(), false, true),
        relay: RelayConfig::default(),
        queue: QueueConfig::default(),
        resources: ResourceConfig::default(),
        force,
    }
}

fn add_public_relay_infrastructure(config: &mut Config, relay: &EndpointArg) -> Result<(), String> {
    let reservation = relay_reservation_address(relay)?;
    let address = relay.address.clone().ok_or_else(|| {
        format!(
            "relay peer {} must include an address as PEER_ID=MULTIADDR",
            relay.id
        )
    })?;

    if !config
        .network
        .bootstrap_peers
        .iter()
        .any(|peer| peer.id == relay.id && peer.address == address)
    {
        config.network.bootstrap_peers.push(BootstrapPeerConfig {
            id: relay.id.clone(),
            address,
        });
    }

    if !config.network.relay.reservations.contains(&reservation) {
        config.network.relay.reservations.push(reservation);
    }

    Ok(())
}

fn write_config_output(config: &Config, output: &Path, force: bool) -> Result<(), String> {
    ensure_config_output_writable(output, force)?;
    config
        .validate_runtime()
        .map_err(|error| format!("generated config is invalid: {error:?}"))?;
    let rendered = serde_json::to_string_pretty(config)
        .map_err(|error| format!("failed to render config: {error}"))?;

    if output.to_string_lossy() == "-" {
        println!("{rendered}");
    } else {
        fs::write(output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
        println!("wrote {}", output.display());
        println!(
            "local peer: {}",
            config
                .local_peer()
                .map_err(|error| format!("failed to resolve local peer: {error:?}"))?
        );
    }

    Ok(())
}

fn ensure_config_output_writable(output: &Path, force: bool) -> Result<(), String> {
    if !force && output.to_string_lossy() != "-" && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }
    Ok(())
}

fn route_ping_target(route: &str, label: &str) -> Result<String, String> {
    route
        .split_once('/')
        .map_or(route, |(address, _)| address)
        .parse::<IpAddr>()
        .map(|address| address.to_string())
        .map_err(|error| {
            format!("{label} route {route} does not start with an IP address: {error}")
        })
}

async fn relay_scan(args: RelayScanArgs) -> Result<(), String> {
    validate_relay_scan_args(&args)?;
    let config = relay_scan_config(
        args.config_path.as_deref(),
        args.bootstrap_peers.clone(),
        args.ipfs_bootstrap_peers,
    )?;
    if config.network.bootstrap_peers.is_empty() {
        return Err(
            "relay-scan needs at least one bootstrap peer; pass --config, --bootstrap-peer, or --ipfs-bootstrap-peers"
                .to_owned(),
        );
    }
    let report = Box::pin(scan_public_relay_candidates(
        &config,
        Duration::from_secs(args.timeout_seconds.max(1)),
        args.max_candidates,
    ))
    .await
    .map_err(|error| format!("public relay scan failed to start: {error:?}"))?;
    let scan_succeeded = report.succeeded();

    for line in report.lines() {
        println!("{line}");
    }

    if let Some(output) = &args.write_report {
        write_public_relay_scan_report(&args, &report, output)?;
    }

    if !scan_succeeded {
        return Err("public relay scan did not discover a relay-hop candidate".to_owned());
    }

    let candidates = relay_scan_candidate_multiaddrs(&report)?;
    if let Some(output) = &args.write_candidates {
        write_public_relay_candidates(&candidates, output, args.force)?;
    }

    if !args.check_candidates {
        return Ok(());
    }

    let (candidates, skipped_candidates) =
        filter_relay_validation_candidates(candidates, local_relay_candidate_reachability());
    for skipped in skipped_candidates {
        println!(
            "public relay scan validation skipped: {} reason {}",
            skipped.address, skipped.reason
        );
    }
    let (candidates, limit) =
        limit_relay_validation_candidates(candidates, args.max_validation_candidates);
    if let Some(limit) = limit {
        println!(
            "public relay scan validation limited: {} of {} host-reachable candidates",
            limit.kept, limit.total
        );
    }
    if candidates.is_empty() {
        return Err(
            "public relay scan did not have a host-reachable candidate to validate".to_owned(),
        );
    }
    let mode = if args.require_relay_reservation {
        PublicRelayProbeMode::RelayReservation
    } else if args.require_dcutr_success {
        PublicRelayProbeMode::DcutrSuccess
    } else {
        PublicRelayProbeMode::RelayedPeerCircuit
    };
    let probe = check_public_relay_candidates(
        &candidates,
        mode,
        Duration::from_secs(args.candidate_timeout_seconds.max(1)),
    )
    .await;
    let probe_succeeded = probe.succeeded();

    for line in probe.lines() {
        println!("public relay scan validation: {line}");
    }

    if probe_succeeded {
        if let Some(output) = args.write_config {
            if args.config_path.is_some() {
                write_public_relay_config_from_base(&probe, config, &output, args.force)?;
            } else {
                write_public_relay_config_from_probe(&probe, &output, args.force)?;
            }
        }
        Ok(())
    } else {
        Err("public relay scan did not validate a usable candidate".to_owned())
    }
}

fn validate_relay_scan_args(args: &RelayScanArgs) -> Result<(), String> {
    if args.max_candidates == 0 {
        return Err("--max-candidates must be greater than zero".to_owned());
    }
    if args.max_validation_candidates == Some(0) {
        return Err("--max-validation-candidates must be greater than zero".to_owned());
    }
    if args.write_config.is_some() && !args.check_candidates {
        return Err("--write-config requires --check-candidates".to_owned());
    }
    if args.require_relay_reservation && !args.check_candidates {
        return Err("--require-relay-reservation requires --check-candidates".to_owned());
    }
    if args.require_relay_reservation && args.require_dcutr_success {
        return Err(
            "--require-relay-reservation and --require-dcutr-success cannot be used together"
                .to_owned(),
        );
    }
    Ok(())
}

fn write_public_relay_candidates(
    candidates: &[libp2p::Multiaddr],
    output: &Path,
    force: bool,
) -> Result<(), String> {
    if !force && output.to_string_lossy() != "-" && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }
    let rendered = candidates
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    if output.to_string_lossy() == "-" {
        if rendered.is_empty() {
            println!();
        } else {
            println!("{rendered}");
        }
    } else {
        fs::write(output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
        println!("wrote {}", output.display());
    }

    Ok(())
}

fn write_public_relay_scan_report(
    args: &RelayScanArgs,
    report: &p2p_vpn::runtime::bootstrap_check::PublicRelayScanReport,
    output: &Path,
) -> Result<(), String> {
    if !args.force && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }

    let rendered = serde_json::to_string_pretty(&public_relay_scan_report_json(args, report))
        .map_err(|error| format!("failed to encode public relay scan report: {error}"))?;
    fs::write(output, format!("{rendered}\n"))
        .map_err(|error| format!("failed to write {}: {error}", output.display()))?;
    println!("wrote {}", output.display());

    Ok(())
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct PublicRelayScanReportJson<'a> {
    schema_version: u8,
    succeeded: bool,
    timeout_seconds: u64,
    max_candidates: usize,
    check_candidates: bool,
    require_relay_reservation: bool,
    require_dcutr_success: bool,
    candidate_timeout_seconds: u64,
    max_validation_candidates: Option<usize>,
    scanned_bootstrap_peers: usize,
    scanned_peers: usize,
    discovered_routing_peers: usize,
    dialed_routing_peers: usize,
    closest_peer_lookup_started: bool,
    closest_peer_lookup_finished: bool,
    closest_peer_results: usize,
    closest_peer_errors: usize,
    connected_bootstrap_peers: usize,
    identified_peers: usize,
    relay_capable_peers: usize,
    dial_failures: usize,
    candidates: Vec<PublicRelayScanCandidateJson<'a>>,
    peer_results: Vec<PublicRelayScanPeerJson<'a>>,
}

#[derive(Serialize)]
struct PublicRelayScanCandidateJson<'a> {
    peer_id: String,
    address: &'a str,
}

#[derive(Serialize)]
struct PublicRelayScanPeerJson<'a> {
    peer_id: String,
    address: &'a str,
    connected: bool,
    identified: bool,
    relay_hop: bool,
    candidate_addresses: usize,
    dial_failures: usize,
    last_error: Option<&'a str>,
}

fn public_relay_scan_report_json<'a>(
    args: &RelayScanArgs,
    report: &'a p2p_vpn::runtime::bootstrap_check::PublicRelayScanReport,
) -> PublicRelayScanReportJson<'a> {
    PublicRelayScanReportJson {
        schema_version: 1,
        succeeded: report.succeeded(),
        timeout_seconds: args.timeout_seconds.max(1),
        max_candidates: args.max_candidates,
        check_candidates: args.check_candidates,
        require_relay_reservation: args.require_relay_reservation,
        require_dcutr_success: args.require_dcutr_success,
        candidate_timeout_seconds: args.candidate_timeout_seconds.max(1),
        max_validation_candidates: args.max_validation_candidates,
        scanned_bootstrap_peers: report.scanned_bootstrap_peers,
        scanned_peers: report.scanned_peers,
        discovered_routing_peers: report.discovered_routing_peers,
        dialed_routing_peers: report.dialed_routing_peers,
        closest_peer_lookup_started: report.closest_peer_lookup_started,
        closest_peer_lookup_finished: report.closest_peer_lookup_finished,
        closest_peer_results: report.closest_peer_results,
        closest_peer_errors: report.closest_peer_errors,
        connected_bootstrap_peers: report.connected_bootstrap_peers,
        identified_peers: report.identified_peers,
        relay_capable_peers: report.relay_capable_peers,
        dial_failures: report.dial_failures,
        candidates: report
            .candidates
            .iter()
            .map(|candidate| PublicRelayScanCandidateJson {
                peer_id: candidate.peer_id.to_string(),
                address: &candidate.address,
            })
            .collect(),
        peer_results: report
            .peer_results
            .iter()
            .map(|peer| PublicRelayScanPeerJson {
                peer_id: peer.peer_id.to_string(),
                address: &peer.address,
                connected: peer.connected,
                identified: peer.identified,
                relay_hop: peer.relay_hop,
                candidate_addresses: peer.candidate_addresses,
                dial_failures: peer.dial_failures,
                last_error: peer.last_error.as_deref(),
            })
            .collect(),
    }
}

fn relay_scan_candidate_multiaddrs(
    report: &p2p_vpn::runtime::bootstrap_check::PublicRelayScanReport,
) -> Result<Vec<libp2p::Multiaddr>, String> {
    let addresses = report
        .candidates
        .iter()
        .map(|candidate| {
            parse_public_relay_addresses(&candidate.address)
                .and_then(|mut addresses| {
                    addresses
                        .pop()
                        .ok_or_else(|| "empty scanned relay candidate".to_owned())
                })
                .map_err(|error| {
                    format!(
                        "failed to parse scanned relay candidate {}: {error}",
                        candidate.address
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    order_relay_validation_candidates(addresses, "scanned relay candidate")
}

fn order_relay_validation_candidates(
    candidates: Vec<libp2p::Multiaddr>,
    context: &str,
) -> Result<Vec<libp2p::Multiaddr>, String> {
    let parsed = candidates
        .into_iter()
        .map(|address| {
            let peer = relay_peer_target(&address)
                .ok_or_else(|| format!("{context} {address} is missing /p2p/RELAY"))?;
            Ok((peer, address))
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(round_robin_candidates_by_peer(parsed))
}

fn filter_relay_validation_candidates(
    candidates: Vec<libp2p::Multiaddr>,
    reachability: RelayCandidateReachability,
) -> (Vec<libp2p::Multiaddr>, Vec<SkippedRelayCandidate>) {
    let mut kept = Vec::new();
    let mut skipped = Vec::new();

    for candidate in candidates {
        if candidate_requires_ipv4(&candidate) && !reachability.ipv4 {
            skipped.push(SkippedRelayCandidate {
                address: candidate.to_string(),
                reason: "ipv4_unreachable",
            });
        } else if candidate_requires_ipv6(&candidate) && !reachability.ipv6 {
            skipped.push(SkippedRelayCandidate {
                address: candidate.to_string(),
                reason: "ipv6_unreachable",
            });
        } else {
            kept.push(candidate);
        }
    }

    (kept, skipped)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RelayValidationLimit {
    kept: usize,
    total: usize,
}

fn limit_relay_validation_candidates(
    mut candidates: Vec<libp2p::Multiaddr>,
    max_validation_candidates: Option<usize>,
) -> (Vec<libp2p::Multiaddr>, Option<RelayValidationLimit>) {
    let Some(max_validation_candidates) = max_validation_candidates else {
        return (candidates, None);
    };
    let total = candidates.len();
    if total <= max_validation_candidates {
        return (candidates, None);
    }

    candidates.truncate(max_validation_candidates);
    (
        candidates,
        Some(RelayValidationLimit {
            kept: max_validation_candidates,
            total,
        }),
    )
}

fn local_relay_candidate_reachability() -> RelayCandidateReachability {
    RelayCandidateReachability {
        ipv4: local_udp_route_available("0.0.0.0:0", "1.1.1.1:53"),
        ipv6: local_udp_route_available("[::]:0", "[2001:4860:4860::8888]:53"),
    }
}

fn local_udp_route_available(bind: &str, target: &str) -> bool {
    UdpSocket::bind(bind)
        .and_then(|socket| socket.connect(target))
        .is_ok()
}

fn candidate_requires_ipv4(candidate: &libp2p::Multiaddr) -> bool {
    candidate.iter().any(|protocol| {
        matches!(
            protocol,
            libp2p::multiaddr::Protocol::Ip4(_) | libp2p::multiaddr::Protocol::Dns4(_)
        )
    })
}

fn candidate_requires_ipv6(candidate: &libp2p::Multiaddr) -> bool {
    candidate.iter().any(|protocol| {
        matches!(
            protocol,
            libp2p::multiaddr::Protocol::Ip6(_) | libp2p::multiaddr::Protocol::Dns6(_)
        )
    })
}

fn round_robin_candidates_by_peer(
    candidates: Vec<(libp2p::PeerId, libp2p::Multiaddr)>,
) -> Vec<libp2p::Multiaddr> {
    let mut grouped: Vec<(libp2p::PeerId, Vec<libp2p::Multiaddr>)> = Vec::new();
    for (peer, address) in candidates {
        if grouped
            .iter()
            .any(|(_, addresses)| addresses.contains(&address))
        {
            continue;
        }
        if let Some((_, addresses)) = grouped
            .iter_mut()
            .find(|(candidate_peer, _)| *candidate_peer == peer)
        {
            addresses.push(address);
        } else {
            grouped.push((peer, vec![address]));
        }
    }
    for (_, addresses) in &mut grouped {
        addresses.sort_by_key(relay_candidate_validation_rank);
    }

    let mut ordered = Vec::new();
    loop {
        let mut added = false;
        for (_, addresses) in &mut grouped {
            if addresses.is_empty() {
                continue;
            }
            ordered.push(addresses.remove(0));
            added = true;
        }
        if !added {
            break;
        }
    }
    ordered
}

fn relay_candidate_validation_rank(candidate: &libp2p::Multiaddr) -> u8 {
    u8::from(!candidate.iter().any(|protocol| {
        matches!(
            protocol,
            libp2p::multiaddr::Protocol::Quic | libp2p::multiaddr::Protocol::QuicV1
        )
    }))
}

fn relay_scan_config(
    config_path: Option<&Path>,
    bootstrap_peers: Vec<EndpointArg>,
    ipfs_bootstrap_peers: bool,
) -> Result<Config, String> {
    let mut config = if let Some(path) = config_path {
        Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?
    } else {
        NodeIdentity::generate_ed25519()
            .map_err(|error| format!("failed to generate identity: {error:?}"))
            .map(|identity| {
                InitConfigTemplate {
                    identity,
                    network_name: "relay-scan".to_owned(),
                    membership_key: None,
                    vpn_ip: None,
                    local_routes: Vec::new(),
                    interface_name: "pv0".to_owned(),
                    mtu: 1280,
                    listen_addresses: Vec::new(),
                    external_addresses: Vec::new(),
                    packet_plane: PacketPlaneConfig::default(),
                    bootstrap_peers: Vec::new(),
                    peers: Vec::new(),
                    discovery: DiscoveryConfig {
                        mdns: false,
                        kademlia: false,
                        kademlia_provider_advertisement: false,
                        kademlia_protocol: PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
                        dcutr: false,
                        autonat: false,
                    },
                    relay: RelayConfig::default(),
                }
                .into_config()
            })?
    };

    if ipfs_bootstrap_peers {
        config.network.discovery.kademlia = true;
        PUBLIC_IPFS_KADEMLIA_PROTOCOL.clone_into(&mut config.network.discovery.kademlia_protocol);
        config.network.discovery.kademlia_provider_advertisement = false;
    }

    for peer in init_bootstrap_peers(bootstrap_peers, Vec::new(), ipfs_bootstrap_peers) {
        let Some(address) = peer.address else {
            return Err(format!(
                "bootstrap peer {} must include an address as PEER_ID=MULTIADDR",
                peer.id
            ));
        };
        let bootstrap_peer = BootstrapPeerConfig {
            id: peer.id,
            address,
        };
        if !config
            .network
            .bootstrap_peers
            .iter()
            .any(|existing| existing == &bootstrap_peer)
        {
            config.network.bootstrap_peers.push(bootstrap_peer);
        }
    }

    Ok(config)
}

async fn daemon_status(
    socket: &Path,
    timeout_seconds: u64,
    format: MetricsFormat,
) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_status(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon status query failed: {error:?}"))?;

    match format {
        MetricsFormat::Text => {
            for line in lines {
                println!("{line}");
            }
        }
        MetricsFormat::Prometheus => {
            for line in prometheus_lines_from_metric_lines(&lines) {
                println!("{line}");
            }
        }
    }

    Ok(())
}

async fn daemon_state(
    socket: &Path,
    timeout_seconds: u64,
    format: DaemonViewFormat,
) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_state(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon state query failed: {error:?}"))?;

    write_daemon_view_output("state", &lines, format)
}

async fn daemon_peers(
    socket: &Path,
    timeout_seconds: u64,
    format: DaemonViewFormat,
) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_peers(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon peers query failed: {error:?}"))?;

    write_daemon_view_output("peers", &lines, format)
}

async fn daemon_routes(
    socket: &Path,
    timeout_seconds: u64,
    format: DaemonViewFormat,
) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_routes(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon routes query failed: {error:?}"))?;

    write_daemon_view_output("routes", &lines, format)
}

async fn daemon_paths(
    socket: &Path,
    timeout_seconds: u64,
    format: DaemonViewFormat,
) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_paths(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon paths query failed: {error:?}"))?;

    write_daemon_view_output("paths", &lines, format)
}

async fn daemon_mtu(
    socket: &Path,
    timeout_seconds: u64,
    format: DaemonViewFormat,
) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_mtu(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon mtu query failed: {error:?}"))?;

    write_daemon_view_output("mtu", &lines, format)
}

async fn daemon_capabilities(
    socket: &Path,
    timeout_seconds: u64,
    format: DaemonViewFormat,
) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_capabilities(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon capabilities query failed: {error:?}"))?;

    write_daemon_view_output("capabilities", &lines, format)
}

fn write_daemon_view_output(
    view: &'static str,
    lines: &[String],
    format: DaemonViewFormat,
) -> Result<(), String> {
    match format {
        DaemonViewFormat::Text => {
            for line in lines {
                println!("{line}");
            }
        }
        DaemonViewFormat::Json => {
            println!("{}", daemon_view_json(view, lines)?);
        }
    }
    Ok(())
}

fn daemon_view_json(view: &'static str, lines: &[String]) -> Result<String, String> {
    serde_json::to_string_pretty(&DaemonViewJson {
        schema_version: 1,
        view,
        lines,
    })
    .map_err(|error| format!("failed to render daemon {view} JSON: {error}"))
}

#[derive(Serialize)]
struct DaemonViewJson<'a> {
    schema_version: u8,
    view: &'static str,
    lines: &'a [String],
}

async fn daemon_dump(
    socket: &Path,
    output_dir: &Path,
    timeout_seconds: u64,
    force: bool,
) -> Result<(), String> {
    prepare_daemon_dump_dir(output_dir, force)?;
    let timeout = Duration::from_secs(timeout_seconds.max(1));
    let mut views = Vec::new();

    views.push(
        dump_daemon_status(socket, output_dir, timeout)
            .await
            .unwrap_or_else(|error| daemon_dump_error(output_dir, "status", error)),
    );

    for view in ["state", "peers", "routes", "paths", "mtu", "capabilities"] {
        views.push(
            dump_daemon_view(socket, output_dir, timeout, view)
                .await
                .unwrap_or_else(|error| daemon_dump_error(output_dir, view, error)),
        );
    }

    let succeeded = views.iter().all(|view| view.ok);
    let summary = DaemonDumpSummary {
        schema_version: 1,
        socket: socket.display().to_string(),
        output_dir: output_dir.display().to_string(),
        timeout_seconds: timeout.as_secs(),
        succeeded,
        views,
    };
    let rendered = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("failed to render daemon dump summary: {error}"))?;
    fs::write(output_dir.join("summary.json"), format!("{rendered}\n"))
        .map_err(|error| format!("failed to write daemon dump summary: {error}"))?;

    println!("daemon dump: {}", output_dir.display());
    println!("summary: {}", output_dir.join("summary.json").display());
    if succeeded {
        Ok(())
    } else {
        Err("daemon dump captured one or more failed views; inspect summary.json".to_owned())
    }
}

fn prepare_daemon_dump_dir(output_dir: &Path, force: bool) -> Result<(), String> {
    if output_dir.exists() {
        if !output_dir.is_dir() {
            return Err(format!(
                "daemon dump output {} exists and is not a directory",
                output_dir.display()
            ));
        }
        if !force
            && fs::read_dir(output_dir)
                .map_err(|error| {
                    format!(
                        "failed to inspect daemon dump dir {}: {error}",
                        output_dir.display()
                    )
                })?
                .next()
                .is_some()
        {
            return Err(format!(
                "daemon dump output {} is not empty; pass --force to reuse it",
                output_dir.display()
            ));
        }
        if force {
            clear_daemon_dump_artifacts(output_dir)?;
        }
    }

    fs::create_dir_all(output_dir).map_err(|error| {
        format!(
            "failed to create daemon dump dir {}: {error}",
            output_dir.display()
        )
    })
}

fn clear_daemon_dump_artifacts(output_dir: &Path) -> Result<(), String> {
    for name in daemon_dump_artifact_names() {
        let path = output_dir.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "failed to remove stale daemon dump artifact {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

fn daemon_dump_artifact_names() -> &'static [&'static str] {
    &[
        "summary.json",
        "status.txt",
        "status.prometheus",
        "status.error.txt",
        "state.txt",
        "state.json",
        "state.error.txt",
        "peers.txt",
        "peers.json",
        "peers.error.txt",
        "routes.txt",
        "routes.json",
        "routes.error.txt",
        "paths.txt",
        "paths.json",
        "paths.error.txt",
        "mtu.txt",
        "mtu.json",
        "mtu.error.txt",
        "capabilities.txt",
        "capabilities.json",
        "capabilities.error.txt",
    ]
}

async fn dump_daemon_status(
    socket: &Path,
    output_dir: &Path,
    timeout: Duration,
) -> Result<DaemonDumpViewSummary, String> {
    let lines = p2p_vpn::runtime::control_socket::query_status(socket, timeout)
        .await
        .map_err(|error| format!("daemon status query failed: {error:?}"))?;
    let text_path = output_dir.join("status.txt");
    write_lines(&text_path, &lines)?;
    let prometheus_lines = prometheus_lines_from_metric_lines(&lines);
    let prometheus_path = output_dir.join("status.prometheus");
    write_lines(&prometheus_path, &prometheus_lines)?;
    Ok(DaemonDumpViewSummary {
        name: "status",
        ok: true,
        line_count: Some(lines.len()),
        text_path: Some(text_path.display().to_string()),
        json_path: None,
        prometheus_path: Some(prometheus_path.display().to_string()),
        error_path: None,
        error: None,
    })
}

async fn dump_daemon_view(
    socket: &Path,
    output_dir: &Path,
    timeout: Duration,
    view: &'static str,
) -> Result<DaemonDumpViewSummary, String> {
    let lines = match view {
        "state" => p2p_vpn::runtime::control_socket::query_state(socket, timeout).await,
        "peers" => p2p_vpn::runtime::control_socket::query_peers(socket, timeout).await,
        "routes" => p2p_vpn::runtime::control_socket::query_routes(socket, timeout).await,
        "paths" => p2p_vpn::runtime::control_socket::query_paths(socket, timeout).await,
        "mtu" => p2p_vpn::runtime::control_socket::query_mtu(socket, timeout).await,
        "capabilities" => {
            p2p_vpn::runtime::control_socket::query_capabilities(socket, timeout).await
        }
        _ => return Err(format!("unknown daemon dump view {view}")),
    }
    .map_err(|error| format!("daemon {view} query failed: {error:?}"))?;

    let text_path = output_dir.join(format!("{view}.txt"));
    write_lines(&text_path, &lines)?;
    let json_path = output_dir.join(format!("{view}.json"));
    fs::write(&json_path, format!("{}\n", daemon_view_json(view, &lines)?))
        .map_err(|error| format!("failed to write daemon {view} JSON: {error}"))?;

    Ok(DaemonDumpViewSummary {
        name: view,
        ok: true,
        line_count: Some(lines.len()),
        text_path: Some(text_path.display().to_string()),
        json_path: Some(json_path.display().to_string()),
        prometheus_path: None,
        error_path: None,
        error: None,
    })
}

fn daemon_dump_error(
    output_dir: &Path,
    view: &'static str,
    error: String,
) -> DaemonDumpViewSummary {
    let error_path = output_dir.join(format!("{view}.error.txt"));
    let error_path_string = error_path.display().to_string();
    let write_result = fs::write(&error_path, format!("{error}\n"));
    DaemonDumpViewSummary {
        name: view,
        ok: false,
        line_count: None,
        text_path: None,
        json_path: None,
        prometheus_path: None,
        error_path: Some(error_path_string),
        error: Some(match write_result {
            Ok(()) => error,
            Err(write_error) => {
                format!("{error}; additionally failed to write error file: {write_error}")
            }
        }),
    }
}

fn write_lines(path: &Path, lines: &[String]) -> Result<(), String> {
    let mut rendered = String::new();
    for line in lines {
        rendered.push_str(line);
        rendered.push('\n');
    }
    fs::write(path, rendered)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

#[derive(Serialize)]
struct DaemonDumpSummary {
    schema_version: u8,
    socket: String,
    output_dir: String,
    timeout_seconds: u64,
    succeeded: bool,
    views: Vec<DaemonDumpViewSummary>,
}

#[derive(Serialize)]
struct DaemonDumpViewSummary {
    name: &'static str,
    ok: bool,
    line_count: Option<usize>,
    text_path: Option<String>,
    json_path: Option<String>,
    prometheus_path: Option<String>,
    error_path: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
struct DaemonHealthRequirements {
    peers: bool,
    validated_peers: bool,
    supported_paths: bool,
    packet_plane_listener: bool,
    packet_plane_session: bool,
    packet_plane_quic_listener: bool,
    packet_plane_quic_session: bool,
    observed_packet_plane_udp_endpoint: bool,
    observed_packet_plane_quic_endpoint: bool,
    auto_relay_infrastructure_peer: bool,
    auto_relay_candidate: bool,
    auto_relay_reservation: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct DaemonHealthOptions {
    wait: Duration,
    requirements: DaemonHealthRequirements,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DaemonHealthSnapshot {
    daemon_running: bool,
    configured_peers: Option<usize>,
    validated_peers: Option<usize>,
    peers_with_supported_path: Option<usize>,
    packet_plane_listeners: Option<usize>,
    packet_plane_sessions: Option<usize>,
    packet_plane_quic_listeners: Option<usize>,
    packet_plane_quic_sessions: Option<usize>,
    observed_packet_plane_udp_endpoint_candidates: Option<usize>,
    observed_packet_plane_quic_endpoint_candidates: Option<usize>,
    relay_infrastructure_peers: Option<usize>,
    auto_relay_current_candidates: Option<usize>,
    auto_relay_active_reservations: Option<usize>,
}

#[derive(Debug, Eq, PartialEq)]
struct DaemonHealthVerdict {
    ready: bool,
    checks: Vec<DaemonHealthCheck>,
}

#[derive(Debug, Eq, PartialEq)]
struct DaemonHealthCheck {
    name: &'static str,
    ok: bool,
    detail: String,
}

async fn daemon_health(
    socket: &Path,
    timeout_seconds: u64,
    options: DaemonHealthOptions,
) -> Result<(), String> {
    let verdict =
        wait_for_daemon_health(socket, Duration::from_secs(timeout_seconds.max(1)), options)
            .await?;
    print_daemon_health_verdict(&verdict)?;
    if verdict.ready {
        Ok(())
    } else {
        Err(format!(
            "daemon health check failed: {}",
            verdict
                .checks
                .iter()
                .filter(|check| !check.ok)
                .map(|check| check.name)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

async fn wait_for_daemon_health(
    socket: &Path,
    query_timeout: Duration,
    options: DaemonHealthOptions,
) -> Result<DaemonHealthVerdict, String> {
    let deadline = Instant::now() + options.wait;
    loop {
        match p2p_vpn::runtime::control_socket::query_state(socket, query_timeout).await {
            Ok(lines) => {
                let verdict = daemon_health_verdict(&lines, options.requirements);
                if verdict.ready || Instant::now() >= deadline {
                    return Ok(verdict);
                }
            }
            Err(error) => {
                let message = format!("daemon health query failed: {error:?}");
                if Instant::now() >= deadline {
                    return Err(message);
                }
            }
        }
        tokio::time::sleep(daemon_health_poll_interval(options.wait)).await;
    }
}

fn daemon_health_poll_interval(wait: Duration) -> Duration {
    wait.min(Duration::from_secs(1))
        .max(Duration::from_millis(100))
}

fn daemon_health_verdict(
    lines: &[String],
    requirements: DaemonHealthRequirements,
) -> DaemonHealthVerdict {
    let snapshot = parse_daemon_health_snapshot(lines);
    let mut checks = vec![DaemonHealthCheck {
        name: "daemon_running",
        ok: snapshot.daemon_running,
        detail: if snapshot.daemon_running {
            "state running".to_owned()
        } else {
            "missing `daemon state: running`".to_owned()
        },
    }];

    if requirements.peers {
        checks.push(count_check(
            "configured_peers",
            snapshot.configured_peers,
            "configured peers",
        ));
    }
    if requirements.validated_peers {
        checks.push(count_check(
            "validated_peers",
            snapshot.validated_peers,
            "validated peers",
        ));
    }
    if requirements.supported_paths {
        checks.push(count_check(
            "supported_paths",
            snapshot.peers_with_supported_path,
            "peers with supported path",
        ));
    }
    if requirements.packet_plane_listener {
        checks.push(count_check(
            "packet_plane_listener",
            snapshot.packet_plane_listeners,
            "packet plane listeners",
        ));
    }
    if requirements.packet_plane_session {
        checks.push(count_check(
            "packet_plane_session",
            snapshot.packet_plane_sessions,
            "packet plane sessions",
        ));
    }
    if requirements.packet_plane_quic_listener {
        checks.push(count_check(
            "packet_plane_quic_listener",
            snapshot.packet_plane_quic_listeners,
            "packet plane QUIC listeners",
        ));
    }
    if requirements.packet_plane_quic_session {
        checks.push(count_check(
            "packet_plane_quic_session",
            snapshot.packet_plane_quic_sessions,
            "packet plane QUIC sessions",
        ));
    }
    if requirements.observed_packet_plane_udp_endpoint {
        checks.push(count_check(
            "observed_packet_plane_udp_endpoint",
            snapshot.observed_packet_plane_udp_endpoint_candidates,
            "observed packet plane UDP endpoint candidates",
        ));
    }
    if requirements.observed_packet_plane_quic_endpoint {
        checks.push(count_check(
            "observed_packet_plane_quic_endpoint",
            snapshot.observed_packet_plane_quic_endpoint_candidates,
            "observed packet plane QUIC endpoint candidates",
        ));
    }
    if requirements.auto_relay_infrastructure_peer {
        checks.push(count_check(
            "auto_relay_infrastructure_peer",
            snapshot.relay_infrastructure_peers,
            "relay infrastructure peers",
        ));
    }
    if requirements.auto_relay_candidate {
        checks.push(count_check(
            "auto_relay_candidate",
            snapshot.auto_relay_current_candidates,
            "auto relay current candidates",
        ));
    }
    if requirements.auto_relay_reservation {
        checks.push(count_check(
            "auto_relay_reservation",
            snapshot.auto_relay_active_reservations,
            "auto relay active reservations",
        ));
    }

    DaemonHealthVerdict {
        ready: checks.iter().all(|check| check.ok),
        checks,
    }
}

fn count_check(name: &'static str, value: Option<usize>, label: &'static str) -> DaemonHealthCheck {
    DaemonHealthCheck {
        name,
        ok: value.is_some_and(|count| count > 0),
        detail: value.map_or_else(
            || format!("missing `{label}`"),
            |count| format!("{label} {count}"),
        ),
    }
}

fn parse_daemon_health_snapshot(lines: &[String]) -> DaemonHealthSnapshot {
    let mut snapshot = DaemonHealthSnapshot::default();
    for line in lines {
        if line == "daemon state: running" {
            snapshot.daemon_running = true;
        } else if let Some(value) = parse_colon_count(line, "configured peers") {
            snapshot.configured_peers = Some(value);
        } else if let Some(value) = parse_colon_count(line, "validated peers") {
            snapshot.validated_peers = Some(value);
        } else if let Some(value) = parse_metric_count(line, "peers_with_supported_path") {
            snapshot.peers_with_supported_path = Some(value);
        } else if let Some(value) = parse_metric_count(line, "packet_plane_listeners") {
            snapshot.packet_plane_listeners = Some(value);
        } else if let Some(value) = parse_metric_count(line, "packet_plane_sessions") {
            snapshot.packet_plane_sessions = Some(value);
        } else if let Some(value) = parse_metric_count(line, "packet_plane_quic_listeners") {
            snapshot.packet_plane_quic_listeners = Some(value);
        } else if let Some(value) = parse_metric_count(line, "packet_plane_quic_sessions") {
            snapshot.packet_plane_quic_sessions = Some(value);
        } else if let Some(value) =
            parse_metric_count(line, "observed_packet_plane_udp_endpoint_candidates")
        {
            snapshot.observed_packet_plane_udp_endpoint_candidates = Some(value);
        } else if let Some(value) =
            parse_metric_count(line, "observed_packet_plane_quic_endpoint_candidates")
        {
            snapshot.observed_packet_plane_quic_endpoint_candidates = Some(value);
        } else if let Some(value) = parse_metric_count(line, "relay_infrastructure_peers") {
            snapshot.relay_infrastructure_peers = Some(value);
        } else if let Some(value) = parse_metric_count(line, "auto_relay_current_candidates") {
            snapshot.auto_relay_current_candidates = Some(value);
        } else if let Some(value) = parse_metric_count(line, "auto_relay_active_reservations") {
            snapshot.auto_relay_active_reservations = Some(value);
        }
    }
    snapshot
}

fn parse_colon_count(line: &str, label: &str) -> Option<usize> {
    let (actual_label, value) = line.split_once(": ")?;
    (actual_label == label)
        .then_some(value)
        .and_then(|value| value.parse().ok())
}

fn parse_metric_count(line: &str, metric: &str) -> Option<usize> {
    let (actual_metric, value) = line.split_once(' ')?;
    (actual_metric == metric)
        .then_some(value)
        .and_then(|value| value.parse().ok())
}

fn print_daemon_health_verdict(verdict: &DaemonHealthVerdict) -> Result<(), String> {
    let stdout = io::stdout();
    write_daemon_health_verdict(&mut stdout.lock(), verdict)
}

fn write_daemon_health_verdict(
    output: &mut impl io::Write,
    verdict: &DaemonHealthVerdict,
) -> Result<(), String> {
    write_health_line(
        output,
        format_args!("daemon_health_ready {}", verdict.ready),
    )?;
    for check in &verdict.checks {
        write_health_line(
            output,
            format_args!(
                "daemon_health_check {} {} {}",
                check.name,
                if check.ok { "ok" } else { "failed" },
                check.detail
            ),
        )?;
    }
    Ok(())
}

fn write_health_line(
    output: &mut impl io::Write,
    line: std::fmt::Arguments<'_>,
) -> Result<(), String> {
    match writeln!(output, "{line}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(format!("failed to write daemon health output: {error}")),
    }
}

async fn daemon_shutdown(socket: &Path, timeout_seconds: u64) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_shutdown(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon shutdown request failed: {error:?}"))?;

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

fn peer_status_lines(status: &RemotePeerStatus) -> Vec<String> {
    let readiness = peer_operational_readiness(status);
    let mut lines = vec![
        format!("peer: {}", status.peer),
        format!(
            "operational ready: {} reason {}",
            readiness.ready,
            readiness.reason.as_str()
        ),
        format!("network: {}", status.service.network_name),
        format!(
            "membership key matched: {}",
            status.service.membership_tag.is_some()
        ),
        format!("wire version: {}", status.service.wire_version),
        format!("packet protocol: {}", status.service.packet_protocol),
        format!("packet header length: {}", status.service.packet_header_len),
        format!(
            "wire max packet payload length: {}",
            optional_usize(status.service.max_packet_payload_len)
        ),
        format!(
            "packet plane datagram overhead length: {}",
            optional_usize(status.service.packet_plane_datagram_overhead_len)
        ),
        format!(
            "packet plane max payload length: {}",
            optional_usize(status.service.packet_plane_max_payload_len)
        ),
        format!("effective mtu: {}", status.service.effective_mtu),
        format!(
            "supports quic datagrams: {}",
            status.service.supports_quic_datagrams
        ),
        format!(
            "supports native quic datagrams: {}",
            status.service.supports_native_quic_datagrams
        ),
        format!(
            "supports owned udp packet plane: {}",
            status.service.supports_owned_udp_packet_plane
        ),
        format!(
            "supports owned quic packet plane: {}",
            status.service.supports_owned_quic_packet_plane
        ),
        format!(
            "packet plane session ttl seconds: {}",
            optional_seconds(status.service.packet_plane_session_ttl_seconds)
        ),
        format!(
            "packet plane replay windows per session: {}",
            optional_usize(status.service.packet_plane_replay_windows_per_session)
        ),
        format!(
            "reported selected path: {}",
            optional_path_name(status.service.selected_path.as_deref())
        ),
        format!(
            "reported selected path score: {}",
            optional_i32(status.service.selected_path_score)
        ),
        format!(
            "reported selected path mtu: {}",
            optional_u16(status.service.selected_path_mtu)
        ),
        format!(
            "reported selected path rtt ms: {}",
            optional_u16(status.service.selected_path_rtt_ms)
        ),
        format!(
            "preferred path: {}",
            path_name(
                PathKind::from_wire_name(&status.capabilities.preferred_path)
                    .unwrap_or(PathKind::DirectQuicStream)
            )
        ),
        format!(
            "advertised routes: {}",
            status.capabilities.advertised_routes.len()
        ),
    ];

    for route in &status.capabilities.advertised_routes {
        lines.push(format!(
            "advertised route: {} metric {}",
            route.prefix, route.metric
        ));
    }

    lines
}

fn optional_seconds(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |seconds| seconds.to_string())
}

fn optional_i32(value: Option<i32>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |count| count.to_string())
}

fn optional_u16(value: Option<u16>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |count| count.to_string())
}

fn optional_usize(value: Option<usize>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |count| count.to_string())
}

fn optional_path_name(path: Option<&str>) -> String {
    path.and_then(PathKind::from_wire_name)
        .map_or_else(|| "unknown".to_owned(), |path| path_name(path).to_owned())
}

async fn up(
    path: &PathBuf,
    dry_run: bool,
    metrics_interval_seconds: Option<u64>,
    control_socket: Option<PathBuf>,
    pairing_state: Option<PathBuf>,
) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    config
        .validate_runtime()
        .map_err(|error| format!("config is not runtime-ready: {error:?}"))?;
    let runtime = TunRuntimeConfig::from_config(&config)
        .map_err(|error| format!("failed to prepare TUN runtime: {error:?}"))?;

    println!("interface: {}", runtime.name);
    println!("configured mtu: {}", config.interface.mtu);
    println!("effective packet mtu: {}", runtime.mtu);
    println!("address4: {}/32", runtime.addresses.ipv4);
    println!("address6: {}/128", runtime.addresses.ipv6);

    let sysctl_commands = runtime.sysctl_commands();
    let commands = runtime.route_commands();
    if dry_run {
        println!("dry-run: would create Linux TUN interface and run:");
        for command in &sysctl_commands {
            println!("{command}");
        }
        for command in commands {
            println!("{command}");
        }
        for address in config
            .listen_multiaddrs()
            .map_err(|error| format!("failed to parse listen address: {error:?}"))?
        {
            println!("libp2p listen {address}");
        }
        for address in config
            .external_multiaddrs()
            .map_err(|error| format!("failed to parse external address: {error:?}"))?
        {
            println!("libp2p advertise {address}");
        }
        for address in config.relay_reservation_multiaddrs().map_err(|error| {
            format!("failed to parse relay reservation listen address: {error:?}")
        })? {
            println!("libp2p listen {address}");
        }
        if let Some(socket) = &control_socket {
            println!("control socket {socket}", socket = socket.display());
        }
        if let Some(state) = &pairing_state {
            println!("pairing state {state}", state = state.display());
        }
        if config.network.relay.server {
            println!("libp2p relay server enabled");
        }
        return Ok(());
    }

    let device = TunDevice::open(&runtime)
        .map_err(|error| format!("failed to open TUN device: {error:?}"))?;
    println!(
        "created interface: {}",
        device
            .name()
            .map_err(|error| format!("failed to inspect TUN device: {error:?}"))?
    );

    apply_tun_sysctls(&sysctl_commands)?;

    for command in commands {
        let status = command
            .execute()
            .map_err(|error| format!("failed to execute `{command}`: {error}"))?;
        if !status.success() {
            return Err(format!("`{command}` exited with {status}"));
        }
    }

    apply_tun_sysctls(&sysctl_commands)?;
    spawn_tun_sysctl_reconciler(sysctl_commands);

    println!("starting libp2p packet forwarding runtime");
    let metrics_interval = metrics_interval_seconds.map(Duration::from_secs);
    Box::pin(runner::run_config_until(
        config,
        device,
        metrics_interval,
        control_socket,
        pairing_state,
        shutdown_signal(),
    ))
    .await
    .map_err(|error| format!("runtime failed: {error:?}"))
}

fn apply_tun_sysctls(commands: &[SysctlCommand]) -> Result<(), String> {
    for command in commands {
        command
            .execute()
            .map_err(|error| format!("failed to execute `{command}`: {error}"))?;
    }
    Ok(())
}

fn spawn_tun_sysctl_reconciler(commands: Vec<SysctlCommand>) {
    tokio::spawn(async move {
        for delay in [
            Duration::from_secs(2),
            Duration::from_secs(5),
            Duration::from_secs(15),
        ] {
            tokio::time::sleep(delay).await;
            for command in &commands {
                match command.execute() {
                    Ok(()) => {}
                    Err(error) => eprintln!(
                        "level=warn event=tun_sysctl_reconcile_failed command=\"{command}\" error=\"{error}\""
                    ),
                }
            }
        }
    });
}

async fn shutdown_signal() -> ShutdownReason {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.expect("failed to install SIGINT handler");
                ShutdownReason::Interrupt
            }
            _ = terminate.recv() => ShutdownReason::Terminate,
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install interrupt handler");
        ShutdownReason::Interrupt
    }
}

fn path_name(path: PathKind) -> &'static str {
    match path {
        PathKind::DirectUdpDatagram => "direct UDP datagram",
        PathKind::DirectQuicDatagram => "direct QUIC datagram",
        PathKind::DirectQuicStream => "direct QUIC stream",
        PathKind::DirectTcpStream => "direct TCP stream",
        PathKind::CircuitRelay => "circuit relay",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use p2p_vpn::pairing::{PairingResponseOptions, build_pairing_response_at};
    use p2p_vpn::runtime::{
        control_socket::{
            PairRpcMembershipRecordPayload, PairRpcNixPlan, PairRpcPeer, PairRpcReceipt,
        },
        packet::AuthorizedPeers,
    };

    const LIVE_PAIRING_RELAY_MULTIADDR_ENV: &str = "P2P_VPN_LIVE_RELAY_MULTIADDR";
    const LIVE_PAIRING_RELAY_MULTIADDRS_ENV: &str = "P2P_VPN_LIVE_RELAY_MULTIADDRS";
    const LIVE_PAIRING_RELAY_TIMEOUT_SECONDS_ENV: &str = "P2P_VPN_LIVE_RELAY_TIMEOUT_SECONDS";

    fn relay_check_args_for_test() -> RelayCheckArgs {
        RelayCheckArgs {
            config_path: None,
            relay_candidates: Vec::new(),
            relay_candidates_file: None,
            timeout_seconds: 45,
            mode: PublicRelayProbeMode::RelayedPeerCircuit,
            max_validation_candidates: None,
            write_report: None,
            write_config: None,
            write_host_a_config: None,
            write_host_b_config: None,
            two_host_network: "public-vpn-repro".to_owned(),
            host_a_interface: "pv0".to_owned(),
            host_b_interface: "pv0".to_owned(),
            host_a_route: "10.42.0.1/32".to_owned(),
            host_b_route: "10.42.0.2/32".to_owned(),
            two_host_mtu: 1280,
            force: false,
        }
    }

    #[test]
    fn peer_route_arg_parses_default_and_explicit_metric() {
        assert_eq!(
            "12D3KooWPeer=10.42.0.0/24"
                .parse::<PeerRouteArg>()
                .expect("route"),
            PeerRouteArg {
                id: "12D3KooWPeer".to_owned(),
                route: RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 100,
                },
            }
        );
        assert_eq!(
            "12D3KooWPeer=fd00::/64,250"
                .parse::<PeerRouteArg>()
                .expect("route"),
            PeerRouteArg {
                id: "12D3KooWPeer".to_owned(),
                route: RouteConfig {
                    prefix: "fd00::/64".to_owned(),
                    metric: 250,
                },
            }
        );
    }

    #[test]
    fn init_peers_preserves_address_and_route_entries_for_template_merge() {
        let peers = init_peers(
            vec![EndpointArg {
                id: "peer-a".to_owned(),
                address: Some("/ip4/127.0.0.1/tcp/4001".to_owned()),
            }],
            vec![PeerVpnIpArg {
                id: "peer-a".to_owned(),
                vpn_ip: "10.42.0.2".to_owned(),
            }],
            vec![
                PeerRouteArg {
                    id: "peer-a".to_owned(),
                    route: RouteConfig {
                        prefix: "10.42.0.0/24".to_owned(),
                        metric: 100,
                    },
                },
                PeerRouteArg {
                    id: "peer-a".to_owned(),
                    route: RouteConfig {
                        prefix: "fd00::/64".to_owned(),
                        metric: 250,
                    },
                },
            ],
        );

        assert_eq!(
            peers,
            vec![
                InitPeer {
                    id: "peer-a".to_owned(),
                    address: Some("/ip4/127.0.0.1/tcp/4001".to_owned()),
                    vpn_ip: None,
                    routes: Vec::new(),
                },
                InitPeer {
                    id: "peer-a".to_owned(),
                    address: None,
                    vpn_ip: Some("10.42.0.2".to_owned()),
                    routes: Vec::new(),
                },
                InitPeer {
                    id: "peer-a".to_owned(),
                    address: None,
                    vpn_ip: None,
                    routes: vec![RouteConfig {
                        prefix: "10.42.0.0/24".to_owned(),
                        metric: 100,
                    }],
                },
                InitPeer {
                    id: "peer-a".to_owned(),
                    address: None,
                    vpn_ip: None,
                    routes: vec![RouteConfig {
                        prefix: "fd00::/64".to_owned(),
                        metric: 250,
                    }],
                },
            ]
        );
    }

    #[test]
    fn status_lines_report_overlay_addresses_and_route_ownership() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local.peer_id.clone(),
                private_key: Some(local.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.44.0.1".to_owned()),
                routes: vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 0,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    listen: Vec::new(),
                    external_endpoints: vec!["203.0.113.10:51820".to_owned()],
                    quic_listen: Vec::new(),
                    quic_external_endpoints: Vec::new(),
                    session_ttl_seconds: default_packet_plane_session_ttl_seconds(),
                    max_replay_windows_per_session: default_packet_plane_replay_windows_per_session(
                    ),
                },
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![p2p_vpn::config::PeerConfig {
                id: remote.peer_id.clone(),
                name: Some("remote".to_owned()),
                ip: None,
                vpn_ip: Some("10.44.0.2".to_owned()),
                addresses: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 0,
                }],
            }],
            queue: p2p_vpn::config::QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: p2p_vpn::config::ResourceConfig::default(),
        };

        let lines = status_lines(&config).expect("status lines");

        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("local overlay ipv4: "))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("local overlay ipv6: "))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "kademlia scope: ipfs-compatible public dht")
        );
        assert!(
            lines.contains(
                &"relay auto policy: 16 candidates / 2 reservations / 30s retry".to_owned()
            )
        );
        assert!(lines.iter().any(|line| line
            == "protocols: control=/p2p-vpn/control/1 packet=/p2p-vpn/packet/1 service=/p2p-vpn/service/1"));
        assert!(
            lines
                .iter()
                .any(|line| line == "local route: 10.41.0.0/24 metric 0 configured")
        );
        assert!(lines.iter().any(|line| line
            == &format!(
                "peer route: {} 10.42.0.0/24 metric 0 configured",
                remote.peer_id
            )));
    }

    #[test]
    fn route_lines_report_compiled_ownership_and_resolution() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local.peer_id.clone(),
                private_key: Some(local.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.44.0.1".to_owned()),
                routes: vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 50,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    listen: Vec::new(),
                    external_endpoints: vec!["203.0.113.10:51820".to_owned()],
                    quic_listen: Vec::new(),
                    quic_external_endpoints: Vec::new(),
                    session_ttl_seconds: default_packet_plane_session_ttl_seconds(),
                    max_replay_windows_per_session: default_packet_plane_replay_windows_per_session(
                    ),
                },
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![p2p_vpn::config::PeerConfig {
                id: remote.peer_id.clone(),
                name: Some("remote".to_owned()),
                ip: None,
                vpn_ip: Some("10.44.0.2".to_owned()),
                addresses: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 100,
                }],
            }],
            queue: p2p_vpn::config::QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: p2p_vpn::config::ResourceConfig::default(),
        };

        let lines =
            route_lines(&config, Some("10.42.0.9".parse().expect("destination"))).expect("routes");

        assert!(lines.iter().any(|line| line == "compiled routes: 8"));
        assert!(lines.iter().any(|line| line
            == &format!(
                "resolve: 10.42.0.9 -> 10.42.0.0/24 owner peer {} name remote metric 100 configured",
                remote.peer_id
            )));
        assert!(lines.iter().any(|line| line
            == &format!(
                "route: 10.41.0.0/24 owner local {} name - metric 50 configured",
                local.peer_id
            )));
        assert!(lines.iter().any(|line| line
            == &format!(
                "route: 10.42.0.0/24 owner peer {} name remote metric 100 configured",
                remote.peer_id
            )));
        assert!(lines.iter().any(|line| line
            == &format!(
                "route: 10.44.0.1/32 owner local {} name - metric 0 vpn-ip",
                local.peer_id
            )));
        assert!(lines.iter().any(|line| line
            == &format!(
                "route: 10.44.0.2/32 owner peer {} name remote metric 0 vpn-ip",
                remote.peer_id
            )));
    }

    #[test]
    fn route_lines_report_unresolved_destination() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local.peer_id.clone(),
                private_key: Some(local.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    listen: Vec::new(),
                    external_endpoints: vec!["203.0.113.10:51820".to_owned()],
                    quic_listen: Vec::new(),
                    quic_external_endpoints: Vec::new(),
                    session_ttl_seconds: default_packet_plane_session_ttl_seconds(),
                    max_replay_windows_per_session: default_packet_plane_replay_windows_per_session(
                    ),
                },
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: p2p_vpn::config::QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: p2p_vpn::config::ResourceConfig::default(),
        };

        let lines = route_lines(&config, Some("203.0.113.9".parse().expect("destination")))
            .expect("routes");

        assert!(
            lines
                .iter()
                .any(|line| line == "resolve: 203.0.113.9 -> no route")
        );
    }

    #[test]
    fn mtu_lines_configured_report_route_mss_and_path_estimates() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local.peer_id.clone(),
                private_key: Some(local.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![p2p_vpn::config::PeerConfig {
                id: remote.peer_id.clone(),
                name: Some("remote".to_owned()),
                ip: None,
                vpn_ip: None,
                addresses: vec![
                    "/ip4/127.0.0.1/tcp/4001".to_owned(),
                    format!(
                        "/ip4/127.0.0.1/tcp/4002/p2p/{}/p2p-circuit/p2p/{}",
                        relay.peer_id, remote.peer_id
                    ),
                ],
                routes: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 100,
                }],
            }],
            queue: p2p_vpn::config::QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: p2p_vpn::config::ResourceConfig::default(),
        };

        let lines = mtu_lines_configured(&config).expect("mtu lines");

        assert!(
            lines
                .iter()
                .any(|line| line == "effective packet mtu: 1280")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("wire max packet payload length: {MAX_PAYLOAD_LEN}"))
        );
        assert!(lines.iter().any(|line| line
            == &format!(
                "packet plane datagram overhead length: {PACKET_PLANE_DATAGRAM_OVERHEAD_LEN}"
            )));
        assert!(lines.iter().any(|line| line
            == &format!("packet plane max payload length: {PACKET_PLANE_MAX_PAYLOAD_LEN}")));
        assert!(
            lines
                .iter()
                .any(|line| line == OVERLAY_FRAGMENTATION_POLICY_LINE)
        );
        assert!(lines.iter().any(|line| line
            == &format!(
                "route mtu: 10.42.0.0/24 owner peer {} name remote metric 100 configured mtu 1280 advmss 1240",
                remote.peer_id
            )));
        assert!(lines.iter().any(|line| line
            == &format!(
                "path mtu candidate: {} direct TCP stream estimated_mtu 1280 address /ip4/127.0.0.1/tcp/4001",
                remote.peer_id
            )));
        assert!(lines.iter().any(|line| line
            == &format!(
                "path mtu candidate: {} circuit relay estimated_mtu 1200 address /ip4/127.0.0.1/tcp/4002/p2p/{}/p2p-circuit/p2p/{}",
                remote.peer_id, relay.peer_id, remote.peer_id
            )));
    }

    #[test]
    fn peer_live_mtu_lines_report_negotiated_path_ceiling() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut capabilities =
            p2p_vpn::runtime::control::ControlCapabilities::local("lab", None, 1400);
        capabilities.preferred_path = PathKind::CircuitRelay.wire_name().to_owned();
        let status = RemotePeerStatus {
            peer,
            capabilities,
            service: p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1400),
        };
        let mut lines = Vec::new();

        push_peer_live_mtu_lines(&mut lines, &status, 1280);

        assert!(lines.iter().any(|line| line
            == &format!(
                "peer live mtu: {peer} effective_mtu 1400 negotiated_mtu 1280 preferred_path circuit relay path_mtu_estimate 1200 wire_max_payload {MAX_PAYLOAD_LEN} packet_plane_max_payload {PACKET_PLANE_MAX_PAYLOAD_LEN} packet_plane_overhead {PACKET_PLANE_DATAGRAM_OVERHEAD_LEN}"
            )));
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer live fragmentation: {peer} overlay disabled"))
        );
    }

    #[test]
    fn peer_status_lines_report_remote_capabilities() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let capabilities = p2p_vpn::runtime::control::ControlCapabilities::local("lab", None, 1200)
            .with_packet_endpoint_candidates(vec!["203.0.113.10:51820".to_owned()])
            .with_owned_quic_packet_endpoint_candidates(vec!["203.0.113.10:4433".to_owned()])
            .with_owned_quic_packet_plane_certificate(vec![1, 2, 3, 4])
            .with_advertised_routes(vec![p2p_vpn::runtime::control::ControlRoute::new(
                "10.42.0.0/24",
                100,
            )]);
        let status = RemotePeerStatus {
            peer,
            capabilities: capabilities.clone(),
            service: p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200)
                .with_packet_data_plane_capabilities(&capabilities)
                .with_packet_plane_session_ttl_seconds(321)
                .with_packet_plane_replay_windows_per_session(654)
                .with_selected_path(
                    PathKind::DirectQuicDatagram.wire_name().to_owned(),
                    96,
                    1180,
                    Some(37),
                ),
        };

        let lines = peer_status_lines(&status);

        assert!(lines.iter().any(|line| line == &format!("peer: {peer}")));
        assert!(
            lines
                .iter()
                .any(|line| line == "operational ready: true reason ready")
        );
        assert!(lines.iter().any(|line| line == "network: lab"));
        assert!(lines.iter().any(|line| line == "effective mtu: 1200"));
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("wire max packet payload length: {MAX_PAYLOAD_LEN}"))
        );
        assert!(lines.iter().any(|line| line
            == &format!(
                "packet plane datagram overhead length: {PACKET_PLANE_DATAGRAM_OVERHEAD_LEN}"
            )));
        assert!(lines.iter().any(|line| line
            == &format!("packet plane max payload length: {PACKET_PLANE_MAX_PAYLOAD_LEN}")));
        assert!(
            lines
                .iter()
                .any(|line| line == "packet plane session ttl seconds: 321")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "packet plane replay windows per session: 654")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "reported selected path: direct QUIC datagram")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "reported selected path score: 96")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "reported selected path mtu: 1180")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "reported selected path rtt ms: 37")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "preferred path: direct QUIC datagram")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "advertised route: 10.42.0.0/24 metric 100")
        );
    }

    #[test]
    fn peer_lines_configured_report_peer_inventory() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local.peer_id.clone(),
                private_key: Some(local.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![p2p_vpn::config::PeerConfig {
                id: remote.peer_id.clone(),
                name: Some("remote".to_owned()),
                ip: None,
                vpn_ip: None,
                addresses: vec!["/ip4/127.0.0.1/tcp/4001".to_owned()],
                routes: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 100,
                }],
            }],
            queue: p2p_vpn::config::QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: p2p_vpn::config::ResourceConfig::default(),
        };

        let lines = peer_lines_configured(&config).expect("peer lines");

        assert!(lines.iter().any(|line| line == "peers: 1"));
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer: {}", remote.peer_id))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer name: {} remote", remote.peer_id))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer addresses: {} 1", remote.peer_id))
        );
        assert!(lines.iter().any(
            |line| line == &format!("peer address: {} /ip4/127.0.0.1/tcp/4001", remote.peer_id)
        ));
        assert!(lines.iter().any(|line| line
            == &format!(
                "peer route: {} 10.42.0.0/24 metric 100 configured",
                remote.peer_id
            )));
        assert!(lines.iter().any(|line| line
            == &format!(
                "peer route: {} {}/32 metric 0 built-in",
                remote.peer_id,
                p2p_vpn::route::builtin_ipv4(
                    remote
                        .peer_id
                        .parse::<p2p_vpn::PeerId>()
                        .expect("remote peer id")
                )
            )));
    }

    #[test]
    fn peer_live_status_lines_report_probe_results() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let capabilities = p2p_vpn::runtime::control::ControlCapabilities::local("lab", None, 1200)
            .with_packet_endpoint_candidates(vec!["203.0.113.10:51820".to_owned()])
            .with_owned_quic_packet_endpoint_candidates(vec!["203.0.113.10:4433".to_owned()])
            .with_owned_quic_packet_plane_certificate(vec![1, 2, 3, 4])
            .with_advertised_routes(vec![p2p_vpn::runtime::control::ControlRoute::new(
                "10.42.0.0/24",
                100,
            )]);
        let status = RemotePeerStatus {
            peer,
            capabilities: capabilities.clone(),
            service: p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200)
                .with_packet_data_plane_capabilities(&capabilities)
                .with_selected_path(
                    PathKind::DirectQuicStream.wire_name().to_owned(),
                    59,
                    1200,
                    Some(8),
                ),
        };
        let mut lines = Vec::new();

        push_peer_live_status_lines(&mut lines, &status);

        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer live: {peer} reachable"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line
                    == &format!("peer live operational: {peer} ready true reason ready"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer live mtu: {peer} 1200"))
        );
        assert!(lines.iter().any(|line| line
            == &format!("peer live owned quic packet plane certificate bytes: {peer} 4")));
        assert!(
            lines.iter().any(|line| line
                == &format!("peer live reported selected path: {peer} direct QUIC stream"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer live reported selected path score: {peer} 59"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer live reported selected path mtu: {peer} 1200"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer live reported selected path rtt ms: {peer} 8"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer live owned quic packet endpoints: {peer} 1"))
        );
        assert!(lines.iter().any(|line| line
            == &format!("peer live owned quic packet endpoint: {peer} 203.0.113.10:4433")));
        assert!(
            lines
                .iter()
                .any(|line| line == &format!("peer live packet endpoints: {peer} 1"))
        );
        assert!(
            lines.iter().any(
                |line| line == &format!("peer live packet endpoint: {peer} 203.0.113.10:51820")
            )
        );
        assert!(
            lines.iter().any(|line| line
                == &format!("peer live packet plane session ttl seconds: {peer} unknown"))
        );
        assert!(lines.iter().any(|line| line
            == &format!("peer live packet plane replay windows per session: {peer} unknown")));
        assert!(
            lines
                .iter()
                .any(|line| line
                    == &format!("peer live preferred path: {peer} direct QUIC datagram"))
        );
        assert!(
            lines.iter().any(|line| line
                == &format!("peer live advertised route: {peer} 10.42.0.0/24 metric 100"))
        );
    }

    #[test]
    fn path_lines_configured_report_direct_relay_and_discovery_candidates() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local.peer_id.clone(),
                private_key: Some(local.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![
                p2p_vpn::config::PeerConfig {
                    id: remote.peer_id.clone(),
                    name: Some("remote".to_owned()),
                    ip: None,
                    vpn_ip: None,
                    addresses: vec![
                        "/ip4/127.0.0.1/udp/4001/quic-v1".to_owned(),
                        "/ip4/127.0.0.1/tcp/4001".to_owned(),
                        format!(
                            "/ip4/127.0.0.1/tcp/4002/p2p/{}/p2p-circuit/p2p/{}",
                            relay.peer_id, remote.peer_id
                        ),
                    ],
                    routes: Vec::new(),
                },
                p2p_vpn::config::PeerConfig {
                    id: relay.peer_id.clone(),
                    name: Some("discovered".to_owned()),
                    ip: None,
                    vpn_ip: None,
                    addresses: Vec::new(),
                    routes: Vec::new(),
                },
            ],
            queue: p2p_vpn::config::QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: p2p_vpn::config::ResourceConfig::default(),
        };

        let lines = path_lines_configured(&config).expect("path lines");

        assert!(
            lines
                .iter()
                .any(|line| line == "configured path candidates: 3")
        );
        assert!(lines.iter().any(|line| line
            == &format!(
                "peer path candidate: {} direct QUIC stream score 75 estimated_mtu 1280 address /ip4/127.0.0.1/udp/4001/quic-v1",
                remote.peer_id
            )));
        assert!(lines.iter().any(|line| line
            == &format!(
                "peer path candidate: {} direct TCP stream score 40 estimated_mtu 1280 address /ip4/127.0.0.1/tcp/4001",
                remote.peer_id
            )));
        assert!(lines.iter().any(|line| line
            == &format!(
                "peer path discovery: {} enabled true",
                relay.peer_id
            )));
        assert!(lines.iter().any(|line| {
            line.starts_with(&format!(
                "peer path candidate: {} circuit relay score 30 estimated_mtu 1200 address ",
                remote.peer_id
            ))
        }));
    }

    #[test]
    fn peer_live_path_lines_report_probe_readiness() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut capabilities =
            p2p_vpn::runtime::control::ControlCapabilities::local("lab", None, 1200);
        capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        let status = RemotePeerStatus {
            peer,
            capabilities,
            service: p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200),
        };
        let mut lines = Vec::new();

        push_peer_live_path_lines(&mut lines, &status);

        assert!(lines.iter().any(|line| line
            == &format!(
                "peer live path: {peer} reachable preferred direct QUIC datagram score 100 mtu 1200 path_mtu_estimate 1200 reported_selected_path unknown reported_selected_path_score unknown reported_selected_path_mtu unknown reported_selected_path_rtt_ms unknown quic_datagrams false native_quic_datagrams false owned_udp_packet_plane false owned_quic_packet_plane false path_probe_ready false operational_ready false operational_reason remote_datagram_support_missing"
            )));
    }

    #[test]
    fn peer_operational_readiness_reports_owned_quic_blockers() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut capabilities =
            p2p_vpn::runtime::control::ControlCapabilities::local("lab", None, 1200)
                .with_owned_quic_packet_plane(true);
        capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        let service = p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200)
            .with_packet_data_plane_capabilities(&capabilities);
        let status = RemotePeerStatus {
            peer,
            capabilities,
            service,
        };

        assert_eq!(
            peer_operational_readiness(&status),
            blocked(PeerOperationalReadinessReason::OwnedQuicCertificateMissing)
        );

        let mut status = status;
        status.capabilities.owned_quic_packet_plane_certificate_der = Some(vec![1, 2, 3]);

        assert_eq!(
            peer_operational_readiness(&status),
            blocked(PeerOperationalReadinessReason::OwnedQuicEndpointMissing)
        );
    }

    #[test]
    fn peer_operational_readiness_reports_owned_udp_blocker() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut capabilities =
            p2p_vpn::runtime::control::ControlCapabilities::local("lab", None, 1200)
                .with_owned_udp_packet_plane(true);
        capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        let service = p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200)
            .with_packet_data_plane_capabilities(&capabilities);
        let status = RemotePeerStatus {
            peer,
            capabilities,
            service,
        };

        assert_eq!(
            peer_operational_readiness(&status),
            blocked(PeerOperationalReadinessReason::OwnedUdpEndpointMissing)
        );
    }

    #[test]
    fn peer_operational_readiness_allows_owned_udp_when_owned_quic_is_incomplete() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut capabilities =
            p2p_vpn::runtime::control::ControlCapabilities::local("lab", None, 1200)
                .with_owned_quic_packet_plane(true)
                .with_owned_udp_packet_plane(true)
                .with_packet_endpoint_candidates(vec!["203.0.113.10:51820".to_owned()]);
        capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        let service = p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200)
            .with_packet_data_plane_capabilities(&capabilities);
        let status = RemotePeerStatus {
            peer,
            capabilities,
            service,
        };

        assert_eq!(peer_operational_readiness(&status), ready());
    }

    #[test]
    fn capability_lines_local_report_wire_contract_and_routes() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local.peer_id.clone(),
                private_key: Some(local.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 50,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    listen: Vec::new(),
                    external_endpoints: vec!["203.0.113.10:51820".to_owned()],
                    quic_listen: Vec::new(),
                    quic_external_endpoints: Vec::new(),
                    session_ttl_seconds: default_packet_plane_session_ttl_seconds(),
                    max_replay_windows_per_session: default_packet_plane_replay_windows_per_session(
                    ),
                },
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: p2p_vpn::config::QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: p2p_vpn::config::ResourceConfig::default(),
        };

        let lines = capability_lines_local(&config).expect("capabilities");

        assert!(
            lines
                .iter()
                .any(|line| line == "local capability network: lab")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "local capability packet protocol: /p2p-vpn/packet/1")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "local capability preferred path: direct UDP datagram")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "local capability packet endpoint candidates: 1")
        );
        assert!(
            lines.iter().any(
                |line| line == "local capability packet endpoint candidate: 203.0.113.10:51820"
            )
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "local capability advertised routes: 3")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "local capability advertised route: 10.41.0.0/24 metric 50")
        );
    }

    #[test]
    fn capability_lines_report_remote_capability_fields() {
        let peer = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut capabilities = p2p_vpn::runtime::control::ControlCapabilities::local(
            "lab",
            Some("tag".to_owned()),
            1200,
        )
        .with_advertised_routes(vec![p2p_vpn::runtime::control::ControlRoute::new(
            "10.42.0.0/24",
            100,
        )]);
        capabilities = capabilities.with_owned_udp_packet_plane(true);
        capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        let mut lines = vec![format!("remote capability peer: {peer}")];

        push_capability_lines(&mut lines, "remote capability", &capabilities);

        assert!(
            lines
                .iter()
                .any(|line| line == &format!("remote capability peer: {peer}"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "remote capability membership key matched: true")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "remote capability preferred path: direct QUIC datagram")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "remote capability supports quic datagrams: true")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "remote capability supports native quic datagrams: false")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "remote capability supports owned udp packet plane: true")
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "remote capability supports owned quic packet plane: false")
        );
        assert!(lines.iter().any(
            |line| line == "remote capability owned quic packet plane certificate bytes: none"
        ));
        assert!(
            lines
                .iter()
                .any(|line| line == "remote capability advertised route: 10.42.0.0/24 metric 100")
        );
    }

    #[test]
    fn path_kind_for_multiaddr_prefers_relay_then_quic_then_tcp() {
        assert_eq!(
            path_kind_for_multiaddr(
                "/ip4/127.0.0.1/tcp/4002/p2p/12D3KooWLSJY9r3syVF7eh1b5CAJSmQkHdHu1QMUGNXk7Nzd4y6f/p2p-circuit/p2p/12D3KooWBCGXBm96czaYf6X41Hd2mD879WoF5Jyi8YUxi2Tiz3aT"
            ),
            Ok(PathKind::CircuitRelay)
        );
        assert_eq!(
            path_kind_for_multiaddr("/ip4/127.0.0.1/udp/4001/quic-v1"),
            Ok(PathKind::DirectQuicStream)
        );
        assert_eq!(
            path_kind_for_multiaddr("/ip4/127.0.0.1/tcp/4001"),
            Ok(PathKind::DirectTcpStream)
        );
    }

    #[test]
    fn cli_parses_keygen_file_output() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "keygen",
            "--output",
            "/var/lib/p2p-vpn/lab/private.key",
            "--force",
        ])
        .expect("keygen cli");

        let Command::Keygen { output, force } = cli.command else {
            panic!("expected keygen command");
        };
        assert_eq!(output, PathBuf::from("/var/lib/p2p-vpn/lab/private.key"));
        assert!(force);
    }

    #[test]
    fn keygen_writes_private_key_with_owner_only_permissions() {
        let output = temp_config_path("p2p-vpn-keygen");

        keygen(&output, false).expect("write identity");

        let private_key = fs::read_to_string(&output).expect("read identity");
        NodeIdentity::from_private_key(private_key.trim()).expect("valid identity");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &fs::metadata(&output)
                .expect("identity metadata")
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
        assert!(
            keygen(&output, false)
                .expect_err("existing identity must not be replaced")
                .contains("already exists")
        );

        keygen(&output, true).expect("replace identity explicitly");
        let replacement = fs::read_to_string(&output).expect("read replacement identity");
        NodeIdentity::from_private_key(replacement.trim()).expect("valid replacement identity");
        assert_ne!(replacement, private_key);

        fs::remove_file(output).expect("remove test identity");
    }

    #[test]
    fn cli_parses_peer_route_arguments() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "init-config",
            "--kademlia-protocol",
            "/ipfs/kad/1.0.0",
            "--disable-kademlia-provider-advertisement",
            "--membership-key",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "--vpn-ip",
            "10.41.0.2",
            "--local-route",
            "10.41.0.0/24,75",
            "--peer",
            "12D3KooWPeer=/ip4/127.0.0.1/tcp/4001",
            "--peer-vpn-ip",
            "12D3KooWPeer=10.42.0.2",
            "--peer-route",
            "12D3KooWPeer=10.42.0.0/24,250",
            "--relay-max-reservations",
            "17",
            "--relay-max-circuits",
            "19",
            "--queue-max-packets-per-peer",
            "12",
            "--queue-max-bytes-per-peer",
            "8192",
            "--queue-max-packet-age-millis",
            "250",
            "--max-concurrent-control-streams",
            "11",
            "--max-concurrent-packet-streams",
            "22",
            "--max-inbound-packets-per-peer-per-second",
            "333",
            "--max-pairing-requests-per-peer-per-second",
            "5",
            "--max-established-connections",
            "88",
        ])
        .expect("cli");

        let Command::InitConfig {
            peers,
            ipfs_bootstrap_peers,
            vpn_ip,
            local_routes,
            peer_vpn_ips,
            peer_routes,
            kademlia_protocol,
            disable_kademlia_provider_advertisement,
            membership_key,
            relay_max_reservations,
            relay_max_circuits,
            queue_max_packets_per_peer,
            queue_max_bytes_per_peer,
            queue_max_packet_age_millis,
            max_concurrent_control_streams,
            max_concurrent_packet_streams,
            max_inbound_packets_per_peer_per_second,
            max_pairing_requests_per_peer_per_second,
            max_established_connections,
            ..
        } = cli.command
        else {
            panic!("expected init-config command");
        };

        assert_eq!(
            peers,
            vec![EndpointArg {
                id: "12D3KooWPeer".to_owned(),
                address: Some("/ip4/127.0.0.1/tcp/4001".to_owned()),
            }]
        );
        assert!(!ipfs_bootstrap_peers);
        assert_eq!(vpn_ip.as_deref(), Some("10.41.0.2"));
        assert_eq!(
            local_routes,
            vec![LocalRouteArg {
                route: RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 75,
                },
            }]
        );
        assert_eq!(
            peer_vpn_ips,
            vec![PeerVpnIpArg {
                id: "12D3KooWPeer".to_owned(),
                vpn_ip: "10.42.0.2".to_owned(),
            }]
        );
        assert_eq!(
            peer_routes,
            vec![PeerRouteArg {
                id: "12D3KooWPeer".to_owned(),
                route: RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 250,
                },
            }]
        );
        assert_eq!(kademlia_protocol, "/ipfs/kad/1.0.0");
        assert!(disable_kademlia_provider_advertisement);
        assert_eq!(
            membership_key.as_deref(),
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        );
        assert_eq!(relay_max_reservations, 17);
        assert_eq!(relay_max_circuits, 19);
        assert_eq!(queue_max_packets_per_peer, 12);
        assert_eq!(queue_max_bytes_per_peer, 8192);
        assert_eq!(queue_max_packet_age_millis, 250);
        assert_eq!(max_concurrent_control_streams, 11);
        assert_eq!(max_concurrent_packet_streams, 22);
        assert_eq!(max_inbound_packets_per_peer_per_second, 333);
        assert_eq!(max_pairing_requests_per_peer_per_second, 5);
        assert_eq!(max_established_connections, 88);
    }

    #[test]
    fn cli_uses_pv0_interface_defaults() {
        let init = Cli::try_parse_from(["p2p-vpn", "init-config"]).expect("init cli");
        let Command::InitConfig {
            interface: init_interface,
            ..
        } = init.command
        else {
            panic!("expected init-config command");
        };
        assert_eq!(init_interface, "pv0");

        let relay = Cli::try_parse_from([
            "p2p-vpn",
            "relay-check",
            "--relay-candidate",
            "/dns4/relay.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
        ])
        .expect("relay-check cli");
        let Command::RelayCheck {
            host_a_interface,
            host_b_interface,
            ..
        } = relay.command
        else {
            panic!("expected relay-check command");
        };
        assert_eq!(host_a_interface, "pv0");
        assert_eq!(host_b_interface, "pv0");

        let invite = Cli::try_parse_from(["p2p-vpn", "invite-import"]).expect("invite cli");
        let Command::InviteImport {
            interface: invite_interface,
            ..
        } = invite.command
        else {
            panic!("expected invite-import command");
        };
        assert_eq!(invite_interface, "pv0");
    }

    #[test]
    fn cli_parses_auto_relay_policy_arguments() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "init-config",
            "--auto-relay-max-candidates",
            "21",
            "--auto-relay-max-reservations",
            "3",
            "--auto-relay-retry-interval-seconds",
            "11",
        ])
        .expect("cli");

        let Command::InitConfig {
            auto_relay_max_candidates,
            auto_relay_max_reservations,
            auto_relay_retry_interval_seconds,
            ..
        } = cli.command
        else {
            panic!("expected init-config command");
        };

        assert_eq!(auto_relay_max_candidates, 21);
        assert_eq!(auto_relay_max_reservations, 3);
        assert_eq!(auto_relay_retry_interval_seconds, 11);
    }

    #[test]
    fn cli_parses_relay_peer_argument() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "init-config",
            "--relay-peer",
            "12D3KooWRelay=/ip4/127.0.0.1/tcp/4002",
        ])
        .expect("cli");

        let Command::InitConfig { relay_peers, .. } = cli.command else {
            panic!("expected init-config command");
        };

        assert_eq!(
            relay_peers,
            vec![EndpointArg {
                id: "12D3KooWRelay".to_owned(),
                address: Some("/ip4/127.0.0.1/tcp/4002".to_owned()),
            }]
        );
    }

    #[test]
    fn cli_parses_packet_plane_arguments() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "init-config",
            "--packet-listen",
            "0.0.0.0:51820",
            "--packet-endpoint",
            "203.0.113.10:51820",
            "--packet-quic-listen",
            "0.0.0.0:51821",
            "--packet-quic-endpoint",
            "203.0.113.10:51821",
            "--packet-session-ttl-seconds",
            "45",
            "--packet-replay-windows-per-session",
            "16",
        ])
        .expect("cli");

        let Command::InitConfig {
            packet_listen,
            packet_endpoints,
            packet_quic_listen,
            packet_quic_endpoints,
            packet_session_ttl_seconds,
            packet_replay_windows_per_session,
            ..
        } = cli.command
        else {
            panic!("expected init-config command");
        };

        assert_eq!(packet_listen, vec!["0.0.0.0:51820"]);
        assert_eq!(packet_endpoints, vec!["203.0.113.10:51820"]);
        assert_eq!(packet_quic_listen, vec!["0.0.0.0:51821"]);
        assert_eq!(packet_quic_endpoints, vec!["203.0.113.10:51821"]);
        assert_eq!(packet_session_ttl_seconds, 45);
        assert_eq!(packet_replay_windows_per_session, 16);
    }

    #[test]
    fn cli_parses_peer_status_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "peer-status",
            "12D3KooWPeer",
            "--config",
            "node-a.json",
            "--timeout-seconds",
            "3",
        ])
        .expect("cli");

        let Command::PeerStatus {
            peer,
            config,
            timeout_seconds,
        } = cli.command
        else {
            panic!("expected peer-status command");
        };

        assert_eq!(peer, "12D3KooWPeer");
        assert_eq!(config, PathBuf::from("node-a.json"));
        assert_eq!(timeout_seconds, 3);
    }

    #[test]
    fn cli_parses_paths_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "paths",
            "--config",
            "node-a.json",
            "--live",
            "--timeout-seconds",
            "3",
        ])
        .expect("cli");

        let Command::Paths {
            config,
            live,
            timeout_seconds,
        } = cli.command
        else {
            panic!("expected paths command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
        assert!(live);
        assert_eq!(timeout_seconds, 3);
    }

    #[test]
    fn cli_parses_mtu_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "mtu",
            "--config",
            "node-a.json",
            "--live",
            "--timeout-seconds",
            "3",
        ])
        .expect("cli");

        let Command::Mtu {
            config,
            live,
            timeout_seconds,
        } = cli.command
        else {
            panic!("expected mtu command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
        assert!(live);
        assert_eq!(timeout_seconds, 3);
    }

    #[test]
    fn cli_parses_capabilities_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "capabilities",
            "--config",
            "node-a.json",
            "--live",
            "--timeout-seconds",
            "3",
        ])
        .expect("cli");

        let Command::Capabilities {
            config,
            live,
            timeout_seconds,
        } = cli.command
        else {
            panic!("expected capabilities command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
        assert!(live);
        assert_eq!(timeout_seconds, 3);
    }

    #[test]
    fn cli_parses_routes_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "routes",
            "--config",
            "node-a.json",
            "--resolve",
            "10.42.0.9",
        ])
        .expect("cli");

        let Command::Routes { config, resolve } = cli.command else {
            panic!("expected routes command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
        assert_eq!(resolve, Some("10.42.0.9".parse().expect("destination")));
    }

    #[test]
    fn cli_parses_peers_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "peers",
            "--config",
            "node-a.json",
            "--live",
            "--timeout-seconds",
            "3",
        ])
        .expect("cli");

        let Command::Peers {
            config,
            live,
            timeout_seconds,
        } = cli.command
        else {
            panic!("expected peers command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
        assert!(live);
        assert_eq!(timeout_seconds, 3);
    }

    #[test]
    fn cli_parses_metrics_command_format() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "metrics",
            "--config",
            "node-a.json",
            "--format",
            "prometheus",
        ])
        .expect("cli");

        let Command::Metrics { config, format } = cli.command else {
            panic!("expected metrics command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
        assert_eq!(format, MetricsFormat::Prometheus);
    }

    #[test]
    fn cli_parses_instance_commands() {
        let list = Cli::try_parse_from([
            "p2p-vpn",
            "instance",
            "list",
            "--runtime-root",
            "/tmp/run",
            "--format",
            "json",
        ])
        .expect("list command");
        let Command::Instance {
            command:
                InstanceCommand::List {
                    runtime_root,
                    format,
                },
        } = list.command
        else {
            panic!("expected instance list command");
        };
        assert_eq!(runtime_root, PathBuf::from("/tmp/run"));
        assert_eq!(format, InstanceFormat::Json);

        let show = Cli::try_parse_from([
            "p2p-vpn",
            "instance",
            "show",
            "runners",
            "--runtime-root",
            "/tmp/run",
        ])
        .expect("show command");
        let Command::Instance {
            command:
                InstanceCommand::Show {
                    instance,
                    runtime_root,
                    format,
                },
        } = show.command
        else {
            panic!("expected instance show command");
        };
        assert_eq!(instance, "runners");
        assert_eq!(runtime_root, PathBuf::from("/tmp/run"));
        assert_eq!(format, InstanceFormat::Text);
    }

    #[test]
    fn instance_discovery_reports_only_public_identity_metadata() {
        let runtime_root = std::env::temp_dir().join(format!(
            "p2p-vpn-instance-list-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&runtime_root).expect("runtime root");

        let write_instance = |name: &str, network: &str, interface: &str| {
            let identity = NodeIdentity::generate_ed25519().expect("identity");
            let config = Config {
                network: p2p_vpn::config::NetworkConfig {
                    name: network.to_owned(),
                    local_peer: String::new(),
                    private_key: Some(identity.private_key.clone()),
                    membership_key: Some("private-membership-key".to_owned()),
                    previous_membership_tags: Vec::new(),
                    member_records: Vec::new(),
                    vpn_ip: None,
                    routes: Vec::new(),
                    listen_addresses: Vec::new(),
                    external_addresses: Vec::new(),
                    bootstrap_peers: Vec::new(),
                    discovery: DiscoveryConfig::default(),
                    relay: RelayConfig::default(),
                    packet_plane: PacketPlaneConfig::default(),
                },
                interface: p2p_vpn::config::InterfaceConfig {
                    name: interface.to_owned(),
                    mtu: 1_280,
                },
                peers: Vec::new(),
                queue: QueueConfig::default(),
                resources: ResourceConfig::default(),
            };
            let directory = runtime_root.join(format!("p2p-vpn-{name}"));
            fs::create_dir(&directory).expect("instance directory");
            fs::write(
                directory.join("config.json"),
                serde_json::to_vec_pretty(&config).expect("config JSON"),
            )
            .expect("runtime config");
            identity
        };

        let beta = write_instance("beta", "runner-net", "pv1");
        let alpha = write_instance("alpha", "lab-net", "pv0");
        fs::create_dir(runtime_root.join("p2p-vpn-incomplete")).expect("incomplete directory");

        let instances = list_instances(&runtime_root).expect("instance list");
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0].instance, "alpha");
        assert_eq!(instances[0].network, "lab-net");
        assert_eq!(instances[0].interface, "pv0");
        assert_eq!(instances[0].peer_id, alpha.peer_id);
        assert_eq!(instances[1].instance, "beta");
        assert_eq!(instances[1].network, "runner-net");
        assert_eq!(instances[1].interface, "pv1");
        assert_eq!(instances[1].peer_id, beta.peer_id);

        let public_json = serde_json::to_string(&instances).expect("public JSON");
        assert!(!public_json.contains(&alpha.private_key));
        assert!(!public_json.contains(&beta.private_key));
        assert!(!public_json.contains("private-membership-key"));
        assert!(validate_runtime_instance_name("../escape").is_err());

        fs::remove_dir_all(runtime_root).expect("cleanup runtime root");
    }

    #[test]
    fn cli_parses_daemon_status_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "daemon-status",
            "--socket",
            "/run/p2p-vpn-node-a/control.sock",
            "--timeout-seconds",
            "3",
            "--format",
            "prometheus",
        ])
        .expect("cli");

        let Command::DaemonStatus {
            socket,
            timeout_seconds,
            format,
        } = cli.command
        else {
            panic!("expected daemon-status command");
        };

        assert_eq!(socket, PathBuf::from("/run/p2p-vpn-node-a/control.sock"));
        assert_eq!(timeout_seconds, 3);
        assert_eq!(format, MetricsFormat::Prometheus);
    }

    #[test]
    fn cli_parses_daemon_state_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "daemon-state",
            "--socket",
            "/run/p2p-vpn-node-a/control.sock",
            "--timeout-seconds",
            "3",
        ])
        .expect("cli");

        let Command::DaemonState {
            socket,
            timeout_seconds,
            format,
        } = cli.command
        else {
            panic!("expected daemon-state command");
        };

        assert_eq!(socket, PathBuf::from("/run/p2p-vpn-node-a/control.sock"));
        assert_eq!(timeout_seconds, 3);
        assert_eq!(format, DaemonViewFormat::Text);
    }

    #[test]
    fn cli_parses_daemon_view_commands() {
        for (command, expected_format) in [
            ("daemon-peers", DaemonViewFormat::Json),
            ("daemon-routes", DaemonViewFormat::Json),
            ("daemon-paths", DaemonViewFormat::Json),
            ("daemon-mtu", DaemonViewFormat::Json),
            ("daemon-capabilities", DaemonViewFormat::Json),
        ] {
            let cli = Cli::try_parse_from([
                "p2p-vpn",
                command,
                "--socket",
                "/run/p2p-vpn-node-a/control.sock",
                "--timeout-seconds",
                "3",
                "--format",
                "json",
            ])
            .expect("cli");

            let (socket, timeout_seconds, format) = match cli.command {
                Command::DaemonPeers {
                    socket,
                    timeout_seconds,
                    format,
                }
                | Command::DaemonRoutes {
                    socket,
                    timeout_seconds,
                    format,
                }
                | Command::DaemonPaths {
                    socket,
                    timeout_seconds,
                    format,
                }
                | Command::DaemonMtu {
                    socket,
                    timeout_seconds,
                    format,
                }
                | Command::DaemonCapabilities {
                    socket,
                    timeout_seconds,
                    format,
                } => (socket, timeout_seconds, format),
                other => panic!("expected daemon view command, got {other:?}"),
            };

            assert_eq!(socket, PathBuf::from("/run/p2p-vpn-node-a/control.sock"));
            assert_eq!(timeout_seconds, 3);
            assert_eq!(format, expected_format);
        }
    }

    #[test]
    fn cli_parses_daemon_dump_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "daemon-dump",
            "--socket",
            "/run/p2p-vpn-node-a/control.sock",
            "--output-dir",
            "dump",
            "--timeout-seconds",
            "3",
            "--force",
        ])
        .expect("cli");

        let Command::DaemonDump {
            socket,
            output_dir,
            timeout_seconds,
            force,
        } = cli.command
        else {
            panic!("expected daemon-dump command");
        };

        assert_eq!(socket, PathBuf::from("/run/p2p-vpn-node-a/control.sock"));
        assert_eq!(output_dir, PathBuf::from("dump"));
        assert_eq!(timeout_seconds, 3);
        assert!(force);
    }

    #[test]
    fn daemon_view_json_wraps_line_output() {
        let lines = vec![
            "daemon state: running".to_owned(),
            "packet_plane_sessions 1".to_owned(),
        ];
        let rendered = daemon_view_json("state", &lines).expect("daemon view json");
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("parse json");

        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["view"], "state");
        assert_eq!(parsed["lines"][0], "daemon state: running");
        assert_eq!(parsed["lines"][1], "packet_plane_sessions 1");
    }

    #[tokio::test]
    async fn daemon_dump_writes_all_views_and_summary() {
        use p2p_vpn::runtime::control_socket::RuntimeControlRequest;

        let socket_path =
            std::env::temp_dir().join(format!("p2p-vpn-dump-{}-control.sock", std::process::id()));
        let output_dir =
            std::env::temp_dir().join(format!("p2p-vpn-dump-{}-out", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&output_dir);
        let (socket, mut rx) = p2p_vpn::runtime::control_socket::ControlSocket::bind(&socket_path)
            .expect("control socket");
        let responder = tokio::spawn(async move {
            for expected in [
                "status",
                "state",
                "peers",
                "routes",
                "paths",
                "mtu",
                "capabilities",
            ] {
                let request = rx.recv().await.expect("control request");
                match (expected, request) {
                    ("status", RuntimeControlRequest::Status { respond_to }) => respond_to
                        .send(vec!["tun_read_packets 1".to_owned()])
                        .expect("status response accepted"),
                    ("state", RuntimeControlRequest::State { respond_to }) => respond_to
                        .send(vec!["daemon state: running".to_owned()])
                        .expect("state response accepted"),
                    ("peers", RuntimeControlRequest::Peers { respond_to }) => respond_to
                        .send(vec!["peers: 1".to_owned()])
                        .expect("peers response accepted"),
                    ("routes", RuntimeControlRequest::Routes { respond_to }) => respond_to
                        .send(vec!["compiled routes: 1".to_owned()])
                        .expect("routes response accepted"),
                    ("paths", RuntimeControlRequest::Paths { respond_to }) => respond_to
                        .send(vec!["configured path candidates: 1".to_owned()])
                        .expect("paths response accepted"),
                    ("mtu", RuntimeControlRequest::Mtu { respond_to }) => respond_to
                        .send(vec!["effective packet mtu: 1280".to_owned()])
                        .expect("mtu response accepted"),
                    ("capabilities", RuntimeControlRequest::Capabilities { respond_to }) => {
                        respond_to
                            .send(vec!["local capability network: lab".to_owned()])
                            .expect("capabilities response accepted");
                    }
                    (expected, other) => panic!("expected {expected} request, got {other:?}"),
                }
            }
        });

        daemon_dump(&socket_path, &output_dir, 1, false)
            .await
            .expect("daemon dump");
        responder.await.expect("responder");

        assert!(output_dir.join("status.txt").is_file());
        assert!(output_dir.join("status.prometheus").is_file());
        assert!(output_dir.join("state.txt").is_file());
        assert!(output_dir.join("state.json").is_file());
        assert!(output_dir.join("capabilities.json").is_file());

        let summary: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(output_dir.join("summary.json")).expect("summary"),
        )
        .expect("summary json");
        assert_eq!(summary["schema_version"], 1);
        assert_eq!(summary["succeeded"], true);
        assert_eq!(summary["views"].as_array().expect("views").len(), 7);

        drop(socket);
        let _ = std::fs::remove_dir_all(&output_dir);
    }

    #[test]
    fn daemon_dump_rejects_nonempty_output_dir_without_force() {
        let output_dir =
            std::env::temp_dir().join(format!("p2p-vpn-dump-{}-nonempty", std::process::id()));
        let _ = std::fs::remove_dir_all(&output_dir);
        std::fs::create_dir_all(&output_dir).expect("output dir");
        std::fs::write(output_dir.join("existing.txt"), "existing\n").expect("existing file");
        std::fs::write(output_dir.join("state.json"), "stale\n").expect("stale state file");

        let error = prepare_daemon_dump_dir(&output_dir, false).expect_err("nonempty rejected");
        assert!(error.contains("is not empty"));
        prepare_daemon_dump_dir(&output_dir, true).expect("force allows reuse");
        assert!(output_dir.join("existing.txt").is_file());
        assert!(!output_dir.join("state.json").exists());

        let _ = std::fs::remove_dir_all(&output_dir);
    }

    #[test]
    fn cli_parses_daemon_health_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "daemon-health",
            "--socket",
            "/run/p2p-vpn-node-a/control.sock",
            "--timeout-seconds",
            "3",
            "--wait-seconds",
            "7",
            "--require-peers",
            "--require-validated-peers",
            "--require-supported-paths",
            "--require-packet-plane-listener",
            "--require-packet-plane-session",
            "--require-packet-plane-quic-listener",
            "--require-packet-plane-quic-session",
            "--require-observed-packet-plane-udp-endpoint",
            "--require-observed-packet-plane-quic-endpoint",
            "--require-auto-relay-infrastructure-peer",
            "--require-auto-relay-candidate",
            "--require-auto-relay-reservation",
        ])
        .expect("cli");

        let Command::DaemonHealth {
            socket,
            timeout_seconds,
            wait_seconds,
            require_peers,
            require_validated_peers,
            require_supported_paths,
            require_packet_plane_listener,
            require_packet_plane_session,
            require_packet_plane_quic_listener,
            require_packet_plane_quic_session,
            require_observed_packet_plane_udp_endpoint,
            require_observed_packet_plane_quic_endpoint,
            require_auto_relay_infrastructure_peer,
            require_auto_relay_candidate,
            require_auto_relay_reservation,
        } = cli.command
        else {
            panic!("expected daemon-health command");
        };

        assert_eq!(socket, PathBuf::from("/run/p2p-vpn-node-a/control.sock"));
        assert_eq!(timeout_seconds, 3);
        assert_eq!(wait_seconds, 7);
        assert!(require_peers);
        assert!(require_validated_peers);
        assert!(require_supported_paths);
        assert!(require_packet_plane_listener);
        assert!(require_packet_plane_session);
        assert!(require_packet_plane_quic_listener);
        assert!(require_packet_plane_quic_session);
        assert!(require_observed_packet_plane_udp_endpoint);
        assert!(require_observed_packet_plane_quic_endpoint);
        assert!(require_auto_relay_infrastructure_peer);
        assert!(require_auto_relay_candidate);
        assert!(require_auto_relay_reservation);
    }

    #[test]
    fn cli_parses_daemon_shutdown_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "daemon-shutdown",
            "--socket",
            "/run/p2p-vpn-node-a/control.sock",
            "--timeout-seconds",
            "3",
        ])
        .expect("cli");

        let Command::DaemonShutdown {
            socket,
            timeout_seconds,
        } = cli.command
        else {
            panic!("expected daemon-shutdown command");
        };

        assert_eq!(socket, PathBuf::from("/run/p2p-vpn-node-a/control.sock"));
        assert_eq!(timeout_seconds, 3);
    }

    #[test]
    fn daemon_health_defaults_to_running_daemon_check() {
        let lines = vec![
            "daemon state: running".to_owned(),
            "configured peers: 0".to_owned(),
            "validated peers: 0".to_owned(),
            "peers_with_supported_path 0".to_owned(),
        ];

        let verdict = daemon_health_verdict(&lines, DaemonHealthRequirements::default());

        assert!(verdict.ready);
        assert_eq!(verdict.checks.len(), 1);
        assert_eq!(verdict.checks[0].name, "daemon_running");
        assert!(verdict.checks[0].ok);
    }

    #[test]
    fn daemon_health_output_tolerates_closed_pipeline() {
        #[derive(Default)]
        struct PipeAfterFirstLine {
            bytes: Vec<u8>,
            closed: bool,
        }

        impl std::io::Write for PipeAfterFirstLine {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.closed {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                let written = bytes
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |index| index + 1);
                self.bytes.extend_from_slice(&bytes[..written]);
                self.closed = written < bytes.len() || bytes[..written].ends_with(b"\n");
                Ok(written)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let verdict = DaemonHealthVerdict {
            ready: true,
            checks: vec![DaemonHealthCheck {
                name: "daemon_running",
                ok: true,
                detail: "state running".to_owned(),
            }],
        };
        let mut output = PipeAfterFirstLine::default();

        write_daemon_health_verdict(&mut output, &verdict).expect("broken pipe is normal");

        assert_eq!(output.bytes, b"daemon_health_ready true\n");
    }

    #[test]
    fn daemon_health_reports_failed_requirements() {
        let lines = vec![
            "daemon state: running".to_owned(),
            "configured peers: 1".to_owned(),
            "validated peers: 0".to_owned(),
            "peers_with_supported_path 0".to_owned(),
            "packet_plane_listeners 1".to_owned(),
            "packet_plane_sessions 0".to_owned(),
            "packet_plane_quic_listeners 0".to_owned(),
            "packet_plane_quic_sessions 0".to_owned(),
            "observed_packet_plane_udp_endpoint_candidates 0".to_owned(),
            "observed_packet_plane_quic_endpoint_candidates 0".to_owned(),
            "relay_infrastructure_peers 0".to_owned(),
            "auto_relay_current_candidates 0".to_owned(),
            "auto_relay_active_reservations 0".to_owned(),
        ];

        let verdict = daemon_health_verdict(
            &lines,
            DaemonHealthRequirements {
                peers: true,
                validated_peers: true,
                supported_paths: true,
                packet_plane_listener: true,
                packet_plane_session: true,
                packet_plane_quic_listener: true,
                packet_plane_quic_session: true,
                observed_packet_plane_udp_endpoint: true,
                observed_packet_plane_quic_endpoint: true,
                auto_relay_infrastructure_peer: true,
                auto_relay_candidate: true,
                auto_relay_reservation: true,
            },
        );

        assert!(!verdict.ready);
        assert!(
            verdict
                .checks
                .iter()
                .any(|check| check.name == "configured_peers" && check.ok)
        );
        for failed in [
            "validated_peers",
            "supported_paths",
            "packet_plane_session",
            "packet_plane_quic_listener",
            "packet_plane_quic_session",
            "observed_packet_plane_udp_endpoint",
            "observed_packet_plane_quic_endpoint",
            "auto_relay_infrastructure_peer",
            "auto_relay_candidate",
            "auto_relay_reservation",
        ] {
            assert!(
                verdict
                    .checks
                    .iter()
                    .any(|check| check.name == failed && !check.ok),
                "missing failed check {failed}"
            );
        }
    }

    #[test]
    fn daemon_health_snapshot_parses_state_lines() {
        let lines = vec![
            "daemon state: running".to_owned(),
            "configured peers: 2".to_owned(),
            "validated peers: 1".to_owned(),
            "peers_with_supported_path 1".to_owned(),
            "packet_plane_listeners 1".to_owned(),
            "packet_plane_sessions 1".to_owned(),
            "packet_plane_quic_listeners 1".to_owned(),
            "packet_plane_quic_sessions 1".to_owned(),
            "observed_packet_plane_udp_endpoint_candidates 1".to_owned(),
            "observed_packet_plane_quic_endpoint_candidates 1".to_owned(),
            "relay_infrastructure_peers 1".to_owned(),
            "auto_relay_current_candidates 2".to_owned(),
            "auto_relay_active_reservations 1".to_owned(),
        ];

        assert_eq!(
            parse_daemon_health_snapshot(&lines),
            DaemonHealthSnapshot {
                daemon_running: true,
                configured_peers: Some(2),
                validated_peers: Some(1),
                peers_with_supported_path: Some(1),
                packet_plane_listeners: Some(1),
                packet_plane_sessions: Some(1),
                packet_plane_quic_listeners: Some(1),
                packet_plane_quic_sessions: Some(1),
                observed_packet_plane_udp_endpoint_candidates: Some(1),
                observed_packet_plane_quic_endpoint_candidates: Some(1),
                relay_infrastructure_peers: Some(1),
                auto_relay_current_candidates: Some(2),
                auto_relay_active_reservations: Some(1),
            }
        );
    }

    #[test]
    fn daemon_health_poll_interval_is_bounded() {
        assert_eq!(
            daemon_health_poll_interval(Duration::from_millis(1)),
            Duration::from_millis(100)
        );
        assert_eq!(
            daemon_health_poll_interval(Duration::from_millis(250)),
            Duration::from_millis(250)
        );
        assert_eq!(
            daemon_health_poll_interval(Duration::from_secs(30)),
            Duration::from_secs(1)
        );
    }

    #[tokio::test]
    async fn daemon_health_waits_until_requirements_pass() {
        let path =
            std::env::temp_dir().join(format!("p2p-vpn-health-{}-wait.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (socket, mut rx) =
            p2p_vpn::runtime::control_socket::ControlSocket::bind(&path).expect("control socket");
        let responder = tokio::spawn(async move {
            for response in [
                vec![
                    "daemon state: running".to_owned(),
                    "configured peers: 1".to_owned(),
                    "validated peers: 0".to_owned(),
                ],
                vec![
                    "daemon state: running".to_owned(),
                    "configured peers: 1".to_owned(),
                    "validated peers: 1".to_owned(),
                ],
            ] {
                let Some(p2p_vpn::runtime::control_socket::RuntimeControlRequest::State {
                    respond_to,
                }) = rx.recv().await
                else {
                    panic!("expected state request");
                };
                respond_to.send(response).expect("state response accepted");
            }
        });

        let verdict = wait_for_daemon_health(
            &path,
            Duration::from_secs(1),
            DaemonHealthOptions {
                wait: Duration::from_secs(1),
                requirements: DaemonHealthRequirements {
                    validated_peers: true,
                    ..DaemonHealthRequirements::default()
                },
            },
        )
        .await
        .expect("health verdict");

        assert!(verdict.ready);
        assert!(
            verdict
                .checks
                .iter()
                .any(|check| check.name == "validated_peers" && check.ok)
        );
        responder.await.expect("responder");
        drop(socket);
    }

    #[tokio::test]
    async fn daemon_health_waits_for_socket_to_appear() {
        let path = std::env::temp_dir().join(format!(
            "p2p-vpn-health-{}-delayed.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener_path = path.clone();
        let responder = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let (socket, mut rx) =
                p2p_vpn::runtime::control_socket::ControlSocket::bind(&listener_path)
                    .expect("control socket");
            let Some(p2p_vpn::runtime::control_socket::RuntimeControlRequest::State { respond_to }) =
                rx.recv().await
            else {
                panic!("expected state request");
            };
            respond_to
                .send(vec!["daemon state: running".to_owned()])
                .expect("state response accepted");
            drop(socket);
        });

        let verdict = wait_for_daemon_health(
            &path,
            Duration::from_millis(100),
            DaemonHealthOptions {
                wait: Duration::from_millis(250),
                requirements: DaemonHealthRequirements::default(),
            },
        )
        .await
        .expect("health verdict after socket appears");

        assert!(verdict.ready);
        responder.await.expect("responder");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cli_parses_bootstrap_check_report_options() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "bootstrap-check",
            "--config",
            "p2p-vpn-public.json",
            "--timeout-seconds",
            "90",
            "--require-all",
            "--require-relay-reservations",
            "--require-autonat-status",
            "--require-dcutr-ready",
            "--require-dcutr-success",
            "--require-relayed-peer-circuits",
            "--require-membership-records",
            "--write-report",
            "bootstrap-report.json",
            "--force",
        ])
        .expect("cli");

        let Command::BootstrapCheck {
            config,
            timeout_seconds,
            require_all,
            require_relay_reservations,
            require_autonat_status,
            require_dcutr_ready,
            require_dcutr_success,
            require_relayed_peer_circuits,
            require_membership_records,
            write_report,
            force,
        } = cli.command
        else {
            panic!("expected bootstrap-check command");
        };

        assert_eq!(config, PathBuf::from("p2p-vpn-public.json"));
        assert_eq!(timeout_seconds, 90);
        assert!(require_all);
        assert!(require_relay_reservations);
        assert!(require_autonat_status);
        assert!(require_dcutr_ready);
        assert!(require_dcutr_success);
        assert!(require_relayed_peer_circuits);
        assert!(require_membership_records);
        assert_eq!(write_report, Some(PathBuf::from("bootstrap-report.json")));
        assert!(force);
    }

    #[test]
    fn bootstrap_check_writes_machine_readable_report() {
        let report = failed_public_dcutr_bootstrap_report();
        let output = std::env::temp_dir().join(format!(
            "p2p-vpn-bootstrap-check-report-{}-{}.json",
            std::process::id(),
            "report"
        ));
        let _ = fs::remove_file(&output);
        let args = BootstrapCheckArgs {
            config_path: PathBuf::from("p2p-vpn-public.json"),
            timeout_seconds: 0,
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: true,
                dcutr_ready: true,
                dcutr_success: true,
                relayed_peer_circuits: true,
                membership_records: false,
            },
            write_report: Some(output.clone()),
            force: false,
        };

        write_bootstrap_check_report(&args, &report, &output).expect("write report");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&output).expect("report file")).expect("json report");

        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["mode"], "bootstrap_check");
        assert_eq!(value["succeeded"], false);
        assert_eq!(value["timeout_seconds"], 1);
        assert_eq!(value["config_path"], "p2p-vpn-public.json");
        assert_failed_public_dcutr_bootstrap_json(&value["bootstrap"]);
        assert!(
            write_bootstrap_check_report(&args, &report, &output)
                .expect_err("overwrite should require force")
                .contains("pass --force")
        );
        fs::remove_file(&output).expect("remove report");
    }

    #[test]
    fn cli_parses_relay_check_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "relay-check",
            "--config",
            "base-config.json",
            "--relay-candidate",
            "/dns4/relay.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
            "--relay-candidates-file",
            "relay-candidates.txt",
            "--require-dcutr-success",
            "--timeout-seconds",
            "60",
            "--max-validation-candidates",
            "3",
            "--write-report",
            "relay-report.json",
            "--write-config",
            "relay-config.json",
            "--write-host-a-config",
            "host-a.json",
            "--write-host-b-config",
            "host-b.json",
            "--two-host-network",
            "public-lab",
            "--host-a-interface",
            "hs-a",
            "--host-b-interface",
            "hs-b",
            "--host-a-route",
            "10.44.0.1/32",
            "--host-b-route",
            "10.44.0.2/32",
            "--two-host-mtu",
            "1420",
            "--force",
        ])
        .expect("cli");

        let Command::RelayCheck {
            config,
            relay_candidates,
            relay_candidates_file,
            require_relay_reservation,
            require_dcutr_success,
            timeout_seconds,
            max_validation_candidates,
            write_report,
            write_config,
            write_host_a_config,
            write_host_b_config,
            two_host_network,
            host_a_interface,
            host_b_interface,
            host_a_route,
            host_b_route,
            two_host_mtu,
            force,
        } = cli.command
        else {
            panic!("expected relay-check command");
        };

        assert_eq!(config, Some(PathBuf::from("base-config.json")));
        assert_eq!(relay_candidates.len(), 1);
        assert!(relay_candidates[0].contains("relay.example.net"));
        assert_eq!(
            relay_candidates_file,
            Some(PathBuf::from("relay-candidates.txt"))
        );
        assert!(!require_relay_reservation);
        assert!(require_dcutr_success);
        assert_eq!(timeout_seconds, 60);
        assert_eq!(max_validation_candidates, Some(3));
        assert_eq!(write_report, Some(PathBuf::from("relay-report.json")));
        assert_eq!(write_config, Some(PathBuf::from("relay-config.json")));
        assert_eq!(write_host_a_config, Some(PathBuf::from("host-a.json")));
        assert_eq!(write_host_b_config, Some(PathBuf::from("host-b.json")));
        assert_eq!(two_host_network, "public-lab");
        assert_eq!(host_a_interface, "hs-a");
        assert_eq!(host_b_interface, "hs-b");
        assert_eq!(host_a_route, "10.44.0.1/32");
        assert_eq!(host_b_route, "10.44.0.2/32");
        assert_eq!(two_host_mtu, 1420);
        assert!(force);
    }

    #[test]
    fn cli_parses_relay_check_reservation_mode() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "relay-check",
            "--relay-candidate",
            "/dns4/relay.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
            "--require-relay-reservation",
        ])
        .expect("cli");

        let Command::RelayCheck {
            require_relay_reservation,
            require_dcutr_success,
            ..
        } = cli.command
        else {
            panic!("expected relay-check command");
        };

        assert!(require_relay_reservation);
        assert!(!require_dcutr_success);
    }

    #[test]
    fn cli_parses_relay_dcutr_listen_command() {
        let peer = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let relay = format!("/dns4/relay.example.net/tcp/4001/p2p/{peer}");
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "relay-dcutr-listen",
            "--relay-candidate",
            &relay,
            "--write-descriptor",
            "listener.json",
            "--write-report",
            "listen-report.json",
            "--reservation-timeout-seconds",
            "12",
            "--serve-seconds",
            "120",
            "--force",
        ])
        .expect("cli");

        let Command::RelayDcutrListen {
            relay_candidate,
            write_descriptor,
            write_report,
            reservation_timeout_seconds,
            serve_seconds,
            force,
        } = cli.command
        else {
            panic!("expected relay-dcutr-listen command");
        };

        assert_eq!(relay_candidate, relay);
        assert_eq!(write_descriptor, PathBuf::from("listener.json"));
        assert_eq!(write_report, Some(PathBuf::from("listen-report.json")));
        assert_eq!(reservation_timeout_seconds, 12);
        assert_eq!(serve_seconds, 120);
        assert!(force);
    }

    #[test]
    fn cli_parses_relay_dcutr_dial_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "relay-dcutr-dial",
            "--descriptor",
            "listener.json",
            "--timeout-seconds",
            "30",
            "--write-report",
            "dial-report.json",
            "--force",
        ])
        .expect("cli");

        let Command::RelayDcutrDial {
            descriptor,
            timeout_seconds,
            write_report,
            force,
        } = cli.command
        else {
            panic!("expected relay-dcutr-dial command");
        };

        assert_eq!(descriptor, PathBuf::from("listener.json"));
        assert_eq!(timeout_seconds, 30);
        assert_eq!(write_report, Some(PathBuf::from("dial-report.json")));
        assert!(force);
    }

    #[test]
    fn cli_parses_relay_scan_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "relay-scan",
            "--bootstrap-peer",
            "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN=/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
            "--ipfs-bootstrap-peers",
            "--timeout-seconds",
            "60",
            "--max-candidates",
            "4",
            "--check-candidates",
            "--require-dcutr-success",
            "--candidate-timeout-seconds",
            "15",
            "--max-validation-candidates",
            "3",
            "--write-candidates",
            "relay-candidates.txt",
            "--write-report",
            "relay-scan-report.json",
            "--write-config",
            "relay-scan-config.json",
            "--force",
        ])
        .expect("cli");

        let Command::RelayScan {
            config,
            bootstrap_peers,
            ipfs_bootstrap_peers,
            timeout_seconds,
            max_candidates,
            check_candidates,
            require_relay_reservation,
            require_dcutr_success,
            candidate_timeout_seconds,
            max_validation_candidates,
            write_candidates,
            write_report,
            write_config,
            force,
        } = cli.command
        else {
            panic!("expected relay-scan command");
        };

        assert_eq!(config, None);
        assert_eq!(bootstrap_peers.len(), 1);
        assert!(ipfs_bootstrap_peers);
        assert_eq!(timeout_seconds, 60);
        assert_eq!(max_candidates, 4);
        assert!(check_candidates);
        assert!(!require_relay_reservation);
        assert!(require_dcutr_success);
        assert_eq!(candidate_timeout_seconds, 15);
        assert_eq!(max_validation_candidates, Some(3));
        assert_eq!(
            write_candidates,
            Some(PathBuf::from("relay-candidates.txt"))
        );
        assert_eq!(write_report, Some(PathBuf::from("relay-scan-report.json")));
        assert_eq!(write_config, Some(PathBuf::from("relay-scan-config.json")));
        assert!(force);
    }

    #[test]
    fn cli_parses_relay_scan_reservation_mode() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "relay-scan",
            "--ipfs-bootstrap-peers",
            "--check-candidates",
            "--require-relay-reservation",
        ])
        .expect("cli");

        let Command::RelayScan {
            check_candidates,
            require_relay_reservation,
            require_dcutr_success,
            ..
        } = cli.command
        else {
            panic!("expected relay-scan command");
        };

        assert!(check_candidates);
        assert!(require_relay_reservation);
        assert!(!require_dcutr_success);
    }

    #[test]
    fn relay_dcutr_writes_and_reads_listener_descriptor() {
        let descriptor = public_dcutr_listener_descriptor();
        let output = std::env::temp_dir().join(format!(
            "p2p-vpn-dcutr-listener-{}-{}.json",
            std::process::id(),
            "descriptor"
        ));
        let _ = fs::remove_file(&output);

        write_public_dcutr_listener_descriptor(&descriptor, &output, false)
            .expect("write descriptor");
        let read = read_public_dcutr_listener_descriptor(&output).expect("read descriptor");

        assert_eq!(read, descriptor);
        assert!(
            write_public_dcutr_listener_descriptor(&descriptor, &output, false)
                .expect_err("overwrite should require force")
                .contains("pass --force")
        );
        fs::remove_file(&output).expect("remove descriptor");
    }

    #[test]
    fn relay_dcutr_writes_machine_readable_listen_report() {
        let descriptor = public_dcutr_listener_descriptor();
        let output = std::env::temp_dir().join(format!(
            "p2p-vpn-dcutr-listen-report-{}-{}.json",
            std::process::id(),
            "report"
        ));
        let _ = fs::remove_file(&output);
        let args = RelayDcutrListenArgs {
            relay_candidate: descriptor.relay_candidate.clone(),
            write_descriptor: PathBuf::from("listener.json"),
            write_report: Some(output.clone()),
            reservation_timeout_seconds: 30,
            serve_seconds: 120,
            force: false,
        };
        let evidence = PublicDcutrReservationEvidence {
            connected_to_relay: true,
            reservation_accepted: true,
            relayed_listen_address_observed: true,
            listen_addresses: descriptor.listen_addresses.clone(),
            last_error: None,
        };

        write_public_dcutr_listen_report(&args, Some(&descriptor), Some(&evidence), None, &output)
            .expect("write report");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&output).expect("report file")).expect("json report");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["mode"], "public_dcutr_listen");
        assert_eq!(value["succeeded"], true);
        assert_eq!(value["relay_candidate"], descriptor.relay_candidate);
        assert_eq!(value["relay_peer"], descriptor.relay_peer);
        assert_eq!(value["listener_peer"], descriptor.listener_peer);
        assert_eq!(value["reservation_timeout_seconds"], 30);
        assert_eq!(value["serve_seconds"], 120);
        assert_eq!(value["connected_to_relay"], true);
        assert_eq!(value["reservation_accepted"], true);
        assert_eq!(value["relayed_listen_address_observed"], true);
        assert_eq!(value["relayed_address"], descriptor.relayed_address);
        assert_eq!(value["listen_addresses"][0], descriptor.listen_addresses[0]);
        assert_eq!(value["created_unix_seconds"], 1_786_230_000);
        assert_eq!(value["error"], serde_json::Value::Null);
        assert!(
            write_public_dcutr_listen_report(
                &args,
                Some(&descriptor),
                Some(&evidence),
                None,
                &output,
            )
            .expect_err("overwrite should require force")
            .contains("pass --force")
        );
        fs::remove_file(&output).expect("remove report");
    }

    #[test]
    fn relay_dcutr_writes_machine_readable_dial_report() {
        let descriptor = public_dcutr_listener_descriptor();
        let report = failed_public_dcutr_bootstrap_report();
        let output = std::env::temp_dir().join(format!(
            "p2p-vpn-dcutr-dial-report-{}-{}.json",
            std::process::id(),
            "report"
        ));
        let _ = fs::remove_file(&output);
        let args = RelayDcutrDialArgs {
            descriptor: PathBuf::from("listener.json"),
            timeout_seconds: 30,
            write_report: Some(output.clone()),
            force: false,
        };

        write_public_dcutr_dial_report(&args, &descriptor, &report, &output).expect("write report");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&output).expect("report file")).expect("json report");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["mode"], "public_dcutr_dial");
        assert_eq!(value["succeeded"], false);
        assert_eq!(value["timeout_seconds"], 30);
        assert_eq!(
            value["descriptor"]["relay_candidate"],
            descriptor.relay_candidate
        );
        assert_eq!(
            value["descriptor"]["listener_peer"],
            descriptor.listener_peer
        );
        assert_failed_public_dcutr_bootstrap_json(&value["bootstrap"]);
        assert!(
            write_public_dcutr_dial_report(&args, &descriptor, &report, &output)
                .expect_err("overwrite should require force")
                .contains("pass --force")
        );
        fs::remove_file(&output).expect("remove report");
    }

    #[test]
    fn relay_check_candidate_multiaddrs_prioritize_quic_with_peer_round_robin() {
        let peer_a = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let peer_b = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        let peer_c = "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb";
        let addresses = [
            format!("/dns4/relay-a.example.net/tcp/4001/p2p/{peer_a}"),
            format!("/dns4/relay-a.example.net/udp/4001/quic-v1/p2p/{peer_a}"),
            format!("/dns4/relay-b.example.net/tcp/4001/p2p/{peer_b}"),
            format!("/dns4/relay-c.example.net/tcp/4001/p2p/{peer_c}"),
            format!("/dns4/relay-b.example.net/udp/4001/quic-v1/p2p/{peer_b}"),
        ];
        let raw = addresses.join("\n");

        let candidates =
            relay_check_candidate_multiaddrs(&raw, None).expect("relay-check candidates");
        let ordered = candidates
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert_eq!(
            ordered,
            vec![
                addresses[1].clone(),
                addresses[4].clone(),
                addresses[3].clone(),
                addresses[0].clone(),
                addresses[2].clone(),
            ]
        );
    }

    #[test]
    fn relay_check_candidate_multiaddrs_accept_large_file_when_validation_is_capped() {
        let peer = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let raw = (0..32)
            .map(|port| format!("/dns4/relay.example.net/tcp/{}/p2p/{peer}", 4001 + port))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            relay_check_candidate_multiaddrs(&raw, None)
                .expect_err("uncapped input should keep the default safety limit")
                .contains("maximum is 8")
        );
        let candidates =
            relay_check_candidate_multiaddrs(&raw, Some(8)).expect("capped relay candidates");

        assert_eq!(candidates.len(), 32);
    }

    #[test]
    fn relay_check_candidate_multiaddrs_reject_missing_peer_id() {
        let error = relay_check_candidate_multiaddrs("/dns4/relay.example.net/tcp/4001", None)
            .expect_err("missing peer id should fail");

        assert!(error.contains("missing /p2p/RELAY"));
    }

    #[test]
    fn relay_check_candidate_input_reads_scan_candidate_file() {
        let peer_a = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let peer_b = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        let inline = format!("/dns4/relay-a.example.net/tcp/4001/p2p/{peer_a}");
        let file_candidate = format!("/dns4/relay-b.example.net/udp/4001/quic-v1/p2p/{peer_b}");
        let output = std::env::temp_dir().join(format!(
            "p2p-vpn-relay-check-candidates-{}-{}.txt",
            std::process::id(),
            "input"
        ));
        let _ = fs::remove_file(&output);
        fs::write(&output, format!("{file_candidate}\n")).expect("write candidates");
        let args = RelayCheckArgs {
            relay_candidates: vec![inline.clone()],
            relay_candidates_file: Some(output.clone()),
            ..relay_check_args_for_test()
        };

        let raw = relay_check_candidate_input(&args).expect("candidate input");
        let candidates =
            relay_check_candidate_multiaddrs(&raw, None).expect("relay-check candidates");
        fs::remove_file(&output).expect("remove candidates");

        assert_eq!(
            candidates
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![inline, file_candidate]
        );
    }

    #[test]
    fn relay_check_writes_machine_readable_probe_report() {
        let peer = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let skipped_peer = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        let candidate = format!("/dns4/relay.example.net/tcp/4001/p2p/{peer}");
        let skipped = SkippedRelayCandidate {
            address: format!("/ip4/203.0.113.10/tcp/4001/p2p/{skipped_peer}"),
            reason: "ipv4_unreachable",
        };
        let report = p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport {
            mode: PublicRelayProbeMode::DcutrSuccess,
            candidates: vec![
                p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateReport {
                    address: candidate.clone(),
                    succeeded: false,
                    failure_stage:
                        p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateFailureStage::DcutrSuccess,
                    error: Some("dcutr success check did not meet success threshold".to_owned()),
                    bootstrap: Some(failed_public_dcutr_bootstrap_report()),
                    elapsed_millis: 45_000,
                },
            ],
        };
        let output = std::env::temp_dir().join(format!(
            "p2p-vpn-relay-check-report-{}-{}.json",
            std::process::id(),
            "probe"
        ));
        let _ = fs::remove_file(&output);
        let args = RelayCheckArgs {
            relay_candidates: vec![candidate.clone()],
            mode: PublicRelayProbeMode::DcutrSuccess,
            max_validation_candidates: Some(1),
            write_report: Some(output.clone()),
            ..relay_check_args_for_test()
        };

        write_public_relay_probe_report(
            &args,
            std::slice::from_ref(&candidate),
            std::slice::from_ref(&skipped),
            &report,
            &output,
        )
        .expect("write report");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&output).expect("report file")).expect("json report");

        assert_eq!(value["schema_version"], 5);
        assert_eq!(value["mode"], "dcutr_success");
        assert_eq!(value["succeeded"], false);
        assert_eq!(value["timeout_seconds"], 45);
        assert_eq!(value["max_validation_candidates"], 1);
        assert_eq!(value["host_reachable_candidates"][0], candidate);
        assert_eq!(value["skipped_candidates"][0]["reason"], "ipv4_unreachable");
        assert_eq!(value["candidates"][0]["failure_stage"], "dcutr_success");
        assert_eq!(
            value["candidates"][0]["diagnosis"],
            "dcutr_no_hole_punch_success"
        );
        assert_eq!(value["candidates"][0]["elapsed_millis"], 45_000);
        assert_eq!(
            value["candidates"][0]["error"],
            "dcutr success check did not meet success threshold"
        );
        assert_failed_public_dcutr_bootstrap_json(&value["candidates"][0]["bootstrap"]);
        assert!(
            write_public_relay_probe_report(&args, &[candidate], &[skipped], &report, &output)
                .expect_err("overwrite should require force")
                .contains("pass --force")
        );
        fs::remove_file(&output).expect("remove report");
    }

    #[test]
    fn relay_scan_writes_machine_readable_scan_report() {
        let peer = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let candidate = format!("/dns4/relay.example.net/tcp/4001/p2p/{peer}");
        let mut report = relay_scan_report_with_candidates(&[&candidate]);
        report.discovered_routing_peers = 4;
        report.dialed_routing_peers = 2;
        report.closest_peer_lookup_started = true;
        report.closest_peer_lookup_finished = true;
        report.closest_peer_results = 3;
        report.peer_results[0].dial_failures = 1;
        report.peer_results[0].last_error = Some("dial failed: no route".to_owned());
        let output = std::env::temp_dir().join(format!(
            "p2p-vpn-relay-scan-report-{}-{}.json",
            std::process::id(),
            "scan"
        ));
        let _ = fs::remove_file(&output);
        let args = RelayScanArgs {
            config_path: None,
            bootstrap_peers: Vec::new(),
            ipfs_bootstrap_peers: true,
            timeout_seconds: 60,
            max_candidates: 4,
            check_candidates: true,
            require_relay_reservation: false,
            require_dcutr_success: true,
            candidate_timeout_seconds: 15,
            max_validation_candidates: Some(1),
            write_candidates: None,
            write_report: Some(output.clone()),
            write_config: None,
            force: false,
        };

        write_public_relay_scan_report(&args, &report, &output).expect("write report");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&output).expect("report file")).expect("json report");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["succeeded"], true);
        assert_eq!(value["timeout_seconds"], 60);
        assert_eq!(value["max_candidates"], 4);
        assert_eq!(value["check_candidates"], true);
        assert_eq!(value["require_relay_reservation"], false);
        assert_eq!(value["require_dcutr_success"], true);
        assert_eq!(value["candidate_timeout_seconds"], 15);
        assert_eq!(value["max_validation_candidates"], 1);
        assert_eq!(value["discovered_routing_peers"], 4);
        assert_eq!(value["dialed_routing_peers"], 2);
        assert_eq!(value["closest_peer_lookup_started"], true);
        assert_eq!(value["closest_peer_results"], 3);
        assert_eq!(value["candidates"][0]["peer_id"], peer);
        assert_eq!(value["candidates"][0]["address"], candidate);
        assert_eq!(value["peer_results"][0]["dial_failures"], 1);
        assert_eq!(
            value["peer_results"][0]["last_error"],
            "dial failed: no route"
        );
        assert!(
            write_public_relay_scan_report(&args, &report, &output)
                .expect_err("overwrite should require force")
                .contains("pass --force")
        );
        fs::remove_file(&output).expect("remove report");
    }

    fn failed_public_dcutr_bootstrap_report()
    -> p2p_vpn::runtime::bootstrap_check::BootstrapCheckReport {
        let bootstrap_peer = NodeIdentity::generate_ed25519()
            .expect("bootstrap peer")
            .peer_id
            .parse()
            .expect("bootstrap peer id");
        let relay_peer = NodeIdentity::generate_ed25519()
            .expect("relay peer")
            .peer_id
            .parse()
            .expect("relay peer id");
        let relayed_peer = NodeIdentity::generate_ed25519()
            .expect("relayed peer")
            .peer_id
            .parse()
            .expect("relayed peer id");
        p2p_vpn::runtime::bootstrap_check::BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: true,
                dcutr_ready: true,
                dcutr_success: true,
                relayed_peer_circuits: true,
                membership_records: false,
            },
            kademlia_protocol: PUBLIC_IPFS_KADEMLIA_PROTOCOL.to_owned(),
            ipfs_compatible: true,
            dcutr: p2p_vpn::runtime::bootstrap_check::BootstrapDcutrCheck {
                enabled: true,
                ready: true,
                successes: 0,
                direct_connections: 0,
                failures: 1,
                last_error: Some("NoDirectConnection".to_owned()),
            },
            configured_bootstrap_peers: 4,
            connected_bootstrap_peers: 3,
            dial_failures: 1,
            configured_relay_reservations: 1,
            accepted_relay_reservations: 1,
            relayed_listen_addresses: 1,
            configured_relayed_peer_circuits: 1,
            connected_relayed_peer_circuits: 1,
            relayed_connection_addresses: vec![
                "12D3KooWRelay /ip4/203.0.113.10/tcp/4001/p2p/12D3KooWRelay/p2p-circuit".to_owned(),
            ],
            direct_connection_addresses: Vec::new(),
            autonat_probe_servers_registered: 1,
            autonat_status: p2p_vpn::runtime::bootstrap_check::BootstrapAutoNatStatus::Private,
            kademlia: p2p_vpn::runtime::bootstrap_check::BootstrapKademliaCheck {
                bootstrap_started: true,
                rendezvous_lookup_started: true,
                rendezvous_advertise_started: true,
            },
            membership_records:
                p2p_vpn::runtime::bootstrap_check::BootstrapMembershipRecordDhtCheck::default(),
            peer_results: vec![p2p_vpn::runtime::bootstrap_check::BootstrapPeerCheck {
                peer_id: bootstrap_peer,
                address: "/dns4/bootstrap.example.net/tcp/4001".to_owned(),
                connected: false,
                dial_failures: 1,
                last_error: Some("TransportError".to_owned()),
            }],
            relay_results: vec![p2p_vpn::runtime::bootstrap_check::RelayReservationCheck {
                relay_peer_id: relay_peer,
                address: "/ip4/203.0.113.10/tcp/4001".to_owned(),
                accepted: true,
                relayed_listen_address: true,
            }],
            relayed_peer_results: vec![
                p2p_vpn::runtime::bootstrap_check::RelayedPeerCircuitCheck {
                    peer_id: relayed_peer,
                    address: "/ip4/203.0.113.10/tcp/4001/p2p/relay/p2p-circuit/p2p/listener"
                        .to_owned(),
                    connected: true,
                    outbound_circuit: true,
                    dial_failures: 0,
                    last_error: None,
                },
            ],
        }
    }

    fn public_dcutr_listener_descriptor() -> PublicDcutrListenerDescriptor {
        let relay_peer = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let listener_peer = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        PublicDcutrListenerDescriptor {
            schema_version: PublicDcutrListenerDescriptor::SCHEMA_VERSION,
            relay_candidate: format!("/dns4/relay.example.net/tcp/4001/p2p/{relay_peer}"),
            relay_peer: relay_peer.to_owned(),
            listener_peer: listener_peer.to_owned(),
            relayed_address: format!(
                "/dns4/relay.example.net/tcp/4001/p2p/{relay_peer}/p2p-circuit/p2p/{listener_peer}"
            ),
            listen_addresses: vec!["/ip4/0.0.0.0/tcp/0".to_owned()],
            created_unix_seconds: 1_786_230_000,
        }
    }

    fn assert_failed_public_dcutr_bootstrap_json(bootstrap: &serde_json::Value) {
        assert_eq!(bootstrap["succeeded"], false);
        assert_eq!(bootstrap["threshold"], "any");
        assert_eq!(bootstrap["requirements"]["relay_reservations"], true);
        assert_eq!(bootstrap["requirements"]["autonat_status"], true);
        assert_eq!(bootstrap["requirements"]["dcutr_ready"], true);
        assert_eq!(bootstrap["requirements"]["dcutr_success"], true);
        assert_eq!(bootstrap["requirements"]["relayed_peer_circuits"], true);
        assert_eq!(bootstrap["requirements"]["membership_records"], false);
        assert_eq!(
            bootstrap["kademlia_protocol"],
            PUBLIC_IPFS_KADEMLIA_PROTOCOL
        );
        assert_eq!(bootstrap["ipfs_compatible"], true);
        assert_eq!(bootstrap["dcutr"]["enabled"], true);
        assert_eq!(bootstrap["dcutr"]["ready"], true);
        assert_eq!(bootstrap["dcutr"]["successes"], 0);
        assert_eq!(bootstrap["dcutr"]["direct_connections"], 0);
        assert_eq!(bootstrap["dcutr"]["failures"], 1);
        assert_eq!(bootstrap["dcutr"]["last_error"], "NoDirectConnection");
        assert_eq!(bootstrap["configured_bootstrap_peers"], 4);
        assert_eq!(bootstrap["connected_bootstrap_peers"], 3);
        assert_eq!(bootstrap["dial_failures"], 1);
        assert_eq!(bootstrap["configured_relay_reservations"], 1);
        assert_eq!(bootstrap["accepted_relay_reservations"], 1);
        assert_eq!(bootstrap["relayed_listen_addresses"], 1);
        assert_eq!(bootstrap["configured_relayed_peer_circuits"], 1);
        assert_eq!(bootstrap["connected_relayed_peer_circuits"], 1);
        assert_eq!(
            bootstrap["relayed_connection_addresses"][0],
            "12D3KooWRelay /ip4/203.0.113.10/tcp/4001/p2p/12D3KooWRelay/p2p-circuit"
        );
        assert_eq!(
            bootstrap["direct_connection_addresses"]
                .as_array()
                .expect("direct connection addresses")
                .len(),
            0
        );
        assert_eq!(bootstrap["autonat_probe_servers_registered"], 1);
        assert_eq!(bootstrap["autonat_status"], "private");
        assert_eq!(bootstrap["kademlia"]["bootstrap_started"], true);
        assert_eq!(bootstrap["kademlia"]["rendezvous_lookup_started"], true);
        assert_eq!(bootstrap["kademlia"]["rendezvous_advertise_started"], true);
        assert_eq!(bootstrap["membership_records"]["configured_records"], 0);
        assert_eq!(bootstrap["membership_records"]["publish_started"], false);
        assert_eq!(bootstrap["membership_records"]["publish_succeeded"], false);
        assert_eq!(bootstrap["membership_records"]["publish_failures"], 0);
        assert_eq!(bootstrap["membership_records"]["lookup_started"], false);
        assert_eq!(bootstrap["membership_records"]["found_records"], 0);
        assert_eq!(bootstrap["membership_records"]["verified_records"], 0);
        assert_eq!(bootstrap["membership_records"]["accepted_records"], 0);
        assert_eq!(bootstrap["membership_records"]["invalid_records"], 0);
        assert_eq!(
            bootstrap["membership_records"]["last_error"],
            serde_json::Value::Null
        );
        assert!(bootstrap["peer_results"][0]["peer_id"].is_string());
        assert_eq!(
            bootstrap["peer_results"][0]["address"],
            "/dns4/bootstrap.example.net/tcp/4001"
        );
        assert_eq!(bootstrap["peer_results"][0]["connected"], false);
        assert_eq!(bootstrap["peer_results"][0]["dial_failures"], 1);
        assert_eq!(bootstrap["peer_results"][0]["last_error"], "TransportError");
        assert!(bootstrap["relay_results"][0]["relay_peer_id"].is_string());
        assert_eq!(
            bootstrap["relay_results"][0]["address"],
            "/ip4/203.0.113.10/tcp/4001"
        );
        assert_eq!(bootstrap["relay_results"][0]["accepted"], true);
        assert_eq!(
            bootstrap["relay_results"][0]["relayed_listen_address"],
            true
        );
        assert!(bootstrap["relayed_peer_results"][0]["peer_id"].is_string());
        assert_eq!(
            bootstrap["relayed_peer_results"][0]["address"],
            "/ip4/203.0.113.10/tcp/4001/p2p/relay/p2p-circuit/p2p/listener"
        );
        assert_eq!(bootstrap["relayed_peer_results"][0]["connected"], true);
        assert_eq!(
            bootstrap["relayed_peer_results"][0]["outbound_circuit"],
            true
        );
        assert_eq!(bootstrap["relayed_peer_results"][0]["dial_failures"], 0);
        assert_eq!(
            bootstrap["relayed_peer_results"][0]["last_error"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn relay_check_candidates_filter_host_unreachable_ipv4() {
        let peer_a = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let peer_b = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        let raw = [
            format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer_a}"),
            format!("/ip6/2001:db8::1/tcp/4001/p2p/{peer_b}"),
        ]
        .join("\n");
        let candidates =
            relay_check_candidate_multiaddrs(&raw, None).expect("relay-check candidates");

        let (kept, skipped) = filter_relay_validation_candidates(
            candidates,
            RelayCandidateReachability {
                ipv4: false,
                ipv6: true,
            },
        );

        assert_eq!(
            kept.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec![format!("/ip6/2001:db8::1/tcp/4001/p2p/{peer_b}")]
        );
        assert_eq!(
            skipped
                .iter()
                .map(|candidate| (&candidate.address, candidate.reason))
                .collect::<Vec<_>>(),
            vec![(
                &format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer_a}"),
                "ipv4_unreachable"
            )]
        );
    }

    #[test]
    fn relay_check_validation_limit_rejects_zero() {
        let args = RelayCheckArgs {
            relay_candidates: vec![
                "/dns4/relay.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"
                    .to_owned(),
            ],
            max_validation_candidates: Some(0),
            ..relay_check_args_for_test()
        };

        assert_eq!(
            validate_relay_check_args(&args).expect_err("validation should fail"),
            "--max-validation-candidates must be greater than zero"
        );
    }

    #[test]
    fn relay_check_reservation_mode_rejects_two_host_config_generation() {
        let args = RelayCheckArgs {
            relay_candidates: vec![
                "/dns4/relay.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"
                    .to_owned(),
            ],
            mode: PublicRelayProbeMode::RelayReservation,
            write_host_a_config: Some(PathBuf::from("host-a.json")),
            write_host_b_config: Some(PathBuf::from("host-b.json")),
            ..relay_check_args_for_test()
        };

        assert_eq!(
            validate_relay_check_args(&args).expect_err("validation should fail"),
            "--write-host-a-config and --write-host-b-config require relayed peer circuit validation"
        );
    }

    #[test]
    fn relay_check_requires_candidate_or_candidate_file() {
        let args = RelayCheckArgs {
            ..relay_check_args_for_test()
        };

        assert_eq!(
            validate_relay_check_args(&args).expect_err("validation should fail"),
            "relay-check needs at least one --relay-candidate or --relay-candidates-file"
        );
    }

    #[test]
    fn relay_scan_write_config_requires_candidate_validation() {
        let mut args = RelayScanArgs {
            config_path: None,
            bootstrap_peers: Vec::new(),
            ipfs_bootstrap_peers: true,
            timeout_seconds: 30,
            max_candidates: 8,
            check_candidates: false,
            require_relay_reservation: false,
            require_dcutr_success: false,
            candidate_timeout_seconds: 45,
            max_validation_candidates: None,
            write_candidates: None,
            write_report: None,
            write_config: Some(PathBuf::from("relay-scan-config.json")),
            force: false,
        };

        assert_eq!(
            validate_relay_scan_args(&args).expect_err("validation should fail"),
            "--write-config requires --check-candidates"
        );

        args.check_candidates = true;
        validate_relay_scan_args(&args).expect("validation should pass");

        args.check_candidates = false;
        args.write_config = None;
        args.require_relay_reservation = true;
        assert_eq!(
            validate_relay_scan_args(&args).expect_err("validation should fail"),
            "--require-relay-reservation requires --check-candidates"
        );

        args.check_candidates = true;
        args.require_dcutr_success = true;
        assert_eq!(
            validate_relay_scan_args(&args).expect_err("validation should fail"),
            "--require-relay-reservation and --require-dcutr-success cannot be used together"
        );
    }

    #[test]
    fn relay_scan_validation_limit_rejects_zero() {
        let args = RelayScanArgs {
            config_path: None,
            bootstrap_peers: Vec::new(),
            ipfs_bootstrap_peers: true,
            timeout_seconds: 30,
            max_candidates: 8,
            check_candidates: true,
            require_relay_reservation: false,
            require_dcutr_success: true,
            candidate_timeout_seconds: 45,
            max_validation_candidates: Some(0),
            write_candidates: None,
            write_report: None,
            write_config: None,
            force: false,
        };

        assert_eq!(
            validate_relay_scan_args(&args).expect_err("validation should fail"),
            "--max-validation-candidates must be greater than zero"
        );
    }

    #[test]
    fn relay_scan_config_can_use_ipfs_bootstrap_defaults_without_local_config() {
        let config =
            relay_scan_config(None, Vec::new(), true).expect("relay scan config from ipfs");

        assert_eq!(config.network.name, "relay-scan");
        assert_eq!(
            config.network.bootstrap_peers.len(),
            PUBLIC_IPFS_BOOTSTRAP_PEERS.len()
        );
        assert!(!config.network.discovery.mdns);
        assert!(config.network.discovery.kademlia);
        assert_eq!(
            config.network.discovery.kademlia_protocol,
            PUBLIC_IPFS_KADEMLIA_PROTOCOL
        );
        assert!(!config.network.discovery.kademlia_provider_advertisement);
    }

    #[test]
    fn relay_scan_candidate_multiaddrs_parse_report_candidates() {
        let report = relay_scan_report_with_candidates(&[
            "/dns4/relay.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
        ]);

        let candidates = relay_scan_candidate_multiaddrs(&report).expect("candidates");

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].to_string(),
            "/dns4/relay.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"
        );
    }

    #[test]
    fn relay_scan_candidate_multiaddrs_accept_full_scan_limit() {
        let addresses = [
            "/dns4/relay-0.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
            "/dns4/relay-1.example.net/tcp/4001/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
            "/dns4/relay-2.example.net/tcp/4001/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
            "/dns4/relay-3.example.net/tcp/4001/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
            "/dns4/relay-4.example.net/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
            "/dns4/relay-5.example.net/tcp/4001/p2p/QmNLeiTeX4gikNaGnwmN8vV1E6AbY3JdC9GrCpVJKiKfVn",
            "/dns4/relay-6.example.net/tcp/4001/p2p/QmRz5F2Yk5YzgT1cJNtSL9vGvDeX9xC4UvK8ZdtXqT6NhP",
            "/dns4/relay-7.example.net/tcp/4001/p2p/QmSoLueR4xBeUbY9WZ9xGUUxunbKWcrNFTDAadQJmocnWm",
            "/dns4/relay-8.example.net/tcp/4001/p2p/QmSoLer265NRgSp2LA3dPaeykiS1J6DifTC88f5uVQKNAd",
            "/dns4/relay-9.example.net/tcp/4001/p2p/QmSoLer265NRgSp2LA3dPaeykiS1J6DifTC88f5uVQKNAd",
            "/dns4/relay-10.example.net/tcp/4001/p2p/QmSoLueR4xBeUbY9WZ9xGUUxunbKWcrNFTDAadQJmocnWm",
            "/dns4/relay-11.example.net/tcp/4001/p2p/QmRz5F2Yk5YzgT1cJNtSL9vGvDeX9xC4UvK8ZdtXqT6NhP",
            "/dns4/relay-12.example.net/tcp/4001/p2p/QmNLeiTeX4gikNaGnwmN8vV1E6AbY3JdC9GrCpVJKiKfVn",
            "/dns4/relay-13.example.net/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
            "/dns4/relay-14.example.net/tcp/4001/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
            "/dns4/relay-15.example.net/tcp/4001/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
        ];
        let report = relay_scan_report_with_candidates(&addresses);

        let candidates = relay_scan_candidate_multiaddrs(&report).expect("candidates");

        assert_eq!(candidates.len(), addresses.len());
    }

    #[test]
    fn relay_scan_candidate_multiaddrs_prioritize_quic_with_peer_round_robin() {
        let peer_a = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let peer_b = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        let peer_c = "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb";
        let addresses = [
            format!("/dns4/relay-a.example.net/tcp/4001/p2p/{peer_a}"),
            format!("/dns4/relay-a.example.net/udp/4001/quic-v1/p2p/{peer_a}"),
            format!("/dns4/relay-b.example.net/tcp/4001/p2p/{peer_b}"),
            format!("/dns4/relay-c.example.net/tcp/4001/p2p/{peer_c}"),
            format!("/dns4/relay-b.example.net/udp/4001/quic-v1/p2p/{peer_b}"),
        ];
        let refs = addresses.iter().map(String::as_str).collect::<Vec<_>>();
        let report = relay_scan_report_with_candidates(&refs);

        let candidates = relay_scan_candidate_multiaddrs(&report).expect("candidates");
        let ordered = candidates
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert_eq!(
            ordered,
            vec![
                addresses[1].clone(),
                addresses[4].clone(),
                addresses[3].clone(),
                addresses[0].clone(),
                addresses[2].clone(),
            ]
        );
    }

    #[test]
    fn public_relay_candidates_write_ordered_newline_file() {
        let peer_a = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let peer_b = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        let addresses = [
            format!("/dns4/relay-a.example.net/tcp/4001/p2p/{peer_a}"),
            format!("/dns4/relay-a.example.net/udp/4001/quic-v1/p2p/{peer_a}"),
            format!("/dns4/relay-b.example.net/tcp/4001/p2p/{peer_b}"),
        ];
        let refs = addresses.iter().map(String::as_str).collect::<Vec<_>>();
        let report = relay_scan_report_with_candidates(&refs);
        let candidates = relay_scan_candidate_multiaddrs(&report).expect("candidates");
        let output = std::env::temp_dir().join(format!(
            "p2p-vpn-relay-candidates-{}-{}.txt",
            std::process::id(),
            "ordered"
        ));
        let _ = fs::remove_file(&output);

        write_public_relay_candidates(&candidates, &output, false).expect("write candidates");
        let rendered = fs::read_to_string(&output).expect("read candidates");
        let overwrite_error = write_public_relay_candidates(&candidates, &output, false)
            .expect_err("existing file should require --force");
        fs::remove_file(&output).expect("remove candidates");

        assert_eq!(
            rendered,
            format!("{}\n{}\n{}\n", addresses[1], addresses[2], addresses[0])
        );
        assert!(overwrite_error.contains("pass --force"));
    }

    #[test]
    fn relay_candidate_validation_rank_prefers_quic_addresses() {
        assert_eq!(
            relay_candidate_validation_rank(
                &"/ip4/203.0.113.10/udp/4001/quic-v1/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"
                    .parse()
                    .expect("quic candidate")
            ),
            0
        );
        assert_eq!(
            relay_candidate_validation_rank(
                &"/ip4/203.0.113.10/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"
                    .parse()
                    .expect("tcp candidate")
            ),
            1
        );
    }

    #[test]
    fn relay_scan_validation_candidates_skip_ipv6_when_host_lacks_ipv6_route() {
        let peer_a = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let peer_b = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        let candidates = vec![
            format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer_a}")
                .parse()
                .expect("ipv4 candidate"),
            format!("/ip6/2001:db8::1/tcp/4001/p2p/{peer_a}")
                .parse()
                .expect("ipv6 candidate"),
            format!("/dns6/relay.example.net/tcp/4001/p2p/{peer_b}")
                .parse()
                .expect("dns6 candidate"),
            format!("/dns4/relay.example.net/tcp/4001/p2p/{peer_b}")
                .parse()
                .expect("dns4 candidate"),
        ];

        let (kept, skipped) = filter_relay_validation_candidates(
            candidates,
            RelayCandidateReachability {
                ipv4: true,
                ipv6: false,
            },
        );

        assert_eq!(
            kept.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec![
                format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer_a}"),
                format!("/dns4/relay.example.net/tcp/4001/p2p/{peer_b}"),
            ]
        );
        assert_eq!(
            skipped
                .iter()
                .map(|candidate| (&candidate.address, candidate.reason))
                .collect::<Vec<_>>(),
            vec![
                (
                    &format!("/ip6/2001:db8::1/tcp/4001/p2p/{peer_a}"),
                    "ipv6_unreachable"
                ),
                (
                    &format!("/dns6/relay.example.net/tcp/4001/p2p/{peer_b}"),
                    "ipv6_unreachable"
                ),
            ]
        );
    }

    #[test]
    fn relay_scan_validation_candidates_keep_ipv6_when_host_supports_ipv6_route() {
        let peer = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let candidates = vec![
            format!("/ip6/2001:db8::1/udp/4001/quic-v1/p2p/{peer}")
                .parse()
                .expect("ipv6 candidate"),
        ];

        let (kept, skipped) = filter_relay_validation_candidates(
            candidates,
            RelayCandidateReachability {
                ipv4: true,
                ipv6: true,
            },
        );

        assert_eq!(kept.len(), 1);
        assert!(skipped.is_empty());
    }

    #[test]
    fn relay_scan_validation_candidates_skip_ipv4_when_host_lacks_ipv4_route() {
        let peer_a = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let peer_b = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        let candidates = vec![
            format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer_a}")
                .parse()
                .expect("ipv4 candidate"),
            format!("/dns4/relay.example.net/tcp/4001/p2p/{peer_b}")
                .parse()
                .expect("dns4 candidate"),
            format!("/ip6/2001:db8::1/tcp/4001/p2p/{peer_a}")
                .parse()
                .expect("ipv6 candidate"),
        ];

        let (kept, skipped) = filter_relay_validation_candidates(
            candidates,
            RelayCandidateReachability {
                ipv4: false,
                ipv6: true,
            },
        );

        assert_eq!(
            kept.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec![format!("/ip6/2001:db8::1/tcp/4001/p2p/{peer_a}")]
        );
        assert_eq!(
            skipped
                .iter()
                .map(|candidate| (&candidate.address, candidate.reason))
                .collect::<Vec<_>>(),
            vec![
                (
                    &format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer_a}"),
                    "ipv4_unreachable"
                ),
                (
                    &format!("/dns4/relay.example.net/tcp/4001/p2p/{peer_b}"),
                    "ipv4_unreachable"
                ),
            ]
        );
    }

    #[test]
    fn relay_scan_validation_limit_truncates_host_reachable_candidates() {
        let peer = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let candidates = vec![
            format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer}")
                .parse()
                .expect("first candidate"),
            format!("/ip4/203.0.113.11/tcp/4001/p2p/{peer}")
                .parse()
                .expect("second candidate"),
            format!("/ip4/203.0.113.12/tcp/4001/p2p/{peer}")
                .parse()
                .expect("third candidate"),
        ];

        let (kept, limit) = limit_relay_validation_candidates(candidates, Some(2));

        assert_eq!(kept.len(), 2);
        assert_eq!(limit, Some(RelayValidationLimit { kept: 2, total: 3 }));
        assert_eq!(
            kept.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec![
                format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer}"),
                format!("/ip4/203.0.113.11/tcp/4001/p2p/{peer}"),
            ]
        );
    }

    #[test]
    fn relay_scan_validation_limit_keeps_quic_priority_order() {
        let peer_a = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let peer_b = "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa";
        let addresses = [
            format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer_a}"),
            format!("/ip4/203.0.113.10/udp/4001/quic-v1/p2p/{peer_a}"),
            format!("/ip4/203.0.113.20/tcp/4001/p2p/{peer_b}"),
        ];
        let refs = addresses.iter().map(String::as_str).collect::<Vec<_>>();
        let report = relay_scan_report_with_candidates(&refs);
        let candidates = relay_scan_candidate_multiaddrs(&report).expect("candidates");

        let (kept, limit) = limit_relay_validation_candidates(candidates, Some(2));

        assert_eq!(limit, Some(RelayValidationLimit { kept: 2, total: 3 }));
        assert_eq!(
            kept.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec![addresses[1].clone(), addresses[2].clone()]
        );
    }

    #[test]
    fn relay_scan_validation_limit_is_noop_when_cap_covers_candidates() {
        let peer = "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let candidates = vec![
            format!("/ip4/203.0.113.10/tcp/4001/p2p/{peer}")
                .parse()
                .expect("first candidate"),
            format!("/ip4/203.0.113.11/tcp/4001/p2p/{peer}")
                .parse()
                .expect("second candidate"),
        ];

        let (kept, limit) = limit_relay_validation_candidates(candidates, Some(2));

        assert_eq!(kept.len(), 2);
        assert_eq!(limit, None);
    }

    fn relay_scan_report_with_candidates(
        addresses: &[&str],
    ) -> p2p_vpn::runtime::bootstrap_check::PublicRelayScanReport {
        let candidates = addresses
            .iter()
            .map(|address| {
                let parsed = address.parse::<libp2p::Multiaddr>().expect("multiaddr");
                let peer_id = parsed
                    .iter()
                    .find_map(|protocol| match protocol {
                        libp2p::multiaddr::Protocol::P2p(peer_id) => Some(peer_id),
                        _ => None,
                    })
                    .expect("candidate peer id");
                p2p_vpn::runtime::bootstrap_check::PublicRelayScanCandidate {
                    peer_id,
                    address: (*address).to_owned(),
                }
            })
            .collect::<Vec<_>>();
        let peer_results = candidates
            .iter()
            .map(
                |candidate| p2p_vpn::runtime::bootstrap_check::PublicRelayScanPeer {
                    peer_id: candidate.peer_id,
                    address: candidate.address.clone(),
                    connected: true,
                    identified: true,
                    relay_hop: true,
                    candidate_addresses: 1,
                    dial_failures: 0,
                    last_error: None,
                },
            )
            .collect();

        p2p_vpn::runtime::bootstrap_check::PublicRelayScanReport {
            scanned_bootstrap_peers: addresses.len(),
            scanned_peers: addresses.len(),
            discovered_routing_peers: 0,
            dialed_routing_peers: 0,
            closest_peer_lookup_started: false,
            closest_peer_lookup_finished: false,
            closest_peer_results: 0,
            closest_peer_errors: 0,
            connected_bootstrap_peers: addresses.len(),
            identified_peers: addresses.len(),
            relay_capable_peers: addresses.len(),
            dial_failures: 0,
            candidates,
            peer_results,
        }
    }

    #[test]
    fn cli_parses_invite_export_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "invite-export",
            "--config",
            "node-a.json",
            "--output",
            "node-a.invite.json",
            "--expires-at-unix-seconds",
            "2000",
            "--membership-epoch",
            "4",
            "--previous-membership-tag",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "--force",
        ])
        .expect("cli");

        let Command::InviteExport {
            config,
            output,
            expires_at_unix_seconds,
            membership_epoch,
            previous_membership_tags,
            force,
        } = cli.command
        else {
            panic!("expected invite-export command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
        assert_eq!(output, PathBuf::from("node-a.invite.json"));
        assert_eq!(expires_at_unix_seconds, Some(2000));
        assert_eq!(membership_epoch, 4);
        assert_eq!(
            previous_membership_tags,
            vec!["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned()]
        );
        assert!(force);
    }

    #[test]
    fn cli_parses_invite_import_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "invite-import",
            "--invite",
            "node-a.invite.json",
            "--output",
            "node-b.json",
            "--interface",
            "hs1",
            "--mtu",
            "1400",
            "--local-route",
            "10.42.0.0/24,100",
            "--peer-name",
            "node-a",
            "--force",
        ])
        .expect("cli");

        let Command::InviteImport {
            invite,
            output,
            interface,
            mtu,
            local_routes,
            peer_name,
            force,
            ..
        } = cli.command
        else {
            panic!("expected invite-import command");
        };

        assert_eq!(invite, PathBuf::from("node-a.invite.json"));
        assert_eq!(output, PathBuf::from("node-b.json"));
        assert_eq!(interface, "hs1");
        assert_eq!(mtu, 1400);
        assert_eq!(local_routes.len(), 1);
        assert_eq!(local_routes[0].route.prefix, "10.42.0.0/24");
        assert_eq!(peer_name.as_deref(), Some("node-a"));
        assert!(force);
    }

    #[test]
    fn cli_parses_live_pairing_commands() {
        let open = Cli::try_parse_from([
            "p2p-vpn",
            "pair",
            "open",
            "--instance",
            "runner-mesh",
            "--expires-in-seconds",
            "120",
            "--format",
            "json",
        ])
        .expect("pair open CLI");
        let Command::Pair {
            command:
                PairCommand::Open {
                    target,
                    expires_in_seconds,
                    format,
                },
        } = open.command
        else {
            panic!("expected live pair open command");
        };
        assert_eq!(target.instance.as_deref(), Some("runner-mesh"));
        assert_eq!(target.socket, None);
        assert_eq!(target.rpc_timeout_seconds, 5);
        assert_eq!(expires_in_seconds, 120);
        assert_eq!(format, PairOutputFormat::Json);
        assert_eq!(
            pair_daemon_target(&target).expect("NixOS target").0,
            PathBuf::from("/run/p2p-vpn-runner-mesh/control.sock")
        );

        let join = Cli::try_parse_from([
            "p2p-vpn",
            "pair",
            "join",
            "ABCD-EFGH-JKLM-NPQR",
            "--socket",
            "/tmp/pair.sock",
            "--vpn-ip",
            "10.42.0.2",
            "--route",
            "10.60.0.0/24,20",
            "--no-wait",
        ])
        .expect("pair join CLI");
        let Command::Pair {
            command:
                PairCommand::Join {
                    code,
                    target,
                    requested_vpn_ip,
                    requested_routes,
                    no_wait,
                    ..
                },
        } = join.command
        else {
            panic!("expected live pair join command");
        };
        assert_eq!(code, "ABCD-EFGH-JKLM-NPQR");
        assert_eq!(target.socket, Some(PathBuf::from("/tmp/pair.sock")));
        assert_eq!(requested_vpn_ip.as_deref(), Some("10.42.0.2"));
        assert_eq!(requested_routes[0].route.prefix, "10.60.0.0/24");
        assert_eq!(requested_routes[0].route.metric, 20);
        assert!(no_wait);

        assert!(
            Cli::try_parse_from([
                "p2p-vpn",
                "pair",
                "status",
                "operation",
                "--instance",
                "runner-mesh",
                "--socket",
                "/tmp/pair.sock",
            ])
            .is_err()
        );
    }

    #[test]
    fn cli_parses_live_pairing_approval_and_artifact_commands() {
        let approve = Cli::try_parse_from([
            "p2p-vpn",
            "pair",
            "approve",
            "operation",
            "approval",
            "--vpn-ip",
            "10.42.0.2",
            "--route",
            "10.60.0.0/24,20",
        ])
        .expect("pair approve CLI");
        let Command::Pair {
            command:
                PairCommand::Approve {
                    operation_id,
                    approval_id,
                    assigned_vpn_ip,
                    granted_routes,
                    ..
                },
        } = approve.command
        else {
            panic!("expected pair approve command");
        };
        assert_eq!(operation_id, "operation");
        assert_eq!(approval_id, "approval");
        assert_eq!(assigned_vpn_ip.as_deref(), Some("10.42.0.2"));
        assert_eq!(granted_routes[0].route.metric, 20);

        let artifacts = Cli::try_parse_from([
            "p2p-vpn",
            "pair",
            "artifacts",
            "operation",
            "--instance",
            "runner-mesh",
            "--output",
            "paired.nix",
            "--nixos-instance",
            "renamed-mesh",
            "--force",
        ])
        .expect("pair artifacts CLI");
        let Command::Pair {
            command:
                PairCommand::Artifacts {
                    operation_id,
                    target,
                    output,
                    nixos_instance,
                    force,
                },
        } = artifacts.command
        else {
            panic!("expected pair artifacts command");
        };
        assert_eq!(operation_id, "operation");
        assert_eq!(target.instance.as_deref(), Some("runner-mesh"));
        assert_eq!(output, PathBuf::from("paired.nix"));
        assert_eq!(nixos_instance.as_deref(), Some("renamed-mesh"));
        assert_eq!(
            pair_artifact_nixos_instance(&target, None),
            Some("runner-mesh")
        );
        assert_eq!(
            pair_artifact_nixos_instance(&target, nixos_instance.as_deref()),
            Some("renamed-mesh")
        );
        assert!(force);

        let acknowledge = Cli::try_parse_from([
            "p2p-vpn",
            "pair",
            "acknowledge",
            "operation",
            "--instance",
            "runner-mesh",
            "--receipt",
            "transcript-digest",
            "--format",
            "json",
        ])
        .expect("pair acknowledge CLI");
        let Command::Pair {
            command:
                PairCommand::Acknowledge {
                    operation_id,
                    target,
                    transcript_sha256,
                    format,
                },
        } = acknowledge.command
        else {
            panic!("expected pair acknowledge command");
        };
        assert_eq!(operation_id, "operation");
        assert_eq!(target.instance.as_deref(), Some("runner-mesh"));
        assert_eq!(transcript_sha256, "transcript-digest");
        assert_eq!(format, PairOutputFormat::Json);
    }

    #[test]
    fn cli_accepts_hyphen_prefixed_live_pairing_ids() {
        let commands: &[&[&str]] = &[
            &["p2p-vpn", "pair", "status", "-operation"],
            &["p2p-vpn", "pair", "approve", "-operation", "-approval"],
            &["p2p-vpn", "pair", "reject", "-operation", "-approval"],
            &["p2p-vpn", "pair", "cancel", "-operation"],
            &["p2p-vpn", "pair", "artifacts", "-operation"],
            &[
                "p2p-vpn",
                "pair",
                "acknowledge",
                "-operation",
                "--receipt",
                "-receipt",
            ],
        ];

        for command in commands {
            Cli::try_parse_from(*command).expect("hyphen-prefixed pairing ID");
        }
    }

    #[test]
    fn cli_parses_pair_offer_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "pair",
            "offer",
            "--config",
            "node-a.json",
            "--output",
            "node-a.pair",
            "--expires-in-seconds",
            "120",
            "--rendezvous-token",
            "BwcHBwcHBwcHBwcHBwcHBw",
            "--discovery-only",
            "--force",
        ])
        .expect("cli");

        let Command::Pair {
            command:
                PairCommand::Offer {
                    config,
                    nixos_instance,
                    output,
                    expires_in_seconds,
                    rendezvous_token,
                    discovery_only,
                    force,
                },
        } = cli.command
        else {
            panic!("expected pair offer command");
        };

        assert_eq!(config, Some(PathBuf::from("node-a.json")));
        assert_eq!(nixos_instance, None);
        assert_eq!(output, PathBuf::from("node-a.pair"));
        assert_eq!(expires_in_seconds, 120);
        assert_eq!(rendezvous_token.as_deref(), Some("BwcHBwcHBwcHBwcHBwcHBw"));
        assert!(discovery_only);
        assert!(force);
    }

    #[test]
    fn cli_parses_nixos_pair_offer_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "pair",
            "offer",
            "--nixos-instance",
            "runner-mesh",
        ])
        .expect("cli");

        let Command::Pair {
            command:
                PairCommand::Offer {
                    config,
                    nixos_instance,
                    ..
                },
        } = cli.command
        else {
            panic!("expected pair offer command");
        };

        assert_eq!(config, None);
        assert_eq!(nixos_instance.as_deref(), Some("runner-mesh"));
    }

    #[test]
    fn pair_offer_resolves_json_and_nixos_sources() {
        assert_eq!(
            pair_offer_config_path(None, None).expect("legacy default"),
            PathBuf::from("p2p-vpn.json")
        );
        assert_eq!(
            pair_offer_config_path(Some(Path::new("custom.json")), None).expect("JSON config"),
            PathBuf::from("custom.json")
        );
        assert_eq!(
            pair_offer_config_path(None, Some("runner-mesh")).expect("NixOS instance"),
            PathBuf::from("/run/p2p-vpn-runner-mesh/config.json")
        );
        assert!(
            pair_offer_config_path(Some(Path::new("custom.json")), Some("runner-mesh"))
                .expect_err("mixed source modes must fail")
                .contains("cannot be used together")
        );
        assert!(
            pair_offer_config_path(None, Some("../runner-mesh"))
                .expect_err("unsafe NixOS instance must fail")
                .contains("--nixos-instance")
        );
    }

    #[test]
    fn cli_parses_pair_inspect_command() {
        let cli =
            Cli::try_parse_from(["p2p-vpn", "pair", "inspect", "node-a.pair", "--show-secret"])
                .expect("cli");

        let Command::Pair {
            command: PairCommand::Inspect { offer, show_secret },
        } = cli.command
        else {
            panic!("expected pair inspect command");
        };

        assert_eq!(offer, "node-a.pair");
        assert!(show_secret);
    }

    #[test]
    fn read_pairing_offer_input_accepts_uri_or_file() {
        let path = temp_config_path("p2p-vpn-pair-inspect");
        fs::write(&path, "p2pvpn:file-offer\n").expect("write offer");

        assert_eq!(
            read_pairing_offer_input(" p2pvpn:inline-offer ").expect("inline offer"),
            "p2pvpn:inline-offer"
        );
        assert_eq!(
            read_pairing_offer_input(path.to_str().expect("path")).expect("file offer"),
            "p2pvpn:file-offer"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn cli_parses_pair_accept_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "pair",
            "accept",
            "p2pvpn:abc",
            "--response",
            "pairing-response.json",
            "--output",
            "node-b.json",
            "--nixos-output",
            "node-b.nix",
            "--nixos-instance",
            "mesh-lab",
            "--nixos-only",
            "--nixos-state-dir",
            "/var/lib/p2p-vpn/mesh-lab",
            "--interface",
            "pv-pair",
            "--mtu",
            "1400",
            "--local-route",
            "10.42.0.2/32,100",
            "--vpn-ip",
            "10.42.0.2",
            "--peer-name",
            "node-a",
            "--timeout-seconds",
            "7",
            "--force",
        ])
        .expect("cli");

        let Command::Pair {
            command:
                PairCommand::Accept {
                    offer,
                    response,
                    output,
                    nixos_output,
                    nixos_instance,
                    nixos_only,
                    nixos_state_dir,
                    interface,
                    mtu,
                    local_routes,
                    vpn_ip,
                    peer_name,
                    timeout_seconds,
                    force,
                    ..
                },
        } = cli.command
        else {
            panic!("expected pair accept command");
        };

        assert_eq!(offer, "p2pvpn:abc");
        assert_eq!(response, Some(PathBuf::from("pairing-response.json")));
        assert_eq!(output, PathBuf::from("node-b.json"));
        assert_eq!(nixos_output, Some(PathBuf::from("node-b.nix")));
        assert_eq!(nixos_instance.as_deref(), Some("mesh-lab"));
        assert!(nixos_only);
        assert_eq!(
            nixos_state_dir.as_deref(),
            Some(Path::new("/var/lib/p2p-vpn/mesh-lab"))
        );
        assert_eq!(interface, "pv-pair");
        assert_eq!(mtu, 1400);
        assert_eq!(local_routes.len(), 1);
        assert_eq!(local_routes[0].route.prefix, "10.42.0.2/32");
        assert_eq!(vpn_ip.as_deref(), Some("10.42.0.2"));
        assert_eq!(peer_name.as_deref(), Some("node-a"));
        assert_eq!(timeout_seconds, 7);
        assert!(force);
    }

    #[test]
    fn pair_accept_output_validation_checks_nixos_paths() {
        assert!(
            validate_pair_accept_outputs(
                Path::new("-"),
                Some(Path::new("node.nix")),
                None,
                None,
                false,
                true
            )
            .expect_err("stdout config should fail")
            .contains("--nixos-output requires --output")
        );
        assert!(
            validate_pair_accept_outputs(
                Path::new("relative.json"),
                Some(Path::new("node.nix")),
                None,
                None,
                false,
                true
            )
            .expect_err("relative config should fail")
            .contains("absolute config path")
        );
        assert!(
            validate_pair_accept_outputs(
                Path::new("/var/lib/p2p-vpn/lab.json"),
                Some(Path::new("/var/lib/p2p-vpn/lab.json")),
                None,
                None,
                false,
                true
            )
            .expect_err("same output should fail")
            .contains("different paths")
        );
        assert!(
            validate_pair_accept_outputs(
                Path::new("/var/lib/p2p-vpn/lab.json"),
                Some(Path::new("node.nix")),
                Some(""),
                None,
                false,
                true,
            )
            .expect_err("empty instance should fail")
            .contains("--nixos-instance")
        );
        validate_pair_accept_outputs(
            Path::new("/var/lib/p2p-vpn/lab.json"),
            Some(Path::new("node.nix")),
            Some("lab"),
            Some(Path::new("/var/lib/p2p-vpn/lab")),
            false,
            true,
        )
        .expect("valid nixos output");
        validate_pair_accept_outputs(
            Path::new("-"),
            Some(Path::new("node.nix")),
            Some("lab"),
            None,
            true,
            true,
        )
        .expect("nixos-only does not require JSON output");
        assert!(
            validate_pair_accept_outputs(Path::new("-"), None, Some("lab"), None, true, true)
                .expect_err("nixos-only without nix output should fail")
                .contains("--nixos-only requires --nixos-output")
        );
        assert!(
            validate_pair_accept_outputs(
                Path::new("-"),
                Some(Path::new("node.nix")),
                Some("../escape"),
                None,
                true,
                true,
            )
            .expect_err("unsafe instance should fail")
            .contains("--nixos-instance")
        );
        assert!(
            validate_pair_accept_outputs(
                Path::new("-"),
                Some(Path::new("node.nix")),
                Some("lab"),
                Some(Path::new("relative")),
                true,
                true,
            )
            .expect_err("relative state path should fail")
            .contains("absolute path")
        );
    }

    #[test]
    fn render_pairing_nixos_module_uses_safe_instance_names() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let mut config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "mesh lab".to_owned(),
                local_peer: String::new(),
                private_key: Some(identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        };
        config.network.local_peer.clear();

        let rendered = render_pairing_nixos_module(
            "mesh-lab",
            &config,
            &PairingNixosSecretPaths {
                private_key_file: Some("/var/lib/p2p-vpn/mesh-lab/private.key".to_owned()),
                membership_key_file: None,
                membership_key_file_is_default: false,
            },
        )
        .expect("render");

        assert!(rendered.contains("instances.\"mesh-lab\""));
        assert!(rendered.contains("networkName = \"mesh lab\";"));
        assert_eq!(default_nixos_instance_name("mesh lab"), "mesh-lab");
        assert_eq!(default_nixos_instance_name("../lab"), "vpn-..-lab");
        assert!(
            render_pairing_nixos_module("../mesh", &config, &PairingNixosSecretPaths::default())
                .expect_err("unsafe instance should fail")
                .contains("--nixos-instance")
        );
    }

    #[test]
    fn render_pairing_nixos_module_uses_module_defaults() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: String::new(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: default_listen_addresses(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        };

        let rendered = render_pairing_nixos_module(
            "lab",
            &config,
            &PairingNixosSecretPaths {
                private_key_file: Some("/var/lib/p2p-vpn/lab/private.key".to_owned()),
                membership_key_file: None,
                membership_key_file_is_default: false,
            },
        )
        .expect("render");

        assert!(rendered.contains("enable = true;"));
        assert!(rendered.contains("localPeer = "));
        assert!(!rendered.contains("\n    networkName ="));
        assert!(!rendered.contains("privateKeyFile"));
        assert!(!rendered.contains("listenAddresses"));
        assert!(!rendered.contains("packetPlane"));
        assert!(!rendered.contains("interfaceName"));
        assert!(!rendered.contains("mtu ="));
    }

    #[test]
    fn render_pair_rpc_nixos_module_emits_minimal_dynamic_fragment() {
        let artifacts: PairRpcCompletionArtifacts = serde_json::from_value(serde_json::json!({
            "receipt": {
                "network_name": "runner mesh",
                "local_peer": "12D3KooWLocal",
                "remote_peer": "12D3KooWRemote",
                "role": "joiner",
                "transcript_sha256": "receipt-digest",
                "completed_at_unix_seconds": 1_700_000_000_u64
            },
            "nix": {
                "instance_name": "runner-mesh",
                "network_name": "runner mesh",
                "local_peer": "12D3KooWLocal",
                "assigned_vpn_ip": "10.42.0.2",
                "additional_local_routes": [
                    { "prefix": "10.60.0.0/24", "metric": 20 }
                ],
                "peer": {
                    "id": "12D3KooWRemote",
                    "name": null,
                    "vpn_ip": null,
                    "routes": [
                        { "prefix": "10.70.0.0/24", "metric": 30 }
                    ]
                },
                "member_records": [
                    {
                        "payload": {
                            "version": 1,
                            "network_name": "runner mesh",
                            "member_peer": "12D3KooWLocal",
                            "member_public_key": "member-public-key",
                            "issuer_peer": "12D3KooWRemote",
                            "issuer_public_key": "issuer-public-key",
                            "membership_epoch": 1,
                            "sequence": 2,
                            "revoked": false,
                            "roles": ["overlay_member", "route_authority"],
                            "route_grants": [
                                { "prefix": "10.60.0.0/24", "metric": 20 }
                            ],
                            "issued_at_unix_seconds": 1_700_000_000_u64,
                            "expires_at_unix_seconds": null
                        },
                        "signature": "record-signature"
                    }
                ],
                "membership_key_file": "/var/lib/p2p-vpn/runner-mesh/membership.key"
            }
        }))
        .expect("pairing artifacts");

        let rendered = render_pair_rpc_nixos_module(&artifacts).expect("render artifacts");

        assert!(rendered.contains("services.p2p-vpn.instances.\"runner-mesh\""));
        assert!(rendered.starts_with("{ lib, ... }:\n{"));
        assert!(rendered.contains("networkName = \"runner mesh\";"));
        assert!(rendered.contains("localPeer = \"12D3KooWLocal\";"));
        assert!(rendered.contains(
            "membershipKeyFile = lib.mkDefault \"/var/lib/p2p-vpn/runner-mesh/membership.key\";"
        ));
        assert!(rendered.contains("vpnIp = \"10.42.0.2\";"));
        assert!(rendered.contains("prefix = \"10.60.0.0/24\";"));
        assert!(!rendered.contains("\"12D3KooWRemote\" = {"));
        assert!(!rendered.contains("prefix = \"10.70.0.0/24\";"));
        assert!(rendered.contains("memberRecords = ["));
        assert!(!rendered.contains("    peers."));
        assert!(!rendered.contains("bootstrapPeers"));
        assert!(!rendered.contains("listenAddresses"));
        assert!(!rendered.contains("addresses ="));
    }

    #[test]
    fn pair_rpc_nixos_config_keeps_signed_revocation_authoritative() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter identity");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner identity");
        let subject = MembershipRecordSubject::from_identity(&joiner).expect("joiner subject");
        let grant = issue_membership_record_for_subject_at(
            &inviter,
            MembershipRecordIssueOptions {
                network_name: "runner-mesh".to_owned(),
                member: subject.clone(),
                membership_epoch: 1,
                sequence: 1,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("membership grant");
        let revocation = issue_membership_record_for_subject_at(
            &inviter,
            MembershipRecordIssueOptions {
                network_name: "runner-mesh".to_owned(),
                member: subject,
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_001,
        )
        .expect("membership revocation");
        let rpc_grant = PairRpcSignedMembershipRecord {
            payload: PairRpcMembershipRecordPayload {
                version: grant.payload.version,
                network_name: grant.payload.network_name.clone(),
                member_peer: grant.payload.member_peer.clone(),
                member_public_key: grant.payload.member_public_key.clone(),
                issuer_peer: grant.payload.issuer_peer.clone(),
                issuer_public_key: grant.payload.issuer_public_key.clone(),
                membership_epoch: grant.payload.membership_epoch,
                sequence: grant.payload.sequence,
                revoked: grant.payload.revoked,
                roles: vec![PairRpcMembershipRole::OverlayMember],
                route_grants: Vec::new(),
                issued_at_unix_seconds: grant.payload.issued_at_unix_seconds,
                expires_at_unix_seconds: grant.payload.expires_at_unix_seconds,
            },
            signature: grant.signature,
        };
        let artifacts = PairRpcCompletionArtifacts {
            receipt: PairRpcReceipt {
                network_name: "runner-mesh".to_owned(),
                local_peer: inviter.peer_id.clone(),
                remote_peer: joiner.peer_id.clone(),
                role: PairRpcRole::Inviter,
                transcript_sha256: "receipt-digest".to_owned(),
                completed_at_unix_seconds: 1_002,
            },
            nix: PairRpcNixPlan {
                instance_name: "runner-mesh".to_owned(),
                network_name: "runner-mesh".to_owned(),
                local_peer: inviter.peer_id,
                assigned_vpn_ip: None,
                additional_local_routes: Vec::new(),
                peer: PairRpcPeer {
                    id: joiner.peer_id.clone(),
                    name: None,
                    vpn_ip: None,
                    routes: Vec::new(),
                },
                member_records: vec![rpc_grant],
                membership_key_file: None,
            },
        };

        let mut config = pair_rpc_nixos_config(&artifacts);
        let joiner_peer = joiner.peer_id.parse().expect("joiner peer ID");
        assert!(config.peers.is_empty());
        assert!(
            AuthorizedPeers::try_from_config(&config)
                .expect("authorized grant")
                .allows(&joiner_peer)
        );

        config.network.member_records.push(revocation);
        assert!(
            !AuthorizedPeers::try_from_config(&config)
                .expect("authorized revocation")
                .allows(&joiner_peer)
        );
    }

    #[test]
    fn render_pairing_nixos_module_emits_typed_instance() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let membership_record: SignedMembershipRecord = serde_json::from_value(serde_json::json!({
            "payload": {
                "version": 1,
                "network_name": "lab",
                "member_peer": "12D3KooWMember",
                "member_public_key": "member-public-key",
                "issuer_peer": "12D3KooWIssuer",
                "issuer_public_key": "issuer-public-key",
                "membership_epoch": 3,
                "sequence": 4,
                "revoked": false,
                "roles": ["overlay_member", "route_authority"],
                "route_grants": [
                    {
                        "prefix": "10.43.0.0/24",
                        "metric": 25
                    }
                ],
                "issued_at_unix_seconds": 1_700_000_000_u64,
                "expires_at_unix_seconds": 1_800_000_000_u64
            },
            "signature": "record-signature"
        }))
        .expect("membership record");
        let mut config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: String::new(),
                private_key: Some(identity.private_key.clone()),
                membership_key: Some("CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=".to_owned()),
                previous_membership_tags: vec!["previous-tag".to_owned()],
                member_records: vec![membership_record],
                vpn_ip: Some("10.42.0.2".to_owned()),
                routes: vec![RouteConfig {
                    prefix: "10.42.0.2/32".to_owned(),
                    metric: 0,
                }],
                listen_addresses: vec![
                    "/ip4/0.0.0.0/tcp/4201".to_owned(),
                    "/ip4/0.0.0.0/udp/4201/quic-v1".to_owned(),
                ],
                external_addresses: vec!["/ip4/198.51.100.8/udp/4201/quic-v1".to_owned()],
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: "12D3KooWBootstrap".to_owned(),
                    address: "/ip4/203.0.113.10/tcp/4001".to_owned(),
                }],
                discovery: DiscoveryConfig {
                    mdns: false,
                    kademlia: true,
                    kademlia_provider_advertisement: false,
                    kademlia_protocol: PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
                    dcutr: false,
                    autonat: false,
                },
                relay: RelayConfig {
                    server: true,
                    reservations: vec![
                        "/ip4/203.0.113.10/tcp/4001/p2p/12D3KooWBootstrap/p2p-circuit".to_owned(),
                    ],
                    auto: AutoRelayConfig {
                        max_candidates: 8,
                        max_reservations: 1,
                        retry_interval_seconds: 11,
                    },
                    resources: RelayResourceConfig {
                        max_reservations: 20,
                        max_reservations_per_peer: 2,
                        reservation_duration_secs: 1200,
                        max_circuits: 12,
                        max_circuits_per_peer: 3,
                        max_circuit_duration_secs: 90,
                        max_circuit_bytes: 262_144,
                    },
                },
                packet_plane: PacketPlaneConfig {
                    listen: vec!["0.0.0.0:52000".to_owned()],
                    external_endpoints: vec!["198.51.100.8:52000".to_owned()],
                    quic_listen: vec!["0.0.0.0:52001".to_owned()],
                    quic_external_endpoints: vec!["198.51.100.8:52001".to_owned()],
                    session_ttl_seconds: 120,
                    max_replay_windows_per_session: 64,
                },
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv-test0".to_owned(),
                mtu: 1400,
            },
            peers: vec![p2p_vpn::config::PeerConfig {
                id: "12D3KooWInviter".to_owned(),
                name: Some("node-a".to_owned()),
                ip: None,
                vpn_ip: None,
                addresses: vec!["/ip4/192.0.2.10/tcp/4001/p2p/12D3KooWInviter".to_owned()],
                routes: vec![RouteConfig {
                    prefix: "10.42.0.1/32".to_owned(),
                    metric: 100,
                }],
            }],
            queue: QueueConfig {
                max_packets_per_peer: 32,
                max_bytes_per_peer: 65_536,
                max_packet_age_millis: 900,
            },
            resources: ResourceConfig {
                max_concurrent_packet_streams: 20,
                max_concurrent_control_streams: 10,
                max_inbound_packets_per_peer_per_second: 1000,
                max_pairing_requests_per_peer_per_second: 2,
                max_pending_incoming_connections: 11,
                max_pending_outgoing_connections: 12,
                max_established_incoming_connections: 13,
                max_established_outgoing_connections: 14,
                max_established_connections_per_peer: 5,
                max_established_connections: 30,
            },
        };
        config.network.local_peer.clear();

        let rendered = render_pairing_nixos_module(
            "lab",
            &config,
            &PairingNixosSecretPaths {
                private_key_file: Some("/run/secrets/p2p-vpn-lab.key".to_owned()),
                membership_key_file: Some("/var/lib/p2p-vpn/lab/membership.key".to_owned()),
                membership_key_file_is_default: false,
            },
        )
        .expect("render");

        assert!(!rendered.contains("\n    networkName ="));
        assert!(rendered.contains("privateKeyFile = \"/run/secrets/p2p-vpn-lab.key\";"));
        assert!(rendered.contains("membershipKeyFile = \"/var/lib/p2p-vpn/lab/membership.key\";"));
        assert!(rendered.contains("interfaceName = \"pv-test0\";"));
        assert!(rendered.contains("mtu = 1400;"));
        assert!(rendered.contains("vpnIp = \"10.42.0.2\";"));
        assert!(rendered.contains("previousMembershipTags = ["));
        assert!(rendered.contains("memberRecords = ["));
        assert!(rendered.contains("membershipEpoch = 3;"));
        assert!(rendered.contains("routeGrants = ["));
        assert!(rendered.contains("\"route_authority\""));
        assert!(rendered.contains("listenAddresses = ["));
        assert!(rendered.contains("externalAddresses = ["));
        assert!(rendered.contains("bootstrapPeers = ["));
        assert!(rendered.contains("kademliaProtocol = \"/p2p-vpn/kad/1\";"));
        assert!(rendered.contains("relayServer = true;"));
        assert!(rendered.contains("relayReservations = ["));
        assert!(rendered.contains("autoRelay = {"));
        assert!(rendered.contains("relayResources = {"));
        assert!(rendered.contains("packetPlane = {"));
        assert!(rendered.contains("externalEndpoints = ["));
        assert!(rendered.contains("quicListen = ["));
        assert!(rendered.contains("sessionTtlSeconds = 120;"));
        assert!(rendered.contains("peers = {"));
        assert!(rendered.contains("\"12D3KooWInviter\" = {"));
        assert!(rendered.contains("metric = 100;"));
        assert!(rendered.contains("queue = {"));
        assert!(rendered.contains("resources = {"));
        assert!(!rendered.contains("configFile"));
        assert!(!rendered.contains("network_name"));
        assert!(!rendered.contains("member_peer"));
        assert!(!rendered.contains(&identity.private_key));
    }

    #[test]
    fn nix_string_literal_escapes_interpolation_and_rejects_controls() {
        assert_eq!(
            nix_string_literal(concat!("lab $", "{builtins.abort \"no\"}"))
                .expect("literal interpolation marker"),
            concat!("\"lab \\$", "{builtins.abort \\\"no\\\"}\"")
        );
        assert!(
            nix_string_literal("lab\u{0008}")
                .expect_err("unsupported control")
                .contains("control")
        );
    }

    #[test]
    fn paired_secret_replacement_does_not_follow_symlinks() {
        let output = temp_config_path("p2p-vpn-paired-secret");
        let victim = temp_config_path("p2p-vpn-paired-secret-victim");
        fs::write(&victim, "keep\n").expect("write victim");
        std::os::unix::fs::symlink(&victim, &output).expect("create secret symlink");

        assert!(
            write_secret_file(&output, "blocked", false)
                .expect_err("normal write must preserve existing path")
                .contains("regular file")
        );
        write_secret_file(&output, "replacement", true).expect("replace symlink atomically");

        assert_eq!(fs::read_to_string(&victim).expect("read victim"), "keep\n");
        assert_eq!(
            fs::read_to_string(&output).expect("read secret"),
            "replacement\n"
        );
        assert!(
            !fs::symlink_metadata(&output)
                .expect("secret metadata")
                .file_type()
                .is_symlink()
        );
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &fs::metadata(&output)
                .expect("secret metadata")
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);

        fs::remove_file(output).expect("remove secret");
        fs::remove_file(victim).expect("remove victim");
    }

    #[test]
    fn nixos_pair_accept_reuses_existing_private_identity() {
        let state_dir = temp_config_path("p2p-vpn-pair-existing-identity");
        ensure_private_directory(&state_dir).expect("private state directory");
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let key_path = state_dir.join("private.key");
        write_secret_file(&key_path, &identity.private_key, false).expect("write identity");

        let reused = resolve_pair_accept_identity(
            None,
            Some(Path::new("/tmp/p2p-vpn-paired.nix")),
            Some("lab"),
            Some(&state_dir),
            "lab",
        )
        .expect("reuse identity");
        assert_eq!(reused.peer_id, identity.peer_id);
        assert_eq!(reused.private_key, identity.private_key);

        write_secret_file(&key_path, &identity.private_key, false)
            .expect("matching identity stays in place");
        assert!(
            write_secret_file(&key_path, "different", false)
                .expect_err("different identity must not overwrite")
                .contains("different content")
        );

        fs::remove_file(key_path).expect("remove identity");
        fs::remove_dir(state_dir).expect("remove state directory");
    }

    #[test]
    fn nixos_pair_accept_rejects_permissive_existing_identity() {
        let state_dir = temp_config_path("p2p-vpn-pair-permissive-identity");
        ensure_private_directory(&state_dir).expect("private state directory");
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let key_path = state_dir.join("private.key");
        fs::write(&key_path, format!("{}\n", identity.private_key)).expect("write identity");
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644))
            .expect("set permissive mode");

        assert!(
            resolve_pair_accept_identity(
                None,
                Some(Path::new("/tmp/p2p-vpn-paired.nix")),
                Some("lab"),
                Some(&state_dir),
                "lab",
            )
            .expect_err("permissive identity must fail")
            .contains("owner-only file")
        );

        fs::remove_file(key_path).expect("remove identity");
        fs::remove_dir(state_dir).expect("remove state directory");
    }

    #[test]
    fn paired_secret_directory_rejects_permissive_existing_paths_without_chmod() {
        let directory = temp_config_path("p2p-vpn-paired-secret-directory");
        fs::create_dir(&directory).expect("create secret directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
            .expect("set permissive mode");

        assert!(
            ensure_private_directory(&directory)
                .expect_err("permissive directory should fail")
                .contains("owner-only")
        );
        let mode = fs::metadata(&directory)
            .expect("directory metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("set private mode");
        ensure_private_directory(&directory).expect("private directory");
        fs::remove_dir(directory).expect("remove secret directory");
    }

    #[test]
    fn pairing_requested_vpn_ip_uses_explicit_or_generated_address() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let generated =
            pairing_requested_vpn_ip(&identity, None).expect("generated requested VPN IP");

        assert!(generated.parse::<IpAddr>().is_ok());
        assert_eq!(
            pairing_requested_vpn_ip(&identity, Some("10.42.0.2")).expect("explicit VPN IP"),
            "10.42.0.2"
        );
        assert!(
            pairing_requested_vpn_ip(&identity, Some("not-an-ip"))
                .expect_err("invalid IP should fail")
                .contains("invalid requested VPN IP")
        );
    }

    #[test]
    fn compact_generated_config_omits_builtin_local_vpn_ip() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let generated_vpn_ip =
            pairing_requested_vpn_ip(&identity, None).expect("generated requested VPN IP");
        let mut config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some(generated_vpn_ip),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        };

        let compact = compact_generated_config(config.clone());
        config.network.vpn_ip = Some("10.42.0.2".to_owned());
        let explicit = compact_generated_config(config);

        assert_eq!(compact.network.vpn_ip, None);
        assert_eq!(explicit.network.vpn_ip.as_deref(), Some("10.42.0.2"));
    }

    #[test]
    fn pairing_accept_diagnostics_summary_includes_route_context() {
        let inviter = NodeIdentity::generate_ed25519().expect("identity");
        let inviter_peer = inviter.peer_id.parse().expect("peer id");
        let direct_address = "/ip4/127.0.0.1/tcp/1".parse().expect("direct address");
        let relayed_address = "/ip4/127.0.0.1/tcp/1/p2p-circuit"
            .parse()
            .expect("relayed address");
        let mut diagnostics = PairingAcceptDiagnostics::new(
            &[
                (inviter_peer, direct_address),
                (inviter_peer, relayed_address),
            ],
            2,
        );

        diagnostics.record_request_attempt();
        diagnostics.record_outbound_failure(&"no address available");
        diagnostics.record_dial_error(None, &"connection refused");
        diagnostics.record_relayed_dial_start_failure(&"missing relay reservation");

        let summary = diagnostics.summary();
        assert!(summary.contains("inviter_hints=2"));
        assert!(summary.contains("relayed_inviter_hints=1"));
        assert!(summary.contains("bootstrap_peers=2"));
        assert!(summary.contains("request_attempts=1"));
        assert!(summary.contains("outbound_failures=1"));
        assert!(summary.contains("dial_errors=1"));
        assert!(summary.contains("relayed_dial_start_failures=1"));
        assert!(summary.contains("last_outbound_failure=\"no address available\""));
        assert!(summary.contains("last_dial_error=unknown peer: \"connection refused\""));
    }

    #[test]
    fn pairing_bootstrap_peers_expands_public_ipfs_defaults_for_compact_offer() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter identity");
        let offer = PairingOffer {
            payload: p2p_vpn::pairing::PairingOfferPayload {
                version: 1,
                network_name: "lab".to_owned(),
                inviter_public_key: STANDARD
                    .encode(inviter.public_key_protobuf().expect("public key")),
                inviter_peer: inviter.peer_id,
                rendezvous_token: "BwcHBwcHBwcHBwcHBwcHBw".to_owned(),
                issued_at_unix_seconds: 1_000,
                expires_at_unix_seconds: 1_600,
                acceptance_mode: p2p_vpn::pairing::PairingAcceptanceMode::FileBearer,
                inviter_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                relay_reservations: Vec::new(),
                discovery: DiscoveryConfig::default(),
                protocols: p2p_vpn::pairing::PairingProtocols::default(),
            },
            signature: String::new(),
        };

        let peers = pairing_bootstrap_peers(&offer).expect("bootstrap peers");

        assert_eq!(
            peers.len(),
            p2p_vpn::config::PUBLIC_IPFS_BOOTSTRAP_PEERS.len()
        );
    }

    #[test]
    fn pairing_dial_address_accepts_direct_and_relayed_inviter_targets() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter identity");
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let other = NodeIdentity::generate_ed25519().expect("other identity");
        let inviter_peer: Libp2pPeerId = inviter.peer_id.parse().expect("inviter peer");
        let relay_peer: Libp2pPeerId = relay.peer_id.parse().expect("relay peer");
        let other_peer: Libp2pPeerId = other.peer_id.parse().expect("other peer");

        let direct = "/ip4/127.0.0.1/tcp/1".parse().expect("direct address");
        assert_eq!(
            pairing_dial_address(inviter_peer, direct)
                .expect("direct target")
                .to_string(),
            format!("/ip4/127.0.0.1/tcp/1/p2p/{inviter_peer}")
        );

        let relayed_base = format!("/ip4/127.0.0.1/tcp/2/p2p/{relay_peer}/p2p-circuit")
            .parse()
            .expect("relayed base");
        assert_eq!(
            pairing_dial_address(inviter_peer, relayed_base)
                .expect("relayed target")
                .to_string(),
            format!("/ip4/127.0.0.1/tcp/2/p2p/{relay_peer}/p2p-circuit/p2p/{inviter_peer}")
        );

        let wrong_relayed_target =
            format!("/ip4/127.0.0.1/tcp/2/p2p/{relay_peer}/p2p-circuit/p2p/{other_peer}")
                .parse()
                .expect("wrong target");
        assert!(pairing_dial_address(inviter_peer, wrong_relayed_target).is_none());
    }

    #[test]
    fn pairing_inviter_addresses_from_kademlia_record_verifies_signed_payload() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter identity");
        let inviter_peer: Libp2pPeerId = inviter.peer_id.parse().expect("inviter peer");
        let config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: inviter.peer_id.clone(),
                private_key: Some(inviter.private_key.clone()),
                membership_key: Some("CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=".to_owned()),
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.42.0.1".to_owned()),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig {
                    mdns: true,
                    kademlia: true,
                    kademlia_provider_advertisement: true,
                    kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
                    dcutr: true,
                    autonat: true,
                },
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        };
        let offer = export_pairing_offer(&config, PairingOfferOptions::default()).expect("offer");
        let payload = PairingKademliaPeerAddressRecordPayload {
            version: 1,
            network_name: "lab".to_owned(),
            membership_tag: None,
            peer_id: inviter.peer_id.clone(),
            public_key_protobuf: inviter.public_key_protobuf().expect("public key"),
            sequence: 42,
            expires_at_unix_seconds: current_unix_seconds_lossy() + 60,
            addresses: vec!["/ip4/127.0.0.1/tcp/9".to_owned()],
        };
        let payload_bytes = serde_json::to_vec(&payload).expect("payload json");
        let record = PairingKademliaPeerAddressRecord {
            payload,
            signature: inviter.sign(&payload_bytes).expect("signature"),
        };
        let value = serde_json::to_vec(&record).expect("record json");

        let addresses =
            pairing_inviter_addresses_from_kademlia_record(&offer, inviter_peer, &value)
                .expect("signed addresses");

        assert_eq!(
            addresses,
            vec![
                format!("/ip4/127.0.0.1/tcp/9/p2p/{inviter_peer}")
                    .parse()
                    .expect("address")
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_pair_accept_reports_diagnostics_on_timeout() {
        let inviter_identity = NodeIdentity::generate_ed25519().expect("inviter identity");
        let joiner_identity = NodeIdentity::generate_ed25519().expect("joiner identity");
        let discovery = DiscoveryConfig {
            mdns: true,
            kademlia: false,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        };
        let inviter_config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: inviter_identity.peer_id.clone(),
                private_key: Some(inviter_identity.private_key),
                membership_key: Some("CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=".to_owned()),
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.42.0.1".to_owned()),
                routes: Vec::new(),
                listen_addresses: vec!["/ip4/127.0.0.1/tcp/9".to_owned()],
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery,
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        };
        let offer =
            export_pairing_offer(&inviter_config, PairingOfferOptions::default()).expect("offer");

        let error = live_pair_accept(
            &offer,
            joiner_identity,
            1280,
            1,
            Some("10.42.0.2".to_owned()),
            Vec::new(),
        )
        .await
        .expect_err("unreachable inviter should time out");

        assert!(error.contains("timed out after 1 seconds"));
        assert!(error.contains("pairing diagnostics:"));
        assert!(error.contains("inviter_hints=1"));
        assert!(error.contains("bootstrap_peers=0"));
        assert!(error.contains("request_attempts="));
        assert!(error.contains("outbound_failures="));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)]
    async fn live_pair_accept_exchanges_with_inviter_over_libp2p() {
        let inviter_identity = NodeIdentity::generate_ed25519().expect("inviter identity");
        let joiner_identity = NodeIdentity::generate_ed25519().expect("joiner identity");
        let membership_key = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=".to_owned();
        let local_discovery = DiscoveryConfig {
            mdns: true,
            kademlia: false,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        };
        let mut inviter_config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: inviter_identity.peer_id.clone(),
                private_key: Some(inviter_identity.private_key.clone()),
                membership_key: Some(membership_key.clone()),
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.42.0.1".to_owned()),
                routes: Vec::new(),
                listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: local_discovery.clone(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        };
        let (address_tx, address_rx) = std::sync::mpsc::channel();
        let (offer_tx, offer_rx) = std::sync::mpsc::channel();
        let (requested_routes_tx, requested_routes_rx) = std::sync::mpsc::channel();
        let inviter_identity_for_thread = inviter_identity.clone();
        let inviter_config_for_thread = inviter_config.clone();
        let membership_key_for_thread = membership_key.clone();
        let inviter_thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("inviter runtime");
            runtime.block_on(async move {
                let mut inviter = build_node(&HostConfig {
                    identity: inviter_identity_for_thread.clone(),
                    network_name: "lab".to_owned(),
                    membership_tag: None,
                    mtu: 1280,
                    max_concurrent_control_streams: 16,
                    max_concurrent_packet_streams: 16,
                    listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
                    external_addresses: Vec::new(),
                    bootstrap_peers: Vec::new(),
                    known_peers: Vec::new(),
                    relay_reservations: Vec::new(),
                    relay_server: false,
                    relay_resources: RelayResourceConfig::default(),
                    resources: ResourceConfig::default(),
                    discovery: local_discovery,
                })
                .expect("inviter node");
                let listener_address = loop {
                    if let SwarmEvent::NewListenAddr { address, .. } =
                        inviter.swarm.select_next_some().await
                    {
                        break address;
                    }
                };
                address_tx
                    .send(listener_address.to_string())
                    .expect("send listener address");
                let offer_for_response = offer_rx.recv().expect("receive offer");
                let mut inviter_config_for_response = inviter_config_for_thread;
                inviter_config_for_response.network.listen_addresses =
                    vec![listener_address.to_string()];

                loop {
                    match inviter.swarm.select_next_some().await {
                        SwarmEvent::Behaviour(BehaviourEvent::Pairing(
                            libp2p::request_response::Event::Message {
                                message:
                                    Message::Request {
                                        request, channel, ..
                                    },
                                ..
                            },
                        )) => {
                            requested_routes_tx
                                .send(request.payload.requested_routes.clone())
                                .expect("send requested routes");
                            let response = build_pairing_response_at(
                                &inviter_config_for_response,
                                &offer_for_response,
                                PairingResponseOptions {
                                    joiner_peer: request.payload.joiner_peer.clone(),
                                    assigned_vpn_ip: request.payload.requested_vpn_ip.clone(),
                                    membership_key: Some(membership_key_for_thread.clone()),
                                    member_records: Vec::new(),
                                    expires_in_seconds: 300,
                                },
                                current_unix_seconds_lossy(),
                            )
                            .expect("response");
                            inviter
                                .swarm
                                .behaviour_mut()
                                .pairing
                                .send_response(channel, response)
                                .expect("send response");
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::Pairing(
                            libp2p::request_response::Event::ResponseSent { .. },
                        )) => return,
                        _ => {}
                    }
                }
            });
        });

        let listener_address = address_rx.recv().expect("listener address");
        inviter_config.network.listen_addresses = vec![listener_address];
        let offer =
            export_pairing_offer(&inviter_config, PairingOfferOptions::default()).expect("offer");
        offer_tx.send(offer.clone()).expect("send offer");

        let requested_vpn_ip =
            pairing_requested_vpn_ip(&joiner_identity, None).expect("generated requested VPN IP");
        let response = live_pair_accept(
            &offer,
            joiner_identity.clone(),
            1280,
            5,
            Some(requested_vpn_ip.clone()),
            vec![RouteConfig {
                prefix: "10.42.0.2/32".to_owned(),
                metric: 100,
            }],
        )
        .await
        .expect("live response");

        inviter_thread.join().expect("inviter thread");
        assert_eq!(
            requested_routes_rx.recv().expect("requested routes"),
            vec![RouteConfig {
                prefix: "10.42.0.2/32".to_owned(),
                metric: 100,
            }]
        );
        assert_eq!(response.payload.inviter_peer, inviter_identity.peer_id);
        assert_eq!(response.payload.joiner_peer, joiner_identity.peer_id);
        assert_eq!(
            response.payload.membership_key.as_deref(),
            Some("CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=")
        );
        assert_eq!(
            response.payload.assigned_vpn_ip.as_deref(),
            Some(requested_vpn_ip.as_str())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)]
    async fn live_pair_accept_uses_bootstrap_when_offer_has_no_inviter_addresses() {
        let inviter_identity = NodeIdentity::generate_ed25519().expect("inviter identity");
        let joiner_identity = NodeIdentity::generate_ed25519().expect("joiner identity");
        let membership_key = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=".to_owned();
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: true,
            kademlia_provider_advertisement: true,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        };
        let mut inviter_config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: inviter_identity.peer_id.clone(),
                private_key: Some(inviter_identity.private_key.clone()),
                membership_key: Some(membership_key.clone()),
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.42.0.1".to_owned()),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: discovery.clone(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        };
        let mut inviter = build_node(&HostConfig {
            identity: inviter_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 16,
            max_concurrent_packet_streams: 16,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("inviter node");
        let listener_address = next_pairing_test_listen_address(&mut inviter.swarm).await;
        inviter_config.network.bootstrap_peers = vec![BootstrapPeerConfig {
            id: inviter_identity.peer_id.clone(),
            address: listener_address.to_string(),
        }];
        let offer =
            export_pairing_offer(&inviter_config, PairingOfferOptions::default()).expect("offer");
        assert!(offer.payload.inviter_addresses.is_empty());
        assert_eq!(offer.payload.bootstrap_peers.len(), 1);
        let mut response_config = inviter_config.clone();
        response_config.network.listen_addresses = vec![listener_address.to_string()];

        let response = {
            let mut accept = Box::pin(live_pair_accept(
                &offer,
                joiner_identity.clone(),
                1280,
                5,
                Some("10.42.0.2".to_owned()),
                Vec::new(),
            ));
            loop {
                tokio::select! {
                    result = &mut accept => break result.expect("live response"),
                    event = inviter.swarm.select_next_some() => {
                        if let SwarmEvent::Behaviour(BehaviourEvent::Pairing(
                            libp2p::request_response::Event::Message {
                                message: Message::Request { request, channel, .. },
                                ..
                            },
                        )) = event {
                            let response = build_pairing_response_at(
                                &response_config,
                                &offer,
                                PairingResponseOptions {
                                    joiner_peer: request.payload.joiner_peer.clone(),
                                    assigned_vpn_ip: request.payload.requested_vpn_ip.clone(),
                                    membership_key: Some(membership_key.clone()),
                                    member_records: Vec::new(),
                                    expires_in_seconds: 300,
                                },
                                current_unix_seconds_lossy(),
                            )
                            .expect("response");
                            inviter
                                .swarm
                                .behaviour_mut()
                                .pairing
                                .send_response(channel, response)
                                .expect("send response");
                        }
                    }
                }
            }
        };

        assert_eq!(response.payload.inviter_peer, inviter_identity.peer_id);
        assert_eq!(response.payload.joiner_peer, joiner_identity.peer_id);
        assert_eq!(
            response.payload.membership_key.as_deref(),
            Some("CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines)]
    async fn live_pair_accept_uses_relay_for_discovery_only_offer() {
        let relay_identity = NodeIdentity::generate_ed25519().expect("relay identity");
        let inviter_identity = NodeIdentity::generate_ed25519().expect("inviter identity");
        let joiner_identity = NodeIdentity::generate_ed25519().expect("joiner identity");
        let membership_key = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=".to_owned();
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: false,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        };
        let mut relay = build_node(&HostConfig {
            identity: relay_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 16,
            max_concurrent_packet_streams: 16,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("relay listen")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("relay node");
        let relay_address = next_pairing_test_listen_address(&mut relay.swarm).await;
        relay.swarm.add_external_address(relay_address.clone());
        let relay_peer = relay.local_peer_id;
        let relay_reservation = relay_address
            .clone()
            .with_p2p(relay_peer)
            .expect("relay p2p address")
            .with(Protocol::P2pCircuit);
        let mut inviter = build_node(&HostConfig {
            identity: inviter_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 16,
            max_concurrent_packet_streams: 16,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            // The inviter only needs to reserve the relay here. The offer still carries
            // relay bootstrap hints for the joiner.
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: vec![relay_reservation.clone()],
            relay_server: false,
            relay_resources: RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("inviter node");
        let inviter_peer = inviter.local_peer_id;
        let relayed_inviter_multiaddr = relay_reservation.with(Protocol::P2p(inviter_peer));
        let relayed_inviter_address = relayed_inviter_multiaddr.to_string();
        wait_for_pairing_test_relay_reservation(
            &mut relay.swarm,
            &mut inviter.swarm,
            relayed_inviter_multiaddr.clone(),
            relay_peer,
        )
        .await;
        let mut inviter_config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "lab".to_owned(),
                local_peer: inviter_identity.peer_id.clone(),
                private_key: Some(inviter_identity.private_key.clone()),
                membership_key: Some(membership_key.clone()),
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.42.0.1".to_owned()),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: relay_identity.peer_id.clone(),
                    address: relay_address.to_string(),
                }],
                discovery,
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        };
        inviter_config.network.relay.reservations = vec![
            relayed_inviter_multiaddr
                .clone()
                .without_p2p_target()
                .to_string(),
        ];
        let offer =
            export_discovery_only_pairing_offer(&inviter_config, PairingOfferOptions::default())
                .expect("offer");
        assert!(offer.payload.inviter_addresses.is_empty());
        assert_eq!(offer.payload.bootstrap_peers.len(), 1);
        assert_eq!(offer.payload.relay_reservations.len(), 1);

        let response = {
            let mut accept = Box::pin(live_pair_accept(
                &offer,
                joiner_identity.clone(),
                1280,
                10,
                Some("10.42.0.2".to_owned()),
                Vec::new(),
            ));
            loop {
                tokio::select! {
                    result = &mut accept => break result.expect("live response"),
                    event = relay.swarm.select_next_some() => {
                        let _ = event;
                    }
                    event = inviter.swarm.select_next_some() => {
                        if let SwarmEvent::Behaviour(BehaviourEvent::Pairing(
                            libp2p::request_response::Event::Message {
                                message: Message::Request { request, channel, .. },
                                ..
                            },
                        )) = event {
                            let mut response_config = inviter_config.clone();
                            response_config.network.external_addresses =
                                vec![relayed_inviter_address.clone()];
                            let response = build_pairing_response_at(
                                &response_config,
                                &offer,
                                PairingResponseOptions {
                                    joiner_peer: request.payload.joiner_peer.clone(),
                                    assigned_vpn_ip: request.payload.requested_vpn_ip.clone(),
                                    membership_key: Some(membership_key.clone()),
                                    member_records: Vec::new(),
                                    expires_in_seconds: 300,
                                },
                                current_unix_seconds_lossy(),
                            )
                            .expect("response");
                            inviter
                                .swarm
                                .behaviour_mut()
                                .pairing
                                .send_response(channel, response)
                                .expect("send response");
                        }
                    }
                }
            }
        };
        assert_eq!(response.payload.inviter_peer, inviter_identity.peer_id);
        assert_eq!(response.payload.joiner_peer, joiner_identity.peer_id);
        assert!(
            response
                .payload
                .inviter_addresses
                .iter()
                .any(|address| address == &relayed_inviter_address)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires P2P_VPN_LIVE_RELAY_MULTIADDRS or P2P_VPN_LIVE_RELAY_MULTIADDR for a reachable public libp2p relay"]
    #[allow(clippy::too_many_lines)]
    async fn live_pair_accept_uses_public_relay_for_discovery_only_offer() {
        let relay_candidates = live_pairing_relay_addresses();
        if relay_candidates.is_empty() {
            eprintln!(
                "skipping live public pairing relay smoke: P2P_VPN_LIVE_RELAY_MULTIADDRS and P2P_VPN_LIVE_RELAY_MULTIADDR are not set"
            );
            return;
        }

        let mut failures = Vec::new();
        for relay_candidate in relay_candidates {
            match try_live_public_relay_pairing(relay_candidate.clone()).await {
                Ok(()) => {
                    eprintln!("live public pairing relay smoke passed through {relay_candidate}");
                    return;
                }
                Err(error) => failures.push(format!("{relay_candidate}: {error}")),
            }
        }

        panic!(
            "no live public relay candidate completed discovery-only live pairing:\n{}",
            failures.join("\n")
        );
    }

    #[allow(clippy::too_many_lines)]
    async fn try_live_public_relay_pairing(relay_address: Multiaddr) -> Result<(), String> {
        let relay_peer = relay_address
            .iter()
            .find_map(|protocol| match protocol {
                Protocol::P2p(peer) => Some(peer),
                _ => None,
            })
            .ok_or_else(|| "relay candidate missing /p2p/RELAY".to_owned())?;
        let relay_reservation = relay_address.clone().with(Protocol::P2pCircuit);
        let inviter_identity = NodeIdentity::generate_ed25519().expect("inviter identity");
        let joiner_identity = NodeIdentity::generate_ed25519().expect("joiner identity");
        let membership_key = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=".to_owned();
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: false,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: true,
            autonat: true,
        };
        let mut inviter = build_node(&HostConfig {
            identity: inviter_identity.clone(),
            network_name: "live-public-pairing".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 16,
            max_concurrent_packet_streams: 16,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            // Avoid a duplicate direct bootstrap dial from the inviter to the relay while
            // the relay reservation listener is being established.
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: vec![relay_reservation.clone()],
            relay_server: false,
            relay_resources: RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .map_err(|error| format!("build inviter node failed: {error:?}"))?;
        let relayed_inviter_multiaddr =
            relay_reservation.with(Protocol::P2p(inviter.local_peer_id));
        tokio::time::timeout(
            live_pairing_relay_timeout(),
            wait_for_pairing_test_relay_reservation(
                &mut NoopRelayDriver,
                &mut inviter.swarm,
                relayed_inviter_multiaddr.clone(),
                relay_peer,
            ),
        )
        .await
        .map_err(|_| {
            format!(
                "relay reservation timed out before {relayed_inviter_multiaddr} was accepted and reported"
            )
        })?;

        let mut inviter_config = Config {
            network: p2p_vpn::config::NetworkConfig {
                name: "live-public-pairing".to_owned(),
                local_peer: inviter_identity.peer_id.clone(),
                private_key: Some(inviter_identity.private_key.clone()),
                membership_key: Some(membership_key.clone()),
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.42.0.1".to_owned()),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: relay_peer.to_string(),
                    address: relay_address.to_string(),
                }],
                discovery,
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: p2p_vpn::config::InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        };
        inviter_config.network.relay.reservations = vec![
            relayed_inviter_multiaddr
                .clone()
                .without_p2p_target()
                .to_string(),
        ];
        let offer =
            export_discovery_only_pairing_offer(&inviter_config, PairingOfferOptions::default())
                .map_err(|error| format!("export offer failed: {error:?}"))?;
        let relayed_inviter_address = relayed_inviter_multiaddr.to_string();
        let response_config = {
            let mut config = inviter_config.clone();
            config.network.external_addresses = vec![relayed_inviter_address.clone()];
            config
        };
        let timeout_seconds = live_pairing_relay_timeout().as_secs().max(1);

        let response = tokio::time::timeout(live_pairing_relay_timeout(), async {
            let mut accept = Box::pin(live_pair_accept(
                &offer,
                joiner_identity.clone(),
                1280,
                timeout_seconds,
                Some("10.42.0.2".to_owned()),
                Vec::new(),
            ));
            loop {
                tokio::select! {
                    result = &mut accept => break result,
                    event = inviter.swarm.select_next_some() => {
                        if let SwarmEvent::Behaviour(BehaviourEvent::Pairing(
                            libp2p::request_response::Event::Message {
                                message: Message::Request { request, channel, .. },
                                ..
                            },
                        )) = event {
                            let response = build_pairing_response_at(
                                &response_config,
                                &offer,
                                PairingResponseOptions {
                                    joiner_peer: request.payload.joiner_peer.clone(),
                                    assigned_vpn_ip: request.payload.requested_vpn_ip.clone(),
                                    membership_key: Some(membership_key.clone()),
                                    member_records: Vec::new(),
                                    expires_in_seconds: 300,
                                },
                                current_unix_seconds_lossy(),
                            )
                            .expect("response");
                            inviter
                                .swarm
                                .behaviour_mut()
                                .pairing
                                .send_response(channel, response)
                                .expect("send response");
                        }
                    }
                }
            }
        })
        .await
        .map_err(|_| "live pairing timed out".to_owned())?
        .map_err(|error| format!("live accept failed: {error}"))?;

        if response.payload.inviter_peer != inviter_identity.peer_id {
            return Err("response inviter peer mismatch".to_owned());
        }
        if response.payload.joiner_peer != joiner_identity.peer_id {
            return Err("response joiner peer mismatch".to_owned());
        }
        if !response
            .payload
            .inviter_addresses
            .iter()
            .any(|address| address == &relayed_inviter_address)
        {
            return Err("response missing relayed inviter address".to_owned());
        }

        Ok(())
    }

    async fn next_pairing_test_listen_address(
        swarm: &mut libp2p::Swarm<p2p_vpn::runtime::p2p::Behaviour>,
    ) -> Multiaddr {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                return address;
            }
        }
    }

    struct NoopRelayDriver;

    impl futures::Stream for NoopRelayDriver {
        type Item = SwarmEvent<BehaviourEvent>;

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _context: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }

    async fn wait_for_pairing_test_relay_reservation(
        relay: &mut (impl futures::Stream<Item = SwarmEvent<BehaviourEvent>> + Unpin),
        listener: &mut libp2p::Swarm<p2p_vpn::runtime::p2p::Behaviour>,
        relayed_address: Multiaddr,
        relay_peer: Libp2pPeerId,
    ) {
        let mut listen_addr_reported = false;
        let mut reservation_accepted = false;

        loop {
            tokio::select! {
                event = relay.next() => {
                    assert!(event.is_some(), "relay event stream ended before reservation completed");
                }
                event = listener.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(BehaviourEvent::Relay(
                            libp2p::relay::client::Event::ReservationReqAccepted {
                                relay_peer_id,
                                renewal,
                                ..
                            },
                        )) if relay_peer_id == relay_peer && !renewal => {
                            reservation_accepted = true;
                        }
                        SwarmEvent::NewListenAddr { address, .. } if address == relayed_address => {
                            listen_addr_reported = true;
                        }
                        _ => {}
                    }
                }
            }

            if listen_addr_reported && reservation_accepted {
                return;
            }
        }
    }

    fn live_pairing_relay_addresses() -> Vec<Multiaddr> {
        let raw = std::env::var(LIVE_PAIRING_RELAY_MULTIADDRS_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| std::env::var(LIVE_PAIRING_RELAY_MULTIADDR_ENV).ok());
        let Some(raw) = raw else {
            return Vec::new();
        };

        raw.split([',', '\n'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                value
                    .parse::<Multiaddr>()
                    .expect("live relay candidate must parse")
            })
            .inspect(|address| {
                assert!(
                    address
                        .iter()
                        .any(|protocol| matches!(protocol, Protocol::P2p(_))),
                    "live relay candidate must include /p2p/RELAY: {address}"
                );
                assert!(
                    !address
                        .iter()
                        .any(|protocol| matches!(protocol, Protocol::P2pCircuit)),
                    "live relay candidate must be a direct relay address without /p2p-circuit: {address}"
                );
            })
            .collect()
    }

    fn live_pairing_relay_timeout() -> Duration {
        let seconds = std::env::var(LIVE_PAIRING_RELAY_TIMEOUT_SECONDS_ENV)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(45)
            .max(1);
        Duration::from_secs(seconds)
    }

    trait PairingTestRelayAddressExt {
        fn without_p2p_target(self) -> Self;
    }

    impl PairingTestRelayAddressExt for Multiaddr {
        fn without_p2p_target(self) -> Self {
            let mut address = Multiaddr::empty();
            let mut after_circuit = false;
            for protocol in &self {
                if after_circuit {
                    continue;
                }
                after_circuit = matches!(protocol, Protocol::P2pCircuit);
                address.push(protocol);
            }
            address
        }
    }

    #[test]
    fn cli_parses_membership_record_commands() {
        let issue = Cli::try_parse_from([
            "p2p-vpn",
            "membership-record-issue",
            "--issuer-config",
            "issuer.json",
            "--member-identity",
            "member-public.json",
            "--output",
            "member-record.json",
            "--network",
            "lab",
            "--membership-epoch",
            "7",
            "--sequence",
            "42",
            "--role",
            "overlay-member",
            "--route-grant",
            "10.77.0.0/24,55",
            "--revoked",
            "--force",
        ])
        .expect("issue cli");
        let Command::MembershipRecordIssue {
            issuer_config,
            member_identity,
            output,
            network,
            membership_epoch,
            sequence,
            roles,
            route_grants,
            revoked,
            force,
            ..
        } = issue.command
        else {
            panic!("expected membership-record-issue command");
        };

        assert_eq!(issuer_config, PathBuf::from("issuer.json"));
        assert_eq!(member_identity, Some(PathBuf::from("member-public.json")));
        assert_eq!(output, PathBuf::from("member-record.json"));
        assert_eq!(network.as_deref(), Some("lab"));
        assert_eq!(membership_epoch, 7);
        assert_eq!(sequence, 42);
        assert_eq!(roles, vec![MembershipRecordRoleArg::OverlayMember]);
        assert_eq!(route_grants[0].route.prefix, "10.77.0.0/24");
        assert_eq!(route_grants[0].route.metric, 55);
        assert!(revoked);
        assert!(force);

        let verify = Cli::try_parse_from([
            "p2p-vpn",
            "membership-record-verify",
            "--input",
            "member-record.json",
            "--network",
            "lab",
        ])
        .expect("verify cli");
        let Command::MembershipRecordVerify { input, network } = verify.command else {
            panic!("expected membership-record-verify command");
        };

        assert_eq!(input, PathBuf::from("member-record.json"));
        assert_eq!(network.as_deref(), Some("lab"));

        let install = Cli::try_parse_from([
            "p2p-vpn",
            "membership-record-install",
            "--config",
            "node-a.json",
            "--record",
            "member-record.json",
            "--record",
            "revocation-record.json",
            "--output",
            "node-a.updated.json",
            "--force",
        ])
        .expect("install cli");
        let Command::MembershipRecordInstall {
            config,
            records,
            output,
            force,
        } = install.command
        else {
            panic!("expected membership-record-install command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
        assert_eq!(
            records,
            vec![
                PathBuf::from("member-record.json"),
                PathBuf::from("revocation-record.json")
            ]
        );
        assert_eq!(output, Some(PathBuf::from("node-a.updated.json")));
        assert!(force);
    }

    #[test]
    fn cli_parses_membership_record_list_command() {
        let list = Cli::try_parse_from([
            "p2p-vpn",
            "membership-record-list",
            "--config",
            "node-a.json",
        ])
        .expect("list cli");
        let Command::MembershipRecordList { config } = list.command else {
            panic!("expected membership-record-list command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
    }

    #[test]
    fn cli_parses_membership_record_self_issue_flag() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "membership-record-issue",
            "--issuer-config",
            "issuer.json",
            "--issuer-as-member",
            "--output",
            "issuer-root.json",
        ])
        .expect("issue cli");

        let Command::MembershipRecordIssue {
            issuer_config,
            issuer_as_member,
            output,
            ..
        } = cli.command
        else {
            panic!("expected membership-record-issue command");
        };

        assert_eq!(issuer_config, PathBuf::from("issuer.json"));
        assert!(issuer_as_member);
        assert_eq!(output, PathBuf::from("issuer-root.json"));
    }

    #[test]
    fn membership_record_issue_and_verify_round_trip() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let issuer_config = temp_config_path("p2p-vpn-membership-issuer");
        let member_public = temp_config_path("p2p-vpn-membership-member-public");
        let record_path = temp_config_path("p2p-vpn-membership-record");
        let issuer_config_value = InitConfigTemplate {
            identity: issuer,
            network_name: "lab".to_owned(),
            membership_key: None,
            vpn_ip: None,
            local_routes: Vec::new(),
            interface_name: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            packet_plane: PacketPlaneConfig::default(),
            bootstrap_peers: Vec::new(),
            peers: Vec::new(),
            discovery: DiscoveryConfig {
                kademlia_protocol: PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
                ..DiscoveryConfig::default()
            },
            relay: RelayConfig::default(),
        }
        .into_config();
        write_config_output(&issuer_config_value, &issuer_config, false).expect("issuer config");
        identity_public(IdentityPublicArgs {
            config: None,
            private_key: Some(member.private_key),
            output: member_public.clone(),
            force: false,
        })
        .expect("member public identity");

        membership_record_issue(MembershipRecordIssueArgs {
            issuer_config: issuer_config.clone(),
            member_identity: Some(member_public.clone()),
            member_peer: None,
            member_public_key: None,
            issuer_as_member: false,
            output: record_path.clone(),
            network: None,
            membership_epoch: 7,
            sequence: 42,
            roles: vec![MembershipRecordRoleArg::OverlayMember],
            route_grants: vec![LocalRouteArg {
                route: RouteConfig {
                    prefix: "10.77.0.0/24".to_owned(),
                    metric: 55,
                },
            }],
            revoked: false,
            expires_at_unix_seconds: None,
            force: false,
        })
        .expect("issue record");
        membership_record_verify(&record_path, Some("lab")).expect("verify record");

        let mut record: SignedMembershipRecord =
            serde_json::from_slice(&fs::read(&record_path).expect("record file"))
                .expect("record json");
        assert_eq!(record.payload.membership_epoch, 7);
        assert_eq!(record.payload.sequence, 42);
        assert!(!record.payload.revoked);
        assert!(
            record
                .payload
                .roles
                .contains(&MembershipRole::OverlayMember)
        );
        assert!(
            record
                .payload
                .roles
                .contains(&MembershipRole::RouteAuthority)
        );
        assert_eq!(record.payload.route_grants[0].prefix, "10.77.0.0/24");
        record.payload.sequence += 1;
        let rendered = serde_json::to_string_pretty(&record).expect("render tampered record");
        fs::write(&record_path, format!("{rendered}\n")).expect("write tampered record");

        assert!(membership_record_verify(&record_path, Some("lab")).is_err());

        let conflicting_subject = membership_record_issue(MembershipRecordIssueArgs {
            issuer_config: issuer_config.clone(),
            member_identity: Some(member_public.clone()),
            member_peer: None,
            member_public_key: None,
            issuer_as_member: true,
            output: record_path.clone(),
            network: None,
            membership_epoch: 7,
            sequence: 43,
            roles: vec![MembershipRecordRoleArg::OverlayMember],
            route_grants: Vec::new(),
            revoked: false,
            expires_at_unix_seconds: None,
            force: true,
        });
        assert!(conflicting_subject.is_err());

        let _ = fs::remove_file(&issuer_config);
        let _ = fs::remove_file(&member_public);
        let _ = fs::remove_file(&record_path);
    }

    #[test]
    fn membership_record_issue_can_create_issuer_trust_root() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let issuer_config = temp_config_path("p2p-vpn-membership-trust-root-issuer");
        let root_record_path = temp_config_path("p2p-vpn-membership-trust-root");
        let member_record_path = temp_config_path("p2p-vpn-membership-trust-root-member");
        let issuer_config_value = InitConfigTemplate {
            identity: issuer.clone(),
            network_name: "lab".to_owned(),
            membership_key: None,
            vpn_ip: None,
            local_routes: Vec::new(),
            interface_name: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            packet_plane: PacketPlaneConfig::default(),
            bootstrap_peers: Vec::new(),
            peers: Vec::new(),
            discovery: DiscoveryConfig {
                kademlia_protocol: PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
                ..DiscoveryConfig::default()
            },
            relay: RelayConfig::default(),
        }
        .into_config();
        write_config_output(&issuer_config_value, &issuer_config, false).expect("issuer config");

        membership_record_issue(MembershipRecordIssueArgs {
            issuer_config: issuer_config.clone(),
            member_identity: None,
            member_peer: None,
            member_public_key: None,
            issuer_as_member: true,
            output: root_record_path.clone(),
            network: None,
            membership_epoch: 1,
            sequence: 1,
            roles: vec![MembershipRecordRoleArg::OverlayMember],
            route_grants: Vec::new(),
            revoked: false,
            expires_at_unix_seconds: None,
            force: false,
        })
        .expect("issue issuer self-record");
        membership_record_verify(&root_record_path, Some("lab")).expect("verify self-record");

        let root_record: SignedMembershipRecord =
            serde_json::from_slice(&fs::read(&root_record_path).expect("root record file"))
                .expect("root record json");
        assert_eq!(root_record.payload.issuer_peer, issuer.peer_id);
        assert_eq!(root_record.payload.member_peer, issuer.peer_id);

        let member_subject =
            MembershipRecordSubject::from_identity(&member).expect("member subject");
        membership_record_issue(MembershipRecordIssueArgs {
            issuer_config: issuer_config.clone(),
            member_identity: None,
            member_peer: Some(member_subject.peer_id.clone()),
            member_public_key: Some(member_subject.public_key.clone()),
            issuer_as_member: false,
            output: member_record_path.clone(),
            network: None,
            membership_epoch: 1,
            sequence: 2,
            roles: vec![MembershipRecordRoleArg::OverlayMember],
            route_grants: Vec::new(),
            revoked: false,
            expires_at_unix_seconds: None,
            force: false,
        })
        .expect("issue member record");
        let member_record: SignedMembershipRecord =
            serde_json::from_slice(&fs::read(&member_record_path).expect("member record file"))
                .expect("member record json");
        let trusted_issuers = p2p_vpn::membership::trusted_membership_issuers_at(
            std::slice::from_ref(&root_record),
            "lab",
            1,
        )
        .expect("trusted issuers");
        let mut records = vec![root_record];

        let stats = p2p_vpn::membership::merge_membership_records_at(
            &mut records,
            std::slice::from_ref(&member_record),
            "lab",
            current_unix_seconds_lossy(),
            &trusted_issuers,
            4,
        )
        .expect("trusted issuer merge");

        assert_eq!(stats.accepted, 1);
        assert_eq!(records.len(), 2);

        let _ = fs::remove_file(&issuer_config);
        let _ = fs::remove_file(&root_record_path);
        let _ = fs::remove_file(&member_record_path);
    }

    #[test]
    fn membership_record_issue_revocation_round_trip() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let issuer_config = temp_config_path("p2p-vpn-revocation-issuer");
        let member_public = temp_config_path("p2p-vpn-revocation-member-public");
        let record_path = temp_config_path("p2p-vpn-revocation-record");
        let issuer_config_value = InitConfigTemplate {
            identity: issuer,
            network_name: "lab".to_owned(),
            membership_key: None,
            vpn_ip: None,
            local_routes: Vec::new(),
            interface_name: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            packet_plane: PacketPlaneConfig::default(),
            bootstrap_peers: Vec::new(),
            peers: Vec::new(),
            discovery: DiscoveryConfig {
                kademlia_protocol: PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
                ..DiscoveryConfig::default()
            },
            relay: RelayConfig::default(),
        }
        .into_config();
        write_config_output(&issuer_config_value, &issuer_config, false).expect("issuer config");
        identity_public(IdentityPublicArgs {
            config: None,
            private_key: Some(member.private_key),
            output: member_public.clone(),
            force: false,
        })
        .expect("member public identity");

        membership_record_issue(MembershipRecordIssueArgs {
            issuer_config: issuer_config.clone(),
            member_identity: Some(member_public.clone()),
            member_peer: None,
            member_public_key: None,
            issuer_as_member: false,
            output: record_path.clone(),
            network: None,
            membership_epoch: 7,
            sequence: 43,
            roles: Vec::new(),
            route_grants: Vec::new(),
            revoked: true,
            expires_at_unix_seconds: None,
            force: false,
        })
        .expect("issue revocation");
        membership_record_verify(&record_path, Some("lab")).expect("verify revocation");

        let record: SignedMembershipRecord =
            serde_json::from_slice(&fs::read(&record_path).expect("record file"))
                .expect("record json");
        assert!(record.payload.revoked);
        assert!(record.payload.roles.is_empty());
        assert!(record.payload.route_grants.is_empty());

        let rejected = membership_record_issue(MembershipRecordIssueArgs {
            issuer_config: issuer_config.clone(),
            member_identity: Some(member_public.clone()),
            member_peer: None,
            member_public_key: None,
            issuer_as_member: false,
            output: record_path.clone(),
            network: None,
            membership_epoch: 7,
            sequence: 44,
            roles: vec![MembershipRecordRoleArg::OverlayMember],
            route_grants: Vec::new(),
            revoked: true,
            expires_at_unix_seconds: None,
            force: true,
        });
        assert!(rejected.is_err());

        let _ = fs::remove_file(&issuer_config);
        let _ = fs::remove_file(&member_public);
        let _ = fs::remove_file(&record_path);
    }

    #[test]
    fn membership_record_install_updates_config_idempotently() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let issuer_config = temp_config_path("p2p-vpn-install-issuer");
        let member_public = temp_config_path("p2p-vpn-install-member-public");
        let member_record = temp_config_path("p2p-vpn-install-member-record");
        let output = temp_config_path("p2p-vpn-install-output");
        let issuer_config_value = InitConfigTemplate {
            identity: issuer,
            network_name: "lab".to_owned(),
            membership_key: None,
            vpn_ip: None,
            local_routes: Vec::new(),
            interface_name: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            packet_plane: PacketPlaneConfig::default(),
            bootstrap_peers: Vec::new(),
            peers: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
        }
        .into_config();
        write_config_output(&issuer_config_value, &issuer_config, false).expect("issuer config");
        identity_public(IdentityPublicArgs {
            config: None,
            private_key: Some(member.private_key),
            output: member_public.clone(),
            force: false,
        })
        .expect("member public identity");

        membership_record_issue(MembershipRecordIssueArgs {
            issuer_config: issuer_config.clone(),
            member_identity: Some(member_public.clone()),
            member_peer: None,
            member_public_key: None,
            issuer_as_member: false,
            output: member_record.clone(),
            network: None,
            membership_epoch: 3,
            sequence: 9,
            roles: Vec::new(),
            route_grants: vec![LocalRouteArg {
                route: RouteConfig {
                    prefix: "10.77.0.0/24".to_owned(),
                    metric: 100,
                },
            }],
            revoked: false,
            expires_at_unix_seconds: None,
            force: false,
        })
        .expect("issue record");

        membership_record_install(
            &issuer_config,
            std::slice::from_ref(&member_record),
            Some(&output),
            false,
        )
        .expect("install record");
        let installed = Config::load(&output).expect("load installed config");
        assert_eq!(installed.network.member_records.len(), 1);
        assert_eq!(
            installed.network.member_records[0].payload.membership_epoch,
            3
        );
        assert_eq!(installed.network.member_records[0].payload.sequence, 9);
        assert_eq!(
            installed.network.member_records[0].payload.route_grants[0].prefix,
            "10.77.0.0/24"
        );

        membership_record_install(&output, std::slice::from_ref(&member_record), None, false)
            .expect("reinstall same record");
        let reinstalled = Config::load(&output).expect("load reinstalled config");
        assert_eq!(reinstalled.network.member_records.len(), 1);

        let _ = fs::remove_file(&issuer_config);
        let _ = fs::remove_file(&member_public);
        let _ = fs::remove_file(&member_record);
        let _ = fs::remove_file(&output);
    }

    #[test]
    fn membership_record_install_keeps_distinct_issuer_records_for_same_member() {
        let issuer_a = NodeIdentity::generate_ed25519().expect("issuer a");
        let issuer_b = NodeIdentity::generate_ed25519().expect("issuer b");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let record_a = issue_membership_record_for_subject_at(
            &issuer_a,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("member subject"),
                membership_epoch: 1,
                sequence: 1,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("record a");
        let record_b = issue_membership_record_for_subject_at(
            &issuer_b,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("member subject"),
                membership_epoch: 1,
                sequence: 1,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("record b");
        let mut records = vec![record_a];

        let stats =
            install_config_membership_records(&mut records, std::slice::from_ref(&record_b));

        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.ignored_stale_or_equal, 0);
        assert_eq!(records.len(), 2);

        let stale = install_config_membership_records(&mut records, &[record_b]);

        assert_eq!(stale.accepted, 0);
        assert_eq!(stale.ignored_stale_or_equal, 1);
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn membership_record_install_rejects_wrong_network_records() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let issuer_config = temp_config_path("p2p-vpn-install-wrong-network-issuer");
        let member_public = temp_config_path("p2p-vpn-install-wrong-network-member-public");
        let member_record = temp_config_path("p2p-vpn-install-wrong-network-record");
        let output = temp_config_path("p2p-vpn-install-wrong-network-output");
        let issuer_config_value = InitConfigTemplate {
            identity: issuer,
            network_name: "lab".to_owned(),
            membership_key: None,
            vpn_ip: None,
            local_routes: Vec::new(),
            interface_name: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            packet_plane: PacketPlaneConfig::default(),
            bootstrap_peers: Vec::new(),
            peers: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
        }
        .into_config();
        write_config_output(&issuer_config_value, &issuer_config, false).expect("issuer config");
        identity_public(IdentityPublicArgs {
            config: None,
            private_key: Some(member.private_key),
            output: member_public.clone(),
            force: false,
        })
        .expect("member public identity");

        membership_record_issue(MembershipRecordIssueArgs {
            issuer_config: issuer_config.clone(),
            member_identity: Some(member_public.clone()),
            member_peer: None,
            member_public_key: None,
            issuer_as_member: false,
            output: member_record.clone(),
            network: Some("other".to_owned()),
            membership_epoch: 1,
            sequence: 1,
            roles: vec![MembershipRecordRoleArg::OverlayMember],
            route_grants: Vec::new(),
            revoked: false,
            expires_at_unix_seconds: None,
            force: false,
        })
        .expect("issue wrong-network record");

        let error = membership_record_install(
            &issuer_config,
            std::slice::from_ref(&member_record),
            Some(&output),
            false,
        )
        .expect_err("wrong network should be rejected");
        assert!(error.contains("NetworkMismatch"));
        assert!(!output.exists());

        let _ = fs::remove_file(&issuer_config);
        let _ = fs::remove_file(&member_public);
        let _ = fs::remove_file(&member_record);
    }

    struct MembershipRecordListFixture {
        config: Config,
        issuer: NodeIdentity,
        member: NodeIdentity,
        revoked: NodeIdentity,
    }

    fn membership_record_list_fixture() -> MembershipRecordListFixture {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let revoked = NodeIdentity::generate_ed25519().expect("revoked");
        let root_record = issue_membership_record_for_subject_at(
            &issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&issuer).expect("issuer subject"),
                membership_epoch: 1,
                sequence: 1,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("root record");
        let member_record = issue_membership_record_for_subject_at(
            &issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("member subject"),
                membership_epoch: 2,
                sequence: 3,
                revoked: false,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: vec![RouteConfig {
                    prefix: "10.77.0.0/24".to_owned(),
                    metric: 55,
                }],
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("member record");
        let revocation = issue_membership_record_for_subject_at(
            &issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&revoked).expect("revoked subject"),
                membership_epoch: 2,
                sequence: 4,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("revocation");
        let mut config = InitConfigTemplate {
            identity: issuer.clone(),
            network_name: "lab".to_owned(),
            membership_key: None,
            vpn_ip: None,
            local_routes: Vec::new(),
            interface_name: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            packet_plane: PacketPlaneConfig::default(),
            bootstrap_peers: Vec::new(),
            peers: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
        }
        .into_config();
        config.network.member_records = vec![root_record, member_record, revocation];

        MembershipRecordListFixture {
            config,
            issuer,
            member,
            revoked,
        }
    }

    #[test]
    fn membership_record_lines_report_trust_roots_grants_and_revocations() {
        let fixture = membership_record_list_fixture();

        let lines = membership_record_lines(&fixture.config).expect("membership record lines");

        assert!(lines.contains(&"membership records configured: 3".to_owned()));
        assert!(lines.contains(&"membership records valid: true".to_owned()));
        assert!(lines.contains(&"trusted issuers: 1".to_owned()));
        assert!(lines.contains(&format!("trusted issuer: {}", fixture.issuer.peer_id)));
        assert!(lines.contains(&"effective overlay members: 2".to_owned()));
        assert!(lines.iter().any(|line| {
            line == &format!(
                "membership record: member {} issuer {} epoch 1 sequence 1 state active roles overlay_member route_grants 0 expires_at never trust_root true",
                fixture.issuer.peer_id, fixture.issuer.peer_id
            )
        }));
        assert!(lines.iter().any(|line| {
            line == &format!(
                "membership record: member {} issuer {} epoch 2 sequence 3 state active roles overlay_member,route_authority route_grants 1 expires_at never trust_root false",
                fixture.member.peer_id, fixture.issuer.peer_id
            )
        }));
        assert!(lines.contains(&format!(
            "membership record route grant: member {} 10.77.0.0/24 metric 55",
            fixture.member.peer_id
        )));
        assert!(lines.iter().any(|line| {
            line == &format!(
                "membership record: member {} issuer {} epoch 2 sequence 4 state revoked roles none route_grants 0 expires_at never trust_root false",
                fixture.revoked.peer_id, fixture.issuer.peer_id
            )
        }));
        assert!(lines.contains(&format!(
            "effective member route grant: {} 10.77.0.0/24 metric 55",
            fixture.member.peer_id
        )));
    }

    #[test]
    fn cli_parses_up_control_socket() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "up",
            "--config",
            "node-a.json",
            "--control-socket",
            "/run/p2p-vpn-node-a/control.sock",
            "--pairing-state",
            "/var/lib/p2p-vpn/node-a/pairing-state.json",
        ])
        .expect("cli");

        let Command::Up {
            config,
            control_socket,
            pairing_state,
            ..
        } = cli.command
        else {
            panic!("expected up command");
        };

        assert_eq!(config, PathBuf::from("node-a.json"));
        assert_eq!(
            control_socket,
            Some(PathBuf::from("/run/p2p-vpn-node-a/control.sock"))
        );
        assert_eq!(
            pairing_state,
            Some(PathBuf::from("/var/lib/p2p-vpn/node-a/pairing-state.json"))
        );
        assert!(
            Cli::try_parse_from([
                "p2p-vpn",
                "up",
                "--pairing-state",
                "/tmp/pairing-state.json",
            ])
            .is_err()
        );
    }

    #[test]
    fn ipfs_kademlia_flag_selects_public_dht_protocol() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "init-config",
            "--ipfs-kademlia",
            "--ipfs-bootstrap-peers",
        ])
        .expect("cli");
        let Command::InitConfig {
            kademlia_protocol,
            ipfs_kademlia,
            ipfs_bootstrap_peers,
            ..
        } = cli.command
        else {
            panic!("expected init-config command");
        };

        assert!(ipfs_kademlia);
        assert!(ipfs_bootstrap_peers);
        assert_eq!(
            selected_kademlia_protocol(kademlia_protocol, ipfs_kademlia),
            PUBLIC_IPFS_KADEMLIA_PROTOCOL
        );
        assert_eq!(
            kademlia_scope(PUBLIC_IPFS_KADEMLIA_PROTOCOL),
            "ipfs-compatible public dht"
        );
        assert_eq!(kademlia_scope("/custom/kad/1"), "custom");
    }

    #[test]
    fn disabling_kademlia_disables_provider_advertisement() {
        let discovery = InitDiscoveryFlags {
            disable_mdns: false,
            disable_kademlia: true,
            disable_kademlia_provider_advertisement: false,
            disable_dcutr: false,
            disable_autonat: false,
        }
        .into_config(PRIVATE_KADEMLIA_PROTOCOL.to_owned(), false, false);

        assert!(!discovery.kademlia);
        assert!(!discovery.kademlia_provider_advertisement);
    }

    #[test]
    fn ipfs_bootstrap_peers_require_ipfs_kademlia() {
        let error = init_config(InitConfigArgs {
            output: PathBuf::from("-"),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: true,
            public_ipfs_profile: false,
            peers: Vec::new(),
            vpn_ip: None,
            peer_vpn_ips: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig {
                kademlia_protocol: PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
                ..DiscoveryConfig::default()
            },
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect_err("private kademlia must not use public IPFS bootstrap defaults");

        assert!(error.contains("--ipfs-bootstrap-peers requires --ipfs-kademlia"));
    }

    #[test]
    fn init_bootstrap_peers_adds_ipfs_defaults_without_duplicates() {
        let peers = init_bootstrap_peers(
            vec![EndpointArg {
                id: PUBLIC_IPFS_BOOTSTRAP_PEERS[0].0.to_owned(),
                address: Some(PUBLIC_IPFS_BOOTSTRAP_PEERS[0].1.to_owned()),
            }],
            Vec::new(),
            true,
        );

        assert_eq!(peers.len(), PUBLIC_IPFS_BOOTSTRAP_PEERS.len());
        for (id, address) in PUBLIC_IPFS_BOOTSTRAP_PEERS {
            assert!(peers.iter().any(|peer| {
                peer.id == *id
                    && peer.address.as_deref() == Some(*address)
                    && peer.routes.is_empty()
            }));
        }
    }

    #[test]
    fn init_config_writes_runtime_valid_ipfs_bootstrap_defaults() {
        let output = temp_config_path("p2p-vpn-ipfs-bootstrap-config");

        init_config(InitConfigArgs {
            output: output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: true,
            public_ipfs_profile: false,
            peers: Vec::new(),
            vpn_ip: None,
            peer_vpn_ips: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: InitDiscoveryFlags {
                disable_mdns: false,
                disable_kademlia: false,
                disable_kademlia_provider_advertisement: false,
                disable_dcutr: false,
                disable_autonat: false,
            }
            .into_config(PRIVATE_KADEMLIA_PROTOCOL.to_owned(), true, false),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect("init config");

        let config = Config::load(&output).expect("load generated config");
        let _ = std::fs::remove_file(&output);

        config.validate_runtime().expect("runtime-valid config");
        assert_eq!(
            config.network.discovery.kademlia_protocol,
            PUBLIC_IPFS_KADEMLIA_PROTOCOL
        );
        assert_eq!(
            config.network.bootstrap_peers.len(),
            PUBLIC_IPFS_BOOTSTRAP_PEERS.len()
        );
        assert_eq!(
            config
                .bootstrap_multiaddrs()
                .expect("bootstrap multiaddrs")
                .len(),
            PUBLIC_IPFS_BOOTSTRAP_PEERS.len()
        );
    }

    #[test]
    fn public_ipfs_profile_selects_safe_public_discovery_defaults() {
        let cli =
            Cli::try_parse_from(["p2p-vpn", "init-config", "--public-ipfs-profile"]).expect("cli");
        let Command::InitConfig {
            public_ipfs_profile,
            ipfs_bootstrap_peers,
            ipfs_kademlia,
            disable_mdns,
            disable_kademlia_provider_advertisement,
            kademlia_protocol,
            ..
        } = cli.command
        else {
            panic!("expected init-config command");
        };

        assert!(public_ipfs_profile);
        assert!(!ipfs_bootstrap_peers);
        assert!(!ipfs_kademlia);

        let discovery = InitDiscoveryFlags {
            disable_mdns,
            disable_kademlia: false,
            disable_kademlia_provider_advertisement,
            disable_dcutr: false,
            disable_autonat: false,
        }
        .into_config(kademlia_protocol, ipfs_kademlia, public_ipfs_profile);
        assert!(discovery.mdns);
        assert!(discovery.kademlia);
        assert!(discovery.kademlia_provider_advertisement);
        assert_eq!(discovery.kademlia_protocol, PUBLIC_IPFS_KADEMLIA_PROTOCOL);
        assert!(discovery.dcutr);
        assert!(discovery.autonat);
    }

    #[test]
    fn init_config_public_ipfs_profile_writes_runtime_valid_config() {
        let output = temp_config_path("p2p-vpn-public-ipfs-profile-config");

        init_config(InitConfigArgs {
            output: output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: false,
            public_ipfs_profile: true,
            peers: Vec::new(),
            vpn_ip: None,
            peer_vpn_ips: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: InitDiscoveryFlags {
                disable_mdns: false,
                disable_kademlia: false,
                disable_kademlia_provider_advertisement: false,
                disable_dcutr: false,
                disable_autonat: false,
            }
            .into_config(PRIVATE_KADEMLIA_PROTOCOL.to_owned(), false, true),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect("init config");

        let config = Config::load(&output).expect("load generated config");
        let _ = std::fs::remove_file(&output);

        config.validate_runtime().expect("runtime-valid config");
        assert!(config.network.discovery.mdns);
        assert!(config.network.discovery.kademlia);
        assert!(config.network.discovery.kademlia_provider_advertisement);
        assert_eq!(
            config.network.discovery.kademlia_protocol,
            PUBLIC_IPFS_KADEMLIA_PROTOCOL
        );
        assert!(config.network.bootstrap_peers.is_empty());
        assert_eq!(
            config
                .effective_bootstrap_multiaddrs()
                .expect("effective bootstrap")
                .len(),
            PUBLIC_IPFS_BOOTSTRAP_PEERS.len()
        );
        assert!(config.network.discovery.autonat);
        assert!(config.network.discovery.dcutr);
    }

    #[test]
    fn public_ipfs_profile_requires_kademlia() {
        let error = init_config(InitConfigArgs {
            output: PathBuf::from("-"),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: true,
            public_ipfs_profile: true,
            peers: Vec::new(),
            vpn_ip: None,
            peer_vpn_ips: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: InitDiscoveryFlags {
                disable_mdns: false,
                disable_kademlia: true,
                disable_kademlia_provider_advertisement: false,
                disable_dcutr: false,
                disable_autonat: false,
            }
            .into_config(PRIVATE_KADEMLIA_PROTOCOL.to_owned(), false, true),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect_err("public profile requires Kademlia");

        assert!(error.contains("--public-ipfs-profile requires Kademlia"));
    }

    #[test]
    fn relay_peer_shortcut_builds_reservation_and_bootstrap_infrastructure() {
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let relay_arg = EndpointArg {
            id: relay.peer_id.clone(),
            address: Some("/ip4/127.0.0.1/tcp/4002".to_owned()),
        };

        assert_eq!(
            relay_reservation_address(&relay_arg).expect("relay reservation"),
            format!("/ip4/127.0.0.1/tcp/4002/p2p/{}/p2p-circuit", relay.peer_id)
        );

        let bootstrap_peers = init_bootstrap_peers(Vec::new(), vec![relay_arg.clone()], false);
        assert_eq!(
            bootstrap_peers,
            vec![InitPeer {
                id: relay.peer_id.clone(),
                address: Some("/ip4/127.0.0.1/tcp/4002".to_owned()),
                vpn_ip: None,
                routes: Vec::new(),
            }]
        );

        let reservations = init_relay_reservations(&[relay_arg]).expect("relay reservations");
        assert_eq!(
            reservations,
            vec![format!(
                "/ip4/127.0.0.1/tcp/4002/p2p/{}/p2p-circuit",
                relay.peer_id
            )]
        );
    }

    #[test]
    fn public_relay_two_host_configs_keep_selected_relay_infrastructure() {
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let relay_address = format!("/ip4/127.0.0.1/tcp/4002/p2p/{}", relay.peer_id);
        let relay_arg = relay_candidate_endpoint_arg(&relay_address).expect("relay endpoint arg");
        let args = relay_check_args_for_test();

        let (host_a, host_b) =
            public_relay_two_host_configs(&args, &relay_arg).expect("two host configs");

        for config in [&host_a, &host_b] {
            config.validate_runtime().expect("runtime-valid config");
            assert_eq!(config.network.bootstrap_peers.len(), 1);
            assert_eq!(config.network.bootstrap_peers[0].id, relay.peer_id);
            assert_eq!(config.network.bootstrap_peers[0].address, relay_address);
            assert_eq!(
                config.network.relay.reservations,
                vec![format!("{relay_address}/p2p-circuit")]
            );
        }
        assert_eq!(host_a.network.vpn_ip.as_deref(), Some("10.42.0.1"));
        assert_eq!(host_b.network.vpn_ip.as_deref(), Some("10.42.0.2"));
    }

    #[test]
    fn relay_candidate_endpoint_arg_parses_validated_direct_address() {
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let address = format!("/ip4/127.0.0.1/tcp/4002/p2p/{}", relay.peer_id);

        let arg = relay_candidate_endpoint_arg(&address).expect("relay endpoint arg");

        assert_eq!(
            arg,
            EndpointArg {
                id: relay.peer_id,
                address: Some(address),
            }
        );
        assert!(
            relay_candidate_endpoint_arg("/ip4/127.0.0.1/tcp/4002")
                .expect_err("missing relay peer should fail")
                .contains("missing /p2p/RELAY")
        );
        assert!(
            relay_candidate_endpoint_arg(&format!(
                "/ip4/127.0.0.1/tcp/4002/p2p/{}/p2p-circuit",
                arg.id
            ))
            .expect_err("relayed address should fail")
            .contains("direct relay address")
        );
    }

    #[test]
    fn public_relay_config_args_generates_runtime_valid_relay_config() {
        let output = temp_config_path("p2p-vpn-public-relay-config");
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let relay_address = format!("/ip4/127.0.0.1/tcp/4002/p2p/{}", relay.peer_id);
        let relay = relay_candidate_endpoint_arg(&relay_address).expect("relay endpoint arg");
        let relay_id = relay.id.clone();
        let relay_direct_address = relay.address.clone();

        init_config(public_relay_config_args(output.clone(), relay, true)).expect("init config");

        let config = Config::load(&output).expect("load generated config");
        let _ = std::fs::remove_file(&output);

        config.validate_runtime().expect("runtime-valid config");
        assert_eq!(config.network.relay.reservations.len(), 1);
        assert_eq!(
            config.network.relay.reservations[0],
            format!("{relay_address}/p2p-circuit")
        );
        assert_eq!(
            config.network.bootstrap_peers.len(),
            PUBLIC_IPFS_BOOTSTRAP_PEERS.len() + 1
        );
        assert!(config.network.bootstrap_peers.iter().any(|peer| {
            peer.id == relay_id && Some(peer.address.as_str()) == relay_direct_address.as_deref()
        }));
        assert_eq!(config.peers.len(), 0);
        assert!(config.network.discovery.mdns);
        assert!(config.network.discovery.kademlia);
        assert!(config.network.discovery.kademlia_provider_advertisement);
        assert_eq!(
            config.network.discovery.kademlia_protocol,
            PUBLIC_IPFS_KADEMLIA_PROTOCOL
        );
        assert!(config.network.discovery.dcutr);
        assert!(config.network.discovery.autonat);
    }

    #[test]
    fn public_relay_two_host_configs_are_reciprocal_runtime_configs() {
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let relay_address = format!("/ip4/127.0.0.1/tcp/4002/p2p/{}", relay.peer_id);
        let relay = relay_candidate_endpoint_arg(&relay_address).expect("relay endpoint arg");
        let args = RelayCheckArgs {
            two_host_network: "public-lab".to_owned(),
            host_a_interface: "hs-a".to_owned(),
            host_b_interface: "hs-b".to_owned(),
            host_a_route: "10.44.0.1/32".to_owned(),
            host_b_route: "10.44.0.2/32".to_owned(),
            two_host_mtu: 1420,
            ..relay_check_args_for_test()
        };

        let (host_a, host_b) =
            public_relay_two_host_configs(&args, &relay).expect("two-host configs");

        host_a.validate_runtime().expect("Host A runtime config");
        host_b.validate_runtime().expect("Host B runtime config");
        assert_eq!(host_a.network.name, "public-lab");
        assert_eq!(host_b.network.name, "public-lab");
        assert_eq!(host_a.interface.name, "hs-a");
        assert_eq!(host_b.interface.name, "hs-b");
        assert_eq!(host_a.interface.mtu, 1420);
        assert_eq!(host_b.interface.mtu, 1420);
        assert_eq!(host_a.network.listen_addresses, default_listen_addresses());
        assert_eq!(host_b.network.listen_addresses, default_listen_addresses());
        assert_eq!(host_a.network.discovery, DiscoveryConfig::default());
        assert_eq!(host_b.network.discovery, DiscoveryConfig::default());
        for config in [&host_a, &host_b] {
            assert_eq!(config.network.bootstrap_peers.len(), 1);
            assert_eq!(config.network.bootstrap_peers[0].id, relay.id);
            assert_eq!(config.network.bootstrap_peers[0].address, relay_address);
            assert_eq!(
                config.network.relay.reservations,
                vec![format!("{relay_address}/p2p-circuit")]
            );
        }
        assert!(!host_a.network.relay.server);
        assert!(!host_b.network.relay.server);
        assert_eq!(host_a.network.vpn_ip.as_deref(), Some("10.44.0.1"));
        assert_eq!(host_b.network.vpn_ip.as_deref(), Some("10.44.0.2"));
        assert!(host_a.network.routes.is_empty());
        assert!(host_b.network.routes.is_empty());
        assert_eq!(
            host_a.peers[0].id,
            host_b.local_peer().expect("Host B peer")
        );
        assert_eq!(
            host_b.peers[0].id,
            host_a.local_peer().expect("Host A peer")
        );
        assert_eq!(host_a.peers[0].vpn_ip.as_deref(), Some("10.44.0.2"));
        assert_eq!(host_b.peers[0].vpn_ip.as_deref(), Some("10.44.0.1"));
        assert!(host_a.peers[0].routes.is_empty());
        assert!(host_b.peers[0].routes.is_empty());
        assert!(host_a.peers[0].addresses.is_empty());
        assert!(host_b.peers[0].addresses.is_empty());
        assert_eq!(
            route_ping_target(&args.host_b_route, "Host B").unwrap(),
            "10.44.0.2"
        );
    }

    fn public_relay_probe_report(
        address: String,
    ) -> p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport {
        p2p_vpn::runtime::bootstrap_check::PublicRelayProbeReport {
            mode: PublicRelayProbeMode::RelayedPeerCircuit,
            candidates: vec![
                p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateReport {
                    address,
                    succeeded: true,
                    failure_stage:
                        p2p_vpn::runtime::bootstrap_check::PublicRelayCandidateFailureStage::None,
                    error: None,
                    bootstrap: None,
                    elapsed_millis: 1_250,
                },
            ],
        }
    }

    #[test]
    fn public_relay_config_from_base_preserves_existing_overlay_config() {
        let base_output = temp_config_path("p2p-vpn-public-relay-base-config");
        let output = temp_config_path("p2p-vpn-public-relay-updated-config");
        let peer = NodeIdentity::generate_ed25519().expect("peer identity");
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let relay_address = format!("/ip4/127.0.0.1/tcp/4002/p2p/{}", relay.peer_id);

        init_config(InitConfigArgs {
            output: base_output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: false,
            public_ipfs_profile: false,
            peers: vec![EndpointArg {
                id: peer.peer_id.clone(),
                address: None,
            }],
            vpn_ip: None,
            local_routes: vec![LocalRouteArg {
                route: RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 90,
                },
            }],
            peer_vpn_ips: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect("init base config");
        let base = Config::load(&base_output).expect("load base config");
        let report = public_relay_probe_report(relay_address.clone());

        write_public_relay_config_from_base(&report, base.clone(), &output, true)
            .expect("write updated config");

        let updated = Config::load(&output).expect("load updated config");
        let _ = std::fs::remove_file(&base_output);
        let _ = std::fs::remove_file(&output);

        updated.validate_runtime().expect("runtime-valid config");
        assert_eq!(updated.network.local_peer, base.network.local_peer);
        assert_eq!(updated.network.routes, base.network.routes);
        assert_eq!(updated.peers, base.peers);
        assert_eq!(
            updated.network.relay.reservations,
            vec![format!("{relay_address}/p2p-circuit")]
        );
        assert_eq!(
            updated.network.bootstrap_peers,
            vec![BootstrapPeerConfig {
                id: relay.peer_id,
                address: relay_address,
            }]
        );
    }

    #[test]
    fn relay_check_write_config_can_preserve_base_overlay_config() {
        let base_output = temp_config_path("p2p-vpn-relay-check-base-config");
        let output = temp_config_path("p2p-vpn-relay-check-updated-config");
        let peer = NodeIdentity::generate_ed25519().expect("peer identity");
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let relay_address = format!("/ip4/127.0.0.1/tcp/4002/p2p/{}", relay.peer_id);

        init_config(InitConfigArgs {
            output: base_output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: false,
            public_ipfs_profile: false,
            peers: vec![EndpointArg {
                id: peer.peer_id.clone(),
                address: None,
            }],
            vpn_ip: None,
            local_routes: vec![LocalRouteArg {
                route: RouteConfig {
                    prefix: "10.43.0.0/24".to_owned(),
                    metric: 80,
                },
            }],
            peer_vpn_ips: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect("init base config");
        let base = Config::load(&base_output).expect("load base config");
        let report = public_relay_probe_report(relay_address.clone());
        let args = RelayCheckArgs {
            config_path: Some(base_output.clone()),
            relay_candidates: vec![relay_address.clone()],
            write_config: Some(output.clone()),
            force: true,
            ..relay_check_args_for_test()
        };

        write_public_relay_config_from_relay_check(&args, &report, &output)
            .expect("write updated config");

        let updated = Config::load(&output).expect("load updated config");
        let _ = std::fs::remove_file(&base_output);
        let _ = std::fs::remove_file(&output);

        updated.validate_runtime().expect("runtime-valid config");
        assert_eq!(updated.network.local_peer, base.network.local_peer);
        assert_eq!(updated.network.routes, base.network.routes);
        assert_eq!(updated.peers, base.peers);
        assert_eq!(
            updated.network.relay.reservations,
            vec![format!("{relay_address}/p2p-circuit")]
        );
        assert_eq!(
            updated.network.bootstrap_peers,
            vec![BootstrapPeerConfig {
                id: relay.peer_id,
                address: relay_address,
            }]
        );
    }

    #[test]
    fn public_relay_infrastructure_insertion_is_idempotent() {
        let output = temp_config_path("p2p-vpn-public-relay-idempotent-config");
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let relay_address = format!("/ip4/127.0.0.1/tcp/4002/p2p/{}", relay.peer_id);
        let relay_arg = relay_candidate_endpoint_arg(&relay_address).expect("relay endpoint arg");

        init_config(public_relay_config_args(
            output.clone(),
            relay_arg.clone(),
            true,
        ))
        .expect("init config");
        let mut config = Config::load(&output).expect("load generated config");
        let _ = std::fs::remove_file(&output);

        add_public_relay_infrastructure(&mut config, &relay_arg).expect("add relay");

        assert_eq!(
            config.network.bootstrap_peers.len(),
            PUBLIC_IPFS_BOOTSTRAP_PEERS.len() + 1
        );
        assert_eq!(
            config
                .network
                .bootstrap_peers
                .iter()
                .filter(|peer| peer.id == relay.peer_id && peer.address == relay_address)
                .count(),
            1
        );
        assert_eq!(config.network.relay.reservations.len(), 1);
    }

    #[test]
    fn init_config_writes_runtime_valid_relay_peer_shortcut() {
        let output = temp_config_path("p2p-vpn-relay-peer-config");
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");

        init_config(InitConfigArgs {
            output: output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: vec![EndpointArg {
                id: relay.peer_id.clone(),
                address: Some("/ip4/127.0.0.1/tcp/4002".to_owned()),
            }],
            ipfs_bootstrap_peers: false,
            public_ipfs_profile: false,
            peers: Vec::new(),
            vpn_ip: None,
            peer_vpn_ips: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect("init config");

        let config = Config::load(&output).expect("load generated config");
        let _ = std::fs::remove_file(&output);

        config.validate_runtime().expect("runtime-valid config");
        assert_eq!(config.peers.len(), 0);
        assert_eq!(config.network.bootstrap_peers.len(), 1);
        assert_eq!(config.network.bootstrap_peers[0].id, relay.peer_id);
        assert_eq!(
            config.network.relay.reservations,
            vec![format!(
                "/ip4/127.0.0.1/tcp/4002/p2p/{}/p2p-circuit",
                relay.peer_id
            )]
        );
    }

    #[test]
    fn init_config_writes_previous_membership_tags() {
        let output = temp_config_path("p2p-vpn-init-config-previous-membership-tags");
        let membership_key = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=".to_owned();
        let previous_tag = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=".to_owned();

        init_config(InitConfigArgs {
            output: output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: Some(membership_key),
            previous_membership_tags: vec![previous_tag.clone()],
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: false,
            public_ipfs_profile: false,
            peers: Vec::new(),
            vpn_ip: None,
            peer_vpn_ips: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect("init config");

        let config = Config::load(&output).expect("load generated config");
        let _ = std::fs::remove_file(&output);

        config.validate_runtime().expect("runtime-valid config");
        assert_eq!(config.network.previous_membership_tags, vec![previous_tag]);
    }

    #[test]
    fn init_config_writes_vpn_ip_shortcuts() {
        let output = temp_config_path("p2p-vpn-init-config-vpn-ip");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");

        init_config(InitConfigArgs {
            output: output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "pv0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: false,
            public_ipfs_profile: false,
            peers: vec![EndpointArg {
                id: remote.peer_id.clone(),
                address: None,
            }],
            vpn_ip: Some("10.44.0.1".to_owned()),
            peer_vpn_ips: vec![PeerVpnIpArg {
                id: remote.peer_id,
                vpn_ip: "10.44.0.2".to_owned(),
            }],
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect("init config");

        let config = Config::load(&output).expect("load generated config");
        let _ = std::fs::remove_file(&output);

        config.validate_runtime().expect("runtime-valid config");
        assert_eq!(config.network.vpn_ip.as_deref(), Some("10.44.0.1"));
        assert!(config.network.routes.is_empty());
        assert_eq!(config.peers[0].vpn_ip.as_deref(), Some("10.44.0.2"));
        assert!(config.peers[0].routes.is_empty());
    }

    #[test]
    fn init_config_writes_compact_default_peer_config() {
        let output = temp_config_path("p2p-vpn-init-config-minimal-peer");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");

        init_config(InitConfigArgs {
            output: output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "pv0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: false,
            public_ipfs_profile: false,
            peers: vec![EndpointArg {
                id: remote.peer_id.clone(),
                address: None,
            }],
            vpn_ip: None,
            peer_vpn_ips: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
            force: true,
        })
        .expect("init config");

        let rendered = std::fs::read_to_string(&output).expect("generated config");
        let _ = std::fs::remove_file(&output);
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("json");

        assert!(value.get("interface").is_none(), "{rendered}");
        assert!(value.get("queue").is_none(), "{rendered}");
        assert!(value.get("resources").is_none(), "{rendered}");
        assert_eq!(
            value["peers"],
            serde_json::json!([{ "id": remote.peer_id }])
        );

        let network = value["network"].as_object().expect("network object");
        assert_eq!(
            network.get("name").and_then(|name| name.as_str()),
            Some("lab")
        );
        assert!(network.contains_key("private_key"));
        assert_eq!(network.len(), 2, "{rendered}");
    }

    #[test]
    fn init_config_writes_custom_queue_and_resource_limits() {
        let output = temp_config_path("p2p-vpn-init-config");

        init_config(InitConfigArgs {
            output: output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            previous_membership_tags: Vec::new(),
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            relay_peers: Vec::new(),
            ipfs_bootstrap_peers: false,
            public_ipfs_profile: false,
            peers: Vec::new(),
            vpn_ip: None,
            peer_vpn_ips: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig {
                server: true,
                reservations: Vec::new(),
                auto: AutoRelayConfig::default(),
                resources: RelayResourceConfig {
                    max_reservations: 17,
                    max_reservations_per_peer: 3,
                    reservation_duration_secs: 45,
                    max_circuits: 19,
                    max_circuits_per_peer: 5,
                    max_circuit_duration_secs: 23,
                    max_circuit_bytes: 4096,
                },
            },
            packet_plane: PacketPlaneConfig::default(),
            queue: QueueConfig {
                max_packets_per_peer: 12,
                max_bytes_per_peer: 8192,
                max_packet_age_millis: 250,
            },
            resources: ResourceConfig {
                max_concurrent_control_streams: 11,
                max_concurrent_packet_streams: 22,
                max_inbound_packets_per_peer_per_second: 333,
                max_pairing_requests_per_peer_per_second: 5,
                max_pending_incoming_connections: 33,
                max_pending_outgoing_connections: 44,
                max_established_incoming_connections: 55,
                max_established_outgoing_connections: 66,
                max_established_connections_per_peer: 7,
                max_established_connections: 88,
            },
            force: true,
        })
        .expect("init config");

        let config = Config::load(&output).expect("load generated config");
        let _ = std::fs::remove_file(&output);

        assert_eq!(
            config.network.relay.resources,
            RelayResourceConfig {
                max_reservations: 17,
                max_reservations_per_peer: 3,
                reservation_duration_secs: 45,
                max_circuits: 19,
                max_circuits_per_peer: 5,
                max_circuit_duration_secs: 23,
                max_circuit_bytes: 4096,
            }
        );
        assert_eq!(
            config.queue,
            QueueConfig {
                max_packets_per_peer: 12,
                max_bytes_per_peer: 8192,
                max_packet_age_millis: 250,
            }
        );
        assert_eq!(
            config.resources,
            ResourceConfig {
                max_concurrent_control_streams: 11,
                max_concurrent_packet_streams: 22,
                max_inbound_packets_per_peer_per_second: 333,
                max_pairing_requests_per_peer_per_second: 5,
                max_pending_incoming_connections: 33,
                max_pending_outgoing_connections: 44,
                max_established_incoming_connections: 55,
                max_established_outgoing_connections: 66,
                max_established_connections_per_peer: 7,
                max_established_connections: 88,
            }
        );
    }

    fn temp_config_path(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ))
    }
}
