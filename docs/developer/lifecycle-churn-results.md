# Lifecycle Churn Results

## Status

Both corrected S5 captures pass the [frozen manifest](lifecycle-churn-review.md).
This closes paired churn measurement, not the overall resource review.
RSS growth triggers further allocation attribution.

## Provenance

| Setting | Value |
| --- | --- |
| Runtime / fixture | `7b59625f` / `d26d1c38` |
| Executable | Debug namespace integration test; two Tokio workers, six daemon threads |
| SHA-256 | `3f91216df32982e13bea93844c978ee0d461832c390941fc1bf175867c6df4c6` |
| Topology | Two isolated namespaces, one static overlay peer each, no Internet route |
| Infrastructure | Five default public bootstrap candidates; failures are local, not WAN observations |
| Sequence | Thirty-second warmup; ten 30-second outages and 40-second recovery observations; 60-second settle |
| Sampling | OS every second, runtime every five seconds, diagnostics every ten seconds |
| Watchdogs | Unchanged internal 890 seconds / external 900 seconds |
| Intervention | No concurrent review build, manual recovery, daemon restart or deadline extension |

Commands are in the [manifest](lifecycle-churn-review.md#commands).
The [portable JSON](lifecycle-churn-samples.json) records raw paths, all 70
artifact hashes, phase summaries, recovery checks and all 332 counter ranges.

## Capture Gates

| Gate | Run 1 | Run 2 |
| --- | ---: | ---: |
| Test exit / elapsed seconds | 0 / 855.21 | 0 / 880.46 |
| Completed cycles | 10 | 10 |
| Per-cycle traffic | 5/5 both directions, every cycle | 5/5 both directions, every cycle |
| Post-settle traffic | 5/5 both directions | 5/5 both directions |
| Resource phases / complete runtime series | 21 / 21 | 21 / 21 |
| OS rows per daemon | 781 | 781 |
| Runtime snapshots per daemon | 152 | 152 |
| Missing or invalid counter values / status errors | 0 / 0 | 0 / 0 |
| Maximum status query, milliseconds | 3.159 | 3.589 |
| Maximum scheduled status lateness, milliseconds | 8.880 | 8.606 |
| Combined JSON bytes, limit 8 MiB | 6,547,308 | 6,538,477 |
| Combined node log bytes, limit 2 MiB | 1,597,689 | 1,628,229 |

Both summaries report complete, proof-eligible results and unchanged PID/start
identities. Both runners terminated; no matching namespace test process remained.
Each node logged ten failed-packet connection retirements in each run.

## Recovery and CPU

Recovery time is from link restoration to successful probes in both directions.
Strict traffic checks follow; their time is not included in the recovery column.

| Cycle | Run 1, Seconds | Run 2, Seconds |
| --- | ---: | ---: |
| 1 | 22.344 | 3.258 |
| 2 | 15.679 | 13.174 |
| 3 | 0.352 | 15.728 |
| 4 | 18.785 | 0.353 |
| 5 | 2.455 | 21.889 |
| 6 | 21.890 | 3.158 |
| 7 | 18.785 | 12.872 |
| 8 | 15.078 | 0.402 |
| 9 | 0.352 | 15.679 |
| 10 | 11.971 | 0.904 |

CPU is percent of one core, calculated from ticks over actual observed time.
Ranges below describe phase averages, not instantaneous CPU peaks.

| Run / Node | Phase CPU Range | Final Settle CPU | First Outage / Settle-End RSS, KiB |
| --- | ---: | ---: | ---: |
| 1 / A | 0.1333-0.4001% | 0.1833% | 35,664 / 35,916 |
| 1 / B | 0.1333-0.3501% | 0.1667% | 35,876 / 36,168 |
| 2 / A | 0.1333-0.4001% | 0.1667% | 35,832 / 36,112 |
| 2 / B | 0.1333-0.3751% | 0.1500% | 35,604 / 35,900 |

Recovery checkpoints always have A: 21 descriptors / 14 sockets, and
B: 19 descriptors / 12 sockets. Each daemon retains six threads.
No phase average reaches the parent review's 0.5-percentage-point CPU trigger.

## Quiescence and Retry Scope

Final status on both nodes in both runs reports:

- One healthy direct UDP path and empty packet/byte queues.
- Zero pending connection attempts, connection-retirement markers and packet hello/responders.
- Zero packet QUIC connection tasks/owners, Kademlia pending RPC requests/bytes and retained queries.
- Zero application recovery and maintenance queries.

One old UDP session remains in its intentional retirement overlap.
That is not a pending TCP connection retirement or a claim that every cache
and scheduled maintenance timer has disappeared.

| Periodic Counter | Run 1 A / B | Run 2 A / B |
| --- | ---: | ---: |
| Outgoing connection errors, first to last | 10-107 / 5-114 | 5-101 / 5-118 |
| Redial attempts, first to last | 0-3 / 0-0 | 0-1 / 0-0 |
| Expired queue packets, final | 41 / 38 | 27 / 26 |
| Expired queue bytes, final | 3444 / 3192 | 2268 / 2184 |

Public bootstrap failures contribute infrastructure activity.
`redial_attempts` is not a total dial counter; do not interpret zero as no dials.
Periodic extrema can miss transient owners between snapshots.

## Open Attribution

| Run / Node | Final Three Recovery RSS Checkpoints, KiB |
| --- | --- |
| 1 / A | 35,884; 35,900; 35,916 |
| 1 / B | 36,160; 36,164; 36,168 |
| 2 / A | 36,088; 36,108; 36,112 |
| 2 / B | 35,876; 35,892; 35,896 |

All four series meet the declared RSS investigation trigger.
Stable descriptors and drained queues do not attribute allocator pages,
transport buffers or whole-runtime retained allocations.

## Limits and Validation

- Local link restoration retains endpoint addresses; it is not WAN address migration.
- Two corrected passes exercise retirement, but do not establish a causal latency improvement.
- Earlier S1/S2 used twenty threads; do not treat their CPU results as a matched comparison.
- Multi-network isolation, Android background measurements and final acceptance remain open.

The [retirement correction](churn-connection-retirement.md) records workspace,
Clippy, formatting, cached Nix source parity and Android-native validation.
This publication changes documentation only; no builds or code tests were repeated.
Raw hashes, JSON structure, sample completeness, traffic and byte budgets were rechecked.
