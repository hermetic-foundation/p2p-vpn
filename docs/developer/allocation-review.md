# Allocation Review

## Scope

S6 supplements RSS with Rust allocator request accounting. It does not measure
allocator arena capacity, native-library allocations, kernel buffers or physical
Android energy use. Results remain separate from ordinary CPU benchmarks.

## Instrumentation

| Property | Control |
| --- | --- |
| Activation | `allocation-review` feature and library test build together |
| Normal binaries | No allocator override, including ordinary feature-enabled non-test builds |
| Allocator | `stats_alloc` 0.1.10 wrapping `std::alloc::System` |
| Unsafe code | Project prohibition unchanged; existing external allocator implementation is used |
| Live requested bytes | Cumulative allocated bytes minus cumulative deallocated bytes |
| Reallocation | Growth/shrinkage already included in byte counters; never add it twice |
| Observation | Fresh process, one selected ignored test, `--test-threads=1` |
| Reporting | Fixed-capacity observer storage allocated first; JSON created after all measured drops |

Counters describe requested Rust allocation sizes, not physical heap usage.
Snapshots are not transactional across threads. These controlled single-test
measurements must not be treated as exact concurrent whole-daemon heap accounting.

### Dependency Provenance

- Upstream: [stats_alloc](https://github.com/neoeinstein/stats_alloc), version 0.1.10.
- License: MIT, as declared by the upstream package manifest.
- Archive: [published crate](https://static.crates.io/crates/stats_alloc/stats_alloc-0.1.10.crate), 4476 bytes.
- SHA-256: `5c0e04424e733e69714ca1bbb9204c1a57f09f5493439520f9f68c132ad25eec`.
- Checksum matched the version's [registry index entry](https://index.crates.io/st/at/stats_alloc).
- Source, manifests, README, changelog and VCS metadata are unmodified; IDE files are omitted.
- A pinned development dependency uses the same vendored patch pattern as the existing Kademlia dependency.
- No transitive dependencies. Downloads were limited to 1200000 bytes/second.

The vendored development dependency keeps subsequent review builds offline.
Linux and Android source filters include its manifest/source so Cargo can
resolve the workspace even when this test feature is disabled.

## Frozen S6 Workload

| Setting | Value |
| --- | --- |
| Fixture | Existing deterministic Ed25519 membership-record generator; retain ledger fingerprint |
| Record counts | 8, 128, 256, including the explicit local root record |
| Cycles | Ten fresh forwarder construct / three refresh / drop cycles per capture |
| Cached mode | `refresh_membership_records` inside the current valid refresh window |
| Forced mode | `prune_membership_records` explicitly reevaluates unchanged records |
| Time | Construction's evaluated timestamp plus offsets 1, 2 and 3; no expiry transitions |
| Sequence per size | Cached, forced, forced, cached; fresh process each time |
| Overall sequence | Complete size 8, then 128, then 256; twelve captures total |
| Assertions | Peer count, identical records, unchanged membership/authorization revisions |
| Observations | Allocations, deallocations, reallocations, live byte/block deltas, RSS and phase time |
| Watchdog / logs | 600 seconds per case; at most 256 KiB per capture log |
| Builds | Finish before observations; same executable for all twelve captures |

Preserve the first cycle and any persistent initialization allocations.
Increasing post-drop live bytes require attribution; growing RSS with released
Rust allocations must not be mislabeled as retained forwarder objects.

The historical RSS-only test uses fixed timestamps near 1000. Those can precede
construction's refresh window and trigger reevaluation; they are not a matched
baseline for S6's explicitly cached mode.

## Commands

```bash
cargo test --offline --locked --lib --features allocation-review --no-run
cargo test --offline --locked --lib --features allocation-review \
  allocation_review::allocator_counts_growth_shrinkage_and_drop_without_double_counting \
  -- --ignored --exact --test-threads=1
```

Execute the built library-test binary directly for captures, outside build jobs:

```bash
P2P_VPN_REVIEW_LEDGER_RECORDS=128 \
P2P_VPN_REVIEW_LEDGER_EVALUATION=cached \
timeout 600 "$TEST_BINARY" \
  runtime::forward::tests::measure_forwarder_signed_membership_allocations \
  --ignored --exact --test-threads=1 --nocapture
```

Use `forced` for the alternate mode. Each capture emits one JSON object after
the `forwarder_allocation_sample` marker. Preserve the full log and executable
hash with the parsed result; a successful test alone is not a no-leak finding.

## Status

Instrumentation calibration passed for allocate, grow, shrink and drop.
The first eight-record cached capture completed in 1.47 seconds: all ten
post-drop live-byte deltas were zero, and all cached refreshes allocated zero.

That first capture used a 120-second watchdog. Before any larger capture or
timeout failure, the prospective limit was set to 600 seconds. Historical debug
results took about 24 seconds for three 256-record reevaluations; ten cycles
plus construction need a larger measurement budget. No functional recovery
deadline was changed.

- First log: `/tmp/p2p-vpn-allocation-s6-8-1-cached.log`.
- Executable SHA-256: `509996713072b3089f29565260f22337d452528c22c2d7623a0c9312d4b89805`.
- RSS after drop rose from 23032 to 23036 KiB despite zero retained Rust-byte deltas.
- [Raw samples and hashes](allocation-review-samples.json) preserve six of twelve planned captures.
- All four eight-record captures passed: 40 cycles, zero post-drop live byte/block deltas.
- Eight-record cached refreshes allocated zero; three forced refreshes made 3171 allocations per cycle.
- The first 128-record cached capture passed in 23.19 seconds: ten zero-retention cycles, zero refresh allocations.
- Its post-drop RSS rose from 23984 to 24036 KiB, then settled; this is not retained Rust requested bytes.
- The first 128-record forced capture passed in 155.04 seconds: ten zero-retention byte/block cycles.
- Three forced refreshes made 47535 allocations per cycle; post-drop RSS rose 24 KiB before settling.
- Six remaining captures and packet allocation attribution are pending.

### Validation After Dependency Packaging Correction

| Gate | Result |
| --- | --- |
| Locked offline metadata | Exactly three intended workspace members; vendored allocator excluded |
| Instrumented library build | Passed; executable hash unchanged |
| Locked offline workspace tests | 1497 passed, 37 ignored, no failures |
| Feature-enabled workspace Clippy | Correctness, suspicious and performance groups passed; warnings retained |
| Changed Rust formatting | Passed |
| Cached evaluated Nix source check | Passed; repository/Linux test-target parity and Linux/Android vendor parity |
| Filtered Rust/manifests | Changed files identical in Linux and both Android source inputs |
| Upstream vendor comparison | All six retained files match the published archive |
| Temporary storage before builds | 8699656 KiB, below 10 GiB |

Validation logs use `/tmp/p2p-vpn-allocation-final-*.log`. The evaluated source
check log is `/tmp/p2p-vpn-allocation-source-check.log`; its artifacts are in
`/tmp/p2p-vpn-allocation-source-check.p4vn484W`.

The source check ran the evaluated assertions with cached Cargo and strict
shell error handling, not a sandboxed Nix package build. No Android cross-build
was run: all Rust changes are test-only; Android source inclusion was checked.
Whole-runtime and Android background measurements remain separate open work.

The initial lockfile regeneration encountered unrelated yanked cached versions.
Adding only the local dependency preserved all existing pins; no registry
dependency upgrade was made. Logs are retained under `/tmp/p2p-vpn-allocation-*`.
