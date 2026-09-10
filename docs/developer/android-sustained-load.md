# Android Sustained Load

## Frozen Workloads

These manifests precede capture. Both use the existing two-network admission,
30-second background warmup and attributed resource collector. Neither proves
independent network transitions or physical battery behavior.

| Setting | Compatibility | Sustained |
| --- | --- | --- |
| Scenario | `multi-network-resource-load-smoke` | `multi-network-resource-load` |
| Phases | 60-second load | 300-second idle, 300-second load, 60-second drain |
| Requests per traffic leg | 3000 | 15000 |
| Total requests | 12000 | 60000 |
| Inner watchdog / kill grace | 440 / 15 seconds | 1000 / 15 seconds |
| Outer watchdog / kill grace | 480 / 20 seconds | 1040 / 20 seconds |

### Common Controls

- Source baseline: `444f3823`, plus the load harness recorded with the capture.
- Same cached debug APK: `ea03c922be28faf3043c540c69dd898ce1b5f7206528ffbafc55c6f5b8527f46`.
- Same cached Linux fixture: `4b1bcd69a410f9a830e813d9457aa0a55a8a507d762c208aad42a1092ab8f8fa`.
- Owned API 35 x86_64 emulator and private fixtures inside the no-public-route wrapper.
- Four Android-originated streams: alpha/beta, IPv4/IPv6; echo replies exercise the reverse path.
- Each ping requests a 20 ms interval, 512-byte payload and one-second reply wait.
- Keep normal discovery and recovery enabled. No route rescue or failed-window deadline extension.
- Total `/tmp/p2p-vpn-*` below 10 GiB; runtime growth cap remains 1,258,291,200 bytes.
- Reuse `/tmp/p2p-vpn-a` for temporary state. No builds or downloads during measurement.

## Traffic Evidence

Each leg retains raw quiet-ping output, parsed transmitted/received/loss counts,
exit status and host monotonic start/end timestamps. Missing, malformed or multiple
summaries fail. Requested count alone is not accepted as actual offered work.

- Require actual sent count equal to requested count, all replies, and zero reported loss.
- Require each leg's elapsed time between duration minus one second and duration plus ten seconds.
- Use a duration-plus-15-second command timeout, with two-second kill grace.
- Retain failures and partial evidence; do not retry a traffic batch to erase packet loss.
- Record actual transport counts. Interpret idle/load comparisons as matched only when topology is comparable.

The interval is a requested pacing rate, not proof of exact packet spacing.
This workload is approximately 1.8 Mbit/s of inner request/reply traffic across
four legs, before outer encapsulation. It is not a saturation benchmark.

## Collection

All phases use the previously tested process/native cadence and duration bounds.
Idle and drain have no deliberate ping load. Load workers are reaped before
the next phase; cleanup also terminates owned worker commands on failure.

The collector's measured observer cost remains part of reported results. App CPU
excludes the separate ping processes; emulator CPU includes the guest workload.
Compatibility success is not sustained-load acceptance.

## Instrumented Compatibility Attempt

- Attempt 2 uses baseline `aa14357a` plus the pending load harness.
- APK: `5f7dd26c6079c673cf46eea04d5a2e83e70265b3c33b4bc1796d81b77077f6a7`.
- Only debug exception diagnostics changed; native library and workload are unchanged.
- Output: `/tmp/p2p-vpn-android-load-smoke-2`; use the compatibility watchdog above.
- Pre-run storage: 9,135,040 KiB. No build or other emulator is running.
- Attempt 1 failed during setup; see [migration investigation](android-migration-resource-investigation.md).
- Attempt 2 reproduced `BackgroundServiceStartNotAllowedException` before load.
- Attempt 3 uses the same APK/workload/watchdog and adds bounded activity/power failure evidence.
- Attempt 3 output: `/tmp/p2p-vpn-android-load-smoke-3`; no production change or rescue.

## Compatibility Results

| Attempt | Result | Evidence SHA-256 |
| --- | --- | --- |
| 2 | Background service start refused; no load | `057d3131cafd7160fcd8081624255a211d5c98342f0010cdd96d6ac0f2197bd4` |
| 3 | Load and collection passed | `0c82e046e0f4b508f7f5764b630b554446bad1659ebbd95e2a9e05a833bb268e` |

| Attempt 3 Measurement | Result |
| --- | --- |
| Four traffic legs | Each 3000 sent, 3000 received, zero loss |
| Leg duration | 61.67-61.71 seconds |
| Process / runtime observations | 60 / 12; cadence checks passed |
| App / emulator clock | 100 / 100 ticks per second |
| App endpoint interval | 61.75 seconds |
| App CPU | 91.97% of one core |
| Emulator CPU | 180.99% of one core |
| App RSS endpoints | 206404 / 207160 KiB |
| Sampled PSS range | 84695-86592 KiB |
| App threads / descriptors, both endpoints | 31 / 123 |
| Sampled data paths | One QUIC stream and one TCP stream throughout |
| Infrastructure count | 0 or 2 private fixture peers; no public WAN |
| Cleanup | All six evidence flags true |

CPU includes debug code and collector overhead; emulator CPU includes guest ping
workers. The high cost remains unattributed. This short mixed-transport run is
not sustained-load, transport comparison, battery or production acceptance.

The successful setup does not resolve attempt 2. No startup correction was made.
Attempt 3 pre-run storage was 9,136,584 KiB. Full sustained phases and repetitions
remain outstanding.

## Harness Verification

- Ping parser and synthetic 60/300-second window tests passed.
- ShellCheck passed for the helper, harness and ping tests; helper/test formatting passed.
- Cached Nix structure derivation evaluated offline; its full shell matrix was not rerun.
- Review added rejection of a valid summary accompanied by a malformed extra summary.
- Attempt 3 raw traffic files also pass the stricter parser; runtime commands are unchanged.
