# Android Multi-Network Review Run

## Result

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
