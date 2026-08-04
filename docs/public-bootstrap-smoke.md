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
  --candidate-timeout-seconds 45 \
  --timeout-seconds 30

nix develop -c cargo run -- relay-scan \
  --ipfs-bootstrap-peers \
  --check-candidates \
  --write-config p2p-vpn-public-relay.json \
  --candidate-timeout-seconds 45 \
  --timeout-seconds 30

nix develop -c cargo run -- relay-scan \
  --bootstrap-peer PEER_ID=/dnsaddr/bootstrap.example.net/p2p/PEER_ID \
  --timeout-seconds 30
```

`relay-scan` reports direct `/p2p/RELAY` candidate multiaddrs. Treat these as
candidate hints only; the peer can advertise relay-hop support and still reject
reservations because of load, policy, or resource limits. With
`--check-candidates`, the command immediately runs the same reservation and
relayed-circuit validation as `relay-check`; add `--require-dcutr-success` when
the candidate must also prove public-relay-assisted hole punching. Add
`--write-config PATH` with `--check-candidates` to write a default
relay-assisted config from the first validated scanned candidate. When the scan
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
`/p2p-circuit`. Reservation setup failures identify whether relay reservation
acceptance or relayed listen-address publication timed out. Failures after
reservation setup include the same detailed bootstrap-check lines that
successful candidates print, so failed relayed-circuit and DCUtR probes show
which prerequisite was missing. Successful candidates also print a
`public relay candidate config:` line with the exact `--relay-peer
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
public relay scan validation: public relay candidate: /ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ succeeded false error relay reservation timed out accepted false relayed_listen_address false
```

This confirms the validation path distinguishes a relay-hop advertisement from
reservation readiness and reports which reservation prerequisites were not
observed before timeout.
