# Pairing and Session Lifecycle Review

## Status

Active; baseline `cbe400b1`. This bounded workstream follows the completed
[recovery ownership review](recovery-event-ownership-review.md).
The checklist below is review scope, not a claim of verified completion.

## Existing Evidence

| Area | Retained Result | Remaining Boundary |
| --- | --- | --- |
| [Durable cancellation](pairing-cancellation-plan.md) | Prepared cancel/reject/replace persists abort intent; cleanup retries and survivor preservation covered | Audit orchestration around those owners; do not redesign the chosen policy |
| [Acceptance transactions](pairing-transaction-review.md) | Matching Submit/Poll retry release, partial-route retry, restart repair and historical acknowledgement covered | Trace current request dispatch, replacement and finalization together |
| [Membership sync](membership-sync-review.md) | Wrong-type/stale response cleanup, authority withdrawal and bounded history corrected | Verify integration boundaries; do not reopen completed cases without new evidence |
| Discovery ownership | Query capacity/retry and inactive join lookup have session/driver regressions | Check pairing terminal transitions against existing query owners |
| Legacy snapshots | Inconsistent historical Prepared snapshots fail closed; limitation documented | Preserve validation; no destructive automatic repair |
| Platform evidence | Existing namespace, VM and Android reports retain revision-specific scope | Run affected local checks; final platform acceptance remains separate |

Historical reports contain intermediate open findings later closed by their final
sections. Their earlier failures remain evidence, not automatically new defects.
New focused runtime evidence is recorded below; whole-workstream gates remain pending.

## Policy Boundary

| State | Required Behavior |
| --- | --- |
| Uncommitted pairing | Cancellation discards pending enrollment |
| Prepared with possible runtime effects | Persist abort ownership, clean up safely, then compact |
| Cleanup failure | Retain durable ownership and retry without granting authority |
| Completed membership | Cancellation does not silently revoke; existing membership policy applies |
| Replacement operation | Older completion, cleanup or retry cannot overwrite its ownership |

The cancellation decision is already resolved. The audit must identify the exact
runtime completion and durable checkpoint boundaries without inventing a new
security or membership policy.

## Ownership Map

| Owner / Source | Transitions to Trace |
| --- | --- |
| [CodePairingSessions](../../src/runtime/pairing_sessions.rs) | Operation IDs, Hello/Submit/Poll request map, inbound sessions, approval tickets and retry flags |
| Same session owner | Prepared/Applied/Aborting ledger, receipts, replay tokens, expiry and replacement |
| [Runtime adapter](../../src/runtime/runner.rs) | RPC mutations, both code protocol dispatchers, file pairing, provider/query retirement and shutdown |
| Same runtime adapter | Runtime enrollment preparation, route application, completion, checkpoint and abort cleanup |
| [PairingStateStore](../../src/runtime/pairing_store.rs) | Atomic save, load validation, error handling and restart ownership |
| [Code protocol codecs](../../src/runtime/pairing_code.rs) | Terminal transport events and bounded messages for both protocol versions |
| [File pairing codec](../../src/runtime/pairing.rs) | Request completion, rejection and disconnect behavior |
| [V2 bootstrap client](../../src/runtime/pairing_bootstrap.rs) | Separate request map, selected peer, pending approval, timeout and future-drop cleanup |
| [CLI tests](../../tests/pair_cli.rs) | Cancellation outcomes, operation resumption, artifact export and acknowledgement |

## Bounded Checklist

- [x] Identify existing completed fixes and explicit historical limitations.
- [x] Record ownership entry points and the chosen cancellation policy.
- [ ] Trace invitation/code creation, authentication and admission on both roles.
- [ ] Trace Hello/Submit/Poll success, rejection, wrong-type reply and transport failure.
- [ ] Verify stale IDs, duplicate replies, cancellation and replacement isolation.
- [ ] Trace expiry, retry deadlines, disconnect, provider removal and query retirement.
- [ ] Trace Prepared, runtime commit, completion, Applied checkpoint and acknowledgement.
- [ ] Trace shutdown/reload, persistence failure and idempotent cleanup/recovery.
- [ ] Reconcile membership-sync integration and file-pairing session boundaries.
- [ ] Reproduce confirmed gaps, fix narrowly and verify recovery after failure.
- [ ] Run affected integration and shared-runtime verification gates.
- [ ] Publish final evidence, update remaining work and verify all commits on `main`.

