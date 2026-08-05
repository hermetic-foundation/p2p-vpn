use std::{
    fs,
    net::{IpAddr, UdpSocket},
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};

use clap::{Parser, Subcommand, ValueEnum};
use p2p_vpn::{
    OVERLAY_FRAGMENTATION_POLICY_LINE, PathKind,
    config::{
        BootstrapPeerConfig, Config, DiscoveryConfig, InitConfigTemplate, InitPeer,
        PacketPlaneConfig, QueueConfig, RelayConfig, RelayResourceConfig, ResourceConfig,
        RouteConfig, RuntimeDefaults, default_packet_plane_replay_windows_per_session,
        default_packet_plane_session_ttl_seconds,
    },
    identity::NodeIdentity,
    invite::{
        InviteExportOptions, InviteImportOptions, SignedInvite, export_signed_invite,
        import_invite_config,
    },
    metrics::{RuntimeMetrics, prometheus_lines_from_metric_lines},
    queue::QueueStats,
    runtime::{
        bootstrap_check::{
            BootstrapCheckRequirements, BootstrapCheckThreshold, PUBLIC_RELAY_CANDIDATE_LIMIT,
            PublicDcutrListenerDescriptor, PublicRelayProbeMode, check_config_bootstrap,
            check_public_dcutr_descriptor, check_public_relay_candidates,
            parse_public_relay_addresses, parse_public_relay_addresses_with_limit,
            scan_public_relay_candidates, start_public_dcutr_listener,
        },
        forward::session_id_for_peer,
        packet_plane::{PACKET_PLANE_DATAGRAM_OVERHEAD_LEN, PACKET_PLANE_MAX_PAYLOAD_LEN},
        remote::{RemotePeerStatus, query_peer_status},
        runner::{self, ShutdownReason},
        service::SERVICE_PROTOCOL,
        tun::{TunAddresses, TunDevice, TunRuntimeConfig, route_advmss},
    },
    wire::{HEADER_LEN, MAX_PAYLOAD_LEN, WIRE_VERSION},
};
use serde::Serialize;

