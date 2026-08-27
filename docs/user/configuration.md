# Configuration

Use `init-config` to generate compact configs.

Manual JSON can be much smaller.

Most sections have defaults.

## Required Fields

| Field | Required | Meaning |
| --- | --- | --- |
| `network.name` | yes | Overlay name. Peers must match. |
| `network.private_key` | yes | Base64 local identity key. |
| `peers[].id` | for VPN traffic | Authorized remote overlay peer. |

Everything else can be omitted for the default profile.

`network.local_peer` is optional.

When omitted, p2p-vpn derives it from `network.private_key`.

Set it only to assert the key belongs to an expected peer ID.

## Defaulted Fields

| Field | Default |
| --- | --- |
| `interface.name` | `pv0` |
| `interface.mtu` | `1280` |
| `peers[].name` | unset |
| `peers[].addresses` | empty |
| `peers[].vpn_ip` | unset |
| `peers[].routes` | empty |
| `network.vpn_ip` | unset |
| `network.routes` | empty |
| `network.listen_addresses` | `/ip4/0.0.0.0/tcp/4001` |
| `network.external_addresses` | empty |
| `network.bootstrap_peers` | empty in JSON; public defaults at runtime |
| `network.dns` | disabled |
| `network.discovery` | public discovery defaults |
| `network.relay` | disabled relay server, no reservations |
| `network.packet_plane` | UDP packet plane listens on `0.0.0.0:0` |
| `queue` | `256` packets, `512 KiB`, `3000 ms` packet age |
| `resources` | built-in connection and stream limits |

## Minimal Shapes

### Identity And Membership

This authorizes one remote peer.

It relies on discovery for reachability:

```json
{
  "network": {
    "name": "lab",
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
    "private_key": "BASE64_PRIVATE_KEY",
    "vpn_ip": "10.44.0.1"
  },
  "peers": [
    {
      "id": "REMOTE_PEER_ID",
      "vpn_ip": "10.44.0.2"
    }
  ]
}
```

This is optional.

Without it, `status` shows built-in IPs derived from peer IDs.

`vpnIp` is accepted as a JSON alias.

Use `routes` only for prefixes or additional routed networks.

### Explicit Dial Address

Add `addresses` when discovery cannot find the peer:

```json
{
  "id": "REMOTE_PEER_ID",
  "addresses": ["/ip4/REMOTE_IP/tcp/4001/p2p/REMOTE_PEER_ID"]
}
```

## Peer Entries

Each peer entry statically grants overlay membership to one peer ID.

Addresses and routes are optional.

| Field | Meaning |
| --- | --- |
| `id` | Remote peer ID. Required. |
| `name` | Optional label. |
| `ip` | Optional direct IP. Uses TCP port `4001`. |
| `vpn_ip` | Optional stable overlay host IP. |
| `addresses` | Optional direct or relayed libp2p multiaddrs. |
| `routes` | Optional prefixes this peer may originate. |

Use peer IDs as the core trust boundary.

When DNS is enabled, `name` becomes that statically authorized peer's hostname.

Use `ip` for ordinary LAN peers on the default port.

Use `addresses` for custom ports, DNS, QUIC, or relay paths.

## Route Rules

Route ownership is explicit and exclusive.

The daemon rejects overlapping prefixes owned by different peers.

| Route Location | Owner |
| --- | --- |
| `network.routes[]` | Local node. |
| `peers[].routes[]` | That peer. |
| Built-in IPv4 host route | Derived from peer ID. |
| Built-in IPv6 host route | Derived from peer ID. |
| Signed member record route | The record subject. |

Use `vpn_ip` for stable, chosen host addresses.
Use explicit route entries for prefixes.

Without them, peers still get built-in host routes.

## Overlay DNS

Enable the loopback authoritative resolver:

```json
{
  "network": {
    "name": "lab",
    "private_key": "BASE64_PRIVATE_KEY",
    "dns": {
      "enabled": true,
      "hostname": "worker-1"
    }
  }
}
```

