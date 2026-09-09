use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
    time::Instant,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSample {
    pub elapsed_seconds: f64,
    pub capture_seconds: f64,
    pub pid: u32,
    pub start_ticks: u64,
    pub cpu_ticks: u64,
    pub rss_kib: u64,
    pub threads: u64,
    #[serde(default)]
    pub total_fds: Option<usize>,
    pub socket_fds: usize,
    pub socket_inodes: usize,
    pub vanished_fds: usize,
    pub process_tcp_states: BTreeMap<String, usize>,
    pub namespace_tcp_states: BTreeMap<String, usize>,
}

#[derive(Debug, PartialEq, Eq)]
struct ProcessStat {
    pid: u32,
    start_ticks: u64,
    cpu_ticks: u64,
}

fn invalid(detail: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail.to_owned())
}

fn parse_stat(stat: &str) -> io::Result<ProcessStat> {
    let (comm, tail) = stat
        .rsplit_once(')')
        .ok_or_else(|| invalid("missing comm"))?;
    let (pid, _) = comm.split_once('(').ok_or_else(|| invalid("missing pid"))?;
    let fields = tail.split_whitespace().collect::<Vec<_>>();
    let number = |index: usize| -> io::Result<u64> {
        fields
            .get(index)
            .ok_or_else(|| invalid("short stat"))?
            .parse()
            .map_err(|_| invalid("invalid stat number"))
    };
    Ok(ProcessStat {
        pid: pid.trim().parse().map_err(|_| invalid("invalid pid"))?,
        start_ticks: number(19)?,
        cpu_ticks: number(11)?
            .checked_add(number(12)?)
            .ok_or_else(|| invalid("CPU counter overflow"))?,
    })
}

fn status_value(status: &str, key: &str, unit: Option<&str>) -> io::Result<u64> {
    let line = status
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .ok_or_else(|| invalid("missing status field"))?;
    let mut fields = line.split_whitespace();
    let value = fields
        .next()
        .ok_or_else(|| invalid("missing status value"))?
        .parse()
        .map_err(|_| invalid("invalid status value"))?;
    if fields.next() != unit || fields.next().is_some() {
        return Err(invalid("unexpected status unit"));
    }
    Ok(value)
}

fn socket_inode(target: &Path) -> io::Result<Option<u64>> {
    let target = target.to_string_lossy();
    let Some(rest) = target.strip_prefix("socket:[") else {
        return Ok(None);
    };
    let inode = rest
        .strip_suffix(']')
        .ok_or_else(|| invalid("invalid socket link"))?
        .parse()
        .map_err(|_| invalid("invalid socket inode"))?;
    Ok(Some(inode))
}

fn count_tcp(
    table: &str,
    inodes: &BTreeSet<u64>,
    process: &mut BTreeMap<String, usize>,
    namespace: &mut BTreeMap<String, usize>,
) -> io::Result<()> {
    let mut lines = table.lines();
    let header = lines.next().ok_or_else(|| invalid("missing TCP header"))?;
    if !header.split_whitespace().any(|field| field == "inode") {
        return Err(invalid("invalid TCP header"));
    }
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        let state = fields.get(3).ok_or_else(|| invalid("missing TCP state"))?;
        let state = u8::from_str_radix(state, 16).map_err(|_| invalid("invalid TCP state"))?;
        let inode = fields
            .get(9)
            .ok_or_else(|| invalid("missing TCP inode"))?
            .parse::<u64>()
            .map_err(|_| invalid("invalid TCP inode"))?;
        let state = format!("{state:02X}");
        *namespace.entry(state.clone()).or_default() += 1;
        if inode != 0 && inodes.contains(&inode) {
            *process.entry(state).or_default() += 1;
        }
    }
    Ok(())
}

fn validate_identity(before: &ProcessStat, after: &ProcessStat, pid: u32) -> io::Result<()> {
    if before.pid != pid || after.pid != pid || before.start_ticks != after.start_ticks {
        return Err(invalid("sampled process was replaced"));
    }
    if after.cpu_ticks < before.cpu_ticks {
        return Err(invalid("CPU accounting decreased during capture"));
    }
    Ok(())
}

