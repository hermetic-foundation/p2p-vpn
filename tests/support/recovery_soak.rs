use super::*;
use p2p_vpn::runtime::control_socket::query_status;
use serde_json::{Value, json};

#[path = "recovery_settling.rs"]
mod settling;

pub const TEST_NAME: &str = "tun_namespace_automatic_discovery_recovers_after_link_changes";
pub(super) const PROFILE_ENV: &str = "P2P_VPN_TUN_E2E_RECOVERY_PROFILE";
pub(super) const SOAK_ENV: &str = "P2P_VPN_TUN_E2E_RECOVERY_SOAK";
pub(super) const COLLISION_ENV: &str = "P2P_VPN_TUN_E2E_RECOVERY_COLLISION";
const PRIVATE_PROTOCOL: &str = "/p2p-vpn/settling-e2e/kad/1";
const INITIAL_LAN: Duration = Duration::from_secs(120);
const RELAY_RECOVERY: Duration = Duration::from_secs(960);
const DIRECT_RECOVERY: Duration = Duration::from_secs(375);
const HEALTHY_DWELL: Duration = Duration::from_secs(30);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
pub const WATCHDOG: Duration = Duration::from_secs(1_650);
const SOAK_MINIMUM: Duration = Duration::from_secs(1_800);
const FINAL_HEALTHY: Duration = Duration::from_secs(600);
const SETTLING_GRACE: Duration = Duration::from_secs(100);
const INFRASTRUCTURE_OUTAGE: Duration = Duration::from_secs(130);
const SOAK_CYCLES: usize = 5;
const SOAK_WATCHDOG: Duration = Duration::from_secs(8_060);

fn soak_requested() -> bool {
    match env::var(SOAK_ENV).as_deref() {
        Err(env::VarError::NotPresent) | Ok("0") => false,
        Ok("1") => true,
        other => panic!("invalid {SOAK_ENV}: {other:?}"),
    }
}

pub fn requested_watchdog() -> Duration {
    if soak_requested() {
        SOAK_WATCHDOG
    } else {
        WATCHDOG
    }
}

pub(super) fn private_profile() -> bool {
    match env::var(PROFILE_ENV).as_deref() {
        Err(env::VarError::NotPresent) | Ok("public") => false,
        Ok("private") => true,
        other => panic!("invalid {PROFILE_ENV}: {other:?}"),
    }
}

pub(super) fn minimal_config(
    local: &NodeIdentity,
    remote: &NodeIdentity,
    infra: &NodeIdentity,
    private: bool,
) -> Value {
    let mut value = json!({
        "network": {
            "name": "settling-e2e",
            "private_key": local.private_key,
            "bootstrap_peers": [{
                "id": infra.peer_id,
                "address": format!("/ip4/11.251.0.254/tcp/42200/p2p/{}", infra.peer_id),
            }],
        },
        "peers": [{"id": remote.peer_id}],
    });
    if private {
        value["network"]["discovery"] = json!({"kademlia_protocol": PRIVATE_PROTOCOL});
    }
    value
}

