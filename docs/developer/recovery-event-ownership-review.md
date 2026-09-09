# Recovery Event Ownership Review

## Status

Completed bounded review on 2026-09-09; baseline `ceb6e4e2`. This follows the
completed Kademlia resource workstream and RM-1. It does not complete the broader
reliability review or constitute physical/public-WAN certification.

## Plan

1. Reconcile historical checklist entries with final evidence.
2. Trace recovery owners, reset boundaries and terminal-event effects.
3. Reproduce stale-event defects through production dispatch before fixing them.
4. Verify focused corrections and applicable recovery behavior.
5. Publish findings and remaining review boundaries.

## Ownership Inventory

Symbols refer to [the runtime adapter](../../src/runtime/runner.rs).
The final verification section records whole-workstream gates separately from
the source traces and focused tests below.

| Boundary | Owners / Entry Points | Review Status |
| --- | --- | --- |
| Network transition | `handle_runtime_network_change`, `ConnectionEpochs::advance` | Reset sequence traced; epoch, listener and QUIC replacement tests pass |
| Dial start / establishment | `ConnectionEpochs`, `handle_swarm_event` | Application/startup registration corrected; admission regressions pass |
| Outgoing failure | `PublicDiscoveryBackoff`, `DiscoveredPeerAddresses` | Old-epoch and late initial-epoch failures guarded; current failures retain backoff |
| Connection close | `active_connections`, `record_path_closed`, capability/session invalidation | Stateful old-close/replacement and last-close cleanup test passed |
| Periodic work | Runtime intervals, `handle_redial_tick`, `KademliaMaintenance` | RE-3 catch-up correction verified; existing cooldown/suppression coverage retained |
| Relay reservations | `ConfiguredRelayReservationRetries`, `RelayReadiness`, `AutoRelayState`, listener sets | RE-4 old/current/pending listener regressions pass; retries retain their owners |
| Packet negotiation | `PacketPlaneNegotiator`, QUIC task generations | Old-result/cancellation, current failure and new-attempt admission test passed |
| Probes / stream completions | `PathProbeTracker`, `PacketInFlight` | Published token/window guards retained; reset/expiry paths traced and current workspace passed |
| Discovery completions | `stale_behaviour_event_connection`, targeted query owner | Connection-ID guards traced; existing stale-response, query-owner and cadence tests passed |

## RE-1: Obsolete Failure Reapplies Backoff

Status: reproduced through production dispatch; focused guard implemented and
regression passed. This correction does not close application-dial registration
or the other recovery-event review obligations.

`ConnectionEstablished` checks the epoch before applying effects.
`OutgoingConnectionError` removes the epoch entry first, then updates global
bootstrap backoff and per-address failure state without checking its generation.

| Evidence | Result |
| --- | --- |
| Regression | `obsolete_outgoing_failure_does_not_reapply_recovery_backoff` |
| Ordering | Start old dial, advance epoch, establish replacement, deliver old failure |
| Observed | New 30-second bootstrap cooldown and address-failure log |
| Expected | Retire old attempt only; preserve replacement and current retry state |
| Negative log | `/tmp/p2p-vpn-recovery-owner-negative.log`; terminal exit 101 |
| Focused fixed log | `/tmp/p2p-vpn-recovery-owner-terminal-fixed.log`; one test with old/current/unobserved cases passed |

The guard captures whether the tracked attempt belongs to an older epoch before
removing its terminal ownership. Only that case skips failure effects. Current
and unobserved failures retain their existing backoff behavior.

This is an injected event reproduction, not a physical-network race-frequency
measurement. Unknown/unobserved dial IDs need explicit treatment; rejecting all
untracked failures could suppress legitimate current-network recovery.

## RE-2: Application Dial Registration

Status: admission regressions reproduced and corrected; workspace, static,
native and isolated recovery checks passed. Timer and relay-listener review remain open.

