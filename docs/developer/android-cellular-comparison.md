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

## Window Bundle

Keep each attempt in a new private directory. Never overwrite failed attempts.

| File | Source / Purpose |
| --- | --- |
| `start.pb`, `end.pb` | Original `dumpsys batterystats --proto` reports |
| `window.json` | UID, arm, clock brackets, identity and endpoint context |
| `trace.pftrace` | Matched, bounded Perfetto power-rail capture |
| Additional raw evidence | Connectivity probes, native state and restoration records |

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

Context records are collector observations, not cryptographic proofs. Retain
their raw sources. Native bilateral quiet/incoming-path state must be verified
separately before candidate admission; zero sessions alone proves neither.

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

## Rail Validation

| Check | Requirement |
| --- | --- |
| Required tracks | `power.rails.modem`, `power.rails.radio.frontend` |
| Track identity | Exactly one track ID per required name |
| Counters | Finite, nonnegative, monotonic cumulative microjoules |
| Samples | Strictly increasing timestamps; no gaps over three seconds |
| Interval | Both rails cover the complete battery-clock comparison interval |
| Integration | Linear interpolation only at interval boundaries; no extrapolation |
| Trace integrity | No positive Perfetto `error` or `data_loss` statistics |
| Tool provenance | Explicit trusted trace-processor argument; record executable hash |

Use fixed SQL. Never execute a tool path or SQL supplied by a bundle. Bound
input sizes, processor runtime and output size. Error messages must not expose
private phone metadata or raw records.

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
