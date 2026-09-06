# Android Lifecycle Review

Reviewed against `bb0c164b` on 2026-09-06. No service behavior has been changed by
this investigation. The lifecycle finding remains open.

## R11: Lost Cleanup

Priority: P2. Executor behavior reproduced; Android device impact unverified.

`onDestroy()` submits `stopNativeRuntime` to the service's single worker, waits
six seconds, then calls `shutdownNow()`. If existing work outlasts that wait,
the queued cleanup can be discarded before it runs.

| Evidence | Source |
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

Each service instance owns a separate executor, but native stop is not scoped to
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
the service passed a regression test. Replace this characterization with tests
against the actual lifecycle owner when implementing the fix.

## Ownership Plan

Introduce a platform-free lifecycle owner aligned with the process-global runtime.
Prefer serialized native lifecycle work with per-service scopes over independent
workers that can start and stop the same runtime concurrently.

| Invariant | Required Behavior |
| --- | --- |
| Retired admission | Ignore late callbacks and reject new work for a closed scope. |
| Pending timers | Cancel owned timers; running callbacks cannot rearm after closure. |
| Cleanup delivery | Schedule exactly one teardown; do not discard it on wait timeout. |
| Replacement isolation | Old teardown completes before replacement native work can run. |
| Service takeover | A replacement retires the old scope even if an old callback arrives late. |
| Main-thread behavior | Do not wait six seconds synchronously in `onDestroy()`. |
| Resource bounds | Do not create replacement threads for indefinitely blocked JNI work. |

Keep pairing operation IDs and per-network supervisor generations intact. Do not
alter JSON, Nix configuration, pairing protocols, or the JNI ABI without a concrete
need demonstrated by the ownership implementation.

## Validation Sequence

1. Test the real owner with blocked work, queued cleanup, takeover, late callbacks,
   timer rearming, and repeated closure using controlled executors and latches.
2. Integrate service destruction and all worker submissions with that owner.
3. Run the Java suite and compile/lint the complete Android app.
4. Exercise Android destroy/recreate, always-on recovery, and network transitions.

If a native call never returns, Java queue ownership alone cannot guarantee timely
teardown. Audit native termination bounds separately; do not hide that limitation
behind a second unbounded executor or a detached cleanup thread.
