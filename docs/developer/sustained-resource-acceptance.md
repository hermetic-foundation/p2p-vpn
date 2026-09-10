# Sustained Resource Acceptance Audit

## Status

Audit started against `21420c0b`. The goal remains active.
This document separates verified evidence from pending reconciliation; it does
not replace the original acceptance criteria with a smaller workload.

## Requirement Map

| Criterion | Evidence To Reconcile | Current Audit Result |
| --- | --- | --- |
| 1. Source, manifests, budgets, exclusions | [Measurement plan](sustained-resource-review.md), per-workload manifests, source history | Final runtime matches graceful-capture revision; earlier Linux/Android reuse still needs reconciliation |
| 2. Sustained resource coverage | S1-S7 captures, process/runtime series, collector controls | Linux/Android evidence reconciled; paired final-code S3 now passes with the documented instrumentation limits |
| 3. Retention and teardown | Ledger/queue controls, pressure/churn, allocation-owner traces | Ledger/queue invariants verified; one/five/five/one pressure inventories match across eight daemons; large pressure-owner attribution pending |
| 4. Android scheduling and background work | [Thread controls](android-thread-controls.md), load/profile/isolation reports | Five captures' raw artifacts and historical harness versions verified; shared shutdown reuse limit explicit below |
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

## Android Evidence Reconciliation

### Integrity And Source

All 392 raw-file hash entries in the following five result sets match:

- `android-thread-control-results.json`
- `android-sustained-load-results.json`
- `android-sustained-thread-results.json`
- `android-resource-isolation-results.json`
- `android-resource-isolation-repeat-results.json`

Their five raw `evidence.json` files report successful completion and all six
cleanup flags true. This verifies retained cleanup evidence, not present device
state; no emulator or physical device was accessed for the audit.

Ten checks against current harness files fail because those files changed later.
All nine distinct recorded script hashes match historical committed contents:

| Script | Matching Content Revisions |
| --- | --- |
| `android-e2e.sh` | `0067e3d3`, `08ecf113`, `d9ec9f16` |
| `android-process-sample.sh` | `65690dcf`, `a71106db` |
| `android-resource-controls.sh` | `d9ec9f16`, `06615d80`, `08ecf113`, `0067e3d3` |

The portable results retain exact hashes. Each match was computed from
`git show REVISION:scripts/FILE`, not inferred from the report's baseline label.
Do not replace historical hashes with current ones to make a check pass.

`git diff aa14357a fc95a4f4 -- android crates/p2p-vpn-android` is empty.
The APK therefore retains unchanged Android source, but later shared Rust TUN
worker shutdown changed. The final native build passes; this is not a final-source
APK execution claim or permission to reuse unrelated platform results.

### Measured Coverage

| Workload | Established | Limit |
| --- | --- | --- |
| Sustained idle/load/drain | Both captures deliver 60,000/60,000 replies; 300/300/60-second phases | Different transports, permission setup and collector modes prevent a controlled cross-run transport comparison |
| Thread-attributed sequence | CPU 2.115%, 96.223%, 2.116%; endpoint arithmetic recomputes exactly | Debug app, observer cost included; process endpoints outlast load thread scans |
| Collector controls | Four windows; thread scanning adds 2.403 emulator CPU percentage points on average | One-boot estimate, not an exact correction or battery measurement |
| Network isolation | Two five-cycle captures; all measured batches pass; disabled network probes receive no replies | Shared runtime restarts; not uninterrupted sibling traffic |
| Scheduling | Stable-thread context-switch and tick deltas; sampled load concentrates in two threads | No hardware wakeup or physical energy counters |

Raw files preserve process identity, queues, paths, memory, descriptors and
private infrastructure counts separately from overlay peers. Empty sampled
queues and overlapping RSS/PSS ranges do not prove allocation release.

### Background Work And Limits

- `AndroidTunReader` checks its stop flag around a 250 ms `poll`. This bounded
  shutdown check is background work, not an event-only sleep or zero-wakeup claim.
- Per-network `PortReader` uses the supervisor queue and does not implement the
  new optional cancellation callback. Android retains supervisor-owned shutdown.
- The [load profile](android-load-profile.md) attributes samples to native Tokio
  workers; absent call chains prevent identifying a single causal optimization.
- Permission-dialog setup and collector cadence failures remain recorded. The
  successful later captures do not retroactively turn failed attempts into passes.
- Physical battery/thermal behavior, release performance and WAN movement are
  outside these emulator observations and remain separate acceptance work.

## Linux Workload Reconciliation

### Revalidated Captures

| Workload | Raw Hash Entries Rechecked | Scope |
| --- | ---: | --- |
| S1 idle | 2 | Two 300-second reports; original runtime `00b1b58a` |
| Observer off/on/on/off | 4 | Four 300-second reports; original runtime `00b1b58a` |
| S2 unavailable | 4 | Two outage/recovery pairs; original runtime `00b1b58a` |
| S3 paced load | 6 | Two load/drain/outcome triplets; runtime `11416a8d` |
| Graceful S4 pressure | 108 | Packet/byte five-round captures; final production runtime `575ef4ac` |
| Graceful S5 reconnect | 70, verified earlier | Two ten-cycle captures; final production runtime `575ef4ac` |

