# Android Lifecycle Audit

## Status and Scope

Bounded review completed against production `518929b3` on 2026-09-09, opened
against `d0e38487`. Final requirement evidence and reuse boundaries appear below.
Earlier results retain their source revisions; no physical-device run is claimed.

| Boundary | Required Outcome |
| --- | --- |
| UI, service and JNI | One current owner for each command, callback, timer and native runtime |
| Lifecycle changes | Disabled networks remain disabled; replacement owners survive stale events |
| Pairing | Cancellation before local commit prevents saving a late enrollment |
| Recovery | A subsequent valid connection succeeds without manual rescue |
| Evidence | Distinguish OS underlay state, service state and overlay packet delivery |

Sustained resource attribution and final cross-platform certification remain
separate workstreams. Core pairing, recovery and Kademlia reviews are closed;
reopen them only if this audit produces new causal evidence.

## Completion Checklist

- [x] Locate existing reports and distinguish historical passes from open attribution.
- [x] Finish source ownership inventory, including failure and replacement paths.
- [x] Test rapid commands and stale callbacks within and across service scopes.
- [x] Test permission success, denial and activity recreation without stale activation.
- [x] Test profile-join cancellation, late completion and subsequent successful local commit (AL1).
- [x] Verify multi-network enablement, persistence and native generation isolation.
- [x] Audit native shutdown, TUN descriptors, callback retirement and timer bounds.
- [x] Verify failed binding and exception cleanup (AL4).
- [x] Reconcile process death, always-on, reboot and app replacement on reviewed source.
- [x] Run bounded cached-emulator lifecycle and applicable underlay scenarios.
- [x] Record historical failure dispositions and precise missing attribution evidence.
- [x] Run affected checks, inspect final diff, publish each atomic commit to `main`.
- [x] Audit every requirement before marking this review complete.

## Final Requirement Audit

| Goal Requirement | Verified Evidence | Boundary |
| --- | --- | --- |
| 1. Reconcile reports | Existing-evidence map, AL1-AL4, historical dispositions and linked review index | Original failures remain visible; completed core workstreams are not reopened |
| 2. Inventory owners | UI/service ownership trace plus native/TUN table; source rechecked through `518929b3` | Covers intent, generations, jobs, callbacks, storage, JNI, descriptors, notifications and timers |
| 3. Verify event order and recovery | Final combined instrumentation passes every opt-in; 68-check multi-network run verifies disabled-set persistence and restoration | Controlled orderings plus real framework/JNI; not arbitrary scheduler or permanent native deadlock proof |
| 4. Reproduce and fix minimally | AL1-AL4 each retain failing assertions before correction and successful subsequent operations | Two Java production files changed; no identity, configuration, profile, CLI or protocol migration |
| 5. Validate affected layers | 131 JVM tests, lint, both APKs, final JNI instrumentation; native provenance and unchanged-source unit evidence below | Debug API 35 x86_64; physical/API 37 and release certification remain separate |
| 6. Reconcile historical failures | Original cellular/update artifacts reinspected; attribution ledger names missing packet and OS observations | Later passes do not explain earlier missing replies |
| 7. Publish structured review | Ownership tables, findings, commands, artifact hashes, limits and remaining-refactor index | Sustained resources and final cross-platform acceptance are not declared complete |
| 8. Publish atomic commits | AL1 `ab45d355`, AL2 `481bc16a`, AL3 `7c8a5b6a`, AL4 `518929b3` on `main@origin` | Final documentation publication is verified separately from runtime evidence |

### Final Evidence Reuse

- Final combined log: `/tmp/p2p-vpn-lifecycle-failed-bind-positive.log`.
  All options pass; final `passed=true`, instrumentation result `-1`.
- Final build: `/tmp/p2p-vpn-lifecycle-failed-bind-fixed-build.log`.
  JVM XML reports total 131 tests, zero failures/errors/skips; lint and APK assembly pass.
- Native unit reuse: `/tmp/p2p-vpn-ps4-workspace.log`, Android crate 70/70.
  Assertions cover lease retirement, fresh queues, stale generations, route changes and TUN pressure.
- `jj diff --from d0e38487 --to 518929b3 --stat src crates Cargo.toml Cargo.lock`
  reports zero changes. No shared-Rust-change gate is triggered by this Java-only review.
- Offline native rebuild and staged JNI remain byte-identical, as recorded below.
  Real JNI startup, stop, replacement and recovery run in the final instrumentation.
- Repository search found no Lean/Lake model. Lint and diff inspection cover Java
  static/style checks; no separate Java formatter is configured in the Android build.

The 68-check scenario uses `d6b6b1b0`, not the final APK. A source diff to
`518929b3` contains only AL3 and AL4: the missing-local-permission timer and
activity-binding cleanup. Final instrumentation exercises both changed paths.

