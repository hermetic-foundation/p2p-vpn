# Sustained Android Resource Review

## Status

Cached capability preflight passes. No emulator or physical device was started.
Artifact reconciliation found that the available APK and fixture cannot yet serve
as current-source resource evidence. Sustained capture has not begun.

## Capability Evidence

Preflight ran `scripts/android-e2e.sh --scenario multi-network --preflight`
on 2026-09-10 at 03:37:17 UTC. The selected launcher, ADB, APK,
Linux fixture, CLI and KVM access passed availability checks.

- [Portable preflight result](sustained-android-preflight.json).
- Raw result: `/tmp/p2p-vpn-sustained-android-preflight/evidence.json`.
- Result SHA-256: `e94b3d96be61a3fd81b05942eb6b8b5c81cb0bde9139c68bdc9cafad112832b9`.
- Harness SHA-256: `d3f993bad2a8cf31340c1a5b484ab44984c35622450290175af81335477d6854`.

Preflight checks presence and capabilities, not source freshness, private egress,
sampling completeness or performance. Its `passed` status closes none of those gates.

## Artifact Reconciliation

| Artifact | SHA-256 | Interpretation |
| --- | --- | --- |
| Available debug APK | `63eb12af5b8b613bfb2bab8aae91d619cc7fd82c14976a626cee42108d3b181c` | Packaging must be refreshed and verified |
| JNI extracted from APK | `b49c3820ee5a3d23e463f75323cd06a9cc4bf7f4861cf9e261f00764ca07b41a` | Not the same machine-code section as cached current JNI |
| Cached unstripped current JNI | `88bbe21ef0948b93b2116a5cb30f2d460bcf1662733ea2bdac8a6ba9587bc203` | Matches native validation of production fix `7b59625f` |
| Available Linux fixture | `3d5a7f52bd82918b8cdbeeeafe14ff372e8fbcad36e66a02f66858eaa9d756df` | Matches historical lifecycle fixture; rebuild before current-source capture |

Stripped and unstripped file hashes naturally differ. To avoid treating that alone
as a mismatch, the audit compared `.text` hex dumps using the same cached GNU
readelf 2.46 executable; those differ too. Neither ELF supplied a GNU build ID.

| `.text` dump SHA-256 | Value |
| --- | --- |
| APK JNI | `4f7c4b52007ab579427290a73376d71cd4a168c7fd4d1557065386a6a67f5105` |
| Cached JNI | `51cac321fa540baff3c5069021efa67c68dc2f332193a3d12ab822d8e2923ddb` |

This detects nonidentical executable content; it does not identify every source
revision represented by the APK. Keep historical lifecycle evidence separate.

## Collector Audit

| Existing source | Supplies | Limitation / Required Addition |
| --- | --- | --- |
| `P2pVpnService` diagnostics | `Process.getElapsedCpuTime`, PSS, private dirty, Java heap | Pair CPU deltas with monotonic time and stable process identity |
| `Thread.activeCount` | Java thread-group estimate | Not Linux task count; collect OS tasks/descriptors independently |
| Multi-network resource gate | One diagnostic snapshot after reboot | Not a time series or sustained bound |
| Concurrent traffic helper | Five packets per leg with up to three batch attempts | Retrying batches is readiness evidence, not an uninterrupted sustained workload |
| Diagnostics and lifecycle controls | Network state, queues, generation and enablement | Retain per-network identities and counters throughout independent transitions |

Wakeup/scheduled-work proxies still need an emulator collector. Do not relabel
process CPU time or Java thread count as wakeup frequency, battery consumption
or physical thermal evidence.

## Safe Execution Boundary

- Run only an owned cached emulator, selected explicitly by its serial; no physical-device commands.
- Place emulator, private bootstrap fixtures and their ADB server in an isolated network namespace with no public route.
- Confirm private endpoint reachability and absence of external routing before workload admission.
- Use a private temporary directory and owned-process cleanup; do not modify deployed services or personal flakes.
- Recheck total `/tmp/p2p-vpn-*` usage before packaging and provisioning; remain below 10 GiB.
- Preflight used a 1,258,291,200-byte runtime growth cap; recalculate available headroom before an actual run.

The cached launcher normally uses the host network and an ADB server. Private
Kademlia protocol configuration alone is not an OS egress restriction. Do not
launch the historical scenario unchanged for this goal's no-public-WAN boundary.

## Next Work

1. Refresh cached JNI packaging and the Linux fixture; verify the APK's packaged native code against the selected build.
2. Add the cheap OS/resource collector and measured collector-on/off controls.
3. Freeze S7's 30-second warmup, 300-second idle/load windows, five independent transitions, actual offered load and watchdogs.
4. Require healthy sibling traffic and identity continuity while the other network is disabled or unavailable.
5. Run paired captures; audit cadence, recovery, teardown and storage cleanup before accepting results.

No Android resource or multi-network sustained acceptance is claimed yet. Linux
allocation attribution remains open in parallel with this workstream.
