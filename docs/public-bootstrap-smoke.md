# Public Bootstrap Smoke

This file records live, rootless smoke-test evidence for public
libp2p/IPFS-compatible bootstrap reachability. Public bootstrap peers are
reachability infrastructure only; they do not grant VPN membership or route
authority.

## 2026-08-04

Command:

```sh
nix develop -c cargo run --quiet -- init-config \
  --output /tmp/p2p-vpn-public-check/p2p-vpn.json \
  --ipfs-kademlia \
  --ipfs-bootstrap-peers \
  --disable-mdns \
  --disable-kademlia-provider-advertisement \
  --force

nix develop -c cargo run --quiet -- bootstrap-check \
  --config /tmp/p2p-vpn-public-check/p2p-vpn.json \
  --timeout-seconds 45 \
  --require-autonat-status
```

Result:

```text
bootstrap check: ok
success threshold: any
require autonat status: true
kademlia protocol: /ipfs/kad/1.0.0
ipfs compatible: true
dcutr enabled: true
kademlia bootstrap started: true
kademlia rendezvous lookup started: true
kademlia rendezvous advertise started: false
autonat probe servers registered: 5
autonat status: private
bootstrap peers: 5 connected 5 dial_failures 0
relay reservations: 0 accepted 0 relayed_listen_addresses 0
relayed peer circuits: 0 connected 0
```

Connected public bootstrap peers:

```text
QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN /dnsaddr/bootstrap.libp2p.io
QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa /dnsaddr/bootstrap.libp2p.io
QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb /dnsaddr/bootstrap.libp2p.io
QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt /dnsaddr/bootstrap.libp2p.io
QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ /ip4/104.131.131.82/tcp/4001
```

This proves current public IPFS-compatible bootstrap connectivity and AutoNAT
observation from an unprivileged process. It does not prove public relay
reservation acceptance, relayed peer circuit dialing, or public-relay-assisted
DCUtR hole punching; those require a known public circuit-relay v2 endpoint that
accepts reservations for the test.

To record the remaining relay evidence, first scan configured or public
bootstrap peers for peers that advertise the circuit-relay v2 hop protocol:

```sh
nix develop -c cargo run -- relay-scan --ipfs-bootstrap-peers --timeout-seconds 30

nix develop -c cargo run -- relay-scan \
  --ipfs-bootstrap-peers \
  --check-candidates \
  --max-validation-candidates 6 \
  --candidate-timeout-seconds 45 \
  --timeout-seconds 30

nix develop -c cargo run -- relay-scan \
  --ipfs-bootstrap-peers \
  --check-candidates \
  --write-config p2p-vpn-public-relay.json \
  --max-validation-candidates 6 \
  --candidate-timeout-seconds 45 \
  --timeout-seconds 30

nix develop -c cargo run -- relay-scan \
  --bootstrap-peer PEER_ID=/dnsaddr/bootstrap.example.net/p2p/PEER_ID \
  --timeout-seconds 30
```

