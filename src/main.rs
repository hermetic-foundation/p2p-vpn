use std::path::PathBuf;

use clap::{Parser, Subcommand};
use p2p_vpn::{
    PathKind,
    config::{Config, RuntimeDefaults},
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
    Status {
        #[arg(short, long, default_value = "p2p-vpn.json")]
        config: PathBuf,
    },
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();

    match cli.command {
        Command::Status { config } => status(&config),
    }
}

fn status(path: &PathBuf) -> Result<(), String> {
    let config = Config::load(path).map_err(|error| format!("failed to load config: {error:?}"))?;
    let routes = config
        .compile_routes()
        .map_err(|error| format!("failed to compile routes: {error:?}"))?;
    let defaults = RuntimeDefaults::default();

    println!("network: {}", config.network.name);
    println!("peers: {}", config.peers.len());
    println!(
        "queue: {} packets / {} bytes per peer",
        config.queue.max_packets_per_peer, config.queue.max_bytes_per_peer
    );
    println!("wire: v{WIRE_VERSION}, {HEADER_LEN}-byte packet header");
    println!(
        "preferred path: {} (score {})",
        path_name(defaults.preferred_path),
        defaults.preferred_path.default_score()
    );
    println!("compiled routes: {}", routes.len());

    Ok(())
}

fn path_name(path: PathKind) -> &'static str {
    match path {
        PathKind::DirectQuicDatagram => "direct QUIC datagram",
        PathKind::DirectQuicStream => "direct QUIC stream",
        PathKind::DirectTcpStream => "direct TCP stream",
        PathKind::CircuitRelay => "circuit relay",
    }
}
