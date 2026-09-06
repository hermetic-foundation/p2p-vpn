# Android Network Workflow Review

## Result

Passed on 2026-09-06 against runtime revision `6dbb680c`. All 24 completed
steps passed in 136 seconds, including startup and cleanup.
Two additional entries recorded startup progress.

| Workflow | Evidence |
| --- | --- |
| Network navigation | Empty home, nested create form, and nested join form rendered |
| Device hostname | Android name normalized to `managed-test-phone` and sent during pairing |
| Join by code | Signed profile created and persisted without a placeholder config |
| Activation | Detail switch started the VPN and reported Connected |
| Peer display | Identity, address, path, and membership provenance rendered |
| Dual-stack traffic | Linux-to-Android and Android-to-Linux each passed 5/5 IPv4 and IPv6 packets |
| Disable | Final network switch stopped the VPN while preserving its profile |
| Re-enable | Same profile reconnected without another pairing; readiness probes converged |
| Theme | Dark/light system changes preserved desired connection state |

The first traffic measurement followed successful readiness probes on the first
attempt. Re-enablement needed two Linux readiness attempts; it did not repeat
the four 5-packet measurements. This is convergence evidence, not lossless switching.

## Method

- One clean API 35 x86_64 KVM emulator, two virtual CPUs, and 2,560 MiB guest memory.
- Current x86_64 JNI, APK, CLI, and Linux fixture; selected APK explicitly installed.
- Cached Nix Rust/NDK/SDK tools and offline Gradle; no review builds during the run.
- Four-GiB runtime-growth guard; retained evidence occupies about 1.4 MiB.
- Screenshots of the empty home and live peer page were inspected.

```bash
P2P_VPN_ANDROID_E2E_MAX_RUNTIME_GROWTH_BYTES=4294967296 \
  scripts/android-e2e.sh --scenario network-workflow --output /path/to/evidence
```

Supply the emulator, ADB, APK, fixture, and CLI executable variables from the
[Android development guide](android.md). A separate preflight passed before launch.

## Artifact Identity

Native compilation passed in 29.32 seconds; fixture compilation passed in
4.46 seconds. Gradle assembly, unit tests, and lint passed in six seconds
(78 tasks: five executed, 73 up-to-date).

| Artifact | SHA-256 |
| --- | --- |
| Native library and merged JNI input | `24eaaf840292c7c8cc2e3c690fdca64c7eaf5cea5158073e6523812aaf371c0d` |
| Stripped library and library extracted from APK | `312496b6ff14342df9b3cd2abd206e5193b0e88d8a901ae1695c8618ee8ebd79` |
| Debug APK | `8aa794e2a6f80cc982dd43e9c8c90ac0fee1d5810287d73591b726babcd0d83c` |
| Linux fixture | `b77fa69e35ae249ab3dcbc7976df8e3ceb1e574e35dc236769f1f348720d94a4` |
| CLI | `58520e3ea49c40dc2d81c2bfdb0e3f290a586dfbca029ee638ff625e7c5ccd80` |

## Resource Sample

| Final diagnostic | Value |
| --- | ---: |
| PSS | 87,606 KiB |
| Process CPU time | 5,398 ms |
| Service uptime | 96,880 ms |
| Java active-thread estimate | 15 |
| Queued packets / bytes | 0 / 0 |
| Outbound packet drops | 1 |
| Public routing peers | 17 |

`Thread.activeCount()` supplies the thread estimate; it is not the native thread
count. UI work, pairing, and reconnects preceded this sample. Do not compare it
directly with the older multi-network samples or infer a memory improvement.

## Limits and Cleanup

- Discovery receives a controlled fixture-address hint; this is not public NAT or relay discovery proof.
- The peer screen showed QUIC stream; final path diagnostics included QUIC and TCP stream availability, not datagram traffic.
- No physical phone, cellular transition, reboot, concurrent networks, revocation, or battery soak was tested here.
- No injected persistence failure, cancellation-after-prepare, or historical-artifact replacement occurred in this scenario.
- Harness cleanup reported stopped emulator/fixture, removed private state, and redacted logs; host checks found no remaining test processes.

## Evidence

| Artifact | Location |
| --- | --- |
| Derived, sanitized summary | [JSON sample](android-network-workflow-review-sample.json) |
| Original harness report | `/tmp/p2p-vpn-review-history-network-workflow/evidence.json` |
| Build logs | `/tmp/p2p-vpn-review-history-android-*.log` |
| Broader acceptance map | [Verification coverage](review-verification.md) |

The [older multi-network run](android-multi-network-review.md) remains separate
historical evidence. Passing this scenario does not close the remaining review work.
