//! Real encrypted TCP transport and production runtimes, with controllable local
//! underlay links. Packet channels replace TUN; no host routes/NICs are modified.
use p2p_vpn::{
    PeerId,
    config::{Config, DiscoveryConfig},
    identity::NodeIdentity,
    route::builtin_ipv4,
    runtime::{
        control_socket::{RuntimeControlHandle, runtime_control_channel},
        runner::{
            PreconfiguredTunRoutes, RuntimePlatform, ShutdownReason,
            run_config_until_with_runtime_platform,
        },
        tun::{PacketIo, PacketRead, PacketWrite},
    },
};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc as async_mpsc, oneshot, watch},
    task::JoinHandle,
};

#[path = "support/connection_recovery_lifecycle.rs"]
mod lifecycle;
#[path = "support/connection_recovery_quic.rs"]
mod quic;
#[path = "support/connection_recovery_relay.rs"]
mod relay;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Link {
    Up,
    Down,
    Blackhole,
    Stop,
}
struct Proxy {
    address: SocketAddr,
    state: watch::Sender<Link>,
    accepted: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    task: JoinHandle<()>,
}
impl Proxy {
    async fn new(ip: Ipv4Addr, backend: SocketAddr, initial: Link) -> Self {
        let listener = TcpListener::bind((ip, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let (state, mut changes) = watch::channel(initial);
        let accepted = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(AtomicU64::new(0));
        let (count, traffic) = (Arc::clone(&accepted), Arc::clone(&bytes));
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    result = changes.changed() => {
                        if result.is_err() || *changes.borrow() == Link::Stop { break; }
                    }
                    result = listener.accept() => {
                        let (mut incoming, _) = result.unwrap();
                        if *changes.borrow() != Link::Up { continue; }
                        count.fetch_add(1, Ordering::Relaxed);
                        let mut mode = changes.clone();
                        let traffic = Arc::clone(&traffic);
                        connections.spawn(async move {
                            let Ok(mut outgoing) = TcpStream::connect(backend).await else { return; };
                            loop {
                                let current = *mode.borrow_and_update();
                                match current {
                                    Link::Down | Link::Stop => break,
                                    Link::Blackhole => { if mode.changed().await.is_err() { break; } }
                                    Link::Up => {
                                        tokio::select! {
                                            result = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing) => {
                                                if let Ok((a, b)) = result { traffic.fetch_add(a + b, Ordering::Relaxed); }
                                                break;
                                            }
                                            result = mode.changed() => { if result.is_err() { break; } }
                                        }
                                    }
                                }
                            }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        Self {
            address,
            state,
            accepted,
            bytes,
            task,
        }
    }
    fn set(&self, mode: Link) {
        self.state.send_replace(mode);
    }
    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"address":self.address.to_string(), "accepted":self.accepted.load(Ordering::Relaxed), "completed_connection_bytes":self.bytes.load(Ordering::Relaxed)})
    }
}

struct Reader {
    packets: mpsc::Receiver<Option<Vec<u8>>>,
    cancel: mpsc::Sender<Option<Vec<u8>>>,
}
impl PacketRead for Reader {
    fn read_packet(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.packets.recv() {
            Ok(Some(packet)) => {
                buffer[..packet.len()].copy_from_slice(&packet);
                Ok(packet.len())
            }
            _ => Ok(0),
        }
    }
    fn cancellation(&self) -> Option<Box<dyn FnOnce() + Send>> {
        let cancel = self.cancel.clone();
        Some(Box::new(move || {
            let _ = cancel.send(None);
        }))
    }
}
struct Writer(async_mpsc::UnboundedSender<Vec<u8>>);
impl PacketWrite for Writer {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
        self.0
            .send(packet.to_vec())
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        Ok(packet.len())
    }
}
struct Node {
    control: RuntimeControlHandle,
    input: mpsc::Sender<Option<Vec<u8>>>,
    output: async_mpsc::UnboundedReceiver<Vec<u8>>,
    shutdown: Option<oneshot::Sender<()>>,
    daemon: JoinHandle<Result<(), p2p_vpn::runtime::runner::RunnerError>>,
    ip: Ipv4Addr,
}
impl Node {
    fn start(config: Config) -> Self {
        Self::start_with_lifecycle(config, false)
    }

