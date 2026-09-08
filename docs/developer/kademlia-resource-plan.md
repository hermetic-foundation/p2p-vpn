# Kademlia Resource-Limits Workstream

## Status

The original broad goal was superseded without being marked complete.
**Aggregate Resource Bounds**, phase 1 below, is complete.
Phase 2, [Sustained Recovery And Healthy Settling](kademlia-settling.md), is complete.
Phase 3, [Before/After Resource Measurements](kademlia-resource-measurements.md), is active.
This workstream does not complete the broader reliability review.
Starting revision: `5ecb01ea`. No deployed service or physical device has changed.

## Phase Status

| Phase | Scope | Acceptance | Status |
| --- | --- | --- | --- |
| 1. Aggregate Resource Bounds | Handler pending work, total routing storage, total retained query state | Enforced limits and deterministic saturation/recovery evidence | Complete |
| 2. Sustained Recovery And Settling | Prolonged failures, churn, transitions, healthy idle behavior | Long-running tests recover without intervention and settle when healthy | Complete; both final 30-minute profiles passed |
| 3. Before/After Measurements | Sockets, dial/query rates, CPU, RSS | Comparable baseline/current captures, commands, and limitations | Active; sampler, CLI smoke, pinned builds, and frozen protocol; no acceptance data yet |
| 4. Final Acceptance | All original requirements, documentation, packaging | Requirement-by-requirement evidence audit and residual risks | Open |

Completing phases 1-2 does not complete phases 3-4. Verified commits remain valid;
the completion gates below retain the original workstream's full scope.
See [Aggregate Bounds](kademlia-aggregate-bounds.md) for phase 1 ownership and tests.

The [final acceptance audit](kademlia-final-ownership-audit.md) records its complete
owner/producer inventory and final checks. Historical findings below preserve
checkpoint-specific gaps; this status table is authoritative for current progress.

## Completion Gates

| Area | Required Evidence | Status |
| --- | --- | --- |
| Internal addresses | Count/byte bounds for present and pending buckets, address changes, and query caches | Aggregate routing and query retention verified, including backing capacity |
| Query state | Bounded candidate identities, active queries, and retained results | All retained owners and production producers audited; saturation/retirement verified |
| Scheduling | Bounded bootstrap, discovery, and dial activity under failure and churn | Aggregate bounds and phase-2 sustained activity/settling passed |
| Recovery | LAN-first lookup, relay fallback, network-change recovery, and healthy-path settling | Namespace gates and both final five-cycle sustained profiles passed |
| Measurements | Comparable before/after CPU, RSS, sockets, dial rates, and query rates | Open |
| Packaging | Matching Cargo, desktop Nix, and Android source inclusion | Phase-2 Nix source parity and native x86_64 Android verified; broader final acceptance open |
| Delivery | Regression tests, broader validation, documentation, atomic verified pushes | Phases 1-2 complete; phases 3-4 remain open |

## Baseline Reproduction

Both existing loopback diagnostics reproduced on 2026-09-07, before source changes.
The cached test binary completed both tests in 3.30 seconds.

| Observation | Shared Public DHT | Separate Public-Pairing DHT |
| --- | ---: | ---: |
| Connections to one identity | 65 | 65 |
| Internally retained bucket addresses | 65 | 65 |
| Query-local addresses for one reported identity | 65 | 65 |
| Query-local encoded address bytes | 3,055 | 3,055 |

These are retention diagnostics, not successful enforcement tests or process
memory measurements. See [the original audit](kademlia-retention-review.md)
for topology isolation and exact source owners.

## Implementation Sequence

1. Preserve long recovery-query cooldowns during idle-state expiry.
2. Enforce internal address and candidate limits in the pinned library owners.
3. Audit all query producers, including library-triggered bootstrap and background jobs.
4. Exercise bounded failure pressure, healthy settling, and automatic recovery.
5. Capture comparable resource measurements and validate desktop/Android packaging.

## Findings

### Cooldown Expiry

`RecoveryQueries::should_query` previously pruned history 310 seconds after the
last query, even when its exponential cooldown had not ended. The fifth failure's
480-second cooldown could therefore be discarded, restarting short retries.

