# Runtime Allocation Review

## Status

Both S7a ten-cycle captures pass on fixture `5a9cab0f`.
After first-use retention, every subsequent runtime teardown has zero additional
retained requested Rust bytes/blocks. Initial allocation owners remain unattributed.

It establishes a no-overlay-traffic baseline; it does not explain the connected
S1/S4/S5 RSS growth or replace connected transport attribution.

## Frozen Baseline

| Control | Value |
| --- | --- |
| Test | `allocation_review::runtime::measure_runtime_teardown_allocations` |
| Activation | Existing `allocation-review` library-test allocator only |
| Isolation | Fresh user/network namespace; loopback only, no external route |
| Overlay / infrastructure | Zero overlay peers; one unavailable loopback bootstrap plus five unreachable public defaults |
| Runtime | Production runner; two Tokio workers; synthetic idle packet reader |
| Listeners | Loopback TCP and owned packet UDP; ephemeral ports |
| Discovery | mDNS off; other discovery settings retained |
| Workload | Ten fresh runtime owners, each idle 30 seconds, within one process |
| Shutdown | Normal shutdown future wakes the synthetic reader; drop Tokio and require baseline thread count within two seconds |
| Repetitions | Two fresh processes using the same executable |
| Watchdog / output | 360 seconds / 2 MiB log per capture |
| Smoke | One two-second cycle, 60-second watchdog; not proof eligible |
| Global timer | Measure first `futures_timer::Delay` initialization separately; retain its one process-lifetime helper |

Record requested Rust bytes/blocks before construction, while active, after
runner return and after Tokio/thread teardown. Reserve observer storage before accounting;
serialize only after all cycles. Keep first-cycle initialization visible.

The thread baseline follows separately reported global timer initialization.
Require exactly one added helper at initialization and no additional threads
after each runtime cycle. This does not hide the first runtime allocation cycle.

## Decision Rules

- Retention after Tokio drop requires attribution; do not require zero before observing initialization.
- Increasing final-cycle live allocations require owner investigation and a reproduced cause.
- RSS growth with flat requested live allocations is not proof of a Rust owner leak.
- Active snapshots use independent atomics and can race; post-join deltas provide stronger evidence.
- This allocator excludes native allocator internals, kernel buffers and physical memory accounting.

## Commands

Build offline and run the existing allocator calibration before captures.
Use the cached executable directly; no builds during observations.

```sh
timeout --signal=TERM --kill-after=10s 360 \
  prlimit --fsize=2097152:2097152 -- \
  unshare --user --map-root-user --net \
  sh -c 'ip link set lo up && exec "$1" \
    allocation_review::runtime::measure_runtime_teardown_allocations \
    --ignored --exact --nocapture --test-threads=1' sh "$TEST_BINARY"
```

Set `P2P_VPN_REVIEW_RUNTIME_SMOKE=1` and use the 60-second watchdog for smoke.
For full captures unset the smoke override; preserve logs and executable hashes.
The command enforces the log-size limit in the kernel.

The synthetic reader blocks without generating packets. The shutdown adapter
wakes it with an interrupted-read result, allowing the production reader thread
to exit. This does not certify physical TUN blocking-read cancellation.

An initial guard incorrectly inspected inherited sysfs and rejected the isolated
namespace before startup. The corrected guard uses namespace-local
`/proc/net/dev`; no socket is created until only loopback is verified.

## Preliminary Thread Attribution

The corrected-isolation smoke failed its original thread gate: two before,
three after shutdown. The sole extra thread was `futures-timer`, parked in
`futex_do_wait`; 31,259 requested Rust bytes / 84 blocks remained.

- Evidence: `/tmp/p2p-vpn-runtime-allocation-smoke-evidence.log`, explicitly incomplete.
- Cached `futures-timer` 3.0.4: `native/timer.rs` installs a global fallback and forgets its helper.
- `native/global.rs` names that helper `futures-timer` and parks it between deadlines.
- The final fixture measures this initialization separately; retained runtime bytes remain under investigation.

## Fixture Validation

| Gate | Result |
| --- | --- |
| Allocator calibration | Same-binary grow/shrink/drop calibration passes |
| Isolated smoke | Complete in 2.01 seconds; one cycle, not proof eligible |
| Thread lifecycle | Two before timer initialization; three after initialization and runtime teardown |
| Timer initialization | 659 requested bytes / 11 blocks retained in this smoke |
| First runtime teardown | 30,752 requested bytes / 75 blocks retained; attribution open |
| Host guard | Exit 101 before any `runtime_started` event |
| Log budget | 11,183 bytes; kernel cap 2 MiB |
| Required Clippy | Feature-enabled workspace/all targets pass; advisory warnings remain |
| Offline locked workspace | 1507 passed; 40 opt-in tests ignored |
| Formatting | Both changed Rust files pass cached rustfmt |
| Cached Nix source check | Evaluated script passes outside sandbox; new source matches Linux and Android inputs |
| Android native build | Not repeated: the new module is Linux library-test-only |
| Formal models | No repository Lean/TLA/Alloy files located; production logic unchanged |
| Storage before builds | 8,742,820 KiB under `/tmp/p2p-vpn-*`, below 10 GiB |

