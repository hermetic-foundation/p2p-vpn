# Cellular Peer-Probe Isolation

## Purpose

Test whether periodic peer probes contribute to the Pixel's idle cellular power.
Pause senders on both ends, rather than dropping packets after the radio wakes.
This diagnostic does not disable probes in production builds.

## Boundaries

| Item | Contract |
| --- | --- |
| Build | Opt-in `battery-diagnostics`; debug Android receiver requires `DUMP` |
| Lease | 0 resumes; 1 through 1800 seconds pauses; invalid durations rejected |
| Scope | All outgoing phone probes; only probes targeting the phone on Linux peers |
| Expiry | Monotonic deadline; sending resumes automatically; restart discards the lease |
| Storage | No identity, membership, persistent configuration, or peer-protocol change |
| Unchanged | DHT, AutoNAT, relays, data delivery, acknowledgements, and recovery policies |

The gate runs before either packet-plane or stream probe dispatch.
Existing in-flight probes can finish; use the full settling interval.
One lease replaces the previous lease; storage cannot grow with repeated commands.

## Controls

### Android

Use the enabled network UUID returned by debug `status`.

```sh
adb -s "$serial" shell am broadcast --receiver-foreground \
  -a org.hermeticfoundation.p2pvpn.debug.AUTOMATION \
  -n org.hermeticfoundation.p2pvpn.debug/org.hermeticfoundation.p2pvpn.DebugAutomationReceiver \
  --es command diagnostic-probe-pause \
  --es network_id "$network_id" --ei seconds 1200
```

### Linux

Use a diagnostic binary built with `cargo build --features battery-diagnostics`.
The hidden command and its local-socket handler are absent from default builds.
Keep the existing socket's owner-only permissions.

```sh
sudo ./p2p-vpn diagnostic-probe-pause \
  --socket /run/p2p-vpn-personal-devices/control.sock \
  --peer "$phone_peer_id" --seconds 1200
```

Omitting `--peer` targets all peers; do not use that form on shared Linux instances.
Set `--seconds 0` to resume. No network-accessible diagnostic protocol is added.

### Confirmation

- Successful controls acknowledge `path_probe_diagnostic_pause_seconds N`.
- Cached Android `resource-status` and Linux `daemon-state` expose
  `path_probe_diagnostic_paused`; allow the normal cache/timer update interval.
- Verify zero phone sent and accepted probes during B, not just control acknowledgements.
- Verify outbound DHT work remains unpaused and query steps advance.

## Measurement

| Stage | Peer Probes | DHT | Settling | Capture |
| --- | --- | --- | ---: | ---: |
| A1 | Normal | Normal | 5 minutes | 10 minutes |
| B | Paused in both directions | Normal | 5 minutes | 10 minutes |
| A2 | Normal again | Normal | 5 minutes | 10 minutes |

Use the same APK, phone VPN process, and Linux processes across all stages.
Use the energy collection and trace checks in [DHT isolation](android-cellular-dht-isolation.md).
Record app-attributed mAh/hour and physical modem/frontend rails separately.

- Preflight every Linux binary and its runtime dependencies before changing any service.
- Verify the analyzer version/checksum and parse a known trace before deployment.
  Protect executable dependencies from garbage collection for the full run.
- Use independent LAN management paths and only temporary per-instance overrides.
- Install an independent bounded rollback timer before restarting a diagnostic service.
- Reject screen wakes, process changes, underlay/power changes, invalid traces, or ineffective pauses.
- Retain failed boundary traffic, peer losses, temperature, and signal changes as outcomes.
- Keep build, polling, and boundary-ping activity outside capture windows.

## Interpretation

1. Confirm high cellular power in normal stages and an effective probe pause during B.
2. Look for energy falling in B and rising after probes resume.
3. Do not infer energy savings from lower probe counts alone.
4. A small or null effect does not exonerate probing: DHT and other maintenance
   may independently keep the radio active. Combined interventions can test that interaction.
5. Repeat a promising effect in reversed order before claiming a stable benefit.

## Restoration

- Explicitly clear all leases, even though they expire automatically.
- Restore the original APK without uninstalling or clearing app data; verify profile and APK hashes.
- Restore Wi-Fi/VPN/screen state and verify ordinary overlay traffic.
- Remove only owned runtime overrides; verify original executables and config hashes.
- Confirm unrelated instance PIDs are unchanged; remove owned rollback timers and staging files.
- Keep raw traces and device metadata private. Never reset battery history for a cleaner result.

## November 11 Results

Pixel 8 Pro, API 37; validated LTE, Wi-Fi disabled, screen off, physically USB powered.
All stages used the same release-profile diagnostic APK and Linux diagnostic processes.
Linux peers used the development profile; no builds ran during accepted captures.

APK SHA-256:
`9f3cac6c0a6229c7af9126a0d9f07f92d3a5b51ed5c5fc47b5ee2e0e33c6b1be`.

### Attempt Accounting

| Attempt | Outcome |
| --- | --- |
| Deployment/control preflights | Missing Linux runtime dependency and missing Android status gauge caught; corrected before energy capture |
| First measurement | Normal window captured; missing local analyzer stopped the series before B; all services and phone restored |
| Fresh retry | Full A1-B-A2 series completed; only this series is compared below |