pub fn capture(pid: u32, started: Instant) -> io::Result<ProcessSample> {
    let capture_started = Instant::now();
    let root = std::path::PathBuf::from(format!("/proc/{pid}"));
    let before = parse_stat(&fs::read_to_string(root.join("stat"))?)?;
    let status = fs::read_to_string(root.join("status"))?;
    let mut socket_fds = 0;
    let mut total_fds = 0;
    let mut vanished_fds = 0;
    let mut inodes = BTreeSet::new();
    for entry in fs::read_dir(root.join("fd"))? {
        match fs::read_link(entry?.path()) {
            Ok(target) => {
                total_fds += 1;
                if let Some(inode) = socket_inode(&target)? {
                    socket_fds += 1;
                    inodes.insert(inode);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => vanished_fds += 1,
            Err(error) => return Err(error),
        }
    }
    let mut process_tcp_states = BTreeMap::new();
    let mut namespace_tcp_states = BTreeMap::new();
    for protocol in ["tcp", "tcp6"] {
        count_tcp(
            &fs::read_to_string(root.join("net").join(protocol))?,
            &inodes,
            &mut process_tcp_states,
            &mut namespace_tcp_states,
        )?;
    }
    let after = parse_stat(&fs::read_to_string(root.join("stat"))?)?;
    validate_identity(&before, &after, pid)?;
    Ok(ProcessSample {
        elapsed_seconds: started.elapsed().as_secs_f64(),
        capture_seconds: capture_started.elapsed().as_secs_f64(),
        pid,
        start_ticks: after.start_ticks,
        cpu_ticks: after.cpu_ticks,
        rss_kib: status_value(&status, "VmRSS:", Some("kB"))?,
        threads: status_value(&status, "Threads:", None)?,
        total_fds: Some(total_fds),
        socket_fds,
        socket_inodes: inodes.len(),
        vanished_fds,
        process_tcp_states,
        namespace_tcp_states,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_sample_counts_non_sockets_and_preserves_historical_unknowns() {
        let file = fs::File::open("/dev/null").unwrap();
        let sample = capture(std::process::id(), Instant::now()).unwrap();
        assert!(sample.total_fds.unwrap() > sample.socket_fds);
        let mut json = serde_json::to_value(&sample).unwrap();
        let restored: ProcessSample = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored.total_fds, sample.total_fds);
        json.as_object_mut().unwrap().remove("total_fds");
        let historical: ProcessSample = serde_json::from_value(json).unwrap();
        assert_eq!(historical.total_fds, None);
        drop(file);
    }

    #[test]
    fn tcp_attribution_excludes_other_processes_and_unowned_time_wait() {
        let table = "sl local remote st queues timers retrans uid timeout inode\n\
            0: a b 01 0:0 0:0 0 1000 0 42\n\
            1: a b 0a 0:0 0:0 0 1000 0 43\n\
            2: a b 06 0:0 0:0 0 1000 0 0\n";
        let mut process = BTreeMap::new();
        let mut namespace = BTreeMap::new();
        count_tcp(table, &BTreeSet::from([42]), &mut process, &mut namespace).unwrap();
        assert_eq!(process, BTreeMap::from([("01".to_owned(), 1)]));
        assert_eq!(namespace.len(), 3);
        assert_eq!(namespace["0A"], 1);
        for bad in [
            "",
            "header\n",
            "inode\ntruncated\n",
            "inode\n0: a b ZZ 0 0 0 0 0 42",
        ] {
            assert!(count_tcp(bad, &BTreeSet::new(), &mut process, &mut namespace).is_err());
        }
    }

    #[test]
    fn status_and_socket_parsing_reject_missing_or_mislabelled_values() {
        assert_eq!(
            status_value("VmRSS: 2048 kB\n", "VmRSS:", Some("kB")).unwrap(),
            2048
        );
        for value in ["", "VmRSS: 1 MB", "VmRSS: -1 kB", "VmRSS: 1 kB extra"] {
            assert!(status_value(value, "VmRSS:", Some("kB")).is_err());
        }
        assert_eq!(socket_inode(Path::new("socket:[42]")).unwrap(), Some(42));
        assert_eq!(
            socket_inode(Path::new("anon_inode:[eventpoll]")).unwrap(),
            None
        );
        assert!(socket_inode(Path::new("socket:[bad]")).is_err());
    }

    #[test]
    fn process_identity_and_cpu_resets_invalidate_capture() {
        let first = ProcessStat {
            pid: 42,
            start_ticks: 100,
            cpu_ticks: 200,
        };
        assert!(validate_identity(&first, &first, 42).is_ok());
        for after in [
            ProcessStat {
                pid: 43,
                start_ticks: 100,
                cpu_ticks: 200,
            },
            ProcessStat {
                pid: 42,
                start_ticks: 101,
                cpu_ticks: 200,
            },
            ProcessStat {
                pid: 42,
                start_ticks: 100,
                cpu_ticks: 199,
            },
        ] {
            assert!(validate_identity(&first, &after, 42).is_err());
        }
        let mut fields = vec!["0"; 22];
        fields[0] = "S";
        fields[11] = "17";
        fields[12] = "4";
        fields[19] = "100";
        let stat = format!("42 (comm ) with spaces)) {}", fields.join(" "));
        assert_eq!(
            parse_stat(&stat).unwrap(),
            ProcessStat {
                pid: 42,
                start_ticks: 100,
                cpu_ticks: 21
            }
        );
        assert!(parse_stat("42 (short) S").is_err());
        fields[11] = "18446744073709551615";
        assert!(parse_stat(&format!("42 (comm) {}", fields.join(" "))).is_err());
    }

    #[test]
    fn live_process_capture_observes_its_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let sample = capture(std::process::id(), Instant::now()).unwrap();
        assert!(
            sample
                .process_tcp_states
                .get("0A")
                .copied()
                .unwrap_or_default()
                >= 1
        );
        assert!(sample.socket_fds >= sample.socket_inodes);
        assert!(sample.rss_kib > 0);
        assert!(sample.capture_seconds >= 0.0);
        let duplicate = listener.try_clone().unwrap();
        let duplicated = capture(std::process::id(), Instant::now()).unwrap();
        assert_eq!(duplicated.socket_fds, sample.socket_fds + 1);
        assert_eq!(duplicated.socket_inodes, sample.socket_inodes);
        assert_eq!(duplicated.process_tcp_states, sample.process_tcp_states);
        drop(duplicate);
        drop(listener);
    }
}
