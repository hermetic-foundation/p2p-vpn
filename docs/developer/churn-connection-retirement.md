# Churn Connection Retirement

## Status

The first full S5 capture failed in cycle three. A deterministic dispatch
regression reproduces a failed packet connection vetoing a fresh replacement.
The targeted correction passes dispatch regressions and an isolated live smoke.
Both corrected ten-cycle repetitions now pass on runtime `7b59625f`;
see [paired results](lifecycle-churn-results.md). RSS attribution remains open.

## Frozen Failure Evidence

| Control | Value |
| --- | --- |
| Fixture | `d26d1c38`; production runtime `11416a8d` |
| Executable SHA-256 | `31ac71768df4c6a24a052de47bff25daa43400141134dafde76c6b7ffc3542a5` |
| Command | Full ten-cycle command in [churn manifest](lifecycle-churn-review.md#commands) |
| Outcome | Exit 101 after 312.48 seconds; no rescue or deadline extension |
| Cycle 1 | Recovery 23.9934 seconds; 5/5 both ways |
| Cycle 2 | Recovery 20.8882 seconds; 5/5 both ways |
| Cycle 3 | No successful probes among 39 observations; sixty-second recovery failed |
| Final traffic | Not attempted in cycle three; empty results are not success |
| Capture quality | All six completed resource windows have complete runtime samples |
| Data volume | 1,948,004 bytes JSON; 585,375 bytes combined node logs |
| Teardown | Outer runner terminal; no matching namespace processes remain |

Each outage has 62 OS rows and twelve runtime rows across both nodes. Each
recovery window has 82 OS rows and sixteen runtime rows. These windows end at
forty seconds; cycle three's recovery probes continue to the original deadline.

The retained summary reports two completed cycles and `complete=false`.
Neither repetition is satisfied by this failed run. Final settling did not run.

## Connection Timeline

1. Both nodes mark their UDP paths unhealthy during the third outage.
2. Pinned TCP packet streams time out on connection 30 at both ends.
3. Packet path inventory removes connection 30, but its epoch remains usable.
4. Four fresh connections establish and are immediately closed as duplicates.
5. Both nodes retain validated peers but report no supported packet path.

Node A rejects connection IDs 43-46. Node B rejects the matching listener
connections 42-44 and 50. Each side prefers the older connection's handshake
role, despite its packet-stream failure.

This is not evidence that the old TCP connection was unusable for every protocol.
AutoNAT control messages still appear. The mismatch is between failed packet
path inventory and the connection eligibility used by deduplication.

## Resource Observations

| Post-Recovery / Failure Checkpoint | A RSS KiB | B RSS KiB | A Total / Socket FDs | B Total / Socket FDs |
| --- | ---: | ---: | --- | --- |
| Cycle 1 | 35992 | 36012 | 21 / 14 | 19 / 12 |
| Cycle 2 | 36012 | 36048 | 21 / 14 | 19 / 12 |
| Cycle 3 failure | 36036 | 36084 | 21 / 14 | 19 / 12 |

Both processes keep six threads and their original PID/start identities. These
three checkpoints do not establish a plateau or prove allocation ownership.
Whole-runtime allocation attribution remains a separate open requirement.

One retiring UDP session per peer appears after renewal. The implementation
stores one retiring entry per peer and preserves its original session age;
this bounded ownership is distinct from the failed TCP deduplication decision.

## Regression and Correction

- The regression dispatches an owned pinned packet failure through production handling.
- It then presents a fresh connection with the opposite handshake role.
- Before correction, deduplication selects that fresh connection for closure.
- The correction retires the exact failed connection before recovery redial.
- Capacity rejection, stale epochs and mismatched ownership must not retire unrelated connections.

The synthetic regression tests state-machine eligibility, not socket delivery.
Live captures must verify actual close requests and autonomous recovery.
Authorization, discovery policy, addresses and membership are unchanged.

## Artifacts

Directory:
`/tmp/p2p-vpn-tun_namespace_measures_lifecycle_churn_resources-1.c9ba49ad452e65c9`.

| Artifact | SHA-256 |
| --- | --- |
| `/tmp/p2p-vpn-lifecycle-churn-full-1.log` | `f762e2f8cdd4c39fb361e8dec3ecd0434aeb73b45236c5803ffe41f36c45dda3` |
| `node-a.log` | `52faf30736b33f470e9e0b31107993c197698c75b116e99c20b989231f3cb673` |
| `node-b.log` | `51373250d4c72d3ee2e1c3f0e20adc5a04892ba37784e0ff84ec96850ee8879b` |
| `churn-03-recovery.json` | `098525e50e949084d740fa7e32c0eca580dd4615d15b665f5fde4ed68d44218a` |

Negative dispatch log: `/tmp/p2p-vpn-churn-retirement-before-run.log`.
An earlier compile-only attempt used a nonexistent identity helper; its log is
`/tmp/p2p-vpn-churn-retirement-before.log`, not negative runtime evidence.

## Correction Validation

Retirement requires an owned in-flight packet request, matching peer/path/relay
metadata in the active connection map, and a usable current epoch. Only stream
upgrade or I/O failures qualify; local capacity rejection is excluded upstream.

The existing retirement set excludes the failed connection from duplicate
selection immediately. Its normal close event removes the marker. The change
adds no owner collection, periodic task, retry loop or protocol/configuration field.

| Gate | Result |
| --- | --- |
| Original dispatch regression | Failed before, passed after; fresh opposite-role connection is no longer vetoed |
| Ownership matrix | I/O, capacity, unowned request, stale epoch, absent active entry, wrong peer/path, non-transport failure, already retiring |
| Duplicate events | Each matrix case dispatched twice; replacement and owner accounting preserved |
| Workspace | Offline locked tests: 1507 passed, 40 opt-in tests ignored |
| Static checks | Workspace/all-target correctness, suspicious and perf Clippy groups pass; advisory warnings remain |
| Format | Cached rustfmt checks pass |
| Packaged source | Cached Nix source-check script passes outside sandbox; changed files match Linux and Android source sets |
| Android | Cached offline x86_64/API-26 native build passes in 42.01 seconds; no phone deployment |
| Formal verification | No Lean/TLA/Alloy assets found; dispatch regressions are not formal proofs |

Validation logs use `/tmp/p2p-vpn-churn-retirement-` with suffixes
`after-corrected.log`, `guards.log`, `workspace.log`, `clippy.log`,
`final-source-check.log` and `android.log`.

`after-run.log` is a compile-only intermediate attempt, not passing evidence.
The Android JNI SHA-256 is
`88bbe21ef0948b93b2116a5cb30f2d460bcf1662733ea2bdac8a6ba9587bc203`.

### Live Branch Evidence

The unchanged one-cycle smoke passed in 188.74 seconds. Both nodes logged
`pinned_stream_connection_retired` with `close_requested=true` for connection 2.
Recovery took 22.3881 seconds without intervention.

- Per-cycle and post-settling traffic: 5/5 replies in each direction.
- Both daemon PID/start identities unchanged; all three runtime series complete.
- Final status: one healthy UDP path per node; empty queues, no pending connection attempts or retirement markers.
- OS rows across both nodes: 62 outage, 82 recovery, 122 settling.
- Runtime rows across both nodes: 12 outage, 16 recovery, 24 settling.
- Outer runner exited zero; no matching namespace processes remained.
- No build overlapped capture; `proof_eligible=false` because this is one cycle.

This exercises actual connection retirement and recovery. It does not establish
ten-cycle stability, a latency improvement, release behavior or WAN migration.
The failed full run and successful smoke are not equal-work CPU comparisons.

| Corrected Smoke Artifact | SHA-256 |
| --- | --- |
| Namespace executable | `3f91216df32982e13bea93844c978ee0d461832c390941fc1bf175867c6df4c6` |
| `churn-01-outage.json` | `6ab6a64e8d13fc20f450ea09c833cc61cbe84a06063d1d7d82021b69f8a606fd` |
| `churn-01-recovery-sample.json` | `ec0014ff5b0e6dd31437315d904b1a04c777e4b19e5e4b1eb7bb0a2e02b42530` |
| `churn-01-recovery.json` | `c1deef125c55e3730976963c6ebd2a82c29b181a4b48e7dce2bfa79f2f46ff9a` |
| `churn-settle.json` | `b58876b29046ee95029c65325fb047b9e481d07f5017aec1167043f738ad1c46` |
| `lifecycle-churn.json` | `f84de7bc945cd7cffbe3f2b3142372eca26e7be38705056986c61b50aa1ad50e` |
| Outer log | `6ba9bc26fcdcd7d477294c30c3fb17877534eddbda445225b6db599bd33648f4` |
| Negative dispatch log | `28dd77c9744d0b665c25517d6a8a4b3f5d57dd25335e6070f65037adacc05327` |

Smoke directory:
`/tmp/p2p-vpn-tun_namespace_measures_lifecycle_churn_resources-1.6a9476981cb48827`.
Outer log: `/tmp/p2p-vpn-churn-retirement-smoke.log`.

Storage before native validation was 8,725,176 KiB across `/tmp/p2p-vpn-*`,
below the 10 GiB limit. No physical host, personal flake or device was changed.
