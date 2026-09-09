use std::{
    collections::BTreeMap,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use p2p_vpn::runtime::control_socket::query_status;
use serde::Serialize;
use serde_json::Value;

const INTERVAL: Duration = Duration::from_secs(5);

#[derive(Serialize)]
pub struct Observation {
    scheduled_seconds: f64,
    elapsed_seconds: f64,
    capture_seconds: f64,
    role: String,
    values: Option<BTreeMap<String, Value>>,
    error: Option<String>,
}

pub fn complete(rows: &[Observation], roles: usize, duration: Duration) -> bool {
    let slots = duration.as_secs().div_ceil(INTERVAL.as_secs());
    rows.len() as u64 == slots * roles as u64
        && rows
            .iter()
            .all(|row| row.values.is_some() && row.error.is_none())
}

fn parse(lines: &[String]) -> Result<BTreeMap<String, Value>, String> {
    if lines.is_empty() || lines.len() > 512 {
        return Err("invalid counter count".to_owned());
    }
    let mut values = BTreeMap::new();
    for line in lines {
        if line.len() > 256 {
            return Err("counter line exceeds limit".to_owned());
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 2
            || !fields[0]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            return Err("invalid counter shape".to_owned());
        }
        let value = match fields[1] {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            text => Value::from(text.parse::<u64>().map_err(|_| "invalid counter value")?),
        };
        if values.insert(fields[0].to_owned(), value).is_some() {
            return Err("duplicate counter".to_owned());
        }
    }
    Ok(values)
}

fn next_slot(slot: Duration, elapsed: Duration) -> Duration {
    let mut next = slot + INTERVAL;
    while next <= elapsed {
        next += INTERVAL;
    }
    next
}

pub fn capture(
    temp: &Path,
    roles: &[(&str, u32)],
    started: Instant,
    duration: Duration,
) -> Vec<Observation> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut rows = Vec::new();
    let mut scheduled = Duration::ZERO;
    while scheduled < duration {
        thread::sleep(scheduled.saturating_sub(started.elapsed()));
        if started.elapsed() >= duration {
            break;
        }
        for (role, _) in roles {
            let capture_started = Instant::now();
            let result = runtime
                .block_on(query_status(
                    &super::node_control_socket(temp, role),
                    Duration::from_secs(1),
                ))
                .map_err(|error| format!("status query: {error:?}"))
                .and_then(|lines| parse(&lines));
            let (values, error) = match result {
                Ok(values) => (Some(values), None),
                Err(error) => (None, Some(error)),
            };
            rows.push(Observation {
                scheduled_seconds: scheduled.as_secs_f64(),
                elapsed_seconds: started.elapsed().as_secs_f64(),
                capture_seconds: capture_started.elapsed().as_secs_f64(),
                role: (*role).to_owned(),
                values,
                error,
            });
        }
        scheduled = next_slot(scheduled, started.elapsed());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_preserve_zero_flags_and_reject_corrupt_or_unbounded_input() {
        let lines = [
            "queue_queued_bytes 0".to_owned(),
            "auto_relay_private_reachability false".to_owned(),
        ];
        let values = parse(&lines).unwrap();
        assert_eq!(values["queue_queued_bytes"], Value::from(0));
        assert_eq!(
            values["auto_relay_private_reachability"],
            Value::Bool(false)
        );
        assert!(!values.contains_key("missing"));
        for bad in [
            vec![],
            vec!["x -1".to_owned()],
            vec!["x 1 extra".to_owned()],
            vec!["x 1".to_owned(); 2],
            vec!["x 1".to_owned(); 513],
            vec![format!("{} 1", "x".repeat(256))],
        ] {
            assert!(parse(&bad).is_err());
        }
    }

    #[test]
    fn failed_and_missing_snapshots_are_incomplete() {
        let row = Observation {
            scheduled_seconds: 0.0,
            elapsed_seconds: 0.1,
            capture_seconds: 0.1,
            role: "a".to_owned(),
            values: Some(BTreeMap::new()),
            error: None,
        };
        assert!(complete(
            std::slice::from_ref(&row),
            1,
            Duration::from_secs(5)
        ));
        assert!(!complete(
            std::slice::from_ref(&row),
            2,
            Duration::from_secs(5)
        ));
        let failed = Observation {
            values: None,
            error: Some("query timeout".to_owned()),
            ..row
        };
        assert!(!complete(&[failed], 1, Duration::from_secs(5)));
    }

    #[test]
    fn slow_queries_skip_slots_without_bursting() {
        assert_eq!(
            next_slot(Duration::ZERO, Duration::from_secs(1)),
            Duration::from_secs(5)
        );
        assert_eq!(
            next_slot(Duration::ZERO, Duration::from_secs(12)),
            Duration::from_secs(15)
        );
        assert_eq!(
            next_slot(Duration::from_secs(5), Duration::from_secs(10)),
            Duration::from_secs(15)
        );
    }
}
