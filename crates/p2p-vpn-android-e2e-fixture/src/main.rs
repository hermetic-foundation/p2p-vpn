use std::{
    error::Error,
    fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
        mpsc,
    },
    time::Duration,
};

use base64::Engine as _;
use clap::{Parser, Subcommand, ValueEnum};
use p2p_vpn::{
    config::{BootstrapPeerConfig, Config, DiscoveryConfig, PRIVATE_KADEMLIA_PROTOCOL},
    identity::NodeIdentity,
    runtime::{
        control_socket::runtime_control_channel,
        runner::{
            self, PreconfiguredTunRoutes, RuntimePlatform, ShutdownReason,
            run_config_until_with_runtime_platform,
        },
        tun::{PacketIo, PacketRead, PacketWrite, TunRuntimeConfig},
    },
};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    net::{UnixListener, UnixStream},
    sync::{broadcast, watch},
};

const SCHEMA_VERSION: u8 = 1;
const MAX_CONTROL_LINE_BYTES: usize = 4 * 1024;
const MAX_PROBE_COUNT: u16 = 100;
const MIN_PROBE_TIMEOUT_MILLIS: u64 = 100;
const MAX_PROBE_TIMEOUT_MILLIS: u64 = 60_000;
const DEFAULT_PROBE_TIMEOUT_MILLIS: u64 = 5_000;
const ICMP_PAYLOAD: &[u8] = b"p2p-vpn-android-e2e";

