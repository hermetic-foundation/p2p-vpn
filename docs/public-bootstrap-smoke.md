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

This first check proves public IPFS-compatible bootstrap connectivity and
AutoNAT observation from an unprivileged process. Later 2026-08-04 runs below
prove public Kademlia-assisted relay discovery, public relay reservation
acceptance, relayed peer circuit dialing, and relay-assisted config generation.
Public-relay-assisted DCUtR success remains unproven.

To reproduce or extend public relay evidence, and to keep searching for the
remaining DCUtR success proof, first scan configured or public bootstrap peers
for peers that advertise the circuit-relay v2 hop protocol:

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
the candidate must also prove public-relay-assisted hole punching plus a direct
non-relayed post-punch connection to the target peer. Validation
tries scanned candidates round-robin by relay peer, so a scan with many
addresses for one relay still tests other relays before cycling through that
peer's alternate addresses. Within each relay peer, validation tries
QUIC-capable addresses before TCP addresses so bounded public DCUtR searches
spend early attempts on transports more likely to hole punch. Use
`--write-candidates PATH` to save the ordered direct relay candidates as
newline-separated multiaddrs for later `relay-check --relay-candidate` runs.
Hosts without a
usable IPv4 or IPv6 route skip relay candidates that require that address
family during validation and print each skip with `reason ipv4_unreachable` or
`reason ipv6_unreachable`, while still showing the candidate in the scan output.
Use `--max-validation-candidates N` to bound each validation pass after host
reachability filtering; this is especially useful with `--require-dcutr-success`
because each public relay has a single end-to-end reservation plus circuit/DCUtR
timeout budget. Add `--write-config PATH` with `--check-candidates` to write a
default relay-assisted config from the first validated scanned candidate; without
`--config`, that config uses the public IPFS profile plus the validated relay
shortcut. When the scan uses `--config p2p-vpn.json`, the output preserves that
config's overlay identity, discovery policy, membership, peers, routes, queue
limits, and packet-plane settings and adds only the validated relay bootstrap and
reservation infrastructure.

Then run the live relay smokes with a known-good relay or the scanned candidate
set. The preferred rootless operator command is:

```sh
nix develop -c cargo run -- relay-check \
  --relay-candidates-file public-relay-candidates.txt \
  --write-report public-relay-check.json \
  --timeout-seconds 45

nix develop -c cargo run -- relay-check \
  --relay-candidate /dns4/relay-a.example.net/tcp/4001/p2p/RELAY_A \
  --relay-candidate /dns4/relay-b.example.net/tcp/4001/p2p/RELAY_B \
  --timeout-seconds 45

nix develop -c cargo run -- relay-check \
  --relay-candidate /dns4/relay-a.example.net/tcp/4001/p2p/RELAY_A \
  --relay-candidate /dns4/relay-b.example.net/tcp/4001/p2p/RELAY_B \
  --require-dcutr-success \
  --max-validation-candidates 4 \
  --write-report public-relay-dcutr.json \
  --timeout-seconds 45

nix develop -c cargo run -- relay-check \
  --relay-candidate /dns4/relay-a.example.net/tcp/4001/p2p/RELAY_A \
  --write-config p2p-vpn-public-relay.json \
  --timeout-seconds 45
```

The command tries candidates until one works, reports per-candidate failures,
and requires direct relay multiaddrs with `/p2p/RELAY` but without
`/p2p-circuit`. Repeated candidates are ordered round-robin by relay peer, with
QUIC-capable addresses before TCP alternates for the same relay, matching
`relay-scan --check-candidates`. Before probing, it skips relay candidates that
require IPv4 or IPv6 when the local host has no usable route for that address
family and prints each skip as `public relay check skipped: ... reason
ipv4_unreachable` or `reason ipv6_unreachable`. Use
`--max-validation-candidates N` to bound the manual check after host
reachability filtering. When that validation cap is set, `relay-check` accepts a
larger bounded scan artifact and applies ordering, host reachability filtering,
and truncation before it opens public relay probes. Reservation setup failures
identify whether the probe connected directly to the relay, whether relay
reservation acceptance or relayed listen-address publication timed out, and the
last direct relay dial error when one was observed. Candidate lines also include
a stable `failure_stage` value: `candidate_setup`, `relay_reservation`,
`relayed_peer_circuit`, `dcutr_success`, or `none` for a usable candidate.
Probe output also includes a
`public relay candidate failure stages:` summary with per-stage counts across
the attempted set. Failures after reservation setup include the same detailed
bootstrap-check lines that successful candidates print, so failed
relayed-circuit and DCUtR probes show which prerequisite was missing. DCUtR
success mode gives both temporary nodes TCP and QUIC direct listen sockets and
requires both libp2p's hole-punch event and a direct non-relayed connection to
the target peer. DCUtR failures also include the last libp2p hole-punch error
as `dcutr last_error`, which distinguishes cases such as no direct connection,
rejected relay prerequisites, or direct handshake timeouts.
Use `--write-report PATH` to persist the same probe outcome as pretty-printed
JSON, including schema version, probe mode, timeout, validation cap,
host-reachable candidates, skipped candidates with reasons, per-candidate
success, failure stage, error, and bootstrap/DCUtR summary fields for candidates
that reached the bootstrap-check phase.
Successful candidates also print a `public relay candidate config:` line with
the exact `--relay-peer
PEER=MULTIADDR` shortcut and full `--relay-reservation .../p2p-circuit`
address to feed into `init-config`. Use `--write-config PATH` to write that
default relay-assisted config automatically after the first candidate validates;
the relay is treated as reachability infrastructure, not as a VPN peer.
Use `--relay-candidates-file PATH` to consume the newline-separated candidate
file from `relay-scan --write-candidates`; repeated `--relay-candidate` flags
and file candidates can be combined in one run before host reachability
filtering and validation limits are applied.

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
remains supported for a single relay. Set
`P2P_VPN_LIVE_RELAY_TIMEOUT_SECONDS` when public relay or DCUtR candidates need
more or less than the default 45-second probe budget.

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

