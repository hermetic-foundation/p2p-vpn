use super::*;
use p2p_vpn::runtime::control_socket::query_status;
use serde_json::{Value, json};

pub const TEST_NAME: &str = "tun_namespace_automatic_discovery_recovers_after_link_changes";
pub(super) const PROFILE_ENV: &str = "P2P_VPN_TUN_E2E_RECOVERY_PROFILE";
const PRIVATE_PROTOCOL: &str = "/p2p-vpn/settling-e2e/kad/1";
const INITIAL_LAN: Duration = Duration::from_secs(120);
const RELAY_RECOVERY: Duration = Duration::from_secs(960);
const DIRECT_RECOVERY: Duration = Duration::from_secs(375);
const HEALTHY_DWELL: Duration = Duration::from_secs(30);
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
pub const WATCHDOG: Duration = Duration::from_secs(1_650);

fn private_profile() -> bool {
    match env::var(PROFILE_ENV).as_deref() {
        Err(env::VarError::NotPresent) | Ok("public") => false,
        Ok("private") => true,
        other => panic!("invalid {PROFILE_ENV}: {other:?}"),
    }
}

fn minimal_config(
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
    if role == "relay" {
        runtime.block_on(run_infrastructure(local, &temp));
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

async fn run_infrastructure(identity: NodeIdentity, temp: &Path) {
    let mut config = relay_config(&identity);
    config.network.name = "settling-e2e".to_owned();
    config.network.listen_addresses = vec!["/ip4/11.251.0.254/tcp/42200".to_owned()];
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
    fs::write(temp.join("ready-relay"), b"ready").unwrap();
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
            let state = self
                .runtime
                .block_on(query_state(&socket, Duration::from_secs(2)))
                .unwrap();
            kademlia_resources::validate(&state)
                .unwrap_or_else(|error| panic!("{stage} {role}: {error}"));
            assert_eq!(
                state_metric_count(&state, "kad_pairing_present"),
                Some(usize::from(private_profile()))
            );
            if !private_profile() {
                for (metric, maximum) in [
                    ("app_maintenance_queries", 1),
                    ("app_address_publication_queries", 1),
                    ("app_recovery_queries", 1),
                    ("app_recovery_query_peers_retained", 256),
                    ("app_recovery_dial_targets_retained", 8_192),
                ] {
                    assert!(
                        state_metric_count(&state, metric).expect(metric) <= maximum,
                        "owner count {metric} exceeded {maximum} at {stage} {role}"
                    );
                }
            }
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
            for log_role in ["a", "b", "relay"] {
                assert!(
                    fs::metadata(self.temp.join(format!("node-{log_role}.log")))
                        .unwrap()
                        .len()
                        < 32 * 1024 * 1024,
                    "node log reached its hard limit"
                );
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

    fn packet_gate(&self, expected_path: &str) {
        let before: [Vec<String>; 2] = std::array::from_fn(|index| {
            self.runtime
                .block_on(query_status(
                    &node_control_socket(self.temp, self.nodes[index].1),
                    Duration::from_secs(2),
                ))
                .unwrap()
        });
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
            assert_output_success("overlay ping", &output);
            assert!(
                complete_ping(&String::from_utf8_lossy(&output.stdout)),
                "{role}: expected exactly 5/5 replies"
            );
        }
        for (index, (_, role, _, _)) in self.nodes.iter().enumerate() {
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
            assert!(
                delta("inbound_accepted_packets") >= 5,
                "missing accepted TUN traffic"
            );
            if expected_path == "circuit_relay" {
                assert!(
                    delta("outbound_relay_stream_fallback_packets") >= 5,
                    "relay path did not carry test traffic"
                );
            }
        }
    }

    fn wait_path(&mut self, stage: &str, path: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            let states = self.states(stage);
            let ready = states
                .iter()
                .zip(&self.nodes)
                .all(|(state, (_, _, peer, _))| peer_path(state, peer, path));
            if ready {
                self.packet_gate(path);
            }
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
        let deadline = Instant::now() + HEALTHY_DWELL;
        while Instant::now() < deadline {
            let states = self.states(stage);
            assert!(
                states
                    .iter()
                    .zip(&self.nodes)
                    .all(|(state, (_, _, peer, _))| peer_path(state, peer, path)),
                "{stage}: healthy path regressed"
            );
            self.packet_gate(path);
            thread::sleep(SAMPLE_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
}

fn peer_path(state: &[String], transport: &str, path: &str) -> bool {
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
        })
}

fn complete_ping(output: &str) -> bool {
    output.lines().any(|line| {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        fields.get(..6) == Some(&["5", "packets", "transmitted,", "5", "received,", "0%"][..])
    })
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
    let hash = idle_sample::fingerprint().expect("test binary fingerprint");
    let local = NodeIdentity::generate_ed25519().unwrap();
    let remote = NodeIdentity::generate_ed25519().unwrap();
    let infra = NodeIdentity::generate_ed25519().unwrap();
    let temp = init_namespace_temp_dir(
        &env::temp_dir().join(format!("p2p-vpn-{TEST_NAME}")),
        TEST_NAME,
    );
    eprintln!("automatic recovery artifacts: {}", temp.display());
    let configurations = [
        minimal_config(&local, &remote, &infra, private),
        minimal_config(&remote, &local, &infra, private),
    ];
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
    for node in [&node_a, &node_b, &relay] {
        wait_for_child_namespace(node.id());
    }
    configure_network_move_underlay_with_prefix(relay.id(), node_a.id(), node_b.id(), "11.251.0");
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
    ] {
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
    run_command("ip", &["link", "set", "veth-mv-host", "down"]);
    for role in ["relay", "a", "b"] {
        fs::write(temp.join(format!("start-{role}")), b"start").unwrap();
    }
    wait_for_file_with_timeout(&temp.join("ready-relay"), Duration::from_secs(10));
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
    };
    let mut summary = json!({"schema_version": 1, "test": TEST_NAME, "profile": if private {"private"} else {"public"},
        "binary_sha256": hash,
        "acceptance_soak": false, "cycles_required": 1, "cycles_completed": 0, "outcome": "running",
        "deadlines_seconds": {"initial_lan": INITIAL_LAN.as_secs(), "relay_recovery": RELAY_RECOVERY.as_secs(), "direct_recovery": DIRECT_RECOVERY.as_secs(), "healthy_dwell": HEALTHY_DWELL.as_secs(), "watchdog": WATCHDOG.as_secs()}});
    fs::write(
        temp.join("recovery-summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observer.wait_path("initial-lan", "direct_udp_datagram", INITIAL_LAN);
        observer.dwell("initial-lan-healthy", "direct_udp_datagram");
        set_network_move_direct_link(node_a.id(), node_b.id(), false);
        run_command("ip", &["link", "set", "veth-mv-host", "up"]);
        underlay_ping(node_a.id(), "10.253.0.2", false);
        underlay_ping(node_b.id(), "10.253.0.1", false);
        underlay_ping(node_a.id(), "11.251.0.2", false);
        underlay_ping(node_b.id(), "11.251.0.1", false);
        observer.wait_path("lan-to-relay", "circuit_relay", RELAY_RECOVERY);
        observer.dwell("relay-healthy", "circuit_relay");
        set_network_move_direct_link(node_a.id(), node_b.id(), true);
        underlay_ping(node_a.id(), "10.253.0.2", true);
        underlay_ping(node_b.id(), "10.253.0.1", true);
        observer.wait_path("relay-to-lan", "direct_udp_datagram", DIRECT_RECOVERY);
        observer.dwell("returned-lan-healthy", "direct_udp_datagram");
        summary["cycles_completed"] = json!(1);
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
    drop((node_a, node_b, relay));
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
    if !keep_temp_artifacts() {
        fs::remove_dir_all(temp).unwrap();
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
}
