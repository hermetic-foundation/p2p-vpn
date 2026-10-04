# Linux Checkpoint Migration

## Status

Migration is in progress. The signed seed primitive is implemented; no CLI or
daemon migration command is available yet. Rebuilding does not convert an
existing version-2 membership authority.

## Ownership

| Object | Contents | Lifetime |
| --- | --- | --- |
| Legacy authority | Existing signed membership and hostname history. | Replaced only by an explicit, validated migration. |
| Signed migration seed | Active roster, canonical grants, selected policy, current names, publisher proof. | Valid for one hour; not retained in installed authority. |
| Network capability | Shared secret and pinned checkpoint anchor. | Protected authority storage; never public diagnostics. |
| Installed authority | Active snapshot and independently signed current hostname claims. | Compacted as membership changes. |

The seed contains no private key, capability secret, revoked-device profile,
inviter chain, or historical hostname records. Its publisher proof authenticates
this transfer, not a permanent controller role.

## Implemented Primitive

Source: `src/membership/checkpoint/migration.rs`.

| Operation | Contract |
| --- | --- |
| `prepare_at` | Validate caller-trusted legacy history, preserve active members and grants, and sign one common seed. |
| `decode_at` / `verify_at` | Enforce encoding, scope, time, snapshot authentication, publisher signature, and canonical names. |
| `instantiate_at` | Restore the same snapshot behind a resync gate and sign only the recipient's own hostname. |

The caller explicitly selects `SnapshotPolicy`; conversion cannot silently
replace that selection with the default policy. Governance remains any-member,
the only governance model currently implemented by the checkpoint protocol.

Latest valid self-signed legacy hostname records take precedence over current
admission labels. Removed and expired subjects are omitted. Route-only legacy
declarations are rejected because the checkpoint member model cannot represent
them without changing their roles.

### Names During Conversion

1. The common seed records each active member's current label.
2. Each recipient signs its own label under the new anchor and incarnation.
3. Ordinary authenticated resync distributes these subject-signed claims.
4. Removal discards both the member and its current claim.

No inviter signature is rewritten as a device signature. Remote DNS labels are
not installed until their subjects have migrated and their claims have synced.
The maintenance workflow must make this temporary availability boundary clear.

## Integration Still Required

| Boundary | Required Behavior |
| --- | --- |
| Preparation | Use current daemon authority, including valid static grants; refuse an incomplete roster or unsupported input. |
| Credential handoff | Transfer the same protected capability and anchor to all recipients. |
| Installation | Serialize against runtime mutations and reject stale or mismatched authority. |
| Commit | Persist atomically before exposing grants; uncertain writes stay gated. |
| Startup | Resume interrupted migration without regenerating scope or restoring legacy grants. |
| Retirement | Remove staged seeds and credentials after completion or bounded expiry; no permanent archive. |
| Consumers | Refresh TUN, DNS, discovery, connections, and normal inventories from installed authority. |

`instantiate_at` is not an authority-file writer and cannot detect a newer
installed checkpoint. The integration owner must enforce that guard before
replacement. This primitive alone does not prove safe end-to-end migration.

## Focused Verification

All nine focused tests pass. The full Rust workspace passes 1,776 tests, with
47 opt-in tests excluded. Nix `rust-test-sources` also passes offline.
Formatting and required correctness/suspicious/performance Clippy groups pass;
existing unrelated non-fatal lint warnings remain.

The offline package dry run succeeds but lists 1,377 uncached derivations. The
full package build was not started. Kernel/TUN migration tests remain for daemon
integration; this step does not require another VM or Android build tree.

```sh
nix develop --offline -c cargo test --lib --locked --offline \
  membership::checkpoint::migration
```

| Coverage | Assertion |
| --- | --- |
| Retention | Removed identities, inviter history, and superseded labels are absent from the seed. |
| Preservation | Descendants, keys, routes, metrics, roles, expiry, and caller-selected policy survive. |
| Names | Recipients sign their own labels; resync merges them without retaining migration proof. |
| Startup gate | Installation and restart cannot mutate or participate before resync. |
| Validation | Wrong scope/key, tampering, future/expired input, unsupported roles, and oversized encoding fail. |
| Compatibility | Existing authority files and default runtime behavior are unchanged by this primitive. |

These are core tests, not packaged-daemon or kernel/TUN evidence. Existing formal
verification assets were not found; executable regression tests cover this step.

See [Checkpoint Acceptance](membership-checkpoint-acceptance.md) for the full
migration, storage-bound, and multi-daemon completion criteria.