Normal API 35 cannot enter the API 37 missing-permission branch. Successful
activity binding retains the same callbacks and service ownership; its cleanup,
recreation and replacement are rerun with AL4. Unchanged multi-network behavior
therefore reuses the earlier run without relabelling its artifact as final.

### Remaining Separate Work

| Workstream | Evidence Still Needed |
| --- | --- |
| Sustained resources | Comparable idle/load CPU, retained heap, connection counts and battery attribution |
| Final platform acceptance | Physical ARM64/API 37 permission behavior, release packaging and affected NixOS/Android acceptance matrix |
| Historical packet attribution | Correlated per-network packet stages/deltas if the old update or isolation symptom recurs |

The lifecycle review is complete within these explicit platform and cooperative
shutdown limits. This is not production certification or proof of zero packet loss.

## Initial Source Map

Java paths below are relative to
`android/app/src/main/java/org/hermeticfoundation/p2pvpn/`.
Entries locate ownership boundaries; they do not assert complete verification.

| Boundary | Source Entry Points | Review Obligation |
| --- | --- | --- |
| UI lifecycle | `MainActivity.onStart`, `onStop`, `onDestroy` | Listener removal, pending commands and recreated activity ownership |
| Permission | `MainActivity.onActivityResult`, `onRequestPermissionsResult` | Bind outcomes to the requested network; do not enable after cancellation |
| Start admission | `P2pVpnService.onStartCommand` | Admitted identity precedes queued worker identity; old stops cannot consume it |
| Scope retirement | `ServiceRuntimeWorker`, service `onDestroy`, `retireServiceRuntime` | Reject retired tasks; deliver cleanup before replacement work |
| Connection | `connectRequested`, `disconnectRequested`, `startConnection` | Reconcile desired state with busy operations and foreground ownership |
| Network mutation | `setNetworkEnabled`, `reconcileEnabledNetworks`, `persistProfileCollection` | Persistence and runtime transition agree after success or failure |
| Health | `scheduleStatusPoll`, `pollNativeStatus`, `vpnManagerModeChanged` | One rearming timer; native failure recovers without changing enabled set |
| Underlay | `registerNetworkCallback`, `handleUnderlayChange`, `UnderlayTracker` | Retire callbacks, coalesce signals and distinguish absent OS connectivity |
| Join completion | `joinProfileByCode`, `cancelProfileJoin`, `completeProfileJoin` | Operation identity plus local cancellation must fence profile commit |
| Existing-network pairing | `resumeActivePairing`, `applyPairingArtifacts`, `clearActivePairing` | Service retirement and persisted operation agree with core commit semantics |
| Native runtime | `crates/p2p-vpn-android/src/lib.rs`, `RuntimeInstance` | Global owner, supervisor tasks, TUN threads and shutdown completion |
| Native network | `crates/p2p-vpn-android/src/supervisor.rs`, `NetworkLease` | Old generations cannot route, enqueue or deactivate replacements |

## Existing Evidence