type BoxError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Parser)]
#[command(about, version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run private bootstrap infrastructure and one rootless overlay endpoint.
    Run {
        #[arg(long)]
        state_dir: PathBuf,
        #[arg(long, default_value = "android-e2e")]
        network: String,
        #[arg(long, default_value = "10.0.2.2")]
        emulator_host_alias: Ipv4Addr,
        #[arg(long, value_enum, default_value = "automatic")]
        path_mode: FixturePathMode,
    },
    /// Send overlay ICMP probes through a running fixture.
    Probe {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        source: IpAddr,
        #[arg(long)]
        destination: IpAddr,
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u16).range(1..=i64::from(MAX_PROBE_COUNT)))]
        count: u16,
        #[arg(long, default_value_t = DEFAULT_PROBE_TIMEOUT_MILLIS)]
        timeout_millis: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct FixtureMetadata {
    schema_version: u8,
    network: String,
    path_mode: FixturePathMode,
    bootstrap: BootstrapMetadata,
    peer: PeerMetadata,
    packet_control_socket: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct BootstrapMetadata {
    peer_id: String,
    android_address: String,
    kademlia_protocol: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PeerMetadata {
    peer_id: String,
    ipv4: Ipv4Addr,
    ipv6: Ipv6Addr,
    control_socket: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum FixturePathMode {
    Automatic,
    QuicStream,
    TcpStream,
    OwnedQuic,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "method", rename_all = "snake_case")]
enum PacketControlRequest {
    Probe {
        source: IpAddr,
        destination: IpAddr,
        count: u16,
        #[serde(default = "default_probe_timeout_millis")]
        timeout_millis: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ProbeResponse {
    schema_version: u8,
    ok: bool,
    family: String,
    sent: u16,
    received: u16,
}

#[derive(Clone)]
struct PacketAgent {
    outbound: mpsc::Sender<Vec<u8>>,
    inbound: broadcast::Sender<Vec<u8>>,
    identifier: Arc<AtomicU16>,
    local_ipv4: Ipv4Addr,
    local_ipv6: Ipv6Addr,
}

struct ChannelPacketReader {
    packets: mpsc::Receiver<Vec<u8>>,
}

struct AgentPacketWriter {
    agent: PacketAgent,
}

struct PendingPacketReader;

struct DiscardPacketWriter;

#[tokio::main]
async fn main() {
    let result = match Cli::parse().command {
        Command::Run {
            state_dir,
            network,
            emulator_host_alias,
            path_mode,
        } => run_fixture(&state_dir, &network, emulator_host_alias, path_mode).await,
        Command::Probe {
            socket,
            source,
            destination,
            count,
            timeout_millis,
        } => run_probe_client(&socket, source, destination, count, timeout_millis).await,
    };

    if let Err(error) = result {
        eprintln!("p2p-vpn Android E2E fixture failed: {error}");
        std::process::exit(1);
    }
}

#[allow(clippy::too_many_lines)]
async fn run_fixture(
    state_dir: &Path,
    network: &str,
    emulator_host_alias: Ipv4Addr,
    path_mode: FixturePathMode,
) -> Result<(), BoxError> {
    prepare_state_directory(state_dir)?;

    let bootstrap_identity = NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate bootstrap identity: {error:?}"))?;
    let peer_identity = NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate fixture identity: {error:?}"))?;
    let bootstrap_port = available_tcp_port()?;
    let peer_tcp_port = available_tcp_port()?;
    let peer_quic_port = available_udp_port()?;
    let packet_quic_port = available_udp_port_except(peer_quic_port)?;
    let membership_key = random_membership_key();

    let bootstrap = bootstrap_config(
        network,
        &bootstrap_identity,
        bootstrap_port,
        emulator_host_alias,
    )?;
    let peer = peer_config(
        network,
        &peer_identity,
        &bootstrap_identity,
        bootstrap_port,
        peer_tcp_port,
        peer_quic_port,
        packet_quic_port,
        emulator_host_alias,
        &membership_key,
        path_mode,
    )?;
    let tun = TunRuntimeConfig::from_config(&peer)
        .map_err(|error| format!("failed to derive fixture addresses: {error:?}"))?;

    let control_socket = state_dir.join("peer-control.sock");
    let packet_control_socket = state_dir.join("packet-control.sock");
    let pairing_state = state_dir.join("pairing-state.json");
    let membership_state = state_dir.join("membership-state.json");

    let (packet_tx, packet_rx) = mpsc::channel();
    let (inbound_tx, _) = broadcast::channel(256);
    let agent = PacketAgent {
        outbound: packet_tx,
        inbound: inbound_tx,
        identifier: Arc::new(AtomicU16::new(1)),
        local_ipv4: tun.addresses.ipv4,
        local_ipv6: tun.addresses.ipv6,
    };
    let peer_packet_io = PacketIo::new(
        ChannelPacketReader { packets: packet_rx },
        AgentPacketWriter {
            agent: agent.clone(),
        },
    );
    let bootstrap_packet_io = PacketIo::new(PendingPacketReader, DiscardPacketWriter);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (bootstrap_control, bootstrap_receiver) = runtime_control_channel();
    let bootstrap_platform = RuntimePlatform::new(bootstrap_packet_io, PreconfiguredTunRoutes)
        .with_control(bootstrap_receiver);
    let mut bootstrap_task = tokio::spawn(run_config_until_with_runtime_platform(
        bootstrap,
        bootstrap_platform,
        None,
        None,
        None,
        None,
        shutdown_signal(shutdown_rx.clone()),
    ));

    wait_for_runtime(&bootstrap_control, "bootstrap").await?;

    let (peer_control, peer_receiver) = runtime_control_channel();
    let peer_platform =
        RuntimePlatform::new(peer_packet_io, PreconfiguredTunRoutes).with_control(peer_receiver);
    let mut peer_task = tokio::spawn(run_config_until_with_runtime_platform(
        peer,
        peer_platform,
        Some(Duration::from_secs(1)),
        Some(control_socket.clone()),
        Some(pairing_state),
        Some(membership_state),
        shutdown_signal(shutdown_rx.clone()),
    ));

    wait_for_runtime(&peer_control, "overlay peer").await?;

    let packet_server = tokio::spawn(serve_packet_control(
        packet_control_socket.clone(),
        agent,
        shutdown_rx,
    ));
    wait_for_path(&packet_control_socket, "packet control socket").await?;

    let metadata = FixtureMetadata {
        schema_version: SCHEMA_VERSION,
        network: network.to_owned(),
        path_mode,
        bootstrap: BootstrapMetadata {
            peer_id: bootstrap_identity.peer_id.clone(),
            android_address: format!(
                "/ip4/{emulator_host_alias}/tcp/{bootstrap_port}/p2p/{}",
                bootstrap_identity.peer_id
            ),
            kademlia_protocol: PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
        },
        peer: PeerMetadata {
            peer_id: peer_identity.peer_id,
            ipv4: tun.addresses.ipv4,
            ipv6: tun.addresses.ipv6,
            control_socket: control_socket.to_string_lossy().into_owned(),
        },
        packet_control_socket: packet_control_socket.to_string_lossy().into_owned(),
    };
    write_json_atomically(&state_dir.join("fixture.json"), &metadata)?;

    eprintln!("p2p-vpn Android E2E fixture ready");
    let failure = tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|error| format!("failed to listen for Ctrl-C: {error}"))?;
            None
        }
        result = &mut bootstrap_task => Some(task_failure("bootstrap", result)),
        result = &mut peer_task => Some(task_failure("overlay peer", result)),
    };

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        if !bootstrap_task.is_finished() {
            let _ = (&mut bootstrap_task).await;
        }
        if !peer_task.is_finished() {
            let _ = (&mut peer_task).await;
        }
        let _ = packet_server.await;
    })
    .await;

    if let Some(error) = failure {
        return Err(error);
    }
    Ok(())
}

