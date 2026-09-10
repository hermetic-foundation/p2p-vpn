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

## Sustained Attempt 1 Manifest

- Baseline: `0067e3d3`; same instrumented APK and fixture hashes as compatibility attempt 3.
- Output: `/tmp/p2p-vpn-android-sustained-load-1`.
- Phases: 300 seconds idle, 300 seconds load, 60 seconds drain, without restarting the app.
- Four load legs, 15000 requests each; unchanged traffic and sample acceptance limits.
- Inner watchdog 1000 seconds plus 15-second kill grace; outer 1040 plus 20 seconds.
- Pre-run storage: 9,139,224 KiB; no emulator, fixture or build process running.
- No manual recovery or deadline extension. Preserve failures before any retry.

## Sustained Attempt 1 Results

All three phases passed. [Portable results](android-sustained-load-results.json)
retain endpoint counters, aggregate ranges, traffic summaries and raw-file hashes.
Evidence SHA-256: `6fc3763f668c0b0e52d7606b87b1c5a336b74746750dc68186ffe14015a6e0a4`.

| Measurement | Idle | Load | Drain |
| --- | --- | --- | --- |
| App endpoint duration, seconds | 303.17 | 308.70 | 60.03 |
| App CPU, percent of one core | 2.12 | 90.99 | 1.98 |
| Emulator CPU, percent of one core | 7.58 | 181.75 | 7.30 |
| Process / runtime samples | 300 / 60 | 300 / 60 | 60 / 12 |
| RSS range, KiB | 205596-208628 | 205916-208820 | 206396-208924 |
| PSS range, KiB | 84509-86789 | 84708-87619 | 85725-87752 |
| Threads | 31 | 31 | 31 |
| Descriptor range | 121-124 | 121-123 | 121-123 |
| Sampled queued packets / bytes | 0 / 0 | 0 / 0 | 0 / 0 |

- All four traffic legs sent and received 15000 packets, with zero reported loss.
- Traffic durations were 308.33-308.66 seconds, within the frozen 299-310 second bounds.
- One QUIC-stream path and one TCP-stream path remained present in every runtime sample.
- App PID 2205 and start identity 1917 were unchanged across all phases.
- Both clocks report 100 Hz. CPU uses user-plus-system tick deltas over endpoint elapsed time.
- All six cleanup flags passed; no emulator or fixture process remained after completion.

CPU returned toward the idle baseline after traffic. RSS/PSS ranges overlap, but
these samples do not prove allocation release. Queue snapshots cannot exclude
short-lived between-sample backlog. High debug load CPU remains unattributed.

This is the first complete idle/load/drain sequence, not completed S7 acceptance.
Repeat capture, five independent network transitions, scheduling attribution and
the intermittent migration setup investigation remain outstanding.

## Thread-Attributed Repeat Manifest

This next capture retains the workload and deadlines above. Set
`P2P_VPN_ANDROID_RESOURCE_LOAD_DETAIL=threads` with `multi-network-resource-load`.
The default remains `process`; invalid values or other scenarios with `threads` fail.

| Setting | Value |
| --- | --- |
| Source | `fd380235` plus explicit sustained thread-sampling dispatch |
| Output | `/tmp/p2p-vpn-android-sustained-load-threads-1` |
| Phases | 300-second idle, 300-second load, 60-second drain |
| Thread collection | Existing bounded no-fork sampler; same mode in all three phases |
| Traffic | Four 15000-request legs; every reply required, no measured retries |
| Watchdogs | Inner 1000 + 15 seconds; outer 1040 + 20 seconds |
| APK / fixture | Same hashes as sustained attempt 1 |
| Setup correction | Explicit notification permission before initial activity launch |

- Record final source hashes and pre-run storage before provisioning; no builds during capture.
- Retain unchanged process cadence and endpoint limits, including duration plus ten seconds.
- Match thread identity by TID and start ticks; do not subtract inventories across reused IDs.
- Report CPU/context-switch deltas per stable thread and unaccounted thread churn explicitly.
- Use process counters for total CPU; thread scans are non-atomic and may omit short-lived work.
- Compare with [thread observer controls](android-thread-controls.md), without subtracting a fixed correction.

The permission fix changes setup relative to attempt 1. Selected transports must
also be compared before treating results as matched. Thread counts and context
switches are scheduling proxies, not physical wakeups or battery measurements.

### Review Status

- Notification setup cause and correction: [investigation](android-migration-resource-investigation.md).
- Independent instance transitions: [two five-cycle captures](android-resource-isolation-cycles.md) passed.
- Sustained load repetition and CPU/thread attribution: pending the capture above.
- This harness change modifies neither production runtime behavior nor the APK.

### Capture Preflight

- Published source: `06615d80`; pre-run storage 8,236,792 KiB.
- APK and fixture hashes match the manifest; no emulator, fixture or build process running.
- Resource helper SHA-256: `bb52a474a399bf4f9e72bc6d7d4540778ae235493ce092f762f5243ddb4d1261`.
- Process sampler SHA-256: `bdebe1c834a5f21cad746eb33c679625082755152b2fc50a566906ac2359b011`.

