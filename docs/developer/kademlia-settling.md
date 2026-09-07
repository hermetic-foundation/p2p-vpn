# Sustained Recovery And Healthy Settling

## Status

Phase 2 is active. Starting revision: `63a380cbcd1a`.
Signed address renewal and AutoNAT admission fixes pass their checkpoint checks.
No acceptance soak has run. The remaining findings and phase-wide acceptance
cases below are still open.

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
| Unavailable bootstrap/routing peers | Backoff survives long failures; useful discovery resumes automatically | Pending |
| Failed or stale relay | Retire failed attempts; discover or select a usable alternative | Pending |
| Repeated network/address changes | LAN-first discovery, relay fallback, eventual direct recovery where available | Pending |
| Foreground/background contention | Preserve unrelated VPN traffic; retire stale owners; resume after release | Pending |
| All overlay peers healthy | Suppress redundant queries/dials; retain legitimate maintenance and fresh records | Pending |
| Shared-public and separate-public-pairing DHTs | Same bounded behavior with independent budgets | Pending |
| AutoNAT with periodic maintenance disabled | No stranded owner or event-driven query storm | Pending |
| Deterministic long timeline | At least 24 simulated hours with timer-boundary assertions | Pending |
| Real-runtime soak | At least 30 minutes, five fault/recovery cycles, ten continuous healthy minutes | Pending |
| Negative control | The tests detect suppressed recovery or uncontrolled retries | Pending |

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

## Findings To Reproduce

| Source Finding | Risk | Required Regression |
| --- | --- | --- |
| Quiet mode previously suppressed signed address renewal | Unchanged healthy records aged past their signed validity | Reproduced and fixed; see the signed-freshness checkpoint below |
| AutoNAT Private events previously ignored maintenance `next_due` and candidate policy | Fast completions and repeated transitions bypassed the 120-second cadence | Reproduced and fixed; see the AutoNAT admission checkpoint |
| Synchronous auto-relay listen failures schedule retries outside timeout failure accounting | Optional failed candidates may be retried indefinitely while overlay paths are healthy | Compare synchronous failure, timeout, candidate retirement, and admission of alternatives |
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

## Harness Gaps

- The current network-move fixture hardcodes direct and relay peer addresses.
- It disables Kademlia and mDNS, and shortens packet-session lifetime to three seconds.
- `run_ready_node` disables the default-bootstrap flag and LAN-first holdoff.
- Existing idle sampling lasts at most five minutes and only supports the direct UDP fixture.
- Runtime counters do not yet expose the complete retained-owner inventory from phase 1.
- Tokio time advancement alone does not advance `std::time::Instant`, signed wall time, or vendored timers.
- Normal packet-session renewal occurs every 600 seconds; do not misclassify it as failed recovery.
- The current outer watchdog kills its immediate child, not a guaranteed contained process tree.

The new fixture must verify namespace isolation before provisioning links and
guarantee child cleanup on timeout. Keep artifacts private and capture bounded
diagnostics before teardown, while control sockets are still available.

A private-protocol local-seed profile cannot prove the public-default holdoff.
Cover public-default scheduling separately. Adding a local seed to the public
protocol does not remove its default bootstrap peers; isolation must be explicit.

Reuse namespace lifecycle, bounded command capture, control-socket observation,
and packet assertions. Extend the fixture for real discovery and default timers;
repeating the old smoke test is not sufficient acceptance evidence.

Sources: [namespace harness](../../tests/tun_namespace.rs) and
[idle sampler](../../tests/support/idle_sample.rs).

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
