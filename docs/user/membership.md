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

## Flat Membership Ledger

An overlay starts from a signed self-record that acts as a trust root.

That record identifies the network history. It does not remain an owner account.

The default governance policy is `any-member`.

| Role | Authority |
| --- | --- |
| `overlay_member` | Join the overlay, admit members, revoke members, and resign. |
| `route_authority` | Originate the routes listed in the signed record. |

| `any-member` Action | Allowed Actor |
| --- | --- |
| Admit a peer | Any active signed member. |
| Revoke a peer | Any active signed member. |
| Resign | The active local member. |
| Re-admit a peer | Any active signed member, at a higher epoch. |

An admitted member can pair a later node without returning to the original root.

Each admission remains valid after its inviter leaves.

Inviter identity is retained as audit provenance, not ongoing authority.

This policy has no permanent owner or administrator account.

A compromised active member can admit or revoke peers. Revoke that identity and
rotate any shared membership key after recovering control from another member.

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

Each member has a network-wide effective state selected by:

```text
(membership_epoch, sequence)
```

| Incoming Record | Result |
| --- | --- |
| Higher version | Wins the member's effective state. |
| Lower version | Remains audit history but does not win. |
| Identical version and payload | Ignored as already known. |
| Identical version, different payload | Rejected as equivocation. |
| Invalid signature or network | Rejected. |
| Untrusted issuer | Rejected. |
| Issue time up to 60 seconds ahead | Retained, then activated at its signed time. |
| Issue time over 60 seconds ahead | Rejected. |

The same result is used after restart and regardless of arrival order.

## Security Limits

Signed membership records authenticate an identity and exact payload.

They do not provide a trusted timestamp service.

| Time Rule | Guarantee |
| --- | --- |
| Up to 60 seconds ahead | Allows bounded clock skew. |
| More than 60 seconds ahead | Rejects obvious forward dating. |
| Older signed time | Cannot prove when the signer created the record. |

A compromised member key can create a new record with an older claimed time.
Revoking that member cannot prove that a newly discovered record was made later.

The default `any-member` policy is intended for networks whose active members
are trusted to govern the whole membership set.

### Compromise Recovery

1. Revoke the compromised Peer ID.
2. Remove any static `peers` authorization for it.
3. Rotate the optional shared membership key.
4. Review `membership-record-list` output from the compromised issuer.
5. Revoke unwanted admissions or issue corrective higher-epoch records.

Rotating the shared key blocks its old discovery scope.

It does not remove signed records that nodes already accepted.

Networks that require administrator roles, quorum approval, or trusted event
ordering need a stronger governance protocol than `any-member`.

## Revocation and Expiry

A revocation is a newer member-state record with `revoked = true`.

It carries no roles, routes, or expiry and remains as a non-expiring tombstone.

| Event | Effective Result |
| --- | --- |
| Member revocation | Removes that member globally after convergence. |
| Inviter revocation | Keeps independently admitted members active. |
| Creator resignation | Keeps the remaining network active and governable. |
| Self-resignation | Removes only the resigning member. |
| Grant expiry | Deactivates the grant at its deadline. |
| Newer expired grant | Does not revive an older grant after restart. |

Any active signed member can revoke another signed member:

```sh
sudo p2p-vpn membership revoke PEER_ID --instance lab
```

A member can leave without deleting the network:

```sh
sudo p2p-vpn membership resign --instance lab
```

The daemon first offers the signed resignation to members connected at command
time. It stops packet and route authorization immediately.

An isolated member cannot publish while it has no path to another member.
Reconnect it before resigning when immediate network-wide convergence matters.

On Android, use **Revoke** in a peer row or **Resign membership** in network
details.

Pair the peer again to readmit it. Re-admission advances the membership epoch.

The daemon stays available for re-pairing but stops authorizing signed peers.

Declarative `peers` entries remain independent authorization.

Remove such entries from Nix or JSON before revoking or resigning.

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
| Superseded applied enrollment | Compacts to a receipt and keeps current declarative authority. |
| State format mismatch | Fails closed instead of discarding authority. |
| Lost learned state | Relearns from reachable members after startup. |
| Lost identity | Creates a different peer and requires new admission. |

| Existing Input | Upgrade Behavior |
| --- | --- |
| Version 1 membership state | Loads with no mutable hostname records, then writes version 2. |
| Older issuer-based history | Keeps configured issuers as compatibility roots. |
| History with an explicit self-record | Uses strict flat-ledger authorization. |
| Existing Nix or JSON member records | Remains valid; no manual rewrite is required. |

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

The record limit counts retained admission, update, and revocation history.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Two admitted members do not see each other | Record counts and `membership_record_sync_failures`. |
| Member appears but has no route | Signed roles, route grants, and `daemon-routes`. |
| Restart loses a learned route | `--membership-state`, file mode, and load events. |
| State load stops the service | Journal reason, local peer, network, and file version. |
| Same-version records disagree | Replace them with one higher-version authoritative record. |
| Record is rejected as future-dated | Synchronize system time; accepted skew is 60 seconds. |
| Revoked member remains reachable | Remove any declarative `peers` entry, then confirm the tombstone converged. |
| Inviter was revoked but invitees remain | Expected; admissions are independent. |
| Public relay is connected but unauthorized | Expected; relay membership is separate from reachability. |