## Verification Plan

| Layer | Required Evidence |
| --- | --- |
| Focused regressions | Real owner/dispatcher where practical; failing-before and passing-after for defects |
| Recovery | Successful retry/replacement/reload, not merely ignoring stale input |
| Native workspace | Offline locked tests; formatting; required correctness/suspicious/performance Clippy groups |
| Integration | Applicable direct/relayed pairing, code exchange, cancellation/retry and restart cases |
| Packaging | Cached Nix source parity; full-package limitations stated separately |
| Shared runtime | Android-native compilation if changed; not APK/device acceptance |
| Formal verification | Inspect applicable existing models; executable tests are not proof |

## Resource and Scope Limits

- Keep all `/tmp/p2p-vpn-*` task storage below 10 GiB; preserve raw evidence.
- Check headroom before builds; reuse cached targets and at most two Cargo jobs.
- Cap downloads at 10 Mbps; no uncontrolled source builds or timing-test builds.
- Use bounded logs/watchdogs; preserve original deadlines and authorization checks.
- No physical deployments, personal-flake edits or public-WAN campaign.
- Android service lifecycle, heap attribution and final platform acceptance remain separate.

## Findings

### Additional Traces Requiring Coverage Reconciliation

| Boundary | Current Source Trace | Remaining Check |
| --- | --- | --- |
| V2 bootstrap lifetime | `join_by_code_v2` owns its swarm/state locally and returns on a bounded deadline or result | Reconcile caller cancellation/drop and late-response tests |
| V2 retry ownership | Separate `BootstrapState.requests`, selected peer and approval poll flag | Audit all terminal replies/failures, not just daemon V1 |
| Daemon shutdown | Main shutdown branch returns without an extra pairing save | Reconcile mutation checkpoints and startup tests; do not assume a final-save guarantee |
| Periodic checkpoint | Expiry, abort retry and discovery run before the pairing checkpoint | Inspect error outcomes and retained ownership |
| Restored requests | Submit/Poll restore clears in-flight flags and starts recovery at resume time | Retain existing exact-request/transcript validation and restart tests |
| State file | Private temporary file, file sync, rename and parent sync; bounded load and envelope validation | Trace caller behavior when save reports an error after rename |

These are source observations, not newly reproduced defects or completed gates.

### PS-1: Retired-Connection Reply Ownership

| Item | Initial Source Evidence |
| --- | --- |
| Filter | `handle_pairing_code_event` returns on an unusable message connection before response dispatch |
| Pending owner | `handle_pairing_code_response` normally removes the request from `outbound_requests` |
| Retry state | `release_outbound_code_request` releases Hello/Submit/Poll in-flight flags |
| Reproduction | Real loopback Hello reply completed in libp2p; retiring its connection before dispatch stranded retry ownership |
| Correction under verification | Release matching peer/request ownership without processing the stale payload; preserve existing backoff |
| Remaining evidence | Broad gates and full pairing/session transition audit |

Pinned `libp2p-request-response` 0.29.0 removes the pending transport response
before queuing `Message::Response`. No subsequent transport timeout can release
the stranded application attempt. The test asserts transport completion directly.

`pairing_hello_terminal_stale_reply_releases_retry_ownership` invokes production
swarm dispatch with retained session state. Before correction it fails at the
post-backoff retry assertion; after correction it passes, retaining immediate backoff.

| Evidence | Result |
| --- | --- |
| `/tmp/p2p-vpn-ps1-hello-negative.log` | Genuine assertion failure; 0 passed, 1 failed |
| `/tmp/p2p-vpn-ps1-hello-fixed.log` | Focused regression passed |
| Storage before builds | 8,210,104 KiB total, approximately 7.83 GiB; privileged enumeration |

