# Network Membership

Pairing admits a node to the whole overlay.

It does not create a permanent pairwise-only tunnel.

## Expected Mesh Behavior

Consider three nodes:

```text
A pairs with B
A pairs with C
B and C do not pair with each other
```

After convergence, all three nodes know the same signed membership history.

`B` and `C` discover each other, install derived routes, and exchange traffic.

| Task | User Action |
| --- | --- |
| Select the overlay | Use the same network name. |
| Create local identity | Let the NixOS module generate it, or provide a private key. |
| Admit a node | Pair it once with an authorized member. |
| Configure every peer | Not required. |
| Configure peer addresses | Not required. |
| Install host routes | Not required. |
| Select direct or relay paths | Not required. |

See [Pairing](pairing.md) for the code and approval workflow.

See [Overlay DNS](dns.md) for hostname conflicts, fallbacks, and expiry behavior.

## Authorization Boundary

A matching network name is only a discovery scope.

It never grants membership, packet access, or route authority.

| Input | What It Proves |
| --- | --- |
| Network name | Which overlay a message claims to target. |
| libp2p identity | Which peer owns the transport connection. |
| Signed member record | The peer is admitted by an authorized issuer. |
| Route grant | Which address or prefix that member may originate. |
| Signed hostname | Which overlay DNS label that member may claim. |
| Optional membership key | Additional shared overlay scope, not membership by itself. |

Public bootstrap and relay nodes provide reachability only.

They do not become overlay members unless they also hold a valid grant.

## Trust Graph

An overlay starts from a signed self-record that acts as a trust root.

Pairing preserves that root and the complete trust path to the new member.

| Role | Authority |
| --- | --- |
| `overlay_member` | Join the overlay and admit another member through approved pairing. |
| `route_authority` | Originate the routes listed in the signed record. |

An admitted member can pair a later node without returning to the original root.

Every receiver still validates the complete signature chain to a configured root.

## Automatic Convergence

Membership records move through two authenticated paths:

| Path | Purpose |
| --- | --- |
| Connected control protocol | Exchange a complete, paged membership snapshot. |
| Kademlia record | Publish a bounded trust-path and discovery sample. |

When a new record is accepted, the daemon automatically:

1. Rebuilds the effective member set.
2. Adds the member as a transport target.
3. Derives its built-in overlay address.
4. Installs authorized host and prefix routes on the TUN interface.
5. Starts LAN-first and public-path discovery.
6. Uses direct datagrams or streams when available.
7. Falls back through circuit relay when required.
8. Publishes authenticated DNS names when DNS is enabled.

No generated runtime address is written back into user configuration.

Network movement therefore does not pin a peer to an old underlay route.

## Updates and Conflicts

Each issuer-to-member relationship has a monotonic version:

```text
(membership_epoch, sequence)
```

| Incoming Record | Result |
| --- | --- |
| Higher version | Replaces the older record. |
| Lower version | Ignored as stale. |
| Identical version and payload | Ignored as already known. |
| Identical version, different payload | Rejected as equivocation. |
| Invalid signature or network | Rejected. |
| Untrusted issuer | Rejected. |

The same result is used after restart and regardless of arrival order.

## Revocation and Expiry

A revocation is a newer signed record with `revoked = true`.

It carries no roles, routes, or expiry and remains as a non-expiring tombstone.

| Event | Effective Result |
| --- | --- |
| Member revocation | Removes that issuer's grant to the member. |
| Issuer revocation | Removes authority derived through that issuer. |
| Root revocation | Removes the root and its delegated descendants. |
| Grant expiry | Deactivates the grant at its deadline. |
| Newer expired grant | Does not revive an older grant after restart. |

Removing a record from one config is not a network-wide revocation.

Distribute a higher-version signed revocation instead.

Manual record tooling is available through:

```sh
p2p-vpn membership-record-issue --help
p2p-vpn membership-record-verify --help
p2p-vpn membership-record-install --help
p2p-vpn membership-record-list --help
```

## Durable State

Learned history must survive a restart before peers are reachable.

The daemon stores it separately from the declarative trust anchor.

