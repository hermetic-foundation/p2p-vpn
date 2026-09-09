# Sustained Resource Review

## Status

Active bounded review opened at `00b1b58a` on 2026-09-09.
This is a prospective measurement plan, not a results report.
No physical device or deployed host is authorized for this work.

## Checklist

- [x] Locate prior measurements and identify reuse limits.
- [x] Define capture windows, budgets and decision rules.
- [ ] Audit collectors and freeze workload manifests before capture.
- [ ] Measure connected idle, unavailable peers and collector overhead.
- [ ] Measure matched sustained traffic and pressure/recovery cycles.
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
| [Queue pressure](queue-pressure-review.md) | Three corrected TCP recovery cycles | Cause of increasing RSS and longer-run retention |
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

1. Audit existing idle, process, queue-pressure and resource collectors.
2. Inventory cached binaries and storage; build only missing affected targets.
3. Run S1/S2 and overhead controls; finalize S3/S5 fixture manifests.
4. Run load, pressure and allocation attribution; reproduce defects before fixes.
5. Run cached Android multi-network/background measurements.
6. Validate fixes and publish quantitative results with precise exclusions.

## Safety and Budgets

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
