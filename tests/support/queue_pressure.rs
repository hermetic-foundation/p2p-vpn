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
pub const LIMIT_ENV: &str = "P2P_VPN_TUN_E2E_PRESSURE_LIMIT";
pub const INITIATOR_ENV: &str = "P2P_VPN_TUN_E2E_PRESSURE_INITIATOR";
pub const METRICS_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub name: &'static str,
    pub packets: usize,
    pub bytes: usize,
}

fn parse_limits(value: Option<&str>) -> Option<Limits> {
    match value {
        None | Some("packets") => Some(Limits {
            name: "packets",
            packets: 4,
            bytes: 8192,
        }),
        Some("bytes") => Some(Limits {
            name: "bytes",
            packets: 16,
            bytes: 4096,
        }),
        _ => None,
    }
}

pub fn requested_limits() -> Limits {
    let value = match env::var(LIMIT_ENV) {
        Ok(value) => Some(value),
        Err(env::VarError::NotPresent) => None,
        Err(error) => panic!("invalid {LIMIT_ENV}: {error}"),
    };
    parse_limits(value.as_deref()).expect("pressure limit must be packets or bytes")
}

fn parse_initiator(value: Option<&str>) -> Option<Option<bool>> {
    match value {
        None => Some(None),
        Some("a") => Some(Some(true)),
        Some("b") => Some(Some(false)),
        _ => None,
    }
}

pub fn requested_initiator() -> Option<bool> {
    let value = match env::var(INITIATOR_ENV) {
        Ok(value) => Some(value),
        Err(env::VarError::NotPresent) => None,
        Err(error) => panic!("invalid {INITIATOR_ENV}: {error}"),
    };
    parse_initiator(value.as_deref()).expect("pressure initiator must be a or b")
}

pub fn order_identities(
    a: p2p_vpn::identity::NodeIdentity,
    b: p2p_vpn::identity::NodeIdentity,
    preferred_a: Option<bool>,
) -> (
    p2p_vpn::identity::NodeIdentity,
    p2p_vpn::identity::NodeIdentity,
) {
    let local = a.peer_id.parse::<libp2p::PeerId>().unwrap().to_bytes();
    let remote = b.peer_id.parse::<libp2p::PeerId>().unwrap().to_bytes();
    if preferred_a.is_some_and(|preferred| preferred != (local < remote)) {
        (b, a)
    } else {
        (a, b)
    }
}

fn ping_counts(output: &str) -> Option<(usize, usize)> {
    output.lines().find_map(|line| {
        let words = line.split_whitespace().collect::<Vec<_>>();
        if words.get(1..3) != Some(&["packets", "transmitted,"][..])
            || words.get(4) != Some(&"received,")
        {
            return None;
        }
        let sent = words.first()?.parse().ok()?;
        let received = words.get(3)?.parse().ok()?;
        (sent > 0 && sent <= 3000 && received <= sent).then_some((sent, received))
    })
}

