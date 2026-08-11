# Pairing

Pairing is the interactive onboarding path.

It is intended to replace manual key and config exchange.

## Requirements

You need one trusted node already running.

The trusted node needs:

| Required | Purpose |
| --- | --- |
| `network.name` | Selects the overlay. |
| `network.private_key` | Signs the offer and response. |
| `network.vpn_ip` | Advertises the inviter route. |

The new node needs either a generated identity or `--private-key`.

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

The generated config preserves the relayed inviter address after accept.

## Offer File

Write the URI to a file:

```sh
p2p-vpn pair offer \
  --config /etc/p2p-vpn/lab.json \
  --output lab.pair
```

The file contains one line.

It is easier to inspect and rotate than a full config.

## Inspect

Inspect a URI or offer file:

```sh
p2p-vpn pair inspect lab.pair
```

The output hides the rendezvous token by default.

Use this only on trusted terminals:

```sh
p2p-vpn pair inspect lab.pair --show-secret
```

Useful fields:

| Field | Meaning |
| --- | --- |
| `pairing offer` | `valid` or `expired`. |
| `discovery only` | Whether direct inviter hints are omitted. |
| `inviter address hints` | Direct plus relayed dial hints. |
| `bootstrap peers` | Peers the accept path can seed from. |
| `rendezvous token` | Hidden unless `--show-secret` is set. |

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

You can also pass an offer file:

```sh
p2p-vpn pair accept lab.pair
```

`pair accept` will:

1. Verify the signed offer.
2. Contact the inviter over libp2p.
3. Request the chosen VPN IP and local routes.
4. Receive a signed membership grant.
5. Write the new local config.

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
| `--local-route` | Extra route to request and write locally. |
| `--vpn-ip` | Requested VPN IP for the new node. |
| `--peer-name` | Label for the inviter peer. |
| `--timeout-seconds` | Live pairing exchange timeout. |
| `--force` | Overwrite output config. |

Without `--private-key`, a new identity is generated.

Without `--vpn-ip`, `pair accept` requests the built-in IP derived from the new
peer ID.

With `--vpn-ip`, record-based inviters grant that host route automatically.

With live pairing, each `--local-route` is included in the signed request.

Record-based inviters return those routes as signed membership grants.

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

The daemon also checks the authenticated libp2p peer.

It must match the signed joiner peer in the pairing request.

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

Minimal configs use public IPFS bootstrap peers by default.

Pairing does not write the built-in public bootstrap list into the URI or
generated config.

Explicit bootstrap peers are still included.

`--discovery-only` omits inviter addresses.

It can still include:

| Hint | Source |
| --- | --- |
| Bootstrap peers | `network.bootstrap_peers` or public IPFS defaults. |
| Relay reservations | `network.relay.reservations`. |
| Discovery settings | `network.discovery`. |

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
