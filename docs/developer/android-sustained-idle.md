# Android Sustained Idle

## Frozen Capture

This manifest is written before capture. It covers S7's idle phase only; fixed-rate
load, independent network transitions, wakeup attribution and repeat captures
remain required by the sustained review.

| Setting | Value |
| --- | --- |
| Source baseline | `5b04a0be`, plus the idle scenario wiring recorded with the result |
| Scenario | `multi-network-resource-idle` |
| Topology | Owned API 35 x86_64 emulator, two private network fixtures, no public route |
| APK | `ea03c922be28faf3043c540c69dd898ce1b5f7206528ffbafc55c6f5b8527f46` |
| Fixture | `4b1bcd69a410f9a830e813d9457aa0a55a8a507d762c208aad42a1092ab8f8fa` |
| Admission | Bidirectional IPv4/IPv6 traffic and attributed counters on both networks |
| Background state | HOME/SLEEP, 30-second warmup, settled noninteractive power state |
| Window | 300 seconds; no deliberately generated traffic |
| Collection | 300 process samples and 60 native/diagnostic samples |
| Checks | Existing identity, missing-data, cadence and observation-duration bounds unchanged |
| Endpoint elapsed bound | 300 to 310 seconds; record actual elapsed time |
| Watchdogs | 600 seconds plus 15-second grace inside; 640 seconds plus 20-second grace outside |
| Storage | `/tmp/p2p-vpn-*` below 10 GiB; per-run growth cap 1,258,291,200 bytes |
| Build isolation | Cached debug binaries; no builds during observation |

Execute with `/tmp/p2p-vpn-a` as `TMPDIR`, the cached artifacts from the Android
review, and `scripts/android-resource-isolation.sh`. Preserve failed runs and
partial observations; do not extend the deadline or manually rescue connectivity.

## Interpretation

- Report app CPU and emulator CPU separately using each system's uptime and clock ticks.
- Retain the [measured collector cost](android-resource-controls.md); do not subtract it from observations.
- Audit queues, connection counts, retry/pending-work counters and resource trends by network.
- RSS/PSS alone cannot establish allocation ownership or absence of leaks.
- A passing idle capture does not prove sustained load, network isolation or physical battery behavior.

## First Completed Capture

Attempt 2 passes all window checks. [Portable results](android-sustained-idle-results.json)
contain process endpoints, sampled owner metrics and raw-file hashes. The full raw
directory is `/tmp/p2p-vpn-android-sustained-idle-2` and occupies 3,748 KiB.

| Observation | Result |
| --- | --- |
| App endpoint duration / CPU | 303.18 seconds / 2.170% of one core |
| Emulator endpoint duration / CPU | 303.20 seconds / 7.487% of one core |
| Process samples | 300; maximum gap 1.02 seconds; maximum observation time 10 ms |
| Runtime samples | 60; maximum gap 5.020 seconds; maximum observation time 80 ms |
| App RSS | 207496 to 208692 KiB; sampled range 206328 to 209304 KiB |
| App PSS | 84968 to 84830 KiB; sampled range 84559 to 86887 KiB |
| Threads / descriptors | 30 throughout; descriptors observed at 121, 123 and 125 |
| Overlay paths | Two connected peers; one QUIC stream and one TCP stream throughout runtime samples |
| Infrastructure count | Zero or two private routing peers; not public-IPFS reachability |
| Queues | Zero queued packets in every diagnostic sample |
| Pending owner gauges | Selected queue/pending/active/attempt gauges remain zero; historical handler peaks remain unchanged |
| Background bounds | Primary and pairing `background_bounded_jobs` metrics remain two per network |

The 300-sample loop includes collection overhead, so endpoint elapsed time exceeds
300 seconds within the frozen 310-second bound. CPU uses actual elapsed time.
Do not interpret this run as exactly 300 seconds of zero-overhead observation.

Automatic transport selection produced a mixed QUIC/TCP topology. The earlier
collector controls used two QUIC streams, so direct CPU comparisons are not
transport-matched. Future idle/load comparisons must retain their actual topology.

### Evidence and Cleanup

| Artifact | SHA-256 |
| --- | --- |
| Passing evidence | `2f19d7f0f8cab0b6bedf4001b314fc59c70c17973f6a3afdf6823911b68bd6b9` |
| Captured window helper | `37ef552d382fd987db2d5d965d162da5726ce220fc536b70544320d2a3e65719` |
| Captured harness | `251c52bb0536f2ebe24bf389fe1e7a048cbdc92d2599e74a38d1ab3e0937ac60` |

All six harness cleanup checks pass, and no matching emulator/fixture remains.
The owned temporary root retains 76 KiB of tool state. Storage before attempts
was 9,128,176 and 9,129,708 KiB; no builds ran during observation.

## Unresolved Setup Failure

Attempt 1 failed legacy-profile migration before measurement. Its final app
diagnostic was unavailable, so `diagnostic_report_redacted` is false; the other
five cleanup flags pass. This is not a completed idle capture.

- Raw directory: `/tmp/p2p-vpn-android-sustained-idle-1`.
- Evidence SHA-256: `bf6592a01a715ede98f0d973174febc1d043bca83274aa86ee40a5cf9ce6fad5`.
- Intermediate migration status was removed during cleanup, preventing causal attribution.
- The harness now preserves a selected before/after status and at most 200 filtered Android error-log records on failure.
- The retry changes failure diagnostics only; migration assertions, runtime behavior and deadlines are unchanged.

The retry passes migration but does not explain the earlier failure. Keep this
setup reliability gap open; do not call it fixed or reopen lifecycle behavior
without stronger causal evidence from a recurrence.

## Remaining Work

- Repeat the sustained capture and complete a transport-comparable load phase.
- Measure independent network disable/enable cycles and healthy sibling traffic.
- Add scheduling/wakeup attribution beyond process-leader context switches.
- Complete full-runtime allocation attribution and the final requirement audit.

ShellCheck, helper formatting, synthetic duration checks and all raw-file hashes
pass. The Nix structure derivation evaluates offline; the full legacy shell matrix
was not rerun. No production Rust/Android source or build changed for this capture.
