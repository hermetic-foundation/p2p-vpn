# Overlay DNS

Overlay DNS maps authenticated members to stable private names.

It is opt-in and does not replace public DNS.

## Name Format

| Part | Value |
| --- | --- |
| Host label | Local device hostname or an approved override |
| Network label | `networkName`; defaults to the NixOS instance name |
| Private suffix | `p2p-vpn.internal` |
| Canonical name | `<host>.<network>.p2p-vpn.internal` |

Example:

```text
midi-desktop-1.monarchic-runners.p2p-vpn.internal
```

NixOS registers the network zone as a search domain.

This also permits the short query `midi-desktop-1`.

## Minimal NixOS Setup

Use the same instance name on every member:

```nix
{
  services.p2p-vpn.instances.monarchic-runners = {
    enable = true;
    dns.enable = true;
  };
}
```

Defaults:

| Setting | Default |
| --- | --- |
| `networkName` | Instance name |
| `dns.hostname` | `networking.hostName` |
| `dns.listen` | Ephemeral IPv4 loopback socket |
| `dns.ttlSeconds` | `30` |
| `resolvedIntegration` | `true` |

Apply the configuration:

```sh
sudo nixos-rebuild switch
```

Pair the node once with an authorized member.

The pairing request includes its configured DNS hostname. Approval signs that
claim unless `pair approve --hostname` supplies another label.

## Minimal JSON Setup

Add `network.dns` to an otherwise complete JSON configuration:

```json
{
  "network": {
    "name": "lab",
    "private_key": "BASE64_PRIVATE_KEY",
    "dns": {
      "enabled": true,
      "hostname": "worker-1"
    }
  },
  "peers": [
    {
      "id": "REMOTE_PEER_ID",
      "name": "worker-2"
    }
  ]
}
```

`peers[].name` is a locally trusted static claim.

Pairing and signed membership records provide transitive names without static
peer entries.

For standalone JSON daemons, resolver integration is operator-owned. A NixOS
instance using `configFile` probes the daemon and manages split DNS automatically.

## Address Records

Each authorized peer receives records for:

| Record | Values |
| --- | --- |
| `A` | Derived IPv4 plus explicit IPv4 host addresses |
| `AAAA` | Derived IPv6 plus explicit IPv6 host addresses |
| `PTR` | Preferred friendly name, otherwise peer-ID fallback |

Explicit host addresses come from `vpnIp` and `/32` or `/128` route grants.

Prefix routes do not become host records.

## Peer-ID Fallback

Every effective member has an unambiguous fallback label:

```text
peer-<encoded-overlay-peer-id>.<network>.p2p-vpn.internal
```

List the exact value:

```sh
sudo p2p-vpn dns list --instance monarchic-runners
```

Old membership records without a hostname remain usable through this fallback.

## Name Rules

Host and network labels must meet all rules:

| Rule | Requirement |
| --- | --- |
| Length | 1 through 63 bytes |
| Characters | ASCII letters, digits, and `-` |
| First character | Not `-` |
| Last character | Not `-` |
| Case | Canonicalized to lowercase |

Each DNS-enabled instance must use a unique `networkName` on one NixOS host.

## Inspect DNS

### Status

```sh
sudo p2p-vpn dns status --instance monarchic-runners
```

The output includes the listener, zone, record counts, query counters, errors,
and zone refresh results.

### Records and Conflicts

```sh
sudo p2p-vpn dns list --instance monarchic-runners
sudo p2p-vpn dns list --instance monarchic-runners --offset 256 --limit 256
```

Use JSON for scripts:

```sh
sudo p2p-vpn dns list \
  --instance monarchic-runners \
  --format json
```

### Controlled Resolution

```sh
sudo p2p-vpn dns resolve midi-desktop-1 \
  --instance monarchic-runners
sudo p2p-vpn dns resolve midi-desktop-1 \
  --instance monarchic-runners \
  --type aaaa
sudo p2p-vpn dns resolve 100.64.50.33 \
  --instance monarchic-runners \
  --type ptr
```

