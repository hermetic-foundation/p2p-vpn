# Public Bootstrap Smoke

This records live public libp2p/IPFS reachability evidence.

Public infrastructure is reachability support only.

## 2026-08-04 Bootstrap Check

Command:

```sh
nix develop -c cargo run --quiet -- init-config \
  --output /tmp/p2p-vpn-public-check/p2p-vpn.json \
  --public-ipfs-profile \
  --force

nix develop -c cargo run --quiet -- bootstrap-check \
  --config /tmp/p2p-vpn-public-check/p2p-vpn.json \
  --timeout-seconds 45 \
  --require-autonat-status \
  --write-report /tmp/p2p-vpn-public-check/bootstrap-check.json \
  --force
```

Result:

```text
bootstrap check: ok
kademlia protocol: /ipfs/kad/1.0.0
ipfs compatible: true
dcutr enabled: true
autonat status: private
bootstrap peers: 5 connected 5 dial_failures 0
```

## Connected Bootstrap Peers

| Peer | Address |
| --- | --- |
| `QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN` | `/dnsaddr/bootstrap.libp2p.io` |
| `QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa` | `/dnsaddr/bootstrap.libp2p.io` |
| `QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb` | `/dnsaddr/bootstrap.libp2p.io` |
| `QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt` | `/dnsaddr/bootstrap.libp2p.io` |
| `QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ` | `/ip4/104.131.131.82/tcp/4001` |

## 2026-08-04 Relay Scan

Command:

```sh
nix develop -c cargo run -- \
  relay-scan \
  --ipfs-bootstrap-peers \
  --timeout-seconds 15 \
  --max-candidates 8
```

Result:

```text
public relay scan: ok
public relay scan peers: 5
public relay scan connected: 2
public relay scan identified: 2
public relay scan relay_capable: 2
public relay candidates: 8
```

## 2026-08-05 Public Relay Repro

The packaged repro found a public relay candidate that accepted a reservation.

It also proved a relayed circuit and generated relay-assisted configs.

The DCUtR-required phase reached relayed circuits but did not record a libp2p
DCUtR success event.

## 2026-08-09 Public Relay Repro

Command:

```sh
P2P_VPN_REPRO_REQUIRE_VPN_RELAY_RESERVATIONS=1 \
P2P_VPN_REPRO_MEMBERSHIP_DHT=1 \
nix run .#public-relay-repro
```

Result:

```text
public relay candidate validation: ok
relayed peer circuit: ok
membership DHT publish: quorum failed
generated-host relay reservations: no accepted reservation
strict DCUtR proof: skipped
```

Notes:

| Item | Evidence |
| --- | --- |
| Shell function phases | Run in-process. |
| Local listen collision | Avoided with ephemeral check ports. |
| Probe listener lifetime | Temporary listener task is aborted after probing. |
| Public relay probing | Reached a real public relay. |
| Generated-host reservations | Optional for public repros. |
| Public DHT membership | Still not operational proof. |

## 2026-08-09 Public Relay Repro Refresh

Command:

```sh
P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS=45 \
P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=60 \
P2P_VPN_RELAY_MAX_CANDIDATES=12 \
P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=8 \
nix run .#public-relay-repro
```

Artifact directory:

```text
/tmp/p2p-vpn-public-relay-repro.F3LdV4he
```

Result:

```text
public relay scan: ok
public relay candidates: 12
public relay probe: ok
public relay probe mode: relayed_peer_circuit
public relay candidates: 2 succeeded 1
```

Evidence:

| Item | Result |
| --- | --- |
| Public routing peers | 11 discovered. |
| Closest-peer lookup | 20 results, 0 errors. |
| Relay-hop candidates | 12 candidate addresses. |
| Host-reachable candidates | 8 after IPv6 skips. |
| Validated candidates | 2 probed. |
| Relayed peer circuit | 1 outbound circuit. |
| Generated host configs | Host A and Host B JSON written. |

Selected relay candidate:

```text
/ip4/159.100.30.75/udp/4001/quic-v1/p2p/12D3KooWSqLypHvFrpsN59nw9mkjfDkTf7fWx32yhjWAMXie433Y
```

Limits:

| Gap | Status |
| --- | --- |
| Generated-host reservations | Disabled for this run. |
| Membership DHT propagation | Disabled for this run. |
| Strict DCUtR success | Disabled for this run. |
| Two-host public VPN ping | Not part of this run. |

## 2026-08-10 Public Relay Repro Refresh

Command:

```sh
nix run .#public-relay-repro
```

Artifact directory:

```text
/tmp/p2p-vpn-public-relay-repro.JkTEdWAZ
```

Result:

```text
public relay scan: ok
public relay candidates: 8
public relay probe: ok
public relay probe mode: relayed_peer_circuit
public relay candidates: 2 succeeded 1
```

Evidence:

| Item | Result |
| --- | --- |
| Public routing peers | 11 discovered. |
| Relay-hop candidates | 8 candidate addresses. |
| Host-reachable candidates | 7 after IPv6 skip. |
| Validated candidates | 2 probed. |
| Relayed peer circuit | 1 outbound circuit. |
| Generated host configs | Host A and Host B JSON written. |

Selected relay candidate:

```text
/ip4/174.27.30.52/udp/25977/quic-v1/p2p/12D3KooWJrmFfkrB4B7aj2zVRsoGrSyGaaqaZzNz8GcL3F8YmCFh
```

Limits:

| Gap | Status |
| --- | --- |
| Generated-host reservations | Disabled for this run. |
| Membership DHT propagation | Disabled for this run. |
| Strict DCUtR success | Disabled for this run. |
| Two-host public VPN ping | Not part of this run. |

## 2026-08-11 Public Relay Reservation Check

Command:

```sh
P2P_VPN_REPRO_RELAY_CANDIDATE=/ip4/154.12.236.71/udp/4001/quic-v1/p2p/12D3KooWGLJopATxExmqvNfFPRv4frkEoX63j2yudJJftAaacH9a \
P2P_VPN_REPRO_REQUIRE_VPN_RELAY_RESERVATIONS=1 \
P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=45 \
P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=1 \
nix run .#public-relay-repro
```

Artifact directory:

```text
/tmp/p2p-vpn-public-relay-repro.fClV3A3f
```

Result:

```text
public relay probe: ok
public relay probe mode: relayed_peer_circuit
public relay candidates: 1 succeeded 1
generated two-host VPN relay reservations: failed
```

Evidence:

| Item | Result |
| --- | --- |
| Supplied relay candidate | Relayed peer circuit succeeded. |
| Generated Host A/B configs | Relay bootstrap and reservation were preserved. |
| Host A relay reservation | Configured `1`, accepted `0`. |
| Host B relay reservation | Configured `1`, accepted `0`. |
| Failure diagnosis | `missing_relay_reservation`. |

Limits:

| Gap | Status |
| --- | --- |
| Public relay reservation | Current candidate connected but did not accept. |
| Public pairing through relay | Still requires a reservable public relay. |
| Two-host public VPN ping | Not part of this run. |

## Current Conclusion

| Capability | Evidence |
| --- | --- |
| Public bootstrap reachability | Proven. |
| Public relay candidate discovery | Proven. |
| Public relay reservation | Previously proven; current candidate did not accept. |
| Public relayed circuit | Proven for at least one run. |
| Generated-host relay reservation | Not consistently proven. |
| Public membership DHT propagation | Not proven yet. |
| Public DCUtR success | Not proven yet. |
