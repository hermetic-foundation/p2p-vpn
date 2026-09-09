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

Published as `578f6dc8`; `main` and `main@origin` matched after push and the
working copy was clean. This completes PS-1, not the broader lifecycle goal.

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

### PS-2: Unauthenticated Candidate Rejection

| Item | Evidence |
| --- | --- |
| Owner | V2 `BootstrapState.requests`; several Hello candidates may be outstanding before inviter selection |
| Defect | InvalidRequest, UserRejected or Expired from a Hello candidate returned a terminal error for the whole join |
| Reproduction | `hello_rejection_cannot_terminate_another_selected_inviter` fails with `Rejected(InvalidRequest)` |
| Correction | Every Hello rejection releases only its candidate with existing retry delay; authenticated Submit/Poll rejection remains terminal |
| Preservation | Selected inviter remains unchanged; retired request is removed; candidate can retry after selection is released and backoff expires |

The request ID comes from the production Hello driver. The test models an earlier
authenticated selection and injects its competing candidate's reply; it does not
perform a PAKE exchange or claim a measured race over a physical network.

| Gate | Result |
| --- | --- |
| Negative | `/tmp/p2p-vpn-ps2-hello-negative.log`: genuine assertion failure before correction |
| Focused positive | `/tmp/p2p-vpn-ps2-bootstrap-fixed.log`: all 11 bootstrap tests pass |
| Rejection control | Both selected Submit and Poll retain terminal UserRejected behavior |
| Workspace | 1,479 passed; 36 opt-in exclusions; `/tmp/p2p-vpn-ps2-workspace.log` |
| Static | Formatting and required Clippy groups pass; advisory warnings remain; `/tmp/p2p-vpn-ps2-clippy.log` |
| Android native | x86_64/API 26 passed in 36.60 s; four target warnings; `/tmp/p2p-vpn-ps2-android.log` |
| Live V2 integration | All 12 bootstrap tests pass, including loopback retry through authenticated acceptance; `/tmp/p2p-vpn-ps2-live-membership.log` |
| Final static | Required Clippy groups and formatting passed; `/tmp/p2p-vpn-ps2-final-clippy.log` |
| Final source parity | Cached sandboxed check passed; `/tmp/p2p-vpn-ps2-nix.log` |

Unknown candidates cannot authenticate a refusal of the whole pairing operation.
This matches the distinction already used by daemon code pairing and does not
change codes, wire formats, admission policy, retry limits or configuration.

These runs use the cached native commands above with 180-second focused/static
and 300-second workspace watchdogs. No Lean project/model was found in the repo;
the regressions are executable evidence, not a formal proof.

#### Live V2 Boundary

`rejected_hello_retries_over_loopback_and_completes_authenticated_pairing` uses
two real TCP libp2p swarms and the production V2 Hello driver/response dispatcher.
It removes seeded DHT state before polling, without public bootstrap traffic.

| Stage | Assertion |
| --- | --- |
| First Hello | Server returns InvalidRequest; client continues rather than terminating |
| Retry | Exactly one replacement Hello arrives after the existing retry delay |
| Challenge / Submit | Real PAKE challenge and request confirmation verification succeed |
| Acceptance | Signed inviter/root and joiner membership records pass response verification |
| Completion | Enrollment is returned for the expected inviter and request owners are empty |

The server is a test responder that approves immediately, not a daemon approval
UI. The test does not install routes, persist an Android profile or test cellular
recovery. Earlier V1 namespace results are not presented as V2 branch coverage.

Earlier `ps2-live*` failures were fixture errors: a helper name, omitted offer
retention and missing membership trust root. They remain preserved; only the
original Hello-rejection regression is negative product-defect evidence.

The 1,479-test workspace and Android-native results predate only the added
`cfg(test)` loopback fixture. Production code is unchanged since those runs;
final focused tests and static/source checks cover the additional test source.

Source-parity output:
`/nix/store/dg2hj6ylg911kqj48mp3y45qd4skzf7m-p2p-vpn-rust-test-sources`.
Pre-build storage was 8,211,968 KiB, approximately 7.83 GiB. No task builds ran
during live test execution; no dependency or flake-lock changes were needed.

PS-2 is ready for atomic publication after these checks. Completion of the
workstream still requires the remaining transition audit and evidence checklist.

PS-2 publication was verified at `8b5f9b89`; local and remote `main` matched and
the working copy was clean before starting the next case.

### File-Pairing Ordering Follow-Up

`handle_pairing_request_event` submits its response before installing membership;
durable code pairing has a different checkpoint sequence. Review actual transport
delivery and route-failure behavior before asserting consistency or a defect.
The source order alone does not prove delivery before local commitment.

