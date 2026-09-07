# Android Multi-Network Review Run

## Latest Attempt

The network-labelled build at `23e5159d` failed automatic process-death recovery
on 2026-09-07. It recorded 46 passing steps and one failure from 00:48:01Z
through 00:56:01Z (480 seconds). Reboot and final isolation were not reached.

| Evidence | Result |
| --- | --- |
| Earlier stages | Pairing, concurrent traffic, overlap rejection, disable, APK replacement, and both underlay transitions passed |
| Restored process | Both networks enabled and running; only one VPN peer connected |
| Alpha | Relay connection followed by a direct connection and capabilities exchange |
| Beta recovery queries | Logged at epoch seconds 1788742352.874, 1788742392.434, and 1788742462.433 |
| Beta query results | Each emitted a final `get_record` event within one second; no beta overlay connection followed |
| Attribution | `runtime_network` identifies beta independently of Tokio worker IDs |
| Cleanup | All six safeguards passed; no emulator or fixture process remained |

The result log does not distinguish a missing record from other `get_record`
outcomes. Recovery timers are running; address lookup and bootstrap behavior
need a focused restart reproduction before attributing a transport defect.

- Evidence: `/tmp/p2p-vpn-review-scoped-multi-network/evidence.json`.
- Native rebuild, offline Gradle unit/lint/assembly, and fixture build passed.
- No manual connection repair or deadline extension occurred.
- Next: isolate private-bootstrap peer-address lookup across a client restart.

### Labelled Run Artifacts

| Artifact | SHA-256 |
| --- | --- |
| Debug APK | `61aad1eefb51748653f15fab6e4cd0a810f653da1d9f87119088a8968d44bb73` |
| Unstripped x86_64 JNI | `7fd3b998fd24b3f7d6ff17dfe460c11194ec533c4c3b63a087132cec720ba3da` |
| Stripped JNI | `2b884044e10b77a195db3a2c26ab02815bcc392602216c32be344bb4030bd6e7` |
| Linux fixture | `436d1ec0beff9cfe04df60979d2092d5ca686695e497a50046227328a35caaad` |

The Linux CLI remains the packaged `461894ad` runtime used in preceding runs;
it is not the rebuilt fixture or Android runtime.

## Earlier Traced Attempt

The opt-in traced fixture at `bac49191` failed reboot recovery on 2026-09-07,
before reaching final isolation. It recorded 58 passing steps and one failure
from 00:25:29Z through 00:37:27Z (718 seconds).

| Evidence | Result |
| --- | --- |
| Earlier stages | Pairing, concurrent traffic, independent disable, updates, underlay transitions, process restoration, and lockdown recovery passed |
| Reboot | Android boot completed; both network profiles were enabled and running |
| Recovery deadline | Only one connected VPN peer returned within the existing 240-second status check |
| Fixture counters during wait | Alpha reported one supported peer path; beta reported zero |
| Final Android snapshot | Wi-Fi validated, one direct QUIC stream path, zero relay paths, 37 public routing peers |
| Final isolation | Not reached; this run cannot explain the earlier missing fifth reply |
| Cleanup | All six safeguards passed; no manual app restart or repair |

Beta's retained log contains direct-dial timeouts and relay-address removal
after missing destination reservations. This identifies failed recovery attempts,
not why fresh reachable addresses failed to restore beta.

- Evidence: `/tmp/p2p-vpn-review-traced-multi-network/evidence.json`.
- Traced fixture SHA-256: `c2515e7267883431e58f8285879c5e122c3d85db99f4a54c753421c6c6ed5d93`.
- APK and runtime binaries match the preceding isolation-failure run below.
- Next: investigate beta's post-reboot address refresh and recovery scheduling.

### Runtime Event Attribution

Runtime event-loop logs now include `runtime_network`, scoped to the runtime
future rather than its worker thread. Tokio can move a future between workers;
thread IDs in older Android logs cannot reliably identify a network.

- Network names use the existing log-value escaping; no keys or payloads are added.
- Linux and Android use the same additive field, including startup failures.
- Independent spawned tasks and library logs do not inherit this task-local scope.
- A fixture's bootstrap and overlay runtimes share a network name; this label alone does not distinguish those roles.
- This is diagnostic instrumentation, not a recovery fix; the labelled run above failed recovery.

