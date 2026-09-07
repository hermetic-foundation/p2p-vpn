# Durable Pairing Cancellation

## Approved Behavior

| State | Cancellation Behavior |
| --- | --- |
| Before preparation | Discard the pending operation normally |
| Prepared, not completed | Persist abort intent, undo partial changes, discard pending enrollment |
| Abort cleanup interrupted | Resume cleanup, never complete enrollment |
| Already established membership | Existing revocation policy applies; cancellation does not silently revoke |

This is the user's chosen policy for closeout Goal 1. It replaces the proposal
to prohibit cancellation after preparation. No additional policy approval is
needed to implement safe abort and rollback.

## Current Ownership

| Owner | Relevant Behavior |
| --- | --- |
| `CodePairingSessions::cancel` / `reject` | Clear operation dependencies but currently leave Prepared enrollment |
| `prepare_enrollment` | Records the signed response before runtime application |
| `commit_pairing_runtime_enrollment_with_route_update` | Applies routes before committing logical membership/configuration |
| `execute_tun_route_update` | Attempts rollback after partial failure; rollback itself can fail |
| `reconcile_persisted_pairing_enrollments_with_route_update` | Replays enrollment after restart; must distinguish abort from commit |
| Pairing RPC handlers | Must not report successful cancellation before abort intent is durable |

## Implementation Sequence

1. Add an explicit abort lifecycle and validated cleanup ownership to persisted pairing state.
2. Persist abort intent before destructive cleanup; persistence failure must not report success or permit accidental enrollment replay.
3. Reconcile affected routes and addresses against surviving authorized state, including unrelated or overlapping memberships.
4. Retain cleanup ownership on failure; make retry and restart idempotent and prevent late responses from reactivating cancellation.
5. Remove pending enrollment material after cleanup; retain only bounded replay protection needed to prevent stale completion.
6. Route cancellation, rejection, and cancellation-then-replacement through this lifecycle for inviter and joiner.
7. Update user-facing outcomes, compatibility notes, and the transaction evidence report after verification.

Do not persist executable shell commands as rollback data. Use validated,
structured ownership data. Do not authorize an aborted response temporarily
to make cleanup or startup reconciliation succeed.

## Acceptance Matrix

| Case | Required Evidence |
| --- | --- |
| Five existing mutation cases | Cancel/reject/replace survives reload without conflict or enrollment replay |
| Abort persistence failure | Clear failure outcome; no false acknowledgement or destructive loss of recovery data |
| Partial route application | Cancellation removes its additions and restores required prior state |
| Cleanup failure and retry | Abort intent survives; retry converges without authorizing the peer |
| Crash after abort persistence | Startup performs cleanup only |
| Crash after cleanup before compaction | Repeated cleanup is safe, including already-absent kernel entries |
| Late response | Cannot complete a cancelled operation or overwrite its replacement |
| Other enrollment/configuration | Existing authorization, routes, identities, and membership keys remain intact |
| Compatibility | Existing valid snapshots load; already-invalid legacy state has tested repair or an explicit documented limitation |
| Established membership | Existing completion and revocation behavior remain separate |

## Verification Gates

- Replace or complement the five broken-behavior diagnostics with passing contracts.
- Run focused tests, the full native workspace, required Clippy groups, formatting, and Nix source inclusion.
- Run relevant existing pairing integration scenarios; exercise injected persistence and route failures at their actual owners.
- Preserve the completed [probe-ownership evidence](path-probe-ownership-review.md).
- Record exact artifacts and limits before marking Goal 1 complete.

## Scope

Implementation is pending. This plan does not claim rollback is already safe.
Kademlia limits, general lifecycle review, resource baselines, and final platform
acceptance remain in the [umbrella checklist](review-verification.md).