All listed hashes match retained raw files. S3's original outcome files each
show fixed transport, unchanged processes, valid delivery and 15,000/15,000
packets with zero invalid or duplicate replies. Both S4 series complete five rounds.

### Assertion Scope

The inspected `tests/support/sustained_traffic.rs` requires at least 98% of
scheduled sends and 98% replies to actual sends, no invalid/duplicate replies,
stable process identities, fixed UDP paths and final strict pings both ways.
The actual captures exceeded the delivery threshold with every reply received.

- Load and drain reports are retained before delivery assertions. Generator
  cleanup has its own five-second exit check; failed work is not replayed.
- Five-second runtime snapshots must retain healthy UDP paths and unchanged
  stream-fallback counts. They cannot exclude between-sample transients.
- S1 uses 20 threads per daemon; later S3 uses six with two Tokio workers.
  Do not call S1-to-S3 CPU differences a matched idle/load experiment.
- S3's own load/drain phases are comparable within each run: load CPU is about
  6.5-6.8% per node, returning below 0.2% after traffic.
- S2 separates static overlay peers from five unreachable bootstrap candidates.
  Infrastructure errors are not all overlay-redial attempts or successful WAN discovery.

### Final-Code Limit

S1-S3 predate Linux readiness-based TUN I/O. Their measurements remain valid for
the named revisions, but are not final-code CPU baselines. S4/S5 cover final-code
pressure, recovery, settling and teardown, not the same moderate paced workload.

The new implementation adds three Linux descriptors and no recurring idle timer.
That source inspection explains descriptor cost; it does not prove unchanged
CPU or throughput. The subsequent [paired final-code S3 captures](resource-followup-manifest.md#s3-independent-repeat)
now establish moderate-load/drain comparison on the cached instrumented runtime.
Their 30,000/30,000 replies and near-idle drain CPU are new evidence, not reuse of
older results or a pressure-only pass.

The complete storage total must be established before a new capture. Reuse the
cached final-runtime executable and original workload gates if follow-up is
needed; do not rebuild or repeat the completed ten-cycle campaign by default.

## Attribution Boundary

### Revalidated Ownership Controls

All sixteen ledger/queue log hashes and eleven epoch-control log hashes match.
The structured results also pass explicit checks over every measured cycle:

| Owner | Checked Invariant | Result |
| --- | --- | --- |
| Signed ledger | Twelve captures, ten cycles each; whole-cycle and initial-relative post-drop requested bytes/blocks zero | All 120 pass |
| Packet queues | Four captures, ten cycles each; drain/expiry bytes equal admitted payload totals | All 40 pass |
| Queue containers | Post-retirement storage 1368 bytes in every cycle; final whole-owner bytes/blocks zero | All four owners fully release |
| Epoch reclamation | Eleven preserved executions; fixed control distinguishes payload destruction from deferred bookkeeping | Logs match; no daemon-wide bound inferred |

These controls establish their named ownership boundaries. They do not transform
RSS into exact allocation counts or retroactively identify all daemon allocations.
The source-gated ledger and queue diagnostics remain unchanged in production.

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

### Required Follow-Up Boundary

The [follow-up manifest](resource-followup-manifest.md) pins cached binaries,
commands, comparison controls and budgets before the remaining captures.

1. Verify the full temporary-storage total before captures. A read-only elevated
   total has been requested; do not delete evidence or alter directory permissions.
2. Final-code moderate-load/drain comparison is complete: existing S3 workload,
   cached executable, original gates and two fresh repetitions pass.
3. Investigate the distinct pressure residual with a bounded matched control or
   owner inventory. Freeze its comparison and quotas before execution.
4. Reconcile remaining diagnostic validation and update the review index only
   after the evidence establishes the corresponding requirement.

No new ten-cycle reconnect run is required by this audit. The unassigned 734
direct bytes and 2088-byte reconnect bucket remain disclosed limitations; they
are not automatically extra work merely because their stack names are unknown.
The [pressure comparison](resource-followup-manifest.md#pressure-comparison-results)
now establishes identical residual size rows across eight one/five-round daemons.
The two large pressure-specific blocks still need allocating-owner correspondence.

## Storage Verification And Cleanup

The current unprivileged scan reports 9,728,584 KiB, but cannot read several
root-owned evidence directories. This is a partial total, not proof that all
`/tmp/p2p-vpn-*` remains below 10 GiB. `sudo -n` requires a password.

The user subsequently authorized elevated measurement and cache cleanup. The
complete pre-cleanup total was 10,655,004 KiB, above the cap. Only `.rlib` and
`.rmeta` intermediates from the two older `p2p-vpn-resource-*-target/release/deps`
caches were removed after verifying no build used them.

The complete post-cleanup total is 9,598,300 KiB. Both historical release
executables retain their original hashes; current test binaries, raw captures
and user files are preserved. The storage blocker is resolved without a rebuild.

## Next Audit Actions

1. Reconcile S1-S7 source revisions, original gates and raw measurement series.
2. Verify correction logs and affected negative/positive regression coverage.
3. Resolve required evidence gaps; distinguish optional attribution from missing
   proof of bounded behavior. Preserve platform and physical-energy exclusions.
4. Update superseded index entries, publish final findings and verify clean main.
