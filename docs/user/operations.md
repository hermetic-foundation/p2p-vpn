# Operations

Use these commands after a daemon is running.

## Start

```sh
sudo p2p-vpn up \
  --config /etc/p2p-vpn/p2p-vpn.json \
  --control-socket /run/p2p-vpn/control.sock \
  --pairing-state /var/lib/p2p-vpn/pairing-state.json \
  --membership-state /var/lib/p2p-vpn/membership-state.json
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

### NixOS Instances

List every prepared NixOS instance:

```sh
sudo p2p-vpn instance list
```

The table maps each instance to its overlay network, TUN interface, and local
peer ID.

Inspect one instance:

```sh
sudo p2p-vpn instance show monarchic-runners
```

Use JSON for scripts:

```sh
sudo p2p-vpn instance list --format json
sudo p2p-vpn instance show monarchic-runners --format json
```

NixOS runtime configurations are root-only because they contain injected
credentials. These commands emit public identity metadata only.

### Network Peers

List effective members across every prepared NixOS instance:

```sh
sudo p2p-vpn peers
```

The command queries every daemon concurrently. It fails instead of returning a
partial inventory when one instance is unavailable.

| Column | Value |
| --- | --- |
| `INSTANCE` | NixOS module instance containing the peer |
| `HOSTNAMES` | Authenticated short names; `-` when unnamed |
| `IPV4` | Identity-derived and explicit IPv4 host addresses |
| `STATE` | Signed-ledger state, or `configured` for a static-only peer |
| `INVITED_BY` | First admission inviter hostname or peer ID; `genesis` for a root |
| `LOCAL` | Whether the row is local to that instance |
| `PEER_ID` | libp2p public identity |

Inspect one instance:

```sh
sudo p2p-vpn peers --instance monarchic-runners
```

Both views include local, explicitly configured, and transitive signed members.

Use JSON for an all-instance inventory:

```sh
sudo p2p-vpn peers --format json
```

| JSON field | Type |
| --- | --- |
| `schema_version` | Aggregate schema integer; currently `1` |
| `instance_count` | Number of queried instances |
| `peers[].instance` | NixOS module instance name |
| `peers[].network` | Overlay network name |
| `peers[].peer_id` | libp2p peer ID |
| `peers[].hostnames` | Sorted hostname array |
| `peers[].ipv4` | Sorted IPv4 array |
| `peers[].ipv6` | Sorted IPv6 array |
| `peers[].local` | Boolean |
| `peers[].membership.state` | `configured`, `active`, `revoked`, `expired`, or `inactive` |
| `peers[].membership.effective_inviter` | Current admission inviter identity and optional hostname |
| `peers[].membership.original_inviter` | First admission inviter identity and optional hostname |
| `peers[].membership.admitted_at_unix_seconds` | Current admission time |
| `peers[].membership.original_admitted_at_unix_seconds` | First admission time |
| `peers[].membership.state_changed_at_unix_seconds` | Effective state-record time |

Use a control socket outside the NixOS module:

```sh
sudo p2p-vpn peers --socket /run/p2p-vpn/control.sock
```

Use the existing single-network JSON schema with an explicit target:

```sh
sudo p2p-vpn peers \
  --instance monarchic-runners \
  --format json
