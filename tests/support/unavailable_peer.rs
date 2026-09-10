use std::{
    net::Ipv4Addr,
    path::Path,
    time::{Duration, Instant},
};

use super::{NODE_A_LOCAL_ROUTE_ADDRESS, idle_sample};

pub const TEST_NAME: &str = "tun_namespace_measures_unavailable_peer_resources";
pub const WATCHDOG_EXTRA: Duration = Duration::from_secs(90);
const RECOVERY_TIMEOUT: Duration = Duration::from_mins(1);

fn recovered(results: &[bool], elapsed: Duration) -> bool {
    elapsed <= RECOVERY_TIMEOUT && results.len() == 2 && results.iter().all(|success| *success)
}

pub fn capture(temp: &Path, pid_a: u32, pid_b: u32, address_b: Ipv4Addr) {
    let roles = [("a", pid_a), ("b", pid_b)];
    idle_sample::capture_with_transition(temp, &roles, "configured_peer_unavailable", || {
        super::ns_command(pid_b, "ip", &["link", "set", "veth-b", "down"]);
        let output = super::ns_command_output(pid_a, "ping", &["-c", "1", "-w", "1", "10.250.0.2"]);
        assert!(
            !output.status.success(),
            "underlay remained reachable during outage"
        );
    });

    recover(
        temp,
        pid_a,
        pid_b,
        address_b,
        "idle-sample.json",
        "unavailable-recovery.json",
    );
}

pub(super) fn recover(
    temp: &Path,
    pid_a: u32,
    pid_b: u32,
    address_b: Ipv4Addr,
    outage_report: &str,
    recovery_report: &str,
) {
    let roles = [("a", pid_a), ("b", pid_b)];
    // The outage report is already durable if recovery subsequently fails.
    let started = Instant::now();
    super::ns_command(pid_b, "ip", &["link", "set", "veth-b", "up"]);
    let deadline = started + RECOVERY_TIMEOUT;
    let endpoints = [
        ("a", pid_a, "hse2ea", address_b),
        ("b", pid_b, "hse2eb", NODE_A_LOCAL_ROUTE_ADDRESS),
    ];
    let mut observations = Vec::new();
    let mut success = false;
    while Instant::now() < deadline {
        let mut round = Vec::new();
        for (role, pid, interface, destination) in endpoints {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let pid = pid.to_string();
            let destination = destination.to_string();
            let result = super::command_output(
                "nsenter",
                &[
                    "-t",
                    &pid,
                    "-n",
                    "ping",
                    "-c",
                    "1",
                    "-w",
                    "1",
                    "-I",
                    interface,
                    &destination,
                ],
                &[],
                remaining.min(Duration::from_secs(2)),
            );
            let passed = result.as_ref().is_ok_and(|output| output.status.success());
            observations.push(serde_json::json!({
                "role": role, "elapsed_seconds": started.elapsed().as_secs_f64(),
                "success": passed,
                "output": result.as_ref().ok().map(super::format_snapshot_output),
                "error": result.as_ref().err().map(ToString::to_string),
            }));
            round.push(passed);
        }
        if recovered(&round, started.elapsed()) {
            success = true;
            break;
        }
        std::thread::sleep(
            Duration::from_secs(1).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    let recovered_seconds = success.then(|| started.elapsed().as_secs_f64());
    let mut final_pings = Vec::new();
    if success {
        for (role, pid, interface, destination) in endpoints {
            let output = super::ping_from_namespace(pid, interface, destination);
            final_pings.push(serde_json::json!({
                "role": role, "success": output.status.success()
                    && super::recovery_soak::complete_ping(&String::from_utf8_lossy(&output.stdout)),
                "output": super::format_snapshot_output(&output),
            }));
        }
    }
    let report = serde_json::json!({
        "schema_version": 1, "recovery_timeout_seconds": RECOVERY_TIMEOUT.as_secs(),
        "recovered_seconds": recovered_seconds, "observations": observations,
        "final_pings": final_pings, "daemon_after": idle_sample::daemon_views(temp, &roles),
        "process_after": roles.iter().map(|(role, pid)|
            idle_sample::process_observation(role, *pid, started)).collect::<Vec<_>>(),
    });
    let bytes = serde_json::to_vec_pretty(&report).unwrap();
    let outage_bytes = std::fs::metadata(temp.join(outage_report)).unwrap().len();
    assert!(
        outage_bytes + u64::try_from(bytes.len()).unwrap() <= 8 * 1024 * 1024,
        "combined outage/recovery reports exceed budget"
    );
    std::fs::write(temp.join(recovery_report), bytes).unwrap();
    assert!(
        success,
        "peer did not recover within the fixed deadline; report retained"
    );
    assert!(
        final_pings.iter().all(|row| row["success"] == true),
        "post-recovery traffic failed; report retained"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_requires_both_directions_in_the_same_round() {
        assert!(recovered(&[true, true], Duration::ZERO));
        assert!(recovered(&[true, true], RECOVERY_TIMEOUT));
        assert!(!recovered(
            &[true, true],
            RECOVERY_TIMEOUT + Duration::from_nanos(1)
        ));
        for results in [
            &[][..],
            &[true],
            &[false, true],
            &[true, false],
            &[false, false],
            &[true, true, true],
        ] {
            assert!(!recovered(results, Duration::ZERO));
        }
    }
}
