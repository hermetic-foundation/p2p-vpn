# Sustained Observer Overhead

## Scope

The [resource review protocol](sustained-resource-review.md) freezes this
off/on/on/off comparison. Only periodic control-socket status queries differ.
Production runtime code is unchanged; allocation attribution remains open.

| Setting | Value |
| --- | --- |
| Fixture revision | `77f882c7` |
| Runtime baseline | `00b1b58a` |
| Build | Debug integration-test executable; not packaged release CLI |
| Topology | Two isolated namespaces, owned direct UDP, no Internet route |
| Warmup / capture | 30 / 300 seconds per run |
| Cadence | OS sampling 1 second; diagnostics and enabled status queries 5 seconds |
| Clock | 100 process ticks per second |
| Safety | 450-second outer watchdog; 8 MiB report and 2 MiB combined node logs |

Executable SHA-256:
`4bdeffb4b9e6c2eed1130e0976fa995dd03eea606c35c322b41f31883f6c506e`.
All runs use this same executable with fresh processes and generated identities.

## Reproduction

Run the saved binary without concurrent builds. Use `MODE=0,1,1,0` in order,
with a distinct `LOG` for each run. Keep each failed attempt separately.

```sh
timeout --signal=TERM --kill-after=10s 450 env \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
  P2P_VPN_TUN_E2E_IDLE_SECONDS=300 \
  P2P_VPN_TUN_E2E_IDLE_RUNTIME_SAMPLING="$MODE" \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-fc4bbc3b02326c73 \
  tun_namespace_ping_crosses_two_node_overlay \
  --ignored --exact --nocapture > "$LOG" 2>&1
```

CPU percent of one core is `100 * delta_ticks / CLK_TCK / delta_elapsed_seconds`.
Use actual observation timestamps, not the nominal capture duration.
Collector snapshots exclude startup and report serialization.

## Results

All four tests passed, with normal teardown and no matching fixture processes
remaining. Durations were 346.54, 346.58, 346.65 and 371.76 seconds, including
startup, warmup and cleanup. Each observation window remained 300 seconds.

| Run | Periodic Queries | A CPU % | B CPU % | Collector CPU % |
| --- | --- | ---: | ---: | ---: |
| 1 | Off | 0.15333 | 0.15667 | 0.61665 |
| 2 | On | 0.17333 | 0.17000 | 0.66332 |
| 3 | On | 0.17334 | 0.16000 | 0.67999 |
| 4 | Off | 0.14667 | 0.14333 | 0.63998 |

| Coverage | Result |
| --- | --- |
| OS observations | 300 per node in each run; stable PID/start identities |
| Runtime observations | 120 per on-mode run, complete; off-mode series empty with completeness null |
| Total / socket descriptors | A: 21 / 14; B: 19 / 12 throughout all four runs |
| Threads | 20 per node throughout all four runs |
| Sampled queues | On-mode packet and byte gauges zero throughout |
| Pending attempts / retiring connections | On-mode gauges zero throughout |
| Boundary counters | Each node/run: 60 probes; zero additional redials, outgoing errors or probe failures |
| Host one-minute load, before / after | Run 1: 1.21 / 1.19; run 2: 1.73 / 3.10; run 3: 3.70 / 3.52; run 4: 3.70 / 2.83 |

Within the two pairs, on-mode daemon CPU is 0.01333 to 0.02667 percentage points
higher. Collector CPU is 0.04000 to 0.04666 points higher. These observations do
not cross the declared CPU investigation threshold; two pairs do not define an SLA.

Host load differed across runs. Identities were regenerated, as in the frozen
fixture. Do not treat the small differences as exact costs or subtract them
from future measurements. Keep the five-second status collector for attribution.

### Memory

| Run | A RSS KiB, First / Last | B RSS KiB, First / Last | Collector RSS KiB, First / Last |
| --- | --- | --- | --- |
| 1, off | 37196 / 37376 | 36648 / 36696 | 22764 / 23856 |
| 2, on | 36580 / 36668 | 36516 / 36592 | 23408 / 30108 |
| 3, on | 36956 / 37060 | 36700 / 36784 | 22784 / 29488 |
| 4, off | 36676 / 36688 | 36844 / 36992 | 22736 / 23968 |

Collector retention is expected to include buffered observations; on-mode also
retains status maps. These are RSS observations, not measured allocation sizes.
Collector storage must not be attributed to daemon memory.

The final three minute checkpoints increase in run 2 for both nodes, and run 3
for A. This preserves the allocation-investigation trigger. Off-mode also grows
in some windows, so periodic queries alone do not explain every RSS increase.

No leak or bounded-retention claim is established. Next attribution needs live
allocation or allocation-owner evidence, including repeated ledger and packet
pressure cycles; stable descriptors and quiet queues are insufficient.

### Evidence and Budgets

[Portable summaries](sustained-observer-samples.json) retain unrounded CPU,
minute RSS checkpoints, process identities, host load, report paths and SHA-256.
Raw reports and logs remain at their original paths; no evidence was deleted.

| Run | Report Suffix | Report Bytes | Combined Node Log Bytes |
| --- | --- | ---: | ---: |
| 1 | `1.286d7186a6229c00` | 363424 | 1130194 |
| 2 | `1.30167d6fbfb6d652` | 2290903 | 1129022 |
| 3 | `1.7e4fe54223e0717e` | 2290862 | 1138850 |
| 4 | `1.a6aa162f4cd700ef` | 363394 | 1218977 |

- Raw report: `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-SUFFIX/idle-sample.json`.
- Outer logs: `/tmp/p2p-vpn-sustained-overhead-{1-off,2-on,3-on,4-off}.log`.
- Post-campaign `/tmp/p2p-vpn-*`: 8,309,212 KiB, below the 10 GiB cap.
- No concurrent review builds, physical-device interactions or manual recovery.

## Interpretation Limits

- Off mode still samples OS resources, logs diagnostics and queries boundaries.
  It is not an uninstrumented baseline.
- Status series cover scheduled seconds 0 through 295; OS samples span 300 seconds.
  Do not compare their deltas as if their windows were identical.
- Payload ping assertions exercise traffic before the idle capture.
  They do not establish payload delivery after the observation window.
- Stable sampled gauges do not rule out between-sample transients.
  RSS increases do not identify live allocations or establish a leak.
- These runs do not exercise public discovery, relays, QUIC streams, Android,
  unavailable peers, sustained offered load or physical energy use.
