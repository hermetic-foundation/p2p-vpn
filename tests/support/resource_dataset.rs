use super::{
    resource_collection,
    resource_counters::{self, Capture},
    resource_metrics::{self, Kind},
    resource_protocol, resource_windows,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap, env, fs, io::Read as _, os::unix::fs::OpenOptionsExt as _, path::Path,
};

type Result<T> = std::result::Result<T, String>;

fn provenance(mut value: Value) -> Result<Value> {
    for pair in value["pairs"].as_array_mut().ok_or("missing pairs")? {
        for run in pair["runs"].as_array_mut().ok_or("missing runs")? {
            let run = run.as_object_mut().ok_or("invalid run")?;
            // Recompute derived floating-point summaries from hash-checked input.
            run.remove("integrity");
            run.remove("elapsed_seconds");
        }
    }
    Ok(value)
}

pub fn summarize(data: &[u8]) -> Result<Value> {
    let metrics = resource_metrics::catalog();
    let selected: Vec<_> = metrics.iter().map(|m| m.name.as_str()).collect();
    let mut samples: BTreeMap<(String, String), Vec<Capture>> = BTreeMap::new();
    let mut bounds: BTreeMap<String, (f64, Option<f64>)> = BTreeMap::new();
    let mut ticks = None;
    for line in data.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let v: Value = serde_json::from_slice(line).map_err(|e| e.to_string())?;
        match v["kind"].as_str() {
            Some("metadata") => {
                ticks = v["clock_ticks_per_second"].as_u64();
            }
            Some("stage_start") => {
                let name = v["stage"]["name"].as_str().ok_or("missing stage name")?;
                if bounds
                    .insert(
                        name.to_owned(),
                        (
                            v["elapsed_seconds"].as_f64().ok_or("missing stage start")?,
                            None,
                        ),
                    )
                    .is_some()
                {
                    return Err("duplicate stage".to_owned());
                }
            }
            Some("stage_end") => {
                let name = v["stage"].as_str().ok_or("missing stage end name")?;
                bounds.get_mut(name).ok_or("end without start")?.1 =
                    Some(v["elapsed_seconds"].as_f64().ok_or("missing stage end")?);
            }
            Some("sample") => {
                let stage = v["stage"]
                    .as_str()
                    .ok_or("missing sample stage")?
                    .to_owned();
                let role = v["role"].as_str().ok_or("missing role")?.to_owned();
                let process =
                    serde_json::from_value(v["process"].clone()).map_err(|e| e.to_string())?;
                let values =
                    resource_counters::parse(v.get("status"), &selected).map_err(str::to_owned)?;
                samples
                    .entry((stage, role))
                    .or_default()
                    .push(Capture { process, values });
            }
            _ => {}
        }
    }
    let ticks = ticks.ok_or("missing clock rate")?;
    let mut windows = Vec::new();
    for ((stage, role), captures) in samples {
        let (start, end) = *bounds.get(&stage).ok_or("sample without stage")?;
        let observed_end = captures
            .iter()
            .filter_map(|c| c.process.as_ref())
            .map(|p| p.elapsed_seconds)
            .reduce(f64::max);
        let Some(last) = end.or(observed_end).filter(|last| *last > start) else {
            windows.push(json!({"stage":stage,"role":role,"unavailable":"no usable time bounds"}));
            continue;
        };
        let mut values = BTreeMap::new();
        for metric in &metrics {
            let value = match metric.kind {
                Kind::Counter => serde_json::to_value(
                    resource_counters::window(&captures, &metric.name, start, last, ticks)
                        .map_err(str::to_owned)?,
                )
                .map_err(|e| e.to_string())?,
                Kind::Gauge => {
                    let observations: Vec<u64> = captures
                        .iter()
                        .filter(|c| c.process.is_some())
                        .filter_map(|c| c.values[&metric.name])
                        .collect();
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "sample means approximate; integer extrema retained"
                    )]
                    let mean = (!observations.is_empty()).then(|| {
                        observations.iter().map(|x| *x as f64).sum::<f64>()
                            / observations.len() as f64
                    });
                    json!({"samples":observations.len(),"missing_samples":captures.len()-observations.len(),"mean":mean,"sampled_peak":observations.iter().max(),"first":captures.first().filter(|c| c.process.is_some()).and_then(|c| c.values[&metric.name]),"last":captures.last().filter(|c| c.process.is_some()).and_then(|c| c.values[&metric.name])})
                }
            };
            values.insert(&metric.name, value);
        }
        windows.push(json!({"stage":stage,"role":role,"start_seconds":start,"end_seconds":last,"partial":end.is_none(),"metrics":values}));
    }
    let process = match resource_windows::summarize(data) {
        Ok(v) => serde_json::to_value(v).map_err(|e| e.to_string())?,
        Err(e) => {
            json!({"unavailable":e,"scope":"process windows unavailable for incomplete run; raw observations retained"})
        }
    };
    Ok(json!({"control_windows":windows,"process":process}))
}

