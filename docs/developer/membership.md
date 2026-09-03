# Membership Convergence

This document defines the signed membership model and runtime convergence path.

User behavior is documented in [Network Membership](../user/membership.md).

## Invariants

| Invariant | Enforcement |
| --- | --- |
| Network names do not authorize | Every dynamic member needs accepted signed admission history. |
| Transport identity is bound | Record public keys must derive their declared peer IDs. |
| Route ownership is explicit | Only signed built-in or granted prefixes compile. |
| Updates are monotonic | `(membership_epoch, sequence)` selects the latest record. |
| Equivocation fails closed | Equal version with unequal payload is rejected. |
| Revocation survives restart | Newer tombstones remain in signed history. |
| Membership is ownerless | A creator may resign without disabling later members. |
| Provenance is not authority | Revoking an inviter does not revoke its invitees. |
| Dissemination is bounded | Record, page, snapshot, concurrency, and rate limits apply. |

## Record Schema

`SignedMembershipRecord` signs a versioned JSON payload under a domain separator.

| Field | Purpose |
| --- | --- |
| `network_name` | Binds authority to one overlay. |
| `member_peer`, `member_public_key` | Binds the subject peer ID to its key. |
| `issuer_peer`, `issuer_public_key` | Binds the signer peer ID to its key. |
| `membership_epoch`, `sequence` | Monotonic relationship version. |
| `revoked` | Marks a non-expiring authority tombstone. |
| `roles` | Grants overlay or route authority. |
| `route_grants` | Prefixes the member may originate. |
| `issued_at_unix_seconds` | Signed issue time. |
| `expires_at_unix_seconds` | Optional grant deadline. |
| `signature` | Issuer signature over the domain and payload. |

The current record version is `1`.

Portable integer fields are capped at signed 64-bit range for native Nix parity.

## Genesis And Authorization

An explicit root is a self-issued `overlay_member` record.

Its issuer and member peer IDs are equal.

The self-record anchors network history and pairing compatibility.

It does not grant permanent owner powers.

Every active member may issue a later admission or revocation event.

Event authorization uses the issuer's state at the event's signed issue time.

Removing an issuer later does not invalidate events accepted while it was active.

### Compatibility Mode

Older histories may not contain an explicit self-record.

For those histories, configured issuers remain trust roots as a compatibility rule.

Once any explicit self-record exists, only explicit roots anchor trust.

Receiver-side pairing rejects implicit multi-issuer migration into that strict mode.

## Ledger Merge

Equal-version equivocation is scoped by:

```text
(issuer_peer, member_peer)
```

The effective state for each member is ordered by:

```text
(membership_epoch, sequence)
```

Merge runs transactionally against a cloned record set.

The forwarder commits routes, peers, and authorization only after validation succeeds.

| Comparison | Result |
| --- | --- |
| New distinct signed event | Retain as bounded audit history. |
| Higher target version | Replace the target's effective state. |
| Lower target version | Keep as history without changing effective state. |
| Version and payload match | Count as already known. |
| Version matches, payload differs | Return `ConflictingRecordVersion`. |
| Record exceeds bounds | Reject the full merge. |
| Issuer was inactive at issue time | Reject incoming authority. |

Concurrent target events use revoked-first safety and stable signer/signature tie-breaks.

## Expiry and Revocation

Expiry removes a record from the effective graph without deleting signed history.

That retained latest record prevents an older grant from reviving after restart.

Revocation records must have:

| Field | Required Value |
| --- | --- |
| `revoked` | `true` |
| `roles` | empty |
| `route_grants` | empty |
| `expires_at_unix_seconds` | absent |

Revocation changes only the target member's effective state.

Previously admitted descendants remain active.

Re-admission requires a membership epoch above the revoked or expired epoch.

## Pairing Admission

Code pairing returns the joiner grant plus a bounded authorization proof.

The response is accepted only when that path reaches a local trust anchor.

The proof includes historical authorizers and their current tombstones.

A joiner therefore does not resurrect a creator that already resigned.

Both sides merge the same signed history before applying forwarding and TUN changes.

Native pairing artifacts retain that history instead of flattening it into static peers.

Pairing a revoked peer again advances its epoch and creates explicit re-admission.

## Connected-Peer Dissemination

Capabilities advertise a deterministic snapshot digest and bounded inline sample.

A digest mismatch starts `/p2p-vpn/control/1` page requests.

```text
capability snapshot differs
  -> request cursor 0
  -> validate page scope and cursor
  -> request next cursor
  -> verify total count and final digest
  -> merge the complete snapshot once
```

Partial pages never mutate membership.

A snapshot change restarts from cursor zero up to the configured bound.

| Bound | Value |
| --- | --- |
| Retained records | 256 |
| Encoded record | 12 KiB |
| Records per control page | 8 |
| Control frame | 16 KiB |
| Aggregate sync bytes | 3 MiB |
| Concurrent peer syncs | 4 |
| Snapshot restarts | 3 |
| Failed-peer retry delay | 30 seconds |
| Page requests per peer per second | 256 |

The aggregate byte limit is `record count * maximum encoded record size`.

## Pre-Authorization Connections

Unknown transport identities receive one bounded membership probe.

The block list applies after the secure transport identifies the remote peer.

| Bound | Policy |
| --- | --- |
| Active probes | One per peer; a direct path may replace a relayed path. |
| Authorization deadline | 30 seconds. |
| Rejection backoff | 30 seconds, doubling to 10 minutes. |
| Backoff reset | 30 minutes without another rejection. |
| Retained peer state | 1,024 entries with oldest-entry eviction. |
| Successful authorization | Clear the block and failure history immediately. |

