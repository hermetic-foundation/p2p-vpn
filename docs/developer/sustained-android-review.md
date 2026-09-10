# Sustained Android Resource Review

## Status

Cached capability preflight passes. The APK and Linux fixture have now been
refreshed, and the packaged JNI matches the selected native build. Isolated cached
emulator boot, process collection and two-network traffic admission pass.
No physical device was used; sustained capture has not begun.

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

## Pre-Refresh Artifacts

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

## Refreshed Artifacts

At source `460a3b9e`, the fixture rebuilt offline in 5.42 seconds. Gradle packaging
and checks completed offline in eight seconds using cached Gradle 9.5.1,
OpenJDK 17.0.20+8 and the SDK containing `android-37.0`.

| Artifact | SHA-256 |
| --- | --- |
| Refreshed debug APK | `e4881658d6a8fc734df0428caf50f4878ea5774e494b234ca0fcf1ee19f39911` |
| Instrumentation APK | `319170af6e2cadeeabfe7d813d75c0be5a9f6067d2c94b75489509b97682c0cd` |
| Packaged x86_64 JNI | `9430fcd79dba9ccd5d3290d031de750bc72cf8bd6f1a4d765da5eacb377e013c` |
| Refreshed Linux fixture | `4b1bcd69a410f9a830e813d9457aa0a55a8a507d762c208aad42a1092ab8f8fa` |
| JNI `.text` dump | `51cac321fa540baff3c5069021efa67c68dc2f332193a3d12ab822d8e2923ddb` |

The packaged JNI is byte-identical to Gradle's stripped JNI output. Its `.text`
dump matches the cached native build validated for `7b59625f`; subsequent changes
are test/documentation-only. No fresh native cross-compilation was needed here.

| Check | Result |
| --- | --- |
| Android unit task | Gradle accepted existing results as up-to-date; not a new test execution |
| Android lint | Pass |
| App assembly | Pass; native merge/strip/package tasks executed |
| Instrumentation assembly | Up-to-date; no device instrumentation executed |
| Linux fixture | Offline locked build passes; two Cargo jobs |
| Storage before builds | 9,072,752 KiB under `/tmp/p2p-vpn-*`, below 10 GiB |
| Storage after packaging and extraction | 9,118,752 KiB; recheck before emulator provisioning |
| Device use | None |

Logs: `/tmp/p2p-vpn-sustained-android-fixture-build.log` and
`/tmp/p2p-vpn-sustained-android-packaging.log`. Their SHA-256 values are
`a8b77a8b36564cdb8be71ebb8457d9357a283f74a7bf671d66d30247c15af022` and
`6f2cd04fbaf247aaed161b5584562d1dce535c0e186877970ebde2510b7d0c99` respectively.

The existing JNI init script selects the cached native staging symlink. Packaging
uses `--offline --no-daemon --max-workers=2 -Dorg.gradle.parallel=false` with
`:app:testDebugUnitTest :app:lintDebug :app:assembleDebug :app:assembleDebugAndroidTest`.

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

The local wrapper now passes namespace/cleanup checks:

```sh
timeout --signal=TERM --kill-after=5s 1200 \
  bash scripts/android-resource-isolation.sh -- <trusted-review-command>
```

This is a wrapper example, not an approved S7 workload command. Workload admission,
collector controls and the final capture watchdog still need to be frozen.

- Run only an owned cached emulator, selected explicitly by its serial; no physical-device commands.
- Place emulator, private bootstrap fixtures and their ADB server in an isolated network namespace with no public route.
- Confirm private endpoint reachability and absence of external routing before workload admission.
- Use a private temporary directory and owned-process cleanup; do not modify deployed services or personal flakes.
- Recheck total `/tmp/p2p-vpn-*` usage before packaging and provisioning; remain below 10 GiB.
- Preflight used a 1,258,291,200-byte runtime growth cap; recalculate available headroom before an actual run.

The cached launcher normally uses the host network and an ADB server. Private
Kademlia protocol configuration alone is not an OS egress restriction. Do not
launch the historical scenario unchanged for this goal's no-public-WAN boundary.

### Wrapper Validation