### PS-3: Cross-Role Cancellation

The owner retains separate inviter and joiner slots. `cancel` can revisit a
terminal old slot while the opposite role has a newer operation, and
`clear_transient_handshakes` currently clears shared request/session maps.

Both directions are reproduced in `/tmp/p2p-vpn-ps3-cross-role-negative.log`:
re-cancelling the old join clears the new invite approval; re-cancelling the old
invite clears the new join's outbound request while its retry flag stays in flight.

| Correction | Scope |
| --- | --- |
| Transient cleanup | Retain requests, inbound sessions and pending approvals owned by other operation IDs |
| Callers | Completion, Prepared recovery, inviter deactivation and joiner deactivation pass their operation ID |
| Policy | Cancellation remains supported, including explicit abort of expired Prepared enrollment |
| Resource behavior | Existing bounded maps and capacities remain; no new persistent state or config |

The positive session run is `/tmp/p2p-vpn-ps3-sessions-fixed.log`. New owner tests
verify repeated cancellation isolation, subsequent inviter completion, join retry
backoff and cancellation of the current join. All 63 session tests pass.

These use existing session fixtures, including synthetic offer/response material
for the approval-owner case. They are not authenticated wire exchanges, daemon
RPC persistence tests or restart evidence. Existing cryptographic and durable
transaction regressions retain their separate scopes.

#### PS-3 Runtime Verification

The existing peerless namespace scenario now creates terminal opposite-role
operations on both daemons. It repeats their cancellations while the current
pairing awaits approval, then completes approval and the original traffic checks.

| Gate | Result |
| --- | --- |
| Workspace | 1,482 passed; 36 opt-in exclusions; `/tmp/p2p-vpn-ps3-workspace.log` |
| Expanded CLI/RPC scenario | Passed in 13.49 s; `/tmp/p2p-vpn-ps3-code-pairing.log` |
| Required Clippy | Passed; advisory warnings remain; `/tmp/p2p-vpn-ps3-clippy.log` |
| Formatting | `cargo fmt --check` passed |
| Android native | x86_64/API 26 passed in 42.64 s; four target warnings; `/tmp/p2p-vpn-ps3-android.log` |
| Source parity | Cached sandboxed check passed; `/tmp/p2p-vpn-ps3-nix.log` |

The workspace run predates only the expansion of the opt-in namespace test.
That final test source was rebuilt and executed separately, then covered by
final Clippy. Production code is unchanged between those verification steps.

Run `tun_namespace_code_pairing_crosses_peerless_overlay` individually with
`--ignored --exact` and retained artifacts. The 120-second outer watchdog and
existing internal deadlines were unchanged; no builds ran during observation.

Source-parity output:
`/nix/store/h1mnmsjaqna8pl9kxmqsrknkbbnf73ns-p2p-vpn-rust-test-sources`.
Pre-build task storage was 8,213,900 KiB, approximately 7.83 GiB. The workspace,
native and source checks used the same cached commands and resource limits above.

PS-3 is ready for atomic publication. It does not change persistent schemas,
wire protocols, membership policy or required configuration. The scoped native
build is not a physical-device, ARM64, APK or full Nix package acceptance claim.

PS-3 publication was verified at `18eb5705`; local and remote `main` matched.
The working copy was clean before beginning PS-4.

### PS-4: File-Pairing Partial Enrollment

`file_pairing_route_failure_keeps_membership_unpublished` reproduces a local
consistency defect: `install_pairing_response_membership` returns a route error,
but the forwarder already authorizes the joining peer. A correction is under verification.

| Evidence | Scope |
| --- | --- |
| `/tmp/p2p-vpn-ps4-file-route-negative.log` | One genuine assertion failure after successful compilation; no fixture or timeout error |
| Fixture | Existing signed code-pairing response passed to the file-pairing installation helper; no transport exchange |
| Injection | Route controller returns an error before applying any commands |
| Observed failure | New transport peer remains authorized despite installation returning an error |
| Storage | Privileged enumeration before compilation: 8,214,588 KiB, approximately 7.83 GiB |

The test used the cached offline native command above, filtering to its exact
test name with a 180-second outer watchdog. Compilation took 58.57 seconds;
the test failed in 0.20 seconds. Production source is unchanged at this checkpoint.

#### Correction Requirements

- Stage logical membership before route reconciliation; publish only after success.
- Preserve trust validation, record merging, retained limits and existing members.
- Check response ordering with an actual file-pairing transport exchange.
- Cover failed routes, successful retry and a closed response channel.
- Do not roll back committed membership merely because response delivery fails.

