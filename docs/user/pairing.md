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
| Live libp2p exchange | Not implemented yet |
| Signed membership grant return | Contract implemented |

## Offer

Run this on an existing trusted node:

```sh
p2p-vpn pair offer --config /etc/p2p-vpn/lab.json
```

The command prints a `p2pvpn:` URI.

Copy that URI to the new node.

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
| Write final config from response | yes |
| Contact inviter live | not yet |

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
| `--force` | Overwrite output config. |

Without `--private-key`, a new identity is generated.

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
| Discovery settings | mDNS, Kademlia, DCUtR, AutoNAT. |

Minimal configs include the public IPFS bootstrap defaults in the URI.

## Current Status

| Capability | Status |
| --- | --- |
| Offer URI generation | Implemented. |
| Signed request and response files | Implemented. |
| Libp2p pairing protocol | Implemented as `/p2p-vpn/pairing/1`. |
| Daemon request validation | Implemented. |
| One-time token replay rejection | Implemented per daemon process. |
| Daemon response generation | Implemented. |
| Live `pair accept` exchange | In progress. |

## Secrets

The pairing URI is sensitive.

Anyone with the URI can attempt pairing until it expires.

Do not post it publicly.

## Planned Exchange

Future `pair accept` will:

1. Discover the inviter through the URI hints.
2. Open an encrypted libp2p control exchange.
3. Send the new peer ID and requested VPN IP.
4. Receive the signed response automatically.
5. Write the minimal local config.

Manual invite files remain useful for offline exchange.
