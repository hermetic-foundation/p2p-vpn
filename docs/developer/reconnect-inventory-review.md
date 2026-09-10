# Reconnect Size Inventory

## Frozen Diagnostic

| Setting | Value |
| --- | --- |
| Baseline | `11c8ba0d` |
| Binary SHA | `ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486` |
| Fixture | Existing one-cycle lifecycle-churn smoke; two isolated nodes |
| Workers / shutdown | Two Tokio workers; existing graceful shutdown |
| Observer | Attach stopped-process inventory reader to original node A PID |
| Samples | Existing periodic emission and size-inventory lifecycle checkpoints |
| Snapshot limits | 128 observations, 512 nonzero size rows, 1 MiB debugger log |
| Fixture watchdog | Original 240 seconds; outer 250 seconds plus two-second grace |
| Other files | 8 MiB per-file ceiling; original fixture aggregate budgets unchanged |
| Recovery gates | Original outage demotion, deadlines, strict traffic and process-resource assertions |

Record executable, role, start identity, namespace PID mapping and capture
directory before attachment. Do not replace the node with a debugger wrapper;
its original PID must remain the subject of resource checks.

## Acceptance

- Require normal fixture and inferior exit, at least 24 snapshots, and no row or
  snapshot quota failures. Retain coherence flags for interrupted allocator updates.
- Require coherent samples in outage, recovery and settling phases before using
  phase comparisons; missing/incoherent observations are not zero activity.
- Require native post-child size rows to match the stopped-process readout.
- Do not use debugger timing for production CPU or throughput claims.

Run one diagnostic first. This locates candidate allocation-size growth, not a
replacement for the completed paired ten-cycle campaigns or allocating stacks.
No public-network campaign, builds or physical-host operations overlap it.

## Results

The unchanged diagnostic passed in 189.02 seconds. Both strict recovery batches
and both final batches delivered 5/5 replies, totaling 20/20. Recovery took
22.435 seconds under observation; no manual intervention occurred.

| Gate | Result |
| --- | --- |
| Process identities | Original PIDs and start ticks preserved |
| Threads before / after | Six on each node |
| FDs before / after | Node A 24 / 24; node B 22 / 22 |
| Inventory observations | 36, all coherent |
| Outage / recovery / settle observations | 6 / 8 / 12, all coherent |
| Final settling | All 12 observations have identical requested byte/block totals |
| Graceful shutdown | Both normal exits; original TUN/control cleanup gates pass |
| Post-child inventories | Both observed snapshots exactly match native size rows |
| Fixture evidence classification | `complete=true`, `proof_eligible=false` smoke |

## Size Changes

Node A's pre-outage samples contain 2529494 requested bytes / 1538 blocks.
Its settled samples contain 2535973 bytes / 1549 blocks: a net increase of
6479 bytes / 11 blocks. Disconnection temporarily reduced both totals.

| Allocation size | Net blocks | Net bytes |
| ---: | ---: | ---: |
| 8 | 3 | 24 |
| 32 | -1 | -32 |
| 40 | 2 | 80 |
| 49 | -1 | -49 |
| 52 | 4 | 208 |
| 84 | -1 | -84 |
| 212 | 1 | 212 |
| 372 | 1 | 372 |
| 532 | 1 | 532 |
| 768 | -1 | -768 |
| 800 | 1 | 800 |
| 1108 | 1 | 1108 |
| 2088 | 1 | 2088 |
| 2140 | -1 | -2140 |
| 4128 | 1 | 4128 |

These are net size-bucket differences, not object identities. Positive and
negative changes must be retained together; equal-sized replacement objects
can cancel in this comparison.

After runtime teardown the residual returns to 40801 bytes / 90 blocks,
matching the direct and full graceful-churn controls. This release evidence
does not yet explain which live owners grew during the reconnect.

## Evidence And Next Step

[Results](reconnect-inventory-results.json) record the verified PID mapping,
commands, observer script, phase alignment, all counter summaries, size deltas
and raw-artifact hashes. No matching fixture process remained after completion.

The next stack targets are the 4128-, 2088- and 1108-byte positive buckets and
the related smaller changes. Do not attribute them to Crossbeam or a hash table
solely from their size; preserve the negative buckets when checking growth.

No production code changed. Cached diagnostics passed; workspace, Android and
Nix builds were not repeated for this documentation-only capture.

## Status

One-cycle inventory capture complete. Live-owner attribution, remaining direct
residual owners and the final acceptance audit remain open.
