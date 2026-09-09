# Android Lifecycle Ownership Review

## Scope

Source reviewed at `6dbb680c` on 2026-09-06. A read-only reviewer identified
the cases below; parent inspection checked the relevant control flow.
The initial source review preceded regression work. A2 is now reproduced and
fixed with JVM and native-failure emulator coverage. A1 and A3 are fixed with
emulator instrumentation. Broader platform and resource gates remain open.

The [bounded Android lifecycle audit](android-lifecycle-audit.md) completes
current-source reconciliation at `518929b3`, including reruns of A1-A3 and
additional cancellation, binding and permission-recovery fixes AL1-AL4.

Successful workflow tests do not exercise every event ordering. Unresolved cases
remain open even when the [network workflow](android-network-workflow-review.md) or
[multi-network scenario](android-multi-network-review.md) passes.

## Findings

| ID | Priority | Ownership gap | Source at reviewed revision |
| --- | --- | --- | --- |
| A1 | P1 | Deferred service stop is not fenced against a newer start in the same instance | `P2pVpnService.java:1483`, `:2323` |
| A2 | P1 | Non-lockdown manager event cancels connected health polling without rearming it | `P2pVpnService.java:295`, `:626` |
| A3 | P2 | Connect intent deferred during profile join is not resumed by join completion | `P2pVpnService.java:271`, `:1242` |

Paths are relative to `android/app/src/main/java/org/hermeticfoundation/p2pvpn/`.
Priorities reflect potential impact, not measured occurrence rates.

### A1: Superseded Stop

1. Disconnect posts a stop callback to the main handler.
2. A newer connect start establishes foreground ownership in the same service.
3. The old callback invokes `stopForeground` and unconditional `stopSelf`.

`postIfActive` checks whether the service worker scope is closed. A newer request
within that scope does not close it. `finishPairingForegroundService` posts the
same unversioned stop effect.

Required regression: hold main callbacks, accept a newer start, then drain the
old stop. Foreground and started-service ownership must remain with the newer request.
Exercise manual disconnect and asynchronous pairing completion separately.

#### Implemented Start Ownership

Each main-thread `onStartCommand` admits a fresh identity and queues a worker
marker before its command. Stop requests capture the processed worker identity;
their main-thread effect is ignored if a newer start has been admitted.

This separates accepted starts from processed starts. Reading the newest identity
on the worker would incorrectly authorize an old stop emitted while the newer
start is still queued. Service-scope retirement remains a separate check.

Manual, pairing-completion, and missing-permission stops now share this check.
No protocol, persisted profile, JNI signature, or configuration format changes.

| API 35 x86_64 instrumentation | Result |
| --- | --- |
| Before fix | Old manual stop removed foreground ownership after newer connect admission |
| Stop posted before admission | All three stop paths preserve the newer foreground service |
| Start admitted before old worker posts | All three paths preserve the newer foreground service |
| Current stop after each case | Foreground ownership is removed normally |
| Combined lifecycle run | Deferred joins and occupied-worker replacement also pass |

Instrumentation holds the main thread and invokes the real `onStartCommand`
callback synchronously to control ordering. Android foreground state and native
startup are real; this does not test delivery of a pending system Binder start.

