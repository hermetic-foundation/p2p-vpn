# Cellular Combined Maintenance Isolation

## Status

| Question | Finding |
| --- | --- |
| Is idle cellular power associated with the VPN? | Normal/off reference: modem power fell from 1,064.08 to 17.28 mW |
| Do outbound DHT work and peer probes explain everything? | Combined suppression left 891.98 mW in a separate diagnostic |
| Is the exact remaining protocol identified? | No; transport maintenance and other background traffic remain candidates |
| Has a feature-preserving 10x improvement shipped? | No production optimization was deployed |

[Structured results](android-cellular-combined-2026-11-11.json) contain full precision,
trace digests, context, counters, and restoration checks. Raw reports and traces
remain private; do not publish installed-app inventories or packet metadata.

## Experiments

| Series | Stages Captured | Outcome |
| --- | --- | --- |
| Combined intervention | A1 normal, B outbound DHT plus bidirectional probes paused | Two valid windows; stopped before A2 |
| Independent reference | N normal, C VPN force-stopped and remote phone probes paused | Both windows completed and passed |

The reference pair is a fresh experiment, with fresh processes and settling.
Do not combine its windows with A1/B into a completed A-B-A-off series.
The earlier [DHT](android-cellular-dht-isolation.md) and
[probe-only](android-cellular-probe-isolation.md) tests used different accounting conditions.

## Measurement Contract

| Item | Control |
| --- | --- |
| Device | Pixel 8 Pro, GrapheneOS, API 37 |
| Underlay | Validated LTE; Wi-Fi disabled |
| Screen | Off throughout accepted windows; event log and accounting-clock checks |
| Settling / capture | 5 minutes / 10 minutes per stage |
| Software | Same diagnostic APK; no builds or phone polling during capture |
| Linux | Four diagnostic processes; only phone-targeted probes paused |
| Collector | Experiment-local harness; no production benchmark-script or CLI change |
| App energy | Android attributed mAh delta divided by measured elapsed hours |
| Hardware energy | Whole-phone modem and radio-frontend rails, reported separately |
| Tracing | Pinned Perfetto v59.1; 601 samples per required rail; no errors or loss |
| Failure handling | Retain failed attempts, topology changes, and failed boundary traffic |

APK SHA-256:
`9f3cac6c0a6229c7af9126a0d9f07f92d3a5b51ed5c5fc47b5ee2e0e33c6b1be`.

### Battery Accounting

- USB remained physically attached and powered; these are not battery-discharge trials.
- Temporary `dumpsys battery unplug -f` enabled on-battery accounting for every accepted window.
- The accounting-mode transition opened a new current-statistics period on this device.
  Preserve earlier snapshots; do not compare cumulative totals across periods or accounting modes.
- `dumpsys battery reset -f` restored hardware reporting afterward.
  This is not the history-erasing `dumpsys batterystats --reset` command.
- A device-side 90-minute fallback was installed to restore reporting if host control was lost.
- Live battery temperature came from the thermal HAL; the frozen BatteryService value was not used for safety admission.

### Session Identity

The initial collector treated `start_clock_time_ms` as an immutable session ID.
Android's time detector corrected the phone clock by approximately -1.2 seconds
before A2, while elapsed counters and app consumption continued increasing.

[AOSP adjusts that wall-clock label after clock changes](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/services/core/java/com/android/server/power/stats/BatteryStatsImpl.java).
The corrected collector uses the boot UUID and a bounded monotonic session-origin
interval derived from device uptime around each report and the report's elapsed time.

| Event | Corrected Behavior |
| --- | --- |
| Wall-clock correction with continuous monotonic session | Retain wall-clock metadata; session remains compatible |
| Statistics reset, even if the next elapsed value is larger | Reject disjoint monotonic origins |
| Reboot or monotonic reversal | Reject |
| Missing per-app estimate | Retain raw report; reject the app-energy window |
| Screen activity | Reject using elapsed screen-off clocks plus screen-event checks |

Automatic time synchronization was not disabled. The correction was identified
from the device's time-detector log and retained raw battery reports.
The rejected A2 was not retroactively counted as a completed measurement.

### VPN-Off Reference

1. Force-stop only the tested app in Android user 0; preserve its profile and VPN settings.
2. Pause remote probes targeting the phone, then allow the full settling interval.
3. Verify absence of the app process and its owned VPN agent.
4. Use OS-only sampling; do not send automation broadcasts that could restart the app.
5. Reject app restarts, underlay changes, screen wakes, resets, or invalid traces.

## Attempt Accounting

| Attempt | Outcome |
| --- | --- |
| First combined run | User-requested interruption during B; partial window excluded; restoration completed after USB returned |
| Charging-mode retry | Per-app estimate unavailable at the first endpoint; aborted and restored; retained modem trace is not an app-energy comparison |
| Fixed-accounting combined run | Valid A1/B; over-strict wall-clock guard stopped the series before A2; no off control captured |
| Independent reference pair | Corrected monotonic guard; N/C completed; all devices restored |

Charging-mode snapshots also showed declining attributed app totals without a
statistics reset. App attribution can reweight; it is not an independent physical
energy counter. Retaining raw reports made the collector failures diagnosable.