| Boundary | Inspected Behavior |
| --- | --- |
| Runner registration | `IncomingConnection` and `Dialing` events call `ConnectionEpochs::record_started` |
| Application dialing | Both dial helpers now register admitted `DialOpts::connection_id()` through `ConnectionEpochs::dial` |
| Startup dialing | An internal constructor observer reports admitted IDs to the epoch owner before runtime handoff |
| Pinned library | `libp2p-swarm 0.47.1` emits `SwarmEvent::Dialing` in its `ToSwarm::Dial` handler, not in `Swarm::dial` itself |
| Unknown completion | Legacy externally constructed nodes assign unobserved startup IDs to epoch zero, never to a later network |

Registering only observed `Dialing` events missed application-started attempts.
Successful admission now registers immediately; immediate rejection leaves no
epoch entry. Terminal events retire the existing entry without tombstones.

The same owner flows through queue draining, discovery, relay readiness and probe
recovery adapters. Behaviour-initiated and inbound attempts still register through
`Dialing` and `IncomingConnection`; no second registry or global state was added.

Public `build_node`, `P2pNode` fields and runtime entry signatures are unchanged.
Only the internal production construction path adds the observer. Initial-epoch
unobserved failures keep normal backoff; late initial-epoch failures are ignored.

| Regression | Evidence |
| --- | --- |
| `runtime_dial_registration_tracks_admission_and_rejects_old_success` | Real application admission and immediate rejection; old success rejected, fresh provider attempt usable |
| `startup_dial_registration_observes_only_admitted_attempts` | Bootstrap/configured startup paths, both admitted and resource-limit rejection |
| `legacy_startup_connection_ids_belong_to_initial_epoch` | Unobserved initial success allowed, late initial success rejected, explicit new attempt accepted |
| `obsolete_outgoing_failure_does_not_reapply_recovery_backoff` | Extended with unobserved initial and late-initial failure cases |

With admission registration disabled, both admission regressions failed at the
missing-owner assertions (exit 101). Restoring registration made both pass.
Logs: `/tmp/p2p-vpn-recovery-registration-{negative,fixed}.log`.

Admission tests use real local swarms; completion ownership assertions are
deterministic state/event checks, not a physical transport-race measurement.

`disconnect_peer_id` aborts pending attempts as well as closing established
connections, but the swarm also queues terminal events before returning them.
Pending-task cancellation alone therefore does not establish ownership of every
already-produced terminal event. Preserve both cancellation and event guards.

## Timer and Completion Checkpoints

| Source Inspection | Required Next Evidence |
| --- | --- |
| Recovery intervals replayed overdue ticks | RE-3 reproduces all four timers; correction passed workspace and isolated recovery verification |
| QUIC task completion matches peer, role and generation | Reset, cancelled joins, old results and current failure verified through the production completion adapter |
| Close events carry a connection ID and established-count snapshot | Replacement-preserving and last-close dispatch verified; source ordering does not support the hypothesized stale-count race |

### Source-Backed Completion Boundaries

| Boundary | Traced Guarantee | Remaining Evidence |
| --- | --- | --- |
| Connection close | The runner removes the exact connection ID; `PathSet::record_connection_lost` does not decrement a candidate for an untracked ID | Combined dispatch regression passed, preserving replacement capabilities and recovery state |
| libp2p close ordering | `libp2p-swarm 0.47.1` derives the count from remaining IDs, queues the event, and drains queued events before polling the pool again | Do not inject a contradictory zero-count ordering and call it a library race |
| QUIC completion | `finish_quic_connection_task` matches peer, role and generation before removing the handle or applying the result | Old-result and cancellation dispatch across network reset/replacement passed |
| Network reset | `PacketPlaneNegotiator::clear` aborts tasks and clears pending owners without resetting the generation counter | Late-result tests preserve newly started task ownership |

## RE-3: Recovery Timer Catch-Up

Status: reproduced with production timer construction; changed recovery timers
to `MissedTickBehavior::Skip`. Workspace, static, native and recovery checks passed.

