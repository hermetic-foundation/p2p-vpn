# Sustained Resource Review

## Status

Active bounded review opened at `00b1b58a` on 2026-09-09.
This contains the measurement plan and partial results, not a completion claim.
No physical device or deployed host is authorized for this work.

## Checklist

- [x] Locate prior measurements and identify reuse limits.
- [x] Define capture windows, budgets and decision rules.
- [ ] Audit collectors and freeze workload manifests before capture.
- [x] Measure connected idle and periodic-collector overhead in the isolated debug fixture.
- [x] Measure unavailable peers and retain retry/backoff timelines in the isolated debug fixture.
- [ ] Measure matched sustained traffic (S3).
- [x] Measure repeated packet/byte pressure and recovery (S4); allocation and probe-owner findings remain open.
- [ ] Attribute retained allocations, including signed-ledger refreshes.
- [ ] Measure lifecycle churn and multi-network isolation.
- [ ] Measure Android background CPU/wakeup proxies on a cached emulator.
- [ ] Reproduce and correct defects; validate before/after behavior.
- [ ] Publish results, cleanup evidence and a requirement-by-requirement audit.

## Existing Evidence

| Evidence | Established | Remaining Gap |
| --- | --- | --- |
| [Idle comparison](idle-resource-comparison.md) | Paired debug 60-second captures; current-only release samples | Sustained plateau and matching release baseline |
| [Membership resources](forwarder-resource-comparison.md) | 8/128/256 records; evaluation reuse | Exact retained allocations and whole-daemon impact |
| [Queue pressure](sustained-pressure-results.md) | Corrected S4: 20 TCP pressure/recovery rounds, two captures per limit profile | RSS allocation attribution and aggregate probe-owner bound |
| [Kademlia acceptance](kademlia-workstream-acceptance.md) | Enforcement and scoped recovery fixes | RM-2 sampling, RM-3 unequal work, RM-4 backend confounding, RM-5 allocation attribution |
| [Android lifecycle](android-lifecycle-audit.md) | Ownership regressions and multi-network restoration | Sustained CPU/memory and physical battery behavior |

Retain historical failures, censoring and sampling gaps. Missing observations
are not zero activity. Reopen completed ownership/enforcement work only with
new causal evidence; do not repeat the public campaign without authorization.

## Capture Protocol

### Controls

- Baseline runtime: `00b1b58a`; record full revision and executable SHA-256.
- Pin fixture revision, toolchain, profile, transport, topology and configuration.
- Use current-only results unless a reproduced fix warrants paired comparison.
- For comparisons, use identical fixtures and alternate baseline/fixed order twice.
- Separate debug, release, Linux process and Android emulator measurements.
- Build first; execute saved binaries without concurrent review builds.
- Use private fixtures for infrastructure; distinguish infrastructure and overlay peers.

### Workloads

These are measurement windows, not extended functional recovery deadlines.
Repeat each capture twice; retain failed attempts separately from successful data.

| ID | Workload | Window / Work |
| --- | --- | --- |
| S1 | Connected idle | Existing 30-second warmup; 300-second capture |
| S2 | Configured peer unavailable | 300 seconds; retain retry/backoff timeline |
| S3 | Fixed-transport sustained traffic | 30-second warmup; 300-second fixed offered load; 60-second drain observation |
| S4 | Queue pressure/recovery | Five existing pressure rounds in the same processes |
| S5 | Lifecycle churn | Ten fixed connect/disconnect or unavailable/recovered cycles; final 60-second settling |
| S6 | Ledger retention | 8/128/256 records; ten construct/refresh/drop cycles; unchanged and forced evaluations separated |
| S7 | Android two-network background | 30-second warmup; 300-second idle and load phases; five independent disable/enable cycles |

Freeze packet size/rate, transition schedules and transport settings in each
fixture manifest before its first capture. Select supported controls from source
and configured limits, not observed results. Record actual delivered work.

### Observations

