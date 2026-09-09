# Sustained Unavailable Peers

## Scope

Two repetitions of the [frozen S2 protocol](sustained-resource-review.md#unavailable-peer-protocol)
passed using fixture `148d4640`. Production runtime code is unchanged from
`00b1b58a`. These are debug namespace measurements, not release benchmarks.

| Control | Value |
| --- | --- |
| Peers | One static overlay peer per node; five default bootstrap candidates |
| Topology | Isolated direct UDP; no Internet route or reachable public infrastructure |
| Warmup | 30 seconds connected after initial traffic |
| Cut | Node B underlay link down; failed underlay ping required |
| Observation | 300 seconds after link-cut validation; no offered payload |
| Restoration | Same link, addresses, identities and live processes |
| Recovery gate | Both directions within 60 seconds; then 5/5 replies each way |
| Watchdogs | 510-second namespace deadline; 540-second external deadline |

Executable SHA-256:
`466f9c5612253f689651771b02c68bb397d4994adcc566cc439ad938b4bd3804`.
Both tests used this saved executable without concurrent builds or manual recovery.

## Reproduction

Use a distinct `LOG` for each fresh-process repetition. The printed artifact
directory contains the outage report, recovery report and bounded diagnostics.

```sh
timeout --signal=TERM --kill-after=10s 540 env \
  P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
  P2P_VPN_TUN_E2E_IDLE_SECONDS=300 \
  P2P_VPN_TUN_E2E_IDLE_RUNTIME_SAMPLING=1 \
  /tmp/p2p-vpn-review-target/debug/deps/tun_namespace-fc4bbc3b02326c73 \
  tun_namespace_measures_unavailable_peer_resources \
  --ignored --exact --nocapture > "$LOG" 2>&1
```

## Results

| Run | A CPU, % One Core | B CPU, % One Core | Recovery Seconds | Total Test Seconds |
| --- | ---: | ---: | ---: | ---: |
| 1 | 0.15667 | 0.14333 | 28.864 | 384.67 |
| 2 | 0.15667 | 0.14000 | 28.804 | 384.56 |

CPU uses actual elapsed time and 100 process ticks per second. Recovery time
starts before the link-up command. Final five-packet checks occur afterward.
Both directions passed 5/5, including node B to node A's routed prefix.

### Capture Quality

- Each run contains 299 OS samples per node and 60 runtime snapshots per node.
- OS samples span 300 seconds; maximum observed gap is below 1.011 seconds.
- The OS loop sleeps after reads, so sampling work adds drift; no fixed 301-row count is assumed.
- Runtime slots 0 through 295 are complete, with no query errors; maximum query time is below 2.883 ms.
- PID/start identities and CPU monotonicity validate through the outage; identities also match after recovery.
- Host one-minute load: run 1, 1.13 to 1.64; run 2, 1.15 to 2.39.

### Resource Owners

| Observation | Both Runs |
| --- | --- |
| Threads | 20 per node throughout |
| A total / socket descriptors | 20-22 / 13-15 during outage; 21 / 14 after recovery |
| B total / socket descriptors | 18-19 / 11-12 during outage; 19 / 12 after recovery |
| Queues, bytes and packets | Zero throughout sampled outage windows |
| Queue drops and expiry | Zero increments |
| Pending connection attempts | A peaks at one; B sampled at zero |
| Maintenance / recovery queries | Each owner count peaks at one |
| Retiring connections, packet hellos/responders | Zero throughout sampled windows |
| Retained recovery dial targets | Five or six; six at the end |
| Retained recovery query peers | At most one |
| Final selected path | `direct_udp_datagram` for both nodes |

The five-second gauge series does not bound unobserved between-sample peaks.
Cooldown entries and the scheduled address-publication refresh are retained
state, not evidence of a pending task leak.

### Retry Activity

| Run / Node | Outgoing Errors, First / Last | Redial Counter Delta |
| --- | --- | ---: |
| 1 / A | 5 / 50 | 1 |
| 1 / B | 5 / 50 | 1 |
| 2 / A | 5 / 48 | 1 |
| 2 / B | 5 / 50 | 1 |

`redial_attempts` is not the total count of every discovery or infrastructure
dial. The error timeline includes bootstrap candidates as well as the overlay
peer. Default bootstrap dials fail locally with `Network is unreachable`.

- Whole-test logs record ten outgoing overlay-peer failures per node/run.
- Each bootstrap candidate records eight failures, except two run-2 A candidates with seven.
- Those log totals include startup and recovery; do not relabel them as outage-only deltas.
- All runs begin the sampled window with five outgoing errors already counted.

Observed retry clocks rise toward 300 seconds for dial/address cooldowns and
240 seconds for recovery queries. This is consistent with the existing
exponential policies, not proof of every transport's internal retry interval.

Policy sources: [dial backoff](../../src/runtime/runner.rs),
[query backoff](../../src/runtime/recovery_queries.rs), and
[owner-counter semantics](../../src/runtime/runner/recovery_snapshot.rs).
Portable samples retain each counter's range and the observed error timeline.

### Memory Attribution Remains Open

| Run / Node | RSS KiB, Outage Start / End | RSS KiB After Recovery |
| --- | --- | ---: |
| 1 / A | 36632 / 36920 | 36988 |
| 1 / B | 36608 / 36756 | 36864 |
| 2 / A | 36396 / 36556 | 36648 |
| 2 / B | 36520 / 36664 | 36744 |

Run 2 B's final minute checkpoints are 36628, 36636 and 36664 KiB. They meet
the declared allocation-investigation trigger. Descriptor recovery and low CPU
do not explain RSS retention or establish that live allocations are bounded.

## Evidence and Cleanup

[Portable summaries](sustained-unavailable-samples.json) include paths, SHA-256,
actual timestamps, identities, resource ranges, retry timelines and recovery data.
Original reports and logs remain preserved.

| Run | Artifact Suffix | Combined JSON Bytes | Combined Node Log Bytes |
| --- | --- | ---: | ---: |
| 1 | `1.df4c4a5643837728` | 2339095 | 1292291 |
| 2 | `1.021525728d92d21f` | 2339092 | 1289672 |

- Artifact prefix: `/tmp/p2p-vpn-tun_namespace_measures_unavailable_peer_resources-`.
- Outer logs: `/tmp/p2p-vpn-sustained-unavailable-{1,2}.log`.
- Both sessions exit zero; no matching fixture process remains.
- Temporary project storage after both runs: 8,318,220 KiB, below 10 GiB.
- No physical devices, host deployments or public-network campaign were used.

## Remaining Scope

- Fixed offered load, packet pressure and repeated churn are separate workloads.
- Live allocation attribution, ledger retention and longer settling still need evidence.
- Multi-network and Android background resource measurements remain outstanding.
- No claim is made about changed WAN addresses, successful public discovery,
  QUIC streams, physical energy use or final release acceptance.
