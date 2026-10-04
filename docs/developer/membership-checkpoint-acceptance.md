# Membership Checkpoint Acceptance

## Status

Implementation in progress. This is an acceptance map, not a completion report.

The peer-inventory cleanup is independent of durable checkpoint compaction.
Neither hidden rows nor a fixed history limit proves that compaction is complete.

Core, storage, wire transfer, version-3 restoration, and protected enrollment
staging are implemented; the fixed-quorum prototype is gone. Ordinary inviter
approval is wired to the checkpoint-aware pairing RPC for existing checkpoint state.

Ordinary fresh-solo `PairOpen` formation and joiner `Accepted` staging, resync,
cancellation, and finalization are wired. Legacy migration and export/artifacts
remain incomplete. Deploying the package does not convert existing authority;
the inspected Linux networks still use version-2 ledger state.

## Current Integration

| Surface | Implemented Path | Boundary / Remaining Work |
| --- | --- | --- |
| Inviter `PairApprove` | Validate the signed request, prepare a signed grant, persist the protected transaction, then commit authority. | Requires an already-participating checkpoint instance. |
| Approval retry | Compare the prepared grant with current authority; reject superseded admissions. | No stale grant may restore a removed member. |
| Active-member re-pair | Reuse unchanged admission without advancing the authority revision. | Changed active grants are rejected; require a separate grant-update workflow. |
| Cancellation | Protected admission ownership distinguishes new admission from an existing member. | Confirm durable removal before clearing ownership or discarding the transaction. |
| Mutation replies | Bound control-only reply ownership by request, peer, connection, and deadline. | No packet grants or permanent departure archive. |
| Identify | Preserve checkpoint-only catch-up and pending reply connections. | Do not classify them as routing infrastructure or overlay members. |
| Joiner `Accepted` | Verify the transcript, persist transaction ownership, stage a gated authority, fetch current state, reconcile TUN, then finalize durably. | Cold reconnect and complete CLI/Android export workflows remain. |
| Fresh formation | Explicit `PairOpen` validates fresh solo history, generates or preserves protected credentials, and persists version-3 authority gated for resync. | Existing legacy networks are not automatically migrated. |
| Legacy migration | Signed active-only seeds preserve grants, selected policy, and current labels; recipients restore gated and sign their own names. | Daemon/CLI conversion, protected handoff, static-grant coverage, and atomic installation remain. |
| Export / artifacts | Generic config import rejects checkpoint credentials; checkpoint RPC artifacts return `Unavailable`. | No unsafe legacy Nix plan; complete protected export remains. |

The cancellation guard retains ownership during staged enrollment or gated
resync. RPC and TCP/Noise regressions pass; this is not deployed-network evidence.

## Current Evidence

