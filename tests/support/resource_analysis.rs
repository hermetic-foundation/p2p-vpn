use super::process_sample::ProcessSample;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    MissingSample,
    InvalidTiming,
    SamplingGap,
    ProcessReplaced,
    CounterReset,
    MissingCounter,
    InvalidClockRate,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Interval {
    pub elapsed_seconds: f64,
    pub cpu_seconds: f64,
    // One fully occupied logical CPU is 100%; multithreaded use may exceed it.
    pub cpu_percent_one_core: f64,
}

pub fn interval(
    before: Option<&ProcessSample>,
    after: Option<&ProcessSample>,
    clock_ticks_per_second: u64,
    maximum_gap_seconds: f64,
) -> Result<Interval, Unavailable> {
    let (Some(before), Some(after)) = (before, after) else {
        return Err(Unavailable::MissingSample);
    };
    if clock_ticks_per_second == 0 {
        return Err(Unavailable::InvalidClockRate);
    }
    if before.pid != after.pid || before.start_ticks != after.start_ticks {
        return Err(Unavailable::ProcessReplaced);
    }
    let elapsed = after.elapsed_seconds - before.elapsed_seconds;
    if !maximum_gap_seconds.is_finite()
        || maximum_gap_seconds <= 0.0
        || !before.elapsed_seconds.is_finite()
        || before.elapsed_seconds < 0.0
        || !elapsed.is_finite()
        || elapsed <= 0.0
        || [before, after].iter().any(|sample| {
            !sample.capture_seconds.is_finite()
                || sample.capture_seconds < 0.0
                || sample.capture_seconds > sample.elapsed_seconds
        })
    {
        return Err(Unavailable::InvalidTiming);
    }
    if elapsed > maximum_gap_seconds {
        return Err(Unavailable::SamplingGap);
    }
    let ticks = counter_delta(Some(before.cpu_ticks), Some(after.cpu_ticks))?;
    #[allow(
        clippy::cast_precision_loss,
        reason = "derived CPU ratios are approximate; raw integer counters remain in samples"
    )]
    let cpu_seconds = ticks as f64 / clock_ticks_per_second as f64;
    Ok(Interval {
        elapsed_seconds: elapsed,
        cpu_seconds,
        cpu_percent_one_core: 100.0 * cpu_seconds / elapsed,
    })
}