- Negative control: `sustained_failures_preserve_backoff_past_state_ttl` failed at attempt five.
- Log: `/tmp/p2p-vpn-kad-backoff-before.log`.
- Fix: count idle expiry from the later of the last query and retry deadline.
- Preserve the membership-sized state cap, pending ownership, revocation cleanup, and network resets.

#### Verified Cooldown Fix

Code revision: `14782e7d`. No config, dependency, or wire-format changes.

| Check | Result |
| --- | --- |
| Negative-control regression | Failed at attempt five before the fix |
| Recovery-query module | Seven passed, including twelve simulated failures up to one-hour cooldown |
| Native workspace | 1,248 passed; 23 opt-in tests ignored |
| Clippy correctness, suspicious, performance groups | Passed; existing nonfatal style warnings remain |
| Direct UDP namespace | Passed, 15.03 seconds |
| Owned QUIC packet-plane namespace | Passed, 16.03 seconds |
| Forced-relay live pairing namespace | Passed, 17.64 seconds |
| Changed-source rustfmt / whitespace | Passed |

Logs use `/tmp/p2p-vpn-kad-backoff-` with `before.log`, `after.log`,
`workspace.log`, `clippy.log`, `udp.log`, `quic.log`, and `relay.log` suffixes.
The cached target remains approximately 2.0 GiB; no dependencies were downloaded.

Full Nix package builds and native Android builds were not repeated for this
scheduler-only fix. They remain required when changing dependency sources.
Host-side Android workspace tests are included above, not device acceptance.

### Internal Bootstrap

The pinned `libp2p-kad 0.48.0` defaults to bootstrapping 500 ms after routing-table
insertion. `set_periodic_bootstrap_interval(None)` does not disable this separate
trigger; its disabling setter is currently library-test-only.

The runtime regression `routing_updates_do_not_start_unowned_bootstrap_queries`
failed after 0.51 seconds with the original dependency. It passes after exposing
the existing setter and disabling insertion-triggered bootstrap for both DHTs.
The test also checks that explicit scheduler-owned bootstrap remains available.

The pinned source is now shared through the root Cargo patch and the desktop and
Android Nix filesets. Original MIT notices and archive provenance are retained in
`vendor/libp2p-kad-0.48.0/P2P-VPN.md`. The source import is approximately 688 KiB.

#### Bootstrap Validation

| Check | Result |
| --- | --- |
| Native workspace | 1,249 passed; 23 opt-in tests ignored |
| Clippy correctness, suspicious, performance groups | Passed; existing nonfatal style warnings remain |
| Direct UDP namespace | Passed, 15.08 seconds |
| Owned QUIC packet-plane namespace | Passed, 16.03 seconds |
| Forced-relay live pairing namespace | Passed, 17.63 seconds |
| Peerless code-pairing namespace | Passed, 13.23 seconds |
| Nix source parity | Passed for desktop and both Android native source inputs |
| Android native x86_64 library | Built with patched dependency, NDK 28, API 26, cached Nix Rust |
| Workspace rustfmt / Nix parsing | Passed |
| Upstream source comparison | Only the setter and patch record differ |

Logs use `/tmp/p2p-vpn-kad-bootstrap-` with `before.log`, `after.log`,
`workspace.log`, `clippy.log`, `udp.log`, `quic.log`, `relay.log`,
`code-pairing.log`, and `nix-sources.log` suffixes.

The source check ran in a Nix sandbox with cached tool inputs and unchanged
assertions. The default tool closure planned 698 builds and was not built.
Output: `/nix/store/qf5rlm0izinb3n3awg0k1igmfrc5bgc6-p2p-vpn-rust-test-sources`.

An upstream generated file has a trailing blank line that triggers `git diff
--check` on initial import. Its bytes were retained; all non-generated changed
files pass whitespace checking. This is not a claim that upstream formatting passes.

These tests do not measure public-network socket rates or close the internal
address/candidate retention gates. No public-network improvement is claimed yet.

#### Android Cache Repair

