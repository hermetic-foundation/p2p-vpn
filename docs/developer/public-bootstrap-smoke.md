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

## Current Conclusion

| Capability | Evidence |
| --- | --- |
| Public bootstrap reachability | Proven. |
| Public relay candidate discovery | Proven. |
| Public relay reservation | Proven for at least one run. |
| Public relayed circuit | Proven for at least one run. |
| Generated-host relay reservation | Not consistently proven. |
| Public membership DHT propagation | Not proven yet. |
| Public DCUtR success | Not proven yet. |
