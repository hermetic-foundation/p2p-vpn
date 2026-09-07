# Kademlia Retention Review

Current follow-up: [resource-limits workstream](kademlia-resource-plan.md).
The audit below records the original diagnostics and patch proposal. The current
workstream distinguishes closed per-peer gaps from remaining aggregate/query limits.

The follow-up enforces per-peer routing-address limits, protects configured seeds,
and adds per-query candidate/address budgets. The original measurements below
are historical; current validation and remaining aggregate limits are in the plan.

## Scope

Source audit against pinned `libp2p-kad 0.48.0`, after the public-pairing Identify
fix in `9c5236ad`. This is not an exploit reproduction or a global memory bound.

## Address Owners

| Owner / Mutation | Pinned Source | Current Protection |
| --- | --- | --- |
| Explicit application insertion | `behaviour.rs:582`, `add_address` | Discovered-address admission is bounded; configured seeds are protected. |
| Confirmed outbound connection | `behaviour.rs:2285`, `ProtocolConfirmed` | Calls `connection_updated` internally, outside application admission. |
| Existing bucket entry update | `behaviour.rs:1331`, `connection_updated` | Appends distinct addresses; does not consult `BucketInserts` for existing peers. |
| Pending bucket entry update | `behaviour.rs:1355`, `connection_updated` | Appends internally; this branch does not emit `RoutingUpdated`. |
| Query-discovered addresses | `behaviour.rs:1215`, `discovered` | Replaces each reported peer's vector; retains distinct peer entries in a query-local map. |
| Connection address change | `behaviour.rs:2043`, `on_address_change` | Replaces matching bucket/query addresses without a `RoutingUpdated` event. |
| Outbound dial candidates | `behaviour.rs:2246`, `handle_pending_outbound_connection` | Combines bucket addresses with addresses from ongoing queries. |

These paths apply to both the primary and separate public-pairing DHT.
`handle_public_pairing_kademlia_event` currently handles provider-query completion,
not bucket-address reconciliation.

## Existing Bounds

| Bound | What It Does Not Prove |
| --- | --- |
| Default packet maximum: 16 KiB (`protocol.rs:51`) | Does not bound cumulative addresses across multiple responses. |
| Default query timeout: 60 seconds (`query.rs:273`) | Limits query lifetime when polled, not peak retained bytes or number of queries. |
| Query parallelism configured by p2p-vpn | Bounds concurrent contacts within a query, not all cached reported identities. |
| Finite routing buckets | Does not bound each peer's address vector. |
| p2p-vpn's discovery retention budgets | Apply only to addresses admitted through that owner. |

The query address map is `FnvHashMap<PeerId, SmallVec<[Multiaddr; 8]>>`
(`query.rs:299`). Eight is inline capacity, not an address limit. `discovered`
stores reported addresses before passing candidate identities to the query strategy.

## Rejected Shortcuts

- **Manual bucket insertion alone:** existing and pending entries still update.
- **`RoutingUpdated` cleanup alone:** misses pending entries and address changes.
- **Query timeout as a memory limit:** does not establish a peak allocation bound.
- **Reinsert every event snapshot:** application insertion itself emits routing
  events; stale snapshots could restore addresses already evicted.

## Next Verification

1. Add enforcement for internally learned bucket addresses and turn the diagnostic
   below into a regression that requires the bound to hold.
2. Extend the query-local address diagnostic below to cumulative peer growth and heap measurements.
3. Verify pending entries and address changes, not only `RoutingUpdated` events.

Any reconciliation must preserve configured seeds, active connections, and fresh
LAN/relay alternatives. Address pruning must not become a reconnect loop or
authorize public routing identities as overlay members.

## Local-Swarm Reproduction

`runtime::p2p::tests::measure_internal_kademlia_connection_address_retention`
uses real TCP loopback listeners and one remote identity. It waits for each
Kademlia routing update, disconnects, and repeats through 65 distinct listener ports.

| DHT Mode | Completed Connections | Retained Addresses After Disconnect |
| --- | ---: | ---: |
| Shared public DHT | 65 | 65 |
| Private primary plus public-pairing DHT | 65 | 65 |

The final combined diagnostic passed in 2.84 seconds. Its success means the internal
growth was reproduced, not that the 64-address application bound held.

All 31 enabled p2p-module tests also passed; the diagnostic is opt-in and was run
separately. No production implementation changes are included in this milestone.

- Public bootstrap seeds are removed before polling either swarm.
- mDNS, AutoNAT, DCUtR, and provider advertisement are disabled.
- No application Identify/admission handler is invoked.
- Each mode has a 30-second outer deadline; dropping the swarms closes listeners.

This establishes library-retained bucket growth beyond the application's limit.
It does not measure RSS, execute the complete daemon event adapter, or demonstrate
a public-network attack. The enforcement gap remains open.

```bash
cargo test --offline --locked --lib \
  measure_internal_kademlia_connection_address_retention -- --ignored --nocapture
```

## Query-Local Reproduction