The final offline native build passed in 1 minute 54 seconds with two build jobs.
The log confirms compilation from the repo's vendored `libp2p-kad` path.
Log: `/tmp/p2p-vpn-kad-bootstrap-android-native-verified.log`.

| Artifact | Value |
| --- | --- |
| Library | `/tmp/p2p-vpn-android-target/x86_64-linux-android/debug/libp2p_vpn_android.so` |
| SHA-256 | `71e31d77f222c2753772ad01fce4edc416dfc2df9f53d0858faeeacdefb0ad1a` |
| Combined target/vendor footprint | Approximately 4.5 GiB |

Earlier attempts failed on removed vendor-cache symlinks, missing host compiler
and archiver settings, and an invalid cached `tracing` archive. Logs are retained.
The cache was regenerated; replacement/downloaded archives were checked against
Cargo.lock and fetched sequentially at no more than 1,000 KiB/s.

The full workspace format check used the cached formatter executable directly;
its old Nix wrapper referenced a removed store path. No source-format rules changed.
No ARM64 native build, APK rebuild, physical-device test, or full Nix package build
is claimed for this patch.

### Candidate Identities

The query-local address map was not the only unbounded owner.
`ClosestPeersIter::on_success` also inserted reported identities into its
distance-ordered map. Address-vector limits alone could not close that gap.

### Query Deadline Enforcement

The query pool checked timeouts only when an iterator could not select another
peer. After a failed request, an expired query could select another candidate
and enqueue a dial before reporting timeout.

- Negative control: `expired_kademlia_query_does_not_dial_remaining_candidates` reproduced the extra dial.
- Log: `/tmp/p2p-vpn-kad-deadline-before.log`.
- Fix: check expiry before advancing an unfinished query's iterator.
- Preserve explicit completion and the existing typed timeout result.

The check applies when the pool examines a query; it does not cancel requests
already queued or sent. Multi-stage operations retain existing stage deadlines.
It is not a bound on candidate storage, active-query count, or aggregate dial rate.

#### Deadline Validation

| Check | Result |
| --- | --- |
| Deadline regressions | Expired candidate rejected; explicit completion preserved |
| Workspace | 1,257 passed; 23 opt-in tests ignored |
| Clippy correctness, suspicious, performance groups | Passed; existing nonfatal style warnings remain |
| DHT namespace | Passed after correcting a role-specific assertion |
| Forced-relay live pairing namespace | Passed |
| Peerless code pairing namespace | Passed |
| Owned QUIC packet-plane namespace | Passed |
| Relay/direct recovery after network move | Passed |
| Nix source parity | Passed with cached tools and unchanged sandbox assertions |
| Android native x86_64 | Built offline; no APK or device deployment |
| Format and whitespace | Passed |

Logs use `/tmp/p2p-vpn-kad-deadline-`. Final runs use `workspace-final.log`,
`clippy-final.log`, `dht-final.log`, and `nix-final.log`. Other checks use
`after.log`, `relay.log`, `code-pairing.log`, `quic.log`, `move.log`, and `android.log`.

The first DHT test transferred packets successfully, but required node A to
originate discovery. Node B had completed the query and both nodes authenticated.
The corrected assertion accepts either initiator and requires both authentications;
a unit test rejects missing discovery or one-sided authentication.

The failed log remains `dht.log`. An older cached binary passed (`dht-control.log`);
that is not evidence that the old binary reproduced the role-specific failure.
No production behavior was changed to accommodate the assertion.

Full Nix package builds, ARM64 native builds, physical devices, public WAN, and
aggregate resource measurements are not validated by these checks.

### Remaining Concurrency Audit

`set_parallelism(1)` does not bound every query phase to one outstanding peer.
The closest-peer iterator permits up to the result count while stalled, and
fixed-peer queries use the replication factor. Both default counts are 20.

- Overlay recovery has its own one-query admission limit in `recovery_queries.rs`.
- Other producers include maintenance, pairing publication/lookup, standalone pairing, and library background work.
- Pool admission, pending RPC concurrency, and outbound address unions still need aggregate bounds.

