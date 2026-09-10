# Startup Allocation Correspondence

## Frozen Trace

| Setting | Value |
| --- | --- |
| Baseline | `e26bbe44` |
| Binary SHA | `ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486` |
| Selected sizes | 212, 372, 532, 2088 bytes |
| Workload | Two unchanged direct fixtures with graceful shutdown and two workers |
| Observer | Validated explicit-return trace from node A child entry |
| Isolation | Existing fresh user/PID/network namespaces; no Internet route |
| Bounds | 4096 allocations, 64 sites, 24 frames/site, 240 characters/frame; 128 pending / 16384 total returns |
| Watchdogs / logs | Original 90-second fixture; 100-second outer plus two-second grace; 1 MiB/file |

Require normal fixture/inferior exit, three lifecycle checkpoints, no missing
returns or quota failure, and original traffic/path/shutdown/cleanup gates.
No concurrent builds. Wrapper PID is only a namespace handle, not runtime cost.

The reconnect trace missed owners allocated before attachment. Starting at child
entry locates candidate owners for remaining size buckets, but does not prove
that every equal-sized allocation in another run has the same owner.

## Trace Output Follow-Up

Both planned runs passed and released every selected allocation. However, stack
sites alone do not map transient objects to their sizes when all objects are
freed before the lifecycle snapshots.

Add an opaque ID/size/site record at each successful allocator return for one
additional direct diagnostic. Keep every existing bound and validity gate.
This fills an observer-output gap; do not infer sizes from type names in the
earlier two captures. No raw address or payload is printed.

## Results

All three fixtures passed: 55.75, 55.76 and 55.95 seconds under the debugger.
Each trace records 14 allocations, 14 frees, ten sites, three checkpoints,
no pending returns and no tracker failure. No selected allocation survives
runtime return or the subsequent settling checkpoint.

The third run's explicit records map sizes without inference:

| Size | Allocations | Observed owners |
| ---: | ---: | --- |
| 212 | 2 | Packet-plane session lifetimes and replay windows |
| 372 | 8 | Kademlia connection IDs; protected addresses; packet in-flight accounting; temporary recovery targets |
| 532 | 4 | DCUtR, connection-limit, Identify and AutoNAT connection bookkeeping |
| 2088 | 0 | Not observed in this direct follow-up |

The five temporary recovery-target allocations share one stack site. The four
532-byte allocations belong to four distinct owners, illustrating why a byte
size alone cannot identify ownership in another capture.

## Evidence And Scope

[Results](startup-table-results.json) preserve commands, observer variants,
explicit size events, lifecycle sets, summaries and artifact hashes. JSON stack
summaries retain 14 frames; raw logs retain the original 24-frame traces.

- No production source changed. Cached direct fixtures passed; workspace,
  Android and Nix builds were not repeated for documentation-only tracing.
- These direct traces do not assign the earlier reconnect's 2088-byte bucket.
  It remains an explicit limit, not an inferred owner or a zero-allocation claim.
- The next action is the original requirement-by-requirement acceptance audit,
  which must distinguish mandatory gaps from additional optional attribution.

## Status

Startup bookkeeping owners and release are documented. Broader acceptance is
not yet established; no goal-completion claim is made here.
