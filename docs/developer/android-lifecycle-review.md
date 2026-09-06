# Android Lifecycle Review

Originally reviewed against `bb0c164b` on 2026-09-06. The service now uses a
process-wide scoped dispatcher. Current-source emulator always-on validation passed;
same-process replacement with stalled JNI remains outstanding.

## R11: Lost Cleanup

Priority: P2. Executor defect reproduced and fixed; device impact unverified.

Previously, `onDestroy()` submitted `stopNativeRuntime` to the service's worker, waited
six seconds, then called `shutdownNow()`. If existing work outlasted that wait,
the queued cleanup can be discarded before it runs.

| Historical Evidence | Source at `bb0c164b` |
| --- | --- |
| One executor per service instance | `P2pVpnService.java:148` |
| Submit cleanup, bounded wait, forced shutdown | `P2pVpnService.java:253-259` |
| Cleanup calls global native stop | `P2pVpnService.java:490-498` |
| Process-global runtime storage | `crates/p2p-vpn-android/src/lib.rs:947` |
| Stop takes the current global instance | `crates/p2p-vpn-android/src/lib.rs:2191-2196` |
| Start stops and later replaces the instance | `crates/p2p-vpn-android/src/lib.rs:1780,1934` |

The wait also occurs synchronously in the lifecycle callback. Its effect on real
device responsiveness has not been measured; this is not a claimed ANR reproduction.

## Why Graceful Shutdown Is Insufficient

Previously each service instance owned a separate executor, but native stop is not scoped to
that service instance. Simply retaining old queued cleanup allows a different
ordering: a replacement starts first, then old cleanup stops the replacement.

Existing Rust network-slot generations protect individual supervised networks.
They do not identify the Java service owner of the process-global native runtime.

## Reproducer

Run the [standalone JVM reproducer](repro/AndroidShutdownRepro.java) with Java 17:

```sh
java -Xmx64m -XX:ActiveProcessorCount=2 \
  docs/developer/repro/AndroidShutdownRepro.java
```

Observed with Nix OpenJDK 17.0.20+8:

```text
discarded_cleanup=1 cleanup_ran=false late_work_rejected=true
graceful_old_cleanup_removed_generation=2 replacement_running=false
```

The first scenario uses the service's six-second wait and an intentionally blocked
worker that does not finish on Java interruption. Latches establish ordering;
every worker is released and joined before the reproducer exits.

The second scenario models global stop with an atomic runtime identifier. It
demonstrates why deferred unscoped cleanup is unsafe, not an executed JNI race.
Neither scenario invokes Android framework callbacks or the real native runtime.

A successful exit means these failure orderings were reproduced. It does not mean
the service passed a regression test. The reproducer is retained as historical
evidence; `ServiceRuntimeWorkerTest` tests the replacement implementation.

## Implemented Ownership

`ServiceRuntimeWorker` serializes native lifecycle work on one process-wide worker.
Opening a service scope retires its predecessor. Retirement cancels pending tasks
and queues exactly one cleanup before any replacement work.

`onDestroy()` closes admission without waiting. Posted snapshots, notifications,
and stop requests check scope liveness on the main thread. Final pairing cleanup
runs after in-flight service work, preventing late lock acquisition from leaking.

| Invariant | Required Behavior |
| --- | --- |
| Retired admission | Ignore late callbacks and reject new work for a closed scope. |
| Pending timers | Cancel owned timers; running callbacks cannot rearm after closure. |
| Cleanup delivery | Schedule exactly one teardown; do not discard it on wait timeout. |
| Replacement isolation | Old teardown completes before replacement native work can run. |
| Service takeover | A replacement retires the old scope even if an old callback arrives late. |
| Main-thread behavior | Do not wait six seconds synchronously in `onDestroy()`. |
| Resource bounds | Do not create replacement threads for indefinitely blocked JNI work. |

Pairing operation IDs, network supervisor generations, configuration, protocols,
and JNI signatures are unchanged. Closed scopes drop late submissions rather
than throwing into Android callbacks. The worker can time out when idle.

## Regression Evidence

Five owner tests cover blocked-work cleanup, replacement ordering, late close,
timer rearming, cancellation retention, and cleanup after task failure.
The complete Android Java unit suite and lint passed with cached dependencies.
Debug APK assembly also passed; this local Gradle build had no JNI libraries,
so it is Java/resource packaging evidence, not a deployable native VPN validation.

These owner tests are separate from the current-source always-on emulator run below.
Same-process destroy/recreate with stalled JNI and network transitions remain to be exercised.

## Validation Sequence

1. Test the real owner with blocked work, queued cleanup, takeover, late callbacks,
   timer rearming, and repeated closure using controlled executors and latches.
