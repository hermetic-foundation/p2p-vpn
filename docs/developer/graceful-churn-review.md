# Graceful Reconnect Review

## Scope

Repeat the existing ten-cycle connected allocation workload after the Linux
reader fix. Add the already validated graceful teardown checkpoints; preserve
all path demotion, autonomous recovery, traffic and process-identity assertions.

## Frozen Matrix

| Setting | Value |
| --- | --- |
| Runtime revision | `575ef4ac`; documentation baseline `08c703a5` |
| Executable | `/tmp/p2p-vpn-review-target/debug/deps/tun_namespace-b64312792e9e0507` |
| SHA-256 | `6516ee1bcb7b227fac264fabfd6e7a47a86d0f9fdba93e15819bea32953fa6ab` |
| Topology | Existing direct-UDP namespaces; no public Internet routing |
| Campaign | Two ten-cycle captures; fresh processes per capture |
| Admission | Corrected direct teardown and both five-round pressure captures already pass |
| Outage / recovery observation | Existing 30 / 40 seconds per cycle |
| Final settling | Existing 60 seconds |
| Watchdogs | Existing 890 seconds internal / 900 seconds external |
| Shutdown | Existing two-second acknowledgement / five-second exit per daemon |
| Allocation cadence | Existing five-second sampling; maximum 5500 ms gap |
| Phase coverage | At least duration / 5 samples per role; maximum 5500 ms boundary gap |
| Clock agreement | At most 100 ms monotonic versus wall-clock disagreement |
| Artifact caps | Existing 8 MiB combined JSON / 2 MiB combined node logs |
| Storage | All `/tmp/p2p-vpn-*` below 10 GiB before each capture |

No build, debugger, deadline extension or manual recovery may overlap a capture.
Stop on the first failed workload assertion or evidence gate; preserve failures.
Matching residual totals are evidence to investigate, not an attribution rule.

## Acceptance

1. All ten cycles demote dead paths and recover autonomously with strict traffic checks.
2. Process identities remain unchanged; final traffic and original counter gates pass.
3. Allocation samples cover every outage, recovery and settling phase.
4. Both daemons acknowledge shutdown, exit normally and release TUN/control interfaces.
5. Record pre-child, runner-return, child-return and child-settled allocations.
6. Compare both captures and prior pressure/direct residuals; attribute owners separately.

## Command

Use a distinct outer log for each capture. Do not set the one-cycle smoke flag.

```sh
env -u P2P_VPN_REVIEW_CHURN_SMOKE \
  -u P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS \
  -u P2P_VPN_TUN_E2E_WAIT_SCALE \
  P2P_VPN_REVIEW_GRACEFUL_SHUTDOWN=1 \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  timeout --signal=TERM --kill-after=10s 900 \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-b64312792e9e0507 \
  tun_namespace_measures_lifecycle_churn_resources \
  --ignored --exact --nocapture --test-threads=1
```

## Status

Manifest frozen before execution. The [first ten-cycle capture](graceful-churn-results.md)
passes its workload, teardown and sampling gates. The independent repetition and
residual attribution remain pending; the two-run campaign is not complete.
