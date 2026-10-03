# Cooperative Membership Checkpoints

## Status

Core, protected storage, bounded snapshot/mutation transfer, and version-3 daemon
restoration are implemented. Ordinary existing networks still use the legacy
ledger: automatic provisioning/migration and pairing integration remain pending.

No new configuration switch enables checkpoints yet. Full retained-artifact
cleanup and deployed multi-node evidence remain required.

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
| Live refresh | Keep installed authority; block local edits but process authenticated exact-base incoming commands. |
| Install | Prepare projection, atomically persist selected authority, then commit forwarding state. |
| Visible-write failure | Keep selected authority gated; never restore older grants; an isolated survivor can retry resync. |
| Kernel cleanup failure | Keep committed packet exclusion, report pending cleanup, and retry without stopping command delivery. |
| Names | Reconcile the local signed hostname independently of authority rank. |
| Revocation | Existing control API durably installs an active-only snapshot, refreshes consumers, and stages bounded command delivery. |

Matching advertisements permit catch-up only. Candidates are capped at 32 and
expire after 15 seconds; the authenticated offer must pass core validation before
it can authorize any peer. Public IPFS peers are not blindly queried for rosters.

Periodic connected-peer refresh runs no more frequently than once per minute
unless a higher advertised rank prompts catch-up. A capability advertisement
does not establish global freshness.

An incoming exact-base command advances the live refresh baseline only after
durable installation. Higher offers, compatible names, the challenge, and the
original deadline survive; startup/return gates are unchanged.

Transfer framing, proofs, resource limits, and compatibility are documented in
[Snapshot Transfer](membership-checkpoint-wire.md).

### Self-Departure

1. Sign the final exact-boundary removal before local exclusion.
2. Capture connected, authorized recipients from the old active roster.
3. Durably remove local authority and stop ordinary publication immediately.
4. Deliver only the frozen final command; receivers persist before acknowledging.
5. Returning peers obtain the resulting active-only snapshot from a survivor.

The daemon retains one command in memory for at most 15 seconds, with 32
in-flight requests and at most three attempts per recipient. Retries wait one
second; each attempt expires after five seconds without extending the deadline.

Handoff requests use existing connections only, not a hidden dial queue. The
codec checks the sender-local monotonic deadline before writing and bounds the
write by that deadline; expired queued requests cannot start transmission.

No excluded-publisher snapshot exception or permanent departure proof is stored.
Completion, deadline, or superseding authority drops the command and recipient
metadata; only aggregate delivery counts remain.

| Delivery Case | Result |
| --- | --- |
| Lost acknowledgment | Matching current result boundary confirms delivery without replaying the command. |
| Concurrent local mutation | Reject while the bounded handoff or resync is pending; do not overwrite delivery state. |
| Stale receiver | Reject the wrong base and obtain the current snapshot through normal resync. |
| Offline/no recipients | Local removal still succeeds; remote convergence is not claimed. |
| Crash during handoff | No durable command outbox; a surviving recipient is needed for later catch-up. |

The control API reports local durable application, not network-wide delivery.
An isolated creator leaving cannot guarantee that an offline survivor has learned
the removal; cooperative reconciliation may discard a losing branch's decision.

### Reply Delivery

Inbound ACK ownership binds request ID, authenticated peer, and connection ID.
At most 32 entries survive for a fixed five seconds; matching send/failure events
or timeout retire them without retaining departed-device history.

Identify preserves checkpoint-only catch-up and pending reply connections instead
of canceling an ACK solely because of exclusion. These are control-only exceptions,
not packet grants or promotion to routing infrastructure.

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
| Mutation handoff | One command; 256 unique recipients, 32 in flight, three attempts each, 15 seconds total. |
| Mutation reply ownership | At most 32 request/peer/connection entries; fixed five-second lifetime. |

Normal authorization uses the active snapshot as a closed namespace. Absent
static peers receive no implicit permission even after their tombstones are gone.

## Pending Pairing Enrollment

Signed checkpoint pairing installs a protected capability and minimum rank,
not an authoritative two-member network. The staging API is implemented and
tested; ordinary pairing RPC activation and legacy migration remain pending.

