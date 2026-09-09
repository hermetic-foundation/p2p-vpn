# Sustained Pressure Investigation

## Status

The frozen four-run S4 campaign stopped at its first failure. One packet-profile
run passed five rounds; the following byte-profile run failed in round two.
Diagnostic passes do not replace that failure or complete the campaign.

The later corrected campaign completed all four captures and 20 rounds.
This establishes bounded local pressure/recovery measurements, not allocation
attribution or a proof of all stream-owner limits; see the open finding below.

| Campaign Run | Profile | Completed Rounds | Result | Seconds |
| --- | --- | ---: | --- | ---: |
| 1 | Packets | 5 / 5 | Pass | 169.67 |
| 2 | Bytes | 1 / 5 | Round-two recovery ping: 3/5 | 69.51 |
| 3 | Bytes | Not started | Investigation first | - |
| 4 | Packets | Not started | Investigation first | - |

Both used fixture `750dbe15`, two Tokio workers and executable SHA-256
`ab3f2fe482dfb48d2915074b04c81a4e037b9a6c02f94e590af07292f68de666`.
No recovery deadline or final ping assertion was changed.

## Passing Packet Run

| Round | Requests / Replies During Pressure | A Final RSS KiB | B Final RSS KiB |
| --- | --- | ---: | ---: |
| 1 | 1971 / 11 | 37340 | 36988 |
| 2 | 1972 / 10 | 37368 | 37164 |
| 3 | 1975 / 11 | 37372 | 37312 |
| 4 | 1975 / 12 | 37372 | 37704 |
| 5 | 1976 / 11 | 37464 | 37344 |

- Each round reached four queued packets / 4112 bytes on both nodes.
- Each ended with empty queues, no in-flight stream requests and 5/5 recovery pings both ways.
- Final total descriptors were 13 per node in every round; daemon identities remained unchanged.
- Sampled-load CPU ranged from 8.00-9.90% of one core on A and 1.90-2.32% on B.
- CPU uses actual sample timestamps and 100 ticks/second, excluding subsequent drain/ping work.
- RSS variation does not identify live allocations or prove retention is bounded.

## Failed Byte Run

The second round's A-to-B recovery ping sent five requests, but only sequences
3, 4 and 5 returned. Node A's queues and in-flight count were zero in the
pre-ping snapshot. They were also zero afterward.

| Counter During Failed Ping | Before | After | Delta |
| --- | ---: | ---: | ---: |
| A TUN reads, packets | 3969 | 3974 | 5 |
| A TUN reads, bytes | 4056352 | 4056772 | 420 |
| A outbound TCP fallback packets | 1129 | 1132 | 3 |
| A queue expiry | 24 | 26 | 2 |
| A queue dropped packets, including expiry | 2835 | 2837 | 2 |
| A outbound failures | 571 | 571 | 0 |
| B inbound accepted packets | 636 | 639 | 3 |
| B inbound drops | 0 | 0 | 0 |

The counts locate the missing requests at A's queue expiry, rather than an
inbound rejection at B. Logs also show a pinned-stream timeout, path demotion
and replacement TCP connection around this recovery phase.

The fixture waits for a selected TCP path before waiting for queues and stream
requests to drain. That earlier selection can become stale. The failed report
did not retain contemporaneous path health, so this remains a hypothesis.

## Diagnostic Controls

The first diagnostic adds observation fields only: healthy TCP paths, peers
without supported paths, blocked-no-path events and per-peer path-state lines.
It does not change the workload, drain condition or final assertions.

That diagnostic passed five byte-profile rounds in 160.91 seconds, using SHA-256
`9e70027ff0a16b7562cabc8e87f91816f5152bce34dc370bb4a3294a29ed4b1b`.
Each pre-ping A snapshot showed one healthy TCP path and no unsupported peer.

The passing diagnostic selected A's listener-side connection; the failed run's
replacement was dialer-side. Random peer identities affect the preferred TCP
initiator. This is a possible confounder, not an established cause.

`P2P_VPN_TUN_E2E_PRESSURE_INITIATOR=a` or `b` now orders fresh test identities
using the runtime's peer-ID byte comparison. Unset preserves random ordering.
Saved replay commands and series metadata retain the override.

The pinned-A diagnostic passed five byte-profile rounds in 210.03 seconds,
using SHA-256
`740dfcb63910f97d90b9ba6299a47b26bda5ce720b4dae5b3ae8bcf87a9a8e7f`.

- All ten pre-ping observations had one healthy TCP path per node and no unsupported peer.
- A's selected connection was dialer-side before every A-to-B recovery ping.
- Initiator direction alone therefore did not reproduce the failure.
- These passes neither prove the stale-readiness hypothesis nor establish a runtime fix.

### Drain-Window Capture

The next diagnostic retains `queue-pressure-drain.json` per round, containing
the existing approximately 250 ms observations through readiness or timeout.
It preserves the queue-only drain predicate, 30-second deadline and strict pings.

