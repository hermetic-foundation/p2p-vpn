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
| DNS | loopback UDP/TCP | Authoritative per-overlay names. |

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

Transport roles are independent from overlay authorization:

| Role | Bound | Promotion rule |
| --- | --- | --- |
| Membership probe | 30-second lifetime | Valid signed membership or pairing |
| Public routing | 32 peers | Valid signed membership only |
| Relay infrastructure | 64 candidates | Valid signed membership only |
| Overlay member | Membership history bound | Already authorized |

Kademlia protocol support permits routing transport only. Application handlers
still reject packets, routes, status, and membership pages from non-members.

Signed records form a delegated trust graph.

Connected members exchange bounded snapshot pages and persist learned history.

Accepted records automatically update transport peers, TUN routes, and discovery targets.

A matching network name never authorizes a peer.

See [Membership Convergence](membership.md) for merge and persistence invariants.

## DNS

DNS is derived from local configuration and effective signed membership.

| Input | Output |
| --- | --- |
| Local or signed hostname | Canonical forward name |
| Derived or explicit host address | `A` and `AAAA` records |
| Preferred peer name | `PTR` record |
| Ambiguous hostname | No address; explicit control-plane conflict |
| Expired or revoked grant | No effective DNS records |

The responder binds loopback, is authoritative only, and never recurses.

NixOS attaches each zone and search domain to its TUN link through
`systemd-resolved`.

See [DNS Architecture](dns.md) for protocol limits and lifecycle details.

## Pairing

Pairing is the interactive onboarding flow.

It must not replace explicit membership checks.

| Piece | Rule |
| --- | --- |
| Default UX | `pair open` and `pair join CODE` through running daemons. |
| Pairing code | 80-bit one-time SPAKE2 password. |
| DHT locator | Network-bound hash that does not expose the code. |
| Inviter identity | Signed by inviter private key. |
| Joiner identity | Signed and matched to the libp2p transport peer. |
| Approval | Required through the inviter's local control socket. |
| Public bootstrap | Discovery hint only. |
| Relay paths | Discovery and reachability only. |
| Membership result | Signed records for both members. |
| Optional shared key | Transferred inside the authenticated response. |
| Durable state | Encrypted, identity-bound, and restart-safe. |
| Offline fallback | Signed `p2pvpn:` offer and `pair accept`. |

Online code messages use `/p2p-vpn/pairing-code/1`.

The exchange is `Hello`, `Challenge`, `Submit`, then approval polling.

SPAKE2 derives separate offer-encryption and request-confirmation keys.

The transcript binds:

| Binding | Purpose |
| --- | --- | --- |
| Network name | Prevent cross-overlay pairing. |
| Inviter and joiner peer IDs | Prevent identity substitution. |
| Authenticated connections | Match signatures to transport identities. |
| Signed encrypted offer | Bind expiry and one-time rendezvous token. |
| Requested authority | Bind address and route requests. |

Discovery is LAN first:

| Stage | Source |
| --- | --- |
| LAN | mDNS candidates. |
| Public | Kademlia provider record for the code locator. |
| Relay | Configured or discovered circuit paths. |

Provider publication repeats with bounded exponential backoff.

This handles DHT bootstrap convergence after the pairing window opens.

Approval produces a durable two-phase enrollment:

```text
persist Prepared
  -> apply forwarding, membership, and TUN routes
  -> persist Applied
  -> render native Nix
  -> acknowledge into compact receipt
```

Startup reconciles both prepared and applied enrollments idempotently.

Generated Nix contains signed records and managed secret paths.

It contains no private key, membership-key contents, JSON, or static peer bypass.

See [Pairing Implementation](pairing.md) for message fields, limits, and tests.

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

QUIC and relay stream packets and health probes are pinned to the selected
connection. A stale connection failure cannot demote a healthy replacement.

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
| Authenticated libp2p hints | Concrete listeners and configured external addresses; VPN addresses are removed. |
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

| Relay State | Policy |
| --- | --- |
| Reservation | Keep ready for inbound fallback and DCUtR. |
| Peer circuit | Open only while no usable direct path exists. |
| Alternate relay address | Try serially for one peer and relay pair. |

Relay peers are not VPN members unless they also appear in `peers[]`.

## Public Discovery

Public IPFS/libp2p routing is default reachability infrastructure.

Unknown connections are classified after Identify. Exact Kademlia protocol
matches remain connected as bounded routing-only peers.

Provider results are dialed only when they match configured overlay peers.

Bootstrap peers are runtime defaults for the public DHT profile.

They are not VPN members and are not serialized into minimal configs.

### Query Scheduling

Public DHT work is serialized per network instance.

| Work | Policy |
| --- | --- |
| Peer recovery | One signed address-record lookup at a time. |
| Recovery priority | Preempts routine maintenance. |
| Failed recovery | Exponential backoff from 30 seconds to 60 minutes. |
| Routine maintenance | One round-robin query every two minutes. |
| Relay discovery | One closest-peer query while relay capacity is needed. |
| Bootstrap refresh | Runs only below eight active routing peers. |
| Healthy paths | Cancel and suppress public recovery queries. |

The default pending outbound connection limit is `16` per instance.

Explicit resource configuration can override this safety limit.

Address scope follows discovery provenance:

| Address source | Private or unresolved DNS | Reason |
| --- | --- | --- |
| Static bootstrap, relay, or peer config | Accepted | The operator authorized the target. |
| mDNS | Accepted | Link-local discovery corroborates LAN reachability. |
| Established endpoint | Accepted | A successful secure connection proves reachability. |
| Admitted member record or Identify | Accepted | Overlay authorization bounds the sender. |
| Public Kademlia or unadmitted Identify | Rejected | Only globally routable literal IPs have verified scope. |

Public records may contain RFC1918, carrier-grade NAT, loopback, link-local,
ULA, documentation, or DNS addresses. They never become dial or relay candidates
without an operator-authorized, locally corroborated, or authenticated source.

Configured peer addresses are optional hints.

They must not be required for normal route convergence or network movement.

## Observability

The daemon exposes text and JSON views:

| View | Command |
| --- | --- |
| Effective network directory | `peers --instance NAME` |
| Metrics | `daemon-status` |
| State | `daemon-state` |
| Peers | `daemon-peers` |
| Routes | `daemon-routes` |
| Paths | `daemon-paths` |
| MTU | `daemon-mtu` |
| Capabilities | `daemon-capabilities` |
| DNS status | `dns status` |
| DNS records | `dns list` |
| DNS resolution | `dns resolve` |

The network directory uses the bounded `network-peers-v1` control request.

Its compact JSON response is schema-versioned before the CLI renders text or
passes structured JSON to callers.

Membership persistence and paging add counters for loads, saves, records, pages,
snapshot restarts, and failures.
