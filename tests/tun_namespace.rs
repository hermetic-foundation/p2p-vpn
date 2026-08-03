use std::{
    env, fs,
    fs::File,
    io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use libp2p::swarm::SwarmEvent;
use p2p_vpn::{
    config::{Config, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig},
    identity::NodeIdentity,
    runtime::{
        forward::Forwarder,
        p2p::{HostConfig, build_node},
        runner,
        tun::{TunDevice, TunRuntimeConfig},
    },
};

const CHILD_ENV: &str = "P2P_VPN_TUN_E2E_MODE";
const TEST_NAME: &str = "tun_namespace_ping_crosses_two_node_overlay";

#[test]
#[ignore = "requires Linux user and network namespaces plus /dev/net/tun"]
fn tun_namespace_ping_crosses_two_node_overlay() {
    match env::var(CHILD_ENV).as_deref() {
        Ok("orchestrator") => run_orchestrator(),
        Ok("node") => run_node_child(),
        _ => reexec_orchestrator(),
    }
}

fn reexec_orchestrator() {
    let current_exe = env::current_exe().expect("current test binary");
    let output = command_output(
        "unshare",
        &[
            "--user",
            "--map-root-user",
            "--mount",
            "--net",
            current_exe.to_str().expect("test binary path is utf-8"),
            "--ignored",
            TEST_NAME,
            "--exact",
            "--nocapture",
        ],
        &[(CHILD_ENV, "orchestrator")],
        Duration::from_secs(90),
    )
    .expect("failed to execute unshare");

    assert_output_success("unshare tun e2e orchestrator", &output);
}

fn run_orchestrator() {
    let identity_a = NodeIdentity::generate_ed25519().expect("node A identity");
    let identity_b = NodeIdentity::generate_ed25519().expect("node B identity");
    let temp_dir = env::temp_dir().join(format!("p2p-vpn-tun-e2e-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    let start_a = temp_dir.join("start-a");
    let start_b = temp_dir.join("start-b");

    let config_b = node_config(
        "tun-e2e-b",
        "hse2eb",
        &identity_b,
        "/ip4/10.250.0.2/tcp/42102",
        peer_config(&identity_a, Some("/ip4/10.250.0.1/tcp/42101")),
    );
    let address_b = TunRuntimeConfig::from_config(&config_b)
        .expect("node B TUN config")
        .addresses
        .ipv4;

    let mut node_a = spawn_node("a", &identity_a, &identity_b, &temp_dir, &start_a);
    let mut node_b = spawn_node("b", &identity_b, &identity_a, &temp_dir, &start_b);
    wait_for_child_namespace(node_a.id());
    wait_for_child_namespace(node_b.id());
    configure_underlay(node_a.id(), node_b.id());
    ns_command(node_a.id(), "ping", &["-c", "1", "-W", "2", "10.250.0.2"]);

    fs::write(&start_b, b"start").expect("write node B start file");
    wait_for_file(&temp_dir.join("ready-b"));
    thread::sleep(Duration::from_secs(2));
    fs::write(&start_a, b"start").expect("write node A start file");
    wait_for_file(&temp_dir.join("ready-a"));
    thread::sleep(Duration::from_secs(4));
    let ping = ping_from_namespace(node_a.id(), "hse2ea", address_b);
    let initiator_addresses = ns_command_output(node_a.id(), "ip", &["addr", "show"]);
    let initiator_routes = ns_command_output(node_a.id(), "ip", &["route", "show", "table", "all"]);
    let responder_addresses = ns_command_output(node_b.id(), "ip", &["addr", "show"]);
    let responder_routes = ns_command_output(node_b.id(), "ip", &["route", "show", "table", "all"]);

    stop_child(&mut node_a);
    stop_child(&mut node_b);
    assert!(
        ping.status.success(),
        "overlay ping failed with {}\nstdout:\n{}\nstderr:\n{}\nnode-a ip addr:\n{}\nnode-a routes:\n{}\nnode-b ip addr:\n{}\nnode-b routes:\n{}\nnode-a log:\n{}\nnode-b log:\n{}",
        ping.status,
        String::from_utf8_lossy(&ping.stdout),
        String::from_utf8_lossy(&ping.stderr),
        String::from_utf8_lossy(&initiator_addresses.stdout),
        String::from_utf8_lossy(&initiator_routes.stdout),
        String::from_utf8_lossy(&responder_addresses.stdout),
        String::from_utf8_lossy(&responder_routes.stdout),
        read_log(&temp_dir.join("node-a.log")),
        read_log(&temp_dir.join("node-b.log"))
    );
    let _ = fs::remove_dir_all(temp_dir);
}

fn spawn_node(
    role: &str,
    local: &NodeIdentity,
    remote: &NodeIdentity,
    temp_dir: &Path,
    start_file: &Path,
) -> Child {
    let current_exe = env::current_exe().expect("current test binary");
    let log = File::create(temp_dir.join(format!("node-{role}.log"))).expect("create node log");
    let log_err = log.try_clone().expect("clone node log");
    Command::new("unshare")
        .args([
            "--net",
            current_exe.to_str().expect("test binary path is utf-8"),
            "--ignored",
            TEST_NAME,
            "--exact",
            "--nocapture",
        ])
        .env(CHILD_ENV, "node")
        .env("P2P_VPN_TUN_E2E_ROLE", role)
        .env("P2P_VPN_TUN_E2E_LOCAL_PEER", &local.peer_id)
        .env("P2P_VPN_TUN_E2E_LOCAL_KEY", &local.private_key)
        .env("P2P_VPN_TUN_E2E_REMOTE_PEER", &remote.peer_id)
        .env("P2P_VPN_TUN_E2E_TEMP", temp_dir)
        .env("P2P_VPN_TUN_E2E_START", start_file)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .spawn()
        .expect("spawn node namespace")
}

fn wait_for_child_namespace(pid: u32) {
    let namespace = PathBuf::from(format!("/proc/{pid}/ns/net"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if namespace.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("child namespace for pid {pid} did not appear");
}

fn configure_underlay(pid_a: u32, pid_b: u32) {
    run_command(
        "ip",
        &[
            "link", "add", "veth-a", "type", "veth", "peer", "name", "veth-b",
        ],
    );
    run_command(
        "ip",
        &["link", "set", "veth-a", "netns", &pid_a.to_string()],
    );
    run_command(
        "ip",
        &["link", "set", "veth-b", "netns", &pid_b.to_string()],
    );
    ns_command(pid_a, "ip", &["link", "set", "lo", "up"]);
    ns_command(pid_b, "ip", &["link", "set", "lo", "up"]);
    configure_sysctls(pid_a);
    configure_sysctls(pid_b);
    ns_command(
        pid_a,
        "ip",
        &["addr", "add", "10.250.0.1/24", "dev", "veth-a"],
    );
    ns_command(
        pid_b,
        "ip",
        &["addr", "add", "10.250.0.2/24", "dev", "veth-b"],
    );
    ns_command(pid_a, "ip", &["link", "set", "veth-a", "up"]);
    ns_command(pid_b, "ip", &["link", "set", "veth-b", "up"]);
}

fn configure_sysctls(pid: u32) {
    ns_command(pid, "sysctl", &["-w", "net.ipv4.conf.all.rp_filter=0"]);
    ns_command(pid, "sysctl", &["-w", "net.ipv4.conf.default.rp_filter=0"]);
    ns_command(pid, "sysctl", &["-w", "net.ipv4.icmp_echo_ignore_all=0"]);
}

fn configure_tun_sysctls(interface: &str) {
    run_command(
        "sysctl",
        &["-w", &format!("net.ipv4.conf.{interface}.rp_filter=0")],
    );
    run_command(
        "sysctl",
        &["-w", &format!("net.ipv4.conf.{interface}.accept_local=1")],
    );
}

fn run_node_child() {
    let role = required_env("P2P_VPN_TUN_E2E_ROLE");
    let local = NodeIdentity {
        peer_id: required_env("P2P_VPN_TUN_E2E_LOCAL_PEER"),
        private_key: required_env("P2P_VPN_TUN_E2E_LOCAL_KEY"),
    };
    let remote = NodeIdentity {
        peer_id: required_env("P2P_VPN_TUN_E2E_REMOTE_PEER"),
        private_key: String::new(),
    };
    let start_file = PathBuf::from(required_env("P2P_VPN_TUN_E2E_START"));
    let temp_dir = PathBuf::from(required_env("P2P_VPN_TUN_E2E_TEMP"));
    wait_for_file(&start_file);

    let (name, interface, listen, remote_address) = match role.as_str() {
        "a" => (
            "tun-e2e-a",
            "hse2ea",
            "/ip4/10.250.0.1/tcp/42101",
            Some("/ip4/10.250.0.2/tcp/42102"),
        ),
        "b" => ("tun-e2e-b", "hse2eb", "/ip4/10.250.0.2/tcp/42102", None),
        other => panic!("unknown node role {other}"),
    };
    let config = node_config(
        name,
        interface,
        &local,
        listen,
        peer_config(&remote, remote_address),
    );
    let runtime = TunRuntimeConfig::from_config(&config).expect("TUN config");
    let device = open_and_configure_tun(&runtime);
    configure_tun_sysctls(interface);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(run_ready_node(
            config,
            device,
            temp_dir.join(format!("ready-{role}")),
        ))
        .expect("node runtime");
}

async fn run_ready_node(
    config: Config,
    device: TunDevice,
    ready_file: PathBuf,
) -> Result<(), runner::RunnerError> {
    let mut node = build_node(HostConfig {
        identity: config.identity()?,
        mtu: config.interface.mtu,
        listen_addresses: config.listen_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
        relay_reservations: config.relay_reservation_multiaddrs()?,
        discovery: config.network.discovery,
    })?;
    wait_for_listen_address(&mut node).await;
    fs::write(ready_file, b"ready").expect("write ready file");
    let forwarder = Forwarder::from_config(&config)?;
    runner::run_node(
        node,
        forwarder,
        device,
        config.interface.mtu,
        config.queue,
        Some(Duration::from_secs(1)),
    )
    .await
}

async fn wait_for_listen_address(node: &mut p2p_vpn::runtime::p2p::P2pNode) {
    loop {
        if let SwarmEvent::NewListenAddr { .. } = node.swarm.select_next_some().await {
            return;
        }
    }
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("start file {} did not appear", path.display());
}

fn node_config(
    name: &str,
    interface: &str,
    identity: &NodeIdentity,
    listen_address: &str,
    peer: PeerConfig,
) -> Config {
    Config {
        network: NetworkConfig {
            name: name.to_owned(),
            local_peer: identity.peer_id.clone(),
            private_key: Some(identity.private_key.clone()),
            listen_addresses: vec![listen_address.to_owned()],
            bootstrap_peers: Vec::new(),
            discovery: p2p_vpn::config::DiscoveryConfig::default(),
            relay: p2p_vpn::config::RelayConfig::default(),
        },
        interface: InterfaceConfig {
            name: interface.to_owned(),
            mtu: 1280,
        },
        peers: vec![peer],
        queue: QueueConfig {
            max_packets_per_peer: 64,
            max_bytes_per_peer: 128 * 1024,
        },
    }
}

fn peer_config(identity: &NodeIdentity, address: Option<&str>) -> PeerConfig {
    PeerConfig {
        id: identity.peer_id.clone(),
        name: None,
        addresses: address.into_iter().map(str::to_owned).collect(),
        routes: Vec::new(),
    }
}

fn open_and_configure_tun(config: &TunRuntimeConfig) -> TunDevice {
    let device = TunDevice::open(config).expect("open TUN device");
    for command in config.route_commands() {
        let status = command.execute().expect("execute ip command");
        assert!(status.success(), "`{command}` exited with {status}");
    }
    device
}

fn ping_from_namespace(pid: u32, interface: &str, destination: Ipv4Addr) -> Output {
    let destination = destination.to_string();
    ns_command_output(
        pid,
        "ping",
        &["-c", "5", "-W", "2", "-I", interface, &destination],
    )
}

fn ns_command(pid: u32, program: &str, args: &[&str]) {
    let output = ns_command_output(pid, program, args);
    assert_output_success("nsenter", &output);
}

fn ns_command_output(pid: u32, program: &str, args: &[&str]) -> Output {
    let pid = pid.to_string();
    let mut nsenter_args = vec!["-t", pid.as_str(), "-n", program];
    nsenter_args.extend_from_slice(args);
    command_output("nsenter", &nsenter_args, &[], Duration::from_secs(20)).expect("execute nsenter")
}

fn run_command(program: &str, args: &[&str]) {
    let output = command_output(program, args, &[], Duration::from_secs(20))
        .unwrap_or_else(|error| panic!("failed to execute `{program}`: {error}"));
    assert_output_success(program, &output);
}

fn command_output(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Duration,
) -> io::Result<Output> {
    let mut command = Command::new(program);
    command.args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait_with_output();
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| format!("failed to read log: {error}"))
}

fn assert_output_success(context: &str, output: &Output) {
    assert!(
        output.status.success(),
        "`{context}` exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
