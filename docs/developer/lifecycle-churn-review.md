# Lifecycle Churn Resource Review

## Status

S5 fixture validated. The first full capture failed on cycle three; both successful
ten-cycle captures remain pending. See [connection retirement investigation](churn-connection-retirement.md).
Fixture validation below used production runtime `11416a8d`.

## Frozen Capture Manifest

| Setting | Value |
| --- | --- |
| Topology | Existing two-node direct UDP namespaces; no Internet route |
| Processes | One daemon pair throughout; two Tokio workers per node |
| Warmup | 30 seconds before the first outage |
| Cycles | Ten; no daemon restart or manual path repair |
| Outage | B's `veth-b` down; failed one-second underlay ping; 30-second resource window |
| Demotion gate | Both nodes report zero healthy direct UDP paths before restoration |
| Restoration | Same link up, same addresses; autonomous overlay recovery |
| Recovery deadline | Existing S2 common 60 seconds, both directions successful in the same round |
| Recovery traffic | Existing strict 5/5 pings each direction after every successful recovery |
| Recovery observation | Independent 40-second OS/runtime window alongside recovery checks |
| Settling | 60 seconds after cycle ten, then strict 5/5 pings both directions |
| Sampling | OS one second; runtime status five seconds; diagnostic logs ten seconds |
| Repetition | Two fresh ten-cycle captures; identical executable hash |
| Watchdogs | Internal 890 seconds; external 900 seconds; overrides prohibited |
| Budgets | Combined reports at most 8 MiB; combined node logs at most 2 MiB |

Reports are compacted without dropping fields after each observation phase.
The successful initial smoke used five-second diagnostics and pretty JSON;
its size prompted this output adjustment before either full capture.

Diagnostic cadence is reported in each phase. Runtime status sampling remains
five seconds; no runtime timer, probe, resource limit or assertion changed.

Recovery sampling starts when its scoped worker is launched. The recovery report
records elapsed time from link restoration separately. Checks that finish before
40 seconds leave an idle remainder. If checks take longer, join them after sampling;
their original 60-second recovery deadline remains unchanged.

The runtime probes every five seconds and expires unanswered probes after twelve
seconds. Thirty-second outages and the demotion gate avoid counting a brief link
interruption that leaves the original path healthy as lifecycle recovery.

The fixed global watchdog also bounds setup, observation and strict-ping overhead.
Reaching it is a failed capture, not permission to increase the budget.

## Commands

Build the namespace target offline first. Record its executable SHA-256 and
fixture revision. Do not run builds during either capture.

```sh
timeout --signal=TERM --kill-after=10s 900 env \
  -u P2P_VPN_REVIEW_CHURN_SMOKE \
  -u P2P_VPN_TUN_E2E_IDLE_SECONDS \
  -u P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS \
  -u P2P_VPN_TUN_E2E_WAIT_SCALE \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  "$NAMESPACE_TEST_BINARY" \
  tun_namespace_measures_lifecycle_churn_resources \
  --ignored --exact --nocapture --test-threads=1
```

Smoke: set `P2P_VPN_REVIEW_CHURN_SMOKE=1` for one cycle, retaining the same phase
durations and checks. Use a 250-second external watchdog; internal is 240 seconds.
Smoke output is explicitly ineligible for ten-cycle evidence.

## Evidence and Decisions

- Retain outage, recovery and recovery-observation JSON independently per cycle.
- Require complete runtime samples; missing observations are not zero activity.
- Compare original PID/start identities after every cycle and final settling.
- Summarize CPU, RSS, descriptors, threads, queue and retry counters across cycles.
- Apply the parent review's growth triggers; RSS alone cannot prove a leak.
- Retain partial evidence and stop after a failed recovery, identity or budget gate.

`lifecycle-churn.json` remains incomplete until all cycles, settling and final
traffic checks succeed. Namespace child ownership supplies process teardown;
verify no matching processes remain after each run.

## Scope Limits

This is repeated local link loss, not discovery after a WAN address change.
Default public bootstrap candidates may fail locally; distinguish those attempts
from the one configured overlay peer.

The recovery worker adds observer activity; report it rather than subtracting an
assumed cost. This fixture does not replace multi-network isolation, whole-runtime
allocation attribution or Android background measurements.

## Initial Harness Failure

The first one-cycle smoke exited 101 after 77.66 seconds. The demotion gate read
`daemon_after.state`; healthy-path counters are exposed in `daemon_after.status`.
Both actual status responses reported zero healthy UDP paths.

The corrected gate reads status, with a regression matching that response shape.
No runtime, duration, recovery deadline or demotion assertion was weakened.

| Evidence | Value |
| --- | --- |
| Outer log | `/tmp/p2p-vpn-lifecycle-churn-smoke-1.log` |
| Outer SHA-256 | `fd5b26971806535d4fe0fd86604a084a6506d4df1068517edf0e3ed820789ee4` |
| Artifact directory | `/tmp/p2p-vpn-tun_namespace_measures_lifecycle_churn_resources-1.c20ee45c7c7eac00` |
| Outage SHA-256 | `403cd5614be3f44745d481e011b3e0ab1dd3d9137dc930dee38f0ccb0db5cd8e` |
| Failed executable SHA-256 | `6776ffb8c5c44bdae2fa8e372f551051adaceea8af240559119faab6a9e2b1ec` |
| Cleanup | Outer runner terminal; no matching namespace test process remains |

