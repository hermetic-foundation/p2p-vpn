# Allocation Size Inventory

## Purpose

Identify the size distribution behind the reproducible post-child residuals.
Sizes are not allocating owners; use this inventory to select narrow traces or
controlled initialization comparisons after accounting is validated.

## Instrumentation

| Property | Value |
| --- | --- |
| Feature | `allocation-sizes`, explicitly separate from `allocation-review` |
| Scope | Vendored development allocator and namespace fixture only |
| Exact sizes | 0 through 65,536 bytes |
| Larger allocations | Separate live byte/block totals; never silently omitted |
| Storage | Fixed atomic counters, about 512 KiB per instrumented allocator on 64-bit targets |
| Hook behavior | No allocation, locking, formatting or stack walking |
| Failures | Failed allocation/reallocation counted separately; diagnostics require zero |
| Snapshot scratch | 512 fixed size/count entries; truncation is a failure |
| Test thread stack | `RUST_MIN_STACK=8388608` for these diagnostics |
| Consistency | Inventory totals must match ordinary counters before and after the scan |
| Checkpoints | Before child, child returned, child settled; no periodic size dumps |

Original operation counters retain their request-count semantics. Size counters
track successful operations; failed reallocations preserve the old allocation.
Concurrent snapshots are not transactional, so inconsistent scans must fail.

Normal builds and existing `allocation-review` captures do not enable this
inventory. Its measurements are diagnostic evidence, not comparable CPU benchmarks.
No cached dependency source is modified; the existing repository vendor patch is extended.

## Frozen Admission

1. Build locked/offline with two jobs after checking the 10 GiB temporary-storage cap.
2. Run existing byte calibration and new exact/oversized resize-release calibration separately.
3. Run vendor accounting tests and the feature-enabled namespace unit suite.
4. Run two direct-UDP graceful admissions with the existing 90/100-second watchdogs.
5. Require original traffic/teardown gates and three complete, coherent inventories per daemon.
6. Compare summed size deltas with lifecycle deltas and the earlier 40,801-byte / 90-block residual.

Stop on a failed gate. Preserve mismatches; do not discard sizes, extend deadlines
or assign stack ownership from size equality alone. No build overlaps measurement.
Existing 8 MiB JSON / 2 MiB combined node-log caps remain in force.

## Follow-Up Boundary

After direct admission, freeze any TCP pressure inventory or owner-trace workload
separately. The completed timing campaigns use their original executable; they
must not be replaced retroactively by this diagnostic build.

## Status

Both direct admissions and calibrations pass. The size inventory accounts for
the original residual without changing its byte/block delta. It does not establish
allocating-stack ownership or complete the resource review.

## Direct Results

Both fresh-process admissions pass original traffic and graceful teardown gates
in 15.39 and 15.38 seconds. All twelve inventories match their corresponding
lifecycle counters, with zero failures, oversized totals or truncation.

Maximum inventory occupancy is 54 of 512 entries. Each of the four daemons
retains the same 17-size distribution: 40,801 bytes and 90 blocks above its
pre-child snapshot. This matches the original direct and ten-cycle UDP residuals.

| Requested Size | Net Retained Blocks | Net Bytes |
| ---: | ---: | ---: |
| 13 | 1 | 13 |
| 14 | 1 | 14 |
| 24 | 2 | 48 |
| 32 | 2 | 64 |
| 40 | 1 | 40 |
| 48 | 3 | 144 |
| 70 | 1 | 70 |
| 112 | 1 | 112 |
| 256 | 1 | 256 |
| 336 | 65 | 21,840 |
| 352 | 1 | 352 |
| 368 | 1 | 368 |
| 640 | 2 | 1280 |
| 1024 | 1 | 1024 |
| 2048 | 1 | 2048 |
| 2072 | 3 | 6216 |
| 2304 | 3 | 6912 |
| Total | 90 | 40,801 |

Matching counter totals does not make a concurrent scan atomic. These are
quiescent-checkpoint observations repeated across four processes, not a general
atomic heap snapshot or proof that every object has one particular owner.

## Artifacts

[Structured results](allocation-size-results.json) retain all inventories,
lifecycle records, deltas, source hashes and raw-log hashes. No allocation
addresses, private keys or packet payloads are emitted by the inventory.

| Run | Directory | Outer Log |
| --- | --- | --- |
| 1 | `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.63bf50cb140d6c27` | `/tmp/p2p-vpn-size-direct-1.log` |
| 2 | `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.23b2d4db07d17585` | `/tmp/p2p-vpn-size-direct-2.log` |

Executable SHA-256:
`ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486`.

```sh
env -u P2P_VPN_TUN_E2E_IDLE_SECONDS \
  -u P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS \
  -u P2P_VPN_TUN_E2E_WAIT_SCALE RUST_MIN_STACK=8388608 \
  P2P_VPN_REVIEW_GRACEFUL_SHUTDOWN=1 \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  timeout --signal=TERM --kill-after=10s 100 \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-92e26a895f071361 \
  tun_namespace_ping_crosses_two_node_overlay \
  --ignored --exact --nocapture --test-threads=1
```

## Validation

| Check | Result / Log |
| --- | --- |
| Byte calibration | Pass; `/tmp/p2p-vpn-size-byte-calibration.log` |
| Size calibration | Pass; `/tmp/p2p-vpn-size-calibration.log` |
| Vendor accounting | Pass; `/tmp/p2p-vpn-size-vendor-unit.log` |
| Feature fixture units | 63 pass, 28 opt-in exclusions; `/tmp/p2p-vpn-size-unit.log` |
| Default workspace | 1,512 pass, 40 opt-in exclusions; `/tmp/p2p-vpn-size-default-workspace.log` |
| Feature library | 1,152 pass, 15 opt-in exclusions; `/tmp/p2p-vpn-size-library.log` |
| Required Clippy groups | Pass; `/tmp/p2p-vpn-size-clippy.log`; existing advisories remain |
| Vendor Clippy | Direct cached driver, required groups pass; `/tmp/p2p-vpn-size-vendor-clippy.log` |
| Cached Nix source parity | Pass; `/tmp/p2p-vpn-size-source-check.log`; outside sandbox |
| Formatting | Changed fixture and new inventory module pass cached rustfmt; whitespace check passes |
| Full Nix derivation | Not built; unavailable tool closure remains outside this cached check |
| Android native | Not repeated: development allocator/fixture only; normal shared runtime source unchanged |

The vendor accounting unit is compiled directly from its dependency-free crate
root with the cached Rust compiler, `--test --cfg 'feature="size-inventory"'`.
No vendor lockfile or new package download is introduced.

## Next Attribution

The 65 blocks of 336 bytes are the largest retained size group. Trace their
allocation/free lifetimes in an isolated initialization control first, then
verify correspondence with the direct-daemon residual.

Tokio's process-global signal registry is a source-derived candidate, not yet
the measured owner. Do not label the 17 groups as harmless caches solely because
the net size totals are repeatable or match a plausible type layout.
