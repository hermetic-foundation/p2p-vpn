# Sustained Traffic Review

## Status

S3 fixture implementation and validation are in progress. No sustained capture
is claimed yet. This workload reuses the S1/S2 direct-UDP namespace topology and
collectors; it does not alter the older Kademlia resource workload definitions.

## Frozen Workload

| Setting | Full Capture | Smoke Only |
| --- | --- | --- |
| Warmup | 30 seconds | 30 seconds |
| Offered traffic | 300 seconds | 10 seconds |
| Drain observation | 60 seconds | 10 seconds |
| Rate / payload | 50 ICMP requests/second, 512-byte payload | Same |
| Maximum offered requests | 15000 | 500 |
| Generator preload | One request; overdue slots skipped, not burst-replayed | Same |
| Acceptance eligibility | Full-window candidate, subject to all checks | Always false |
| Orchestrator watchdog | 480 seconds | 140 seconds |

- Repeat the full capture twice, using one executable and no overlapping builds.
- Two isolated nodes, no Internet route, selected direct UDP packet path.
- One request stream from A's routed overlay address to B; replies exercise both directions.
- Preserve actual sent/received/skipped counts, invalid replies, duplicates and pacing lateness.
- Require at least 98% of scheduled sends and replies to at least 98% of actual sends.
- Require zero duplicate/invalid replies and final strict 5/5 pings in both directions.
- Require healthy direct-UDP samples and unchanged stream-fallback counters across both phases.
- Do not clamp counters or replace lost traffic with a retry to obtain a passing result.

## Observations And Budgets

| Item | Control |
| --- | --- |
| OS observations | Every second, both daemon PIDs; CPU ticks, RSS, FDs, sockets, threads |
| Runtime counters | Every five seconds; mandatory complete series |
| Daemon identity | Preserve process start ticks within and across phases |
| Phase reports | `traffic-sample.json`, `traffic-drain-sample.json`; at most 8 MiB each |
| Generator report | `traffic-generator.json`; 64 KiB worker file-size limit |
| Generator log | `traffic-worker.log`; same 64 KiB limit |
| Worker cleanup | RAII child ownership; five-second exit check after traffic observation |
| Outcome | `sustained-traffic.json`, retained before delivery/health assertions |
| Interference | Host load, collector CPU, actual sample timing and generator exit wait |

The load collector starts immediately after spawning the generator. Retain that
transition duration and any generator-exit wait; neither is hidden in the drain
window. Resource reports remain available if delivery or final pings fail.

## Commands

```bash
P2P_VPN_TUN_E2E_KEEP_TEMP=1 TOKIO_WORKER_THREADS=2 \
  "$TEST_BINARY" tun_namespace_measures_sustained_traffic_resources \
  --ignored --exact --nocapture --test-threads=1
```

For fixture smoke only, additionally set `P2P_VPN_REVIEW_TRAFFIC_SMOKE=1`.
Other values are rejected. Do not set the older idle-duration environment option;
this workload uses its own frozen load/drain phases. Runtime sampling cannot be disabled.

## Limits

- Moderate paced load, not a throughput ceiling or TCP/QUIC performance comparison.
- Debug fixture timings remain separate from release measurements.
- Default public bootstrap attempts fail locally; no public discovery success is exercised.
- RSS does not identify retained allocations; allocation attribution remains separate.

## Preliminary Smoke

The first smoke passed in 76.32 seconds: 500 sent/received, no skipped slots,
duplicates or invalid replies, unchanged daemon identities, and strict final
pings in both directions. Both phases had 22 OS rows and four runtime snapshots.

- Eligibility: false; ten-second load and drain windows only.
- Artifact suffix: `1.7cea4ef2667f1a12` under `/tmp/p2p-vpn-tun_namespace_measures_sustained_traffic_resources-*`.
- Artifact storage: 648 KiB; all fixture processes exited.
- Binary SHA-256: `268b582fc510cd15850e90db30f8c3434bdb403dc6452d6af29f9e273340ef88`.
- This preceded the explicit fixed-transport gate; final validation remains pending.

## Validation Follow-Up

| Gate | Result |
| --- | --- |
| Locked offline workspace | 1500 passed, 39 ignored |
| Required workspace Clippy | Correctness, suspicious and performance groups passed |
| Formatting | Changed Rust files passed |
| Cached evaluated Nix source check | Passed; new helper matches filtered Linux input |
| Updated smoke | Passed in 76.37 seconds; fixed transport and identity checks passed |
| Original idle entry point | Failed during startup, before the collector ran; investigation open |

The updated smoke again delivered 500/500 with zero skipped slots, invalid
replies or duplicates. Its eligibility is false. Artifacts use suffix
`1.709c8bad1255ab17`; binary SHA-256 is
`5282cc4eea4b027dec87d27303b8e7add82fc3cc8e7babd626bb984571cb6b27`.

Source-check artifacts: `/tmp/p2p-vpn-sustained-traffic-source.5nRmliTF`.
Checks ran with cached tools outside a Nix build sandbox. No production Rust
or Android source changed; no cross-build or physical-device test was run.

### Startup Failure

The unchanged idle entry point failed after 32.45 seconds, waiting for node A
to validate its peer and establish a UDP packet session. It did not reach the
modified collector, and no `idle-sample.json` was produced.

| Observation | Evidence |
| --- | --- |
| TCP setup | Both sides established two connections and retired connection 1 as a duplicate |
| Control requests | Both sides logged two failures because a connection closed before responses arrived |
| Remaining connection | Connection 2 stayed present through the startup wait |
| Capabilities | Zero accepts/rejections; node A still reported an unvalidated peer |
| Deadline | Existing startup deadline unchanged; no manual rescue |
| Cleanup | No fixture processes remained |

This is consistent with lost capability exchange during duplicate-connection
retirement. Exact request-to-connection assignment and retry timing still need
a focused reproduction; do not claim a root-cause fix from this single run.

- Outer log: `/tmp/p2p-vpn-sustained-traffic-idle-regression.log`.
- Artifact suffix: `1.749665621a19b660` under `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-*`.
- Outer log SHA-256: `b11f846edf8d02794ecf5de56218b4fc7b655b91bfeccb9fd14f0a6de0158800`.
- Node A log SHA-256: `1bd10b3526b8a04b0d17077c82ad3a946e37a483d049a6f324eab128192760c9`.
- Node B log SHA-256: `1b58a4ef408d27c53678d6974a2e50c75ff58cd1682dcd7b270793efd48e6f0f`.

At inspection, non-membership control outbound failures record a metric and log
the error in `handle_control_event`; that branch does not schedule a capability
retry. Review the surrounding connection and retry ownership before changing it.
The [capability-retirement review](capability-retirement-review.md) records a
guarded retry correction, before/after regression, pinned-behavior dispatch
test, native compilation and live compatibility checks.

### Corrected Runtime

- Workspace: 1502 passed, 39 ignored; required Clippy, format and source checks passed.
- Android x86_64/API 26 native build passed; no device deployment.
- Two idle compatibility captures passed with unchanged deadlines.
- Traffic smoke passed in 101.31 seconds: 500/500, no skips/invalid/duplicate replies.
- Fixed-transport, same-process and final strict ping checks passed; eligibility remains false.
- Artifact suffix: `1.27696ede4be78c2e`; log `/tmp/p2p-vpn-capability-retirement-traffic-smoke.log`.
- Namespace binary SHA-256: `60deab7bb283ba183b2b34658de358e27372e1cf5bd5bee828fbb94ae5174d6e`.

The live follow-ups did not trigger the new retry branch; deterministic tests
cover that branch. The two full S3 captures are the next measurement step.
