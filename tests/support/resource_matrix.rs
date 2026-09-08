use super::resource_protocol::{self as protocol, Pair, Profile, Subject, Workload};
use p2p_vpn::identity::NodeIdentity;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    env, fs,
    io::{Read as _, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;
const CONTROLLER_LOG_BYTES: u64 = 32 * 1024 * 1024;

fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn required(name: &str) -> Result<String> {
    env::var(name).map_err(|_| format!("{name} is required"))
}

fn pair_limit(value: Option<&str>) -> Result<usize> {
    match value {
        None => Ok(usize::MAX),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|limit| *limit > 0)
            .ok_or_else(|| "P2P_VPN_MATRIX_MAX_PAIRS must be a positive integer".to_owned()),
    }
}

fn hash(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(error)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let size = file.read(&mut buffer).map_err(error)?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn read_json(path: &Path) -> Result<Value> {
    if fs::metadata(path).map_err(error)?.len() > 1024 * 1024 {
        return Err("manifest exceeds one MiB".to_owned());
    }
    serde_json::from_slice(&fs::read(path).map_err(error)?).map_err(error)
}

fn create_json(path: &Path, value: &Value) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(error)?;
    serde_json::to_writer_pretty(&mut file, value).map_err(error)?;
    file.write_all(b"\n").map_err(error)?;
    file.sync_all().map_err(error)
}

fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(error)
}

fn task_bytes() -> Result<u64> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(env::temp_dir()).map_err(error)? {
        let entry = entry.map_err(error)?;
        if entry.file_name().to_string_lossy().starts_with("p2p-vpn-") {
            paths.push(entry.path());
        }
    }
    if paths.is_empty() {
        return Ok(0);
    }
    let mut output = Command::new("du")
        .args(["-sk", "--"])
        .args(&paths)
        .output()
        .map_err(error)?;
    if !output.status.success() {
        output = Command::new("sudo")
            .args(["-n", "du", "-sk", "--"])
            .args(&paths)
            .output()
            .map_err(error)?;
    }
    if !output.status.success() {
        return Err("unable to account for task storage".to_owned());
    }
    let text = String::from_utf8(output.stdout).map_err(error)?;
    text.lines().try_fold(0_u64, |sum, line| {
        let kib: u64 = line
            .split_once('\t')
            .ok_or("invalid du output")?
            .0
            .parse()
            .map_err(error)?;
        sum.checked_add(kib.checked_mul(1024).ok_or("storage size overflow")?)
            .ok_or_else(|| "storage size overflow".to_owned())
    })
}

fn verify_subjects(builds: &Value) -> Result<()> {
    for name in ["baseline", "current"] {
        let subject = &builds["subjects"][name];
        let binary = subject["binary"].as_str().ok_or("missing subject binary")?;
        if hash(Path::new(binary))? != subject["sha256"].as_str().ok_or("missing subject hash")? {
            return Err(format!("{name} executable differs from pinned build"));
        }
    }
    Ok(())
}

fn pair_plan(smoke: bool) -> Vec<Pair> {
    protocol::matrix()
        .into_iter()
        .filter(|pair| !smoke || (pair.repetition == 1 && pair.workload == Workload::Idle))
        .collect()
}

fn outcome(result: Option<&Value>, exit_success: bool) -> &'static str {
    match result {
        Some(result) if result == &json!({"Ok": null}) && exit_success => "completed",
        Some(result) => match result.get("Err").and_then(Value::as_str) {
            Some(reason) if reason.starts_with("censored:") => "censored",
            Some(reason) if reason.starts_with("failed:") => "failed",
            _ => "harness_error",
        },
        None => "harness_error",
    }
}

fn validate_worker(raw: &Value, pair: &Pair, subject_hash: &str, harness_hash: &str) -> Result<()> {
    if raw["protocol_version"] != protocol::VERSION
        || raw["acceptance_measurement"] != true
        || raw["profile"] != json!(pair.profile)
        || raw["workload"] != json!(pair.workload)
        || raw["stages"] != json!(pair.workload.stages())
        || raw["subject_sha256"] != subject_hash
        || raw["harness_sha256"] != harness_hash
    {
        return Err("worker provenance differs from frozen inputs".to_owned());
    }
    Ok(())
}