Verification passed: three focused log tests, the workspace suite (1,231 passed,
19 opt-in ignored), required Clippy categories, Rust formatting, and the Nix
`rust-test-sources` check. The labelled emulator run verifies attribution;
physical-device verification and successful recovery remain open.

## Earlier Isolation Failure

The rebuilt APK and Linux fixture at `fe950142` failed the final isolation stage
on 2026-09-07. The run lasted 652 seconds, from 00:05:06Z through 00:15:58Z.
It recorded 65 passing steps and one failure; two final checks were not reached.

| Evidence | Result |
| --- | --- |
| Earlier lifecycle stages | Pairing, dual-stack concurrent traffic, updates, underlay transitions, and reboot passed |
| After alpha fixture termination | Beta Linux-to-Android IPv4 and IPv6 each received 5/5 replies |
| Beta Android-to-Linux IPv4 | Received 4/5 replies, sequences 1, 2, 3, 4 |
| Failed measurement interval | Epoch milliseconds `1788740130522` through `1788740135550` |
| Remaining checks | Reverse IPv6 and final isolation assertion were not reached |
| Cleanup | All six harness cleanup safeguards reported success |

The probe used `ping -c 5 -W 5`. The fixture generates incoming echo replies
directly in `AgentPacketWriter::write_packet`, without a probe deadline there.
This does not establish whether the missing packet was lost or delayed.

Final Android diagnostics show one expired 84-byte queued packet and ten
outbound drops. These are aggregate counters, not per-network measurement
deltas; they cannot identify the missing packet or establish its cause.

- Original evidence: `/tmp/p2p-vpn-review-final-multi-network/evidence.json`.
- Cached native rebuild, offline Gradle unit tests, lint, and APK assembly passed.
- No retry or relaxed assertion replaced the failed measurement.
- Next: capture per-network packet progress and counter deltas around isolation.

### Opt-In Packet Trace

Set `P2P_VPN_ANDROID_E2E_TRACE_ECHO=1` when running `scripts/android-e2e.sh`.
Each fixture records at most 4,096 echo events in its existing bounded log;
tracing is disabled by default and does not change packet acceptance.

| Stage | Meaning |
| --- | --- |
| `runtime_write` | VPN delivered an echo packet to the fixture |
| `reply_generated` | Fixture generated a reply before enqueueing it |
| `runtime_read` | VPN consumed a packet from the fixture's outbound queue |

Events contain family, ICMP kind, identifier, sequence, and epoch milliseconds.
They omit IP addresses and payloads. A read does not prove network delivery;
compare separate fixture logs with Android ping intervals before attributing loss.

Validation: all ten fixture tests, Rust formatting, and the repository's required
Clippy categories passed. Strict `-D warnings` failed on 99 existing core-library
warnings. The traced emulator run above failed before final isolation.

### Isolation Run Artifacts

| Artifact | SHA-256 |
| --- | --- |
| Debug APK | `7b88fcc3f11c5df76674ac5f26fd1a0b25e518f473bf420ca70b1641cdd8f847` |
| Unstripped x86_64 JNI | `2875e212a1ba42f388467dc132e6f1179cec87c74422838a27670d7dcdc20240` |
| Stripped JNI / library extracted from APK | `972b7732a25c540a65186fe1baafc690f1ff5589b77d585065f97dc57609a1b8` |
| Linux fixture | `3450e4079b16bf9a03a83752c23811eac783442e9df9da7e9bcdf6a32ed5dd75` |
| Linux CLI | `649b8ffe2a536df7563d8d4b3cf3dd720883c851f331088e7316e0ba8853eae1` |

## Earlier Passing Attempt

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

### Earlier Passing Resource Samples

| Sample | PSS (KiB) | Java Threads | Queued Packets / Bytes |
| --- | ---: | ---: | --- |
| Two networks after reboot | 74,514 | 7 | 0 / 0 |
| After alpha fixture termination | 74,210 | 7 | 0 / 0 |

Final diagnostics recorded 35 public routing peers, one direct TCP path, one
expired queued packet, and 24 outbound drops after the failure-isolation stage.
These debug snapshots do not prove battery efficiency, leak freedom, or zero loss.

### Earlier Passing Artifacts

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
