# Developer Documentation

Use these docs when changing, testing, or debugging `p2p-vpn`.

## Contents

| Document | Use It For |
| --- | --- |
| [Architecture](architecture.md) | Runtime and protocol layout. |
| [Protocol Specification](https://github.com/hermetic-foundation/libp2p-vpn/blob/cc40da135edd90abcd008f8fc9c54a6695272c95/docs/developer/protocol/README.md) | Interoperability rules, wire formats, discovery, membership, and conformance. |
| [SDK dependency](sdk-dependency.md) | Update and roll back the shared SDK pin. |
| [Browser WASM Validation](browser-wasm-validation.md) | Browser/native packet proof, compatibility fixes, bounds, and remaining gates. |
| [Browser Code Reuse](browser-code-reuse.md) | Shared protocol logic and the browser and companion adapter boundaries. |
| [Browser Real-Network Validation](browser-real-network-validation.md) | Physical LAN enrollment and credential reopen, plus public relay traffic across an LTE-to-Wi-Fi handoff. |
| [Shared Path Policy](shared-path-policy.md) | Draft common routing contract and the browser enforcement boundary. |
| [Browser LAN Harness](browser-lan-validation.md) | Split-device setup, paired evidence checks, local smoke results, and the pending physical LAN gate. |
| [QUIC Data Plane](quic.md) | Current protocol, lifecycle, bounds, evidence, and requirement audit. |
| [QUIC Interface Patches](quic-interface-patches.md) | Additive Quinn/UDP interface-selection APIs, provenance, and fail-closed behavior. |
| [Physical Packet Ownership](packet-plane-physical-ownership.md) | Route-owned packet primitives, independent UDP RX/TX authorization, lifetime bounds and verification limits. |
| [Reliability Review](refactor-review.md) | Findings, refactoring sequence, and completion evidence. |
| [Kademlia Resource Plan](kademlia-resource-plan.md) | Completed aggregate bounds and remaining settling/measurement phases. |
| [Aggregate Ownership Audit](kademlia-final-ownership-audit.md) | Enforced limits, production query owners, compatibility, and final phase-1 evidence. |
| [Recovery And Settling](kademlia-settling.md) | Phase-2 acceptance matrix, timer contracts, findings, and harness gaps. |
| [Android Architecture](android.md) | Shared TUN, network isolation, VpnService, persistence, and recovery. |
| [Android Network Attempts](android-network-attempts.md) | Bounded backend selection/callback contracts and host evidence; not runtime or device activation. |
| [Android Live Transport Snapshot](android-live-transport-snapshot.md) | Bounded, on-demand peer/circuit/socket evidence during device handoff failures. |
| [Android LAN Discovery](android-lan-discovery.md) | Network-bound LAN selection, multicast ownership, receive scoping and unfinished device acceptance. |
| [Linux Route Monitor](linux-route-monitor.md) | Notification admission, bounded stale-route diagnostics and regression evidence. |
| [UDP Path Proof](udp-path-proof.md) | First-session and replacement ACK admission, directional authorization and resource bounds. |
| [Android Battery Efficiency](android-battery-efficiency.md) | 10x target, physical-device measurement contract, and feature-preservation gates. |
| [Cellular Maintenance Isolation](android-cellular-combined-isolation.md) | Combined DHT/probe diagnostic, normal/off reference, and battery-accounting caveats. |
| [Cellular Battery Comparison](android-cellular-comparison.md) | Matched raw-report/rail analysis, conservative uncertainty bounds, and paired criteria. |
| [Cellular Window Capture](android-cellular-capture.md) | USB-only collection contract, frozen workloads, trace padding, and restoration safeguards. |
| [Demand-Driven Lifecycle](demand-driven-lifecycle.md) | Planned idle negotiation, incoming relay policy, compatibility, and battery acceptance gates. |
| [Activated Runner Trials](demand-lifecycle-activated-runner.md) | Native idle/wake evidence, controlled application builds, and remaining device/energy gates. |
| [Incoming Anchor Ownership](demand-lifecycle-incoming-anchor.md) | Native reservation ownership, TCP preference, compatibility, and staged activation. |
| [Incoming Relay Recovery](demand-lifecycle-relay-recovery.md) | Bounded repair independent of application traffic and ordinary discovery. |
| [Idle Maintenance](demand-lifecycle-idle-maintenance.md) | Sparse native ping, surplus infrastructure retirement, and shared-work protection. |
| [Quiet Relay Validation](relay-idle-validation.md) | Bounded public reservation test, raw idle traffic, and fresh incoming-circuit checks. |
| [Dormant Path Retirement](demand-lifecycle-retirement.md) | Datagram cleanup ownership, demand/work fences, and staged activation. |
| [Dormant Control Work](demand-lifecycle-control-work.md) | Native wire ownership, codec-I/O fences, protected stream retirement, and dormant metadata. |
| [Maintenance Drain](demand-lifecycle-drain.md) | Stop self-renewing probes before idle negotiation without claiming peer quiet permission. |
| [Feature Matrix](feature-matrix.md) | Current implementation status. |
| [NixOS Module Design](nixos-module.md) | Modes, secret lifecycle, service contract, and tests. |
| [Pairing Implementation](pairing.md) | Code PAKE, approval, durability, and proof commands. |
| [Membership Convergence](membership.md) | Ownerless ledger, authorization, merge rules, paging, persistence, and routes. |
| [Cooperative Checkpoints](membership-checkpoints.md) | Active-only protocol, authentication, fork selection, resync, and retention bounds. |
| [Linux Checkpoint Migration](checkpoint-migration.md) | Signed migration seed, hostname preservation, ownership, and remaining integration. |
| [Membership Checkpoint Acceptance](membership-checkpoint-acceptance.md) | In-progress compaction, accepted fork tradeoffs, and remaining verification. |
| [DNS Architecture](dns.md) | Authenticated naming, resolver protocol, limits, and NixOS lifecycle. |
| [Testing](testing.md) | Unit, Nix, namespace, and two-host tests. |
| [Network Debugging](network-debugging.md) | Artifact capture and failure triage. |
| [LAN-First Routing Audit](lan-first-routing-audit.md) | Discovery, promotion, selection gaps, and verification contract. |
| [Physical Network Policy](physical-network-policy.md) | OS-directed selection, validated failover/failback, discovery restrictions, and open acceptance checks. |
| [Linux Overlay Ownership](linux-overlay-ownership.md) | Kernel-scoped cross-instance exclusion, marker lifecycle, and source validation. |
| [Public Bootstrap Smoke](public-bootstrap-smoke.md) | Recorded public reachability evidence. |

## Development Shell

```sh
nix develop
```

## Fast Check

```sh
nix run .#check-fast
```

## Full Local Check

```sh
nix run .#check-operational
```

Use `nix flake check` when you want every exported check.

Some checks need Linux namespace and TUN privileges.

See [Testing](testing.md) before treating skipped namespace checks as failures.
