use super::{NODE_A_LOCAL_ROUTE_ADDRESS, NamespaceChild, idle_sample, paced_ping};
use std::{
    env,
    fs::{self, File},
    net::Ipv4Addr,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

pub const TEST_NAME: &str = "tun_namespace_measures_sustained_traffic_resources";
pub const SMOKE_ENV: &str = "P2P_VPN_REVIEW_TRAFFIC_SMOKE";
const WORKER: &str = "sustained_traffic::traffic_worker";

fn smoke(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some("1") => true,
        _ => panic!("traffic smoke must be absent or 1"),
    }
}

pub fn settings() -> (paced_ping::Settings, Duration) {
    let value = match env::var(SMOKE_ENV) {
        Ok(value) => Some(value),
        Err(env::VarError::NotPresent) => None,
        Err(error) => panic!("invalid {SMOKE_ENV}: {error}"),
    };
    let smoke = smoke(value.as_deref());
    (
        paced_ping::Settings {
            rate: 50,
            seconds: if smoke { 10 } else { 300 },
            payload_bytes: 512,
            preload: 1,
        },
        Duration::from_secs(if smoke { 10 } else { 60 }),
    )
}

pub fn duration_budget() -> Duration {
    let (traffic, drain) = settings();
    idle_sample::WARMUP + Duration::from_secs(u64::from(traffic.seconds)) + drain
}

fn delivered(report: &paced_ping::Report, settings: paced_ping::Settings) -> bool {
    let offered = u64::from(settings.rate) * u64::from(settings.seconds);
    (offered * 98 / 100..=offered).contains(&report.sent)
        && report.received >= report.sent * 98 / 100
        && report.received <= report.sent
        && report.invalid_replies == 0
        && report.duplicate_replies == 0
}

fn fixed_transport(load: &serde_json::Value, drain: &serde_json::Value) -> bool {
    let metric = "outbound_stream_fallback_packets";
    ["a", "b"].into_iter().all(|role| {
        let state_metric = |phase: &serde_json::Value, boundary: &str| {
            let lines: Vec<String> =
                serde_json::from_value(phase[boundary][role]["state"].clone()).ok()?;
            super::state_metric_count(&lines, metric)
        };
        let Some(initial) = state_metric(load, "daemon_before") else {
            return false;
        };
        [load, drain].into_iter().all(|phase| {
            state_metric(phase, "daemon_before") == Some(initial)
                && state_metric(phase, "daemon_after") == Some(initial)
                && phase["runtime_samples"].as_array().is_some_and(|rows| {
                    let samples: Vec<_> = rows.iter().filter(|row| row["role"] == role).collect();
                    !samples.is_empty()
                        && samples.iter().all(|row| {
                            row["values"][metric].as_u64() == u64::try_from(initial).ok()
                                && row["values"]["path_healthy_direct_udp_datagram_paths"]
                                    .as_u64()
                                    .is_some_and(|count| count > 0)
                        })
                })
        })
    })
}

#[test]
#[ignore = "internal sustained traffic generator; needs namespace, destination and report path"]
fn traffic_worker() {
    let (settings, _) = settings();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let report = runtime
        .block_on(paced_ping::run(
            NODE_A_LOCAL_ROUTE_ADDRESS,
            env::var("P2P_VPN_TRAFFIC_DESTINATION")
                .unwrap()
                .parse()
                .unwrap(),
            settings,
            Some("hse2ea"),
        ))
        .unwrap();
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(env::var("P2P_VPN_TRAFFIC_REPORT").unwrap())
        .unwrap();
    serde_json::to_writer(&file, &report).unwrap();
    file.sync_all().unwrap();
}

