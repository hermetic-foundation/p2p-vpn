use super::*;

// Four allowed query phases at 60s each, plus 10s for library cleanup.
pub(super) const PRIMARY_POOL_DRAIN_BUDGET: Duration = Duration::from_secs(250);

// No pairing operation or membership change occurs during this healthy window.
pub(super) const QUIET_COUNTERS: &[&str] = &[
    "redial_attempts",
    "kademlia_provider_lookups",
    "kademlia_provider_dial_attempts",
    "kademlia_provider_advertisements",
    "kademlia_membership_record_lookups",
    "kademlia_membership_record_publications",
    "kademlia_bootstrap_refreshes",
    "auto_relay_reservation_attempts",
    "auto_relay_discovery_queries",
];

pub(super) struct Baseline {
    states: [Vec<String>; 2],
    elapsed: Duration,
    nonempty_since: [Option<Duration>; 2],
}

impl Baseline {
    pub(super) fn new(states: [Vec<String>; 2], elapsed: Duration) -> Self {
        let nonempty_since = std::array::from_fn(|index| {
            (state_metric_count(&states[index], "kad_primary_present").unwrap_or(0) != 0
                && state_metric_count(&states[index], "kad_primary_query_pool_retained")
                    .unwrap_or(0)
                    != 0)
                .then_some(elapsed)
        });
        Self {
            states,
            elapsed,
            nonempty_since,
        }
    }

