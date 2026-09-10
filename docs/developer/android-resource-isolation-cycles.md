# Android Resource Isolation Cycles

## Frozen Manifest

| Setting | Value |
| --- | --- |
| Baseline | `2531b141` plus isolation workload and state-validation tests |
| Scenario | `multi-network-resource-isolation` |
| APK SHA-256 | `5f7dd26c6079c673cf46eea04d5a2e83e70265b3c33b4bc1796d81b77077f6a7` |
| Fixture SHA-256 | `4b1bcd69a410f9a830e813d9457aa0a55a8a507d762c208aad42a1092ab8f8fa` |
| Platform | Owned cached API 35 x86_64 emulator in private isolation wrapper |
| Setup | Explicit notification permission, two-network admission, 30-second background warmup |
| Work | Five alpha disable/enable cycles, then 60-second settling |
| Per-transition limits | 120 disable polls, 180 enable polls, one-second spacing |
| Overall watchdog | Inner 850 seconds plus 15-second kill grace; outer 890 plus 20 |
| Output | `/tmp/p2p-vpn-android-resource-isolation-1` |
| Runtime growth cap | 1,258,291,200 bytes; total project temporary storage below 10 GiB |

The overall deadline can expire before all per-transition allowances are consumed.
It is not extended on failure. No builds, physical devices, public routing or
manual runtime recovery participate.

## Cycle Checks

1. Disable alpha through normal debug-authorized network controls; beta must remain enabled/running.
2. Require one measured 5/5 batch for beta in both directions and address families.
3. Reject one inbound IPv4 and IPv6 alpha probe, each with sent=1, received=0 and non-success result.
4. Require outbound alpha probes to fail with zero replies or explicit network-unreachable output.
5. Re-enable alpha; retain bounded readiness convergence, then require one measured concurrent 5/5 batch per leg.
6. Preserve both networks' IDs, hostnames, peer IDs and addresses; retain failed batch output.

Traffic checks occur after state transitions. They do not prove zero packet loss
during every instant of transition. The initial continuous-running assumption
was corrected below: beta remains configured enabled while the shared runtime restarts.

## Resource Collection

- Process counters every second; status, native counters and diagnostic resources every five seconds.
- No thread-scanning mode or extra collector during settling; observer cost remains included.
- Sampling spans all cycles and settling, with before/after process and emulator endpoints.
- Maximum 900 process observations and 180 runtime observations; normal completion explicitly stops collectors.
- Require stable process identity, process gaps 1-1.5 seconds and runtime gaps 4.5-5.5 seconds.
- Runtime observations must finish within two seconds and include beta's native path/queue counters.
- Final audit requires beta enabled/running, unchanged identities and complete first/last process coverage.
- Counters and memory must be interpreted after capture; overlapping RSS is not allocation-release proof.

## Scope

This tests VPN network-instance isolation, not a cellular, hotspot or VPN-underlay
transition. Repetition and the other Sustained Resource Review requirements remain
separate from a single five-cycle result.

## Attempt 1 Failure

- Pre-run storage: 9,152,508 KiB.
- Admission and initial single-attempt traffic passed. The first disable cycle failed.
- Beta's measured inbound IPv4 batch sent five packets and received four.
- Alpha was disabled and beta reported running; shared runtime generation changed from 4 to 5.
- Process collector retained 11 rows; native observer rejected its first row and retained none.
- All six cleanup flags passed; no emulator or fixture remained.
- No measured batch was retried. Four later cycles and settling did not run.

| Captured Artifact | SHA-256 |
| --- | --- |
| Evidence | `e19cfb2dc79a70e21d43ea42ff8739afa3d38838ed696da0e92c199be3625b2d` |
| Disabled state | `ce2dac1c663529215a0571de70f86d1fed67edb568e72eeb1a8ce14a62cfad3f` |
| Failed beta batch | `9fa7b8854a82299e002d691e0880cd7e2a6290dd9467aedc7d4357d33667d183` |
| Process rows | `d670496afb2479b4cb911e5d06f2e9c09ab63644feff30b831321fe55b0ea928` |
| Resource helper | `471ecbbc64759b277cbaa61488830f50c526015324ed850821fdc5a7c05844cc` |
| E2E harness | `5f19a7f1e4a7158ea099ce345e1b110dad16ab5c789244282e50671e8cda2635` |

## Contract Reconciliation

`setNetworkEnabled` calls `suspendConnectionForNetworkChange`, which stops the
shared native runtime before `reconcileEnabledNetworks` reconnects it. The existing
lifecycle scenario performs traffic-readiness checks after disabling alpha.

- The new workload incorrectly assumed beta never leaves the running phase during reconfiguration.
- Native status can legitimately return stopped with no networks while the shared runtime is absent.
- The rejected observer payload was not retained, so its exact missing field remains unknown.
- The 4/5 batch is evidence of loss at this measurement point, not proof of a new transport regression.
- The next harness revision must capture transitional state and bound recovery before steady-state measurement.
- Retain this attempt; do not relabel it as a pass or relax the measured batch's 5/5 requirement.

At this point the cycle harness required correction. No production behavior was
changed, and attempt 1 does not establish five-cycle isolation or resource stability.

## Attempt 2 Manifest

