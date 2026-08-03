use std::{fs, net::IpAddr, path::PathBuf, str::FromStr, time::Duration};

use clap::{Parser, Subcommand};
use p2p_vpn::{
    PathKind,
    config::{
        Config, DiscoveryConfig, InitConfigTemplate, InitPeer, QueueConfig, RelayConfig,
        RelayResourceConfig, ResourceConfig, RouteConfig, RuntimeDefaults,
    },
    identity::NodeIdentity,
    metrics::RuntimeMetrics,
    queue::QueueStats,
    runtime::{
        forward::session_id_for_peer,
        remote::{RemotePeerStatus, query_peer_status},
        runner::{self, ShutdownReason},
        service::SERVICE_PROTOCOL,
        tun::{TunAddresses, TunDevice, TunRuntimeConfig},
    },
    wire::{HEADER_LEN, WIRE_VERSION},
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
        #[arg(long, default_value = "hs0")]
        interface: String,
        #[arg(long, default_value_t = 1_280)]
        mtu: u16,
        #[arg(long = "listen-address")]
        listen_addresses: Vec<String>,
        #[arg(long = "external-address")]
        external_addresses: Vec<String>,
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
            interface,
            mtu,
            listen_addresses,
            external_addresses,
            bootstrap_peers,
            ipfs_bootstrap_peers,
            peers,
            local_routes,
            peer_routes,
            relay_reservations,
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
            interface,
            mtu,
            listen_addresses,
            external_addresses,
            bootstrap_peers,
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
        Command::Metrics { config } => metrics(&config),
        Command::Peers {
            config,
            live,
            timeout_seconds,
        } => Box::pin(peers(&config, live, timeout_seconds)).await,
        Command::PeerStatus {
            peer,
            config,
            timeout_seconds,
        } => Box::pin(peer_status(&config, &peer, timeout_seconds)).await,
        Command::Up {
            config,
            dry_run,
            metrics_interval_seconds,
        } => Box::pin(up(&config, dry_run, metrics_interval_seconds)).await,
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
    interface: String,
    mtu: u16,
    listen_addresses: Vec<String>,
    external_addresses: Vec<String>,
    bootstrap_peers: Vec<EndpointArg>,
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
        .find(|peer| peer.address.is_none())
    {
        return Err(format!(
            "bootstrap peer {} must include an address as PEER_ID=MULTIADDR",
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
    let bootstrap_peers = init_bootstrap_peers(args.bootstrap_peers, args.ipfs_bootstrap_peers);
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
        bootstrap_peers,
        peers: init_peers(args.peers, args.peer_routes),
        discovery: args.discovery,
        relay: args.relay,
    }
    .into_config();
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

fn init_bootstrap_peers(mut peers: Vec<EndpointArg>, include_ipfs_defaults: bool) -> Vec<InitPeer> {
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
        format!("effective mtu: {}", status.service.effective_mtu),
        format!(
            "supports quic datagrams: {}",
            status.service.supports_quic_datagrams
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

async fn up(
    path: &PathBuf,
    dry_run: bool,
    metrics_interval_seconds: Option<u64>,
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
                routes: vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 0,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
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
                routes: vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 50,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
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
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
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
            service: p2p_vpn::runtime::service::ServiceStatusResponse::local("lab", None, 1, 1200),
        };

        let lines = peer_status_lines(&status);

        assert!(lines.iter().any(|line| line == &format!("peer: {peer}")));
        assert!(lines.iter().any(|line| line == "network: lab"));
        assert!(lines.iter().any(|line| line == "effective mtu: 1200"));
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
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
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
        assert!(lines.iter().any(|line| line
            == &format!("peer live preferred path: {peer} direct QUIC stream")));
        assert!(
            lines.iter().any(|line| line
                == &format!("peer live advertised route: {peer} 10.42.0.0/24 metric 100"))
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
        assert_eq!(max_established_connections, 88);
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
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            ipfs_bootstrap_peers: true,
            peers: Vec::new(),
            local_routes: Vec::new(),
            peer_routes: Vec::new(),
            discovery: DiscoveryConfig::default(),
            relay: RelayConfig::default(),
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
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
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
    fn init_config_writes_custom_queue_and_resource_limits() {
        let output = temp_config_path("p2p-vpn-init-config");

        init_config(InitConfigArgs {
            output: output.clone(),
            network: "lab".to_owned(),
            private_key: None,
            membership_key: None,
            interface: "hs0".to_owned(),
            mtu: 1280,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
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
            queue: QueueConfig {
                max_packets_per_peer: 12,
                max_bytes_per_peer: 8192,
                max_packet_age_millis: 250,
            },
            resources: ResourceConfig {
                max_concurrent_control_streams: 11,
                max_concurrent_packet_streams: 22,
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
