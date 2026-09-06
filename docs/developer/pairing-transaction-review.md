# Pairing Transaction Review

## Scope

Review date: 2026-09-06. This tracks pairing lifecycle and persistence findings
within the [reliability review](refactor-review.md).

## Findings

| Priority | Case | Evidence and status |
| --- | --- | --- |
| P1 | Cancel after completion produces unrestorable state | Reproduced for inviter and joiner; cancellation now preserves completed state. |
| P1 | Prepared enrollment loses recovery dependencies | Live join expiry reproduced and fixed; cancellation, rejection, and replacement remain open. |
| P2 | Accepted Submit failure leaves submission in flight | Reproduced at response dispatch; release the matching request before applying acceptance. |
| P2 | New operation prevents acknowledgement of older enrollment | Replacement/restore regression reproduced; acknowledgement now uses Applied ledger state with matching-slot consistency checks. |

## Completed Cancellation

### Failure

1. Complete an inviter or joiner operation and mark its enrollment applied.
2. Cancel the operation; the old code adds a terminal status to its completion.
3. Persist and restore; validation rejects a completed-and-terminal operation.

Relevant ownership boundaries:

- `src/runtime/pairing_sessions.rs`: `cancel`, completion methods, and `validate_restored_operation`.
- `src/runtime/runner.rs`: `PairCancel` persists the mutated session before returning its status.

### Correction

- Completed cancellation succeeds without changing session state or provider actions.
- Completion artifacts and the inviter's accepted polling receipt remain available.
- Active cancellation and unknown-operation errors retain their existing behavior.
- Restore validation stays strict; storage and wire formats are unchanged.

### Regression Evidence

Both `completed_*_ignores_late_cancel_and_remains_restorable` tests failed before
the correction with `operation cannot be both completed and terminal`.
They check exact persisted-state preservation and repeated cancellation across restore.

The corrected code passes all 48 session tests and the full native workspace
(1,210 passed, 18 opt-in tests ignored). No Lean model exists for this state machine;
these are executable regressions, not formal proof.

| Additional gate | Result | Scope |
| --- | --- | --- |
| Code-pairing namespace | Passed, 13.29 seconds | Peerless-overlay workflow, not injected persistence failures |
| Clippy | Required correctness, suspicious, and performance groups pass | Existing non-fatal style warnings remain |
| Formatting | Changed Rust and whitespace checks pass | No unrelated formatting changes |
| Nix `rust-test-sources` | Built offline | Test-source inclusion, not a full package build |

Android runtime verification was not rerun for this change. The prior emulator
evidence and broader outstanding platform gates remain listed in
[verification coverage](review-verification.md).

This prevents new invalid snapshots. It does not automatically repair snapshots
already corrupted by the old transition. Recovery compatibility remains an open review item.

## Remaining Plan

1. Inject failure after Prepared persistence and before runtime commit; exercise
   cancellation, rejection, expiry, and same-role replacement before recovery.
2. Extend historical acknowledgement coverage through daemon startup compaction
   under incompatible declarative authority and multiple sequential pairings.
3. Reconcile old invalid snapshots without silently dropping committed membership
   or weakening restored-state authorization checks.
4. Run affected native, namespace, and Android gates after transaction fixes.