struct IsolatedChild(Child);
impl Drop for IsolatedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn execute(
    harness: &Path,
    subject: &Path,
    keys: &Path,
    directory: &Path,
    pair: &Pair,
    smoke: bool,
) -> Result<Value> {
    let log = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join("controller.log"))
        .map_err(error)?;
    // The outer namespace ensures killing this child reaps descendants even if
    // the inner fixture creates a separate process group for its watchdog.
    let mut command = Command::new("prlimit");
    command
        .args(["--fsize=33554432:33554432", "--", "unshare"])
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--net",
            "--pid",
            "--fork",
            "--kill-child=SIGKILL",
            "--mount-proc",
            "--",
        ])
        .arg(harness)
        .args([
            "--ignored",
            "--exact",
            if smoke {
                "tun_namespace_resource_cli_smoke"
            } else {
                "tun_namespace_resource_workload"
            },
            "--nocapture",
        ])
        .env_remove("P2P_VPN_TUN_E2E_MODE")
        .env_remove("P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS")
        .env_remove("P2P_VPN_TUN_E2E_WAIT_SCALE")
        .env_remove("RUST_LOG")
        .env("P2P_VPN_RESOURCE_SUBJECT", subject)
        .env("P2P_VPN_RESOURCE_KEYS", keys)
        .env("P2P_VPN_RESOURCE_POINTER", directory.join("location.json"))
        .env("P2P_VPN_RESOURCE_ACCEPTANCE", if smoke { "0" } else { "1" })
        .env(
            "P2P_VPN_TUN_E2E_RECOVERY_PROFILE",
            match pair.profile {
                Profile::Public => "public",
                Profile::Private => "private",
            },
        )
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(error)?)
        .stderr(log);
    if smoke {
        command.env_remove("P2P_VPN_RESOURCE_WORKLOAD");
    } else {
        command.env(
            "P2P_VPN_RESOURCE_WORKLOAD",
            serde_json::to_value(pair.workload)
                .map_err(error)?
                .as_str()
                .unwrap(),
        );
    }
    let began = Instant::now();
    let mut child = IsolatedChild(command.spawn().map_err(error)?);
    let deadline = Duration::from_secs(if smoke {
        240
    } else {
        protocol::WATCHDOG_SECONDS + 60
    });
    let mut timed_out = false;
    let exit = loop {
        if let Some(status) = child.0.try_wait().map_err(error)? {
            break Some(status);
        }
        if began.elapsed() >= deadline {
            timed_out = true;
            child.0.kill().map_err(error)?;
            child.0.wait().map_err(error)?;
            break None;
        }
        thread::sleep(Duration::from_millis(100));
    };
    let location = read_json(&directory.join("location.json")).ok();
    let artifact = location
        .as_ref()
        .and_then(|value| value["artifact_dir"].as_str())
        .map(PathBuf::from);
    let raw = artifact.as_ref().and_then(|path| {
        read_json(&path.join(if smoke { "smoke.json" } else { "workload.json" })).ok()
    });
    let exit_success = exit.is_some_and(|status| status.success());
    let status = if timed_out {
        "censored"
    } else if smoke
        && exit_success
        && raw
            .as_ref()
            .is_some_and(|value| value["outcome"] == "completed")
    {
        "completed"
    } else {
        outcome(
            raw.as_ref().and_then(|value| value.get("result")),
            exit_success,
        )
    };
    let mut configs = json!({});
    if let Some(path) = &artifact {
        for role in ["a", "b"] {
            let config = path.join(format!("config-{role}.json"));
            if config.exists() {
                configs[role] = json!(hash(&config)?);
            }
        }
    }
    if !smoke {
        if let Some(raw) = &raw {
            validate_worker(raw, pair, &hash(subject)?, &hash(harness)?)?;
        }
        if status == "completed" && (configs["a"].is_null() || configs["b"].is_null()) {
            return Err("completed worker is missing endpoint configurations".to_owned());
        }
    }
    Ok(
        json!({"status": status, "timed_out": timed_out, "exit_code": exit.and_then(|status| status.code()), "elapsed_seconds": began.elapsed().as_secs_f64(), "artifact_dir": artifact, "configuration_sha256": configs, "worker_result": raw}),
    )
}