pub fn run() -> Result<()> {
    let root = env::var("P2P_VPN_ANALYSIS_ROOT").map_err(|e| e.to_string())?;
    let audit = resource_collection::audit(Path::new(&root))?;
    let published: Value = serde_json::from_str(include_str!(
        "../../docs/developer/kademlia-resource-campaign-v3.json"
    ))
    .map_err(|e| e.to_string())?;
    if provenance(audit.clone())? != provenance(published)? {
        return Err("collection differs from published audited index".to_owned());
    }
    let mut runs = Vec::new();
    for pair in audit["pairs"].as_array().ok_or("missing pairs")? {
        for run in pair["runs"].as_array().ok_or("missing runs")? {
            let path = Path::new(run["artifact_dir"].as_str().ok_or("missing artifact")?)
                .join("observations.jsonl");
            let mut data = Vec::new();
            fs::File::open(path)
                .map_err(|e| e.to_string())?
                .take(resource_protocol::OBSERVATION_BYTES + 1)
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            if data.len() as u64 > resource_protocol::OBSERVATION_BYTES {
                return Err("observation exceeds cap".to_owned());
            }
            let hash: String = Sha256::digest(&data)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if run["observations_sha256"] != hash {
                return Err("observation changed since audit".to_owned());
            }
            runs.push(json!({"repetition":pair["repetition"],"cell":pair["cell"],"profile":pair["profile"],"workload":pair["workload"],"subject":run["subject"],"status":run["status"],"reason":run["reason"],"observations_sha256":hash,"useful_work":run["integrity"]["traffic"],"stage_outcomes":run["integrity"]["stages_ended"],"measurements":summarize(&data)?}));
        }
    }
    let mut value = json!({"schema_version":2,"scope":"per-run measurements and gated paired comparisons","catalog":resource_metrics::catalog(),"runs":runs});
    value["paired"] = super::resource_pairs::summarize(&value)?;
    let output = env::var("P2P_VPN_ANALYSIS_OUTPUT").map_err(|e| e.to_string())?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(&file, &value).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

#[test]
fn provenance_ignores_derived_results_but_not_raw_hashes_or_outcomes() {
    let original = json!({"pairs":[{"runs":[{"status":"completed","observations_sha256":"abc","integrity":{"coverage":0.99},"elapsed_seconds":1.1}]}]});
    let mut derived = original.clone();
    derived["pairs"][0]["runs"][0]["integrity"] = Value::Null;
    assert_eq!(
        provenance(original.clone()).unwrap(),
        provenance(derived.clone()).unwrap()
    );
    derived["pairs"][0]["runs"][0]["observations_sha256"] = json!("different");
    assert_ne!(provenance(original).unwrap(), provenance(derived).unwrap());
}

#[test]
fn partial_control_window_has_explicit_bounds_and_no_raw_control_payloads() {
    let mut records = vec![
        json!({"kind":"metadata","clock_ticks_per_second":100}),
        json!({"kind":"stage_start","stage":{"name":"startup"},"elapsed_seconds":0.0}),
    ];
    for time in [0, 5, 10] {
        records.push(json!({"kind":"sample","stage":"startup","role":"a","status":[format!("redial_attempts {time}"),"secret ignored"],"process":{"elapsed_seconds":time,"capture_seconds":0,"pid":1,"start_ticks":1,"cpu_ticks":0,"rss_kib":1,"threads":1,"socket_fds":0,"socket_inodes":0,"vanished_fds":0,"process_tcp_states":{},"namespace_tcp_states":{}}}));
    }
    let text = records
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let result = summarize(text.as_bytes()).unwrap();
    let window = &result["control_windows"][0];
    assert_eq!(window["partial"], true);
    assert_eq!(window["end_seconds"], 10.0);
    assert_eq!(window["metrics"]["redial_attempts"]["delta"], 10);
    assert!(window["metrics"]["kad_primary_query_requests"]["delta"].is_null());
    assert!(!result.to_string().contains("secret"));
}