| Series | Required Evidence |
| --- | --- |
| CPU | User/system tick deltas over monotonic time; percent of one core |
| Memory | RSS/PSS plus live/retained allocation or allocation-owner evidence |
| OS resources | PID/start identity, threads, total/socket descriptors, connection states |
| Runtime resources | Queue packets/bytes/in-flight work, task/timer/query owners and caps |
| Network work | Offered/transmitted/accepted/dropped packets/bytes; attempts, errors, probes and path changes by backend/role |
| Interference | Host load, collector overhead, actual timestamps and missing intervals |
| Android | App/native identity, background CPU and wakeup/scheduled-work proxies |

Sample cheap OS counters every second and compact runtime counters every five
seconds. Avoid recurring full routing dumps. Functional probes must not depend
on collector latency. Compare collector-on/off before attributing sensitive costs.

## Decision Rules

### Collector Audit: Descriptor Coverage

The newer `process_sample` collector validates process identity across capture
and separates process-owned TCP sockets from namespace-wide state. The older
idle collector lacks that attribution and capture-duration metadata.

The shared newer collector now records `total_fds` as well as socket descriptors.
It counts successfully resolved descriptor links; concurrent disappearances
remain in `vanished_fds`. This is a non-atomic sample, not a kernel resource bound.

Historical samples without `total_fds` deserialize as unknown, not zero. Window
summaries count these as missing and preserve unknown first/last values.
This adds test-tool evidence only; no production runtime changed.

| Validation | Evidence |
| --- | --- |
| Resource suite | 44 passed, four opt-in campaign tests ignored; `/tmp/p2p-vpn-sustained-collector-final-tests.log` |
| Namespace sampler | Five passed; `/tmp/p2p-vpn-sustained-collector-namespace-tests.log`; no namespace campaign executed |
| Live process / compatibility | Non-socket descriptors counted; old JSON remains readable; missing gauge samples are not zero |
| Static checks | Required correctness/suspicious/performance Clippy groups pass for resource and namespace targets; advisory warnings retained |
| Format | Cached rustfmt check passes on all four changed support files |

Initial `cargo fmt` could not locate its subcommand in the cached Cargo wrapper.
The existing `/tmp/p2p-vpn-review-rustfmt/bin/rustfmt` performed the check directly;
no formatter or dependency download was needed.

Full collector integration, independently timed probes and compact runtime
sampling remain open for full S1/S2 acceptance. This unit gate is not a sustained capture.

### Idle Collector Integration

The isolated idle fixture now delegates to the shared process sampler. Existing
report fields retain their meanings; total descriptors, capture duration,
vanished descriptors and process-owned TCP states are additive.

One descriptor-duplication unit test failed under parallel tests because another
test changed the process socket count. It now runs in an isolated child process;
the exact descriptor/inode assertions are unchanged.

- Negative: `/tmp/p2p-vpn-sustained-idle-final-tests.log`, expected three socket descriptors but observed two.
- Final tests: `/tmp/p2p-vpn-sustained-idle-cadence-tests.log`, 44 resource and 42 namespace tests pass.
- Static: `/tmp/p2p-vpn-sustained-idle-cadence-clippy.log`, required groups pass; advisory warnings remain.
- Cached rustfmt passes. Opt-in namespace/campaign tests remain ignored by the unit gate.

The first 300-second attempt was stopped before its diagnostic logs exceeded the
declared budget: 1,733,189 bytes retained. This is an incomplete capture, not a
daemon failure or valid CPU/memory comparison. No completed sample report exists.

Its retained directory is
`/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.974ebd4b7fd17955`;
outer log: `/tmp/p2p-vpn-sustained-idle-os-1.log`.

The unshare parent did not terminate its namespace on SIGTERM. Killing the owned
namespace init ended all child processes; the outer runner exited 101. Evidence
was preserved. No deployed service or physical device was involved.

Idle-mode diagnostic cadence is now five seconds, matching the planned runtime
observation cadence; other modes retain one second. This changes fixture overhead,
so earlier one-second captures are not paired efficiency baselines.

### First Five-Minute OS Capture

The revised fixture passed in 346.49 seconds, including its 30-second warmup
and 300-second capture. Runtime source is unchanged from `00b1b58a`; test tooling
is based on `2f430c58` plus the idle-collector integration described above.