Aggregate limits and comparable resource measurements remain open.

#### Scheduler Retirement

Maintenance preemption, reset, expiry, and targeted recovery cleanup now use
immediate `cancel_query` retirement. Previously they called `QueryMut::finish`,
which hides a query from running-query inspection without immediately removing
its retained state; multi-stage operations may also continue when polled.

The regression failed on its first bootstrap cancellation before the fix. It
exercises 64 cancellation cycles across bootstrap, provider publication, and
record lookup, while preserving an unrelated query. A second cancellation must
find neither query state nor queued actions for the retired ID.

Timeouts, recovery cooldowns, authorization checks, and metrics are unchanged.
Dispatched handler work remains outside this local retirement guarantee.
Negative-control log: `/tmp/p2p-vpn-kad-retire-before.log`.

Review also found that cancellation must decrement the library's active-bootstrap
counter. Removing only the query left automatic bootstrap suppressed. The fix and
regression preserve suppression while a second bootstrap remains, then permit
periodic bootstrap after both are canceled, without double-decrementing.

Both regressions failed before their respective fixes. The bootstrap counter's
negative-control log is `/tmp/p2p-vpn-kad-retire-bootstrap-before.log`.

#### Verified Scheduler Checks

| Check | Result |
| --- | --- |
| Workspace | 1,270 passed; 22 opt-in tests ignored |
| Clippy correctness, suspicious, performance groups | Passed; style warnings remain |
| Peerless code pairing / forced-relay pairing | Passed: 13.42 / 17.64 seconds |
| DHT / owned QUIC / relay-direct network move | Passed: 16.73 / 16.08 / 52.35 seconds |
| Native x86_64 Android | Built offline in 39.12 seconds; existing native-target warnings remain |
| Nix source parity / formatting / whitespace | Passed |

Final logs use `/tmp/p2p-vpn-kad-retire-` with `workspace`, `clippy`, `code`,
`relay`, `dht`, `quic`, `move`, `android`, and `nix` `-verified.log` suffixes.
Earlier runs without that suffix preceded the bootstrap-counter correction.

Nix source output:
`/nix/store/p7p3xc2mgsafswg4h3i04vcmhnjv05iz-p2p-vpn-rust-test-sources`.
These are controlled regression checks, not sustained resource measurements or
physical-network validation. No full Nix package or ARM64 build was performed.

#### Address Publication Admission

The old event helper discarded publication query IDs. Listener/external-address
changes and relay acceptance could therefore start overlapping queries outside
the one-query maintenance owner.

| Owner / Transition | Bound / Behavior |
| --- | --- |
| Event notifications | One pending bit; no address snapshots or event queue retained by this owner |
| Event publication | One query ID and start time |
| Start rate | At most one every five seconds, including timer catch-up and early completion |
| New addresses | Cancel older event query and encode current addresses at the next eligible tick |
| Completion | Release query ownership without clearing a newer pending update |
| Timeout | Retire after 90 seconds; do not continually recreate an unchanged event update |
| Healthy paths / recovery | Ordinary maintenance suppression does not cancel an address update |
| Discovery disabled | Notifications do not schedule publication |

Periodic address refresh remains part of the existing single-query maintenance
cycle. Its query can coexist with the one event-publication query; this is not
an aggregate query limit. Background jobs also retain their existing admission.

The churn regression exercises 32,000 notifications across 64 address changes.
It inspects the signed local-store record for the newest address and checks
retirement, interval enforcement, and idle settling. A second test covers
completion, pending updates, disabled discovery, and no-address state.

`kademlia_address_update_coalesced` reports whether a scheduled update started.
Already dispatched handler requests and remote records are not recalled. These
synthetic tests do not measure sockets, successful remote delivery, CPU, or RSS.

#### Verified Publication Checks

| Check | Result |
| --- | --- |
| Workspace | 1,272 passed; 22 opt-in tests ignored |
| Clippy correctness, suspicious, performance groups | Passed; style warnings remain |
| Peerless code pairing / forced-relay pairing | Passed: 13.38 / 17.64 seconds |
| DHT / owned QUIC / relay-direct network move | Passed: 16.73 / 15.99 / 52.36 seconds |
| Native x86_64 Android | Built offline in 37.53 seconds; existing native-target warnings remain |
| Nix source parity / formatting / whitespace | Passed |