## Results

### Combined Intervention

| Measurement | A1: Normal | B: DHT And Probes Paused |
| --- | ---: | ---: |
| Local start, November 11 CST | 20:33:13 | 20:48:36 |
| Accounting interval, seconds | 604.483 | 604.401 |
| App energy delta, mAh | 50.280739 | 39.513384 |
| App estimate, mAh/hour | 299.447 | 235.354 |
| Modem rail, mean mW | 1,085.05 | 891.98 |
| Frontend rail, mean mW | 182.62 | 112.23 |
| DHT query steps / dispatched dial intents | 1,785 / 1,840 | 0 / 0 |
| Sent / accepted path probes | 409 / 395 | 0 / 0 |
| AutoNAT scheduling intents | 483 | 134 |
| Sent / accepted overlay payload packets | 0 / 0 | 0 / 0 |
| Connected peers, start / end | 3 / 4 | 2 / 4 |
| LTE RSRP, start / end, dBm | -109 / -109 | -110 / -110 |
| Live battery temperature, start / end, C | 32.2 / 33.2 | 33.2 / 33.1 |
| Boundary ping replies, before / after | 3/3 / 3/3 | 3/3 / 3/3 |

- B was 21.40% lower in attributed app energy and 17.79% lower in modem power than A1.
- About 82% of A1's modem power remained while both targeted activities were quiet.
- These are descriptive differences from one incomplete fixed-order series, not replicated savings.
- Peer-count changes were retained. Query, dial, and AutoNAT intents are not wire-packet counts.

### Independent Normal/Off Reference

| Measurement | N: Normal | C: VPN Off |
| --- | ---: | ---: |
| Local start, November 11 CST | 21:20:23 | 21:35:47 |
| Accounting interval, seconds | 605.398 | 605.168 |
| App energy delta, mAh | 49.654955 | 0.411909 |
| App estimate, mAh/hour | 295.273 | 2.450 |
| Modem rail, mean mW | 1,064.08 | 17.28 |
| Frontend rail, mean mW | 184.91 | 0.50 |
| DHT query steps / dispatched dial intents | 1,846 / 1,872 | Not sampled; app stopped |
| Sent / accepted path probes | 388 / 391 | Not sampled; app stopped |
| AutoNAT scheduling intents | 571 | Not sampled; app stopped |
| Sent / accepted overlay payload packets | 0 / 0 | Not sampled; app stopped |
| Connected peers, start / end | 3 / 3 | 0 / 0 |
| LTE RSRP, start / end, dBm | -107 / -107 | -108 / -108 |
| Live battery temperature, start / end, C | 32.1 / 33.3 | 31.6 / 29.3 |

- Modem power was 98.38% lower, approximately 61.6 times lower, with the VPN stopped.
- No tested-app process, owned VPN agent, or app restart was observed during C.
- All accepted windows reported Android `IDLE`, with light state `OVERRIDE`, at both endpoints.
  Mid-window idle transitions were not traced; no screen wakes were reported.
- C's small positive app estimate is residual attribution, not evidence that the app executed.
- Stopping the VPN is a reference control, not a feature-preserving optimization.

## Interpretation And Next Test

| Evidence | Interpretation |
| --- | --- |
| Two normal windows near 295-299 mAh/hour and 1,064-1,085 mW modem | High idle cellular cost reproduced with fixed accounting |
| No sent/accepted overlay payload packets in normal windows | Bulk overlay payload was not needed to reproduce the cost |
| Combined pause leaves 891.98 mW | Targeted outbound DHT work and peer probes are not the whole explanation |
| Independent off reference reaches 17.28 mW | Strong evidence associates most observed modem load with VPN networking |

The [host configuration](../../src/runtime/p2p.rs) includes 15-second libp2p pings
and uses the default `libp2p-quic` 0.13.1 configuration, which has a 5-second
keepalive interval. Existing transport traffic is outside the DHT/probe pause gates.

Next, measure actual transport-maintenance traffic and isolate keepalives,
connection retention, and other unpaused control traffic. Configured intervals
are candidates, not proof of which packets account for the remaining energy.

| Acceptance | Value |
| --- | ---: |
| Normal app-attributed baseline for this workload | 295.273 mAh/hour |
| 10x target under matched conditions | At most 29.527 mAh/hour |
| Feature-preserving production improvement demonstrated | None |

Repeat promising effects with reversed order and feature-preservation tests.
These current experiments do not establish November 7's exact historical cause
or whole-day unplugged battery life.

## Restoration And Validation

- Original APK, encrypted profile, and Android VPN settings matched after restoration.
- Hardware battery reporting, Wi-Fi, and VPN connectivity were restored; four peers connected and ping passed 3/3.
- All four original Linux executables/config hashes matched; unrelated instance PIDs were unchanged.
- Owned runtime overrides, rollback timers, staging, and phone background fallback processes were removed.
- Collector tests: 17 passed, including clock correction, reset/reboot detection, screen clocks, and raw-report retention.
- Existing battery-decoder tests: 14 passed.
- No production source or APK changes were made; full Rust/Android/Nix matrices were not rerun for this follow-up.