pub fn run_node() {
    // Enforce the log bound in the kernel, including while the orchestrator is waiting.
    run_command(
        "prlimit",
        &[
            "--pid",
            &std::process::id().to_string(),
            "--fsize=33554432:33554432",
        ],
    );
    let role = required_env("P2P_VPN_TUN_E2E_ROLE");
    let local = NodeIdentity {
        peer_id: required_env("P2P_VPN_TUN_E2E_LOCAL_PEER"),
        private_key: required_env("P2P_VPN_TUN_E2E_LOCAL_KEY"),
    };
    let temp = PathBuf::from(required_env("P2P_VPN_TUN_E2E_TEMP"));
    wait_for_file_with_timeout(
        &PathBuf::from(required_env("P2P_VPN_TUN_E2E_START")),
        Duration::from_secs(30),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    if matches!(role.as_str(), "relay" | "backup") {
        runtime.block_on(run_infrastructure(local, &temp, &role));
    } else {
        let config = read_child_config(&temp, &role);
        let tun = TunRuntimeConfig::from_config(&config).unwrap();
        let device = open_and_configure_tun(&tun);
        configure_tun_sysctls(&config.interface.name);
        runtime
            .block_on(runner::run_config_until(
                config,
                device,
                None,
                Some(node_control_socket(&temp, &role)),
                None,
                std::future::pending(),
            ))
            .expect("production runtime");
    }
}

async fn run_infrastructure(identity: NodeIdentity, temp: &Path, role: &str) {
    let mut config = relay_config(&identity);
    config.network.name = "settling-e2e".to_owned();
    let suffix = if role == "relay" { 254 } else { 253 };
    config.network.listen_addresses = vec![format!("/ip4/11.251.0.{suffix}/tcp/42200")];
    config.network.external_addresses = config.network.listen_addresses.clone();
    config.network.discovery = DiscoveryConfig {
        mdns: false,
        ..DiscoveryConfig::default()
    };
    if private_profile() {
        config.network.discovery.kademlia_protocol = PRIVATE_PROTOCOL.to_owned();
    }
    let mut node = build_node(&HostConfig {
        identity,
        network_name: config.network.name.clone(),
        membership_tag: config.membership_tag().unwrap(),
        mtu: config.effective_packet_mtu(),
        max_concurrent_control_streams: config.resources.control_stream_limit(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
        listen_addresses: config.listen_multiaddrs().unwrap(),
        external_addresses: config.external_multiaddrs().unwrap(),
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: Vec::new(),
        relay_server: true,
        relay_resources: config.network.relay.resources,
        resources: config.resources,
        discovery: config.network.discovery,
    })
    .unwrap();
    // This is the isolated routing/relay server, not an overlay peer or a recovery command.
    node.swarm
        .behaviour_mut()
        .kad
        .set_mode(Some(libp2p::kad::Mode::Server));
    if let Some(kad) = node.swarm.behaviour_mut().pairing_kad.as_mut() {
        kad.set_mode(Some(libp2p::kad::Mode::Server));
    }
    wait_for_listen_address(&mut node).await;
    fs::write(temp.join(format!("ready-{role}")), b"ready").unwrap();
    while let Some(event) = node.swarm.next().await {
        if let SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info,
            ..
        })) = &event
        {
            for address in &info.listen_addrs {
                node.swarm
                    .behaviour_mut()
                    .kad
                    .add_address(peer_id, address.clone());
                if let Some(kad) = node.swarm.behaviour_mut().pairing_kad.as_mut() {
                    kad.add_address(peer_id, address.clone());
                }
            }
        }
        eprintln!("{event:?}");
    }
}