The existing enrollment transaction applies routes before publishing logical
state. Reuse requires checking its config-application semantics: it appends
signed records, while the file helper currently uses membership merging.
Moving `send_response` alone does not repair the reproduced partial mutation.

The negative log is not a claim that acceptance was observed remotely before
commitment. The patch is not yet published to `main`.

#### PS-4 Correction Under Verification

| Change | Boundary |
| --- | --- |
| Staged merge | Reuses live trust anchors, the membership merger and its retained-record limit |
| Logical publication | Build membership and TUN snapshots, reconcile routes, then commit authorization |
| Config preservation | Keep the original configured records; do not substitute config append semantics for runtime merging |
| Response ordering | Queue acceptance after local commitment; consume the bearer token at commitment |
| Closed channel | Already closed channels do not install membership; a later send failure does not undo commitment |

`/tmp/p2p-vpn-ps4-file-route-fixed.log` passes the strengthened helper regression
in 0.40 seconds. It now builds a FileBearer request and response, verifies no
logical publication on failure, then retries and compares against normal merging.

The test does not establish daemon-level retry after a fatal route error. It
invokes the helper again from unchanged logical state. Transport dispatch,
closed-channel coverage and broader verification remain separate requirements.

Pre-build storage was 8,214,596 KiB. No applicable Lean files or repository
`AGENTS.md` were found. The first loopback-test build had a fixture peer-ID type
error (`ps4-file-live.log`), not a product regression or transport result.

`/tmp/p2p-vpn-ps4-file-live-verified.log` passes all four `file_pairing_` tests
in 0.41 seconds, including real TCP loopback dispatch. The failed route attempt
returns an outbound transport failure without acceptance or token consumption;
the subsequent attempt returns a verified signed response after commitment.

The fixture disables public discovery and retains replay tokens across dispatches.
It retries the production dispatcher, not a complete daemon restart. Formatting
passes; closed-channel and full patch verification remain pending.

The offline locked workspace run passes: 1,484 passed, zero failed and 36
opt-in exclusions (`/tmp/p2p-vpn-ps4-workspace.log`). Documentation links and
paragraph lengths also pass. Required Clippy, Android-native compilation,
cached Nix parity, selected namespace cases and publication remain pending.

#### PS-4 Closed-Channel Verification

The loopback regression also holds a real inbound request, disconnects its
transport and waits until the response channel is closed. Dispatch retains the
admitted epoch so the assertion exercises channel closure, not stale filtering.

| Assertion | Result |
| --- | --- |
| Membership and TUN state | Unchanged after the closed request |
| Route controller | No additional invocation |
| Fresh bearer token | Not consumed |
| Focused file tests | Four passed in 0.50 seconds; `/tmp/p2p-vpn-ps4-file-closed.log` |

Only test code changed after the 1,484-test workspace run. Its production-code
evidence remains applicable; the expanded loopback test was rebuilt separately.
Pre-build storage was 8,214,820 KiB, approximately 7.83 GiB.

#### PS-4 Patch Verification

| Gate | Result | Log |
| --- | --- | --- |
| Required Clippy groups | Passed in 24.23 s; advisory warnings remain | `/tmp/p2p-vpn-ps4-clippy.log` |
| Formatting | `cargo fmt --check` passed | Terminal result |
| Android native | x86_64/API 26 passed in 37.77 s; four warnings | `/tmp/p2p-vpn-ps4-android.log` |
| Source parity | Cached sandboxed check passed | `/tmp/p2p-vpn-ps4-nix.log` |
| Direct file acceptance | Passed in 8.73 s | `/tmp/p2p-vpn-ps4-pair-direct.log` |
| Relayed file acceptance | Passed in 17.70 s | `/tmp/p2p-vpn-ps4-pair-relay.log` |
| Peerless code pairing | Passed in 13.44 s | `/tmp/p2p-vpn-ps4-code-pairing.log` |

All three namespace cases used the PS-4 workspace binary and commands listed
under PS-1, individually with retained artifacts and a 120-second outer watchdog.
No build ran during these observations; production and namespace sources were
unchanged after the workspace run.

Source-parity output:
`/nix/store/rdklrfgiyx1v1sc1y3ljaks18yhrjamb-p2p-vpn-rust-test-sources`.
Privileged storage checks before Android and Nix were 8,215,140 and 8,215,088 KiB.
These checks do not establish APK, ARM64, physical-device or full-package acceptance.

PS-4 is ready for atomic publication. The new commit boundary is local runtime
enrollment, not remote delivery acknowledgement or a new durable file-pairing
transaction protocol. Fatal route-error restart recovery and the remaining
whole-workstream transition audit are not established by the loopback fixture.
