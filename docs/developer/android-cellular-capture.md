# Cellular Window Capture

## Design Contract

| Boundary | Behavior |
| --- | --- |
| Purpose | Collect the raw bundles consumed by [cellular comparison](android-cellular-comparison.md) |
| Target | Explicit USB device serial; never automatic device selection or Wi-Fi ADB |
| Network | Already connected, screen off, validated cellular, Wi-Fi disabled |
| APK | Already installed and running; hash must match the planned arm |
| Peers | Freeze the authorized inventory and counterpart builds in a private workload record |
| Changes | Temporary simulated-on-battery accounting only; no install, pairing, route or radio changes |
| Incoming access | Source-bound overlay wake/steady checks before settling and after measurement |
| Timed interval | No ADB/UI/status/traffic polling and no builds |
| Trace | Starts before the first binary report, ends after the last, with padding |
| Trace storage | Stream every five seconds through an 8 MiB ring buffer; stop at a 32 MiB file limit |
| Failure | Preserve the attempt, raw files, error and restoration outcome; never overwrite or silently retry |

This collector measures Android attribution while USB supplies physical power.
It does not claim physical battery discharge. APK staging and final restoration
belong to the enclosing experiment, not the per-window capture command.

## Commands

The collector targets the debug package in Android user `0`. Build tools before
timed captures. The harness is tested with a fake device; live Pixel verification
remains required before relying on its output.

```sh
tool=$(nix build .#android-cellular-capture --no-link --print-out-paths)
capture="$tool/bin/p2p-vpn-android-cellular-capture"
"$capture" init "$new_series_directory" \
  --reference-apk "$reference_apk" --candidate-apk "$candidate_apk" \
  --workload "$private_workload_file"
"$capture" capture "$new_series_directory" --slot 01-A \
  --serial "$usb_serial" --simulate-on-battery
```

Stage the planned APK and establish the network before each `capture` command.
Continue with slots `02-B`, `03-B`, `04-A`, `05-A`, `06-B`, switching builds
outside captures. Existing, failed and out-of-order slots are never replaced.

| Result | Meaning |
| --- | --- |
| `captured` | Raw reports, trace and metadata were retained; accounting/context checks passed |
| `boundary_probes_passed` | Source-bound IPv4 first-packet and 3/3 steady checks passed in both directions; not independent cold-direction proof |
| `battery_reporting_restored` | Battery reporting was verified back in hardware mode |
| `restoration_verified` | Reporting and unchanged app/profile checks passed without cleanup failure |
| Exit `0` | Capture, supplied probes and restoration passed; not a savings claim |

Use `android-cellular-compare` afterward to validate trace coverage and compare
all six windows. A fake-device trace is not battery evidence. Do not change a
collector build midway through a series; its source hash is frozen in the plan.
This boundary-probe change requires a new plan/series for both arms. A changed
collector rejects old plans before device access; do not edit their hashes or
replace retained attempts to continue them with new probe semantics.

## Frozen Workload

Keep this record private. Hash it into every window; it is data, not a source
of shell commands. Use the actual controlled network and public peer IDs.

```json
{
  "schema_version": 1,
  "network_name": "battery-trial",
  "phone_peer_id": "<phone peer ID>",
  "phone_address": "10.42.0.2",
  "peer_address": "10.42.0.1",
  "authorized_peer_ids": ["<Linux peer ID>"],
  "counterpart_builds": {"linux": "<SHA-256 of running Linux executable>"},
  "diagnostic_overrides": false
}
```

- Use the same enabled network and authorized peers in both arms.
- Keep counterpart executables and settings unchanged throughout the series.
- Do not pause probes/DHT work or disconnect peers to manufacture a low rate.
- Record actual paths and membership evidence separately; a declared inventory is not proof of convergence.

## Capture Order

1. Create a new private series directory and freeze APK hashes, workload and `AB BA AB` order.
2. Stage the correct APK outside capture; preserve the original installed APK and profile beforehand.
3. Establish cellular connectivity, enable the existing network, then turn the screen off.
4. Capture the next planned slot; do not replace failed or incomplete attempts.
5. Analyze all slots together, preserving failures and functional outcomes.
6. Restore and verify the original APK/configuration after the complete experiment.

