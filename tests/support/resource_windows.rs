#![allow(
    clippy::cast_precision_loss,
    reason = "summary ratios are approximate; raw integer samples are retained"
)]

use super::{process_sample::ProcessSample, resource_analysis, resource_protocol};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, env, fs, io::Read as _, os::unix::fs::OpenOptionsExt as _};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Observation {
    Metadata {
        protocol_version: u32,
        clock_ticks_per_second: u64,
    },
    StageStart {
        stage: resource_protocol::Stage,
        elapsed_seconds: f64,
    },
    StageEnd {
        stage: String,
        elapsed_seconds: f64,
    },
    Sample {
        stage: String,
        role: String,
        process: Option<ProcessSample>,
    },
    InfrastructureSample {
        stage: String,
        process: Option<ProcessSample>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Serialize)]
pub struct Gauge {
    samples: usize,
    missing_samples: usize,
    mean: Option<f64>,
    sampled_peak: Option<f64>,
    first: Option<f64>,
    last: Option<f64>,
}

fn gauge(samples: &[Option<ProcessSample>], value: impl Fn(&ProcessSample) -> f64) -> Gauge {
    optional_gauge(samples, |sample| Some(value(sample)))
}

fn optional_gauge(
    samples: &[Option<ProcessSample>],
    value: impl Fn(&ProcessSample) -> Option<f64>,
) -> Gauge {
    let values: Vec<_> = samples
        .iter()
        .filter_map(|sample| sample.as_ref().and_then(&value))
        .collect();
    Gauge {
        samples: values.len(),
        missing_samples: samples.len() - values.len(),
        mean: (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64),
        sampled_peak: values.iter().copied().reduce(f64::max),
        first: samples.first().and_then(Option::as_ref).and_then(&value),
        last: samples.last().and_then(Option::as_ref).and_then(value),
    }
}

#[derive(Debug, Serialize)]
pub struct Window {
    stage: String,
    role: String,
    window_seconds: f64,
    sample_count: usize,
    valid_intervals: usize,
    invalid_intervals: Vec<resource_analysis::Unavailable>,
    valid_elapsed_seconds: f64,
    observed_cpu_seconds: f64,
    temporal_coverage: f64,
    cpu_seconds: Option<f64>,
    cpu_percent_one_core: Option<f64>,
    cpu_unavailable_reasons: Vec<&'static str>,
    gauges: BTreeMap<String, Gauge>,
}

fn window(
    stage: &str,
    role: &str,
    start: f64,
    end: f64,
    samples: &[Option<ProcessSample>],
    ticks: u64,
) -> Result<Window, String> {
    if !start.is_finite() || start < 0.0 || !end.is_finite() || end <= start {
        return Err(format!("invalid window bounds for {stage}"));
    }
    if samples
        .iter()
        .flatten()
        .any(|sample| sample.elapsed_seconds < start || sample.elapsed_seconds > end)
    {
        return Err(format!("sample outside {stage} window"));
    }
    let mut valid_intervals = 0;
    let mut invalid_intervals = Vec::new();
    let mut elapsed = 0.0;
    let mut cpu = 0.0;
    for pair in samples.windows(2) {
        match resource_analysis::interval(pair[0].as_ref(), pair[1].as_ref(), ticks, 7.5) {
            Ok(interval) => {
                valid_intervals += 1;
                elapsed += interval.elapsed_seconds;
                cpu += interval.cpu_seconds;
            }
            Err(error) => invalid_intervals.push(error),
        }
    }
    let coverage = elapsed / (end - start);
    let mut reasons = Vec::new();
    if samples.len() < 3 {
        reasons.push("fewer than three samples");
    }
    if ticks == 0 {
        reasons.push("invalid clock rate");
    }
    if !invalid_intervals.is_empty() {
        reasons.push("invalid sample interval");
    }
    if coverage < 0.95 {
        reasons.push("less than 95 percent temporal coverage");
    }
    let comparable = reasons.is_empty();
    let mut gauges = BTreeMap::from([
        (
            "total_fds".to_owned(),
            optional_gauge(samples, |s| s.total_fds.map(|count| count as f64)),
        ),
        ("rss_kib".to_owned(), gauge(samples, |s| s.rss_kib as f64)),
        ("threads".to_owned(), gauge(samples, |s| s.threads as f64)),
        (
            "socket_fds".to_owned(),
            gauge(samples, |s| s.socket_fds as f64),
        ),
        (
            "socket_inodes".to_owned(),
            gauge(samples, |s| s.socket_inodes as f64),
        ),
        (
            "vanished_fds".to_owned(),
            gauge(samples, |s| s.vanished_fds as f64),
        ),
    ]);
    // An absent TCP state in a captured map means no sockets in that state;
    // an absent process capture remains unavailable, not an empty map.
    for (name, state) in [("established", "01"), ("listen", "0A"), ("time_wait", "06")] {
        gauges.insert(
            format!("process_tcp_{name}"),
            gauge(samples, |s| {
                s.process_tcp_states.get(state).copied().unwrap_or(0) as f64
            }),
        );
        gauges.insert(
            format!("namespace_tcp_{name}"),
            gauge(samples, |s| {
                s.namespace_tcp_states.get(state).copied().unwrap_or(0) as f64
            }),
        );
    }
    Ok(Window {
        stage: stage.to_owned(),
        role: role.to_owned(),
        window_seconds: end - start,
        sample_count: samples.len(),
        valid_intervals,
        invalid_intervals,
        valid_elapsed_seconds: elapsed,
        observed_cpu_seconds: cpu,
        temporal_coverage: coverage,
        cpu_seconds: comparable.then_some(cpu),
        cpu_percent_one_core: comparable.then(|| 100.0 * cpu / elapsed),
        cpu_unavailable_reasons: reasons,
        gauges,
    })
}