| Observation | Node A | Node B |
| --- | ---: | ---: |
| Samples | 300 | 300 |
| CPU, percent of one core | 0.1633 | 0.1600 |
| RSS range, KiB | 36,424-36,716 | 36,404-36,444 |
| Total descriptors | 21 throughout | 19 throughout |
| Socket descriptors | 14 throughout | 12 throughout |
| Threads | 20 throughout | 20 throughout |
| Maximum sample gap, seconds | 1.0093 | 1.0097 |
| Maximum capture duration, milliseconds | 5.782 | 5.176 |
| New redials / connection errors | 0 / 0 | 0 / 0 |
| New probes / probe failures | 60 / 0 | 60 / 0 |

Both processes retain one start identity. Both boundary snapshots select validated
direct UDP paths; queue occupancy, queue drops and expiry are zero at boundaries.
These boundary readings are not continuous queue or allocation measurements.

Host one-minute load changed from 1.37 to 1.91. No review builds ran during
capture. This is a debug integration fixture with no Internet route, not a
packaged-daemon, release, QUIC-stream, public-DHT or Android measurement.

| Artifact | Value |
| --- | --- |
| Report | `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.69b7706adef627f1/idle-sample.json` |
| Outer log | `/tmp/p2p-vpn-sustained-idle-os-2.log` |
| Fixture SHA-256 | `517f34821d2d9bd37e697def3b790e45e733edfe1597b9d2ddb6fa1131095dbe` |
| CLI helper SHA-256 | `44ee4819a945a3d0714d19c6ff13a38ba5a5d45b8002b63942f78e69736378e0` |
| Node diagnostic bytes | 1,130,187 combined, below 2 MiB |
| Report bytes | 362,608, below 8 MiB |
| Teardown | Test exits zero; no matching fixture processes remain; evidence retained |

```sh
timeout --signal=TERM --kill-after=10s 450 env \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 P2P_VPN_TUN_E2E_IDLE_SECONDS=300 \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-fc4bbc3b02326c73 \
  tun_namespace_ping_crosses_two_node_overlay --ignored --exact --nocapture
```

This supplies the first S1 OS-resource observation, not completed S1 acceptance.
Repetition, collector overhead, compact runtime ownership series, unavailable-peer
behavior and retained-allocation attribution remain open. Stable descriptors
and small RSS ranges do not establish long-term leak freedom.

### Workload Acceptance

The idle collector now has a separate five-second runtime-status thread. OS
sampling proceeds independently; every runtime observation includes its scheduled
time, completion time, query duration, values and an explicit error when unavailable.

| Capture Guard | Behavior |
| --- | --- |
| Status request | One-second timeout; no recurring full routing-state request |
| Parser | At most 512 lines of 256 bytes; numeric counters and Boolean flags retained |
| Invalid response | Duplicate keys, malformed values and oversized input recorded as failure, not zero |
| Scheduling | Slow queries skip slots; no catch-up burst |
| Completeness | Missing/error snapshots make the series incomplete; report is written before test failure |
| Report | At most 8 MiB serialized output |

The first live-socket smoke run passed with 22 OS observations and four runtime
snapshots, each containing 332 metrics. Maximum query duration was 3.016 ms.
This ten-second capture validates the tool, not a sustained workload.

- Initial smoke: `/tmp/p2p-vpn-sustained-runtime-sampler-smoke.log`.
- Initial artifact: `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.c0418b05d9a85f6f/idle-sample.json`.
- Final unit gate: `/tmp/p2p-vpn-sustained-runtime-sampler-complete-tests.log`, 45 passed, 22 opt-in tests ignored.
- Required Clippy groups and cached rustfmt pass; advisory warnings retained.

The final unit gate adds missing/error completeness checks. Counter queries add
observer work; earlier OS-only captures are not interchangeable baselines for
this collector. Sustained repetitions and collector-on/off controls remain required.

Final live-socket smoke also passes in 56.41 seconds, including warmup and teardown.
Its four 332-metric snapshots set `runtime_samples_complete=true`; no matching
fixture process remains. No builds ran during either observation window.

- Final smoke: `/tmp/p2p-vpn-sustained-runtime-sampler-final-smoke.log`.
- Report: `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.2d3c6ceafb56c4dc/idle-sample.json`.
- Fixture SHA-256: `0b77b6c8e603a4b3fff775ad51a1cae56f6d45f3dfa8d7cea7a14007eb3d20f1`.

