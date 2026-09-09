use super::resource_analysis::{self, Comparison, Outcome};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, String>;
type Measurements = BTreeMap<(String, String, String), Option<f64>>;

fn outcome(run: &Value) -> Result<Outcome> {
    let reason = run["reason"].as_str().unwrap_or("unspecified").to_owned();
    match run["status"].as_str() {
        Some("completed") => Ok(Outcome::Completed),
        Some("censored") => Ok(Outcome::Censored { reason }),
        Some("failed") => Ok(Outcome::Failed { reason }),
        _ => Err("unknown run outcome".to_owned()),
    }
}

fn work_compatible(b: &Value, c: &Value) -> bool {
    if b["workload"] != c["workload"] || b["profile"] != c["profile"] {
        return false;
    }
    match b["workload"].as_str() {
        Some("traffic" | "pressure") => ["sent", "received", "payload_bytes"].iter().all(|field| {
            b["useful_work"][field].as_u64().is_some()
                && b["useful_work"][field] == c["useful_work"][field]
        }),
        Some("idle" | "recovery") => true,
        _ => false,
    }
}

fn measurements(run: &Value, catalog: &[Value]) -> Result<Measurements> {
    let mut result = BTreeMap::new();
    for w in run["measurements"]["control_windows"]
        .as_array()
        .ok_or("missing control windows")?
    {
        let stage = w["stage"].as_str().ok_or("missing stage")?;
        let role = w["role"].as_str().ok_or("missing role")?;
        for metric in catalog
            .iter()
            .filter(|m| m["availability"] == "common" && m["kind"] == "counter")
        {
            let name = metric["name"].as_str().ok_or("missing metric name")?;
            for field in ["delta", "per_second"] {
                let value = (w["partial"] == false)
                    .then(|| w["metrics"][name][field].as_f64())
                    .flatten();
                result.insert(
                    (stage.to_owned(), role.to_owned(), format!("{name}.{field}")),
                    value,
                );
            }
        }
    }
    if let Some(windows) = run["measurements"]["process"]["windows"].as_array() {
        for w in windows {
            let stage = w["stage"].as_str().ok_or("missing process stage")?;
            let role = w["role"].as_str().ok_or("missing process role")?;
            for field in ["cpu_seconds", "cpu_percent_one_core"] {
                result.insert(
                    (stage.to_owned(), role.to_owned(), field.to_owned()),
                    w[field].as_f64(),
                );
            }
            let valid = w["sample_count"].as_u64().is_some_and(|n| n >= 3)
                && w["temporal_coverage"].as_f64().is_some_and(|n| n >= 0.95)
                && w["invalid_intervals"].as_array().is_some_and(Vec::is_empty);
            for (name, gauge) in w["gauges"].as_object().ok_or("missing process gauges")? {
                for field in ["mean", "sampled_peak", "first", "last"] {
                    let value = (valid && gauge["missing_samples"] == 0)
                        .then(|| gauge[field].as_f64())
                        .flatten();
                    result.insert(
                        (stage.to_owned(), role.to_owned(), format!("{name}.{field}")),
                        value,
                    );
                }
            }
        }
    }
    Ok(result)
}

