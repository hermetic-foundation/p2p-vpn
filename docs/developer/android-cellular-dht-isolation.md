# Cellular DHT Isolation

## Purpose

Identify whether outbound DHT query activity contributes to cellular energy
consumption. This is a diagnostic intervention, not a battery optimization.
Disabling discovery is not a production fix.

## Build Boundary

| Layer | Requirement |
| --- | --- |
| Native Android crate | Opt-in `battery-diagnostics` Cargo feature |
| Java | Debug APK; receiver requires `android.permission.DUMP` |
| Runtime control | In-process only; no network or Unix socket command |
| Default packages | Feature disabled; diagnostic command reports unavailable |
| Persistent state | No profile, identity, membership, or configuration changes |

The Android feature forwards to the root runtime feature. Build the native
library with `--package p2p-vpn-android --features battery-diagnostics`, using
the Android toolchain and APK packaging described in [Android](android.md).

## Control

Use the existing enabled network UUID from the debug `status` response.
Always select the intended device explicitly.

```sh
adb -s "$serial" shell am broadcast --receiver-foreground \
  -a org.hermeticfoundation.p2pvpn.debug.AUTOMATION \
  -n org.hermeticfoundation.p2pvpn.debug/org.hermeticfoundation.p2pvpn.DebugAutomationReceiver \
  --es command diagnostic-dht-pause \
  --es network_id "$network_id" --ei seconds 1200
```

| Value | Behavior |
| --- | --- |
| `0` | Resume immediately |
| `1` through `1800` | Pause for that many seconds; automatic expiry |
| Missing, negative, or over `1800` | Reject without changing mode |
| Unknown or stopped network | Reject |

The response uses the normal base64-encoded JSON envelope. Confirm `ok` and
the acknowledged duration. `resource-status` exposes
`kad_primary_diagnostic_paused`; it becomes zero after resume or polled expiry.

## Intervention Boundary

- Pause outbound query advancement, automatic bootstrap, and background job starts.
- Preserve Kademlia connections, routing state, inbound request handling, and queued actions.
- Already dispatched requests can finish; use a fixed settling interval before measuring.
- Other protocol policies remain unchanged; their resulting activity can change indirectly.
- Query state remains bounded by existing resource limits and can expire during the pause.
- Runtime restarts discard the pause; record restarts rather than silently reapplying it.

## Measurement Plan

| Stage | DHT Mode | Settling | Capture |
| --- | --- | ---: | ---: |
| A1 | Normal | 5 minutes | 10 minutes |
| B | Paused, 20-minute lease | 5 minutes | 10 minutes |
| A2 | Normal again | 5 minutes | 10 minutes |

Use one APK and one running VPN session for all stages. Test boundary
connectivity before settling and after capture, never inside the idle window.
Keep screen off and cellular selected throughout.

Run Perfetto with `--background-wait` and a bounded `duration_ms`, not a
long-lived foreground ADB shell. A USB data disconnect must not terminate
the device-side trace. This does not require weakening USB port security.

- Allow a fixed, identical flush margin before endpoint collection.
- Reject USB power changes using battery-accounting elapsed time, not just endpoint plug state.
- Use bounded reconnect attempts for cleanup and retain interrupted traces.

### Measurements

| Signal | Role |
| --- | --- |
| UID mAh delta / elapsed hours | Android per-app energy estimate |
| Modem and radio frontend rails | Physical whole-phone subsystem energy |
| Query request steps and dial intents | Confirm intervention took effect; not wire-packet counts |
| Path probes, AutoNAT, relay events | Identify indirect effects and remaining work |
| Peers and boundary traffic | Detect loss of useful connectivity |
| Screen events, PID, reset epoch, underlay | Validate each complete window |
| Temperature and cellular signal, if exposed | Record environmental variation |

### Interpretation

1. Verify normal stages reproduce elevated consumption before attributing a cause.
2. Require zero new query steps and query dial intents in the paused window.
3. Look for power falling during B and returning during A2.
4. Repeat a promising result with reversed order before claiming a stable effect.
5. Retain connectivity failures and peer losses as outcomes, not exclusions.

Reject interrupted windows consistently, retain their evidence, and report an
incomplete series as inconclusive. Do not crop a failed full window into a
passing result or extrapolate a short result to whole-day battery savings.

## Cleanup

- Explicitly resume DHT work, even though the lease also expires automatically.
- Restore the original APK and Wi-Fi/VPN state without uninstalling or clearing data.
- Verify identity/profile hashes and ordinary peer traffic after restoration.
- Remove only owned phone trace/config files; keep bounded captures private.
- Never reset battery history to improve the appearance of a result.

## First Device Attempt

November 10, 2026: Pixel 8 Pro, API 37, screen off, LTE, USB powered.
The diagnostic APK's SHA-256 is
`cc3d2abbcbc86ca5c1e0e582f51cc2bcc2ac3b1ee659842b33087b39153ac88d`.

| Stage | Outcome |
| --- | --- |
| A1 | Valid 601.448-second per-app window; no screen wake or underlay change |
| B | Interrupted by USB disconnect; no usable trace or end-energy reading |
| A2 | Not run; collector aborted and original APK was restored |

| A1 Measurement | Value |
| --- | ---: |
| Android app energy | 9.809527 mAh, or 58.715462 mAh/hour |
| Modem rail | 1,054.80 mW average over 599.747 seconds |
| Radio frontend rail | 160.52 mW average over 599.747 seconds |
| DHT query request steps | 1,390 |
| DHT dispatched dial intents | 1,407 |
| AutoNAT scheduling intents | 535 |
| Outbound path probes | 260 |
| Sent / accepted overlay data packets | 0 / 0 |
| LTE RSRP endpoints | -107 / -108 dBm |
| Connected overlay peers, endpoints | 3 / 2 |

- Power-rail counters were monotonic; no trace parsing errors or loss reported.
- USB on-battery elapsed time was zero; per-app readings remain Android estimates.
- Boundary ping results were 2/3 before settling and 3/3 after capture.
- Host USB disconnected at 19:40:05 Central and reconnected at 19:42:51.
- The original APK/profile were verified unchanged after recovery; Wi-Fi and VPN restored.

No completed comparison exists from this attempt. It does not establish a
DHT energy contribution, a battery improvement, or Saturday's historical cause.
The background recorder passed a separate 20-second collection smoke test.
