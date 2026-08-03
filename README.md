# p2p-vpn

Rust foundation for a Hyprspace-inspired peer-to-peer mesh VPN.

The v2 design keeps libp2p as the identity, discovery, NAT traversal, and relay
substrate while making the VPN data plane packet-oriented:

- direct QUIC streams as the current preferred operational packet transport
- native QUIC datagrams as the intended promotion path when the libp2p data
  plane exposes a usable datagram handle
- framed libp2p streams as TCP and relay fallback
- bounded per-peer packet queues with intentional drop policy
- separate control, packet, and service protocol surfaces
- explicit route ownership and source-address authorization
- path scoring and promotion from relay to direct paths
- first-class metrics for packet, queue, route, relay, AutoNAT, DCUtR, and path behavior

Discovery is overlay-scoped. Configure bootstrap peers and relay-capable peers
that intentionally participate in the VPN; do not rely on the public IPFS DHT
as the membership or routing authority. When Kademlia is enabled, nodes announce
and query an overlay provider key derived from the configured network name,
`/p2p-vpn/<network>/providers/1`. Provider results are only dialed when the peer
ID is already present in the configured peer list, so DHT discovery is a
reachability hint rather than route or membership authorization. Public libp2p
relays can be useful for experiments only when they support the needed relay
reservations and are acceptable for the deployment's trust and availability
requirements.

Runtime connections are membership-filtered before they are used for control or
packet forwarding. The local peer, configured VPN peers, configured bootstrap
peers, and relay peers named in reservation addresses are allowed to remain
connected; other peers are disconnected and counted as unauthorized connection
drops. This connection-level membership does not grant routing authority:
packet source ownership is still checked separately against configured routes.

Discovery toggles control runtime behaviour construction. Disabling mDNS or
DCUtR prevents the corresponding libp2p behaviour from being installed in the
swarm; disabling AutoNAT prevents libp2p reachability probing; disabling
Kademlia prevents overlay provider advertisement and lookup.

The control plane exposes `/p2p-vpn/control/1` over a bounded reliable
request-response stream. Peers exchange capabilities when a configured transport
peer connects, including wire version, packet protocol, effective MTU, preferred
path, and whether native QUIC datagrams are currently supported. The current
local capability advertises direct QUIC streams as preferred and native QUIC
datagrams as unsupported, so peers do not negotiate an unreliable data path
before one is implemented. Outbound queue draining respects the peer's
advertised effective MTU and drops oversized packets before sending them to the
packet stream fallback. Capability requests from unconfigured peers are
rejected, and configured peers are only accepted when they advertise a
compatible wire version, packet protocol, packet header length, known preferred
path, coherent datagram support, and non-zero effective MTU.

The current stream data plane uses a fixed binary header followed by the raw IP
packet payload. The header includes a non-zero packet session id derived from
the local peer identity plus a per-session packet sequence number. It is
intentionally reusable over libp2p streams now and QUIC datagrams later.
Inbound packet acceptance keeps a 64-packet replay window per configured
peer/session pair, so duplicate frames and frames older than the current window
are dropped before they can be written to TUN.

The libp2p runtime exposes `/p2p-vpn/packet/1` for framed packet exchange over
request-response streams. Inbound packet handling checks the configured peer
allowlist, source-route ownership, and local overlay destination ownership
before writing to TUN. Outbound packets read from the local TUN interface must
also use the local peer's built-in overlay source address before they are queued
for a remote peer. Accepted packet requests receive a compact success response;
rejected requests return a compact rejection reason such as oversized packet,
replay, unauthorized peer, unauthorized source, unauthorized destination,
unexpected payload, or malformed packet instead of relying on request timeout.

Route ownership is static and exclusive. Each configured peer may own its
built-in host routes plus any advertised prefixes, but route compilation rejects
overlapping prefixes owned by different peers, including more-specific prefixes
that would hijack another peer's aggregate route.

Outbound packet draining is path-aware. The runtime records direct TCP, direct
QUIC stream, and circuit-relay paths from libp2p connection events, marks paths
unhealthy when their connections close, and only drains a peer's bounded queue
while that peer has a healthy path that the negotiated packet transport can use.
Until native datagram sending is implemented, datagram-only paths do not release
packets into the stream fallback. Packets for disconnected peers remain bounded
by the per-peer queue limits instead of expanding into unbounded stream requests.
Queued packets also have a configurable age limit; stale packets are dropped on
a runtime expiry tick rather than sent after a long outage. A configured age of
zero is treated as a one millisecond effective age.

