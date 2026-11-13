# Cellular Battery Comparison

## Scope

| Item | Contract |
| --- | --- |
| Primary metric | Android UID end-minus-start mAh, divided by elapsed hours |
| Corroboration | Modem and radio-front-end rail energy over the same interval |
| Comparison | Three adjacent reference/candidate pairs, ordered `AB BA AB` |
| Target | Candidate/reference app rate at most `0.10` in every valid pair |
| Not evidence | VPN-off controls, historical totals, fewer counters, or host-only tests |
| Goal completion | Requires separate lifecycle, reachability and recovery verification |

Android app attribution is the feedback metric. An external meter and physical
discharge are not prerequisites. USB-powered, simulated-on-battery accounting
must be identical across arms and must not be called physical discharge.

## Implementation Sequence

1. Add a host-only analyzer for retained binary reports and Perfetto traces.
2. Add endpoint-only capture with device-side restoration and a fixed run plan.
3. Run fresh paired cellular windows on the intended phone.
4. Use measured results to select the next runtime change, then repeat.

The analyzer does not install APKs, invoke ADB, change settings, reset history,
or infer that a phone is reachable because it has no live sessions. A valid
energy calculation alone cannot establish functional equivalence.

## Analyzer Commands

The host-only analyzer and [capture adapter](android-cellular-capture.md) are
implemented. Fresh cellular comparisons remain outstanding. These commands
analyze existing bundles; they do not collect them or claim measured savings.

```sh
tool=$(nix build .#android-cellular-compare --no-link --print-out-paths)
analyzer="$tool/bin/p2p-vpn-android-cellular-compare"
"$analyzer" window "$window_directory" --trace-processor "$trusted_processor"
"$analyzer" series "$series_directory" --trace-processor "$trusted_processor"
nix build .#checks.x86_64-linux.android-cellular-compare --no-link
```

The processor must support `query --quiet --format csv`; v59.1 was used in
the earlier cellular diagnostic. Realize tools before any measured interval.
JSON results include the executable hash; invalid evidence exits nonzero.
Retain analyzer/decoder/projection, raw metadata and plan hashes with results.

| Result | Meaning |
| --- | --- |
| `valid` | Accounting/context/rail evidence passed analysis |
| `functionality_passed` | All four supplied boundary-probe results passed |
| `energy_criterion_observed` | All three app ratios meet `0.10`; modem and combined-rail reductions exceed sampling/clock uncertainty; probes pass |
| `goal_complete` | Always `false`; full lifecycle acceptance is outside this tool |
| Invalid series | Retains all six slot outcomes; no automatic replacement or filtering |

`series_directory/plan.json` names six distinct child directories. Freeze this
plan before capture; a post-hoc manifest alone cannot prove preregistration.

```json
{
  "schema_version": 1,
  "reference_apk": "<64 lowercase SHA-256 hex characters>",
  "candidate_apk": "<64 lowercase SHA-256 hex characters>",
  "window_seconds": 1800,
  "windows": ["01-A", "02-B", "03-B", "04-A", "05-A", "06-B"]
}
```

The analyzer rejects reused reports/traces, overlapping windows, changed boot
identity, unmatched device/profile/peer inventory, and changed accounting or
power policy. APKs must match the two planned hashes and differ from each other.

It accepts only known schema-1 metadata fields, not arbitrary private records.
Write results with `umask 077` and a new filename. Each input report is bounded
to 2 MiB; traces to 64 MiB; processor output to 2 MiB and 45 seconds per query.
Processor address space is capped at 1 GiB and CPU time at 40 seconds per query.
The overall 30-minute analysis deadline stops remaining slots, preserving their
unfinished status and every already-computed result.

Exit `0` means valid evidence and passing supplied probes, not 10x acceptance.
Read `energy_criterion_observed` explicitly; this tool never completes the goal.

## Window Bundle

