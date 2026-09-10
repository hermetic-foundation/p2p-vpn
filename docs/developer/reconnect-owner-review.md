# Reconnect Allocation Owners

## Frozen Trace

| Setting | Value |
| --- | --- |
| Baseline | `d68d5ab4` |
| Binary SHA | `ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486` |
| Selected sizes | 212, 372, 532, 800, 1108, 2088, 4128 bytes |
| Workload | Existing one-cycle lifecycle-churn smoke; two workers; graceful shutdown |
| Attachment | Original node A PID after executable/role/start/namespace verification |
| Return tracking | Validated exact-entry, thread/stack-scoped observer; follow resizes/frees |
| Bounds | 4096 allocations, 64 sites, 24 frames/site, 240 characters/frame; 128 pending / 16384 total returns |
| Snapshots | Existing periodic and post-child size emissions; at least 24 |
| Watchdogs | Original 240-second fixture; 250-second outer; two-second kill grace |
| Logs | 1 MiB debugger; 8 MiB other files; original aggregate budgets |

Require normal inferior/fixture exit, empty pending returns, no tracker failure
and original recovery, traffic, process-resource and cleanup gates. No wrapper
PID replaces the daemon. No builds, public network or physical hosts involved.

Capture one diagnostic first. Match live allocation IDs to allocating stacks
and eventual frees, not sizes alone. Existing allocations before attachment are
outside the trace; net size differences from another run are not object identity.

## Results

The unchanged diagnostic passed in 190.91 seconds. Recovery completed in
3.257 seconds under observation; strict recovery and final traffic checks passed.
Original process identities remained intact and graceful cleanup succeeded.

| Trace gate | Result |
| --- | ---: |
| Allocations / matching frees | 28 / 28 |
| Return events / pending at exit | 32 / 0 |
| Stack sites / snapshots | 14 / 37 |
| Tracker failures | 0 |
| Selected blocks after runtime return | 0 |
| Selected blocks at process exit | 0 |

## Settled Owners

Four tracked blocks survive settling, totaling 6248 bytes. They all disappear
during teardown. These are surviving allocation IDs, not a whole-heap net delta.

| ID | Bytes | Owner |
| ---: | ---: | --- |
| 4 | 4128 | Tokio MPSC block for `RuntimeControlRequest` |
| 26 | 1108 | Packet-plane retiring-session hash table |
| 27 | 212 | Packet-plane replay-window hash table |
| 28 | 800 | Tokio MPSC block for TUN-reader `Vec<u8>` packets |

Other selected allocations, including recovery target sets and log-string
growth, do not survive settling. The 2088-byte bucket from the earlier inventory
is not identified as a surviving post-attachment allocation in this run.

### Ownership Rules

- Control requests use a 16-entry bounded channel; TUN reads use a 1024-entry
  bounded channel. Tokio may append reclaimed blocks for reuse before freeing them.
- The retiring-session map holds at most one value per peer. Replacement,
  forgetting and expiry remove logical entries; the map can retain capacity.
- Replay-window insertion evicts the oldest entry at the configured limit.
  This is bounded per session, not an unlimited history of received session IDs.

The trace identifies owner classes and teardown release. It does not prove a
fixed process-wide memory bound for arbitrary peer counts or traffic patterns.
No capacity-shrinking workaround is justified by these observations alone.

## Validation

- All 42 cached packet-plane tests pass, including bounded replay windows,
  overlap limits, forgetting, replacement and expiry.
- The one-cycle fixture retains `proof_eligible=false`; the completed paired
  ten-cycle evidence remains separate.
- No production code changed. Workspace, Android and Nix builds were not rerun
  for this documentation-only trace; source ownership rules were inspected.

[Results](reconnect-owner-results.json) retain the exact command, observer,
process identity, allocating stacks, live sets, source hashes and artifact hashes.

## Status

Four settled owner classes and their release are established. Remaining bucket
correspondence, smaller residual owners and the final audit remain open.
