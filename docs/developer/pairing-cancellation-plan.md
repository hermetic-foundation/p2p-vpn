# Durable Pairing Cancellation

## Approved Behavior

| State | Cancellation Behavior |
| --- | --- |
| Before preparation | Discard the pending operation normally |
| Prepared, not completed | Persist abort intent, undo partial changes, discard pending enrollment |
| Abort cleanup interrupted | Resume cleanup, never complete enrollment |
| Already established membership | Existing revocation policy applies; cancellation does not silently revoke |

This is the user's chosen policy for closeout Goal 1. It replaces the proposal
to prohibit cancellation after preparation. No additional policy approval is
needed to implement safe abort and rollback.

## Current Ownership

| Owner | Relevant Behavior |
| --- | --- |
| `CodePairingSessions::cancel` / `reject` | Mark pending Prepared enrollment Aborting before clearing operation dependencies |
| `prepare_enrollment` | Records the signed response before runtime application |
| `commit_pairing_runtime_enrollment_with_route_update` | Applies routes before committing logical membership/configuration |
| `execute_tun_route_update` | Attempts rollback after partial failure; rollback itself can fail |
| `reconcile_persisted_pairing_enrollments_with_route_update` | Skips Aborting entries; checkpoints cleanup ownership before replaying Prepared entries |
| Pairing RPC handlers | Persist abort intent and clean up before reporting cancellation success |
| Runtime startup/timer | Restore surviving membership/routes before abort cleanup; retry failed cleanup every ten seconds |
| `finish_enrollment_abort_with` | Persist compacted state before discarding pending material in memory |

## Implementation Sequence

1. Add an explicit abort lifecycle and validated cleanup ownership to persisted pairing state.
2. Persist abort intent before destructive cleanup; persistence failure must not report success or permit accidental enrollment replay.
3. Reconcile affected routes and addresses against surviving authorized state, including unrelated or overlapping memberships.
4. Retain cleanup ownership on failure; make retry and restart idempotent and prevent late responses from reactivating cancellation.
5. Remove pending enrollment material after cleanup; retain only bounded replay protection needed to prevent stale completion.
6. Route cancellation, rejection, and cancellation-then-replacement through this lifecycle for inviter and joiner.
7. Update user-facing outcomes, compatibility notes, and the transaction evidence report after verification.

Do not persist executable shell commands as rollback data. Use validated,
structured ownership data. Do not authorize an aborted response temporarily
to make cleanup or startup reconciliation succeed.

## Acceptance Matrix

| Case | Required Evidence |
| --- | --- |
| Five existing mutation cases | Cancel/reject/replace survives reload without conflict or enrollment replay |
| Abort persistence failure | Clear failure outcome; no false acknowledgement or destructive loss of recovery data |
| Partial route application | Cancellation removes its additions and restores required prior state |
| Cleanup failure and retry | Abort intent survives; retry converges without authorizing the peer |
| Crash after abort persistence | Startup performs cleanup only |
| Crash after cleanup before compaction | Repeated cleanup is safe, including already-absent kernel entries |
| Late response | Cannot complete a cancelled operation or overwrite its replacement |
| Other enrollment/configuration | Existing authorization, routes, identities, and membership keys remain intact |
| Compatibility | Existing valid snapshots load; already-invalid legacy state has tested repair or an explicit documented limitation |
| Established membership | Existing completion and revocation behavior remain separate |

## Verification Gates

- Replace or complement the five broken-behavior diagnostics with passing contracts.
- Run focused tests, the full native workspace, required Clippy groups, formatting, and Nix source inclusion.
- Run relevant existing pairing integration scenarios; exercise injected persistence and route failures at their actual owners.
- Preserve the completed [probe-ownership evidence](path-probe-ownership-review.md).
- Record exact artifacts and limits before marking Goal 1 complete.

## Implementation Progress