Rail validation rejects positive per-buffer discard/overwrite counters even
when Perfetto labels them `info`. Service-global historical discards are not
this session's losses. Identical repeated HAL timestamp/value samples are collapsed;
conflicting duplicates, counter resets and uncovered endpoints remain invalid.

Keep each attempt in a new private directory. Never overwrite failed attempts.

| File | Source / Purpose |
| --- | --- |
| `start.pb`, `end.pb` | Original `dumpsys batterystats --proto` reports |
| `window.json` | UID, arm, clock brackets, identity and endpoint context |
| `trace.pftrace` | Matched, bounded Perfetto power-rail capture |
| Additional raw evidence | Connectivity probes, native state and restoration records |
| `result.json` | Collector completion and restoration gates; incomplete/rejected attempts cannot pass |

For collector bundles, the analyzer retains calculated energy even when collection
or restoration failed, but marks the window invalid. A capture marker without a
completion record is an unfinished attempt, not a successful manual bundle.

### Metadata, Schema 1

| Field | Shape |
| --- | --- |
| `schema_version` | Integer `1` |
| `arm` | `reference` or `candidate` |
| `uid` | Package UID, not copied from another phone |
| `settle_seconds` | Measured settling duration, at least `300` |
| `window_seconds` | Preselected duration, `1800` through `7200` |
| `identity` | SHA-256 strings described below |
| `start`, `end` | Endpoint objects described below |
| `probes` | Four Boolean results: `inbound_before`, `outbound_before`, `inbound_after`, `outbound_after` |

`identity` contains `device`, `os_build`, `apk`, `profile`, `peer_inventory`,
`workload` and `trace_config`. Hash canonical, private source records; retain
the records locally. Neither APK nor process identity must match across arms.

Each endpoint contains `boot_id`, `before_uptime_ms`, `after_uptime_ms` and
`context`. Uptime means `/proc/uptime` including suspend, sampled immediately
before and after the binary report. Both endpoints must belong to one boot.

### Endpoint Context

| Field | Requirement |
| --- | --- |
| `app_pid`, `runtime_generation` | Positive integers, stable within a window |
| `screen_interactive`, `wifi_enabled` | Both `false` |
| `underlay_kind`, `underlay_validated` | `cellular`, `true` |
| `underlay_generation` | Unchanged selection/loss/recovery fingerprint |
| `accounting_mode` | `simulated_on_battery` or `discharging`; matched across arms |
| `physical_usb_powered` | Boolean; matched across arms, not confused with simulated accounting |
| `power_save`, `device_idle` | Recorded power policy; matched endpoints and arms |
| `thermal_status` | Integer `0`; no thermal throttling |
| `temperature_tenths_c` | Live HAL battery temperature, not frozen BatteryService data |
| `rsrp_dbm` | Serving cellular signal, recorded at both endpoints |
| `authorized_peers` | Positive count, matched across arms; not the live-session count |
| `payload_sent_packets`, `payload_received_packets` | Native cumulative payload counters; unchanged throughout the idle window |

Context records are collector observations, not cryptographic proofs. Retain
their raw sources. Full lifecycle acceptance also needs native bilateral quiet
and incoming-path evidence; zero sessions alone proves neither.

Do not discard a high-energy candidate merely because it failed to become quiet.
Measure that failure and investigate it; reduced reachability cannot pass the goal.

Include remote host build identities and diagnostic-override state in the
hashed workload record. Keep counterparts identical across both arms; do not
pause probes or DHT work as a substitute for the implemented lifecycle.

Map payload fields from native `outbound_sent_packets` and
`inbound_accepted_packets`, summed across the measured app's enabled networks.
Capture end counters before post-window probes; probes must not enter the interval.

## Accounting Validation

- Decode original Protobufs with the existing bounded, full-precision decoder.
- Missing attribution is unavailable, never zero. Preserve explicitly reported zero.
- Reject reboots, resets, changed UID/schema, decreasing counters or missing clocks.
- Match session origins using monotonic brackets minus cumulative report realtime.
- Do not use Android's wall-clock session label as reset identity: NTP can change it.
- Require battery and screen-off deltas to cover the entire window.
- Reject screen wakes, underlay changes, process restarts and mismatched power policy.

