# Membership Checkpoint Acceptance

## Status

Implementation in progress. This is an acceptance map, not a completion report.

The peer-inventory cleanup is independent of durable checkpoint compaction.
Neither hidden rows nor a fixed history limit proves that compaction is complete.

The cooperative core is implemented; the rejected fixed-quorum prototype is gone.
Production runtime and wire activation remain incomplete. Core or storage unit
tests alone do **not** establish a functioning checkpoint network.

## Current Evidence

| Surface | Verified Result | Remaining Gap |
| --- | --- | --- |
| Peer inventory | `25243a3c` omits revoked rows from shared Linux/Android snapshots, including stale static metadata. | Durable enforcement history is still retained. |
| Runtime paths | `df78cfb3` erases unauthorized stream/datagram/relay path history and pending probes. | Other retained device state must follow checkpoint installation. |
| Forwarding projection | Checkpoint prepare/commit rejects stale contexts, seals static fallback, and erases legacy metadata. | Daemon/wire activation is still pending. |
| Rust workspace | 1,623 tests pass; 47 opt-in tests excluded, including 30 core, 23 storage, and 11 checkpoint-forwarder tests. | This is not a completed checkpoint runtime. |
| Durable checkpoint state | Version-3 authentication, legacy dispatch, rollback, failure/retry, and 128 durable churn cycles pass. | Automatic migration, activation, and a full crash campaign remain. |
| Static analysis | Formatting and required correctness/suspicious/performance Clippy groups pass. | Existing non-fatal lint warnings remain. |
| Android consumer | JVM unit tests and debug lint pass for revoked-local filtering and missing-local snapshots. | No new APK or physical-device checkpoint test. |
| NixOS VM preflight | The membership-convergence expression evaluates. | Offline dry run plans 2,859 uncached derivations; VM execution was not started. |

Existing test passes do not prove durable compaction, automatic resync, or fork
reconciliation. Those require the integration and evidence below.

## Required Behavior

| Requirement | Acceptance Evidence |
| --- | --- |
| Remove revoked devices | CLI JSON/text and Android snapshots omit their identities, names, and addresses. |
| Remove authority | Packet, route, DNS, discovery, and connection consumers agree after revocation. |
| Remove retained history | Compacted durable state contains no removed-device admission, tombstone, hostname, or inviter profile. |
| Bound storage | Repeated admit/revoke cycles retain only active state and a bounded synchronization window. |
| Advance with one online member | A single online survivor can advance without a fixed quorum or central authority. |
| Preserve offline members | Disconnection alone does not remove an admitted device from the active set. |
| Catch up on return | A stale node authenticates current state before contributing membership updates. |
| Preserve surviving members | Removing a creator or inviter does not remove independent descendants. |
| Preserve governance | Any active member may admit, revoke, and resign without acquiring owner authority. |
| Reject stale inputs on the selected branch | Pre-boundary records, stale static entries, and restarts cannot restore removed identities. |
| Expose reconciliation losses | A fork switch reports discarded local changes, including revocations. |
| Minimize provenance | Default governance does not retain unnecessary historical inviter metadata after compaction. |

Offline status is not a revocation. Local connectivity is not an authoritative
network-wide inventory of online members.

## Accepted Consistency Boundary

User clarification on 2026-10-03 accepts eventual consistency: reconciliation
may discard changes from a losing branch, including revocations. An operator
may need to revoke again after synchronizing with the selected branch.

The original no-resurrection requirement therefore applies to replay on the
selected branch, not irreversible revocation across conflicting branches. This
is an explicit user-approved tradeoff, not a claim of Byzantine-safe consensus.

| Requirement | Meaning |
| --- | --- |
| No fixed quorum | One online survivor may establish a checkpoint. |
| No central authority | Checkpoint creation does not require a designated owner. |
| Resync before participation | Returning devices reconcile before membership mutations or full network activation. |
| Cooperative fork choice | Authenticated branch advancement and member population may inform deterministic ranking. |
| Discard superseded state | Losing-branch records and removed-device metadata are not kept as a permanent archive. |
| Visible reconciliation | Sync state and discarded changes are surfaced instead of implying irreversible success. |

The [core ranking and bounded catch-up protocol](membership-checkpoints.md) are
implemented and unit-tested. A score is a convergence rule, not evidence of global
freshness or resistance to a former member manufacturing a competing branch.

### Deterministic Rank

```text
(authority_revision, active_member_count, snapshot_digest)
```

Compare lexicographically. A local revision advances only for an actual authority
change, not a no-op or transport recovery. Count unique admitted members, including
offline members; connectivity is not the membership population.

Cosmetic hostname updates need separate monotonic handling so an old name cannot
win merely because its digest sorts higher. Publisher signatures authenticate
offers, not a permanent inviter profile in the retained snapshot.

### Network Authentication

