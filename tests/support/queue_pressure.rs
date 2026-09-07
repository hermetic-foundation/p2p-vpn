use std::{
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

pub fn capture(temp: &Path, pid_a: u32, pid_b: u32, destination: Ipv4Addr) {
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
    let log = File::create(temp.join("pressure-ping.log")).unwrap();
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
    let ping_log = fs::read_to_string(temp.join("pressure-ping.log")).unwrap();
    let transmitted: usize = ping_log
        .lines()
        .find_map(|line| line.split_once(" packets transmitted")?.0.parse().ok())
        .expect("ping transmission summary");
    assert!(transmitted > 0 && transmitted <= 3000);
    fs::write(
        temp.join("queue-pressure-partial.json"),
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
        if snapshot["a"]["queued_packets"] == 0 && snapshot["b"]["queued_packets"] == 0 {
            assert_eq!(snapshot["a"]["queued_bytes"], 0);
            assert_eq!(snapshot["b"]["queued_bytes"], 0);
            break;
        }
        assert!(
            Instant::now() < recovery_deadline,
            "queues did not drain after pressure"
        );
        thread::sleep(Duration::from_millis(250));
    }
    for (pid, interface, address) in [
        (pid_a, "hse2ea", destination),
        (pid_b, "hse2eb", NODE_A_LOCAL_ROUTE_ADDRESS),
    ] {
        let output = ns_command_output(
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
        assert_output_success("post-pressure ping", &output);
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("5 received"),
            "post-pressure ping lost packets"
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
    fs::write(temp.join("queue-pressure.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1, "before": before, "samples": samples, "after_load": after_load, "after": after,
        "packet_limit": 3000, "transmitted_packets": transmitted,
        "traffic_deadline_seconds": 20, "ping_payload_bytes": 1000, "interval_millis": 5,
        "underlay_rate_kbit": 64, "underlay_delay_millis": 50,
        "queue_packet_limit": 4, "queue_byte_limit": 8192,
    })).unwrap()).unwrap();
    eprintln!(
        "queue pressure report: {}",
        temp.join("queue-pressure.json").display()
    );
}
