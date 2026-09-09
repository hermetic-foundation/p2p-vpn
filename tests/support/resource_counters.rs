use super::{process_sample::ProcessSample, resource_analysis};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub struct Capture {
    pub process: Option<ProcessSample>,
    pub values: BTreeMap<String, Option<u64>>,
}

#[derive(Debug, Serialize)]
pub struct CounterWindow {
    pub samples: usize,
    pub missing_values: usize,
    pub first: Option<u64>,
    pub last: Option<u64>,
    pub valid_intervals: usize,
    pub valid_seconds: f64,
    pub valid_delta: u64,
    pub invalid_intervals: Vec<resource_analysis::Unavailable>,
    pub coverage: f64,
    pub delta: Option<u64>,
    pub per_second: Option<f64>,
    pub unavailable_reasons: Vec<&'static str>,
}

/// `valid_delta` is diagnostic partial work; only `delta` is eligible for comparison.
pub fn window(
    captures: &[Capture],
    name: &str,
    start: f64,
    end: f64,
    ticks: u64,
) -> Result<CounterWindow, &'static str> {
    if !start.is_finite() || start < 0.0 || !end.is_finite() || end <= start {
        return Err("invalid counter window bounds");
    }
    if captures.iter().filter_map(|c| c.process.as_ref()).any(|p| {
        !p.elapsed_seconds.is_finite() || p.elapsed_seconds < start || p.elapsed_seconds > end
    }) {
        return Err("counter capture outside window");
    }
    let value = |capture: &Capture| capture.values.get(name).copied().flatten();
    let mut result = CounterWindow {
        samples: captures.len(),
        missing_values: captures.iter().filter(|c| value(c).is_none()).count(),
        first: captures.first().and_then(value),
        last: captures.last().and_then(value),
        valid_intervals: 0,
        valid_seconds: 0.0,
        valid_delta: 0,
        invalid_intervals: Vec::new(),
        coverage: 0.0,
        delta: None,
        per_second: None,
        unavailable_reasons: Vec::new(),
    };
    for pair in captures.windows(2) {
        let interval = resource_analysis::interval(
            pair[0].process.as_ref(),
            pair[1].process.as_ref(),
            ticks,
            7.5,
        );
        match interval.and_then(|interval| {
            resource_analysis::counter_delta(value(&pair[0]), value(&pair[1]))
                .map(|delta| (interval, delta))
        }) {
            Ok((interval, delta)) => {
                result.valid_delta = result
                    .valid_delta
                    .checked_add(delta)
                    .ok_or("counter delta sum overflow")?;
                result.valid_seconds += interval.elapsed_seconds;
                result.valid_intervals += 1;
            }
            Err(error) => result.invalid_intervals.push(error),
        }
    }
    result.coverage = result.valid_seconds / (end - start);
    if result.samples < 3 {
        result.unavailable_reasons.push("fewer than three samples");
    }
    if result.missing_values > 0 {
        result.unavailable_reasons.push("missing counter values");
    }
    if !result.invalid_intervals.is_empty() {
        result.unavailable_reasons.push("invalid sample interval");
    }
    if result.coverage < 0.95 {
        result
            .unavailable_reasons
            .push("less than 95 percent temporal coverage");
    }
    if result.unavailable_reasons.is_empty() {
        result.delta = Some(result.valid_delta);
        #[allow(
            clippy::cast_precision_loss,
            reason = "derived rates are approximate; raw integer deltas retained"
        )]
        {
            result.per_second = Some(result.valid_delta as f64 / result.valid_seconds);
        }
    }
    Ok(result)
}

/// Extract only explicitly selected numeric fields, never raw control payloads.
pub fn parse(
    lines: Option<&Value>,
    selected: &[&str],
) -> Result<BTreeMap<String, Option<u64>>, &'static str> {
    let mut result: BTreeMap<_, _> = selected
        .iter()
        .map(|name| ((*name).to_owned(), None))
        .collect();
    let Some(lines) = lines.filter(|value| !value.is_null()) else {
        return Ok(result);
    };
    let lines = lines.as_array().ok_or("control capture is not an array")?;
    if lines.len() > 16384 {
        return Err("too many control lines");
    }
    let mut seen = BTreeSet::new();
    let mut total = 0_usize;
    for line in lines {
        let line = line.as_str().ok_or("control line is not a string")?;
        total = total
            .checked_add(line.len())
            .ok_or("control size overflow")?;
        if total > 1024 * 1024 {
            return Err("control capture exceeds one MiB");
        }
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(slot) = result.get_mut(name) else {
            continue;
        };
        if !seen.insert(name) {
            return Err("duplicate selected metric");
        }
        let value = fields.next().ok_or("selected metric has no value")?;
        if fields.next().is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit())
        {
            return Err("selected metric is not an unsigned integer");
        }
        *slot = Some(value.parse().map_err(|_| "selected metric overflows u64")?);
    }
    Ok(result)
}

