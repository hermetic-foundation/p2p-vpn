# Android Load Profile

## Purpose

The sustained thread capture concentrates about 93.5% of sampled load CPU in two
threads. Native startup creates two Tokio workers, but creation order alone does
not identify those threads or explain their function-level cost.

## Frozen Manifest

| Setting | Value |
| --- | --- |
| Baseline | `97a761d5` plus opt-in load-smoke profiling harness |
| Scenario | `multi-network-resource-load-smoke` |
| Platform | Owned cached API 35 x86_64 emulator inside private isolation wrapper |
| APK | Unchanged debug APK from sustained thread capture |
| Recorder | Cached NDK 28.2 Simpleperf, Android x86_64 binary |
| Selection | App PID, existing threads only; userspace `cpu-clock:u` |
| Frequency / duration | 99 Hz / 60 seconds |
| Buffers | 64 kernel pages per CPU; 1 MiB userspace buffer |
| Output cap | 8 MiB; empty or capped data fails |
| Recorder watchdog | 75 seconds plus two-second kill grace |
| Harness watchdogs | Inner 440 + 15 seconds; outer 480 + 20 seconds |
| Traffic | Four 3000-request legs, 512-byte payload, 20 ms requested interval |
| Output | `/tmp/p2p-vpn-android-load-profile-1` |

## Invocation

Use the existing isolated Android E2E invocation, adding:

```sh
P2P_VPN_ANDROID_SIMPLEPERF_BINARY=/path/to/ndk/simpleperf/bin/android/x86_64/simpleperf
```

- The option is accepted only for load-smoke; default workloads remain unchanged.
- No call graphs, stack-memory dumps, kernel samples or system-wide profiling.
- No production changes, builds, downloads, physical devices or public WAN.
- Preserve failed traffic and recorder output; do not retry measurements or extend deadlines.
- Copy profile data before validation, then remove the owned guest profiling directory.

## Analysis Gates

1. Require normal admission, all four 3000/3000 traffic legs and normal cleanup.
2. Inspect recorder duration, lost samples and file size; a created file alone is not acceptance.
3. Report actual transport mix and profiler interference; this is not an unprofiled CPU baseline.
4. Resolve symbols against the existing unstripped JNI artifact; verify its executable sections match the packaged library.
5. Report unknown symbols and sampled thread identities; do not infer functions from thread order.

Recording starts immediately before traffic; it does not cover every final reply.
This short diagnostic attributes cost, not sustained stability or physical energy.
Raw profile data remains local and may contain synthetic app paths and thread names.

## Preflight

- Pre-run storage: 8,250,028 KiB; no competing emulator, fixture or build process.
- Existing APK, native symbols and fixtures are reused; no compilation required.
- Freeze source and recorder hashes with capture results; no source changes during recording.

## Attempt 1 Outcome

- Admission passed; the filesystem growth guard stopped setup before load/profiling.
- Exit status 75; no profile file or load measurement was produced.
- All six cleanup flags passed; post-cleanup project temporary storage was 8,251,928 KiB.
- No limit was increased and no failed measurement was retried.
- The guard monitors filesystem free space, not exclusively project-owned bytes.
- The exact source of transient growth was not retained; unrelated writes are possible, not proven.

| Artifact | SHA-256 |
| --- | --- |
| Evidence | `3b1b81c19c5c9ecbf8a9e78f20a1b80149c371ca6b688471af4406ccf452bd3d` |
| Resource helper | `e5d838008ee63a6a58cb30b0503d1f30f72f9888668a34b3b0fa5873c5b9db39` |
| Android Simpleperf | `570bf3bcebb261e98e20d523aa454d63a9dfb7c6580951dbef1297bf68f49c06` |
| Unstripped JNI library | `88bbe21ef0948b93b2116a5cb30f2d460bcf1662733ea2bdac8a6ba9587bc203` |

## Verification Status

- Mocked command bounds, invalid inputs, failed recorder/traffic and cleanup checks pass.
- Empty/capped output fails; partial output is retained after recorder or traffic failure.
- ShellCheck and formatting pass. No Java/Rust production source changed.
- Actual recorder permissions, symbol resolution and function attribution remain unverified.
- A fresh bounded attempt is required; this setup failure is not profiling acceptance.

