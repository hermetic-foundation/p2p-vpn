# Graceful Pressure Results

## Scope

Two five-round pressure captures at `575ef4ac` pass the frozen
[graceful review](graceful-allocation-review.md) gates. Each uses fresh daemons;
their identities and processes remain unchanged within the capture.

Unlike the earlier [pressure matrix](pressure-allocation-results.md), these runs
request shutdown over the control socket and require normal child exits, removed
TUN interfaces and removed control sockets. Neither uses a debugger or runtime rescue.

## Workload

| Setting | Value |
| --- | --- |
| Transport / topology | Direct TCP; two isolated namespace daemons; no Internet route |
| Build | Cached unoptimized `allocation-review` binary; two Tokio workers |
| Packet profile | Four packets / 8192 bytes; five rounds |
| Byte profile | Sixteen packets / 4096 bytes; five rounds |
| Pressure | Existing S4 1000-byte ping payload, 5 ms interval, shaped link |
| Watchdogs | Original 450-second inner / 480-second outer limits |
| Shutdown | Two-second acknowledgement; five-second child exit per role |
| Observation | Five-second allocation cadence; 100 ms post-child settling |
| Storage | Below 10 GiB before both runs; 9,332,972 KiB after collection |

No builds overlapped either measurement. Initial direct admission and allocator
calibration are recorded in the [fix report](graceful-allocation-review.md).

## Outcomes

| Profile | Duration | Pressure Sent / Replies | Recovery | Graceful Exit |
| --- | ---: | ---: | --- | --- |
| Packets | 163.27 s | 9732 / 61 | Ten 5/5 batches | Both pass |
| Bytes | 160.40 s | 9735 / 46 | Ten 5/5 batches | Both pass |

Pressure intentionally causes loss. Across both captures, all twenty recovery
batches pass: 100 requests and 100 replies. Cumulative queue drops reach 6561
and 6807 respectively; those are pressure outcomes, not failed recovery pings.

- Every round ends with zero queued bytes, packets and stream-in-flight owners.
- Both peers have a healthy TCP path and no unsupported-path condition after recovery.
- All recovery snapshots show six threads and 16 descriptors per daemon.
- The earlier packet baseline showed six threads and 13 descriptors.
- The three added descriptors correspond to two readiness polls and one cancellation wakeup.

The descriptor comparison describes the implementation cost, not a throughput
or CPU-equivalence claim. The production fix adds no recurring idle polling timer.

## Allocation Checkpoints

All values below are live requested Rust bytes. Peaks are sampled, not guaranteed
whole-run maxima. The final periodic sample is not an exact round boundary.

| Profile / Role | Sampled Peak | Last Periodic | Runner Returned | Child Settled |
| --- | ---: | ---: | ---: | ---: |
| Packets A | 5,580,455 | 4,879,137 | 176,332 | 81,322 |
| Packets B | 5,232,996 | 4,720,749 | 144,139 | 81,322 |
| Bytes A | 5,390,546 | 4,554,551 | 192,852 | 81,322 |
| Bytes B | 5,014,026 | 4,606,233 | 144,139 | 81,322 |

| Checkpoint | Bytes | Blocks |
| --- | ---: | ---: |
| Before child, all roles | 21,615 | 423 |
| Child returned, all roles | 81,322 | 507 |
| Child settled, all roles | 81,322 | 507 |
| Settled minus before | 59,707 | 84 |

All four daemons converge to the same post-child values, with no allocation change
during the final 100 ms. Packet captures have 34 runner samples per role; byte
captures have 33. The maximum pre-return sampling gap is 5003 ms.

## Evidence

[Structured results](graceful-pressure-results.json) include lifecycle records,
sample summaries, per-round process/queue counters, raw recovery summaries and
SHA-256 manifests for 53 artifacts plus the outer log per capture.

| Profile | Evidence Directory | Outer Log |
| --- | --- | --- |
| Packets | `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-1.26a255b2e93e345e` | `/tmp/p2p-vpn-graceful-packets-1.log` |
| Bytes | `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-1.6adfcbe1758222cc` | `/tmp/p2p-vpn-graceful-bytes-1.log` |

Packet JSON totals 3,479,724 bytes and node logs total 1,296,655 bytes. Byte JSON
totals 3,437,569 bytes and node logs total 1,223,673 bytes. Both remain below the
original 8 MiB JSON / 2 MiB combined node-log caps.

## Reproduction

Use `bytes` for the second profile and a distinct output log. The binary hash is
`6516ee1bcb7b227fac264fabfd6e7a47a86d0f9fdba93e15819bea32953fa6ab`.

```sh
env -u P2P_VPN_TUN_E2E_PRESSURE_INITIATOR \
  -u P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS \
  -u P2P_VPN_TUN_E2E_WAIT_SCALE \
  P2P_VPN_REVIEW_GRACEFUL_SHUTDOWN=1 \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  P2P_VPN_TUN_E2E_PRESSURE_LIMIT=packets \
  P2P_VPN_TUN_E2E_PRESSURE_ROUNDS=5 \
  timeout --signal=TERM --kill-after=10s 480 \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-b64312792e9e0507 \
  tun_namespace_recovers_after_tcp_queue_pressure \
  --ignored --exact --nocapture --test-threads=1
```

## Remaining Work

The observed teardown releases most runtime allocations, including the formerly
blocked TUN reader. This does not identify the remaining 59,707 bytes / 84 blocks
or establish that all live reconnect growth is bounded.

Next evidence must cover graceful teardown after sustained reconnect churn and
attribute the residual owners. Identical totals and flat short intervals alone
are insufficient for final resource-review acceptance.
