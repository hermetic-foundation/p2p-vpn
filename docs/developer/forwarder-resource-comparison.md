# Forwarder Membership Resources

## Scope

Measured on 2026-09-06. The original debug comparison pairs `f24831fa` with `44da11bd`, which retains the
effective-membership projection for daemon DNS to borrow. Neither build changes
the signed-record limit or wire protocol.

The [release follow-up](#release-profile-follow-up) compares the same baseline
with `89709e4f`, including deadline-aware evaluation reuse and later runtime fixes.

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
| Release-profile refresh measurement | Measured below; this diagnostic is not whole-daemon CPU or sustained-load evidence. |
| Unchanged-ledger refresh review | Deadline-aware reuse is implemented; the follow-ups below measure skipped evaluations. |
| Deadline-aware evaluation | Time-boundary, rollback, and pending-notification regressions pass; see the [verification map](review-verification.md). |
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

## Release-Profile Follow-Up

### Method

- Baseline `f24831fa`; current `89709e4f`; identical manifests, lockfile, and diagnostic.
- Nix Rust 1.97.1; Cargo release defaults; incremental compilation disabled; two build jobs; offline.
- Baseline workspace adds only the current diagnostic. No baseline runtime behavior was modified.
- Save current executables, clean only package release outputs, rebuild baseline against cached dependencies.
- Two fresh processes per revision and size; baseline/current order reversed for the second round.
- No task builds during capture. Other host activity was not isolated; timings are wall-clock durations.

All 12 tests passed. Each size's four ledger fingerprints match, including the
earlier debug fixtures. [Raw samples](forwarder-release-resource-samples.json)
record executable and manifest hashes, revisions, memory readings, and timings.

### Results

Ranges cover two samples. RSS and growth are KiB; durations are milliseconds.
Growth subtracts post-fixture RSS from post-refresh RSS, not allocator usage.

| Records | Build | Construction (ms) | Three Refreshes (ms) | Post-Refresh RSS | Fixture-to-Refresh Growth |
| ---: | --- | ---: | ---: | ---: | ---: |
| 8 | Baseline | 0.659-0.661 | 3.520-5.189 | 10,504-10,568 | 1,384-1,512 |
| 8 | Current | 0.628-0.660 | 1.118-1.397 | 10,312-10,608 | 1,260-1,420 |
| 128 | Baseline | 10.825-11.381 | 59.017-60.754 | 11,312-11,408 | 1,904-1,908 |
| 128 | Current | 10.347-10.391 | 19.390-19.775 | 11,452-11,500 | 2,060-2,124 |
| 256 | Baseline | 22.757-23.908 | 122.730-125.537 | 12,188-12,204 | 2,708-2,772 |
| 256 | Current | 21.901-23.385 | 40.297-41.786 | 12,192-12,248 | 2,760-2,780 |

### Interpretation

- At 256 records, current refresh duration is approximately one-third of baseline: two evaluations are skipped.
- The first refresh moves backward from construction wall-clock time to 1001 and reevaluates; 1002 and 1003 reuse the window.
- Construction still evaluates the ledger. These results do not claim faster signature verification.
- Current 128-record RSS growth is higher; 256-record growth ranges overlap. No memory reduction is established.
- Two samples do not establish a latency bound, exact retained allocations, long-running leaks, or public-discovery cost.
- This optimized test executable is not a packaged daemon or Android battery measurement.

### Reproduction

```bash
CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 \
  cargo test --release --offline --locked --lib --no-run
P2P_VPN_REVIEW_LEDGER_RECORDS=256 RUST_MIN_STACK=8388608 \
  /path/to/saved-libtest \
  runtime::forward::tests::measure_forwarder_signed_membership_resources \
  --ignored --exact --nocapture --test-threads=1
```

Repeat at 8 and 128 records with both prebuilt executables. Add only the identical
diagnostic to the baseline; preserve its runtime source. Record binary hashes
before sampling and do not compare debug and release timings as a revision change.