### Thread Attempt 1 Failure

Admission passed on the first measured batch. The idle phase failed its unchanged
310-second endpoint limit; load and drain did not run. All six cleanup flags
passed. No measurement was retried or deadline extended.

| Observation | Result |
| --- | --- |
| Process endpoint elapsed | 311.14 seconds |
| Process samples retained | 300 |
| First / last sample uptime | 107.41 / 418.47 seconds |
| Per-scan time | 20-40 ms; 9.52 seconds total |
| Sample-start gaps | 1.03-1.05 seconds |
| Evidence SHA-256 | `11c9e023bce3ff7056d67423befd2af1127b20394a3f23f7b43ec5b528bca2a5` |
| Process rows SHA-256 | `e0d2b64618469e18f1132ec010d499038dda2926717047da7ab4881d8698c921` |
| Window SHA-256 | `14c214bfa8cd21f58acc4bf57c3de5a62e8547c522cfa58db71583b30bb2e81d` |

The sampler slept one second after each scan, accumulating scan cost over the
window. This is a collector scheduling defect, not evidence of VPN overload.
The shorter thread-control windows did not exceed their endpoint allowance.

- Correction: thread mode waits until one second after the preceding sample start.
- Process-only behavior remains unchanged; scan overruns and invalid clocks fail explicitly.
- Tests cover scan-cost subtraction, second-boundary rollover, default live sampling and invalid clocks.
- Existing timing/count/traffic limits remain unchanged; a fresh sustained capture is still required.
- Do not relabel the retained failed capture as a successful idle or load result.

### Thread Attempt 2 Manifest

- Baseline `a71106db`; only thread sampling cadence changed from attempt 1.
- Sampler SHA-256: `f27f0f681ad234af45b0d1b28a240b3905c42529a240b327dc15351e891baa2d`.
- Output: `/tmp/p2p-vpn-android-sustained-load-threads-2`.
- Pre-run storage: 8,241,740 KiB; no emulator, fixture or build process running.
- Same APK, fixture, permission setup, 300/300/60 phases, traffic and 1000/1040-second watchdogs.
- All count, loss, cadence and endpoint bounds unchanged; retain failures without rescue.

### Thread Attempt 2 Results

All phases passed with all six cleanup flags true.
[Portable thread results](android-sustained-thread-results.json) retain process
endpoints, thread deltas, traffic summaries and 49 raw/source hashes.

| Measurement | Idle | Load | Drain |
| --- | --- | --- | --- |
| Endpoint elapsed, seconds | 301.59 | 308.46 | 60.03 |
| App CPU, percent of one core | 2.115 | 96.223 | 2.116 |
| Emulator CPU, percent of one core | 10.279 | 193.623 | 10.178 |
| Process / runtime rows | 300 / 60 | 300 / 60 | 60 / 12 |
| RSS range, KiB | 207032-210056 | 207140-210124 | 207572-210212 |
| PSS range, KiB | 84820-86876 | 84963-87940 | 86224-88108 |
| Descriptor range | 120-122 | 120-123 | 120-122 |
| Process CPU ticks between sampled endpoints | 638 | 29090 | 126 |
| Sum of stable-thread CPU tick deltas | 642 | 29085 | 119 |

- All four traffic legs sent and received 15000 packets; durations were 308.33-308.43 seconds.
- The app PID/start identity remained 2204/1918; all samples retained the same 30 thread identities.
- Every runtime sample showed two QUIC-stream overlay paths; private infrastructure count was zero or two.
- Sampled queues were empty. These snapshots do not exclude between-sample backlog.
- Thread scans took 20-40 ms idle/drain and 30-40 ms under load; corrected cadence passed unchanged limits.

#### Thread Attribution

| Thread ID / Start Tick | Idle CPU Ticks | Load CPU Ticks | Drain CPU Ticks |
| --- | --- | --- | --- |
| 2556 / 6494 | 157 | 13586 | 28 |
| 2557 / 6494 | 169 | 13606 | 33 |

These two threads account for 27192 of 29090 sampled load process ticks, about
93.5%. Their load voluntary context-switch deltas are 420353 and 418614.
This identifies where scheduling cost concentrates, not which functions cause it.

- Thread spans: 301.51 seconds idle, 301.78 load and 59.54 drain.
- Load thread sampling ends before the final ping replies; process endpoints span the complete traffic interval.
- Tick rounding and non-atomic scans explain why thread totals need not equal process totals exactly.
- Stable sampled inventories cannot exclude short-lived threads between observations.
- Context switches are scheduling proxies, not hardware wakeups or measured battery consumption.

#### Interpretation

CPU returned to the same idle level after load. The two complete sustained
captures both deliver all 60000 replies, but differ in transport mix, notification
setup and thread-collector mode. They are not a controlled QUIC-versus-TCP comparison.

- High debug-load CPU is reproducible; function-level attribution remains open.
- RSS/PSS overlap is not evidence of complete allocation release.
- Collector timing correction is verified on the sustained emulator workload.
- This closes neither the whole resource review nor release/platform acceptance.
