use super::*;
use p2p_vpn::runtime::control_socket::query_status;
use serde_json::json;
use std::os::unix::process::CommandExt as _;

#[path = "process_sample.rs"]
mod process_sample;

const TEST_NAME: &str = "tun_namespace_resource_cli_smoke";
const SUBJECT_ENV: &str = "P2P_VPN_RESOURCE_SUBJECT";

pub fn reexec() {
    let subject = fs::canonicalize(required_env(SUBJECT_ENV)).expect("subject executable");
    let harness = env::current_exe().expect("test executable");
    let output = namespace_orchestrator_output(
        &[
            harness.to_str().unwrap(),
            "--ignored",
            TEST_NAME,
            "--exact",
            "--nocapture",
        ],
        &[
            (CHILD_ENV, "orchestrator"),
            (SUBJECT_ENV, subject.to_str().unwrap()),
        ],
        Duration::from_secs(180),
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
            "--fsize=33554432:33554432",
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
    let subject = required_env(SUBJECT_ENV);
    let hash = Command::new("sha256sum").arg(&subject).output().unwrap();
    assert_output_success("subject fingerprint", &hash);
    let subject_sha256 = String::from_utf8(hash.stdout).unwrap();
    let subject_sha256 = subject_sha256.split_whitespace().next().unwrap();
    let harness_sha256 = idle_sample::fingerprint().unwrap();
    let private = recovery_soak::private_profile();
    let local = NodeIdentity::generate_ed25519().unwrap();
    let remote = NodeIdentity::generate_ed25519().unwrap();
    let infra = NodeIdentity::generate_ed25519().unwrap();
    let temp = init_namespace_temp_dir(
        &env::temp_dir().join("p2p-vpn-resource-cli-smoke"),
        TEST_NAME,
    );
    eprintln!("resource CLI smoke artifacts: {}", temp.display());
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
        TEST_NAME,
        "a",
        &local,
        None,
        None,
        &temp,
        &temp.join("start-a"),
    );
    let b = spawn_node(
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
