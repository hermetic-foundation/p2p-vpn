# Kademlia Retention Review

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
2. Exercise query-local retention with bounded synthetic peer responses.
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
