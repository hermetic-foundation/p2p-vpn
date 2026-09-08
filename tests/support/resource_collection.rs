use super::{resource_protocol as protocol, resource_windows};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet, env, fs, io::Read as _, os::unix::fs::OpenOptionsExt as _, path::Path,
};

type Result<T> = std::result::Result<T, String>;

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    require(bytes.len() as u64 <= limit, "artifact exceeds size bound")?;
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn read(path: &Path) -> Result<Value> {
    serde_json::from_slice(&bytes(path, 1024 * 1024)?).map_err(|e| e.to_string())
}

fn observations(data: &[u8], workload: protocol::Workload, completed: bool) -> Result<Value> {
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut missing = json!({"process":0,"state":0,"status":0});
    let mut samples = 0_u64;
    let mut gaps = 0_u64;
    let mut boundaries = Vec::new();
    let mut traffic = Value::Null;
    let mut metadata = 0;
    for line in data.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let value: Value = serde_json::from_slice(line).map_err(|e| e.to_string())?;
        match value["kind"].as_str().ok_or("missing observation kind")? {
            "metadata" => {
                metadata += 1;
                require(value["protocol_version"] == 3 && value["workload"] == json!(workload), "observation metadata mismatch")?;
            }
            "stage_start" => starts.push(value["stage"].clone()),
            "stage_end" => ends.push(json!({"stage":value["stage"], "elapsed_seconds":value["elapsed_seconds"], "first_success_seconds":value["first_success_seconds"], "confirmed_recovery_seconds":value["confirmed_recovery_seconds"], "five_consecutive_successes":value["five_consecutive_successes"]})),
            "sample" => {
                samples += 1;
                for field in ["process", "state", "status"] {
                    if value[field].is_null() { missing[field] = json!(missing[field].as_u64().unwrap() + 1); }
                }
            }
            "sampling_gap" => gaps += 1,
            "boundary" => boundaries.push(json!({"name":value["name"],"node":value["node"],"success":value["success"]})),
            "traffic_summary" => {
                require(traffic.is_null(), "duplicate traffic summary")?;
                traffic = json!({"sent":value["sent"],"received":value["received"],"payload_bytes":value["payload_bytes"],"pacing":value["pacing"]});
            }
            _ => {}
        }
    }
    require(metadata == 1, "expected one observation metadata record")?;
    let expected: Vec<Value> = workload.stages().into_iter().map(|s| json!(s)).collect();
    require(
        starts.len() <= expected.len() && starts == expected[..starts.len()],
        "unexpected stage sequence",
    )?;
    require(ends.len() <= starts.len(), "stage end without start")?;
    for (i, end) in ends.iter().enumerate() {
        require(
            end["stage"] == starts[i]["name"],
            "stage-end sequence mismatch",
        )?;
    }
    if completed {
        require(
            starts == expected && ends.len() == starts.len(),
            "completed run has incomplete stages",
        )?;
        require(
            boundaries.len() == 4 && boundaries.iter().all(|b| b["success"] == true),
            "completed run lacks successful boundaries",
        )?;
        if let Some(offered) = workload.traffic() {
            let sent = traffic["sent"].as_u64().ok_or("missing offered count")?;
            let received = traffic["received"]
                .as_u64()
                .ok_or("missing received count")?;
            require(
                offered.offered_count_valid(sent) && received > 0 && received <= sent,
                "invalid completed traffic counts",
            )?;
        }
    }
    let process_check = match resource_windows::summarize(data) {
        Ok(summary) => {
            let summary = serde_json::to_value(summary).map_err(|e| e.to_string())?;
            json!({"parsed":true,"windows":summary["windows"].as_array().ok_or("missing windows")?.iter().map(|w| json!({"stage":w["stage"],"role":w["role"],"sample_count":w["sample_count"],"temporal_coverage":w["temporal_coverage"],"invalid_intervals":w["invalid_intervals"],"cpu_unavailable_reasons":w["cpu_unavailable_reasons"]})).collect::<Vec<_>>()})
        }
        Err(error) if !completed => json!({"parsed":false,"reason":error}),
        Err(error) => return Err(format!("completed run observation validation: {error}")),
    };
    Ok(
        json!({"endpoint_samples":samples,"missing_endpoint_captures":missing,"sampling_gaps":gaps,"stages_started":starts,"stages_ended":ends,"boundary_checks":boundaries,"traffic":traffic,"process_validation":process_check}),
    )
}

