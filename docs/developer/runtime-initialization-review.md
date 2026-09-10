# Runtime Initialization Attribution

## Purpose

The [paired runtime baseline](runtime-allocation-review.md#paired-baseline-results)
retains 30,752 requested Rust bytes / 75 blocks after first use, with no added
retention in later cycles. This diagnostic narrows that residual by component.

## Frozen Diagnostic

| Setting | Value |
| --- | --- |
| Test | `allocation_review::runtime::initialization::measure_runtime_initialization_components` |
| Isolation | Fresh user/network namespace, loopback only; guard before networking |
| Allocator | Existing library-test-only `stats_alloc` feature |
| Preparation | Fixed identity/configuration and reserved report storage before accounting |
| Timer | First global timer initialization recorded separately |
| Sequence | Tokio only, Noise config, QUIC config, node construction; ten repetitions in one process |
| Runtime | Fresh two-worker Tokio runtime for each stage; full drop after component drop |
| Node | Same startup defaults as the baseline, before the public-discovery holdoff; no overlay peers |
| Snapshots | Before stage, immediately after runtime drop, and after fixed 100 ms settling |
| Budget | 60-second watchdog; kernel-enforced 2 MiB log cap |
| Repetition | Two fresh processes using one executable hash |

This is ordered attribution, not independent component sizing. A prior stage may
initialize state reused by later stages. Constructor-only results do not measure
connection establishment, packet traffic or the full runner shutdown path.

## Command

```sh
timeout --signal=TERM --kill-after=10s 60 \
  prlimit --fsize=2097152:2097152 -- \
  unshare --user --map-root-user --net \
  sh -c 'ip link set lo up && exec "$1" \
    allocation_review::runtime::initialization::measure_runtime_initialization_components \
    --ignored --exact --nocapture --test-threads=1' sh "$TEST_BINARY"
```

## Interpretation

- Retained byte/block deltas narrow the allocating stage; they do not identify individual call stacks.
- Difference between immediate and settled samples can indicate deferred cleanup.
- Increasing retention across repetitions requires investigation, not a larger settling budget.
- Preserve first-use allocations and all negative deltas; do not average them away.
- No production behavior or protocol changes are justified by an unverified attribution hypothesis.

Both captures passed with no overlapping builds. Full samples, source hashes and
log fingerprints are in [portable evidence](runtime-initialization-samples.json).

## Results

| First-Cycle Stage | Run 1 Retained Bytes / Blocks | Run 2 Retained Bytes / Blocks |
| --- | ---: | ---: |
| Tokio construction/drop | 24,528 / 67 | 24,648 / 68 |
| Noise configuration/drop | 400 / 2 | 400 / 2 |
| QUIC configuration/drop | 0 / 0 | 0 / 0 |
| Node construction/drop | 5,704 / 5 | 5,704 / 5 |
| Sum of ordered stages | 30,632 / 74 | 30,752 / 75 |

Every stage in cycles 2-10 retained zero additional requested Rust bytes and
blocks. Immediate and 100 ms settled byte/block deltas agree in both captures.
The separate global timer initialization remains outside these stage totals.

Each capture passed in 4.08 seconds, with 40 observations and a 16,531-byte log.
No matching test process remained after completion. Neither capture establishes
connected transport, packet-buffer or sustained background resource behavior.

### Attribution Boundary

Most initial retention occurs without constructing a VPN node: the Tokio-only
stage accounts for about 24 KiB. A 120-byte / one-block difference between
processes remains visible; it is not averaged away or assigned a guessed owner.

Cached Tokio 1.53.1 source provides a candidate explanation: its signal registry
is process-global through `OnceLock`, and Unix initialization allocates signal
records containing watch channels. These captures do not isolate that registry.

- Measured: initialization stage and lack of accumulation across repeated drops.
- Not yet proven: precise static/TLS/cache owners within each stage.
- Not justified: treating the node-stage residual as a leak or changing production limits.
- Not equivalent: summing ordered controls and claiming exact reconciliation with full-runner behavior.

## Validation

| Gate | Result |
| --- | --- |
| Allocator calibration | Same-binary growth/shrink/drop test passes |
| Diagnostic captures | Two passes, 80 stage observations total |
| Existing runtime smoke | Pass, 2.02 seconds; unchanged one-cycle criteria |
| Host isolation guard | Expected exit 101 before component construction |
| Offline locked workspace | 1507 passed; 40 opt-in tests ignored |
| Required Clippy | Feature-enabled workspace/all targets pass; advisory warnings retained |
| Formatting | Changed Rust files pass cached rustfmt |
| Cached Nix source parity | Evaluated script passes outside sandbox; Linux/Android source directories match |
| Android native compilation | Not repeated; change is Linux library-test-only |
| Storage before build | 8,744,592 KiB under `/tmp/p2p-vpn-*`, below 10 GiB |

No production logic, dependency version or public protocol changed.
Validation logs use `/tmp/p2p-vpn-runtime-initialization-` followed by
`calibration.log`, `baseline-smoke.log`, `host-rejection.log`,
`workspace.log`, `clippy.log` and `source-check.log`.

Executable SHA-256:
`efd48bfeebb8b1629befb090aad4678340635ce8030ee472632dabe5176c2027`.