| Component | Status |
| --- | --- |
| TUN cleanup calculation | Implemented in `TunRuntimeConfig::pairing_abort_commands_from`; validated by the final workspace run |
| Surviving configuration | Reasserts addresses/routes before deleting pairing-only entries; IPv4/IPv6 and shared-address cases covered |
| Identity boundary | Rejects interface-name, MTU, and built-in address changes |
| Abort ledger state | `Aborting` round-trips through persistence; retries cannot prepare or apply it |
| Durable cleanup ownership | Optional `tun_cleanup` captures added prefixes/addresses; retries merge ownership, and reload validates it |
| Live preparation | Inviter and joiner record cleanup ownership before the existing Prepared persistence checkpoint |
| Completed-operation guard | A completed operation cannot begin abort even before its Applied checkpoint |
| Session verification | Five original mutations are now a normal regression requiring Aborting state, reload, and safe compaction |
| Runtime hooks | Cancellation/rejection, startup, and periodic retry are connected and covered by final verification |
| Abort compaction | Save failure preserves memory/disk ownership; success drops enrollment and retains bounded replay protection, not a completed receipt |
| Cleanup execution | Forward-only failure/retry regression passes; ordinary updates retain rollback |
| Already-absent entries | Exact JSON queries check address/prefix or route/device/metric; malformed data and query failures remain errors |
| Kernel verification | Isolated IPv4/IPv6 address/route deletion and repeated-delete checks pass; missing interface remains an error |

Focused log: `/tmp/p2p-vpn-goal1-abort-routes-verified.log`.
The calculation tests do not prove crash recovery or end-to-end cancellation.
No cancellation behavior has been deployed yet.

Execution evidence:

- `/tmp/p2p-vpn-goal1-abort-executor.log`: forward-only cleanup failure/retry regression.
- `/tmp/p2p-vpn-goal1-abort-kernel-build.log`: 21 normal TUN tests pass; namespace test is explicitly ignored by default.
- `/tmp/p2p-vpn-goal1-abort-kernel.log`: explicitly invoked namespace test passes against real IPv4/IPv6 kernel state.

Run the isolated kernel test with cached project tooling:

```sh
cargo test --lib cleanup_absence_checks_match_kernel_state -- --ignored --nocapture
```

The test launches itself through `unshare` and verifies namespace separation
before creating its dummy interface. The first harness attempt could not inspect
PID 1 from a user namespace; the corrected check compares parent/child namespace IDs.

Initial ledger log: `/tmp/p2p-vpn-goal1-abort-ledger-final.log`.
The temporary startup guard has been replaced with cleanup/retry integration.
Older binaries cannot read `aborting`; do not downgrade while cleanup is pending.
Backward readability by older binaries is not claimed.

Cleanup-ownership evidence:

- `/tmp/p2p-vpn-goal1-cleanup-ownership-tun.log`: 22 normal TUN tests pass, including merge/reload and survivor preservation.
- `/tmp/p2p-vpn-goal1-cleanup-ownership-sessions.log`: 55 normal session tests pass, including old snapshots without ownership and invalid new ownership rejection.
- `/tmp/p2p-vpn-goal1-cleanup-ownership-runtime-authenticated.log`: live acceptance retry test passes four submit/poll and persistence/route failure combinations.

The live test reloads the saved ledger inside the route controller and requires
cleanup ownership before its first command. Its original fixture lacked matching
code authentication; adding a real code exchange corrected the fixture without
removing the reload assertion. The failed run is retained in `...-runtime.log`.

`tun_cleanup` contains structured prefixes, not executable commands. It records
only additions, retains additions across retries, and checks the current interface
before generating cleanup. The existing 512 KiB pairing-state limit still applies.

Startup reconciliation now checkpoints cleanup ownership before replaying Prepared
entries, including valid older snapshots without that field. Aborting entries
without ownership cannot be cleaned up automatically and remain pending.

Already-inconsistent legacy snapshots (Prepared with cancelled recovery dependencies)
still fail closed. The legacy regression constructs that exact historical state;
it does not represent cancellation produced by the new implementation.

## Intermediate Evidence

| Check | Result | Evidence |
| --- | --- | --- |
| Pairing-focused library tests | 168 passed | `/tmp/p2p-vpn-goal1-abort-runtime-regressions.log` |
| Full library | 989 passed, 9 opt-in tests ignored | `/tmp/p2p-vpn-goal1-abort-lib.log` |
| Abort compaction failure | Pending ownership retained until final save succeeds | `abort_compaction_persists_before_discard_and_retains_only_replay_protection` |
| Both-role restart cleanup | Abort save failure prevents cleanup; cleanup failure survives reload; retry never authorizes peer | `cancelled_pairing_restart_retries_cleanup_without_authorizing_peer` |
| Cancel/reject RPC | Cleanup failure returns an error; retry succeeds without authorizing peer | `/tmp/p2p-vpn-goal1-abort-rpc-fixed.log` |
| Full native workspace | 1,244 passed, 23 opt-in tests ignored | `/tmp/p2p-vpn-goal1-abort-workspace.log` |
| Required Clippy groups | Passed before the final RPC/replacement corrections | `/tmp/p2p-vpn-goal1-abort-clippy.log` |
| Final session regressions | 58 passed, none ignored | `/tmp/p2p-vpn-goal1-abort-expired-replacement-fixed.log` |

