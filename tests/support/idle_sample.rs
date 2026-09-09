use std::{
    collections::BTreeMap,
    env,
    fmt::Write as _,
    fs,
    io::{self, Read},
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use p2p_vpn::runtime::control_socket::{query_state, query_status};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::process_sample;

pub const SAMPLE_ENV: &str = "P2P_VPN_TUN_E2E_IDLE_SECONDS";
pub const WARMUP: Duration = Duration::from_secs(30);
pub const METRICS_INTERVAL: Duration = Duration::from_secs(5);

pub fn requested_duration() -> Option<Duration> {
    match env::var(SAMPLE_ENV) {
        Err(env::VarError::NotPresent) => None,
        Ok(value) => Some(parse_duration(&value).expect("idle sample must be 10..=300 seconds")),
        Err(error) => panic!("invalid {SAMPLE_ENV}: {error}"),
    }
}

fn parse_duration(value: &str) -> Option<Duration> {
    let seconds = value.parse::<u64>().ok()?;
    (10..=300)
        .contains(&seconds)
        .then(|| Duration::from_secs(seconds))
}

#[derive(Debug, Serialize)]
struct ProcessSample {
    elapsed_seconds: f64,
    role: String,
    pid: u32,
    start_ticks: u64,
    cpu_ticks: u64,
    rss_kib: u64,
    threads: u64,
    socket_fds: usize,
    tcp_states: BTreeMap<String, usize>,
    total_fds: Option<usize>,
    capture_seconds: f64,
    vanished_fds: usize,
    process_tcp_states: BTreeMap<String, usize>,
}

fn sample(role: &str, pid: u32, started: Instant) -> io::Result<ProcessSample> {
    let observed = process_sample::capture(pid, started)?;
    Ok(ProcessSample {
        elapsed_seconds: observed.elapsed_seconds,
        role: role.to_owned(),
        pid: observed.pid,
        start_ticks: observed.start_ticks,
        cpu_ticks: observed.cpu_ticks,
        rss_kib: observed.rss_kib,
        threads: observed.threads,
        socket_fds: observed.socket_fds,
        // Preserve the historical field's namespace-wide meaning.
        tcp_states: observed.namespace_tcp_states,
        total_fds: observed.total_fds,
        capture_seconds: observed.capture_seconds,
        vanished_fds: observed.vanished_fds,
        process_tcp_states: observed.process_tcp_states,
    })
}

pub(super) fn fingerprint() -> io::Result<String> {
    let mut file = fs::File::open(env::current_exe()?)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let mut hex = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(hex, "{byte:02x}").expect("write digest into String");
    }
    Ok(hex)
}

pub fn process_observation(role: &str, pid: u32, started: Instant) -> serde_json::Value {
    serde_json::to_value(sample(role, pid, started).expect("live process observation"))
        .expect("serializable process observation")
}

fn daemon_views(temp: &Path, roles: &[(&str, u32)]) -> serde_json::Value {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("sampling runtime");
    let mut views = BTreeMap::new();
    for (role, _) in roles {
        let socket = super::node_control_socket(temp, role);
        let status = runtime
            .block_on(query_status(&socket, Duration::from_secs(2)))
            .expect("idle daemon status");
        let state = runtime
            .block_on(query_state(&socket, Duration::from_secs(2)))
            .expect("idle daemon state");
        views.insert(*role, serde_json::json!({"status": status, "state": state}));
    }
    serde_json::to_value(views).expect("serializable daemon views")
}

pub fn capture(temp: &Path, roles: &[(&str, u32)]) {
    let Some(duration) = requested_duration() else {
        return;
    };
    let hash = fingerprint().expect("test binary fingerprint");
    let ticks = Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .expect("getconf CLK_TCK");
    assert!(ticks.status.success(), "getconf CLK_TCK failed");
    let ticks: u64 = String::from_utf8(ticks.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("clock ticks per second");
    assert!(ticks > 0);
    thread::sleep(WARMUP);
    let before = daemon_views(temp, roles);
    let load_before = fs::read_to_string("/proc/loadavg").expect("host load before sample");
    let started = Instant::now();
    let mut samples = Vec::new();
    loop {
        for (role, pid) in roles {
            samples.push(sample(role, *pid, started).expect("live process sample"));
        }
        if started.elapsed() >= duration {
            break;
        }
        thread::sleep(Duration::from_secs(1).min(duration.saturating_sub(started.elapsed())));
    }
    for (role, _) in roles {
        let first = samples.iter().find(|row| row.role == *role).unwrap();
        let last = samples.iter().rfind(|row| row.role == *role).unwrap();
        assert_eq!(
            first.start_ticks, last.start_ticks,
            "sampled daemon restarted"
        );
        assert!(
            last.cpu_ticks >= first.cpu_ticks,
            "CPU accounting decreased"
        );
    }
    let report = serde_json::json!({
        "schema_version": 1, "binary_sha256": hash, "binary": env::current_exe().unwrap(),
        "build_profile": "cargo integration test", "topology": "two isolated namespaces; direct UDP; no Internet route",
        "fixture_metrics_interval_seconds": METRICS_INTERVAL.as_secs(),
        "available_parallelism": thread::available_parallelism().unwrap().get(),
        "kernel_release": fs::read_to_string("/proc/sys/kernel/osrelease").unwrap().trim(),
        "host_load_before": load_before.trim(),
        "host_load_after": fs::read_to_string("/proc/loadavg").unwrap().trim(),
        "warmup_seconds": WARMUP.as_secs(), "requested_seconds": duration.as_secs(),
        "clock_ticks_per_second": ticks, "samples": samples,
        "daemon_before": before, "daemon_after": daemon_views(temp, roles),
    });
    fs::write(
        temp.join("idle-sample.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .expect("idle report");
    eprintln!("idle sample: {}", temp.join("idle-sample.json").display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_is_bounded_and_rejects_invalid_inputs() {
        for value in ["", "9", "301", "-1", "1.5", "no", "18446744073709551616"] {
            assert!(parse_duration(value).is_none());
        }
        assert_eq!(parse_duration("10"), Some(Duration::from_secs(10)));
        assert_eq!(parse_duration("300"), Some(Duration::from_mins(5)));
    }

    #[test]
    fn live_idle_sample_preserves_fields_and_adds_process_attribution() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let observed = sample("a", std::process::id(), Instant::now()).unwrap();
        assert_eq!(observed.role, "a");
        assert!(observed.total_fds.unwrap() >= observed.socket_fds);
        assert!(observed.process_tcp_states.get("0A").copied().unwrap_or(0) >= 1);
        assert!(observed.tcp_states["0A"] >= observed.process_tcp_states["0A"]);
        assert!(observed.capture_seconds >= 0.0);
        let value = serde_json::to_value(observed).unwrap();
        for field in [
            "role",
            "pid",
            "cpu_ticks",
            "rss_kib",
            "tcp_states",
            "total_fds",
        ] {
            assert!(value.get(field).is_some(), "missing field: {field}");
        }
        drop(listener);
    }
}