| Gate | Verified Result |
| --- | --- |
| User/network/mount/PID namespaces | Wrapper creates fresh namespaces and rejects matching parent namespace IDs |
| Networking | Only loopback; IPv4/IPv6 routes use loopback only |
| USB | Private empty mount hides `/dev/bus/usb`; host bus directory listing unchanged |
| ADB | Dedicated loopback TCP server endpoint; inherited serial removed; automatic mDNS connect disabled |
| KVM | Device remains present/readable/writable; subsequent isolated boot passes below |
| Process exit | Child exit 23 propagates; invalid invocation rejected |
| Descendants | Tagged live descendant disappears after namespace init exits |
| Timeout | External timeout returns 124 and leaves no tagged isolated command |
| Static checks | ShellCheck and shfmt pass; both scripts added to the Nix Android structure check |
| Nix | Structure-check derivation evaluates offline; full existing Android shell matrix not rerun |

Run the opt-in local checks with:

```sh
timeout --signal=TERM --kill-after=2s 30 \
  bash tests/android-resource-isolation.sh
```

Verified log: `/tmp/p2p-vpn-android-resource-isolation-verified-tests.log`, SHA-256
`b209683f962c271678090b827e68977100c7c528cc301a42fdd29b6b75b683ff`.
The earlier timeout attempt failed because multicall `sleep` rejected renamed
`argv[0]`; the corrected test verifies a live tagged shell before checking cleanup.

This runs trusted local review commands; it is not a filesystem sandbox for
untrusted programs. The workspace remains shared. No Rust/Android source changed,
so their build/test suites were not repeated for this shell-only addition.

## Next Work

1. Preserve refreshed artifact hashes and verify them again before emulator admission.
2. Integrate runtime sampling and collector-on/off controls with the emulator-tested process collector.
3. Freeze S7's 30-second warmup, 300-second idle/load windows, five independent transitions, actual offered load and watchdogs.
4. Require healthy sibling traffic and identity continuity while the other network is disabled or unavailable.
5. Run paired captures; audit cadence, recovery, teardown and storage cleanup before accepting results.

No Android resource or multi-network sustained acceptance is claimed yet. Linux
allocation attribution remains open in parallel with this workstream.

## Isolated Boot Results

The first attempt failed before readiness: ADB treats the numeric socket
`tcp:127.0.0.1:5037` as remote and will not auto-start a server. Changing only
the socket to `tcp:localhost:5037` allows auto-start inside the same isolated network.

| Attempt | Result | Evidence SHA-256 |
| --- | --- | --- |
| Numeric socket | Launcher exited before readiness | `19c40b0e37094d93a95a921f4414d72d92067a2c660be8008af766e414d78beb` |
| Localhost socket | Boot smoke passes in 33 seconds | `561bb2c5e857856072d13261ea753bcc8f93992cc65aaf31ad647239991346a0` |

[Portable evidence](sustained-android-boot-samples.json) preserves both attempts.
Raw directories are `/tmp/p2p-vpn-sustained-android-isolated-boot` and the same
path suffixed `-2`. Successful emulator log SHA-256:
`502a0ffaf85709fbf9aa6f019482d301554ef386458da4e034a6a501e2a83a68`.

### Frozen Boot Controls

- API 35 x86_64 cached emulator; no downloads or concurrent builds.
- Isolated network/process/mount namespaces with USB hidden and private ADB.
- 200-second inner timeout plus 15-second kill grace; 240-second outer timeout plus 20-second grace.
- Original 1,258,291,200-byte runtime growth cap, unchanged between attempts.
- Dedicated `TMPDIR=/tmp/p2p-vpn-sustained-android-boot-state`; existing harness cleanup retained.
- Before first attempt: 9,118,764 KiB across `/tmp/p2p-vpn-*`, below 10 GiB.

The boot scenario verifies the device contract, installed package, activity and
structured debug status. All six harness cleanup checks pass; no matching emulator
process remains. The dedicated temporary root retains only 68 KiB of small tool state.

### Scope Limits

- The immutable launcher installs its bundled historical smoke APK; boot-smoke does not reinstall the refreshed APK.
- No profile is stored, no VPN runtime is connected, and public-routing peer count is zero.
- Android reports unvalidated emulated Wi-Fi; its `internet` capability is not proof of external reachability.
- A modem `::1` resolution warning remains; no cellular-emulation claim is made.
- The one CPU/PSS diagnostic is only smoke evidence, not a sustained resource sample or physical battery estimate.

