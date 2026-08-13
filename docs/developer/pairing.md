# Pairing Implementation

This page defines the protocol, trust transition, and verification surface.

User workflows live in [../user/pairing.md](../user/pairing.md).

## Protocol Surface

| Surface | Value |
| --- | --- |
| Stream protocol | `/p2p-vpn/pairing/1` |
| Offer transport | `p2pvpn:` URI |
| Offer signer | Inviter identity |
| Request signer | Joiner identity |
| Response signer | Inviter identity |
| Preferred grant | Signed membership record |
| Optional grant | Shared membership key |

## Exchange

1. Inviter signs network, reachability, token, and expiry.
2. Joiner verifies the offer and signs its requested authority.
3. libp2p authenticates the connected joiner peer ID.
4. Inviter compares transport identity with the signed request.
5. Inviter signs the member record and pairing response.
6. Both runtimes install the grant before packet forwarding.

The response always includes a signed member record.

This remains true when a shared membership key is also returned.

## Offer Contents

| Field | Security purpose |
| --- | --- |
| Network name | Prevent cross-overlay import |
| Inviter peer and public key | Bind signer identity |
| Token | Scope one acceptance window |
| Issue and expiry times | Bound replay lifetime |
| Address hints | Seed direct or relayed dialing |
| Bootstrap peers | Seed public discovery |
| Discovery settings | Reproduce compatible lookup behavior |
| Protocol versions | Reject incompatible peers early |

Discovery-only offers omit direct inviter addresses.

Signed relay and bootstrap hints remain available.

## Request Validation

The inviter rejects a request when any check fails:

- Offer signature, token, network, or expiry.
- Joiner signature or public-key binding.
- Authenticated libp2p peer differs from `joiner_peer`.
- Requested VPN IP or route syntax is invalid.
- Token was consumed by the current daemon process.
- Per-peer pairing request limit was exceeded.

The default rate limit is four requests per peer per second.

## Grant Construction

The record subject contains the joiner peer ID and public key.

Default roles:

| Condition | Roles |
| --- | --- |
| No requested custom routes | `overlay_member` |
| Custom VPN IP or routes | `overlay_member`, `route_authority` |

A custom VPN IP becomes a host route grant.

The peer-derived built-in address does not require an extra grant.

## Live Installation

After response delivery, the inviter:

1. Merges the signed record into bounded live state.
2. Rebuilds peer authorization and route ownership.
3. Synchronizes TUN routes.
4. Removes relay-infrastructure classification for the joiner.
5. Promotes the pairing connection to an overlay path.
6. Starts control, service, and packet-plane negotiation.

The joiner writes the same grant into its selected config format.

## Restart Recovery

Inviter live state is intentionally reconstructable.

The joiner advertises its stored signed record in control capabilities.

An inviter that signed the record trusts its own issuer identity and can restore:

- Overlay membership.
- Route authority.
- Kernel route state.
- Packet-path negotiation.

Unknown inbound peers begin as provisional membership probes.

They gain no overlay authority until record verification succeeds.

## NixOS Renderer

The NixOS accept path emits typed module options.

It omits module defaults for:

- Network name when it equals the instance.
- Default state identity path.
- Default listener set.
- Default packet plane.
- Default `pv0` interface and MTU.

The renderer includes signed records and non-default protocol settings.

It never embeds private key material in Nix.

## Identity File Safety

NixOS accept performs these checks before identity reuse:

| Check | Failure behavior |
| --- | --- |
| Absolute state path | Reject relative path |
| Store boundary | Reject `/nix/store` |
| File type | Reject symlink and non-regular file |
| Permissions | Reject group or world access |
| Existing content | Keep only when identity matches |

Forced replacement uses a same-directory temporary file and atomic rename.

## Runtime Counters

| Counter | Meaning |
| --- | --- |
| `pairing_requests_received` | Inbound requests decoded |
| `pairing_requests_accepted` | Signed responses installed |
| `pairing_requests_rejected` | Requests denied before response |
| `pairing_reject_invalid_offer` | Signature, expiry, or shape failure |
| `pairing_reject_replayed_token` | Token already consumed |
| `pairing_reject_rate_limited` | Per-peer rate limit reached |
| `pairing_responses_received` | Outbound client responses seen |
| `pairing_outbound_failures` | Request send or response failure |
| `pairing_inbound_failures` | Response delivery failure |

## Focused Tests

```sh
nix develop -c cargo test pairing_response_for_request
nix develop -c cargo test capability_response_authorizes_peer_presenting_local_signed_record
nix develop -c cargo test nixos_pair_accept
nix develop -c cargo test render_pairing_nixos_module
```

## VM Proof

```sh
nix build .#checks.x86_64-linux.nixos-vm-pairing --no-link -L
```

The VM check proves:

| Stage | Assertion |
| --- | --- |
| Offer | Running NixOS instance emits a signed URI |
| Accept | Joiner writes Nix only |
| Identity | Existing module key is reused unchanged |
| Defaults | Generated Nix omits redundant transport settings |
| Evaluation | Output evaluates through the exported module |
| Replay | Reusing the token is rejected with diagnostics |
| Live install | Inviter learns unconfigured joiner authority |
| Traffic | Bidirectional ICMP crosses `pv0` |
| Restart | Inviter reconstructs authority from joiner record |

## Public Relay Proof

The ignored public smoke requires an externally reachable relay:

```sh
P2P_VPN_LIVE_RELAY_MULTIADDR=/ip4/RELAY_IP/udp/4001/quic-v1/p2p/RELAY_ID \
P2P_VPN_LIVE_RELAY_TIMEOUT_SECONDS=90 \
nix develop -c cargo test \
  live_pair_accept_uses_public_relay_for_discovery_only_offer \
  -- --ignored --nocapture
```

Public DHT-only inviter discovery remains a separate evidence target.

## Evidence

| Date | Evidence | Result |
| --- | --- | --- |
| 2026-08-11 | Public relay pairing smoke | Discovery-only accept completed through relay |
| 2026-08-12 | NixOS pairing VM | Nix-only accept, identity reuse, traffic, and restart recovery passed |
