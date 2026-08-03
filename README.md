# p2p-vpn

Rust foundation for a Hyprspace-inspired peer-to-peer mesh VPN.

The v2 design keeps libp2p as the identity, discovery, NAT traversal, and relay
substrate while making the VPN data plane packet-oriented:

- direct QUIC datagrams for preferred packet transport
- framed libp2p streams as compatibility and relay fallback
- bounded per-peer packet queues with intentional drop policy
- separate control, packet, and service protocol surfaces
- explicit route ownership and source-address authorization
- path scoring and promotion from relay to direct paths
- first-class metrics for packet, queue, route, relay, DCUtR, and path behavior

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

The current stream data plane uses a fixed binary header followed by the raw IP
packet payload. The header includes a non-zero packet session id derived from
the local peer identity plus a per-session packet sequence number. It is
intentionally reusable over libp2p streams now and QUIC datagrams later.

The libp2p runtime exposes `/p2p-vpn/packet/1` for framed packet exchange over
request-response streams. Inbound packet handling checks the configured peer
allowlist and route ownership before writing to TUN.

Route ownership is static and exclusive. Each configured peer may own its
built-in host routes plus any advertised prefixes, but route compilation rejects
overlapping prefixes owned by different peers, including more-specific prefixes
that would hijack another peer's aggregate route.

Outbound packet draining is path-aware. The runtime records direct TCP, direct
QUIC stream, and circuit-relay paths from libp2p connection events, marks paths
unhealthy when their connections close, and only drains a peer's bounded queue
while that peer has a healthy path. Packets for disconnected peers remain
bounded by the per-peer queue limits instead of expanding into unbounded stream
requests.

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

Run local checks:

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
nix flake check
nix build .#default
```

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
    "bootstrap_peers": [
      {
        "id": "12D3KooW...",
        "address": "/dns4/bootstrap.example.net/tcp/4001"
      }
    ],
    "discovery": {
      "mdns": true,
      "kademlia": true,
      "dcutr": true
    },
    "relay": {
      "server": false,
      "reservations": [
        "/dns4/relay.example.net/tcp/4001/p2p/12D3KooWRelay.../p2p-circuit"
      ]
    }
  },
  "interface": {
    "name": "hs0",
    "mtu": 1280
  },
  "queue": {
    "max_packets_per_peer": 256,
    "max_bytes_per_peer": 524288
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

`bootstrap_peers` are dialed and added to the overlay-scoped Kademlia routing
table when Kademlia discovery is enabled. Kademlia nodes advertise themselves
under the network provider key and query that same key for other configured
peers. `relay.reservations` are full libp2p relay listen addresses; listening on
one asks that relay for a circuit relay v2 reservation. Peer `addresses` may
also contain full relayed target addresses such as
`/dns4/relay.example.net/tcp/4001/p2p/<relay>/p2p-circuit/p2p/<peer>`. Set
`relay.server` to `true` on nodes that should accept relay reservations and
relay circuits for the overlay.

Inspect the compiled local view:

```sh
cargo run -- status --config p2p-vpn.json
```

Inspect the runtime metric names and startup snapshot:

```sh
cargo run -- metrics --config p2p-vpn.json
```

The metrics output includes packet counters, queue occupancy and drops, direct
versus relayed connection counts, relay reservation/circuit counts, relay-server
accept counts, and DCUtR success/failure counts. Those counters are intended to
show whether a deployment is using direct paths, relay fallback, and
hole-punching as expected.

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
