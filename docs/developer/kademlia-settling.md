# Sustained Recovery And Healthy Settling

## Status

Phase 2 is complete. Starting revision: `63a380cbcd1a`.
Verified runtime and regression changes are published in `a25f78dd8723`.
Both final sustained profiles passed on the same binary, including five recovery
cycles, continuous healthy traffic, UDP renewal, owner cleanup, and quiet settling.

The final evidence below supersedes the historical checkpoints and failures.
Phase 3 [resource comparisons](kademlia-resource-final-report.md) are complete
with limitations. The [phase-4 audit](kademlia-workstream-acceptance.md) records
the unresolved later direct-promotion result and deferred overall acceptance.

See the [workstream plan](kademlia-resource-plan.md) and the completed
[aggregate ownership audit](kademlia-final-ownership-audit.md).

## Scope

| Included | Separate Work |
| --- | --- |
| Autonomous recovery across repeated faults and network changes | Before/after resource comparisons: phase 3 |
| Healthy maintenance without redundant retry activity | Original-requirement final acceptance: phase 4 |
| Retained-owner bounds throughout recovery and settling | Physical WAN, phone, and personal-flake deployment |
| Minimal configuration, LAN-first, relay fallback, TCP and QUIC | Unrelated refactoring and production-readiness claims |

## Required Evidence

| Case | Required Outcome | Status |
| --- | --- | --- |
| Unavailable bootstrap/routing peers | Backoff survives long failures; useful discovery resumes automatically | Passed: explicit-time capped backoff plus unavailable-startup and complete-outage recovery in both profiles |
| Failed or stale relay | Retire failed attempts; discover or select a usable alternative | Passed: both profiles replaced unavailable R1 with R2 and recovered after the complete outage |
| Repeated network/address changes | LAN-first discovery, relay fallback, eventual direct recovery where available | Passed: five cycles per final profile, including changed and restored LAN addresses |
| Foreground/background contention | Preserve unrelated VPN traffic; retire stale owners; resume after release | Live loopback passes in both profiles over TCP and QUIC; see scoped gate below |
| All overlay peers healthy | Suppress redundant queries/dials; retain legitimate maintenance and fresh records | Passed: final healthy windows exceed ten minutes, with unchanged redundant-activity counters and bounded library maintenance |
| Shared-public and separate-public-pairing DHTs | Same bounded behavior with independent budgets | Passed: contention matrix and complete sustained matrix in both profiles |
| AutoNAT with periodic maintenance disabled | No stranded owner or event-driven query storm | Passed: cadence, policy, capacity and cleanup regressions; production AutoNAT included in both sustained runs |
| Deterministic long timeline | At least 24 simulated hours with timer-boundary assertions | Eight explicit-time tests pass, including capped backoff and cleanup interactions |
| Real-runtime soak | At least 30 minutes, five fault/recovery cycles, ten continuous healthy minutes | Passed: private 1,801.32s / 602.52s healthy; public 1,801.32s / 682.91s healthy |
| Negative control | The tests detect suppressed recovery or uncontrolled retries | Reproduced missing renewal, uncontrolled admission, unaccounted relay errors, and failure to restore LAN UDP |

Topology changes are allowed during fault tests. Daemon restarts, configuration
edits, peer-address injection, manual dialing, and management-plane rescue are
not recovery evidence. Packet delivery and path state must be checked throughout.

## Timer-Derived Thresholds

These are existing scheduler contracts, not measured performance. Tests must
distinguish admitted work from attempted work and inspect retained owners, not
just active-query iterators. Polling allowances assume a serviced event loop.

| Owner | Deadline / Cadence | Healthy Expectation |
| --- | --- | --- |
| Ordinary maintenance | One owner; at least 120 seconds between starts; 90-second owner timeout | Cancel within the next 5-second maintenance tick; no new lookup cycle while suppressed |
| Redial-only maintenance cleanup | 90-second timeout plus at most one 10-second redial interval | No stranded AutoNAT-owned query when periodic maintenance is disabled |
| Targeted peer recovery | One concurrent query; 60-second timeout | Cancel within the next 10-second redial tick once all peers are healthy |
| Targeted recovery failures | 30-second exponential backoff, capped at one hour | Quiet suppression must not reset failure history |
| Default public bootstrap failures | 30-second exponential backoff, capped at ten minutes | Default-bootstrap dialing is suppressed when all peers have usable paths |
| Recovery dial target failures | 10-second exponential backoff, capped at five minutes | No redundant overlay dialing for an already usable path |
| Address-change publication | One separate owner; at least five seconds between starts | Preserve the latest update and renew signed freshness without full rediscovery |
| Default-bootstrap LAN-first grace | 60 seconds at startup and recognized network changes | Do not substitute public lookup for the grace period |
| Path health probes | Five-second cadence; 12-second probe timeout | Probe traffic is legitimate maintenance, not a discovery storm |
| Automatic relay reservation attempt | 30-second pending timeout; default retry interval 30 seconds | Distinguish failed acquisition from renewal of an accepted reservation |

Sources: [runner timers and owners](../../src/runtime/runner.rs),
[recovery-query owner](../../src/runtime/recovery_queries.rs), and
[configuration defaults](../../src/config.rs).

The fixture-specific recovery and settling budgets below were frozen from these
timers and the topology before acceptance. Failures were fixed without widening
the recovery deadlines, observation durations, or maintenance budgets.

### Automatic Discovery Fixture

The initial fixture is one cycle, not the acceptance soak. Freeze these deadlines
before its first run. A failure requires investigation, not a larger timeout.
These are serviced-loop fixture budgets, not a general public-network recovery SLA.

| Stage | Budget | Derivation / Constraint |
| --- | --- | --- |
| Initial LAN | 120 seconds | 60-second discovery window, 10-second redial, 25-second hello, 10-second QUIC attempt, 15-second observation allowance |
| Lost LAN to discovered relay | 960 seconds | 60-second LAN grace, 600-second bootstrap backoff, 120-second maintenance cadence, 60-second lookup, 30-second reservation, 25-second hello, 10-second redial, 17-second probe, 38-second observation allowance |
| Relay to restored LAN | 375 seconds | 300-second address retry cap, 10-second redial, 25-second hello, 10-second QUIC attempt, 17-second probe, 13-second observation allowance |
| Per-stage healthy observation | 30 seconds | Sample every five seconds and require bidirectional 5/5 ICMP plus packet-counter growth |
| Orchestrator | 1,650 seconds | Stage budgets total 1,545 seconds; 105 seconds for namespace setup, readiness and final diagnostics |
| Retained ordinary/publication query age | 100 seconds | 90-second owner expiry plus one 10-second cleanup interval |
| Retained targeted query age | 70 seconds | 60-second expiry plus one 10-second cleanup interval |
| Pending packet hello age | 35 seconds | 25-second expiry plus one 10-second cleanup interval |