Logs use `/tmp/p2p-vpn-kad-publish-` with `workspace`, `clippy`, `code`, `relay`,
`dht`, `quic`, `move`, `android`, and `nix` `.log` suffixes. Nix source output:
`/nix/store/rmzpxqbrciglwqg6q1w888qnfiscb61n-p2p-vpn-rust-test-sources`.

Namespace timings are smoke-test observations, not performance comparisons.
No full Nix package, ARM64 build, or physical-device deployment was performed.

### Background Job Admission

Provider and record jobs previously reused the same available query capacity
when the provider job filled its batch. With 99 existing queries, one poll
could exceed the default 100-query background admission ceiling.

- Negative control: `kademlia_background_jobs_share_remaining_query_capacity` failed before the fix.
- Log: `/tmp/p2p-vpn-kad-jobs-before.log`.
- Both jobs now consume a shared allowance and alternate which job runs first.
- p2p-vpn allows one new background query per poll, only below two existing queries.

All active queries count against background admission. Foreground API calls
are not capped or canceled by this setting. The library default ceiling stays
100; its default batch of ten is now shared instead of available to each job.

The second regression fills the foreground allowance, verifies no background
queries start, retires the foreground queries, and injects dial failures until
all ten provider keys and ten record keys have received background work.
It checks the two-query ceiling and one-new-query allowance after every poll.

#### Verified Background Checks

| Check | Result |
| --- | --- |
| Negative control | Shared-capacity regression failed before the fix |
| Admission/fairness regressions | Both passed; foreground preserved and both record classes resumed |
| Workspace | 1,261 passed; 22 opt-in tests ignored |
| DHT, forced-relay pairing, peerless pairing namespaces | Passed |
| Owned QUIC and relay/direct network-move namespaces | Passed |
| Clippy correctness, suspicious, performance groups | Passed; nonfatal style warnings remain |
| Nix source parity | Passed with cached tools and unchanged assertions |
| Android native x86_64 | Built offline with the patched library |
| Root/changed-vendor formatting and whitespace | Passed |

Logs use `/tmp/p2p-vpn-kad-jobs-` with `before.log`, `after.log`,
`workspace.log`, `clippy.log`, `dht.log`, `relay.log`, `code-pairing.log`,
`quic.log`, `move.log`, `nix.log`, and `android.log` suffixes.

The admission tests inject failures without opening sockets. These checks do
not establish aggregate foreground admission, long-duration healthy settling,
socket rates, public-WAN behavior, or physical-device acceptance.
Full Nix package and ARM64 native builds were not repeated.

#### Provider Cancellation

Pairing cleanup clears provider query IDs and returns stop-provider locators.
Previously, `stop_providing` only removed the local store record: active queries
and the background job's snapshot survived. The regression failed on its first
cancel/reopen cycle before the fix.

| Owner | Cancellation behavior |
| --- | --- |
| Query pool | Remove all provider queries for the stopped key, without starting another phase |
| Background job | Remove matching records from the pending republication snapshot |
| Behaviour queue | Remove matching requests/results and unneeded queued dials |
| Other queries | Preserve unrelated work, including lookups for the same key and shared dials |
| Dispatched work | Not recalled; handler requests and remote announcements may outlive cancellation |

Four regressions cover 64 cancel/reopen cycles, exclusive/shared queued dials,
and stopping ten providers during an active republication snapshot. Workspace
verification passed 1,265 tests with 22 opt-in tests ignored. Logs:
`/tmp/p2p-vpn-kad-stop-before.log` and `...-workspace.log`.

The additive `cancel_query` API supports immediate local retirement. Unlike
graceful `QueryMut::finish`, it does not emit completion or start another phase.
Callers must discard ownership of the canceled ID.

