# User Documentation

Use these docs when you want to run `p2p-vpn`.

## Contents

| Document | Use It For |
| --- | --- |
| [Quick Start](quick-start.md) | Build two configs and start a small overlay. |
| [Configuration](configuration.md) | Understand required config fields and common options. |
| [Pairing](pairing.md) | Generate and validate short-lived onboarding URIs. |
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
| Local identity key | Determines this node's libp2p peer ID. |
| Remote peer IDs | Authorize who may join the overlay. |

## First Command

```sh
nix run .# -- init-config --output p2p-vpn.json --force
```

For ordinary setups, do not add peer multiaddrs first.

Start with:

| Value | Configure It When |
| --- | --- |
| `network.private_key` | Always. |
| `network.vpn_ip` | You want a stable overlay IP. |
| `peers.<id>` | Always for each trusted peer. |
| `peers.<id>.vpn_ip` | You want the peer to have a stable overlay IP. |

Leave listen addresses, public relays, and peer multiaddrs unset unless you need
an override for a controlled network or test.

Use [Quick Start](quick-start.md) for the full two-node flow.
