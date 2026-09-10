# Sustained Traffic Review

## Status

Both full S3 captures passed; this closes the bounded direct-UDP workload only.
This workload reuses the S1/S2 direct-UDP namespace topology and
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
cover that branch.

## First Full Capture

Fixture `af5951cc`, runtime `11416a8d`, using the corrected-runtime executable
hash above. No overlapping builds or manual recovery. The 480-second internal
and 510-second external watchdogs were unchanged.

| Result | Observation |
| --- | --- |
| Total elapsed | 441.49 seconds, including startup and checks |
| Traffic | 15000 sent / 15000 received over 300.001 seconds |
| Pacing | Zero skipped slots; maximum lateness 2.866 ms |
| Packet validity | Zero duplicate or invalid replies |
| Transport / identity | Fixed-transport gate passed; same daemon identities across both phases |
| Final connectivity | Strict 5/5 replies in both directions |
| Artifacts | 4228 KiB, suffix `1.11261118cb7d5688` |

### Resource Observations

| Phase / Node | CPU, % One Core | RSS First / Last, KiB | Total / Socket FDs | Threads |
| --- | ---: | --- | --- | ---: |
| Load A | 6.7967 | 35844 / 35856 | 21 / 14 | 6 |
| Load B | 6.5367 | 35620 / 35648 | 19 / 12 | 6 |
| Drain A | 0.1833 | 35856 / 35856 | 21 / 14 | 6 |
| Drain B | 0.1667 | 35648 / 35648 | 19 / 12 | 6 |

Each node has 300 load OS samples and 61 drain samples. Maximum sample gap is
below 1.009 seconds. Both 60-snapshot load and 12-snapshot drain runtime series
are complete, with no query errors. CPU uses observed intervals and 100 Hz ticks.

- Sampled queues, expiry/drop counters, retiring connections and pending connection attempts stayed zero.
- Outgoing connection-error counts did not increase during either phase.
- Redial attempts stayed zero; connected-skip counters increased as maintenance ran.
- Kademlia pending RPC/query storage stayed zero in these five-second observations.
- RSS increased slightly during load and stayed flat through drain; no allocation cause is established.

Five-second runtime samples do not bound between-sample peaks. The final load
snapshot precedes the traffic deadline, so its payload deltas do not equal the
generator total; drain snapshots contain the final 15010 packet counters,
including the ten initial fixture packets. Do not treat this as lost traffic.

[Counter summaries, outcome and hashes](sustained-traffic-samples.json) preserve
the observations. Full logs remain under the printed artifact directory and
`/tmp/p2p-vpn-sustained-traffic-full-1.log`. The repeated capture below completes
the planned pair; neither run proves sustained behavior generally.

## Repeated Capture

The second run used the identical executable and workload, without builds or
manual recovery. It passed in 416.33 seconds, with 15000/15000 requests over
300.002 seconds, zero skipped/invalid/duplicate packets, and 2.644 ms maximum
pacing lateness. Transport, process identity and final strict pings passed.

| Phase / Node | CPU, % One Core | RSS First / Last, KiB | Total / Socket FDs | Threads |
| --- | ---: | --- | --- | ---: |
| Load A | 6.8101 | 35852 / 35916 | 21 / 14 | 6 |
| Load B | 6.5434 | 35780 / 35808 | 19 / 12 | 6 |
| Drain A | 0.1667 | 35916 / 35916 | 21 / 14 | 6 |
| Drain B | 0.1667 | 35808 / 35808 | 19 / 12 | 6 |

- Each node again has 300 load and 61 drain OS samples, with gaps below 1.009 seconds.
- Runtime series are complete: 60 load and 12 drain snapshots per node; no query errors.
- Sampled queues, drops, expiry, pending connection attempts and retiring connections stayed zero.
- Redial attempts stayed zero; outgoing connection errors did not increase in either phase.
- Artifact suffix: `1.c6e80b73f0d9ffe2`, 4132 KiB; full log `/tmp/p2p-vpn-sustained-traffic-full-2.log`.
- Both run summaries and all six primary report hashes are preserved in the linked JSON.

Together, these runs delivered 30000 requests and replies, with CPU settling
below 0.2% of one core per node after load. Descriptors and threads remained
constant; modest load-phase RSS increases did not continue during drain.

This is current-runtime, moderate-rate debug evidence, not a before/after
performance improvement, release throughput ceiling or allocation proof.
Lifecycle churn, multi-network isolation, Android background work and retained
transport/runtime allocation attribution remain open.