All APK changes occur outside energy windows. A new process between windows is
expected; a restart within a window invalidates it. A failed quiet transition
is an outcome to investigate, not a reason to hide that candidate's consumption.

## Endpoint Evidence

| Evidence | Handling |
| --- | --- |
| APK, OS, profile and membership-state hashes | Capture before/after; unexpected changes invalidate the attempt |
| Binary battery reports | Retain before decoding, including malformed/unavailable reports |
| Failed battery commands | Retain partial stdout and private stderr, even on timeout |
| Sampling clocks | Boot UUID and `/proc/uptime` brackets around each report |
| Native status | Cached numeric counters at boundaries; distinguish payload from maintenance |
| App status | Enabled network, identity, runtime generation and underlay counters |
| Radio | Validated default cellular path and extracted signal readings; never retain full telephony registry |
| Temperature | Live thermal HAL reading; frozen BatteryService temperature is not current |
| Probe results | Separate first-packet/steady status, stdout, monotonic timing and command failures in both directions |
| Lifecycle state | Verify separately for lifecycle acceptance; zero sessions alone is not a quiet/reachability proof |

Addresses are parsed as CIDRs, matching the Android producer. Signal extraction
binds the selected cellular subscription to its registry phone block. It currently
requires the active data subscription to equal the registry's default subscription;
unknown mappings fail before accounting changes, not by borrowing another SIM's signal.

