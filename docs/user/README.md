# User Documentation

Use these docs when you want to run `p2p-vpn`.

## Contents

| Document | Use It For |
| --- | --- |
| [Quick Start](quick-start.md) | Start and code-pair a minimal NixOS overlay. |
| [Android](android.md) | Build, manage networks, pair, connect, and recover the Android app. |
| [Configuration](configuration.md) | Understand required config fields and common options. |
| [NixOS Module](nixos.md) | Start from one native Nix option and run managed instances. |
| [Pairing](pairing.md) | Pair by code, approve a peer, and install native Nix grants. |
| [Network Membership](membership.md) | Understand whole-overlay convergence, trust, routes, and recovery. |
| [Overlay DNS](dns.md) | Resolve authenticated members by short and canonical names. |
| [Operations](operations.md) | Inspect a daemon, health-check it, and stop it. |
| [Public libp2p/IPFS](public-libp2p.md) | Use public bootstrap and relay infrastructure safely. |

## Minimum Requirements

| Requirement | Why |
| --- | --- |
| Linux/NixOS or Android | Run the Linux daemon or supported Android APK. |
| `/dev/net/tun` | Required on Linux for packet forwarding. |
| `CAP_NET_ADMIN` or root | Required on Linux to configure TUN. |
| Android VPN approval | Required on Android to establish the TUN. |
| Nix flakes | Build, test, and package every supported target. |
| Local identity | Generated automatically by the NixOS module. |
| Peer authorization | Added through pairing or explicit peer settings. |

## NixOS First Step

```nix
{
  services.p2p-vpn.instances.lab.enable = true;
}
```

Continue with [NixOS Module](nixos.md).

The default pairing workflow transfers only a one-time human-readable code.

Pair once with an authorized member; signed membership then converges across the overlay.

Enable `dns.enable` to resolve converged members by hostname.

## Android First Step

```sh
nix run .#android-install
```

Continue with [Android](android.md).

## Linux JSON First Command

```sh
nix run .# -- init-config --output p2p-vpn.json --force
```

For ordinary JSON setups, do not add peer multiaddrs first.

Start with:

| Value | Configure It When |
| --- | --- |
| `network.private_key` | Always in user-owned JSON. |
| `network.vpn_ip` | You want a stable overlay IP. |
| `peers.<id>` | For static authorization instead of pairing. |
| `peers.<id>.vpn_ip` | You want the peer to have a stable overlay IP. |

Leave listen addresses, public relays, and peer multiaddrs unset unless you need
an override for a controlled network or test.

Use [Quick Start](quick-start.md) for NixOS code pairing and the JSON fallback.