#[derive(Debug, Serialize)]
pub struct Summary {
    schema_version: u32,
    protocol_version: u32,
    observation_sha256: String,
    scope: &'static str,
    windows: Vec<Window>,
}

pub fn summarize(bytes: &[u8]) -> Result<Summary, String> {
    if bytes.len() as u64 > resource_protocol::OBSERVATION_BYTES {
        return Err("observation file exceeds protocol size limit".to_owned());
    }
    let mut metadata = None;
    let mut starts = BTreeMap::new();
    let mut ends = BTreeMap::new();
    let mut samples: BTreeMap<(String, String), Vec<Option<ProcessSample>>> = BTreeMap::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let observation: Observation = serde_json::from_slice(line)
            .map_err(|_| format!("invalid observation at line {}", index + 1))?;
        match observation {
            Observation::Metadata {
                protocol_version,
                clock_ticks_per_second,
            } => {
                if metadata
                    .replace((protocol_version, clock_ticks_per_second))
                    .is_some()
                {
                    return Err("duplicate metadata".to_owned());
                }
            }
            Observation::StageStart {
                stage,
                elapsed_seconds,
            } => {
                if starts.insert(stage.name, elapsed_seconds).is_some() {
                    return Err("duplicate stage start".to_owned());
                }
            }
            Observation::StageEnd {
                stage,
                elapsed_seconds,
            } => {
                if ends.insert(stage, elapsed_seconds).is_some() {
                    return Err("duplicate stage end".to_owned());
                }
            }
            Observation::Sample {
                stage,
                role,
                process,
            } => {
                if !["a", "b"].contains(&role.as_str()) {
                    return Err("unknown endpoint role".to_owned());
                }
                samples.entry((stage, role)).or_default().push(process);
            }
            Observation::InfrastructureSample { stage, process } => {
                samples
                    .entry((stage, "infrastructure".to_owned()))
                    .or_default()
                    .push(process);
            }
            Observation::Other => {}
        }
    }
    let (version, ticks) = metadata.ok_or("missing metadata")?;
    if ![2, 3].contains(&version) {
        return Err("unsupported observation protocol version".to_owned());
    }
    if starts.is_empty() || starts.keys().ne(ends.keys()) {
        return Err(
            "missing stage boundary; partial run requires explicit censoring analysis".to_owned(),
        );
    }
    let mut windows = Vec::new();
    for (stage, start) in starts {
        for role in ["a", "b", "infrastructure"] {
            let values = samples
                .remove(&(stage.clone(), role.to_owned()))
                .unwrap_or_default();
            windows.push(window(&stage, role, start, ends[&stage], &values, ticks)?);
        }
    }
    if !samples.is_empty() {
        return Err("samples without stage boundaries".to_owned());
    }
    Ok(Summary {
        schema_version: 1,
        protocol_version: version,
        observation_sha256: Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        scope: "process windows only; not a paired efficiency comparison or campaign outcome",
        windows,
    })
}

