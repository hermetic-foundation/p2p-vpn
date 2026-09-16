# QUIC Data Plane

This document is the current QUIC protocol and implementation audit.

The historical development record remains in
[Automatic QUIC Packet Transport](automatic-quic-plan.md).

## Architecture

`p2p-vpn` uses two separate QUIC implementations.

| Layer | Implementation | Purpose |
| --- | --- | --- |
| Secure control transport | libp2p QUIC | Identify, capabilities, packet negotiation, discovery, and direct packet streams |
| Packet transport | Quinn QUIC DATAGRAM | Unordered, unreliable IP payload delivery |
| Packet fallback | Owned UDP | Compatible datagram delivery |
| Stream fallback | libp2p QUIC, TCP, or circuit relay | Framed packet delivery when datagrams are unavailable |

libp2p QUIC does not expose application datagrams through the Swarm API.
The pinned `libp2p-quic 0.13.1` also disables Quinn datagram receive buffers.

The owned Quinn plane remains necessary until upstream provides an authenticated,
application-level datagram API suitable for packet forwarding.

## Three Independent States

Do not infer packet transport from one connection or status field.

| State | Evidence | What It Proves |
| --- | --- | --- |
| Connection establishment | libp2p connection and `connection_id` | A secure control or stream path exists |
| Selected path | `daemon-paths` or `selected_path` | The scheduler currently prefers that path |
| Payload backend | Backend-specific packet counter growth | Packets were submitted to that backend |

`packet_plane_quic_sessions > 0` proves an installed session.
It does not prove that payload packets used the session.

## Defaults And Overrides

Minimal configuration enables ephemeral owned UDP and QUIC listeners.
Discovery advertises authenticated endpoints after listeners start.

| Configuration | Result |
| --- | --- |
| Omit `packet_plane` | Enable automatic UDP and QUIC listeners |
| `quic_listen: []` | Disable owned QUIC; retain UDP and streams |
| `listen: []` without `quic_listen` | Preserve legacy stream-only behavior |
| Empty `listen` plus nonempty `quic_listen` | Run QUIC without owned UDP |
| External endpoint list | Advertise an operator-known public mapping |

Static peer addresses are optional reachability hints.
They are not required for minimal configuration or network movement.

## Negotiation And Authentication

Capability exchange happens over an authenticated libp2p connection.
Owned QUIC requires the versioned bound-QUIC capability on both peers.

The lower overlay peer ID is the packet-handshake initiator.
The connection role can reverse when only the other endpoint is reachable.

The signed packet hello binds:

| Value | Purpose |
| --- | --- |
| Local and remote identities | Prevent peer substitution |
| Ephemeral keys | Derive the packet-session keys |
| Session IDs and nonce | Separate reconnects and rekeys |
| Role | Prevent initiator/responder reflection |
| MTU and capabilities | Bind negotiated transport behavior |

The QUIC listener uses a fresh self-signed certificate.
Its DER bytes travel in the authenticated capability exchange.

The client trusts only that certificate for the negotiated endpoint.
A generic public CA or hostname does not authorize packet transport.

## Connection Binding

The first unidirectional stream carries `p2pvpnQ1` and a 32-byte token.
The token hashes the signed handshake identity, key, session, nonce, and role.

| Condition | Action |
| --- | --- |
| Matching waiter | Assign the connection to that peer negotiation |
| Connection arrives before waiter | Retain it for at most two seconds |
| Unknown or malformed token | Close the connection |
| Preface stalls | Close after two seconds |
| Waiter is cancelled | Remove its registration and queued connection |
| More than 16 binding handshakes | Reject additional work |

One shared accept router dispatches by token.
Concurrent peers never depend on task wake order.

## Session Lifecycle

The default session lifetime is 600 seconds.
Pending packet hellos expire after 25 seconds.

QUIC connection attempts time out after 10 seconds.
Failed owned-QUIC attempts retry after 30 seconds.

| Event | Required Transition |
| --- | --- |
| Capability or authorization removal | Cancel pending work and remove the owned session |
| Peer or route removal | Drop stale queued packets before framing |
| Session expiry | Mark QUIC unhealthy and renegotiate if still eligible |
| Connection failure | Demote QUIC and use the next healthy backend |
| Replacement negotiation | Preserve a healthy session until replacement is valid |
| Network reset | Withdraw observed endpoints; retain configured endpoints and listeners |
| New authenticated endpoint | Retry and promote healthy QUIC automatically |
| Shutdown | Cancel owners, close connections, and release listener tasks |