- Logs: `/tmp/p2p-vpn-review-stop-owner-{before,after}.txt`.
- Final runner: `superseded_stop=passed`, `passed=true`, result code `-1`.
- Unit tests, lint, debug APK, and instrumentation APK assembly passed offline.
- Add `-e superseded_stop true` to the [lifecycle runner](android-lifecycle-review.md#run-it).

| Tested artifact | SHA-256 |
| --- | --- |
| App APK | `846aaa646a4b2e67ecfbc6b3361d29bc808151a2ff6100aa6bc67443805d39a5` |
| Instrumentation APK | `ab67464e44b27c6bb2482a2fad3c3e04b8a62fc635c5828bb67b9b02f0ce152d` |

### A2: Lost Health Timer

1. A connected runtime has a scheduled `statusFuture`.
2. A non-lockdown VPN-manager event cancels and clears that future.
3. The connected branch publishes a snapshot but schedules no replacement.

`pollNativeStatus` normally rearms itself and detects native failure. Cancelling
its pending invocation removes that periodic path until another action schedules it.
An already-running poll is a different ordering and must be tested separately.

Required regression: deliver the mode event before a pending poll fires, advance
a controlled scheduler, and verify recurring status reads. Inject native failure
afterward and require the ordinary bounded recovery path.

#### Implemented Fix and Evidence

The connected non-lockdown branch now calls `scheduleStatusPoll`. The existing
delay and cancellation owner are reused; lockdown behavior is unchanged.

| Check | Result |
| --- | --- |
| Before fix | Real service mode handler cancelled the pending timer without replacement; JVM assertion failed |
| After fix | Three consecutive manual-mode events each retain a live poll and cancel superseded timers |
| Queue bound | At most the executing test task and one pending poll remain in the scope |
| Android build | Unit suite, lint, debug APK, and instrumentation APK assembly passed offline |

`ServiceHealthPollingTest` invokes the service handler on its real scoped worker.
Android methods use the existing JVM stubs; the test checks pending ownership
before the poll executes, not recurring JNI reads or native-failure recovery.

- Before log: `/tmp/p2p-vpn-review-health-poll-before.log`.
- After log: `/tmp/p2p-vpn-review-health-poll-after.log`.
- The subsequent platform case below covers recurring JNI reads and native-failure recovery.
- This fix is not evidence that the separate multi-network underlay failure is resolved.

#### Native Health Recovery Instrumentation

Passed on 2026-09-06 using API 35 x86_64, shared runtime `4b90f3bc`, and rebuilt JNI.
The case runs in the existing isolated-emulator lifecycle instrumentation APK;
no fault-injection hook was added to the production app.

| Stage | Checked Invariant |
| --- | --- |
| Three manager callbacks | Old poll cancelled; a distinct live poll remains; runtime generation unchanged |
| Three ordinary polls | Each performs JNI status work and rearms itself without further manager callbacks |
| Native stop | Worker calls real `nativeStop`; Java remains connected with connection intent and health polling intact |
| Failure detection | Ordinary poll clears connected state, records health failure, and schedules one native-runtime recovery |
| Automatic recovery | Generation increases exactly once; peer identity and enabled networks survive; backoff resets and polling resumes |
| Existing lifecycle cases | Deferred join, three superseded-stop paths, occupied-worker replacement, and native cleanup pass |

Removing the connected branch's `scheduleStatusPoll()` reproduced
`manager event lost health polling`, with `passed=false` and result code `0`.
After restoring the branch and rebuilding, the combined scenario passed again.

- Initial pass: `/tmp/p2p-vpn-review-health-poll-instrumentation.log`.
- Negative control: `/tmp/p2p-vpn-review-health-poll-mutation.log`.
- Final pass: `/tmp/p2p-vpn-review-health-poll-final-instrumentation.log`.
- Final Gradle unit, lint, app, and instrumentation checks passed offline.
- One disposable emulator; app data cleared only there between runs; emulator stopped and temporary state removed.

| Final Artifact | SHA-256 |
| --- | --- |
| Debug APK | `51271da4c14072954ebe64ab3ab2f15d9ac4aecde1870e64fb32590a6ebb19e3` |
| Instrumentation APK | `275ebdee2f8f2bbc8d07e69af1840acdb868b2714310124ee1ee3bf5209be640` |
| Unstripped x86_64 JNI | `1873e08e5f6f010d380e22b0185b8f0e0cd30a691f7a68154f1104ffb5d74ced` |

Add `-e health_poll true` to the [lifecycle runner](android-lifecycle-review.md#run-it).
Require `health_poll=passed`, `passed=true`, and `INSTRUMENTATION_CODE: -1`;
ADB may return success even when the test fails.

Callbacks enter the real service on the main thread but are synthesized by the
test, not emitted by Android's VPN manager. Native stop is graceful fault
injection, not a crash. This case does not measure packet traffic or battery use.

The test establishes one automatic recovery, not retry exhaustion. Production
backoff caps the retry delay, not the total retry count. The earlier underlay
failure still lacks causal attribution; this result does not supply it.

### A3: Deferred Connect Intent

1. An enabled network is disconnected while a second network is being joined.
2. Connect records `desiredConnected` but returns because an operation is busy.
3. Join completion clears busy and updates notification state without connecting.

The success and failure paths share this completion block. A prior reconnect or
status timer could mask the omission; the regression must start without either.

Required regression: latch the join result, request connection, then release
success and failure variants. The existing enabled network must start afterward;
the newly joined network must retain its explicit disabled default.

#### Implemented Recovery

Join completion now resumes the existing startup path when connection intent
remains set and no runtime is connected. It runs after clearing join ownership
and busy state, on both success and failure. It does not alter network enablement.

| Instrumented ordering | Result on API 35 x86_64 |
| --- | --- |
| Failed join with deferred connect | Native VPN starts after completion |
| Connection intent withdrawn before completion | VPN remains disconnected |
| Successful join with deferred connect | Existing enabled network starts; joined network persists disabled |
| Subsequent occupied-worker replacement | Existing lifecycle and native-cleanup assertions pass |

The test injects join results at the real service worker boundary, not over
public pairing. It uses real encrypted profile storage, Android VPN preparation,
and JNI startup. It does not establish packet delivery or public discovery.

- Before fix: `join completion did not resume requested connection`.
- Final run: `passed=true`, instrumentation result code `-1`.
- Logs: `/tmp/p2p-vpn-review-deferred-join-{before,after}.txt`.
- Unit tests, lint, debug APK, and instrumentation APK assembly passed offline.

The harness now uses `startService` for its bound-service debug setup, which
does not enter foreground mode. Initial setup attempts also omitted VPN
preparation; neither setup failure is counted as production regression evidence.

Run the existing [lifecycle instrumentation](android-lifecycle-review.md#run-it)
with the additional argument `-e deferred_join true`. Require the
`deferred_join=passed` status as well as the final instrumentation success.

| Tested artifact | SHA-256 |
| --- | --- |
| App APK | `83cd125da404ab0b0634489caea08badf09875906f5d9f41923eb9b5d2610ea3` |
| Instrumentation APK | `b9e7e330b77d2900dfa011aac845efaea48b0754566be8fcc3a7372bf23b8a20` |

## Implementation Sequence

1. Reproduce each ordering using existing worker and lifecycle test infrastructure.
2. Fence stop effects with their initiating start/request identity, including a
   main-thread check before removing foreground ownership.
3. Make connected health-poll ownership explicit across VPN-manager mode changes;
   preserve lockdown polling and existing delays.
4. Reconcile deferred connection intent after join completion without enabling
   a newly joined network implicitly or retrying terminally cancelled work.
5. Rerun focused Java tests, instrumentation, and affected Android scenarios.

Do not substitute the latest start ID read later on the worker for the initiating
request identity: that can incorrectly authorize a stale stop. Preserve service
instance retirement as a separate ownership boundary.

## Existing Coverage and Limits

- `ServiceRuntimeWorkerTest` covers worker scopes and retirement, not superseding starts in one scope.
- `ServiceLifecycleInstrumentation` covers replacement instances, not every main-handler ordering.
- `VpnModeTest` covers policy decisions, not ownership of the service's scheduled poll.
- `ProfileJoinRequestTest` covers input validation, not connect intent during completion.
- No additional defect was identified in worker retirement within this limited inspection.

Earlier worker-retirement fixes and platform evidence remain in the
[lifecycle review](android-lifecycle-review.md).

No production service, phone, or configuration was modified for the source review.
