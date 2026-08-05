# Network Debugging

Use this runbook when a namespace, relay, DCUtR, discovery, or packet-plane run
fails and you need artifacts that can be inspected after the command exits.

## Namespace E2E

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
`P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES`. The repro runs discovery,
relay-circuit validation, and DCUtR validation as separate phases and preserves
`public-relay-scan-report.json`, `public-relay-check-report.json`, and
`public-relay-dcutr-report.json` when those phases can run. If relay-circuit
validation succeeds, it also writes `public-relay-config.json`, a runnable
relay-assisted config generated from the validated public relay. It exits
nonzero if any phase fails, but earlier artifacts remain in the repro
directory.

Probe candidates and write a machine-readable report:

```sh
p2p-vpn relay-check \
  --relay-candidates-file /tmp/p2p-vpn-relays.txt \
  --write-report /tmp/p2p-vpn-relay-report.json
```

When debugging hole punching, require DCUtR evidence so the command fails at
the relevant stage:

```sh
p2p-vpn relay-check \
  --relay-candidates-file /tmp/p2p-vpn-relays.txt \
  --require-dcutr-success \
  --write-report /tmp/p2p-vpn-dcutr-report.json
```

Inspect scan report peer counts, routing peer counts, candidate addresses,
candidate peer results, `failure_stage`, bootstrap details, relay readiness,
DCUtR counters, direct connection counts, and `last_error` fields.