pub fn audit(root: &Path) -> Result<Value> {
    let manifest = read(&root.join("matrix.json"))?;
    let plan = protocol::matrix();
    require(
        manifest["protocol_version"] == 3
            && manifest["acceptance_measurement"] == true
            && manifest["plan"] == json!(plan),
        "frozen plan mismatch",
    )?;
    let builds: Value = serde_json::from_str(include_str!(
        "../../docs/developer/kademlia-resource-builds.json"
    ))
    .map_err(|e| e.to_string())?;
    require(
        manifest["builds"] == builds && read(&root.join("builds.json"))? == builds,
        "build provenance mismatch",
    )?;
    for name in ["harness", "controller"] {
        require(
            json!(hash(&root.join(name))?) == manifest[format!("{name}_sha256")],
            "campaign executable hash mismatch",
        )?;
    }
    for name in ["baseline", "current"] {
        require(
            json!(hash(&root.join(name))?) == builds["subjects"][name]["sha256"],
            "subject binary mismatch",
        )?;
    }
    require(
        read(&root.join("complete.json"))?
            == json!({"protocol_version":3,"runs":48,"acceptance_measurement":true}),
        "missing or mismatched completion marker",
    )?;
    let mut artifacts = BTreeSet::new();
    let mut pairs = Vec::new();
    let mut completed = 0;
    let mut censored = 0;
    let mut failed = 0;
    for pair in plan {
        let directory = root.join(format!("r{}-c{}", pair.repetition, pair.cell));
        let keys_hash = hash(&directory.join("keys.json"))?;
        let mut runs = Vec::new();
        let mut prior_config = None;
        for subject in pair.subjects {
            let name = serde_json::to_value(subject).unwrap();
            let name = name.as_str().unwrap();
            let run_dir = directory.join(name);
            let start = read(&run_dir.join("start.json"))?;
            require(
                start["pair"] == json!(pair)
                    && start["subject"] == json!(subject)
                    && start["keys_sha256"] == keys_hash,
                "run identity provenance mismatch",
            )?;
            let result = read(&run_dir.join("result.json"))?;
            let artifact = result["artifact_dir"]
                .as_str()
                .ok_or("missing artifact directory")?;
            require(
                artifacts.insert(artifact.to_owned()),
                "reused artifact directory",
            )?;
            let path = Path::new(artifact);
            let worker = read(&path.join("workload.json"))?;
            require(
                worker == result["worker_result"],
                "worker result differs from saved result",
            )?;
            require(
                worker["protocol_version"] == 3
                    && worker["acceptance_measurement"] == true
                    && worker["profile"] == json!(pair.profile)
                    && worker["workload"] == json!(pair.workload)
                    && worker["stages"] == json!(pair.workload.stages())
                    && worker["subject_sha256"] == builds["subjects"][name]["sha256"]
                    && worker["harness_sha256"] == manifest["harness_sha256"],
                "worker provenance mismatch",
            )?;
            let config = json!({"a":hash(&path.join("config-a.json"))?,"b":hash(&path.join("config-b.json"))?});
            require(
                config == result["configuration_sha256"],
                "configuration hash mismatch",
            )?;
            if let Some(prior) = prior_config {
                require(config == prior, "paired configuration mismatch")?;
            }
            prior_config = Some(config.clone());
            let status = result["status"].as_str().ok_or("missing outcome")?;
            match status {
                "completed" => {
                    require(
                        worker["result"] == json!({"Ok":null}) && result["exit_code"] == 0,
                        "inconsistent completed outcome",
                    )?;
                    completed += 1;
                }
                "censored" => {
                    require(
                        worker["result"]["Err"]
                            .as_str()
                            .is_some_and(|s| s.starts_with("censored:")),
                        "censoring reason missing",
                    )?;
                    censored += 1;
                }
                "failed" => {
                    require(
                        worker["result"]["Err"]
                            .as_str()
                            .is_some_and(|s| s.starts_with("failed:")),
                        "failure reason missing",
                    )?;
                    failed += 1;
                }
                _ => return Err("unexpected harness outcome".to_owned()),
            }
            let data = bytes(
                &path.join("observations.jsonl"),
                protocol::OBSERVATION_BYTES,
            )?;
            let integrity = observations(&data, pair.workload, status == "completed")?;
            runs.push(json!({"subject":subject,"status":status,"reason":worker["result"]["Err"],"elapsed_seconds":result["elapsed_seconds"],"artifact_dir":artifact,"observations_sha256":digest(&data),"observation_bytes":data.len(),"configuration_sha256":config,"keys_sha256":keys_hash,"integrity":integrity}));
        }
        pairs.push(json!({"repetition":pair.repetition,"cell":pair.cell,"profile":pair.profile,"workload":pair.workload,"configuration_bytes_equal":true,"runs":runs}));
    }
    Ok(
        json!({"status":"collected","protocol_version":3,"expected_runs":48,"recorded_runs":artifacts.len(),"completed_runs":completed,"censored_runs":censored,"failed_runs":failed,"root":root,"harness_sha256":manifest["harness_sha256"],"controller_sha256":manifest["controller_sha256"],"matrix_manifest_sha256":hash(&root.join("matrix.json"))?,"pairs":pairs,"scope":"Collection integrity only; censored runs retained; comparative analysis and final report deferred"}),
    )
}

