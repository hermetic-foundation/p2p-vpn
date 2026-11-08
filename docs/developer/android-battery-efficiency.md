# Android Battery Efficiency

## Status

| Item | State |
| --- | --- |
| Target | At least 90% less app-attributable connected-idle energy on Wi-Fi and cellular |
| Baseline source | `8a302265`, plus measurement-only changes recorded with the capture |
| Device baseline | Not captured; ADB lists no devices at the initial November 8 preflight |
| Runtime optimizations | None yet; preserve baseline behavior until device measurements exist |
| Power endpoint | Debug-only `power-status`; unit checks are not physical battery evidence |
| Java verification | Normal Gradle unit, lint and instrumentation-assembly tasks now pass |
| Native baseline APK | ARM64 phone-test APK built, signed and statically verified; not deployed |
| Completion | Open; CPU, emulator and estimated power results cannot satisfy the target |

## Measurement Contract

### Workloads

| Setting | Frozen Requirement |
| --- | --- |
| Device | Same physical device, OS, app build type and power settings for each comparison |
| Build identity | Record commit, APK/native hashes and build configuration for both versions |
| Topology | Same peers, enabled networks, transport and relay role; record actual paths |
| Underlays | Separate Wi-Fi and cellular series; no USB network tunnel or LAN shortcut in cellular runs |
| Replicates | At least three matched baseline/candidate pairs per underlay; alternate order |
| Off control | Matched VPN-off windows bracket connected runs to estimate device background energy |
| Settling | At least five minutes after install/connect/radio change, plus a stable-path admission check |
| Idle window | At least 30 minutes; select a longer common duration before comparison if gauge resolution requires it |
| Screen | Off throughout idle windows; record actual screen/Doze and power-save states |
| Charging | Physically unplugged and discharging; `dumpsys battery unplug` does not disconnect USB power |
| Observer | Endpoint-only samples; no recurring ADB, UI polling or throughput probes during idle windows |
| Other activity | No builds, profiling, app updates or deliberate unrelated phone use during energy windows |
| Raw budget | At most 64 MiB per power run; no bugreport or unbounded logcat capture by default |
| Build budget | Reuse existing Gradle/Cargo caches; at most two build workers; no new emulator image |

Use a separate short profiling run for wakeups, CPU stacks and radio attribution.
Do not subtract the historical collector overhead as an exact correction.
[Existing controls](android-resource-controls.md) already show material observer cost.

### Energy And Uncertainty

1. Measure device energy from supported fuel-gauge energy counters or validated power hardware.
2. Record observation timestamps, charging, temperature, capacity and hardware counter resolution.
3. Subtract matched VPN-off power from connected power to estimate app-attributable power.
4. Compare candidate versus baseline app-attributable power, not total device drain.
5. Retain every replicate and report dispersion and the uncertainty of both subtractions.
6. Require positive, resolvable baseline excess and evidence supporting a ratio of at most `0.10`.

- Energy counters describe the whole device, not an Android UID or VPN network.
- Unsupported counters, negative deltas, charging and clock discontinuities invalidate a fuel-gauge window.
- Charge times endpoint voltage, instantaneous current and Android power-profile attribution are estimates.
- If counter resolution or off-control variability swamps the excess, report inconclusive rather than 10x.
- Missing energy support requires another validated measurement method, not fabricated zero readings.

