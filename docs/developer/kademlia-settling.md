# Sustained Recovery And Healthy Settling

## Status

Phase 2 is active. Starting revision: `63a380cbcd1a`.
Signed freshness, AutoNAT admission, synchronous relay errors, and resource
reporting pass their checkpoint checks. Automatic LAN/relay/LAN recovery now
passes one cycle per DHT profile after fixing packet endpoint selection.

No acceptance soak has run. The remaining phase-wide cases below are open.

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
| Unavailable bootstrap/routing peers | Backoff survives long failures; useful discovery resumes automatically | Short startup outage and automatic fallback verified; long-failure case pending |
| Failed or stale relay | Retire failed attempts; discover or select a usable alternative | Synchronous-error component verified; live remote-failure case pending |
| Repeated network/address changes | LAN-first discovery, relay fallback, eventual direct recovery where available | One cycle per profile verified; repeated-fault matrix pending |
| Foreground/background contention | Preserve unrelated VPN traffic; retire stale owners; resume after release | Pending |
| All overlay peers healthy | Suppress redundant queries/dials; retain legitimate maintenance and fresh records | Signed freshness and AutoNAT quiet-mode components verified; whole-runtime case pending |
| Shared-public and separate-public-pairing DHTs | Same bounded behavior with independent budgets | Components and one recovery cycle per profile pass; full matrix pending |
| AutoNAT with periodic maintenance disabled | No stranded owner or event-driven query storm | Cadence, policy, capacity and cleanup tests pass; sustained runtime evidence pending |
| Deterministic long timeline | At least 24 simulated hours with timer-boundary assertions | Freshness, AutoNAT and synchronous-relay timelines pass; remaining timer interactions pending |
| Real-runtime soak | At least 30 minutes, five fault/recovery cycles, ten continuous healthy minutes | Pending |
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

Before acceptance runs, complete the fixture-specific numeric recovery and
settling budgets from these timers and the actual topology. Record them here
and in test assertions; do not widen them after a failure merely to pass.

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

The targeted-query one-hour failure cap still needs its separate long-timeline
case. This fresh fixture does not claim its 960-second budget bounds every
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

## Harness Gaps

- The current network-move fixture hardcodes direct and relay peer addresses.
- It disables Kademlia and mDNS, and shortens packet-session lifetime to three seconds.
- `run_ready_node` disables the default-bootstrap flag and LAN-first holdoff.
- Existing idle sampling lasts at most five minutes and only supports the direct UDP fixture.
- DHT reporting and sampled namespace bounds pass; full soak assertions and application recovery-owner reporting remain pending.
- Tokio time advancement alone does not advance `std::time::Instant`, signed wall time, or vendored timers.
- Normal packet-session renewal occurs every 600 seconds; do not misclassify it as failed recovery.
- Process-tree timeout containment and private artifact directories are now verified; the full soak driver is still missing.

The shared launcher now verifies distinct namespaces before provisioning links
and guarantees descendant termination on timeout. The soak driver must still
capture bounded diagnostics before teardown, while control sockets are available.

A private-protocol local-seed profile cannot prove the public-default holdoff.
Cover public-default scheduling separately. Adding a local seed to the public
protocol does not remove its default bootstrap peers; isolation must be explicit.

Reuse namespace lifecycle, bounded command capture, control-socket observation,
and packet assertions. Extend the fixture for real discovery and default timers;
repeating the old smoke test is not sufficient acceptance evidence.

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