The retained first normal window measured 220.707 mAh/hour, 1,030.28 mW modem,
and 185.73 mW frontend power. It is not combined with the retry's intervention.
The restored analyzer was checksum-verified and parsed that trace before retrying.

### Energy

| Measurement | A1: Normal | B: Probes Paused | A2: Normal |
| --- | ---: | ---: | ---: |
| Local window start, CST | 14:25:24 | 14:40:45 | 14:56:06 |
| App accounting interval, seconds | 604.315 | 604.539 | 604.457 |
| Android app energy, mAh | 35.894951 | 33.913778 | 37.055283 |
| Android app estimate, mAh/hour | 213.832 | 201.955 | 220.692 |
| Modem rail, mean mW | 954.96 | 935.18 | 1,002.39 |
| Radio frontend rail, mean mW | 181.65 | 144.44 | 172.09 |
| Power-rail span, seconds | 599.274 | 599.221 | 599.730 |
| Samples per required rail | 601 | 601 | 601 |

Hardware rails measure whole-phone subsystems, not per-app physical energy.
These USB-powered measurements do not establish whole-day unplugged battery life.

### Activity And Context

| Measurement | A1 | B | A2 |
| --- | ---: | ---: | ---: |
| Outbound path probes | 401 | 0 | 400 |
| Accepted inbound path probes | 397 | 0 | 396 |
| DHT query request steps | 1,981 | 1,207 | 1,402 |
| DHT dispatched dial intents | 1,979 | 1,197 | 1,429 |
| AutoNAT scheduling intents | 571 | 510 | 479 |
| Sent / accepted overlay data packets | 0 / 0 | 0 / 0 | 0 / 0 |
| Connected peers, start / end | 3 / 4 | 4 / 3 | 4 / 3 |
| LTE RSRP, start / end, dBm | -109 / -109 | -107 / -109 | -109 / -109 |
| Battery temperature, start / end, C | 29.5 / 31.0 | 31.3 / 31.6 | 32.0 / 32.5 |
| Boundary ping replies, before / after | 1/3 / 3/3 | 3/3 / 3/3 | 3/3 / 3/3 |

- All complete windows passed context, screen, trace-integrity, and intervention checks.
- The phone and four Linux process IDs stayed unchanged throughout the retry.
- The app UID and battery-accounting reset epoch matched across all six endpoints.
- No screen wakes, underlay changes, USB power loss, rail resets, or trace loss were reported.
- Both directions were quiet during B; DHT stayed unpaused and continued advancing queries.
- Failed initial transition pings and changing peer counts were retained, not filtered out.
- Query, dial, and AutoNAT intent counts are not wire-packet counts.

### Findings

1. Energy dipped during B and rose after probes resumed: consistent with a modest contribution.
2. Against the mean of the normal stages, B was 7.05% lower in app attribution,
   4.44% lower on the modem rail, and 18.33% lower on the frontend rail.
3. About 96% of normal modem power remained. Probe suppression alone is not near the 10x target.
4. These are descriptive differences from one fixed-order series, not replicated savings.
   Peer counts, signal, temperature, and other activity varied between windows.
5. Next, pause outbound DHT work and bidirectional probes together to test interaction.
   Either group may independently sustain radio activity; neither individual pause exonerates it.

No production battery optimization was deployed. This does not identify Saturday's
historical cause or prove that a feature-preserving change achieves these differences.

### Restoration And Validation

- Original phone APK and encrypted profile hashes matched after restoration.
- Validated Wi-Fi returned; VPN and always-on stayed enabled, lockdown disabled, screen off.
- Four peers were connected after restoration; overlay ping succeeded 3/3.
- All four original Linux executables and config hashes matched; unrelated instance PIDs were unchanged.
- Owned Linux runtime overrides, rollback timers, staging files, and phone capture files were removed.
- Raw traces, failure logs, and device metadata remain private.

| Check | Result |
| --- | --- |
| Diagnostic Rust library tests | 1,553 passed; 8 existing ignored |
| Default Rust library tests | 1,546 passed; 8 existing ignored |
| Diagnostic CLI parsing | 47 passed with a 16 MiB test-thread stack |
| Default CLI diagnostic-command exclusion | Passed |
| Android unit tests / lint / debug APK | Passed |
| Native ARM64 release / Linux diagnostic binary | Built and exercised on devices |

CLI parsing checks used `RUST_MIN_STACK=16777216`. The default-stack parsing
overflow was also reproduced with the pre-change `main.rs`; that limitation remains.
The full Nix/Windows matrix was not run for this diagnostic change.

### Trace Digests

| Stage | SHA-256 |
| --- | --- |
| First attempt A1 | `84456ec1063e27e58625d2dbb802c7e1620f8164903ab74df46158736343b868` |
| Retry A1 | `95092e04b255ef2b16998a806aea472f505ab7c0f2951924bc588be05bf4240a` |
| Retry B | `bb102342cfaa016ef31b9fc708ad82fb82bc9d9d1221fbc718c2bf8766eaccb9` |
| Retry A2 | `287c2affd56ff0ff1aa302de2f011b17ab850831d0d042f0e09f98359eacc8ae` |