| Dimension | Required Outcome / Trigger |
| --- | --- |
| Function | Existing traffic, admission, isolation and recovery assertions pass without rescue |
| Hard resources | Configured caps hold; unexpected owner growth is a failure |
| Teardown | Owned processes, descriptors, tasks and disposable state released |
| Quiescence | Traffic queues drain and temporary owners retire; required discovery/health work remains allowed |
| Retries | Configured backoff/concurrency respected; no uncontrolled dial growth |
| Allocations | Equivalent checkpoints have explained retention; accumulating live allocation requires attribution |
| RSS | Growth at each of the final three equivalent checkpoints triggers investigation, not an automatic leak conclusion |
| CPU | Repeated increase exceeding both 20% relative and 0.5 percentage points triggers attribution |
| Comparability | No paired efficiency claim with unequal delivered packet/byte totals or different transports |
| Capture quality | Missing/truncated required data means incomplete evidence; retain and correct the capture |

CPU thresholds trigger investigation; they are not a production SLA or permission
to ignore smaller reproduced defects. RSS alone cannot prove a leak or its absence.
No universal RSS ceiling or physical-energy claim is declared.

## Execution Order

### S1: Two Full Idle Captures

Two current-only captures pass at fixture `67b1ff3b`, with no runtime changes
from `00b1b58a`. Each uses 30-second warmup, 300-second observation, identical
debug binaries and settings, and a fresh isolated pair of processes.

| Run | A CPU, One Core | B CPU, One Core | A RSS Range, KiB | B RSS Range, KiB |
| --- | ---: | ---: | ---: | ---: |
| 1 | 0.1733% | 0.1667% | 36,448-36,692 | 36,868-37,004 |
| 2 | 0.1833% | 0.1733% | 37,076-37,168 | 36,808-36,872 |

| Observation Across Both Runs | A | B |
| --- | ---: | ---: |
| OS samples per run | 300 | 300 |
| Runtime snapshots per run | 60 | 60 |
| Total descriptors | 21 throughout | 19 throughout |
| Socket descriptors | 14 throughout | 12 throughout |
| Threads | 20 throughout | 20 throughout |
| Sampled queue packets/bytes | 0 throughout | 0 throughout |
| Pending connection attempts / retiring connections | 0 / 0 throughout | 0 / 0 throughout |
| Added redials / outgoing connection errors | 0 / 0 | 0 / 0 |
| Added probes over runtime-series interval | 59 | 59 |
| Added probe failures | 0 | 0 |

Every runtime slot from scheduled second 0 through 295 is present without errors.
The OS series spans 300 seconds; do not equate its duration with the 295-second
first-to-last runtime-counter interval. Maximum OS gap was 1.011 seconds rounded up.

Maximum query time was 3.696 ms rounded up. Host one-minute load changed from
3.14 to 1.69 in run 1 and 1.64 to 1.61 in run 2. No review builds ran during either
capture. Native health and AutoNAT work continue; these are not zero-work idle claims.

Both test processes exit successfully: 371.85 seconds and 346.57 seconds including
startup, warmup and teardown. No matching fixture processes remain. Each report
is about 2.29 MB; each pair of node logs is below 1.22 MB, within declared limits.

The [portable derived samples](sustained-idle-samples.json) retain raw paths,
report hashes, binary identity, resource ranges and counter deltas. Raw logs are
`/tmp/p2p-vpn-sustained-s1-1.log` and `/tmp/p2p-vpn-sustained-s1-2.log`.

### RSS Attribution Trigger

Run 2 node B has final three minute checkpoints of 36,856, 36,864 and 36,872 KiB.
This meets the declared investigation trigger. It does not establish a leak:
allocator retention, committed pages and allocating owners are not isolated yet.

Run 1 RSS is unchanged at its final two minute checkpoints; run 2 A rises again
at the final checkpoint. Neither is evidence of an asymptotic heap bound.
The allocation and observer-overhead work must account for these observations.

S1 now has repeated debug idle resource evidence. Collector-on/off comparison,
release/representative transport coverage, unavailable peers, sustained load,
lifecycle churn and Android resource evidence remain open.

### Next Execution Steps

#### Observer Control Protocol