| Surface | Verified Result | Remaining Gap |
| --- | --- | --- |
| Peer inventory | `25243a3c` omits revoked rows from shared Linux/Android snapshots, including stale static metadata. | Existing legacy networks still retain enforcement history. |
| Runtime paths | `df78cfb3` erases unauthorized stream/datagram/relay path history and pending probes. | Other retained device state must follow checkpoint installation. |
| Address retirement | Eight API tests cover protected-only deletion, canonical deduplication, and 1,024 cycles. Six wired-runner tests cover both DHTs, cached values/providers, relearning, control-owner expiry, and 512 cycles. | Full migration/artifact cleanup and deployed evidence remain. |
| Forwarding projection | Seal static fallback; restore signed local aliases after restart; strip ungranted routes/aliases; preserve signed metrics and route policy. | Full discovery/cache/artifact cleanup and kernel integration remain. |
| Snapshot/mutation transfer | 21 snapshot and 19 mutation tests cover authenticated TCP/Noise, scope, replay, deadlines, correlated replies, and frame/session bounds. | Cold gated reconnect and deployed convergence remain. |
| Daemon coordinator | 37 tests cover restart, resync, solo APIs, removal, DNS, catch-up, handoff, enrollment gates, ACK/Identify ownership, and retained-state erasure. | Existing networks need explicit migration; departure intermittency remains. |
| Pairing enrollment | Signed grants, protected minimum-rank staging, restart, replay, and a 102-member TCP/Noise paged fetch pass. | Both RPC roles are wired; migration and export/artifacts remain. |
| Inviter pairing RPC | 11 tests cover signed grants, TCP/Noise delivery, retry, resync gates, cancellation ownership, and fail-closed artifacts. | Full CLI/Android export and deployed pairing remain. |
| Joiner pairing RPC | 12 tests cover ordinary fresh formation, TCP/Noise full fetch, minimum-rank gating, expired protected restart, Fresh/Repair cancellation, ambiguous final writes, and signed TUN alias retry. | Packet/kernel adapters are fixtures; cold reconnect, export, Android, and deployed evidence remain. |
| Creator departure | Three real daemon loops deliver removal to two survivors; later survivor governance and injected route-cleanup retry pass. | Packet/route adapters are fixtures, not real TUN or deployed-node evidence. |
| Handoff scheduling | Five tests verify recipient/in-flight bounds, fairness, fixed deadlines, retry limits, and forgotten payloads. | No deployed checkpoint-network completion claim. |
| Fresh solo APIs | Eight tests cover empty/self-only versions 1/2, signature/scope/key/history rejection, routes, pins, isolated enrollment gates, and write-boundary recovery. | Normal daemon pairing is wired; migration, export, and deployment remain. |
| Migration seed | Nine core regressions cover active-only transfer, descendant/grant/name preservation, shared scope, expiry, self-signing, gates, validation, and restart without staging proof. | No migration command, authority replacement, or multi-daemon deployment claim. |
| Rust workspace | 1,776 tests pass; 47 opt-in tests excluded. Core, storage, forwarding, enrollment, both pairing roles, migration seeds, and discovery cleanup are covered. | Full VM/package/device checks remain. |
| Durable checkpoint state | Version-3 authentication, legacy dispatch, rollback, failure/retry, and 128 durable churn cycles pass. | Automatic migration and a full crash campaign remain. |
| Static analysis | Formatting and required correctness/suspicious/performance Clippy groups pass. | Existing non-fatal lint warnings remain. |
| Android consumer | JVM unit tests and debug lint pass for revoked-local filtering and missing-local snapshots. | No new APK or physical-device checkpoint test. |
| Nix source coverage | Offline `rust-test-sources` check passes with the new modules. | Full package/VM rebuilds were not run for this slice. |
| Package preflight | Offline `default` package dry run succeeds. | Plans 1,377 uncached derivations; the build was not started to avoid the large storage cost. |
| NixOS VM preflight | The membership-convergence expression evaluates. | Offline dry run plans 2,859 uncached derivations; VM execution was not started. |

Core, store, and coordinator tests verify those individual layers. They do not
prove automatic migration, complete cleanup, or deployed multi-daemon convergence;
those require the integration and evidence below.

The daemon test also exposed and fixed zero-byte packet-reader EOF spinning.
Regression coverage verifies no empty packet is queued or counted after EOF.

The large-roster fixture allocates distinct derived IPv4 addresses after a full
run exposed a collision. Route-conflict rejection stays enabled; the fixture
proves paging, not automatic resolution of address collisions.

A live-refresh ordering regression is covered explicitly: an authenticated
exact-base departure can advance installed authority without losing an already
collected higher offer, extending the deadline, or bypassing startup gates.

### Open Departure Evidence

| Observation | Interpretation |
| --- | --- |
| Earlier full-suite run: no ACKs after all retries | Cause remains unproven; a passing isolated replay does not resolve it. |
| Latest focused run: zero recipients, attempts, ACKs, and failures | Departure occurred without captured connections; investigate transport readiness. |
| Formation/projection validation: one recipient and one ACK | Only one connection was captured; this does not establish an ACK-delivery failure or prove stale path counters. |
| Full-suite reruns pass | Useful evidence, not proof that the historical failures are resolved. |

The connected-only test must establish real transport readiness before expecting
two deliveries. Keep the two-ACK requirement; do not weaken it to accept an empty
recipient set or infer connectivity from membership participation alone.

The fixture now initiates each mesh edge once and checks live eligible-peer count
in addition to path readiness. It requires two captured recipients and two ACKs;
this test change does not claim to fix production connection churn or lost ACKs.

`connected_overlay_peers` in daemon status/state uses the same live swarm and
authorization filter as departure recipient capture. Sequential readiness samples
are not an atomic guarantee that a later command still has every connection.

