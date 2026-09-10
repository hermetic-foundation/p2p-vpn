# Android Thread Sampling

## Interface

```sh
sh android-process-sample.sh PID SAMPLE_COUNT --threads
```

- Opt-in only; the two-argument process collector and default output remain unchanged.
- Existing count limit: 1-900, one-second sleeps between observations.
- Each row adds `thread_scan`: `listed`, `observed`, `skipped` and `threads`.
- At most 256 listed TIDs; an absent task directory or larger inventory fails the sample.

| Thread Field | Meaning |
| --- | --- |
| `tid`, `start_ticks` | Identity; compare both before calculating deltas |
| `user_ticks`, `system_ticks` | Per-thread CPU counters; use measured clock frequency |
| `voluntary_context_switches` | Scheduling proxy, not a count of hardware wakeups |
| `involuntary_context_switches` | Preemption proxy, not battery consumption |

## Quality Rules

- Read each thread's stat before and after status; skip changed or unreadable identities.
- Missing/malformed context-switch fields are `null`, never zero.
- Thread names, status text, arguments and paths are not emitted.
- Match TID and start time across rows. Do not subtract lifetime sums across different inventories.
- A scan is not atomic; threads can appear or disappear between enumeration and reads.
- Skipped tasks or null required counters make attribution incomplete for that interval.
- Thread scanning adds observer work; measure that overhead before comparing sensitive results.

## Frozen Compatibility Capture

| Setting | Value |
| --- | --- |
| Scenario | `process-thread-sample-smoke` |
| Baseline | `4f330ae0` plus optional collector and smoke changes |
| APK SHA-256 | `5f7dd26c6079c673cf46eea04d5a2e83e70265b3c33b4bc1796d81b77077f6a7` |
| Platform | Owned cached API 35 x86_64 emulator inside isolation wrapper |
| Work | Ten samples, no profile or deliberate VPN traffic |
| Acceptance | Stable process identity, all listed threads read, required counters present |
| Inner / outer watchdog | 160 / 180 seconds; 15 / 20 seconds kill grace |
| Runtime growth cap | 1,258,291,200 bytes |
| Pre-run storage | 9,144,928 KiB across `/tmp/p2p-vpn-*` |
| Output | `/tmp/p2p-vpn-android-thread-smoke-1` |

No build, public route, physical device or other emulator participates. This checks
Android shell/proc compatibility only. Sustained connected-thread attribution,
observer comparison and physical energy evidence are separate.

## Results

- Ten rows passed; PID 2073 stayed unchanged, with 19-20 listed and observed threads.
- No skipped threads or missing required counters; all six cleanup flags passed.
- Maximum scan duration: 130 ms. This is material collector work, not app scheduling latency.
- This first capture used a subshell per thread; the follow-up below removes that cost.
- [Portable results](android-thread-smoke-results.json) retain row metadata and first/last inventories.
- Evidence SHA-256: `528c400ecfb8f6cbe19eff73e4e1734e714216ca3c8eefb94cfc342e02b40dcd`.

## Verification

- Local tests: unchanged default fields, live threads, missing fields, vanished tasks and privacy.
- Deterministic FIFO fixture: replacement between stat reads is skipped, not accepted as continuity.
- Inventory boundaries: 256 accepted, 257 rejected; invalid mode rejected.
- ShellCheck and focused formatting passed after correcting the test's ShellCheck directive syntax.
- Nix structure derivation evaluated offline; the full structure matrix was not executed.
- No Java/native changes or builds; Android compatibility used the existing cached APK.

## No-fork Follow-up Manifest

- Baseline `2de47d24` plus in-process thread record assembly; schema and skip rules unchanged.
- Same cached APK, isolated emulator, ten-sample workload and 160/180-second watchdogs.
- Output: `/tmp/p2p-vpn-android-thread-smoke-2`.
- No default collector change; no production/runtime build or physical interaction.
- Compare scan duration descriptively across boots, not as a controlled CPU-overhead estimate.
- Retain the prior capture and its slower scanner as separate evidence.

## No-fork Results

- Pre-run storage: 9,145,004 KiB, below 10 GiB.
- Ten rows passed, each with 20 listed/observed threads and no skips; PID 2083 stayed unchanged.
- Eight scans took 20 ms; two took 30 ms. Previous capture maximum was 130 ms.
- All six cleanup flags passed. [Portable results](android-thread-no-fork-results.json) preserve evidence hashes.
- Collector SHA-256: `bdebe1c834a5f21cad746eb33c679625082755152b2fc50a566906ac2359b011`.
- Local collector tests, ShellCheck and focused formatting passed; Nix structure evaluated offline.

In-process record assembly removes the per-thread subprocesses without changing
the serialized fields or identity checks. Separate boots limit the timing
comparison; collector CPU and connected-workload interference remain unmeasured.
