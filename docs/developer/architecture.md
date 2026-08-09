# Architecture

This document describes what exists today.

It avoids planned behavior unless a section marks it as a gap.

## High-Level Shape

```text
TUN interface
  -> bounded per-peer queues
  -> path selector
  -> packet protocol
  -> libp2p transport, relay, or owned packet plane
```

## Runtime Surfaces

| Surface | Protocol | Role |
| --- | --- | --- |
| Control | `/p2p-vpn/control/1` | Capability exchange and validation. |
| Packet | `/p2p-vpn/packet/1` | Framed packet forwarding. |
| Service | `/p2p-vpn/service/1` | Peer status and live views. |
| Daemon socket | local Unix socket | Operator inspection and shutdown. |

## Identity

Each node has a libp2p identity key.

The peer ID is both a transport identity and an overlay member key.

## Membership

Membership sources are explicit:

| Source | Use |
| --- | --- |
| Local config `peers[]` | Static overlay membership. |
| `membership_key` | Shared overlay proof. |
| `member_records` | Signed grants and revocations. |
| Public bootstrap peers | Reachability only. |
| Relay peers | Reachability only. |

## Routing

Routes are statically authorized.

Runtime route advertisements are claims, not dynamic routing authority.

| Check | Behavior |
| --- | --- |
| Overlapping owners | Rejected during route compilation. |
| Unauthorized advertised prefix | Rejected in control validation. |
| Unauthorized source address | Packet dropped. |
| Wrong local destination | Packet dropped. |

## Data Paths

| Path | State |
| --- | --- |
| Owned UDP packet plane | Implemented. |
| Owned Quinn QUIC DATAGRAM packet plane | Implemented. |
| libp2p QUIC stream fallback | Implemented. |
| libp2p TCP stream fallback | Implemented. |
| libp2p circuit relay stream fallback | Implemented. |
| Native libp2p QUIC DATAGRAM | Blocked by dependency surface. |

## Queueing

Queues are per peer.

They are bounded by packet count, bytes, and age.

| Policy | Behavior |
| --- | --- |
| Queue full | Drop. |
| Packet too old | Drop. |
| No supported path | Retain only inside queue limits. |
| Stream fallback busy | Keep same flow shard queued. |

## Path Selection

The path manager scores available paths.

Direct datagram paths are preferred when a compatible packet-plane session
exists.

| Path Kind | Relative Preference |
| --- | --- |
| Direct UDP datagram | highest when healthy |
| Direct QUIC datagram | highest when healthy |
| Direct QUIC stream | stream fallback |
| Direct TCP stream | stream fallback |
| Circuit relay | fallback |

## Candidate Hygiene

Path discovery advertises underlay addresses only.

Configured overlay prefixes are filtered from:

| Candidate Source | Filtered Values |
| --- | --- |
| libp2p listener candidates | Concrete and wildcard-expanded VPN addresses. |
| UDP packet-plane candidates | Observed or configured VPN endpoints. |
| QUIC packet-plane candidates | Observed or configured VPN endpoints. |

This prevents recursive routing after a peer moves networks.

The daemon must rediscover LAN, relay, or public paths instead.

Recovery tries direct underlay candidates before relayed paths.

This keeps LAN return and LAN-first rediscovery from being masked by an older
relay address.

## Relay Behavior

Circuit relay is a fallback path.

It also supports DCUtR setup when the topology and relay allow it.

Relay peers are not VPN members unless they also appear in `peers[]`.

## Public Discovery

Public IPFS/libp2p routing is default reachability infrastructure.

Provider results are dialed only when they match configured overlay peers.

Bootstrap peers are runtime defaults for the public DHT profile.

They are not VPN members and are not serialized into minimal configs.

Configured peer addresses are optional hints.

They must not be required for normal route convergence or network movement.

## Observability

The daemon exposes text and JSON views:

| View | Command |
| --- | --- |
| Metrics | `daemon-status` |
| State | `daemon-state` |
| Peers | `daemon-peers` |
| Routes | `daemon-routes` |
| Paths | `daemon-paths` |
| MTU | `daemon-mtu` |
| Capabilities | `daemon-capabilities` |
