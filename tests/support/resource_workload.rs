use super::*;
use protocol::{Action, Stage, Workload};

pub struct Context<'a> {
    pub nodes: [u32; 2],
    pub infrastructure: u32,
    pub configs: &'a [Config; 2],
    pub temp: &'a Path,
    pub runtime: &'a tokio::runtime::Runtime,
    pub evidence: &'a mut File,
    pub started: Instant,
}

impl Context<'_> {
    fn record(&mut self, value: &serde_json::Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        let length = self
            .evidence
            .metadata()
            .map_err(|error| error.to_string())?
            .len();
        if length + bytes.len() as u64 + 1 > protocol::OBSERVATION_BYTES {
            return Err("censored: observation limit exceeded".to_owned());
        }
        self.evidence
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        self.evidence
            .write_all(b"\n")
            .map_err(|error| error.to_string())
    }

    fn ping(&self, index: usize, count: &str) -> Output {
        let destination = TunRuntimeConfig::from_config(&self.configs[1 - index])
            .unwrap()
            .addresses
            .ipv4;
        command_output(
            "nsenter",
            &[
                "-t",
                &self.nodes[index].to_string(),
                "-n",
                "ping",
                "-n",
                "-q",
                "-c",
                count,
                "-i",
                "0.2",
                "-W",
                "1",
                "-w",
                "4",
                "-I",
                &self.configs[index].interface.name,
                &destination.to_string(),
            ],
            &[("LC_ALL", "C")],
            Duration::from_secs(5),
        )
        .expect("bounded ping")
    }

    fn boundary(&mut self, name: &str) -> Result<(), String> {
        let mut complete = true;
        for index in 0..2 {
            let output = self.ping(index, "5");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let success = output.status.success()
                && stdout
                    .lines()
                    .any(|line| line.starts_with("5 packets transmitted, 5 received,"));
            complete &= success;
            self.record(&json!({"kind": "boundary", "name": name, "node": index, "success": success, "stdout": stdout, "stderr": String::from_utf8_lossy(&output.stderr), "elapsed_seconds": self.started.elapsed().as_secs_f64()}))?;
        }
        if complete {
            Ok(())
        } else {
            Err(format!("failed: {name} boundary traffic"))
        }
    }

    fn observe(&mut self, stage: &Stage) -> Result<bool, String> {
        let mut healthy = true;
        for (index, role) in ["a", "b"].into_iter().enumerate() {
            let process = process_sample::capture(self.nodes[index], self.started);
            let socket = node_control_socket(self.temp, role);
            let state = self
                .runtime
                .block_on(query_state(&socket, Duration::from_secs(1)));
            let status = self
                .runtime
                .block_on(query_status(&socket, Duration::from_secs(1)));
            let ping = stage.probe_each_sample.then(|| self.ping(index, "1"));
            let delivered = ping.as_ref().is_some_and(|output| output.status.success());
            let path_matches = state.as_ref().is_ok_and(|lines| {
                path_matches(lines, &self.configs[index].peers[0].id, &stage.name)
            });
            healthy &= delivered && process.is_ok() && status.is_ok() && path_matches;
            self.record(&json!({
                "kind": "sample", "stage": stage.name, "role": role,
                "elapsed_seconds": self.started.elapsed().as_secs_f64(),
                "process": process.as_ref().ok(), "process_error": process.as_ref().err().map(ToString::to_string),
                "state": state.as_ref().ok(), "state_error": state.as_ref().err().map(|error| format!("{error:?}")),
                "status": status.as_ref().ok(), "status_error": status.as_ref().err().map(|error| format!("{error:?}")),
                "probe_success": ping.as_ref().map(|output| output.status.success()),
                "path_matches": path_matches,
            }))?;
        }
        let process = process_sample::capture(self.infrastructure, self.started);
        self.record(&json!({"kind": "infrastructure_sample", "stage": stage.name, "process": process.as_ref().ok(), "error": process.as_ref().err().map(ToString::to_string)}))?;
        Ok(healthy)
    }

    fn qdisc(&mut self, phase: &str) -> Result<(), String> {
        let output = ns_command_output(
            self.nodes[0],
            "tc",
            &["-s", "-j", "qdisc", "show", "dev", "veth-dir-a"],
        );
        if !output.status.success() {
            return Err("failed: qdisc snapshot".to_owned());
        }
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
        self.record(&json!({"kind": "qdisc", "phase": phase, "value": value, "elapsed_seconds": self.started.elapsed().as_secs_f64()}))
    }
}

