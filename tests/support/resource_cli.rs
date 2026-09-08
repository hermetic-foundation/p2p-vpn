use super::*;
use p2p_vpn::runtime::control_socket::query_status;
use serde_json::json;
use std::os::unix::process::CommandExt as _;

#[path = "process_sample.rs"]
mod process_sample;
#[path = "resource_protocol.rs"]
mod protocol;
#[path = "resource_workload.rs"]
mod resource_workload;

const TEST_NAME: &str = "tun_namespace_resource_cli_smoke";
const SUBJECT_ENV: &str = "P2P_VPN_RESOURCE_SUBJECT";
const WORKLOAD_TEST: &str = "tun_namespace_resource_workload";

#[test]
#[ignore = "creates a new private identity file at P2P_VPN_RESOURCE_KEYS for workload preflight"]
fn generate_pair_keys() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let keys: [String; 3] =
        std::array::from_fn(|_| NodeIdentity::generate_ed25519().unwrap().private_key);
    let path = PathBuf::from(required_env("P2P_VPN_RESOURCE_KEYS"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    serde_json::to_writer(&mut file, &keys).unwrap();
    file.write_all(b"\n").unwrap();
    eprintln!("fixed pair keys: {}", path.display());
}

#[test]
#[ignore = "isolated calibration of ping offered rate with and without reply loss"]
fn calibrate_ping_rate() {
    const NAME: &str = "resource_cli::calibrate_ping_rate";
    if env::var(CHILD_ENV).as_deref() != Ok("orchestrator") {
        let executable = env::current_exe().unwrap();
        let output = namespace_orchestrator_output(
            &[
                executable.to_str().unwrap(),
                "--ignored",
                "--exact",
                NAME,
                "--nocapture",
            ],
            &[(CHILD_ENV, "orchestrator")],
            Duration::from_secs(60),
        )
        .unwrap();
        assert_output_success("ping calibration namespace", &output);
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    run_command("ip", &["link", "set", "lo", "up"]);
    let cases = [
        ("0%", "1", "0.005"),
        ("100%", "1", "0.005"),
        ("0%", "2", "0.005"),
        ("100%", "2", "0.005"),
    ]
    .into_iter()
    .chain((0..3).flat_map(|_| {
        [
            ("0%", "3", "0.005"),
            ("100%", "3", "0.005"),
            ("0%", "3", "0.02"),
            ("100%", "3", "0.02"),
        ]
    }));
    for (loss, preload, interval) in cases {
        run_command(
            "tc",
            &[
                "qdisc", "replace", "dev", "lo", "root", "netem", "loss", loss,
            ],
        );
        let mut arguments = vec![
            "-q",
            "-n",
            "-i",
            interval,
            "-s",
            "1000",
            "-w",
            "2",
            "127.0.0.1",
        ];
        arguments.splice(0..0, ["-l", preload]);
        let output = command_output(
            "ping",
            &arguments,
            &[("LC_ALL", "C")],
            Duration::from_secs(5),
        )
        .unwrap();
        eprintln!(
            "loss={loss} preload={preload} interval={interval} exit={} {}",
            output.status,
            String::from_utf8_lossy(&output.stdout)
        );
        if preload == "3" {
            let (sent, received) =
                resource_workload::ping_counts(&String::from_utf8_lossy(&output.stdout))
                    .expect("calibration summary");
            let nominal = if interval == "0.005" { 400 } else { 100 };
            assert!(
                (nominal * 98 / 100..=nominal + 3).contains(&sent),
                "offered rate mismatch: {sent}/{nominal}"
            );
            assert_eq!(received, if loss == "0%" { sent } else { 0 });
        }
    }
}

pub fn reexec() {
    reexec_test(TEST_NAME, Duration::from_secs(180));
}

pub fn reexec_workload() {
    reexec_test(
        WORKLOAD_TEST,
        Duration::from_secs(protocol::WATCHDOG_SECONDS),
    );
}

fn reexec_test(test_name: &str, timeout: Duration) {
    let subject = fs::canonicalize(required_env(SUBJECT_ENV)).expect("subject executable");
    let harness = env::current_exe().expect("test executable");
    let output = namespace_orchestrator_output(
        &[
            harness.to_str().unwrap(),
            "--ignored",
            test_name,
            "--exact",
            "--nocapture",
        ],
        &[
            (CHILD_ENV, "orchestrator"),
            (SUBJECT_ENV, subject.to_str().unwrap()),
        ],
        timeout,
    )
    .expect("resource smoke namespace");
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    assert_output_success("resource CLI smoke", &output);
}

pub fn run_node() {
    let role = required_env("P2P_VPN_TUN_E2E_ROLE");
    if role == "relay" {
        recovery_soak::run_node();
        return;
    }
    let temp = PathBuf::from(required_env("P2P_VPN_TUN_E2E_TEMP"));
    wait_for_file_with_timeout(
        &PathBuf::from(required_env("P2P_VPN_TUN_E2E_START")),
        Duration::from_secs(30),
    );
    run_command(
        "prlimit",
        &[
            "--pid",
            &std::process::id().to_string(),
            &if env::var_os("P2P_VPN_RESOURCE_WORKLOAD").is_some() {
                format!("--fsize={0}:{0}", protocol::ENDPOINT_LOG_BYTES)
            } else {
                "--fsize=33554432:33554432".to_owned()
            },
        ],
    );
    // exec preserves the namespace child's PID, so /proc samples belong to the CLI.
    let error = Command::new(required_env(SUBJECT_ENV))
        .args(["up", "--config"])
        .arg(child_config_path(&temp, &role))
        .arg("--control-socket")
        .arg(node_control_socket(&temp, &role))
        .env("TOKIO_WORKER_THREADS", "2")
        .env_remove("RUST_LOG")
        .exec();
    panic!("subject exec failed: {error}");
}

pub fn smoke() {
    run(None);
}

pub fn workload() {
    let workload = serde_json::from_value(json!(required_env("P2P_VPN_RESOURCE_WORKLOAD")))
        .expect("workload must be idle, traffic, recovery, or pressure");
    run(Some(workload));
}

fn run(workload: Option<protocol::Workload>) {
    let test_name = if workload.is_some() {
        WORKLOAD_TEST
    } else {
        TEST_NAME
    };
    let subject = required_env(SUBJECT_ENV);
    let hash = Command::new("sha256sum").arg(&subject).output().unwrap();
    assert_output_success("subject fingerprint", &hash);
    let subject_sha256 = String::from_utf8(hash.stdout).unwrap();
    let subject_sha256 = subject_sha256.split_whitespace().next().unwrap();
    let harness_sha256 = idle_sample::fingerprint().unwrap();
    let private = recovery_soak::private_profile();
    let [local, remote, infra] = if workload.is_some() {
        let keys: [String; 3] =
            serde_json::from_slice(&fs::read(required_env("P2P_VPN_RESOURCE_KEYS")).unwrap())
                .expect("three fixed private keys in JSON array");
        keys.map(|key| NodeIdentity::from_private_key(&key).unwrap())
    } else {
        std::array::from_fn(|_| NodeIdentity::generate_ed25519().unwrap())
    };
    let temp = init_namespace_temp_dir(
        &env::temp_dir().join("p2p-vpn-resource-cli-smoke"),
        test_name,
    );
    eprintln!("resource CLI smoke artifacts: {}", temp.display());
    if let Some(pointer) = env::var_os("P2P_VPN_RESOURCE_POINTER") {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(pointer)
            .unwrap();
        serde_json::to_writer(file, &json!({"artifact_dir": temp})).unwrap();
    }
    let values = [
        recovery_soak::minimal_config(&local, &remote, &infra, private),
        recovery_soak::minimal_config(&remote, &local, &infra, private),
    ];
    let configs: [Config; 2] = values
        .clone()
        .map(|value| serde_json::from_value(value).unwrap());
    for (role, value) in ["a", "b"].into_iter().zip(&values) {
        fs::write(
            child_config_path(&temp, role),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
    let a = spawn_node(
        test_name,
        "a",
        &local,
        None,
        None,
        &temp,
        &temp.join("start-a"),
    );
    let b = spawn_node(
        test_name,
        "b",
        &remote,
        None,
        None,
        &temp,
        &temp.join("start-b"),
    );
    let relay = spawn_node(
        test_name,
        "relay",
        &infra,
        None,
        None,
        &temp,
        &temp.join("start-relay"),
    );
    for child in [&a, &b, &relay] {
        wait_for_child_namespace(child.id());
    }
    configure_network_move_underlay_with_prefix(relay.id(), a.id(), b.id(), "11.251.0");
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
        (a.id(), "veth-a"),
        (b.id(), "veth-b"),
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
    let started = Instant::now();
    for role in ["relay", "a", "b"] {
        fs::write(temp.join(format!("start-{role}")), b"start").unwrap();
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let deadline = started + Duration::from_secs(120);
    let mut evidence = File::create(temp.join("observations.jsonl")).unwrap();
    if let Some(workload) = workload {
        let result = resource_workload::run(
            resource_workload::Context {
                nodes: [a.id(), b.id()],
                infrastructure: relay.id(),
                configs: &configs,
                temp: &temp,
                runtime: &runtime,
                evidence: &mut evidence,
                started,
            },
            workload,
        );
        fs::write(
            temp.join("workload.json"),
            serde_json::to_vec_pretty(&json!({
                "acceptance_measurement": env::var("P2P_VPN_RESOURCE_ACCEPTANCE").as_deref() == Ok("1"),
                "protocol_version": protocol::VERSION,
                "workload": workload,
                "stages": workload.stages(),
                "profile": if private { "private" } else { "public" },
                "subject": subject, "subject_sha256": subject_sha256,
                "harness_sha256": harness_sha256,
                "result": result,
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(result.is_ok(), "workload preflight failed: {result:?}");
        return;
    }
    loop {
        let mut healthy = true;
        for (index, (child, role)) in [(&a, "a"), (&b, "b")].into_iter().enumerate() {
            let state = runtime.block_on(query_state(
                &node_control_socket(&temp, role),
                Duration::from_secs(1),
            ));
            let status = runtime.block_on(query_status(
                &node_control_socket(&temp, role),
                Duration::from_secs(1),
            ));
            let process = process_sample::capture(child.id(), started);
            let destination = TunRuntimeConfig::from_config(&configs[1 - index])
                .unwrap()
                .addresses
                .ipv4;
            let ping = ns_command_output(
                child.id(),
                "ping",
                &[
                    "-n",
                    "-c",
                    "1",
                    "-W",
                    "1",
                    "-I",
                    &configs[index].interface.name,
                    &destination.to_string(),
                ],
            );
            healthy &= state.is_ok() && status.is_ok() && process.is_ok() && ping.status.success();
            let observation = serde_json::to_vec(&json!({
                "role": role,
                "elapsed_seconds": started.elapsed().as_secs_f64(),
                "process": process.as_ref().ok(),
                "process_error": process.as_ref().err().map(ToString::to_string),
                "state": state.as_ref().ok(),
                "state_error": state.as_ref().err().map(|error| format!("{error:?}")),
                "status": status.as_ref().ok(),
                "status_error": status.as_ref().err().map(|error| format!("{error:?}")),
                "ping_success": ping.status.success(),
            }))
            .unwrap();
            assert!(
                evidence.metadata().unwrap().len() + observation.len() as u64 + 1 <= 33_554_432,
                "smoke observation limit exceeded"
            );
            evidence.write_all(&observation).unwrap();
            evidence.write_all(b"\n").unwrap();
        }
        if healthy {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "CLI smoke failed to deliver bidirectionally within 120s; see {}",
            temp.display()
        );
        thread::sleep(Duration::from_secs(1));
    }
    fs::write(
        temp.join("smoke.json"),
        serde_json::to_vec_pretty(&json!({
            "acceptance_measurement": false,
            "profile": if private { "private" } else { "public" },
            "subject": subject,
            "subject_sha256": subject_sha256,
            "harness_sha256": harness_sha256,
            "elapsed_seconds": started.elapsed().as_secs_f64(),
            "outcome": "completed",
        }))
        .unwrap(),
    )
    .unwrap();
    // Keep compact evidence; namespace child RAII reaps the endpoints and helper.
}