| Timer | Work / Clock Semantics |
| --- | --- |
| Redial | Current connection, address, reservation and cooldown state; `Instant::now()` for expiry and retry admission |
| Kademlia maintenance | Current publication/query owners and deadlines; healthy-path suppression retained |
| Queue expiry | Current packet age, replay-session age and membership validity; expired records are still pruned |
| Path probe | Current probe deadlines and path state; expiration precedes sending current probes |

`recovery_timers_coalesce_missed_deadlines_and_resume` primes the production
timers, backdates each deadline by 4.5 periods and checks subsequent tick timestamps.
It also verifies that resetting a timer afterward leaves it usable.

All four replayed expired ticks before the correction. The negative run exited
101 and recorded `redial`, `kademlia`, `queue_expiry` and `path_probe` in its failure.
Log: `/tmp/p2p-vpn-recovery-timer-negative.log`.

- One overdue pass remains immediate; subsequent missed ticks are skipped.
- Periods, first-tick priming, expiry thresholds and retry backoff are unchanged.
- Metrics and code-pairing intervals are unchanged; pairing orchestration is separate.
- This reproduces timer catch-up, not a measured physical-network packet storm.

### Verification

| Check | Result |
| --- | --- |
| Workspace | 1,466 passed; 36 opt-in exclusions; timer regression passed |
| Formatting / Clippy | Formatting and required correctness, suspicious and performance groups passed; advisory warnings remain |
| Android native | x86_64/API 26 compiled in 35.11 seconds; four target warnings |
| Nix source parity | Cached-tool source and test-target assertions passed; not a full package build |
| Public-profile delayed/renumbered recovery | Passed in 167.93 seconds including setup; direct LAN, circuit relay, direct UDP return and healthy dwell |

Logs use `/tmp/p2p-vpn-recovery-timer-` with `negative.log`, `workspace.log`,
`clippy.log`, `android.log`, `nix.log` and `public.log`.
Commands and cached-tool limitations match the checkpoints below.

Recovery artifact:
`/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.b650c6138cea2ede`.
Recorded test-binary SHA-256:
`8f2c522b757c01b2b5586d8caeb817a4fdff8db2360c4167e9cf0340e293dd7c`.

- One cycle; 161.251 seconds excluding setup; original 375-second direct deadline.
- Storage before native compilation: 7.821 GiB; evidence retained.
- No builds during recovery observations or physical deployment.
- No ARM64, device, long-soak, public-WAN or formal-proof claim.

## RE-4: Relay Listener Ownership

Status: listener and acceptance regressions reproduced through production dispatch;
corrections passed full scoped verification. Tests live in
[recovery_event_tests.rs](../../src/runtime/runner/recovery_event_tests.rs).

| Defect / Boundary | Correction / Evidence |
| --- | --- |
| Old automatic close removes replacement readiness | Readiness addresses now retain listener IDs; old close/expiry cannot remove another listener's address |
| Late announcement restores addresses after reset | Announcements require a current configured listener or the matching automatic relay/listener pair |
| Two configured listeners share an address | Each retains ownership; returned dial addresses remain deduplicated |
| Peer-only acceptance consumes a new pending deadline | Automatic acceptance waits for a matching listener announcement; peer-level readiness/metrics behavior remains unchanged |
| Listener error clears another listener's readiness | Automatic termination forgets only its listener's addresses |
| Retired configured error loses its terminal marker | The marker remains until `ListenerClosed`; defensive error/close coverage passed |
| Configured address loss cancels a pending automatic replacement | Reproduced; close/expiry now release automatic state only for a matching listener ID |

The first three regressions failed before correction (exit 101), then passed.
Peer-only acceptance separately failed by consuming the new deadline (exit 101).
Logs: `/tmp/p2p-vpn-recovery-listener-negative.log` and
`/tmp/p2p-vpn-recovery-listener-acceptance-negative.log`.

Pinned `libp2p-relay 0.21.1` queues listener announcements for successful initial
reservations and renewals. Closing a listener drains already queued events before
its terminal close; announcement IDs therefore need explicit ownership checks.