fn start_traffic(context: &Context<'_>, workload: Workload) -> NamespaceChild {
    let traffic = workload.traffic().expect("traffic workload");
    let destination = TunRuntimeConfig::from_config(&context.configs[1])
        .unwrap()
        .addresses
        .ipv4;
    let log = File::create(context.temp.join("traffic.log")).unwrap();
    NamespaceChild {
        child: Command::new("nsenter")
            .env("LC_ALL", "C")
            .args([
                "-t",
                &context.nodes[0].to_string(),
                "-n",
                "ping",
                "-q",
                "-n",
                "-i",
                &format!("{}", 1.0 / f64::from(traffic.requests_per_second)),
                "-s",
                &traffic.payload_bytes.to_string(),
                "-c",
                &traffic.maximum_requests.to_string(),
                "-w",
                &traffic.seconds.to_string(),
                "-W",
                "1",
                "-I",
                &context.configs[0].interface.name,
                &destination.to_string(),
            ])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    }
}

fn stop_traffic(traffic: &mut Option<NamespaceChild>) {
    if let Some(mut traffic) = traffic.take() {
        if traffic.child.try_wait().unwrap().is_none() {
            run_command("kill", &["-INT", &traffic.id().to_string()]);
            let deadline = Instant::now() + Duration::from_secs(2);
            while traffic.child.try_wait().unwrap().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

pub fn run(mut context: Context<'_>, workload: Workload) -> Result<(), String> {
    let clock = Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .map_err(|error| error.to_string())?;
    assert_output_success("clock tick rate", &clock);
    let ticks: u64 = String::from_utf8(clock.stdout)
        .unwrap()
        .trim()
        .parse()
        .map_err(|error| format!("invalid CLK_TCK: {error}"))?;
    if ticks == 0 {
        return Err("invalid CLK_TCK: zero".to_owned());
    }
    context.record(&json!({"kind": "metadata", "clock_ticks_per_second": ticks, "kernel": command_metadata("uname", &["-a"]), "loadavg": fs::read_to_string("/proc/loadavg").ok(), "effective_configs": context.configs, "workload": workload, "protocol_version": protocol::VERSION}))?;
    let mut traffic = None;
    let mut failure = None;
    for (stage_index, stage) in workload.stages().into_iter().enumerate() {
        if stage_index == 2 {
            if let Err(error) = context.boundary("before_workload") {
                failure = Some(error);
            }
        }
        match stage.action {
            Action::None => {}
            Action::StartTraffic => traffic = Some(start_traffic(&context, workload)),
            Action::StopTraffic => stop_traffic(&mut traffic),
            Action::DisconnectLanAndInfrastructure => {
                set_network_move_direct_link(context.nodes[0], context.nodes[1], false);
                run_command("ip", &["link", "set", "veth-mv-host", "down"]);
            }
            Action::RestoreInfrastructure => {
                run_command("ip", &["link", "set", "veth-mv-host", "up"])
            }
            Action::RenumberAndRestoreLan => {
                for (index, interface) in ["veth-dir-a", "veth-dir-b"].into_iter().enumerate() {
                    let suffix = index + 1;
                    ns_command(
                        context.nodes[index],
                        "ip",
                        &[
                            "addr",
                            "del",
                            &format!("10.253.0.{suffix}/24"),
                            "dev",
                            interface,
                        ],
                    );
                    ns_command(
                        context.nodes[index],
                        "ip",
                        &[
                            "addr",
                            "add",
                            &format!("10.253.1.{suffix}/24"),
                            "dev",
                            interface,
                        ],
                    );
                }
                set_network_move_direct_link(context.nodes[0], context.nodes[1], true);
            }
            Action::ShapeAndStartTraffic => {
                context.qdisc("before")?;
                ns_command(
                    context.nodes[0],
                    "tc",
                    &[
                        "qdisc",
                        "add",
                        "dev",
                        "veth-dir-a",
                        "root",
                        "netem",
                        "delay",
                        "50ms",
                        "rate",
                        "64kbit",
                        "limit",
                        "16",
                    ],
                );
                traffic = Some(start_traffic(&context, workload));
            }
            Action::StopTrafficAndRelease => {
                context.qdisc("after_load")?;
                stop_traffic(&mut traffic);
                ns_command(
                    context.nodes[0],
                    "tc",
                    &["qdisc", "del", "dev", "veth-dir-a", "root"],
                );
                context.qdisc("released")?;
            }
        }
        let began = if stage_index == 0 {
            context.started
        } else {
            Instant::now()
        };
        let duration = Duration::from_secs(stage.seconds);
        context.record(&json!({"kind": "stage_start", "stage": stage, "elapsed_seconds": context.started.elapsed().as_secs_f64()}))?;
        let mut slot = 0_u64;
        let mut consecutive = 0;
        let mut recovered = false;
        let mut confirmed_recovery_seconds = None;
        let mut first_success = None;
        loop {
            let healthy = context.observe(&stage)?;
            if healthy {
                consecutive += 1;
                first_success.get_or_insert(began.elapsed().as_secs_f64());
                recovered |= consecutive >= 5;
                if recovered {
                    confirmed_recovery_seconds.get_or_insert(began.elapsed().as_secs_f64());
                }
            } else {
                consecutive = 0;
            }
            if stage.name == "pressure" && slot == 1 {
                context.qdisc("during")?;
            }
            let next_slot = next_slot(slot, began.elapsed());
            if next_slot > slot + 1 {
                context.record(&json!({"kind": "sampling_gap", "stage": stage.name, "skipped_slots": next_slot - slot - 1, "elapsed_seconds": context.started.elapsed().as_secs_f64()}))?;
            }
            slot = next_slot;
            let next = Duration::from_secs(slot * protocol::SAMPLE_SECONDS);
            if next > duration {
                break;
            }
            if began.elapsed() < next {
                thread::sleep(next - began.elapsed());
            }
        }
        if matches!(
            stage.name.as_str(),
            "startup" | "relay_recovery" | "direct_recovery"
        ) && !recovered
        {
            failure = Some(format!(
                "censored: {} did not reach five consecutive bidirectional successes",
                stage.name
            ));
        }
        context.record(&json!({"kind": "stage_end", "stage": stage.name, "elapsed_seconds": context.started.elapsed().as_secs_f64(), "first_success_seconds": first_success, "confirmed_recovery_seconds": confirmed_recovery_seconds, "five_consecutive_successes": stage.probe_each_sample.then_some(recovered)}))?;
    }
    stop_traffic(&mut traffic);
    if let Some(offered) = workload.traffic() {
        let output = fs::read_to_string(context.temp.join("traffic.log"))
            .map_err(|error| error.to_string())?;
        let counts =
            ping_counts(&output).ok_or_else(|| "failed: missing traffic summary".to_owned())?;
        context.record(&json!({"kind": "traffic_summary", "sent": counts.0, "received": counts.1, "payload_bytes": offered.payload_bytes, "offered": offered, "output": output}))?;
        if counts.0 == 0
            || counts.0 > u64::from(offered.maximum_requests)
            || counts.1 == 0
            || counts.1 > counts.0
        {
            failure = Some("failed: invalid or undelivered offered traffic".to_owned());
        }
    }
    if let Err(error) = context.boundary("after_workload") {
        failure = Some(error);
    }
    failure.map_or(Ok(()), Err)
}

fn next_slot(previous: u64, elapsed: Duration) -> u64 {
    (previous + 1).max(elapsed.as_secs() / protocol::SAMPLE_SECONDS + 1)
}

fn path_matches(lines: &[String], peer: &str, stage: &str) -> bool {
    lines
        .iter()
        .filter(|line| line.starts_with("peer state: "))
        .any(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            let field = |name| {
                fields
                    .windows(2)
                    .find_map(|pair| (pair[0] == name).then_some(pair[1]))
            };
            if field("transport") != Some(peer) || field("validated") != Some("true") {
                return false;
            }
            match stage {
                "relay_recovery" => field("selected_path") == Some("circuit_relay"),
                "direct_recovery" | "post_recovery" => {
                    field("selected_path").is_some_and(|path| path.starts_with("direct_"))
                }
                _ => true,
            }
        })
}

fn ping_counts(output: &str) -> Option<(u64, u64)> {
    output.lines().find_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.get(1..3) != Some(&["packets", "transmitted,"][..])
            || fields.get(4) != Some(&"received,")
        {
            return None;
        }
        Some((fields[0].parse().ok()?, fields[3].parse().ok()?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_delays_skip_slots_instead_of_bursting() {
        assert_eq!(next_slot(0, Duration::from_millis(100)), 1);
        assert_eq!(next_slot(0, Duration::from_secs(12)), 3);
        assert_eq!(next_slot(2, Duration::from_secs(15)), 4);
        assert_eq!(next_slot(5, Duration::from_secs(26)), 6);
    }

    #[test]
    fn recovery_paths_require_the_expected_validated_peer() {
        let state = |peer, path, valid| {
            vec![format!(
                "peer state: overlay transport {peer} validated {valid} selected_path {path}"
            )]
        };
        assert!(path_matches(
            &state("remote", "circuit_relay", "true"),
            "remote",
            "relay_recovery"
        ));
        assert!(!path_matches(
            &state("infra", "circuit_relay", "true"),
            "remote",
            "relay_recovery"
        ));
        assert!(!path_matches(
            &state("remote", "circuit_relay", "false"),
            "remote",
            "relay_recovery"
        ));
        assert!(!path_matches(
            &state("remote", "direct_tcp_stream", "true"),
            "remote",
            "relay_recovery"
        ));
        assert!(path_matches(
            &state("remote", "direct_udp_datagram", "true"),
            "remote",
            "direct_recovery"
        ));
    }

    #[test]
    fn ping_summary_retains_loss_and_rejects_missing_data() {
        assert_eq!(
            ping_counts(
                "9000 packets transmitted, 8997 received, 0.03% packet loss, time 180000ms"
            ),
            Some((9000, 8997))
        );
        assert_eq!(
            ping_counts("12 packets transmitted, 0 received, +12 errors, 100% packet loss"),
            Some((12, 0))
        );
        assert_eq!(ping_counts("ping: interface not found"), None);
        assert_eq!(
            ping_counts("not-a-number packets transmitted, 0 received,"),
            None
        );
    }
}
