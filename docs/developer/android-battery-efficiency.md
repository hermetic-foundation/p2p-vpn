# Android Battery Efficiency

## Status

| Item | State |
| --- | --- |
| Target | At least 90% less app-attributable connected-idle energy on Wi-Fi and cellular |
| Baseline source | `8a302265`, plus measurement-only changes recorded with the capture |
| Device baseline | Not captured; ADB lists no devices at the initial November 8 preflight |
| Runtime optimizations | None yet; preserve baseline behavior until device measurements exist |
| Power endpoint | Debug-only `power-status`; unit checks are not physical battery evidence |
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

## Optimization Candidates

These are source-audit leads, not measured battery attribution or approved tradeoffs.

| Owner | Existing Work | Required Investigation |
| --- | --- | --- |
| Shared Android TUN reader | `poll` timeout every 250 ms | Cancellable blocking readiness without changing stop/error semantics |
| Java service | Native health snapshot every two seconds | Event-driven state/failure delivery, retaining recovery and permission checks |
| Membership checkpoints | One-second maintenance tick per active runtime | Next-deadline scheduling with unchanged mutation/expiry bounds |
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
The Gradle/lint/package gates remain outstanding, not replaced by host compilation.

Restore the pinned Gradle artifacts, then run the normal verification without
changing dependency versions:

```sh
nix develop .#android -c gradle -p android \
  --offline --no-daemon --max-workers=2 -Dorg.gradle.parallel=false \
  :app:testDebugUnitTest :app:lintDebug :app:assembleDebugAndroidTest
```
