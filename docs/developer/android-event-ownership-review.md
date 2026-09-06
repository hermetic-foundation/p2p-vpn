# Android Lifecycle Ownership Review

## Scope

Source reviewed at `6dbb680c` on 2026-09-06. A read-only reviewer identified
the cases below; parent inspection checked the relevant control flow.
This initial source-review record precedes deterministic regression results.

Successful workflow tests do not exercise every event ordering. These cases remain
open even when the [network workflow](android-network-workflow-review.md) or
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

### A3: Deferred Connect Intent

1. An enabled network is disconnected while a second network is being joined.
2. Connect records `desiredConnected` but returns because an operation is busy.
3. Join completion clears busy and updates notification state without connecting.

The success and failure paths share this completion block. A prior reconnect or
status timer could mask the omission; the regression must start without either.

Required regression: latch the join result, request connection, then release
success and failure variants. The existing enabled network must start afterward;
the newly joined network must retain its explicit disabled default.

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