Types are `auto`, `a`, `aaaa`, `ptr`, and `any`.

### System Resolver

```sh
resolvectl query midi-desktop-1
resolvectl query \
  midi-desktop-1.monarchic-runners.p2p-vpn.internal
```

## Conflicts

Two effective members may present the same valid signed hostname.

The daemon handles this deterministically:

| Surface | Result |
| --- | --- |
| DNS query | No address is returned |
| `dns resolve` | `status=conflict` with bounded peer details |
| `dns list` | `dns_conflict` entry |
| Peer fallback | Both unique fallback names remain available |

Resolve a conflict by issuing a newer signed claim through pairing or membership
record management.

## Revocation and Expiry

DNS follows effective membership state.

| Event | DNS Result |
| --- | --- |
| New signed hostname | Appears after membership convergence |
| Signed rename | Replaces the old friendly name |
| Last effective grant expires | Friendly, fallback, address, and PTR records disappear |
| Last effective grant is revoked | The revoked member's records disappear |
| Daemon restart | Persisted valid claims restore before peer reconnection |

Records use a short TTL so clients stop using stale answers quickly.

## Security Boundary

| Property | Behavior |
| --- | --- |
| Listener | Numeric loopback address only |
| Authority | One overlay zone per instance |
| Recursion | Never performed |
| Unrelated forward query | Refused |
| Unrelated reverse query | Refused |
| DNSSEC | Not used; signed membership authenticates records before they are served |
| Public bootstrap peer | Never receives an overlay record by discovery alone |
| Signed claim | Validated through the membership trust graph |
| Private suffix guard | Prevents unmatched `p2p-vpn.internal` queries from leaking |

The responder supports bounded UDP and TCP DNS.

It caps request size, response size, TCP connections, and queries per connection.

## NixOS Lifecycle

The module registers each enabled zone on that instance's TUN link.

It also maintains a separate guard link for the parent private suffix.

| Event | Resolver Behavior |
| --- | --- |
| Service start | Wait for DNS status, then register listener and search domain |
| Service restart | Re-register after the TUN interface returns |
| Service failure | Remove stale per-instance resolver state |
| `systemd-resolved` restart | Recreate guard and instance registrations |
| Instance disable | Revert link DNS and remove saved runtime state |

The guard refuses unknown private-zone queries instead of forwarding them to
another resolver.

## Migration

DNS is disabled by default for backward compatibility.

Existing JSON, native Nix, and signed membership records remain valid.

| Existing Setup | Migration |
| --- | --- |
| NixOS native mode | Set `dns.enable = true` on every member |
| NixOS JSON mode | Enable DNS in JSON; keep `resolvedIntegration = true` |
| Standalone JSON | Enable DNS and configure the host resolver separately |
| Old signed grants | Re-pair for friendly names, or use fallback names |
| Static peers | Add `peers.<id>.name` or re-pair |

Changing `networkName` changes the DNS zone and invalidates network-bound signed
records. Rename every member and re-pair the overlay.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Short name fails | `resolvectl domain pvN` contains the network search domain |
| FQDN fails | `p2p-vpn dns resolve NAME --instance INSTANCE` |
| `status=nxdomain` | Membership converged and the claim is active |
| `status=conflict` | `p2p-vpn dns list --instance INSTANCE` |
| Only fallback exists | Re-pair or add an authenticated/static friendly name |
| Address is missing | Route grant, `vpnIp`, and effective membership |
| Resolver unit failed | `journalctl -u p2p-vpn-INSTANCE-resolved.service` |
| DNS daemon errors | `p2p-vpn dns status --instance INSTANCE` |

`dns status` reports `degraded=true` when a failed zone refresh has temporarily
removed all answers. The daemon retries the bounded rebuild every five seconds.

See [Network Membership](membership.md) for trust and convergence behavior.