`relay-scan` reports direct `/p2p/RELAY` candidate multiaddrs. With
`--ipfs-bootstrap-peers`, it uses the bundled public IPFS bootstrap set and
actively samples additional peers through the public `/ipfs/kad/1.0.0` routing
table; scan output reports both configured bootstrap peers and total scanned
peers, whether the active closest-peer lookup started and finished, how many
peer records it returned, and how many routing-table peers were discovered. The
bounded candidate set prefers distinct relay peers when it can replace
duplicate addresses from an already represented relay. Treat reported candidates
as hints only; the peer can advertise relay-hop support and still reject
reservations because of load, policy, or resource limits. The scanner filters
out transport protocols this binary cannot dial. With
`--check-candidates`, the command immediately runs the same reservation and
relayed-circuit validation as `relay-check`; add `--require-dcutr-success` when
the candidate must also prove public-relay-assisted hole punching. Validation
tries scanned candidates round-robin by relay peer, so a scan with many
addresses for one relay still tests other relays before cycling through that
peer's alternate addresses. Within each relay peer, validation tries
QUIC-capable addresses before TCP addresses so bounded public DCUtR searches
spend early attempts on transports more likely to hole punch. Hosts without a
usable IPv6 route skip IPv6-only relay candidates during validation and print each skip with
`reason ipv6_unreachable`, while still showing the candidate in the scan output.
Use `--max-validation-candidates N` to bound each validation pass after host
reachability filtering; this is especially useful with `--require-dcutr-success`
because each public relay has a single end-to-end reservation plus circuit/DCUtR
timeout budget. Add `--write-config PATH` with `--check-candidates` to write a
default relay-assisted config from the first validated scanned candidate. When the scan
uses `--config p2p-vpn.json`, the output preserves that config's overlay
identity, membership, peers, routes, queue limits, and packet-plane settings and
adds only the validated relay bootstrap and reservation infrastructure.

Then run the live relay smokes with a known-good relay or the scanned candidate
set. The preferred rootless operator command is:

```sh
nix develop -c cargo run -- relay-check \
  --relay-candidate /dns4/relay-a.example.net/tcp/4001/p2p/RELAY_A \
  --relay-candidate /dns4/relay-b.example.net/tcp/4001/p2p/RELAY_B \
  --timeout-seconds 45

nix develop -c cargo run -- relay-check \
  --relay-candidate /dns4/relay-a.example.net/tcp/4001/p2p/RELAY_A \
  --relay-candidate /dns4/relay-b.example.net/tcp/4001/p2p/RELAY_B \
  --require-dcutr-success \
  --timeout-seconds 45

nix develop -c cargo run -- relay-check \
  --relay-candidate /dns4/relay-a.example.net/tcp/4001/p2p/RELAY_A \
  --write-config p2p-vpn-public-relay.json \
  --timeout-seconds 45
```

The command tries candidates until one works, reports per-candidate failures,
and requires direct relay multiaddrs with `/p2p/RELAY` but without
`/p2p-circuit`. Reservation setup failures identify whether the probe connected
directly to the relay, whether relay reservation acceptance or relayed
listen-address publication timed out, and the last direct relay dial error when
one was observed. Candidate lines also include a stable `failure_stage` value:
`candidate_setup`, `relay_reservation`, `relayed_peer_circuit`,
`dcutr_success`, or `none` for a usable candidate. Probe output also includes a
`public relay candidate failure stages:` summary with per-stage counts across
the attempted set. Failures after reservation setup include the same detailed
bootstrap-check lines that successful candidates print, so failed
relayed-circuit and DCUtR probes show which prerequisite was missing. DCUtR
failures also include the last libp2p hole-punch error as `dcutr last_error`,
which distinguishes cases such as no direct connection, rejected relay
prerequisites, or direct handshake timeouts.
Successful candidates also print a `public relay candidate config:` line with
the exact `--relay-peer
PEER=MULTIADDR` shortcut and full `--relay-reservation .../p2p-circuit`
address to feed into `init-config`. Use `--write-config PATH` to write that
default relay-assisted config automatically after the first candidate validates;
the relay is treated as reachability infrastructure, not as a VPN peer.

The ignored test harness can run the same live checks:

```sh
P2P_VPN_LIVE_RELAY_MULTIADDRS='/dns4/relay-a.example.net/tcp/4001/p2p/RELAY_A,/dns4/relay-b.example.net/tcp/4001/p2p/RELAY_B' \
  nix develop -c cargo test runtime::bootstrap_check::tests::bootstrap_check_can_probe_live_public_relayed_peer_circuit \
  -- --ignored --exact --nocapture

P2P_VPN_LIVE_RELAY_MULTIADDRS='/dns4/relay-a.example.net/tcp/4001/p2p/RELAY_A,/dns4/relay-b.example.net/tcp/4001/p2p/RELAY_B' \
  nix develop -c cargo test runtime::bootstrap_check::tests::bootstrap_check_can_probe_live_public_dcutr_success \
  -- --ignored --exact --nocapture
```

