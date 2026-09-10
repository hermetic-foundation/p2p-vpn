# Connected Allocation Results

## Status

Both full ten-cycle captures pass the frozen traffic and sampling gates.
Requested allocations grow across recoveries in both. Owner attribution remains
open; this is not a bounded-memory or leak-freedom claim.

## First Capture

| Property | Value |
| --- | --- |
| Manifest | [Connected allocation review](connected-allocation-review.md) |
| Fixture / production | `691a2455` / `7b59625f` |
| Profile | Debug integration executable; `allocation-review`; two Tokio workers |
| Topology | Two isolated namespaces; one configured overlay peer each; no Internet route |
| Infrastructure | Five default public bootstrap candidates, unreachable in this fixture |
| Duration / exit | 854.98 seconds / zero |
| Cycles / processes | Ten of ten; original PID/start identities retained |
| Portable evidence | [Samples, counters, phase boundaries and hashes](connected-allocation-samples.json) |

The executable SHA-256 is
`7bd693606049cc7df7de8d5294ab91fe2bab8dca81fdbfdd4b3c77c747a5ad04`.
No build overlapped this capture. No manual rescue or watchdog extension occurred.

### Reproduction

Use the already calibrated executable, with the original S5 timing controls:

```sh
env \
  -u P2P_VPN_REVIEW_CHURN_SMOKE \
  -u P2P_VPN_TUN_E2E_IDLE_SECONDS \
  -u P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS \
  -u P2P_VPN_TUN_E2E_WAIT_SCALE \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  timeout --signal=TERM --kill-after=10s 900 \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-8ef8d4fc7581d039 \
  tun_namespace_measures_lifecycle_churn_resources \
  --ignored --exact --nocapture --test-threads=1
```

Captured stdout/stderr: `/tmp/p2p-vpn-connected-allocation-full-1.log`.
The evidence directory is
`/tmp/p2p-vpn-tun_namespace_measures_lifecycle_churn_resources-1.c9fd253ea401304b`.
Check the executable hash before repeating; build outputs can be overwritten.

## Coverage Audit

| Gate | Node A | Node B |
| --- | ---: | ---: |
| Allocation rows | 171 | 171 |
| Maximum row gap, ms | 5,004 | 5,003 |
| Maximum cumulative clock disagreement, ms | 1 | 1 |
| Rows per outage / recovery / settle phase | 6 / 8 / 12 | 6 / 8 / 12 |
| Runtime snapshots | 152 | 152 |
| Runtime metric series | 332 | 332 |
| Runtime snapshot errors | 0 | 0 |
| Final sampled descriptors / sockets / threads | 21 / 14 / 6 | 19 / 12 / 6 |

- All 21 phases have complete runtime samples and matching binary hashes.
- Allocation PIDs match OS samples; phase-boundary gaps meet the 5,500 ms limit.
- Wall/monotonic phase duration differences meet the 100 ms limit.
- Combined JSON: 6,548,424 bytes; combined node logs: 1,689,898 bytes.
- Original 8 MiB JSON and 2 MiB log caps pass; all 35 artifact hashes were verified.
- Runner exited; no matching namespace test process remained after capture.

Sampling gates were audited after capture, separately from the S5 traffic test.
The orchestrator terminates the daemons; absent `returned` samples do not prove
graceful runtime teardown. See the separate [teardown baseline](runtime-allocation-review.md).

## Recovery and Retention

Values below are the last periodic allocation sample in each 40-second recovered
window, not an instantaneous snapshot at the exact boundary. RSS is the final OS
sample in that window. Allocation counts are concurrent, non-transactional loads.

| Cycle | Recovery, seconds | A live bytes | B live bytes | A RSS, KiB | B RSS, KiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 22.391 | 2,536,676 | 2,525,215 | 35,764 | 35,752 |
| 2 | 15.679 | 2,535,350 | 2,526,902 | 35,792 | 35,784 |
| 3 | 0.402 | 2,536,732 | 2,527,154 | 35,824 | 35,804 |
| 4 | 18.786 | 2,535,543 | 2,534,462 | 35,828 | 35,828 |
| 5 | 2.606 | 2,550,950 | 2,549,026 | 35,872 | 35,872 |
| 6 | 21.889 | 2,550,155 | 2,549,026 | 35,892 | 35,888 |
| 7 | 18.784 | 2,548,505 | 2,547,879 | 35,900 | 35,908 |
| 8 | 15.280 | 2,548,557 | 2,548,443 | 35,912 | 35,932 |
| 9 | 0.453 | 2,556,930 | 2,547,514 | 35,928 | 35,968 |
| 10 | 12.224 | 2,557,513 | 2,549,071 | 35,944 | 35,972 |
| Final settle | n/a | 2,557,513 | 2,549,071 | 35,944 | 35,972 |

All ten recoveries and the final traffic checks pass strict 5/5 probes in both
directions. Both peers finish with a healthy direct UDP path. Checked queue,
connection-attempt, retiring-connection, packet-task and pending-query owners drain.