| Drain Diagnostic | Result |
| --- | --- |
| Profile / initiator | Bytes / A |
| Rounds / elapsed | 5 passed / 180.02 seconds |
| Drain samples by round | 7, 8, 10, 1, 1 |
| Unsupported peers during captured drain | Zero on both nodes in all samples |
| Final readiness snapshots | One healthy TCP path per node in each round |
| Combined JSON / node logs | 3419605 / 1300475 bytes |

Executable SHA-256:
`2e44cc4f31e9e51e43893a6c4ae65d25be5b3e425541835a7c521bdfddd67b61`.
Snapshot retention and checkpoint I/O can affect boundary timing; this is not
a matched performance comparison or proof that the original failure is fixed.

### Deterministic Readiness Regression

`recovery_requires_current_path_health_after_queue_drain` uses the runtime's
real `PathSet`: establish TCP, observe selection, demote it, then observe empty
queues. The former queue-only predicate incorrectly accepts this state.

| Step | Evidence |
| --- | --- |
| Before correction | Regression fails: `empty queues do not restore a's demoted path` |
| Correction | Require healthy TCP, zero unsupported peers, empty packet/byte queues and zero stream owners on both nodes |
| Positive transition | Re-establishing the path restores readiness |
| Negative cases | Either node demoted, any queue/stream work, unsupported peer or missing observation rejects readiness |
| Verification | 52 unit tests passed; required Clippy groups passed with 37 advisory warnings |

The condition uses the current observation cycle, not the earlier path-selection
result. Queries remain sequential, so it cannot promise a path will remain
healthy afterward; unchanged 5/5 pings still check actual delivery.

- The existing 30-second drain deadline and preceding TCP-path waits are unchanged.
- `queue-pressure-drain.json` now records completion of this strengthened drain condition.
- Production recovery logic and timers are unchanged.
- Negative log: `/tmp/p2p-vpn-sustained-pressure-readiness-negative.log`.
- Passing checks: `/tmp/p2p-vpn-sustained-pressure-readiness-{tests,clippy}.log`.

The corrected single-round byte/A smoke passed in 59.93 seconds. Both nodes
had healthy TCP paths and empty queues/stream owners at readiness, followed
by 5/5 delivery in both directions. This is not a sustained repetition.

| Smoke Evidence | Value |
| --- | --- |
| Artifact suffix | `1.eca19d1fb8ba1c43` |
| Executable SHA-256 | `b49b4f71429dd5f24e08ea43fb9ca48d1ac1de10e5be32af3898e54e750ba028` |
| JSON / node logs, bytes | 663387 / 347858 |
| Outer log | `/tmp/p2p-vpn-sustained-pressure-readiness-smoke.log` |
| Cleanup | Fixture terminal; no matching processes remain |

### Remaining Investigation

The deterministic test proves a fixture flaw, not the original failure's exact
timeline: its pre-ping path-health evidence is missing. Preserve that failure
as unresolved retrospective attribution. The four-run campaign below uses
the corrected readiness condition; any new delivery failure remains actionable.

## Corrected Campaign

Fixture `82980db4` uses executable `b49b4f71429dd5f24e08ea43fb9ca48d1ac1de10e5be32af3898e54e750ba028`.
The [portable summaries](sustained-pressure-samples.json) retain per-round work
deltas, CPU windows, queue observations, process identities and source hashes.

| Run | Profile | Result | Duration |
| --- | --- | --- | ---: |
| 1 | Packets | Five rounds passed | 189.82 s |
| 2 | Bytes | Five rounds passed | 169.43 s |
| 3 | Bytes | Five rounds passed | 249.66 s |
| 4 | Packets | Five rounds passed | 170.38 s |

| Run 1 Round | Pressure Requests / Replies | Final RSS A / B, KiB |
| --- | ---: | ---: |
| 1 | 1975 / 10 | 36740 / 37148 |
| 2 | 1973 / 15 | 36940 / 37228 |
| 3 | 1974 / 10 | 37120 / 37124 |
| 4 | 1974 / 11 | 37244 / 37236 |
| 5 | 1972 / 10 | 37284 / 37164 |

- Queue peaks reached four packets / 4112 bytes on both nodes in every round.
- Every final queue and stream window was empty; recovery pings passed 5/5 both ways.
- Final total descriptors were 13 per node throughout; process identities stayed unchanged.
- Sampled-load CPU ranged from 7.90-9.14% of one core on A and 1.78-2.50% on B.
- A's final three RSS checkpoints rise; the planned allocation-attribution investigation remains required.
- JSON totaled 3419022 bytes and node logs 1332605 bytes, within their respective budgets.
- Capture terminated without matching fixture processes; no builds overlapped observations.

Artifact suffix: `1.e44e49d8c1aabbb5`; outer log:
`/tmp/p2p-vpn-sustained-pressure-corrected-1-packets.log`.
### Full Campaign Summary

All 20 rounds ended with empty queues/stream owners and unchanged daemon
identities. All 40 recovery-ping reports show 5/5 replies. Corrected results do
not establish the missing historical failure timeline retrospectively.

