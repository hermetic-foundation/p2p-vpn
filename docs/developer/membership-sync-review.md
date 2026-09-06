# Membership Sync Lifecycle Review

## Scope

Source review at `f026342f`, covering outbound membership paging and its owner
maps in `src/runtime/runner.rs`. A read-only review agent identified these cases;
parent inspection confirmed the dispatch order and map retention behavior.

The proposed reproductions below have not yet been executed. These items remain
open acceptance work, not approved deferrals or packet-admission bypass claims.

## Open Findings

| Priority | Finding | Source |
| --- | --- | --- |
| P2 | Packet-plane response variants can return before membership request ownership is checked. | `handle_control_event` |
| P2 | Authorization-change cleanup omits pending membership syncs; page continuation does not recheck current authority. | Runtime authorization-revision reconciliation; membership-page response handling |
| P2 | Completed snapshots and retry deadlines lack an explicit retention bound; disconnect cleanup retains both maps. | `MembershipRecordSyncs::mark_completed`, `mark_failed`, `remove_peer` |

### Regression Cases

1. Deliver a packet-plane rejection using an outstanding membership request ID.
   Verify the membership owner is released and bounded retry remains possible.
2. Withdraw remote authority during a multi-page sync, then deliver its old page.
   Verify no continuation or merge occurs and pending ownership is retired.
3. Cycle distinct peer identities through failure/completion and disconnection.
   Verify explicit retention bounds while preserving intended reconnect backoff.
4. Deliver a queued response from a retiring connection while another survives.
   Verify stale-event filtering does not strand the request owner.

Case four also needs event-order validation: the early-return path is visible,
but its occurrence with real libp2p event ordering has not been reproduced.
Map growth is an ownership concern; process-memory exhaustion was not measured.

## Existing Safeguards

- Request IDs identify pending syncs; matching peer-index removal does not consume a newer request.
- Last-connection closure and normal outbound failure release pending ownership.
- Normal admission permits four concurrent syncs and one sync per peer.
- Page validation bounds records/bytes and checks cursors, snapshot identity, totals, and final digest.
- Partial pages do not merge; existing tests cover tampering, restarts, and retry delay.

## Implementation Plan

| Step | Constraint |
| --- | --- |
| Reproduce owner leaks | Exercise real response dispatch, not only isolated map methods. |
| Dispatch by owner before response type | A malformed reply must fail its own request, not become an unrelated protocol response. |
| Reconcile owners against sync policy | Preserve the inactive-local exception and reject withdrawn remote members. |
| Bound historical bookkeeping | Preserve reconnect backoff deliberately; do not silently erase it on every disconnect. |
| Verify late completions | Old responses must not consume or mutate newer request state. |

Sync authorization differs from packet authorization when local membership is
inactive. Retirement must account for committed membership changes even when
packet authorization is already empty and its revision does not advance.