| Surface | Required Behavior |
| --- | --- |
| Approval | Validate the signed transcript and actual local key before any write. |
| Existing scope | Preserve configured secrets and saved anchor/capability pins. |
| Provisional seed | Retain only inviter/joiner descriptors; gate packet and mutation authority. |
| Persistence | Version-3 envelope adds optional `enrollment_floor`; absent for ready state. |
| Empty/insufficient resync | Remain gated; do not lower or discard the enrollment floor. |
| Activation | Observe a remote authenticated snapshot meeting the complete pinned rank. |
| Newer exclusion | Install current exclusion instead of honoring a superseded approval. |
| Write/commit failure | Preserve visible selected authority, gate participation, and retry. |
| Replay | Reuse newer retained state; do not reinstall an old seed or lower a pending floor. |
| Legacy state | Require explicit migration; staging does not silently replace the ledger. |

An older strict version-3 reader rejects pending state with the unknown floor
field. The generic core restoration API also rejects pending enrollment, so
consumers cannot accidentally drop its gate. Ready state remains byte-compatible.

One local node may still resume a previously established snapshot after a bounded
empty resync. That availability rule does not apply to an incomplete enrollment.

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
| `checkpoint_enrollment_pending` | Credentials are installed, but a qualifying remote snapshot is still required. |
| `checkpoint_enrollment_minimum_revision` | Minimum approved revision; emitted only while enrollment is pending. |
| `checkpoint_authority_revision` | Installed authority revision. |
| `checkpoint_active_members` | Retained roster population, including offline members. |
| `checkpoint_pending_requests` | Owned outbound page requests. |
| `checkpoint_buffered_bytes` | Reserved snapshot-transfer memory. |
| `checkpoint_retired_transfer_slots` | Short-lived replay slots, not durable device history. |
| `checkpoint_offers_accepted` | Authenticated offers collected by this daemon. |
| `checkpoint_transfer_failures` | Aggregate failed transfers. |
| `checkpoint_decisions_may_have_been_discarded` | Last selection may have discarded losing-branch decisions. |
| `checkpoint_handoff_active` | Whether a frozen mutation is awaiting bounded delivery. |
| `checkpoint_handoff_pending_requests` | Owned outbound mutation request IDs. |
| `checkpoint_mutation_pending_responses` | Bounded inbound reply owners awaiting send completion, failure, or expiry. |
| `checkpoint_handoff_recipients` / `checkpoint_handoff_pending` | Aggregate recipient and outstanding delivery counts. |
| `checkpoint_handoff_acknowledged` / `checkpoint_handoff_failed` | Confirmed and terminal delivery counts, not per-device history. |
| `checkpoint_handoff_attempts` | Total attempts in the current or last handoff. |
| `checkpoint_route_cleanup_pending` | Kernel route reconciliation is pending; removed members still have no packet authority. |

### Commands

```sh
nix develop -c cargo test --locked --lib membership::checkpoint
nix develop -c cargo test --locked --lib runtime::checkpoint_runtime
nix develop -c cargo test --locked --lib runtime::checkpoint_handoff
nix develop -c cargo test --locked --lib runtime::control
```

The 30 core tests cover singleton progress, offline return, deterministic forks,
accepted losing-branch rollback, stale replay, hostname conflicts, re-admission,
creator departure, route policy, migration, and thousands of churn cycles.

Runtime tests cover real TCP/Noise catch-up, an excluded requester, a publisher
missing from the stale roster, isolated recovery, durable revocation, DNS gating,
bounded candidates, failed-transfer retirement, and the existing daemon control API.

Enrollment tests cover restart, insufficient offers, configured-secret pins,
legacy rejection, superseded approvals, and initial/replacement write failures.
A 102-member roster crosses real TCP/Noise paging without enlarging pairing frames;
a joiner removed after approval receives exclusion instead of packet authority.
Handoff tests cover deadlines, scheduling fairness, local exclusion, persistence
failure, duplicate rejection, and removal without retained per-device history.
Three in-process daemons exchange authenticated commands over TCP/Noise; creator
departure, two survivor acknowledgments, route-cleanup retry, retained-state
erasure, and subsequent survivor governance pass through the real control API.

The fixture uses packet-device and route-controller adapters; it does not claim
real TUN forwarding, deployed devices, or WAN evidence.

These tests do not establish a deployed checkpoint network. Provisioning,
automatic migration, pairing/self-departure, Android sync UI, full artifact
erasure, and multi-daemon/NixOS end-to-end evidence remain required for completion.
