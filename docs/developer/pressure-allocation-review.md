# Pressure Allocation Review

## Scope

Extend the completed S4 pressure campaign with the existing calibrated
`allocation-review` integration executable. Preserve the workload and recovery
assertions; do not attach GDB during these baseline captures.

## Frozen Matrix

| Setting | Value |
| --- | --- |
| Baseline | `5cf4eff5`; cached integration executable, no new build |
| Binary SHA-256 | `7bd693606049cc7df7de8d5294ab91fe2bab8dca81fdbfdd4b3c77c747a5ad04` |
| Runtime | Two Tokio workers; direct TCP; isolated namespaces; no Internet route |
| Admission | One single-round smoke for each profile |
| Campaign | Packets, bytes, bytes, packets; five rounds each; fresh processes per capture |
| Packet profile | Four packets / 8192 bytes |
| Byte profile | Sixteen packets / 4096 bytes |
| Pressure and drain | Original S4 workload and strict 5/5 recovery checks |
| Watchdogs | Five rounds: 450 seconds internal / 480 external; smoke: 90 / 120 |
| Evidence caps | Per-case JSON below 8 MiB; combined node logs below 2 MiB |
| Allocation observer | Existing initial sample and five-second periodic samples |
| Storage | All `/tmp/p2p-vpn-*` below 10 GiB before each capture |

## Gates

1. Verify the executable hash and completed integration allocator calibration.
2. Run both smoke profiles before the four-run campaign.
3. Stop the campaign on its first failed assertion; preserve partial evidence.
4. Require queue drops under pressure, configured queue bounds, drained owners and strict recovery traffic.
5. Check allocation sample presence, monotonic ordering and maximum 5,500 ms gaps for both daemons.
6. Compare whole-run requested-byte/block trajectories and final samples with queue and process evidence.
7. Attribute retained growth before claiming bounded memory; RSS and empty queues alone are insufficient.

The current pressure reports lack a shared Unix timestamp for exact round
boundaries. Do not infer precise allocation checkpoints from file modification
times or relative round clocks. Periodic allocation samples can miss brief peaks.

Final process termination is not graceful runtime teardown. Completed queue-owner
diagnostics supply narrower release evidence; whole-daemon attribution remains
separate from the successful traffic and capacity gates.

## Command

Use distinct output logs for every capture. Substitute the profile and round count;
use the corresponding frozen watchdog above. Do not set initiator or wait overrides.

```sh
env -u P2P_VPN_TUN_E2E_PRESSURE_INITIATOR \
  -u P2P_VPN_TUN_E2E_ORCHESTRATOR_TIMEOUT_SECONDS \
  -u P2P_VPN_TUN_E2E_WAIT_SCALE \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  P2P_VPN_TUN_E2E_PRESSURE_LIMIT=packets \
  P2P_VPN_TUN_E2E_PRESSURE_ROUNDS=5 \
  timeout --signal=TERM --kill-after=10s 480 \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-8ef8d4fc7581d039 \
  tun_namespace_recovers_after_tcp_queue_pressure \
  --ignored --exact --nocapture --test-threads=1
```

## Status

Both admission smokes and all four campaign captures passed. The
[results](pressure-allocation-results.md) preserve quantitative outcomes and
remaining attribution limits. No production or platform configuration changed.
