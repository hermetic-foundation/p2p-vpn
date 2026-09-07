# Kademlia Resource-Limits Workstream

## Status

Active. This workstream does not complete the broader reliability review.
Starting revision: `5ecb01ea`. No deployed service or physical device has changed.

## Completion Gates

| Area | Required Evidence | Status |
| --- | --- | --- |
| Internal addresses | Count/byte bounds for present and pending buckets, address changes, and query caches | Per-peer routing verified; aggregate/query limits open |
| Query state | Bounded candidate identities, active queries, and retained results | Open |
| Scheduling | Bounded bootstrap, discovery, and dial activity under failure and churn | Cooldown and automatic bootstrap fixed; aggregate audit open |
| Recovery | LAN-first lookup, relay fallback, network-change recovery, and healthy-path settling | Open |
| Measurements | Comparable before/after CPU, RSS, sockets, dial rates, and query rates | Open |
| Packaging | Matching Cargo, desktop Nix, and Android source inclusion | Source parity and native x86_64 Android verified; final checks open |
| Delivery | Regression tests, broader validation, documentation, atomic verified pushes | In progress |

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

The query-local address map is not the only owner. `ClosestPeersIter::on_success`
also inserts reported identities into its distance-ordered map. Limiting address
vectors alone would leave cumulative candidate identity retention unbounded.

### Routing-Address Owner

Both runtime DHTs enable a 64-address limit per routing peer and a 2,048-byte
limit per encoded address. The library's default remains unbounded unless its
caller opts in. This is not yet an aggregate routing or query-memory budget.

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

The query-cache diagnostic uses a deliberately unbounded responder. Otherwise
limiting the responder could hide the client's independent query-cache gap.
Its 65-address client expectation remains diagnostic evidence, not desired behavior.

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
