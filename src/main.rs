use std::{fs, path::PathBuf, str::FromStr, time::Duration};

use clap::{Parser, Subcommand};
use p2p_vpn::{
    PathKind,
    config::{
        Config, DiscoveryConfig, InitConfigTemplate, InitPeer, RelayConfig, RelayResourceConfig,
        RuntimeDefaults,
    },
    identity::NodeIdentity,
    metrics::RuntimeMetrics,
    queue::QueueStats,
    runtime::{
        forward::session_id_for_peer,
        runner,
        tun::{TunDevice, TunRuntimeConfig},
    },
    wire::{HEADER_LEN, WIRE_VERSION},
};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Keygen,
    InitConfig {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        output: PathBuf,
        #[arg(long, default_value = "lab")]
        network: String,
        #[arg(long)]
        private_key: Option<String>,
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
        #[arg(long = "peer")]
        peers: Vec<EndpointArg>,
        #[arg(long = "relay-reservation")]
        relay_reservations: Vec<String>,
        #[arg(long)]
        relay_server: bool,
        #[arg(long)]
        disable_mdns: bool,
        #[arg(long)]
        disable_kademlia: bool,
        #[arg(long)]
        disable_dcutr: bool,
        #[arg(long)]
        force: bool,
    },
    Status {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
    },
    Metrics {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
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
async fn main() -> Result<(), String> {
    let cli = Cli::parse();

    match cli.command {
        Command::Keygen => keygen(),
        Command::InitConfig {
            output,
            network,
            private_key,
            interface,
            mtu,
            listen_addresses,
            external_addresses,
            bootstrap_peers,
            peers,
            relay_reservations,
            relay_server,
            disable_mdns,
            disable_kademlia,
            disable_dcutr,
            force,
        } => init_config(InitConfigArgs {
            output,
            network,
            private_key,
            interface,
            mtu,
            listen_addresses,
            external_addresses,
            bootstrap_peers,
            peers,
            discovery: DiscoveryConfig {
                mdns: !disable_mdns,
                kademlia: !disable_kademlia,
                dcutr: !disable_dcutr,
            },
            relay: RelayConfig {
                server: relay_server,
                reservations: relay_reservations,
                resources: RelayResourceConfig::default(),
            },
            force,
        }),
        Command::Status { config } => status(&config),
        Command::Metrics { config } => metrics(&config),
        Command::Up {
            config,
            dry_run,
            metrics_interval_seconds,
        } => Box::pin(up(&config, dry_run, metrics_interval_seconds)).await,
    }
}

#[derive(Clone, Debug)]
struct InitConfigArgs {
    output: PathBuf,
    network: String,
    private_key: Option<String>,
    interface: String,
    mtu: u16,
    listen_addresses: Vec<String>,
    external_addresses: Vec<String>,
    bootstrap_peers: Vec<EndpointArg>,
    peers: Vec<EndpointArg>,
    discovery: DiscoveryConfig,
    relay: RelayConfig,
    force: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EndpointArg {
    id: String,
    address: Option<String>,
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

    let identity = match args.private_key {
        Some(private_key) => NodeIdentity::from_private_key(&private_key)
            .map_err(|error| format!("failed to decode private key: {error:?}"))?,
        None => NodeIdentity::generate_ed25519()
            .map_err(|error| format!("failed to generate key: {error:?}"))?,
    };
    let config = InitConfigTemplate {
        identity,
        network_name: args.network,
        interface_name: args.interface,
        mtu: args.mtu,
        listen_addresses: args.listen_addresses,
        external_addresses: args.external_addresses,
        bootstrap_peers: args
            .bootstrap_peers
            .into_iter()
            .map(EndpointArg::into)
            .collect(),
        peers: args.peers.into_iter().map(EndpointArg::into).collect(),
        discovery: args.discovery,
        relay: args.relay,
    }
    .into_config();
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
        }
    }
}

fn status(path: &PathBuf) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    config
        .validate_runtime()
        .map_err(|error| format!("config is not runtime-ready: {error:?}"))?;
    let routes = config
        .compile_routes()
        .map_err(|error| format!("failed to compile routes: {error:?}"))?;
    let defaults = RuntimeDefaults::default();

    println!("network: {}", config.network.name);
    println!(
        "interface: {} mtu {}",
        config.interface.name, config.interface.mtu
    );
    println!("effective packet mtu: {}", config.effective_packet_mtu());
    println!(
        "packet session: {}",
        session_id_for_peer(
            config
                .local_peer_id()
                .map_err(|error| format!("failed to parse local peer id: {error:?}"))?
        )
    );
    println!("peers: {}", config.peers.len());
    println!(
        "queue: {} packets / {} bytes / {} ms per peer",
        config.queue.max_packets_per_peer,
        config.queue.max_bytes_per_peer,
        config.queue.max_packet_age().as_millis()
    );
    println!(
        "resources: {} concurrent control streams / {} concurrent packet streams",
        config.resources.control_stream_limit(),
        config.resources.packet_stream_limit()
    );
    println!(
        "connection limits: {} pending in / {} pending out / {} established in / {} established out / {} per peer / {} total",
        config.resources.max_pending_incoming_connections,
        config.resources.max_pending_outgoing_connections,
        config.resources.max_established_incoming_connections,
        config.resources.max_established_outgoing_connections,
        config.resources.max_established_connections_per_peer,
        config.resources.max_established_connections
    );
    println!("wire: v{WIRE_VERSION}, {HEADER_LEN}-byte packet header");
    println!(
        "preferred path: {} (score {})",
        path_name(defaults.preferred_path),
        defaults.preferred_path.default_score()
    );
    println!("compiled routes: {}", routes.len());
    println!(
        "listen addresses: {}",
        config.network.listen_addresses.len()
    );
    println!(
        "external addresses: {}",
        config.network.external_addresses.len()
    );
    println!("bootstrap peers: {}", config.network.bootstrap_peers.len());
    println!(
        "discovery: mdns={} kademlia={} dcutr={}",
        config.network.discovery.mdns,
        config.network.discovery.kademlia,
        config.network.discovery.dcutr
    );
    println!("relay server: {}", config.network.relay.server);
    println!(
        "relay reservations: {}",
        config.network.relay.reservations.len()
    );
    println!(
        "relay resources: {} reservations / {} per peer / {}s reservation / {} circuits / {} per peer / {}s circuit / {} bytes",
        config.network.relay.resources.max_reservations,
        config.network.relay.resources.max_reservations_per_peer,
        config.network.relay.resources.reservation_duration_secs,
        config.network.relay.resources.max_circuits,
        config.network.relay.resources.max_circuits_per_peer,
        config.network.relay.resources.max_circuit_duration_secs,
        config.network.relay.resources.max_circuit_bytes
    );
    println!(
        "configured peer addresses: {}",
        config
            .peers
            .iter()
            .map(|peer| peer.addresses.len())
            .sum::<usize>()
    );

    Ok(())
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
    Box::pin(runner::run_config(config, device, metrics_interval))
        .await
        .map_err(|error| format!("runtime failed: {error:?}"))
}

fn path_name(path: PathKind) -> &'static str {
    match path {
        PathKind::DirectQuicDatagram => "direct QUIC datagram",
        PathKind::DirectQuicStream => "direct QUIC stream",
        PathKind::DirectTcpStream => "direct TCP stream",
        PathKind::CircuitRelay => "circuit relay",
    }
}
