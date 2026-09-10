# Sustained Resource Acceptance Audit

## Status

Audit started against `21420c0b`. The goal remains active.
This document separates verified evidence from pending reconciliation; it does
not replace the original acceptance criteria with a smaller workload.

## Requirement Map

| Criterion | Evidence To Reconcile | Current Audit Result |
| --- | --- | --- |
| 1. Source, manifests, budgets, exclusions | [Measurement plan](sustained-resource-review.md), per-workload manifests, source history | Final runtime matches graceful-capture revision; earlier Linux/Android reuse still needs reconciliation |
| 2. Sustained resource coverage | S1-S7 captures, process/runtime series, collector controls | Paired graceful reconnect artifacts revalidated; remaining workload series need final audit |
| 3. Retention and teardown | Ledger/queue controls, pressure/churn, allocation-owner traces | Reconnect teardown evidence verified; residual scope and pressure ownership conclusions still need reconciliation |
| 4. Android scheduling and background work | [Thread controls](android-thread-controls.md), load/profile/isolation reports | Reports contain CPU and scheduling proxies; final source/artifact correspondence pending |
| 5. Reproduction and minimal corrections | Capability retirement, connection retirement, Linux TUN cancellation | Reproductions and fixes documented; complete changed-source inventory pending |
| 6. Verification and invariant preservation | Workspace, Clippy, source parity, Android-native logs, regression sources | Recorded gates located; terminal logs and source applicability still need final audit |
| 7. Results and documentation | Structured results, historical failures, review index | Index contains superseded open items; reconcile after evidence audit |
| 8. Publication and final acceptance | Jujutsu main, clean tree, this audit | Startup findings published; final acceptance not established |

## Verified This Pass

### Runtime Correspondence

```sh
git diff --stat 575ef4ac 21420c0b -- \
  src android crates vendor/libp2p-kad-0.48.0 Cargo.lock
git diff 575ef4ac 21420c0b -- Cargo.toml
```

- The first command produces no differences.
- The manifest difference adds only `allocation-sizes`, an opt-in diagnostic
  feature. Test and allocator instrumentation changes remain separate.
- The production TUN readiness implementation waits with `Poll::poll(..., None)`
  and uses an explicit cancellation `Waker`; it adds no recurring idle timer.

This permits reuse of the final-production graceful captures. It does not
automatically transfer earlier CPU measurements across the TUN I/O change.

### Raw Evidence Integrity

```sh
jq -r '.records[].artifacts[] | "\(.sha256)  \(.path)"' \
  docs/developer/startup-table-results.json | sha256sum --check --quiet
jq -r '(.artifacts[], .outer_log) | "\(.sha256)  \(.path)"' \
  docs/developer/graceful-churn-{first,repeat}.json | sha256sum --check --quiet
```

| Evidence | Revalidation |
| --- | --- |
| Startup traces | All 27 artifact hashes match; three raw traces show 14 allocations/frees, normal exit and no tracker failures |
| Paired ten-cycle captures | All 70 artifact hashes match; original result summaries preserve ten cycles and unchanged process identities |
| Reconnect release | Four post-child residuals equal 40,801 requested bytes / 90 blocks; final twelve samples per daemon are flat |

Hash integrity is not sufficient alone. The remaining audit must inspect
collector/assertion scope, timelines and source correspondence for each workload.
The completed paired ten-cycle campaign is not scheduled for repetition.

## Attribution Boundary

- Direct traces assign 40,067 of 40,801 residual requested bytes to specific
  owner classes; 734 bytes / 11 blocks remain unassigned.
- Reconnect traces identify reusable channel storage and session/replay tables,
  with matching frees at teardown. They are not a whole-heap allocation census.
- The earlier reconnect inventory's 2088-byte bucket remains unassigned.
  Startup traces cannot identify an equal-sized object in another run.
- Pressure has a distinct 59,707-byte / 84-block residual. Do not merge it with
  the direct/churn baseline or infer ownership from matching aggregate totals.

The goal requires investigating growth, bounding resource behavior and verifying
release. It does not explicitly require naming every allocator byte. Whether
remaining uncertainty prevents those conclusions is still an audit question,
not permission to declare unexplained growth harmless.

## Storage Verification Limit

The current unprivileged scan reports 9,728,584 KiB, but cannot read several
root-owned evidence directories. This is a partial total, not proof that all
`/tmp/p2p-vpn-*` remains below 10 GiB. `sudo -n` requires a password.

Do not build, provision or start another large capture until the complete total
is verified. Continue source/documentation audit without deleting evidence or
changing directory permissions. No new capture or build ran in this pass.

## Next Audit Actions

1. Reconcile S1-S7 source revisions, original gates and raw measurement series.
2. Verify correction logs and affected negative/positive regression coverage.
3. Resolve required evidence gaps; distinguish optional attribution from missing
   proof of bounded behavior. Preserve platform and physical-energy exclusions.
4. Update superseded index entries, publish final findings and verify clean main.