These runs preceded the last corrections. The final verification below supersedes
them; they remain available to explain the implementation sequence.

An expired Prepared entry cannot lose its recovery slot through replacement.
Explicit cancellation still permits replacement. A completed slot with a pending
Applied checkpoint is finalized before replacement; it is not treated as cancelled.

### Nix Check Tooling

The standard source-check build requested 706 dependency derivations and failed
fetching an Autoconf source. No large compiler rebuild was pursued. The flake's
evaluated source-comparison script passed using cached Cargo/JQ/diff tools.

The sandboxed check passed with cached Cargo 1.97.1, JQ 1.8.2, diffutils 3.12,
and declared cached shared libraries. No flake or lock-file changes were made;
the source-filter comparison is unchanged.

- Log: `/tmp/p2p-vpn-goal1-abort-nix-sources-cached.log`.
- Output: `/nix/store/1q8kvgkqzkp56qkq2hpmnnmlp495nbxy-p2p-vpn-rust-test-sources`.
- This validates source inclusion, not a full package build with the flake's default tool closure.

## Final Verification

Verified on 2026-09-07 and published as `8ed9562764d7d3c9b25c11b724b39ad8ee58f056`
(`fix: abort prepared pairing transactions durably`). No deployment is included.

| Gate | Result |
| --- | --- |
| Native workspace | 1,247 passed, 23 opt-in tests ignored |
| Required Clippy groups | Correctness, suspicious, and performance groups pass; non-fatal style warnings remain |
| Peerless code pairing | Namespace pass, 13.42 seconds |
| Direct UDP packets | Namespace pass, 15.02 seconds |
| QUIC datagrams | Namespace pass, 16.08 seconds |
| Direct pairing acceptance | Namespace pass, 8.72 seconds |
| Relay pairing acceptance | Namespace pass, 17.63 seconds |
| Kernel cleanup | Isolated IPv4/IPv6 deletion and repeated cleanup pass, 0.09 seconds |
| Formatting/whitespace | Changed Rust files and diff checks pass |
| Nix source parity | Sandboxed source check passes with the cached tool inputs described above |

Final logs: `/tmp/p2p-vpn-goal1-abort-verified-{workspace,clippy,code-pairing,udp,quic,pair-accept,pair-relay,kernel,nix-sources}.log`.

Final Nix output: `/nix/store/r8qs3jbix4gcdl1j4bhdna7ii24cfxky-p2p-vpn-rust-test-sources`.

### Built-In Address Regression

Explicit assignment of a node's derived address previously looked like an added
address. Rollback or cancellation could therefore remove that built-in address.
The shared address-presence check now protects derived IPv4/IPv6 addresses in
normal pairing updates, rollback ownership, and retained cleanup records.

- Negative control: `/tmp/p2p-vpn-goal1-abort-builtin-before.log`.
- Passing contract: `pairing_cannot_take_cleanup_ownership_of_builtin_addresses`, included in the final workspace run.
- Surviving aliases/routes, replay bounds, failed-save retention, and both-role restart cleanup have separate regression coverage.

### Limits

- Existing inconsistent legacy snapshots require recovery from consistent state; no automatic destructive repair is attempted.
- Cancellation is local; already-established remote membership still follows network revocation policy.
- No physical deployment, Android device run, full NixOS package rebuild, or formal proof is claimed.
- Platform acceptance and broader reliability work remain separate goals.

## Scope

The implementation and scoped verification are complete and pushed to `main`.
Together with probe ownership at `8e5950fa`, this closes Goal 1. No service was redeployed.
Kademlia limits, general lifecycle review, resource baselines, and final platform
acceptance remain in the [umbrella checklist](review-verification.md).
