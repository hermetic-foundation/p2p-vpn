# Pairing and Session Lifecycle Review

## Status

Active; baseline `cbe400b1`. This bounded workstream follows the completed
[recovery ownership review](recovery-event-ownership-review.md).
The checklist below is review scope, not a claim of verified completion.

## Existing Evidence

| Area | Retained Result | Remaining Boundary |
| --- | --- | --- |
| [Durable cancellation](pairing-cancellation-plan.md) | Prepared cancel/reject/replace persists abort intent; cleanup retries and survivor preservation covered | Audit orchestration around those owners; do not redesign the chosen policy |
| [Acceptance transactions](pairing-transaction-review.md) | Matching Submit/Poll retry release, partial-route retry, restart repair and historical acknowledgement covered | Trace current request dispatch, replacement and finalization together |
| [Membership sync](membership-sync-review.md) | Wrong-type/stale response cleanup, authority withdrawal and bounded history corrected | Verify integration boundaries; do not reopen completed cases without new evidence |
| Discovery ownership | Query capacity/retry and inactive join lookup have session/driver regressions | Check pairing terminal transitions against existing query owners |
| Legacy snapshots | Inconsistent historical Prepared snapshots fail closed; limitation documented | Preserve validation; no destructive automatic repair |
| Platform evidence | Existing namespace, VM and Android reports retain revision-specific scope | Run affected local checks; final platform acceptance remains separate |

Historical reports contain intermediate open findings later closed by their final
sections. Their earlier failures remain evidence, not automatically new defects.
No new runtime validation has been performed for this workstream yet.

## Policy Boundary

| State | Required Behavior |
| --- | --- |
| Uncommitted pairing | Cancellation discards pending enrollment |
| Prepared with possible runtime effects | Persist abort ownership, clean up safely, then compact |
| Cleanup failure | Retain durable ownership and retry without granting authority |
| Completed membership | Cancellation does not silently revoke; existing membership policy applies |
| Replacement operation | Older completion, cleanup or retry cannot overwrite its ownership |

The cancellation decision is already resolved. The audit must identify the exact
runtime completion and durable checkpoint boundaries without inventing a new
security or membership policy.

## Ownership Map

| Owner / Source | Transitions to Trace |
| --- | --- |
| [CodePairingSessions](../../src/runtime/pairing_sessions.rs) | Operation IDs, Hello/Submit/Poll request map, inbound sessions, approval tickets and retry flags |
| Same session owner | Prepared/Applied/Aborting ledger, receipts, replay tokens, expiry and replacement |
| [Runtime adapter](../../src/runtime/runner.rs) | RPC mutations, both code protocol dispatchers, file pairing, provider/query retirement and shutdown |
| Same runtime adapter | Runtime enrollment preparation, route application, completion, checkpoint and abort cleanup |
| [PairingStateStore](../../src/runtime/pairing_store.rs) | Lock, atomic save, load validation, error handling and restart ownership |
| [Code protocol codecs](../../src/runtime/pairing_code.rs) | Terminal transport events and bounded messages for both protocol versions |
| [File pairing codec](../../src/runtime/pairing.rs) | Request completion, rejection and disconnect behavior |
| [CLI tests](../../tests/pair_cli.rs) | Cancellation outcomes, operation resumption, artifact export and acknowledgement |

## Bounded Checklist

- [x] Identify existing completed fixes and explicit historical limitations.
- [x] Record ownership entry points and the chosen cancellation policy.
- [ ] Trace invitation/code creation, authentication and admission on both roles.
- [ ] Trace Hello/Submit/Poll success, rejection, wrong-type reply and transport failure.
- [ ] Verify stale IDs, duplicate replies, cancellation and replacement isolation.
- [ ] Trace expiry, retry deadlines, disconnect, provider removal and query retirement.
- [ ] Trace Prepared, runtime commit, completion, Applied checkpoint and acknowledgement.
- [ ] Trace shutdown/reload, persistence failure and idempotent cleanup/recovery.
- [ ] Reconcile membership-sync integration and file-pairing session boundaries.
- [ ] Reproduce confirmed gaps, fix narrowly and verify recovery after failure.
- [ ] Run affected integration and shared-runtime verification gates.
- [ ] Publish final evidence, update remaining work and verify all commits on `main`.

## Verification Plan

| Layer | Required Evidence |
| --- | --- |
| Focused regressions | Real owner/dispatcher where practical; failing-before and passing-after for defects |
| Recovery | Successful retry/replacement/reload, not merely ignoring stale input |
| Native workspace | Offline locked tests; formatting; required correctness/suspicious/performance Clippy groups |
| Integration | Applicable direct/relayed pairing, code exchange, cancellation/retry and restart cases |
| Packaging | Cached Nix source parity; full-package limitations stated separately |
| Shared runtime | Android-native compilation if changed; not APK/device acceptance |
| Formal verification | Inspect applicable existing models; executable tests are not proof |

## Resource and Scope Limits

- Keep all `/tmp/p2p-vpn-*` task storage below 10 GiB; preserve raw evidence.
- Check headroom before builds; reuse cached targets and at most two Cargo jobs.
- Cap downloads at 10 Mbps; no uncontrolled source builds or timing-test builds.
- Use bounded logs/watchdogs; preserve original deadlines and authorization checks.
- No physical deployments, personal-flake edits or public-WAN campaign.
- Android service lifecycle, heap attribution and final platform acceptance remain separate.

## Findings

### PS-1: Retired-Connection Reply Ownership

| Item | Initial Source Evidence |
| --- | --- |
| Filter | `handle_pairing_code_event` returns on an unusable message connection before response dispatch |
| Pending owner | `handle_pairing_code_response` normally removes the request from `outbound_requests` |
| Retry state | `release_outbound_code_request` releases Hello/Submit/Poll in-flight flags |
| Hypothesis | A terminal reply filtered before dispatch can retain application ownership and prevent retry |
| Next evidence | Reproduce with a completed transport reply, then verify matching cleanup, retry and replacement isolation |

This is a candidate, not a reproduced defect. Inspect upstream terminal-event
semantics and exercise the actual dispatcher before making a correction.
The completed membership-sync fix supplies an existing test pattern, not proof
that code pairing has the same observable failure.