```

| JSON field | Type |
| --- | --- |
| `schema_version` | Single-network schema integer; currently `1` |
| `network` | Network name |
| `peers[].peer_id` | libp2p peer ID |
| `peers[].hostnames` | Sorted hostname array |
| `peers[].ipv4` | Sorted IPv4 array |
| `peers[].ipv6` | Sorted IPv6 array |
| `peers[].local` | Boolean |
| `peers[].membership` | Optional signed-ledger state and admission provenance |

Revoked, expired, and inactive signed members remain visible for audit.

Their derived addresses are omitted unless declarative configuration still
authorizes them independently.

Legacy config inspection remains available:

```sh
p2p-vpn peers --config p2p-vpn.json
p2p-vpn peers --config p2p-vpn.json --live
```

## Zsh Completion

Nix installs the completion definition into the package automatically.

Generate it manually for another installation:

```sh
mkdir -p ~/.zfunc
p2p-vpn completions zsh > ~/.zfunc/_p2p-vpn
```

Add the directory before `compinit` in `~/.zshrc`:

```zsh
fpath=(~/.zfunc $fpath)
autoload -Uz compinit
compinit
```

## Pairing Operations

Use `--instance NAME` to target a NixOS daemon.

| Command | Purpose |
| --- | --- |
| `pair open` | Create a one-time code on an inviter. |
| `pair join CODE` | Discover and authenticate the inviter. |
| `pair status OPERATION` | Show progress, candidate, and path diagnostics. |
| `pair approve OPERATION APPROVAL` | Grant membership and selected routes. |
| `pair reject OPERATION APPROVAL` | Reject a pending candidate. |
| `pair cancel OPERATION` | Stop a local unfinished operation. |
| `pair artifacts OPERATION` | Render secret-free native Nix. |
| `pair acknowledge OPERATION` | Compact installed durable enrollment state. |

Example:

```sh
sudo p2p-vpn pair status OPERATION --instance monarchic-runners
```

Pairing diagnostics identify LAN, public routing, and relay recovery stages.

See [Pairing](pairing.md) for the complete approval and persistence workflow.

### Daemon Views

| Command | Output |
| --- | --- |
| `peers --instance NAME` | Effective peer names, addresses, and identities. |
| `daemon-status` | Metrics counters. |
| `daemon-state` | Runtime state summary. |
| `daemon-peers` | Peer membership and validation. |
| `daemon-routes` | Compiled route table. |
| `daemon-paths` | Direct, datagram, and relay paths. |
| `daemon-mtu` | MTU and fragmentation policy. |
| `daemon-capabilities` | Local and peer capability state. |
| `dns status` | Resolver listener, zone, counters, and refresh state. |
| `dns list` | Forward records, PTR records, and conflicts. |
| `dns resolve` | Controlled short-name, FQDN, or reverse lookup. |

`daemon-paths` and `daemon-state` include `connection_id` for live paths.

Direct QUIC stream and relay stream fallback use that ID to pin packets to
the selected connection.

Useful packet-path counters:

| Counter | Meaning |
| --- | --- |
| `outbound_quic_datagram_packets` | Packets sent over QUIC datagram packet plane. |
| `outbound_direct_quic_stream_fallback_packets` | Packets sent over direct QUIC stream fallback. |
| `outbound_direct_tcp_stream_fallback_packets` | Packets sent over direct TCP stream fallback. |
| `outbound_relay_stream_fallback_packets` | Packets sent over relay stream fallback. |

Useful membership counters:

| Counter | Meaning |
| --- | --- |
| `membership_record_syncs_completed` | Complete signed snapshots merged from peers. |
| `membership_record_sync_failures` | Snapshot validation or transfer failures. |
| `membership_records_accepted` | Records accepted through connected-peer sync. |
| `membership_state_loads` | Successful durable-state loads. |
| `membership_state_load_failures` | Rejected or unreadable state files. |
| `membership_state_persists` | Successful atomic saves. |
| `membership_state_persist_failures` | Failed saves. |
| `unauthorized_connections_dropped` | Rejected or quarantined transport connections. |
| `public_routing_peers` | Connected routing-only peers using the configured Kademlia protocol. |
| `public_discovery_unverified_addresses_rejected` | Unverified address scopes ignored from public discovery. |

Useful routing-role journal events:

| Event | Meaning |
| --- | --- |
| `public_routing_peer_identified` | Peer supports the configured Kademlia protocol. |
| `membership_probe_peer_classified` | Pending peer became routing-only infrastructure. |
| `public_routing_peer_promoted` | Signed membership promoted the peer into the overlay. |
| `rejected_control_probe_disconnected` | Routing peer attempted an invalid overlay exchange. |
| `public_discovery_address_rejected` | A sampled public record used a non-public transport. |

Example:

```sh
sudo p2p-vpn daemon-paths \
  --socket /run/p2p-vpn/control.sock
```

Overlay DNS examples:

```sh
sudo p2p-vpn dns status --instance monarchic-runners
sudo p2p-vpn dns list --instance monarchic-runners
sudo p2p-vpn dns resolve midi-desktop-1 \
  --instance monarchic-runners
```

See [Overlay DNS](dns.md) for resolver and conflict diagnostics.

## JSON Output

Daemon view commands support JSON:

```sh
sudo p2p-vpn daemon-paths \
  --socket /run/p2p-vpn/control.sock \
  --format json
```

`peers --format json` returns structured fields rather than diagnostic lines.

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
| Mesh inventory differs | membership record count and sync failure counters. |
| Learned routes vanish on restart | membership state path and `membership_state_load_*`. |
| Overlay name does not resolve | `dns status`, `dns list`, and `resolvectl query`. |
| DNS conflict | `dns resolve` reports `status=conflict`; use peer fallbacks. |
| Rejected peer reconnects continuously | `unauthorized_connections_dropped`, `public_routing_peers`, and quarantine journal events. |

See [Network Membership](membership.md) for convergence and state recovery.
