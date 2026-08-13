# Feature Matrix

Status is conservative.

`Operational` means implemented and covered by tests or recorded evidence.

## Status Key

| Status | Meaning |
| --- | --- |
| Operational | Works with a verification path. |
| Partial | Works in some cases; evidence or production polish remains. |
| Blocked | Not possible through the current dependency or environment. |

## Matrix

| Area | Status | Verification |
| --- | --- | --- |
| Static LAN peers | Operational | Two-host LAN proof, namespace E2E. |
| Circuit relay fallback | Operational | Forced-relay LAN proof, namespace E2E. |
| Network move recovery | Operational | Minimal-config NixOS network-move VM proof. |
| Bounded queues | Operational | `cargo test queue::tests`. |
| Route ownership | Operational | `cargo test route::tests`. |
| Control protocol | Operational | `cargo test runtime::control`. |
| Packet protocol | Operational | `cargo test runtime::packet`. |
| Service protocol | Operational | `cargo test runtime::service`. |
| Direct QUIC stream fallback | Operational | Connection-pinned test and `nixos-vm-quic-stream`. |
| TCP and relay stream fallback | Operational | Flow-sharded compatibility stream tests. |
| Owned UDP packet plane | Operational | Packet-plane unit and namespace tests. |
| Owned QUIC packet plane | Operational | Quinn packet-plane tests, namespace E2E, and `nixos-vm-quic-datagram`. |
| Native libp2p QUIC datagrams | Blocked | Current libp2p swarm API lacks this handle. |
| Relay-to-direct DCUtR | Operational on supported Linux hosts | Namespace promotion test. |
| Public IPFS bootstrap | Partial | Rootless bootstrap checks. |
| Public relay discovery | Operational | Public reservation scan and live pairing smoke. |
| Public discovery-only pairing | Operational | `live_pair_accept_uses_public_relay_for_discovery_only_offer`. |
| Public DCUtR proof | Partial | Needs non-LAN topology evidence. |
| Underlay candidate hygiene | Operational | `cargo test overlay`, forced-relay VM proof. |
| Membership key | Operational | Config and control validation tests. |
| Signed membership records | Partial | Local and DHT tests; public DHT evidence remains. |
| NixOS module | Operational | Evaluation, consumer-flake, lifecycle, mesh, and pairing checks. |
| Release archive | Operational | Release archive sanity check. |

## Current Recorded Evidence

| Date | Evidence | Result |
| --- | --- | --- |
| 2026-08-04 | Namespace E2E suite | 7 passed. |
| 2026-08-04 | Public bootstrap smoke | Bootstrap and AutoNAT observed. |
| 2026-08-05 | LAN direct two-host test | Bidirectional ping passed. |
| 2026-08-05 | Forced relay two-host test | Bidirectional relay ping passed. |
| 2026-08-09 | Forced-relay VM candidate filter | Overlay addresses were not advertised as underlay candidates. |
| 2026-08-09 | NixOS network-move VM test | Minimal config recovered LAN to discovered relay to LAN. |
| 2026-08-09 | Relay-ready dialing regression | Stale direct connections no longer suppress relay fallback dials. |
| 2026-08-09 | Public relay repro refresh | Public relay scan found 12 candidates; one relayed peer circuit passed. |
| 2026-08-11 | Public relay reservation scan | One public relay accepted a reservation. |
| 2026-08-11 | Public discovery-only pairing smoke | Pairing completed through the public relay. |
| 2026-08-12 | Native NixOS pairing VM | Nix-only accept, identity reuse, traffic, and restart recovery passed. |

## Main Remaining Gap

Public non-LAN proof is still open.

The next evidence target is a host pair split by hotspot or VPN.

That run must prove overlay ping and route recovery without manual route edits.
