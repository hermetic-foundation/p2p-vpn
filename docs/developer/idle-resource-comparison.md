# Controlled Idle Comparison

Recorded 2026-09-06. This compares an isolated integration-test workload, not
packaged daemons, public DHT operation, or Android power consumption.
The original comparison uses debug builds; the [release follow-up](#release-profile-follow-up)
records current-only optimized observations separately.

## Subjects

| Subject | Runtime Source | Fixture Source |
| --- | --- | --- |
| Baseline | `560754de` | `c2e8a1f7` |
| Reviewed | `2dfa965a` | `c2e8a1f7` |

The baseline used a separate Jujutsu workspace. Only `tests/tun_namespace.rs`
and `tests/support/idle_sample.rs` were copied from the fixture revision.
Baseline `src/`, `Cargo.toml`, and `Cargo.lock` were unchanged.

Both revisions have identical Cargo manifests and lockfiles. Fixture file hashes
matched across workspaces. Clean package rebuilds reproduced both executable
hashes used in the captures.

## Workload

| Setting | Value |
| --- | --- |
| Topology | Two network namespaces; direct UDP; no Internet route |
| Membership | One static peer per node; empty signed-membership ledger |
| Discovery | Fixture defaults retained; external connectivity unavailable |
| Warmup / sample | 30 / 60 seconds, after verified packet forwarding |
| Sampling | 61 observations per node; approximately one-second intervals |
| Runtime metrics | Logged every second |
| Compiler | Nix Rust 1.97.1, `8bab26f4f` |
| Build | Unoptimized; dev/test debug information and incremental compilation off |
| Build limits | Two Cargo jobs; shared dependency cache; offline |
| Execution | No task builds during captures |
| Host | Linux 6.18.47; 16 available CPUs; 100 clock ticks per second |

The sampler observes process CPU, RSS, threads, socket descriptors, TCP states,
and boundary daemon counters. It generates no payload traffic during sampling.
See [sampling instructions](testing.md#controlled-idle-sampling).

## Results

CPU values are percentages of one core. RSS ranges are KiB, not allocation counts.

| Run | A CPU | B CPU | A RSS Range | B RSS Range | Host 1-Minute Load, Before / After |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline 1 | 0.250% | 0.250% | 34,400-34,428 | 33,916-34,104 | 3.63 / 3.20 |
| Baseline 2 | 0.267% | 0.267% | 34,196-34,264 | 34,304-34,332 | 3.27 / 3.81 |
| Reviewed 1 | 0.250% | 0.233% | 34,756-34,804 | 34,300-34,320 | 3.98 / 3.43 |
| Reviewed 2 | 0.250% | 0.250% | 34,836-34,880 | 34,316-34,372 | 3.10 / 3.50 |

These observations were identical across successful runs:

| Observation | Node A | Node B |
| --- | ---: | ---: |
| Threads | 20 | 20 |
| Socket descriptor range | 13-14 | 11-12 |
| New direct connections | 1 | 1 |
| Redial attempts | 0 | 1 |
| New outgoing connection errors | 0 | 0 |
| Path probes sent | 12 | 12 |
| Probe failures / path demotions | 0 / 0 | 0 / 0 |
| New sent / accepted payload packets | 0 / 0 | 0 / 0 |

Both boundary snapshots selected validated direct UDP paths. CPU differences are
only one or two clock ticks per interval; one tick is about 0.017 percentage points.
These runs do not establish a meaningful CPU improvement or a large CPU regression.

Node A's reviewed RSS was consistently higher. Its peak was 376 and 616 KiB higher
in the corresponding runs. This is a retained footprint observation, not zero
memory cost or proof of a leak; the cause and release-profile impact are unmeasured.

## Failed Attempt

Baseline required three startup attempts to produce two idle captures. The middle
attempt failed at the unchanged 30-second UDP-session deadline. No idle report
was produced, and no timeout or baseline runtime code was changed to obtain a pass.

The log contained one sent UDP hello followed by pending-hello expiry, without a
replacement before failure. This is consistent with the previously fixed timeout
recovery gap, but the sample does not independently prove that causal attribution.

The two reported reviewed runs passed. Successful idle measurements are conditional
on startup succeeding; they must not conceal this baseline failure or be used to
estimate general startup reliability from this small sample.

## Evidence

Reports are retained under `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-<ID>/`.

| Run | Directory ID | Artifact |
| --- | --- | --- |
| Reviewed 1 | `1183087` | `idle-sample.json` |
| Baseline 1 | `1190122` | `idle-sample.json` |
| Reviewed 2 | `1192085` | `idle-sample.json` |
| Baseline failed attempt | `1194598` | Node logs and daemon snapshots; no idle report |
| Baseline 2 | `1196744` | `idle-sample.json` |

Executable SHA-256 values:

```text
baseline: dad671fbeeabe75669ed791c5b09a51e398708f1db95be18ee5dc9ce48a03c22
reviewed: 4a599126d487379378e934e9defa653971f522ec60480c2702d3a573cee1e6e5
```

## Reproduction Notes

- Pin both runtime and fixture revisions; do not silently use a future fixture.
- Keep toolchain, build flags, warmup, duration, and logging interval identical.
- Use separate target directories, or run `cargo clean -p p2p-vpn` when switching
  workspaces in a shared task target. Verify executable hashes before capturing.
- Retain failed attempts separately from measurements of successfully started nodes.
- Keep raw samples and account for host load and clock-tick resolution.

Shared-target reuse returned a stale baseline executable after switching back to
the reviewed workspace. Package-scoped cleaning preserved third-party dependencies;
independent clean rebuilds reproduced both recorded hashes.

## Remaining Scope

- Larger signed-membership ledgers and multiple network instances.
- A matching release-profile baseline and attribution of the observed RSS difference.
- Public-discovery failure pressure and Android lifecycle/power behavior.

The broader review goal remains active. This comparison supplies limited idle
evidence; it does not establish production readiness or finish runtime ownership work.

## Release-Profile Follow-Up

### Method

- Runtime and fixture: `89709e4f`; Nix Rust 1.97.1; Cargo release defaults; incremental compilation disabled.
- Same two-node direct-UDP topology, empty signed ledger, 30-second warmup, and 60-second sample.
- Two successful captures, each with 61 observations per node. No task builds during sampling.
- Current-only evidence: no matching release baseline was measured. Do not attribute debug/release differences to a revision change.
- [Derived summaries](release-idle-resource-summary.json) retain raw artifact paths, hashes, counter boundaries, and executable identities.

### Results

CPU is a percentage of one core. RSS is KiB and includes the integration-test
process running the daemon; it is not an allocation counter or packaged-CLI footprint.

| Run | A CPU | B CPU | A RSS Range | B RSS Range | Host 1-Minute Load, Before / After |
| --- | ---: | ---: | ---: | ---: | ---: |
| Release 1 | 0.150% | 0.167% | 19,444-19,448 | 19,484-19,500 | 1.73 / 1.21 |
| Release 2 | 0.150% | 0.133% | 19,600-19,620 | 19,220-19,224 | 0.85 / 0.67 |

Both captures had these observations during the measured minute:

| Observation | Node A | Node B |
| --- | ---: | ---: |
| Threads | 20 | 20 |
| Socket descriptor range | 13-14 | 11-12 |
| New direct connections | 1 | 1 |
| Redial attempts | 0 | 1 |
| New outgoing connection errors | 0 | 0 |
| Path probes sent / failed | 12 / 0 | 12 / 0 |
| New sent / accepted payload packets | 0 / 0 | 0 / 0 |
| TUN reads / unauthorized-source drops | 1 / 1 | 1 / 1 |

- Each node retained its process start time; both boundary snapshots selected a healthy direct UDP path.
- Stable socket counts do not mean zero reconnection work. Connection and redial increments remain visible above.
- The dropped packets were not captured; their protocol/source is not established by these counters.
- Startup connection errors predate sampling: 35 per node in run 1; 10 on A and 35 on B in run 2. None were added during capture.
- Short, isolated samples do not prove absence of leaks, public-discovery storms, or sustained-load regressions.

### Artifacts and Failed Setup

| Attempt | Directory ID | Result |
| --- | --- | --- |
| Setup failure | `2011119` | No idle report; missing CLI helper after package cleanup |
| Release 1 | `2014856` | Passed; `idle-sample.json` retained |
| Release 2 | `2016577` | Passed; `idle-sample.json` retained |

Directories use `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-<ID>/`.
The failed attempt's node logs show live control sockets, but the harness could
not execute the removed `CARGO_BIN_EXE_p2p-vpn` helper to query them.

Rebuilding the matching current CLI restored the helper. Runtime source, fixture
assertions, and deadlines were unchanged. The failure log remains at
`/tmp/p2p-vpn-review-release-idle-1.log`; no successful sample hides that attempt.

All capture processes exited. No physical device, public relay, or deployed
service was changed for these measurements.
