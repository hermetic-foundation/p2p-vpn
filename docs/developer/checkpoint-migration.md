# Linux Checkpoint Migration

## Status

Migration acceptance is in progress. The signed seed, protected handoff owner,
serialized Linux command workflow, and isolated native packet/DNS campaign are
implemented. Rebuilding does not convert existing version-2 membership authority.

## Ownership

| Object | Contents | Lifetime |
| --- | --- | --- |
| Legacy authority | Existing signed membership and hostname history. | Replaced only by an explicit, validated migration. |
| Signed migration seed | Active roster, canonical grants, selected policy, current names, publisher proof. | Valid for one hour; not retained in installed authority. |
| Network capability | Shared secret and pinned checkpoint anchor. | Protected authority storage; never public diagnostics. |
| Installed authority | Active snapshot and independently signed current hostname claims. | Compacted as membership changes. |
| Pairing artifacts | Unfinished transaction ownership and bounded recent checkpoint results. | Legacy completed proofs retire after installation; current results expire or retire on removal/incarnation change. |

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
| Isolated multi-daemon packet/DNS migration and offline catch-up. | Native three-daemon campaign passes; evidence below. |
| Superseded legacy pairing artifacts. | Startup, explicit installation, and periodic retirement implemented; focused evidence below. |
| Combined storage bounds through repeated admission/removal cycles. | Existing checkpoint churn coverage; migration-wide campaign still required. |
| Interrupted-process temporary residue. | Write-failure coverage exists; crash-residue retirement still required. |
| Live networks. | Not migrated, revoked, or restarted by this work. |

The workflow is not a claim that the full migration goal is complete. Android
activation and complete export/native-Nix artifacts remain separate goals.

## Focused Verification

Nine core migration, six protected-artifact, and nine runtime workflow tests
pass. Socket-contract and CLI-parsing regressions also pass. The isolated kernel
route-cleanup and native migration tests pass when invoked explicitly.
The full Rust workspace passes 1,805 tests, with 48 opt-in tests excluded.

Nix `rust-test-sources` passes offline.
Formatting and required correctness/suspicious/performance Clippy groups pass;
existing unrelated non-fatal lint warnings remain.

The offline package dry run succeeds but lists 1,377 uncached derivations. The
full package build was not started. The native campaign uses unprivileged network
namespaces; it creates no additional VM or Android build tree.

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

Those unit-level daemon tests use fixture packet/route adapters, not real TUN.
The unit cohort test validates authority installation, not packet/DNS convergence.
Existing formal verification assets were not found; executable regressions cover
this step. Do not infer full migration acceptance from these passing layers.

## Native Linux Campaign

Source: `tests/support/checkpoint_migration.rs`. Three separate network namespaces
run the native CLI daemon with real `pv0` TUN interfaces. A fourth identity is
revoked in the synthetic legacy ledger before migration.

```sh
nix develop --offline -c env TMPDIR=/tmp cargo test \
  --test tun_namespace --locked --offline \
  tun_namespace_checkpoint_migration_compacts_and_recovers \
  -- --ignored --exact --nocapture
```

| Phase | Required Evidence |
| --- | --- |
| Legacy baseline | ICMP in every direction, wire UDP DNS on each node, and a metric-bearing alias route. |
| Explicit installation | One common protected handoff; matching fingerprints; preserved keys, roles, addresses, alias, metrics, and current names. |
| Post-installation | Every active peer has a live supported path; fresh bidirectional packets and wire DNS pass. |
| Sole survivor | `B` advances while `A` and `C` are stopped; removes creator `A` without removing offline descendant `C`. |
| Offline catch-up | Stale `C` starts gated, denies packets, resyncs autonomously, then exchanges packets and DNS with `B`. |
| Removed restart | Stale `A` resyncs to excluded state; unchanged static peers cannot restore its access. |
| Survivor restart | `B` restarts gated and restores bidirectional packet/DNS service without authority fallback. |
| Retained state | No removed roster identities, legacy history, inviter fields, obsolete labels, migration archive, or temporary files. |

### Recorded Runs

Three fresh-identity campaigns pass, including two after extracting the final
path-reconciliation helper. Each completes in approximately 78 seconds.