fn task_failure(
    label: &str,
    result: Result<Result<(), runner::RunnerError>, tokio::task::JoinError>,
) -> BoxError {
    match result {
        Ok(Ok(())) => format!("{label} runtime stopped unexpectedly").into(),
        Ok(Err(error)) => format!("{label} runtime failed: {error:?}").into(),
        Err(error) => format!("{label} runtime task failed: {error}").into(),
    }
}

async fn shutdown_signal(mut shutdown: watch::Receiver<bool>) -> ShutdownReason {
    while !*shutdown.borrow() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
    ShutdownReason::Terminate
}

async fn wait_for_runtime(
    control: &p2p_vpn::runtime::control_socket::RuntimeControlHandle,
    label: &str,
) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if tokio::time::timeout(Duration::from_secs(1), control.status())
            .await
            .is_ok_and(|result| result.is_ok())
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("{label} runtime did not become ready").into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_path(path: &Path, label: &str) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("{label} did not become ready").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

fn bootstrap_config(
    network: &str,
    identity: &NodeIdentity,
    port: u16,
    emulator_host_alias: Ipv4Addr,
) -> Result<Config, BoxError> {
    let mut config = minimal_config(network, identity)?;
    config.network.listen_addresses = vec![format!("/ip4/0.0.0.0/tcp/{port}")];
    config.network.external_addresses = vec![format!("/ip4/{emulator_host_alias}/tcp/{port}")];
    config.network.discovery = private_discovery(false);
    disable_packet_plane(&mut config);
    validate_config(config, "bootstrap")
}

#[allow(clippy::too_many_arguments)]
fn peer_config(
    network: &str,
    identity: &NodeIdentity,
    bootstrap: &NodeIdentity,
    bootstrap_port: u16,
    tcp_port: u16,
    quic_port: u16,
    packet_quic_port: u16,
    emulator_host_alias: Ipv4Addr,
    membership_key: &str,
    path_mode: FixturePathMode,
) -> Result<Config, BoxError> {
    let mut config = minimal_config(network, identity)?;
    config.network.membership_key = Some(membership_key.to_owned());
    let tcp_listen = format!("/ip4/0.0.0.0/tcp/{tcp_port}");
    let tcp_external = format!("/ip4/{emulator_host_alias}/tcp/{tcp_port}");
    let quic_listen = format!("/ip4/0.0.0.0/udp/{quic_port}/quic-v1");
    let quic_external = format!("/ip4/{emulator_host_alias}/udp/{quic_port}/quic-v1");
    match path_mode {
        FixturePathMode::Automatic | FixturePathMode::OwnedQuic => {
            config.network.listen_addresses = vec![tcp_listen, quic_listen];
            config.network.external_addresses = vec![tcp_external, quic_external];
        }
        FixturePathMode::QuicStream => {
            config.network.listen_addresses = vec![quic_listen];
            config.network.external_addresses = vec![quic_external];
        }
        FixturePathMode::TcpStream => {
            config.network.listen_addresses = vec![tcp_listen];
            config.network.external_addresses = vec![tcp_external];
        }
    }
    config.network.bootstrap_peers = vec![
        BootstrapPeerConfig {
            id: bootstrap.peer_id.clone(),
            address: format!(
                "/ip4/127.0.0.1/tcp/{bootstrap_port}/p2p/{}",
                bootstrap.peer_id
            ),
        },
        BootstrapPeerConfig {
            id: bootstrap.peer_id.clone(),
            address: format!(
                "/ip4/{emulator_host_alias}/tcp/{bootstrap_port}/p2p/{}",
                bootstrap.peer_id
            ),
        },
    ];
    config.network.discovery = private_discovery(true);
    if path_mode == FixturePathMode::OwnedQuic {
        config.network.packet_plane.listen.clear();
        config.network.packet_plane.external_endpoints.clear();
        config.network.packet_plane.quic_listen = vec![format!("0.0.0.0:{packet_quic_port}")];
        config.network.packet_plane.quic_external_endpoints =
            vec![format!("{emulator_host_alias}:{packet_quic_port}")];
    } else {
        disable_packet_plane(&mut config);
    }
    validate_config(config, "overlay peer")
}