pub fn capture(temp: &Path, pid_a: u32, pid_b: u32, destination: Ipv4Addr) {
    let (traffic, drain) = settings();
    let roles = [("a", pid_a), ("b", pid_b)];
    let started = Instant::now();
    let process_before =
        roles.map(|(_, pid)| super::process_sample::capture(pid, started).unwrap());
    super::ns_command(pid_a, "sysctl", &["-w", "net.ipv4.ping_group_range=0 0"]);
    let mut generator = None;
    idle_sample::capture_phase(
        temp,
        &roles,
        idle_sample::Phase {
            workload: "sustained_traffic",
            duration: Duration::from_secs(u64::from(traffic.seconds)),
            warmup: idle_sample::WARMUP,
            report_name: "traffic-sample.json",
            diagnostics_interval: idle_sample::METRICS_INTERVAL,
        },
        || {
            let log = File::create(temp.join("traffic-worker.log")).unwrap();
            generator = Some(NamespaceChild {
                child: Command::new("nsenter")
                    .args([
                        "-t",
                        &pid_a.to_string(),
                        "-n",
                        "prlimit",
                        "--fsize=65536:65536",
                        "--",
                    ])
                    .arg(env::current_exe().unwrap())
                    .args([
                        "--ignored",
                        "--exact",
                        WORKER,
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("P2P_VPN_TRAFFIC_DESTINATION", destination.to_string())
                    .env(
                        "P2P_VPN_TRAFFIC_REPORT",
                        temp.join("traffic-generator.json"),
                    )
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .unwrap(),
            });
        },
    );
    let mut generator = generator.unwrap();
    let wait_started = Instant::now();
    let status = loop {
        if let Some(status) = generator.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            wait_started.elapsed() < Duration::from_secs(5),
            "traffic generator did not exit"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "traffic generator failed; log retained");
    let generator_exit_wait_seconds = wait_started.elapsed().as_secs_f64();
    let report: paced_ping::Report =
        serde_json::from_slice(&fs::read(temp.join("traffic-generator.json")).unwrap()).unwrap();
    // Preserve both resource windows before evaluating delivery or post-load health.
    idle_sample::capture_phase(
        temp,
        &roles,
        idle_sample::Phase {
            workload: "sustained_traffic_drain",
            duration: drain,
            warmup: Duration::ZERO,
            report_name: "traffic-drain-sample.json",
            diagnostics_interval: idle_sample::METRICS_INTERVAL,
        },
        || {},
    );
    let process_after = roles.map(|(_, pid)| super::process_sample::capture(pid, started).unwrap());
    let phase = |name| {
        serde_json::from_slice::<serde_json::Value>(&fs::read(temp.join(name)).unwrap()).unwrap()
    };
    let fixed_transport = fixed_transport(
        &phase("traffic-sample.json"),
        &phase("traffic-drain-sample.json"),
    );
    let same_processes = process_before
        .iter()
        .zip(&process_after)
        .all(|(before, after)| before.pid == after.pid && before.start_ticks == after.start_ticks);
    let pings: Vec<_> = [
        ("a", pid_a, "hse2ea", destination),
        ("b", pid_b, "hse2eb", NODE_A_LOCAL_ROUTE_ADDRESS),
    ]
    .into_iter()
    .map(|(role, pid, interface, target)| {
        let output = super::ping_from_namespace(pid, interface, target);
        serde_json::json!({"role": role, "success": output.status.success()
            && super::recovery_soak::complete_ping(&String::from_utf8_lossy(&output.stdout)),
            "output": super::format_snapshot_output(&output)})
    })
    .collect();
    let complete = fixed_transport
        && delivered(&report, traffic)
        && same_processes
        && pings.iter().all(|row| row["success"] == true);
    let summary = serde_json::json!({
        "schema_version": 1, "complete": complete,
        "proof_eligible": complete && traffic.seconds == 300 && drain.as_secs() == 60,
        "requests_per_second": traffic.rate, "payload_bytes": traffic.payload_bytes,
        "requested_traffic_seconds": traffic.seconds, "drain_seconds": drain.as_secs(),
        "generator": report, "generator_exit_wait_seconds": generator_exit_wait_seconds,
        "delivery_passed": delivered(&report, traffic), "final_pings": pings,
        "fixed_transport": fixed_transport,
        "same_processes": same_processes, "process_before": process_before, "process_after": process_after,
    });
    fs::write(
        temp.join("sustained-traffic.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    assert!(
        fixed_transport,
        "fixed-transport evidence failed; reports retained"
    );
    assert!(
        same_processes,
        "daemon restarted across resource phases; reports retained"
    );
    assert!(
        delivered(&report, traffic),
        "sustained delivery below frozen budget; reports retained"
    );
    assert!(
        pings.iter().all(|row| row["success"] == true),
        "post-drain ping failed; reports retained"
    );
}

#[test]
fn transport_evidence_rejects_fallback_missing_and_unhealthy_samples() {
    let phase = |count| {
        serde_json::json!({
            "daemon_before": {"a": {"state": ["outbound_stream_fallback_packets 2"]}, "b": {"state": ["outbound_stream_fallback_packets 2"]}},
            "daemon_after": {"a": {"state": ["outbound_stream_fallback_packets 2"]}, "b": {"state": ["outbound_stream_fallback_packets 2"]}},
            "runtime_samples": [
                {"role": "a", "values": {"outbound_stream_fallback_packets": count, "path_healthy_direct_udp_datagram_paths": 1}},
                {"role": "b", "values": {"outbound_stream_fallback_packets": 2, "path_healthy_direct_udp_datagram_paths": 1}}]
        })
    };
    assert!(fixed_transport(&phase(2), &phase(2)));
    assert!(!fixed_transport(&phase(3), &phase(2)));
    assert!(!fixed_transport(&phase(2), &phase(3)));
    let mut missing = phase(2);
    missing["daemon_after"]["b"]["state"] = serde_json::json!([]);
    assert!(!fixed_transport(&phase(2), &missing));
    let mut unhealthy = phase(2);
    unhealthy["runtime_samples"][0]["values"]["path_healthy_direct_udp_datagram_paths"] =
        serde_json::json!(0);
    assert!(!fixed_transport(&unhealthy, &phase(2)));
    let mut missing_role = phase(2);
    missing_role["runtime_samples"] = serde_json::json!([]);
    assert!(!fixed_transport(&missing_role, &phase(2)));
}

#[test]
fn delivery_requires_offered_work_and_valid_replies() {
    let settings = paced_ping::Settings {
        rate: 50,
        seconds: 300,
        payload_bytes: 512,
        preload: 1,
    };
    let mut report = paced_ping::Report {
        sent: 15000,
        received: 15000,
        skipped_slots: 0,
        duplicate_replies: 0,
        invalid_replies: 0,
        elapsed_seconds: 300.0,
        maximum_lateness_seconds: 0.0,
    };
    assert!(delivered(&report, settings));
    report.received = 14700;
    assert!(delivered(&report, settings));
    report.received -= 1;
    assert!(!delivered(&report, settings));
    report.sent = 0;
    report.received = 0;
    assert!(!delivered(&report, settings));
    report.sent = 15000;
    report.received = 15001;
    assert!(!delivered(&report, settings));
    report.received = 15000;
    report.invalid_replies = 1;
    assert!(!delivered(&report, settings));
    report.invalid_replies = 0;
    report.duplicate_replies = 1;
    assert!(!delivered(&report, settings));
}

#[test]
fn smoke_is_only_explicit_and_does_not_change_default() {
    assert!(!smoke(None));
    assert!(smoke(Some("1")));
    for invalid in ["", "0", "true", "300"] {
        assert!(std::panic::catch_unwind(|| smoke(Some(invalid))).is_err());
    }
}