pub fn run() -> Result<(), String> {
    let input = env::var("P2P_VPN_RESOURCE_OBSERVATIONS").map_err(|error| error.to_string())?;
    let output = env::var("P2P_VPN_RESOURCE_SUMMARY").map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    fs::File::open(input)
        .map_err(|error| error.to_string())?
        .take(resource_protocol::OBSERVATION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let summary = summarize(&bytes)?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(&file, &summary).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(time: f64, ticks: u64) -> Option<ProcessSample> {
        Some(ProcessSample {
            elapsed_seconds: time,
            capture_seconds: 0.0,
            pid: 10,
            start_ticks: 100,
            cpu_ticks: ticks,
            rss_kib: 1024,
            threads: 2,
            total_fds: Some(4),
            socket_fds: 3,
            socket_inodes: 3,
            vanished_fds: 0,
            process_tcp_states: BTreeMap::new(),
            namespace_tcp_states: BTreeMap::new(),
        })
    }
    #[test]
    fn missing_historical_descriptor_counts_are_not_zero() {
        let mut historical = sample(0.0, 0);
        historical.as_mut().unwrap().total_fds = None;
        let samples = [historical, sample(5.0, 1), None];
        let result = window("idle", "a", 0.0, 10.0, &samples, 100).unwrap();
        let descriptors = &result.gauges["total_fds"];
        assert_eq!(descriptors.samples, 1);
        assert_eq!(descriptors.missing_samples, 2);
        assert_eq!(descriptors.mean, Some(4.0));
        assert_eq!(descriptors.first, None);
        assert_eq!(descriptors.last, None);
    }

    #[test]
    fn weighted_cpu_and_sample_gauges_use_distinct_denominators() {
        let samples = [sample(0.0, 0), sample(4.0, 100), sample(10.0, 300)];
        let result = window("idle", "a", 0.0, 10.0, &samples, 100).unwrap();
        assert_eq!(result.cpu_percent_one_core, Some(30.0));
        assert_eq!(result.gauges["rss_kib"].mean, Some(1024.0));
        assert_eq!(result.valid_intervals, 2);
    }
    #[test]
    fn missing_samples_and_gaps_cannot_become_zero_usage() {
        for samples in [
            vec![None, None, None],
            vec![sample(0.0, 0), None, sample(10.0, 100)],
            vec![sample(0.0, 0), sample(9.0, 10), sample(10.0, 100)],
        ] {
            let result = window("idle", "a", 0.0, 10.0, &samples, 100).unwrap();
            assert!(result.cpu_seconds.is_none());
            assert!(!result.invalid_intervals.is_empty());
        }
        let result = window("idle", "a", 0.0, 10.0, &[None, None, None], 100).unwrap();
        assert!(result.gauges["rss_kib"].mean.is_none());
        assert!(result.gauges["process_tcp_established"].mean.is_none());
    }
    #[test]
    fn coverage_sample_count_and_counter_identity_are_required() {
        for samples in [
            vec![sample(0.0, 0), sample(5.0, 1)],
            vec![sample(1.0, 0), sample(5.0, 1), sample(9.0, 2)],
            vec![sample(0.0, 5), sample(5.0, 0), sample(10.0, 5)],
        ] {
            assert!(
                window("idle", "a", 0.0, 10.0, &samples, 100)
                    .unwrap()
                    .cpu_seconds
                    .is_none()
            );
        }
        let mut changed = sample(5.0, 50);
        changed.as_mut().unwrap().start_ticks += 1;
        assert!(
            window(
                "idle",
                "a",
                0.0,
                10.0,
                &[sample(0.0, 0), changed, sample(10.0, 100)],
                100
            )
            .unwrap()
            .cpu_seconds
            .is_none()
        );
    }
    #[test]
    fn parser_separates_roles_and_never_serializes_identity_metadata() {
        let mut lines = vec![
            serde_json::json!({"kind": "metadata", "protocol_version": resource_protocol::VERSION, "clock_ticks_per_second": 100, "effective_configs": {"private_key": "secret"}}),
            serde_json::json!({"kind": "stage_start", "stage": {"name": "idle", "seconds": 10, "action": "none", "probe_each_sample": false}, "elapsed_seconds": 0.0}),
        ];
        for (time, ticks) in [(0.0, 0), (5.0, 100), (10.0, 200)] {
            lines.push(serde_json::json!({"kind": "sample", "stage": "idle", "role": "a", "process": sample(time, ticks)}));
        }
        let incomplete = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(summarize(incomplete.as_bytes()).is_err());
        lines.push(
            serde_json::json!({"kind": "stage_end", "stage": "idle", "elapsed_seconds": 10.0}),
        );
        let bytes = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let result = summarize(bytes.as_bytes()).unwrap();
        assert_eq!(result.windows.len(), 3);
        assert_eq!(result.windows[0].cpu_percent_one_core, Some(20.0));
        assert!(result.windows[1].cpu_seconds.is_none());
        assert_eq!(result.observation_sha256.len(), 64);
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("private_key"));
        lines[0]["protocol_version"] = serde_json::json!(2);
        let archived = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(summarize(archived.as_bytes()).unwrap().protocol_version, 2);
        lines[0]["protocol_version"] = serde_json::json!(4);
        let future = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(summarize(future.as_bytes()).is_err());
    }

    #[test]
    fn missing_endpoints_are_not_replaced_with_first_available_gauges() {
        let values = [None, sample(5.0, 10), None];
        let result = gauge(&values, |s| s.rss_kib as f64);
        assert_eq!(result.missing_samples, 2);
        assert_eq!(result.mean, Some(1024.0));
        assert!(result.first.is_none());
        assert!(result.last.is_none());
        assert!(
            window(
                "idle",
                "a",
                0.0,
                5.0,
                &[sample(0.0, 0), sample(5.0, 10)],
                100
            )
            .unwrap()
            .cpu_seconds
            .is_none()
        );
    }

    #[test]
    fn parser_rejects_malformed_and_incomplete_evidence_without_echoing_secrets() {
        assert!(summarize(b"{private-key").unwrap_err().contains("line 1"));
        assert!(
            !summarize(b"{private-key")
                .unwrap_err()
                .contains("private-key")
        );
        assert!(summarize(b"{}").is_err());
        assert!(window("idle", "a", 0.0, 10.0, &[sample(11.0, 1)], 100).is_err());
    }
}