struct Observer<'a> {
    temp: &'a Path,
    nodes: [(u32, &'a str, String, Ipv4Addr); 2],
    runtime: tokio::runtime::Runtime,
    started: Instant,
    process_starts: [Value; 2],
    configurations: [Vec<u8>; 2],
    samples: File,
    infrastructure: Vec<(u32, &'a str, Value)>,
}

impl Observer<'_> {
    fn states(&mut self, stage: &str) -> [Vec<String>; 2] {
        std::array::from_fn(|index| {
            let (pid, role, _, _) = &self.nodes[index];
            let process = idle_sample::process_observation(role, *pid, self.started);
            assert_eq!(
                process["start_ticks"], self.process_starts[index],
                "daemon restarted"
            );
            assert_eq!(
                fs::read(child_config_path(self.temp, role)).unwrap(),
                self.configurations[index],
                "configuration changed"
            );
            let socket = node_control_socket(self.temp, role);
            let mut state = self
                .runtime
                .block_on(query_state(&socket, Duration::from_secs(2)))
                .unwrap();
            kademlia_resources::validate(&state)
                .unwrap_or_else(|error| panic!("{stage} {role}: {error}"));
            assert_eq!(
                state_metric_count(&state, "kad_pairing_present"),
                Some(usize::from(private_profile()))
            );
            for (metric, maximum) in [
                ("app_maintenance_queries", 1),
                ("app_address_publication_queries", 1),
                ("app_recovery_queries", 1),
                ("app_recovery_query_peers_retained", 256),
                ("app_recovery_dial_targets_retained", 8_192),
                ("packet_plane_retiring_sessions", 1),
            ] {
                assert!(
                    state_metric_count(&state, metric).expect(metric) <= maximum,
                    "owner count {metric} exceeded {maximum} at {stage} {role}"
                );
            }
            assert!(
                state_metric_count(&state, "packet_plane_retiring_sessions").unwrap()
                    <= state_metric_count(&state, "packet_plane_sessions").unwrap(),
                "retiring session without a current owner at {stage} {role}"
            );
            for (metric, maximum) in [
                ("app_maintenance_oldest_pending_age_millis", 100_000),
                ("app_address_publication_oldest_pending_age_millis", 100_000),
                ("app_recovery_query_oldest_pending_age_millis", 70_000),
                ("app_packet_hello_oldest_pending_age_millis", 35_000),
            ] {
                assert!(
                    state_metric_count(&state, metric).expect(metric) <= maximum,
                    "stale owner {metric} at {stage} {role}"
                );
            }
            for (infra_pid, infra_role, start) in &self.infrastructure {
                assert_eq!(
                    idle_sample::process_observation(infra_role, *infra_pid, self.started)["start_ticks"],
                    *start,
                    "infrastructure restarted"
                );
            }
            for log_role in ["a", "b"]
                .into_iter()
                .chain(self.infrastructure.iter().map(|(_, role, _)| *role))
            {
                assert!(
                    fs::metadata(self.temp.join(format!("node-{log_role}.log")))
                        .unwrap()
                        .len()
                        < 32 * 1024 * 1024,
                    "node log reached its hard limit"
                );
            }
            let status = self
                .runtime
                .block_on(query_status(&socket, Duration::from_secs(2)))
                .unwrap();
            for metric in settling::QUIET_COUNTERS {
                if state_metric_count(&state, metric).is_none() {
                    state.push(format!(
                        "{metric} {}",
                        state_metric_count(&status, metric).expect(metric)
                    ));
                }
            }
            let record = json!({"stage": stage, "elapsed_seconds": self.started.elapsed().as_secs_f64(), "role": role, "process": process, "state": state});
            serde_json::to_writer(&mut self.samples, &record).unwrap();
            writeln!(self.samples).unwrap();
            self.samples.flush().unwrap();
            assert!(
                self.samples.metadata().unwrap().len() < 32 * 1024 * 1024,
                "sample log limit"
            );
            fs::write(
                self.temp.join(format!("latest-{role}.state")),
                state.join("\n"),
            )
            .unwrap();
            state
        })
    }

    fn packet_gate(&self, expected_path: &str) -> bool {
        let before: [Vec<String>; 2] = std::array::from_fn(|index| {
            self.runtime
                .block_on(query_status(
                    &node_control_socket(self.temp, self.nodes[index].1),
                    Duration::from_secs(2),
                ))
                .unwrap()
        });
        let mut delivered = true;
        for (pid, role, _, destination) in &self.nodes {
            let output = ns_command_output(
                *pid,
                "env",
                &[
                    "LC_ALL=C",
                    "ping",
                    "-n",
                    "-c",
                    "5",
                    "-i",
                    "0.2",
                    "-W",
                    "2",
                    "-I",
                    "pv0",
                    &destination.to_string(),
                ],
            );
            fs::write(
                self.temp.join(format!("latest-{role}.ping")),
                &output.stdout,
            )
            .unwrap();
            assert!(
                matches!(output.status.code(), Some(0 | 1)),
                "{role}: ping command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            delivered &=
                output.status.success() && complete_ping(&String::from_utf8_lossy(&output.stdout));
        }
        let mut path_unchanged = true;
        for (index, (_, role, peer, _)) in self.nodes.iter().enumerate() {
            let after = self
                .runtime
                .block_on(query_status(
                    &node_control_socket(self.temp, role),
                    Duration::from_secs(2),
                ))
                .unwrap();
            let delta = |name| {
                state_metric_count(&after, name)
                    .expect(name)
                    .checked_sub(state_metric_count(&before[index], name).expect(name))
                    .expect("counter regressed")
            };
            let accepted = delta("inbound_accepted_packets");
            if delivered {
                assert!(accepted >= 5, "missing accepted TUN traffic");
            }
            if expected_path == "circuit_relay" {
                let relayed = delta("outbound_relay_stream_fallback_packets");
                if delivered {
                    assert!(relayed >= 5, "relay path did not carry test traffic");
                }
            }
            if expected_path == "direct_udp_datagram" {
                // The legacy QUIC counter also counts owned UDP packet-plane sends.
                let datagrams = delta("outbound_quic_datagram_packets");
                let streams = delta("outbound_stream_fallback_packets");
                path_unchanged &= direct_udp_traffic(datagrams, streams);
                let state = self
                    .runtime
                    .block_on(query_state(
                        &node_control_socket(self.temp, role),
                        Duration::from_secs(2),
                    ))
                    .unwrap();
                path_unchanged &= peer_path(&state, peer, expected_path);
            }
        }
        delivered && path_unchanged
    }

    fn wait_path(&mut self, stage: &str, path: &str, budget: Duration) {
        self.wait_path_via(stage, path, budget, None);
    }

    fn wait_path_via(&mut self, stage: &str, path: &str, budget: Duration, relay: Option<&str>) {
        let deadline = Instant::now() + budget;
        loop {
            let states = self.states(stage);
            let ready = states
                .iter()
                .zip(&self.nodes)
                .all(|(state, (_, _, peer, _))| peer_path_via(state, peer, path, relay));
            // A stale selected path can survive until its probe timeout. Packet loss
            // during recovery is not success, but still has the original deadline.
            let ready = ready && self.packet_gate(path);
            assert!(
                Instant::now() < deadline,
                "{stage}: autonomous {path} recovery including packet gate exceeded {} seconds",
                budget.as_secs()
            );
            if ready {
                eprintln!(
                    "recovery stage={stage} path={path} elapsed={:.1}s",
                    self.started.elapsed().as_secs_f64()
                );
                return;
            }
            thread::sleep(SAMPLE_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn dwell(&mut self, stage: &str, path: &str) {
        self.dwell_until(stage, path, Instant::now() + HEALTHY_DWELL, None);
    }

    fn dwell_until(
        &mut self,
        stage: &str,
        path: &str,
        deadline: Instant,
        mut baseline: Option<&mut settling::Baseline>,
    ) {
        while Instant::now() < deadline {
            let states = self.states(stage);
            assert!(
                states
                    .iter()
                    .zip(&self.nodes)
                    .all(|(state, (_, _, peer, _))| peer_path(state, peer, path)),
                "{stage}: healthy path regressed"
            );
            if let Some(baseline) = baseline.as_deref_mut() {
                for (index, state) in states.iter().enumerate() {
                    baseline
                        .validate(index, state, self.started.elapsed())
                        .unwrap_or_else(|error| panic!("{stage} {}: {error}", self.nodes[index].1));
                }
            }
            assert!(
                self.packet_gate(path),
                "{stage}: packet gate failed delivery or selected-path recheck"
            );
            thread::sleep(SAMPLE_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    fn unavailable(&mut self) {
        let deadline = Instant::now() + INFRASTRUCTURE_OUTAGE;
        while Instant::now() < deadline {
            self.states("cycle-4-all-infrastructure-unavailable");
            thread::sleep(SAMPLE_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
}

fn peer_path(state: &[String], transport: &str, path: &str) -> bool {
    peer_path_via(state, transport, path, None)
}

fn peer_path_via(state: &[String], transport: &str, path: &str, relay: Option<&str>) -> bool {
    state
        .iter()
        .filter(|line| line.starts_with("peer state: "))
        .any(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            let field = |name| {
                fields
                    .windows(2)
                    .find_map(|pair| (pair[0] == name).then_some(pair[1]))
            };
            field("transport") == Some(transport)
                && field("validated") == Some("true")
                && field("selected_path") == Some(path)
                && relay.is_none_or(|relay| field("selected_path_relay_peer") == Some(relay))
        })
}

fn complete_ping(output: &str) -> bool {
    output.lines().any(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        fields.get(..6) == Some(&["5", "packets", "transmitted,", "5", "received,", "0%"][..])
    })
}

fn direct_udp_traffic(datagrams: usize, streams: usize) -> bool {
    datagrams >= 5 && streams == 0
}

fn underlay_ping(pid: u32, destination: &str, succeeds: bool) {
    let output = ns_command_output(pid, "ping", &["-n", "-c", "1", "-W", "1", destination]);
    assert_eq!(
        output.status.success(),
        succeeds,
        "unexpected underlay reachability for {destination}: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

pub fn run_orchestrator() {
    assert!(
        env_duration_override(ORCHESTRATOR_TIMEOUT_ENV).is_none(),
        "fixture deadlines cannot be overridden"
    );
    assert!(
        env::var_os(WAIT_TIMEOUT_SCALE_ENV).is_none(),
        "fixture deadlines cannot be scaled"
    );
    let private = private_profile();
    let soak = soak_requested();
    let collision = match env::var(COLLISION_ENV).as_deref() {
        Err(env::VarError::NotPresent) | Ok("0") => false,
        Ok("1") => true,
        other => panic!("invalid {COLLISION_ENV}: {other:?}"),
    };
    assert!(
        !collision || !soak,
        "collision diagnostic is a single cycle"
    );
    let hash = idle_sample::fingerprint().expect("test binary fingerprint");
    let local = NodeIdentity::generate_ed25519().unwrap();
    let remote = NodeIdentity::generate_ed25519().unwrap();
    let infra = NodeIdentity::generate_ed25519().unwrap();
    let backup_identity = soak.then(|| NodeIdentity::generate_ed25519().unwrap());
    let temp = init_namespace_temp_dir(
        &env::temp_dir().join(format!("p2p-vpn-{TEST_NAME}")),
        TEST_NAME,
    );
    eprintln!("automatic recovery artifacts: {}", temp.display());
    let mut configurations = [
        minimal_config(&local, &remote, &infra, private),
        minimal_config(&remote, &local, &infra, private),
    ];
    if let Some(backup) = &backup_identity {
        for config in &mut configurations {
            config["network"]["bootstrap_peers"]
                .as_array_mut()
                .unwrap()
                .push(json!({
                    "id": backup.peer_id,
                    "address": format!("/ip4/11.251.0.253/tcp/42200/p2p/{}", backup.peer_id),
                }));
        }
    }
    let configs: [Config; 2] = configurations
        .clone()
        .map(|value| serde_json::from_value(value).unwrap());
    for (index, role) in ["a", "b"].iter().enumerate() {
        configs[index].validate_runtime().unwrap();
        fs::write(
            child_config_path(&temp, role),
            serde_json::to_vec_pretty(&configurations[index]).unwrap(),
        )
        .unwrap();
    }
    let node_a = spawn_node(
        TEST_NAME,
        "a",
        &local,
        None,
        None,
        &temp,
        &temp.join("start-a"),
    );
    let node_b = spawn_node(
        TEST_NAME,
        "b",
        &remote,
        None,
        None,
        &temp,
        &temp.join("start-b"),
    );
    let relay = spawn_node(
        TEST_NAME,
        "relay",
        &infra,
        None,
        None,
        &temp,
        &temp.join("start-relay"),
    );
    let backup = backup_identity.as_ref().map(|identity| {
        spawn_node(
            TEST_NAME,
            "backup",
            identity,
            None,
            None,
            &temp,
            &temp.join("start-backup"),
        )
    });
    for node in [&node_a, &node_b, &relay]
        .into_iter()
        .chain(backup.as_ref())
    {
        wait_for_child_namespace(node.id());
    }
    configure_network_move_underlay_with_prefix(relay.id(), node_a.id(), node_b.id(), "11.251.0");
    if let Some(backup) = &backup {
        attach_veth_to_bridge(backup.id(), "bkup", "11.251.0.253/24", "br-mv");
    }
    // No router or IPv6 link-local detour may bypass the isolated bridge ports.
    run_command(
        "sysctl",
        &[
            "-w",
            "net.ipv4.ip_forward=0",
            "net.ipv6.conf.all.forwarding=0",
            "net.ipv6.conf.br-mv.disable_ipv6=1",
        ],
    );
    for (pid, interface) in [
        (node_a.id(), "veth-a"),
        (node_b.id(), "veth-b"),
        (relay.id(), "veth-mv"),
    ]
    .into_iter()
    .chain(backup.as_ref().map(|backup| (backup.id(), "veth-bkup")))
    {
        ns_command(
            pid,
            "sysctl",
            &[
                "-w",
                "net.ipv4.ip_forward=0",
                "net.ipv6.conf.all.forwarding=0",
                &format!("net.ipv6.conf.{interface}.disable_ipv6=1"),
            ],
        );
    }
    underlay_ping(node_a.id(), "11.251.0.2", false);
    underlay_ping(node_b.id(), "11.251.0.1", false);
    underlay_ping(node_a.id(), "11.251.0.254", true);
    underlay_ping(node_b.id(), "11.251.0.254", true);
    underlay_ping(node_a.id(), "10.253.0.2", true);
    underlay_ping(node_b.id(), "10.253.0.1", true);
    if backup.is_some() {
        underlay_ping(node_a.id(), "11.251.0.253", true);
        underlay_ping(node_b.id(), "11.251.0.253", true);
        run_command("ip", &["link", "set", "veth-bkup-host", "down"]);
    }
    run_command("ip", &["link", "set", "veth-mv-host", "down"]);
    for role in ["relay", "a", "b"]
        .into_iter()
        .chain(backup.as_ref().map(|_| "backup"))
    {
        fs::write(temp.join(format!("start-{role}")), b"start").unwrap();
    }
    wait_for_file_with_timeout(&temp.join("ready-relay"), Duration::from_secs(10));
    if backup.is_some() {
        wait_for_file_with_timeout(&temp.join("ready-backup"), Duration::from_secs(10));
    }
    for role in ["a", "b"] {
        wait_for_daemon_running(&temp, role);
    }
    let started = Instant::now();
    let mut observer = Observer {
        temp: &temp,
        nodes: [
            (
                node_a.id(),
                "a",
                remote.peer_id.clone(),
                TunRuntimeConfig::from_config(&configs[1])
                    .unwrap()
                    .addresses
                    .ipv4,
            ),
            (
                node_b.id(),
                "b",
                local.peer_id.clone(),
                TunRuntimeConfig::from_config(&configs[0])
                    .unwrap()
                    .addresses
                    .ipv4,
            ),
        ],
        runtime: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
        started,
        process_starts: [
            idle_sample::process_observation("a", node_a.id(), started)["start_ticks"].clone(),
            idle_sample::process_observation("b", node_b.id(), started)["start_ticks"].clone(),
        ],
        configurations: [
            fs::read(child_config_path(&temp, "a")).unwrap(),
            fs::read(child_config_path(&temp, "b")).unwrap(),
        ],
        samples: File::create(temp.join("recovery-samples.jsonl")).unwrap(),
        infrastructure: [(relay.id(), "relay")]
            .into_iter()
            .chain(backup.as_ref().map(|backup| (backup.id(), "backup")))
            .map(|(pid, role)| {
                (
                    pid,
                    role,
                    idle_sample::process_observation(role, pid, started)["start_ticks"].clone(),
                )
            })
            .collect(),
    };
    let mut summary = json!({"schema_version": 1, "test": TEST_NAME, "profile": if private {"private"} else {"public"},
        "binary_sha256": hash,
        "acceptance_soak": soak, "collision_diagnostic": collision, "cycles_required": if soak { SOAK_CYCLES } else { 1 }, "cycles_completed": 0, "outcome": "running",
        "deadlines_seconds": {"initial_lan": INITIAL_LAN.as_secs(), "relay_recovery": RELAY_RECOVERY.as_secs(), "direct_recovery": DIRECT_RECOVERY.as_secs(), "healthy_dwell": HEALTHY_DWELL.as_secs(), "watchdog": requested_watchdog().as_secs(), "primary_pool_drain": settling::PRIMARY_POOL_DRAIN_BUDGET.as_secs(),
            "soak_minimum": SOAK_MINIMUM.as_secs(), "final_healthy_minimum": FINAL_HEALTHY.as_secs(), "settling_grace": SETTLING_GRACE.as_secs(), "infrastructure_outage": INFRASTRUCTURE_OUTAGE.as_secs()}});
    fs::write(
        temp.join("recovery-summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observer.wait_path("initial-lan", "direct_udp_datagram", INITIAL_LAN);
        observer.dwell("initial-lan-healthy", "direct_udp_datagram");
        let mut subnet = 253;
        for cycle in 1..=if soak { SOAK_CYCLES } else { 1 } {
            set_network_move_direct_link(node_a.id(), node_b.id(), false);
            if matches!(cycle, 2 | 5) {
                let next = if cycle == 2 { 254 } else { 253 };
                change_lan_addresses(node_a.id(), node_b.id(), subnet, next);
                subnet = next;
            }
            if cycle == 1 {
                run_command("ip", &["link", "set", "veth-mv-host", "up"]);
            }
            if cycle == 3 {
                run_command("ip", &["link", "set", "veth-mv-host", "down"]);
                run_command("ip", &["link", "set", "veth-bkup-host", "up"]);
            }
            if cycle == 4 {
                run_command("ip", &["link", "set", "veth-bkup-host", "down"]);
                underlay_ping(node_a.id(), "11.251.0.253", false);
                underlay_ping(node_b.id(), "11.251.0.254", false);
                observer.unavailable();
                run_command("ip", &["link", "set", "veth-bkup-host", "up"]);
            }
            underlay_ping(node_a.id(), &format!("10.{subnet}.0.2"), false);
            underlay_ping(node_b.id(), &format!("10.{subnet}.0.1"), false);
            underlay_ping(node_a.id(), "11.251.0.2", false);
            underlay_ping(node_b.id(), "11.251.0.1", false);
            let relay_identity = if cycle >= 3 {
                backup_identity.as_ref().unwrap()
            } else {
                &infra
            };
            let relay_overlay =
                p2p_vpn::PeerId::from_libp2p(relay_identity.peer_id.parse().unwrap()).to_string();
            observer.wait_path_via(
                &format!("cycle-{cycle}-lan-to-relay"),
                "circuit_relay",
                RELAY_RECOVERY,
                Some(&relay_overlay),
            );
            observer.dwell(&format!("cycle-{cycle}-relay-healthy"), "circuit_relay");
            if collision {
                change_lan_addresses(node_a.id(), node_b.id(), subnet, 254);
                subnet = 254;
                for (pid, interface) in [(node_a.id(), "veth-dir-a"), (node_b.id(), "veth-dir-b")] {
                    ns_command(
                        pid,
                        "tc",
                        &[
                            "qdisc", "add", "dev", interface, "root", "netem", "delay", "100ms",
                        ],
                    );
                }
            }
            set_network_move_direct_link(node_a.id(), node_b.id(), true);
            underlay_ping(node_a.id(), &format!("10.{subnet}.0.2"), true);
            underlay_ping(node_b.id(), &format!("10.{subnet}.0.1"), true);
            observer.wait_path(
                &format!("cycle-{cycle}-relay-to-lan"),
                "direct_udp_datagram",
                DIRECT_RECOVERY,
            );
            observer.dwell(
                &format!("cycle-{cycle}-returned-lan-healthy"),
                "direct_udp_datagram",
            );
            summary["cycles_completed"] = json!(cycle);
            fs::write(
                temp.join("recovery-summary.json"),
                serde_json::to_vec_pretty(&summary).unwrap(),
            )
            .unwrap();
        }
        if soak {
            observer.dwell_until(
                "final-settling-grace",
                "direct_udp_datagram",
                Instant::now() + SETTLING_GRACE,
                None,
            );
            let baseline_states = observer.states("healthy-baseline");
            let healthy_started = Instant::now();
            let mut baseline = settling::Baseline::new(baseline_states, started.elapsed());
            let deadline = (healthy_started + FINAL_HEALTHY).max(started + SOAK_MINIMUM);
            observer.dwell_until(
                "final-healthy",
                "direct_udp_datagram",
                deadline,
                Some(&mut baseline),
            );
            summary["continuous_healthy_seconds"] = json!(healthy_started.elapsed().as_secs_f64());
            assert!(started.elapsed() >= SOAK_MINIMUM);
            assert!(healthy_started.elapsed() >= FINAL_HEALTHY);
        }
    }));
    // Capture while the daemons are alive, including a failure before path convergence.
    capture_daemon_snapshots(&temp, &["a", "b"]);
    summary["outcome"] = json!(if result.is_ok() { "passed" } else { "failed" });
    summary["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
    fs::write(
        temp.join("recovery-summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    drop(observer);
    drop((node_a, node_b, relay, backup));
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
    if !keep_temp_artifacts() {
        fs::remove_dir_all(temp).unwrap();
    }
}

fn change_lan_addresses(a: u32, b: u32, old: u8, new: u8) {
    for (pid, interface, suffix) in [(a, "veth-dir-a", 1), (b, "veth-dir-b", 2)] {
        ns_command(
            pid,
            "ip",
            &[
                "addr",
                "del",
                &format!("10.{old}.0.{suffix}/24"),
                "dev",
                interface,
            ],
        );
        ns_command(
            pid,
            "ip",
            &[
                "addr",
                "add",
                &format!("10.{new}.0.{suffix}/24"),
                "dev",
                interface,
            ],
        );
    }
}

#[test]
fn recovery_minimal_configs_serialize_peer_id_only() {
    let local = NodeIdentity::generate_ed25519().unwrap();
    let remote = NodeIdentity::generate_ed25519().unwrap();
    let infra = NodeIdentity::generate_ed25519().unwrap();
    for private in [false, true] {
        let mut value = minimal_config(&local, &remote, &infra, private);
        assert_eq!(value["peers"], json!([{"id": remote.peer_id}]));
        assert_eq!(
            value["network"]
                .as_object_mut()
                .unwrap()
                .remove("discovery"),
            private.then(|| json!({"kademlia_protocol": PRIVATE_PROTOCOL}))
        );
        assert_eq!(
            value,
            json!({
                "network": {
                    "name": "settling-e2e",
                    "private_key": local.private_key,
                    "bootstrap_peers": [{
                        "id": infra.peer_id,
                        "address": format!("/ip4/11.251.0.254/tcp/42200/p2p/{}", infra.peer_id),
                    }],
                },
                "peers": [{"id": remote.peer_id}],
            }),
            "private={private}"
        );
    }
}

#[test]
fn recovery_minimal_configs_retain_default_policies() {
    let local = NodeIdentity::generate_ed25519().unwrap();
    let remote = NodeIdentity::generate_ed25519().unwrap();
    let infra = NodeIdentity::generate_ed25519().unwrap();
    let defaults: Config = serde_json::from_value(json!({
        "network": {"name": "settling-e2e", "private_key": local.private_key},
    }))
    .unwrap();
    for private in [false, true] {
        let bytes = serde_json::to_vec(&minimal_config(&local, &remote, &infra, private)).unwrap();
        let config: Config = serde_json::from_slice(&bytes).unwrap();
        config.validate_runtime().unwrap();
        let mut expected = defaults.clone();
        expected.network.bootstrap_peers = serde_json::from_value(json!([{
            "id": infra.peer_id,
            "address": format!("/ip4/11.251.0.254/tcp/42200/p2p/{}", infra.peer_id),
        }]))
        .unwrap();
        expected.peers = serde_json::from_value(json!([{"id": remote.peer_id}])).unwrap();
        if private {
            expected.network.discovery.kademlia_protocol = PRIVATE_PROTOCOL.to_owned();
        }
        assert_eq!(config, expected, "private={private}");
        assert_eq!(
            serde_json::to_value(&config.peers).unwrap(),
            json!([{"id": remote.peer_id}])
        );
    }
}

#[test]
fn recovery_direct_udp_gate_requires_datagrams_without_stream_fallback() {
    assert!(direct_udp_traffic(5, 0));
    assert!(direct_udp_traffic(10, 0));
    assert!(!direct_udp_traffic(4, 0));
    assert!(!direct_udp_traffic(0, 5));
    assert!(!direct_udp_traffic(5, 1));
}

#[test]
fn recovery_packet_and_peer_gates_reject_partial_or_unrelated_success() {
    assert!(complete_ping(
        "5 packets transmitted, 5 received, 0% packet loss, time 801ms"
    ));
    assert!(!complete_ping(
        "5 packets transmitted, 4 received, 20% packet loss, time 801ms"
    ));
    assert!(!complete_ping(
        "15 packets transmitted, 15 received, 0% packet loss"
    ));
    let state = vec![
        "peer state: overlay transport remote validated true selected_path circuit_relay"
            .to_owned(),
    ];
    assert!(peer_path(&state, "remote", "circuit_relay"));
    assert!(!peer_path(&state, "different", "circuit_relay"));
    assert!(!peer_path(&state, "remote", "direct_udp_datagram"));
    assert!(!peer_path(
        &[state[0].replace("validated true", "validated false")],
        "remote",
        "circuit_relay"
    ));
    let via = vec![format!("{} selected_path_relay_peer replacement", state[0])];
    assert!(peer_path_via(
        &via,
        "remote",
        "circuit_relay",
        Some("replacement")
    ));
    assert!(!peer_path_via(
        &via,
        "remote",
        "circuit_relay",
        Some("stale")
    ));
    assert!(!peer_path_via(
        &state,
        "remote",
        "circuit_relay",
        Some("replacement")
    ));
}

#[test]
fn recovery_soak_budget_preserves_all_stage_deadlines() {
    let stage_total = INITIAL_LAN
        + HEALTHY_DWELL
        + (RELAY_RECOVERY + DIRECT_RECOVERY + HEALTHY_DWELL * 2) * SOAK_CYCLES as u32
        + INFRASTRUCTURE_OUTAGE
        + SETTLING_GRACE
        + FINAL_HEALTHY
        + Duration::from_secs(105);
    assert_eq!(SOAK_WATCHDOG, stage_total);
    assert_eq!(SOAK_MINIMUM, Duration::from_secs(1_800));
    assert_eq!(FINAL_HEALTHY, Duration::from_secs(600));
    for suffix in ["mv", "a", "b", "bkup"] {
        assert!(format!("veth-{suffix}-host").len() <= 15);
    }
}