The idle fixture accepts `P2P_VPN_TUN_E2E_IDLE_RUNTIME_SAMPLING=0` to omit only
periodic status queries. Default/on mode uses `1`. OS sampling, before/after
queries, diagnostic cadence, topology and VPN runtime behavior are unchanged.

Disabled reports have `runtime_sampling_enabled=false`, an empty runtime series
and `runtime_samples_complete=null`. Disabled is not a zero-valued or failed
metric series. Replay artifacts retain the selected mode.

Both modes record the collector process before and after sampling. These capture
sampling-phase CPU/retention, not fixture startup, report serialization or the
total cost of diagnostics. Do not subtract assumed overhead from daemon samples.

| Frozen Comparison Setting | Value |
| --- | --- |
| Sequence | Off, on, on, off; fresh processes for each capture |
| Binary / fixture | Identical for all four runs; record executable hash |
| Warmup / capture | 30 / 300 seconds |
| OS / diagnostic cadence | One / five seconds in both modes |
| Workload | Two static peers; no added payload; same isolated direct-UDP topology |
| Scope | Incremental cost of periodic status queries, not all instrumentation |

Short off/on smoke runs verify the control before the four sustained captures.
Those smoke runs do not count as the declared repetitions.

| Control Validation | Result |
| --- | --- |
| Unit tests | 46 passed, 22 opt-in tests ignored; `/tmp/p2p-vpn-sustained-observer-control-tests.log` |
| Static checks | Required Clippy groups and cached rustfmt pass; advisory warnings remain |
| Off smoke | Pass, 56.44 seconds; 22 OS samples, no periodic runtime samples, completeness null |
| On smoke | Pass, 56.49 seconds; 22 OS samples, four runtime snapshots, completeness true |
| Collector identity | Stable before/after; CPU counters nondecreasing in both modes |
| Teardown | Both tests exit zero; no matching fixture process remains |

Both smoke reports use fixture SHA-256
`4bdeffb4b9e6c2eed1130e0976fa995dd03eea606c35c322b41f31883f6c506e`.
Neither used a physical device or concurrent review build.

- Off report: `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.521ad8940416b3ee/idle-sample.json`.
- On report: `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.f7f806fadb8f1b42/idle-sample.json`.
- Outer logs: `/tmp/p2p-vpn-sustained-observer-{off,on}-smoke.log`.

Sustained comparison results and raw-report fingerprints are recorded separately
in [Observer overhead](sustained-observer-overhead.md). This comparison does not
close unavailable-peer, sustained-load or retained-allocation work.

1. Audit existing idle, process, queue-pressure and resource collectors.
2. Inventory cached binaries and storage; build only missing affected targets.
3. Run S1/S2 and overhead controls; finalize S3/S5 fixture manifests.
4. Run load, pressure and allocation attribution; reproduce defects before fixes.
5. Run cached Android multi-network/background measurements.
6. Validate fixes and publish quantitative results with precise exclusions.

## Safety and Budgets

### Unavailable Peer Protocol

S2 uses `tun_namespace_measures_unavailable_peer_resources`, a separate opt-in
direct-UDP fixture. Freeze these controls before its first sustained capture.
The short harness smoke test does not count as a sustained repetition.

| Setting | Frozen Value |
| --- | --- |
| Topology | Existing two-node isolated direct UDP fixture; static overlay peer bindings; no reachable public infrastructure |
| Preparation | Initial overlay traffic, then 30 seconds connected warmup |
| Transition | Node B `veth-b` down; require failed one-second underlay ping |
| Outage observation | 300 seconds after negative ping; OS 1 second, runtime status and diagnostics 5 seconds |
| Restoration | Node B `veth-b` up; same addresses, identities and daemon processes |
| Recovery | Common 60-second deadline, one-packet checks both directions per round, one-second pause between rounds |
| Final traffic | Five packets each direction; all replies required, including B to A's routed prefix |
| Repetitions | Two fresh runs using the same saved executable; record hash |
| Deadlines | Default namespace watchdog 510 seconds; external watchdog 540 seconds; no timeout scaling |
| Data budget | Outage plus recovery JSON combined below 8 MiB; combined node diagnostics below 2 MiB |

