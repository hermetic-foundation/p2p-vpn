# Resource Setup Migration Investigation

## Reproduction

| Field | Evidence |
| --- | --- |
| Attempt | `/tmp/p2p-vpn-android-load-smoke-1` |
| Baseline | `444f3823` plus pending load harness |
| APK SHA-256 | `ea03c922be28faf3043c540c69dd898ce1b5f7206528ffbafc55c6f5b8527f46` |
| Evidence SHA-256 | `4b8299f2b536a8dca537968b7b50403c3bfe0bd67c71abfbc5df05c2ff87b976` |
| Result | Failed before any resource/load window |

- Before restart: one valid alpha profile, with peer ID and dual-stack addresses.
- After restart: automation returned `ok:false`, `error:automation_internal_error`, no snapshot.
- The 60-second migration assertion expired; no extended deadline or runtime rescue.
- Emulator, fixtures and private state cleanup passed. Diagnostic-report redaction was false.

## Interpretation

The failure does not demonstrate a changed identity: no post-restart identity was
read successfully. It reproduces the setup failure observed before sustained idle,
but the underlying exception remains unknown. A later successful run is not a fix.

## Diagnostic Change

- Debug receiver logs exception class and at most four code locations under `P2pVpnAutomation`.
- Output is capped at 1024 characters; messages, causes, filenames and arguments are excluded.
- The automation response contract and failure assertions are unchanged.
- Release sources and native code are unchanged.

## Verification

- Offline Android debug unit tests, lint, APK and instrumentation APK assembly passed.
- Two diagnostic tests cover privacy exclusions, frame count and output length.
- Instrumented APK: `5f7dd26c6079c673cf46eea04d5a2e83e70265b3c33b4bc1796d81b77077f6a7`.
- Build log: `/tmp/p2p-vpn-android-automation-failure-build.log`.
- Pre-build storage audit: 9,135,032 KiB across `/tmp/p2p-vpn-*`, below 10 GiB.

## Next Capture

Use the instrumented APK and include `P2pVpnAutomation:E` in the bounded migration
failure log capture. Preserve the original watchdog and workload limits. This is
diagnostic preparation, not migration or sustained-load acceptance.

## Instrumented Follow-up

- Attempt `/tmp/p2p-vpn-android-load-smoke-2` reproduced the failure.
- Fifteen retained log entries identify `android.app.BackgroundServiceStartNotAllowedException`.
- The error originates in `ContextImpl.startServiceCommon`; status has no service snapshot.
- The debug status path calls `enqueueService("ensure")` when the snapshot is absent.
- Emulator and fixture cleanup passed; diagnostic-report redaction remained false.

The harness checks activity presence, not resumed visibility. The cached launcher
does not explicitly dismiss keyguard. Activity/background state needs direct
evidence before changing startup behavior; keyguard is only a hypothesis.

Attempt 3 added bounded activity/power capture on failure but passed setup without
a startup fix. It therefore did not capture the failing activity state. See
[load results](android-sustained-load.md); the intermittent setup issue remains open.
