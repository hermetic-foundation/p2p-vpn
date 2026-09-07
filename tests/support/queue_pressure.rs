use std::{
    env,
    fs::{self, File},
    net::Ipv4Addr,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use super::{
    NODE_A_LOCAL_ROUTE_ADDRESS, NamespaceChild, assert_output_success, idle_sample,
    node_control_socket, ns_command, ns_command_output, state_metric_count, wait_for_selected_path,
};

pub const ROUNDS_ENV: &str = "P2P_VPN_TUN_E2E_PRESSURE_ROUNDS";

fn parse_rounds(value: &str) -> Option<u64> {
    value.parse().ok().filter(|rounds| (1..=5).contains(rounds))
}

pub fn requested_rounds() -> u64 {
    match env::var(ROUNDS_ENV) {
        Err(env::VarError::NotPresent) => 1,
        Ok(value) => parse_rounds(&value).expect("pressure rounds must be 1..=5"),
        Err(error) => panic!("invalid {ROUNDS_ENV}: {error}"),
    }
}

pub fn capture(temp: &Path, pid_a: u32, pid_b: u32, destination: Ipv4Addr) {
    let rounds = requested_rounds();
    let mut series = Vec::<serde_json::Value>::new();
    for round in 1..=rounds {
        let output = if rounds == 1 {
            temp.to_path_buf()
        } else {
            temp.join(format!("round-{round}"))
        };
        fs::create_dir_all(&output).expect("pressure artifact directory");
        let report = capture_round(temp, &output, pid_a, pid_b, destination);
        if let Some(first) = series.first() {
            for role in ["a", "b"] {
                assert_eq!(
                    first["before"][role]["process"]["start_ticks"],
                    report["after"][role]["process"]["start_ticks"],
                    "daemon restarted between rounds"
                );
            }
        }
        series.push(serde_json::json!({
            "round": round, "before": report["before"], "after_load": report["after_load"],
            "after": report["after"], "transmitted_packets": report["transmitted_packets"],
        }));
        fs::write(
            temp.join("queue-pressure-series.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1, "requested_rounds": rounds, "completed_rounds": series.len(),
                "complete": round == rounds, "rounds": series,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}

fn capture_round(
    temp: &Path,
    output: &Path,
    pid_a: u32,
    pid_b: u32,
    destination: Ipv4Addr,
) -> serde_json::Value {
    let started = Instant::now();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let observe = || {
        let mut nodes = serde_json::Map::new();
        for (role, pid) in [("a", pid_a), ("b", pid_b)] {
            let lines = runtime
                .block_on(p2p_vpn::runtime::control_socket::query_status(
                    &node_control_socket(temp, role),
                    Duration::from_secs(2),
                ))
                .unwrap();
            let state = runtime
                .block_on(p2p_vpn::runtime::control_socket::query_state(
                    &node_control_socket(temp, role),
                    Duration::from_secs(2),
                ))
                .unwrap();
            let packets =
                state_metric_count(&lines, "queue_queued_packets").expect("queue packets");
            let bytes = state_metric_count(&lines, "queue_queued_bytes").expect("queue bytes");
            assert!(
                packets <= 4 && bytes <= 8192,
                "queue exceeded configured bound"
            );
            nodes.insert(role.to_owned(), serde_json::json!({
                "process": idle_sample::process_observation(role, pid, started),
                "queued_packets": packets, "queued_bytes": bytes,
                "dropped_packets": state_metric_count(&lines, "queue_dropped_packets").expect("queue drops"),
                "expired_packets": state_metric_count(&lines, "queue_expired_packets").expect("queue expiry"),
                "stream_in_flight": state_metric_count(&state, "packet_stream_fallback_in_flight").expect("stream owners"),
                "outbound_failures": state_metric_count(&lines, "outbound_failures").expect("outbound failures"),
                "inbound_dropped_packets": state_metric_count(&lines, "inbound_dropped_packets").expect("inbound drops"),
            }));
        }
        serde_json::Value::Object(nodes)
    };
    let before = observe();
    ns_command(
        pid_a,
        "tc",
        &[
            "qdisc", "add", "dev", "veth-a", "root", "netem", "delay", "50ms", "rate", "64kbit",
            "limit", "16",
        ],
    );
    let log = File::create(output.join("pressure-ping.log")).unwrap();
    let mut traffic = NamespaceChild {
        child: Command::new("nsenter")
            .env("LC_ALL", "C")
            .args([
                "-t",
                &pid_a.to_string(),
                "-n",
                "ping",
                "-q",
                "-i",
                "0.005",
                "-s",
                "1000",
                "-c",
                "3000",
                "-w",
                "20",
                "-W",
                "1",
                "-I",
                "hse2ea",
                &destination.to_string(),
            ])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    };
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut samples = Vec::new();
    loop {
        samples.push(observe());
        if let Some(status) = traffic.child.try_wait().unwrap() {
            assert!(
                matches!(status.code(), Some(0 | 1)),
                "ping setup failed: {status}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "paced traffic exceeded deadline");
        thread::sleep(Duration::from_millis(250));
    }
    ns_command(pid_a, "tc", &["qdisc", "del", "dev", "veth-a", "root"]);
    let after_load = observe();
    let ping_log = fs::read_to_string(output.join("pressure-ping.log")).unwrap();
    let transmitted: usize = ping_log
        .lines()
        .find_map(|line| line.split_once(" packets transmitted")?.0.parse().ok())
        .expect("ping transmission summary");
    assert!(transmitted > 0 && transmitted <= 3000);
    fs::write(
        output.join("queue-pressure-partial.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "stage": "load_completed", "before": before, "samples": samples,
            "after_load": after_load, "transmitted_packets": transmitted,
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        after_load["a"]["dropped_packets"].as_u64().unwrap()
            > before["a"]["dropped_packets"].as_u64().unwrap(),
        "workload did not exercise queue drops"
    );
    wait_for_selected_path(temp, "a", "direct_tcp_stream");
    wait_for_selected_path(temp, "b", "direct_tcp_stream");
    let recovery_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let snapshot = observe();
        if ["a", "b"].into_iter().all(|role| {
            snapshot[role]["queued_packets"] == 0 && snapshot[role]["stream_in_flight"] == 0
        }) {
            assert_eq!(snapshot["a"]["queued_bytes"], 0);
            assert_eq!(snapshot["b"]["queued_bytes"], 0);
            break;
        }
        assert!(
            Instant::now() < recovery_deadline,
            "queues and stream requests did not drain after pressure"
        );
        thread::sleep(Duration::from_millis(250));
    }
    for (role, pid, interface, address) in [
        ("a", pid_a, "hse2ea", destination),
        ("b", pid_b, "hse2eb", NODE_A_LOCAL_ROUTE_ADDRESS),
    ] {
        let before_ping = observe();
        let ping_output = ns_command_output(
            pid,
            "env",
            &[
                "LC_ALL=C",
                "ping",
                "-c",
                "5",
                "-W",
                "2",
                "-I",
                interface,
                &address.to_string(),
            ],
        );
        fs::write(
            output.join(format!("recovery-ping-{role}.stdout")),
            &ping_output.stdout,
        )
        .unwrap();
        fs::write(
            output.join(format!("recovery-ping-{role}.stderr")),
            &ping_output.stderr,
        )
        .unwrap();
        fs::write(
            output.join(format!("recovery-ping-{role}.json")),
            serde_json::to_vec_pretty(&serde_json::json!({
                "before": before_ping,
                "after": observe(),
                "exit_code": ping_output.status.code(),
            }))
            .unwrap(),
        )
        .unwrap();
        assert_output_success("post-pressure ping", &ping_output);
        assert!(
            String::from_utf8_lossy(&ping_output.stdout).contains("5 received"),
            "post-pressure ping from {role} lost packets: {}",
            String::from_utf8_lossy(&ping_output.stdout)
        );
    }
    let after = observe();
    for role in ["a", "b"] {
        assert_eq!(
            before[role]["process"]["start_ticks"],
            after[role]["process"]["start_ticks"]
        );
        assert_eq!(after[role]["queued_packets"], 0);
    }
    let report = serde_json::json!({
        "schema_version": 1, "before": before, "samples": samples, "after_load": after_load, "after": after,
        "packet_limit": 3000, "transmitted_packets": transmitted,
        "traffic_deadline_seconds": 20, "ping_payload_bytes": 1000, "interval_millis": 5,
        "underlay_rate_kbit": 64, "underlay_delay_millis": 50,
        "queue_packet_limit": 4, "queue_byte_limit": 8192,
    });
    fs::write(
        output.join("queue-pressure.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!(
        "queue pressure report: {}",
        output.join("queue-pressure.json").display()
    );
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_are_bounded_and_invalid_values_are_rejected() {
        for value in ["", "0", "6", "-1", "1.5", "no", "18446744073709551616"] {
            assert_eq!(parse_rounds(value), None);
        }
        for rounds in 1..=5 {
            assert_eq!(parse_rounds(&rounds.to_string()), Some(rounds));
        }
    }
}