fn stats(mut values: Vec<f64>) -> Value {
    if values.is_empty() {
        return json!({"eligible_pairs":0,"median":null,"minimum":null,"maximum":null});
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let median = if n % 2 == 0 {
        values[n / 2 - 1] / 2.0 + values[n / 2] / 2.0
    } else {
        values[n / 2]
    };
    json!({"eligible_pairs":n,"median":median,"minimum":values[0],"maximum":values[n-1]})
}

pub fn summarize(dataset: &Value) -> Result<Value> {
    let runs = dataset["runs"].as_array().ok_or("missing runs")?;
    let catalog = dataset["catalog"].as_array().ok_or("missing catalog")?;
    let mut groups: BTreeMap<(u64, u64), Vec<&Value>> = BTreeMap::new();
    for run in runs {
        groups
            .entry((
                run["repetition"].as_u64().ok_or("missing repetition")?,
                run["cell"].as_u64().ok_or("missing cell")?,
            ))
            .or_default()
            .push(run);
    }
    let mut pairs = Vec::new();
    let mut aggregate: BTreeMap<(u64, String, String, String), Vec<Value>> = BTreeMap::new();
    for ((repetition, cell), pair) in groups {
        if pair.len() != 2 {
            return Err("expected two subjects per pair".to_owned());
        }
        let b = pair
            .iter()
            .find(|r| r["subject"] == "baseline")
            .ok_or("missing baseline")?;
        let c = pair
            .iter()
            .find(|r| r["subject"] == "current")
            .ok_or("missing current")?;
        let bo = outcome(b)?;
        let co = outcome(c)?;
        let compatible = work_compatible(b, c);
        let bv = measurements(b, catalog)?;
        let cv = measurements(c, catalog)?;
        let keys: BTreeSet<_> = bv.keys().chain(cv.keys()).cloned().collect();
        let mut rows = Vec::new();
        for (stage, role, metric) in keys {
            let key = (stage.clone(), role.clone(), metric.clone());
            let baseline = bv.get(&key).copied().flatten();
            let current = cv.get(&key).copied().flatten();
            let comparison = if bo != Outcome::Completed || co != Outcome::Completed {
                resource_analysis::compare(&bo, &co, baseline, current)
            } else if !compatible {
                Comparison::Unavailable {
                    reason: "offered/delivered work differs or workload identity mismatches"
                        .to_owned(),
                }
            } else {
                resource_analysis::compare(&bo, &co, baseline, current)
            };
            let comparison = serde_json::to_value(comparison).map_err(|e| e.to_string())?;
            aggregate
                .entry((cell, stage.clone(), role.clone(), metric.clone()))
                .or_default()
                .push(json!({"repetition":repetition,"comparison":comparison}));
            rows.push(json!({"stage":stage,"role":role,"metric":metric,"baseline":baseline,"current":current,"comparison":comparison}));
        }
        pairs.push(json!({"repetition":repetition,"cell":cell,"profile":b["profile"],"workload":b["workload"],"baseline_outcome":bo,"current_outcome":co,"work_compatible":compatible,"baseline_work":b["useful_work"],"current_work":c["useful_work"],"metrics":rows}));
    }
    let cells: Vec<_> = aggregate.into_iter().map(|((cell,stage,role,metric),observations)| {
        let mut statistics = BTreeMap::new();
        for field in ["baseline","current","absolute_delta","percent_delta"] {
            statistics.insert(field,stats(observations.iter().filter(|r| r["comparison"]["status"] == "comparable").filter_map(|r| r["comparison"][field].as_f64()).collect()));
        }
        json!({"cell":cell,"stage":stage,"role":role,"metric":metric,"recorded_repetitions":observations.len(),"statistics":statistics,"outcomes":observations})
    }).collect();
    Ok(
        json!({"policy":"Completed outcomes, matching windows, valid coverage and identical offered/received packet counts; current-only metrics excluded from paired deltas", "pairs":pairs,"cell_statistics":cells}),
    )
}

#[test]
fn incompatible_useful_work_and_profiles_are_not_equivalent() {
    let baseline = json!({"profile":"public","workload":"pressure","useful_work":{"sent":12000,"received":300,"payload_bytes":1000}});
    let mut current = baseline.clone();
    assert!(work_compatible(&baseline, &current));
    current["useful_work"]["received"] = json!(301);
    assert!(!work_compatible(&baseline, &current));
    current = baseline.clone();
    current["profile"] = json!("private");
    assert!(!work_compatible(&baseline, &current));
}

#[test]
fn statistics_use_actual_eligible_count_and_handle_empty_and_even_sets() {
    assert_eq!(stats(vec![])["eligible_pairs"], 0);
    assert_eq!(stats(vec![1.0, 9.0])["median"], 5.0);
    assert_eq!(stats(vec![9.0, 1.0, 2.0])["median"], 2.0);
    assert!(stats(vec![])["median"].is_null());
}

#[test]
fn missing_or_duplicate_subjects_are_rejected() {
    let run = json!({"repetition":1,"cell":0,"subject":"baseline"});
    assert!(summarize(&json!({"runs":[run],"catalog":[]})).is_err());
    assert!(summarize(&json!({"runs":[run,run],"catalog":[]})).is_err());
}

#[test]
fn paired_gating_preserves_zero_and_rejects_censoring_partial_or_missing_metrics() {
    let baseline = json!({"repetition":1,"cell":0,"subject":"baseline","status":"completed","profile":"public","workload":"idle","measurements":{"control_windows":[{"stage":"idle","role":"a","partial":false,"metrics":{"n":{"delta":0,"per_second":0}}}]}});
    let mut current = baseline.clone();
    current["subject"] = json!("current");
    let catalog = json!([{"name":"n","kind":"counter","availability":"common"},{"name":"private","kind":"counter","availability":"current_only"}]);
    let result = summarize(&json!({"catalog":catalog,"runs":[baseline,current]})).unwrap();
    let rows = result["pairs"][0]["metrics"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["comparison"]["status"], "comparable");
    assert!(rows[0]["comparison"]["percent_delta"].is_null());
    for mode in 0..3 {
        let mut changed = current.clone();
        match mode {
            0 => changed["status"] = json!("censored"),
            1 => changed["measurements"]["control_windows"][0]["partial"] = json!(true),
            _ => changed["measurements"]["control_windows"][0]["metrics"]["n"] = Value::Null,
        }
        let result = summarize(&json!({"catalog":catalog,"runs":[baseline,changed]})).unwrap();
        assert_eq!(
            result["pairs"][0]["metrics"][0]["comparison"]["status"],
            "unavailable"
        );
        assert_eq!(
            result["cell_statistics"][0]["statistics"]["absolute_delta"]["eligible_pairs"],
            0
        );
    }
}
