# Cooperative Membership Checkpoints

## Status

Core, protected storage, bounded snapshot transfer, and version-3 daemon
restoration are implemented. Ordinary existing networks still use the legacy
ledger: automatic provisioning/migration and pairing integration remain pending.

No new configuration switch enables checkpoints yet. Self-departure delivery,
full retained-artifact cleanup, and deployed multi-node evidence remain required.

See [Acceptance](membership-checkpoint-acceptance.md) for the completion audit.

## Consistency Contract

| Rule | Behavior |
| --- | --- |
| Authority | Any participating member can admit, revoke, resign, or change policy. |
| Offline member | Remains admitted; disconnection is not revocation. |
| Singleton progress | No quorum or designated checkpoint authority is required. |
| Return/restart | Authenticate and reconcile before packet authority or mutations. |
| Partition | Select an authenticated branch deterministically. |
| Losing branch | Discard its state, including revocations; an operator may revoke again. |
| Replay | Reject an older snapshot or mutation against the wrong current boundary. |
| History | Retain the active set and compatible current names, not past devices. |

This is cooperative eventual consistency, not Byzantine consensus. A former
holder of the shared capability can manufacture a competing branch and its
score. The user has explicitly accepted this limitation for the current design.

## Authentication

| Object | Authentication |
| --- | --- |
| Network anchor | Pinned immutable network identity and supported policy/rank versions. |
| Snapshot | Domain-separated HMAC-SHA256 with an HKDF-derived network capability. |
| Mutation | Issuer signature, active membership, and exact base checkpoint. |
| Snapshot offer | Publisher signature, transport identity, and fresh resync challenge. |
| Hostname claim | Subject signature bound to network identity and admission incarnation. |

The capability requires a shared secret of 32-4,096 bytes. A public network name
or discovery tag is not secret authority. Initial formation and trusted legacy
migration must provision the same anchor and secret through authenticated pairing.

Credentials are not part of public peer inventories or diagnostics. Capability
debug output omits the MAC key; offers contain no raw network secret.

## Branch Selection

```text
(authority_revision, roster_member_count, canonical_snapshot_digest)
```

Compare lexicographically. Count unique admitted members, including offline
members, rather than locally observed connections. Expiry is enforced on access;
an explicit expiry-pruning mutation removes expired entries from retained state.

| Update | Rank Effect |
| --- | --- |
| Genuine roster/policy change | Increment authority revision. |
| No-op | Reject; cannot increase revision. |
| Hostname rename | Independent sequence; no authority revision increase. |
| Reconnect or empty resync | No rank change. |
| Equal revision and roster count | Canonical digest provides a deterministic tie-break. |

The parent digest is an opaque predecessor boundary, not a retained ledger.
Rank selects a branch; it does not prove globally current or honest authority.

## Lifecycle

1. Restore the authenticated snapshot with `ResyncRequired` authority gating.
2. Open a fresh nonce-bound, monotonic-time resync window; the daemon uses 15 seconds.
3. Collect authenticated offers and keep only the best snapshot and current names.
4. Finish the window, install the chosen state, and report aggregate changes.
5. Participate only if the local identity remains active in the chosen snapshot.

An isolated survivor may finish without an offer. This preserves availability,
but cannot establish that a disconnected partition has no newer state.

### Runtime Ownership

| Stage | Behavior |
| --- | --- |
| Restart | Version-3 state restores gated; static peers and completed pairing artifacts cannot restore grants. |
| Catch-up | Query retained peers and bounded matching-scope control-only candidates, including newly admitted publishers. |
| Live refresh | Keep the installed packet authority while reconciling; reject membership mutations during the round. |
| Install | Prepare projection, atomically persist selected authority, then commit forwarding state. |
| Visible-write failure | Keep selected authority gated; never restore older grants; an isolated survivor can retry resync. |
| Names | Reconcile the local signed hostname independently of authority rank. |
| Revocation | Existing control API installs an active-only snapshot and refreshes routes, inventory, and DNS. |

Matching advertisements permit catch-up only. Candidates are capped at 32 and
expire after 15 seconds; the authenticated offer must pass core validation before
it can authorize any peer. Public IPFS peers are not blindly queried for rosters.

Periodic connected-peer refresh runs no more frequently than once per minute
unless a higher advertised rank prompts catch-up. A capability advertisement
does not establish global freshness.

Transfer framing, proofs, resource limits, and compatibility are documented in
[Snapshot Transfer](membership-checkpoint-wire.md).

### Self-Departure