This failure is test-tool evidence, not a production recovery regression or a
successful S5 repetition. The retained outage report precedes the failed gate.

## Intermediate Smoke and Compatibility

The corrected five-second-diagnostic smoke passed in 188.78 seconds. Recovery
took 2.2547 seconds; per-cycle and post-settling traffic passed 5/5 both ways.
All runtime series were complete; process identities remained unchanged.

This one-cycle result has `proof_eligible=false`. It validates the lifecycle
checks, not the ten-cycle workload or the final ten-second diagnostic cadence.

| Artifact | Value |
| --- | --- |
| Executable SHA-256 | `4521d37461f2c45e5d1d5599372d067360d49e33f8dbd91ac12e750d61acf140` |
| Directory | `/tmp/p2p-vpn-tun_namespace_measures_lifecycle_churn_resources-1.9e67888b82d5ae14` |
| Outer log | `/tmp/p2p-vpn-lifecycle-churn-smoke-2.log` |
| Outer SHA-256 | `29a6aebcf4e9a8bd68b519bc56f0779347df1ad565a2ac0c570dc9635bd81589` |
| Summary SHA-256 | `b043e767cfa8de5fe9fc628ea13e50e1a1ab777f0f19ed1b9980377d1a774232` |
| Combined JSON / logs | 1,323,039 / 644,783 bytes |
| S2 compatibility | Existing ten-second unavailable smoke passed in 66.08 seconds |
| S2 log | `/tmp/p2p-vpn-lifecycle-churn-unavailable-regression.log` |

S2 retained its report names, original recovery algorithm, deadlines and strict
traffic checks. It used the same intermediate executable. Both runners exited
successfully; no churn fixture process remained when compatibility began.

## Final Fixture Validation

The ten-second-diagnostic smoke passed in 213.98 seconds. It recovered in
23.9948 seconds without rescue; per-cycle and post-settling traffic passed 5/5
both ways. All original daemon PID/start identities were unchanged.

| Phase | OS Rows, Both Nodes | Runtime Rows, Both Nodes | Complete |
| --- | ---: | ---: | --- |
| Outage, 30 seconds | 62 | 12 | Yes |
| Recovery, 40 seconds | 82 | 16 | Yes |
| Settling, 60 seconds | 122 | 24 | Yes |

All phases report ten-second diagnostics. Combined JSON is 1,039,805 bytes;
combined node logs are 393,939 bytes. The runner exited zero and no matching
namespace processes remained. `proof_eligible=false`: this is one cycle only.

| Validation | Result |
| --- | --- |
| Offline locked workspace | 1505 passed; 40 opt-in tests ignored |
| Namespace units | 58 passed; 26 opt-in tests ignored |
| Required Clippy groups | Workspace/all targets pass correctness, suspicious and perf; advisory warnings remain |
| Formatting | Cached rustfmt checks pass for all five affected Rust files |
| Nix source parity | Evaluated source-check script passes with cached Cargo outside the sandbox; packaged test files match |
| Android native build | Not repeated: no production/shared Android Rust source changed |
| Formal models | No repository Lean/TLA source located; this change adds test orchestration only |
| Storage | `/tmp/p2p-vpn-*` 8,720,508 KiB before final smoke; below 10 GiB |

Validation logs use `/tmp/p2p-vpn-lifecycle-churn-` followed by
`final-workspace.log`, `output-final-clippy.log`, `output-source-check.log`
and `final-smoke.log`. No build overlapped either successful smoke.

| Final Artifact | SHA-256 |
| --- | --- |
| Namespace executable | `31ac71768df4c6a24a052de47bff25daa43400141134dafde76c6b7ffc3542a5` |
| `churn-01-outage.json` | `ee33d08530c2541c4b6d0cd3fd6b8eabb1e8d4c11304263cdc47191a205b50fc` |
| `churn-01-recovery-sample.json` | `bb9bcb053bae48fa5b7b8eca268c261acdc8406cd20e6d321428c8b16e4cc752` |
| `churn-01-recovery.json` | `16a86c75c9d6a728e7e92431e3a6a4097fb66c3e00d6dce2207746ede299030a` |
| `churn-settle.json` | `48b575dc11e70c8037043cfad0ab26c9f6da4537a4b8ee5d9ee4903516a86065` |
| `lifecycle-churn.json` | `04fa078a681097f5dcd858c7b3f9bbb690bc010a49b56ca762f07dd0cf74969d` |
| Outer log | `95dcdd7c892a4e77b251981ecdfee8231a726fe4bada521a29f842feb5966941` |

Final reports are under
`/tmp/p2p-vpn-tun_namespace_measures_lifecycle_churn_resources-1.63193a6d7f4a4f94`.
The executable is the cached `debug/deps/tun_namespace-67f2a6135855e937` under
`/tmp/p2p-vpn-review-target`; verify its hash before both full runs.
