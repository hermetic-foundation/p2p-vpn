# Sustained Recovery And Healthy Settling

## Status

Phase 2 is active. Starting revision: `63a380cbcd1a`.
This initial source audit is not a passing test report. No acceptance soak has
run, and no production behavior has changed in this checkpoint.

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

## Findings To Reproduce

| Source Finding | Risk | Required Regression |
| --- | --- | --- |
| Quiet mode suppresses periodic signed address publication; event publication requires a pending change | Unchanged healthy records can age past their signed validity | Advance wall time past the 1,800-second lifetime and 5,400-second grace boundary; verify fresh signatures |
| AutoNAT Private events test pending ownership but not maintenance `next_due` | Fast completions and repeated transitions can bypass the 120-second cadence | Repeated status transitions with immediate completion, full candidates, and disabled automatic relays |
| Synchronous auto-relay listen failures schedule retries outside timeout failure accounting | Optional failed candidates may be retried indefinitely while overlay paths are healthy | Compare synchronous failure, timeout, candidate retirement, and admission of alternatives |
| Configured relay reservations retry independently of healthy suppression | An explicitly requested standby is different from optional discovery | Test explicit reservations separately; do not silently disable configured intent |

Passing discovered addresses into `redial_known_addresses` is not itself an
infrastructure redial bug. Its target selector filters discovered entries by
overlay authorization. Preserve that filter in the regression matrix.

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