Late task generations and stale request IDs cannot install a replacement.
Authorization is rechecked at dequeue and session-install boundaries.

## Replay And Sequence Rules

Packet sessions use an epoch, session ID, and sequence number.
Duplicate or stale packets are rejected before TUN delivery.

Replay windows are bounded per session.
The default maximum is 1,024 windows.

The sender rotates the packet epoch before sequence wrap.
Loss and reordering inside the accepted window do not force retransmission.

## Path Ordering

The scheduler uses the highest healthy compatible path.

| Priority | Path |
| ---: | --- |
| 1 | Owned Quinn QUIC DATAGRAM |
| 2 | Owned UDP datagram |
| 3 | Direct libp2p QUIC stream |
| 4 | Direct libp2p TCP stream |
| 5 | libp2p circuit-relay stream |

LAN discovery and direct candidates run before public recovery.
Restored healthy QUIC is promoted without restart or route changes.

Stream packets are pinned to the selected connection ID.
They cannot silently move to another peer-scoped connection.

## MTU Contract

The packet hello negotiates the smaller peer MTU.
The live Quinn connection limit is checked again for every send.

| Condition | Behavior |
| --- | --- |
| Payload fits negotiated and live limits | Submit one QUIC datagram |
| QUIC limit shrinks below payload | Reject QUIC without destroying the session |
| UDP can carry the packet | Try owned UDP fallback |
| Only a smaller stream path exists | Reject at the selected path MTU |
| Oversized IPv4 packet | Return ICMP fragmentation-needed; do not fragment internally |
| Oversized IPv6 packet | Return packet-too-big when the platform path supports it |

The default direct overlay MTU is 1,280 bytes.
Circuit-relay paths advertise 1,200 bytes.

Physical IPv6 PMTU behavior across arbitrary routers is not certified.
The deterministic tests cover negotiated boundaries and smaller-path fallback.

## Bounds

| Owner | Bound |
| --- | ---: |
| QUIC binding handshakes plus early connections | 16 |
| QUIC unidirectional streams per connection | 1 |
| QUIC datagram receive buffer | Four maximum packet-plane datagrams |
| Pending path probes | 4,096 |
| Pending outbound libp2p connections | 16 by default |
| Packet session replay windows | 1,024 by default |
| Packet session lifetime | 600 seconds by default |
| QUIC bind preface and early retention | 2 seconds |
| QUIC connect timeout | 10 seconds |
| Packet hello timeout | 25 seconds |

Per-peer queues also cap packets, bytes, and age.
Resource settings retain their existing configurable limits.

## Compatibility

All QUIC capability fields are additive and version-gated.
Peers that omit bound-QUIC support continue through UDP or streams.

The implementation preserves:

| Surface | Compatibility Rule |
| --- | --- |
| Identity and membership | Existing keys, signed records, and route authority remain unchanged |
| Minimal configuration | QUIC is automatic; no endpoint or route is required |
| Explicit overrides | Empty and explicit listener settings retain documented behavior |
| NixOS | Native options generate the same validated runtime configuration |
| Android | Existing serialized config remains accepted |
| CLI and status | New fields and counters are additive |
| Multiple networks | Every instance owns independent listeners, sessions, queues, and state |

## Diagnostics

Start with these read-only commands:

```sh
p2p-vpn daemon-paths --instance NAME
p2p-vpn daemon-state --instance NAME
p2p-vpn daemon-mtu --instance NAME
p2p-vpn daemon-capabilities --instance NAME
```

| Metric | Interpretation |
| --- | --- |
| `packet_plane_quic_sessions` | Installed owned-QUIC sessions |
| `outbound_owned_quic_datagram_packets` | Successful owned-QUIC payload submissions |
| `outbound_owned_udp_datagram_packets` | Successful owned-UDP payload submissions |
| `outbound_direct_quic_stream_fallback_packets` | Direct QUIC stream payloads |
| `outbound_direct_tcp_stream_fallback_packets` | Direct TCP stream payloads |
| `outbound_relay_stream_fallback_packets` | Circuit-relay stream payloads |
| Queue packet, byte, drop, and expiry counters | Backpressure and stale-work evidence |
| QUIC task, owner, failure, and demotion counters | Lifecycle and recovery evidence |

## Deterministic Evidence

