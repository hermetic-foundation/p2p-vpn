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
