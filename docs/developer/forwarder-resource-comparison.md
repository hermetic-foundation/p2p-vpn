# Forwarder Membership Resources

## Scope

Measured on 2026-09-06. Compare `f24831fa` with `44da11bd`, which retains the
effective-membership projection for daemon DNS to borrow. Neither build changes
the signed-record limit or wire protocol.

## Method

- Installed Nix Rust 1.97.1; unoptimized test profile, debug information and incremental compilation disabled.
- Fresh Linux process per sample; two rounds per size, alternating baseline/current order.
- Deterministic Ed25519 fixtures: one root, direct members, no expiry or route grants.
- Sizes include the root. Builtin IPv4 collisions are skipped deterministically.
- Measure RSS after fixture creation, forwarding construction, and three unchanged refreshes.
- Assert peer count, retained records, and unchanged membership/authorization revisions.
- No concurrent review builds, sockets, DNS queries, or packets. Other host activity was not isolated.

The diagnostic prints a ledger fingerprint; all four samples at each size match.
Fixtures use test-only keys and are never connected to a network.

```bash
P2P_VPN_REVIEW_LEDGER_RECORDS=256 RUST_MIN_STACK=8388608 \
  cargo test --offline --locked --lib \
  runtime::forward::tests::measure_forwarder_signed_membership_resources \
  -- --ignored --exact --nocapture --test-threads=1
```

Repeat with 8 and 128. For cross-revision comparisons, apply the same diagnostic
to both revisions, compile first, and execute saved test binaries without builds
during sampling. [Raw samples](forwarder-resource-samples.json) include binary hashes.

## Results

Ranges cover the two fresh processes, in KiB. Growth subtracts each process's
post-fixture RSS from its post-refresh RSS; it is not an allocation counter.

| Records | Build | Post-Refresh RSS | Fixture-to-Refresh Growth | Three Refreshes (ms) |
| ---: | --- | ---: | ---: | ---: |
| 8 | Baseline | 24,884-25,392 | 5,344-5,412 | 755-756 |
| 8 | Current | 23,924-24,232 | 5,236 | 748-749 |
| 128 | Baseline | 25,700-25,880 | 6,016-6,232 | 12,126-12,352 |
| 128 | Current | 25,592-25,840 | 6,560-6,744 | 12,182-12,288 |
| 256 | Baseline | 26,424-26,560 | 6,244-6,256 | 24,251-24,594 |
| 256 | Current | 25,536-26,348 | 6,596-7,308 | 24,491-24,544 |

### Interpretation

- Raw current RSS is lower, but initial process RSS differs. This does not demonstrate a memory reduction.
- Growth is higher in current 128/256-record samples. Retaining the projection is not free.
- These samples cannot isolate map allocations from allocator retention, executable pages, or other forwarding state.
- Refresh timings are similar across revisions and scale with record count in this debug workload.

## Diagnostic Correction

An initial 512-record baseline attempt failed with `TooManyRecords { max: 256,
actual: 512 }`. The production limit was unchanged; the diagnostic guard was
corrected to use `MAX_MEMBERSHIP_RECORDS` for the maximum-size sample.

The eight valid 8/128-record samples were retained. The four 256-record samples
use rebuilt binaries with only that guard correction. Raw metadata records both
binary generations. The rejected attempt is not included as a successful sample.

## Follow-Up

| Work | Reason |
| --- | --- |
| Release-profile refresh measurement | Debug timings do not establish production CPU cost. |
| Unchanged-ledger refresh review | `prune_membership_records` currently merges an empty update and reevaluates the ledger. |
| Deadline-aware evaluation | Any optimization must preserve future activation, expiry, clock rollback, and pending policy refreshes. |
| Allocator/long-running daemon measurement | Two process samples do not establish exact retained-map cost, leaks, or idle CPU. |
| Android validation | No battery, JNI, or mobile lifecycle result is provided by this diagnostic. |

This is bounded signed-ledger scale evidence, not a production certification or
a substitute for the [idle comparison](idle-resource-comparison.md).

## Refresh-Window Follow-Up

A subsequent working change based on `60c83310` added deadline-aware timer reuse.
One fresh 256-record debug sample used the same diagnostic and ledger fingerprint.
No concurrent review build ran during this sample.

| Measurement | Value |
| --- | ---: |
| Construction | 4,064,476 microseconds |
| Three refreshes | 8,202,474 microseconds |
| Fixture RSS | 16,548 KiB |
| Constructed RSS | 22,716 KiB |
| Refreshed RSS / peak RSS | 24,204 KiB |

Construction uses wall-clock time; the first refresh at 1001 moves backward and
therefore reevaluates. The next two reuse the resulting window. This explains the
reduction from roughly 24.5 seconds for three full evaluations, not a faster evaluator.

Raw log: `/tmp/p2p-vpn-review-refresh-window-256.log`.
This single follow-up is not a replacement for the paired baseline samples,
a memory-improvement claim, or release-profile/Android evidence.
