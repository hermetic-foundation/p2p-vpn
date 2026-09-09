# Recovery Event Ownership Review

## Status

Active; baseline `ceb6e4e2`. This follows the completed Kademlia resource
workstream and RM-1. It does not complete the broader reliability review.

## Plan

1. Reconcile historical checklist entries with final evidence.
2. Trace recovery owners, reset boundaries and terminal-event effects.
3. Reproduce stale-event defects through production dispatch before fixing them.
4. Verify focused corrections and applicable recovery behavior.
5. Publish findings and remaining review boundaries.

## Ownership Inventory

Symbols refer to [the runtime adapter](../../src/runtime/runner.rs).
This initial inventory is not completed verification.

| Boundary | Owners / Entry Points | Review Status |
| --- | --- | --- |
| Network transition | `handle_runtime_network_change`, `ConnectionEpochs::advance` | Reset sequence traced; stale terminal effects under review |
| Dial start / establishment | `ConnectionEpochs`, `handle_swarm_event` | Application/startup registration corrected; admission regressions pass |
| Outgoing failure | `PublicDiscoveryBackoff`, `DiscoveredPeerAddresses` | Old-epoch and late initial-epoch failures guarded; current failures retain backoff |
| Connection close | `active_connections`, `record_path_closed`, capability/session invalidation | Source traced; replacement-preservation tests pending |
| Periodic work | Runtime intervals, `handle_redial_tick`, `KademliaMaintenance` | RE-3 catch-up correction verified; existing cooldown/suppression coverage retained |
| Relay reservations | `ConfiguredRelayReservationRetries`, `RelayReadiness`, `AutoRelayState`, listener sets | Old listener failure and replacement ownership review pending |
| Packet negotiation | `PacketPlaneNegotiator`, QUIC task generations | Existing generation implementation; cancellation/completion review pending |
| Probes / stream completions | `PathProbeTracker`, `PacketInFlight` | Completed targeted fixes retained; timer integration review pending |
| Discovery completions | `stale_behaviour_event_connection`, targeted query owner | Completed Kademlia ownership retained; connection-bound event review pending |

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
| QUIC task completion matches peer, role and generation | Reconcile existing replacement/cancellation tests with the network-change reset path |
| Close events carry a connection ID and established-count snapshot | Verify per-connection retirement preserves a replacement path; inspect library event ordering before claiming a stale-count defect |

### Source-Backed Completion Boundaries

| Boundary | Traced Guarantee | Remaining Evidence |
| --- | --- | --- |
| Connection close | The runner removes the exact connection ID; `PathSet::record_connection_lost` does not decrement a candidate for an untracked ID | Combined dispatch regression preserving replacement capabilities and recovery state |
| libp2p close ordering | `libp2p-swarm 0.47.1` derives the count from remaining IDs, queues the event, and drains queued events before polling the pool again | Do not inject a contradictory zero-count ordering and call it a library race |
| QUIC completion | `finish_quic_connection_task` matches peer, role and generation before removing the handle or applying the result | Old-result and cancellation dispatch tests across network reset/replacement |
| Network reset | `PacketPlaneNegotiator::clear` aborts tasks and clears pending owners without resetting the generation counter | Verify late results cannot consume newly started task ownership |

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

## Relay Listener Ordering Hypothesis

`ListenerError` removes a configured listener from the retired-listener set and
requests its removal. A later `ListenerClosed` no longer sees that retired marker
and processes its addresses through relay readiness.

Pinned `libp2p-relay 0.21.1` emits `ListenerClosed` for reservation errors, not
`ListenerError`. The initial error-then-close hypothesis is therefore not yet
a demonstrated production relay path; an injected error alone would not prove it.

Review ordinary close ordering against replacement listeners with the same relay
base address, especially automatic listeners reset without a retired-ID marker.
No reproduced listener defect or fix is claimed yet.

## Constraints and Remaining Work

- Preserve LAN-first discovery, minimal configuration, authentication, DCUtR and relay fallback.
- No retry/deadline relaxation, route injection, manual rescue or production deployment.
- Use focused event regressions, applicable namespace checks, workspace/static checks and affected native compilation.
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
claimed by this checkpoint. The overall goal remains active.

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
the repository has no Lean project. The full timer/event goal remains active.