    fn start_with_lifecycle(config: Config, enabled: bool) -> Self {
        let ip = builtin_ipv4(config.local_peer_id().unwrap());
        let (input, packets) = mpsc::channel();
        let (writer, output) = async_mpsc::unbounded_channel();
        let io = PacketIo::new(
            Reader {
                packets,
                cancel: input.clone(),
            },
            Writer(writer),
        );
        let (control, receiver) = runtime_control_channel();
        let mut platform = RuntimePlatform::new(io, PreconfiguredTunRoutes).with_control(receiver);
        if enabled {
            platform = platform.with_experimental_demand_lifecycle();
        }
        let (shutdown, stopped) = oneshot::channel();
        let daemon = tokio::spawn(run_config_until_with_runtime_platform(
            config,
            platform,
            None,
            None,
            None,
            None,
            async {
                let _ = stopped.await;
                ShutdownReason::ControlSocket
            },
        ));
        Self {
            control,
            input,
            output,
            shutdown: Some(shutdown),
            daemon,
            ip,
        }
    }
}

fn config(
    local: &NodeIdentity,
    remote: &NodeIdentity,
    listener: SocketAddr,
    addresses: Vec<String>,
) -> Config {
    let mut config: Config = serde_json::from_value(serde_json::json!({
        "network":{"name":"windows-recovery-test", "private_key":local.private_key,
            "listen_addresses":[format!("/ip4/{}/tcp/{}",listener.ip(),listener.port())],
            "packet_plane":{"listen":[], "quic_listen":[]}},
        "peers":[{"id":remote.peer_id,"addresses":addresses}]
    }))
    .unwrap();
    config.network.discovery = DiscoveryConfig {
        mdns: false,
        kademlia: false,
        autonat: false,
        dcutr: false,
        kademlia_provider_advertisement: false,
        ..DiscoveryConfig::default()
    };
    config.network.relay.auto.max_reservations = 0;
    config
}
fn free_address() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn packet(source: Ipv4Addr, destination: Ipv4Addr, sequence: u64) -> Vec<u8> {
    let mut packet = vec![0_u8; 36];
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(&36_u16.to_be_bytes());
    packet[8] = 64;
    packet[9] = 17;
    packet[12..16].copy_from_slice(&source.octets());
    packet[16..20].copy_from_slice(&destination.octets());
    packet[20..22].copy_from_slice(&42001_u16.to_be_bytes());
    packet[22..24].copy_from_slice(&42002_u16.to_be_bytes());
    packet[24..26].copy_from_slice(&16_u16.to_be_bytes());
    packet[28..].copy_from_slice(&sequence.to_be_bytes());
    let mut sum: u32 = packet[..20]
        .chunks_exact(2)
        .map(|c| u32::from(u16::from_be_bytes([c[0], c[1]])))
        .sum();
    while sum >> 16 != 0 {
        sum = (sum & 65535) + (sum >> 16);
    }
    packet[10..12].copy_from_slice(&(!(sum as u16)).to_be_bytes());
    packet
}
async fn probe(a: &mut Node, b: &mut Node, sequence: u64, wait: Duration) -> bool {
    let ab = packet(a.ip, b.ip, sequence);
    let ba = packet(b.ip, a.ip, sequence);
    a.input.send(Some(ab.clone())).unwrap();
    b.input.send(Some(ba.clone())).unwrap();
    let mut got_a = false;
    let mut got_b = false;
    let deadline = tokio::time::sleep(wait);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            Some(received) = a.output.recv(), if !got_a => { got_a = received == ba; }
            Some(received) = b.output.recv(), if !got_b => { got_b = received == ab; }
            _ = &mut deadline => return false,
        }
        if got_a && got_b {
            return true;
        }
    }
}
async fn stage(
    name: &str,
    a: &mut Node,
    b: &mut Node,
    sequence: &mut u64,
    deadline: Duration,
) -> serde_json::Value {
    let start = Instant::now();
    let mut attempts = 0;
    let mut consecutive = 0;
    let mut first = None;
    while start.elapsed() < deadline {
        *sequence += 1;
        attempts += 1;
        if probe(a, b, *sequence, Duration::from_millis(500)).await {
            if first.is_none() {
                first = Some(start.elapsed().as_secs_f64());
            }
            consecutive += 1;
            if consecutive == 10 {
                let result = serde_json::json!({"stage":name,"first_bidirectional_seconds":first,"qualified_seconds":start.elapsed().as_secs_f64(),"probe_pairs_sent":attempts,"consecutive_fresh_bidirectional_pairs":consecutive,
                    "a_paths":a.control.paths().await.unwrap(),"b_paths":b.control.paths().await.unwrap(),
                    "a_state":a.control.state().await.unwrap(),"b_state":b.control.state().await.unwrap()});
                eprintln!(
                    "RECOVERY {name}: first={:.3}s qualified={:.3}s attempts={attempts}",
                    first.unwrap(),
                    start.elapsed().as_secs_f64()
                );
                write_evidence(name, &result);
                return result;
            }
        } else {
            consecutive = 0;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let failure = serde_json::json!({"stage":name,"failed":true,"deadline_seconds":deadline.as_secs(),"a_state":a.control.state().await.unwrap(),"b_state":b.control.state().await.unwrap(),"a_metrics":a.control.status().await.unwrap(),"b_metrics":b.control.status().await.unwrap()});
    write_evidence(name, &failure);
    panic!(
        "{name}: no sustained bidirectional recovery within {deadline:?}; see recovery evidence"
    );
}

#[test]
fn automatic_transport_recovery_after_link_change() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(scenario());
        })
        .unwrap()
        .join()
        .unwrap();
}
async fn scenario() {
    let alice = NodeIdentity::generate_ed25519().unwrap();
    let bob = loop {
        let b = NodeIdentity::generate_ed25519().unwrap();
        if builtin_ipv4(PeerId::from_libp2p(b.peer_id.parse().unwrap()))
            != builtin_ipv4(PeerId::from_libp2p(alice.peer_id.parse().unwrap()))
        {
            break b;
        }
    };
    let backend = free_address();
    let alice_listener = free_address();
    let primary = Proxy::new(Ipv4Addr::new(127, 0, 0, 1), backend, Link::Up).await;
    let backup = Proxy::new(Ipv4Addr::new(127, 0, 0, 2), backend, Link::Down).await;
    let addresses = [&primary, &backup]
        .map(|p| format!("/ip4/{}/tcp/{}", p.address.ip(), p.address.port()))
        .to_vec();
    let mut a = Node::start(config(&alice, &bob, alice_listener, addresses));
    let mut b = Node::start(config(&bob, &alice, backend, vec![]));
    let mut sequence = 0;
    let mut stages = Vec::new();
    stages.push(
        stage(
            "initial_primary",
            &mut a,
            &mut b,
            &mut sequence,
            Duration::from_secs(45),
        )
        .await,
    );
    assert!(primary.accepted.load(Ordering::Relaxed) > 0);
    primary.set(Link::Down);
    backup.set(Link::Up);
    stages.push(
        stage(
            "hard_disconnect_to_changed_address",
            &mut a,
            &mut b,
            &mut sequence,
            Duration::from_secs(60),
        )
        .await,
    );
    assert!(
        backup.accepted.load(Ordering::Relaxed) > 0,
        "backup link was not used"
    );
    backup.set(Link::Down);
    tokio::time::sleep(Duration::from_millis(500)).await;
    sequence += 1;
    assert!(
        !probe(&mut a, &mut b, sequence, Duration::from_secs(2)).await,
        "traffic bypassed both disabled links"
    );
    primary.set(Link::Up);
    stages.push(
        stage(
            "return_after_complete_outage",
            &mut a,
            &mut b,
            &mut sequence,
            Duration::from_secs(60),
        )
        .await,
    );
    let backup_before = backup.accepted.load(Ordering::Relaxed);
    primary.set(Link::Blackhole);
    backup.set(Link::Up);
    stages.push(
        stage(
            "silent_blackhole_to_backup",
            &mut a,
            &mut b,
            &mut sequence,
            Duration::from_secs(90),
        )
        .await,
    );
    assert!(
        backup.accepted.load(Ordering::Relaxed) > backup_before,
        "silent-link recovery bypassed backup"
    );
    primary.set(Link::Down);
    let notification = a.control.network_changed().await.unwrap();
    stages.push(
        stage(
            "explicit_network_change_notification",
            &mut a,
            &mut b,
            &mut sequence,
            Duration::from_secs(60),
        )
        .await,
    );
    let report = serde_json::json!({"test":"automatic_transport_recovery_after_link_change", "transport":"real authenticated TCP on native OS loopback", "packet_io":"in-process packet channels, no TUN adapter", "physical_adapter_changed":false,
        "automatic_stages_require_manual_network_notification":false,"explicit_notification":notification,"primary":primary.snapshot(),"backup":backup.snapshot(),"stages":stages,
        "a_metrics":a.control.status().await.unwrap(),"b_metrics":b.control.status().await.unwrap()});
    let directory = std::env::var_os("P2P_VPN_RECOVERY_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from("target")
                .join(format!("connection-recovery-{}", std::process::id()))
        });
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!("Recovery evidence: {}", directory.display());
    for node in [&mut a, &mut b] {
        let _ = node.shutdown.take().unwrap().send(());
    }
    for node in [a, b] {
        tokio::time::timeout(Duration::from_secs(15), node.daemon)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    primary.set(Link::Stop);
    backup.set(Link::Stop);
    primary.task.await.unwrap();
    backup.task.await.unwrap();
}

fn write_evidence(name: &str, value: &serde_json::Value) {
    let directory = std::env::var_os("P2P_VPN_RECOVERY_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from("target")
                .join(format!("connection-recovery-{}", std::process::id()))
        });
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec_pretty(value).unwrap(),
    )
    .unwrap();
}
