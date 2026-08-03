use std::{fs, path::PathBuf, str::FromStr, time::Duration};

use clap::{Parser, Subcommand};
use p2p_vpn::{
    PathKind,
    config::{
        Config, DiscoveryConfig, InitConfigTemplate, InitPeer, RelayConfig, RelayResourceConfig,
        RouteConfig, RuntimeDefaults,
    },
    identity::NodeIdentity,
    metrics::RuntimeMetrics,
    queue::QueueStats,
    runtime::{
        forward::session_id_for_peer,
        runner,
        tun::{TunAddresses, TunDevice, TunRuntimeConfig},
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
        #[arg(long)]
        disable_mdns: bool,
        #[arg(long)]
        disable_kademlia: bool,
        #[arg(long, default_value = "/p2p-vpn/kad/1")]
        kademlia_protocol: String,
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
            membership_key,
            interface,
            mtu,
            listen_addresses,
            external_addresses,
            bootstrap_peers,
            peers,
            local_routes,
            peer_routes,
            relay_reservations,
            relay_server,
            disable_mdns,
            disable_kademlia,
            kademlia_protocol,
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
            peers,
            local_routes,
            peer_routes,
            discovery: DiscoveryConfig {
                mdns: !disable_mdns,
                kademlia: !disable_kademlia,
                kademlia_protocol,
                dcutr: !disable_dcutr,
                autonat: !disable_autonat,
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
    membership_key: Option<String>,
    interface: String,
    mtu: u16,
    listen_addresses: Vec<String>,
    external_addresses: Vec<String>,
    bootstrap_peers: Vec<EndpointArg>,
    peers: Vec<EndpointArg>,
    local_routes: Vec<LocalRouteArg>,
    peer_routes: Vec<PeerRouteArg>,
    discovery: DiscoveryConfig,
    relay: RelayConfig,
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

    let identity = match args.private_key {
        Some(private_key) => NodeIdentity::from_private_key(&private_key)
            .map_err(|error| format!("failed to decode private key: {error:?}"))?,
        None => NodeIdentity::generate_ed25519()
            .map_err(|error| format!("failed to generate key: {error:?}"))?,
    };
    let config = InitConfigTemplate {
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
        bootstrap_peers: args
            .bootstrap_peers
            .into_iter()
            .map(EndpointArg::into)
            .collect(),
        peers: init_peers(args.peers, args.peer_routes),
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
        "discovery: mdns={} kademlia={} dcutr={} autonat={}",
        config.network.discovery.mdns,
        config.network.discovery.kademlia,
        config.network.discovery.dcutr,
        config.network.discovery.autonat
    ));
    lines.push(format!(
        "kademlia protocol: {}",
        config.network.discovery.kademlia_protocol
    ));
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
                .any(|line| line == "local route: 10.41.0.0/24 metric 0 configured")
        );
        assert!(lines.iter().any(|line| line
            == &format!(
                "peer route: {} 10.42.0.0/24 metric 0 configured",
                remote.peer_id
            )));
    }

    #[test]
    fn cli_parses_peer_route_arguments() {
        let cli = Cli::try_parse_from([
            "p2p-vpn",
            "init-config",
            "--kademlia-protocol",
            "/ipfs/kad/1.0.0",
            "--membership-key",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "--local-route",
            "10.41.0.0/24,75",
            "--peer",
            "12D3KooWPeer=/ip4/127.0.0.1/tcp/4001",
            "--peer-route",
            "12D3KooWPeer=10.42.0.0/24,250",
        ])
        .expect("cli");

        let Command::InitConfig {
            peers,
            local_routes,
            peer_routes,
            kademlia_protocol,
            membership_key,
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
        assert_eq!(
            membership_key.as_deref(),
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
        );
    }
}
