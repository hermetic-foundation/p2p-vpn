# User Documentation

Use these docs when you want to run `p2p-vpn`.

## Contents

| Document | Use It For |
| --- | --- |
| [Quick Start](quick-start.md) | Build two configs and start a small overlay. |
| [Configuration](configuration.md) | Understand required config fields and common options. |
| [NixOS Module](nixos.md) | Run p2p-vpn as managed NixOS services. |
| [Operations](operations.md) | Inspect a daemon, health-check it, and stop it. |
| [Public libp2p/IPFS](public-libp2p.md) | Use public bootstrap and relay infrastructure safely. |

## Minimum Requirements

| Requirement | Why |
| --- | --- |
| Linux | The daemon uses a TUN interface. |
| `/dev/net/tun` | Required for packet forwarding. |
| `CAP_NET_ADMIN` or root | Required to create and configure TUN. |
| Nix flakes | Recommended build and run path for this repo. |
| Peer multiaddrs | Needed when peers cannot discover each other. |

## First Command

```sh
nix run .# -- init-config --output p2p-vpn.json --force
```

Then edit the peer list, routes, and listen addresses.

Use [Quick Start](quick-start.md) for the full two-node flow.
