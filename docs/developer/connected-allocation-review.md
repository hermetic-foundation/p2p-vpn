# Connected Allocation Review

## Status

Test-only connected allocation instrumentation passes calibration, a connected
one-cycle recovery smoke and the first full ten-cycle capture. The independent
repeat and retained-allocation attribution remain pending.

It extends the existing namespace workload rather than substituting constructors
or synthetic packet devices for connected peers.

See [full capture results](connected-allocation-results.md) for the measured
growth and remaining evidence gaps. Passing recovery does not prove bounded memory.

## Instrumentation

| Property | Value |
| --- | --- |
| Activation | Namespace test built with existing `allocation-review` feature |
| Normal binaries | No global allocator or allocation sampler added |
| Allocator | Existing vendored `stats_alloc` wrapping `System` |
| Daemon sampling | Initial snapshot, then every five seconds; skip missed ticks |
| Owner | Sampler wraps the daemon future; no extra thread or detached task |
| Output | Structured `allocation_sample` JSON in each node log |
| Time correlation | Unix milliseconds plus monotonic elapsed time; phase reports include start/end Unix milliseconds |
| Accounting | Requested bytes/blocks; reallocation already included, never counted twice |

Serialization streams directly to stderr after the snapshot. Observer activity
remains part of subsequent cumulative counters. Concurrent counter loads are
not transactional; do not interpret individual spikes as exact live heap bounds.

The initial snapshot follows host and packet-endpoint construction. It is not a
pre-startup allocation baseline. Samples measure each daemon process separately.

## Frozen First Workload

Use the [S5 churn manifest](lifecycle-churn-review.md) unchanged, except for the
explicit feature-enabled executable and allocation sampling described above.

| Control | Value |
| --- | --- |
| Topology | Existing isolated two-node direct-UDP namespaces; no Internet route |
| Repetition | Two ten-cycle captures using one executable hash |
| Gates | Existing path demotion, autonomous recovery, strict traffic and process-identity checks |
| Timing | Existing 30-second outages, 40-second recovery observations, 60-second final settle |
| Watchdogs | Existing 890-second internal / 900-second external limits |
| Budgets | Existing 8 MiB combined JSON / 2 MiB combined node logs, including allocation rows |
| Smoke | Existing one-cycle profile, 240/250-second watchdogs; not sustained evidence |

Do not increase watchdogs or log caps if instrumentation exceeds a budget.
Preserve failed evidence and investigate. Complete all builds and calibration
before measurements; no build may overlap a capture.

## Evidence Gates

1. Calibrate the integration executable separately; library-test calibration is insufficient.
2. Check structured serialization and wrapped-future completion.
3. Require allocation rows for both daemons throughout each phase.
4. Reject missing cadence coverage or wall-clock/monotonic disagreement.
5. Compare post-recovery and settled requested allocations against RSS, queue and connection owners.
6. Investigate recurring growth; retain initialization and negative deltas.

Before full captures, freeze cadence acceptance at a maximum 5,500 ms inter-row
gap and phase-boundary gap. Require at least duration/5 rows per daemon per phase.
Reject clock disagreement above 100 ms, including cumulative drift from the first row.

Compare phase wall-clock duration against its monotonic collector duration too.
These are evidence-audit gates; the normal S5 traffic test does not assert them
automatically. Missing or malformed allocation rows cannot count as a complete capture.

The namespace orchestrator normally terminates daemon processes after its checks.
Absent `returned` samples are therefore not evidence of graceful runtime teardown;
the separate runtime-drop campaign supplies that narrower evidence.

These instrumented CPU results are not directly comparable with uninstrumented
captures. Whole-runtime attribution, multi-network isolation and Android resource
acceptance remain open until their respective evidence is complete.

## Smoke Results

| Gate | Result |
| --- | --- |
| Total duration | 213.94 seconds; original watchdogs unchanged |
| Recovery | 3.257 seconds; strict 5/5 traffic both directions |
| Final traffic | Strict 5/5 both directions; process identities unchanged |
| Allocation rows | 43 per daemon; PIDs match OS reports |
| Outage / recovery / settle rows | 6 / 8 / 12 per daemon |
| Runtime counter series | Complete in all three phases |
| Maximum allocation row gap | A: 5,004 ms; B: 5,002 ms |
| Maximum cumulative clock disagreement | 1 ms per daemon |
| Phase clock agreement | Wall and monotonic durations differ by less than 1 ms |
| Combined JSON / node logs | 1,034,658 / 416,338 bytes; within original caps |
| Cleanup | Runner exited zero; no matching namespace test process remained |

During final settling, sampled live bytes range from 2,536,598 to 2,538,128 on A,
and 2,535,406 to 2,535,438 on B. Both return to their first settling value by the
last sample. This one-cycle observation does not establish sustained boundedness.

[Portable smoke evidence](connected-allocation-smoke.json) retains every allocation
row, phase boundary, cadence summary and artifact hash.
The capture explicitly has `proof_eligible=false`.

## Validation

| Gate | Result |
| --- | --- |
| Integration allocator calibration | Pass on the captured executable |
| Feature-enabled namespace units | 60 passed; 27 opt-in tests ignored |
| Offline locked workspace | 1507 passed; 40 opt-in tests ignored |
| Required Clippy | Feature-enabled workspace/all targets pass; advisory warnings remain |
| Formatting | Changed Rust files pass cached rustfmt |
| Cached Nix source check | Evaluated script passes outside sandbox; Linux test source matches |
| Android | Native input intentionally excludes namespace integration tests; no shared production change or cross-build |
| Storage before builds | 8,745,104 KiB under `/tmp/p2p-vpn-*`, below 10 GiB |

Validation logs use `/tmp/p2p-vpn-connected-allocation-` with suffixes
`calibration.log`, `units.log`, `workspace.log`, `clippy.log`,
`source-check.log` and `smoke.log`. No production allocator override was added.

Executable SHA-256:
`7bd693606049cc7df7de8d5294ab91fe2bab8dca81fdbfdd4b3c77c747a5ad04`.
Run the unchanged S5 command with this feature-enabled executable for both full captures.
