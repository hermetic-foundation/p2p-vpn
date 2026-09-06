# Android Multi-Network Review Run

## Latest Attempt

The rebuilt APK and Linux fixture at `643d798e` passed all 68 checks on
2026-09-06. The run took 913 seconds, from 21:02:01Z through 21:17:14Z.
No manual connection or service intervention occurred during the scenario.

| Stage | Result |
| --- | --- |
| Pairing, migration, and concurrent traffic | Two isolated identities carried dual-stack traffic through the shared TUN |
| Disable and APK replacement | Alpha stayed disabled; beta passed all four traffic directions/families |
| Re-enable and overlap rejection | Both networks remained isolated and reachable |
| Wi-Fi / emulated cellular / Wi-Fi | Both networks recovered without restarting the shared runtime |
| Process death and APK replacement | Always-on restored both identities and traffic automatically |
| Temporary lockdown | Expected rejection followed by automatic recovery |
| Emulator reboot | Both networks restored and passed dual-stack traffic |
| Alpha fixture termination | Beta traffic, Android process, and shared-runtime generation remained stable |

Readiness checks retried before steady measurements. Alpha required four Linux
IPv4 attempts after the cellular transition and ten after reboot. Each subsequent
traffic measurement received 5/5 replies; this is not uninterrupted-delivery evidence.

The new harness retained ping intervals and successful reply sequence numbers.
This pass follows connection-selection, closure, and stream-budget fixes, but
does not establish which change caused either earlier failure to disappear.

- Original: `/tmp/p2p-vpn-review-transport-multi-network/evidence.json`.
- Sanitized [current transport review sample](android-multi-network-transport-review-sample.json).
- Cached Nix Rust/NDK rebuild and offline Gradle unit, lint, APK, and instrumentation assembly passed.
- This run did not execute the separate native-failure health-poll instrumentation case.
- Cleanup reported all six safeguards successful; no emulator or fixture process remained afterward.

### Current Resource Samples

| Sample | PSS (KiB) | Java Threads | Queued Packets / Bytes |
| --- | ---: | ---: | --- |
| Two networks after reboot | 74,514 | 7 | 0 / 0 |
| After alpha fixture termination | 74,210 | 7 | 0 / 0 |

Final diagnostics recorded 35 public routing peers, one direct TCP path, one
expired queued packet, and 24 outbound drops after the failure-isolation stage.
These debug snapshots do not prove battery efficiency, leak freedom, or zero loss.

### Current Artifacts

| Artifact | SHA-256 |
| --- | --- |
| Debug APK | `197db0303e9e167aaf35fb6ad08b928145d3dda808f61f9da172cb696068530f` |
| Unstripped x86_64 JNI | `5b26280db71d60997c2ef81ca713050e2591f25db4ffd13474af66c902d2ef7b` |
| Stripped JNI / library extracted from APK | `6ab5fdb54a927728cfe3d1dad27892594d1d98ba02096dbbee9bd72761f301a6` |
| Linux fixture | `17f6851bdd9112577c7a9d5f115e1215480f745b1149c70800bb55769dd7ba2e` |
| Linux CLI | `be2a08a2be83d14454ba499d0bfea0662ff3da9b4ef67ff00d24e6ab69d43d2e` |

This is an API 35 x86_64 emulator result. Physical cellular, carrier NAT, hotspot
VPN, sustained saturation, and the targeted health-poll recovery case remain
outside this run's evidence. Earlier failures remain below rather than being overwritten.

## Earlier Update Failure

The lifecycle-fixed APK (`39c2ecb6`) with diagnostic harness `2d4d06e7` failed
on 2026-09-06 after 262 seconds. Thirty-two steps passed before Android-to-Linux
IPv4 received 4/5 replies on beta after APK replacement with alpha disabled.

| Observation | Evidence boundary |
| --- | --- |
| Initial concurrent traffic and overlap rejection | Both networks passed dual-stack traffic checks |
| Independent disable | Beta passed both directions and address families |
| APK replacement | Disabled set restored; Linux-to-Android checks passed before reverse IPv4 lost one reply |
| OS underlay at failure | Validated non-VPN Wi-Fi and cellular agents were present |
| App state | Connected peer, Wi-Fi selected, no queue drops, one relay fallback and two direct promotions |
| Later scenarios | Re-enable, cellular transition, reboot, lockdown, and failure isolation were not reached |

The lost reply is not explained by the final aggregate counters. Logs show path
changes after restart, but this is not proof they caused the loss. Retain the
failure and investigate packet timing; do not replace it with a passing retry.

- Original: `/tmp/p2p-vpn-review-lifecycle-multi-network/evidence.json`.
- Sanitized [update-failure sample](android-multi-network-update-failure-sample.json).
- Cleanup stopped the emulator and fixtures, removed private state, and redacted logs.
- The original OS summary labels a mixed Wi-Fi/VPN agent as Wi-Fi; `not_vpn=false` remains correct.
- A subsequent parser fix recognizes mixed VPN transports; it does not fix packet recovery.

