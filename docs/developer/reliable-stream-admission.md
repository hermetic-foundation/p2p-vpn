# Reliable Stream Admission

The [stream specification](https://github.com/hermetic-foundation/libp2p-vpn/blob/716ef177c26b1ff7e7d4659d97d770ffeac9e6b5/docs/developer/protocol/stream.md#connection-admission) requires a
grant on the authenticated member connection carrying the packet. The native
peer capability cache may inform discovery or datagram negotiation, but it
cannot grant reliable-stream access on a different connection.

## Ownership

| Layer | Owns |
| --- | --- |
| Shared Rust | MTU negotiation, the exact-binding grant and size gate at ingress, and forwarding checks against current membership and routes. |
| Browser | WebRTC and relay I/O, connection-local request tracking, candidate validation, and the in-app IP device. |
| Native | libp2p event and socket ownership, capability publication and cancellation, queue selection, physical-route evidence, and the system packet device. |

The browser and native ingress handlers call the same grant and packet-size
check before the shared forwarder. Both use the authenticated binding's
negotiated MTU. This does not require them to share a swarm loop, a capability
publication table, or path-selection policy.

The native queue ranks eligible send paths, so an unadmitted stream cannot
displace a healthy datagram path. During overlap, an admitted, confirmed
connection remains eligible while the candidate proves it can carry traffic.
Control acceptance alone does not retire an admitted direct sibling.

For a peer-initiated relay handoff, the grant permits the first authorized
incoming packet without exposing the candidate to native sends or health probes
early. A direct readiness probe that fails cannot retire the candidate.

## Coverage

| Trace | Observable assertion |
| --- | --- |
| Native direct and relayed replacements | The packet-device sink receives nothing before the replacement's own capability response, including after a delayed success on the closed connection. |
| Native and browser overlap | Closing the old connection leaves the admitted survivor able to deliver the next packet. The test checks both selection and delivery. |
| Browser stale completion | An earlier capability or probe result cannot authorize or promote a separate candidate. |
| Native datagram fallback | An unadmitted stream with a higher raw score does not stop delivery over the eligible datagram path. |

## Verification

The November 24, 2026 runs used local network namespaces. The wire suite runs
each file under a separate timeout, so a later file cannot consume another
file's remaining test time.

| Revision | Check | Result |
| --- | --- | --- |
| `5149cfba` | Browser Rust library and protocol vectors | 28 browser tests and 3 vectors passed. |
| `5149cfba` | Production and test-hook WASM; library without default features | Builds and the no-default-features check passed. |
| `5149cfba` | Chromium wire suite | 39 passed: 26 receiver, 1 Circuit Relay v2, 8 incoming relay, and 4 checkpoint. |
| `230fb12d` | Native Rust library | 2,248 passed, zero failures, 41 existing ignored. |
| `230fb12d` | Production Chromium with native TUN | IPv4 and IPv6 HTTP, mixed MTUs, address-free discovery, and scope rejection passed. |
| `230fb12d` | Production Chromium preferred handoff | Fresh HTTP completed in 2,414 ms; both underlays stayed healthy for 31 seconds. |
| `230fb12d` with `3667cd92` WASM | Pre-handoff Chromium compatibility | A fresh pre-extension build kept HTTP and its original circuit working for 32,724 ms. No handoff commit occurred. |
| `230fb12d` | Formatting and native Clippy | Formatting passed; the correctness, suspicious, and performance lint groups passed with warnings outside the gate. |

The initial live handoff run exposed a regression: an exploratory native probe
received EOF while the browser candidate was still gated. The native runtime
mistook the EOF for a failure of the whole circuit. The repaired run reached
payload proof and commit before retiring the old member circuit.

Reproduce with `scripts/test-browser-linux.sh wire` and the
[preferred handoff harness](preferred-handoff-verification.md#reproduce).
These runs do not qualify public-relay timing, Android device behavior, or an
independent implementation of the wire specification.

## Further Extraction

Keep native capability publication and browser candidate tracking separate.
Native publication also owns bounded discovery, generation cancellation, and
control-only updates. The browser tracks one exact dial and its candidate
probe. Sharing the tables would tie these distinct lifecycles together.

Keep OS route ownership and WebRTC signaling in their adapters. The
[preferred handoff extension](https://github.com/hermetic-foundation/libp2p-vpn/blob/716ef177c26b1ff7e7d4659d97d770ffeac9e6b5/docs/developer/protocol/preferred-handoff.md) needs physical
socket evidence, not just an accepted packet probe. Its wire messages,
initiation rules, and commit criteria did not change here.

Checkpoint publisher expiry and relay pin interpretation require separate
conformance decisions. Neither was changed to make session tests pass.

Membership snapshot transfer and demand lifecycle already use shared Rust
transitions. The reliable-stream traces do not show another common decision
there that needs extracting.
