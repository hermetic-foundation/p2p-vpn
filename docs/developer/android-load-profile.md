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
