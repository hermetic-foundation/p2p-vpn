# Pairing

Pairing is the interactive onboarding path.

It is intended to replace manual key and config exchange.

## Status

| Piece | Status |
| --- | --- |
| `pair offer` URI format | Implemented |
| `pair accept` URI validation | Implemented |
| Live libp2p exchange | Not implemented yet |
| Minimal config writing | Not implemented yet |
| Signed membership grant return | Not implemented yet |

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
| Contact inviter | not yet |
| Write final config | not yet |

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

## Secrets

The pairing URI is sensitive.

Anyone with the URI can attempt pairing until it expires.

Do not post it publicly.

## Planned Exchange

Future `pair accept` will:

1. Generate or load a local node identity.
2. Discover the inviter through the URI hints.
3. Open an encrypted libp2p control exchange.
4. Send the new peer ID and requested VPN IP.
5. Receive a minimal local config.
6. Prefer a signed membership record over sharing a network key.

Manual invite files remain useful for offline exchange.