The first log also reported missing AVD registration. A separate isolated AVD
creation succeeded, and the boot retry required no AVD change; do not attribute
that secondary message to a new application or AVD defect.

### Regression Validation

With `P2P_VPN_ADB` pointing to cached ADB, `tests/android-resource-isolation.sh`
also verifies real private-server auto-start and an empty device list. The regular
namespace, failure-propagation and descendant-cleanup checks continue to pass.

ShellCheck and shfmt pass. Validation log:
`/tmp/p2p-vpn-android-resource-isolation-adb-tests.log`. This shell-only correction
does not require repeating unchanged Rust/Android builds or device lifecycle tests.

## Process Collector

`scripts/android-process-sample.sh` emits one JSON object per observation. Run it
from a privileged ADB shell inside the isolated root-capable emulator. Local
shell tests and emulator compatibility pass; collector overhead is not verified yet.

| Field | Meaning / Limit |
| --- | --- |
| PID and start ticks | Checked before and after observation; changes abort capture |
| Started/finished uptime | Kernel uptime seconds; includes observation duration |
| User/system ticks | Process CPU ticks; record device `CLK_TCK` before conversion |
| OS threads | Kernel process thread count, not Java `activeCount` |
| RSS | Kernel RSS in KiB; not allocation attribution or PSS |
| Descriptors | Non-atomic count of visible descriptor links; inaccessible directory is null |
| Leader context switches | Main-thread voluntary/involuntary counters, not all-thread wakeups |

### Invocation Contract

```sh
# Only inside the isolated wrapper, with an explicitly selected owned emulator.
adb -s "$serial" shell -T \
  sh -s -- "$app_pid" 300 < scripts/android-process-sample.sh
```

- Accepts 1 to 900 samples; rejects invalid or zero PID and noncanonical numeric arguments.
- Sleeps one second between observations; actual cadence includes collection overhead.
- Missing optional fields become null; unreadable/malformed process identity aborts.
- Pair output with an external watchdog, byte cap, app identity and device clock metadata.
- `P2P_VPN_SAMPLE_PROC_ROOT` exists for synthetic parser tests; do not override during capture.

### Validation and Remaining Gates

`timeout 10 bash tests/android-process-sample.sh` passes live-process and synthetic
checks, including names containing spaces/parentheses, missing optional fields,
malformed stat records and argument bounds. ShellCheck and shfmt pass.

Both scripts are included in the Nix Android structure lint check. No production
code changed; unchanged Rust and Android builds were not repeated. This collector
alone does not satisfy S7 or measure scheduled work, PSS, queues or network isolation.

## Collector Emulator Results

The `process-sample-smoke` scenario installs the selected refreshed APK, obtains
privileged ADB on the owned emulator, verifies UID zero, and checks ten samples.
It does not configure or connect a VPN network.

| Attempt | Result | Evidence SHA-256 |
| --- | --- | --- |
| 1: `exec-out`, run-as | Shell prompt instead of JSON; original command timeout fired | `85bd7a1eb7c3365499cd19b9beb6444270af81dbeea85d1d0b5d9a028714f64e` |
| 2: `shell -T`, run-as | `/proc/uptime` permission denied; failed without samples | `1d7d854893485af1672a1931d62ee9a30b07668c8a31fa059fc6820159c510f8` |
| 3: `shell -T`, privileged | Ten samples pass; device `CLK_TCK` is 100 | `f242d52541b86381fd84c0ba8b9c83b51275150653b72f9376fd7e69ec1dbfa4` |

Raw directories: `/tmp/p2p-vpn-android-process-smoke-{1,2,3}`; outer logs use
the same paths with `.log`. Sample JSONL SHA-256:
`d64bd7d7496e69fe9d2cfef2bfad794e573a2f89a32884c29b58b4c8ffe62699`.
[Portable samples](android-process-smoke-samples.json) retain all ten observations.

### Controls and Scope

