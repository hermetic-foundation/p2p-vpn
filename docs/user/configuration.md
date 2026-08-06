# Configuration

Use `init-config` to generate full configs.

Manual JSON can be much smaller.

Most sections have defaults.

## Required Fields

| Field | Required | Meaning |
| --- | --- | --- |
| `network.name` | yes | Overlay name. Peers must match. |
| `network.local_peer` | yes | Local libp2p peer ID. |
| `network.private_key` | yes | Base64 local identity key. |
| `peers[].id` | for VPN traffic | Authorized remote overlay peer. |

Everything else can be omitted for the default profile.

## Defaulted Fields

| Field | Default |
| --- | --- |
| `interface.name` | `hs0` |
| `interface.mtu` | `1280` |
| `peers[].name` | unset |
| `peers[].addresses` | empty |
| `peers[].routes` | empty |
| `network.routes` | empty |
| `network.listen_addresses` | empty |
| `network.external_addresses` | empty |
| `network.bootstrap_peers` | empty |
| `network.discovery` | enabled defaults |
| `network.relay` | disabled relay server, no reservations |
| `network.packet_plane` | no owned datagram listeners |
| `queue` | built-in bounded queue defaults |
| `resources` | built-in connection and stream limits |

## Minimal Shapes

### Identity And Membership

This authorizes one remote peer.

It relies on discovery for reachability:

```json
{
  "network": {
    "name": "lab",
    "local_peer": "LOCAL_PEER_ID",
    "private_key": "BASE64_PRIVATE_KEY"
  },
  "peers": [
    { "id": "REMOTE_PEER_ID" }
  ]
}
```

### Stable Overlay IPs

Add route ownership for human-chosen VPN IPs:

```json
{
  "network": {
    "name": "lab",
    "local_peer": "LOCAL_PEER_ID",
    "private_key": "BASE64_PRIVATE_KEY",
    "routes": [{ "prefix": "10.44.0.1/32" }]
  },
  "peers": [
    {
      "id": "REMOTE_PEER_ID",
      "routes": [{ "prefix": "10.44.0.2/32" }]
    }
  ]
}
```

This is optional.

Without it, `status` shows built-in IPs derived from peer IDs.

### Explicit Dial Address

Add `addresses` when discovery cannot find the peer:

```json
{
  "id": "REMOTE_PEER_ID",
  "addresses": ["/ip4/REMOTE_IP/tcp/4001/p2p/REMOTE_PEER_ID"]
}
```

## Peer Entries

Each peer entry grants overlay membership to one peer ID.

Addresses and routes are optional.

| Field | Meaning |
| --- | --- |
| `id` | Remote peer ID. Required. |
| `name` | Optional label. |
| `addresses` | Optional direct or relayed libp2p multiaddrs. |
| `routes` | Optional prefixes this peer may originate. |

Use peer IDs as the core trust boundary.

Use addresses only as reachability hints.

## Route Rules

Route ownership is static and exclusive.

The daemon rejects overlapping prefixes owned by different peers.

| Route Location | Owner |
| --- | --- |
| `network.routes[]` | Local node. |
| `peers[].routes[]` | That peer. |
| Built-in IPv4 host route | Derived from peer ID. |
| Built-in IPv6 host route | Derived from peer ID. |

You only need explicit route entries for stable, chosen IPs.

Without them, peers still get built-in host routes.

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