Logs use `/tmp/p2p-vpn-runtime-allocation-` with suffixes
`final-smoke.log`, `calibration.log`, `final-host-rejection.log`,
`clippy.log`, `workspace.log` and `final-source-check.log`.

| Artifact | SHA-256 |
| --- | --- |
| Executable | `cbcd302dce3fcc22695d7cdb913aa614f5b9c2798c9d68541d37e09142b7c537` |
| Final smoke log | `6602bb23f4236c1a6f3caffcdaf0a559f20f786fc55fbc7e341f8efd85d4129b` |
| Calibration log | `4d374b97ab0a3daf93ef05fea5ee8080a59344ce36582c69622c82c7eb754f17` |
| Incomplete thread evidence | `e37055d9fc6ab37d7e748bdefc24eb5f7f8d719230c9c0cf85c6dbb3dc0ae003` |

## Remaining Work

1. Refine [component-stage attribution](runtime-initialization-review.md) to precise allocating owners.
2. Extend attribution to connected transport, pressure and repeated recovery owners.
3. Reconcile those findings with the observed RSS series and final resource audit.

## Paired Baseline Results

Both captures used the frozen 30-second dwell, ten cycles and 360-second watchdog.
No builds, manual cleanup, runtime changes or deadline adjustments overlapped
either capture. Both processes terminated and no matching test process remained.

| Observation | Run 1 | Run 2 |
| --- | ---: | ---: |
| Exit / elapsed seconds | 0 / 300.08 | 0 / 300.09 |
| Completed runtime cycles | 10 | 10 |
| First runtime post-drop bytes / blocks | 30,752 / 75 | 30,752 / 75 |
| Added retained bytes / blocks, each cycle 2-10 | 0 / 0 | 0 / 0 |
| Global timer initialization bytes / blocks | 659 / 11 | 659 / 11 |
| Threads after every runtime teardown | 3 | 3 |
| First post-drop RSS, KiB | 36,308 | 35,932 |
| Final post-drop RSS, KiB | 36,492 | 36,112 |
| Log bytes, 2 MiB cap | 104,660 | 104,660 |

The three remaining threads are the harness, selected test and global timer.
Runtime workers and the synthetic packet-reader thread retire in each cycle.
This is the synthetic reader's shutdown contract, not physical TUN certification.

| Cycle | Run 1 Post-Drop RSS, KiB | Run 2 Post-Drop RSS, KiB |
| --- | ---: | ---: |
| 1 | 36,308 | 35,932 |
| 2 | 36,452 | 36,080 |
| 3 | 36,460 | 36,084 |
| 4 | 36,472 | 36,084 |
| 5 | 36,484 | 36,100 |
| 6 | 36,492 | 36,112 |
| 7 | 36,492 | 36,112 |
| 8 | 36,492 | 36,112 |
| 9 | 36,492 | 36,112 |
| 10 | 36,492 | 36,112 |

RSS rises early despite zero net added requested Rust allocation in cycles 2-10,
then plateaus over the final five checkpoints. This separates requested live
allocation from resident pages; it does not identify native allocator ownership.

### Interpretation Limits

- No accumulating post-teardown Rust allocation was observed after the first cycle.
- The initial 30,752 bytes / 75 blocks are repeatable, but their precise owners remain open.
- The separately measured timer initialization is not subtracted from or hidden inside the runtime residual.
- Runner-return snapshots still include Tokio; only the later post-drop checkpoints support the teardown comparison.
- The global timer remains concurrent, so allocator counters are not transactional snapshots.
- Thirty-second runtime lifetimes do not exercise the default sixty-second public-discovery holdoff.

Default bootstrap records are retained, but infrastructure retry behavior and
connected packet paths require the other workloads. This baseline cannot close
S1/S4/S5 memory attribution, multi-network isolation or Android resource acceptance.

### Artifacts

[Portable samples](runtime-allocation-samples.json) retain every allocation phase,
RSS checkpoint, remaining thread name, runtime revision and log fingerprint.
Executable SHA-256 is the same as the validated smoke above.

| Log | SHA-256 |
| --- | --- |
| `/tmp/p2p-vpn-runtime-allocation-full-1.log` | `1334acdfca6ac8899749d64a0a329e30f8b00c148f943e9bcd3331395cf05151` |
| `/tmp/p2p-vpn-runtime-allocation-full-2.log` | `a4e669ebdff0dd060df77f1b5d4996eabe720dc0a85193f7e7fd1e630d6b335f` |

Both JSON summaries are complete and proof eligible for this baseline.
Publication is documentation-only; prior fixture validation is unchanged.
