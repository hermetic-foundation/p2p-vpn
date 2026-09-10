# Android Collector Controls

## Frozen Workload

This manifest precedes the first capture. These controls measure observer cost;
they do not replace S7's sustained load or independent-network transitions.

| Setting | Value |
| --- | --- |
| Source baseline | `3a36a371`, plus the recorded control harness changes |
| Topology | Owned API 35 emulator; two private bootstrap/overlay fixtures; no public route |
| APK | `ea03c922be28faf3043c540c69dd898ce1b5f7206528ffbafc55c6f5b8527f46` |
| Fixture | `4b1bcd69a410f9a830e813d9457aa0a55a8a507d762c208aad42a1092ab8f8fa` |
| Build profile | Cached debug APK/native runtime; no builds during observation |
| Admission | Existing bidirectional IPv4/IPv6 two-network checks; setup excluded |
| App state | HOME then SLEEP; require noninteractive/asleep power state; 30-second warmup |
| Order | Off, on, on, off; four 60-second idle windows |
| Traffic | None deliberately generated during windows; discovery/recovery remain enabled |
| Process collection on | 60 Android process observations, one-second sleeps |
| Runtime collection on | 12 cached counter/diagnostic observations on five-second deadlines |
| Collection off | Only matching boundary observations; no periodic collector |
| Boundary data | Android and emulator process CPU/identity/RSS/FD/thread samples; native counters and PSS |
| Extra context | Host load averages and guest/host clock tick rates |
| Deadlines | 600-second inner plus 15-second grace; 640-second outer plus 20-second grace |
| Storage | Same 1,258,291,200-byte growth cap; total `/tmp/p2p-vpn-*` below 10 GiB |

Use `multi-network-resource-controls` through the isolated wrapper, with the
cached artifacts and `/tmp/p2p-vpn-a` as `TMPDIR`. The resource helper is supplied
by `P2P_VPN_ANDROID_RESOURCE_CONTROLS` in the Nix launcher.

## Admission Rules

- No process identity change or decreasing CPU tick counters across a window.
- Process boundary elapsed time must be 60 to 70 seconds, using each system's uptime.
- Every on-window requires all 60 process samples and all 12 runtime samples.
- Process gaps: 1 to 1.5 seconds; runtime gaps: 4.5 to 5.5 seconds.
- Each runtime observation must finish within two seconds and retain both running network IDs.
- Retain failures and partial samples; do not lengthen deadlines or replace failed windows silently.

## Interpretation

- Compute CPU as tick deltas divided by the matching uptime delta and clock tick rate.
- Report app and emulator costs separately; emulator CPU includes Android/ADB work.
- Boundary diagnostics occur outside the nominal window but process endpoint spans include small command gaps.
- RSS/PSS are not allocation ownership evidence. Leader context switches are not all-thread wakeups.
- Four windows in one boot give a paired order control, not independent boot replicates.
- Review measured interference before accepting collector timing for sustained S7.

## Results

The second attempt completes all four windows with stable process identities.
Both clock tick rates are 100 Hz; each process endpoint span is 60.04 seconds.
CPU below is percent of one core, not percent of the whole machine.

| Window | Collector | App CPU | Emulator CPU | App RSS Before/After, KiB | App FDs Before/After |
| --- | --- | --- | --- | --- | --- |
| 1 | Off | 1.716% | 4.963% | 206664 / 204908 | 124 / 124 |
| 2 | On | 2.082% | 7.462% | 207628 / 207572 | 124 / 122 |
| 3 | On | 2.149% | 7.578% | 207284 / 207696 | 122 / 124 |
| 4 | Off | 1.682% | 4.747% | 207012 / 206876 | 124 / 124 |

Observed on/off mean differences are approximately **0.416 percentage points**
for the app and **2.665 points** for the emulator. This is material observer cost,
not a correction factor to subtract from subsequent workloads.

- App thread count stays at 30 across every endpoint.
- Every boundary diagnostic reports two connected peers and zero queued packets.
- Boundary PSS spans 84,258 to 86,595 KiB; this is not allocation attribution.
- Both on-windows contain all 60 process and 12 runtime observations.
- Maximum process gap is 1.02 seconds; maximum runtime gap is 5.010 seconds.
- Maximum observation duration is 10 ms for process data and 80 ms for runtime/diagnostic data.

[Portable endpoint data and raw-file hashes](android-resource-control-results.json)
retain the CPU inputs and sample quality. The full raw directory is
`/tmp/p2p-vpn-android-resource-controls-2`, occupying 3,116 KiB after cleanup.

### Failed Attempt and Background Admission

Attempt 1 stopped before any control window: power was `Dozing`, still changing,
and the HAL interactive flag was true. The harness had checked immediately after
SLEEP instead of after the planned warmup. No runtime defect is claimed.

The retry checks after the same 30-second warmup. Admission requires Asleep or
Dozing, `mWakefulnessChanging=false` and `mHalInteractiveModeEnabled=false`.
The actual passing capture is Asleep. No timeout or measurement window was extended.

| Capture | Evidence SHA-256 |
| --- | --- |
| Early power-state failure | `ba15978726bab149e95803370d0c10bb6019ba932b61adf6db2586d83ce78238` |
| Four-window pass | `44b3dda9bcc24c2771e5e572269f09d670d0fa1561fdf8d092e9725d12e7132a` |
| Captured helper source | `33b4c5e915bdae99af3d415e7df2309f7024f64fa2c6114ce96805c2f0c8db41` |

Both attempts report complete harness cleanup. The owned temporary root retains
72 KiB of tool state. Storage before attempts was 9,123,284 and 9,125,056 KiB.
No application, emulator or fixture build ran during the observations.

### Validation and Limits

Power-state tests reject transitional, interactive and missing states. Resource
sample tests reject mismatched IDs, failed phases, missing queue/path counters and
missing PSS. All captured native rows also satisfy the tightened counter checks.

The helper was tightened after capture to reject missing required counters and
diagnostic schema versions; workload timing and collected data are unchanged.
The failed initial synthetic power test reread a nonseekable stream; the parser
now scans once, matching both files and streams.

This is one emulator boot, not independent replications or physical battery data.
Protocol background work can vary over time; host-side jq/shell CPU is not isolated
from other host activity. S7 idle/load and independent network transitions remain open.

ShellCheck, new-script formatting, negative/positive resource tests and all raw
hash checks pass. The Nix structure derivation evaluates offline; its full legacy
matrix was not rerun. No production code or native build changed in this step.

## Sustained Window Support

`resource_control_window` now accepts an optional third argument of `60` or `300`
seconds. Its default remains 60, preserving the recorded control workload.
The 300-second option is preparation for S7, not evidence that S7 has run.

| Contract | 60 Seconds | 300 Seconds |
| --- | --- | --- |
| Process samples when on | 60 | 300 |
| Runtime samples when on | 12 | 60 |
| Sampler timeout | 75 seconds | 315 seconds |
| Endpoint elapsed bound | 60 to 70 seconds | 300 to 310 seconds |

Cadence and process identity checks are unchanged. Invalid durations and modes
are rejected before collection. Synthetic clock tests verify off-window timing
and metadata for both durations; these tests do not sleep or run an emulator.

The first mock collector test failed because its generated command lacked a
line continuation. The corrected fixture passes. Live 300-second sampling and
fixed-rate traffic remain the next verification gate.
