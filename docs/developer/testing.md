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
nix develop -c cargo test overlay
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

## NixOS Module Check

Run the module evaluation check:

```sh
nix build .#checks.x86_64-linux.nixos-module
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Service wiring | `ExecStart` uses the generated config path. |
| Secrets | `privateKeyFile` is injected at service start. |
| Minimal config | ID-only peer configs serialize as compact JSON. |
| Defaults | Discovery, relay, packet-plane, queue, and resources are omitted. |
| Auto relay | Typed policy writes `network.relay.auto` only when set. |
| Firewall | Configured TCP, UDP, and packet-plane ports are opened. |

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
| Relay proof | Data nodes record relay circuit usage. |
| Candidate hygiene | Overlay IPs are not advertised as underlay endpoints. |

## NixOS VM Network Move

Run the move-recovery check:

```sh
nix build .#checks.x86_64-linux.nixos-vm-network-move
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Initial LAN | mDNS discovers a direct path. |
| Move away | The moved node loses LAN reachability. |
| Relay fallback | `pv0` traffic recovers through relay. |
| No config change | The daemon keeps running during the move. |
| Return to LAN | The selected path promotes back to direct. |

The committed-source run on 2026-08-09 passed from `main`.

Observed path events:

| Event | Evidence |
| --- | --- |
| Direct start | Initial `pv0` traffic used a direct LAN path. |
| Relay fallback | `path_fell_back_to_relay` was emitted after movement. |
| Direct recovery | `path_promoted_to_direct` was emitted after LAN return. |

## Underlay Candidate Hygiene

Run the focused regression tests:

```sh
nix develop -c cargo test overlay
```

Coverage:

| Scenario | Assertion |
| --- | --- |
| Listener expansion | Configured VPN prefixes are not direct dial candidates. |
| Concrete listeners | Concrete VPN listeners are rejected as direct candidates. |
| Observed UDP endpoints | VPN endpoints are rejected as packet-plane candidates. |
| Configured packet endpoints | VPN endpoints are rejected before advertisement. |

This protects network-move recovery.

The daemon must find a new LAN, relay, or public path.

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
