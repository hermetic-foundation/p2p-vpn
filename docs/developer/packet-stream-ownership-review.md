# Packet Stream Ownership Review

## Scope

Source inspected at `be99de43` on 2026-09-06 by a read-only reviewer and parent
inspection. These findings do not establish the cause of the retained Android
[post-update packet loss](android-multi-network-review.md#earlier-update-failure).

## Findings

| Priority | Finding | Source |
| --- | --- | --- |
| P2 | Direct-TCP dispatch ignored selected connection; reproduced and corrected | Original: `src/runtime/runner.rs:9904`, `src/runtime/forward.rs:280` |
| P2 | Pinned-stream connection closure lacked terminal request events; corrected with lifecycle regressions | Original: `src/runtime/pinned_packet_stream.rs:165`, `:234` |
| P2 | Stale terminal packet replies stranded runtime window slots; corrected in both stream handlers | `handle_pinned_packet_stream_event`, `handle_packet_event` |
| P2 | Epoch-valid replies from failed connections could revive paths with no connection; connection-scoped RTT guard added | `handle_packet_response`, `PathSet::record_stream_rtt` |

### Failed-Connection Responses

A stream failure can remove a connection from the path inventory before its
underlay epoch is retired. A late accepted packet response previously updated
RTT by peer/path alone, restoring `healthy` even with zero connections.

The deterministic pre-fix regression produced a selectable TCP path with
`established_connections=0` and `latest_connection_id=None`. It failed before
the guard was added; this is distinct from closed/retiring-epoch filtering.

Both stream handlers now pass the response connection ID to `record_stream_rtt`.
The path owner requires that exact connection in the matching peer/path/relay
inventory before changing RTT or health. Request completion still releases its slot.

| Contract | Coverage |
| --- | --- |
| Failed last connection | Late acceptance must not revive a path |
| Replacement connection | Old acceptance must not alter the replacement path |
| Duplicate response | Only its own request slot is released; a newer request remains |
| Valid RTT | Tracked connections, including non-selected ones, can update RTT |
| Isolation | Wrong peer, relay, or unknown connection cannot update a candidate |
| Transport scope | Runtime handlers plus TCP, QUIC-stream, and relay path-owner cases |

- Negative-control log: `/tmp/p2p-vpn-review-failed-path-response-before.log`.
- Existing public datagram RTT methods, wire formats, and configuration are unchanged.
- No Lean model exists in this repository; these are executable regression checks.
- The intermittent [queue-pressure failure](queue-pressure-review.md#repetition-evidence) is not yet causally attributed to this defect.

#### Verification

- Native workspace: 1,234 passed; 23 opt-in tests ignored.
- Runtime regression: 16 cases across both response handlers, including duplicate responses.
- Path-owner regression: TCP, QUIC-stream, and relay cases preserve valid RTT updates.
- Required Clippy groups, changed-file rustfmt, whitespace, and offline Nix source inclusion passed.
- Logs: `/tmp/p2p-vpn-review-failed-path-response-workspace-final.log` and `/tmp/p2p-vpn-review-failed-path-response-clippy.log`.
- An initial path test incorrectly expected direct paths to match relay metadata; corrected to test relay isolation only on relay paths.
- No Android deployment or full NixOS package rebuild was performed for this fix.

The full namespace run passed 11/12 in 252.06 seconds, including queue pressure.
Movement passed its live path/ping checks but failed a log assertion requiring
an immediate direct-to-relay event rather than direct-to-unavailable-to-relay.

The movement fixture now requires at least five additional relay-forwarded
packets while the direct link is down, retaining path selection, ping, and
direct-promotion checks. Its focused rerun passed in 51.58 seconds.

- Suite log: `/tmp/p2p-vpn-review-failed-path-response-namespace.log`.
- Corrected movement log: `/tmp/p2p-vpn-review-failed-path-response-move-final.log`.
- Final fixture unit tests: 11 passed, 12 ignored; focused Clippy and Nix source checks passed.
- The full workspace and 12-scenario suite preceded the test-only movement assertion correction.

### Stale Response Accounting

Both packet handlers previously discarded stale-connection responses before
completing their `PacketInFlight` entries. A terminal reply queued before closure
could therefore retain its peer/shard slot until the ordinary request timeout.

Stale terminal responses now release their own request ID before returning.
They still cannot update RTT, promote paths, inject packets, or record response
rejections against the current path. No packet is requeued or retransmitted.

The regression dispatches accepted and rejected replies through both handlers
after closure, retirement, and network-epoch advancement. Repeated old replies
must leave a newer request intact, free capacity, and preserve paths and metrics.

- Before the fix, the closed pinned case retained two slots instead of one.
- Events and connection retirement are injected; this is not socket race-frequency evidence.
- Transport-level exact-owner validation remains separate from runtime accounting.
- All 12 stale-response combinations pass, including duplicate delivery and newer-request isolation.
- Workspace: 1,225 passed, 18 opt-in tests ignored; required Clippy groups, changed-file formatting, and Nix source parity pass.
- Non-fatal style warnings remain, including the new regression's function length.
- Namespace rerun: all 11 pass after correcting [relay-LAN test isolation](testing.md#namespace-e2e); retain the original 10/11 result separately.
- Logs: `/tmp/p2p-vpn-review-stale-packet-*`.

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
owns pending frames. Previously, `on_swarm_event` ignored connection closure;
destroying a handler could discard requests without outbound-failure events.

Required regression: close a connection with queued and in-progress requests.
Each request must get exactly one terminal outcome; unaffected connections must
retain their requests. Include closure before handler notification is delivered.

This is not a promise to retransmit every lost IP packet. Distinguish definitely
unsent frames from ambiguous delivery before considering requeue; preserve
bounded memory, duplicate protection, and accurate drop accounting.

#### Correction and Evidence

The behaviour now tracks request ID, peer, and connection ownership separately
from payloads. Closure removes queued notifications for that connection and
emits one outbound failure for each outstanding request, including handler-owned work.

- Matching success or failure removes ownership before queuing its terminal event.
- Duplicate, late, wrong-peer, and wrong-connection terminal events are ignored.
- Already-queued terminal outcomes and requests on other connections survive closure.
- Unknown, closed, and wrong-peer targets fail without retaining request ownership.
- Existing framing, failure variants, and retransmission semantics are unchanged.

`closure_completes_queued_and_delivered_requests_once` reproduced a notification
targeting the closed connection before the fix. It now covers queued dispatch
and a real handler emitting an outbound upgrade request before being dropped.

`terminal_events_require_exact_owner_and_survive_later_closure` covers success
and failure, mismatched owners, duplicates, and closure before terminal delivery.
These are deterministic lifecycle tests, not socket-level race or formal proofs.

- Workspace: 1,219 tests passed; 18 opt-in tests ignored.
- Required Clippy groups and changed-file Rust formatting passed.
- All 11 namespace scenarios passed on final code in 187.50 seconds.
- Nix `rust-test-sources` passed with offline, single-job execution.
- Logs: `/tmp/p2p-vpn-review-closure-*`.
- Rebuilt Android recovery validation remains outstanding; no loss-causality claim follows from these checks.

### Pinned Resource Ownership

Ownership holds metadata only, with logarithmic insertion/removal and linear
closure scanning. Established-connection events govern admission, preventing
orphan ownership when callers target nonexistent or already-closed connections.

Previously, `src/runtime/p2p.rs` passed `max_concurrent_packet_streams` only to
request-response packet transport. Pinned handlers had no configured capacity
and retained inbound streams and response writes without a response deadline.

The same setting now configures pinned transport. A Tokio semaphore belongs to
each handler and is shared with inbound upgrades. Permits follow the actual
work and release on completion, read/write failure, timeout, or cancellation.

| Owner | Bound / Lifetime |
| --- | --- |
| Behaviour outbound notifications | At most the configured limit of outstanding requests per connection |
| Handler outbound requests | Permit held across queueing and outbound upgrade |
| Admitted inbound frame read | Permit acquired before reading packet bytes |
| Overload response | Drain through a 256-byte buffer; return existing `RateLimited` without retaining a payload |
| Inbound response worker | Retains the read permit through application wait and response write |
| Response deadline | Ten seconds from admitting a fully read inbound packet |
| Closed handler | Dropped workers and request records release permits |

The public constructor remains available with the existing resource default of
256; host construction supplies the configured limit. Framing and failure enum
variants are unchanged. A local I/O marker distinguishes capacity exhaustion.

Local capacity failures release runtime in-flight accounting but do not demote
the path or start redialing. They still count as outbound failures. Remote
capacity rejection uses the existing packet-level `RateLimited` response.

Resetting an overloaded inbound stream was rejected during review: simultaneous
outbound requests could fill both peers' budgets and make healthy connections
look failed. Rejection now drains one validated, MTU-bounded frame before replying.

Protocol negotiation and overload-drain concurrency remain governed by libp2p's
bounded inbound-upgrade pool and upgrade timeout. Overload drains retain no
packet payload and never enter the application response-worker queue.

Regression coverage includes:

- Per-connection admission, zero-limit normalization, and reuse after completion or closure.
- Outbound capacity held until upgrade success or failure.
- Inbound admission before parsing, parse failure, and read-future cancellation.
- Response success, write failure, omitted response, stalled write, and handler destruction.
- Capacity exhaustion preserving the selected relay connection and its replacement.
- A TCP/QUIC exchange with an occupied one-slot receiver budget returns `RateLimited`, then completes its outstanding request without disconnecting.

#### Inbound Owner Boundary

The default host advertises `/p2p-vpn/packet/1` through both request-response and
pinned behaviours. The socket regression initially received the request through
request-response, not the pinned receiver, and dropped that unhandled response channel.

The overload regression therefore disables request-response inbound support
on its two test nodes. It exercises real TCP and QUIC sockets with a pinned
receiver, but does not prove default-host inbound dispatch selects that receiver.

Default-host request-response inbound work has its existing independent budget.
Pinned outbound work has its own bound. Source review confirms that the pinned
libp2p `SelectUpgrade` gives the first matching handler priority.

The chosen compatibility contract retains `packet` before `pinned_packet_stream`
in the derived host. Default inbound requests emit `BehaviourEvent::Packet`;
standalone pinned behaviour users retain their existing inbound support.

The default TCP/QUIC socket test now rejects inbound `PinnedPacketStream` events
instead of accepting either owner. It also checks the authenticated peer, frame,
and matching pinned outbound response. This documents and guards the existing
owner; it does not remove the second registration or merge their budgets.

The focused TCP/QUIC exchange passed in 0.29 seconds. Source basis:
`libp2p-core 0.43.2` (`upgrade/select.rs`), `libp2p-swarm 0.47.1`
(`handler/select.rs`), and `libp2p-swarm-derive 0.35.1` (field-order handler composition).

Log: `/tmp/p2p-vpn-review-default-packet-owner.log`.

Workspace verification passed: 1,226 tests, 18 opt-in tests ignored. Required
Clippy groups, changed-file formatting, and Nix source parity passed. Runtime
behaviour is unchanged; namespace and Android scenarios were not rerun for this contract test.

#### Remaining Evidence

1. Extend the [TCP namespace queue-pressure evidence](queue-pressure-review.md) to longer memory trends, other transports, and independent byte-limit saturation.
2. Repeat affected platform checks after further runtime changes; retain earlier packet-loss evidence.
3. Preserve the documented default inbound owner in any future handler consolidation.

#### Verification

- Workspace: 1,224 tests passed; 18 opt-in tests ignored.
- Required Clippy groups, changed-file Rust formatting, and Nix source parity passed.
- TCP and QUIC overload regression passed with isolated pinned receivers.
- All 11 namespace scenarios passed with the default host, including network move and relay promotion.
- Logs: `/tmp/p2p-vpn-review-stream-bounds-*`.
- Rebuilt Android and fixture passed the [68-step multi-network run](android-multi-network-review.md#latest-attempt) at `643d798e`.
- The later stale-response accounting regression passes; sustained-overload recovery remains a separate gate.

These limits bound admitted payload/stream work, not arbitrary accumulation of
terminal events by an embedding caller that never polls the public behaviour.
Unit tests exercise ownership transitions; they are not formal verification.

## Paced Stream Saturation

The opt-in `sustained_pinned_overload_recovers_on_the_same_connection` test
extends the existing socket overload harness with 1,500 cycles per transport.
Each cycle waits 20 ms before repeating on the same selected connection.
The opt-in run has a three-minute overall deadline; each exchange also has
the original ten-second deadline.

| Stage | Required Assertion |
| --- | --- |
| Occupy receiver | A pending outbound request holds its only stream permit |
| Reverse request | The full receiver replies `RateLimited`, rather than resetting the stream |
| Release held response | Original request completes with `Accepted` |
| Per-cycle cleanup | Both behaviours have zero outstanding outbound request owners |
| Connection continuity | No observed connection closure; both peers remain connected |

```bash
cargo test --offline --lib sustained_pinned_overload_recovers_on_the_same_connection \
  -- --ignored --nocapture
```

### Scope and Limits

- Real loopback TCP and QUIC streams; 1,024-byte synthetic payloads, not kernel TUN traffic.
- Pinned inbound reception is isolated, as in the original overload regression; default hosts still use the documented Packet event owner.
- Pacing limits offered packet payload to about 0.82 Mbps before framing; protocol/transport overhead is additional.
- Public discovery features are disabled and bootstrap entries removed before polling.
- Counts inspect behaviour-owned request metadata, not handler heap allocations, allocator reclamation, or total daemon memory.
- The test checks repeated full-budget recovery, not maximum throughput, full-daemon queues, relay saturation, or carrier NAT.
- No production protocol, configuration, or retry behavior changes; the new ownership accessor is compiled only for tests.

### Initial Measurement

| Transport | Completed Cycles | Duration | Outstanding Owners After Every Cycle |
| --- | ---: | ---: | ---: |
| TCP | 1,500 | 37.728 s | 0 on both peers |
| QUIC | 1,500 | 38.269 s | 0 on both peers |

Both passes required `RateLimited` followed by `Accepted` in every cycle, with
no observed connection closure. The complete test passed in 76.06 seconds.
Log: `/tmp/p2p-vpn-review-sustained-pinned-final.log`.

This first run preceded the addition of an overall deadline and transport labels.
The existing one-cycle regression now shares the same harness and assertions.

### Final Verification

The final p2p-module run passed all 35 tests, including all three opt-in exercises,
in 76.03 seconds. TCP completed 1,500 cycles in 37.710 seconds and QUIC in
38.249 seconds; every cycle passed the same cleanup and continuity assertions.

- Final module log: `/tmp/p2p-vpn-review-sustained-pinned-module.log`.
- Handler lifecycle log: `/tmp/p2p-vpn-review-sustained-pinned-handler.log`.
- All nine handler tests, required Clippy groups, changed-file formatting, whitespace, and offline Nix test-source inclusion passed; non-fatal style warnings remain.
- Full workspace and platform deployments were not repeated for this test-only change; their earlier results remain separately scoped.
- Builds reused existing targets with two Cargo jobs and no downloads; retained build directories remained about 3.6 GiB combined.

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