Android documents [fuel-gauge units and device-dependent resolution](https://source.android.com/docs/core/power/device)
and [physical charging and radio measurement controls](https://source.android.com/docs/core/power/component).
The [BatteryManager API](https://developer.android.com/reference/android/os/BatteryManager)
specifies unavailable-property sentinels; these become JSON `null`, not zero.

## Endpoint Capture

`power-status` is additive to the existing schema-1 debug automation protocol.
It reads battery/power services synchronously and does not start or connect the VPN.
It also works for a VPN-off control; it creates no timer, listener or wakelock.

```sh
adb -s "$serial" shell am broadcast \
  --receiver-foreground \
  -n org.hermeticfoundation.p2pvpn.debug/org.hermeticfoundation.p2pvpn.DebugAutomationReceiver \
  -a org.hermeticfoundation.p2pvpn.debug.AUTOMATION \
  --es command power-status
```

Decode the existing Base64 result envelope and retain its `value` unchanged.
USB capture is a capability check, not an admissible discharging energy window.
ADB must be disconnected during cellular measurement; endpoint transport must be recorded.

| Field | Meaning |
| --- | --- |
| `schema_version`, `scope` | Power snapshot schema `1`; scope is `device_battery` |
| `started_elapsed_realtime_ms`, `finished_elapsed_realtime_ms` | Monotonic observation bounds including device suspend |
| `uptime_ms` | Awake uptime; excludes deep sleep |
| `app_cpu_time_ms` | Process CPU proxy only; invalid across process identity changes |
| `energy_remaining_nwh` | Hardware-reported remaining device energy in nanowatt-hours, or null |
| `charge_remaining_uah` | Remaining device charge in microampere-hours, or null |
| `current_now_ua`, `current_average_ua` | Signed device current; averaging period is vendor-dependent |
| `capacity_percent` | Coarse integer state of charge, not an energy metric |
| `battery_status`, `plugged_sources` | Raw Android charging status/source values; unavailable is null |
| `voltage_mv`, `temperature_tenths_c` | Battery voltage and temperature; unavailable is null |
| `screen_interactive`, `device_idle` | Screen-interactive and Android device-idle state, or null |

No identity, hostname, pairing code, address or private key is included.
The endpoint is absent from release sources and retains the debug receiver's `DUMP` permission.

### Bounded Capture Command

With the complete measurement-enabled debug APK installed, run:

```sh
nix develop .#android -c bash scripts/android-power-capture.sh \
  --serial "$serial" --output ./phone-power-capability.json
```

| Contract | Behavior |
| --- | --- |
| Selection | Requires one explicit ADB serial; never selects or connects another device |
| Queries | Device readiness, SDK/model/ABI/OS metadata and one `power-status` request |
| Mutations | No VPN start/stop, pairing, settings, permission grants or network changes |
| Bounds | Ten seconds per request by default; at most 64 KiB per response |
| Timeout override | `--rpc-timeout-seconds` accepts integers from 1 through 30 |
| Output | New mode-0600 JSON file; existing files/symlinks and publication races are rejected |
| Validation | Exact schema, numeric units/ranges, nullable properties and monotonic observation bounds |
| Privacy | Excludes serials and VPN identities; rejects unexpected response fields |
| Energy claims | Every capture sets `energy_window_eligible=false`; one endpoint is not a measurement window |

`P2P_VPN_ADB` selects an existing ADB executable, matching the other Android tools.
The wrapper retains Android `null` values; it does not turn unavailable energy
into zero or convert CPU/current/charge readings into measured energy.

`capabilities.energy_counter_reported=true` means only that Android returned a
numeric energy counter. Counter resolution, physical accuracy, unplugged operation
and whole-window stability still require separate device validation.

The helper can record USB capability checks and emulator parser checks, but neither
is physical battery acceptance. Model and OS build metadata identify the platform,
not a unique device; operators must still keep matched runs on the same phone.

Run the fake-ADB regression suite without touching any device:

```sh
bash tests/android-power-capture.sh
```

The Nix `android-e2e-structure` check also lints and runs this suite. It covers
successful, null-counter, zero-counter, charging and emulator records; invalid
schemas/metadata/clocks; oversized responses/timeouts; privacy and output races.

The initial suite passes 31 cases locally and inside the Nix structure check.
ShellCheck and shfmt pass. A malformed-value regression first reproduced jq
echoing a private value; parser diagnostics are now suppressed and replaced with
a bounded, value-free failure message.

The broader check also exposed a pre-existing fixture publication race. Fixture
JSON now becomes visible only after all relay/promotion fields are complete.
The original unmodified check also passed once, consistent with a race rather
than a deterministic failure; assertions and readiness deadlines are unchanged.

## Optimization Candidates

These are source-audit leads, not measured battery attribution or approved tradeoffs.

| Owner | Existing Work | Required Investigation |
| --- | --- | --- |
| Shared Android TUN reader | `poll` timeout every 250 ms | Cancellable blocking readiness without changing stop/error semantics |
| Shared Android TUN writer | Condition-variable timeout every 250 ms | Queue/close notifications without periodic empty-queue wakeups |
| Java service | Native health snapshot every two seconds | Event-driven state/failure delivery, retaining recovery and permission checks |
| Native supervisor | Health and peer-snapshot refresh every five seconds | Preserve failure detection while attributing repeated status work |
| Membership checkpoints | One-second maintenance tick per active runtime | Next-deadline scheduling with unchanged mutation/expiry bounds |
| Wi-Fi discovery | Multicast lock held while connected; separate pairing lock | Measure radio impact before changing LAN-first discovery behavior |
| Public networking | Discovery, dialing, relay and connection maintenance | Actual radio bursts, failed retries and settled-peer behavior |
| Multi-network supervisor | Per-network runtime/cache ownership | Duplicate idle work without merging security or routing authority |

## Feature Gates

| Area | Required Regression Evidence |
| --- | --- |
| Enrollment | Create/join/approval/rejection/cancellation and persistent identity |
| Membership | Indirect propagation, hostname updates, peer lists, revocation and DNS |
| Isolation | Single/multiple networks; disabling/failing one preserves healthy siblings |
| Paths | Direct QUIC, stream/TCP fallback and relay-backed reachability |
| Mobility | Automatic Wi-Fi/cellular recovery; LAN-first discovery and LAN return |
| Background | Screen-off/Doze reachability, no manual rescue, foreground notification behavior |
| Lifecycle | Always-on/lockdown, process restart/reboot and resource cleanup |
| Traffic | Bidirectional IPv4/IPv6, equal delivered active work, latency/loss/throughput |
| Compatibility | Existing minimal configuration, Android/Linux peers and unchanged authority |

Freeze workload-specific performance bounds before collecting candidate results.
Do not lengthen recovery deadlines after observing failures or trade functionality for idle savings.

## Next Gates

1. Verify endpoint availability and counter resolution on an explicitly selected physical device.
2. Freeze device/topology metadata and quantitative traffic/recovery bounds before baseline capture.
3. Capture matched Wi-Fi/cellular baseline and VPN-off controls before runtime changes.
4. Rank actual CPU/wakeup/radio costs, then implement and regression-test each optimization.
5. Repeat physical measurements and all feature gates; publish raw hashes and structured summaries.

## Initial Validation, November 8

| Check | Result / Limit |
| --- | --- |
| SDK compile | Main, debug, unit and instrumentation Java sources compile against pinned API 37 with JDK 17 |
| Host unit suite | 156 tests pass via direct JUnit with the existing mockable Android jar; not Gradle or device execution |
| Power tests | Nine pass: units, nulls, signed values, clocks, field allowlist and no-service capture |
| Gradle attempt | Offline unit/lint/instrumentation build stops at missing pinned AGP `9.3.2` artifacts |
| Nix shell attempt | Cancelled when absent prerequisites triggered unrelated source builds |
| Native sources | Unchanged; Rust rebuilds and protocol tests not repeated for this debug-only endpoint |
| Device tests | Not run; no attached ADB device, no battery improvement claimed |
| Temporary storage | Approximately 1 MiB; existing app build directory remains 311 MiB |

The validation fallback uses the existing pinned JDK/SDK, generated resource jar,
JUnit `4.13.2`, and JSON `20250517` verified against `nix/android-gradle-deps.json`.
It avoids SDK downloads, native cross-compilation and an emulator image rebuild.

Those initial host checks did not replace Gradle, lint or package verification.
The subsequent section records the restored Java gates; native APK packaging is still open.

With the pinned Gradle artifacts available, the normal verification command is:

```sh
nix develop .#android -c gradle -p android \
  --offline --no-daemon --max-workers=2 -Dorg.gradle.parallel=false \
  :app:testDebugUnitTest :app:lintDebug :app:assembleDebugAndroidTest
```

## Restored Gradle Verification, November 8

[Portable verification evidence](android-battery-verification-2026-11-08.json)
records source, toolchain, dependency hashes and results. This closes the earlier
Java build-verification gap, not the battery or device acceptance gates.

| Check | Result |
| --- | --- |
| Source | `948e57bc`, including the measurement-only endpoint |
| Dependencies | 327 Maven artifacts verified against the unchanged Nix Gradle lock |
| Downloads | Sequential, capped at 1 MiB/s; 283 fetched and 44 reused |
| Artifact storage | 240,865,130 bytes, below the frozen 256 MiB cap |
| Toolchain | Existing Nix JDK 17, API 37 SDK and Gradle 9.5.1 wrapper |
| Build isolation | Offline after restore; local Maven mirror; two workers and two JVM processors |
| Gradle result | Successful in 18 seconds; 24 tasks executed, one from cache |
| Unit XML | 35 suites, 156 tests; zero failures, errors or skipped tests |
| Lint XML | Zero errors; seven warnings in unchanged resources |
| Instrumentation APK | Assembly passes; no instrumentation test ran on a device |
| Main VPN APK / JNI | Not built or deployed; fresh native staging remains required |
| Device / energy | No device attached; no energy measurement or efficiency claim |

The warnings concern launcher shape, pluralization and an input label, not the
power endpoint. Existing compiled native merge outputs were not valid source
inputs; do not deploy a Java-only APK as a VPN baseline.

### Local Replay

The temporary mirror and its init script are under
`/tmp/p2p-vpn-battery-build-Qm1dQ1k2`. They are build inputs, not app configuration.
The normal Nix build remains the supported APK packaging workflow.

```sh
JAVA_TOOL_OPTIONS=-XX:ActiveProcessorCount=2 \
  ./android/gradlew -p android --offline --no-daemon --max-workers=2 \
  -Dorg.gradle.parallel=false '-Dorg.gradle.jvmargs=-Xmx1g -Dfile.encoding=UTF-8' \
  -Pandroid.aapt2FromMavenOverride="$ANDROID_HOME/build-tools/37.0.0/aapt2" \
  --init-script /tmp/p2p-vpn-battery-build-Qm1dQ1k2/local-repository.gradle \
  :app:testDebugUnitTest :app:lintDebug :app:assembleDebugAndroidTest
```

Use the pinned JDK/SDK environment from `nix/android.nix`. The replay does not
rebuild the SDK, download an emulator image or compile native Rust libraries.
Deleting the mirror requires restoring the locked artifacts before this replay.

## ARM64 Baseline APK, November 8

[Build evidence](android-battery-baseline-2026-11-08.json) records the APK, JNI,
source and verification facts. This is a phone-test artifact, not a new dual-ABI
release and not physical battery or feature-preservation evidence.

| Item | Result |
| --- | --- |
| Runtime source | Unchanged from `8a302265`; no battery optimization in this baseline |
| Measurement hook | Android Java source from `948e57bc` |
| Build tree | `d198b2cb`; subsequent changes only affect tooling and documentation |
| Native package | Pinned Nix ARM64 derivation passes; release compilation takes 359 seconds |
| Native limits | One Nix job, two Cargo jobs/cores, existing sub-10-Mbps download cap, 2,400-second watchdog |
| Native warnings | Five nonfatal warnings; target compilation is not device execution |
| Java packaging | Offline Gradle assembly passes in 16 seconds; unit/lint results reused as up-to-date |
| Unit / lint reports | 156 tests without failures/skips; zero lint errors and seven resource warnings |
| Architecture | `arm64-v8a`, ELF64 AArch64, minimum Android API 26, target API 37 |
| Signature | Public repository development key; APK v2 signature verifies |
| Alignment | All four native LOAD segments align to 16 KiB; APK zip alignment check passes |
| JNI provenance | Packaged bytes match Gradle's stripped output; native `.text` dumps match before/after stripping |
| Artifact | 19,125,268 bytes; private mode-0600 retained copy |
| Deployment / measurements | None; ADB still reports no devices |

### Local Artifact

```text
/home/midischwarz12/.cache/p2p-vpn/battery-baselines/873b7b5a06f1db65ee18486a32f593e39906d26aa5a4512e07496f69963e9ee0/p2p-vpn-baseline-arm64.apk
```

SHA-256:

```text
873b7b5a06f1db65ee18486a32f593e39906d26aa5a4512e07496f69963e9ee0
```

The normal Nix APK output still supports both configured ABIs. This test build
uses only the current-source ARM64 library to avoid an unnecessary emulator build.
No host service, personal flake, VPN profile, key or network configuration changed.

The private cache retains one content-addressed baseline APK, not a directory
of dated copies. Temporary measurement artifacts remain subject to the capture budget.

### Next Required Input

1. Attach the intended physical Android phone with authorized USB debugging.
2. Install the retained APK in place without clearing profiles or pairing state.
3. Run the bounded capability capture and verify the actual energy counter behavior.
4. Select and validate an unplugged measurement method for Wi-Fi and cellular.
5. Capture the frozen baseline before making runtime optimizations.

The goal is not complete. Missing physical-device access prevents establishing
the baseline and ranking actual energy costs; emulator/CPU results cannot replace it.
