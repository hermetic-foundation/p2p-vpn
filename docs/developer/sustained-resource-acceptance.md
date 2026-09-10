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
| 5. Reproduction and minimal corrections | Capability retirement, connection retirement, Linux TUN cancellation | Production-fix inventory and guarded regression sources inspected; workload reconciliation remains |
| 6. Verification and invariant preservation | Workspace, Clippy, source parity, Android-native logs, regression sources | Final runtime terminal logs verified; source-parity inventories match; instrumentation gates still need reconciliation |
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

## Production Correction Audit

Source history from `00b1b58a` identifies three production corrections. Other
Rust changes are test-gated allocation diagnostics or regression coverage;
Android additions are debug resource/failure reporting, not main app behavior.

| Revision | Correction | Inspected Regression Boundary |
| --- | --- | --- |
| `11416a8d` | Retry unvalidated capabilities after retiring connection removal | Seven guard scenarios and duplicate closes; pinned request-response dispatch targets the surviving connection |
| `7b59625f` | Retire owned failed packet connections before deduplication | Nine ownership/error scenarios, repeated events and preserved replacement connection |
| `575ef4ac` | Cancel and join Linux TUN readers | Sticky readiness cancellation, delivery after `WouldBlock`, full-channel reader/metrics release |

Production ownership checks remain in `src/runtime/runner.rs`; dispatch tests
are in `src/runtime/runner/recovery_event_tests.rs`. Linux readiness tests are
in `src/runtime/tun/readiness.rs`.

- Capability retry requires current retirement, a configured peer, absent
  capabilities and another usable connection. Duplicate closes cannot repeat it.
- Packet retirement excludes capacity errors and requires matching request,
  peer/path/relay ownership plus a usable epoch and transport failure.
- TUN teardown closes the receiver before cancellation and join. This releases
  both full-channel and idle-device waits without closing a borrowed raw fd.

No identity, membership policy, configuration schema or discovery-order change
appears in this production diff. Existing recovery/dial functions remain in use;
the review does not claim arbitrary public-network recovery certification.

### Validation Records Inspected

| Gate | Observed Terminal Evidence |
| --- | --- |
| Capability regression | Before: one expected failure; after: 13 tests pass, plus one dispatch test |
| Packet-retirement regression | Before: one expected failure; after: one corrected test and four guard-selection tests pass |
| Final production workspace | 1,512 pass, zero failures, 40 explicit opt-in exclusions |
| TUN focused checks | Two readiness tests pass; three-test selection includes full-channel ownership |
| Graceful fixture units | 63 pass; 27 opt-in exclusions at that revision |
| Required Clippy | Final unoptimized dev completion in 22.06 seconds; advisory warnings remain |
| Android native | Final unoptimized dev completion in 1m47s; compilation, not device execution |
| Source parity | Both saved packaged/repository test inventories compare equal; completion markers exist |
| Formal models | No tracked/discoverable Lean, TLA or Alloy model found for this boundary |

The source-parity logs are empty. Their inventories and `passed` markers support
the recorded check execution, not a fresh full Nix package build. This audit has
not rerun compilation or changed the production source.

The preserved randomized address-collision workspace failure remains separate.
An unchanged successful rerun does not fix nondeterministic fixture identities.
See [validation limits](graceful-allocation-review.md#validation).

### Log Fingerprints

Paths below use prefix `/tmp/p2p-vpn-`. These hashes pin the terminal records
inspected during this audit; they are not substitutes for test coverage review.

| Log Suffix | SHA-256 |
| --- | --- |
| `tun-cancel-workspace-2.log` | `4203ec164ee3244739be0aa63ebd9c9b0e914a8acdf02c388176201e95762bc8` |
| `tun-cancel-clippy.log` | `d64882ed8f0d74a000767925e0902e09120c3179913c8218b56d6a1f16f722e7` |
| `tun-cancel-android-build.log` | `c94823b9bc75dc9c0aeb298a6b7251251c5d1f7b8bfcef3687dd79bb0fec447d` |
| `tun-cancel-unit-1.log` | `32eaf170c67ad3850430e1dacba11c86b3427649636f22868b645b94e77b12ef` |
| `tun-cancel-unit-2.log` | `fb6f12af83da39784b3f6b7fc34855bf43c46f960098082e757955204130e99c` |
| `tun-cancel-integration-units.log` | `5fff7141cad921e1f9a6d010e0762ad663dc23903b1139616538b3362f55aea7` |
| `capability-retirement-before.log` | `376f79fe29a7d04af2cabad71d35fdee854a4fef8d01e8aea9b571413edcd23c` |
| `capability-retirement-after-registered.log` | `66ce90ff181db6e4b72906c441e84dfe94ee67979530bff7bcc7dc3db6eadb75` |
| `capability-retirement-dispatch-run.log` | `c213c65e61883ca6923deaf6a45ecc32de019d19bb0142f279c20282cebec50b` |
| `churn-retirement-before-run.log` | `28dd77c9744d0b665c25517d6a8a4b3f5d57dd25335e6070f65037adacc05327` |
| `churn-retirement-after-corrected.log` | `59d6ebf08c71f6d5a850a752e4dee65fa890b1a598441ebbc11ef53d7df0b5e8` |
| `churn-retirement-guards.log` | `f1c510cc51ef71630f265a7789cfcc900db03a2f9adc9148e75274f3ff3d5d69` |

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