pub fn run() -> Result<()> {
    let root = env::var("P2P_VPN_COLLECTION_ROOT").map_err(|e| e.to_string())?;
    let result = audit(Path::new(&root))?;
    let output = env::var("P2P_VPN_COLLECTION_OUTPUT").map_err(|e| e.to_string())?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&file, &result).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

#[test]
fn observation_audit_rejects_missing_metadata_and_wrong_workload() {
    assert!(observations(b"", protocol::Workload::Idle, false).is_err());
    assert!(
        observations(
            br#"{"kind":"metadata","protocol_version":3,"workload":"traffic"}"#,
            protocol::Workload::Idle,
            false
        )
        .is_err()
    );
}

#[test]
fn partial_capture_is_retained_but_not_completed() {
    let data = br#"{"kind":"metadata","protocol_version":3,"workload":"idle"}
{"kind":"sample","process":null,"state":null,"status":null}
"#;
    let result = observations(data, protocol::Workload::Idle, false).unwrap();
    assert_eq!(result["missing_endpoint_captures"]["process"], 1);
    assert_eq!(result["process_validation"]["parsed"], false);
    assert!(observations(data, protocol::Workload::Idle, true).is_err());
    assert!(!result.to_string().contains("private_key"));
}

#[test]
fn malformed_duplicate_metadata_and_out_of_order_stages_are_rejected() {
    let metadata = json!({"kind":"metadata","protocol_version":3,"workload":"idle"});
    let stages = protocol::Workload::Idle.stages();
    for records in [
        vec![metadata.clone(), metadata.clone()],
        vec![
            metadata.clone(),
            json!({"kind":"stage_start","stage":stages[1]}),
        ],
        vec![
            metadata.clone(),
            json!({"kind":"stage_end","stage":"startup"}),
        ],
    ] {
        let data = records
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(observations(data.as_bytes(), protocol::Workload::Idle, false).is_err());
    }
    assert!(observations(b"{broken", protocol::Workload::Idle, false).is_err());
}
