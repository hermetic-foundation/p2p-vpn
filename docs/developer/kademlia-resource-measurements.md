# Before/After Resource Measurements

## Status

Phase 3 is active. No acceptance measurements have been collected.
Subject selection and sampler development are complete enough for harness work;
the full numeric protocol must be frozen before acceptance runs start.

See the [workstream plan](kademlia-resource-plan.md).
Phases 1 and 2 remain complete; this phase does not establish production readiness.

## Subjects

| Subject | Revision | Rationale |
| --- | --- | --- |
| Baseline | `5ecb01ea` | Immediately precedes the first workstream fix, `14782e7d`, for recovery-query backoff |
| Current | `3b503ad2` | Completed aggregate bounds and sustained recovery/settling acceptance |
| Measurement harness | Not yet frozen | One external orchestrator and infrastructure helper for both subjects |

- Build each subject from its own unchanged runtime, manifest, and lockfile.
- Record full revisions, binary hashes, compiler, profile, environment, and harness hashes before runs.
- Use matching optimized build settings; inspect cache and disk budgets before building.
- Preserve baseline registry Kademlia and current vendored Kademlia. Their implementation difference is part of the comparison.
- Separate subject artifacts to avoid stale executable reuse documented in the [earlier comparison](idle-resource-comparison.md#reproduction-notes).

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

Before accepting data, record numeric warmup, sample cadence, window durations,
traffic parameters, fault schedule, watchdogs, ordering, and aggregation rules.
Derive durations from production timers and workload needs, not observed winners.

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

```sh
cargo test --offline --locked --test resource_measurement -- --test-threads=2
```

Four sampler tests passed with cached Nix Rust tooling. Coverage includes inode
attribution, unowned TCP rows, malformed/missing fields, unit validation, process
replacement, CPU overflow/reset, and a live process listener.
The live check also verifies duplicate socket descriptors do not create extra
socket-inode or TCP-connection counts.

Required Clippy groups, formatting, whitespace, and cached Nix test-source
integration passed. The full workspace and namespace acceptance suites were not
rerun for this independent sampler-only checkpoint.

This is tooling validation, not baseline/current acceptance or a performance
result. No daemon implementation or Phase 2 acceptance fixture was changed.

## Resource Limits

- Initial retained task storage: 5.13 GiB; total limit: 10 GiB.
- At most two Cargo build jobs across the task; downloads capped at 10 Mbps.
- No builds or other task workloads during comparative observations.
- Freeze per-run log/sample caps and retention policy before measurements.
- Do not remove prior acceptance evidence merely to create build space.
