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
| Dial start / establishment | `ConnectionEpochs`, `handle_swarm_event` | Existing old-epoch success rejection; direct-dial tracking needs inspection |
| Outgoing failure | `PublicDiscoveryBackoff`, `DiscoveredPeerAddresses` | Old-epoch regression reproduced and guarded; application registration still open |
| Connection close | `active_connections`, `record_path_closed`, capability/session invalidation | Source traced; replacement-preservation tests pending |
| Periodic work | Runtime intervals, `handle_redial_tick`, `KademliaMaintenance` | Existing timeline coverage; reset/event interactions pending |
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

Status: source-confirmed tracking gap; transport-event regression and correction pending.

| Boundary | Inspected Behavior |
| --- | --- |
| Runner registration | `IncomingConnection` and `Dialing` events call `ConnectionEpochs::record_started` |
| Application dialing | `dial_known_peer_addresses` and `dial_configured_peer` call `Swarm::dial` directly |
| Startup dialing | `p2p::install_listeners_and_dials` starts bootstrap and configured-peer dials before runner epoch initialization |
| Pinned library | `libp2p-swarm 0.47.1` emits `SwarmEvent::Dialing` in its `ToSwarm::Dial` handler, not in `Swarm::dial` itself |
| Unknown success | `record_established` assigns an unknown connection to the current epoch |

Consequently, registering only observed `Dialing` events does not cover every
application-started attempt. A fix must cover attempt admission as well as terminal
effects, without dropping valid unobserved failures or creating unbounded tombstones.

Next: register successful application dial admission with the existing epoch
owner and test network-change completion plus immediate rejection cleanup.
Thread the owner through existing runtime adapter contexts rather than adding
global state, an unrelated behaviour or a second epoch registry.

Startup handoff must preserve existing public construction APIs. Inspect an
additive tracked-construction path or initial-attempt handoff before changing
`P2pNode`'s public fields; adding a required field breaks struct-literal consumers.

`disconnect_peer_id` aborts pending attempts as well as closing established
connections, but the swarm also queues terminal events before returning them.
Pending-task cancellation alone therefore does not establish ownership of every
already-produced terminal event. Preserve both cancellation and event guards.

## Timer and Completion Checkpoints

| Source Inspection | Required Next Evidence |
| --- | --- |
| `RuntimeTimers::new` uses default interval missed-tick behavior | Test delayed polling and whether recovery/probe work bursts after missed deadlines |
| QUIC task completion matches peer, role and generation | Reconcile existing replacement/cancellation tests with the network-change reset path |
| Close events carry a connection ID and established-count snapshot | Verify per-connection retirement preserves a replacement path; inspect library event ordering before claiming a stale-count defect |

## Relay Listener Ordering Hypothesis

`ListenerError` removes a configured listener from the retired-listener set and
requests its removal. A later `ListenerClosed` no longer sees that retired marker
and processes its addresses through relay readiness.

Review the error-then-close ordering against a replacement using the same relay
base address. Automatic relay listeners have separate ID ownership; address-expiry
events also need checking. No reproduced defect or fix is claimed yet.

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
