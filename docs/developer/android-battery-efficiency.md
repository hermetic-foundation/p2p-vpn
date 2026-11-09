# Android Battery Efficiency

## Status

| Item | State |
| --- | --- |
| Target | At least 90% less app-attributable connected-idle energy on Wi-Fi and cellular |
| Baseline source | Working runtime `eec88da9`; corrected ARM64 APK retained before CPU changes |
| Device baseline | OnePlus 9 Pro capability capture completed; no supported energy counter reported |
| Runtime optimizations | Sort keys cached, unconsumed metric parsing removed and duplicate pairing writes skipped; no measured battery gain |
| Power endpoint | Debug-only `power-status`; unit checks are not physical battery evidence |
| Java verification | Normal Gradle unit, lint and instrumentation-assembly tasks now pass |
| Native APKs | Working ARM64 baseline retained; current candidate installed; earlier bidirectional IPv4/IPv6 and DNS smoke checks pass |
| Per-app feedback | Android `BatteryUsageStats` estimates can compare APKs; unsupported raw energy counters do not prevent this |
| First per-app comparison | One matched Wi-Fi pair completed; no detectable improvement; 10x target remains open |

## Per-App Feedback First

Use Android's built-in per-app consumption report before making further battery
optimizations. Compare the retained original APK with the candidate on the same
phone; do not substitute reduced CPU, allocations or file writes for this result.

| Quantity | Calculation / Requirement |
| --- | --- |
| Metric | Android-attributed app consumption in mAh per hour |
| Window consumption | End UID mAh minus start UID mAh; not the cumulative historical total |
| Rate | Window mAh divided by elapsed hours |
| Comparison | Candidate rate divided by original rate; `0.10` corresponds to 90% lower reported consumption |
| Scope | Same UID and unchanged statistics-reset epoch; reject resets and negative deltas |
| Conditions | Same phone, enabled networks, peer count, underlay and screen state |
| Components | Record which components Android actually attributes; missing CPU energy is not zero CPU cost |
| Resolution | Retain endpoint precision; do not claim a small difference below report rounding or run variability |
| Replication | A short first pair is exploratory; repeat and alternate build order before claiming sustained savings |

Read the current report without clearing battery history:

```sh
package=org.hermeticfoundation.p2pvpn.debug
adb -s "$serial" shell dumpsys batterystats --sync
adb -s "$serial" shell dumpsys batterystats --usage --model power-profile "$package"
adb -s "$serial" shell dumpsys batterystats -c "$package"
```

Select the package's UID explicitly. On this phone, `--usage` still prints other
UIDs despite the package argument; retain only the target block and global reset
metadata. Parse checkin data as CSV rather than splitting strings manually.

The first comparison uses five-minute settling and ten-minute idle windows for
each APK. Check Android's cumulative screen-off time at both endpoints, not just
one screen-state sample. Reject a window interrupted by a screen wake-up.

### USB Accounting

| Setting | Behavior |
| --- | --- |
| Physical connection | USB remains attached and supplies power |
| Accounting | Temporary `dumpsys battery unplug` enables battery-only accounting; it does not disconnect power |
| History | Temporarily enable `batterystats enable no-auto-reset` to preserve the existing reset scope |
| Restoration | Always run `dumpsys battery reset` and restore the prior no-auto-reset setting, including on interruption |
| Interpretation | Report Android's attributed estimates, not physical battery discharge or total-phone battery life |

Do not reset battery history, uninstall the app, erase its profile or change
membership for a comparison. Preserve the APK hashes, endpoint reports, elapsed
times, active topology and rejected windows.

