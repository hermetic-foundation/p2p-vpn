# Testing

Use Nix commands when possible.

They provide the expected Rust toolchain and native dependencies.

## Fast Loop

```sh
nix run .#check-fast
```

## Rust Tests

```sh
nix develop -c cargo test
```

Focused examples:

```sh
nix develop -c cargo test queue::tests
nix develop -c cargo test route::tests
nix develop -c cargo test packet_plane
```

## Format

```sh
nix develop -c cargo fmt -- --check
```

## Clippy

```sh
nix develop -c cargo clippy --all-targets -- -D warnings
```

## Nix Checks

```sh
nix flake check
```

## NixOS VM Mesh

Run the two-node VM mesh check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-mesh
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Minimal config | Peer IDs plus route ownership are enough. |
| Discovery | No explicit peer dial addresses are configured. |
| TUN setup | Both nodes create default `pv0`. |
| Data plane | Bidirectional ping crosses the overlay. |
| Packet plane | A direct LAN packet-plane session is negotiated. |

## NixOS VM Forced Relay

Run the three-node forced-relay check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-forced-relay
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Topology | Data nodes cannot reach each other directly. |
| Relay | Both data nodes can reach the relay. |
| Discovery | Public discovery is disabled for isolation. |
| Minimal config | `vpnIp` plus peer IDs carry overlay IPs. |
| Data plane | Bidirectional ping crosses `pv0`. |
| Path selection | Data nodes select `circuit_relay`. |

## Namespace E2E

These tests require Linux namespace and TUN support.

Run preflight first:

```sh
nix run .#namespace-preflight
```

Run the full ignored namespace suite:

```sh
nix run .#tun-e2e -- -- --ignored --nocapture
```

Run a focused relay promotion case:

```sh
nix run .#tun-e2e -- \
  tun_namespace_relay_overlay_promotes_to_direct_path \
  -- --ignored --exact --nocapture
```

## Preserve Artifacts

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
nix run .#tun-e2e -- -- --ignored --nocapture
```

The test prints the artifact directory.

It includes logs, generated configs, replay commands, and daemon snapshots.

## Recorded Namespace Smoke

On 2026-08-04, the ignored namespace suite passed:

```text
7 passed; 0 failed; finished in 18.54s
```

Coverage:

- Direct static-peer overlay ping.
- mDNS-discovered packet forwarding.
- DHT/bootstrap-discovered packet forwarding.
- Circuit-relay fallback packet forwarding.
- Signed-invite onboarding over relay.
- Relay-to-direct promotion with DCUtR.
- Owned QUIC packet-plane forwarding.

## Two-Host LAN Evidence

Artifacts from the 2026-08-05 LAN proof:

```text
/tmp/p2p-vpn-lan-two-host-20260806T003426Z/direct-run-builtin
/tmp/p2p-vpn-lan-two-host-20260806T003426Z/forced-relay/run-firewalled-wide
```

Results:

| Scenario | Result |
| --- | --- |
| Direct LAN | Bidirectional ping passed. |
| Forced relay | Bidirectional `5/5` ping passed. |
| Forced relay path | Data peer selected `circuit_relay`. |