Configured bootstrap and peer addresses are redialed periodically when they are
not already connected. This keeps the overlay trying to recover after transient
network loss instead of depending on a one-shot startup dial or a future
discovery event. Addresses learned from mDNS or identify are retained only for
configured peers and are included in the same periodic redial loop; mDNS-expired
addresses are removed from that transient address book. Redial attempts,
connected-peer skips, and failures are exposed in the runtime metrics output.
Discovered addresses that include an explicit `/p2p/<peer>` target are rejected
when that target does not match the configured peer being learned, including
relayed target addresses after `/p2p-circuit`.
When Kademlia is enabled, the runtime also periodically refreshes the overlay
provider advertisement, reruns the provider lookup, and retries Kademlia
bootstrap. That lets a long-running node find configured peers that join the DHT
after startup rather than relying on the initial one-shot provider query.
Observed external address candidates reported by libp2p identify are passed to
AutoNAT when that behaviour is enabled, and configured bootstrap/peer addresses
are registered as AutoNAT probe servers. AutoNAT probes candidate reachability
through connected or configured probe peers, so later identify exchanges and
Kademlia announcements can carry a tested public address alongside any
operator-configured external addresses.
Candidate, scheduled-probe, confirmed, and expired external address events are
exposed in metrics for NAT traversal diagnostics.

Resource limits are part of the overlay config. The packet request-response
fallback has a bounded concurrent stream limit, exposed through `resources`, so
operators can tighten or relax stream pressure without changing code. Swarm
connection limits are also exposed there so public bootstrap and relay-capable
nodes can bound pending handshakes, established connections, and connections per
peer.

The configured interface MTU is treated as the requested packet MTU. The
effective packet MTU is capped by the fixed wire header's `u16` payload length
field and is used consistently by the TUN setup and packet forwarder. The
`status` and `up` commands print both configured and effective MTU values.

## Development

Enter the reproducible development shell:

```sh
nix develop
```

Generate a libp2p identity:

```sh
cargo run -- keygen
```

Generate a starter node config with a new private key:

```sh
cargo run -- init-config \
  --network lab \
  --output node-a.json \
  --listen-address /ip4/0.0.0.0/tcp/0 \
  --listen-address /ip4/0.0.0.0/udp/0/quic-v1 \
  --external-address /dns4/node-a.example.net/udp/4001/quic-v1
```

Generate another node config that knows how to dial node A:

```sh
cargo run -- init-config \
  --network lab \
  --output node-b.json \
  --listen-address /ip4/0.0.0.0/tcp/0 \
  --listen-address /ip4/0.0.0.0/udp/0/quic-v1 \
  --peer <node-a-peer-id>=/dns4/<node-a-hostname>/udp/4001/quic-v1 \
  --peer-route <node-a-peer-id>=10.42.0.0/24,100 \
  --bootstrap-peer <node-a-peer-id>=/dns4/<node-a-hostname>/udp/4001/quic-v1
```

Use `--private-key` to regenerate a config for an existing identity, `--force`
to overwrite an existing file, and `--output -` to print the generated JSON to
stdout. Repeat `--peer PEER_ID=MULTIADDR` for additional peer addresses; repeat
`--peer-route PEER_ID=CIDR[,METRIC]` for prefixes that peer is allowed to
originate. The default generated route metric is `100`, preserving the built-in
host routes at metric `0`. Repeat `--bootstrap-peer PEER_ID=MULTIADDR` for
Kademlia bootstrap nodes. By default the generated config uses the private
`/p2p-vpn/kad/1` Kademlia protocol; pass `--kademlia-protocol /ipfs/kad/1.0.0`
only when you intentionally want IPFS/public-DHT protocol compatibility with
your bootstrap peers. DNS multiaddrs, including `/dns4`, `/dns6`, `/dns`, and
`/dnsaddr`, are resolved by the libp2p transport for startup dials and redials.
Repeat `--external-address MULTIADDR` for stable public or DNS addresses that
libp2p should advertise to peers in addition to observed addresses learned
through identify and confirmed through AutoNAT. Use
`--disable-mdns`, `--disable-kademlia`, `--disable-dcutr`, or
`--disable-autonat` to omit optional discovery and NAT traversal behaviours from
the generated config.