Clock brackets may span at most two seconds. Allow only bounded sampling
uncertainty; report that uncertainty. Do not silently shorten a window until
its screen or radio conditions look better.

### Pixel Wire Compatibility

| Case | Handling |
| --- | --- |
| Omitted zero `start_count` | Decode as `0`; still require the timing fields |
| Uptime versus realtime | Permit at most `5 ms` endpoint skew; preserve original values |
| Same-clock ordering | Remains strict; screen-off time cannot exceed battery time |
| Interval admission | Independently require matching monotonic session origins and screen-off deltas |

Retained Pixel reports reproduce the omitted counter and `1-2 ms` clock skew.
AOSP's [writer](https://raw.githubusercontent.com/aosp-mirror/platform_frameworks_base/android16-release/core/java/android/os/BatteryStats.java)
samples uptime and realtime separately; its
[serializer](https://raw.githubusercontent.com/aosp-mirror/platform_frameworks_base/android16-release/core/java/android/util/proto/ProtoOutputStream.java)
omits zero integer fields. The five-millisecond limit is our conservative
decoder tolerance, not an Android API guarantee.

## Rail Validation

| Check | Requirement |
| --- | --- |
| Required tracks | `power.rails.modem`, `power.rails.radio.frontend` |
| Track identity | Exactly one track ID per required name |
| Counters | Finite, nonnegative, monotonic cumulative microjoules |
| Samples | Strictly increasing timestamps; no gaps over three seconds |
| Interval | Both rails cover the complete battery-clock comparison interval |
| Estimate | Linear interpolation only at interval boundaries; no extrapolation |
| Bounds | Neighboring-counter extrema cover burst timing and clock uncertainty; not interpolated bounds |
| Trace integrity | No positive Perfetto `error` or `data_loss` statistics |
| Tool provenance | Explicit trusted trace-processor argument; record executable hash |

Use fixed SQL. Never execute a tool path or SQL supplied by a bundle. Bound
input sizes, processor runtime and output size. Error messages must not expose
private phone metadata or raw records.

Start the trace before the first report and stop it after the last report.
Include enough boundary padding for endpoint commands and sampling uncertainty;
tracing for exactly the nominal battery-window duration is insufficient.

### Historical Replay

The [replay record](android-cellular-analyzer-replay-2026-11-13.json) exercises
the new parser against retained Pixel data. It is not a fresh comparison.

| Observation | Replay Result |
| --- | --- |
| Android app rate | `295.27325494963645 mAh/hour` |
| Battery interval | `605.398 seconds`; too short for acceptance |
| Modem rail, trace interval only | `1064.0814355233986 mW` |
| Trace boundary coverage | Rejected: rails miss both ends of the battery interval |
| New savings measurement | None |

## Replication And Outcomes

1. Freeze identities, workload, durations, trace configuration and `AB BA AB` order.
2. Perform real inbound and outbound probes before settling and after capture.
3. Keep screen off and avoid ADB/UI/network probes during the measured interval.
4. Retain every failed, invalid and incomplete attempt in sequence.
5. Report per-window rates, paired ratios and dispersion; do not cherry-pick.
6. Treat failed reachability as a functional failure, not a favorable energy result.

App rate at most `0.10` in all three pairs is an observed metric criterion,
not automatic project completion. Require reduced modem energy too, then audit
the full lifecycle goal, including LAN return and screen-off inbound recovery.

## Restoration

- Never clear battery history, uninstall the app, or erase pairing/membership.
- Preserve installed APK, profile, power settings and accounting mode before changes.
- Restore battery reporting on exceptions and through a device-side timeout.
- Verify restoration explicitly; retain failures rather than declaring success.

See [trial build provenance](android-lifecycle-trial-build-2026-11-13.json)
and [battery methodology](android-battery-efficiency.md) for attribution limits.