`P2P_VPN_LIVE_RELAY_MULTIADDRS` accepts up to eight comma, semicolon, or
newline-separated direct relay multiaddrs. `P2P_VPN_LIVE_RELAY_MULTIADDR`
remains supported for a single relay.

Recorded public relay scan evidence on 2026-08-04:

```text
$ nix develop -c cargo run -- relay-scan --ipfs-bootstrap-peers --timeout-seconds 15 --max-candidates 8
public relay scan: ok
public relay scan peers: 5
public relay scan connected: 2
public relay scan identified: 2
public relay scan relay_capable: 2
public relay scan dial_failures: 0
public relay candidates: 8
```

The scan found TCP/QUIC relay-hop candidate addresses for
`QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ` and
`QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa`. A follow-up
`relay-check` against their IPv4 TCP and QUIC candidates did not prove usable
relay service: all four candidates timed out waiting for relay reservation
acceptance. That means the public Identify scan is operational, but public
reservation and public-relay-assisted DCUtR evidence still require a relay that
both advertises hop support and accepts reservations at the time of the smoke.

Additional short validation on 2026-08-04:

```text
$ nix develop -c cargo run --quiet -- relay-scan --ipfs-bootstrap-peers --check-candidates --timeout-seconds 10 --candidate-timeout-seconds 3 --max-candidates 1
public relay scan: ok
public relay scan peers: 5
public relay scan connected: 2
public relay scan identified: 1
public relay scan relay_capable: 1
public relay candidates: 1
public relay scan validation: public relay probe: failed
public relay scan validation: public relay candidate: /ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ succeeded false failure_stage relay_reservation error relay reservation timed out connected false accepted false relayed_listen_address false last_error none
```

This confirms the validation path distinguishes a relay-hop advertisement from
reservation readiness and reports which reservation prerequisites were not
observed before timeout.

Additional widened public Kademlia scan evidence on 2026-08-04:

```text
$ nix develop -c cargo run --quiet -- relay-scan --ipfs-bootstrap-peers --timeout-seconds 45 --max-candidates 64
public relay scan: ok
public relay scan peers: 5
public relay scan total_peers: 16
public relay scan routing_peers: 11 dialed 0
public relay scan connected: 5
public relay scan identified: 13
public relay scan relay_capable: 13
public relay scan dial_failures: 3
public relay candidates: 45
```

This confirms `relay-scan --ipfs-bootstrap-peers` now goes beyond the configured
bootstrap peers and collects relay-hop candidates from peers learned through the
public IPFS Kademlia routing table. Smaller bounded scans also keep candidates
diverse by relay peer when they can replace duplicate addresses from a relay
already in the candidate set.

Additional round-robin public relay validation evidence on 2026-08-04:

```text
$ nix develop -c cargo run --quiet -- relay-scan --ipfs-bootstrap-peers --check-candidates --timeout-seconds 45 --candidate-timeout-seconds 6 --max-candidates 24
public relay scan: ok
public relay scan peers: 5
public relay scan total_peers: 10
public relay scan routing_peers: 5 dialed 0
public relay candidates: 24
public relay scan validation: public relay probe: ok
public relay scan validation: public relay probe mode: relayed_peer_circuit
public relay scan validation: public relay candidates: 6 succeeded 1
public relay scan validation: public relay candidate: /ip4/158.69.208.229/udp/4001/quic-v1/p2p/12D3KooWHtzDJvs5ziiQ2o2JEWdxSV95mFxuvS2hk1wVDAWScXeE succeeded true failure_stage none error none
public relay scan validation: public relay candidate detail: relayed peer circuits: 1 connected 1
```