Reservation errors use `ListenerClosed`, not `ListenerError`. The error/close
tests are defensive handler coverage, not evidence that the initial hypothesized
error sequence occurs in the pinned relay transport.

The pending-replacement regression separately failed with the newer listener
missing from its owner map. Raw evidence is retained at
`/tmp/p2p-vpn-recovery-listener-pending-negative.log` (exit 101).

The same event fixture verifies old connection closure and QUIC cancellation/result
dispatch. It preserves current connection capabilities, task generations and
retry owners; current failure and final closure still perform normal cleanup.

### Review Boundary Map

| Boundary | Evidence / Disposition |
| --- | --- |
| Epoch admission and terminal cleanup | RE-1/RE-2: old/current/initial failure dispatch, admitted/rejected dials, late success classification |
| Network-change reset | Production namespace transitions plus explicit epoch, listener and QUIC reset regressions |
| Close while a replacement is healthy | `old_connection_close_preserves_replacement_until_last_connection_closes`; capabilities/path/epoch preserved, last close cleaned up |
| QUIC task replacement/cancellation | `quic_reset_discards_cancelled_and_old_results_without_consuming_new_tasks`; both roles, cancelled joins, old result, current failure and new admission |
| Timer missed deadlines | RE-3; all four recovery intervals coalesce overdue ticks without disabling future ticks |
| Cooldown and healthy settling | Existing `settling_timeline_tests`: 24-hour explicit-time bootstrap backoff, healthy suppression and released capacity |
| Configured/automatic relay retry | RE-4 plus existing pending-listener, accepted-listener, timeout, failure-eviction and 24-hour synchronous-failure tests |
| Probe and packet completion | Existing `stale_packet_responses_release_only_their_own_window_without_path_effects`, token ownership and pending-probe expiry tests |
| Discovery completion | Existing connection-event guards, targeted-query ownership, AutoNAT and Kademlia cadence/expiry tests; no change to completed Kademlia enforcement |

Cancellation is tested with actual aborted Tokio tasks. Old-result and connection
events are injected through production adapters; they do not measure physical
race frequency. Namespace recovery supplies separate successful-transition evidence.

The relay changes add no configuration, route override or public struct fields.
Unowned relay listeners cannot advertise readiness; normal constructors and the
runtime reservation manager already record their listener IDs. No retired-ID
tombstone store or independent epoch registry was added.

## Scope Limits

- Preserve LAN-first discovery, minimal configuration, authentication, DCUtR and relay fallback.
- No retry/deadline relaxation, route injection, manual rescue or production deployment.
- Focused event regressions, namespace checks, workspace/static checks and affected native compilation passed.
- Keep all temporary task storage below 10 GiB; initial total is 7.818 GiB. Preserve raw evidence and use at most two build jobs.

Pairing orchestration, Android service ownership, heap attribution and final
cross-platform certification remain separate workstreams.

## Tracked-Failure Checkpoint

| Verification | Result |
| --- | --- |
| Negative/fixed event regression | Failed before guard; old/current/unobserved cases pass afterward |
| Workspace | 1,462 passed; 36 opt-in exclusions |
| Clippy / formatting | Required correctness, suspicious and performance groups passed; advisory warnings remain; formatting passed |
| Android native | x86_64/API 26 compiled in 34.92 seconds; four target warnings |
| Nix source parity | Cached-tool source and test-target assertions passed |
| Public delayed/renumbered recovery | Passed in 167.96 seconds including setup; unchanged processes/configs, relay fallback, direct UDP return and healthy dwell |

Logs use `/tmp/p2p-vpn-recovery-owner-` with `workspace.log`, `clippy.log`,
`android.log`, `nix.log`, `public.log`, `negative.log` and `terminal-fixed.log`.
Storage before native compilation: 7.819 GiB. No physical devices were deployed.

Recovery artifact:
`/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.a093023255f2a473`.
Binary SHA-256: `db91f7404d678c311cc518e669c373af71864952b90d6a3abd88bac4c2d226c5`.

