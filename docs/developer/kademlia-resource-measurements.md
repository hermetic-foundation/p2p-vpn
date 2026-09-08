# Before/After Resource Measurements

## Status

Phase 3 is active. Two of 48 acceptance runs have been collected.
Subject selection, sampling, CLI smoke, and timed workload preflight are implemented.
The numeric protocol below is frozen as version 2. The matrix controller has
passed a four-run smoke campaign and its first timed pair. Aggregation and
the remaining 46 acceptance runs are still outstanding.

See the [workstream plan](kademlia-resource-plan.md).
Phases 1 and 2 remain complete; this phase does not establish production readiness.

### First Acceptance Pair

The [partial campaign index](kademlia-resource-campaign.json) records executable
and observation hashes. Both subjects completed repetition 1 of public-profile
idle using identical endpoint configuration bytes. This is not the final report.

| Check | Baseline | Current |
| --- | --- | --- |
| Timed run including teardown | 529.53 seconds | 529.55 seconds |
| Idle samples per endpoint | 61 | 61 |
| Idle span per endpoint | 300.00 seconds | 300.00 seconds |
| Largest idle sampling gap | 5.004 seconds | 5.003 seconds |
| Missing idle observations | 0 | 0 |
| Startup observations missing control | 1 | 1 |
| Bidirectional boundary checks passed | 4/4 | 4/4 |
| Idle process replacements / CPU counter resets | 0 / 0 | 0 / 0 |

- Each boundary check requires five transmitted and five received echo requests.
- The unavailable startup control observations remain in the raw data; they are not zero metrics.
- The controller paused after this pair. Resume the existing campaign, not a new set of identities.
- Project temporary storage after this pair: 6.96 GiB. Recheck before each new run.

### Matrix Controller Preflight

| Evidence | Result |
| --- | --- |
| Campaign | `/tmp/p2p-vpn-phase3-matrix-smoke-20260908d` |
| Subjects / profiles | Both pinned subjects, public and private profiles |
| Smoke results | Four completed runs in 33.06 seconds; not acceptance measurements |
| Resume check | Six seconds; all saved results reused without launching subjects |
| Earlier attempts | `20260908a` and `20260908b` suffixes retain permission failures |

- The controller copies and hashes subjects and the namespace harness into a private campaign directory.
- Each paired workload receives fixed identities; execution follows the frozen matrix order.
- Resume verifies controller, harness, subjects, plan, and saved-run identity hashes. Unfinished markers require inspection, not automatic retry.
- `complete.json` means campaign execution ended; it does not mean analysis or phase-3 acceptance is complete.
- Reserve an additional 32 MiB for the capped controller log and one MiB for run manifests. Retain failed and censored results.
- Run under the same OS user for creation and resume. Root-owned historical artifacts may require sudo for complete storage accounting.

The initial permission failures occurred before subject startup. Copying subjects
into the campaign directory avoids private build-directory traversal from the
isolated user namespace. No subject runtime changes were made.

Controller verification: 20 measurement tests and 40 non-ignored namespace tests
passed. Formatting, required Clippy groups, and the cached Nix source-inventory
check passed. Broader runtime and Android suites were not repeated for this
measurement-only change; the four smoke runs cover the new process launcher.

### Campaign Commands

Build `resource_measurement` and `tun_namespace` test executables with the cached
Nix toolchain, offline and with at most two jobs. Set `CONTROLLER` and `HARNESS`
to their absolute executable paths from Cargo's `--no-run` output.

```bash
sudo env \
  P2P_VPN_MATRIX_ROOT=/tmp/p2p-vpn-resource-campaign \
  P2P_VPN_MATRIX_MODE=full \
  P2P_VPN_MATRIX_MAX_PAIRS=1 \
  P2P_VPN_MATRIX_BUILDS="$PWD/docs/developer/kademlia-resource-builds.json" \
  P2P_VPN_MATRIX_HARNESS="$HARNESS" \
  "$CONTROLLER" --ignored --exact resource_matrix_campaign --nocapture
```

