# Graceful Reconnect Results

## Status

The first of two frozen ten-cycle captures passes. The independent repeat is
pending. This is stronger teardown evidence, not final resource acceptance or
an attribution of every allocation retained while the runtime is connected.

The [manifest](graceful-churn-review.md) preserves the original outage, recovery,
settling, traffic, process-identity and watchdog gates. No build or debugger ran
during the capture; no daemon was restarted or manually repaired.

## First Capture

| Property | Result |
| --- | --- |
| Production revision | `575ef4ac` |
| Duration | 852.82 seconds; original 890/900-second watchdogs |
| Reconnect cycles | All ten pass |
| Recovery time range | 0.503-21.294 seconds; original 60-second recovery deadline |
| Strict traffic | Twenty recovery batches plus two final batches; 110/110 replies |
| Process identity | Both original PID/start identities remain unchanged |
| Graceful shutdown | Both acknowledge, exit normally, release TUN interfaces and remove control sockets |
| Cleanup | No matching fixture executable remains running |
| Combined JSON | 6,544,833 bytes; below 8 MiB |
| Combined node logs | 1,706,606 bytes; below 2 MiB, including teardown samples |
| Temporary storage after audit | 9,341,132 KiB; below 10 GiB |

## Sampling Gates

| Gate | Node A | Node B |
| --- | ---: | ---: |
| Runner allocation rows | 172 | 172 |
| Maximum inter-row gap | 5004 ms | 5004 ms |
| Maximum phase-boundary gap | 4974 ms | 4945 ms |
| Maximum clock drift | 1 ms | 1 ms |
| Settling rows | 12 | 12 |
| Final threads | 6 | 6 |
| Initial / final descriptors | 24 / 24 | 22 / 22 |

All 21 observation phases have complete runtime-counter series and the required
allocation coverage. Every 30-second outage has six allocation rows per role;
every 40-second recovery has eight or nine; the 60-second settling phase has twelve.

Phase wall-clock duration agrees with the final monotonic observation within
the frozen 100 ms limit. Every phase records the same executable hash.
OS descriptors vary during outages; equal endpoints are not constant-count claims.

## Allocation Results

These values measure requested Rust allocations, not resident memory or the
complete native heap. Phase samples cannot establish exact peak allocation use.

| Checkpoint | Node A Bytes / Blocks | Node B Bytes / Blocks |
| --- | ---: | ---: |
| Before child | 21,619 / 423 | 21,619 / 423 |
| First recovery, last sample | 2,537,317 / 1550 | 2,524,968 / 1527 |
| Final settling, every sample | 2,558,207 / 1565 | 2,549,731 / 1537 |
| Runner returned | 174,113 / 651 | 127,196 / 594 |
| Child returned | 62,420 / 513 | 62,420 / 513 |
| Child settled | 62,420 / 513 | 62,420 / 513 |
| Settled minus before | 40,801 / 90 | 40,801 / 90 |

The final minute's sampled heap is flat. Net growth from the first recovery's
last sample to final settling is 20,890 bytes / 15 blocks on A and 24,763 bytes /
10 blocks on B. Empty queues do not explain or excuse those retained allocations.

After real shutdown, both roles match the short direct-UDP admission's net
residual of 40,801 bytes / 90 blocks. No additional net post-child retention is
observed after these ten cycles; this does not identify each allocation owner.

The [TCP pressure residual](graceful-pressure-results.md) is different:
59,707 bytes / 84 blocks. Do not combine these transport/workload baselines or
assume that equal totals prove an absence of offsetting allocation changes.

## Artifacts

[Structured evidence](graceful-churn-first.json) includes every allocation row,
phase coverage summaries, lifecycle checkpoints, recovery outputs, process
observations and hashes for 34 capture artifacts plus the outer log.

| Artifact | Path |
| --- | --- |
| Capture | `/tmp/p2p-vpn-tun_namespace_measures_lifecycle_churn_resources-1.7306b1a288be1591` |
| Outer log | `/tmp/p2p-vpn-graceful-churn-1.log` |
| Executable | `/tmp/p2p-vpn-review-target/debug/deps/tun_namespace-b64312792e9e0507` |

Executable SHA-256:
`6516ee1bcb7b227fac264fabfd6e7a47a86d0f9fdba93e15819bea32953fa6ab`.
Commands and frozen budgets are in the [manifest](graceful-churn-review.md#command).

## Remaining Work

1. Run and audit the independent ten-cycle repetition with the same executable.
2. Attribute live reconnect retention and the post-child residual owners.
3. Reconcile all resource-review requirements against final evidence.

The Linux reader defect is corrected and graceful teardown is demonstrated in
this capture. Physical battery evidence, public-network campaigns and broader
production certification remain outside this bounded result.
