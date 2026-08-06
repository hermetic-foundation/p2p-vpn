# Configuration

Use `init-config` to generate configs.

Manual JSON edits are supported, but generated configs include safe defaults.

## Required Fields

| Field | Required | Meaning |
| --- | --- | --- |
| `network.name` | yes | Overlay name. Peers must match. |
| `network.local_peer` | yes | Local libp2p peer ID. |
| `network.private_key` | yes | Base64 local identity key. |
| `network.listen_addresses` | usually | Libp2p listen multiaddrs. |
| `interface.name` | yes | TUN interface name. |
| `interface.mtu` | yes | TUN MTU. Default is `1280`. |
| `peers[]` | for VPN traffic | Authorized remote overlay peers. |

## Peer Entries

Each peer entry grants overlay membership to one peer ID.

It also defines how to dial that peer and which routes it owns.

| Field | Meaning |
| --- | --- |
| `id` | Remote peer ID. |
| `name` | Optional label. |
| `addresses` | Direct or relayed libp2p multiaddrs. |
| `routes` | Prefixes this peer may originate. |

## Route Rules

Route ownership is static and exclusive.

The daemon rejects overlapping prefixes owned by different peers.

| Route Location | Owner |
| --- | --- |
| `network.routes[]` | Local node. |
| `peers[].routes[]` | That peer. |
| Built-in IPv4 host route | Derived from peer ID. |
| Built-in IPv6 host route | Derived from peer ID. |

## Discovery

| Setting | Default | Use |
| --- | --- | --- |
| `mdns` | enabled | LAN peer discovery. |
| `kademlia` | enabled | Overlay provider discovery. |
| `kademlia_provider_advertisement` | enabled | Advertise this overlay peer. |
| `dcutr` | enabled | Hole punching support. |
| `autonat` | enabled | Reachability detection. |

## Public IPFS Profile

Use this only for reachability assistance:

```sh
nix run .# -- init-config \
  --output public.json \
  --public-ipfs-profile \
  --force
```

Public IPFS/libp2p peers are not VPN members.

They are bootstrap, relay, AutoNAT, and routing infrastructure only.

## Relay Settings

| Setting | Meaning |
| --- | --- |
| `relay.server` | Enables local circuit-relay service. |
| `relay.reservations[]` | Relay addresses to reserve. |
| `relay.auto.max_candidates` | Retained automatic relay candidates. |
| `relay.auto.max_reservations` | Active automatic reservations. |

Use `--relay-peer` to add relay infrastructure from the CLI:

```sh
--relay-peer RELAY_PEER_ID=/ip4/RELAY_IP/tcp/4001/p2p/RELAY_PEER_ID
```

This also creates a matching reservation address.

## Packet Plane

| Setting | Meaning |
| --- | --- |
| `packet_plane.listen` | Owned UDP packet-plane bind addresses. |
| `packet_plane.external_endpoints` | Advertised UDP endpoints. |
| `packet_plane.quic_listen` | Owned Quinn QUIC DATAGRAM bind address. |
| `packet_plane.quic_external_endpoints` | Advertised QUIC endpoints. |
| `packet_plane.session_ttl_seconds` | Session lifetime. Default `600`. |

Stream fallback works without packet-plane listeners.

Datagram forwarding needs compatible direct paths and negotiated sessions.

## Membership Key

`network.membership_key` is optional.

When set, peers must present the same network-scoped membership tag.

| Field | Use |
| --- | --- |
| `membership_key` | Current overlay-wide shared secret. |
| `previous_membership_tags` | Temporary accept list during rotation. |
| `member_records` | Signed grants and revocations. |

## Validate Before Running

```sh
nix run .# -- status --config p2p-vpn.json
nix run .# -- routes --config p2p-vpn.json
nix run .# -- mtu --config p2p-vpn.json
```
