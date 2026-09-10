# Pressure Allocation Results

## Status

Both admission smokes and all four five-round captures passed the frozen
[pressure allocation matrix](pressure-allocation-review.md). This adds requested
allocation evidence; whole-daemon ownership and graceful release remain separate.

The [portable results](pressure-allocation-results.json) contain both daemon
time series, round-end state, traffic counts and artifact hashes. Failed attempts
from the earlier uninstrumented campaign remain documented in its own report.

## Campaign

| Capture | Profile | Rounds | Duration, seconds | Recovery batches |
| --- | --- | ---: | ---: | ---: |
| Packets 1 | Four packets / 8192 bytes | 5 | 161.06 | 10 |
| Bytes 1 | Sixteen packets / 4096 bytes | 5 | 160.51 | 10 |
| Bytes 2 | Sixteen packets / 4096 bytes | 5 | 162.70 | 10 |
| Packets 2 | Four packets / 8192 bytes | 5 | 160.60 | 10 |

- Every recovery batch received 5/5 packets: 200 replies across the four captures.
- Forced pressure transmitted 38,924 requests and received 164 replies; drops were expected and required.
- All round-end snapshots had empty packet/byte queues, no in-flight stream requests and healthy TCP paths.
- Original process start identities survived each capture's five rounds.
- The two admission smokes passed in 39.97 and 40.70 seconds; they are not campaign repetitions.

## Sampled Heap

Values are requested live bytes, not RSS. Peaks are sampled peaks; final values
are the last periodic snapshot before fixture termination, not exact post-round
checkpoints or measurements after graceful shutdown.

| Capture | A sampled peak | A final sample | B sampled peak | B final sample |
| --- | ---: | ---: | ---: | ---: |
| Packets 1 | 5,672,929 | 4,555,044 | 4,968,416 | 4,599,577 |
| Bytes 1 | 5,419,258 | 4,553,855 | 5,001,616 | 4,606,847 |
| Bytes 2 | 5,386,131 | 4,620,722 | 5,033,422 | 4,601,761 |
| Packets 2 | 5,559,150 | 4,555,064 | 4,975,460 | 4,602,036 |

Every endpoint is below its capture's sampled peak. Packet-repeat A endpoints
differ by 20 bytes, while byte-repeat A endpoints differ by 66,867 bytes.
That variation remains visible; do not round it away or assign it to a guessed buffer.

These trajectories show allocation contraction after sampled peaks, not a proof
of leak freedom. No exact round-boundary allocation timestamp was added; the
portable rows preserve their own wall and monotonic clocks.

## Evidence Gates

| Gate | Result |
| --- | --- |
| Executable hash | Matched frozen calibrated artifact before and after campaign |
| Allocation gap | At most 5,005 ms across all six captures; limit 5,500 ms |
| Clock disagreement | At most 1 ms; limit 100 ms |
| Largest case JSON total | 3,477,364 bytes; below 8 MiB |
| Largest combined node logs | 1,241,692 bytes; below 2 MiB |
| Builds, debugger, physical hosts, public routes | None during captures |
| Cleanup | All command sessions terminal; no matching fixture process remained |

No watchdog, queue bound, query timeout or traffic assertion changed. All raw
logs, reports and reproduction metadata remain local at the recorded paths.
No production source changed, so Rust/Android rebuild gates were not rerun.

## Remaining Attribution

The [queue-owner diagnostic](queue-allocation-review.md) establishes bounded
container capacity and payload release in isolation. The new daemon captures
provide the missing pressure trajectories but do not identify every retained owner.

Next, distinguish initialized transport/handler capacity from pressure-dependent
retention, and verify any proposed explanation with allocation-site or teardown
evidence. Reconnect-wide ownership and the final resource audit also remain open.
