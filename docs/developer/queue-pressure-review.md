# Runtime Queue Pressure Review

## Result

**Recovery remains under investigation.** A later repeat failed its first
post-pressure ping check. The passing runs below do not establish reliable
recovery across repetitions; see [Repeated Workload](#repeated-workload).

The new TCP-only namespace test passed in 39.88 seconds. It exercised real Linux
TUN traffic, queue overflow, and recovery in two daemon processes. No production
runtime change was required.

| Observation | Node A | Node B |
| --- | ---: | ---: |
| Maximum sampled queue packets | 4 | 4 |
| Maximum sampled queue bytes | 4,112 | 4,112 |
| Queue drops before load | 0 | 0 |
| Queue drops after recovery | 1,424 | 5 |
| Queue packets after recovery | 0 | 0 |
| RSS before load, KiB | 33,220 | 33,416 |
| Maximum sampled RSS during load, KiB | 34,612 | 34,132 |
| RSS after recovery, KiB | 34,588 | 34,948 |
| Threads before / after | 6 / 6 | 6 / 6 |
| Socket descriptors before / after | 6 / 6 | 6 / 6 |
| Process start ticks before / after | Unchanged | Unchanged |
| Post-pressure ping replies | 5/5 | 5/5 |

The ping generator transmitted 1,977 packets over approximately 20 seconds.
Only 15 replies arrived during pressure; loss is intentional in this constrained
topology. Recovery assertions require five replies in each direction afterward.

## Reproduce

```bash
TOKIO_WORKER_THREADS=2 P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
  cargo test --offline --test tun_namespace \
  tun_namespace_recovers_after_tcp_queue_pressure -- --ignored --exact --nocapture
```

| Parameter | Fixture Setting |
| --- | --- |
| Topology | Two user/network namespaces, private veth link, no Internet route |
| Transport | Direct TCP; packet datagram listeners disabled |
| Queue limits | Four packets and 8,192 bytes per peer |
| Underlay constraint | Node A egress: 64 kbit/s, 50 ms netem delay, 16-packet qdisc limit |
| Traffic | Up to 3,000 ICMP requests, 1,000-byte payloads, 5 ms interval |
| Traffic deadline | Ping 20 seconds; fixture supervision 25 seconds |
| Sampling | Both daemons approximately every 250 ms; 79 sample pairs in this run |
| Recovery | Delete the test qdisc, await TCP selection, empty queues and stream-request windows, then ping both directions |
| Cleanup | Existing child guards kill and reap daemons; no matching test process remained |

The offered ICMP request rate is at most approximately 1.65 Mbps before transport
encapsulation. Replies add traffic, while the constrained egress is much slower.
No physical interface or production service is modified.

## Evidence

- Base revision: `8f308a1d`; working-tree changes add the namespace pressure fixture.
- Run log: `/tmp/p2p-vpn-review-queue-pressure-run-3.log`.
- Raw report: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-2441007/queue-pressure.json`.
- Report SHA-256: `4dc89534589fd10416acddbbf888e50119a0ec72519ed04878d5740146046a8f`.
- Integration-test executable SHA-256: `e4379cc87f709891e84bc776d63401b36ba12d0b2b71594cbd03d6ffa40d225c`.
- Initial fixture source SHA-256: `d1dcb254b81b03647f4c872618c9052af1d34c84ec31fb59ced48e255a1ea4e8`.

### Diagnostic Attempts

| Attempt | Outcome |
| --- | --- |
| Initial sampler | Queried state instead of status; failed before sending load |
| First traffic generator | QueueFull drops occurred, but `ping -W` did not bound total run time under backpressure |
| Corrected generator | Added `-w 20`, validates actual transmission count, saves partial measurements before recovery checks; passed |

The fixture now retains `queue-pressure-partial.json` after load completion.
That file is not a successful-recovery report; later assertions may still fail.

## Suite Verification

All 12 namespace scenarios passed sequentially in 256.18 seconds, including
pairing, discovery, datagrams, relay fallback, network movement, and promotion.
Log: `/tmp/p2p-vpn-review-queue-pressure-namespace-suite.log`.

The pressure repeat transmitted 1,976 packets and reached four queued packets
on both nodes. Drops were 1,381 and 19; both queues drained and both post-pressure
five-packet checks passed. Neither daemon restarted.

- Repeat report: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-2446844/queue-pressure.json`.
- RSS before / after: node A 33,504 / 34,804 KiB; node B 33,520 / 35,064 KiB.
- The repeated increase does not distinguish normal retained allocation from a leak.

The native workspace also passed: 1,232 tests, 23 opt-in tests ignored.
Log: `/tmp/p2p-vpn-review-queue-pressure-workspace.log`.
These full-suite runs precede the final locale hardening and explicit-import cleanup.

The fixture sets `LC_ALL=C` for its own ping commands so summary parsing does
not depend on the operator's locale. This changes test execution only.

The final pressure case passed again in 39.63 seconds. Required Clippy groups,
changed-file formatting, whitespace, and offline Nix test-source inclusion passed.
Non-fatal style warnings remain; no VM or Android deployment was repeated.

- Final run log: `/tmp/p2p-vpn-review-queue-pressure-final.log`.
- Final report: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-2453110/queue-pressure.json`.
- Final report SHA-256: `915d5093aa8ea5587fa6a75a742c6e3bc1795e51e816bb44c02d6ae6ec9f50d6`.
- Final executable SHA-256: `94fe7ba840c2e2853bf72cf504af4c416be236ceff8509c4731b2ee477fd2e69`.
- Final fixture SHA-256: `bfd1b10be1102bec98e89a44e8e86748147d4c1ecef615af691fc3f01abc7da2`.

## Repeated Workload

The default remains one cycle. Set `P2P_VPN_TUN_E2E_PRESSURE_ROUNDS` to an
integer from one through five to repeat pressure and recovery on the same daemons.
Invalid values are rejected; no new production configuration is introduced.

```bash
TOKIO_WORKER_THREADS=2 P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
  P2P_VPN_TUN_E2E_PRESSURE_ROUNDS=3 \
  cargo test --offline --test tun_namespace \
  tun_namespace_recovers_after_tcp_queue_pressure -- --ignored --exact --nocapture
```

| Contract | Behavior |
| --- | --- |
| One cycle | Existing root-level artifacts remain in place |
| Multiple cycles | Each saves its own `round-N/` reports and ping summary |
| Series checkpoint | `queue-pressure-series.json` records requested/completed counts and `complete` |
| Process continuity | First-cycle start ticks must match every later cycle's final observations |
| Recovery | Each cycle independently requires queue drops, drained queues/stream windows, and 5/5 pings both ways |
| Supervision | Default orchestrator budget is 90 seconds per requested cycle; explicit overrides still apply |
| Replay | Generated commands preserve the pressure-round environment setting |
| Failure diagnostics | Each recovery ping saves stdout, stderr, exit code, and before/after observations before assertions |

### Repetition Evidence

The first three-cycle attempt failed in cycle one after 37.21 seconds.
Its log records a replacement TCP connection followed by another stream-upgrade
timeout and path demotion. Causality is not yet established.

- Failure log: `/tmp/p2p-vpn-review-pressure-series-run.log`.
- Partial artifacts: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-2460255/round-1/`.
- No successful series report was produced. Recovery-ping output was not retained in that attempt.

After adding failure diagnostics only, three cycles passed in 119.64 seconds.
Every cycle required 5/5 replies in both directions, empty final queues, and
unchanged daemon start ticks. No production runtime fix was made.

| Cycle | Transmitted Packets | A RSS Before / After, KiB | B RSS Before / After, KiB |
| --- | ---: | ---: | ---: |
| 1 | 1,982 | 33,320 / 34,448 | 33,276 / 33,872 |
| 2 | 1,981 | 34,448 / 35,024 | 33,872 / 34,600 |
| 3 | 1,981 | 35,024 / 35,112 | 34,600 / 34,696 |

- Successful run: `/tmp/p2p-vpn-review-pressure-diagnostics-run.log`.
- Series: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-2464869/queue-pressure-series.json`.
- Namespace unit checks: 11 passed, 12 opt-in scenarios ignored.
- These observations do not resolve the earlier intermittent recovery failure.

### After Failed-Connection RTT Fix

A five-cycle attempt at `2a1ab1a2` failed in cycle one after 36.81 seconds.
The first post-pressure request was lost; requests 2-5 returned in 3.11-4.34 ms.
The connection-scoped RTT fix therefore did not eliminate this symptom.

| Observation | Evidence |
| --- | --- |
| Recovery ping | 5 transmitted, 4 received; exit status 0, strict assertion failed |
| Queue overflow during ping | Unchanged: A 1,427, B 6 |
| Queue occupancy before / after ping | Zero packets and bytes on both nodes |
| Process continuity | Start ticks unchanged; sockets decreased from 7 to 6 per node |
| Connection lifecycle | A timed out connection 12 and established replacement 14; packet-level causality remains unproven |

- Log: `/tmp/p2p-vpn-review-failed-path-pressure-five.log`.
- Ping and snapshots: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-2483559/round-1/recovery-ping-a.*`.
- Empty application queues and a selected path do not prove all transport work has completed.
- Samples now also retain queue expiry, stream in-flight owners, outbound failures, and inbound drops.
- No timing or 5/5 assertion was relaxed; stable recovery versus initial readiness remains under investigation.

### Recovery Boundary Correction

With the additional counters, two cycles passed and the third reproduced 4/5
replies. Its pre-ping sample showed empty queues but 256 in-flight stream
requests on A and 24 on B. These are separate stages of the forwarding pipeline.

| Third-Cycle Counter | A Before / After Ping | B Before / After Ping |
| --- | ---: | ---: |
| Stream requests in flight | 256 / 0 | 24 / 0 |
| Queue expiry | 102 / 103 | 20 / 20 |
| Queue overflow | 4,457 / 4,458 | 39 / 41 |
| Outbound failures | 773 / 773 | 159 / 159 |
| Inbound drops | 0 / 0 | 0 / 0 |

The fixture was starting its steady-state assertion while substantial prior
traffic remained in flight. It now requires both queues and runtime stream
windows to drain within the existing 30-second recovery deadline.

- The subsequent 5/5 ping requirements remain unchanged; no retries were added to those assertions.
- This corrects the measurement boundary, not production transport behavior or a promise of lossless traffic during recovery.
- Runtime windows and libp2p-internal work are distinct; expired runtime owners do not prove every internal task has stopped.
- Diagnostic run: `/tmp/p2p-vpn-review-pressure-owner-sampling-final.log` (104.42 seconds; cycle three failed).
- Evidence: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-2486328/round-3/recovery-ping-a.json`.
- The incomplete series checkpoint correctly records two completed cycles, not three.
- Initial instrumentation queried stream ownership from status instead of state; corrected before this diagnostic run.

The corrected gate passed three cycles in 119.84 seconds. Each cycle retained
the same daemon processes, ended with zero queued/in-flight packets, and passed
5/5 pings in both directions. No production runtime change was made.

| Cycle | Transmitted Packets | A Final RSS, KiB | B Final RSS, KiB |
| --- | ---: | ---: | ---: |
| 1 | 1,980 | 34,864 | 34,028 |
| 2 | 1,979 | 35,072 | 34,112 |
| 3 | 1,982 | 35,344 | 35,124 |

- Run log: `/tmp/p2p-vpn-review-pressure-drain-run.log`.
- Complete series: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-2488364/queue-pressure-series.json`.
- Namespace unit tests: 11 passed, 12 ignored; focused Clippy, rustfmt, whitespace, and Nix source checks passed.
- Full workspace, other namespace scenarios, VMs, and Android were not repeated for this test-only change.
- Status and state are separate sequential queries, not an atomic combined snapshot.
- RSS still increases across these cycles; this does not establish a plateau or leak-free behavior.

The checkpoints compare current-process RSS under repeated work. They do not
identify allocating owners or prove an asymptotic heap bound. A plateau over a
few cycles is evidence for that workload only, not a general leak-free guarantee.

## Interpretation Limits

- Queue assertions cover sampled values; separate queue unit tests enforce insertion bounds between observations.
- The four-packet limit binds before the byte limit. This is not independent byte-limit saturation evidence.
- RSS stayed above the initial value, and node B's final RSS exceeded its during-load samples. No leak-free or allocator-reclamation claim follows.
- Process CPU during the sampled load interval was approximately 8.54% and 2.32% of one core, using 100 clock ticks/second. This is a debug integration build, not a release throughput benchmark.
- This is a current-run pre/post comparison, not a historical-revision baseline comparison or a long-duration memory trend.
- Datagram, QUIC-stream, relay, multiple peers, and concurrent networks are not exercised by this pressure case.
- Source-linked daemon processes run the normal Linux runtime; this is not packaged CLI deployment or NixOS service activation evidence.
- Full-goal resource and lifecycle acceptance remains in the [verification map](review-verification.md).
