# Network Debugging

Use this when discovery, relay, DCUtR, packet-plane, or TUN traffic fails.

Capture artifacts before changing the environment.

## Debug Bundle

```sh
nix run .#debug-bundle
```

Choose the output directory:

```sh
P2P_VPN_DEBUG_BUNDLE_DIR=/tmp/p2p-vpn-debug \
nix run .#debug-bundle
```

Include fast checks:

```sh
P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST=1 \
nix run .#debug-bundle
```

Attach a daemon socket:

```sh
P2P_VPN_DEBUG_BUNDLE_CONTROL_SOCKET=/run/p2p-vpn/control.sock \
nix run .#debug-bundle
```

## Daemon Dump

```sh
dump_dir="/tmp/p2p-vpn-daemon-dump-$(date -u +%Y%m%dT%H%M%SZ)"

p2p-vpn daemon-dump \
  --socket /run/p2p-vpn/control.sock \
  --output-dir "$dump_dir"
```

Inspect the summary:

```sh
jq . "$dump_dir/summary.json"
```

## Health Gates

```sh
p2p-vpn daemon-health \
  --socket /run/p2p-vpn/control.sock \
  --require-validated-peers \
  --require-supported-paths \
  --wait-seconds 30
```

Additional gates:

| Gate | Use |
| --- | --- |
| `--require-packet-plane-session` | UDP packet-plane proof. |
| `--require-packet-plane-quic-session` | Owned QUIC proof. |
| `--require-auto-relay-reservation` | Auto relay convergence. |
| `--require-observed-packet-plane-udp-endpoint` | Public endpoint derivation. |
| `--require-observed-packet-plane-quic-endpoint` | Public QUIC endpoint derivation. |

## Inspect By Failure Type

| Failure | First Files Or Commands |
| --- | --- |
| Peer not validated | `daemon-peers`, control logs. |
| Route missing | `daemon-routes`, `routes --resolve`. |
| Relay not healthy | `daemon-paths`, relay counters. |
| DCUtR did not promote | logs with `event=dcutr_hole_punch_result`. |
| Packet plane missing | `daemon-capabilities`, `daemon-paths`. |
| MTU problem | `daemon-mtu`, packet-too-big counters. |

## Log Search Patterns

```sh
rg 'control capabilities|service status' node-*.log
rg 'CircuitReqAccepted|relay_reservations' node-*.log
rg 'event=dcutr|hole_punch' node-*.log
rg 'packet_plane_session|owned_quic' node-*.log
rg 'path_selected|path_promoted|path_demoted' node-*.log
```

## Public Discovery Backoff

Look for this event when a host is offline from the public internet:

```sh
rg 'public_discovery_bootstrap_backoff' node-*.log
```

The event means default public IPFS bootstrap retries are paused.

Expected behavior during the pause:

| Path | Expected |
| --- | --- |
| mDNS LAN discovery | Continues |
| Configured peer redial | Continues |
| Discovered relay redial | Continues |
| Kademlia peer record lookup | Continues |
| Public Kademlia bootstrap | Paused |

Do not treat this event as a LAN failure by itself.

## Public Address Pollution

Public discovery must not create neighbor entries for arbitrary private hosts.

```sh
ip -4 neigh show
sudo p2p-vpn daemon-status --socket /run/p2p-vpn/control.sock \
  | rg '^public_discovery_unverified_addresses_rejected '
```

| Evidence | Interpretation |
| --- | --- |
| Rejection counter rises | Public records contained non-public transports. |
| Small set of real LAN neighbors | Expected mDNS and established-path behavior. |
| Many unrelated `FAILED` private neighbors | Regression in discovery-source filtering. |

Rejection journal events are sampled at power-of-two totals.
This bounds log volume while retaining the peer, address, and ingestion surface.

## Namespace Repro

```sh
P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
nix run .#tun-e2e -- \
  tun_namespace_relay_overlay_promotes_to_direct_path \
  -- --ignored --exact --nocapture
```

Read the generated `repro-metadata.txt`.

Run `repro-commands.sh` from the artifact directory to replay the case.

## Public Relay Repro

Run a bounded public relay probe:

```sh
P2P_VPN_REPRO_PHASE_TIMEOUT_SECONDS=210 \
nix run .#public-relay-repro
```

Useful knobs:

| Variable | Use |
| --- | --- |
| `P2P_VPN_REPRO_PHASE_TIMEOUT_SECONDS` | Wall-clock cap for each repro phase. |
| `P2P_VPN_RELAY_CANDIDATE_TIMEOUT_SECONDS` | Per-candidate protocol timeout. |
| `P2P_VPN_RELAY_MAX_VALIDATION_CANDIDATES` | Number of relay candidates to probe. |
| `P2P_VPN_REPRO_REQUIRE_PUBLIC_RELAY_RESERVATION=0` | Skip the default public relay reservation proof. |
| `P2P_VPN_REPRO_MEMBERSHIP_DHT=1` | Require public DHT membership-record propagation. |
| `P2P_VPN_REPRO_REQUIRE_VPN_RELAY_RESERVATIONS=1` | Require generated local host configs to reserve on the selected relay. |
| `P2P_VPN_REPRO_REQUIRE_DCUTR=1` | Require strict DCUtR proof after relay probing. |

Each phase writes separate stdout and stderr logs.

The manifest is `repro-phase-logs.tsv`.

The reservation proof report is `public-relay-reservation-check-report.json`.