| Measurement | Result |
| --- | --- |
| Packet/DNS phases | Legacy, installed, offline catch-up, and survivor restart pass. |
| Removal | Stale creator remains excluded; obsolete names and routes are absent. |
| Final authority bytes per node | 3,001 through 3,043 bytes across the three runs. |
| Workspace | 1,805 passing tests; 48 opt-in tests excluded from that command. |
| Tooling | Formatting, required Clippy groups, and offline Nix source coverage pass. |

### Path Recovery Regression

A connection established during resync can be control-only. Once checkpoint
selection commits forwarding authority, it must install a packet path for each
still-live authorized connection; another admission event may never occur.

The runner now reconciles these paths after selection. The focused regression
rejects gated, unknown, removed, retiring, and obsolete-epoch connections and
verifies that repeated reconciliation does not double-count a connection.

### Scope

| Boundary | Limitation |
| --- | --- |
| Transport | Isolated LAN discovery and authenticated TCP streams; not WAN, NAT, relay, or QUIC performance evidence. |
| Restart | Kill/reap then service-managed transient socket cleanup; durable authority is untouched. |
| Storage | Small-cohort authority stays below 64 KiB; this is not combined repeated authority/pairing churn proof. |
| Durability | Restart works; power-loss and abandoned temporary-write cleanup remain separate acceptance checks. |
| Deployment | No user's live network or device was migrated, revoked, or restarted. |

## Pairing Artifact Retirement

Sources: `runtime/pairing_sessions/checkpoint_retirement.rs` and the serialized
`runner/checkpoint_pairing.rs` owner. The legacy runtime does not call retirement.

| Boundary | Contract |
| --- | --- |
| Authority first | Only an installed checkpoint owner permits proof retirement. |
| Unresolved transactions | Prepared/aborting enrollment and uncertain completion ownership survive, even past expiry. |
| Proof copies | Remove applied legacy enrollment, completed operation, accepted polling-ticket, and corresponding receipt copies. |
| Current results | Preserve active checkpoint results until expiry; remove missing or changed incarnations. |
| Replay | Keep at most 256 opaque tokens with nonrenewing, at-most-one-hour deadlines; never retain signed history as a replay guard. |
| Receipts | At most 256, active endpoints only, expiry bounded to one hour from completion. |
| Persistence | Atomic encrypted replacement before in-memory retirement; ambiguous visibility blocks ordinary saves until durable readback reconciliation. |
| Retry | Startup flushes restore-pruned copies; maintenance retries; migration reports `cleanup_pending` on failure. |

Existing configured `membership.key` files are not deleted: they may still be
the consumer's explicit credential pin. This is not an authority archive.

### Verified Evidence

| Test | Result |
| --- | --- |
| Eleven retirement regressions. | Completed legacy copies disappear; active operations and unfinished ownership survive; expiry and changed incarnations retire results. |
| Encrypted churn: 512 distinct removed identities. | One file, no archive; replay window under 64 KiB; under 512 bytes after each expiry window. |
| Write failures. | Definite failure preserves state; ambiguous visibility blocks rollback; unsaved expiry is compared with the actual stored baseline. |
| Daemon migration and restart. | Explicit installation erases signed legacy pairing history; a restored stale sidecar is erased before readiness and cannot reopen static authorization. |
| Isolated kernel route cleanup. | IPv4/IPv6 address and route absence checks pass; this is not multi-daemon migration packet/DNS evidence. |

```sh
nix develop --offline -c cargo test --lib --locked --offline \
  checkpoint_retirement
nix develop --offline -c cargo test --lib --locked --offline \
  runtime::tun::tests::cleanup_absence_checks_match_kernel_state \
  -- --ignored --exact
```

The kernel test reexecutes itself in a separate user/network namespace and checks
that it differs from its parent. No live network, device, or authority was changed.
Formatting, required Clippy groups, and Nix `rust-test-sources` pass. Existing
unrelated lint warnings remain; no new retirement/migration warnings were added.

See [Checkpoint Acceptance](membership-checkpoint-acceptance.md) for the full
migration, storage-bound, and multi-daemon completion criteria.