`MAX_PAIRS=1` pauses after one newly executed pair without changing the 24-pair
plan. Omit it to execute all remaining pairs. Do not build during observations.
For smoke-only validation, use `MODE=smoke` and a separate campaign root.

```bash
sudo env \
  P2P_VPN_MATRIX_ROOT=/tmp/p2p-vpn-resource-campaign \
  P2P_VPN_MATRIX_MODE=full \
  P2P_VPN_MATRIX_RESUME=1 \
  P2P_VPN_MATRIX_MAX_PAIRS=1 \
  P2P_VPN_MATRIX_BUILDS=/tmp/p2p-vpn-resource-campaign/builds.json \
  P2P_VPN_MATRIX_HARNESS=/tmp/p2p-vpn-resource-campaign/harness \
  /tmp/p2p-vpn-resource-campaign/controller \
  --ignored --exact resource_matrix_campaign --nocapture
```

- Preserve the original subject binaries as well as the campaign copies; both are hash-checked.
- Keep campaign directories private: `keys.json` and endpoint configs contain ephemeral private keys.
- Publish redacted summaries and hashes, not raw identity/configuration files.

### Process Window Summaries

The `resource_summarize_observations` test executable reads an existing bounded
JSONL artifact. It writes a new, private JSON summary without running VPN nodes.
Set `ANALYZER` to the current `resource_measurement` executable from Cargo.

```bash
sudo env \
  P2P_VPN_RESOURCE_OBSERVATIONS=/absolute/path/to/observations.jsonl \
  P2P_VPN_RESOURCE_SUMMARY=/tmp/p2p-vpn-process-summary.json \
  "$ANALYZER" --ignored --exact resource_summarize_observations --nocapture
```

| Output | Meaning |
| --- | --- |
| `observation_sha256` | Hash of the exact input bytes |
| `windows` | Separate stage summaries for endpoints A/B and infrastructure |
| `cpu_seconds`, `cpu_percent_one_core` | Available only with three samples, 95% coverage, and no invalid interval |
| `observed_cpu_seconds` | Diagnostic sum over valid intervals, even when the full window is unavailable |
| `invalid_intervals`, `cpu_unavailable_reasons` | Explicit reset, replacement, gap, timing, or coverage failures |
| `gauges` | RSS, threads, sockets, vanished descriptors, and selected process/namespace TCP states |
| Gauge `mean`, `sampled_peak` | Sample mean and sampled maximum, not time integration or kernel high-water marks |

- Missing process captures are not zero gauges. First/last values remain unavailable if those captures are missing.
- An absent TCP state inside a valid captured map means zero sockets in that state.
- Unknown metadata fields are discarded; configurations and private keys never enter the output.
- Output creation refuses to overwrite an existing file. Input is bounded by the protocol's 16-MiB observation limit.
- Incomplete stage boundaries are rejected explicitly; this tool does not yet summarize interrupted windows.
- This is process-only analysis. Counter aggregation, run-outcome gating, paired deltas, and repetition statistics remain outstanding.

Validation: 26 measurement tests passed, plus analysis of both first-pair
acceptance artifacts. Each endpoint's idle window had over 99.99% coverage
and no invalid interval. Required Clippy checks and cached Nix source checks
passed; no runtime, Android, or Lean verification was added for this reader.

## Subjects

| Subject | Revision | Rationale |
| --- | --- | --- |
| Baseline | `5ecb01ea` | Immediately precedes the first workstream fix, `14782e7d`, for recovery-query backoff |
| Current | `3b503ad2` | Completed aggregate bounds and sustained recovery/settling acceptance |
| Measurement harness | `2a1ff204` | One external orchestrator and infrastructure helper for both subjects; executable hashes in the campaign evidence |

