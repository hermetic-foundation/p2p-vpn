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
- first-class metrics for packet, queue, route, and path behavior

The current stream data plane uses a fixed binary header followed by the raw IP
packet payload. It is intentionally reusable over libp2p streams now and QUIC
datagrams later.

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

## Example Config

```json
{
  "network": {
    "name": "lab",
    "local_peer": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff"
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
      "id": "ffeeddccbbaa99887766554433221100ffeeddccbbaa99887766554433221100",
      "name": "node-a",
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

Inspect the compiled local view:

```sh
cargo run -- status --config p2p-vpn.json
```

Inspect the Linux interface setup plan without requiring root:

```sh
cargo run -- up --config p2p-vpn.json --dry-run
```

Attempt to create the TUN device and install routes:

```sh
sudo target/debug/p2p-vpn up --config p2p-vpn.json
```
