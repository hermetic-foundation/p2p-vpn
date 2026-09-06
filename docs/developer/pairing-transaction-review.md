# Pairing Transaction Review

## Scope

Review date: 2026-09-06. This tracks pairing lifecycle and persistence findings
within the [reliability review](refactor-review.md).

## Findings

| Priority | Case | Evidence and status |
| --- | --- | --- |
| P1 | Cancel after completion produces unrestorable state | Reproduced for inviter and joiner; cancellation now preserves completed state. |
| P1 | Prepared enrollment loses recovery dependencies | Secondary source review; failure-injection reproduction and fix remain pending. |
| P2 | Accepted Submit failure leaves submission in flight | Reproduced at response dispatch; release the matching request before applying acceptance. |
| P2 | New operation prevents acknowledgement of older enrollment | Secondary source review; reproduce across restart and declarative configuration changes. |

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
2. Extend retry-eligibility coverage to successful continuation after repair,
   including partial route application and rollback errors.
3. Separate durable receipt ownership from the replaceable operation slot where
   reproduction confirms older enrollments cannot be acknowledged.
4. Reconcile old invalid snapshots without silently dropping committed membership
   or weakening restored-state authorization checks.
5. Run affected native, namespace, and Android gates after transaction fixes.

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

The runner regression `accepted_pairing_retries_after_local_application_failure`
dispatches signed acceptance directly, with public discovery disabled.
The initial Submit persistence case failed at `submit must retry` before correction.

The test matrix covers Submit and Poll with a rejected state-file destination or
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
