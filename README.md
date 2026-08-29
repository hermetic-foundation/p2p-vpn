# p2p-vpn

`p2p-vpn` is a Rust, libp2p-native mesh VPN for Linux, NixOS, and Android.

It uses libp2p for identity, encrypted transports, discovery, relay, AutoNAT,
and DCUtR. It forwards overlay IP packets through authenticated packet
protocols and bounded queues.

## Documentation

| Audience | Start Here | Purpose |
| --- | --- | --- |
| Users | [User Guide](docs/user/README.md) | Install, configure, run, and inspect a VPN node. |
| Developers | [Developer Guide](docs/developer/README.md) | Architecture, tests, and debugging. |

## Quick Links

- [Quick Start](docs/user/quick-start.md)
- [Android](docs/user/android.md)
- [NixOS Module](docs/user/nixos.md)
- [Pairing](docs/user/pairing.md)
- [Network Membership](docs/user/membership.md)
- [Overlay DNS](docs/user/dns.md)
- [Configuration](docs/user/configuration.md)
- [Operations](docs/user/operations.md)
- [Public libp2p/IPFS Reachability](docs/user/public-libp2p.md)
- [Architecture](docs/developer/architecture.md)
- [Feature Matrix](docs/developer/feature-matrix.md)
- [Testing](docs/developer/testing.md)
- [Network Debugging](docs/developer/network-debugging.md)

## Current Status

| Area | State |
| --- | --- |
| Minimal config | Operational in NixOS VM mesh tests without peer addresses. |
| NixOS module | Operational with typed service instances and sane defaults. |
| Android pair and connect | Operational in an API 35 emulator/Linux E2E; debug APK only. |
| Code pairing | Operational with LAN-first discovery, approval, relay fallback, and native Nix artifacts. |
| Network-wide membership | Operational with signed convergence, durable learned state, and derived routes. |
| Authenticated overlay DNS | Operational with split DNS, transitive names, and lifecycle tests. |
| Static LAN peers | Operational in VM and recorded two-host tests. |
| Network move recovery | Operational in VM tests with LAN-first rediscovery. |
| Circuit relay fallback | Operational in VM and recorded forced-relay tests. |
| Public IPFS/libp2p bootstrap | Partial; useful for reachability hints. |
| Public DCUtR proof | Still needs non-LAN host evidence. |
| Native libp2p QUIC datagrams | Blocked by the current libp2p surface. |

## Build

```sh
nix build
```

Build the Android debug APK on `x86_64-linux`:

```sh
nix build .#android-debug-apk
```

## Run Help

```sh
nix run .# -- --help
```

## Fast Verification

```sh
nix run .#check-fast
```