fn minimal_config(network: &str, identity: &NodeIdentity) -> Result<Config, BoxError> {
    serde_json::from_value(serde_json::json!({
        "network": {
            "name": network,
            "private_key": identity.private_key,
            "listen_addresses": []
        },
        "interface": {"name": "pv-android-e2e", "mtu": 1280}
    }))
    .map_err(Into::into)
}

fn private_discovery(advertise: bool) -> DiscoveryConfig {
    DiscoveryConfig {
        mdns: false,
        kademlia: true,
        kademlia_provider_advertisement: advertise,
        kademlia_protocol: PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
        dcutr: false,
        autonat: false,
    }
}

fn disable_packet_plane(config: &mut Config) {
    config.network.packet_plane.listen.clear();
    config.network.packet_plane.external_endpoints.clear();
    config.network.packet_plane.quic_listen.clear();
    config.network.packet_plane.quic_external_endpoints.clear();
}

fn validate_config(config: Config, label: &str) -> Result<Config, BoxError> {
    config
        .validate_runtime()
        .map_err(|error| format!("invalid {label} config: {error:?}"))?;
    Ok(config)
}

fn random_membership_key() -> String {
    let mut key = [0_u8; 32];
    OsRng.fill_bytes(&mut key);
    base64::engine::general_purpose::STANDARD.encode(key)
}

fn available_tcp_port() -> io::Result<u16> {
    Ok(TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port())
}

fn available_udp_port() -> io::Result<u16> {
    Ok(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port())
}

fn available_udp_port_except(excluded: u16) -> io::Result<u16> {
    for _ in 0..32 {
        let port = available_udp_port()?;
        if port != excluded {
            return Ok(port);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AddrNotAvailable,
        "could not allocate distinct fixture UDP ports",
    ))
}

fn prepare_state_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    for socket in ["peer-control.sock", "packet-control.sock"] {
        match fs::remove_file(path.join(socket)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> Result<(), BoxError> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    fs::write(&temporary, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

impl PacketRead for ChannelPacketReader {
    fn read_packet(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let packet = self
            .packets
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "packet agent stopped"))?;
        if packet.len() > buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture packet exceeds runtime MTU",
            ));
        }
        buffer[..packet.len()].copy_from_slice(&packet);
        Ok(packet.len())
    }
}

impl PacketWrite for AgentPacketWriter {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
        let _ = self.agent.inbound.send(packet.to_vec());
        if let Some(reply) = echo_reply(packet) {
            self.agent
                .outbound
                .send(reply)
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "packet agent stopped"))?;
        }
        Ok(packet.len())
    }
}

impl PacketRead for PendingPacketReader {
    fn read_packet(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            std::thread::sleep(Duration::from_mins(1));
        }
    }
}

impl PacketWrite for DiscardPacketWriter {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
        Ok(packet.len())
    }
}

async fn serve_packet_control(
    socket: PathBuf,
    agent: PacketAgent,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), BoxError> {
    let listener = UnixListener::bind(socket)?;
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let agent = agent.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_packet_control(stream, agent).await {
                        eprintln!("packet control request failed: {error}");
                    }
                });
            }
        }
    }
}

async fn handle_packet_control(mut stream: UnixStream, agent: PacketAgent) -> Result<(), BoxError> {
    let line = read_bounded_control_line(&mut stream).await?;
    let request: PacketControlRequest = serde_json::from_slice(&line)?;
    let response = match request {
        PacketControlRequest::Probe {
            source,
            destination,
            count,
            timeout_millis,
        } => {
            validate_probe(&agent, source, destination, count, timeout_millis)?;
            agent
                .probe(
                    source,
                    destination,
                    count,
                    Duration::from_millis(timeout_millis),
                )
                .await
        }
    };
    let mut encoded = serde_json::to_vec(&response)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    Ok(())
}

async fn read_bounded_control_line(stream: &mut UnixStream) -> Result<Vec<u8>, BoxError> {
    let mut line = Vec::with_capacity(256);
    let mut byte = [0_u8; 1];
    while line.len() <= MAX_CONTROL_LINE_BYTES {
        let read = stream.read(&mut byte).await?;
        if read == 0 {
            return Err("incomplete packet control frame".into());
        }
        if byte[0] == b'\n' {
            return Ok(line);
        }
        line.push(byte[0]);
    }
    Err("packet control frame exceeds size limit".into())
}

