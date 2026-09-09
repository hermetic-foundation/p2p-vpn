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
- [ ] Test profile-join cancellation, late completion and subsequent successful joining.
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

## Candidate Ordering: Cancelled Profile Join

Status: source-backed hypothesis; not yet reproduced or fixed.

1. Native join returns a successful profile to the dedicated join worker.
2. The service worker processes `cancelProfileJoin` before queued completion.
3. Cancellation signals JNI but leaves the Java operation eligible for completion.
4. `completeProfileJoin` checks only the operation ID before saving the profile.

The JNI request may already have finished, so native cancellation alone cannot
fence this local commit. Reproduce the ordering at the real service boundary,
then verify cancellation cleanup and a successful replacement join.

Cancellation after a completed local commit is a different case. Do not silently
revoke committed membership or alter the network's revocation policy.

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

No build, emulator or physical-device scenario was started for this initial map.