const PRIVATE_KADEMLIA_PROTOCOL: &str = "/p2p-vpn/kad/1";
const IPFS_KADEMLIA_PROTOCOL: &str = "/ipfs/kad/1.0.0";
const RELAY_CHECK_CAPPED_INPUT_LIMIT: usize = 256;
const IPFS_BOOTSTRAP_PEERS: &[(&str, &str)] = &[
    (
        "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    ),
    (
        "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    ),
    (
        "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    ),
    (
        "QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
    ),
    (
        "QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
        "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
    ),
];

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Command {
    Keygen,
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
        #[arg(long, default_value = "hs0")]
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
        #[arg(long = "peer")]
        peers: Vec<EndpointArg>,
        #[arg(long = "local-route")]
        local_routes: Vec<LocalRouteArg>,
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
        #[arg(long, default_value_t = 256)]
        queue_max_packets_per_peer: usize,
        #[arg(long, default_value_t = 524_288)]
        queue_max_bytes_per_peer: usize,
        #[arg(long, default_value_t = 1_000)]
        queue_max_packet_age_millis: u64,
        #[arg(long, default_value_t = 64)]
        max_concurrent_control_streams: usize,
        #[arg(long, default_value_t = 256)]
        max_concurrent_packet_streams: usize,
        #[arg(long, default_value_t = 4096)]
        max_inbound_packets_per_peer_per_second: u32,
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
        #[arg(long, default_value = PRIVATE_KADEMLIA_PROTOCOL)]
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
        #[arg(long, default_value = "hs0")]
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
    },
    DaemonPeers {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
    },
    DaemonRoutes {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
    },
    DaemonPaths {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
    },
    DaemonMtu {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
    },
    DaemonCapabilities {
        #[arg(long, default_value = "/run/p2p-vpn/control.sock")]
        socket: PathBuf,
        #[arg(long, default_value_t = 5)]
        timeout_seconds: u64,
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
    },
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> Result<(), String> {
    let cli = Cli::parse();

    match cli.command {
        Command::Keygen => keygen(),
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
            peers,
            local_routes,
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
            queue_max_packets_per_peer,
            queue_max_bytes_per_peer,
            queue_max_packet_age_millis,
            max_concurrent_control_streams,
            max_concurrent_packet_streams,
            max_inbound_packets_per_peer_per_second,
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
            ipfs_bootstrap_peers,
            peers,
            local_routes,
            peer_routes,
            discovery: InitDiscoveryFlags {
                disable_mdns,
                disable_kademlia,
                disable_kademlia_provider_advertisement,
                disable_dcutr,
                disable_autonat,
            }
            .into_config(kademlia_protocol, ipfs_kademlia),
            relay: RelayConfig {
                server: relay_server,
                reservations: relay_reservations,
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
            require_dcutr_success,
            timeout_seconds,
            max_validation_candidates,
            write_report,
            write_config,
            force,
        } => {
            let mode = if require_dcutr_success {
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
                force,
            }))
            .await
        }
        Command::RelayDcutrListen {
            relay_candidate,
            write_descriptor,
            reservation_timeout_seconds,
            serve_seconds,
            force,
        } => {
            Box::pin(relay_dcutr_listen(RelayDcutrListenArgs {
                relay_candidate,
                write_descriptor,
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
        Command::DaemonStatus {
            socket,
            timeout_seconds,
            format,
        } => Box::pin(daemon_status(&socket, timeout_seconds, format)).await,
        Command::DaemonState {
            socket,
            timeout_seconds,
        } => Box::pin(daemon_state(&socket, timeout_seconds)).await,
        Command::DaemonPeers {
            socket,
            timeout_seconds,
        } => Box::pin(daemon_peers(&socket, timeout_seconds)).await,
        Command::DaemonRoutes {
            socket,
            timeout_seconds,
        } => Box::pin(daemon_routes(&socket, timeout_seconds)).await,
        Command::DaemonPaths {
            socket,
            timeout_seconds,
        } => Box::pin(daemon_paths(&socket, timeout_seconds)).await,
        Command::DaemonMtu {
            socket,
            timeout_seconds,
        } => Box::pin(daemon_mtu(&socket, timeout_seconds)).await,
        Command::DaemonCapabilities {
            socket,
            timeout_seconds,
        } => Box::pin(daemon_capabilities(&socket, timeout_seconds)).await,
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
        } => {
            Box::pin(up(
                &config,
                dry_run,
                metrics_interval_seconds,
                control_socket,
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
    fn into_config(self, kademlia_protocol: String, ipfs_kademlia: bool) -> DiscoveryConfig {
        DiscoveryConfig {
            mdns: !self.disable_mdns,
            kademlia: !self.disable_kademlia,
            kademlia_provider_advertisement: !self.disable_kademlia
                && !self.disable_kademlia_provider_advertisement,
            kademlia_protocol: selected_kademlia_protocol(kademlia_protocol, ipfs_kademlia),
            dcutr: !self.disable_dcutr,
            autonat: !self.disable_autonat,
        }
    }
}

fn selected_kademlia_protocol(kademlia_protocol: String, ipfs_kademlia: bool) -> String {
    if ipfs_kademlia {
        IPFS_KADEMLIA_PROTOCOL.to_owned()
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
    peers: Vec<EndpointArg>,
    local_routes: Vec<LocalRouteArg>,
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
struct PeerRouteArg {
    id: String,
    route: RouteConfig,
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
#[allow(clippy::struct_excessive_bools)]
struct RelayScanArgs {
    config_path: Option<PathBuf>,
    bootstrap_peers: Vec<EndpointArg>,
    ipfs_bootstrap_peers: bool,
    timeout_seconds: u64,
    max_candidates: usize,
    check_candidates: bool,
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

fn keygen() -> Result<(), String> {
    let identity = NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate key: {error:?}"))?;

    println!("peer_id: {}", identity.peer_id);
    println!("private_key: {}", identity.private_key);
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
    if args.ipfs_bootstrap_peers {
        if !args.discovery.kademlia {
            return Err("--ipfs-bootstrap-peers requires Kademlia discovery".to_owned());
        }
        if args.discovery.kademlia_protocol != IPFS_KADEMLIA_PROTOCOL {
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
    let mut config = InitConfigTemplate {
        identity,
        network_name: args.network,
        membership_key: args.membership_key,
        local_routes: args
            .local_routes
            .into_iter()
            .map(|route| route.route)
            .collect(),
        interface_name: args.interface,
        mtu: args.mtu,
        listen_addresses: args.listen_addresses,
        external_addresses: args.external_addresses,
        packet_plane: args.packet_plane,
        bootstrap_peers,
        peers: init_peers(args.peers, args.peer_routes),
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
    let rendered = serde_json::to_string_pretty(&config)
        .map_err(|error| format!("failed to render config: {error}"))?;

    if args.output.to_string_lossy() == "-" {
        println!("{rendered}");
    } else {
        fs::write(&args.output, format!("{rendered}\n"))
            .map_err(|error| format!("failed to write {}: {error}", args.output.display()))?;
        println!("wrote {}", args.output.display());
        println!("local peer: {}", config.network.local_peer);
    }

    Ok(())
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
        println!("local peer: {}", config.network.local_peer);
        println!("invited by: {}", invite.payload.inviter_peer);
    }

    Ok(())
}

impl From<EndpointArg> for InitPeer {
    fn from(value: EndpointArg) -> Self {
        Self {
            id: value.id,
            address: value.address,
            routes: Vec::new(),
        }
    }
}

fn init_peers(addresses: Vec<EndpointArg>, routes: Vec<PeerRouteArg>) -> Vec<InitPeer> {
    addresses
        .into_iter()
        .map(EndpointArg::into)
        .chain(routes.into_iter().map(|route| InitPeer {
            id: route.id,
            address: None,
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
        for (id, address) in IPFS_BOOTSTRAP_PEERS {
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
    lines.push(format!("local peer: {}", config.network.local_peer));
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
            .peers
            .iter()
            .map(|peer| peer.addresses.len())
            .sum::<usize>()
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
            config.network.local_peer.clone(),
            "-".to_owned(),
            route_source(&config.network.routes, route),
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
        route_source(&peer.routes, route),
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
    if protocol == IPFS_KADEMLIA_PROTOCOL {
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
        let source = route_source(&config.network.routes, route);
        lines.push(format!(
            "local route: {} metric {} {source}",
            route.prefix, route.metric
        ));
    }
    for peer in &config.peers {
        let owner = peer.peer_id().expect("status config is valid");
        for route in routes.routes_for(owner) {
            let source = route_source(&peer.routes, route);
            lines.push(format!(
                "peer route: {} {} metric {} {source}",
                peer.id, route.prefix, route.metric
            ));
        }
    }
}

fn route_source(configured_routes: &[RouteConfig], route: p2p_vpn::route::Route) -> &'static str {
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
            let source = route_source(&peer.routes, route);
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
        PathKind::DirectQuicDatagram | PathKind::DirectQuicStream | PathKind::DirectTcpStream => {
            mtu
        }
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
    let capabilities = p2p_vpn::runtime::control::ControlCapabilities::local(
        &config.network.name,
        config
            .membership_tag()
            .map_err(|error| format!("failed to compute membership tag: {error:?}"))?,
        config.effective_packet_mtu(),
    )
    .with_packet_endpoint_candidates(
        config
            .packet_plane_endpoint_candidates()
            .map_err(|error| format!("failed to parse packet endpoints: {error:?}"))?,
    )
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
    );
    let mut lines = vec![
        format!("local peer: {}", config.network.local_peer),
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
        .map_err(|error| format!("failed to parse --relay-candidate: {error}"))?;
    let listener = start_public_dcutr_listener(
        &relay_candidate,
        Duration::from_secs(args.reservation_timeout_seconds.max(1)),
    )
    .await?;
    write_public_dcutr_listener_descriptor(
        listener.descriptor(),
        &args.write_descriptor,
        args.force,
    )?;

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
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct BootstrapRequirementsJson {
    relay_reservations: bool,
    autonat_status: bool,
    dcutr_ready: bool,
    dcutr_success: bool,
    relayed_peer_circuits: bool,
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

fn bootstrap_check_report_file_json<'a>(
    args: &BootstrapCheckArgs,
    report: &'a p2p_vpn::runtime::bootstrap_check::BootstrapCheckReport,
) -> BootstrapCheckReportFileJson<'a> {
    BootstrapCheckReportFileJson {
        schema_version: 1,
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
        schema_version: 3,
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
    }
}

const fn public_relay_probe_mode_name(mode: PublicRelayProbeMode) -> &'static str {
    match mode {
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
        interface: "hs0".to_owned(),
        mtu: 1280,
        listen_addresses: Vec::new(),
        external_addresses: Vec::new(),
        packet_plane: PacketPlaneConfig::default(),
        bootstrap_peers: Vec::new(),
        relay_peers: vec![relay],
        ipfs_bootstrap_peers: false,
        peers: Vec::new(),
        local_routes: Vec::new(),
        peer_routes: Vec::new(),
        discovery: DiscoveryConfig::default(),
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
    if !force && output.to_string_lossy() != "-" && output.exists() {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            output.display()
        ));
    }
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
        println!("local peer: {}", config.network.local_peer);
    }

    Ok(())
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
    let mode = if args.require_dcutr_success {
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
                    local_routes: Vec::new(),
                    interface_name: "hs0".to_owned(),
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
        IPFS_KADEMLIA_PROTOCOL.clone_into(&mut config.network.discovery.kademlia_protocol);
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

async fn daemon_state(socket: &Path, timeout_seconds: u64) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_state(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon state query failed: {error:?}"))?;

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

async fn daemon_peers(socket: &Path, timeout_seconds: u64) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_peers(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon peers query failed: {error:?}"))?;

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

async fn daemon_routes(socket: &Path, timeout_seconds: u64) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_routes(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon routes query failed: {error:?}"))?;

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

async fn daemon_paths(socket: &Path, timeout_seconds: u64) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_paths(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon paths query failed: {error:?}"))?;

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

async fn daemon_mtu(socket: &Path, timeout_seconds: u64) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_mtu(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon mtu query failed: {error:?}"))?;

    for line in lines {
        println!("{line}");
    }

    Ok(())
}

async fn daemon_capabilities(socket: &Path, timeout_seconds: u64) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_capabilities(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon capabilities query failed: {error:?}"))?;

    for line in lines {
        println!("{line}");
    }

    Ok(())
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
    print_daemon_health_verdict(&verdict);
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

fn print_daemon_health_verdict(verdict: &DaemonHealthVerdict) {
    println!("daemon_health_ready {}", verdict.ready);
    for check in &verdict.checks {
        println!(
            "daemon_health_check {} {} {}",
            check.name,
            if check.ok { "ok" } else { "failed" },
            check.detail
        );
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

    let commands = runtime.route_commands();
    if dry_run {
        println!("dry-run: would create Linux TUN interface and run:");
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

    for command in commands {
        let status = command
            .execute()
            .map_err(|error| format!("failed to execute `{command}`: {error}"))?;
        if !status.success() {
            return Err(format!("`{command}` exited with {status}"));
        }
    }

    println!("starting libp2p packet forwarding runtime");
    let metrics_interval = metrics_interval_seconds.map(Duration::from_secs);
    Box::pin(runner::run_config_until(
        config,
        device,
        metrics_interval,
        control_socket,
        shutdown_signal(),
    ))
    .await
    .map_err(|error| format!("runtime failed: {error:?}"))
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
        PathKind::DirectQuicDatagram => "direct QUIC datagram",
        PathKind::DirectQuicStream => "direct QUIC stream",
        PathKind::DirectTcpStream => "direct TCP stream",
        PathKind::CircuitRelay => "circuit relay",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    routes: Vec::new(),
                },
                InitPeer {
                    id: "peer-a".to_owned(),
                    address: None,
                    routes: vec![RouteConfig {
                        prefix: "10.42.0.0/24".to_owned(),
                        metric: 100,
                    }],
                },
                InitPeer {
                    id: "peer-a".to_owned(),
                    address: None,
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
                .any(|line| line == "kademlia scope: private overlay")
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

        assert!(lines.iter().any(|line| line == "compiled routes: 6"));
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
            capabilities,
            service: p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200)
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
                .any(|line| line == "preferred path: direct QUIC stream")
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
            capabilities,
            service: p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200)
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
        assert!(lines.iter().any(|line| line
            == &format!("peer live preferred path: {peer} direct QUIC stream")));
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
                "peer path candidate: {} direct TCP stream score 60 estimated_mtu 1280 address /ip4/127.0.0.1/tcp/4001",
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
                .any(|line| line == "local capability preferred path: direct QUIC stream")
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
    fn cli_parses_peer_route_arguments() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "init-config",
            "--kademlia-protocol",
            "/ipfs/kad/1.0.0",
            "--disable-kademlia-provider-advertisement",
            "--membership-key",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "--local-route",
            "10.41.0.0/24,75",
            "--peer",
            "12D3KooWPeer=/ip4/127.0.0.1/tcp/4001",
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
            "--max-established-connections",
            "88",
        ])
        .expect("cli");

        let Command::InitConfig {
            peers,
            ipfs_bootstrap_peers,
            local_routes,
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
        assert_eq!(max_established_connections, 88);
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
        } = cli.command
        else {
            panic!("expected daemon-state command");
        };

        assert_eq!(socket, PathBuf::from("/run/p2p-vpn-node-a/control.sock"));
        assert_eq!(timeout_seconds, 3);
    }

    #[test]
    fn cli_parses_daemon_view_commands() {
        for command in [
            "daemon-peers",
            "daemon-routes",
            "daemon-paths",
            "daemon-mtu",
            "daemon-capabilities",
        ] {
            let cli = Cli::try_parse_from([
                "p2p-vpn",
                command,
                "--socket",
                "/run/p2p-vpn-node-a/control.sock",
                "--timeout-seconds",
                "3",
            ])
            .expect("cli");

            let (socket, timeout_seconds) = match cli.command {
                Command::DaemonPeers {
                    socket,
                    timeout_seconds,
                }
                | Command::DaemonRoutes {
                    socket,
                    timeout_seconds,
                }
                | Command::DaemonPaths {
                    socket,
                    timeout_seconds,
                }
                | Command::DaemonMtu {
                    socket,
                    timeout_seconds,
                }
                | Command::DaemonCapabilities {
                    socket,
                    timeout_seconds,
                } => (socket, timeout_seconds),
                other => panic!("expected daemon view command, got {other:?}"),
            };

            assert_eq!(socket, PathBuf::from("/run/p2p-vpn-node-a/control.sock"));
            assert_eq!(timeout_seconds, 3);
        }
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
            },
            write_report: Some(output.clone()),
            force: false,
        };

        write_bootstrap_check_report(&args, &report, &output).expect("write report");
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&output).expect("report file")).expect("json report");

        assert_eq!(value["schema_version"], 1);
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
            "--force",
        ])
        .expect("cli");

        let Command::RelayCheck {
            config,
            relay_candidates,
            relay_candidates_file,
            require_dcutr_success,
            timeout_seconds,
            max_validation_candidates,
            write_report,
            write_config,
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
        assert!(require_dcutr_success);
        assert_eq!(timeout_seconds, 60);
        assert_eq!(max_validation_candidates, Some(3));
        assert_eq!(write_report, Some(PathBuf::from("relay-report.json")));
        assert_eq!(write_config, Some(PathBuf::from("relay-config.json")));
        assert!(force);
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
            reservation_timeout_seconds,
            serve_seconds,
            force,
        } = cli.command
        else {
            panic!("expected relay-dcutr-listen command");
        };

        assert_eq!(relay_candidate, relay);
        assert_eq!(write_descriptor, PathBuf::from("listener.json"));
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
            config_path: None,
            relay_candidates: vec![inline.clone()],
            relay_candidates_file: Some(output.clone()),
            timeout_seconds: 45,
            mode: PublicRelayProbeMode::RelayedPeerCircuit,
            max_validation_candidates: None,
            write_report: None,
            write_config: None,
            force: false,
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
            config_path: None,
            relay_candidates: vec![candidate.clone()],
            relay_candidates_file: None,
            timeout_seconds: 45,
            mode: PublicRelayProbeMode::DcutrSuccess,
            max_validation_candidates: Some(1),
            write_report: Some(output.clone()),
            write_config: None,
            force: false,
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

        assert_eq!(value["schema_version"], 3);
        assert_eq!(value["mode"], "dcutr_success");
        assert_eq!(value["succeeded"], false);
        assert_eq!(value["timeout_seconds"], 45);
        assert_eq!(value["max_validation_candidates"], 1);
        assert_eq!(value["host_reachable_candidates"][0], candidate);
        assert_eq!(value["skipped_candidates"][0]["reason"], "ipv4_unreachable");
        assert_eq!(value["candidates"][0]["failure_stage"], "dcutr_success");
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
        p2p_vpn::runtime::bootstrap_check::BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: true,
                dcutr_ready: true,
                dcutr_success: true,
                relayed_peer_circuits: true,
            },
            kademlia_protocol: IPFS_KADEMLIA_PROTOCOL.to_owned(),
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
            peer_results: Vec::new(),
            relay_results: Vec::new(),
            relayed_peer_results: Vec::new(),
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
        assert_eq!(bootstrap["kademlia_protocol"], IPFS_KADEMLIA_PROTOCOL);
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
            config_path: None,
            relay_candidates: vec![
                "/dns4/relay.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"
                    .to_owned(),
            ],
            relay_candidates_file: None,
            timeout_seconds: 45,
            mode: PublicRelayProbeMode::RelayedPeerCircuit,
            max_validation_candidates: Some(0),
            write_report: None,
            write_config: None,
            force: false,
        };

        assert_eq!(
            validate_relay_check_args(&args).expect_err("validation should fail"),
            "--max-validation-candidates must be greater than zero"
        );
    }

    #[test]
    fn relay_check_requires_candidate_or_candidate_file() {
        let args = RelayCheckArgs {
            config_path: None,
            relay_candidates: Vec::new(),
            relay_candidates_file: None,
            timeout_seconds: 45,
            mode: PublicRelayProbeMode::RelayedPeerCircuit,
            max_validation_candidates: None,
            write_report: None,
            write_config: None,
            force: false,
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
            IPFS_BOOTSTRAP_PEERS.len()
        );
        assert!(!config.network.discovery.mdns);
        assert!(config.network.discovery.kademlia);
        assert_eq!(
            config.network.discovery.kademlia_protocol,
            IPFS_KADEMLIA_PROTOCOL
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
    fn cli_parses_up_control_socket() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "up",
            "--config",
            "node-a.json",
            "--control-socket",
            "/run/p2p-vpn-node-a/control.sock",
        ])
        .expect("cli");

        let Command::Up {
            config,
            control_socket,
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
            IPFS_KADEMLIA_PROTOCOL
        );
        assert_eq!(
            kademlia_scope(IPFS_KADEMLIA_PROTOCOL),
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
        .into_config(PRIVATE_KADEMLIA_PROTOCOL.to_owned(), false);

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
            peers: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
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
                id: IPFS_BOOTSTRAP_PEERS[0].0.to_owned(),
                address: Some(IPFS_BOOTSTRAP_PEERS[0].1.to_owned()),
            }],
            Vec::new(),
            true,
        );

        assert_eq!(peers.len(), IPFS_BOOTSTRAP_PEERS.len());
        for (id, address) in IPFS_BOOTSTRAP_PEERS {
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
            peers: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: InitDiscoveryFlags {
                disable_mdns: false,
                disable_kademlia: false,
                disable_kademlia_provider_advertisement: false,
                disable_dcutr: false,
                disable_autonat: false,
            }
            .into_config(PRIVATE_KADEMLIA_PROTOCOL.to_owned(), true),
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
            IPFS_KADEMLIA_PROTOCOL
        );
        assert_eq!(
            config.network.bootstrap_peers.len(),
            IPFS_BOOTSTRAP_PEERS.len()
        );
        assert_eq!(
            config
                .bootstrap_multiaddrs()
                .expect("bootstrap multiaddrs")
                .len(),
            IPFS_BOOTSTRAP_PEERS.len()
        );
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

        init_config(public_relay_config_args(output.clone(), relay, true)).expect("init config");

        let config = Config::load(&output).expect("load generated config");
        let _ = std::fs::remove_file(&output);

        config.validate_runtime().expect("runtime-valid config");
        assert_eq!(config.network.relay.reservations.len(), 1);
        assert_eq!(
            config.network.relay.reservations[0],
            format!("{relay_address}/p2p-circuit")
        );
        assert_eq!(config.network.bootstrap_peers.len(), 1);
        assert_eq!(config.peers.len(), 0);
        assert!(config.network.discovery.dcutr);
        assert!(config.network.discovery.autonat);
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
            peers: vec![EndpointArg {
                id: peer.peer_id.clone(),
                address: None,
            }],
            local_routes: vec![LocalRouteArg {
                route: RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 90,
                },
            }],
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
            peers: vec![EndpointArg {
                id: peer.peer_id.clone(),
                address: None,
            }],
            local_routes: vec![LocalRouteArg {
                route: RouteConfig {
                    prefix: "10.43.0.0/24".to_owned(),
                    metric: 80,
                },
            }],
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
            relay_candidates_file: None,
            timeout_seconds: 45,
            mode: PublicRelayProbeMode::RelayedPeerCircuit,
            max_validation_candidates: None,
            write_report: None,
            write_config: Some(output.clone()),
            force: true,
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

        assert_eq!(config.network.bootstrap_peers.len(), 1);
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
            peers: Vec::new(),
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
            peers: Vec::new(),
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
            peers: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig {
                server: true,
                reservations: Vec::new(),
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