The updated fixture passes the full workspace run and three additional isolated
runs. Packet and route devices remain fixtures; no production dial-churn or
deployed checkpoint-network claim follows from those passes.

### Cancellation Evidence

| Boundary | Current Evidence / Gap |
| --- | --- |
| Final pairing save fails after admission | Injected final-save failure and restart from protected `Prepared` pass; cancel removes only the owned admission. |
| Existing-member re-pair | Cancellation preserves prior authority; no-op re-pair does not invent a revision. |
| Removal rename succeeds but directory sync fails | Store failure injection is covered; direct cancellation injection at this boundary remains. |
| Startup / resync | Gated restoration and failed resync writes retain ownership until a successful durable resync. |
| Missing ownership metadata | Regression verifies fail-closed retention of the unresolved transaction. |
| Kernel cleanup fails | Selected packet authority remains installed; a complete abort/crash campaign remains. |
| Joiner final write has uncertain visibility | Preserve the activation barrier and reject cancellation until the exact protected state is read back and durably reconfirmed. |
| Joiner Fresh versus Repair | A provisional enrollment floor does not constitute prior established membership; duplicate acceptance preserves the original ownership. |
| Released joiner cleanup restarts | Keep packets and mutations gated after resync until pending kernel cleanup finishes. |

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

### Cleanup Ownership

A capacity limit is not identity erasure. Checkpoint cleanup must remove obsolete
device references from each owner, not just hide a peer-list row or replace the
membership file.

| Owner | Current Boundary | Remaining Check |
| --- | --- | --- |
| Selected snapshot and names | Retain the current roster and compatible claims, without inviter history or tombstones. | Exercise complete normal pairing, migration, restart, and churn. |
| Forwarding config, queues, paths, and sessions | Closed checkpoint namespace and authorization pruning are covered. | Combine these effects with one ordinary membership change. |
| Static dial references, address protection, and both DHTs | Wired cleanup removes absent overlay peers, owned address/legacy values, and scoped provider records. | Preserve returning roster members, public infrastructure, and bounded control-only owners; migration-era scopes remain part of migration review. |
| Recovery queries, dial backoff, and LAN-first state | Wired cleanup cancels owned queries and removes absent-peer backoff and LAN-first state; unrelated queries survive. | Extend fixture evidence to cold reconnect and deployed convergence. |
| Connection, request, and rate-limit owners | Existing disconnect, completion, and timeout cleanup applies. | Verify checkpoint removal and pending-reply expiry across all owners. |
| Pairing transcripts, receipts, and completion state | Counts are bounded, but receipt retention does not prove device erasure. | Compact obsolete identity-bearing artifacts while preserving pending transaction recovery. |
| Android profiles and presentation | Revoked rows are filtered; checkpoint enrollment and sync/loss UI remain incomplete. | Verify protected restart, profile cleanup, and discarded-change presentation. |

Temporary catch-up, reply, and departure ownership is not permanent revocation
history. Its deadline and scope must be bounded, and it must not authorize packets.

External Nix/Git configuration and system journals are separate owners. Runtime
compaction must not silently edit those files or delete unrelated system logs;
stale declarative peers must still remain unauthorized.

### Deliverables

| Workstream | Required Deliverables | Status |
| --- | --- | --- |
| Cooperative core | Singleton snapshots, canonical rank, scoped authentication, resync gate, active-only authorization, bounded churn tests. | Implemented; 30 core tests pass. |
| Durable state | Atomic persistence, crash boundaries, migration, metadata cleanup, bounded retention. | Store and churn tests pass; automatic migration and full crash campaign remain. |
| Runtime and wire | Version negotiation, sync, branch selection, pairing/departure, route/DNS/discovery cleanup. | Restoration, bounded departures, fresh formation, and both pairing roles are wired; cold reconnect, migration, export/artifacts, and full cleanup remain. |
| User surfaces | Linux/Android sync and loss status, minimal provenance, structured instructions. | Linux aggregate status and inventories tested; Android sync/loss UI and activation workflow remain. |
| End-to-end proof | Multi-daemon forks/offline return, restart, churn, CLI/Android/NixOS contracts. | Real paging and three-daemon fixtures pass; departure intermittency and deployed multi-node evidence remain. |
