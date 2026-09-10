# Automatic QUIC Packet Transport

## Status

Implementation pending. Baseline: `e483775d`.
This plan does not certify QUIC-first minimal configuration or physical recovery.

## Baseline Findings

| Surface | Current behavior | Required change |
| --- | --- | --- |
| Rust defaults | UDP binds `0.0.0.0:0`; QUIC listener list is empty | Automatically enable QUIC alongside compatible fallback |
| Path ranking | QUIC datagram 100, UDP 95, QUIC stream 75, TCP 40, relay 30 | Preserve datagram preference and health gating |
| NixOS | Assigns UDP packet ports from 51820; only explicit QUIC listeners open firewall ports | Allocate collision-checked QUIC listeners and firewall rules automatically |
| Discovery | Observed endpoint updates/withdrawals already accept both packet backends | Verify wildcard listeners, address changes and reachable candidate validation |
| Capabilities | QUIC certificate and endpoint candidates gate owned-QUIC support | Advertise only usable capability state; preserve old-peer fallback |
| Android | Profiles use shared config defaults; saved profile is encrypted | Apply defaults without rewriting identity or pairing state |

Existing source anchors:

- `src/config.rs`: `PacketPlaneConfig`, packet-listener parsing and defaults.
- `src/lib.rs`: `PathKind::default_score`.
- `src/runtime/runner.rs`: startup capabilities, observed endpoints and withdrawal.
- `nix/nixos-module.nix`: effective packet listeners, rendering and firewall ports.
- `crates/p2p-vpn-android/src/lib.rs`: shared runtime and explicit test overrides.

## Compatibility Rules

| Input | Intended behavior |
| --- | --- |
| Minimal config | Automatic QUIC datagrams with UDP and streams retained |
| Explicit `quic_listen: []` | QUIC packet listener remains disabled |
| Explicit nonempty QUIC listeners | Preserve requested bind addresses |
| Existing `listen: []`, no QUIC override | Preserve documented stream-only intent |
| UDP-only older peer | Negotiate UDP or streams without requiring peer upgrade |
| Persisted Android identity/profile | Preserve membership, addresses and enabled state |

Distinguish omission from explicit disable during deserialization and serialization.
Do not omit an explicit disable when rendering or round-tripping configuration.

## Implementation Sequence

1. Add configuration regression tests for omission, explicit disable, overrides and round trips.
2. Implement automatic listeners; retain existing fallback and stream-only semantics.
3. Integrate NixOS port allocation, collision checks, configuration and firewall tests.
4. Verify candidate publication, authenticated negotiation, withdrawal and recovery.
5. Exercise packet selection and fallbacks with minimal configurations in local fixtures.
6. Build Android ARM64 and validate new/existing-profile behavior.
7. Obtain deployment authorization, then collect physical payload and recovery evidence.
8. Reconcile user documentation and perform the full acceptance audit.

## Evidence Matrix

| Scenario | Required observation |
| --- | --- |
| Minimal direct peers | Successful traffic plus QUIC datagram payload-counter growth |
| Older UDP-only peer | Successful authorized UDP payload traffic |
| Explicit stream-only override | No packet listeners; working stream payload traffic |
| Blocked QUIC | Working fallback with bounded retries and queue memory |
| QUIC restored | Autonomous healthy-path promotion and QUIC payload traffic |
| Changed endpoint/network | Old session/path retired; new validated path carries traffic |
| LAN return | LAN discovery and local path recovery without manual rescue |
| Multiple networks | Independent identities, listeners, authorization and packet delivery |
| Android update | Profile/identity preserved; actual preferred-path payload traffic |

## Risks And Limits

- An observed public IP plus a local port is not proof of a usable NAT mapping.
- Libp2p QUIC connections are distinct from owned Quinn packet sessions.
- Established path counts alone do not prove packet selection or delivery.
- Default QUIC startup must not make usable fallback fail unnecessarily.
- Extra listeners must not collide across NixOS instances or explicit overrides.
- Existing emulator and transport results retain their original revisions and limits.
- The Pixel process-termination restart finding remains a separate issue.

## Verification And Resources

- Run affected Rust unit/integration tests, formatting and required Clippy groups.
- Inspect formal models; check NixOS rendering/firewall and cached source parity.
- Run Android ARM64 compilation and affected Java/lifecycle checks.
- Freeze bounded scenario windows before measurement; preserve failed evidence.
- Use cached tools, at most two Cargo jobs and downloads capped at 10 Mbps.
- Keep all `/tmp/p2p-vpn-*` below 10 GiB with full storage accounting.
- No physical deployment, personal-flake mutation or remote-host access without authorization.
- Publish atomic verified Conventional Commits through Jujutsu to `main`.
