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

pub const SAMPLE_ENV: &str = "P2P_VPN_TUN_E2E_IDLE_SECONDS";
pub const WARMUP: Duration = Duration::from_secs(30);

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
}

fn process_times(stat: &str) -> io::Result<(u64, u64)> {
    // The comm field may contain spaces and parentheses; numeric fields follow its last ')'.
    let fields = stat
        .rsplit_once(')')
        .ok_or_else(invalid_sample)?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let number = |index: usize| {
        fields
            .get(index)
            .ok_or_else(invalid_sample)?
            .parse::<u64>()
            .map_err(|_| invalid_sample())
    };
    Ok((
        number(19)?,
        number(11)?
            .checked_add(number(12)?)
            .ok_or_else(invalid_sample)?,
    ))
}

fn invalid_sample() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid process sample")
}

fn status_number(status: &str, key: &str) -> io::Result<u64> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|rest| rest.split_whitespace().next())
        .ok_or_else(invalid_sample)?
        .parse()
        .map_err(|_| invalid_sample())
}

fn tcp_states(table: &str, counts: &mut BTreeMap<String, usize>) -> io::Result<()> {
    for line in table.lines().skip(1).filter(|line| !line.trim().is_empty()) {
        let state = line.split_whitespace().nth(3).ok_or_else(invalid_sample)?;
        *counts.entry(state.to_owned()).or_default() += 1;
    }
    Ok(())
}

fn sample(role: &str, pid: u32, started: Instant) -> io::Result<ProcessSample> {
    let root = format!("/proc/{pid}");
    let (start_ticks, cpu_ticks) = process_times(&fs::read_to_string(format!("{root}/stat"))?)?;
    let status = fs::read_to_string(format!("{root}/status"))?;
    let mut socket_fds = 0;
    for entry in fs::read_dir(format!("{root}/fd"))? {
        match fs::read_link(entry?.path()) {
            Ok(target) => {
                socket_fds += usize::from(target.to_string_lossy().starts_with("socket:["));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let mut tcp_counts = BTreeMap::new();
    for protocol in ["tcp", "tcp6"] {
        tcp_states(
            &fs::read_to_string(format!("{root}/net/{protocol}"))?,
            &mut tcp_counts,
        )?;
    }
    Ok(ProcessSample {
        elapsed_seconds: started.elapsed().as_secs_f64(),
        role: role.to_owned(),
        pid,
        start_ticks,
        cpu_ticks,
        rss_kib: status_number(&status, "VmRSS:")?,
        threads: status_number(&status, "Threads:")?,
        socket_fds,
        tcp_states: tcp_counts,
    })
}

fn fingerprint() -> io::Result<String> {
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
        "fixture_metrics_interval_seconds": 1,
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
    fn stat_parser_handles_parentheses_and_rejects_truncation_and_overflow() {
        let mut fields = vec!["0"; 22];
        fields[0] = "S";
        fields[11] = "17";
        fields[12] = "4";
        fields[19] = "1234";
        let stat = format!("42 (worker ) with (spaces)) {}", fields.join(" "));
        assert_eq!(process_times(&stat).unwrap(), (1234, 21));
        assert!(process_times("42 (truncated) S 1").is_err());
        fields[11] = "18446744073709551615";
        assert!(process_times(&format!("42 (worker) {}", fields.join(" "))).is_err());
    }

    #[test]
    fn status_and_tcp_parsers_preserve_units_and_states() {
        let status = "Name:\tworker\nVmRSS:\t2048 kB\nThreads:\t19\n";
        assert_eq!(status_number(status, "VmRSS:").unwrap(), 2048);
        assert_eq!(status_number(status, "Threads:").unwrap(), 19);
        assert!(status_number(status, "Missing:").is_err());
        let mut tcp_counts = BTreeMap::new();
        tcp_states(
            "header\n0: local remote 01 rest\n1: local remote 0A rest\n",
            &mut tcp_counts,
        )
        .unwrap();
        tcp_states("header\n0: local remote 01 rest\n", &mut tcp_counts).unwrap();
        assert_eq!(tcp_counts["01"], 2);
        assert_eq!(tcp_counts["0A"], 1);
        assert!(tcp_states("header\nbroken\n", &mut tcp_counts).is_err());
    }
}
