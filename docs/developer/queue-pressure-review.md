# Runtime Queue Pressure Review

## Result

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
| Recovery | Delete the test qdisc, await TCP selection and empty queues, then ping both directions |
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

## Interpretation Limits

- Queue assertions cover sampled values; separate queue unit tests enforce insertion bounds between observations.
- The four-packet limit binds before the byte limit. This is not independent byte-limit saturation evidence.
- RSS stayed above the initial value, and node B's final RSS exceeded its during-load samples. No leak-free or allocator-reclamation claim follows.
- Process CPU during the sampled load interval was approximately 8.54% and 2.32% of one core, using 100 clock ticks/second. This is a debug integration build, not a release throughput benchmark.
- This is a current-run pre/post comparison, not a historical-revision baseline comparison or a long-duration memory trend.
- Datagram, QUIC-stream, relay, multiple peers, and concurrent networks are not exercised by this pressure case.
- Source-linked daemon processes run the normal Linux runtime; this is not packaged CLI deployment or NixOS service activation evidence.
- Full-goal resource and lifecycle acceptance remains in the [verification map](review-verification.md).
