# Network Debugging

Use this runbook when a namespace, relay, DCUtR, discovery, or packet-plane run
fails and you need artifacts that can be inspected after the command exits.

## Namespace E2E

Check host namespace/TUN prerequisites before running the slower ignored suite:

```sh
nix run .#namespace-preflight
```

Run the whole Linux namespace suite:

```sh
nix run .#tun-e2e -- -- --ignored --nocapture
```

Run the same suite with artifacts preserved by default:

```sh
nix run .#namespace-repro
```

Run a focused case:

```sh
nix run .#tun-e2e -- tun_namespace_relay_overlay_promotes_to_direct_path -- --ignored --exact --nocapture
nix run .#tun-e2e -- tun_namespace_ping_crosses_owned_quic_packet_plane -- --ignored --exact --nocapture
```

Preserve generated configs and node logs after successful runs:

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 nix run .#tun-e2e -- -- --ignored --nocapture
```

`tun-e2e` runs `namespace-preflight` by default. Set
`P2P_VPN_TUN_E2E_SKIP_PREFLIGHT=1` only when debugging the preflight itself or
when a controlled environment has already proved user namespaces, network
namespaces, veth setup, and `/dev/net/tun`.

The preserved directory is printed on stderr. It contains `node-a.log`,
`node-b.log`, and, for relay or bootstrap scenarios, `node-relay.log` or
`node-bootstrap.log`. Failed tests already include node logs, `ip addr`, and
route-table output in the assertion message.

## What To Inspect

For discovery failures, check node logs for `kademlia query progressed`,
`discovered_address_dial_attempts`, and `control capabilities accepted`.

For relay failures, check relay logs for `CircuitReqAccepted` and peer logs for
relay readiness metrics and `relay_reservations_lost`.

For DCUtR or path-promotion failures, check peer logs for `event=dcutr_enabled`,
`event=autonat_enabled`, `event=dcutr_hole_punch_result`, and
`event=path_promoted_to_direct`.

For packet-plane failures, check for `event=packet_plane_session_established`,
`backend=owned_quic` when testing owned QUIC, positive
`path_healthy_direct_quic_datagram_paths`, and positive packet counters such as
`outbound_quic_datagram_packets` or `inbound_accepted_packets`.

## Public Relay Smoke

Scan public IPFS-compatible bootstrap peers and write candidates:

```sh
p2p-vpn relay-scan \
  --ipfs-bootstrap-peers \
  --check-candidates \
  --write-candidates /tmp/p2p-vpn-relays.txt \
  --write-report /tmp/p2p-vpn-relay-scan-report.json
```

Run the packaged repro bundle:

```sh
nix run .#public-relay-repro
```

The app writes artifacts to a temporary directory and prints that path. Set
`P2P_VPN_REPRO_DIR` to choose the directory. Tune long-running network probes
with `P2P_VPN_RELAY_SCAN_TIMEOUT_SECONDS`,
`P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS`, `P2P_VPN_RELAY_MAX_CANDIDATES`, and
`P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES`. Set
`P2P_VPN_REPRO_BASE_CONFIG` to an existing overlay config when a successful
relay-circuit phase should preserve that identity, membership, routes, peers,
and packet-plane settings in the generated config. Set
`P2P_VPN_REPRO_CANDIDATES_FILE` to a nonempty newline-separated candidate file
from an earlier `relay-scan --write-candidates` or `public-relay-repro` run to
skip public discovery and replay relay-circuit/DCUtR validation immediately
against the same candidates. The repro runs discovery, relay-circuit
validation, and DCUtR validation as separate phases and preserves
`public-relay-scan-report.json`, `public-relay-check-report.json`, and
`public-relay-dcutr-report.json` when those phases can run. If relay-circuit
validation succeeds, it also writes `public-relay-config.json`, a runnable
relay-assisted config generated from the validated public relay. It exits
nonzero if any phase fails, but earlier artifacts remain in the repro
directory.
Probe reports use schema version 3 and include per-candidate
`elapsed_millis`, plus bootstrap-level `direct_connection_addresses` and
`relayed_connection_addresses` when a candidate reaches the bootstrap-check
phase. These fields help distinguish fast setup failures from full-budget
reservation, relayed-circuit, or DCUtR timeouts, and confirm which concrete path
type was observed.

Every repro directory also contains `repro-metadata.txt`,
`repro-host-network.txt`, `repro-commands.sh`, and `repro-summary.txt`. Start
with the summary when triaging a failure: it records each phase status,
phase duration, candidate counts, skipped candidates, routing-peer counts,
failure-stage counts, candidate elapsed-time ranges, and the first candidate
error from each report. For relay-check reports, it also summarizes observed
relayed and direct connection addresses so DCUtR runs show whether they reached
relay fallback only or completed direct promotion. Use the metadata file to
compare the exact timeout/candidate-limit environment between runs. Use the
host-network file to compare OS/kernel, IPv4/IPv6 route availability, interface
addresses, and route tables across machines. Use the commands file to replay
the same scan, relay-circuit, and DCUtR probes against the preserved candidate
file.

Probe candidates and write a machine-readable report:

```sh
p2p-vpn relay-check \
  --config p2p-vpn.json \
  --relay-candidates-file /tmp/p2p-vpn-relays.txt \
  --write-report /tmp/p2p-vpn-relay-report.json \
  --write-config /tmp/p2p-vpn-public-relay-config.json
```

When debugging hole punching, require DCUtR evidence so the command fails at
the relevant stage:

```sh
p2p-vpn relay-check \
  --relay-candidates-file /tmp/p2p-vpn-relays.txt \
  --require-dcutr-success \
  --write-report /tmp/p2p-vpn-dcutr-report.json
```

Inspect `repro-summary.txt` first, then the scan report peer counts, routing
peer counts, candidate addresses, candidate peer results, `failure_stage`,
bootstrap details, relay readiness, DCUtR counters, direct and relayed
connection address lists, and `last_error` fields.
