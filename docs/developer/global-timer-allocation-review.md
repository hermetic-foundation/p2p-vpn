# Global Timer Allocation Review

## Hypothesis

The isolated AutoNAT diagnostic retains 224 more bytes after runtime teardown
when probes run. The cached futures-timer implementation keeps two heap/index
vectors in its process-global helper, without shrinking them on timer removal.

- On this 64-bit layout, the candidate item/index storage totals 56 bytes per capacity slot.
- A four-slot capacity increase would account for 224 bytes.
- This is a source-derived hypothesis, not established ownership of the AutoNAT residual.

## Frozen Workload

| Setting | Value |
| --- | --- |
| Source baseline | `7f2b448f` plus Linux test-only timer diagnostic |
| Allocator | Existing feature-gated instrumented System allocator |
| Execution | Fresh process, one test thread, no network or Tokio runtime |
| Initial warmup | One completed 1 ms timer, then 100 ms settling |
| Concurrent widths | 1, 4, 5, 8, 9, 16, 1, 16, 1, 16, 1, 16, 1, 16 |
| Delay / settling | 100 ms / 100 ms per width |
| Captures | Two fresh-process repetitions |
| Outer watchdog | 15 seconds plus two-second kill grace |
| Output cap | 128 KiB per capture |

## Evidence Rules

1. Measure after all delays complete and again after the fixed settling interval.
2. Preallocate result rows and serialize only after all checkpoints.
3. Report requested live bytes and blocks, not RSS or allocator capacity guesses.
4. Compare capacity-boundary steps and repeated equal/lower widths.
5. Keep any mismatch or failed capture; do not change widths or limits to match the hypothesis.

The helper is process-global and cannot be dropped through the public Delay API.
This diagnostic can establish retained high-water behavior, not independently
prove the AutoNAT residual's owner or complete whole-daemon leak analysis.

## Invocation

Run the cached feature-enabled library test executable:

```sh
timeout --signal=TERM --kill-after=2s 15 \
  prlimit --fsize=131072:131072 -- "$TEST_BINARY" \
  allocation_review::runtime::timer::measure_global_timer_high_water \
  --ignored --exact --nocapture --test-threads=1
```

- Build offline with at most two Cargo jobs; no build during measurements.
- Pre-build project temporary storage: 8,257,392 KiB.
- No production source, dependency configuration or cached dependency edits.
- If timer high-water steps match, test a separately documented AutoNAT prewarm intervention next.

## AutoNAT Intervention Manifest

The two timer captures show matching settled capacity steps: +224 bytes at five
concurrent delays and +448 at nine, with no new live blocks. Repeated equal/lower
widths add zero settled bytes. The AutoNAT intervention now tests this candidate.

- Use the existing 10-refusal/control diagnostic and its unchanged 25/30-second deadlines.
- Add optional `P2P_VPN_REVIEW_AUTONAT_TIMER_PREWARM=16`; absent or `1` preserves the original setup.
- Before the baseline snapshot, complete 16 concurrent 100 ms delays and settle for 100 ms.
- Preserve default AutoNAT security, request counts, connection lifecycle and all measurement checkpoints.
- Capture order: unprimed control/probe, primed control/probe/probe/control, unprimed probe/control.
- Fresh process and isolated loopback namespace per capture; 128 KiB output cap each.
- Compare after-runtime-drop probe minus control; expect the 224-byte differential to disappear only if this candidate explains it.
- Record prewarm mode explicitly; do not hide changed baseline allocation in an apparent production fix.

## Settled-Control Manifest

The eight initial intervention captures passed. Primed control/probe captures all
ended at 25048 bytes/70 blocks. Unprimed results also varied by 120 bytes/one block,
consistent with the standalone timer's visible asynchronous node-release interval.

- Retain the initial captures under `/tmp/p2p-vpn-autonat-prewarm-{1,16}-{control,probe}-{1,2}.log`.
- Add prewarm `4`, using exactly the same 100 ms delay and 100 ms settling as prewarm `16`.
- This changes timer capacity without changing the settling schedule; legacy prewarm `1` remains unchanged.
- Capture order: 4-control, 4-probe, 16-control, 16-probe, 16-probe, 16-control, 4-probe, 4-control.
- Prefix: `/tmp/p2p-vpn-autonat-settled-prewarm-`; same request counts, isolation, timeouts and log caps.
- Compare post-runtime residuals and actual reallocation deltas; do not discard the startup variation.

## Results

[Portable evidence](global-timer-allocation-results.json) retains both timer
captures, both eight-capture intervention matrices, executable fingerprints,
checkpoint values and raw/source hashes. All bounded captures passed.

### Timer Capacity

| Concurrent Delays | Settled Increment, Bytes | New Live Blocks |
| --- | --- | --- |
| 1 then 4 | 0 | 0 |
| First 5 | 224 | 0 |
| First 8 | 0 | 0 |
| First 9 | 448 | 0 |
| First 16 and subsequent alternating 1/16 | 0 | 0 |

Both captures agree after settling and finish in 2.81 seconds. Each capacity
increase uses two reallocations. Some immediate completion snapshots still own
120 bytes per timer node; those nodes are released before the settled snapshot.

### Matched AutoNAT Intervention

Each table entry agrees across two fresh-process repetitions. Values are above
the respective post-prewarm baseline, after both swarms and Tokio runtime drop.

| Prewarm Width | Control Bytes / Blocks | Probe Bytes / Blocks | Difference |
| --- | --- | --- | --- |
| 4, settled | 25048 / 70 | 25272 / 70 | 224 bytes, zero blocks |
| 16, settled | 25048 / 70 | 25048 / 70 | Zero bytes or blocks |

- Every probe run observed exactly ten requests and ten refusals on each side; controls observed zero.
- Probe reallocation count fell from 24 to 22 with width 16; control count stayed at seven.
- The intervention moves timer capacity allocation before accounting; it does not free the process-global cache.
- This attributes the isolated diagnostic's 224-byte differential to timer heap high-water capacity.
- No production timer prewarming or cache suppression is proposed.

The initial unprimed matrix also had a 120-byte/one-block baseline variation.
It is retained separately. Equal settling in the four-versus-sixteen comparison
removes that variation without changing probe behavior or measurement deadlines.

## Validation

| Gate | Result |
| --- | --- |
| Allocator calibration | Final executable passed |
| Negative guards | Invalid prewarm and host-namespace execution rejected with exit 101 |
| Feature-enabled library suite | 1149 passed, 15 opt-in tests ignored; isolated network namespace |
| Required Clippy groups | `--features allocation-review --lib --tests` passed; advisory warnings remain |
| Formatting | Changed Rust modules pass cached rustfmt |
| Source parity | Evaluated Nix build command passed with cached Cargo; outside Nix sandbox |
| Hermetic Nix build | Stopped when it attempted an uncached toolchain closure; not passed |
| Full workspace / Android rebuild | Not repeated: Linux test-only changes, no production code changed |

The first cached Cargo wrapper referenced a removed store path; the parity script
then passed using the existing cached `.cargo-wrapped` directly. No dependency
download or toolchain rebuild was completed for the parity check.

## Remaining Scope

This closes the isolated 224-byte residual question, not all retained memory.
Full-daemon reconnect steps, pressure allocation owners and the final requirements
audit remain open. Earlier Android and multi-network results are separate evidence.
