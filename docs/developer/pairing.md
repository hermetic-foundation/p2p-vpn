# Pairing Implementation

This page tracks implementation details and verification evidence.

User commands live in [../user/pairing.md](../user/pairing.md).

## Protocol Surface

| Surface | Status |
| --- | --- |
| Pairing protocol | `/p2p-vpn/pairing/1` |
| Offer URI format | Implemented |
| Request validation | Implemented |
| Response validation | Implemented |
| Config import | Implemented |
| Signed membership records | Preferred grant path |
| Shared membership key | Compatibility grant path |

## Runtime Behavior

| Capability | Status |
| --- | --- |
| Live `pair accept` exchange | Implemented for URI and bootstrap hints. |
| Discovery-only offer | Implemented. |
| Relay-assisted discovery-only accept | Implemented. |
| Public relay discovery-only accept | Proven with live public relay smoke. |
| Public DHT-only inviter discovery | Path implemented; public DHT proof pending. |
| Replay rejection | Implemented per daemon process. |
| Pairing request rate limit | Implemented per libp2p peer. |
| Pairing counters | Exposed through status and metrics. |

## Config Guarantees

| Guarantee | Verification |
| --- | --- |
| Compact inviter configs derive `local_peer` from `private_key`. | Unit test. |
| Accepted configs include the inviter VPN route. | Unit test and NixOS VM. |
| Live membership install routes to requested VPN IP. | Unit test. |
| Generated joiner config starts without manual peer routes. | NixOS VM. |
| Bidirectional overlay ICMP works after pairing. | NixOS VM. |

## Tests

Run focused unit tests:

```sh
nix develop -c cargo test pairing_response_imports_minimal_config_with_shared_key
nix develop -c cargo test merge_membership_records_routes_packets_to_requested_vpn_ip
nix develop -c cargo test pairing_response_for_request_grants_custom_requested_vpn_ip_route
```

Run the VM workflow proof:

```sh
system=$(nix eval --raw --impure --expr builtins.currentSystem)
nix build .#checks.$system.nixos-vm-pairing --no-write-lock-file -L
```

Run the live public relay smoke:

```sh
P2P_VPN_LIVE_RELAY_MULTIADDR=/ip4/84.200.154.199/udp/4001/quic-v1/p2p/12D3KooWRpbRYnbLvKjW2LMt4cUaiZyJ2tPNpRoRHXx9YSngLWie \
P2P_VPN_LIVE_RELAY_TIMEOUT_SECONDS=90 \
nix develop -c cargo test live_pair_accept_uses_public_relay_for_discovery_only_offer -- --ignored --nocapture
```

## Recorded Evidence

| Date | Evidence | Result |
| --- | --- | --- |
| 2026-08-11 | NixOS pairing VM | Offer, accept, replay rejection, health, and bidirectional ICMP passed. |
| 2026-08-11 | Live public relay pairing smoke | Discovery-only accept completed through the selected public relay. |
