# Android Thread Collector Controls

## Frozen Manifest

| Setting | Value |
| --- | --- |
| Baseline | `65690dcf` plus dedicated thread-control scenario |
| APK SHA-256 | `5f7dd26c6079c673cf46eea04d5a2e83e70265b3c33b4bc1796d81b77077f6a7` |
| Fixture SHA-256 | `4b1bcd69a410f9a830e813d9457aa0a55a8a507d762c208aad42a1092ab8f8fa` |
| Scenario | `multi-network-resource-thread-controls` |
| Platform | Owned cached API 35 x86_64 emulator, private isolation wrapper |
| Setup | Two independent networks admitted; HOME/SLEEP and 30-second warmup |
| Windows | Process / threads / threads / process; each 60 seconds, same app process |
| Inner / outer watchdog | 600 / 640 seconds; 15 / 20 seconds kill grace |
| Runtime growth cap | 1,258,291,200 bytes |
| Output | `/tmp/p2p-vpn-android-thread-controls-1` |

## Controlled Difference

- Process samples every second; runtime/diagnostic samples every five seconds in all windows.
- Only the two middle windows add `--threads` to recurring process sampling.
- App and emulator endpoint snapshots remain process-only in all windows.
- No deliberate traffic after admission; normal protocol/discovery work remains enabled.
- No builds, manual route rescue, deadline extensions or public/physical-device access.

## Acceptance

- Existing fixed window, process identity and sample cadence assertions remain enabled.
- Each window requires 60 process and 12 runtime observations.
- Thread windows require 1-256 distinct listed/observed TIDs, no skips and all required counters.
- Reject zero start identities, duplicate TIDs, missing counters or incomplete scans.
- Preserve raw samples, source hashes, endpoints and cleanup evidence before interpretation.
- Treat background path changes or host interference as comparison limitations, not collector effects.

## Interpretation

Compare the mean middle-window CPU with the two surrounding process-only windows.
This estimates observer contribution in one boot; do not subtract it as an exact
correction or equate context switches with hardware wakeups or energy consumption.

## Attempt 1 Setup Failure

- No control windows ran; the unchanged 60-second migration assertion failed.
- Debug status repeatedly hit `BackgroundServiceStartNotAllowedException`.
- Activity capture: permission controller resumed; app only STARTED, keyguard false, screen awake.
- `MainActivity.onCreate` requests notification permission, which the resource harness had not handled.
- Emulator, fixtures and private-state cleanup passed; diagnostic-report redaction was false.
- Pre-run storage: 9,145,076 KiB.

## Corrected Admission Manifest

- Resource scenarios now grant `POST_NOTIFICATIONS` before launching the selected APK.
- Permission grant failure aborts setup. Non-resource workflows remain unchanged.
- No app/service/protocol change, manual runtime rescue or migration deadline extension.
- Output: `/tmp/p2p-vpn-android-resource-notification-admission-1`.
- Scenario: `multi-network-resource-admission`; same APK and fixture.
- Inner/outer watchdog: 240/280 seconds, with 15/20-second kill grace; unchanged storage growth cap.
- Acceptance: migration, both networks admitted, eight bidirectional dual-stack 5/5 traffic checks.

Explicit notification permission changes the test's starting state. Future resource
comparisons must record this prerequisite; historical measurements are retained,
not silently relabeled as permission-matched captures.

## Corrected Admission Results

- Pre-run storage: 9,146,628 KiB; no build or concurrent emulator.
- Migration, two-network pairing and all eight 5/5 traffic checks passed.
- All six cleanup flags passed. No thread-control windows ran in this admission test.
- Thread-control capture remains outstanding; no observer-cost estimate is claimed yet.

| Artifact | SHA-256 |
| --- | --- |
| Failed control setup evidence | `18fb01a226e95fb934c56ccaaabf51eea7b844fea76cf1bd68f844633fb2abf6` |
| Failed setup activity | `b5aefbce0a185e6810e9bb045485bbeaa725457faaefdab959b5ec00e64d649b` |
| Failed setup power | `f88a56f38720e85e37ae3c2d65f52c0c003a7c2e7890d8fc790048a3487d70d4` |
| Failed setup exception log | `d12e9dccff702563b40e396a18ec5c282298bb66d08f15756c4a92b3440dfee3` |
| Corrected admission evidence | `32f5ff3b5a4354ac60aeeb3adb77a48604c893a4f4bbe41b4c2a950a4917e03a` |

Local tests cover permission scoping/failure propagation and thread-sample quality
rejection. ShellCheck and focused formatting pass. The Nix structure derivation
evaluates offline; its full matrix was not rerun. No production code was rebuilt.
