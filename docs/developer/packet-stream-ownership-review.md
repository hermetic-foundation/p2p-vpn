# Packet Stream Ownership Review

## Scope

Source inspected at `be99de43` on 2026-09-06 by a read-only reviewer and parent
inspection. These findings do not establish the cause of the retained Android
[post-update packet loss](android-multi-network-review.md#latest-attempt).

## Findings

| Priority | Finding | Source |
| --- | --- | --- |
| P2 | Direct-TCP dispatch ignored selected connection; reproduced and corrected | Original: `src/runtime/runner.rs:9904`, `src/runtime/forward.rs:280` |
| P2 | Pinned-stream connection closure has no terminal request event | `src/runtime/pinned_packet_stream.rs:165`, `:234` |

### Direct TCP Selection

Previously, `send_dequeued_stream_fallback` pinned QUIC-stream and relay traffic,
but sent direct TCP through peer-level request-response dispatch. The selected
connection ID was omitted while metrics still used the selected path.

With multiple connections to one peer, dispatch can use a different connection
from the healthy direct candidate. A stale or relayed connection can therefore
carry a packet attributed to direct TCP.

Required regression: establish multiple connections, select one direct TCP
candidate, dispatch a queued packet, and verify the emitted handler notification
targets that exact connection. Include fallback after the selected connection fails.

#### Correction and Evidence

Direct-TCP health probes and queued packets now use the existing pinned-stream
dispatcher, just like QUIC streams and relays. Packet framing, protocol names,
authorization, MTU checks, queue limits, and public Forwarder APIs are unchanged.

`stream_dispatch_honors_selected_connection_and_replacement` failed before the
fix with `DirectTcpStream probe used peer-level request-response`. It now checks
both probes and packets against the selected handler's exact connection ID.

The test models two tracked connections for TCP and QUIC, then fails the newest
and verifies dispatch to the remaining connection. It polls real behaviour
notifications but does not establish those synthetic connections over sockets.

- Workspace: 1,216 tests passed; 18 opt-in tests ignored.
- Eight queue/MTU/probe fixtures now provide connection IDs; their assertions remain unchanged.
- Required Clippy groups, Rust formatting, and Nix `rust-test-sources` passed.
- All 11 Linux namespace scenarios passed in 222.43 seconds, including network moves and relay promotion.
- Logs: `/tmp/p2p-vpn-review-tcp-pin-*`.
- Rebuilt Android and final multi-network recovery validation remain outstanding.

### Pinned Closure

The pinned behaviour owns queued notifications, while its connection handler
owns pending frames. `on_swarm_event` currently ignores connection closure;
destroying a handler can discard pending requests without outbound-failure events.

Required regression: close a connection with queued and in-progress requests.
Each request must get exactly one terminal outcome; unaffected connections must
retain their requests. Include closure before handler notification is delivered.

This is not a promise to retransmit every lost IP packet. Distinguish definitely
unsent frames from ambiguous delivery before considering requeue; preserve
bounded memory, duplicate protection, and accurate drop accounting.

## Implementation Plan

1. Reproduce selected-connection dispatch and closure ownership separately.
2. Route direct TCP through connection-specific dispatch without changing framing.
3. Track terminal request ownership across handler events and connection closure.
4. Exercise replacement paths, bounded pending state, and stale terminal events.
5. Run native, namespace, and rebuilt Android recovery tests before closing findings.

## Fixture Boundary

The fixture echo responder queues replies independently of probe subscriptions.
`PacketAgent::probe` receives Linux-originated probe replies, the opposite
direction from the failed Android-originated ping. No fixture defect was
demonstrated in this limited inspection.
