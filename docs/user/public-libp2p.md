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

## Faster Relay Repro

Validate relay discovery and generated configs:

```sh
nix run .#public-relay-repro
```

This still records scan, relay, and generated config artifacts.

Relay-reservation artifacts are recorded when the local reservation check is
enabled.

## Local Reservation Check

Generated Host A and Host B configs are written by default.

Single-machine reservation checks are opt-in:

```sh
P2P_VPN_REPRO_REQUIRE_VPN_RELAY_RESERVATIONS=1 \
nix run .#public-relay-repro
```

Use this with controlled relays or known relay policy.

Public relays may cap reservations per source address.

## Strict Public Checks

Require public DHT membership propagation:

```sh
P2P_VPN_REPRO_MEMBERSHIP_DHT=1 \
nix run .#public-relay-repro
```

Require DCUtR hole-punch evidence:

```sh
P2P_VPN_REPRO_REQUIRE_DCUTR=1 \
nix run .#public-relay-repro
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

## Prove Two-Host Operation

Create host-run scripts from matched configs:

```sh
P2P_VPN_VPN_REPRO_PUBLIC_RELAY_DIR=/tmp/p2p-vpn-public-relay \
nix run .#public-vpn-repro
```

Run the generated Host A script on Host A.

Run the generated Host B script on Host B.

Each host writes:

| File | Purpose |
| --- | --- |
| `vpn-repro-evidence.json` | Machine-readable proof summary. |
| `daemon-health.txt` | Readiness checks. |
| `daemon-paths-final.json` | Final selected paths. |
| `daemon-status-prometheus-final.txt` | Metrics used by the checker. |
| `ping.txt` | Overlay data-plane ping result. |

Check both evidence files:

```sh
nix run .#public-vpn-evidence-check -- \
  --host-a /tmp/host-a/vpn-repro-evidence.json \
  --host-b /tmp/host-b/vpn-repro-evidence.json \
  --require-relay \
  --write-report /tmp/p2p-vpn-public-proof.json
```

For strict hole-punch proof, add:

```text
--require-direct --require-dcutr --require-quic-session
```

Use strict mode only when both hosts should have direct recovery evidence.

Relay-only fallback proof should require `--require-relay`.

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
| `dcutr_success` | Hole-punch proof did not complete. |
| `none` | Candidate passed requested checks. |

## Current Evidence

Public bootstrap and relay candidate discovery have been observed.

The latest public relay repro found a usable relayed-peer circuit.

| Evidence | Status |
| --- | --- |
| Public relay scan | 12 candidates found. |
| Public relay validation | 1 of 2 probed candidates passed. |
| Relayed peer circuit | Proven through a selected public relay. |
| Generated two-host configs | Written by the repro. |
| Two-host public VPN ping | Still requires two separated hosts. |
| Public non-LAN DCUtR | Still needs host evidence. |