| Scenario | Coverage |
| --- | --- |
| Direct payload and minimal defaults | Owned-QUIC namespace tests and `nixos-vm-quic-datagram` |
| Both initiator directions | Four movement role combinations: `aa`, `ab`, `ba`, and `bb` |
| Concurrent and early accepts | Binding-router unit tests |
| Unknown, malformed, stalled, and cancelled binding | Binding-router negative tests |
| QUIC blocked at startup | UDP fallback, restoration, and QUIC promotion namespace test |
| QUIC blocked after establishment | Demotion, UDP delivery, and autonomous promotion namespace test |
| Endpoint change and LAN return | Address-movement namespace and NixOS movement tests |
| Loss, reordering, duplicate, and replay | Packet-plane deterministic tests |
| MTU boundaries | Negotiated, live Quinn limit, fallback, and oversized packet tests |
| Older UDP-only peer | Minimal-config compatibility namespace test |
| Stream-only peer | Minimal-config compatibility namespace test |
| Relay-only path | `nixos-vm-forced-relay` |
| Membership and multi-peer isolation | NixOS membership-convergence VM |

## Sustained QUIC Result

The 2026-09-16 Linux namespace run used 50 packets per second for 300 seconds.
It then observed a 60-second idle drain.

| Result | Node A | Node B |
| --- | ---: | ---: |
| Sent and received packets | 15,000 | 15,000 |
| Duplicate, invalid, or skipped packets | 0 | 0 |
| Threads | 20 stable | 20 stable |
| File descriptors | 17 stable | 17 stable |
| Socket descriptors | 7 stable | 7 stable |
| RSS range | 39,040-39,168 KiB | 39,024-39,128 KiB |
| Approximate one-core CPU during load | 9.49% | 7.97% |
| Approximate one-core CPU during drain | 0.2% | 0.2% |

Exactly one owned-QUIC session stayed active on each node.
Queues, task owners, stream fallback, failures, and path demotions remained zero.

Run the same proof with:

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
  nix develop -c cargo test --test tun_namespace \
  tun_namespace_measures_sustained_quic_resources \
  -- --ignored --exact --nocapture
```

## Requirement Audit

| Requirement | State | Evidence |
| --- | --- | --- |
| Exact transport architecture | Complete | Architecture and three-state tables above |
| Authenticated binding and lifecycle | Complete | Binding, lifecycle, replay, and negative tests |
| QUIC-first fallback and promotion | Complete in deterministic Linux tests | Startup, live-block, movement, and NixOS VM checks |
| MTU enforcement | Complete for tested IPv4 paths | Unit and namespace boundary tests |
| Resource bounds and sustained load | Complete | Bounds table and 360-second resource capture |
| Mixed-version and configuration compatibility | Complete | UDP-only, stream-only, NixOS, and config tests |
| Physical Linux mesh | Complete for installed fleet build | Five-host bidirectional LAN result below |
| Physical hotspot or upstream VPN | Deferred | Requires a later movement-certification goal |
| Physical Android movement | Deferred | Requires a connected authorized device |
| Arbitrary physical IPv6 PMTU | Deferred | Deterministic behavior is covered; router diversity is not |

No deferred physical claim is implied by deterministic or VM evidence.

## Read-Only Linux Mesh Result

The 2026-09-16 audit inspected five Linux hosts on `personal-devices`.
No service, configuration, route, or underlay was changed.

| Check | Result |
| --- | --- |
| Installed package | Same Nix store path on all five hosts |
| Service state | Active on all hosts; zero systemd restarts |
| Authenticated QUIC | Four owned-QUIC sessions per Linux host |
| Selected path | Direct QUIC datagram between every Linux host and the audit host |
| Audit host to peers | 20/20 IPv4 packets delivered |
| Peers to audit host | 20/20 IPv4 packets delivered |
| Backend proof | Audit-host QUIC payload counter increased by exactly 20 |
| Unexpected fallback | UDP and stream counters stayed flat during the outbound burst |
| Active lifecycle work | Zero QUIC connection tasks and task owners |
| Process stability | Stable PIDs; 7-19 threads; 30,588-43,608 KiB RSS |

The installed fleet build was
`/nix/store/gzc2x4z4nc82fy6904falysggk9823sx-p2p-vpn-0.1.0`.

This read-only audit does not claim that the un-deployed working-tree build ran
on those hosts. Current-source behavior is covered by deterministic and VM tests.