`daemon_before` precedes the link transition; periodic/OS samples start after it.
Boundary deltas therefore include transition time. `transition_seconds` preserves
that distinction. No payload is offered during the measured outage.

Retain `idle-sample.json` even when subsequent recovery fails. Recovery evidence
is in `unavailable-recovery.json`; compare final PID/start identities against
the outage samples. Do not restart a daemon or manually repair runtime paths.

This measures local link loss with a known endpoint, not discovery of a new WAN
address. It does not close public-path migration or multi-network isolation.
Default bootstrap candidates remain configured and fail locally without an
Internet route; distinguish those attempts from the single overlay peer.

#### Harness Validation

| Gate | Result |
| --- | --- |
| Namespace unit tests | 47 passed; 23 opt-in tests ignored |
| Required Clippy groups | Correctness, suspicious and perf pass; advisory warnings remain |
| Formatting | Cached rustfmt check passes |
| Final outage smoke | Pass, 66.06 seconds total; ten-second observation only |
| Smoke recovery | 0.3024 seconds after link restoration; 5/5 packets each direction |
| Smoke process identity | Stable PID/start identity through outage and recovery |
| Smoke final path | Both nodes select `direct_udp_datagram` |
| Smoke observations | 22 OS rows and four runtime snapshots; runtime series complete |
| Smoke report / node logs | 232193 combined JSON bytes; 225350 combined diagnostic bytes |
| Connected-idle regression | Same final executable passes, 81.64 seconds; 22 OS rows and four complete runtime snapshots |
| Teardown | Both final smoke tests exit zero; no matching fixture process remains |
| Production changes | None; test harness, shared test helpers and developer docs only |

Final fixture SHA-256:
`466f9c5612253f689651771b02c68bb397d4994adcc566cc439ad938b4bd3804`.
Both declared 300-second S2 captures passed. See
[sustained unavailable-peer results](sustained-unavailable-results.md) for resource
ranges, infrastructure/overlay retry attribution and the open RSS investigation.

- Final smoke artifacts: `/tmp/p2p-vpn-tun_namespace_measures_unavailable_peer_resources-1.cf63a9f794fa498f/`.
- Validation logs: `/tmp/p2p-vpn-sustained-unavailable-final-{tests,clippy,smoke}.log`.
- Connected smoke: `/tmp/p2p-vpn-sustained-unavailable-connected-smoke.log`; report suffix `1.0de2f593ff99346b` under the connected-idle artifact prefix.
- Earlier smoke retained in `1.0f4c43171f24e683`, with executable hash `7454336fc3fa3f2c572ac3086d1cf4108b3c7058dce8d03d3bd6bf40ff0e9ddb`.
- No builds overlapped either smoke capture; no physical device was contacted.

### Pressure Protocol

S4 reuses `tun_namespace_recovers_after_tcp_queue_pressure` with five existing
pressure/drain rounds per process pair. Packet and byte profiles are separate
captures. Freeze the following settings before collecting sustained evidence.

| Setting | Frozen Value |
| --- | --- |
| Runtime | Debug integration binary, `TOKIO_WORKER_THREADS=2`; direct TCP, no packet datagram listeners |
| Profiles | `packets`: 4 packets / 8192 bytes; `bytes`: 16 packets / 4096 bytes |
| Repetitions | Two fresh five-round captures per profile; same executable for all four |
| Sequence | Packets, bytes, bytes, packets |
| Traffic | Existing ping: 1000-byte payload, 5 ms interval, at most 3000 requests, 20-second deadline |
| Shaping | A egress: 64 kbit/s, 50 ms delay, 16-packet netem queue |
| Sampling / diagnostics | Approximately 250 ms sequential status/state/process observations; five-second metric logs |
| Recovery | Remove qdisc; existing TCP-path waits; queues and stream requests drain within 30 seconds; unchanged 5/5 checks |
| Watchdogs | Existing 450-second namespace budget; external watchdog 480 seconds; no wait scaling |
| Budgets | All per-case JSON below 8 MiB; combined node logs below 2 MiB |

Single-round smoke tests validate each profile before the four captures. They
do not count as sustained repetitions. Record executable hash, profile, actual
transmissions/replies and TUN byte/packet deltas, not just nominal offered load.

