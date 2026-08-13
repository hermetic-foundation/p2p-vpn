# User Documentation

Use these docs when you want to run `p2p-vpn`.

## Contents

| Document | Use It For |
| --- | --- |
| [Quick Start](quick-start.md) | Start a small JSON-managed overlay. |
| [Configuration](configuration.md) | Understand required config fields and common options. |
| [NixOS Module](nixos.md) | Start from one native Nix option and run managed instances. |
| [Pairing](pairing.md) | Authorize a peer and generate native Nix or JSON output. |
| [Operations](operations.md) | Inspect a daemon, health-check it, and stop it. |
| [Public libp2p/IPFS](public-libp2p.md) | Use public bootstrap and relay infrastructure safely. |

## Minimum Requirements

| Requirement | Why |
| --- | --- |
| Linux | The daemon uses a TUN interface. |
| `/dev/net/tun` | Required for packet forwarding. |
| `CAP_NET_ADMIN` or root | Required to create and configure TUN. |
| Nix flakes | Recommended build and run path for this repo. |
| Local identity | Generated automatically by the NixOS module. |
| Peer authorization | Added through pairing or explicit peer settings. |

## NixOS First Step

```nix
{
  services.p2p-vpn.instances.lab.enable = true;
}
```

Continue with [NixOS Module](nixos.md).

## JSON First Command

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

Use [Quick Start](quick-start.md) for the full JSON two-node flow.