## Earlier Cellular Failure

The rerun at `6dbb680c` failed on 2026-09-06 after 456 seconds: 39 steps passed,
then Wi-Fi-to-emulated-cellular recovery failed. It used the verified APK and
fixture hashes in the [network-workflow report](android-network-workflow-review.md).

| Completed before failure | Result |
| --- | --- |
| Legacy migration and independent pairing | Passed |
| Concurrent dual-stack traffic | Both networks passed 5/5 packets in every direction and family |
| Overlap rejection | Passed without changing existing traffic |
| Independent disable and APK replacement | Beta stayed reachable; disabled alpha remained disabled after replacement |
| Re-enable | Concurrent dual-stack traffic restored |
| Cellular transition | Failed; app reported no selected or available underlay |

Final diagnostics showed `underlay.kind=none`, zero available networks, one
network-change signal, zero peers, and no runtime restart. Transport logs
reported network-unreachable errors. Root cause is not established.

The harness did not retain Android connectivity-service capabilities. The next
reproduction must capture OS underlay availability and validation independently
of the application's tracker before attributing this to routing or the fixture.

- Original report: `/tmp/p2p-vpn-review-history-multi-network/evidence.json`.
- Sanitized failure summary: [JSON sample](android-multi-network-failure-sample.json).
- Emulator and both fixtures stopped; private state was removed and logs redacted.
- Later reboot, lockdown, and single-fixture-failure stages were not reached.
- The single-network workflow pass does not override this failed gate.

## Historical Result

Passed on 2026-09-06 against runtime revision `f026342f` and its rebuilt x86_64
debug APK. The run took 837 seconds, including startup and cleanup. All 68
completed steps passed; three additional entries recorded startup progress.

| Scenario | Evidence |
| --- | --- |
| Independent pairing | Two identities paired by code behind one shared Android TUN. |
| Dual-stack traffic | Each measurement passed 5/5 packets in both directions and address families, on both active networks. |
| Overlapping routes | Rejected before persistence or runtime mutation; existing traffic remained available. |
| Independent enablement | Disabling one network preserved sibling traffic; re-enabling restored both. |
| Disabled-set persistence | APK replacement restored selected-disabled alpha and enabled beta. |
| Underlay transitions | Wi-Fi to emulated cellular and back recovered without restarting the shared runtime. |
| Always-on restoration | Process death, APK replacement, and emulator reboot restored identities and traffic. |
| Lockdown | Expected rejection followed by automatic recovery when lockdown was removed. |
| Failure isolation | Terminating alpha's fixture left beta traffic, Android process, and runtime generation intact. |

Readiness retries preceded some traffic measurements. This proves recovery after
convergence, not instantaneous recovery or uninterrupted packet delivery.

## Resource Samples

| Sample | PSS (KiB) | Java Threads | Queued Packets / Bytes |
| --- | ---: | ---: | ---: |
| Two networks after reboot | 71,083 | 6 | 0 / 0 |
| Final, after alpha fixture termination | 76,034 | 6 | 0 / 0 |

These are point-in-time debug-build measurements, not battery results, native
thread counts, or a long-duration leak test. The final diagnostic reported 52
public routing peers; this run does not establish low public-discovery activity.

## Method and Artifacts

- One clean API 35 x86_64 KVM emulator; 2 virtual CPUs and 2,560 MiB guest memory.
- Cached Nix Rust/NDK/SDK tools; host fixture rebuilt from the same current runtime.
- Test APK explicitly installed over the launcher's older bundled APK.
- Four-GiB runtime storage-growth guard; no concurrent review builds.
- Rootless Linux fixtures supplied controlled overlay discovery and packet endpoints.

```bash
P2P_VPN_ANDROID_E2E_MAX_RUNTIME_GROWTH_BYTES=4294967296 \
  scripts/android-e2e.sh --scenario multi-network --output /path/to/evidence
```

Supply the emulator, ADB, APK, fixture, and CLI executable variables documented by
the [Android development guide](android.md). This command does not download tools.

| Artifact | Location |
| --- | --- |
| Versioned result and sanitized diagnostics | [JSON sample](android-multi-network-review-sample.json) |
| Original bounded evidence | `/tmp/p2p-vpn-review-current-multi-network/evidence.json` |
| APK and JNI hashes | [Verification coverage](review-verification.md#android-startup-artifact) |

## Cleanup and Limits

- Harness cleanup reported emulator/fixture termination, private-state removal, log redaction, and cleared always-on settings.
- Host checks independently found no live harness, emulator, or fixture processes and confirmed both temporary state directories were gone.
- The review agent was closed after returning its source findings.
- No physical phone, carrier NAT, hotspot/VPN, or public-relay reachability claim follows from this run.
- The open [membership-sync lifecycle cases](membership-sync-review.md) were not exercised by this successful-traffic scenario.
