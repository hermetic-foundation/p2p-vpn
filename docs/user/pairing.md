# Pairing

Pairing is the interactive onboarding path.

It is intended to replace manual key and config exchange.

## Status

| Piece | Status |
| --- | --- |
| `pair offer` URI format | Implemented |
| `pair accept` URI validation | Implemented |
| Signed response validation | Implemented |
| Config writing from response | Implemented |
| Live libp2p exchange | Implemented for URI and bootstrap hints |
| Signed membership grant return | Contract implemented |
| Bootstrap-only accept | Implemented |
| Discovery-only offer | Implemented |
| Discovery-only relay accept | Implemented with local relay proof |
| Public discovery-only accept | Implemented path, public proof pending |
| Timeout diagnostics | Implemented |
| Daemon pairing counters | Implemented |

## Offer

Run this on an existing trusted node:

```sh
p2p-vpn pair offer --config /etc/p2p-vpn/lab.json
```

The command prints a `p2pvpn:` URI.

Copy that URI to the new node.

## Discovery-Only Offer

Use this when the URI should not embed current inviter addresses:

```sh
p2p-vpn pair offer \
  --config /etc/p2p-vpn/lab.json \
  --discovery-only
```

This keeps bootstrap and discovery hints in the URI.

The accept side must discover the inviter over mDNS, Kademlia, or relay hints.

Relay reservation hints are signed when present.

They do not expose a direct inviter address.

## Offer File

Write the URI to a file:

```sh
p2p-vpn pair offer \
  --config /etc/p2p-vpn/lab.json \
  --output lab.pair
```

The file contains one line.

It is easier to inspect and rotate than a full config.

## Expiry

Default expiry:

| Option | Default |
| --- | --- |
| `--expires-in-seconds` | `600` |

Use a shorter window for public or shared terminals:

```sh
p2p-vpn pair offer --expires-in-seconds 120
```

## Accept

Run this on the new node:

```sh
p2p-vpn pair accept 'p2pvpn:...'
```

Current behavior:

| Step | Behavior |
| --- | --- |
| Parse URI | yes |
| Verify inviter signature | yes |
| Check expiry | yes |
| Show discovery hints | yes |
| Import signed response file | yes |
| Contact inviter from URI hints | yes |
| Contact inviter from bootstrap hints | yes |
| Contact inviter through signed relay reservation hints | yes |
| Write final config from response | yes |
| Discover inviter from only public DHT | implemented path, public proof pending |

## Accept Response File

Use this when a signed response was produced out of band:

```sh
p2p-vpn pair accept 'p2pvpn:...' \
  --response pairing-response.json \
  --output p2p-vpn.json
```

Optional fields:

| Option | Meaning |
| --- | --- |
| `--private-key` | Use an existing local identity. |
| `--interface` | Interface name for the generated config. |
| `--mtu` | Interface MTU. |
| `--local-route` | Extra local route ownership. |
| `--peer-name` | Label for the inviter peer. |
| `--timeout-seconds` | Live pairing exchange timeout. |
| `--force` | Overwrite output config. |

Without `--private-key`, a new identity is generated.

## Live Accept

By default, `pair accept` contacts the inviter over libp2p.

It uses direct, relayed, bootstrap, mDNS, and Kademlia hints from the URI.

```sh
p2p-vpn pair accept 'p2pvpn:...' \
  --output /etc/p2p-vpn/lab.json
```

Use `--timeout-seconds` when the path may need relay setup:

```sh
p2p-vpn pair accept 'p2pvpn:...' \
  --timeout-seconds 60
```

Use `--response` for offline response import.

## Live Diagnostics

If live pairing times out, the final error includes route context.

Example fields:

| Field | Meaning |
| --- | --- |
| `inviter_hints` | Inviter addresses embedded in the URI. |
| `relayed_inviter_hints` | URI hints that use `/p2p-circuit`. |
| `bootstrap_peers` | Bootstrap peers embedded in the URI. |
| `request_attempts` | Pairing request sends attempted. |
| `outbound_failures` | libp2p request failures. |
| `dial_errors` | connection setup errors observed. |
| `relayed_dial_start_failures` | relay dial attempts rejected locally. |

The diagnostic is intentionally compact.

It does not print the pairing URI or rendezvous token.

## Daemon Status

The daemon exports pairing counters through normal status and metrics views.

```sh
p2p-vpn status --config /etc/p2p-vpn/lab.json
p2p-vpn metrics --config /etc/p2p-vpn/lab.json
```

Useful counters:

| Counter | Meaning |
| --- | --- |
| `pairing_requests_received` | Live pairing requests seen by the daemon. |
| `pairing_requests_accepted` | Requests that produced a response. |
| `pairing_requests_rejected` | Requests rejected before response. |
| `pairing_reject_invalid_offer` | Bad signature, expiry, network, or grant shape. |
| `pairing_reject_replayed_token` | One-time token already consumed. |
| `pairing_reject_rate_limited` | Per-peer request limit exceeded. |
| `pairing_outbound_failures` | Local pairing request-response sends failed. |
| `pairing_inbound_failures` | Remote pairing request-response sends failed. |

## Daemon Limits

The inviter daemon rate-limits live pairing requests per libp2p peer.

| Setting | Default |
| --- | --- |
| `resources.max_pairing_requests_per_peer_per_second` | `4` |

Rate-limited requests are rejected before response generation.

This limit is separate from packet forwarding limits.

## URI Contents

The URI includes:

| Field | Meaning |
| --- | --- |
| Network name | Overlay to join. |
| Inviter peer ID | Existing trusted node. |
| Inviter public key | Verifies the signed offer. |
| Rendezvous token | One-time pairing secret. |
| Expiry | Rejects stale offers. |
| Inviter addresses | Direct or relayed hints. |
| Bootstrap peers | Discovery hints. |
| Relay reservations | Relay paths for dialing the inviter. |
| Discovery settings | mDNS, Kademlia, DCUtR, AutoNAT. |

Minimal configs include the public IPFS bootstrap defaults in the URI.

`--discovery-only` omits inviter addresses.

It can still include:

| Hint | Source |
| --- | --- |
| Bootstrap peers | `network.bootstrap_peers` or public IPFS defaults. |
| Relay reservations | `network.relay.reservations`. |
| Discovery settings | `network.discovery`. |

## Current Status

| Capability | Status |
| --- | --- |
| Offer URI generation | Implemented. |
| Signed request and response files | Implemented. |
| Libp2p pairing protocol | Implemented as `/p2p-vpn/pairing/1`. |
| Daemon request validation | Implemented. |
| One-time token replay rejection | Implemented per daemon process. |
| Pairing request rate limit | Implemented per libp2p peer. |
| Pairing daemon counters | Implemented in status and metrics views. |
| Daemon response generation | Implemented. |
| Live `pair accept` exchange | Implemented for URI and bootstrap hints. |
| Live `pair accept` diagnostics | Implemented. |
| Relay-assisted discovery-only `pair accept` | Implemented with local relay proof. |
| Public discovery-only `pair accept` | Implemented path, public proof pending. |

## Secrets

The pairing URI is sensitive.

Anyone with the URI can attempt pairing until it expires.

Do not post it publicly.

## Exchange

Live `pair accept`:

1. Discover the inviter through the URI hints.
2. Open an encrypted libp2p control exchange.
3. Send the new peer ID and requested VPN IP.
4. Receive the signed response automatically.
5. Write the minimal local config.

Manual invite files remain useful for offline exchange.