pub fn run() -> Result<()> {
    let limit = pair_limit(env::var("P2P_VPN_MATRIX_MAX_PAIRS").ok().as_deref())?;
    let root = PathBuf::from(required("P2P_VPN_MATRIX_ROOT")?);
    if root.parent() != Some(env::temp_dir().as_path())
        || !root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("p2p-vpn-"))
    {
        return Err(
            "matrix root must be a direct p2p-vpn-* child of the temporary directory".to_owned(),
        );
    }
    let smoke = match required("P2P_VPN_MATRIX_MODE")?.as_str() {
        "smoke" => true,
        "full" => false,
        _ => return Err("matrix mode must be smoke or full".to_owned()),
    };
    let builds = read_json(Path::new(&required("P2P_VPN_MATRIX_BUILDS")?))?;
    verify_subjects(&builds)?;
    let harness_source = fs::canonicalize(required("P2P_VPN_MATRIX_HARNESS")?).map_err(error)?;
    let harness_hash = hash(&harness_source)?;
    let plan = pair_plan(smoke);
    let resume = env::var("P2P_VPN_MATRIX_RESUME").as_deref() == Ok("1");
    if !resume {
        let copy_bytes = fs::metadata(&harness_source).map_err(error)?.len()
            + fs::metadata(env::current_exe().map_err(error)?)
                .map_err(error)?
                .len()
            + ["baseline", "current"]
                .into_iter()
                .map(|name| {
                    fs::metadata(builds["subjects"][name]["binary"].as_str().unwrap())
                        .map(|metadata| metadata.len())
                        .map_err(error)
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .sum::<u64>();
        if task_bytes()?
            + copy_bytes
            + protocol::RUN_ALLOWANCE_BYTES
            + CONTROLLER_LOG_BYTES
            + 1024 * 1024
            >= 39 * 1024 * 1024 * 1024 / 4
        {
            return Err("task storage plus campaign copies reaches 9.75 GiB".to_owned());
        }
        private_dir(&root)?;
        fs::copy(&harness_source, root.join("harness")).map_err(error)?;
        fs::copy(env::current_exe().map_err(error)?, root.join("controller")).map_err(error)?;
        create_json(&root.join("builds.json"), &builds)?;
        for name in ["baseline", "current"] {
            fs::copy(
                builds["subjects"][name]["binary"].as_str().unwrap(),
                root.join(name),
            )
            .map_err(error)?;
        }
        create_json(
            &root.join("matrix.json"),
            &json!({"protocol_version": protocol::VERSION, "acceptance_measurement": !smoke, "smoke": smoke, "builds": builds, "harness_sha256": harness_hash, "controller_sha256": hash(&env::current_exe().map_err(error)?)?, "plan": plan}),
        )?;
        for pair in &plan {
            let directory = root.join(format!("r{}-c{}", pair.repetition, pair.cell));
            private_dir(&directory)?;
            let keys: [String; 3] =
                std::array::from_fn(|_| NodeIdentity::generate_ed25519().unwrap().private_key);
            create_json(&directory.join("keys.json"), &json!(keys))?;
        }
    }
    let manifest = read_json(&root.join("matrix.json"))?;
    if manifest["protocol_version"] != protocol::VERSION
        || manifest["smoke"] != smoke
        || manifest["builds"] != builds
        || manifest["harness_sha256"] != harness_hash
        || manifest["controller_sha256"] != hash(&env::current_exe().map_err(error)?)?
        || manifest["plan"] != json!(plan)
        || hash(&root.join("harness"))? != harness_hash
    {
        return Err("resume inputs differ from the frozen matrix".to_owned());
    }
    let pair_count = plan.len();
    let mut executed_pairs = 0;
    for (pair_index, pair) in plan.into_iter().enumerate() {
        let mut executed = false;
        let directory = root.join(format!("r{}-c{}", pair.repetition, pair.cell));
        let keys = directory.join("keys.json");
        let mut previous_config = None;
        for subject in pair.subjects {
            let name = match subject {
                Subject::Baseline => "baseline",
                Subject::Current => "current",
            };
            let run_directory = directory.join(name);
            if hash(&root.join(name))? != builds["subjects"][name]["sha256"] {
                return Err("campaign subject differs from pinned build".to_owned());
            }
            let result_path = run_directory.join("result.json");
            let result = if result_path.exists() {
                let start = read_json(&run_directory.join("start.json"))?;
                if start["keys_sha256"] != hash(&keys)?
                    || start["pair"] != json!(pair)
                    || start["subject"] != json!(subject)
                {
                    return Err("saved run inputs differ from the frozen pair".to_owned());
                }
                read_json(&result_path)?
            } else {
                if run_directory.exists() {
                    return Err(format!(
                        "unfinished run marker at {}; inspect live processes and evidence before any retry",
                        run_directory.display()
                    ));
                }
                let bytes = task_bytes()?;
                if bytes + protocol::RUN_ALLOWANCE_BYTES + CONTROLLER_LOG_BYTES + 1024 * 1024
                    >= 39 * 1024 * 1024 * 1024 / 4
                {
                    return Err("task storage plus run allowance reaches 9.75 GiB".to_owned());
                }
                verify_subjects(&builds)?;
                executed = true;
                private_dir(&run_directory)?;
                create_json(
                    &run_directory.join("start.json"),
                    &json!({"pair": pair, "subject": subject, "keys_sha256": hash(&keys)?, "task_bytes_before": bytes, "unix_seconds": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(error)?.as_secs()}),
                )?;
                eprintln!(
                    "matrix r{} c{} {name}: starting",
                    pair.repetition, pair.cell
                );
                let result = execute(
                    &root.join("harness"),
                    &root.join(name),
                    &keys,
                    &run_directory,
                    &pair,
                    smoke,
                )?;
                create_json(&result_path, &result)?;
                eprintln!(
                    "matrix r{} c{} {name}: {}",
                    pair.repetition, pair.cell, result["status"]
                );
                result
            };
            if result["status"] == "harness_error" {
                return Err(format!(
                    "harness failure retained at {}",
                    result_path.display()
                ));
            }
            if !smoke {
                if let Some(previous) = &previous_config {
                    if previous != &result["configuration_sha256"] {
                        return Err("paired configuration bytes differ".to_owned());
                    }
                }
                previous_config = Some(result["configuration_sha256"].clone());
            }
        }
        if executed {
            executed_pairs += 1;
        }
        if executed_pairs >= limit && pair_index + 1 < pair_count {
            eprintln!(
                "matrix paused after {executed_pairs} new pairs; resume with the pinned controller"
            );
            return Ok(());
        }
    }
    if !root.join("complete.json").exists() {
        create_json(
            &root.join("complete.json"),
            &json!({"protocol_version": protocol::VERSION, "runs": pair_plan(smoke).len() * 2, "acceptance_measurement": !smoke}),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pair_limits_pause_execution_without_changing_the_plan() {
        assert_eq!(pair_limit(None).unwrap(), usize::MAX);
        assert_eq!(pair_limit(Some("1")).unwrap(), 1);
        for value in ["", "0", "-1", "two"] {
            assert!(pair_limit(Some(value)).is_err());
        }
        assert_eq!(pair_plan(false).len(), 24);
    }
    #[test]
    fn worker_provenance_must_match_every_frozen_input() {
        let pair = &pair_plan(false)[0];
        let raw = json!({
            "protocol_version": protocol::VERSION,
            "acceptance_measurement": true,
            "profile": pair.profile,
            "workload": pair.workload,
            "stages": pair.workload.stages(),
            "subject_sha256": "subject",
            "harness_sha256": "harness",
        });
        assert!(validate_worker(&raw, pair, "subject", "harness").is_ok());
        for field in raw.as_object().unwrap().keys() {
            let mut changed = raw.clone();
            changed[field] = Value::Null;
            assert!(
                validate_worker(&changed, pair, "subject", "harness").is_err(),
                "{field}"
            );
        }
    }
    #[test]
    fn smoke_is_separate_and_full_plan_has_48_runs() {
        assert_eq!(pair_plan(true).len(), 2);
        assert_eq!(pair_plan(false).len(), 24);
        assert!(
            pair_plan(true)
                .iter()
                .all(|pair| pair.workload == Workload::Idle && pair.repetition == 1)
        );
    }
    #[test]
    fn failed_and_censored_results_are_not_successful_measurements() {
        assert_eq!(outcome(Some(&json!({"Ok": null})), true), "completed");
        assert_eq!(outcome(Some(&json!({"Ok": null})), false), "harness_error");
        assert_eq!(
            outcome(Some(&json!({"Err": "failed: delivery"})), false),
            "failed"
        );
        assert_eq!(
            outcome(Some(&json!({"Err": "censored: recovery"})), false),
            "censored"
        );
        assert_eq!(outcome(None, false), "harness_error");
        assert_eq!(outcome(Some(&json!({"Ok": false})), true), "harness_error");
        assert_eq!(
            outcome(Some(&json!({"Err": "parse failure"})), false),
            "harness_error"
        );
    }
}
