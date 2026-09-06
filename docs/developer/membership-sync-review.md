# Membership Sync Lifecycle Review

## Scope

Source review at `f026342f`, covering outbound membership paging and its owner
maps in `src/runtime/runner.rs`. A read-only review agent identified these cases;
parent inspection confirmed the dispatch order and map retention behavior.

Wrong-type response dispatch, authorization withdrawal, and history growth are
reproduced and fixed. Stale-connection ordering remains open acceptance work,
not an approved deferral or a packet-admission bypass claim.

## Resolved: Wrong-Type Response Ownership

| Item | Evidence |
| --- | --- |
| Reproduction | `membership_record_sync_wrong_response_releases_owner_and_preserves_retry` failed at the pending-owner assertion before the fix. |
| Dispatch | Membership request ownership now takes precedence over the stale packet-plane response guard. |
| Cleanup | The existing `unexpected_response_type` failure path releases both owner indexes and records one sync failure. |
| Retry | The regression checks rejection immediately before the retry deadline and admission at the deadline. |
| Late reply | An old request ID cannot consume a newer sync or change its retry deadline/failure count. |
| Compatibility | No wire, configuration, or public API changes; unknown packet-plane responses retain their existing stale handling. |

The test injects a response into `handle_control_event` with a usable connection
epoch and a real outbound request ID. It needs no TUN privileges or public peers;
it does not claim live transport event-order coverage.

Pinned `libp2p-request-response` 0.29.0 removes its pending outbound response before
emitting the application event. Application cleanup therefore cannot rely on a
later transport timeout after consuming this reply.

### Validation

| Check | Result |
| --- | --- |
| Focused membership-sync tests | Four passed, including the new failing-before/passing-after regression. |
| Workspace | 1,196 passed; 18 opt-in tests ignored. |
| Namespace code pairing | Passed in 13.32 seconds. |
| Static checks | Required Clippy groups, changed-file rustfmt, and whitespace checks pass. |
| Nix source parity | `rust-test-sources` built offline using cached dependencies. |

Logs use `/tmp/p2p-vpn-review-sync-dispatch-*`. No emulator or public-network test
was run for this patch; final affected platform gates remain in the acceptance map.

## Resolved: Authorization Withdrawal

| Boundary | Behavior |
| --- | --- |
| Committed policy | Sync ownership uses `Forwarder::authorizes_membership_sync`; it does not reevaluate wall-clock membership independently. |
| Runtime reconciliation | Both membership and packet-authorization revisions trigger retirement of unauthorized pending requests. New requests invalidate the cached check. |
| Reply handling | Authority is checked before processing any page or snapshot-restart response, including before final merge. |
| Local expiry | An already pending sync with an active signed remote retains its recovery authority. Initial outgoing admission is unchanged. |
| Remote withdrawal | Revocation, remote expiry, and removal of a static peer retire both pending-owner indexes. |
| Diagnostics | Retirement records `membership_record_sync_failed` with reason `authorization_withdrawn`, using existing failure/backoff accounting. |

The runtime releases application ownership before awaiting another event.
It does not cancel the underlying libp2p request; a later response cannot resume
the retired sync. Historical retry and completion bounds are covered below.

### Regression Evidence

- Before the fix, a revoked remote's first page requested another page.
- First-page, final-page, and snapshot-restart replies now stop without continuation or merge.
- Local expiry preserves an active remote; later remote expiry retires it even when packet authorization is already empty.
- Static-peer removal retires a sync when only the packet-authorization revision changes.
- Repeated reconciliation and late replies do not count the same sync retirement twice.

Seven focused sync tests pass. The successful paging and tampering fixtures now
include the remote's initial membership grant, preserving coverage beyond the
new authority guard.

| Check | Result |
| --- | --- |
| Workspace | 1,199 passed; 18 opt-in tests ignored. |
| Namespace integration | All 11 passed in 188.76 seconds, including network movement and relay promotion. |
| Static checks | Required Clippy groups, changed-file rustfmt, and whitespace checks pass. |
| Nix source parity | `rust-test-sources` built offline using cached dependencies. |

Logs use `/tmp/p2p-vpn-review-sync-authority-*`. Final Android and VM gates remain
in the acceptance map; this patch has no protocol or configuration changes.

## Resolved: Bounded Sync History

`MembershipSyncHistory` owns completion caching and retry retention in
`src/runtime/membership_sync_history.rs`. Pending request ownership remains in
`MembershipRecordSyncs`; no protocol or configuration fields changed.

| State | Bound and Policy |
| --- | --- |
| Completed snapshots | 1,024 peers; evict the least recently completed/refreshed entry. Refreshing an existing peer does not evict another. |
| Per-peer retry deadlines | 1,024 peers; reclaim expired entries before evicting a live deadline. |
| Overflow deadline | One deadline per network instance covers evicted live retries. New sync admission waits for it; existing packet paths are unaffected. |
| Reconnect | Disconnect cleanup preserves history, so reconnecting does not erase backoff. |
| Authorization | Policy reconciliation removes unauthorized completion/retry entries. The shared deadline remains until expiry because it may cover an authorized evicted peer. |
| Diagnostics | `membership_sync_history_pressure` reports `retry_after_ms` when a live retry entry must be evicted. |

The shared deadline preserves the maximum deadline among evicted retries. Each
eviction can defer new syncs for at most the existing 30-second retry interval;
retained peers can still have later individual deadlines.

### Evidence

- Before the fix, both disconnect-churn tests retained 1,025 entries and failed the proposed bound.
- Six history-owner tests cover refresh, overflow, exact retry boundaries, expired-entry reclamation, and authorization pruning.
- Runtime tests verify disconnect retention and history cleanup after expiry or static-peer removal.
- The workspace passes 1,207 tests, with 18 opt-in tests ignored.
- Namespace code pairing passes in 13.21 seconds; required Clippy groups, formatting, and Nix source parity also pass.

These tests establish entry bounds and retry behavior, not daemon RSS or live
overload performance. Logs use `/tmp/p2p-vpn-review-sync-history-*`.

## Open Review Gap

| Priority | Finding | Source |
| --- | --- | --- |
| Unreproduced | Stale-connection filtering precedes membership request retirement. Validate terminal response ordering and owner cleanup. | `handle_control_event`, `request_response_message_is_usable` |

### Regression Cases

1. **Passed:** deliver a packet-plane rejection using an outstanding membership
   request ID; verify owner release, retry boundaries, and late-reply isolation.
2. **Passed:** withdraw remote authority during paging; verify no continuation,
   restart, or merge, plus retirement without a reply and local-recovery preservation.
3. **Passed:** cycle peer identities through failure/completion and disconnection;
   verify entry bounds and preserve backoff for retained and evicted peers.
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