The targeted-query one-hour failure cap has separate 24-hour explicit-time
coverage. This fresh fixture does not claim its 960-second budget bounds every
possible accumulated cooldown history.

| Fixture Property | Enforcement |
| --- | --- |
| Minimal peer configuration | Local identity, network name, remote peer ID; one isolated bootstrap override |
| Public profile | Default discovery, packet plane, queues, relay policy and runtime entry point |
| Private profile | Only the primary DHT protocol differs; public pairing DHT remains separate |
| Initial unavailable infrastructure | Bootstrap/relay bridge port down while peers establish the direct LAN path |
| Automatic fallback | Remove direct link and restore infrastructure; no addresses, reservations or dials injected |
| No alternate underlay | Isolated edge bridge ports, forwarding off, bridge-side IPv6 disabled, reachability assertions |
| No rescue | PID/start-time and serialized configuration checks throughout the cycle |
| Bounded evidence | 32 MiB kernel file-size limit per child log; bounded samples and latest snapshots |

The controlled infrastructure process serves Kademlia and circuit relay. It is
not an authorized overlay member. Its server mode and Identify address ingestion
are infrastructure setup, not recovery actions on the two overlay daemons.

```sh
nix develop -c cargo test --offline --locked --test tun_namespace --no-run
# Run the built test executable separately; do not build during observation.
P2P_VPN_TUN_E2E_KEEP_TEMP=1 P2P_VPN_TUN_E2E_RECOVERY_PROFILE=public \
  "$TEST_BINARY" --ignored --exact \
  tun_namespace_automatic_discovery_recovers_after_link_changes --nocapture
```

Repeat with `P2P_VPN_TUN_E2E_RECOVERY_PROFILE=private`. Timeout/scale overrides
are rejected. `recovery-summary.json` labels this as `acceptance_soak: false`;
`recovery-samples.jsonl` retains path, process and resource observations.

### Sustained Fixture Matrix

Enable `P2P_VPN_TUN_E2E_RECOVERY_SOAK=1` for the same ignored test.
The default remains the short one-cycle fixture. Both modes reject timeout
overrides. The following matrix and budgets are frozen before the first soak.

| Cycle | Fault | Required Recovery |
| --- | --- | --- |
| 1 | Initial infrastructure unavailable; then remove direct LAN and release R1 | Discover R1, deliver over its relay, then return to LAN UDP |
| 2 | Remove LAN; change its addresses from `10.253.0.x` to `10.254.0.x` | Relay fallback, then discover the new LAN endpoints |
| 3 | Remove LAN and R1; release previously unavailable R2 | Deliver through R2 specifically, then return to LAN |
| 4 | Remove LAN and both infrastructure paths for 130 seconds; release R2 | Expire stale owners and recover through R2, then return to LAN |
| 5 | Remove LAN and restore its original addresses | Relay fallback, then restore UDP on `10.253.0.x` |

R1 and R2 start before any fault. The only extra configuration is a second
isolated bootstrap override; overlay peers remain ID-only. PID/start-time and
configuration checks reject daemon replacement or manual recovery configuration.

| Limit | Frozen Value / Derivation |
| --- | --- |
| Recovery stages | Unchanged initial 120s, relay 960s, direct 375s; each successful stage has a 30s healthy dwell |
| Infrastructure outage | 130s: 90s owner timeout + 10s cleanup + 30s relay attempt interval |
| Final settling grace | 100s: 90s owner timeout + 10s cleanup; packet delivery remains mandatory |
| Continuous final healthy window | At least 600s; extend it until the complete run reaches 1,800s |
| Orchestrator watchdog | 8,060s: 120 + 30 + 5 * (960 + 30 + 375 + 30) + 130 + 100 + 600 + 105 |
| Ordinary and targeted query owners | Zero throughout the final healthy window; public discovery remains suppressed |
| Redundant application activity | Zero new redials, provider lookups/dials/advertisements, membership lookups/publications, bootstrap refreshes, or relay acquisitions |
| Primary query phases | At most `4 + 2 * floor(healthy_seconds / 900) + 6 * crossed_hours` |
| Separate pairing query phases | Zero: this fixture has no pairing operation or pairing records |
| DHT dial intents | At most eight per allowed phase: five built-in bootstrap identities, two relays, one overlay peer |
| Healthy capacity rejection | Zero new query-pool rejections |

The four initial phases allow one signed renewal and one trailing address
retirement, each with lookup and put phases. Further renewals have a 900s cadence.
Each crossed hour allows replication of at most three regular records.

The library replication cadence is 3,600s; record and provider republishing
intervals are 22h and 12h. Automatic and periodic Kademlia bootstrap are disabled.
The frozen watchdog cannot reach either republishing interval.

Every sample checks retained resources and owner ages. Recovery requires exact
bidirectional 5/5 delivery plus ingress-counter growth; relayed delivery also
requires relay-counter growth. Any loss during a healthy dwell fails the run.

Expected loss while a stale path is being replaced does not immediately fail
the recovery stage, but does not establish success or extend its deadline.
Malformed diagnostics, command failures, and counter regression still fail.

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 P2P_VPN_TUN_E2E_RECOVERY_PROFILE=public \
  P2P_VPN_TUN_E2E_RECOVERY_SOAK=1 "$TEST_BINARY" --ignored --exact \
  tun_namespace_automatic_discovery_recovers_after_link_changes --nocapture
