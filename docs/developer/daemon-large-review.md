# Large Retained Allocations

## Frozen Diagnostic

| Setting | Value |
| --- | --- |
| Baseline | `c4dcaf14` |
| Selected sizes | 640, 2048, 2072, 2304 bytes |
| Inventory residual in selected sizes | 16456 bytes / 9 blocks |
| Fixture | Existing direct two-node fixture with graceful shutdown |
| Binary / SHA | Same `ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486` executable as [336-byte trace](daemon-336-review.md) |
| Observer | Native filtered GDB, node A from child entry to normal exit |
| Checkpoints | Three existing size-inventory emissions |
| Workers | `TOKIO_WORKER_THREADS=2` |
| Isolation | Fresh existing user/PID/network namespace fixture; no Internet route |
| Limits | 4096 tracked allocations, 64 sites, 24 frames/site, 240 characters/frame |
| Watchdogs / log cap | Unchanged 90-second fixture, 100-second outer plus two-second kill grace; 1 MiB/file |
| Repetition | Two fresh fixtures; no builds overlap |

Reuse the validated namespace wrapper. Expand only the native allocation-size
filters; follow matching reallocations and frees, including resizing into and
out of selected sizes. Keep failed-resize owners and invalidate failed traces.

## Acceptance

- Require unchanged traffic, selected-path, graceful shutdown and cleanup gates.
- Require all three checkpoints, normal inferior exit and no quota/failure.
- Attribute by stack and live allocation ID, never size coincidence alone.
- Do not interpret debugger timing or its namespace-handle PID as runtime cost.

## Results

Both unchanged direct fixtures passed, including graceful shutdown and cleanup.
Each trace records 64 allocations, 54 frees, 12 sites and three checkpoints;
neither reports an allocation failure or exceeded quota.

| Measurement | First | Repeat |
| --- | ---: | ---: |
| Duration under debugger | 16.63 seconds | 55.95 seconds |
| Node A log bytes / 1048576 limit | 175944 | 486027 |
| Selected blocks before child | 0 | 0 |
| Selected blocks after child / settled | 9 / 9 | 9 / 9 |
| Selected bytes after child / settled | 16456 / 16456 | 16456 / 16456 |
| All-size residual after child | 40801 bytes / 90 blocks | Same |

The same opaque IDs, sizes and sites survive both shutdown checkpoints in both
runs. Durations remain evidence of observer-instrumented execution, not a
production latency comparison.

## Owners

| Site | Retained bytes | Stack evidence |
| --- | ---: | --- |
| Tokio signal array | 2048 | `OsStorage::default` through `globals_init` |
| Global timer heap capacity | 640 | `Heap::push`, `Timer::update_or_add`, global helper thread |
| Crossbeam global collector | 640 | `Arc<Global>::new`, default collector initialization |
| Crossbeam initial sealed-bag queue node | 2072 | `Queue::new` through `Global::new` |
| Crossbeam local registrations | 3 x 2304 | `Local::register`, default thread-local handle |
| Crossbeam finalization queue nodes | 2 x 2072 | `Global::push_bag` through `Local::finalize` |

Two local registrations arise through Hickory DNS-cache access via Moka; a third
arises during cache drop. Crossbeam keeps a global collector in `OnceLock` and
registers thread-local handles with it.

On last-handle release, `Local::finalize` queues its bag and marks the local
entry deleted. Actual deallocation is deferred through epoch collection. The
source does not promise reclamation merely because 100 ms elapsed.

### Exit Distinction

After the final checkpoint, the test thread's local-handle destructor allocates
one additional 2072-byte queue node. Thus the final live set has ten selected
blocks, while the measured post-child residual has nine. Do not conflate them.

### Remaining Work

- Combined with the [336-byte trace](daemon-336-review.md), stack attribution
  covers 38296 bytes / 74 blocks of the direct residual.
- Another 2505 bytes / 16 blocks remain unattributed. Live reconnect growth and
  whether deferred reclamation remains bounded still require review.
- Ownership attribution alone is not proof of a leak or proof that all retention
  is harmless. No runtime flush, cache bypass or dependency workaround was added.

## Evidence

[Results](daemon-large-results.json) retain commands, observer sources, stack
sites, live sets, lifecycle counters and raw-artifact/source hashes.

Only documentation changed. Both cached fixture tests passed; workspace, Android
and Nix builds were not rerun. No physical host or public network was contacted.

## Status

Selected-size attribution complete. Broader retention and the final audit remain
open; no production change is justified by these traces alone.
