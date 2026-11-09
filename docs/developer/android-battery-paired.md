# Legacy Wi-Fi Battery Pairs

## Scope

This developer tool compares Android UID estimates under a connected-idle Wi-Fi
workload. Host tests cover analysis and fake-device control, not physical-phone
acceptance of this runner.

| Supported | Not Established |
| --- | --- |
| Fixed-order `AB BA AB` connected-idle observations | Cellular modem energy or sleep residency |
| Raw UID reports and selected endpoint context | Whole-phone battery-life improvement |
| Preserved invalid and interrupted attempts | Negotiated dormant-path behavior |

Use [cellular capture](android-cellular-capture.md) and
[cellular comparison](android-cellular-comparison.md) for the demand-lifecycle
experiment. Do not run this legacy tool on an active everyday VPN.

## Requirements

- Dedicated, already paired debug phone with VPN consent granted.
- USB debugging and physical USB power throughout the run.
- Initially disconnected VPN; no always-on, lockdown, or active pairing.
- Original normal battery reporting; no pending accounting override.
- Prebuilt APKs with the same signer, retained at absolute paths.

## Plan

Create the plan before starting. Use the actual USB serial and APK hashes:

```json
{
  "serial": "DEVICE_SERIAL",
  "reference": { "apk": "/absolute/reference.apk", "sha256": "REFERENCE_SHA256" },
  "candidate": { "apk": "/absolute/candidate.apk", "sha256": "CANDIDATE_SHA256" },
  "expected_peers": 5,
  "settle_seconds": 300,
  "window_seconds": 1800
}
```

Each trial installs its declared APK, waits for expected peer/QUIC readiness,
settles for at least five minutes, and captures at least thirty minutes. Six
trials take about 3.5 hours plus installation and readiness time.

## Run

Realize the package before any timed interval:

```sh
runner=$(nix build .#android-battery-paired --no-link --print-out-paths)
"$runner/bin/p2p-vpn-android-battery-paired" \
  --plan plan.json --output /tmp/battery-pairs
nix build .#checks.x86_64-linux.android-battery-paired --no-link
```

Use a new output directory for every series. Keep the phone unused and do not
run builds, probes, UI automation, or other collectors during measurement.

## Validation

| Check | Rule |
| --- | --- |
| UID accounting | End-minus-start mAh divided by elapsed hours; never historical totals |
| Within-window state | Stable APK, UID/schema, process, network generation, identity, and power policy |
| Cross-trial context | Same profile, enabled-network identity, Wi-Fi fingerprint/frequency, UID/schema, and power policy |
| Radio | RSSI at least -65 dBm, window/pair drift at most 5 dB, no thermal throttling |
| Payload | No outgoing or incoming payload-counter increase; missing counters reject the window |
| Recovery | Retain demotions, expirations, and probe failures as outcomes |
| Series | Missing or invalid trials invalidate the series; no selective replacements |

Raw `start.pb` and `end.pb`, decoded endpoints, context, and every trial result
remain in the private output directory. Counter deltas are diagnostics, not
measured energy savings.

Raw battery reports can contain other apps' usage. Keep them private; do not
commit or share unredacted captures. Cached native counters can lag, so endpoint
checks do not prove the absence of every packet throughout the interval.

## Safety

- Share the cellular collector's exclusive device lock and bounded ADB commands.
- Arm the existing device-side restoration fallback before simulated unplugging.
- Keep a host watchdog; retain failed cleanup and stop the series on device failure.
- Restore only this runner's accounting override, including after interruption.
- Never reset battery statistics or change the automatic-reset policy.
- Leave the VPN disconnected and screen off, with the last tested APK installed.

The device-side fallback restores normal battery reporting if USB management
disappears. It does not restore an APK or repair VPN connectivity.

## Interpretation

Results are descriptive. They do not isolate every source of natural Wi-Fi,
infrastructure, device-idle, or peer variability. Endpoint counts do not prove
continuous reachability or identical remote inventories/builds.

An A/A plan may use the same APK to investigate session noise. Missing context
or mixed effect directions is inconclusive. Even a 0.10 ratio does not prove
modem savings, feature preservation, or completion of the protocol work.