An active join may temporarily admit peers in its bounded mDNS candidate set.

An open inviter must admit unknown peers until the pairing protocol authenticates them.

## Kademlia Dissemination

Kademlia publishes at most eight records in a 64 KiB bundle.

The local authorization proof is prioritized before other records.

| Property | Rule |
| --- | --- |
| DHT key | Network and optional membership-tag scoped. |
| Bundle | Versioned JSON with network and membership scope. |
| Authority | None until every signature and trust edge validates. |
| Purpose | Bootstrap authorization discovery and convergence acceleration. |
| Complete history | Retrieved from authenticated connected-peer paging. |

Public DHT writers cannot authorize themselves by publishing a matching network name.

Invalid bundles increment rejection metrics and do not partially merge.

## Runtime Reconciliation

An accepted record update changes four runtime surfaces:

1. `Forwarder` recomputes transport peers and authorized routes.
2. `OverlayMembership` recomputes connection authorization.
3. `TunRuntimeConfig` reconciles kernel routes transactionally.
4. Local control capabilities publish the new snapshot digest.

Kernel route commands have inverse operations for rollback.

Route conflicts abort before a new forwarding state is committed.

Local revoke and resign requests use the same staged route transaction.

Successful mutations persist state and advertise new capabilities immediately.

## Restart Order

Startup order matters because `p2p-vpn up` installs file-backed routes first.

```text
capture installed TUN snapshot
  -> restore prepared/applied pairing enrollments
  -> update the TUN snapshot for pairing changes
  -> load learned membership history
  -> reconcile learned kernel routes
  -> persist the canonical history
  -> begin normal swarm processing
```

The installed snapshot must represent actual kernel state.

Building it from already-restored membership would skip required route commands.

## Persistent Format

`MembershipStateStore` writes versioned JSON:

```json
{
  "version": 2,
  "network_name": "lab",
  "local_peer": "12D3KooW...",
  "records": [],
  "hostname_records": []
}
```

| Property | Enforcement |
| --- | --- |
| File type | Regular file only; symlinks rejected. |
| File mode | Group and other permission bits rejected. |
| Size | Bounded from record limits plus a 64 KiB envelope allowance. |
| Scope | Network name and local peer must match. |
| Records | Full history validates before return. |
| Hostnames | Latest peer-signed mutable name per identity validates before return. |
| Replacement | Owner-only temporary file, fsync, atomic rename. |
| Directory durability | Parent directory is synced after rename. |
| Unknown version | Startup fails closed. |

Version 1 remains readable and loads with no mutable hostname records.

The persistence revision changes when retained membership or hostname records
change.

Runtime saves are skipped while that revision remains unchanged.

## NixOS Persistence

The module always supplies these native-instance paths:

| State | Path |
| --- | --- |
| Pairing | `/var/lib/p2p-vpn/<name>/pairing-state.json` |
| Membership | `/var/lib/p2p-vpn/<name>/membership-state.json` |

Read-only module outputs expose both resolved paths for evaluation tests.

Declarative `memberRecords` supply trust anchors after rebuild or deployment.

Learned records remain mutable service state and never rewrite the Nix source.

This preserves Nix evaluation purity while supporting offline restart recovery.

Declarative `peers` remain separate static authorization.

A signed revoke refuses targets still authorized through that option.

## Observability

### Status Views

| View | Membership Evidence |
| --- | --- |
| `peers --instance NAME` | Local, static, and effective signed members with derived addresses. |
| `daemon-capabilities` | Local record count and snapshot inventory. |
| `daemon-peers` | Effective peer validation. |
| `daemon-routes` | Derived route owner and resolution. |
| `daemon-paths` | Direct or relay path selected for each member. |

### Metrics

| Group | Metrics |
| --- | --- |
| Paging | Requests, pages, restarts, completions, failures. |
| Merge | Accepted control and Kademlia records. |
| Persistence | Loads, saves, record totals, and failures. |
| Routing | Route, path, and packet-drop counters. |

Persistence metric names are listed in [User Operations](../user/operations.md).

### Structured Events

| Event Prefix | Use |
| --- | --- |
| `membership_record_page_*` | Request and page-level diagnosis. |
| `membership_record_sync_*` | Full-snapshot completion or failure. |
| `membership_state_*` | Startup load and atomic persistence. |
| `membership_authorization_*` | Expiry or trust-graph refresh. |
| `membership_probe_peer_quarantined` | Rejected peer, reason, failure count, and retry delay. |
| `membership_probe_peer_unblocked` | Expired quarantine or active pairing exception. |
| `membership_probe_peer_authorized` | Signed membership or infrastructure authorization. |

Secrets are never included in membership events.

## Test Layers

| Layer | Coverage |
| --- | --- |
| Membership unit tests | Signatures, bounds, flat authorization, conflict, expiry, and re-admission. |
| Control unit tests | Snapshot stability, cursor validation, frame-sized pages. |
| Runner unit tests | Atomic merge, sync retry, persistence, route restore. |
| Pairing tests | Authorization proofs, departed creators, and receiver anchor checks. |
| Nix evaluation | Native records and durable state wiring. |
| Four-VM convergence | Independent pairing, full mesh, restart, relay, movement. |

Run the convergence proof:

```sh
nix build --no-link \
  .#checks.x86_64-linux.nixos-vm-membership-convergence \
  -L
```

The topology and assertions are documented in [Testing](testing.md).
