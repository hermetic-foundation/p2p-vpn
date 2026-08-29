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
| Code pairing on LAN | Operational | Namespace E2E and `nixos-vm-code-pairing-lan`. |
| Code pairing through relay | Operational | Isolated `nixos-vm-code-pairing-relay`. |
| Code pairing approval | Operational | CLI, RPC, protocol, namespace, and VM tests. |
| Durable pairing enrollment | Operational | Encrypted-state unit tests and service-restart VM proofs. |
| Network-wide membership convergence | Operational | Delegated-admission four-VM full-mesh proof. |
| Bounded path recovery | Operational | Coalesced dials, tracked DHT queries, and relay-pressure VM assertions. |
| Durable learned membership | Operational | Offline restart and owner-only state tests. |
| Membership conflict, expiry, revocation | Operational | Trust-graph and restart-safe history unit tests. |
| Public DCUtR proof | Partial | Needs non-LAN topology evidence. |
| Underlay candidate hygiene | Operational | `cargo test overlay`, forced-relay VM proof. |
| Membership key | Operational | Config and control validation tests. |
| Signed membership records | Operational | Unit, DHT, code-pairing, restart, and revocation tests. |
| Authenticated overlay DNS | Operational | Unit, CLI, module lifecycle, transitive VM, and physical four-host proofs. |
| NixOS module | Operational | Evaluation, lifecycle, mesh, offline pairing, and code-pairing checks. |
| Android Nix build | Operational | Rust tests, cross build, Java tests, lint, APK, signature, and manifest gate. |
| Android pair and connect | Partial | API 35 persistence, code pairing, and dual-stack traffic proven; recovery and hardware remain. |
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
| 2026-08-22 | Peerless code-pairing namespace | LAN discovery, approval, and bidirectional traffic passed. |
| 2026-08-22 | NixOS code-pairing LAN VM | Artifacts, evaluation, acknowledgment, restart, and traffic passed. |
| 2026-08-22 | NixOS code-pairing relay VM | DHT discovery, relay transport, restart, and traffic passed. |
| 2026-08-25 | Membership restart and movement VM | Offline route restore, relay fallback, cold restart, and LAN promotion passed. |
| 2026-08-26 | NixOS membership convergence VM | A admitted B, B admitted C, and all three converged. |
| 2026-08-26 | Authenticated DNS lifecycle VM | Transitive names, relay, restart, expiry, conflict, rename, and revocation passed. |
| 2026-08-26 | Physical four-member LAN | Four signed histories converged; 240/240 directed TCP probes passed. |
| 2026-08-26 | Physical desktop restart | Routes and three direct path classes recovered; 120/120 probes passed. |
| 2026-08-26 | Public discovery address hygiene | Unverified private IPFS addresses were rejected without failed-neighbor growth. |
| 2026-08-26 | IPv6 first activation | `nodad` removed the route-source race; the service started with zero retries. |
| 2026-08-26 | Physical authenticated DNS mesh | Four signed names converged; short, canonical, PTR, IPv4, IPv6, and restart checks passed. |
| 2026-08-29 | Android/Linux emulator E2E | Profile persistence, code pairing without an overlay peer address, and four 5/5 traffic checks passed. |

## Main Remaining Gaps

| Gap | Required Evidence |
| --- | --- |
| Public code pairing | Two fresh hosts pair from only a code and pass traffic without manual routes. |
| Android recovery | Emulator survives process death and controlled underlay changes. |
| Android path modes | QUIC stream, TCP fallback, and relay-only scenarios pass separately. |
| Android hardware | Physical arm64 device pairs, reconnects, and passes dual-stack traffic. |