```

Repeat with the private profile. No build may run during either observation.
Summaries record completed cycles, continuous healthy duration, and outcome;
`acceptance_soak: true` describes the requested mode, not a passing result.

Foreground/background query saturation with unrelated VPN traffic remains a
separate acceptance case. This topology does not manufacture query overload.

#### First Sustained Attempt

- All five cycles completed; last direct recovery at 886.4s.
- The run failed at the initial final-healthy sample, 1,016.4s, on an incorrect backoff assertion.
- `app_public_discovery_suppressed` is the failure-backoff timer, not healthy-peer quiet mode. Expired backoff is valid while healthy.
- The corrected oracle checks all intended peers have usable paths, zero ordinary/recovery owners, and unchanged redundant-activity counters, including AutoNAT discovery.
- Recovery deadlines, observation durations, and phase/dial budgets are unchanged. No runtime behavior changed for this correction.

Artifacts: `/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.219fc69fc843b23c`.
Log: `/tmp/p2p-vpn-settling-sustained-public-topology-fixed.log`.
This failed 1,025.15s attempt is not a completed 30-minute soak.

### Healthy Control Connection Churn

The corrected public-profile run passed all five recovery cycles, then failed
the unchanged zero-redundant-redial assertion during the final healthy window.
UDP remained healthy; both peers' redial counters increased by one.

| Evidence | Interpretation |
| --- | --- |
| Direct and relay control connections closed while UDP probes continued | Separate UDP traffic does not keep libp2p connections active |
| New relay/direct connections followed those closures | The redial scheduler restored the control plane |
| Production libp2p idle timeout is 60 seconds | Healthy idle control connections need explicit, scoped retention |

- Initial fix retained one validated control connection per authorized VPN peer; review below replaces local-ID selection with a shared transport preference.
- Release retention when authorization, validation, path health, or the live connection disappears.
- Keep public infrastructure idle expiry unchanged; do not add wire heartbeats or suppress necessary control-plane recovery.
- The earlier public-profile run passed with this fix; both final runs remain pending after review corrections.

Failure artifacts: `/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.de44b3cbd9c8215e`.
Run log: `/tmp/p2p-vpn-settling-sustained-public-oracle-fixed.log`.

#### Pre-Review Retention Checkpoint

| Check | Result |
| --- | --- |
| Focused retention tests | 11 passed, including TCP/QUIC idle retention, release, and unretained negative controls |
| Policy selection | Authorized and validated peers only; healthy live stream IDs; one connection per peer; direct preferred |
| Owner lifetime | Policy updates coalesce per live connection; exact closure removes pending notification state |
| Workspace | 1,390 passed; 24 opt-in tests excluded from this command |
| Formatting and whitespace | Passed |
| Clippy correctness, suspicious-code, performance | Passed; advisory warnings remain |
| Android native | Cached x86_64/API 26 compilation passed; four target warnings remain |
| Nix source integration | Cached source check and byte-for-byte runtime/support source comparison passed |
| Namespace gates | 13 passed in 257.54 seconds |
| Sustained acceptance | Public profile passed; separate-public-pairing profile pending |

Logs: `/tmp/p2p-vpn-retention-{final-focused,workspace,clippy,android,nix}.log`.
The loopback idle tests use a 200 ms idle timeout, not production settling timers.
This checkpoint does not establish APK, ARM64, physical-device, WAN, full Nix
package, or formal-proof acceptance.

#### Earlier Public Sustained Run

| Requirement | Evidence |
| --- | --- |
| Terminal result | Passed; test process exited zero after 1,808.69 seconds including teardown |
| Runtime duration | 1,801.32 seconds |
| Recovery cycles | All five passed without management rescue |
| Continuous healthy window | 798.60 seconds |
| Assertions | Packet delivery, selected paths, unchanged process/configuration, owner bounds, and settling checked throughout |
| Redial regression | Prior failed run reproduced idle churn; retention run passed the unchanged oracle |
| Build isolation | No task builds during observation |

Artifacts: `/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.1fb979b50b304de4`.
Log: `/tmp/p2p-vpn-retention-sustained-public.log`.
Binary SHA-256: `864dce9887bf7188c1c22874ec2e440f43fa0d9138a0e1452e7813f99ff56d0c`.

This is controlled public-protocol infrastructure, not public-internet/WAN
acceptance. It predates the review corrections below and is not final acceptance.

#### Review Corrections

| Finding | Required Correction |
| --- | --- |
| Opposing TCP/QUIC connection-ID order | Choose the same transport preference at both endpoints; retain eligible connections within existing per-peer bounds |
| Selected UDP snapshot could hide stream fallback | Check actual datagram counter growth, no stream fallback, and the post-packet selected path |
| Balanced retained query count could remain stranded | Require library-owner drain within a bounded interval; pairing DHT remains empty during the no-pairing healthy window |

The added primary-pool drain budget is 250 seconds: four allowed query phases
at the production 60-second query timeout, plus ten seconds for observation.
This strengthens the oracle; it does not increase any recovery or query budget.
Primary pools must return to zero between nonempty episodes; pairing pools
must remain zero throughout the no-pairing healthy window.

The private run was deliberately stopped for these corrections, not recorded as
an autonomous runtime failure. Artifacts remain at
`/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.5d4504acbd1ad145`.
Recovery deadlines and existing settling budgets are unchanged.

#### Revised Verification

| Gate | Result |
| --- | --- |
| Workspace | 1,396 passed; 24 opt-in tests excluded |
| Retention selection | Eight tests passed, including live mixed TCP/QUIC endpoints and opposing connection-ID order |
| Oracle regressions | UDP fallback rejection, post-gate path checks, primary drain boundaries, independent node timers, and empty pairing pool |
| Formatting | Passed |
| Clippy correctness, suspicious-code, performance | Passed; advisory warnings remain |
| Android native | Cached x86_64/API 26 compilation passed; not APK or ARM64 acceptance |
| Nix integration | Cached source check and byte-for-byte runtime/support source comparison passed |
| Final sustained runs | Pending for both profiles |

Logs: `/tmp/p2p-vpn-settling-reviewed-{workspace,clippy,android,nix}.log`.
Source check: `/nix/store/99sdlfphykxa4v1sj75zwy4p1p627bnr-p2p-vpn-rust-test-sources`.

#### Session Renewal Finding

The strengthened private-profile run completed five recovery cycles, then failed
the final healthy packet gate after 1,600.92 seconds. The test process exited
nonzero after 1,608.38 seconds; it is not sustained acceptance.

| Evidence | Observation |
| --- | --- |
| Final ping batches | 5/5 replies in both directions |
| Stream fallback counters | Increased from 300 to 304 on each peer during the failing gate |
| Datagram counters | Increased from 1,200 to 1,206 on each peer |
| Session logs | UDP session expiry followed by a new session on the same LAN endpoints |
| Final path snapshots | A selected TCP; B had returned to UDP |
| Settling checks before packet gate | No redial, query-bound, or query-drain assertion failure |

This exposes a healthy session-renewal transition into stream fallback. The
packet gate correctly rejects it despite successful pings. Both final sustained
runs must be repeated after the renewal fix; thresholds remain unchanged.

Artifacts: `/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.8bea9652516f8cff`.
Log: `/tmp/p2p-vpn-settling-reviewed-private.log`.
Test binary SHA-256: `426b3c2fde10011517952a14249a9d11bf23769ed410afc94c66ee2e1a928ec3`.

#### UDP Renewal Implementation Checkpoint

| Mechanism | Bound / Behavior |
| --- | --- |
| Proactive renewal | Initiator starts within the last `min(TTL / 4, 60 seconds)` of a session |
| Scheduling | Existing maintenance tick, authenticated control handshake, and pending-request limit |
| Key overlap | At most one previous session per current peer, at the same endpoint |
| Previous-key expiry | Original establishment time and lifetime; renewal never extends it |
| Responder transmit | Previous keys until an authenticated, replay-accepted new-key packet arrives |
| Lost Accept retry | Preserve the still-used original keys instead of an unconfirmed replacement |
| Path health | Preserve an already healthy UDP path only while valid overlap exists |
| Cleanup | Expiry, normal replacement, peer removal, and network invalidation retire overlap |
| Diagnostics | Status/State report `packet_plane_retiring_sessions`, including retained entries awaiting cleanup |
| Soak assertion | At most one retiring owner per node, never more than current session owners |

| Verification | Result |
| --- | --- |
| Focused packet plane | 108 passed, including signed renewal and UDP during/after Accept delivery |
| Workspace after lifetime/diagnostic changes | 1,402 passed; 24 opt-in tests excluded |
| Required Clippy groups | Passed; existing/advisory warnings remain |
| Additional timer regression | Passed; pre-deadline trigger, duplicate suppression, retry, authorization, direct-path and responder guards |
| Formatting and whitespace | Passed |
| Final namespace/soak/native/source checks | Still pending on the renewal implementation |

Logs: `/tmp/p2p-vpn-renewal-{focused,workspace,clippy,timer}.log`.
The workspace and Clippy runs precede the final timer-only regression addition;
final verification must include it. No final sustained acceptance is claimed.
Wire formats and QUIC session behavior are unchanged.

#### Final Renewal Verification

| Check | Result |
| --- | --- |
| Offline locked workspace | 1,403 passed; 24 opt-in tests excluded |
| Required Clippy groups | Passed in 22.73s; advisory warnings remain |
| Formatting and whitespace | Passed |
| Android x86_64 / API 26 native library | Cached offline compilation passed in 34.57s; four target warnings |
| Nix source check | Passed with cached tools and unchanged assertions |
| Packaged runtime and test-support sources | Byte-for-byte comparison passed |
| Opt-in namespace regression suite | 13 passed in 228.32s |
| Private sustained run | Passed: 1,801.32s, five cycles, 602.52s continuous healthy window |
| Public sustained run | Passed: 1,801.32s, five cycles, 682.91s continuous healthy window |

Logs use `/tmp/p2p-vpn-renewal-final-` with `workspace.log`, `clippy.log`,
`android.log`, `nix.log`, `namespace.log`, `private.log`, and `public.log` suffixes.

- Nix check: `/nix/store/yjj1ga2xdprwhbmqlfil63v1g247iaz0-p2p-vpn-rust-test-sources`.
- Packaged source: `/nix/store/iimh46f9nxi6rihyhbliykkh3x58d92x-source`.
- Test binary SHA-256: `b8583c257248f75a7d569379e5ae195226d23bee435ac23937dca9e153d06077`.
- Private artifacts: `/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.e8d346a279ff2eca`.

No task builds ran during runtime observations. Temporary project storage was
5.11 GiB before the soaks and 5.13 GiB afterward. This is not APK, ARM64,
physical-device, WAN, full Nix package, or formal-proof acceptance.

#### Private Renewal Soak Evidence

- The outer test passed in 1,808.70s; its summary reports `outcome: passed` and `cycles_completed: 5`.
- Ninety healthy samples per peer covered the complete quiet window.
- Both peers reported one retiring UDP session from about 1,608.5s through 1,655.7s, then returned to zero.
- Stream-fallback counters stayed at 300 on both peers throughout the healthy window.
- Redial counters stayed at 20 and 21 respectively; the strict quiet-activity, packet, and query-drain assertions passed.
- PID/start-time and unchanged-configuration assertions passed; no daemon restart, injected addresses, or manual recovery occurred.

#### Public Renewal Soak Evidence

- The outer test passed in 1,808.77s; its summary reports `outcome: passed` and `cycles_completed: 5`.
- One hundred two healthy samples per peer covered the complete quiet window.
- Both peers reported one retiring UDP session from about 1,521.5s through 1,575.4s, then returned to zero.
- Stream-fallback counters stayed at 300 on both peers throughout the healthy window.
- Redial counters stayed at 23 and 20 respectively; the strict quiet-activity, packet, and query-drain assertions passed.
- PID/start-time and unchanged-configuration assertions passed, with the same frozen deadlines and binary as the private run.

Artifacts: `/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.5b1966ccf4728177`.

#### Completion Audit

| Requirement | Final Evidence |
| --- | --- |
| Preserve aggregate bounds | Existing aggregate assertions, workspace regressions, and per-sample resource validation passed |
| Failure/recovery matrix | Both final summaries report five completed cycles with autonomous relay/direct recovery |
| Overload and release | Both DHT profiles over TCP and QUIC; 512 authorized packets, resumed replication/AutoNAT, final query/RPC drain |
| Long timers | Eight passing explicit-time timeline tests; production-owner logic, not a claim of 24 hours of real networking |
| Healthy settling | Zero redundant-activity deltas, bounded legitimate DHT phases, query drain, continuous UDP-only packet gates |
| Negative controls | Retained historical failures plus focused tests detect uncontrolled retries, idle-control churn, and UDP renewal fallback |
| Integration | Final workspace, namespace, formatting, Clippy, cached Nix source parity, and x86_64 Android-native gates passed |
| Delivery | Runtime, regressions, and user documentation published in `a25f78dd8723`; this report records acceptance separately |

Independent read-only review found no additional coverage omission beyond the
then-pending public run and publication; both are now accounted for. No physical
host, phone, personal flake, or production service was changed.

### LAN Endpoint Recovery Finding

The first public-profile run established LAN UDP at 6.7 seconds and discovered
relay fallback at 65.9 seconds. Direct TCP returned after the LAN was restored,
but UDP did not recover within its unchanged 375-second stage deadline.

| Evidence | Interpretation |
| --- | --- |
| Healthy direct TCP plus unhealthy UDP in both final snapshots | Peer discovery and direct transport recovery had succeeded |
| Repeated signed UDP sessions selected `11.251.0.x`, not reachable `10.253.0.x` | Public-address ranking overrode the working LAN interface |
| Empty pending hello owners | Negotiations completed, but selected an unusable packet endpoint |
| Full fixture failed after 477.64 seconds | The gate rejects relay/TCP-only recovery when the preferred UDP path should return |

Failure artifacts:
`/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.101d1050fb5319e4`.
Run log: `/tmp/p2p-vpn-settling-auto-public.log`.

The fix uses the existing authenticated direct-connection inventory.
For an on-link peer, it prefers a mutually advertised LAN endpoint pair for
negotiation. It does not add addresses, change signatures, or retain another cache.

- Endpoint authority still comes from the original validated capabilities.
- No healthy direct connection, off-link peer, or one-sided candidate: preserve existing selection.
- Relay and unrelated-peer connections cannot supply the LAN preference.
- Both profile reruns and the existing namespace compatibility gates pass; see the checkpoint below.

### Signed Freshness Gate

Freeze the unchanged-address refresh target at 900 seconds, half the existing
1,800-second signed lifetime. In a serviced five-second timeline, refresh starts
within five seconds of its due time; capacity rejection retains the pending work.

- Run 24 simulated hours with both monotonic and signed wall time advanced.
- With unchanged addresses and immediate completion, allow 96 starts in `[0, 86400)`.
- Quiet-mode cancellation must not prevent refresh, and signatures must remain valid.
- Address churn still shares the five-second start limit and one publication owner.
- Empty or permanently rejected snapshots must not become recurring retry work.

### AutoNAT Admission Gate

AutoNAT-triggered candidate lookup shares the existing 120-second maintenance
cadence. Freeze the following assertions before running the regression tests.

- Immediate completions and repeated Private/Public transitions permit at most 720 starts in `[0, 86400)`.
- Quiet mode, disabled acquisition, full candidates, or sufficient accepted reservations permit zero new candidate queries.
- A full retained-query pool defers admission without claiming an owner or advancing the start deadline.
- Released capacity permits admission on the next eligible event; unrelated primary and pairing queries remain retained.
- Maintenance and targeted-recovery owners continue to exclude concurrent AutoNAT discovery.
- Existing terminal-completion and redial-timeout cleanup remain required with periodic maintenance disabled.

These are event-handler tests with explicit monotonic time, not a simulation of
all libp2p timers or a proof that a physical network transition produces an event.

### Long Timer Interactions

These explicit-time tests span 86,400 seconds. They exercise scheduler owners,
not a simulated network or packet delivery. Eight `twenty_four` tests pass,
including the existing freshness, AutoNAT and synchronous-relay timelines.

| Timeline | Independent Expected Outcome |
| --- | --- |
| Targeted query timeout and capped backoff | 30 starts and 30 expirations; 60s timeout; cooldown reaches 3,600s and survives 310s idle pruning |
| Targeted cancellation and capacity release | 72 cancellations and 48 expirations; stale completion cannot retire a replacement; successful connection resets retry to 10s |
| Public bootstrap failure bursts | 148 admitted failure steps; repeated reports do not extend cooldown; next deadline is 86,730s |
| Healthy direct and relay peers | Zero eligible public dials or maintenance starts for 24h; losing one peer re-enables due work without resetting timers |
| Maintenance cleanup | 480 expirations, 240 quiet cancellations and 720 capacity releases per profile and cleanup cadence |

Maintenance runs with the five-second maintenance poll and ten-second redial
cleanup. Primary owner expiry leaves an unrelated query and the separate pairing
DHT intact. Its two-slot test pool isolates ownership, not production capacity.

```sh
cargo test --offline --locked --lib twenty_four -- --test-threads=2
```

### Live Contention Gate

The loopback fixture uses real authenticated TCP and QUIC connections and the
production packet stream and forwarding authorization. It is not a TUN, WAN,
or default-timer soak. The following limits were frozen before execution.

| Stage | Required Evidence / Deadline |
| --- | --- |
| Connect | Both ends retain the same authenticated connection; 5s |
| Full discovery pool | 32 retained queries per DHT, one explicit capacity rejection; 750ms dwell within 3s |
| AutoNAT retry after release | One freed primary slot admits the production AutoNAT query owner; 250ms dwell within 3s |
| Background work after release | A stalled foreground query survives while records reach the other node; 500ms dwell within 5s |
| Cleanup | All test queries and pending RPCs retire; packet traffic still passes within 2s |
| Each complete case | 20s maximum; public-primary and separate-public-pairing profiles, each over TCP and QUIC |
| Packet delivery | 16 unique authorized IP packets each direction in every stage; 64 each direction per case |

A held loopback TCP handshake keeps discovery pending while both swarms keep
polling. Only library replication cadence is accelerated to 100ms to inject
background contention; query timeouts, capacity and job limits are unchanged.

No external bootstrap endpoint is contacted. Test-only routing setup gives new
background work a live server after capacity release; this is contention evidence,
not evidence of autonomous discovery after a network fault.

The four cases passed in 8.62s, with 512 total authorized packet deliveries and
empty final query/RPC ownership. Log: `/tmp/p2p-vpn-settling-live-contention-fixed.log`.
The first attempt exposed an idle-dwell completion check in the fixture; its
correction did not change deadlines or runtime behavior.

### Synchronous Relay Failure Gate

Freeze the existing default retry interval at 30 seconds and candidate eviction
at two failures. Synchronous listener errors must use the same failure count
as pending timeouts and unsuccessful listener termination.

- Two fixed failing candidates permit four attempts over 24 simulated hours, with the second attempt per candidate at 30 seconds.
- Repeated calls before retry eligibility create no new attempts, pending listeners, or reservations.
- Eviction releases candidate, retry, failure-history, and listener ownership; unrelated query state survives.
- Freed capacity admits a valid alternative without resetting the relay owner or daemon.

This gate covers one candidate lifetime and local transport-error handling.
It does not establish remote relay availability or prevent later rediscovery
from admitting a new candidate lifetime for the same identity.

## Findings To Reproduce

| Source Finding | Risk | Required Regression |
| --- | --- | --- |
| Quiet mode previously suppressed signed address renewal | Unchanged healthy records aged past their signed validity | Reproduced and fixed; see the signed-freshness checkpoint below |
| AutoNAT Private events previously ignored maintenance `next_due` and candidate policy | Fast completions and repeated transitions bypassed the 120-second cadence | Reproduced and fixed; see the AutoNAT admission checkpoint |
| Synchronous auto-relay listen failures previously bypassed failure accounting | Failed candidates could retain their slot and retry indefinitely | Reproduced and fixed; see the synchronous relay failure checkpoint |
| Configured relay reservations retry independently of healthy suppression | An explicitly requested standby is different from optional discovery | Test explicit reservations separately; do not silently disable configured intent |

Passing discovered addresses into `redial_known_addresses` is not itself an
infrastructure redial bug. Its target selector filters discovered entries by
overlay authorization. Preserve that filter in the regression matrix.

## Signed Freshness Checkpoint

The negative control failed at the first missing renewal, 900 simulated seconds.
The publication owner now retains one refresh deadline in addition to its
existing query ID, pending bit, and five-second start limiter.

| Transition | Behavior |
| --- | --- |
| Startup with usable addresses | Publish without relying on a listener event that preceded runtime startup |
| Unchanged healthy addresses | Re-sign every 900 seconds through the existing publication owner |
| Address change | Coalesce as before; the latest admitted snapshot resets the refresh deadline |
| Temporary capacity rejection | Keep pending work; retry no faster than every five seconds |
| Publication owner times out | Retire the query; wait for the next refresh or address change |
| Empty or permanently oversized snapshot | Stop scheduled refresh attempts until another address event |

`kademlia_address_update_coalesced` retains its existing fields and adds
`refresh_due`. Packet formats, signature verification, record lifetime, configuration,
query limits, and the separate ordinary-maintenance owner are unchanged.

### Regression Evidence

- `address_publication_stays_fresh_for_twenty_four_quiet_hours`: 96 signed publications per protocol over 24 simulated hours; unrelated retained work survives.
- `address_refresh_waits_for_capacity_and_retires_without_fast_retries`: startup, full pool, latest snapshot, rate limit, timeout, and next refresh.
- Empty and oversized snapshot tests cover a full simulated day without repeated admission.
- The churn test retains 32,000 updates and now checks the legitimate next freshness renewal.

The timeline advances explicit monotonic and signing clocks. Query completion is
delivered locally without transport dials; it is not a simulation of the entire
swarm clock or proof of remote publication delivery.

Negative-control log: `/tmp/p2p-vpn-settling-freshness-before.log`.
The focused `address_` suite passed 57 tests with one opt-in test ignored.

### Checkpoint Verification

| Check | Result |
| --- | --- |
| Offline locked workspace suite | 1,331 passed; 22 opt-in tests ignored |
| DHT, peerless pairing, forced-relay pairing, owned QUIC, network-move namespaces | Five passed; 115.04 seconds combined |
| Clippy correctness, suspicious, and performance groups | Passed; nonfatal style warnings remain, including two new test warnings |
| Android x86_64 native library | Compiled offline in 35.02 seconds; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tools and unchanged assertions |
| Formatting / whitespace | Passed |

Logs use `/tmp/p2p-vpn-settling-freshness-` with `before.log`, `focused.log`,
`workspace.log`, `namespace.log`, `clippy.log`, `android.log`, and `nix.log`
suffixes. Readable task temporary storage was approximately 5.05 GiB.

All builds used cached dependencies and at most two Cargo jobs. These checks
are not the phase-2 soak, remote publication acceptance, an APK/ARM64 build,
a full Nix package build, a formal proof, or a physical-device/WAN deployment.

## AutoNAT Admission Checkpoint

The negative-control run used the original admission conditions with the new
explicit clock and tests. It failed in three independent cases; all 16 focused
AutoNAT tests pass with the admission fix.

| Negative Control | Observed Failure |
| --- | --- |
| Immediate result followed by another Private transition | Second lookup started at one second, before the 120-second deadline |
| Automatic relay candidates disabled | Candidate discovery still started |
| Full retained-query pool | An unnecessary rejected query-start attempt was recorded |

The handler now reuses the existing maintenance deadline, checks whether relay
candidates are needed, and checks retained capacity before generating a lookup
target. No new timers, owners, wire fields, or configuration options are added.

### Regression Scope

- Both public-primary and private-primary/separate-public-pairing configurations are exercised.
- The 24-hour timeline delivers real terminal Kademlia events for 720 starts per active profile and zero while quiet.
- Capacity tests retain finished-but-not-retired work and unrelated queries, then release space and verify readmission.
- Candidate/reservation policy, pending recovery ownership, terminal cleanup, and disabled-maintenance expiry remain covered.

The event-handler timeline does not poll network transports. It cannot establish
physical AutoNAT event delivery, relay availability, or whole-runtime settling.
The acceptance soak remains pending.

```sh
nix develop -c cargo test --offline --locked --lib autonat_ -- --test-threads=2
```

Logs: `/tmp/p2p-vpn-settling-autonat-before.log` and
`/tmp/p2p-vpn-settling-autonat-focused.log`.

### Checkpoint Verification

| Check | Result |
| --- | --- |
| Offline locked workspace suite | 1,335 passed; 22 opt-in tests ignored |
| DHT, peerless pairing, forced-relay pairing, owned QUIC, network-move namespaces | Five passed; 115.23 seconds combined |
| Clippy correctness, suspicious, and performance groups | Passed; advisory warnings remain, including duration-unit suggestions in new tests |
| Android x86_64 native library | Compiled offline in 33.22 seconds; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tools and unchanged assertions |
| Formatting / whitespace | Passed |

Final logs use `/tmp/p2p-vpn-settling-autonat-` with `workspace-final.log`,
`namespace.log`, `clippy.log`, `android.log`, and `nix-final.log` suffixes.
The first workspace run exposed a now-unused helper; the final run includes
its reuse in the scheduler and has no Rust compiler warnings.

Readable task temporary storage remained about 5.05 GiB, with cached tools and
at most two Cargo jobs. These checks are not an APK/ARM64 build, full Nix package
build, formal proof, physical-device/WAN test, or the sustained acceptance soak.

## Synchronous Relay Failure Checkpoint

The negative control failed immediately: both failed listener attempts had
scheduled retries but neither had recorded its failure. The test exercises the
actual relay transport's malformed-address error, not an injected error result.

| Change | Behavior |
| --- | --- |
| Synchronous `listen_on` error | Release the pending attempt, count the failure, and evict at the existing two-failure threshold |
| Error diagnostic | Preserve `auto_relay_reservation_failed`; add `evicted` and retain the underlying debug error instead of an empty display string |
| Retry cadence | Preserve the default 30-second interval and existing configured policy |
| Other ownership | Leave accepted reservations, listener-conflict handling, unrelated queries, and explicit reservation policy unchanged |

The 24-hour test covers both DHT profiles, repeated calls before retry deadlines,
complete failed-candidate retirement, and admission of a valid replacement.
All 26 focused `auto_relay_` tests pass, including timeout/listener regressions.

```sh
nix develop -c cargo test --offline --locked --lib auto_relay_ -- --test-threads=2
```

Logs: `/tmp/p2p-vpn-settling-relay-before.log` and
`/tmp/p2p-vpn-settling-relay-focused.log`.

No transport polling or remote reservation occurs in the new test. The valid
replacement owns a queued listener request; it is not evidence of an accepted
reservation, packet delivery, remote relay failure, or rediscovery behavior.

### Checkpoint Verification

| Check | Result |
| --- | --- |
| Offline locked workspace suite | 1,336 passed; 22 opt-in tests ignored |
| DHT, peerless pairing, forced-relay pairing, owned QUIC, network-move namespaces | Five passed; 115.18 seconds combined |
| Clippy correctness, suspicious, and performance groups | Passed; advisory style warnings remain |
| Android x86_64 native library | Compiled offline in 33.35 seconds; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tools and unchanged assertions |
| Formatting / whitespace | Passed |

Final logs use `/tmp/p2p-vpn-settling-relay-` with `workspace.log`,
`namespace.log`, `clippy.log`, `android.log`, and `nix.log` suffixes.
Readable task temporary storage was about 5.01 GiB; no downloads occurred.

These checks do not establish an APK/ARM64 build, full Nix package build,
physical-device/WAN behavior, formal proof, or the sustained acceptance soak.

## Legacy Fixture Limits

- The older network-move fixture hardcodes direct and relay peer addresses.
- It disables Kademlia and mDNS, and shortens packet-session lifetime to three seconds.
- `run_ready_node` disables the default-bootstrap flag and LAN-first holdoff.
- Existing idle sampling lasts at most five minutes and only supports the direct UDP fixture.
- The new automatic-discovery fixture includes application owners, resource bounds and sustained assertions; its complete acceptance run remains pending.
- Tokio time advancement alone does not advance `std::time::Instant`, signed wall time, or vendored timers.
- Normal packet-session renewal occurs every 600 seconds; do not misclassify it as failed recovery.
- Process-tree containment, private artifacts and pre-teardown diagnostics are implemented in the new driver.

The shared launcher now verifies distinct namespaces before provisioning links
and guarantees descendant termination on timeout. The new driver captures
bounded diagnostics before teardown, while control sockets are available.

A private-protocol local-seed profile cannot prove the public-default holdoff.
Cover public-default scheduling separately. Adding a local seed to the public
protocol does not remove its default bootstrap peers; isolation must be explicit.

The new driver reuses namespace lifecycle, bounded command capture, control
observations and packet assertions with real discovery and default timers.
Repeating the old smoke test is not sufficient acceptance evidence.

Do not require optional relay acquisition while every overlay peer is already
healthy: that conflicts with quiet-mode bootstrap suppression. After proving
LAN-only discovery, the first direct-link fault must trigger fresh infrastructure
discovery; budget that separately from later fallback to retained alternatives.

Sources: [namespace harness](../../tests/tun_namespace.rs) and
[idle sampler](../../tests/support/idle_sample.rs).

## DHT Resource Observation

`daemon-status` and `daemon-state` append the same numeric `kad_primary_*` and
`kad_pairing_*` snapshots. Collection is on diagnostic requests only, not the
packet hot path. Missing optional DHTs emit only `present 0`, not fictional zeros.

| Owner | Fields After DHT Prefix | Semantics |
| --- | --- | --- |
| Routing | `routing_entries`, `routing_address_bytes`, rejection counters | Includes retained reservations, not just visible table entries. |
| Query pool | `query_pool_retained`, `query_pool_capacity`, `query_pool_limited`, `query_pool_rejected` | Finished-but-retained phases count; capacity is meaningful only when limited is `1`. |
| Query lifecycle | `query_phases_*`, `query_requests`, `query_successes`, `query_failures` | Counters include retired phases; a multi-stage operation may reuse its ID. |
| Query caches | `query_bounded_caches`, `query_candidates`, `query_address_bytes`, `query_max_*` | Aggregate retained caches and current per-query maxima; not historical peaks. |
| Query metadata | `query_payload_bytes`, result/provider/iterator slot counts | Retained metadata, including consumed fixed-iterator backing slots. |
| Pending RPCs | `pending_rpc_*` | Aggregate pre-handler requests/bytes and admission rejections. |
| Background jobs | `background_*` | Bounded batches, cursors, skip storage, and rejection counters. |
| Behavior queue | `events`, `event_bytes`, limits and rejections | Unsent events and retained payloads; separate limited flags distinguish unlimited from zero. |
| Dial intents | `dial_intents_attempted`, `admitted`, `dispatched`, `discarded` | Local Kademlia intents only, not all swarm socket attempts or connection successes. |
| Handlers | `handlers`, `handler_pending_*`, stream gauges, rejection/expiry counters | Gauges sum live handlers; lifetime counters survive handler closure. |
| Handler peaks | `handler_peak_*` | Historical maximum reported on any one handler, not an aggregate maximum. |

### Accounting Contracts

- `admitted_phases - retired_phases == query_pool_retained` before counter saturation.
- Retired phases split into completed, timed-out, and canceled phases; explicit finish is completion, not application success.
- Selected query requests remain counted after retirement; repeated snapshots must not double-count them.
- Admitted dial intents minus dispatched/discarded intents equal retained queued dial intents.
- Handler closure removes its live gauges without erasing prior rejection or expiry counters.
- `query_retained_rejected_reports` belongs to currently retained caches and may decrease at retirement.

Query requests count iterator selection, not successful remote receipt. Observed
payload bytes exclude allocator/container overhead unless the owner explicitly
accounts for capacity. These reports are not RSS or a phase-3 resource comparison.

Handler accounting uses one fixed-size shared aggregate per DHT and one previous
snapshot per handler. It has no history map or event backlog; unchanged snapshots
avoid locking. Peaks are sampled at callback/poll boundaries, not internal allocations.

The namespace state poller now checks reported resources against the frozen
[phase-1 ceilings](kademlia-final-ownership-audit.md#per-dht-storage). Missing fields,
inconsistent phase totals, orphaned query payloads, and excessive handler peaks
fail immediately. This checks each observed state, not unsampled runtime intervals.

### Observation Checkpoint Verification

| Check | Result |
| --- | --- |
| Query lifecycle | Admission, rejected starts, finish, timeout, repeated cancellation, bootstrap continuation, and two-phase publication pass. |
| Retained caches | Finished-but-retained state is reported; retirement releases bytes and current maxima; request totals remain monotonic. |
| Dial intents | Admission rejection, dispatch, queued cancellation, and unrelated-query preservation pass; swarm-originated dials are not counted as Kademlia intents. |
| Handler ownership | Eight exact-source owner tests plus real handler construction/preload/callback/drop tests pass. |
| Live handler wiring | TCP and QUIC loopback connect/query/disconnect cycles pass for both independently accounted DHTs. |
| Diagnostic contract | Both control views preserve prior lines and report the same DHT fields; absent DHTs and unlimited library limits are explicit. |
| Namespace validator | Synthetic missing, duplicate, nonnumeric, over-budget, and orphaned-state reports are rejected. |
| Offline workspace | 1,358 passed; 23 opt-in tests ignored. |
| Complete opt-in namespace suite | All 13 passed in 265.79 seconds, including TCP pressure, network movement, and relay promotion, with resource assertions on state polls. |
| Clippy / formatting / whitespace | Required correctness, suspicious, and performance groups pass; nonfatal style warnings remain. Rust formatting and whitespace checks pass. |
| Android x86_64 native library | Compiled offline in 40.50 seconds; four existing target-specific warnings. |
| Nix desktop/Android source parity | Passed with cached tools and unchanged assertions. |

Logs use `/tmp/p2p-vpn-settling-telemetry-` with `focused.log`, `workspace.log`,
`namespace.log`, `clippy.log`, `android.log`, and `nix.log` suffixes.
Task temporary storage stayed near 5.07 GiB. No build downloads occurred, and
no task builds ran during the namespace observations.

These results establish the diagnostic checkpoint, not the sustained acceptance
soak, a before/after resource comparison, full Nix packages, APK/ARM64 builds,
formal verification, or physical-device/WAN acceptance. The goal remains active.

## Namespace Lifecycle Checkpoint

The shared launcher now places the orchestrator at PID 1 in a private PID
namespace, with matching procfs. Its watchdog kills that namespace's init;
the kernel then terminates descendants, including separate process groups.

| Boundary | Verification |
| --- | --- |
| Timeout negative control | Omitting only `--kill-child` made the pipe readers outlive the two-second watchdog; regression failed after 15.01 seconds |
| Timeout fixed path | SIGKILL, both inherited-pipe markers, and distinct-namespace readiness verified in 2.01 seconds, below the frozen six-second allowance |
| Namespace readiness | Existing `/proc/PID/ns/net` alone is rejected; device and inode must differ from the orchestrator's namespace |
| Artifact creation | Fresh random-suffixed directories are `0700` before writing files; fixture prefixes are preserved |
| Replay | Already-built commands use the same outer watchdog; they no longer bypass containment |
| Nix preflight | Evaluated script verifies PID-local procfs, veth and TUN creation; ShellCheck passes |

Process IDs in new reports are namespace-local, not host PIDs. Existing reports
remain historical. No packet runtime, protocol, identity, or user configuration
changes are included in this harness checkpoint.

```sh
nix develop -c cargo test --test tun_namespace \
  namespace_orchestrator_timeout_reaps_pipe_inheritors -- --ignored --exact