| State | NixOS Path | Contents |
| --- | --- | --- |
| Identity | `/var/lib/p2p-vpn/<instance>/private.key` | Private libp2p identity. |
| Pairing | `/var/lib/p2p-vpn/<instance>/pairing-state.json` | Encrypted pairing operations. |
| Membership | `/var/lib/p2p-vpn/<instance>/membership-state.json` | Signed learned record history. |

All files are owner-only.

Membership history is signed public authority data, not private key material.

The NixOS module enables both state paths automatically.

For a standalone JSON daemon, pass both paths:

```sh
sudo p2p-vpn up \
  --config /etc/p2p-vpn/lab.json \
  --control-socket /run/p2p-vpn/control.sock \
  --pairing-state /var/lib/p2p-vpn/pairing-state.json \
  --membership-state /var/lib/p2p-vpn/membership-state.json
```

The state file is network- and local-peer-bound.

Invalid permissions, identity mismatch, corruption, or an unknown version stop startup.

## NixOS Rebuilds and Upgrades

Generated pairing Nix stores the local trust anchor and signed admission records.

The service state directory stores records learned after that artifact was generated.

| Operation | Behavior |
| --- | --- |
| `nixos-rebuild switch` | Keeps identity, pairing state, and learned membership. |
| Daemon restart | Restores members and routes before normal network processing. |
| State format mismatch | Fails closed instead of discarding authority. |
| Lost learned state | Relearns from reachable members after startup. |
| Lost identity | Creates a different peer and requires new admission. |

Back up the identity and imported pairing Nix together.

Back up membership state when offline restart must retain the full learned mesh.

## Inspect Convergence

For a NixOS instance named `lab`:

```sh
sudo p2p-vpn daemon-capabilities \
  --socket /run/p2p-vpn-lab/control.sock
sudo p2p-vpn daemon-peers \
  --socket /run/p2p-vpn-lab/control.sock
sudo p2p-vpn daemon-routes \
  --socket /run/p2p-vpn-lab/control.sock
sudo p2p-vpn daemon-paths \
  --socket /run/p2p-vpn-lab/control.sock
```

Check these signals:

| Signal | Healthy Result |
| --- | --- |
| Local membership record count | Matches the expected signed history. |
| Peer validation | Every admitted peer validates. |
| Route owner | Each derived prefix maps to its signed member. |
| Selected path | Direct path, or `circuit_relay` when isolated. |
| Config hash | Unchanged across path movement. |

## Persistence Metrics

`daemon-status` and Prometheus output include:

| Metric | Meaning |
| --- | --- |
| `membership_state_loads` | Successful state-file loads. |
| `membership_state_records_loaded` | Records read across successful loads. |
| `membership_state_load_failures` | Rejected or unreadable state files. |
| `membership_state_persists` | Successful atomic saves. |
| `membership_state_records_persisted` | Records written across saves. |
| `membership_state_persist_failures` | Failed saves. |
| `membership_record_syncs_completed` | Completed connected-peer snapshots. |
| `membership_record_sync_failures` | Rejected or incomplete snapshots. |

Structured journal events use `membership_state_*` and `membership_record_sync_*` names.

Failure events include a stable reason or an operator action.

## Resource Limits

| Resource | Limit |
| --- | --- |
| Retained signed records | 256 |
| Encoded record | 12 KiB |
| Control records per page | 8 |
| Control message | 16 KiB |
| Concurrent snapshot syncs | 4 |
| Snapshot-change restarts | 3 |
| Failed-sync retry delay | 30 seconds |

The record limit counts issuer-to-member relationships, not only unique nodes.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Two admitted members do not see each other | Record counts and `membership_record_sync_failures`. |
| Member appears but has no route | Signed roles, route grants, and `daemon-routes`. |
| Restart loses a learned route | `--membership-state`, file mode, and load events. |
| State load stops the service | Journal reason, local peer, network, and file version. |
| Same-version records disagree | Replace them with one higher-version authoritative record. |
| Revoked descendant remains reachable | Confirm the newer revocation reached every member. |
| Public relay is connected but unauthorized | Expected; relay membership is separate from reachability. |
