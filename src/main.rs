use std::{
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use clap::{Parser, Subcommand};
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
    metrics::RuntimeMetrics,
    queue::QueueStats,
    runtime::{
        bootstrap_check::{
            BootstrapCheckRequirements, BootstrapCheckThreshold, PublicRelayProbeMode,
            check_config_bootstrap, check_public_relay_candidates, parse_public_relay_addresses,
            scan_public_relay_candidates,
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

const PRIVATE_KADEMLIA_PROTOCOL: &str = "/p2p-vpn/kad/1";
const IPFS_KADEMLIA_PROTOCOL: &str = "/ipfs/kad/1.0.0";
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
    },
    RelayCheck {
        #[arg(long = "relay-candidate", required = true)]
        relay_candidates: Vec<String>,
        #[arg(long)]
        require_dcutr_success: bool,
        #[arg(long, default_value_t = 45)]
        timeout_seconds: u64,
        #[arg(long = "write-config")]
        write_config: Option<PathBuf>,
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
        Command::Metrics { config } => metrics(&config),
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
        } => {
            let threshold = if require_all {
                BootstrapCheckThreshold::All
            } else {
                BootstrapCheckThreshold::Any
            };
            Box::pin(bootstrap_check(
                &config,
                timeout_seconds,
                threshold,
                BootstrapCheckRequirements {
                    relay_reservations: require_relay_reservations,
                    autonat_status: require_autonat_status,
                    dcutr_ready: require_dcutr_ready,
                    dcutr_success: require_dcutr_success,
                    relayed_peer_circuits: require_relayed_peer_circuits,
                },
            ))
            .await
        }
        Command::RelayCheck {
            relay_candidates,
            require_dcutr_success,
            timeout_seconds,
            write_config,
            force,
        } => {
            let mode = if require_dcutr_success {
                PublicRelayProbeMode::DcutrSuccess
            } else {
                PublicRelayProbeMode::RelayedPeerCircuit
            };
            Box::pin(relay_check(RelayCheckArgs {
                relay_candidates,
                timeout_seconds,
                mode,
                write_config,
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
        } => Box::pin(daemon_status(&socket, timeout_seconds)).await,
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
    write_config: Option<PathBuf>,
    force: bool,
}

#[derive(Clone, Debug)]
struct RelayCheckArgs {
    relay_candidates: Vec<String>,
    timeout_seconds: u64,
    mode: PublicRelayProbeMode,
    write_config: Option<PathBuf>,
    force: bool,
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

fn metrics(path: &PathBuf) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    let snapshot = RuntimeMetrics::default().snapshot(QueueStats::default());

    println!("network: {}", config.network.name);
    println!("runtime metrics:");
    for line in snapshot.lines() {
        println!("{line}");
    }
    println!("live output: run `up --metrics-interval-seconds N`");
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
    lines.push(format!("peer live: {} reachable", status.peer));
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
    lines.push(format!(
        "peer live preferred path: {} {}",
        status.peer,
        path_name(
            PathKind::from_wire_name(&status.capabilities.preferred_path)
                .unwrap_or(PathKind::DirectQuicStream)
        )
    ));
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
    let packet_datagram_ready = status.service.supports_owned_udp_packet_plane
        || status.service.supports_owned_quic_packet_plane
        || status.service.supports_quic_datagrams
        || status.service.supports_native_quic_datagrams;
    let path_probe_ready = !preferred_path.requires_quic_datagrams() || packet_datagram_ready;
    let estimated_path_mtu =
        configured_path_mtu_estimate(preferred_path, status.service.effective_mtu);

    lines.push(format!(
        "peer live path: {} reachable preferred {} score {} mtu {} path_mtu_estimate {} quic_datagrams {} native_quic_datagrams {} owned_udp_packet_plane {} owned_quic_packet_plane {} path_probe_ready {}",
        status.peer,
        path_name(preferred_path),
        preferred_path.default_score(),
        status.service.effective_mtu,
        estimated_path_mtu,
        status.service.supports_quic_datagrams,
        status.service.supports_native_quic_datagrams,
        status.service.supports_owned_udp_packet_plane,
        status.service.supports_owned_quic_packet_plane,
        path_probe_ready
    ));
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

async fn bootstrap_check(
    path: &Path,
    timeout_seconds: u64,
    threshold: BootstrapCheckThreshold,
    requirements: BootstrapCheckRequirements,
) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    let report = Box::pin(check_config_bootstrap(
        &config,
        Duration::from_secs(timeout_seconds.max(1)),
        threshold,
        requirements,
    ))
    .await
    .map_err(|error| format!("bootstrap check failed to start: {error:?}"))?;
    let succeeded = report.succeeded();

    for line in report.lines() {
        println!("{line}");
    }

    if succeeded {
        Ok(())
    } else {
        Err("bootstrap check did not meet success threshold".to_owned())
    }
}

async fn relay_check(args: RelayCheckArgs) -> Result<(), String> {
    let raw = args.relay_candidates.join("\n");
    let addresses = parse_public_relay_addresses(&raw)
        .map_err(|error| format!("failed to parse relay candidates: {error}"))?;
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

    if succeeded {
        if let Some(output) = args.write_config {
            write_public_relay_config_from_probe(&report, &output, args.force)?;
        }
        Ok(())
    } else {
        Err("public relay check did not find a usable candidate".to_owned())
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
        args.bootstrap_peers,
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

    if !scan_succeeded {
        return Err("public relay scan did not discover a relay-hop candidate".to_owned());
    }

    if !args.check_candidates {
        return Ok(());
    }

    let candidates = relay_scan_candidate_multiaddrs(&report)?;
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
    if args.write_config.is_some() && !args.check_candidates {
        return Err("--write-config requires --check-candidates".to_owned());
    }
    Ok(())
}

fn relay_scan_candidate_multiaddrs(
    report: &p2p_vpn::runtime::bootstrap_check::PublicRelayScanReport,
) -> Result<Vec<libp2p::Multiaddr>, String> {
    report
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
        .collect()
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

async fn daemon_status(socket: &Path, timeout_seconds: u64) -> Result<(), String> {
    let lines = p2p_vpn::runtime::control_socket::query_status(
        socket,
        Duration::from_secs(timeout_seconds.max(1)),
    )
    .await
    .map_err(|error| format!("daemon status query failed: {error:?}"))?;

    for line in lines {
        println!("{line}");
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
    let mut lines = vec![
        format!("peer: {}", status.peer),
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

fn optional_usize(value: Option<usize>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |count| count.to_string())
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
            .with_advertised_routes(vec![p2p_vpn::runtime::control::ControlRoute::new(
                "10.42.0.0/24",
                100,
            )]);
        let status = RemotePeerStatus {
            peer,
            capabilities,
            service: p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200)
                .with_packet_plane_session_ttl_seconds(321)
                .with_packet_plane_replay_windows_per_session(654),
        };

        let lines = peer_status_lines(&status);

        assert!(lines.iter().any(|line| line == &format!("peer: {peer}")));
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
            .with_advertised_routes(vec![p2p_vpn::runtime::control::ControlRoute::new(
                "10.42.0.0/24",
                100,
            )]);
        let status = RemotePeerStatus {
            peer,
            capabilities,
            service: p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200),
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
                .any(|line| line == &format!("peer live mtu: {peer} 1200"))
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
                "peer live path: {peer} reachable preferred direct QUIC datagram score 100 mtu 1200 path_mtu_estimate 1200 quic_datagrams false native_quic_datagrams false owned_udp_packet_plane false owned_quic_packet_plane false path_probe_ready false"
            )));
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
    fn cli_parses_daemon_status_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "daemon-status",
            "--socket",
            "/run/p2p-vpn-node-a/control.sock",
            "--timeout-seconds",
            "3",
        ])
        .expect("cli");

        let Command::DaemonStatus {
            socket,
            timeout_seconds,
        } = cli.command
        else {
            panic!("expected daemon-status command");
        };

        assert_eq!(socket, PathBuf::from("/run/p2p-vpn-node-a/control.sock"));
        assert_eq!(timeout_seconds, 3);
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
    fn cli_parses_relay_check_command() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "relay-check",
            "--relay-candidate",
            "/dns4/relay.example.net/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
            "--require-dcutr-success",
            "--timeout-seconds",
            "60",
            "--write-config",
            "relay-config.json",
            "--force",
        ])
        .expect("cli");

        let Command::RelayCheck {
            relay_candidates,
            require_dcutr_success,
            timeout_seconds,
            write_config,
            force,
        } = cli.command
        else {
            panic!("expected relay-check command");
        };

        assert_eq!(relay_candidates.len(), 1);
        assert!(relay_candidates[0].contains("relay.example.net"));
        assert!(require_dcutr_success);
        assert_eq!(timeout_seconds, 60);
        assert_eq!(write_config, Some(PathBuf::from("relay-config.json")));
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
        assert_eq!(write_config, Some(PathBuf::from("relay-scan-config.json")));
        assert!(force);
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
            .collect();

        p2p_vpn::runtime::bootstrap_check::PublicRelayScanReport {
            scanned_bootstrap_peers: addresses.len(),
            scanned_peers: addresses.len(),
            discovered_routing_peers: 0,
            dialed_routing_peers: 0,
            connected_bootstrap_peers: addresses.len(),
            identified_peers: addresses.len(),
            relay_capable_peers: addresses.len(),
            dial_failures: 0,
            candidates,
            peer_results: Vec::new(),
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
                    error: None,
                    bootstrap: None,
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