```

### Checkpoint Verification

| Check | Result |
| --- | --- |
| Offline locked workspace suite | 1,338 passed; 23 opt-in tests ignored |
| Complete opt-in namespace suite | All 13 passed, including the new watchdog, TCP pressure, network movement, and relay promotion; 238.80 seconds combined |
| Clippy required groups / Rust formatting / whitespace | Passed; advisory style warnings remain |
| Nix source parity / parsing | Passed with cached tools and unchanged source assertions |
| Evaluated preflight / ShellCheck | Passed; no full preflight-package derivation build claimed |
| Nix formatting | Existing drift reproduced on parent revision `1794bcab`; formatter differences are outside the edited preflight block |
| Idle-sampler compatibility | Ten-second sample plus normal warmup/traffic gates passed in 56.31 seconds; 22 samples, stable process identity per role, readable CPU/RSS/socket data |

Logs use `/tmp/p2p-vpn-settling-namespace-` with `before.log`, `watchdog.log`,
`workspace.log`, `suite.log`, `clippy.log`, `nix-final.log`, `preflight.log`,
`shellcheck.log`, `nixfmt-baseline.log`, `nixfmt.log`, and `idle.log` suffixes.

The idle sample is
`/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.ce04121036f17264/idle-sample.json`.
No task builds ran during this compatibility sample.

Task temporary storage remained about 5.06 GiB. No downloads occurred. Android
native compilation was not repeated for test-launcher and preflight-only changes;
the preceding runtime checkpoint covers unchanged native sources.

This is harness validation, not the 30-minute acceptance soak, a resource
comparison, or physical-device/WAN recovery evidence.

## Automatic Recovery Checkpoint

The new production-entry-point fixture first reproduced a failure to restore
UDP after relay fallback. The endpoint-selection fix passes both DHT profiles
without widening deadlines, changing configuration, or restarting a daemon.

| Profile | Initial LAN UDP | Discovered Relay | Restored LAN UDP | Total Test Time |
| --- | --- | --- | --- | --- |
| Shared public | 6.7 seconds | 65.9 seconds | 188.0 seconds | 226.04 seconds |
| Private primary / separate public pairing | 6.7 seconds | 65.9 seconds | 112.9 seconds | 150.86 seconds |

Stage observations are elapsed time from the fixture's ready-daemon baseline,
not each individual transition's latency. Each path receives a 30-second healthy
window, bidirectional 5/5 ICMP checks, and accepted-packet counter checks.

### Ownership Evidence

- `app_*` snapshots report 33 fixed numeric application-owner gauges on Status/State requests only.
- Pending work is distinguished from retained idle caches and future cooldowns.
- Snapshots report overdue owners rather than pruning them during observation.
- Recovery-cache counts do not claim to measure `AddressRetention`'s private index.
- DHT bounds, application query age/count bounds, process identity, and unchanged configs are checked throughout each cycle.

### Artifacts

Both successful profiles used test-binary SHA-256
`be9273cfa500ff0c3f28af88b39c5c94abfe4032ca8f2548cd9864b20acc774c`.

| Profile | Artifact Directory Suffix |
| --- | --- |
| Public | `.875220a29f311fa9` |
| Private | `.e1388afe77c465f7` |

Directory prefix:
`/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes`.
Each contains the summary, sampled states, latest packet checks, node logs,
private fixture configs, and profile-preserving replay commands.

### Verification

| Check | Result |
| --- | --- |
| Offline locked workspace suite | 1,371 passed; 24 opt-in tests ignored |
| New endpoint-selection regressions | Three passed; reciprocal candidates, retained signature authority, stale/unrelated/relay exclusion |
| Minimal config and replay regressions | Four passed |
| Public and private automatic recovery | Both passed; same frozen deadlines |
| Existing namespace regression suite | All 13 passed in 239.97 seconds, including pairing, DHT, mDNS, QUIC, relay promotion, network movement and queue pressure |
| Required Clippy groups | Passed in 22.00 seconds; advisory warnings include new long functions, naming and duration style |
| Rust formatting / whitespace | Passed |
| Android x86_64 native library | Compiled offline in 33.05 seconds; four existing target warnings |
| Nix source parity | Passed with cached tools and unchanged assertions |

Logs use `/tmp/p2p-vpn-settling-auto-` with `workspace.log`, `clippy.log`,
`android.log`, `nix.log`, `namespace.log`, `public-fixed.log`, and
`private-fixed.log` suffixes. Temporary project storage finished at about 5.08 GiB.

No task builds ran during recovery observations. This is not the 30-minute soak,
the full fault matrix, an APK/ARM64 or full Nix package build, a formal proof,
a physical-device/WAN test, or a production-readiness claim.

## Delivery Gates

1. Freeze fixture-specific acceptance budgets and add failing regressions.
2. Fix reproduced scheduling/retirement/freshness defects without weakening phase-1 bounds.
3. Run deterministic timelines, workspace tests, and relevant namespace gates.
4. Run the sustained soak on final relevant code with no concurrent task builds.
5. Verify formatting, selected Clippy checks, Nix source parity, and affected Android native compilation.
6. Publish atomic Conventional Commits to `main` and document evidence and residual risks.

Keep Cargo jobs at most two, build downloads at most 10 Mbps, and task temporary
storage at most 10 GiB. Bound log retention. No physical hosts, phones, or personal
flakes are part of this phase's deployment scope.