- Same APK, fixture, five cycles, final settling, sampling cadence and 850/890-second watchdogs.
- Output: `/tmp/p2p-vpn-android-resource-isolation-2`; baseline remains `2531b141` plus revised harness.
- State/readiness checks now precede beta's measured batch, matching the existing lifecycle workflow.
- Measured batches remain single-attempt and require 5/5; failed attempt 1 is retained unchanged.
- Runtime stopped/starting states are recorded during shared-runtime reconfiguration, not replaced by zero counters.
- Failed native states, malformed data and missing counters in running beta still fail, with rejected rows retained.
- Beta must remain configured enabled with unchanged identity; continuous native running is not assumed.
- Disable/enable event timestamps bound state and traffic-readiness recovery for later analysis.

This changes the observation contract to match existing shared-runtime behavior;
it does not modify production code, prolong deadlines or claim uninterrupted traffic.

## Attempt 2 Results

All five cycles and final sampling checks passed. [Portable results](android-resource-isolation-results.json)
retain resource endpoints, transition timings, events and raw-file hashes.
Pre-run storage: 9,154,752 KiB; all six cleanup flags passed with no owned processes remaining.

| Measurement | Result |
| --- | --- |
| Collection interval | 280.03 seconds |
| Process / runtime rows | 277 / 56 |
| App / emulator CPU | 2.985% / 10.370% of one core |
| Final 60 process samples | 2.141% app CPU over 59.78 seconds |
| App RSS endpoints | 205752 / 206144 KiB |
| Sampled RSS / PSS ranges | 199616-208396 / 78794-86375 KiB |
| Sampled thread counts | 24, 25, 30, 31 |
| Sampled descriptor range | 106-124 |
| Native states | 54 running, one starting, one stopped |
| Shared runtime generations | 4 through 14 |
| Sampled diagnostic queue packets / bytes | 0 / 0 |
| Verified disable readiness | 11.446-11.605 seconds |
| Verified enable readiness | 11.568-11.754 seconds |

- Thirty Linux and thirty Android measured batches each passed 5/5, without measurement retries.
- Ten inbound disabled-alpha probes sent one packet each and received none.
- Ten outbound disabled-alpha probes failed with zero replies or network-unreachable evidence.
- The same app PID/start identity persisted, as did both network identities and beta's enabled setting.
- The final twelve runtime samples show two connected TCP-stream paths.

Readiness durations include polling and traffic checks, not exact outage time.
Counters are absent during native restart where appropriate; diagnostic queue
snapshots and RSS/PSS do not prove absence of transient backlog or retained allocations.

## Verification

- State, identity, bounded polling and observation-quality regression tests passed.
- Tests distinguish valid stopped/starting states from failed or malformed native observations.
- Rejected observations are retained; missing running counters still fail.
- Resource-window and single-attempt traffic tests passed; ShellCheck and focused formatting passed.
- Nix structure evaluated offline; the full structure matrix was not rerun. No Java/native rebuild was needed.
- All 129 recorded raw/source hashes were rechecked after capture.
- At attempt 2, repetition, sustained-load/thread attribution and final review audit remained outstanding.

## Attempt 3 Repeat Manifest

- Baseline `08ecf113`; APK, fixture and capture code unchanged from successful attempt 2.
- Same five cycles, readiness/measurement separation, 60-second settling and 850/890-second watchdogs.
- Output: `/tmp/p2p-vpn-android-resource-isolation-3`.
- Pre-run storage: 9,158,984 KiB; no other emulator, fixture or build process running.
- Use a fresh owned emulator and fixture identities; record actual selected transports.
- No measurement retries, manual runtime rescue, public route or physical-device access.

## Attempt 3 Repeat Results

All five cycles and settling passed with unchanged capture code.
[Portable repeat results](android-resource-isolation-repeat-results.json) retain
endpoints, events and 130 raw/source hashes, including the evidence file.

| Measurement | Attempt 2 | Attempt 3 |
| --- | --- | --- |
| Duration | 280.03 s | 280.04 s |
| Process / runtime rows | 277 / 56 | 277 / 56 |
| App CPU, one core | 2.985% | 2.985% |
| Emulator CPU, one core | 10.370% | 10.316% |
| Final 60 samples app CPU | 2.141% | 2.158% |
| RSS endpoints, KiB | 205752 / 206144 | 206128 / 206652 |
| RSS range, KiB | 199616-208396 | 202924-208812 |
| PSS range, KiB | 78794-86375 | 78860-86951 |
| Descriptor range | 106-124 | 107-124 |
| Native running / starting / stopped rows | 54 / 1 / 1 | 54 / 1 / 1 |
| Disable readiness range | 11.446-11.605 s | 11.569-11.586 s |
| Enable readiness range | 11.568-11.754 s | 11.540-11.732 s |

- Repeat: all 60 measured batches passed 5/5; all 20 disabled-network probes received zero replies.
- Process identity stayed fixed; shared native generations advanced from 4 to 14.
- Final twelve runtime samples show two connected TCP-stream overlay paths and two private infrastructure peers.
- Sampled diagnostic queues remained empty; all six cleanup flags passed.
- Both runs settle near measured idle CPU. Neither proves leak freedom or uninterrupted sibling traffic.

This closes the repeated bounded Android network-instance cycle measurement.
Sustained-load CPU attribution, transport/runtime allocation attribution and the
full requirement audit remain open. No production behavior changed for this repeat.
