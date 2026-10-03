# Cooperative Membership Checkpoints

## Status

The core protocol is implemented and unit-tested. Runtime activation, wire
negotiation, pairing migration, and user-visible sync status remain integration
work. Existing networks still use the legacy membership ledger.

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
2. Open a fresh nonce-bound, monotonic-time resync window of at most 60 seconds.
3. Collect authenticated offers and keep only the best snapshot and current names.
4. Finish the window, install the chosen state, and report aggregate changes.
5. Participate only if the local identity remains active in the chosen snapshot.

An isolated survivor may finish without an offer. This preserves availability,
but cannot establish that a disconnected partition has no newer state.

### Self-Departure

1. Sign the final exact-boundary removal before local exclusion.
2. Capture the bounded recipient set and hand the command to surviving peers.
3. Remove local authority and stop ordinary publication.
4. Returning peers obtain the resulting active-only snapshot from a survivor.

Transport integration must bound handoff retries and lifetime. No excluded
publisher exception or permanent departure proof is stored in the core state.

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

```sh
nix develop -c cargo test --locked --lib membership::checkpoint
```

The 30 core tests cover singleton progress, offline return, deterministic forks,
accepted losing-branch rollback, stale replay, hostname conflicts, re-admission,
creator departure, route policy, migration, and thousands of churn cycles.

These tests establish core behavior, not a deployed checkpoint network. Durable
storage, wire/runtime integration, CLI/Android status, and multi-node end-to-end
evidence remain required before goal completion.