The byte profile has capacity for three 1028-byte requests before the byte limit
binds, well below its 16-packet limit. Per-drop runtime counters do not distinguish
which limit fired; report this limitation alongside the queue-admission tests.

This is pressure/recovery evidence, not the separate 300-second fixed-load S3
workload. Two-worker results are not matched CPU comparisons with the earlier
default-worker idle captures. RSS changes still require allocation attribution.

#### Pressure Harness Validation

The final fixture passes 50 namespace unit tests, required Clippy groups and
cached rustfmt checks. Advisory Clippy warnings remain. Only test code and
developer documentation changed; no production runtime was modified.

| One-Round Smoke | Packets Profile | Bytes Profile |
| --- | ---: | ---: |
| Test duration, seconds | 39.13 | 59.94 |
| Generator transmitted / received | 1975 / 10 | 1972 / 10 |
| A peak queued packets / bytes | 4 / 4112 | 3 / 3084 |
| Final queues / stream requests | Empty / zero | Empty / zero |
| Recovery traffic | 5/5 both directions | 5/5 both directions |
| Combined node logs, bytes | 271248 | 347702 |

Both smoke tests pass, retain the same daemon identities and leave no matching
fixture processes. Replay scripts preserve the limit profile and round count.
No builds overlapped either smoke. The subsequent campaign stopped on a
round-two byte-profile failure; see [pressure investigation](sustained-pressure-results.md).

- Executable SHA-256: `ab3f2fe482dfb48d2915074b04c81a4e037b9a6c02f94e590af07292f68de666`.
- Packet artifact suffix: `1.35cc20df7fd8c5a8`; byte suffix: `1.3e33a24e9bf968d6`.
- Artifact prefix: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-`.
- Smoke logs: `/tmp/p2p-vpn-sustained-pressure-{packets,bytes}-smoke.log`.
- Final checks: `/tmp/p2p-vpn-sustained-pressure-profile-{admission-tests,clippy}.log`.
- Initial JSON-macro compile error was corrected before runtime tests; its log remains at `/tmp/p2p-vpn-sustained-pressure-profile-tests.log`.

#### Corrected S4 Campaign

The deterministic path-demotion regression exposed a stale readiness check.
All four captures were restarted after its correction; historical captures remain
separate evidence and do not substitute for corrected-fixture repetitions.

| Setting | Corrected Campaign |
| --- | --- |
| Sequence | Packets, bytes, bytes, packets; five rounds each, fresh process pairs |
| Initiator | Unset override; preserve normal random peer-ID ordering |
| Recovery readiness | Current healthy TCP paths and drained packet/byte queues plus stream requests on both nodes |
| Deadline / delivery | Existing 30-second drain deadline and preceding path waits; unchanged 5/5 pings both ways |
| Other controls | Frozen S4 traffic, two Tokio workers, shaping, logging, watchdogs and evidence budgets above |
| Failure handling | Retain failed evidence and stop the campaign for causal investigation |

All four corrected captures passed on fixture `82980db4`; see the
[full campaign summary](sustained-pressure-results.md#full-campaign-summary).
The 257-request aggregate peak requires the documented probe-owner follow-up;
S4 measurement completion does not close resource-bound or allocation review.

### Shared Limits

| Resource | Limit |
| --- | --- |
| All `/tmp/p2p-vpn-*` | Below 10 GiB; check before each build/provisioning phase |
| Capture output | 8 MiB compact observations plus 2 MiB bounded diagnostic logs |
| Case watchdog | At most 900 seconds; retain partial evidence on expiry |
| Builds | Cached dependencies, offline where possible, at most two Cargo jobs |
| Downloads | At most 10 Mbps; no uncontrolled fetches or large uncached builds |
| Observations | No concurrent review builds, manual rescue or relaxed functional deadlines |
| Devices / WAN | Fresh authorization for physical devices, hosts, deployments or public campaigns |

Preserve evidence and user data. Remove only owned disposable state after process
termination; ask when cleanup needs a user decision. Use atomic Conventional
Commits with Jujutsu and push verified work to `main`.

Physical energy/thermal measurements, release packaging and final cross-platform
acceptance remain separate. Emulator CPU/wakeup proxies cannot certify battery use.
