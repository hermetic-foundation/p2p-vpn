# Linux Checkpoint Migration

## Status

Migration acceptance is in progress. The signed seed, protected handoff owner,
and serialized Linux command workflow are implemented. Rebuilding does not
convert an existing version-2 membership authority.

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
| `prepare_authorized_at` | Sign the daemon's validated current projection, including authorized static aliases and grants. |
| `migration_id` | Fingerprint the complete signed public seed, including its current-name plan. |
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

## Linux Integration

| Boundary | Required Behavior |
| --- | --- |
| Preparation | Current daemon projection, including static addresses/routes; preserve configured secret pins. |
| Credential handoff | One protected `membership-state.migration.json` per instance; transfer the identical file to recipients. |
| Installation | `membership checkpoint install --accept-id ...`; daemon event loop rejects authority drift and pending pairing. |
| Commit | Validate before rename; durable checkpoint before gated projection. Visible uncertain writes restore the gate. |
| Restart/retry | Restore selected v3 authority; never replace a newer selection with staged genesis. |
| Retirement | Successful installation or one-hour expiry removes the managed artifact; periodic retry, no backup/archive. |
| Consumers | Refresh membership, capabilities, TUN, DNS, discovery scope, and authorization-controlled caches. |

`instantiate_at` is not an authority-file writer and cannot detect a newer
installed checkpoint. The integration owner must enforce that guard before
replacement. `runtime::membership_migration` owns that guard. Local control RPC
uses bounded, strict JSON requests and secret-free summaries. Artifact deletion
is restricted to the daemon-owned path and requires the expected fingerprint.

### Remaining Acceptance

| Item | Status |
| --- | --- |
| Isolated multi-daemon packet/DNS migration and offline catch-up. | Still required. |
| Superseded legacy pairing artifacts. | Cleanup integration and evidence still required. |
| Combined storage bounds through repeated admission/removal cycles. | Existing checkpoint churn coverage; migration-wide campaign still required. |
| Interrupted-process temporary residue. | Write-failure coverage exists; crash-residue retirement still required. |
| Live networks. | Not migrated, revoked, or restarted by this work. |

The workflow is not a claim that the full migration goal is complete. Android
activation and complete export/native-Nix artifacts remain separate goals.

## Focused Verification

Nine core migration, six protected-artifact, and nine runtime workflow tests
pass. Socket-contract and CLI-parsing regressions also pass. The full Rust
workspace passes 1,793 tests, with 47 opt-in tests excluded.

Nix `rust-test-sources` passes offline.
Formatting and required correctness/suspicious/performance Clippy groups pass;
existing unrelated non-fatal lint warnings remain.

The offline package dry run succeeds but lists 1,377 uncached derivations. The
full package build was not started. Unprivileged network namespaces are available
for the remaining kernel/TUN campaign; this step creates no additional VM or
Android build tree.

```sh
nix develop --offline -c cargo test --lib --locked --offline \
  membership::checkpoint::migration
nix develop --offline -c cargo test --lib --locked --offline \
  runtime::membership_store::migration
nix develop --offline -c cargo test --lib --locked --offline \
  runtime::membership_migration
```

| Coverage | Assertion |
| --- | --- |
| Retention | Removed identities, inviter history, and superseded labels are absent from the seed. |
| Preservation | Descendants, keys, routes, metrics, roles, expiry, and caller-selected policy survive. |
| Names | Recipients sign their own labels; resync merges them without retaining migration proof. |
| Startup gate | Installation and restart cannot mutate or participate before resync. |
| Validation | Wrong scope/key, tampering, future/expired input, unsupported roles, and oversized encoding fail. |
| Compatibility | Existing authority files and default runtime behavior are unchanged by this primitive. |
| Protected handoff | Permission/symlink/key/scope rejection, fixed expiry, identical retries, and uncertain write/unlink recovery. |
| Cohort | Two recipients install one scope and independently sign their names; staging files are retired. |
| Installation | Preserve static aliases and canonical metrics; reject grant drift, wrong fingerprints, and locally revoked subjects. |
| Persistence | Failure before rename preserves legacy; uncertain visible replacement installs a gate; staged replay cannot roll back removal. |
| Daemon | Local control migration gates packets/mutations, completes resync autonomously, and removes a peer from durable state and inventory. |

The daemon uses fixture packet/route adapters, not real TUN. The cohort test
validates authority installation, not peer-to-peer packet/DNS convergence.
Existing formal verification assets were not found; executable regressions cover
this step. Do not infer full migration acceptance from these passing layers.

See [Checkpoint Acceptance](membership-checkpoint-acceptance.md) for the full
migration, storage-bound, and multi-daemon completion criteria.
