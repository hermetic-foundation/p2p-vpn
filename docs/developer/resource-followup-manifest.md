# Final Resource Follow-Up Manifest

## Status

Frozen at `489da6b5`. The first S3 capture has started after authorized cache
cleanup and a complete elevated storage total of 9,598,300 KiB.
This closes specific gaps from the [acceptance audit](sustained-resource-acceptance.md),
not a new review or a repeat of the completed ten-cycle reconnect campaign.

## Shared Preconditions

- Verify all `/tmp/p2p-vpn-*`, including root-owned directories, total less than
  10 GiB with at least 128 MiB remaining for this bounded campaign.
- Run sequentially using cached binaries, two Tokio workers and existing isolated
  namespaces. No builds, debugger, downloads, physical hosts or public routes.
- Preserve failures and stop for investigation. Do not extend deadlines, change
  packet assertions, replay failed traffic or alter runtime state manually.
- Record executable hashes, environment, exit codes, artifacts and cleanup.
  Stop before another case if storage or artifact budgets are exceeded.

## Final-Code S3

| Setting | Frozen Value |
| --- | --- |
| Executable suffix | `tun_namespace-b64312792e9e0507` |
| SHA-256 | `6516ee1bcb7b227fac264fabfd6e7a47a86d0f9fdba93e15819bea32953fa6ab` |
| Production source | `575ef4ac`, unchanged in current runtime |
| Profile | Unoptimized, `allocation-review`; no size inventory |
| Work | Two fresh repetitions; existing 30-second warmup, 300-second load, 60-second drain |
| Traffic | 50 requests/second, 512-byte payload; original delivery/transport/process gates |
| Deadlines | Existing 480-second inner; 510-second outer plus ten-second kill grace |
| Artifacts | Original 8 MiB per phase report, 64 KiB generator output; 8 MiB outer log |

```sh
env -u P2P_VPN_REVIEW_GRACEFUL_SHUTDOWN \
  -u P2P_VPN_REVIEW_TRAFFIC_SMOKE \
  -u P2P_VPN_TUN_E2E_IDLE_SECONDS \
  -u P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS \
  -u P2P_VPN_TUN_E2E_WAIT_SCALE \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  RUST_MIN_STACK=8388608 \
  timeout --signal=TERM --kill-after=10s 510 \
  prlimit --fsize=8388608:8388608 -- \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-b64312792e9e0507 \
  tun_namespace_measures_sustained_traffic_resources \
  --ignored --exact --nocapture --test-threads=1
```

Use a separate outer log per run. Compare load/drain CPU within each capture,
preserving collector overhead and actual process/runtime sampling intervals.
Allocation instrumentation differs from historical S3: do not claim a precise
before/after performance improvement or a release benchmark.

S3 rejects the graceful-review option. Leave it unset; its ordinary owned-child
cleanup is not post-runtime allocation-release evidence. Existing final-code
graceful pressure and reconnect captures cover that distinct requirement.

## First S3 Result

First S3 result: [structured evidence](final-s3-results.json). The original
fixture passed in 416.15 seconds with 15,000/15,000 replies, no skipped/invalid/
duplicate packets, fixed transport and unchanged processes. No matching fixture
process remained after normal test completion. The independent repeat is pending.

| Phase | A CPU, % One Core | B CPU, % One Core | OS Rows Per Node | Runtime Rows Per Node |
| --- | ---: | ---: | ---: | ---: |
| Load | 8.197 | 7.413 | 299 | 60 |
| Drain | 0.183 | 0.217 | 61 | 12 |

CPU uses actual first/last sample timestamps and 100 Hz ticks. Each node retained
six threads and its fixed descriptor count (A 24, B 22). RSS endpoints were
unchanged during drain. These observations include allocation-review overhead;
do not interpret the historical S3 difference as a measured TUN regression.

## Pressure Residual Inventory

| Setting | Frozen Value |
| --- | --- |
| Executable suffix | `tun_namespace-92e26a895f071361` |
| SHA-256 | `ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486` |
| Profile | Unoptimized, `allocation-sizes`; same production runtime |
| Sequence | One, five, five, one rounds; fresh daemons for each capture |
| Limit profile | Packets: four packets / 8192 bytes; natural identity ordering |
| Work | Original shaped TCP pressure, strict recovery and graceful teardown gates |
| Deadlines | Original fixture deadline; outer 480 seconds plus ten-second kill grace |
| Reports | Original 8 MiB JSON / 2 MiB combined node-log budgets |
| Inventory | Before child, child returned, child settled; fixed 512-row output |

```sh
env -u P2P_VPN_TUN_E2E_PRESSURE_INITIATOR \
  -u P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS \
  -u P2P_VPN_TUN_E2E_WAIT_SCALE \
  P2P_VPN_REVIEW_GRACEFUL_SHUTDOWN=1 \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  RUST_MIN_STACK=8388608 \
  P2P_VPN_TUN_E2E_PRESSURE_LIMIT=packets \
  P2P_VPN_TUN_E2E_PRESSURE_ROUNDS="$ROUNDS" \
  timeout --signal=TERM --kill-after=10s 480 \
  prlimit --fsize=8388608:8388608 -- \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-92e26a895f071361 \
  tun_namespace_recovers_after_tcp_queue_pressure \
  --ignored --exact --nocapture --test-threads=1
```

### Interpretation Gates

1. Require successful original traffic/queue/shutdown gates before interpreting
   a capture as a passing workload. Retain all inventory rows on failure.
2. Reject incoherent/truncated inventories, accounting failures or oversized
   buckets from exact size attribution; do not silently omit them.
3. Subtract each daemon's own pre-child inventory. Compare one/five-round
   residuals by size and block count, not just aggregate byte totals.
4. Determine whether pressure residuals depend on repeated rounds. A stable
   size inventory is not allocating-stack identity or an arbitrary-load bound.
5. If ownership remains unclear, use the resulting candidate sizes to freeze
   one targeted trace. Do not begin unbounded stack collection or broad reruns.

The existing byte-profile capture remains independent evidence. This comparison
isolates packet-profile round count; it does not newly certify byte-profile
allocation ownership or claim zero retained process-global memory.
