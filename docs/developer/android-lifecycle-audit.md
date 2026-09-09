# Android Lifecycle Audit

## Status and Scope

Active bounded review, opened against `d0e38487` on 2026-09-09.
This checklist is not a completion report. Earlier results retain their original
source revisions and platform limits; no new device run is claimed here.

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
- [ ] Finish source ownership inventory, including failure and replacement paths.
- [ ] Test rapid commands and stale callbacks within and across service scopes.
- [ ] Test permission success, denial and activity recreation without stale activation.
- [x] Test profile-join cancellation, late completion and subsequent successful local commit (AL1).
- [ ] Verify multi-network enablement, persistence and native generation isolation.
- [ ] Audit native shutdown, TUN descriptors, callback retirement and timer bounds.
- [ ] Reconcile process death, always-on, reboot and app replacement on final source.
- [ ] Run bounded cached-emulator lifecycle and applicable underlay scenarios.
- [ ] Record historical failure dispositions and precise missing attribution evidence.
- [ ] Run affected checks, inspect final diff, publish each atomic commit to `main`.
- [ ] Audit every requirement before marking this review complete.

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
| Multi-network lifecycle | [68-check pass](android-multi-network-review.md#latest-attempt) at `deedd041` | Emulator process death, update, reboot, underlay and isolation; not current-source certification |
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
- Outstanding: saved-state recreation, actual permission delivery and local-network permission outcomes.

`MainActivity` persists a pending enable network in instance state. Pending join
code/hostname are not serialized there; rotation before local permission returns
can discard that unstarted request. No enrollment exists to commit at that point.

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
Full activity recreation and permission outcome validation remain open.

| Previous Ordering | Consequence |
| --- | --- |
| `onStart` requests an asynchronous binding | `bound` remains false until the callback |
| Activity stops before the callback | `onStop` skips unbinding because `bound` is false |
| Late `onServiceConnected` arrives | Stopped activity attaches a binder and listener |
| System disconnects before activity stop | Connected state clears, but the binding registration still needs release |

Each activity start now creates a distinct connection owner. Registration state
is separate from connected state; stop retires the owner before releasing its
listener and binding. Old connect, disconnect and snapshot callbacks are ignored.

### Verified and Pending

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

JNI is the same cached artifact identified in AL1. Current-source native and
broader multi-network reconciliation are still required by this review.

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

No physical device was used. The AL1 run above does not close the remaining
inventory, permission, multi-network, underlay and historical-attribution gates.
