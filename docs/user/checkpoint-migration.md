# Linux Checkpoint Migration

## Status

The explicit Linux workflow and isolated migration/cleanup tests pass. This is
not production migration approval; Android and complete export/Nix pairing
remain separate work. Rebuilding alone never converts an existing network.

| Supported Here | Not Included |
| --- | --- |
| Legacy Linux authority to one common checkpoint scope. | Android activation and mixed-client migration. |
| Preserved identities, IP grants, current names, and any-member governance. | Complete checkpoint configuration export and native-Nix pairing artifacts. |
| Atomic protected replacement and gated restart. | Automatic migration of your running networks. |

## Before Starting

1. Use an isolated Linux test network first.
2. Upgrade every member that will participate in the migration cohort.
3. Finish or cancel pending pairing operations.
4. Confirm peer inventory, names, addresses, and route grants agree.
5. Arrange management access independent of the overlay being converted.

| Requirement | Reason |
| --- | --- |
| Running daemon and local control socket. | The daemon serializes migration against membership updates. |
| Protected `membership-state` storage. | The old authority and new authority share one atomic replacement. |
| One common artifact for the whole cohort. | Preparing independently on each host creates unrelated network scopes. |
| Protected transfer and owner-only file permissions. | The handoff contains the shared capability secret. |
| Complete the cohort within one hour. | The handoff expires; retries never extend its deadline. |
| Configured shared key, if present, is at least 32 bytes. | Existing key pins are preserved, not silently replaced. |

For NixOS, `--instance lab` selects module-managed paths. For a manually started
daemon, replace that selector with `--socket /path/to/control.sock`.

## Prepare Once

On one member with a complete, current view:

```sh
sudo p2p-vpn membership checkpoint prepare --instance lab --format json
```

Record the returned `migration_id`, `artifact_path`, expiry, and counts.
The public summary does not contain the shared capability or private identity.

Repeating preparation returns the same artifact. If membership or grants change,
cancel the prepared artifact and prepare a new one explicitly.

## Transfer And Inspect

On each recipient:

```sh
sudo p2p-vpn membership checkpoint inspect --instance lab --format json
```

1. Note its daemon-owned `artifact_path`.
2. Transfer the prepared file there using an authenticated management path.
3. Keep the file owner-only (`0600`) and its parent non-writable by others.
4. Never overwrite a different prepared artifact or an authority file.
5. Run `inspect` again and compare `migration_id` with the source.

The artifact contains a capability secret. Do not print its contents, place it
in the Nix store, commit it, or attach it to logs. Delete any transfer copies
after installation; the daemon only manages the path it reported.

## Install Explicitly

On every cohort member, use the same inspected fingerprint:

```sh
sudo p2p-vpn membership checkpoint install \
  --instance lab --accept-id '<migration_id>' --format json
```

| Result | Meaning |
| --- | --- |
| `installed` | Checkpoint authority is durable; packet access remains gated until resync. |
| `cleanup_pending` | Authority is installed, but handoff or pairing-state retirement needs retry or repair. |
| Fingerprint or grant error. | No conversion; resolve inconsistent state before retrying. |
| Uncertain write error. | Do not delete authority or regenerate scope; inspect and retry the same operation. |

Installation rejects a missing active member, changed identity or expiry,
missing existing grant, differing current name, or locally revoked subject.
It never replaces a newer installed checkpoint with the handoff's genesis.

Static peers whose IDs do not embed public keys need signed admissions before
migration. Unsupported route-only legacy declarations are rejected, not dropped.

## Verify Convergence

```sh
sudo p2p-vpn state --instance lab
sudo p2p-vpn peers --instance lab
sudo p2p-vpn membership checkpoint inspect --instance lab --format json
```

Check participation, expected addresses, and peer-to-peer packet/DNS reachability.
Do not equate a successful local installation with completed cohort migration.

Each subject signs its own name under the new scope. Remote names become
available after those subjects migrate and resync, not by converting inviter
signatures into device signatures.

## Cancel A Prepared Handoff

```sh
sudo p2p-vpn membership checkpoint cancel \
  --instance lab --accept-id '<migration_id>'
```

Cancellation erases only the prepared handoff. It does not roll back installed
authority. Installed and expired daemon-owned handoffs are also retired
automatically, with cleanup retried on the maintenance loop.

## Consistency And Retention

- One online member can advance; offline peers do not block checkpoints.
- Returning peers authenticate and resync before packet access or mutations.
- Selected authority retains active members and current claims, not inviter history.
- Cooperative fork reconciliation may discard losing-branch changes, including revocations.
- A discarded revocation may need to be issued again after resync.

This is cooperative eventual consistency, not Byzantine consensus. More updates
or members are a branch-selection heuristic, not costly proof of work.

This workflow replaces daemon-owned authority. It does not rewrite personal
flakes, external backups, historical Nix generations, or system journals.

### Pairing Results After Migration

| State | Retention |
| --- | --- |
| Completed legacy results and polling responses. | Removed once checkpoint authority is installed; old operation IDs stop resolving. |
| Unfinished pairing transactions. | Preserved until their commit or cancellation cleanup finishes. |
| Current checkpoint pairing results. | Available until expiry, removal, or a changed member incarnation. |
| Acknowledgement receipts and opaque replay guards. | Capacity-bounded, expiring state; never a permanent device archive. |

Restart and periodic maintenance erase expired copies from protected pairing
storage. Cleanup failures are retried; a full replay window is not evicted early
to claim successful compaction. Keep the protected state directory writable.

### Interrupted Writes

- Recognized abandoned write copies retire when protected state is next loaded or saved.
- Temporary copies are never recovered as membership authority or kept as backups.
- Unsafe links or permissions require inspection; cleanup does not sweep unrelated files.
- Upgrade and stop old writers before using the same state directory with the new daemon.