This run proves public Kademlia-assisted relay discovery, public relay
reservation acceptance, and relayed circuit dialing through a routing-table
candidate. The same candidate also produced a runtime-valid relay-assisted
config through `relay-check --write-config` and `status`:

```text
$ nix develop -c cargo run --quiet -- relay-check --relay-candidate /ip4/158.69.208.229/udp/4001/quic-v1/p2p/12D3KooWHtzDJvs5ziiQ2o2JEWdxSV95mFxuvS2hk1wVDAWScXeE --write-config /tmp/p2p-vpn-public-relay-success.json --timeout-seconds 30
public relay probe: ok
public relay candidate: /ip4/158.69.208.229/udp/4001/quic-v1/p2p/12D3KooWHtzDJvs5ziiQ2o2JEWdxSV95mFxuvS2hk1wVDAWScXeE succeeded true error none
wrote /tmp/p2p-vpn-public-relay-success.json

$ nix develop -c cargo run --quiet -- status --config /tmp/p2p-vpn-public-relay-success.json
bootstrap peers: 1
relay reservations: 1
```

Public-relay-assisted DCUtR is still unproven from this host. A follow-up run
against the same relay with `--require-dcutr-success --timeout-seconds 45`
connected the relayed circuit but did not observe a successful hole punch; the
direct QUIC attempt ended with `HandshakeTimedOut`.

Additional bounded public DCUtR search evidence on 2026-08-04:

```text
$ nix develop -c cargo run --quiet -- relay-scan --ipfs-bootstrap-peers --check-candidates --require-dcutr-success --timeout-seconds 20 --candidate-timeout-seconds 5 --max-candidates 12 --max-validation-candidates 3
public relay scan: ok
public relay scan total_peers: 16
public relay scan routing_peers: 11 dialed 0
public relay candidates: 12
public relay scan validation skipped: /ip6/... reason ipv6_unreachable
public relay scan validation limited: 3 of 9 host-reachable candidates
public relay scan validation: public relay probe: failed
public relay scan validation: public relay probe mode: dcutr_success
public relay scan validation: public relay candidates: 3 succeeded 0
```

This confirms public DCUtR search runs can now be bounded after host
reachability filtering. In this sample, all three validated candidates timed
out during relay reservation; the larger unbounded run in the same environment
validated 17 host-reachable candidates with zero DCUtR successes. Several
candidates established relayed circuits, but direct hole-punch dials still
ended in `HandshakeTimedOut` or direct TCP timeout from this host.

Additional DCUtR-required public scan evidence on 2026-08-04:

```text
$ nix develop -c cargo run --quiet -- relay-scan --ipfs-bootstrap-peers --check-candidates --require-dcutr-success --timeout-seconds 45 --candidate-timeout-seconds 20 --max-candidates 12 --max-validation-candidates 6
public relay scan: ok
public relay scan total_peers: 16
public relay scan routing_peers: 11 dialed 0
public relay candidates: 12
public relay scan validation limited: 6 of 12 host-reachable candidates
public relay scan validation: public relay probe: failed
public relay scan validation: public relay probe mode: dcutr_success
public relay scan validation: public relay candidates: 6 succeeded 0
public relay scan validation: public relay candidate failure stages: candidate_setup 0 relay_reservation 5 relayed_peer_circuit 0 dcutr_success 1
public relay scan validation: public relay candidate detail: relayed peer circuits: 1 connected 1
public relay scan validation: public relay candidate detail: dcutr last_error: direct_dial: Transport([(/ip4/216.114.103.137/tcp/26649/p2p/12D3KooWKrZ4gn9B72QdxqKVnM937mUiTa4trNrRqFA8sxUMAmPE, Other(Custom { kind: Other, error: Timeout }))])
```

This confirms the public scan path can reach a public relay, establish a relayed
peer circuit, and then fail specifically at the direct DCUtR dial from this
host. It keeps public-relay-assisted DCUtR success unproven, but narrows the
remaining live gap to NAT/path conditions rather than relay discovery or relayed
circuit setup.