1. Sign the final exact-boundary removal before local exclusion.
2. Capture the bounded recipient set and hand the command to surviving peers.
3. Remove local authority and stop ordinary publication.
4. Returning peers obtain the resulting active-only snapshot from a survivor.

Transport integration must bound handoff retries and lifetime. No excluded
publisher exception or permanent departure proof is stored in the core state.

The core supports this transition; daemon self-departure handoff is not wired yet.
Checkpoint-mode resignation currently returns an explicit unsupported error.

## Names And Re-Admission

| Case | Rule |
| --- | --- |
| Rename | Greatest signed sequence wins independently of snapshot rank. |
| Equal-sequence conflict | Lexicographically greater canonical label wins. |
| Revocation | Remove the member and its hostname claims. |
| Re-admission | Derive a new incarnation from the current boundary and subject. |
| Old name/admission replay | Reject the old incarnation or stale mutation base. |

Cosmetic conflicts cannot stall membership reconciliation. Checkpoints do not
retain inviter identity, old admission proofs, or per-device revocation markers.

## Resource Bounds

| Surface | Limit |
| --- | --- |
| Roster | 256 members; policy may set a lower limit. |
| Route grants | 32 per member. |
| Snapshot encoding | 2 MiB. |
| Snapshot offer/retained encoding | 4 MiB. |
| Shared capability | 32-4,096 bytes. |
| Resync | One bounded round, best snapshot, and compatible names. |

Normal authorization uses the active snapshot as a closed namespace. Absent
static peers receive no implicit permission even after their tombstones are gone.

## Verification

### Forwarding Projection

The forwarder accepts authenticated core state through a scoped prepare/commit
update. Daemon startup uses it when protected version-3 authority already exists;
legacy networks are not automatically migrated.

| Surface | Checkpoint Behavior |
| --- | --- |
| Prepare | Bind local identity and pinned anchor; reject older selected rank. |
| Commit | Reject stale or wrong-context prepared updates. |
| Authority | Replace routes, peer permissions, and effective membership together. |
| Retention | Clear legacy records and removed-device metadata/replay windows. |
| Resync | Keep the namespace sealed while packet authority is gated. |
| Re-admission | Forget replay sessions from the previous incarnation. |
| Reload | Cannot restore removed peers from stale declarative configuration. |
| Expiry | Refresh from checkpoint projection, not an empty legacy ledger. |
| Inventory | Omit absent identities, including local self-removal, without tombstones. |
| Provenance | No invented admission time or inviter identity after compaction. |

Runtime integration must persist the selected core state before announcing it.
After a visible checkpoint replacement, route/commit failures must fail closed
or retry the selected authority; they must not restore stale grants as a fallback.

### Diagnostics

`daemon-status` and `daemon-state` include aggregate `checkpoint_*` fields for
checkpoint instances. They do not export credentials, discarded rosters, or a
per-device revocation archive.

| Field | Meaning |
| --- | --- |
| `checkpoint_sync_state` | `resync_required`, `resyncing`, `participating`, or `excluded`. |
| `checkpoint_authority_revision` | Installed authority revision. |
| `checkpoint_active_members` | Retained roster population, including offline members. |
| `checkpoint_pending_requests` | Owned outbound page requests. |
| `checkpoint_buffered_bytes` | Reserved snapshot-transfer memory. |
| `checkpoint_retired_transfer_slots` | Short-lived replay slots, not durable device history. |
| `checkpoint_offers_accepted` | Authenticated offers collected by this daemon. |
| `checkpoint_transfer_failures` | Aggregate failed transfers. |
| `checkpoint_decisions_may_have_been_discarded` | Last selection may have discarded losing-branch decisions. |

### Commands

```sh
nix develop -c cargo test --locked --lib membership::checkpoint
nix develop -c cargo test --locked --lib runtime::checkpoint_runtime
nix develop -c cargo test --locked --lib runtime::control
```

The 30 core tests cover singleton progress, offline return, deterministic forks,
accepted losing-branch rollback, stale replay, hostname conflicts, re-admission,
creator departure, route policy, migration, and thousands of churn cycles.

Runtime tests cover real TCP/Noise catch-up, an excluded requester, a publisher
missing from the stale roster, isolated recovery, durable revocation, DNS gating,
bounded candidates, failed-transfer retirement, and the existing daemon control API.

These tests do not establish a deployed checkpoint network. Provisioning,
automatic migration, pairing/self-departure, Android sync UI, full artifact
erasure, and multi-daemon/NixOS end-to-end evidence remain required for completion.
