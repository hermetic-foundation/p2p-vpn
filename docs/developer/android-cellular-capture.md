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
| Incoming access | Source-bound overlay probes before settling and after measurement |
| Timed interval | No ADB/UI/status/traffic polling and no builds |
| Trace | Starts before the first binary report, ends after the last, with padding |
| Failure | Preserve the attempt, raw files, error and restoration outcome; never overwrite or silently retry |

This collector measures Android attribution while USB supplies physical power.
It does not claim physical battery discharge. APK staging and final restoration
belong to the enclosing experiment, not the per-window capture command.

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
| Sampling clocks | Boot UUID and `/proc/uptime` brackets around each report |
| Native status | Cached numeric counters at boundaries; distinguish payload from maintenance |
| App status | Enabled network, identity, runtime generation and underlay counters |
| Radio | Validated default cellular path and extracted signal readings; never retain full telephony registry |
| Temperature | Live thermal HAL reading; frozen BatteryService temperature is not current |
| Probe results | Retain exit status and bounded packet summary in both directions |
| Lifecycle state | Verify separately for lifecycle acceptance; zero sessions alone is not a quiet/reachability proof |

Do not infer authorization from the number of live sessions. Bind the expected
inventory to unchanged profile/membership evidence, and verify the actual
membership through the existing peer tools before the series.

## Restoration And Limits

- Refuse an existing battery-reporting override rather than reset someone else's state.
- Install a uniquely tagged device-side restoration timeout before simulated unplug.
- Restore reporting in `finally`; verify that `UPDATES STOPPED` is absent.
- Cancel a fallback or trace process only after matching its exact command-line tag.
- Keep the fallback armed if the device disappears; report restoration as unverified.
- Never reset battery history, alter no-auto-reset policy, uninstall, or erase app state.

| Limit | Bound |
| --- | --- |
| Concurrent collectors | One advisory lock per explicit serial |
| Default settling / capture | `300 / 1800 seconds` |
| Binary endpoint | `2 MiB` each |
| Retained trace | `32 MiB` |
| Metadata / individual command | Bounded size and timeout; private files only |
| Device fallback | Settling plus capture plus bounded cleanup allowance |
| Overall claim | No automated goal completion or unmeasured savings claim |