| Input | Intended Use |
| --- | --- |
| Pinned network anchor | Bind immutable network identity and supported ranking rules. |
| Secret network capability | Domain-separated snapshot authentication. |
| Public membership tag | Discovery only; not a substitute for the secret capability. |
| Active snapshot | Packet, route, naming, and mutation eligibility. |

Existing pairing responses can carry `membership_key`, but it is optional.
Keyless networks need explicit authenticated provisioning/migration; do not derive
secret authority from a public name, tag, or unauthenticated remote offer.

A former member retaining the capability can construct competing authenticated
branches. This remains an accepted limitation, not an irreversible revocation system.

### Limits Of Fork Scores

| Proposed Rule | Counterexample |
| --- | --- |
| Greatest generation or longest update history | An old signer can manufacture additional updates on an obsolete branch. |
| Largest active membership | Revoking `C` shrinks `{A, B, C}` to `{A, B}`; the stale three-member snapshot would win. |
| Largest claimed membership | Any-member admission lets a competing branch create extra identities. |
| Reachable signer means current authority | A returning device can reach a revoked signer before any surviving member. |

An authenticated signature proves who signed a snapshot, not that its branch is
globally current. A partition can leave both groups believing the other is offline.
Treat forged signatures differently from valid signatures on obsolete branches.

## Authentication Gates

| Gate | Required Result |
| --- | --- |
| Network identity | A checkpoint is bound to the existing network trust scope. |
| Snapshot integrity | Signatures cover the canonical active set and checkpoint boundary. |
| Signer authentication | Snapshot content and network scope are authenticated; old branch credentials alone do not prove global freshness. |
| Replay | Records from discarded generations cannot affect current membership. |
| Rollback | Older snapshots cannot lower a durable checkpoint boundary. |
| Equivocation | Cooperative fork choice is deterministic; score manipulation is an explicit security limitation. |
| Partition | A selected fork can discard revocations; this outcome is surfaced, not advertised as permanent revocation. |
| Full turnover | Catch-up must state its network-continuity assumption instead of claiming an unavailable authorization proof. |
| Static configuration | A stale declarative peer does not bypass checkpoint membership. |

### Stale Signer Counterexample

1. `A` goes offline while `B` and `C` are active members.
2. `B` revokes `C` and compacts a checkpoint while `A` remains offline.
3. `C` signs a conflicting snapshot using its retained old credentials.
4. `A` reconnects and still considers `C` active in its obsolete local state.

The earlier unconditional requirement to reject every such branch is superseded
by the accepted consistency boundary. Keep this case as a threat-model test:
cooperative fork choice does not establish which valid old signer is globally current.

## Storage And Compatibility

| Surface | Required Coverage |
| --- | --- |
| State file | Atomic replacement and durable generation/active-set persistence. |
| Crash recovery | Restart at each prepare/write/rename/application boundary. |
| Old state | Explicit migration with no accidental new trust root. |
| Old software | Unsupported checkpoint authority fails closed; no downgrade to stale grants. |
| Pairing state | Completed artifacts cannot reinstall discarded membership authority. |
| Hostname state | Removed metadata is pruned; old name claims cannot replay after re-admission. |
| Retained proofs | Catch-up evidence does not become an unbounded checkpoint archive. |
| Resource limits | Active-member count, encoded sizes, pending proposals, and retry state stay bounded. |

## Verification Layers

1. Signed protocol and compaction unit tests, including the stale-signer case.
2. Cross-consumer authorization and static-configuration regressions.
3. Durable-state replay, migration, corruption, and crash-boundary tests.
4. Multi-node convergence with offline catch-up and conflicting partitions.
5. Repeated churn with measured retained bytes and identity-level erasure checks.
6. CLI and Android consumer tests, plus relevant NixOS integration checks.

No Lean or other proof project exists in the current repository. Executable
protocol tests remain required; a formal model must match the chosen protocol
before it can support a completion claim.

## Remaining Integration

| Workstream | Required Deliverables | Status |
| --- | --- | --- |
| Cooperative core | Singleton snapshots, canonical rank, scoped authentication, resync gate, active-only authorization, bounded churn tests. | Implemented; 30 core tests pass. |
| Durable state | Atomic snapshot/capability persistence, crash boundaries, migration, retired metadata cleanup, bounded disk retention. | Version-3 store and durable churn tests implemented; runtime migration/activation and full crash campaign remain. |
| Runtime and wire | Capability/version negotiation, bounded sync window, branch selection, pairing handoff, stale-record rejection, route/DNS/discovery cleanup. | Forwarder projection API implemented; operational activation, wire, and remaining cleanup are pending. |
| User surfaces | Linux/Android sync state, visible discarded changes, no retained inviter history, structured user/developer instructions. | Active-only inventories and missing-provenance rendering tested; sync/loss status and runtime integration remain. |
| End-to-end proof | Multi-node forks/offline return, restart, churn measurements, CLI/Android/NixOS contracts, practical formal invariants. | Not established. |
