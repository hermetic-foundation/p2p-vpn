# Operations

Use these commands after a daemon is running.

## Start

```sh
sudo p2p-vpn up \
  --config /etc/p2p-vpn/p2p-vpn.json \
  --control-socket /run/p2p-vpn/control.sock
```

## Health Check

```sh
sudo p2p-vpn daemon-health \
  --socket /run/p2p-vpn/control.sock \
  --require-validated-peers \
  --require-supported-paths \
  --wait-seconds 30
```

## Inspect

| Command | Output |
| --- | --- |
| `daemon-status` | Metrics counters. |
| `daemon-state` | Runtime state summary. |
| `daemon-peers` | Peer membership and validation. |
| `daemon-routes` | Compiled route table. |
| `daemon-paths` | Direct, datagram, and relay paths. |
| `daemon-mtu` | MTU and fragmentation policy. |
| `daemon-capabilities` | Local and peer capability state. |

Example:

```sh
sudo p2p-vpn daemon-paths \
  --socket /run/p2p-vpn/control.sock
```

## JSON Output

Daemon view commands support JSON:

```sh
sudo p2p-vpn daemon-paths \
  --socket /run/p2p-vpn/control.sock \
  --format json
```

## Remote Views

Use remote views when you can reach a peer but not its control socket.

```sh
p2p-vpn peer-status --config p2p-vpn.json --peer PEER_ID
p2p-vpn paths --config p2p-vpn.json --live
p2p-vpn mtu --config p2p-vpn.json --live
```

## Debug Bundle

Capture host and daemon state:

```sh
P2P_VPN_DEBUG_BUNDLE_CONTROL_SOCKET=/run/p2p-vpn/control.sock \
nix run .#debug-bundle
```

Add fast checks to the same bundle:

```sh
P2P_VPN_DEBUG_BUNDLE_RUN_CHECK_FAST=1 nix run .#debug-bundle
```

## Stop

```sh
sudo p2p-vpn daemon-shutdown \
  --socket /run/p2p-vpn/control.sock
```

## Common Failure Checks

| Symptom | Check |
| --- | --- |
| No peer validates | `daemon-peers`, network name, membership key. |
| No traffic flows | `daemon-routes`, source address, route ownership. |
| Relay not selected | `daemon-paths`, relay reservation counters. |
| Datagram path missing | packet-plane listener, endpoints, direct path. |
| Public discovery idle | bootstrap peers, Kademlia, AutoNAT status. |