fn recovery_ready(snapshot: &serde_json::Value) -> bool {
    ["a", "b"].into_iter().all(|role| {
        snapshot[role]["queued_packets"] == 0
            && snapshot[role]["queued_bytes"] == 0
            && snapshot[role]["stream_in_flight"] == 0
            && snapshot[role]["healthy_tcp_paths"]
                .as_u64()
                .is_some_and(|count| count > 0)
            && snapshot[role]["peers_without_supported_path"] == 0
    })
}

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
            "received_packets": report["received_packets"],
        }));
        fs::write(
            temp.join("queue-pressure-series.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1, "requested_rounds": rounds, "completed_rounds": series.len(),
                "complete": round == rounds, "rounds": series,
                "queue_limit_profile": requested_limits().name,
                "preferred_initiator_override": requested_initiator().map(|a| if a { "a" } else { "b" }),
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
    let limits = requested_limits();
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
                packets <= limits.packets && bytes <= limits.bytes,
                "queue exceeded configured bound"
            );
            let work = [
                "tun_read_packets",
                "tun_read_bytes",
                "tun_write_packets",
                "tun_write_bytes",
                "inbound_accepted_packets",
                "outbound_sent_packets",
                "outbound_direct_tcp_stream_fallback_packets",
            ]
            .into_iter()
            .map(|name| {
                (
                    name,
                    state_metric_count(&lines, name).expect("work counter"),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
            nodes.insert(role.to_owned(), serde_json::json!({
                "process": idle_sample::process_observation(role, pid, started),
                "queued_packets": packets, "queued_bytes": bytes,
                "dropped_packets": state_metric_count(&lines, "queue_dropped_packets").expect("queue drops"),
                "expired_packets": state_metric_count(&lines, "queue_expired_packets").expect("queue expiry"),
                "stream_in_flight": state_metric_count(&state, "packet_stream_fallback_in_flight").expect("stream owners"),
                "outbound_failures": state_metric_count(&lines, "outbound_failures").expect("outbound failures"),
                "inbound_dropped_packets": state_metric_count(&lines, "inbound_dropped_packets").expect("inbound drops"),
                "healthy_tcp_paths": state_metric_count(&lines, "path_healthy_direct_tcp_stream_paths").expect("healthy TCP paths"),
                "peers_without_supported_path": state_metric_count(&lines, "path_peers_without_supported_path").expect("unsupported paths"),
                "blocked_no_path_events": state_metric_count(&lines, "outbound_queue_blocked_no_supported_path_events").expect("blocked path events"),
                "path_states": state.iter().filter(|line| line.starts_with("peer state:")).collect::<Vec<_>>(),
                "work": work,
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
    let (transmitted, received) =
        ping_counts(&ping_log).expect("valid ping transmission/reply summary");
    fs::write(
        output.join("queue-pressure-partial.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "stage": "load_completed", "before": before, "samples": samples,
            "after_load": after_load, "transmitted_packets": transmitted,
            "received_packets": received, "queue_limit_profile": limits.name,
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
    let mut drain_samples = Vec::new();
    loop {
        let snapshot = observe();
        let drained = recovery_ready(&snapshot);
        let expired = Instant::now() >= recovery_deadline;
        drain_samples.push(snapshot.clone());
        if drained || expired {
            fs::write(
                output.join("queue-pressure-drain.json"),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "schema_version": 1,
                    "drained": drained,
                    "deadline_reached": expired,
                    "samples": drain_samples,
                }))
                .unwrap(),
            )
            .unwrap();
        }
        if drained {
            assert_eq!(snapshot["a"]["queued_bytes"], 0);
            assert_eq!(snapshot["b"]["queued_bytes"], 0);
            break;
        }
        assert!(
            !expired,
            "healthy TCP paths and drained queues/stream requests did not coexist after pressure"
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
        "queue_packet_limit": limits.packets, "queue_byte_limit": limits.bytes,
        "queue_limit_profile": limits.name, "received_packets": received,
        "fixture_metrics_interval_seconds": METRICS_INTERVAL.as_secs(),
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
    fn recovery_requires_current_path_health_after_queue_drain() {
        use p2p_vpn::{
            PathKind, PeerId,
            path::{PathSet, PathTransportSupport},
        };

        let peer = PeerId::from_bytes([1; 32]);
        let mut paths = PathSet::new();
        paths.record_established(peer, PathKind::DirectTcpStream);
        assert_eq!(
            paths.best_for(peer).unwrap().kind,
            PathKind::DirectTcpStream
        );
        let observation = |paths: &PathSet| {
            let stats =
                paths.runtime_stats_for_peers([peer], |_| PathTransportSupport::stream_fallback());
            serde_json::json!({
                "queued_packets": 0, "queued_bytes": 0, "stream_in_flight": 0,
                "healthy_tcp_paths": stats.healthy_direct_tcp_stream_paths,
                "peers_without_supported_path": stats.peers_without_supported_path,
            })
        };
        let healthy = observation(&paths);
        let mut snapshot = serde_json::json!({"a": healthy, "b": healthy});
        assert!(recovery_ready(&snapshot));

        paths.mark_unhealthy(peer, PathKind::DirectTcpStream);
        assert!(paths.best_for(peer).is_none());
        for role in ["a", "b"] {
            snapshot[role] = observation(&paths);
            assert!(
                !recovery_ready(&snapshot),
                "empty queues do not restore {role}'s demoted path"
            );
            snapshot[role] = healthy.clone();
        }
        paths.record_established(peer, PathKind::DirectTcpStream);
        snapshot["a"] = observation(&paths);
        assert!(recovery_ready(&snapshot));

        for role in ["a", "b"] {
            for field in [
                "queued_packets",
                "queued_bytes",
                "stream_in_flight",
                "peers_without_supported_path",
            ] {
                snapshot[role][field] = 1.into();
                assert!(
                    !recovery_ready(&snapshot),
                    "nonzero {role}.{field} is not ready"
                );
                snapshot[role] = healthy.clone();
            }
            for field in [
                "queued_packets",
                "queued_bytes",
                "stream_in_flight",
                "healthy_tcp_paths",
                "peers_without_supported_path",
            ] {
                snapshot[role].as_object_mut().unwrap().remove(field);
                assert!(
                    !recovery_ready(&snapshot),
                    "missing {role}.{field} is not ready"
                );
                snapshot[role] = healthy.clone();
            }
        }
    }

    #[test]
    fn initiator_override_preserves_default_and_orders_actual_peer_ids() {
        assert_eq!(parse_initiator(None), Some(None));
        assert_eq!(parse_initiator(Some("a")), Some(Some(true)));
        assert_eq!(parse_initiator(Some("b")), Some(Some(false)));
        assert_eq!(parse_initiator(Some("random")), None);
        let first = p2p_vpn::identity::NodeIdentity::generate_ed25519().unwrap();
        let second = p2p_vpn::identity::NodeIdentity::generate_ed25519().unwrap();
        let (a, b) = order_identities(first.clone(), second.clone(), None);
        assert_eq!(a.peer_id, first.peer_id);
        assert_eq!(b.peer_id, second.peer_id);
        for preferred_a in [false, true] {
            let (a, b) = order_identities(first.clone(), second.clone(), Some(preferred_a));
            assert_eq!(
                a.peer_id.parse::<libp2p::PeerId>().unwrap().to_bytes()
                    < b.peer_id.parse::<libp2p::PeerId>().unwrap().to_bytes(),
                preferred_a
            );
        }
    }

    #[test]
    fn profiles_preserve_default_and_separately_bind_packet_or_byte_capacity() {
        let packets = parse_limits(None).unwrap();
        assert_eq!(Some(packets), parse_limits(Some("packets")));
        assert_eq!((packets.packets, packets.bytes), (4, 8192));
        let bytes = parse_limits(Some("bytes")).unwrap();
        assert_eq!((bytes.packets, bytes.bytes), (16, 4096));
        let ip_packet_bytes = 1000 + 8 + 20;
        assert!(packets.bytes / ip_packet_bytes > packets.packets);
        assert!(bytes.bytes / ip_packet_bytes < bytes.packets);
        for invalid in ["", "byte", "packet", "0", "PACKETS"] {
            assert_eq!(parse_limits(Some(invalid)), None);
        }
    }

    #[test]
    fn ping_summary_requires_valid_sent_and_received_counts() {
        assert_eq!(
            ping_counts("100 packets transmitted, 5 received, 95% packet loss"),
            Some((100, 5))
        );
        assert_eq!(
            ping_counts("100 packets transmitted, 0 received, +100 errors, 100% packet loss"),
            Some((100, 0))
        );
        for invalid in [
            "",
            "5 received",
            "0 packets transmitted, 0 received,",
            "3001 packets transmitted, 1 received,",
            "1 packets transmitted, 2 received,",
            "1 packets transmitted, invalid received,",
        ] {
            assert_eq!(ping_counts(invalid), None);
        }
    }

    #[test]
    fn queue_admission_exercises_each_profile_limit_independently() {
        use p2p_vpn::{
            PeerId,
            queue::{EnqueueError, Packet, PeerQueue},
        };
        for (profile, accepted) in [("packets", 4), ("bytes", 3)] {
            let limits = parse_limits(Some(profile)).unwrap();
            let mut queue = PeerQueue::new(limits.packets, limits.bytes);
            let packet = || Packet::new(PeerId::from_bytes([1; 32]), 1, vec![0; 1028]);
            for _ in 0..accepted {
                queue.enqueue(packet()).unwrap();
            }
            assert_eq!(
                queue.enqueue(packet()),
                Err(EnqueueError::QueueFull { packet_bytes: 1028 })
            );
            assert_eq!(queue.stats().queued_packets, accepted);
            assert_eq!(queue.stats().queued_bytes, accepted * 1028);
            assert_eq!(queue.stats().dropped_packets, 1);
        }
    }

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