- Build each subject from its own unchanged runtime, manifest, and lockfile.
- Record full revisions, binary hashes, compiler, profile, environment, and harness hashes before runs.
- Use matching optimized build settings; inspect cache and disk budgets before building.
- Preserve baseline registry Kademlia and current vendored Kademlia. Their implementation difference is part of the comparison.
- Separate subject artifacts to avoid stale executable reuse documented in the [earlier comparison](idle-resource-comparison.md#reproduction-notes).

Both optimized subjects have been built offline in unchanged, revision-pinned
Jujutsu workspaces. The [build manifest](kademlia-resource-builds.json) records
full revisions, binary/lockfile hashes, cached Nix toolchain, and build settings.

## Harness Boundary

| Component | Common Contract |
| --- | --- |
| Endpoint | `p2p-vpn up --config ... --control-socket ...` using the unmodified subject CLI |
| Underlay | Isolated namespaces; no Internet route or physical-host management |
| Infrastructure | A separately pinned helper serving DHT and relay protocols for both subjects |
| Membership | Same ID-only peer configuration, with only isolated infrastructure overrides |
| Diagnostics | Shared control-socket fields plus external process observations |
| New counters | Supplemental current-only metrics; missing baseline values remain unavailable |

Public-protocol CLI nodes run Kademlia in client mode. Enabling relay service
does not make them DHT servers. Freeze the existing fixture's server-mode helper
separately; do not change baseline runtime behavior to create infrastructure.

The phase-2 harness requires new resource gauges and current-only limits.
Those assertions remain valid for current acceptance, but are not common
baseline readiness conditions. Failed baseline recovery must remain visible.

## Required Matrix

Each workload runs under shared-public and private-primary/separate-pairing DHT
profiles, with three independent paired repetitions: eight cells, 24 pairs,
48 subject runs. Run subjects sequentially, not concurrently.

| Workload | Required Stimulus / Evidence |
| --- | --- |
| Healthy idle | Verified readiness; no application payload during the sampled window; boundary traffic checks |
| Sustained traffic | Fixed offered rate, packet size, direction, and duration; delivered bytes/packets and loss |
| Failure and recovery | Fixed link/infrastructure faults and address changes; automatic recovery or explicit censoring |
| Pressure and release | Identical bounded offered load and underlay restriction; measured pressure, release, drain, and post-release footprint |

### Frozen Protocol v2

These timings are selected before acceptance observations. Any necessary protocol
change must be documented and versioned; do not mix versions in a paired cell.
Smoke results are excluded from the acceptance dataset.

Version 2 retains version 1's topology, rates, payloads, durations, and ordering.
It adds a three-packet preload and offered-count validation after preflight
exposed reply-dependent underdriving. Version 1 preflight data remains excluded.

| Common Setting | Value |
| --- | --- |
| Startup window | 120 seconds from releasing both endpoint start gates; readiness must occur within this window |
| Warmup | 100 seconds after the fixed startup window, regardless of earlier readiness |
| Sampling | Five-second scheduled cadence; record actual timestamps, capture time, and missed slots |
| Control queries | State and status per endpoint; each call limited to one second |
| Maximum valid interval | 7.5 seconds; never interpolate across an unavailable observation |
| Runtime workers | `TOKIO_WORKER_THREADS=2` for each endpoint; fixed two-worker infrastructure helper |
| Logging | Unset `RUST_LOG`; no periodic metrics CLI override; eight MiB per endpoint log and 32 MiB for infrastructure |
| Observation storage | 16 MiB JSONL limit per subject run; exceeding it censors the run, not silent truncation |
| Outer watchdog | 2,400 seconds per subject, including setup, workload, final capture, and teardown |
| Isolation | Fresh namespaces and daemon processes per subject; no Internet route, build, or parallel task workload |

| Workload | Schedule After Warmup |
| --- | --- |
| Idle | 300 seconds without application traffic; five-request boundary ping before and after |
| Traffic | 180 seconds at 50 echo requests/second, 512-byte payload, A to B; 100-second drain; 180-second post-load idle |
| Failure/recovery | Disable direct LAN and infrastructure link for 130 seconds; restore infrastructure for 960 seconds; change LAN addresses and restore direct link for 375 seconds; observe another 180 seconds |
| Pressure/release | 60 seconds at 200 echo requests/second, 1,000-byte payload, A to B; remove shaping and stop load; drain 100 seconds; observe post-release idle for 180 seconds |

- Traffic caps: 9,000 requests for sustained traffic; 12,000 for pressure. Record actual sent and received counts, not theoretical offered work.
- Use `ping -l 3 -c COUNT` with the fixed interval; the runner stops traffic at the stage deadline. Do not add ping's `-w`, which changes count-limit behavior.
- Require 98% to 100% of the nominal request count. Outside that range, report workload-fidelity failure and exclude efficiency deltas, even if final connectivity works.
- The three-packet preload allows small bursts; timing precision is not hard real-time. Report actual offered work and account for up to 2% rate deviation in comparisons.
- Pressure shaping: direct A egress, `netem delay 50ms rate 64kbit limit 16`. Preserve default packet transport and queue configuration.
- Capture `tc -s -j qdisc` before, during, and after pressure; record actual shaping activity and path changes rather than assuming offered load reached the bottleneck.
- Recovery addressing: move direct underlay `10.253.0.1/2` to `10.253.1.1/2`; keep overlay identity and configuration unchanged.
- Recovery probes: one echo request in each direction every five seconds. Record first success and first five consecutive bidirectional successes.
- Recovery stages end at fixed times, not on first success. Record relay/direct path evidence; do not infer path type from ping success alone.
- Boundary probes have a separate ten-second budget each, outside resource windows. A failed boundary gate invalidates equivalent-work comparisons.

### Timer Rationale

| Production Timer | Measurement Consequence |
| --- | --- |
| 60-second swarm idle timeout; 120-second maintenance interval | A 300-second idle window crosses multiple idle and maintenance opportunities |
| 90-second query owner deadline; up to ten seconds cleanup allowance | Use 100-second warmup/drain, matching Phase 2 cleanup reasoning |
| 15-second stream in-flight deadline | Sixty seconds of pressure spans multiple timeout opportunities |
| LAN-first grace, discovery backoff, relay reservation, and direct address quarantine | Reuse Phase 2's 130-second outage, 960-second relay and 375-second direct recovery budgets |
| 600-second packet sessions; 900-second address publication refresh | Long recovery runs cross these timers; short idle/traffic runs do not establish renewal or long-term leak behavior |

Timer sources: [runtime constants](../../src/runtime/runner.rs),
[swarm idle timeout](../../src/runtime/p2p.rs), and the accepted
[recovery fixture](../../tests/support/recovery_soak.rs).
No timer is shortened in either subject to make this experiment pass.

### Ordering and Comparability

1. Repeat three times; within each repetition visit public then private profiles.
2. Within each profile run idle, traffic, recovery, then pressure.
3. Number the eight cells from zero. Baseline runs first when cell index plus zero-based repetition index is even; otherwise current runs first.
4. Reuse each pair's generated identities, config bytes, and infrastructure identity. Start fresh processes and namespaces for the second subject.
5. Record full source revisions, binary/config hashes, toolchain, kernel, clock tick rate, effective config, and helper revision before acceptance runs.

- Keep failures, watchdog expiries, missing metrics, and exclusions in the report. Never replace a failed subject with a favorable smoke result.
- Per window, report CPU seconds, one-core-normalized CPU, sample count, coverage, sampled RSS mean/peak, socket/connection counts, and counter deltas/rates.
- CPU ratios use total valid CPU seconds divided by total valid elapsed time, not an unweighted mean of unequal intervals.
- Require at least 95% temporal coverage, three samples, and no invalid interval for a window's paired CPU delta. Report rejected windows and retained raw observations.
- RSS/socket gauge means are sample means, not time-integrated values. Peaks are sampled peaks, not kernel or allocator high-water marks.
- Preserve current-only owner/admission gauges separately. Compare common metrics only when both subjects expose them and counters do not reset.
- Report all three paired results plus median and range of absolute values/deltas. Percentage deltas require a nonzero baseline.
- Diagnose contaminated or invalid harness runs before replacements; retain original outcomes and document the replacement. No result-driven ordering changes.

## Process Sampling

Implementation: [process sampler](../../tests/support/process_sample.rs).
The existing Phase 2 sampler is unchanged.

| Field | Meaning |
| --- | --- |
| `pid`, `start_ticks` | Process identity, checked before and after capture |
| `cpu_ticks` | User plus system CPU ticks; normalize using recorded `CLK_TCK` and elapsed time |
| `rss_kib` | Current process RSS; report peak sampled RSS separately from allocator usage |
| `socket_fds` | Socket file descriptors, including duplicate descriptors |
| `socket_inodes` | Distinct socket inodes owned by the process at the FD observation |
| `process_tcp_states` | TCP/TCP6 rows whose nonzero inode appeared in the process's FD inventory |
| `namespace_tcp_states` | All TCP/TCP6 rows in the namespace; never label these process-owned connections |
| `vanished_fds` | Descriptors that disappeared while being inspected |
| `capture_seconds` | Capture duration; observations are not an atomic kernel snapshot |

- Unowned TIME_WAIT rows appear only in namespace totals.
- Socket creation/closure during capture can cause attribution gaps; retain capture timing and FD-race counts.
- Missing or malformed process fields are errors, not zero-valued observations.
- Counter regression or process replacement invalidates the capture.
- Analysis across captures must also detect identity changes, resets, missing data, and sampling gaps.

## Initial Verification

### Interval Analysis

Implementation: [resource analysis](../../tests/support/resource_analysis.rs).

| Condition | Analysis Behavior |
| --- | --- |
| Valid CPU interval | User/system tick delta divided by recorded clock rate and actual elapsed time |
| CPU normalization | One fully occupied logical CPU is 100%; multiple threads may exceed 100% |
| Missing observation or counter | Explicit unavailable result; never substituted with zero |
| PID/start-time change or decreasing counter | Reject interval rather than joining different process lifetimes |
| Nonfinite, reversed, or excessive sample interval | Reject invalid timing or report a sampling gap |
| Failed/censored subject | Retain outcome and reason; do not calculate paired efficiency deltas |
| Zero baseline | Absolute delta remains available; relative percentage is undefined |

The caller must supply matching workload/metric windows and a frozen maximum
sampling gap. Interval checks alone do not establish workload equivalence.
Acceptance matrix execution and run-level aggregation remain to be implemented.

### CLI Harness Smoke

The [CLI harness](../../tests/support/resource_cli.rs) reuses the namespace and
infrastructure helpers without running the measured daemon inside a test binary.
It accepts an external subject executable through `P2P_VPN_RESOURCE_SUBJECT`.

| Component | Smoke Behavior |
| --- | --- |
| Endpoints | Execute the supplied CLI with minimal configuration and two Tokio workers |
| Process identity | `exec` preserves the namespace child's PID for external sampling |
| Underlay | Direct LAN link plus isolated bridge ports to the DHT/relay helper; no Internet route |
| Infrastructure | Existing Phase 2 server-mode helper, identical for either subject |
| Readiness | Successful control queries, process capture, and bidirectional overlay ping within 120 seconds |
| Watchdog | 180 seconds for the enclosing namespace process tree |
| Artifacts | Private temporary directory containing configs, bounded daemon logs, observations, and smoke result |

```sh
nix develop -c cargo build --locked --bin p2p-vpn
P2P_VPN_RESOURCE_SUBJECT="$PWD/target/debug/p2p-vpn" \
P2P_VPN_TUN_E2E_RECOVERY_PROFILE=public \
  nix develop -c cargo test --locked --test tun_namespace \
  tun_namespace_resource_cli_smoke -- --ignored --exact --nocapture
```

Repeat with `P2P_VPN_TUN_E2E_RECOVERY_PROFILE=private` for a private primary DHT
and separate public pairing DHT. Set the subject path to the actual Cargo target
directory when overriding `CARGO_TARGET_DIR`.

Smoke artifacts explicitly contain `acceptance_measurement: false`. This check
does not measure idle efficiency, sustained traffic, fault recovery, or pressure.
It does not require baseline-absent Kademlia owner gauges to declare readiness.

The optimized baseline and current subjects each passed public and private smoke
checks sequentially with one common harness binary. Evidence directories and
hashes are recorded in the build manifest. No builds ran during these checks.

At the CLI smoke checkpoint, the namespace target passed 31 non-ignored tests; 15 opt-in tests were ignored in
that invocation. The four CLI smoke invocations above were run separately.
Earlier full sustained acceptance was not repeated for helper visibility and
measurement-only additions; production runtime and vendor sources are unchanged.

### Timed Workload Preflight

The [typed protocol](../../tests/support/resource_protocol.rs) defines the 24
pairs, subject order, stage durations/actions, offered traffic, and storage caps.
The [workload runner](../../tests/support/resource_workload.rs) executes these
stages against the supplied CLI, retaining raw observations and fault evidence.

```sh
P2P_VPN_RESOURCE_KEYS=/tmp/p2p-vpn-measurement-keys.json \
  nix develop -c cargo test --locked --test tun_namespace \
  resource_cli::generate_pair_keys -- --ignored --exact --nocapture

P2P_VPN_RESOURCE_KEYS=/tmp/p2p-vpn-measurement-keys.json \
P2P_VPN_RESOURCE_WORKLOAD=pressure \
P2P_VPN_RESOURCE_SUBJECT=/path/to/pinned/p2p-vpn \
P2P_VPN_TUN_E2E_RECOVERY_PROFILE=public \
  nix develop -c cargo test --locked --test tun_namespace \
  tun_namespace_resource_workload -- --ignored --exact --nocapture
```

- The key file contains three isolated test identities, is created mode `0600`, and is never overwritten. Reuse it for paired subjects.
- Workloads: `idle`, `traffic`, `recovery`, `pressure`. Timings are not shortened through environment overrides.
- These invocations write `acceptance_measurement: false`; they are preflight tools, not the acceptance matrix executor.
- Endpoint, infrastructure, and control-query observations remain distinct. Failed captures retain their errors and do not become zero samples.
- Delayed sampling records skipped slots instead of producing a burst of catch-up observations.
- Recovery confirmation checks the expected validated peer and actual selected path, not only successful ping.

### Pressure Preflight Evidence

| Item | Observed Result |
| --- | --- |
| Subject/profile | Optimized current `3b503ad2`, public primary DHT |
| Artifacts | `/tmp/p2p-vpn-resource-cli-smoke.edee6791209e29da` |
| Harness SHA-256 | `833b0f97a3ceeeca558db0e061939f17865cc658649430ba0918ee56508617be` |
| Duration | 567.89 seconds including enclosing namespace setup/teardown |
| Stage behavior | Full startup, warmup, pressure, drain, and post-release windows completed |
| Pressure evidence | Kernel queue reached 16 packets; 5,275 shaping drops before release |
| Release evidence | Shaping removed; both application queues observed empty in post-release idle |
| Final delivery | Five of five requests succeeded in each direction |
| Samples | 234 endpoint observations; no skipped sampling slots |
| Missing observation | One startup control-socket absence, retained explicitly |
| **Fidelity limitation** | Generator sent 5,956 requests in 60 seconds, below the configured 200 requests/second |

This preflight verifies stage execution and recovery after pressure, **not a valid
fixed-rate resource comparison**. Its workload result indicates functional
completion only. No acceptance run has been collected or inferred from it.

### Generator Investigation

An isolated loopback calibration reproduced the rate change without p2p-vpn.
At a five-millisecond interval, the installed ping sent 401 requests in two
seconds with replies, but only 197 with 100% loss. Flood mode did not fix it.

```sh
nix develop -c cargo test --locked --test tun_namespace \
  resource_cli::calibrate_ping_rate -- --ignored --exact --nocapture
```

The upstream [ping implementation](https://github.com/iputils/iputils/blob/20250605/ping/ping_common.c#L297)
uses a bounded send-token budget and minimum short waits when requests remain in
flight. Version 2 raises the preload to three and uses an external stop deadline;
the endpoint binaries and VPN protocol are unchanged.

Three repeated calibrations per rate/loss combination passed the predeclared
calibration bounds: 98% of the nominal count through nominal plus initial preload.
At 200 requests/second, two-second counts were 403 with replies and 395 without.
At 50 requests/second they were 100 with replies and 100-101 without.

The actual workload uses a hard request cap, unlike the two-second calibration.
Its stricter upper bound is 100% of the nominal count. The full-pressure rerun
below passed with the unchanged optimized current subject and fixed identities.

### Version 2 Pressure Preflight

| Item | Observed Result |
| --- | --- |
| Artifacts | `/tmp/p2p-vpn-resource-cli-smoke.a9c7fa8dce491cff` |
| Harness SHA-256 | `139f3c240ed9295cdc727feec228e0f5a50c4cfebed99b0f979d1c9b44f88594` |
| Subject/profile | Optimized current `3b503ad2`, public primary DHT |
| Offered work | 11,809 requests, within the frozen 11,760-12,000 gate |
| Received replies | 405 under deliberate shaping; loss remains part of the result |
| Shaping evidence | 16-packet queue during pressure; 10,283 shaping drops before release |
| Release | Both application queues empty in post-release idle |
| Final delivery | Five of five requests succeeded in each direction |
| Observations | 234 endpoint samples; no skipped slots; one retained startup control-socket absence |
| Duration | 567.99 seconds including enclosing setup/teardown |

This validates the revised generator in this preflight, not all workload/profile
combinations or the acceptance matrix. Actual offered-rate deviation was about
1.6%; preserve counts and the declared tolerance rather than claiming exact pacing.

### Verification Scope

```sh
cargo test --offline --locked --test resource_measurement -- --test-threads=2
```

Sixteen sampler/analysis/protocol tests passed with cached Nix Rust tooling. Coverage includes inode
attribution, unowned TCP rows, malformed/missing fields, unit validation, process
replacement, CPU overflow/reset, and a live process listener.

The live check also verifies duplicate socket descriptors do not create extra
socket-inode or TCP-connection counts.

Analysis tests cover CPU normalization, missing samples and serialized fields,
counter resets, process replacement, sampling gaps, failed/censored outcomes,
and zero-baseline comparisons.

The namespace target passed 40 non-ignored tests. Full-duration public/current
pressure preflight and isolated ping calibration were run separately. Other timed
workload/profile combinations have not yet received end-to-end verification.

Required Clippy groups, formatting, whitespace, and cached Nix test-source
integration passed. The full workspace and namespace acceptance suites were not
rerun for these independent measurement-tooling checkpoints.

This is tooling validation, not baseline/current acceptance or a performance
result. No daemon implementation or Phase 2 acceptance behavior was changed.

## Resource Limits

- Initial retained task storage: 5.13 GiB; total limit: 10 GiB.
- After isolated optimized subject builds: 6.45 GiB, including previous acceptance artifacts.
- After workload preflight and calibration: 6.46 GiB.
- At most two Cargo build jobs across the task; downloads capped at 10 Mbps.
- No builds or other task workloads during comparative observations.
- Apply the frozen per-run log/sample caps and retention policy before measurements.
- Do not remove prior acceptance evidence merely to create build space.
- Acceptance logs and observations reserve at most 64 MiB per subject: three GiB for 48 runs, excluding small manifests/configs.
- Before each run, require current usage plus its full allowance to remain below 9.75 GiB; retain the remaining headroom for summaries and diagnostics.
- On budget pressure, compress completed text artifacts or remove only disposable subject dependency caches, keeping pinned binaries and build manifests.
- Preserve failed-run outcomes and evidence when rerunning. Recheck the budget for replacements; never silently discard failed comparisons.