| Capture | A Load CPU, % Core | B Load CPU, % Core | JSON / Node Logs, Bytes |
| --- | ---: | ---: | ---: |
| 1, packets | 7.90-9.14 | 1.78-2.50 | 3419022 / 1332605 |
| 2, bytes | 7.90-9.85 | 1.88-2.57 | 3450859 / 1255369 |
| 3, bytes | 8.04-9.74 | 1.85-2.20 | 3363191 / 1530291 |
| 4, packets | 7.60-9.39 | 1.95-2.17 | 3445877 / 1241835 |

- Packet-profile queue peaks: four packets / 4112 bytes on each node in every round.
- Byte-profile queue peaks: three packets / 3084 bytes on each node in every round.
- Load observations: 13-15 total descriptors, 6-8 socket descriptors and six threads per daemon.
- Final total descriptors: 13 per daemon after every round.
- A's final three RSS checkpoints rose in all four captures; B's also rose in capture 4.
- Source report hashes, actual traffic counts and counter increments are retained in the portable JSON.

Capture 3's last post-load snapshot had no supported path on either node.
Both recovered within the existing path waits and passed strict delivery.
Its longer duration is retained, not discarded or hidden by a timeout extension.

### Stream-Owner Accounting Follow-Up

The aggregate `packet_stream_fallback_in_flight` peak was 257 on A in captures
1 and 2. Data admission uses a per-peer limit of 256. This is not proof of
257 simultaneously open transport streams: the gauge counts tracked requests.

| Source Evidence | Implication |
| --- | --- |
| `PacketInFlight::can_send` in `src/runtime/runner.rs` | Data admission checks the per-peer total |
| `send_path_probes` and `record_path_probe` | Stream probes enter the same request tracker without that check |
| Five-second probe timer; 15-second request expiry | Time-based controls exist; a strict aggregate bound is not established by these samples |

- Verify saturated data admission plus repeated probes with a deterministic ownership regression.
- Establish or correct the probe reserve and expiry bound without starving recovery.
- Do not classify the extra request as either harmless or an unbounded leak from this observation alone.
- Allocation attribution and the owner-accounting finding remain open after S4 measurement completion.

### Verification and Cleanup

- Fixture revision and binary hash stayed unchanged across all four captures.
- Series checkpoints match corresponding round reports; portable summaries were derived from raw reports.
- Every readiness trace ended before its deadline with healthy TCP and empty queues/stream owners.
- All captures exited successfully and left no matching fixture process.
- No builds, deployments, physical devices or public-network tests occurred during the campaign.

## Diagnostic Validation

| Check | Result |
| --- | --- |
| Namespace unit tests | 51 passed; 23 opt-in integration cases ignored |
| Pinned-A namespace integration | Five pressure/recovery rounds passed |
| Drain-window namespace integration | Five pressure/recovery rounds passed; 51 unit tests also passed |
| Required Clippy groups | Correctness, suspicious and performance passed; 37 advisory warnings remain |
| Rust formatting | Passed |
| Production/platform gates | Not rerun: changes are namespace-test diagnostics and developer documentation only |

## Evidence

Artifact prefix: `/tmp/p2p-vpn-tun_namespace_recovers_after_tcp_queue_pressure-`.

| Capture | Suffix | Principal Evidence |
| --- | --- | --- |
| Campaign packets | `1.78581c27f5f923be` | Complete series and five round reports |
| Campaign bytes | `1.a93f736c9be7df9a` | Incomplete series, round-two partial report and failed ping snapshots |
| Observation-only diagnostic | `1.940022bd725a5c46` | Five rounds with pre-ping path state |
| Pinned-A diagnostic | `1.ad3f49791461f1ea` | Five rounds, initiator override and pre-ping path state |
| Drain-window diagnostic | `1.6c9c7cdd4d2c8002` | Five rounds and bounded drain traces |

- Passing series SHA-256: `ca9934318243afe3195f780bea09ddf6d55236c6d6ce00156621bab42b9226c8`.
- Failed series SHA-256: `039ed26d1b8ddeac8201eb321af99470c2e1891735b653fc19540f2bdd0a6ef6`.
- Failed ping JSON SHA-256: `1f720277a38ab7640ad43db8718b6ed0e5456bd569a56ebb230f02bf336af824`.
- Pinned-A series SHA-256: `b3f7cf8fc9f3513618b1068cb100873e0bbfd31dccb47fc1c3fa5c2f32f9fe0c`.
- Drain-window series SHA-256: `8f26ff46a53e087cff0c6c61d0cb4da9bc7909b4798a2100f4e93c49bc064d7c`.
- Outer logs: `/tmp/p2p-vpn-sustained-pressure-{1-packets,2-bytes,path-diagnostic,initiator-a-diagnostic}.log`.
- Drain logs: `/tmp/p2p-vpn-sustained-pressure-drain-{diagnostic,tests,clippy}.log`.
- Campaign node logs total 1243784 and 532673 bytes respectively, below 2 MiB each.
- Pinned-A node logs total 1422965 bytes, also below 2 MiB.
- All five captures terminated and left no matching fixture process; failed evidence is preserved.
- No production code, recovery timers, physical host or public-network route was changed.