#[test]
fn missing_and_zero_are_distinct_and_unselected_data_is_discarded() {
    let selected = ["attempts", "current_only"];
    let baseline = serde_json::json!(["attempts 0", "peer private-key-sensitive-payload"]);
    let parsed = parse(Some(&baseline), &selected).unwrap();
    assert_eq!(parsed["attempts"], Some(0));
    assert_eq!(parsed["current_only"], None);
    assert_eq!(parsed.len(), 2);
    assert!(
        parse(None, &selected)
            .unwrap()
            .values()
            .all(Option::is_none)
    );
    let current = serde_json::json!(["attempts 9", "current_only 3"]);
    assert_eq!(
        parse(Some(&current), &selected).unwrap()["current_only"],
        Some(3)
    );
}

#[test]
fn duplicate_malformed_and_overflowing_selected_fields_fail_closed() {
    for lines in [
        serde_json::json!(["n 1", "n 1"]),
        serde_json::json!(["n -1"]),
        serde_json::json!(["n +1"]),
        serde_json::json!(["n 1 extra"]),
        serde_json::json!(["n 18446744073709551616"]),
        serde_json::json!(["n"]),
        serde_json::json!([false]),
        serde_json::json!({"n":1}),
    ] {
        assert!(parse(Some(&lines), &["n"]).is_err());
    }
    assert_eq!(
        parse(Some(&serde_json::json!(["n 18446744073709551615"])), &["n"]).unwrap()["n"],
        Some(u64::MAX)
    );
}

#[test]
fn control_capture_bounds_apply_even_to_unselected_fields() {
    let oversized = serde_json::json!(["x".repeat(1024 * 1024 + 1)]);
    assert!(parse(Some(&oversized), &["n"]).is_err());
    let many = serde_json::json!(vec![""; 16385]);
    assert!(parse(Some(&many), &["n"]).is_err());
}

#[cfg(test)]
mod window_tests {
    use super::*;

    fn capture(at: f64, count: Option<u64>) -> Capture {
        Capture {
            process: Some(ProcessSample {
                elapsed_seconds: at,
                capture_seconds: 0.0,
                pid: 10,
                start_ticks: 1,
                cpu_ticks: 0,
                rss_kib: 1,
                threads: 1,
                socket_fds: 0,
                socket_inodes: 0,
                vanished_fds: 0,
                process_tcp_states: BTreeMap::new(),
                namespace_tcp_states: BTreeMap::new(),
            }),
            values: BTreeMap::from([("n".to_owned(), count)]),
        }
    }

    #[test]
    fn valid_counter_rate_uses_elapsed_time_and_keeps_zero() {
        let captures = [
            capture(0.0, Some(10)),
            capture(4.0, Some(12)),
            capture(10.0, Some(20)),
        ];
        let result = window(&captures, "n", 0.0, 10.0, 100).unwrap();
        assert_eq!(result.delta, Some(10));
        assert_eq!(result.per_second, Some(1.0));
        let captures = [
            capture(0.0, Some(0)),
            capture(5.0, Some(0)),
            capture(10.0, Some(0)),
        ];
        assert_eq!(
            window(&captures, "n", 0.0, 10.0, 100).unwrap().delta,
            Some(0)
        );
    }

    #[test]
    fn reset_missing_replacement_and_gap_prevent_full_delta() {
        for mode in 0..5 {
            let mut captures = [
                capture(0.0, Some(10)),
                capture(5.0, Some(12)),
                capture(10.0, Some(14)),
            ];
            match mode {
                0 => {
                    captures[1].values.insert("n".to_owned(), Some(1));
                }
                1 => {
                    captures[1].values.clear();
                }
                2 => {
                    captures[1].process.as_mut().unwrap().pid = 11;
                }
                3 => {
                    captures[1].process.as_mut().unwrap().elapsed_seconds = 9.0;
                }
                _ => {
                    captures[1].process = None;
                }
            }
            let result = window(&captures, "n", 0.0, 10.0, 100).unwrap();
            assert_eq!(result.delta, None);
            assert_eq!(result.per_second, None);
            assert!(!result.invalid_intervals.is_empty());
        }
    }

    #[test]
    fn partial_coverage_is_not_full_work_and_invalid_bounds_fail() {
        let captures = [
            capture(5.0, Some(1)),
            capture(10.0, Some(3)),
            capture(15.0, Some(5)),
        ];
        let result = window(&captures, "n", 0.0, 20.0, 100).unwrap();
        assert_eq!(result.valid_delta, 4);
        assert_eq!(result.coverage, 0.5);
        assert_eq!(result.delta, None);
        assert!(window(&captures, "n", 0.0, 10.0, 100).is_err());
        assert!(window(&captures, "n", f64::NAN, 20.0, 100).is_err());
    }
}
