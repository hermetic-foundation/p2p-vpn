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
| Bounded queues | Operational | `cargo test queue::tests`. |
| Route ownership | Operational | `cargo test route::tests`. |
| Control protocol | Operational | `cargo test runtime::control`. |
| Packet protocol | Operational | `cargo test runtime::packet`. |
| Service protocol | Operational | `cargo test runtime::service`. |
| Stream fallback | Operational | Flow-sharded stream fallback tests. |
| Owned UDP packet plane | Operational | Packet-plane unit and namespace tests. |
| Owned QUIC packet plane | Operational | Quinn packet-plane tests and namespace E2E. |
| Native libp2p QUIC datagrams | Blocked | Current libp2p swarm API lacks this handle. |
| Relay-to-direct DCUtR | Operational on supported Linux hosts | Namespace promotion test. |
| Public IPFS bootstrap | Partial | Rootless bootstrap checks. |
| Public relay discovery | Partial | `relay-scan` and `relay-check`. |
| Public DCUtR proof | Partial | Needs non-LAN topology evidence. |
| Membership key | Operational | Config and control validation tests. |
| Signed membership records | Partial | Local and DHT tests; public DHT evidence remains. |
| NixOS module | Operational | NixOS module and VM smoke checks. |
| Release archive | Operational | Release archive sanity check. |

## Current Recorded Evidence

| Date | Evidence | Result |
| --- | --- | --- |
| 2026-08-04 | Namespace E2E suite | 7 passed. |
| 2026-08-04 | Public bootstrap smoke | Bootstrap and AutoNAT observed. |
| 2026-08-05 | LAN direct two-host test | Bidirectional ping passed. |
| 2026-08-05 | Forced relay two-host test | Bidirectional relay ping passed. |

## Main Remaining Gap

Public non-LAN proof is still open.

The next evidence target is a host pair split by hotspot or VPN.