2. Integrate service destruction and all worker submissions with that owner.
3. Run the Java suite and compile/lint the complete Android app.
4. Exercise Android destroy/recreate, always-on recovery, and network transitions.

If a native call never returns, Java queue ownership alone cannot guarantee timely
teardown. Audit native termination bounds separately; do not hide that limitation
behind a second unbounded executor or a detached cleanup thread.

## Native Termination Audit

Source inspection at `3fab5e16`; these are configured limits, not measured deadlines.
Paths below are relative to `crates/p2p-vpn-android/src/`.

| Component | Mechanism | Source |
| --- | --- | --- |
| Control RPC | Five-second async timeout | `lib.rs:59`, `CONTROL_TIMEOUT` |
| TUN reader | Nonblocking descriptor, stop flag, 250 ms poll | `lib.rs:1227` |
| TUN writer | Stop flag and 250 ms total write poll budget | `lib.rs:1271` |
| Network attempt | Four-second graceful stop, then abort with one-second wait | `lib.rs:2141` |
| Async runtime | One-second shutdown timeout after supervisors finish | `lib.rs:1912` |
| Native stop | Signals all supervisors, sets TUN stop, closes queues, joins threads | `lib.rs:2192` |

`PacketSwitch::write_next` holds a network's state lock during TUN writes.
Queue closure can wait for that lock; the Android writer's polling budget matters
to shutdown as well as packet backpressure (`supervisor.rs:284`).

The supervisor thread waits for all network supervisors before shutting down Tokio.
Async timeouts require scheduler progress, and `thread::join` has no timeout here.
Do not add the constants together and claim a hard native-stop deadline.

## Emulator Readiness

On 2026-09-06, the cached Nix harness passed boot and always-on preflight.
KVM, launcher, ADB, and scenario prerequisites were available. No emulator was
started, and no current-source JNI lifecycle scenario was run.

| Evidence | Location |
| --- | --- |
| Boot preflight | `/tmp/p2p-vpn-review-android-preflight/evidence.json` |
| Always-on preflight | `/tmp/p2p-vpn-review-android-always-on-preflight/evidence.json` |
| Cached harness | `/nix/store/sq4xzqlac96snx4dsglbj41dbga2c356-p2p-vpn-android-e2e` |

An offline, substitution-disabled dry run for current `.#android-e2e-runtime`
planned 1,479 builds. None were started. This is not the plan with public caches
enabled and does not establish that those inputs must be built from source.

Before lifecycle E2E, obtain a bounded current-source JNI build and package it with
the current Java code. Verify artifact provenance; the cached harness points to
older APK and fixture outputs and cannot certify the new service owner unchanged.

## Current-Source Always-On Run

Passed on API 35 x86_64 from 12:55:11 to 12:56:56 UTC, 2026-09-06.
Application sources were `bb973229`; the harness additionally installed the selected
APK before scenario checks instead of trusting the cached launcher's older APK.

| Scenario | Result |
| --- | --- |
| Encrypted profile creation and manual native connection | Passed |
| Always-on ownership and ignored manual disconnect | Passed |
| APK replacement starts a fresh process with the same identity | Passed |
| Unsupported lockdown stops the native runtime | Passed |
| Removing lockdown autonomously restores the runtime | Passed |
| Clear always-on settings, stop emulator, remove private state | Passed |

Evidence: `/tmp/p2p-vpn-review-android-always-on-current/evidence.json`.
This peerless scenario does not prove packet transport, underlay recovery, or
same-process service replacement while JNI is deliberately stalled.

### Build Provenance

- Rust 1.97.1, installed Nix Rust sources and NDK 28.2.13676358, Android API 26 linker.
- Offline `build-std=std,panic_abort`, locked dependencies, two jobs, no debug symbols.
- Reused `/tmp/p2p-vpn-android-target`; 1.4 GiB after the build, no downloads.
- Offline Gradle assembly used current Java plus only the new x86_64 JNI library.
- This is a development APK, not verification of the dual-ABI Nix release derivation.

The evaluated `android-e2e-structure` check body also passed using installed tools
outside the Nix sandbox. It includes the selected-APK ordering assertion and mocked
transport cases; those mocks are not additional real-network transport evidence.

ShellCheck and Bash syntax checks passed. `nixfmt --check flake.nix` failed on both
the working file and its committed parent; unrelated formatting was left unchanged.

SHA-256 of the JNI library before Gradle stripping:

```text
6078db6d37d457c146e789b279bf199011390251a4df2ee2442911d10beb2b26
```

SHA-256 of the tested APK:

```text
d5959da94fe859220da702f5b608817dc3d26b81e491189e45141064a5d77df1
```