- Same refreshed APK hash recorded above; API 35 x86_64; no builds or public route.
- Fixed 200-second inner and 240-second outer watchdogs, with 15/20-second kill grace.
- Same 1,258,291,200-byte runtime growth cap; no deadline extensions or manual rescue.
- Initial storage: 9,118,916 KiB; before attempt 3: 9,118,988 KiB. In-run audit: 9,623,624 KiB.
- Every attempt reports all six cleanup checks passing; no matching emulator remains.

Samples span uptime 26.24 to 35.35 seconds with stable PID/start identity,
20 threads and 125 descriptors. RSS changes from 153,064 to 153,448 KiB.
This boot-adjacent interval is compatibility evidence, not a plateau or CPU baseline.

ShellCheck passes. The large existing harness does not match current shfmt output;
no whole-file formatting rewrite was applied. The small collector tests remain
the local parsing gate; S7 and collector overhead controls are still outstanding.

## Two-Network Admission

`multi-network-resource-admission` reuses profile creation, pairing and shared-TUN
activation from the lifecycle fixture, then stops after initial traffic checks.
It is not a substitute for sustained load or independent network transitions.

| Gate | Result |
| --- | --- |
| Networks | Alpha and beta both running; independent identities and addresses |
| Transport snapshot | Two direct QUIC-stream paths; zero TCP, relay or owned-datagram paths |
| Traffic | 5/5 packets on each of eight direction/address-family legs |
| Infrastructure | Two private bootstrap peers; no public route in the namespace |
| Cleanup | All six harness checks pass |

The runtime field `public_routing_peers=2` counts the private fixture bootstrap
peers in this topology. It is not evidence of connections to public IPFS peers.
Traffic batches can retry for admission; they cannot prove fixed offered load.

[Portable admission evidence](android-resource-admission.json) records the passing
steps and snapshot. Raw captures remain under `/tmp/p2p-vpn-android-resource-admission-{1,2}`.

### Path-Length Failure and Correction

The first attempt stopped before emulator startup. Under the long dedicated
`TMPDIR`, the second fixture's control socket path was 112 bytes; the first was
102. A direct Rust `UnixListener::bind` check accepts 102 and rejects 112.

- The initial Perl probe truncated the address and was unsuitable as negative evidence.
- Rust reproducer: `/tmp/p2p-vpn-unix-path.rs`; cached compiler and linker, no Cargo build.
- Retry used `/tmp/p2p-vpn-a`; no timeout increase or runtime recovery change.
- The harness now checks the longest fixture socket path before starting processes.
- `tests/android-fixture-path-budget.sh` verifies 107-byte acceptance and 108-byte rejection.

| Evidence | SHA-256 |
| --- | --- |
| Initial readiness failure | `1820eb63d11d647d457473c0c5702127340145aef7028d55915be51602f069ed` |
| Short-path admission pass | `bfae1e9268e195ed403f2b8a1b500070193bfbe94b0b405838d23e844c3663ca` |
| Early path-guard failure | `9cc09db8b387c247f40f047d28d74aa6a2785fb57ef16f1e8b3d0c5f88f161d3` |
| Admission snapshot | `a1ac00e588d2c35e795d6dd04d2255a7f443e381fcfa3d86c580c3320829ea60` |

The guard capture is `/tmp/p2p-vpn-android-resource-path-guard`; it rejects the
long path before starting fixtures or emulator and reports complete cleanup.
The fixture's existing readiness error still obscures early task failures generally.

### Frozen Admission Controls

- 440-second inner timeout plus 15-second grace; 480-second outer timeout plus 20-second grace.
- 1,258,291,200-byte runtime growth cap, same APK and fixture hashes recorded above.
- CLI SHA-256: `b43c0dc2e8a81960901a4ed18fa2d1af67c7af995a2d0bfc21290df365a28313`.
- Storage before first run: 9,119,032 KiB; before retry: 9,119,800 KiB, both below 10 GiB.
- No concurrent builds or public networking; only owned emulator and private fixtures.

The fixture probe is response-paced and reports requested count as `sent`.
Do not use it as the S7 fixed-rate load generator. Sustained workload admission
still needs actual paced packet counts, observer controls and per-network sampling.

ShellCheck, the socket boundary regression and new-test formatting pass. The Nix
structure derivation evaluates offline; its full existing shell matrix was not
rerun. No production Rust/Android source changed or required rebuilding.