Report-driven public relay evidence on 2026-08-04:

```text
$ nix develop -c cargo run --quiet -- relay-scan --ipfs-bootstrap-peers --timeout-seconds 45 --max-candidates 32 --write-candidates /tmp/p2p-vpn-public-relay-candidates.txt --force
public relay scan: ok
public relay scan peers: 5
public relay scan total_peers: 16
public relay scan routing_peers: 11 dialed 0
public relay scan connected: 5
public relay scan relay_capable: 8
public relay candidates: 32

$ nix develop -c cargo run --quiet -- relay-scan --ipfs-bootstrap-peers --timeout-seconds 45 --max-candidates 8 --write-candidates /tmp/p2p-vpn-public-relay-candidates-8.txt --force
public relay scan: ok
public relay candidates: 8

$ nix develop -c cargo run --quiet -- relay-check --relay-candidates-file /tmp/p2p-vpn-public-relay-candidates-8.txt --max-validation-candidates 8 --write-report /tmp/p2p-vpn-public-relay-check.json --write-config /tmp/p2p-vpn-public-relay-config.json --force --timeout-seconds 45
public relay probe: ok
public relay probe mode: relayed_peer_circuit
public relay candidates: 5 succeeded 1
public relay candidate failure stages: candidate_setup 0 relay_reservation 4 relayed_peer_circuit 0 dcutr_success 0
wrote /tmp/p2p-vpn-public-relay-check.json
wrote /tmp/p2p-vpn-public-relay-config.json

$ nix develop -c cargo run --quiet -- relay-check --relay-candidates-file /tmp/p2p-vpn-public-relay-candidates-8.txt --max-validation-candidates 8 --require-dcutr-success --write-report /tmp/p2p-vpn-public-relay-dcutr.json --force --timeout-seconds 60
public relay probe: failed
public relay probe mode: dcutr_success
public relay candidates: 8 succeeded 0
public relay candidate failure stages: candidate_setup 0 relay_reservation 4 relayed_peer_circuit 0 dcutr_success 4
wrote /tmp/p2p-vpn-public-relay-dcutr.json
```

The relayed-circuit report proved public IPFS bootstrap, relay candidate
discovery, public relay reservation acceptance, relayed circuit dialing, and
relay-assisted config generation in one repeatable flow. The DCUtR report
proved four candidates reached the DCUtR proof stage after relayed-circuit
setup, but each direct dial timed out from this host; public-relay-assisted
DCUtR success remains the outstanding live evidence gap.

Packaged public relay repro evidence on 2026-08-05:

```text
$ P2P_VPN_REPRO_DIR=/tmp/p2p-vpn-public-relay-repro-2026-08-05T0145 \
  P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS=30 \
  P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS=45 \
  P2P_VPN_RELAY_MAX_CANDIDATES=8 \
  P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES=8 \
  nix run .#public-relay-repro
public relay scan: ok
public relay scan peers: 5
public relay scan total_peers: 16
public relay scan routing_peers: 11 dialed 0
public relay scan connected: 5
public relay scan relay_capable: 10
public relay candidates: 8
public relay probe mode: relayed_peer_circuit
public relay candidates: 4 succeeded 1
public relay candidate failure stages: candidate_setup 0 relay_reservation 3 relayed_peer_circuit 0 dcutr_success 0
wrote /tmp/p2p-vpn-public-relay-repro-2026-08-05T0145/public-relay-check-report.json
wrote /tmp/p2p-vpn-public-relay-repro-2026-08-05T0145/public-relay-config.json
public relay probe mode: dcutr_success
public relay candidates: 8 succeeded 0
public relay candidate failure stages: candidate_setup 0 relay_reservation 4 relayed_peer_circuit 0 dcutr_success 4
wrote /tmp/p2p-vpn-public-relay-repro-2026-08-05T0145/public-relay-dcutr-report.json
```

The generated `repro-summary.txt` reported a 1-second scan, 136-second
relay-circuit validation, and 360-second DCUtR validation. The relay-circuit
phase proved one public QUIC relay candidate
(`/ip4/45.32.205.244/udp/4001/quic-v1/...`) and wrote a runnable
relay-assisted config. The DCUtR-required phase reached four relayed circuits
and recorded one non-relayed direct connection address, but that address was a
private `/ip4/192.168.0.180/...` address and no libp2p DCUtR success event was
observed. Public-relay-assisted DCUtR therefore remains unproven from this host;
the new summary path-evidence fields make the distinction visible without
hand-parsing the JSON reports.

For repeated runs against the same public candidate set, source the generated
`repro-retry-env.sh` from a previous repro directory and then run
`nix run .#public-relay-repro`; override `P2P_VPN_REPRO_DIR` after sourcing to
write the retry into a fresh directory. The generated `repro-phases.tsv`
records each phase status with UTC start/end timestamps and elapsed seconds for
cross-host comparison. When debugging one known relay, set
`P2P_VPN_REPRO_RELAY_CANDIDATE` to its direct `/p2p/RELAY` multiaddr; the
packaged repro writes that single candidate to its candidate file and skips
public discovery.
