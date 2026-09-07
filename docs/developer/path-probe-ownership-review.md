# Path Probe Ownership Review

## Finding

`PathProbeTracker::confirm` removed a token before checking its peer owner.
A wrong-peer acknowledgement carrying another outstanding token therefore
prevented that probe from receiving its legitimate acknowledgement or timeout.

| Property | Behavior |
| --- | --- |
| Token generation | Random nonzero 64-bit token from `OsRng` |
| Before correction | Remove token, then compare peer |
| After correction | Compare stored peer, then remove the matching token |
| Expiry | Existing RTT retention and timeout behavior remain unchanged |
| Compatibility | No wire, configuration, persistence, or CLI change |

The defect concerns mutation ownership. The regression injects a known token;
it does not demonstrate token prediction, unauthorized packet admission, or
successful exploitation across the encrypted transport.

## Regression

Source: `src/runtime/runner.rs`,
`path_probe_wrong_peer_cannot_consume_another_peers_probe`.

| Case | Required Result |
| --- | --- |
| Wrong peer, repeated twice | Reject without consuming either pending probe |
| Legitimate owner | Confirm original path and measured RTT |
| Duplicate acknowledgement | No second confirmation |
| Independent probe | Remains pending and expires through the normal timeout |
| Backends | UDP and QUIC datagram path kinds |

The pre-fix regression failed because the original owner's probe was missing
after the first wrong-peer acknowledgement. Retained negative control:
`/tmp/p2p-vpn-review-probe-owner-before.log`.

## Verification

Probe-only correction: `8e5950fa`. The table records its initial checks; the
[Goal 1 closeout](pairing-cancellation-plan.md#final-verification) reruns the owner
regression and datagram integration at `8ed95627`.

| Check | Result |
| --- | --- |
| Full native workspace | 1,235 passed; 23 opt-in tests ignored |
| Probe regressions | Wrong-peer ownership, normal acknowledgement/MTU, and timeout-to-relay tests passed |
| QUIC datagram namespace | Passed in 16.03 seconds |
| Direct two-node UDP namespace | Passed in 15.03 seconds |
| Static/source checks | Required Clippy groups, changed-file rustfmt, whitespace, and offline Nix test-source inclusion passed |

- Logs: `/tmp/p2p-vpn-goal1-probe-workspace.log`, `/tmp/p2p-vpn-goal1-probe-quic.log`, and `/tmp/p2p-vpn-goal1-probe-udp.log`.
- Clippy retains existing non-fatal style warnings.
- Namespace checks exercise normal packet/probe traffic; wrong-owner responses are injected at the tracker boundary.
- No Android deployment, full NixOS package rebuild, or formal proof was performed for this fix.

## Review Boundary

- This is the probe-ownership portion of closeout Goal 1, not completion of the broader review.
- Prepared-pairing mutations are closed by the [durable cancellation implementation](pairing-cancellation-plan.md).
- The umbrella acceptance checklist remains [Review Verification Coverage](review-verification.md).