The mapping follows AOSP's
[registry dump](https://raw.githubusercontent.com/aosp-mirror/platform_frameworks_base/android16-release/services/core/java/com/android/server/TelephonyRegistry.java)
and [network capability subscription IDs](https://android.googlesource.com/platform/packages/modules/Connectivity/+/e0051cdeb9d51a75ddea747657221e6089981794/framework/src/android/net/NetworkCapabilities.java).
Full telephony records, phone numbers and subscriber identifiers are never retained.
The current-state section is filtered on-device, before the bounded ADB read;
historical log growth cannot exhaust that read. Dump failures still fail capture.
Connectivity reads likewise retain only the active-default identifier and
current network-agent lines before applying the same bounded read.

Do not infer authorization from the number of live sessions. Bind the expected
inventory to unchanged profile/membership evidence, and verify the actual
membership through the existing peer tools before the series.

## Restoration And Limits

- Refuse an existing battery-reporting override rather than reset someone else's state.
- Install a uniquely tagged device-side restoration timeout before simulated unplug.
- Verify its readiness receipt, parent command line and live sleeping child before arming.
- A separate private armed marker gates resets; an unarmed watchdog cannot reset a later experiment.
- Restore reporting in `finally`; verify that `UPDATES STOPPED` is absent.
- Cancel a fallback or trace process only after matching its exact command-line tag.
- Verify process termination; unreadable command lines are not treated as absent processes.
- Keep the fallback armed if the device disappears; report restoration as unverified.
- Never reset battery history, alter no-auto-reset policy, uninstall, or erase app state.

| Limit | Bound |
| --- | --- |
| Concurrent collectors | One advisory lock per explicit serial |
| Default settling / capture | `300 / 1800 seconds` |
| Binary endpoint | `2 MiB` each |
| Retained trace | `32 MiB` |
| Metadata / individual command | Bounded size and timeout; private files only |
| First packet / steady command | `45-second` first-packet wait, `50-second` subprocess timeout; `15-second` steady timeout |
| Device fallback | Settling plus capture plus `490 seconds`: existing `360` plus two `65-second` post-window checks |
| Host watchdog | Settling plus capture plus `740 seconds`: existing `480` plus four `65-second` boundary checks |
| Overall claim | No automated goal completion or unmeasured savings claim |

The collector prewarms Protobuf decoding outside the interval, then schedules
the end sample against an absolute deadline. Endpoint/UID metadata is retained
incrementally; post-window probe errors remain functional failures.

An in-memory-only `DISCARD` buffer exhausted after approximately 21 minutes on
the Pixel. Keep that failed trace; periodic file streaming prevents this growth
without enlarging the buffer. Per-buffer losses invalidate rail corroboration.
Streaming uses `RING_BUFFER`; Perfetto flags `DISCARD` streaming as data loss.

The [November 13 pilot](android-cellular-lifecycle-pilot-2026-11-13.json) retains
the measured primary rate, rejected trace, functional results and smoke checks.
It is not a valid paired comparison or a battery-improvement claim.

The subsequent [complete first pair](android-cellular-lifecycle-pair-2026-11-13.json)
passed accounting, rail and boundary-probe checks, but failed the energy target.
The observed rate changed from 304.36 to 296.03 mAh/hour, not the required 10x.

| Candidate Observation | Consequence |
| --- | --- |
| No payload during either energy interval | Maintenance remained active without application traffic |
| Phone supported lifecycle, but was neither prepared nor quiet | Investigate idle eligibility before another long comparison |
| Two offline members retained in both arms | Do not remove members to improve the result |
| One original/candidate pair only | No replicated or statistically confirmed savings claim |

### Cold-Wake Probe Limit

`Device.probe` remains the legacy diagnostic: three pings with a three-second
per-packet wait and a 15-second subprocess timeout, with unchanged result semantics.
A failed short diagnostic does not prove that a dormant path cannot recover
within its first-wake budget. C2's failed short probes remain retained; it also
failed to enter maintenance pause and is not accepted.

Capture now uses the distinct `Device.wake_probe` at each boundary, identically
for reference and candidate arms:

1. Send exactly one numeric IPv4 ping, source-bound to the declared overlay address. Allow 45 seconds to observe its round-trip reply, with a bounded 50-second subprocess timeout including command overhead.
2. Retain that first packet's complete bounded stdout, exit status, packet summary, monotonic start/elapsed time and any command failure. Lossless base64 stdout and failure stderr remain in private boundary JSON, not console output.
3. Only after successful 1/1 first-packet delivery, send a separate three-packet steady check with three-second per-packet waits and a 15-second subprocess timeout. Require 3/3 received, not just ping exit status zero.
4. Keep `cold` and `steady` evidence separate in each existing `inbound_before.json`, `outbound_before.json`, `inbound_after.json` and `outbound_after.json`. A failed first packet has `steady: null` and cannot pass from later delivery.

No retries, warm-up packets, manual dials, relay changes or restarts rescue a
failed cold packet. Checks run before settling or after the end battery report
and trace retention, never inside the energy interval. A post-window delivery
failure or command timeout still retains accounting and a false functional
result. Interruption preserves available probe/window evidence before cleanup.

The `cold` field names the first-packet phase, not proof of prior dormancy.
Inbound runs before outbound; the first check can warm the reverse path. Do not
claim both sequential directions were independently cold. Separate device
preflight must verify lifecycle state and test each direction after an independent
idle period. The runtime's `FIRST_WAKE_PACKET_RETENTION` is 30 seconds per queue
admission; the 45-second observation window is not a retention guarantee or a
proof that recovery met that deadline. This change does not modify the runtime.

### Physical Preflight: C3

| Check | Result |
| --- | --- |
| Counterparts | Four Linux candidates plus the same candidate on Android; six authorized members, two offline |
| Independent idle | Three minutes before each direction; screen off and validated cellular |
| Incoming | One packet delivered in 1.21 seconds, then 3/3 steady packets |
| Outgoing | First packet failed after 45 seconds; no retry or steady check |
| Lifecycle | Local maintenance pause observed; phone peer did not commit quiet locally |
| Remaining work | First window: 33 outbound path probes, 165 scheduled AutoNAT events, seven DHT lookups |
| Acceptance | Rejected; no energy measurement or savings claim |
| Restoration | Original APK, profile, membership, Wi-Fi, and all four Linux services verified |

See the [retained C3 record](android-lifecycle-membership-preflight-2026-11-14.json).
The windows contain small payload-counter changes and are not clean energy
samples. Scheduled AutoNAT events are not transmitted-packet counts.

See Perfetto's [streaming contract](https://perfetto.dev/docs/concepts/config#streaming-long-traces)
and [buffer statistics](https://raw.githubusercontent.com/google/perfetto/main/protos/perfetto/common/trace_stats.proto).