| Field | Default | Rule |
| --- | --- | --- |
| `enabled` | `false` | Keeps existing configurations unchanged |
| `hostname` | unset | Required when enabled |
| `listen` | `127.0.0.1:0` | Numeric loopback socket only |
| `ttl_seconds` | `30` | Range `1` through `300` |

See [Overlay DNS](dns.md) for split DNS, signed names, and JSON integration.

## Discovery

| Setting | Default | Use |
| --- | --- | --- |
| `mdns` | enabled | LAN peer discovery. |
| `kademlia` | enabled | Public DHT-backed provider discovery. |
| `kademlia_provider_advertisement` | enabled | Advertise this overlay peer. |
| `kademlia_protocol` | `/ipfs/kad/1.0.0` | Public libp2p/IPFS DHT. |
| `dcutr` | enabled | Hole punching support. |
| `autonat` | enabled | Reachability detection. |

## Public Bootstrap

Public IPFS/libp2p bootstrap is automatic by default.

It is not serialized into generated configs.

To write bootstrap peers explicitly:

```sh
nix run .# -- init-config \
  --output public.json \
  --ipfs-bootstrap-peers \
  --force
```

Public IPFS/libp2p peers are not VPN members.

They are bootstrap, relay, AutoNAT, and routing infrastructure only.

For private-only DHTs, set a private Kademlia protocol.

Then provide private bootstrap peers.

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

## Resource Limits

| Setting | Default | Meaning |
| --- | --- | --- |
| `resources.max_inbound_packets_per_peer_per_second` | `4096` | Packet data-plane rate limit. |
| `resources.max_pairing_requests_per_peer_per_second` | `4` | Live pairing request rate limit. |

The pairing limit is per libp2p peer.

It does not consume packet forwarding allowance.

## Packet Plane

| Setting | Meaning |
| --- | --- |
| `packet_plane.listen` | Owned UDP packet-plane bind addresses. |
| `packet_plane.external_endpoints` | Advertised UDP endpoints. |
| `packet_plane.quic_listen` | Owned Quinn QUIC DATAGRAM bind address. |
| `packet_plane.quic_external_endpoints` | Advertised QUIC endpoints. |
| `packet_plane.session_ttl_seconds` | Session lifetime. Default `600`. |

Omit `network.packet_plane` for automatic UDP packet-plane setup.

Use `"listen": []` only to force stream fallback.

Datagram forwarding needs compatible direct paths and negotiated sessions.

Default path preference:

| Available Path | Used As |
| --- | --- |
| QUIC datagram packet plane | Preferred packet path. |
| UDP datagram packet plane | Default minimal-config packet path. |
| Direct QUIC stream | First stream fallback, pinned to the selected connection. |
| Direct TCP stream | Lower direct stream fallback. |
| Circuit relay stream | Public fallback, pinned to the selected relay connection. |

## Membership Key

`network.membership_key` is optional.

When set, peers must present the same network-scoped membership tag.

| Field | Use |
| --- | --- |
| `membership_key` | Current overlay-wide shared secret. |
| `previous_membership_tags` | Temporary accept list during rotation. |
| `member_records` | Signed grants and revocations. |

The membership key narrows discovery and connection scope.

It does not authorize a peer without static membership or a valid signed record.

## Signed Membership State

`network.member_records` supplies declarative trust anchors and admission history.

The daemon learns additional valid records from admitted peers.

Persist learned history with:

```sh
p2p-vpn up \
  --config p2p-vpn.json \
  --membership-state /var/lib/p2p-vpn/membership-state.json
```

The NixOS module supplies this option automatically.

See [Network Membership](membership.md) for convergence, revocation, and limits.

## Validate Before Running

```sh
nix run .# -- status --config p2p-vpn.json
nix run .# -- routes --config p2p-vpn.json
nix run .# -- mtu --config p2p-vpn.json
```
