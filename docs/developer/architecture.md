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

## Pairing

Pairing is the interactive onboarding flow.

It must not replace explicit membership checks.

| Piece | Rule |
| --- | --- |
| Pairing URI | Short-lived `p2pvpn:` offer. |
| Rendezvous token | One-time pairing secret. |
| Inviter identity | Signed by inviter private key. |
| Public bootstrap | Discovery hint only. |
| Relay paths | Discovery and reachability only. |
| Membership result | Signed record preferred over shared key. |

The URI format is JSON encoded with base64url under `p2pvpn:`.

The URI must not include the local private key.

It should not include the membership key unless an explicit future mode requests
shared-secret onboarding.

Pairing exchange messages are signed:

| Message | Signed By | Required Binding |
| --- | --- | --- |
| Offer | Inviter | network, inviter peer, rendezvous token, expiry |
| Request | Joiner | offer network, inviter peer, rendezvous token |
| Response | Inviter | offer, joiner peer, membership grant |

Live requests use a compact offer proof.

| Field | Purpose |
| --- | --- |
| `offer_issued_at_unix_seconds` | Reconstruct inviter offer. |
| `offer_expires_at_unix_seconds` | Reconstruct inviter offer. |
| `offer_signature` | Bind request to the signed offer. |

Legacy embedded offers are still accepted.

Pairing transport uses libp2p request-response:

| Field | Value |
| --- | --- |
| Protocol | `/p2p-vpn/pairing/1` |
| Encoding | 2-byte length-prefixed JSON |
| Limit | 32 KiB per message |
| Stream class | Control-plane stream |

Daemon handling rules:

| Check | Behavior |
| --- | --- |
| Offer proof | Compact proof or legacy embedded offer. |
| Offer signer | Must match the local daemon identity. |
| Offer network | Must match the local daemon network. |
| Rendezvous token | Consumed after the first accepted response. |
| Membership key | Returned when the network uses shared-key membership. |
| Member record | Issued when the network uses record-based membership. |

A response is invalid without either:

| Grant | Meaning |
| --- | --- |
| `membership_key` | Shared-secret onboarding. |
| `member_records[]` | Signed membership grant for the joiner. |

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
| Direct libp2p QUIC stream | Connection-pinned packet stream. |
| libp2p TCP stream fallback | Compatibility request-response stream. |
| libp2p circuit relay stream fallback | Connection-pinned packet stream. |
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
| Direct QUIC datagram | highest when negotiated and healthy |
| Direct UDP datagram | preferred owned packet-plane fallback |
| Direct QUIC stream | connection-pinned stream fallback |
| Direct TCP stream | lower direct stream fallback |
| Circuit relay | connection-pinned public reachability fallback |

Capability advertisement follows local support:

| Local Support | Advertised Preferred Path |
| --- | --- |
| Owned QUIC packet plane or native QUIC datagrams | Direct QUIC datagram |
| Owned UDP packet plane only | Direct UDP datagram |
| Stream-only libp2p | Direct QUIC stream |

Stream fallback evidence is split by selected path:

| Metric | Meaning |
| --- | --- |
| `outbound_direct_quic_stream_fallback_packets` | Packets sent through selected direct QUIC stream fallback. |
| `outbound_direct_tcp_stream_fallback_packets` | Packets sent through selected direct TCP stream fallback. |
| `outbound_relay_stream_fallback_packets` | Packets sent through selected circuit relay stream fallback. |

Direct QUIC stream and circuit relay egress use the latest libp2p
`connection_id`.

TCP stream egress still uses the compatibility request-response stream path.

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