| Area | Existing Result | Reuse Boundary |
| --- | --- | --- |
| Worker retirement | [R11 and occupied-worker instrumentation](android-lifecycle-review.md) | Real JNI replacement with a controlled Java latch; not indefinitely stalled JNI |
| Superseded stop | [A1](android-event-ownership-review.md#a1-superseded-stop) | Three stop paths and two admission orderings; synthesized callback delivery |
| Health polling | [A2](android-event-ownership-review.md#a2-lost-health-timer) | Recurring JNI reads and one autonomous recovery at `4b90f3bc`; not retry exhaustion |
| Deferred connect | [A3](android-event-ownership-review.md#a3-deferred-connect-intent) | Injected join success/failure; encrypted storage and JNI startup are real |
| Multi-network lifecycle | Historical [68-check pass](android-multi-network-review.md) at `deedd041` | Superseded for this audit by the `d6b6b1b0` run and final-source reuse analysis |
| Native network generations | `supervisor.rs` reactivation and lease tests | Inspect assertions and rerun affected scope before claiming final-source coverage |
| Formal models | No Lean/Lake files found by repository file search | No applicable existing Lean model identified; not a formal correctness claim |

## Current Ownership Trace

Inspected against `8a761622`. This is source evidence, not a replacement for
the executable gates below. Service methods run on the scoped worker unless
explicitly identified as main-thread callbacks.

| Event / Resource | Admission and Owner | Retirement / Recovery |
| --- | --- | --- |
| Activity start/stop | Main-thread per-start `ActivityServiceConnection`; registration separate from `bound` | Stop clears owner first, removes its listener and unbinds even before connection delivery |
| Service start | Main-thread `admittedStartOwner`; queued `processedStartOwner` | Deferred stops require the captured processed identity to remain the admitted identity |
| Service replacement | Process-wide `ServiceRuntimeWorker.Dispatcher` | New scope closes old admission; old cleanup precedes replacement native work |
| Desired connection | Worker `desiredConnected` plus `operationInProgress` | Deferred join completion reconciles intent; explicit disconnect clears it |
| Persisted activation | Encrypted `ProfileCollection.Entry.enabled`, not UI selection | System/null start uses `restorePersistedActivation`; zero enabled networks stay idle |
| Network mutation | Validate and save collection before `suspendConnectionForNetworkChange` | Successful mutation reconciles the enabled set; failed mutation does not publish candidate state |
| Always-on / lockdown | `VpnMode` plus manager callback | Manual disconnect cannot override always-on; lockdown stops runtime and retains a mode poll |
| Java health poll | One `statusFuture`, superseded before scheduling | Stop cancels it; connected mode event rearms it; native failure triggers recovery |
| Native reconnect | One `reconnectFuture`; current connection intent checked when fired | Disconnect cancels it; busy operations reschedule; exponential delay is capped, attempt count is not |
| Underlay callback | Main-handler registration for physical Internet networks; `UnderlayTracker` | Service destruction unregisters it; scoped worker rejects late work |
| Underlay recovery | One coalesced `underlayRecoveryFuture` | Rechecks desired/connected state; signalling failure retries then requests native restart |
| Profile-free join | Dedicated join executor plus operation ID; AL1 cancellation flag | Completion releases busy owner and multicast lock; scope retirement cancels JNI and closes executor |
| Existing-network pairing | Saved operation references its network ID | Load rejects absent/disabled network ownership; explicit disconnect cancels and clears operation |
| Notifications and snapshots | Main-handler effects gated by live service scope | Manual stop additionally checks start identity; activity snapshots check their binding owner |

### Native and TUN Ownership

Paths are relative to `crates/p2p-vpn-android/src/`.

| Resource | Source Owner | Failure / Teardown Path |
| --- | --- | --- |
| Detached TUN fd | JNI start entry adopts `File` before string parsing | Validation errors drop the adopted file; Java closes the detached wrapper |
| Read/write endpoints | `start_runtime` duplicates the writer fd into an independently owned reader `File` | Failed duplicate/setup drops owners; each running TUN thread owns its file |
| TUN worker startup | Reader, writer, then supervisor thread | Writer spawn failure stops/joins reader; supervisor spawn failure stops/joins both TUN threads |
| Runtime storage | Process-global `RUNTIME: Mutex<Option<RuntimeInstance>>` | `stop_runtime` takes the instance before signalling shutdown and joining owned threads |
| Per-network attempt | `supervise_network` holds runtime task, health probe, shutdown sender and `NetworkLease` | Attempt exit retires its generation; sibling supervisors remain independent |
| Packet ownership | `PacketSwitch`, per-network queues and generation-tagged routes | Closed or stale leases cannot deactivate replacement generations; queue limits remain per network |
| Shared runtime failure | TUN error sets stop flag, closes switch and signals supervisor shutdown | Java health polling observes the aggregate failure and retains connection intent for recovery |
| Native supervision | `JoinSet` owns network supervisors; Tokio worker count is clamped to 2-4 | Supervisor thread drains tasks, closes switch and shuts down Tokio |

`AndroidTunReader` checks its stop flag around 250 ms polls. The writer has a
total polling budget. Network shutdown requests grace, then aborts the task;
the constants are four seconds of grace and one second of abort wait.

These are cooperative limits. Scheduler progress is required, and native
`thread::join` has no hard deadline. Existing occupied-worker instrumentation
does not simulate a permanently wedged native call or certify that case.

### Permission Callback Evidence

`ActivityPermissionTest` invokes actual activity result handlers with JVM Android
stubs. It verifies local command ownership, not framework permission grants or
service delivery. No production permission behavior changed.

| Case | Verified Result |
| --- | --- |
| Denied VPN request | Pending network cleared; no activation mutation dispatched |
| Success after denial | Cleared request cannot be revived |
| Accepted request | Exactly the pending network moves to activation state |
| Duplicate success without a pending request | No second activation mutation |
| Unrelated result code | Pending network ownership is preserved |

- Focused log: `/tmp/p2p-vpn-lifecycle-permission-unit.log`.
- Full checks: `/tmp/p2p-vpn-lifecycle-permission-verified.log`.
- Platform recreation and VPN denial are covered below; local-network recovery is covered by AL3.

`MainActivity` persists a pending enable network in instance state. Pending join
code/hostname are not serialized there; rotation before local permission returns
can discard that unstarted request. No enrollment exists to commit at that point.

### Recreation and System VPN Denial

Passed on API 35 x86_64 with production source `8674af4c` and the new
`activity_permissions` instrumentation option. No production change was needed.

| Stage | Required Result |
| --- | --- |
| Framework `Activity.recreate()` | A different activity instance receives the saved pending network |
| Old activity | Binding owner cleared; replacement establishes a real service binding |
| Injected denial then late success | Restored request clears without an activation mutation |
| Fresh request | Android's real VPN consent dialog appears |
| System Cancel button | Real activity result clears the pending network without activation |
| Subsequent combined run | Native connection, health recovery, cancelled joins and service replacement pass |

The saved request is seeded at the activity boundary; no pending system dialog
is carried through recreation. This is not process-death restoration or a grant
of Android's newer local-network permission, which API 35 does not exercise.

#### Setup Failure and Correction

Two initial attempts timed out waiting for the VPN denial callback, first with
Back and then the explicit Cancel button. The notification-permission dialog
remained underneath VPN consent, keeping the target activity paused.

Retained system logs show `REQUEST_PERMISSIONS` and VPN `ConfirmDialog` together.
The harness now grants only notification permission before launching activities,
then explicitly removes VPN consent and clicks the system Cancel button.

| Evidence | Path |
| --- | --- |
| Back attempt | `/tmp/p2p-vpn-lifecycle-permission-platform.log` |
| Cancel with overlapping notification dialog | `/tmp/p2p-vpn-lifecycle-permission-platform-cancel.log` |
| System activity logs | `/tmp/p2p-vpn-lifecycle-permission-dialog-overlap.log` |
| Isolated pass | `/tmp/p2p-vpn-lifecycle-permission-platform-isolated.log` |
| Offline JVM, lint and APK checks | `/tmp/p2p-vpn-lifecycle-permission-isolated-build.log` |

The passing run reports `activity_permissions=passed`, `passed=true` and result
code `-1`. Add `-e activity_permissions true` to AL1's instrumentation command.
The emulator was stopped, private state removed and all result logs retained.

| Artifact | SHA-256 |
| --- | --- |
| App APK | `1f2e8a985f64a933e462545e73c4e1cffee463a1cbc4aa7e7ff76d63c26326d0` |
| Instrumentation APK | `643f32c5f577da6c6bd646e0b9065bf77a674b17dec6fda10a3335391ea4a51e` |

## AL1: Cancelled Profile Join

Priority: P1. Reproduced and fixed with API 35 x86_64 instrumentation.

1. Native join returns a successful profile to the dedicated join worker.
2. The service worker processes `cancelProfileJoin` before queued completion.
3. Cancellation signals JNI but leaves the Java operation eligible for completion.
4. `completeProfileJoin` checks only the operation ID before saving the profile.

Native cancellation alone cannot fence this local commit after JNI returns.
The operation now records cancellation on the service worker before signalling
JNI. Completion checks that flag before preparing or saving any profile state.

The existing `finally` block releases operation ownership and the multicast lock,
publishes the snapshot and resumes deferred connection intent. The busy slot stays
owned until completion, so cancellation does not admit overlapping native joins.

Cancellation after a completed local commit is a different case. Do not silently
revoke committed membership or alter the network's revocation policy.

### Regression Evidence

`ServiceLifecycleInstrumentation` holds a successful result at the service-worker
boundary and invokes real cancellation before completion. It uses JNI-created
profiles and real encrypted storage, not public pairing or an injected file store.

| Assertion | Result |
| --- | --- |
| Before fix | `cancelled join persisted a late successful profile`; `passed=false`, result code `0` |
| Repeated cancellation then late success | Stored profile collection remains byte-identical after decryption |
| Completion cleanup | Busy flag and operation owner released |
| Old completion during replacement | Replacement retains busy flag and owner |
| New successful result | Exactly one additional network persists |
| Cancel after commit and duplicate completion | Committed profile collection remains unchanged |
| Combined lifecycle regressions | Deferred join, superseded stops, recurring health recovery and occupied-worker replacement pass |

- Negative log: `/tmp/p2p-vpn-lifecycle-cancel-before.log`.
- Positive log: `/tmp/p2p-vpn-lifecycle-cancel-after.log`; `passed=true`, result code `-1`.
- Offline JVM tests, lint, app and instrumentation assembly: `/tmp/p2p-vpn-lifecycle-cancel-after-build.log`.
- Initial test compile error and launcher lifetime error were setup failures, not defect evidence.

### Commands and Provenance

The cached Gradle 9.5.1 distribution used Nix OpenJDK 17.0.20+8 and SDK 37.
No Rust source changed. JNI was reused from the cached review staging directory;
this is Java ownership regression evidence, not final-source native certification.

```sh
gradle -p android --offline --no-daemon --max-workers=2 \
  -Dorg.gradle.parallel=false -I /tmp/p2p-vpn-review-android-jni.gradle \
  :app:testDebugUnitTest :app:lintDebug :app:assembleDebug :app:assembleDebugAndroidTest
adb -s emulator-5554 shell am instrument -w -r \
  -e isolated_emulator true -e cancelled_join true -e deferred_join true \
  -e superseded_stop true -e health_poll true \
  org.hermeticfoundation.p2pvpn.debug.test/org.hermeticfoundation.p2pvpn.ServiceLifecycleInstrumentation
```

| Artifact | SHA-256 |
| --- | --- |
| Fixed APK | `3e4193cc2808a1ed95c7654a6b46728e5de7aa0eec85965674e6a00e4ac075fc` |
| Instrumentation APK | `53724de7e3194e3ff0c65c597ddfa6e64642388ad8b1cab765352f2400274be9` |
| Reused unstripped JNI | `cf9a1d40b71cc17b38759ced5352690183818c5c27bbebeb9a7fd82fa88c548b` |

Only disposable emulator state was cleared between negative and positive runs.
The emulator was stopped and private state removed afterward; logs remain outside
that directory. Temporary storage began at 7.84 GiB; emulator state reached 1.1 GiB.

## AL2: Activity Binding Ownership

Priority: P2. JVM callback ordering reproduced and corrected at `481bc16a`.
API 35 x86_64 pending-stop and framework-rebind instrumentation now passes.
Framework recreation and VPN denial also pass in the permission scenario above.

| Previous Ordering | Consequence |
| --- | --- |
| `onStart` requests an asynchronous binding | `bound` remains false until the callback |
| Activity stops before the callback | `onStop` skips unbinding because `bound` is false |
| Late `onServiceConnected` arrives | Stopped activity attaches a binder and listener |
| System disconnects before activity stop | Connected state clears, but the binding registration still needs release |

Each activity start now creates a distinct connection owner. Registration state
is separate from connected state; stop retires the owner before releasing its
listener and binding. Old connect, disconnect and snapshot callbacks are ignored.

### Verification Boundaries

| Check | Evidence / Limit |
| --- | --- |
| Negative control | `ActivityBindingTest`: `late callback attached a stopped activity` before correction |
| Retired callback | Cannot install a binder or listener after stop |
| Replacement | Old connect/disconnect and snapshot events preserve the new binder |
| Recovery | Current connection accepts a reconnect after disconnect |
| Listener cleanup | No listener remains after stop; one remains during replacement |
| Offline checks | Full JVM suite, lint, app and instrumentation assembly passed |
| Platform gate | Three pending-stop and real framework-rebind cycles pass; details below |

The JVM uses Android stubs. Tests explicitly set registration admission for
controlled callbacks; they do not prove Android unbind delivery or process
recreation. No native runtime or physical device is involved in these tests.

- Negative: `/tmp/p2p-vpn-lifecycle-binding-before.log`.
- Focused positive: `/tmp/p2p-vpn-lifecycle-binding-after.log`.
- Full checks: `/tmp/p2p-vpn-lifecycle-binding-verified.log`.
- Tools and offline Gradle command match AL1, without instrumentation execution.

### Framework Binding Run

`ServiceLifecycleInstrumentation` launches the real activity and waits for its
framework binding. On the main thread, it invokes stop/start/stop while connection
delivery is held, then requires registration and callback ownership to be cleared.

| Stage, Repeated Three Times | Required Result |
| --- | --- |
| Bind then stop before delivery | Registered but not connected before stop; retired immediately afterward |
| Drain main callbacks | Stopped activity remains disconnected |
| Start again | Real framework callback installs the replacement binder |
| Inject retired connect/disconnect | Replacement binder remains connected |
| Final activity teardown | Stop, finish and main-queue drain complete |
| Combined service run | AL1, deferred joins, stale stops, health recovery and occupied-worker replacement pass |

The lifecycle methods are invoked by instrumentation to control ordering, not by
an OS backgrounding event. Binding and rebinding use Android's real service APIs.
This does not prove rotation, process recreation, permission dialogs or battery use.

- Source: production `481bc16a`, plus this instrumentation case.
- Run: `/tmp/p2p-vpn-lifecycle-binding-platform.log`; `activity_binding=passed`, `passed=true`, result code `-1`.
- Build: `/tmp/p2p-vpn-lifecycle-binding-platform-build.log`; offline JVM tests, lint and both APKs pass.
- Use AL1's instrumentation command with the additional `-e activity_binding true` argument.
- Emulator stopped, private state removed, logs retained; no physical device involved.

| Artifact | SHA-256 |
| --- | --- |
| App APK | `1f2e8a985f64a933e462545e73c4e1cffee463a1cbc4aa7e7ff76d63c26326d0` |
| Instrumentation APK | `6b6a5d3f02d209a2b9d826d2881c50cbe517834bad09363a99afe7f8051d059d` |

JNI is the same artifact identified in AL1 and verified against current source
below. The subsequent multi-network run and final audit complete reconciliation.

## AL3: Local Permission Recovery

Priority: P2. The always-on missing-permission handler stopped the runtime but
scheduled no further permission check. Permission restoration alone therefore
had no timer-driven path back to a connected runtime.

The handler now calls the existing `scheduleBlockedModePoll` before returning
in always-on mode. This retains the existing 30-second interval and scoped
cancellation owner; manual mode still withdraws connection intent and stops.

### Reproduction and Recovery

The first JVM attempt hit Android's stubbed notification builder before the
assertion. It was replaced with emulator instrumentation, not counted as defect
evidence, and no JVM-stub workaround was added to production code.

| API 35 Instrumentation | Result |
| --- | --- |
| Before correction | `always-on permission loss stranded recovery`; missing live timer reproduced |
| Three loss events | Exactly one live timer; superseded timers cancelled; runtime remains stopped |
| Ordinary timer fires | Exactly one replacement starts without an explicit connect/poll invocation |
| State preservation | Peer identity, active network IDs and encrypted profile contents unchanged |
| Health after recovery | Ordinary recurring health polling resumes |
| Manual mode | Connection intent and polling withdrawn; explicit connection succeeds afterward |
| Combined run | Activity binding, recreation, VPN denial, AL1, stale stops and native health recovery pass |

The test injects permission loss and always-on mode at the real service handler.
JNI stop/start, notifications and timer execution are real. API 35 treats local
permission as granted, modelling restoration at the next ordinary poll.

Actual API 37 permission revocation/grant delivery remains a final-platform
certification limit, not evidence supplied by this injection. The pure
`LocalNetworkPermissionTest` covers device/target enforcement thresholds.

- Stub setup failure: `/tmp/p2p-vpn-lifecycle-local-permission-before.log`.
- Emulator negative: `/tmp/p2p-vpn-lifecycle-local-permission-negative.log`.
- Emulator positive: `/tmp/p2p-vpn-lifecycle-local-permission-positive.log`.
- Full offline JVM, lint and APK checks: `/tmp/p2p-vpn-lifecycle-local-permission-fixed-build.log`.
- Run AL1's instrumentation command with `-e local_permission true`; require its pass plus final result code `-1`.
- Emulator stopped and private state removed; result logs retained.

| Artifact | SHA-256 |
| --- | --- |
| Fixed app APK | `714a394dfe80a94a5b287fce380f76d2eff5fda0e3c32bee77280408180449c6` |
| Instrumentation APK | `358dee3fecf698a43e568f936e1621eb92fe331b5beac3a5a7cbb73cf2dc7887` |

### Multi-Network Evidence Reuse

AL3 is a one-line production change in the missing-local-permission handler.
The normal API 35 path cannot enter it: device API 35 is below the enforcement
threshold. The 68-check run below is reused for unchanged API 35 behavior.

The changed handler has its own failing/passing real-service instrumentation
and automatic recovery evidence. No Rust, profile format, protocol, membership
policy or packaging configuration changed.

## AL4: Failed Binding Cleanup

Status: reproduced and fixed at `518929b3`; both failure paths and subsequent
framework rebinding pass on the API 35 emulator.

Previously, `MainActivity.onStart` assigned `bindingRegistered` from the Boolean returned by
`bindService`. A false return therefore prevents `onStop` from releasing the
connection's tracking resources; a thrown security exception also lacks cleanup.

[Android's binding contract](https://developer.android.com/reference/kotlin/android/content/ContextWrapper)
requires releasing the connection after a false return or `SecurityException`.
The existing pending-stop tests cover successful registration, not these outcomes.

The activity now owns cleanup before attempting admission. A false return retains
that owner until stop; `SecurityException` releases it immediately and propagates.
Both paths use the same owner-retirement helper, preserving stale-callback guards.

### Regression Evidence

| Gate | Result |
| --- | --- |
| JVM negative | `ActivityBindingTest` fails: failed binding lost cleanup ownership |
| Emulator false-return negative | Fails: failed binding was not released exactly once |
| Emulator security-exception negative | Fails: security exception leaked binding tracking |
| Combined emulator positive | Both paths release exactly once; repeated stop is inert; real replacement binding survives stale callbacks |
| Broader instrumentation | Binding cycles, recreation, VPN denial, join cancellation, deferred join, superseded stop, health poll, local-permission recovery and occupied-worker replacement pass |
| JVM, lint and APKs | 131 JVM tests pass with no skips; full gate passes: 78 tasks, 15 executed, 17 seconds |

The instrumentation wraps the activity context only in the test APK. It allocates
real framework binding resources, then injects a false return or security exception
before callback delivery. These are controlled admission outcomes, not an OS policy denial.

No fault-injection hook, permission bypass, profile change or wire change enters
the production app. Native source and staged JNI are unchanged from the provenance
below; the 68-check multi-network run retains its original APK and reuse boundary.

| Artifact | Path or SHA-256 |
| --- | --- |
| JVM negative | `/tmp/p2p-vpn-lifecycle-failed-bind-negative.log` |
| False-return negative | `/tmp/p2p-vpn-lifecycle-failed-bind-platform-negative.log` |
| Exception negative | `/tmp/p2p-vpn-lifecycle-security-bind-negative.log` |
| Combined positive | `/tmp/p2p-vpn-lifecycle-failed-bind-positive.log`; `passed=true`, code `-1` |
| Full build | `/tmp/p2p-vpn-lifecycle-failed-bind-fixed-build.log` |
| App APK | `63eb12af5b8b613bfb2bab8aae91d619cc7fd82c14976a626cee42108d3b181c` |
| Test APK | `319170af6e2cadeeabfe7d813d75c0be5a9f6067d2c94b75489509b97682c0cd` |

Use the AL1 instrumentation command with `-e failed_binding true` plus the
existing binding, permission, join, stop and polling opt-ins. The combined run
kept its 90-second watchdog; no builds ran during observation.

The emulator exited normally after `emu kill`; its disposable private state was
removed. Negative and positive logs remain. No physical device was accessed.
Final temporary storage is 8,286,676 KiB, below the 10 GiB limit.

## Current Native Provenance

The offline locked x86_64/API 26 native rebuild passed in 39.03 seconds after
the permission run. Its output is byte-identical to the staged library used
by AL1, AL2 and the recreation/permission instrumentation.

| Check | Result |
| --- | --- |
| `jj diff --from d0e38487 --to @ --stat src crates Cargo.toml Cargo.lock` | Zero changed files at production `8674af4c` |
| Build | `/tmp/p2p-vpn-lifecycle-current-native.log`; four existing Rust warnings |
| Fresh and staged unstripped library hashes | Both `cf9a1d40b71cc17b38759ced5352690183818c5c27bbebeb9a7fd82fa88c548b` |
| Toolchain | Cached Rust 1.97.1, NDK 28.2.13676358, two Cargo jobs, no debug symbols/incremental state |

The command used the existing cached Android vendor directory and Rust source
wrapper, with `CARGO_TARGET_DIR=/tmp/p2p-vpn-android-target`:

```sh
cargo build --offline --locked \
  --config 'source.crates-io.replace-with="cached-android"' \
  --config 'source.cached-android.directory="/tmp/p2p-vpn-kad-android-vendor"' \
  -Z build-std=std,panic_abort --target x86_64-linux-android \
  --package p2p-vpn-android --lib
```

This establishes native provenance for the lifecycle scenarios, not ARM64,
release packaging, packet delivery or sustained resource attribution.

## Historical Attribution Ledger

| Case | What the Retained Report Establishes | Missing Evidence / Next Check |
| --- | --- | --- |
| Cellular failure at `6dbb680c` | App selected no underlay; native transport reported network unreachable | OS connectivity capabilities were not retained; cannot distinguish absent OS connectivity from tracker failure |
| Update failure at `39c2ecb6` | Beta received 4/5 reverse IPv4 replies after replacement; validated OS underlays existed | Correlated packet stages and per-network deltas; final counters and path changes do not locate the missing reply |
| Isolation failure at `fe950142` | Beta received sequences 1-4, not the fifth reply, after alpha termination | Per-network packet trace and interval deltas; aggregate expired/drop counts do not identify the packet |
| Restart failures at `bac49191` and `23e5159d` | One network did not regain its peer within the existing deadline | [Admission reproduction](private-discovery-restart-review.md) supports a fixed defect, not attribution of every historical loss |

The [multi-network report](android-multi-network-review.md) retains original
paths and sanitized samples. Inspect those artifacts during final reconciliation;
a later pass does not supply missing historical observations.

The original `evidence.json` files for the cellular and update failures were
reinspected during this audit. Cellular `device.diagnostics` contains only
`final_runtime` and `export`; update evidence additionally contains `os_underlay`.

This supports distinct dispositions: cellular OS/tracker attribution is unresolved;
the update lost reply cannot be attributed to absent OS connectivity. Its recorded
validated underlays and aggregate counters still do not locate packet loss or delay.

## Current Multi-Network Run

Production `d6b6b1b0` passed all 68 checks on API 35 x86_64, from
2026-09-09T10:47:40Z to 10:55:19Z (459 seconds). The APK, native library and
rebuilt Linux fixture use that revision; no runtime intervention was performed.

| Lifecycle Stage | Result |
| --- | --- |
| Legacy migration and two independent joins | Identities preserved; private bootstrap discovers peers without configured overlay addresses |
| Concurrent traffic and overlap rejection | Both networks pass dual-stack traffic; rejected overlap does not mutate live state |
| Disable, app update, re-enable | Disabled set survives replacement; re-enable restores both networks |
| Wi-Fi / emulated cellular / Wi-Fi | Both recover without shared-runtime restart |
| Process death and app replacement | Both identities and traffic restore autonomously |
| Temporary lockdown | Both recover automatically when lockdown is removed |
| Emulator reboot | Both enabled networks restore and pass concurrent traffic |
| Stop alpha fixture | Beta remains reachable; process identity and queue bounds remain stable |
| Cleanup | Emulator, fixtures and private state removed; all six safeguards pass |

Readiness retries precede fixed packet measurements. Beta required five Linux
IPv4 attempts after the disabled-set update, twelve after cellular transition,
and six after returning to Wi-Fi; alpha required two on Wi-Fi return.

Every fixed traffic assertion passed 5/5. Retained reply sequences include the
update and isolation measurements. This is recovery evidence, not uninterrupted
traffic or proof of the cause of earlier failed measurements.

### Artifacts and Limits

- Evidence: `/tmp/p2p-vpn-lifecycle-multi-current/evidence.json`.
- Preflight: `/tmp/p2p-vpn-lifecycle-multi-preflight/evidence.json`.
- Fixture build: `/tmp/p2p-vpn-lifecycle-fixture-build.log`; offline locked, two jobs, 38.17 seconds.
- Bounded fixture echo tracing enabled; no builds ran during traffic observation.
- Final temporary storage: 8,286,588 KiB, below 10 GiB; growth watchdog limited the run to 1.5 GiB.

| Artifact | SHA-256 |
| --- | --- |
| App APK | `1f2e8a985f64a933e462545e73c4e1cffee463a1cbc4aa7e7ff76d63c26326d0` |
| Linux fixture | `3d5a7f52bd82918b8cdbeeeafe14ff372e8fbcad36e66a02f66858eaa9d756df` |
| Linux CLI | `44ee4819a945a3d0714d19c6ff13a38ba5a5d45b8002b63942f78e69736378e0` |

Final diagnostics: PSS 67,106 KiB, seven Java-reported active threads, empty
packet queues, one expired 84-byte packet and seven outbound drops. The path
snapshot has one direct QUIC stream and one relay; it is not all-QUIC evidence.

The original OS diagnostics distinguish physical Wi-Fi/cellular from the VPN.
These are emulator transports, not physical carrier NAT, hotspot/VPN, ARM64,
release packaging or sustained battery/heap certification.

```sh
P2P_VPN_ANDROID_E2E_MAX_RUNTIME_GROWTH_BYTES=1610612736 \
P2P_VPN_ANDROID_E2E_TRACE_ECHO=1 \
bash scripts/android-e2e.sh --scenario multi-network \
  --output /tmp/p2p-vpn-lifecycle-multi-current
```

The command selects the cached Nix launcher and ADB through the documented
`P2P_VPN_ANDROID_EMULATOR`/`P2P_VPN_ADB` variables. `P2P_VPN_ANDROID_APK`,
`P2P_VPN_ANDROID_E2E_FIXTURE` and `P2P_VPN_BIN` select the artifacts above.

## Validation Plan and Limits

1. Extend existing JVM and instrumentation fixtures with controlled event orderings.
2. Capture a failing assertion before each production fix; verify recovery afterward.
3. Run affected JVM tests, static checks, packaging and current-source JNI scenarios.
4. For shared Rust changes, run offline locked workspace tests, required Clippy
   categories, cached Nix source parity and Android-native compilation.
5. Record exact commands, source revisions, artifacts, results and exclusions here.

| Operational Limit | Enforcement |
| --- | --- |
| Physical devices and hosts | Fresh authorization required; no deployment or personal-flake changes |
| Temporary storage | Check all `/tmp/p2p-vpn-*` before builds; remain below 10 GiB |
| Evidence retention | Preserve raw evidence and user data; ask before necessary cleanup |
| Build resources | Cached tools; at most two Cargo jobs; conservative Gradle concurrency |
| Downloads | At most 10 Mbps; no uncontrolled fetches |
| Timing observations | No concurrent builds, manual repair or relaxed deadlines |
| Cleanup | Bounded logs and watchdogs; terminate owned emulator/fixture processes |

No physical device was used. Completion relies on the full requirement audit,
not AL1 alone; individual runs retain their documented source and platform limits.