```sh
cargo test --offline --locked --workspace -- --test-threads=2
cargo clippy --offline --locked --workspace --all-targets -- \
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
cargo fmt --check
P2P_VPN_TUN_E2E_KEEP_TEMP=1 \
P2P_VPN_TUN_E2E_RECOVERY_PROFILE=public \
P2P_VPN_TUN_E2E_RECOVERY_COLLISION=1 \
  "$TEST_BINARY" --ignored --exact \
  tun_namespace_automatic_discovery_recovers_after_link_changes --nocapture
```

The source-parity check uses the documented cached-tool override; it is not a
full Nix package build. Run recovery only after all builds have finished.

Full Nix packages, ARM64, APK/device acceptance and a new long soak are not
claimed by this checkpoint. The overall goal remained active at this stage.

## Dial-Registration Checkpoint

| Verification | Result |
| --- | --- |
| Negative / fixed admission tests | Both fail with registration omitted; both pass with registration restored |
| Workspace | 1,465 passed; 36 opt-in exclusions; includes all four failure-event cases |
| Clippy / formatting | Required correctness, suspicious and performance groups passed; advisory warnings remain; formatting passed |
| Android native | x86_64/API 26 compiled in 40.13 seconds; four target warnings |
| Nix source parity | Cached-tool source and test-target assertions passed |
| Public-profile delayed/renumbered recovery | Passed in 167.99 seconds including setup; direct LAN, circuit relay, direct UDP return, healthy dwell |

Logs use `/tmp/p2p-vpn-recovery-registration-` with `negative.log`, `fixed.log`,
`workspace.log`, `clippy-fixed.log`, `android.log`, `nix.log` and `public.log`.
`clippy.log` preserves an intermediate test-import compilation failure.

The final import cleanup only moves `build_node` behind `cfg(test)`; Clippy
compiled all test targets afterward. Workspace and recovery behavior are unchanged
by that import cleanup and the final whitespace edits.

Recovery artifact:
`/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.36ddea49ccd4b9ca`.
Recorded test-binary SHA-256:
`fc90d95ecdc5a13181b59e5d3b91273e16456fc3aacb99aa58f21b9f062947f8`.

- Original direct-recovery deadline: 375 seconds; no extension.
- One cycle; 161.245 seconds excluding setup; not a long acceptance soak.
- Storage before native compilation: 7.820 GiB; raw evidence retained.
- No builds during recovery observations, physical deployments or public-WAN claims.

This checkpoint uses the commands and cached-tool boundaries above. It does not
claim full Nix packages, ARM64, APK/device acceptance or formal proof coverage;
the repository has no Lean project. The full timer/event goal remained active at this stage.

## Final Verification

| Gate | Final Result |
| --- | --- |
| Workspace | 1,475 passed; 36 opt-in exclusions; all nine event-dispatch tests passed |
| Required Clippy / formatting | Passed; advisory warnings remain, including test-style warnings |
| Android native | x86_64/API 26 compiled in 35.94 seconds; four target warnings |
| Cached Nix source parity | Passed with unchanged source/test-target assertions; not a full package build |
| Namespace compatibility | All 12 selected cases passed; summed test duration 236.60 seconds |
| Public-profile recovery | Passed in 142.90 seconds including setup; one delayed/renumbered cycle and healthy dwell |
| Private-profile recovery | Passed in 177.93 seconds including setup; one delayed/renumbered cycle and healthy dwell |

Final logs use `/tmp/p2p-vpn-recovery-final-` with `fixed-workspace.log`,
`clippy.log`, `android.log`, `nix.log`, `namespace-suite.log`, `public.log` and
`private.log`. The earlier `workspace.log` preserves the 1,474-test checkpoint
before adding the pending-replacement regression and correction.

Both recovery profiles used the same test executable, unchanged daemon processes
and configurations, and the original 375-second direct-recovery deadline.
No task builds ran during observations; no routes or operator rescue were injected.

