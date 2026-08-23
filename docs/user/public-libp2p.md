# Public libp2p/IPFS Reachability

Public libp2p/IPFS infrastructure can help find paths.

It must not be treated as VPN membership or route authority.

## What Public Infrastructure Can Do

| Capability | Supported |
| --- | --- |
| Bootstrap into public routing | yes |
| Discover relay-hop candidates | partial |
| Reserve usable public relays | depends on relay policy |
| Carry relayed fallback traffic | yes, when relay policy allows it |
| Attempt DCUtR hole punching | topology-dependent |
| Authorize VPN routes | no |
| Authorize VPN membership | no |

## Code Pairing Over Public Paths

The default code workflow can use public routing and relays.

It does not require a public peer to know the pairing code.

| Step | Public infrastructure sees |
| --- | --- |
| Inviter advertisement | A network-scoped derived locator. |
| Joiner lookup | The same derived locator. |
| Relay transport | Encrypted libp2p traffic between peer identities. |
| Approval | Local daemon RPC only. |
| Membership grant | End-to-end authenticated pairing exchange. |

Start with normal defaults on both hosts:

```sh
sudo p2p-vpn pair open --instance lab
sudo p2p-vpn pair join CODE --instance lab --no-wait
```

Inspect the selected discovery and transport path:

```sh
sudo p2p-vpn pair status OPERATION --instance lab
```

Expected public fields include `discovery: Relay` or public lookup counters.

Relay availability still depends on each public relay's reservation policy.

## Create A Public Profile Config

```sh
nix run .# -- init-config \
  --output public.json \
  --public-ipfs-profile \
  --force
```

This profile:

| Setting | Value |
| --- | --- |
| Kademlia protocol | `/ipfs/kad/1.0.0` |
| Public bootstrap peers | enabled |
| mDNS | disabled |
| Provider advertisement | disabled |
| AutoNAT | enabled |
| DCUtR | enabled |

## Check Bootstrap Reachability

```sh
nix run .# -- bootstrap-check \
  --config public.json \
  --timeout-seconds 45 \
  --require-autonat-status \
  --write-report bootstrap-check.json \
  --force
```

## Scan For Relay Candidates

```sh
nix run .# -- relay-scan \
  --ipfs-bootstrap-peers \
  --timeout-seconds 30 \
  --write-candidates public-relay-candidates.txt
```

## Validate Relay Candidates

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --write-report public-relay-check.json \
  --timeout-seconds 45
```

Validate reservation acceptance:

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --require-relay-reservation \
  --max-validation-candidates 4 \
  --write-report public-relay-reservation.json \
  --timeout-seconds 45
```

Use this before discovery-only public pairing.

It checks whether the inviter can reserve the relay.

Validate DCUtR when the topology should allow hole punching:

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --require-dcutr-success \
  --max-validation-candidates 4 \
  --write-report public-relay-dcutr.json \
  --timeout-seconds 45
```

## Use A Validated Relay

Generate a relay-assisted config:

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --write-config public-relay.json \
  --timeout-seconds 45
```

The relay is added as infrastructure.

It is not added to `peers[]`.

## Generate Minimal Two-Host Configs

Use this for a mobile LAN-to-hotspot test:

```sh
nix run .# -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --write-host-a-config host-a.json \
  --write-host-b-config host-b.json \
  --timeout-seconds 45 \
  --force
```

The generated configs use:

| Setting | Value |
| --- | --- |
| Interface | `pv0` |
| LAN discovery | default mDNS |
| Public routing | default IPFS-compatible Kademlia |
| Provider ads | default enabled |
| Relay fallback | automatic relay candidates |
| Peer addresses | omitted |
| Relay reservations | omitted |
| Bootstrap peers | omitted from JSON; defaults apply at runtime |

This is the normal mobile profile.

The selected relay remains in the relay-check report.

It is not written into the host configs.

To force relay-only testing, disable direct listeners and mDNS in a copy.

## Move Between Networks

Use the same generated configs for every phase.

Do not add peer addresses, relay routes, or manual OS routes between phases.

| Phase | Host Placement | Required Result |
| --- | --- | --- |
| Baseline | Both hosts on LAN | Overlay ping succeeds. |
| Split | One host on hotspot or VPN | Overlay ping recovers through relay or direct public path. |
| Return | Both hosts back on LAN | Overlay ping succeeds again. |

For each move:

1. Start the Host A and Host B scripts once on LAN.
2. Keep both daemons running.
3. Move one host between LAN, hotspot, and LAN return.
4. Check overlay ping after each move.

Expected result:

| Phase | Expected Result |
| --- | --- |
| LAN baseline | Overlay ping succeeds. |
| Public split | Overlay ping recovers through relay or direct public path. |
| LAN return | Overlay ping succeeds again. |

For reproducible proof capture, use
[developer testing](../developer/testing.md).

## No-Route Backoff

Default public IPFS bootstrap peers are retried with backoff when the OS reports
`network unreachable` or `host unreachable`.

| Item | Behavior |
| --- | --- |
| First delay | 30 seconds |
| Maximum delay | 10 minutes |
| Reset | Successful connection to a default public bootstrap peer |

This only restrains public bootstrap retries.

LAN discovery, configured peers, discovered relay paths, and Kademlia record
lookups continue during the backoff window.

## Interpret Results

| Field | Meaning |
| --- | --- |
| `relay_reservation` | Reservation setup failed or timed out. |
| `relayed_peer_circuit` | Relay path did not connect to target. |
| `dcutr_success` | Hole punching did not complete. |
| `none` | Candidate passed requested checks. |

## More References

Strict checks and deeper debugging guides live in:

| Document | Contents |
| --- | --- |
| [Developer Testing](../developer/testing.md) | VM, namespace, relay, and two-host proof commands. |
| [Public Bootstrap Smoke](../developer/public-bootstrap-smoke.md) | Public reachability notes. |
| [Feature Matrix](../developer/feature-matrix.md) | Current implementation status. |