    pub(super) fn validate(
        &mut self,
        index: usize,
        state: &[String],
        elapsed: Duration,
    ) -> Result<(), String> {
        let before = &self.states[index];
        let get = |lines: &[String], name: &str| {
            state_metric_count(lines, name).ok_or_else(|| format!("missing settling metric {name}"))
        };
        let delta = |name: &str| {
            get(state, name)?
                .checked_sub(get(before, name)?)
                .ok_or_else(|| format!("settling counter regressed: {name}"))
        };
        for (metric, expected) in [
            ("peers_with_supported_path", 1),
            ("peers_without_supported_path", 0),
            ("app_maintenance_queries", 0),
            ("app_recovery_queries", 0),
        ] {
            if get(state, metric)? != expected {
                return Err(format!("unsettled owner {metric}"));
            }
        }
        for metric in QUIET_COUNTERS {
            if delta(metric)? != 0 {
                return Err(format!("redundant healthy activity: {metric}"));
            }
        }
        let duration = elapsed
            .checked_sub(self.elapsed)
            .ok_or("observation time regressed")?;
        // A put has at most two phases. Allow the next 900s renewal and one
        // trailing external-address retirement, then one renewal per 900s.
        // At each crossed hour, at most three records (two address records and
        // one membership aggregate) can enter the library replication job.
        let hours = elapsed.as_secs() / 3_600 - self.elapsed.as_secs() / 3_600;
        let phases = 4 + 2 * (duration.as_secs() / 900) + 6 * hours;
        for prefix in ["kad_primary", "kad_pairing"] {
            if get(state, &format!("{prefix}_present"))? == 0 {
                continue;
            }
            let retained_metric = format!("{prefix}_query_pool_retained");
            let retained = get(state, &retained_metric)?;
            if prefix == "kad_pairing" {
                if retained != 0 || get(before, &retained_metric)? != 0 {
                    return Err("kad_pairing retained queries during quiet window".to_owned());
                }
            } else if retained == 0 {
                self.nonempty_since[index] = None;
            } else {
                let since = self.nonempty_since[index].get_or_insert(elapsed);
                let nonempty_for = elapsed
                    .checked_sub(*since)
                    .ok_or("observation time regressed")?;
                if nonempty_for > PRIMARY_POOL_DRAIN_BUDGET {
                    return Err(
                        "kad_primary query pool did not drain within 250 seconds".to_owned()
                    );
                }
            }
            let limit = if prefix == "kad_primary" { phases } else { 0 } as usize;
            if delta(&format!("{prefix}_query_phases_admitted"))? > limit {
                return Err(format!(
                    "{prefix} exceeded healthy query-phase budget {limit}"
                ));
            }
            // Five built-in bootstrap identities, two fixture relays, one VPN
            // peer. Each phase can attempt a candidate once, not endlessly.
            if delta(&format!("{prefix}_dial_intents_attempted"))? > 8 * limit {
                return Err(format!(
                    "{prefix} exceeded healthy dial budget {}",
                    8 * limit
                ));
            }
            if delta(&format!("{prefix}_query_pool_rejected"))? != 0 {
                return Err(format!("{prefix} exhausted query capacity while healthy"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
fn quiet_state() -> Vec<String> {
    let mut state = QUIET_COUNTERS
        .iter()
        .map(|name| format!("{name} 0"))
        .collect::<Vec<_>>();
    state.extend(
        [
            "app_public_discovery_suppressed 0",
            "peers_with_supported_path 1",
            "peers_without_supported_path 0",
            "app_maintenance_queries 0",
            "app_recovery_queries 0",
            "kad_primary_present 1",
            "kad_pairing_present 1",
            "kad_primary_query_phases_admitted 0",
            "kad_primary_dial_intents_attempted 0",
            "kad_primary_query_pool_rejected 0",
            "kad_primary_query_pool_retained 0",
            "kad_pairing_query_phases_admitted 0",
            "kad_pairing_dial_intents_attempted 0",
            "kad_pairing_query_pool_rejected 0",
            "kad_pairing_query_pool_retained 0",
        ]
        .map(str::to_owned),
    );
    state
}

#[test]
fn settling_baseline_rejects_storms_and_stranded_owners() {
    let state = quiet_state();
    let mut baseline = Baseline::new([state.clone(), state.clone()], Duration::from_secs(100));
    let now = Duration::from_secs(700);
    assert!(baseline.validate(0, &state, now).is_ok());
    for (metric, value) in [
        ("redial_attempts", 1),
        ("app_maintenance_queries", 1),
        ("app_recovery_queries", 1),
        ("peers_with_supported_path", 0),
        ("peers_without_supported_path", 1),
        ("kad_primary_query_phases_admitted", 5),
        ("kad_primary_dial_intents_attempted", 33),
        ("kad_primary_query_pool_rejected", 1),
        ("kad_pairing_query_phases_admitted", 1),
        ("kad_pairing_query_pool_retained", 1),
    ] {
        let mut regression = state.clone();
        *regression
            .iter_mut()
            .find(|line| line.starts_with(&format!("{metric} ")))
            .unwrap() = format!("{metric} {value}");
        assert!(
            baseline.validate(0, &regression, now).is_err(),
            "accepted {metric}"
        );
    }
    let mut renewal = state;
    *renewal
        .iter_mut()
        .find(|line| line.starts_with("kad_primary_query_phases_admitted "))
        .unwrap() = "kad_primary_query_phases_admitted 4".to_owned();
    assert!(baseline.validate(0, &renewal, now).is_ok());
}

#[cfg(test)]
fn with_retained_queries(prefix: &str, count: usize) -> Vec<String> {
    let mut state = quiet_state();
    let metric = format!("{prefix}_query_pool_retained");
    *state
        .iter_mut()
        .find(|line| line.starts_with(&format!("{metric} ")))
        .unwrap() = format!("{metric} {count}");
    state
}

#[test]
fn settling_rejects_primary_query_held_from_baseline_beyond_drain_budget() {
    let held = with_retained_queries("kad_primary", 1);
    let mut baseline = Baseline::new([held.clone(), held.clone()], Duration::from_secs(100));
    assert_eq!(PRIMARY_POOL_DRAIN_BUDGET, Duration::from_secs(250));
    assert!(
        baseline
            .validate(0, &held, Duration::from_secs(350))
            .is_ok()
    );
    let error = baseline
        .validate(0, &held, Duration::from_secs(350) + Duration::from_nanos(1))
        .unwrap_err();
    assert!(error.contains("did not drain within 250 seconds"));
    // Node 1 has not been validated yet: baseline retention must still count.
    assert!(
        baseline
            .validate(1, &held, Duration::from_secs(351))
            .is_err()
    );
}

#[test]
fn settling_primary_drain_resets_independent_node_deadlines() {
    let empty = quiet_state();
    let held = with_retained_queries("kad_primary", 1);
    let mut baseline = Baseline::new([empty.clone(), empty.clone()], Duration::from_secs(100));
    assert!(
        baseline
            .validate(0, &held, Duration::from_secs(110))
            .is_ok()
    );
    assert!(
        baseline
            .validate(1, &held, Duration::from_secs(120))
            .is_ok()
    );
    assert!(
        baseline
            .validate(0, &empty, Duration::from_secs(350))
            .is_ok()
    );
    assert!(
        baseline
            .validate(0, &held, Duration::from_secs(360))
            .is_ok()
    );
    assert!(
        baseline
            .validate(1, &held, Duration::from_secs(370))
            .is_ok()
    );
    assert!(
        baseline
            .validate(1, &held, Duration::from_secs(371))
            .is_err()
    );
    assert!(
        baseline
            .validate(0, &held, Duration::from_secs(610))
            .is_ok()
    );
    assert!(
        baseline
            .validate(0, &empty, Duration::from_secs(610))
            .is_ok()
    );
    assert!(
        baseline
            .validate(0, &empty, Duration::from_secs(900))
            .is_ok()
    );
    assert!(
        baseline
            .validate(0, &held, Duration::from_secs(901))
            .is_ok()
    );
    assert!(
        baseline
            .validate(0, &held, Duration::from_secs(1151))
            .is_ok()
    );
    assert!(
        baseline
            .validate(0, &held, Duration::from_secs(1152))
            .is_err()
    );
}

#[test]
fn settling_rejects_pairing_queries_even_when_only_retained_at_baseline() {
    let empty = quiet_state();
    let held = with_retained_queries("kad_pairing", 1);
    let mut baseline = Baseline::new([held, empty.clone()], Duration::from_secs(100));
    assert!(
        baseline
            .validate(0, &empty, Duration::from_secs(100))
            .is_err()
    );
    assert!(
        baseline
            .validate(1, &empty, Duration::from_secs(100))
            .is_ok()
    );
}