fn validate_probe(
    agent: &PacketAgent,
    source: IpAddr,
    destination: IpAddr,
    count: u16,
    timeout_millis: u64,
) -> Result<(), BoxError> {
    let valid_source = match source {
        IpAddr::V4(address) => address == agent.local_ipv4 && destination.is_ipv4(),
        IpAddr::V6(address) => address == agent.local_ipv6 && destination.is_ipv6(),
    };
    if !valid_source {
        return Err(
            "probe source must be the fixture address and match the destination family".into(),
        );
    }
    if count == 0 || count > MAX_PROBE_COUNT {
        return Err(format!("probe count must be between 1 and {MAX_PROBE_COUNT}").into());
    }
    if !(MIN_PROBE_TIMEOUT_MILLIS..=MAX_PROBE_TIMEOUT_MILLIS).contains(&timeout_millis) {
        return Err(format!(
            "probe timeout must be between {MIN_PROBE_TIMEOUT_MILLIS} and {MAX_PROBE_TIMEOUT_MILLIS} milliseconds"
        )
        .into());
    }
    Ok(())
}

impl PacketAgent {
    async fn probe(
        &self,
        source: IpAddr,
        destination: IpAddr,
        count: u16,
        timeout: Duration,
    ) -> ProbeResponse {
        let identifier = self.identifier.fetch_add(1, Ordering::Relaxed);
        let mut received = 0_u16;
        let mut packets = self.inbound.subscribe();
        for sequence in 0..count {
            let request = echo_request(source, destination, identifier, sequence);
            if self.outbound.send(request).is_err() {
                break;
            }
            let reply = async {
                loop {
                    let packet = packets.recv().await.ok()?;
                    let Some(echo) = parse_echo(&packet) else {
                        continue;
                    };
                    if echo.kind == EchoKind::Reply
                        && echo.identifier == identifier
                        && echo.sequence == sequence
                        && echo.source == destination
                        && echo.destination == source
                    {
                        return Some(());
                    }
                }
            };
            if tokio::time::timeout(timeout, reply).await == Ok(Some(())) {
                received = received.saturating_add(1);
            }
        }
        ProbeResponse {
            schema_version: SCHEMA_VERSION,
            ok: received == count,
            family: if source.is_ipv4() { "ipv4" } else { "ipv6" }.to_owned(),
            sent: count,
            received,
        }
    }
}

