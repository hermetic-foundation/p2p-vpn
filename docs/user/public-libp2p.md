# Public libp2p/IPFS Reachability

Public libp2p/IPFS infrastructure can help find paths.

It must not be treated as VPN membership or route authority.

## What Public Infrastructure Can Do

| Capability | Supported |
| --- | --- |
| Bootstrap into public routing | yes |
| Discover relay-hop candidates | partial |
| Reserve usable public relays | depends on relay policy |
| Carry relayed fallback traffic | proven with selected relays |
| Prove DCUtR hole punching | topology-dependent |
| Authorize VPN routes | no |
| Authorize VPN membership | no |

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

Add DCUtR proof requirements:

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

## Generate Two Host Configs

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
| Direct listener | `/ip4/0.0.0.0/tcp/4001` |
| LAN discovery | mDNS enabled |
| Public routing | `/ipfs/kad/1.0.0` |
| Relay fallback | selected relay reservation |

This is the normal mobile profile.

To force relay-only testing, disable direct listeners and mDNS in a copy.

## Interpret Results

| Field | Meaning |
| --- | --- |
| `relay_reservation` | Reservation setup failed or timed out. |
| `relayed_peer_circuit` | Relay path did not connect to target. |
| `dcutr_success` | Hole-punch proof did not complete. |
| `none` | Candidate passed requested checks. |

## Current Evidence

Public bootstrap and relay candidate discovery have been observed.

LAN relay fallback is proven. Public non-LAN DCUtR still needs host evidence.
