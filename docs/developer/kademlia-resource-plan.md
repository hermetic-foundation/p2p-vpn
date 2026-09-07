# Kademlia Resource-Limits Workstream

## Status

Active. This workstream does not complete the broader reliability review.
Starting revision: `5ecb01ea`. No deployed service or physical device has changed.

## Completion Gates

| Area | Required Evidence | Status |
| --- | --- | --- |
| Internal addresses | Count/byte bounds for present and pending buckets, address changes, and query caches | Open |
| Query state | Bounded candidate identities, active queries, and retained results | Open |
| Scheduling | Bounded bootstrap, discovery, and dial activity under failure and churn | Open |
| Recovery | LAN-first lookup, relay fallback, network-change recovery, and healthy-path settling | Open |
| Measurements | Comparable before/after CPU, RSS, sockets, dial rates, and query rates | Open |
| Packaging | Matching Cargo, desktop Nix, and Android source inclusion | Open |
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

This source finding needs a runtime regression. Do not attribute observed socket
activity to this trigger without measuring it.

### Candidate Identities

The query-local address map is not the only owner. `ClosestPeersIter::on_success`
also inserts reported identities into its distance-ordered map. Limiting address
vectors alone would leave cumulative candidate identity retention unbounded.

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
