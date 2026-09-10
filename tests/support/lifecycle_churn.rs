use std::{
    env, fs,
    net::Ipv4Addr,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use super::{idle_sample, process_sample, unavailable_peer};

pub const TEST_NAME: &str = "tun_namespace_measures_lifecycle_churn_resources";
pub const SMOKE_ENV: &str = "P2P_VPN_REVIEW_CHURN_SMOKE";
pub const DIAGNOSTICS_INTERVAL: Duration = Duration::from_secs(10);

fn cycles(value: Option<&str>) -> usize {
    match value {
        None => 10,
        Some("1") => 1,
        _ => panic!("churn smoke must be absent or 1"),
    }
}

pub fn requested_cycles() -> usize {
    match env::var(SMOKE_ENV) {
        Ok(value) => cycles(Some(&value)),
        Err(env::VarError::NotPresent) => cycles(None),
        Err(error) => panic!("invalid {SMOKE_ENV}: {error}"),
    }
}

pub fn watchdog() -> Duration {
    Duration::from_secs(if requested_cycles() == 10 { 890 } else { 240 })
}

fn same_processes(before: &[serde_json::Value], after: &[serde_json::Value]) -> bool {
    before.len() == 2
        && after.len() == 2
        && before.iter().zip(after).all(|(a, b)| {
            ["pid", "start_ticks"]
                .iter()
                .all(|field| a[*field].as_u64().is_some() && a[*field] == b[*field])
        })
}

fn outage_demoted(report: &serde_json::Value) -> bool {
    ["a", "b"].into_iter().all(|role| {
        let lines =
            serde_json::from_value::<Vec<String>>(report["daemon_after"][role]["status"].clone());
        lines.is_ok_and(|lines| {
            super::state_metric_count(&lines, "path_healthy_direct_udp_datagram_paths") == Some(0)
        })
    })
}

pub fn capture(temp: &Path, pid_a: u32, pid_b: u32, address_b: Ipv4Addr) {
    let count = requested_cycles();
    let roles = [("a", pid_a), ("b", pid_b)];
    let started = Instant::now();
    let processes = || {
        roles
            .iter()
            .map(|(role, pid)| idle_sample::process_observation(role, *pid, started))
            .collect::<Vec<_>>()
    };
    let before = processes();
    let mut reports = Vec::new();
    for cycle in 1..=count {
        let outage = format!("churn-{cycle:02}-outage.json");
        let recovery = format!("churn-{cycle:02}-recovery.json");
        let observations = format!("churn-{cycle:02}-recovery-sample.json");
        idle_sample::capture_phase(
            temp,
            &roles,
            idle_sample::Phase {
                workload: "lifecycle_churn_unavailable",
                duration: Duration::from_secs(30),
                warmup: if cycle == 1 {
                    idle_sample::WARMUP
                } else {
                    Duration::ZERO
                },
                report_name: &outage,
                diagnostics_interval: DIAGNOSTICS_INTERVAL,
            },
            || {
                super::ns_command(pid_b, "ip", &["link", "set", "veth-b", "down"]);
                let output =
                    super::ns_command_output(pid_a, "ping", &["-c", "1", "-w", "1", "10.250.0.2"]);
                assert!(
                    !output.status.success(),
                    "underlay remained reachable during outage"
                );
            },
        );
        let outage_state = serde_json::from_slice(&fs::read(temp.join(&outage)).unwrap()).unwrap();
        compact_report(temp, &outage);
        assert!(
            outage_demoted(&outage_state),
            "outage did not demote both UDP paths; report retained"
        );
        // Recovery and its strict traffic checks run alongside independent resource sampling.
        thread::scope(|scope| {
            let mut recovery_task = None;
            idle_sample::capture_phase(
                temp,
                &roles,
                idle_sample::Phase {
                    workload: "lifecycle_churn_recovery",
                    duration: Duration::from_secs(40),
                    warmup: Duration::ZERO,
                    report_name: &observations,
                    diagnostics_interval: DIAGNOSTICS_INTERVAL,
                },
                || {
                    recovery_task = Some(scope.spawn(|| {
                        unavailable_peer::recover(temp, pid_a, pid_b, address_b, &outage, &recovery)
                    }));
                },
            );
            recovery_task
                .unwrap()
                .join()
                .expect("autonomous recovery failed; cycle reports retained");
        });
        compact_report(temp, &observations);
        compact_report(temp, &recovery);
        reports.extend([outage, recovery, observations]);
        let after = processes();
        let same = same_processes(&before, &after);
        let summary = serde_json::json!({
            "schema_version": 1, "requested_cycles": count, "completed_cycles": cycle,
            "same_processes": same, "process_before": before, "process_after": after,
            "reports": reports, "complete": false, "proof_eligible": false,
        });
        fs::write(
            temp.join("lifecycle-churn.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .unwrap();
        assert!(
            within_budget(temp, &reports),
            "churn evidence exceeds budget"
        );
        assert!(same, "daemon identity changed; cycle reports retained");
    }
    let settle = "churn-settle.json";
    idle_sample::capture_phase(
        temp,
        &roles,
        idle_sample::Phase {
            workload: "lifecycle_churn_settle",
            duration: Duration::from_secs(60),
            warmup: Duration::ZERO,
            report_name: settle,
            diagnostics_interval: DIAGNOSTICS_INTERVAL,
        },
        || {},
    );
    compact_report(temp, settle);
    reports.push(settle.to_owned());
    let final_pings: Vec<_> = [
        ("a", pid_a, "hse2ea", address_b),
        ("b", pid_b, "hse2eb", super::NODE_A_LOCAL_ROUTE_ADDRESS),
    ]
    .into_iter()
    .map(|(role, pid, interface, destination)| {
        let output = super::ping_from_namespace(pid, interface, destination);
        serde_json::json!({"role": role, "success": output.status.success()
            && super::recovery_soak::complete_ping(&String::from_utf8_lossy(&output.stdout)),
            "output": super::format_snapshot_output(&output)})
    })
    .collect();
    let after = processes();
    let same = same_processes(&before, &after);
    let complete = same && final_pings.iter().all(|ping| ping["success"] == true);
    let mut summary = serde_json::json!({
        "schema_version": 1, "requested_cycles": count, "completed_cycles": count,
        "same_processes": same, "process_before": before, "process_after": after,
        "reports": reports, "complete": complete, "proof_eligible": complete && count == 10,
        "final_pings": final_pings,
        "elapsed_seconds": started.elapsed().as_secs_f64(),
        "collector_after": process_sample::capture(std::process::id(), started).unwrap(),
    });
    fs::write(
        temp.join("lifecycle-churn.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    let budget_passed = within_budget(temp, &reports);
    if !budget_passed {
        summary["complete"] = serde_json::json!(false);
        summary["proof_eligible"] = serde_json::json!(false);
        fs::write(
            temp.join("lifecycle-churn.json"),
            serde_json::to_vec_pretty(&summary).unwrap(),
        )
        .unwrap();
    }
    assert!(budget_passed, "churn evidence exceeds budget");
    assert!(
        same,
        "daemon identity changed during settling; reports retained"
    );
    assert!(complete, "post-settling traffic failed; reports retained");
}

fn within_budget(temp: &Path, reports: &[String]) -> bool {
    let bytes: u64 = reports
        .iter()
        .map(|name| fs::metadata(temp.join(name)).unwrap().len())
        .sum();
    let reports_fit = bytes
        + fs::metadata(temp.join("lifecycle-churn.json"))
            .unwrap()
            .len()
        <= 8 * 1024 * 1024;
    let logs: u64 = ["node-a.log", "node-b.log"]
        .iter()
        .map(|name| fs::metadata(temp.join(name)).unwrap().len())
        .sum();
    reports_fit && logs <= 2 * 1024 * 1024
}

fn compact_report(temp: &Path, name: &str) {
    let path = temp.join(name);
    let report: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(&report).unwrap()).unwrap();
}

#[test]
fn outage_evidence_requires_both_demoted_paths() {
    let mut report = serde_json::json!({"daemon_after": {
        "a": {"state": [], "status": ["path_healthy_direct_udp_datagram_paths 0"]},
        "b": {"state": [], "status": ["path_healthy_direct_udp_datagram_paths 0"]}
    }});
    assert!(outage_demoted(&report));
    report["daemon_after"]["b"]["status"] =
        serde_json::json!(["path_healthy_direct_udp_datagram_paths 1"]);
    assert!(!outage_demoted(&report));
    report["daemon_after"]["b"]["status"] = serde_json::json!([]);
    assert!(!outage_demoted(&report));
    assert!(!outage_demoted(&serde_json::json!({})));
}

#[test]
fn smoke_is_explicit_and_full_capture_is_ten_cycles() {
    assert_eq!(cycles(None), 10);
    assert_eq!(cycles(Some("1")), 1);
    for value in ["", "0", "2", "10", "true"] {
        assert!(std::panic::catch_unwind(|| cycles(Some(value))).is_err());
    }
}

#[test]
fn identity_evidence_rejects_missing_changed_and_partial_processes() {
    let before = vec![
        serde_json::json!({"pid": 18, "start_ticks": 10}),
        serde_json::json!({"pid": 19, "start_ticks": 10}),
    ];
    assert!(same_processes(&before, &before));
    assert!(!same_processes(&[], &[]));
    assert!(!same_processes(&before, &before[..1]));
    for field in ["pid", "start_ticks"] {
        let mut after = before.clone();
        after[1][field] = serde_json::json!(20);
        assert!(!same_processes(&before, &after));
        after[1][field] = serde_json::Value::Null;
        assert!(!same_processes(&after, &after));
    }
}