The initial test checked eligibility at a later instant. Its strengthened version
establishes two real connections, retires the first reply's connection, waits for
normal backoff and delivers a Hello retry over the surviving connection.

Old-request and wrong-peer stale events cannot release the replacement attempt.
The matching current reply releases its owner and retains backoff. Log:
`/tmp/p2p-vpn-ps1-hello-live-retry.log`; one pass in 1.89 seconds.

`pairing_submit_poll_stale_cleanup_preserves_replacement_and_backoff` exercises
both request types through production dispatch with controlled events. It checks
owner removal, immediate backoff, later retry eligibility and cancellation/replacement.

| Matrix Evidence | Boundary |
| --- | --- |
| `/tmp/p2p-vpn-ps1-dispatch-matrix-verified.log` | Both tests pass; stale payloads do not complete enrollment |
| Submit/Poll fixture | Existing offer/request builder; synthetic reply dispatch, not full authenticated exchange |
| Earlier matrix logs | Test compilation and invalid fixture-ticket errors; not product-defect evidence |

These checks do not measure physical race frequency or establish full enrollment
over the retry path. Existing acceptance-through-completion tests remain separate.
The patch verification below is complete. Whole-workstream verification remains pending.

### PS-1 Intermediate Verification

| Gate | Result |
| --- | --- |
| Workspace | 1,477 passed; 36 opt-in exclusions; `/tmp/p2p-vpn-ps1-workspace.log` |
| Required Clippy groups | Passed; advisory warnings remain; `/tmp/p2p-vpn-ps1-clippy.log` |
| Formatting | `cargo fmt --check` passed |
| Documentation | Local file links and paragraph lengths checked |
| Remaining patch gates | Cached Nix source parity, Android-native compilation and affected integration cases |

Commands use the same cached native toolchain and target as the
[recovery review](recovery-event-ownership-review.md#commands-and-provenance):

```sh
cargo test --offline --locked --workspace -- --test-threads=2
cargo clippy --offline --locked --workspace --all-targets -- \
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
cargo fmt --check
```

The complete pairing/session checklist remains open. This checkpoint establishes
neither full workstream completion nor new physical-platform acceptance.

### PS-1 Patch Verification

| Gate | Result | Log |
| --- | --- | --- |
| Android native | x86_64/API 26 passed in 37.22 s; four target warnings | `/tmp/p2p-vpn-ps1-android.log` |
| Nix source parity | Cached sandboxed source/test-target check passed | `/tmp/p2p-vpn-ps1-nix.log` |
| Peerless code pairing | Passed, 13.48 s | `/tmp/p2p-vpn-ps1-code-pairing.log` |
| Direct acceptance | Passed, 8.78 s | `/tmp/p2p-vpn-ps1-pair-direct.log` |
| Relayed acceptance | Passed, 17.65 s | `/tmp/p2p-vpn-ps1-pair-relay.log` |

These supplement the unchanged-source 1,477-test workspace and required Clippy
results above. No build ran during the integration observations. Pre-build task
storage was 8,210,644 KiB, approximately 7.83 GiB; artifacts were retained.

Nix output: `/nix/store/x869ga0di8anwqqqwnbh6v26gj0v05ih-p2p-vpn-rust-test-sources`.
The cached overrides and native build command follow the linked recovery tooling;
these are not full Nix package, APK, ARM64 or physical-device checks.

The namespace binary came from this workspace run:
`/tmp/p2p-vpn-review-target/debug/deps/tun_namespace-fc4bbc3b02326c73`.
Each case ran individually with `--ignored --exact "$CASE" --nocapture`, retained
artifacts and a 120-second outer watchdog; internal deadlines were unchanged.

| Case Name |
| --- |
| `tun_namespace_code_pairing_crosses_peerless_overlay` |
| `tun_namespace_pair_accept_crosses_live_pairing_overlay` |
| `tun_namespace_pair_accept_crosses_relayed_live_pairing_overlay` |

PS-1 changes neither configuration nor wire/persisted formats. It releases only
matching peer/request state and never consumes the stale payload. The wider
V2, shutdown, persistence and session review remains on the bounded checklist.