Handler queue enforcement is now covered by the
[aggregate-bounds checkpoint](kademlia-aggregate-bounds.md#implemented-admission).
Aggregate routing now includes retained generations, buffers, and notifications.
Total query admission and sustained process-resource measurements remain open.
Synthetic cancellation tests do not measure remote record expiry.

#### Join Lookup Ownership

The join lookup owner now lives in `CodePairingSessions`, outside the replaceable
join operation. Its single slot retains the operation ID and query ID until
completion or explicit retirement. New lookups wait for that slot to clear.

| Transition | Behavior |
| --- | --- |
| Active operation | Keep the lookup and suppress duplicate admission |
| Cancel / expire / fail / complete | Retire the lookup on the next discovery-driver tick |
| Replace terminal operation | Preserve the old lookup owner until retirement |
| Late provider result | Count only for the active operation that owns the lookup |
| Final query event | Release ownership even if its operation has ended |
| Process restart | Restore no runtime query IDs; persisted pairing format is unchanged |

The driver runs on the existing one-second pairing timer. This is a scheduling
interval, not a hard cancellation deadline under load. Already dispatched
network requests retain the limitations described under Provider Cancellation.

Regression coverage exercises 64 terminal/replacement transitions, late results,
and completion after cancellation. A runtime-driver test checks actual query
retirement while preserving an unrelated lookup. No new configuration is needed.

#### Verified Join Checks

| Check | Result |
| --- | --- |
| Workspace | 1,268 passed; 22 opt-in tests ignored |
| Clippy correctness, suspicious, performance groups | Passed; style warnings remain |
| Peerless code pairing / forced-relay pairing | Passed: 13.28 / 7.53 seconds |
| DHT / owned QUIC / relay-direct network move | Passed: 16.73 / 16.04 / 52.35 seconds |
| Native x86_64 Android | Built offline in 37.68 seconds; existing native-target warnings remain |
| Nix source parity / formatting / whitespace | Passed |

Logs use `/tmp/p2p-vpn-kad-join-` with `workspace`, `clippy`, `code`, `relay`,
`dht`, `quic`, `move`, `android`, and `nix` `.log` suffixes. Nix source output:
`/nix/store/idl62sqh9h74mhn6jbx5ims1g5x6vn82-p2p-vpn-rust-test-sources`.

No full Nix package, ARM64 build, or physical-device deployment was performed.
These checks do not establish aggregate resource bounds or performance gains.

#### Verified Cancellation Checks

| Check | Result |
| --- | --- |
| Workspace | 1,265 passed; 22 opt-in tests ignored |
| Clippy correctness, suspicious, performance groups | Passed; style warnings remain |
| DHT / forced-relay pairing / peerless code pairing | Passed: 16.64 / 7.48 / 13.28 seconds |
| Owned QUIC / relay-direct network move | Passed: 15.99 / 51.39 seconds |
| Native x86_64 Android library | Built offline in 32.72 seconds |
| Nix source parity | Desktop and both Android source inputs match using cached tools |
| Formatting / whitespace | Passed |

Logs share `/tmp/p2p-vpn-kad-stop-` with `workspace`, `clippy`, `dht`,
`relay`, `code-pairing`, `quic`, `move`, `android`, and `nix` `.log` suffixes.
The build targets and vendor cache total approximately 4.6 GiB.

Android library SHA-256:
`0717203ac33357ec7fecf3a7ea1f6de4d788cc57ecb09483654cb09293dc05f1`.
Nix source check output:
`/nix/store/4m7b340p515nc5fbj68n6c0hz8lc2rfy-p2p-vpn-rust-test-sources`.

No full Nix package, ARM64 build, or physical-device deployment was performed.
Namespace timings are smoke-test observations, not performance comparisons.

### Per-Query Retention

All three production DHT constructors enable shared candidate/address accounting.
The library default remains unbounded for callers that do not configure limits.

| Limit | Value | Rationale |
| --- | ---: | --- |
| Candidate identities per phase | 256 | More than twelve standard 20-peer response sets, while bounding cumulative iterator state |
| Addresses per query peer | 64 | Matches routing-address admission |
| Encoded bytes per address | 2,048 | Matches routing-address admission |
| Encoded query address bytes | 256 KiB | Allows roughly 5,500 typical 47-byte addresses without permitting every candidate to fill its worst-case allowance |

Initial candidates and response candidates share one lifetime identity budget.
Rejected identities enter neither the address map nor the closest/disjoint iterator.
Fixed queries also cap initial candidates; their original quorum is not reduced.
Heavy branching can produce fewer results, just as finite search time can.

Failed addresses release byte capacity, but their identities remain counted.
Migration checks individual and aggregate byte budgets, collapses duplicates,
and preserves the old address on rejection. Query retirement drops the owner.
`QueryRef::resource_usage()` exposes current candidates, encoded bytes, and rejects.

The loopback regression now uses
`internal_kademlia_query_addresses_remain_bounded`. A deliberately unbounded
responder advertises 65 addresses. Production limits retain 64 (3,008 bytes);
a 1,024-byte test budget retains 21 (987 bytes) and rejects excess candidates.

The four combinations cover both DHT layouts and both byte budgets, 100 address
migrations each, individual/aggregate size rejection, duplicate collapse, and
failed-address cleanup. This regression now runs in the normal workspace suite.

#### Verified Query Checks

| Check | Result |
| --- | --- |
| Workspace | 1,259 passed; 22 opt-in tests ignored |
| Fixed-query comparison | 512 dial actions without limits; 256 with limits; original quorum preserved |
| Loopback query retention | Four budget/layout combinations passed, including retirement cleanup |
| DHT-discovered overlay | Passed |
| Forced-relay live pairing | Passed |
| Peerless code pairing | Passed |
| Owned QUIC packet plane | Passed |
| Relay/direct network-move recovery | Passed |
| Workspace Clippy correctness, suspicious, performance groups | Passed; nonfatal style warnings remain |
| Nix source parity | Passed with cached tools and unchanged sandbox assertions |
| Android native x86_64 | Built offline with the patched library |
| Root/changed-vendor formatting and whitespace | Passed |

Logs use `/tmp/p2p-vpn-kad-query-`: `workspace-verified.log`,
`churn-final.log`, `dht.log`, `relay.log`, `code-pairing.log`, `quic.log`,
`move.log`, `clippy.log`, `nix-verified.log`, and `android.log`.

The separate dependency-package Clippy command returned a cached success without
a compiler invocation; it is not counted as independent vendor lint evidence.
Vendor behavior is exercised through the workspace tests and native compilation.

The fixed-query comparison injects dial failures without opening sockets. It is
not a network dial-rate, CPU, or RSS benchmark. No physical-device, public-WAN,
ARM64-native, or full Nix-package validation is claimed for this patch.
Active-query admission and measurement gates remain open. Aggregate routing
enforcement is covered by the [phase-1 checkpoint](kademlia-aggregate-bounds.md#aggregate-routing-admission).

### Routing-Address Owner

Both runtime DHTs enable a 64-address limit per routing peer and a 2,048-byte
limit per encoded address. The library's default remains unbounded unless its
caller opts in. These per-peer limits now compose with the shared aggregate
routing budget; total query-memory admission remains unfinished.

The standalone pre-network code-pairing host also uses this shared configuration,
including disabled automatic bootstrap and protected seeds/candidate hints.
Its explicit initial bootstrap remains enabled. A constructor-level test checks
that seed retention and address limits apply before a network instance exists.

| Mutation | Enforcement |
| --- | --- |
| Explicit insertion | Reject oversized input; rotate unprotected entries at capacity |
| Confirmed connection | Filter oversized endpoints before entry creation; bounded insertion for existing/pending entries |
| Address change | Bounded replacement, duplicate collapse, no replacement with oversized input |
| Configured seed | Explicit protection, included in capacity, removable through normal APIs |
| Churn | Refresh recency; prefer same-category eviction, then a category with multiple addresses |

The loopback regression is now named
`internal_kademlia_connection_addresses_remain_bounded`. It requires 64 retained
addresses after 65 connections in both DHT layouts, replacing the old diagnostic's
65-address expectation.

The original query-cache diagnostic used a deliberately unbounded responder.
Its historical 65-address observation is now replaced by the enforcement
regression described under Per-Query Retention.

#### Fixture Corrections

- Pending insertion requires a connected candidate and a full bucket of disconnected entries; the fixture now injects that transition with deterministic identities.
- The existing application-retention fixture now protects its synthetic configured seed in both owners, matching production startup; its expiry assertion is unchanged.
- The pending-entry test injects oversized and valid address-change events without consuming routing events, then inspects the removed entry.

Initial failed logs remain at `/tmp/p2p-vpn-kad-address-tests.log` and
`/tmp/p2p-vpn-kad-address-workspace.log`. They are not counted as passing runs.

Review added a regression for singleton-category preservation and migration
recency. It failed before the eviction correction; the negative-control log is
`/tmp/p2p-vpn-kad-address-churn-before.log`. Fresh migrations now move to the end
of the eviction order, and protected entries remain ineligible eviction targets.

#### Verified Routing Checks

| Check | Result |
| --- | --- |
| Workspace | 1,254 passed; 23 opt-in tests ignored |
| Routing/query loopback checks | Both passed in 3.38 seconds; routing bounded at 64, query diagnostic still at 65 |
| Clippy correctness, suspicious, performance groups | Passed; existing style warnings remain |
| Direct UDP namespace | Passed, 40.18 seconds |
| Owned QUIC namespace | Passed, 16.08 seconds |
| Forced-relay pairing namespace | Passed, 7.61 seconds |
| Peerless code-pairing namespace | Passed, 13.32 seconds |
| Native x86_64 Android library | Built in 41.89 seconds using cached Nix Rust and NDK tools |
| Source parity | Desktop and both Android source inputs match |
| Formatting / whitespace | Workspace and changed vendor files passed |

Logs use `/tmp/p2p-vpn-kad-address-` with `workspace-verified.log`,
`retention-verified.log`, `clippy-verified.log`, `udp-verified.log`,
`quic-verified.log`, `relay-verified.log`, and `code-pairing-verified.log`.

Android output: `/tmp/p2p-vpn-android-target/x86_64-linux-android/debug/libp2p_vpn_android.so`.
SHA-256: `34124cb7599ac886bcfa6623034ad7828892428d194df6b887c716bc7c7748ef`.
Log: `/tmp/p2p-vpn-kad-address-android-verified.log`.

Nix source-parity output:
`/nix/store/8bh8nb3y3kxcva5zn0p41in4h27x18k9-p2p-vpn-rust-test-sources`.
This uses the cached-tool sandbox method described above, not a full package build.
Log: `/tmp/p2p-vpn-kad-address-nix-sources-verified.log`.

Namespace durations are smoke-test observations, not comparable performance
measurements. Targets plus the repaired vendor cache occupy approximately 4.6 GiB.
No device was deployed, no public-network test ran, and no aggregate memory ceiling
or upstream standalone test-suite pass is claimed by these application checks.

## Patch Constraints

- Retain libp2p identity, security, transports, and DHT wire format.
- Preserve fresh LAN/public/relay alternatives under address churn.
- Do not grant routing identities overlay membership or change minimal configuration requirements.
- Keep any third-party patch focused, pinned, licensed, and shared across build targets.
- Do not substitute visible routing-event cleanup for inaccessible query/pending-state enforcement.

## Resource Budget

| Resource | Limit / Practice |
| --- | --- |
| Cargo jobs | At most two; reuse existing target and cached Nix tools |
| Nix builds | Inspect plan first; one job, two cores; avoid compiler bootstrap |
| Build downloads | At most 10 Mbps |
| Task temporary storage | At most 10 GiB; baseline review artifacts total approximately 2.2 GiB |
| Performance capture | No concurrent task builds; retain failed attempts separately |

## Evidence Limits

- Simulated-time cooldown tests are not sustained process-resource measurements.
- Loopback and namespace tests are not physical WAN/NAT acceptance.
- No Lean model currently covers this scheduler; executable invariant tests remain required.
- Broader lifecycle auditing and final cross-platform acceptance remain separate workstreams.