async fn run_probe_client(
    socket: &Path,
    source: IpAddr,
    destination: IpAddr,
    count: u16,
    timeout_millis: u64,
) -> Result<(), BoxError> {
    let mut stream = UnixStream::connect(socket).await?;
    let request = PacketControlRequest::Probe {
        source,
        destination,
        count,
        timeout_millis,
    };
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_millis(timeout_millis.saturating_mul(u64::from(count)) + 2_000),
        reader.read_line(&mut line),
    )
    .await
    .map_err(|_| "packet control response timed out")??;
    let response: ProbeResponse = serde_json::from_str(line.trim_end())?;
    println!("{}", serde_json::to_string(&response)?);
    if !response.ok {
        return Err(format!(
            "{} probe received {} of {} replies",
            response.family, response.received, response.sent
        )
        .into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EchoKind {
    Request,
    Reply,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EchoPacket {
    kind: EchoKind,
    identifier: u16,
    sequence: u16,
    source: IpAddr,
    destination: IpAddr,
}

fn echo_request(source: IpAddr, destination: IpAddr, identifier: u16, sequence: u16) -> Vec<u8> {
    match (source, destination) {
        (IpAddr::V4(source), IpAddr::V4(destination)) => {
            ipv4_echo_packet(source, destination, 8, identifier, sequence)
        }
        (IpAddr::V6(source), IpAddr::V6(destination)) => {
            ipv6_echo_packet(source, destination, 128, identifier, sequence)
        }
        _ => unreachable!("validated probe families match"),
    }
}

fn echo_reply(packet: &[u8]) -> Option<Vec<u8>> {
    let echo = parse_echo(packet)?;
    if echo.kind != EchoKind::Request {
        return None;
    }
    match (echo.source, echo.destination) {
        (IpAddr::V4(_), IpAddr::V4(_)) => ipv4_echo_reply(packet),
        (IpAddr::V6(_), IpAddr::V6(_)) => ipv6_echo_reply(packet),
        _ => None,
    }
}

fn ipv4_echo_reply(packet: &[u8]) -> Option<Vec<u8>> {
    let total_len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    let mut reply = packet.get(..total_len)?.to_vec();
    let source = <[u8; 4]>::try_from(&reply[12..16]).ok()?;
    let destination = <[u8; 4]>::try_from(&reply[16..20]).ok()?;
    reply[12..16].copy_from_slice(&destination);
    reply[16..20].copy_from_slice(&source);
    reply[8] = 64;
    reply[10..12].fill(0);
    reply[20] = 0;
    reply[22..24].fill(0);
    let icmp_checksum = checksum(&reply[20..]);
    reply[22..24].copy_from_slice(&icmp_checksum.to_be_bytes());
    let header_checksum = checksum(&reply[..20]);
    reply[10..12].copy_from_slice(&header_checksum.to_be_bytes());
    Some(reply)
}

fn ipv6_echo_reply(packet: &[u8]) -> Option<Vec<u8>> {
    let payload_len = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
    let mut reply = packet.get(..40 + payload_len)?.to_vec();
    let original_source = <[u8; 16]>::try_from(&reply[8..24]).ok()?;
    let original_destination = <[u8; 16]>::try_from(&reply[24..40]).ok()?;
    reply[8..24].copy_from_slice(&original_destination);
    reply[24..40].copy_from_slice(&original_source);
    reply[7] = 64;
    reply[40] = 129;
    reply[42..44].fill(0);
    let source = Ipv6Addr::from(original_destination);
    let destination = Ipv6Addr::from(original_source);
    let icmp_checksum = icmpv6_checksum(source, destination, &reply[40..]);
    reply[42..44].copy_from_slice(&icmp_checksum.to_be_bytes());
    Some(reply)
}

fn ipv4_echo_packet(
    source: Ipv4Addr,
    destination: Ipv4Addr,
    icmp_type: u8,
    identifier: u16,
    sequence: u16,
) -> Vec<u8> {
    let total_len = 20 + 8 + ICMP_PAYLOAD.len();
    let mut packet = vec![0_u8; total_len];
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(
        &u16::try_from(total_len)
            .expect("small packet")
            .to_be_bytes(),
    );
    packet[6..8].copy_from_slice(&0x4000_u16.to_be_bytes());
    packet[8] = 64;
    packet[9] = 1;
    packet[12..16].copy_from_slice(&source.octets());
    packet[16..20].copy_from_slice(&destination.octets());
    let header_checksum = checksum(&packet[..20]);
    packet[10..12].copy_from_slice(&header_checksum.to_be_bytes());

    packet[20] = icmp_type;
    packet[24..26].copy_from_slice(&identifier.to_be_bytes());
    packet[26..28].copy_from_slice(&sequence.to_be_bytes());
    packet[28..].copy_from_slice(ICMP_PAYLOAD);
    let icmp_checksum = checksum(&packet[20..]);
    packet[22..24].copy_from_slice(&icmp_checksum.to_be_bytes());
    packet
}

fn ipv6_echo_packet(
    source: Ipv6Addr,
    destination: Ipv6Addr,
    icmp_type: u8,
    identifier: u16,
    sequence: u16,
) -> Vec<u8> {
    let payload_len = 8 + ICMP_PAYLOAD.len();
    let mut packet = vec![0_u8; 40 + payload_len];
    packet[0] = 0x60;
    packet[4..6].copy_from_slice(
        &u16::try_from(payload_len)
            .expect("small packet")
            .to_be_bytes(),
    );
    packet[6] = 58;
    packet[7] = 64;
    packet[8..24].copy_from_slice(&source.octets());
    packet[24..40].copy_from_slice(&destination.octets());
    packet[40] = icmp_type;
    packet[44..46].copy_from_slice(&identifier.to_be_bytes());
    packet[46..48].copy_from_slice(&sequence.to_be_bytes());
    packet[48..].copy_from_slice(ICMP_PAYLOAD);

    let icmp_checksum = icmpv6_checksum(source, destination, &packet[40..]);
    packet[42..44].copy_from_slice(&icmp_checksum.to_be_bytes());
    packet
}

fn parse_echo(packet: &[u8]) -> Option<EchoPacket> {
    match packet.first().map(|byte| byte >> 4) {
        Some(4) => parse_ipv4_echo(packet),
        Some(6) => parse_ipv6_echo(packet),
        _ => None,
    }
}

fn parse_ipv4_echo(packet: &[u8]) -> Option<EchoPacket> {
    if packet.len() < 28 || packet[0] & 0x0f != 5 || packet[9] != 1 {
        return None;
    }
    let total_len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    if total_len < 28 || total_len > packet.len() || checksum(&packet[..20]) != 0 {
        return None;
    }
    let icmp = &packet[20..total_len];
    if checksum(icmp) != 0 || icmp[1] != 0 {
        return None;
    }
    Some(EchoPacket {
        kind: match icmp[0] {
            8 => EchoKind::Request,
            0 => EchoKind::Reply,
            _ => return None,
        },
        identifier: u16::from_be_bytes([icmp[4], icmp[5]]),
        sequence: u16::from_be_bytes([icmp[6], icmp[7]]),
        source: IpAddr::V4(Ipv4Addr::new(
            packet[12], packet[13], packet[14], packet[15],
        )),
        destination: IpAddr::V4(Ipv4Addr::new(
            packet[16], packet[17], packet[18], packet[19],
        )),
    })
}

fn parse_ipv6_echo(packet: &[u8]) -> Option<EchoPacket> {
    if packet.len() < 48 || packet[6] != 58 {
        return None;
    }
    let payload_len = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
    if payload_len < 8 || 40 + payload_len > packet.len() {
        return None;
    }
    let source = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[8..24]).ok()?);
    let destination = Ipv6Addr::from(<[u8; 16]>::try_from(&packet[24..40]).ok()?);
    let icmp = &packet[40..40 + payload_len];
    if icmp[1] != 0 || icmpv6_checksum(source, destination, icmp) != 0 {
        return None;
    }
    Some(EchoPacket {
        kind: match icmp[0] {
            128 => EchoKind::Request,
            129 => EchoKind::Reply,
            _ => return None,
        },
        identifier: u16::from_be_bytes([icmp[4], icmp[5]]),
        sequence: u16::from_be_bytes([icmp[6], icmp[7]]),
        source: IpAddr::V6(source),
        destination: IpAddr::V6(destination),
    })
}

fn checksum(bytes: &[u8]) -> u16 {
    finalize_checksum(checksum_sum(bytes))
}

fn icmpv6_checksum(source: Ipv6Addr, destination: Ipv6Addr, packet: &[u8]) -> u16 {
    let mut sum = checksum_sum(&source.octets());
    sum += checksum_sum(&destination.octets());
    sum += u64::try_from(packet.len()).expect("packet length fits u64");
    sum += 58;
    sum += checksum_sum(packet);
    finalize_checksum(sum)
}

fn checksum_sum(bytes: &[u8]) -> u64 {
    let mut chunks = bytes.chunks_exact(2);
    let mut sum = chunks
        .by_ref()
        .map(|chunk| u64::from(u16::from_be_bytes([chunk[0], chunk[1]])))
        .sum::<u64>();
    if let Some(byte) = chunks.remainder().first() {
        sum += u64::from(*byte) << 8;
    }
    sum
}

fn finalize_checksum(mut sum: u64) -> u16 {
    while sum > u64::from(u16::MAX) {
        sum = (sum & u64::from(u16::MAX)) + (sum >> 16);
    }
    !u16::try_from(sum).expect("folded checksum fits u16")
}

const fn default_probe_timeout_millis() -> u64 {
    DEFAULT_PROBE_TIMEOUT_MILLIS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_echo_request_and_reply_are_valid() {
        let source = Ipv4Addr::new(100, 64, 1, 2);
        let destination = Ipv4Addr::new(100, 64, 3, 4);
        let request = echo_request(source.into(), destination.into(), 42, 7);

        assert_eq!(
            parse_echo(&request),
            Some(EchoPacket {
                kind: EchoKind::Request,
                identifier: 42,
                sequence: 7,
                source: source.into(),
                destination: destination.into(),
            })
        );
        let reply = echo_reply(&request).expect("echo reply");
        assert_eq!(
            parse_echo(&reply),
            Some(EchoPacket {
                kind: EchoKind::Reply,
                identifier: 42,
                sequence: 7,
                source: destination.into(),
                destination: source.into(),
            })
        );
    }

    #[test]
    fn ipv6_echo_request_and_reply_are_valid() {
        let source = "fd00:6879:7072:7370:6163:6500::1"
            .parse::<Ipv6Addr>()
            .expect("source");
        let destination = "fd00:6879:7072:7370:6163:6500::2"
            .parse::<Ipv6Addr>()
            .expect("destination");
        let request = echo_request(source.into(), destination.into(), 9, 11);

        assert_eq!(
            parse_echo(&request),
            Some(EchoPacket {
                kind: EchoKind::Request,
                identifier: 9,
                sequence: 11,
                source: source.into(),
                destination: destination.into(),
            })
        );
        let reply = echo_reply(&request).expect("echo reply");
        assert_eq!(
            parse_echo(&reply),
            Some(EchoPacket {
                kind: EchoKind::Reply,
                identifier: 9,
                sequence: 11,
                source: destination.into(),
                destination: source.into(),
            })
        );
    }

    #[test]
    fn corrupted_packets_are_not_reflected() {
        let mut packet = echo_request(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            1,
            1,
        );
        packet[28] ^= 0xff;

        assert_eq!(parse_echo(&packet), None);
        assert_eq!(echo_reply(&packet), None);
    }

    #[test]
    fn fixture_configs_are_valid_and_metadata_contains_no_secrets() {
        let bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
        let peer = NodeIdentity::generate_ed25519().expect("peer identity");
        let membership_key = random_membership_key();
        let bootstrap_config = bootstrap_config(
            "android-e2e",
            &bootstrap,
            42_300,
            Ipv4Addr::new(10, 0, 2, 2),
        )
        .expect("bootstrap config");
        let peer_config = peer_config(
            "android-e2e",
            &peer,
            &bootstrap,
            42_300,
            42_301,
            42_302,
            42_303,
            Ipv4Addr::new(10, 0, 2, 2),
            &membership_key,
            FixturePathMode::Automatic,
        )
        .expect("peer config");

        assert!(bootstrap_config.peers.is_empty());
        assert!(peer_config.peers.is_empty());
        assert!(peer_config.network.packet_plane.listen.is_empty());
        let metadata = FixtureMetadata {
            schema_version: SCHEMA_VERSION,
            network: "android-e2e".to_owned(),
            path_mode: FixturePathMode::Automatic,
            bootstrap: BootstrapMetadata {
                peer_id: bootstrap.peer_id.clone(),
                android_address: format!("/ip4/10.0.2.2/tcp/42300/p2p/{}", bootstrap.peer_id),
                kademlia_protocol: PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
            },
            peer: PeerMetadata {
                peer_id: peer.peer_id.clone(),
                ipv4: TunRuntimeConfig::from_config(&peer_config)
                    .expect("TUN config")
                    .addresses
                    .ipv4,
                ipv6: TunRuntimeConfig::from_config(&peer_config)
                    .expect("TUN config")
                    .addresses
                    .ipv6,
                control_socket: "/private/peer.sock".to_owned(),
            },
            packet_control_socket: "/private/packet.sock".to_owned(),
        };
        let encoded = serde_json::to_string(&metadata).expect("metadata JSON");

        assert!(!encoded.contains(&bootstrap.private_key));
        assert!(!encoded.contains(&peer.private_key));
        assert!(!encoded.contains(&membership_key));
    }

    #[test]
    fn fixture_path_modes_constrain_data_plane_listeners() {
        let bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
        let peer = NodeIdentity::generate_ed25519().expect("peer identity");
        let membership_key = random_membership_key();
        let host = Ipv4Addr::new(10, 0, 2, 2);
        let config = |path_mode| {
            peer_config(
                "android-e2e",
                &peer,
                &bootstrap,
                42_300,
                42_301,
                42_302,
                42_303,
                host,
                &membership_key,
                path_mode,
            )
            .expect("peer config")
        };

        let automatic = config(FixturePathMode::Automatic);
        assert_eq!(automatic.network.listen_addresses.len(), 2);
        assert!(automatic.network.packet_plane.quic_listen.is_empty());

        let quic_stream = config(FixturePathMode::QuicStream);
        assert_eq!(quic_stream.network.listen_addresses.len(), 1);
        assert!(quic_stream.network.listen_addresses[0].contains("/quic-v1"));

        let tcp_stream = config(FixturePathMode::TcpStream);
        assert_eq!(tcp_stream.network.listen_addresses.len(), 1);
        assert!(tcp_stream.network.listen_addresses[0].contains("/tcp/"));

        let owned_quic = config(FixturePathMode::OwnedQuic);
        assert_eq!(owned_quic.network.listen_addresses.len(), 2);
        assert!(owned_quic.network.packet_plane.listen.is_empty());
        assert_eq!(
            owned_quic.network.packet_plane.quic_listen,
            ["0.0.0.0:42303"]
        );
        assert_eq!(
            owned_quic.network.packet_plane.quic_external_endpoints,
            ["10.0.2.2:42303"]
        );
    }
}