pub fn counter_delta(before: Option<u64>, after: Option<u64>) -> Result<u64, Unavailable> {
    let (Some(before), Some(after)) = (before, after) else {
        return Err(Unavailable::MissingCounter);
    };
    after.checked_sub(before).ok_or(Unavailable::CounterReset)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Failed { reason: String },
    Censored { reason: String },
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Comparison {
    Comparable {
        baseline: f64,
        current: f64,
        absolute_delta: f64,
        percent_delta: Option<f64>,
    },
    Unavailable {
        reason: String,
    },
}

// Call only for the same metric/window in an otherwise matched workload pair.
pub fn compare(
    baseline_outcome: &Outcome,
    current_outcome: &Outcome,
    baseline: Option<f64>,
    current: Option<f64>,
) -> Comparison {
    if baseline_outcome != &Outcome::Completed || current_outcome != &Outcome::Completed {
        return Comparison::Unavailable {
            reason: "pair includes failed or censored work".to_owned(),
        };
    }
    let (Some(baseline), Some(current)) = (baseline, current) else {
        return Comparison::Unavailable {
            reason: "metric missing from one or both subjects".to_owned(),
        };
    };
    if !baseline.is_finite() || !current.is_finite() || baseline < 0.0 || current < 0.0 {
        return Comparison::Unavailable {
            reason: "invalid resource metric".to_owned(),
        };
    }
    let absolute_delta = current - baseline;
    let percent_delta = if baseline > 0.0 {
        let percent = (absolute_delta / baseline) * 100.0;
        percent.is_finite().then_some(percent)
    } else {
        None
    };
    Comparison::Comparable {
        baseline,
        current,
        absolute_delta,
        percent_delta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn sample(time: f64, cpu: u64) -> ProcessSample {
        ProcessSample {
            elapsed_seconds: time,
            capture_seconds: 0.001,
            pid: 42,
            start_ticks: 100,
            cpu_ticks: cpu,
            rss_kib: 1024,
            threads: 2,
            socket_fds: 3,
            socket_inodes: 3,
            vanished_fds: 0,
            process_tcp_states: BTreeMap::new(),
            namespace_tcp_states: BTreeMap::new(),
        }
    }

    fn error(before: &ProcessSample, after: &ProcessSample) -> Unavailable {
        interval(Some(before), Some(after), 100, 2.0).unwrap_err()
    }

    #[test]
    #[allow(
        clippy::float_cmp,
        reason = "chosen integer-valued results are exactly representable"
    )]
    fn cpu_normalization_uses_elapsed_time_and_can_exceed_one_core() {
        let before = sample(1.0, 100);
        let after = sample(3.0, 400);
        let result = interval(Some(&before), Some(&after), 100, 2.0).unwrap();
        assert_eq!(result.elapsed_seconds, 2.0);
        assert_eq!(result.cpu_seconds, 3.0);
        assert_eq!(result.cpu_percent_one_core, 150.0);
    }

    #[test]
    fn missing_and_reset_counters_are_not_zero() {
        assert_eq!(
            counter_delta(None, Some(10)),
            Err(Unavailable::MissingCounter)
        );
        assert_eq!(
            counter_delta(Some(10), None),
            Err(Unavailable::MissingCounter)
        );
        assert_eq!(
            counter_delta(Some(10), Some(9)),
            Err(Unavailable::CounterReset)
        );
        assert_eq!(counter_delta(Some(10), Some(10)), Ok(0));
        assert_eq!(counter_delta(Some(u64::MAX - 1), Some(u64::MAX)), Ok(1));
    }

    #[test]
    fn identity_changes_and_missing_observations_invalidate_intervals() {
        let before = sample(1.0, 100);
        let mut after = sample(2.0, 200);
        assert_eq!(
            interval(None, Some(&after), 100, 2.0).unwrap_err(),
            Unavailable::MissingSample
        );
        assert_eq!(
            interval(Some(&before), None, 100, 2.0).unwrap_err(),
            Unavailable::MissingSample
        );
        after.pid += 1;
        assert_eq!(error(&before, &after), Unavailable::ProcessReplaced);
        after.pid = before.pid;
        after.start_ticks += 1;
        assert_eq!(error(&before, &after), Unavailable::ProcessReplaced);
        after.start_ticks = before.start_ticks;
        after.cpu_ticks = 99;
        assert_eq!(error(&before, &after), Unavailable::CounterReset);
    }

    #[test]
    fn invalid_time_and_gaps_are_explicit() {
        let before = sample(1.0, 100);
        for time in [f64::NAN, f64::INFINITY, -1.0, 0.5, 1.0] {
            assert_eq!(
                error(&before, &sample(time, 200)),
                Unavailable::InvalidTiming
            );
        }
        let mut after = sample(4.0, 200);
        assert_eq!(error(&before, &after), Unavailable::SamplingGap);
        after.elapsed_seconds = 2.0;
        for capture in [f64::NAN, f64::INFINITY, -0.1, 3.0] {
            after.capture_seconds = capture;
            assert_eq!(error(&before, &after), Unavailable::InvalidTiming);
        }
        after.capture_seconds = 0.001;
        assert_eq!(
            interval(Some(&before), Some(&after), 0, 2.0).unwrap_err(),
            Unavailable::InvalidClockRate
        );
        for gap in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                interval(Some(&before), Some(&after), 100, gap).unwrap_err(),
                Unavailable::InvalidTiming
            );
        }
    }

    #[test]
    fn missing_serialized_fields_are_not_defaulted() {
        let mut value = serde_json::to_value(sample(1.0, 100)).unwrap();
        value.as_object_mut().unwrap().remove("cpu_ticks");
        assert!(serde_json::from_value::<ProcessSample>(value).is_err());
    }

    #[test]
    fn failed_or_censored_work_cannot_be_reported_as_a_resource_improvement() {
        for outcome in [
            Outcome::Failed {
                reason: "no packets delivered".to_owned(),
            },
            Outcome::Censored {
                reason: "recovery watchdog expired".to_owned(),
            },
        ] {
            assert!(matches!(
                compare(&outcome, &Outcome::Completed, Some(1.0), Some(0.0)),
                Comparison::Unavailable { .. }
            ));
            assert!(matches!(
                compare(&Outcome::Completed, &outcome, Some(1.0), Some(0.0)),
                Comparison::Unavailable { .. }
            ));
            let json = serde_json::to_string(&outcome).unwrap();
            assert_eq!(serde_json::from_str::<Outcome>(&json).unwrap(), outcome);
        }
    }

    #[test]
    fn paired_deltas_preserve_zero_and_unavailable_distinctions() {
        let compare = |baseline, current| {
            super::compare(&Outcome::Completed, &Outcome::Completed, baseline, current)
        };
        assert_eq!(
            compare(Some(100.0), Some(80.0)),
            Comparison::Comparable {
                baseline: 100.0,
                current: 80.0,
                absolute_delta: -20.0,
                percent_delta: Some(-20.0)
            }
        );
        assert_eq!(
            compare(Some(0.0), Some(1.0)),
            Comparison::Comparable {
                baseline: 0.0,
                current: 1.0,
                absolute_delta: 1.0,
                percent_delta: None
            }
        );
        for missing in [None, Some(f64::NAN), Some(f64::INFINITY), Some(-1.0)] {
            assert!(matches!(
                compare(missing, Some(1.0)),
                Comparison::Unavailable { .. }
            ));
            assert!(matches!(
                compare(Some(1.0), missing),
                Comparison::Unavailable { .. }
            ));
        }
    }
}
