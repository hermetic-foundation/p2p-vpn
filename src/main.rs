use std::{path::PathBuf, time::Duration};

use clap::{Parser, Subcommand};
use p2p_vpn::{
    PathKind,
    config::{Config, RuntimeDefaults},
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
        Command::Status { config } => status(&config),
        Command::Metrics { config } => metrics(&config),
        Command::Up {
            config,
            dry_run,
            metrics_interval_seconds,
        } => up(&config, dry_run, metrics_interval_seconds).await,
    }
}

fn keygen() -> Result<(), String> {
    let identity = NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate key: {error:?}"))?;

    println!("peer_id: {}", identity.peer_id);
    println!("private_key: {}", identity.private_key);
    Ok(())
}

fn status(path: &PathBuf) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
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
        "resources: {} concurrent packet streams",
        config.resources.packet_stream_limit()
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
    runner::run_config(config, device, metrics_interval)
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