Historical baseline below. The replacement regression is
`internal_kademlia_query_addresses_remain_bounded`; see
[current limits and evidence](kademlia-resource-plan.md#per-query-retention).

`runtime::p2p::tests::measure_internal_kademlia_query_address_retention` runs
two real loopback swarms. The responder reports 65 addresses for one synthetic
peer in a closest-peers response; the client never admits it to a routing bucket.

| DHT Mode | Query Addresses | Encoded Address Bytes | Candidates After Query Retirement |
| --- | ---: | ---: | ---: |
| Shared public DHT | 65 | 3,055 | 0 |
| Private primary plus public-pairing DHT | 65 | 3,055 | 0 |

The two-mode diagnostic passed in 0.07 seconds. It reads the addresses through
`NetworkBehaviour::handle_pending_outbound_connection`, verifies bucket absence,
then explicitly finishes and polls the query before checking candidate cleanup.

```bash
cargo test --offline --locked --lib internal_kademlia_query_addresses_remain_bounded \
  -- --nocapture
```

### Interpretation

- Query caching bypasses the application's 64-address admission limit, independently of bucket insertion.
- Encoded bytes sum retained multiaddress lengths, including normalized peer suffixes; this is not heap allocation or RSS.
- Explicit completion releases query candidates; this does not establish timeout behavior under load or immediate allocator reclamation.
- One response and one reported identity do not measure cumulative peer growth, concurrent-query memory, or a public-network attack.
- The diagnostic expects current retention behavior. A future bound-enforcing patch must replace that expectation, not keep it as a required contract.

### Isolation and Verification

- Public bootstrap entries are removed before polling; mDNS, AutoNAT, DCUtR, and provider advertisements are disabled.
- The source connection uses loopback TCP; reported `/memory` addresses have no supported transport and cannot contact unrelated services.
- Each retention/cleanup phase has a ten-second deadline; dropping both swarms closes their listeners.
- The existing bucket diagnostic and new query diagnostic share only their test configuration helper.
- No dependency, production runtime, wire protocol, or configuration change is included.
- Logs: `/tmp/p2p-vpn-review-query-retention.log` and `/tmp/p2p-vpn-review-query-retention-p2p.log`.

The combined module run passed all 34 tests, including both opt-in diagnostics,
in 17.05 seconds. After a redundant-closure cleanup, all 34 passed again in
17.11 seconds (`/tmp/p2p-vpn-review-query-retention-p2p-final.log`). Required Clippy
groups, changed-file formatting, whitespace, and Nix test-source inclusion passed.

The full workspace was not repeated for this diagnostic-only change; its prior
1,232-test pass is recorded separately. No VM, emulator, or physical device was
started. Existing build directories were reused, remaining about 3.6 GiB combined.

## Reproduce the Audit

Inspect the exact Cargo-locked source, not documentation for a newer release:

```bash
rg -n 'connection_updated|on_address_change|fn discovered|BucketInserts' \
  PATH_TO_VENDOR/libp2p-kad-0.48.0/src/behaviour.rs
rg -n 'timeout:|struct QueryPeers|addresses:' \
  PATH_TO_VENDOR/libp2p-kad-0.48.0/src/query.rs
```

The source audit used the installed Nix vendor directory. The local-swarm
diagnostic used the cached Nix Rust toolchain; no dependency upgrade, external
network test, or additional toolchain build was performed.

## Enforcement Decision

A pinned crate patch is proposed, pending the user's preference about maintaining
modified third-party code. No dependency or runtime change has been made for it.

| Public Hook | Limitation Verified in Pinned Source |
| --- | --- |
| `KBucketRef` | Exposes present entries and `has_pending`, not pending entry contents. |
| `QueryMut` | Exposes ID, query info, statistics, and `finish`; no address-cache mutation. |
| `QueryRef` | Exposes information/statistics, not retained address bytes. |
| Routing events | Do not cover every pending-entry or address-change mutation. |

An application sweep can improve visible bucket retention, but cannot enforce
strict limits on inaccessible query caches. Cancelling queries solely on elapsed
time or request counts does not measure their retained addresses.

### Proposed Patch Boundary

1. Keep libp2p's identity, transport, DHT wire format, and query semantics.
2. Bound insertion/replacement inside bucket address and query-cache owners.
3. Cover confirmed connections, pending entries, peer responses, and address changes.
4. Test size/count overflow and preserve useful alternatives under churn.
5. Convert the loopback diagnostic into bound-enforcing regression coverage.

### Packaging Requirements

| Surface | Required Work If Approved |
| --- | --- |
| Source provenance | Retain crate version, license, upstream checksum, and a focused change record. |
| Cargo | Use one pinned source for desktop, Android, tests, and offline development. |
| Desktop Nix | Extend `flake.nix`'s explicit `rustSource` fileset. |
| Android Nix | Extend `nix/android.nix`'s explicit `nativeSource` fileset. |
| Verification | Test lock/source parity, native Android compilation, and discovery/recovery scenarios. |

The installed upstream crate source occupies approximately 688 KiB. This is a
source-size observation, not a build-space estimate. Reuse existing targets and
check derivation plans before starting any rebuild.