Run local checks:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
nix flake check
nix build .#default
```

`nix flake check` builds and tests the package, checks formatting, and runs
clippy with warnings denied.

Run the Linux TUN end-to-end smoke test on hosts that allow unprivileged user
and network namespaces:

```sh
cargo test --test tun_namespace -- --ignored --nocapture
```

## Example Config

```json
{
  "network": {
    "name": "lab",
    "local_peer": "12D3KooW...",
    "private_key": "CAES...",
    "listen_addresses": [
      "/ip4/0.0.0.0/tcp/0",
      "/ip4/0.0.0.0/udp/0/quic-v1"
    ],
    "external_addresses": [
      "/dns4/node-a.example.net/udp/4001/quic-v1"
    ],
    "bootstrap_peers": [
      {
        "id": "12D3KooW...",
        "address": "/dns4/bootstrap.example.net/tcp/4001"
      }
    ],
    "discovery": {
      "mdns": true,
      "kademlia": true,
      "kademlia_protocol": "/p2p-vpn/kad/1",
      "dcutr": true,
      "autonat": true
    },
    "relay": {
      "server": false,
      "reservations": [
        "/dns4/relay.example.net/tcp/4001/p2p/12D3KooWRelay.../p2p-circuit"
      ],
      "resources": {
        "max_reservations": 128,
        "max_reservations_per_peer": 4,
        "reservation_duration_secs": 3600,
        "max_circuits": 16,
        "max_circuits_per_peer": 4,
        "max_circuit_duration_secs": 120,
        "max_circuit_bytes": 131072
      }
    }
  },
  "interface": {
    "name": "hs0",
    "mtu": 1280
  },
  "queue": {
    "max_packets_per_peer": 256,
    "max_bytes_per_peer": 524288,
    "max_packet_age_millis": 1000
  },
  "resources": {
    "max_concurrent_control_streams": 64,
    "max_concurrent_packet_streams": 256,
    "max_pending_incoming_connections": 64,
    "max_pending_outgoing_connections": 64,
    "max_established_incoming_connections": 256,
    "max_established_outgoing_connections": 256,
    "max_established_connections_per_peer": 8,
    "max_established_connections": 512
  },
  "peers": [
    {
      "id": "12D3KooW...",
      "name": "node-a",
      "addresses": [
        "/ip4/192.0.2.10/tcp/4001",
        "/ip4/192.0.2.10/udp/4001/quic-v1"
      ],
      "routes": [
        {
          "prefix": "10.42.0.0/24",
          "metric": 10
        }
      ]
    }
  ]
}
```

`bootstrap_peers` are dialed and added to the configured Kademlia routing table
when Kademlia discovery is enabled. TCP and QUIC startup dials support DNS
multiaddrs, including `/dns4`, `/dns6`, `/dns`, and `/dnsaddr`. Kademlia nodes
advertise themselves under the network provider key and query that same key for
other configured peers.
`discovery.kademlia_protocol` defaults to the private `/p2p-vpn/kad/1`
protocol. Set it to `/ipfs/kad/1.0.0` only for deployments that explicitly use
IPFS-compatible public bootstrap peers; those peers help discovery and NAT
traversal, but do not grant overlay membership or route authority.
`external_addresses` are registered with the libp2p swarm as explicit
advertised addresses; use them for stable public socket, DNS, or port-forwarded
addresses that peers should prefer over wildcard listen addresses.
`relay.reservations` are full libp2p relay listen addresses; listening on
one asks that relay for a circuit relay v2 reservation. Peer `addresses` may
also contain full relayed target addresses such as
`/dns4/relay.example.net/tcp/4001/p2p/<relay>/p2p-circuit/p2p/<peer>`. Set
`relay.server` to `true` on nodes that should accept relay reservations and
relay circuits for the overlay. `relay.resources` maps to circuit relay v2
server limits; its defaults match libp2p's relay defaults while retaining the
library's default rate limiters.

Inspect the compiled local view:

```sh
cargo run -- status --config p2p-vpn.json
```

`status` validates the config before printing the compiled view. It checks that
the private key matches `network.local_peer`, configured routes compile, all
listen and external multiaddrs parse, bootstrap and peer multiaddrs either omit
an explicit peer id or match the configured peer, and relay reservation
multiaddrs contain `/p2p/<relay>/p2p-circuit`.

Inspect the runtime metric names and startup snapshot:

```sh
cargo run -- metrics --config p2p-vpn.json
```

The metrics output includes control-plane exchange counters, packet counters,
queue occupancy, total queue drops, queue expiry drops, inbound and outbound
packet drop reasons including expired outbound queue packets, direct versus
relayed connection counts, relay reservation/circuit counts, relay-server
accept counts, DCUtR success/failure counts, observed external address
candidate/scheduled-probe/confirmed/expired counts, AutoNAT current
public/private/unknown reachability gauges and status-change counters, Kademlia
provider lookup/advertisement and bootstrap refresh counts, unauthorized
connection drops, configured peer redial counters, rejected discovered-address
counters, healthy path counts by transport kind, configured peers with and
without a currently supported packet path, and queue-drain stalls caused by
peers having no currently supported packet path. Those counters are intended to
show whether a deployment is exchanging capabilities, probing and advertising
observed public addresses, refreshing DHT discovery, rejecting bad discovered
addresses, using direct paths, relay fallback, hole-punching, enforcing
membership, recovering connections, and waiting on path negotiation as expected.

Inspect the Linux interface setup plan without requiring root:

```sh
cargo run -- up --config p2p-vpn.json --dry-run
```

Attempt to create the TUN device and install routes:

```sh
sudo target/debug/p2p-vpn up --config p2p-vpn.json
```

Print live forwarding counters while the runtime is up:

```sh
sudo target/debug/p2p-vpn up --config p2p-vpn.json --metrics-interval-seconds 10
```