| Profile | Artifact Suffix | Elapsed Excluding Setup |
| --- | --- | ---: |
| Public | `db7ceb2102a2d6d6` | 136.199 seconds |
| Private | `cd4cc9628e849aaa` | 171.291 seconds |

Artifact directory prefix:
`/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes.`.
Both summaries record SHA-256
`99a8022db6d8b793003da77d73cb6e6deb759e613d13bd69b181c55a0b51b3fd`.

### Commands and Provenance

| Tool / Setting | Used |
| --- | --- |
| Compiler | Rust 1.97.1, `8bab26f4f`, cached Nix tool |
| Cargo executable | Reports 1.97.0, `c980f4866`, cached Nix tool |
| Native build | Cached NDK 28.2.13676358; Android x86_64/API 26; cached `build-std` inputs |
| Build limits | Two Cargo jobs; debug info and incremental compilation disabled; 8 MiB Rust minimum stack |
| Dependencies | Offline and locked; no dependency/flake-lock changes or uncontrolled source builds |
| Task storage | 7.824 GiB before final native compilation; 7.830 GiB after verification; below 10 GiB |
| Nix parity output | `/nix/store/9hn3rmr3wkals62fkj7mncrmlqf3k4j3-p2p-vpn-rust-test-sources` |

Use the matching cached toolchain and target directories, as in the
[verification tooling notes](pairing-cancellation-plan.md#nix-check-tooling).
The native command additionally requires the cached NDK linker/AR/CC and
Android compiler wrapper; it is not an APK/device verification command.

```sh
export RUST_MIN_STACK=8388608 CARGO_BUILD_JOBS=2
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=/tmp/p2p-vpn-review-target
cargo test --offline --locked --workspace -- --test-threads=2
cargo clippy --offline --locked --workspace --all-targets -- \
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
cargo fmt --check
RUSTC_BOOTSTRAP=1 RUSTC=/tmp/p2p-vpn-review-android-rustc \
CARGO_TARGET_DIR=/tmp/p2p-vpn-android-target \
cargo build --offline --locked --package p2p-vpn-android --lib \
  --target x86_64-linux-android -Z build-std=std,panic_abort \
  --config 'source.crates-io.replace-with="cached-android"' \
  --config 'source.cached-android.directory="/tmp/p2p-vpn-kad-android-vendor"'
```

Run only the 12 named cases in `namespace-suite.log`, individually with
`--ignored --exact "$CASE" --nocapture` and `P2P_VPN_TUN_E2E_KEEP_TEMP=1`.
Do not substitute every ignored test; that includes separate resource campaigns.

For recovery, use the command in the tracked-failure checkpoint with `public`
and `private` profiles, collision diagnostics enabled, and soak mode unset.
The standard source-parity attribute is `checks.x86_64-linux.rust-test-sources`;
the recorded run used cached input overrides rather than the unavailable default closure.

### Acceptance Audit

| Requirement | Disposition |
| --- | --- |
| Reconcile completed Kademlia work | Index/checklist updated; RM-1 and historical exclusions preserved |
| Inventory recovery ownership | Connection, epoch, timer, listener, probe, query and QUIC owners traced above |
| Regress stale/terminal events | RE-1 through RE-4 and nine stateful dispatch tests; current loss and retry remain exercised |
| Reproduce before correction | Negative registration, timer, listener, acceptance and pending-replacement logs retained |
| Preserve architecture / defaults | No new configuration, route override, protocol/security change or public construction fields |
| Verify affected behavior | Workspace, required static checks, Nix source parity, Android native, 12 compatibility cases and both recovery profiles passed |
| Publish structured evidence | This report plus the updated broader refactor indexes; raw artifacts retained |
| Atomic publication | Verified corrections use Conventional Commits on `main`; publication is checked before completing the goal |

This review closes its declared timer/connection-event boundary. Pairing orchestration,
Android lifecycle, heap attribution and final cross-platform acceptance remain separate.
It makes no new physical WAN, ARM64, APK/device, full Nix package, long-soak or formal-proof claim.