Accepted-response continuation now has the [live retry coverage](#live-retry-through-completion)
below. Lower-level partial rollback and persisted restart remain separate tests.

Secondary-review observations are not yet equivalent to reproduced failures.
No production state has been altered to investigate these cases.

## Acceptance Retry Ownership

### Failure and Correction

- `handle_pairing_code_response` handles accepted Submit and Poll responses together.
- Both local failure branches previously released only the Poll in-flight flag.
- Submit then remained in flight indefinitely, despite its transport request having completed.
- Accepted responses now release the matching request before local application.

The existing release helper preserves the retry delay and operation/peer checks.
Invalid acceptance still fails the operation; successful application clears its pending state.
This does not add retries to terminally rejected pairings or change the wire protocol.

### Evidence Boundary

The original runner regression `accepted_pairing_retries_after_local_application_failure`
dispatched signed acceptance directly, with public discovery disabled.
The initial Submit persistence case failed at `submit must retry` before correction.

The original test matrix covers Submit and Poll with a rejected state-file destination or
an injected route-controller error. It checks retry eligibility, no immediate retry,
and unchanged peer configuration. It does not simulate partial route rollback,
power loss, or delivery of the subsequent retry over a physical network.

Verification after the retry fix:

| Gate | Result |
| --- | --- |
| Native workspace | 1,211 passed, 18 opt-in tests ignored |
| Four-case regression | Passed, 1.16 seconds |
| Code-pairing namespace | Passed, 13.32 seconds |
| Clippy | Required correctness, suspicious, and performance groups pass |
| Formatting | Changed Rust and whitespace checks pass |

Android runtime gates remain outstanding for the combined transaction changes.

### Partial Route Retry

`pairing_commit_retries_after_partial_route_and_rollback_failure` exercises the
runtime transaction after one route command succeeds and the second fails.
It tests both successful rollback and an injected failure of that rollback command.

| Boundary | Assertion |
| --- | --- |
| Failed application | Only the successful step is rolled back; the original application error is retained |
| Before retry | Forwarder config, membership, TUN snapshot, and advertised capabilities remain unchanged |
| Repaired command executor | Fresh preparation replays every apply command, including the step whose rollback failed |
| Successful commit | Expected config and TUN state are installed; the new peer is authorized |

Both cases pass without a production-code change. The command executor is
injected: this checks transaction ordering and logical publication, not actual
kernel state, crash recovery, durable finalization, or automatic retry delivery.

The accepted-response test now drives live loopback retries through completion,
as described below. Persisted restart reconciliation remains separate coverage.
Log: `/tmp/p2p-vpn-review-pairing-partial-routes.log`.

The full workspace passed with 1,226 tests and 18 opt-in tests ignored.
No runtime implementation changed, so device and namespace deployment tests
were not repeated for this coverage-only addition.

### Live Retry Through Completion

The accepted-response regression now runs two real TCP libp2p swarms. The remote
serves a signed fixture acceptance; the joiner uses `drive_code_pairing_discovery`
and dispatches received responses through `handle_pairing_code_response`.

| Case | Failure | Recovery Assertion |
| --- | --- | --- |
| Submit and Poll | Directory occupies the state-file path | Remove obstruction; normal retry persists and completes enrollment |
| Submit and Poll | Route controller rejects reconciliation | Repair controller; normal retry applies and persists enrollment |

- Every attempt crosses loopback; the remote verifies the exact expected request and sender identity.
- Retry ownership is created by the production driver, not inserted directly by the test.
- Failed application leaves the peer unauthorized and completion absent; immediate retry remains suppressed.
- After repair, a new request ID delivers acceptance without manually advancing time or clearing in-flight state.
- Successful recovery authorizes the peer, persists Applied state, and retains completion after reload.
- Completed operations have no pending submission or poll even at a later retry deadline.

The four-case test passed in 5.51 seconds. Public discovery is disabled, and the
route controller is injected. Connection promotion, actual kernel changes, full
inviter approval, PAKE negotiation, and physical WAN retries are not covered here.

Log: `/tmp/p2p-vpn-review-pairing-live-retry.log`.

Removing the accepted-response ownership release as a temporary negative control
made the first Submit retry time out after 10 seconds. The production block was
restored unchanged; the final Rust diff only extends tests.

Negative-control log: `/tmp/p2p-vpn-review-pairing-live-retry-mutation.log`.

| Gate After Restoring Production Code | Result |
| --- | --- |
| Full native workspace | 1,226 passed, 18 opt-in tests ignored; includes the live retry matrix |
| Code-pairing namespace | Passed, 13.28 seconds |
| Clippy | Required correctness, suspicious, and performance groups pass; non-fatal style warnings remain |
| Formatting and whitespace | Changed Rust and diff checks pass |
| Nix `rust-test-sources` | Built offline; verifies test inclusion, not a complete package build |

Logs use `/tmp/p2p-vpn-review-pairing-live-*`. No production implementation changed,
so Android and full NixOS VM gates were not repeated for this test-only extension.
This remains executable coverage, not a formal proof of the pairing state machine.

### Persisted Restart Repair

The existing inviter and joiner expired-restart tests now inject failure of the
second route command and its rollback during startup reconciliation. Both roles
retain the original application error and leave their logical state unchanged.

| Stage | Evidence |
| --- | --- |
| Failed restart | Config, membership, TUN snapshot, session encoding, and saved bytes remain unchanged |
| Reload after repair | A new session owner decodes the retained Prepared state from the real store |
| Successful reconciliation | Enrollment becomes Applied and the peer is authorized |
| Repeated reconciliation | No additional route commands are issued |
| Durable completion | Loading the saved state again retains Applied enrollment; joiner status remains Completed |

Both role tests pass without changing production code. The route executor is
injected, so residual kernel changes and power loss are not modeled. This is
startup-function coverage, not a rebooted daemon or physical-network retry.

Log: `/tmp/p2p-vpn-review-pairing-restart-repair.log`.
Cancellation/replacement policy and recovery of previously invalid snapshots
remain separate, unresolved work.

## Live Join Expiry

### Failure and Correction

1. Prepare a joiner enrollment but leave runtime application incomplete.
2. Expire the operation in the running daemon, then checkpoint its state.
3. Restore and validate recovery; the old expiry path has deleted the remote approval.

`deactivate_join` now retains recovery dependencies on expiry only when a matching
Prepared joiner enrollment exists. This matches the existing restore policy.
Cancellation and other terminal transitions are not changed by this fix.

### Evidence Boundary

- `prepared_join_survives_live_expiry_checkpoint` reproduced failed recovery validation before the correction.
- The test covers live expiry, serialization, restore, validation, and session finalization.
- The expired operation loses its code and cannot issue polls or submissions.
- An unprepared expiry test checks that remote approval state is still discarded.

These are session-state tests, not injected daemon route failure followed by restart.
Replacement or cancellation can still invalidate Prepared recovery; that broader finding
remains open, as does migration of already-invalid saved state.

| Gate after live-expiry fix | Result |
| --- | --- |
| Pairing sessions | 50 passed |
| Native workspace | 1,213 passed, 18 opt-in tests ignored |
| Code-pairing namespace | Passed, 13.36 seconds |
| Clippy | Required correctness, suspicious, and performance groups pass |
| Formatting | Changed Rust and whitespace checks pass |
| Nix `rust-test-sources` | Built offline; source inclusion only |

Android runtime and full NixOS VM gates were not rerun for this incremental fix.

## Historical Acknowledgement

### Failure and Correction

1. Complete enrollment A and mark it Applied without acknowledging it.
2. Start pairing B in the same role; it replaces the current-operation slot.
3. Restore and acknowledge A; the old completion lookup returns `Conflict`.

Applied ledger state and the supplied transcript identify the acknowledgement.
A matching current slot must still agree with the completion; a different or
absent slot no longer prevents compaction of the historical enrollment.

The same session-owned readiness check gates artifact export and completed RPC
status. Historical status takes its expiry from the enrollment response when the
original operation is absent. RPC fields and native Nix artifact formats are unchanged.

### Coverage

| Case | Assertion |
| --- | --- |
| Inviter and joiner replacement | Old acknowledgement succeeds after restore; replacement code remains intact |
| Wrong transcript | Rejected before compaction |
| Repeated acknowledgement | Same receipt after another restore |
| Replay protection | Old rendezvous token remains retained while valid |
| No current slot | Historical Applied entry can be acknowledged after restore |
| Incomplete or contradictory matching slot | Prepared, missing completion, changed response, and changed offer are rejected |
| Native artifacts after replacement/restore | Inviter and joiner exports retain grants, address assignment, and membership-key handling |

The initial replacement test failed with `Conflict` before correction.
These are session/persistence regressions, not a physical multi-pairing deployment.
The no-slot case constructs that historical state directly.

The inviter artifact test separately reproduced `InvalidState` before readiness
was centralized. Artifact assertions also check converged records and exclusion
of private-key and membership-key material from serialized inviter output.

| Gate after historical-readiness fix | Result |
| --- | --- |
| Native workspace | 1,216 passed, 18 opt-in tests ignored |
| Pairing RPC tests | 16 passed |
| Code-pairing namespace | Passed, 13.22 seconds |
| Clippy | Required correctness, suspicious, and performance groups pass |
| Formatting | Changed Rust and whitespace checks pass |
| Nix `rust-test-sources` | Built offline; source inclusion only |

Android runtime and full NixOS VM checks remain outstanding for the combined changes.

## Pending Cancellation Decision

The proposed policy is to finish recovery after durable preparation, rejecting
cancellation that would erase an unresolved commit. Removal afterward uses revocation.
User confirmation is pending; this policy has not been implemented.

Deleting Prepared state alone is unsafe: runtime routes or membership may already
have changed. A cancellable transaction would instead need durable abort state and
verified rollback, including crash recovery.

## Platform Follow-Up

The [Android network workflow](android-network-workflow-review.md) passed at
`6dbb680c`: join by code, persisted profile, switches, peer display, and dual-stack
traffic. It does not inject the transaction failures reviewed here.

Earlier per-fix notes above record which gates were unrun at those milestones.
Multi-network lifecycle, targeted failure recovery, and full VM follow-up remain open.

All 11 Linux namespace tests also passed at `6dbb680c` in 213.88 seconds,
including pairing, discovery, QUIC packets, relay fallback, network move, and
relay-to-direct promotion. Log: `/tmp/p2p-vpn-review-history-all-namespace.log`.