### Final Settling

| Observation | Node A | Node B |
| --- | ---: | ---: |
| First recovered window to settle: live-byte increase | 20,837 | 23,856 |
| First recovered window to settle: live-block increase | 14 | 5 |
| Final settle live blocks | 1,558 | 1,529 |
| Allocation variation across 12 settling samples | Zero | Zero |
| RSS variation during 60-second settle | Zero | Zero |

Final traffic follows settling. A's final process snapshot is 35,948 KiB,
four KiB above the settle value; B remains 35,972 KiB. Do not conflate those windows.

## Independent Repeat

The repeat used the same calibrated executable, workload and watchdogs. No build
overlapped the run. Storage before capture was 9,016,812 KiB, below 10 GiB.
Original process identities survived all ten cycles; final traffic passed both ways.

| Property | Result |
| --- | --- |
| Total duration / exit | 881.29 seconds / zero |
| Allocation rows | 176 per daemon |
| Maximum row gap / clock disagreement | 5,003 ms / 1 ms, both daemons |
| Phase coverage | All 21 complete; original cadence and boundary gates pass |
| Recovery-window samples | Eight per daemon, except A cycle ten has nine |
| Runtime samples / metric series | 152 / 332 per daemon; no errors or missing values |
| Combined JSON / node logs | 6,540,561 / 1,724,781 bytes; original caps pass |
| Final descriptors / sockets / threads | A: 21 / 14 / 6; B: 19 / 12 / 6 |
| Final checked pending owners | Zero, with healthy direct UDP paths on both nodes |
| Cleanup | Runner exited; no matching namespace test process remained |
| Evidence | [Repeat samples and 35 artifact hashes](connected-allocation-repeat.json) |

Outer log: `/tmp/p2p-vpn-connected-allocation-full-2.log`.
Evidence directory:
`/tmp/p2p-vpn-tun_namespace_measures_lifecycle_churn_resources-1.ef4010ec157a6c00`.
Reproduce with the same command above and a distinct output log.

| Cycle | Recovery, seconds | A live bytes | B live bytes | A RSS, KiB | B RSS, KiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 3.155 | 2,535,193 | 2,533,185 | 35,780 | 36,216 |
| 2 | 13.020 | 2,535,245 | 2,538,197 | 35,840 | 36,264 |
| 3 | 15.675 | 2,535,431 | 2,537,372 | 35,864 | 36,280 |
| 4 | 0.252 | 2,534,877 | 2,535,852 | 35,888 | 36,320 |
| 5 | 18.828 | 2,534,118 | 2,548,904 | 35,904 | 36,340 |
| 6 | 2.755 | 2,557,485 | 2,549,092 | 35,936 | 36,352 |
| 7 | 12.570 | 2,555,988 | 2,549,897 | 35,956 | 36,364 |
| 8 | 0.302 | 2,557,082 | 2,550,552 | 35,968 | 36,376 |
| 9 | 15.024 | 2,554,689 | 2,549,833 | 35,972 | 36,400 |
| 10 | 0.352 | 2,555,731 | 2,549,040 | 35,976 | 36,420 |
| Final settle | n/a | 2,557,243 | 2,550,552 | 35,976 | 36,420 |

The same periodic-snapshot interpretation applies to this table. All ten recovery
checks pass strict 5/5 traffic in both directions. B's process snapshot after final
traffic is 36,424 KiB, four KiB above its settling value; A remains 35,976 KiB.

### Paired Retention Summary

| First recovered window to final settle | First A | First B | Repeat A | Repeat B |
| --- | ---: | ---: | ---: | ---: |
| Live-byte increase | 20,837 | 23,856 | 22,050 | 17,367 |
| Live-block increase | 14 | 5 | 21 | 8 |
| RSS increase, KiB | 180 | 220 | 196 | 204 |

Unlike the first capture, the repeat gains 1,512 bytes and four blocks per node
during its final minute. Both RSS series remain flat during that minute. The
repeat therefore confirms that RSS settling is not sufficient allocation evidence.

## Interpretation and Next Gates

- Both nodes' final three recovered RSS checkpoints increase; final-minute flatness does not resolve that growth.
- Live requested bytes also grow overall, so RSS allocator high-water behavior alone is not a sufficient explanation.
- Step-like increases might reflect retained capacity, but no specific owner is causally established yet.
- Paired captures reproduce overall retention without establishing an exact per-cycle slope or a leak.
- Next, isolate retained application and transport capacities from active session allocations using bounded ownership measurements.
- Attribute connected transport retention and S4 pressure growth; keep the first-use initialization gap separate.
- Complete multi-network and Android background measurements before the final resource-review audit.

This instrumented debug capture does not establish release performance, physical
battery impact or public-network behavior. Instrumentation validation is recorded
in the [manifest](connected-allocation-review.md); this publication changes docs only.
