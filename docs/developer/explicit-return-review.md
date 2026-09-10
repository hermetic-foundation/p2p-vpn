# Explicit Allocator Returns

## Frozen Observer Check

The prior small-allocation observer lost `FinishBreakpoint` returns. Disassembly
shows distinct allocator prologues. Test machine-entry breakpoints and explicit
thread/stack-scoped return addresses without relying on finish-frame unwinding.

| Property | Bound |
| --- | --- |
| Baseline | `aa116658` |
| Binary | Same cached `ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486` fixture |
| Architecture | This x86-64 executable only; return address at entry RSP, return RSP = entry + 8 |
| Selected sizes | 13, 14, 352, 368, 1024 bytes |
| Return scoping | Exact thread and stack pointer; remove each breakpoint after its return |
| Quotas | Existing 4096 allocations / 64 sites; at most 128 pending and 16384 total returns |
| Calibration | Existing allocation/grow/shrink/free test; 15 seconds plus two-second grace, 256 KiB log |
| Daemon validation | Only after calibration; two unchanged direct fixtures, two workers |
| Daemon watchdogs / logs | Original 90-second fixture; 100-second outer plus two-second grace; 1 MiB/file |

Require empty pending-return tracking and no failures at normal exit. Preserve
failed reallocations' old owners. Do not accept duplicates, drop events or waive
the previous completeness gate. Native size filtering remains before Python.

This is an observer experiment, not a production fix or a causal diagnosis of
GDB. All prior invalid traces remain excluded. No build or public-network test
overlaps the private fixtures.

## Results

Calibration passed with one allocation, three allocator returns including two
resizes, one free and no pending returns. Both unchanged daemon fixtures then
passed traffic/path, graceful-shutdown and cleanup gates.

| Measurement | First | Repeat |
| --- | ---: | ---: |
| Duration under debugger | 16.88 seconds | 56.09 seconds |
| Tracked allocations / frees | 109 / 106 | 200 / 197 |
| Allocator return events | 128 | 220 |
| Distinct stack sites | 59 | 60 |
| Checkpoints / pending returns at exit | 3 / 0 | 3 / 0 |
| Tracked post-child blocks / bytes | 5 / 1771 | 5 / 1771 |
| All-size post-child residual | 90 blocks / 40801 bytes | Same |

Neither run reports missed returns, duplicate live addresses, allocation failure
or quota breach. The same five owners appear after child return and 100 ms later;
per-run opaque IDs differ, so match them through the recorded stack-site mapping.

## Retained Owners

| Bytes | Owner | End-of-process observation |
| ---: | --- | --- |
| 1024 | Parking-lot hash-table bucket storage | No matching free before exit |
| 368 | Thread-local RNG, adapter-reseeding variant | Freed before exit |
| 352 | Thread-local RNG, reseeding variant | Freed before exit |
| 14 | Futures-timer helper thread-name storage | No matching free before exit |
| 13 | Thread-name copy for stack-overflow handling | No matching free before exit |

Parking-lot's static hash-table contract explicitly retains published tables;
capacity is tied to its thread-count growth policy. This identifies the owner,
not a claim that arbitrary thread creation has a fixed memory bound.

The timer helper remains process-global. Thread-name storage is separate from
the timer heap capacity attributed in the large-block trace.

## Limits And Evidence

- The explicit observer passes these fixtures. This does not establish the exact
  cause of GDB's earlier `FinishBreakpoint` failures or validate every workload.
- Earlier invalid traces remain excluded. No production source or allocator was
  changed, and no missing-return assertion was removed.
- Combined direct-residual attribution now covers 40067 bytes / 79 blocks.
  Another 734 bytes / 11 blocks and live reconnect-growth ownership remain open.
- Only cached calibration and fixture executions ran. Workspace, Android and Nix
  builds were not repeated for this documentation-only observer work.

[Results](explicit-return-results.json) preserve commands, observer sources,
retained allocating stacks, live sets, counters and source/artifact hashes.

## Status

Explicit-return admission and five-size attribution complete. Remaining smaller
owners, bounded reclamation, reconnect growth and the final audit remain open.