## Attempt 2 Manifest

- Baseline `3a7ee380`; capture code, APK, recorder and fixtures unchanged.
- Output: `/tmp/p2p-vpn-android-load-profile-2`.
- Pre-run project temporary storage: 8,251,928 KiB; no emulator, fixture or build running.
- Free-space check over at least 30 seconds: 683578662912 to 683582525440 bytes available.
- Same 60-second profile/load, 8 MiB output cap, 440/480-second watchdogs and 1,258,291,200-byte growth guard.
- Preserve attempt 1; no measured traffic retry, deadline extension or manual runtime rescue.

## Attempt 2 Results

Capture and cleanup passed. [Portable results](android-load-profile-results.json)
retain the leading resolved native functions and hashes of the raw profile,
recorder log, address report and E2E evidence.

| Measurement | Result |
| --- | --- |
| Recording duration | 59.9909 seconds |
| Samples / reported lost samples | 4847 / 0 |
| Profile size | 2192902 bytes, below 8 MiB |
| Load traffic | Four legs, each 3000 sent / 3000 received |
| Overlay paths | Two TCP streams throughout sampled runtime states |
| Infrastructure peers | Zero or two private fixture peers, no public WAN |
| Resolved native samples | 4513, across 3732 sampled addresses and 2468 functions |
| Other samples | 334; Android system/JIT symbols not resolved |

### Busy Threads

- TIDs 2538 and 2539 are named `tokio-rt-worker` in this capture.
- Their native-library shares are 43.90% and 44.30% of sampled periods.
- Their additional libc shares are 3.26% and 2.50%; libc functions remain unresolved.
- TUN reader/writer native-library shares are 1.30% and 0.72%.
- Thread IDs are local to this capture, not identities carried over from earlier runs.

### Leading Native Functions

| Function, Abbreviated | Samples | Share of All Samples |
| --- | --- | --- |
| `core::ub_checks::check_language_ub` | 144 | 2.97% |
| `Atomic<usize>::load` | 48 | 0.99% |
| `Option<Ordering>::is_some_and` | 42 | 0.87% |
| `tracing::span::Span::log` | 31 | 0.64% |
| `usize::checked_mul` | 27 | 0.56% |

Cost is distributed across debug checks, atomics, tracing, generic helpers and
libp2p polling. No individual resolved function dominates. This is sampled
instruction attribution, not proof of which callers cause the work.

### Symbol Verification

1. Extract the x86_64 JNI library from the selected APK using `jar --extract`.
2. Its SHA-256 matches the stripped build artifact: `9430fcd79dba9ccd5d3290d031de750bc72cf8bd6f1a4d765da5eacb377e013c`.
3. Dump `.text` with `llvm-objcopy --dump-section .text=/dev/stdout LIBRARY /dev/null` and hash stdout.
4. Stripped and unstripped `.text` hashes both equal `02b77130855a780df2319a2fccd71c7a0cf79786d82fbff50ad347072c6e935e`.
5. Generate `simpleperf report --csv --raw-period -n --sort dso,vaddr_in_file -i profile.data`.
6. Resolve native `VaddrInFile` values with `llvm-symbolizer --output-style=JSON --obj=UNSTRIPPED_LIBRARY`.

- Address input/output equality was checked for all 3732 native rows; none resolved to an empty function.
- Aggregate CSV sample/period counts by resolved function; the portable report retains the leading 40.
- Native symbols lack a build ID and debug line sections. Direct APK `--symdir`/`--symfs` lookup did not resolve them.
- Local archive/symlink cache experiments were unsuccessful; address resolution used the original matching ELF.
- APK, recorder and runtime code were not changed for symbol analysis.

## Interpretation

The profile confirms the workload's CPU concentrates in native Tokio workers,
not the main UI thread or TUN reader/writer. It does not prove that debug-check
removal or any specific production optimization would be correct or sufficient.

- This is a 60-second profiled TCP diagnostic, not a repeat of the all-QUIC sustained workload.
- Recorder interference, missing system symbols and absent call chains limit causal conclusions.
- No production fix is justified solely by this profile; retained-allocation review remains open.
- Full release/platform acceptance and physical energy testing remain separate.