Android documents [per-UID battery reporting](https://developer.android.com/tools/dumpsys)
and [activity-based power estimation](https://source.android.com/docs/core/power).
Missing raw counters or cellular service does not block Wi-Fi attribution tests.
Full-device discharge validation below is a separate measurement level.

### First Comparison, November 8

[Structured results](android-per-app-battery-2026-11-08.json) retain the APK
identities, endpoint values, times, capture hashes and rounding bounds.

| Android Report | Original APK | Current APK |
| --- | --- | --- |
| Rounded cumulative UID mAh | `12.8` to `13.1` | `13.3` to `13.7` |
| UID consumption rate | About `1.8 mAh/hour` | About `2.4 mAh/hour` |
| Finer screen-off Wi-Fi rate | About `1.88 mAh/hour` | About `1.88 mAh/hour` |
| Window duration | `600.87 seconds` | `600.86 seconds` |
| Peers / owned QUIC sessions | `5 / 5` at both endpoints | `5 / 5` at both endpoints |
| Accounted screen-on time | `0 ms` | `0 ms` |

The rounded UID totals suggest 33% higher candidate consumption, but their
`0.1 mAh` endpoint precision is too coarse to establish that regression. The
finer Wi-Fi breakdown shows no detectable change. Neither shows a 10x gain.

- One interrupted screen-on attempt was excluded; the restarted pair stayed screen-off.
- CPU time was recorded, but Android reported no CPU energy; missing active/frequency tables limit attribution.
- Normal battery reporting, disconnected VPN and screen-off state were restored; the encrypted profile was unchanged.
- No performance code changed; app data was preserved and no new members were paired.

Next, retain higher-precision Android UID consumption and repeat with alternating
build order. Do not treat the short pair, display-rounding bounds or Wi-Fi-only
breakdown as a precise whole-app battery-life improvement.

## Full-Device Discharge Validation

These controls apply to separate physical-discharge cross-checks, not as
prerequisites for the per-app Android feedback loop above.

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

## Physical Preflight, November 8

[Preflight evidence](android-battery-device-preflight-2026-11-08.json) records
capabilities and the rejected startup workload, not an energy baseline.

| Check | Observation |
| --- | --- |
| Phone | OnePlus 9 Pro, model `LE2125`, Android 16 / API 36, ARM64 |
| Installed APK | SHA-256 `873b7b5a06f1db65ee18486a32f593e39906d26aa5a4512e07496f69963e9ee0` |
| Data preservation | In-place update; encrypted profile hash unchanged; no reset or pairing |
| Existing topology | One enabled `personal-devices` network on validated Wi-Fi |
| Device energy | `energy_remaining_nwh=null`; charge counter exists, average current is unavailable |
| Other power access | `dumpsys powerstats` returns no data; shell cannot read battery sysfs |
| Cellular | No SIM reported; no cellular series possible in the current device setup |
| Measurement limits | USB-powered and charging; no valid discharge window or battery-savings claim |
| Startup gate | Membership loading fails with `lock() not supported`; supervisor retries |

The pinned Android standard library rejects `File::lock()` before networking starts.
See the [upstream Rust issue](https://github.com/rust-lang/rust/issues/148325).
A portable `rustix::fs::flock` correction retains advisory directory ownership,
interrupted-call retries and release on descriptor close.

This is a compatibility prerequisite, not an idle-power optimization.
Freeze a working, compatibility-corrected APK before collecting baseline windows.
Do not compare a connected candidate against this failed-startup workload.

The phone supports functional and CPU/wakeup/radio debugging and Android's per-app
battery-attribution comparisons. Unsupported raw energy counters do not prevent
that feedback loop. Keep attributed estimates separate from full-device discharge.

### Compatibility Verification

[Verification evidence](android-state-lock-verification-2026-11-08.json) identifies
the corrected APK, native library, source hashes, test results and diagnostic limits.

| Check | Result |
| --- | --- |
| Storage locking | Eight tests pass, including cross-process exclusion, crash release and Linux standard-library interoperability |
| Core Rust | 1,456 pass; eight ignored; format and repository Clippy safety/performance gates pass |
| Android native | Pinned Nix ARM64 build passes; signature, 16 KiB alignment and packaged native code verified |
| Deployment | In-place update; installed hash verified; original encrypted profile unchanged |
| LAN smoke | Five peers discovered; 20/20 IPv4/IPv6 probes across both directions; phone hostname resolves |
| Payload path | Owned QUIC packet counters increase; available TCP/UDP paths retained |
| Energy acceptance | Still open; no supported energy counter or cellular service on this phone |

The retained working APK is `b1359996a8fbab7ea8eec2fd4c0649dfe0ea6290bf9466145af8242bf58453a3`.
It is a phone-test artifact, not a newly verified dual-ABI release.
No identities, config defaults or authority rules changed.

The first full workspace run exposed random address collisions in an unrelated
delegated-inviter test fixture. Its separate test-only correction selects distinct
built-in addresses without relaxing production collision checks or trust assertions.

| Follow-Up | Result |
| --- | --- |
| Complete workspace command | 1,821 pass, zero failures, 48 ignored; physical/privileged ignored cases remain open |
| Static gates | Workspace Clippy correctness/suspicious/performance groups, format and Nix test-source check pass; style warnings remain |
| Second phone connection | Five peers rediscovered; 5/5 IPv4 replies with owned-QUIC payload traffic |
| Device restoration | Original disconnected state restored; encrypted profile unchanged; foreground service and TUN/Tokio workers retired |
| Retained global helpers | One `async-io` and one `futures-timer` thread remain; the separate VPN-off CPU control is recorded below |

### Short CPU Diagnostic

| Observation | Result / Limit |
| --- | --- |
| Screen-off interval | 107.843 seconds; process identity unchanged; USB charging throughout |
| App process CPU | 7.965 CPU-seconds, about 7.39% of one core; not a steady-state energy baseline |
| Public work | Cached counters show 73 dispatched DHT dial intents and 74 query requests |
| Thread follow-up | 30 matched threads; 504/718 CPU ticks in Tokio workers, 175 in a runtime-named thread |
| TUN workers | Reader/writer together use 5/718 CPU ticks; this does not quantify wakeup or radio energy |
| VPN-off CPU control | 44 CPU-ms over 145.422 seconds, same process and charging/screen state; not an optimization result |

These samples direct further profiling toward networking and service-worker work.
They do not establish which operations caused energy drain, justify reduced
reachability, or demonstrate any battery improvement.

### Perfetto Energy Fallback

The physical phone also completed a bounded `android.power` capability trace.
The [preflight evidence](android-battery-device-preflight-2026-11-08.json)
records its configuration, schemas, hashes and decoder checks.

| Check | Result |
| --- | --- |
| Capture | Perfetto `51.2`; ten seconds; 1 MiB discard buffer; 30-second ADB watchdog |
| Footprint | 1,422-byte trace; 68 KiB additional scratch; no SDK, emulator or native rebuild |
| Battery source | Eleven charge/capacity/current/voltage samples; no average-current or energy-counter samples |
| Hardware rails | One empty rails packet; zero rail descriptors and zero energy samples |
| Decoder checks | Upstream v51.2 Protobuf messages; six synthetic controls; C++ text decode agrees |
| Restoration | Trace session ended, remote copy removed, VPN remained off, encrypted profile unchanged |
| Admission | Not an energy window; charging; no usable measured energy source returned |

Reproduce the source selection using [Perfetto's power data source](https://perfetto.dev/docs/data-sources/battery-counters):

1. Select the phone explicitly and keep `android.power` as the only data source.
2. Use the ten-second configuration recorded in `perfetto_energy_probe`; poll battery counters every second.
3. Enable hardware rails, but disable energy-estimation breakdowns and entity-state residency.
4. Check the remote file size before pulling; reject captures over 2 MiB and retain a SHA-256-verified copy.
5. Decode with the pinned message definitions, retaining absent fields rather than fabricating zero values.
6. Confirm the session ended and remove only the probe's remote file.

The [battery message](https://github.com/google/perfetto/blob/v51.2/protos/perfetto/trace/power/battery_counters.proto)
and [rail message](https://github.com/google/perfetto/blob/v51.2/protos/perfetto/trace/power/power_rails.proto)
are projected through TracePacket fields `38` and `40`, respectively.
The local reader rejects compressed traces and empty/truncated inputs.

This rules out the tested Perfetto source as an energy-measurement path on this
setup, not the phone's hardware in every possible firmware or laboratory setup.
Charge, current and voltage remain proxies; do not relabel their product as measured energy.

Retain the original APK for matched per-app battery comparisons. The tested
Perfetto source is not required for Android's own attribution reports. Additional
CPU work must follow a battery comparison rather than replace it.

### Settled Wi-Fi CPU Profiling

[Structured evidence](android-wifi-cpu-profile-2026-11-08.json) separates quiet CPU
windows from intrusive stack traces. These are diagnostic runs, not energy windows.
The local date is November 8; the recorded UTC endpoints fall on November 9.

| Observation | Result / Limit |
| --- | --- |
| Topology | One network, five connected overlay peers and five owned-QUIC sessions at every endpoint |
| Settling | At least 350 seconds before three endpoint-only, screen-off 120-second windows |
| Process CPU | 7.22%, 7.03% and 6.07% of one core; mean 6.78%; not energy |
| Conditions | Same process/runtime generation; USB powered; full battery; not Doze |
| Public networking | First cached interval adds 228 DHT query requests and 200 dispatched dial intents |
| Native sampling | 151 app stack samples over about 30 seconds; 86 in Tokio workers, 50 in the Java service worker |
| Native attribution | Relay snapshot sorting and Base58 encoding share 20 inclusive samples; pairing persistence also appears |
| Java sampling | Status/peer parsing and metric-parsing exceptions observed; imported durations are wall time, not CPU |
| Sampling limits | Eleven incomplete native unwinds; short statistical sample; no precise energy attribution |

#### Narrow First Change

Relay snapshots previously encoded each peer ID on repeated sort comparisons.
`sort_by_cached_key` computes each text key once per snapshot, preserving the same
text order. Temporary keys are bounded by the existing 64-infrastructure-peer cap.

- No longer-lived cache or stale address/connection state is introduced.
- No discovery interval, LAN preference, recovery, persistence or authority behavior changes.
- Regression coverage compares legacy order at capacity, changed addresses, removal and reverse insertion order.
- Pairing-state writes and Java parsing are addressed in separate follow-ups below.

#### Comparison Result

| Window | Baseline One-Core CPU | Candidate One-Core CPU | Baseline / Candidate DHT Queries |
| --- | --- | --- | --- |
| 1 | 7.22% | 8.81% | 228 / 380 |
| 2 | 7.03% | 6.81% | 42 / 195 |
| 3 | 6.07% | 6.41% | 74 / 39 |
| Mean CPU / Total Queries | 6.78% | 7.35% | 344 / 614 |

All six endpoints per build retain five connected peers and five QUIC sessions.
The candidate does **not** demonstrate a whole-app CPU gain. Live public-network
work and temperatures differ; builds were not alternated. Retain every window.

The host-only component probe uses six alternating-order batches of 32 sorts.
For the same 64-entry synthetic input, cached sorting reduces key encodings from
776 to 64 per sort with identical output. This is not Android battery evidence.

| Validation | Result / Limit |
| --- | --- |
| Workspace | 1,822 pass; zero failures; 48 ignored; format, repository Clippy gates and Nix test-source check pass |
| Packaging | Pinned ARM64 native build and offline Gradle assembly pass; unchanged Java unit/lint tasks stay up to date |
| Deployment | In-place update; installed APK/native hashes, signature and 16 KiB alignment verified; profile unchanged |
| Reachability | 20/20 bidirectional IPv4/IPv6 smoke probes, dual-stack DNS and ten further post-idle replies |
| Payload | Owned-QUIC outbound and accepted inbound counters reach 30; no data-plane behavior changed |
| Restoration | Original disconnected state restored; native TUN/Tokio workers and owned remote traces retired |
| Acceptance | No measured energy gain or full feature/non-regression claim; cellular and sustained energy gates remain open |

Remaining work includes unconsumed status snapshots and public-network costs.
Preserve crash durability, malformed-input handling and discovery/recovery behavior.

After disconnection, the unchanged debug receiver cannot cold-start an idle
background service on this Android build (`BackgroundServiceStartNotAllowedException`).
Cleanup uses the disconnect acknowledgement and native-worker retirement; improve this harness path separately.

#### Reproduction

1. Retain the original APK and record source/native/APK hashes before installing a candidate.
2. Connect on Wi-Fi; wait at least five minutes and confirm the same enabled-network and peer/session counts.
3. Use `android-power-capture.sh` only at each window boundary; retain process-start identity and cached runtime status.
4. Record at least three 120-second quiet CPU windows with the screen off; do not run profilers inside them.
5. Profile separately with the bounded configuration below; retain trace hashes and check loss/unwind errors.
6. Analyze locally; verify APK/native symbol provenance, then remove only the owned remote trace files.

```protobuf
buffers { size_kb: 4096 fill_policy: DISCARD }
duration_ms: 30000
max_file_size_bytes: 8388608
data_sources {
  config {
    name: "linux.perf"
    perf_event_config {
      timebase { counter: SW_CPU_CLOCK frequency: 99 timestamp_clock: PERF_CLOCK_MONOTONIC }
      callstack_sampling {
        scope { target_cmdline: "org.hermeticfoundation.p2pvpn.debug" }
        kernel_frames: false
      }
    }
  }
}
```

Use an explicitly selected phone and a 50-second capture watchdog.
Keep `security.perf_harden` unchanged; this debuggable app supports app-scoped
sampling without root. Filter samples to the app, not the machine-wide total.

For Java attribution, use `am profile start --sampling 10000 --clock-type dual`
and stop it with a host watchdog. Record the actual trace-header duration.
Do not interpret parked-thread wall time or nested sampled durations as CPU.

### Metric Parsing Follow-Up

The [parsing evidence](android-metric-parsing-2026-11-08.json) records a separate,
Java-only change. Both parsers now recognize consumed metric names before numeric
conversion, avoiding exceptions for ordinary text metadata and unknown counters.

| Check | Result / Limit |
| --- | --- |
| Unknown metadata probe | 100 synthetic dual-parser iterations: 800 baseline format exceptions, zero candidate exceptions |
| Malformed control | Known invalid values remain rejected; 200 candidate exceptions instead of parsing each unknown name too |
| Compatibility | Negative/overflow/whitespace handling, saturating sums, multi-network aggregation and queue-age maximum preserved |
| Android verification | 158 unit tests pass, including two new boundary regressions; lint has zero errors and seven existing warnings |
| Packaging | Offline APK assembly passes with the unchanged ARM64 native library; no new SDK, emulator or native rebuild |
| Phone smoke | In-place update; unchanged profile; five peers/QUIC sessions; 20/20 bidirectional IPv4/IPv6 probes and DNS pass |
| Cleanup | Original disconnected/screen-off state restored; native workers retired; approximately 536 KiB of extra host capture data |
| Acceptance | Host exception counts are not measured phone CPU, radio energy or battery savings; the full target remains open |

The bounded host probe uses JDK 17's `jdk.JavaExceptionThrow`, filtered to parser
stack frames. Each case has a 20-second watchdog and a 1 MiB trace cap.
No profiler runs on the phone during the functional smoke checks.

### Pairing-State Write Follow-Up

[Write evidence](android-pairing-writes-2026-11-08.json) records a shared-runtime
optimization. The one-second pairing tick remains unchanged, but saving an already
committed, unchanged state no longer encrypts, replaces and syncs the file again.

| Requirement | Implementation |
| --- | --- |
| Durability | Only a successful file sync and confirmed parent sync seed a reusable hint |
| Visibility | Loading state never seeds that hint; failed saves and removal invalidate it |
| Selection | File and protected-directory identity, permissions and ownership must still match |
| Content | Bounded, non-following/nonblocking reads recheck the stored ciphertext digest |
| Concurrency | Per-store mutex and existing directory lock cover comparison or commit |
| Confidentiality | Hint holds two digests and metadata, not plaintext; it is omitted from diagnostics |
| Compatibility | Existing formats, keys, nonce generation, expiry, cancellation and protocol behavior remain |
| Unsupported sync | Existing success behavior remains, but unsupported parent sync never enables deduplication |
| Legacy storage | Unprotected directories retain unconditional writes |

Real state changes still follow file sync, atomic rename and parent sync before
acknowledgement. The optimization does not suppress pairing maintenance, discovery,
route recovery or data traffic, and it does not remove the one-second wakeup.

| Verification | Result / Limit |
| --- | --- |
| Store regressions | 28 pass, including 16 new cases for durability, corruption, replacement, ownership and concurrency |
| Full Rust workspace | 1,838 pass, zero failures, 48 ignored; format, repository Clippy gates and Nix test-source checks pass |
| Clippy scope | Pedantic warnings remain, including intentional omission of the private hint from `Debug` |
| Phone baseline | Six idle samples contain six different ciphertexts/inode versions |
| Phone candidate | All six idle samples retain the same file while five peers and QUIC sessions remain connected |
| Mutation controls | Opening an invitation, canceling via disconnect and reconnecting each produce genuine persisted changes |
| Restart idle | Six more samples remain unchanged after reconnect; canceled pairing does not reappear in the app |
| Traffic | 20/20 bidirectional IPv4/IPv6 probes and DNS pass; five more replies pass after cancellation/reconnect |
| Deployment | ARM64 Nix build and offline APK assembly pass; signature/alignment verified; encrypted profile unchanged |
| Cleanup | Owned invitation canceled; original disconnected/screen-off state restored; approximately 560 KiB of captures |

These are file-I/O and functional checks, not energy or long-duration acceptance.
The phone still reports no energy counter while USB powered. No battery-life
percentage, extra operating hours or 10x improvement is established by these results.

## Optimization Candidates

These are investigation leads, not measured battery attribution or approved tradeoffs.

| Owner | Existing Work | Required Investigation |
| --- | --- | --- |
| Shared Android TUN reader | `poll` timeout every 250 ms | Cancellable blocking readiness without changing stop/error semantics |
| Shared Android TUN writer | Condition-variable timeout every 250 ms | Queue/close notifications without periodic empty-queue wakeups |
| Java service | Native health snapshot every two seconds | Event-driven state/failure delivery, retaining recovery and permission checks |
| Native supervisor | Health and peer-snapshot refresh every five seconds | Preserve failure detection while attributing repeated status work |
| Membership checkpoints | One-second maintenance tick per active runtime | Next-deadline scheduling with unchanged mutation/expiry bounds |
| Code-pairing store | Duplicate committed writes now skipped; one-second tick retained | Attribute remaining timer/serialization costs without weakening durability |
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
3. Capture matched Wi-Fi/cellular energy baseline and VPN-off controls using the retained original APK.
4. Continue available Wi-Fi CPU/wakeup/radio profiling; implement and regression-test narrowly evidenced changes.
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
