# Quiet Discovery Scheduling

## Status

The contract was published before native/runtime changes. Shared scheduling
gates and additive native pause APIs are implemented. Production lifecycle
advertisement stays disabled; activation and energy acceptance remain open.

## Entry Gates

| Gate | Requirement |
| --- | --- |
| Activation | Local capabilities advertise the existing authenticated lifecycle protocol |
| Incoming path | Existing readiness owner verifies a native relay reservation on a current, usable connection |
| Overlay paths | Every currently configured peer has maintenance paused for its current application-demand generation |
| Preparation | A negotiated idle preparation may pause new maintenance before quiet commitment; it cannot drop payloads |
| Pairing | No live code invitation or join operation |
| Serving role | Keep configured relay servers' ordinary maintenance unchanged |

- Cached public status, advertised addresses, accepted peers, and elapsed timers are not incoming proof.
- An unknown/legacy peer is not automatically dormant; preserve its existing compatibility behavior.
- No configured peers is allowed only after the activation and verified-incoming gates pass.
- Genuine application traffic invalidates the maintenance pause atomically; recheck before the next Swarm poll.
- Incoming-path loss, authorization changes, or a live code operation must re-evaluate the gate immediately.

## Native Boundaries

| Owner | Quiet Treatment |
| --- | --- |
| AutoNAT client | Pause starts from the existing probe timer, not the complete behaviour |
| AutoNAT serving | Continue inbound requests, dial-backs, replies, failures, and terminal events |
| Existing AutoNAT work | Drain normally; retain original request IDs, work fences, confidence, and throttle history |
| AutoNAT addresses | Retain observations and normal scheduling state; do not enqueue a timer debt per skipped tick |
| DHT automatic jobs | Pause new record/provider jobs and automatic bootstrap on client-mode DHTs |
| DHT foreground | Keep explicit control, pairing, incoming repair, and essential address/provider publication working |
| DHT serving | Continue inbound protocol handling and current queries; do not replace or disable the behaviour |
| DHT server role | Preserve normal background obligations |
| Diagnostic pause | Keep existing diagnostic query suspension separate; never use it as the production quiet switch |

Pause setters are additive native APIs, defaulting to unpaused. They retain no
second request registry or periodic timer. Resume wakes the existing poller;
existing per-poll and shared query budgets still apply.

## Per-Peer Recovery

- Exclude maintenance-paused peers from periodic targeted data discovery, even when a different peer still needs recovery.
- Connection-close and dial-error callbacks must also avoid starting data queries for paused peers; do not create a false cooldown by starting then cancelling them.
- Cancel only those peers' ordinary data-query owners through the existing native cancellation path.
- Preserve scoped, live control-query ownership and failure cooldowns.
- Keep explicit application recovery, LAN-first selection, incoming-relay repair, and configured reservations available.
- Maintenance events cannot advance application demand or clear negotiated quiet permission.
- Recheck maintenance pause before ordinary discovered-address and provider dial effects, including late closest-peer success/timeout results.
- Cache validated address hints while paused without creating a data-dial cooldown; actual payload demand restores normal eligibility.
- Scoped control-result delivery retains its separate ownership and original deadline, even for a maintenance-paused member.

This change does not declare an unseen/offline legacy peer dormant. Broader
offline-peer retry policy and integrated first-packet recovery remain activation
requirements; suppressing reachability is not an energy improvement.

## Offline Retry Budget

This extension is implemented for explicit lifecycle trials, not production
activation. An unreachable member must not prevent quiet scheduling indefinitely.
Exhausting background retries is not dormancy and cannot retire a legacy path.

| Boundary | Policy |
| --- | --- |
| Scope | Experimental lifecycle builds only; no new configuration or wire format |
| Owner | Extend the existing bounded `RecoveryQueries` per-member state |
| Ticket | Current application-demand version plus underlay generation |
| Startup allowance | One admitted background data lookup per member in a ten-minute window; foreground and connected legacy recovery remain separate |
| Charging | Charge a native query only after admission succeeds; failed admission consumes nothing |
| Lookup exhaustion | Sticky for the ticket after admission or deadline; no replenishment from maintenance |
| Cached-address expiry | Ordinary cached-address retries remain eligible until the original deadline, not merely query admission |
| Admitted lookup | Let its native owner drain before offline pause; spending an allowance cannot immediately cancel the admitted attempt |
| Native capacity | Preserve the existing shared one-query ceiling and query deadlines |
| Retention | Keep authorized exhausted markers across idle pruning; revocation removes them |
| Missing evidence | Missing demand version, owner capacity, or current ticket cannot authorize suppression |

The allowance is a bounded starting policy, not a measured optimum. Each member
has a separate allowance; capacity and deadlines can still prevent admission.
Existing control failure cooldowns remain independent.

### Suppression Gates

- Require lifecycle activation and a currently verified incoming relay path.
- Require no active pairing operation and no configured relay-server role.
- For each offline member, require a matching expired recovery window and no usable connection, supported packet path, queued packet, or in-flight application work.
- Connected legacy members retain existing compatibility behavior; lack of capability support is not proof of dormancy.
- Negotiated maintenance pause remains the separate gate for live idle members.

Apply the offline predicate to both global discovery gates and ordinary targeted
queries, callbacks, retries, and late-result dial effects. Retain validated
addresses; do not create cooldowns for attempts never admitted.

### Work That Survives

| Work | Required Behavior |
| --- | --- |
| Incoming reservation | Keep the verified incoming path and repair it on loss |
| Scoped control / pairing | Preserve request IDs, original deadlines, budgets, and delivery |
| Signed address publication | Keep essential publication and authorization updates |
| Native serving | Continue accepting and answering inbound protocol requests |
| Existing data lookup | Drain the admitted attempt; subsequent cancellation uses its exact ordinary owner, never scoped control |
| Overlay retention | Never grant quiet permission or release a legacy connection from budget exhaustion |

### Reactivation

Real application demand invalidates the ticket before the next Swarm poll and
uses existing LAN-first recovery. Give the new demand generation one ordinary
lookup allowance without clearing a scoped control cooldown.

An underlay change starts a new finite ticket. Capability churn, elapsed
cooldowns, TTL pruning, repeated maintenance, and late query results do not.
Loss of the incoming anchor must resume repair immediately.

### Required Counterexamples

| Scenario | Required Result |
| --- | --- |
| Four idle live members, two offline | Eventually pause background discovery without declaring offline members dormant |
| Six offline members | Preserve a finite initial discovery opportunity for every member |
| Repeated ticks / expiry / late events | Never replenish an exhausted ticket |
| Fresh admitted lookup | Remain owned through completion/timeout; cache validated late hints without reopening background dials |
| Payload / roaming / re-admission | Invalidate stale tickets and restore LAN-first recovery |
| Connected legacy / missing evidence | Preserve compatibility and fail conservatively |
| Pairing / control / publication / anchor loss | Preserve foreground ownership and incoming recovery |

Unit tests cannot prove battery savings or public-network recovery. Repeat live
cold-packet checks and fresh matched energy measurements after integration.

### Hint-Delivery Correction

This correction was specified before implementation and is now implemented for
experimental lifecycle builds. C4 could discard hints during a new LAN grace
period, or strand a hint refused while another address was already dialing.

| Boundary | Required Correction |
| --- | --- |
| Lookup allowance | One background DATA lookup per ticket; foreground work and connected legacy recovery remain separate |
| Cached-address window | Keep existing bounded/backed-off redials eligible until the ticket's original ten-minute deadline |
| LAN-first | Validate and cache hints immediately; defer WAN dial effects until LAN grace ends |
| Pending dial | A refused singleton dial cannot consume the entire remaining cached-address opportunity |
| Expiry | New hints, dial refusal, query completion and cooldown expiry never move the deadline |
| Late native query | Preserve native ownership until terminal completion; post-deadline results may cache but cannot reopen offline background dials |
| Quiet | Wait for both the fixed window and an admitted native query to drain |
| Reactivation | Genuine application demand or an underlay change starts a new ticket, never a maintenance event |

The existing query owner and address cache retain all state. No additional retry
registry, user option, wire message or timer is required. A blocked attempt can
exhaust the deadline without reaching every address; this is an explicit bound.

The 22 runtime/native cases cover retained WAN hints without early WAN
dials, cached replacement admission after a real stale loopback dial fails,
deadline expiry, late results, fresh demand and independent control ownership.
Replacement admission is not a remote handshake or handset reachability proof.

### Implementation Evidence

| Boundary | Focused Evidence |
| --- | --- |
| Query owner | 35 cases pass, including five new cached-window cases; existing lookup-allowance semantics retained |
| Runtime / native relay | 22 quiet-discovery cases pass, including five new hint-window regressions |
| Budget ownership | Only successfully admitted new DATA IDs spend allowance; pending DATA drains before quiet |
| Current authority | Missing evidence, anchor loss, pairing, serving, and underlay changes restore conservative scheduling |
| Reactivation | Fresh queued demand has a DATA-only allowance without clearing control cooldowns or LAN grace |
| Late results | Native DATA drains independently; after the fixed window, even owned results only cache offline hints |
| Foreground | Scoped control ownership and essential signed address publication remain available |
| Resources | Reuse scheduling snapshot capacity; retained tickets stay in the existing bounded owner |
| C5 experimental workspace | 2,161 passed; zero failed; 50 existing or opt-in exclusions across 16 suites |
| C5 default library | 1,758 passed; zero failed; nine existing exclusions |
| Static checks | Rust formatting and Clippy correctness, suspicious-code and performance groups pass; advisory warnings remain |
| Packaging mode | Nix contract confirms separate trial builds and empty default features; no runtime/energy acceptance implied |
| C5 Android build | ARM64 Nix native build, APK signature/alignment checks, 158 Java tests and lint pass; seven lint warnings remain |
| Operational acceptance | Trial Linux counterparts were deployed for a reference capture; the C5 phone candidate remains unmeasured and live wake/recovery verification remains open |

The [C3 cellular preflight](android-lifecycle-membership-preflight-2026-11-14.json)
predates this extension. It failed outgoing first-packet delivery and did not
reach committed quiet toward the phone; it is not acceptance evidence here.

The [C4 build record](android-lifecycle-offline-budget-build-2026-11-14.json)
retains source/artifact hashes and validation results. Building a candidate
does not prove device recovery or battery savings.

The [C4 startup preflight](android-lifecycle-offline-budget-preflight-2026-11-14.json)
retains its failed short diagnostics and restoration evidence. An original-build
control also missed its first inbound packet, then passed outbound relay traffic.
Neither outcome identifies the candidate's failed packet leg or proves savings.

The [same-build C4 repeat](android-lifecycle-offline-budget-repeat-2026-11-14.json)
passed startup cold/steady checks both ways on its second attempt. A Linux host
reboot interrupted idle observation. It contains no energy window and does not
prove extended idle wake; incomplete restoration checks remain explicitly marked.

The [C5 hint-window build](android-lifecycle-hint-window-build-2026-11-14.json)
retains corrected source/artifact hashes and verification results. It was sealed
without deployment or energy measurement; no new battery-improvement claim is made.

The [latest reference capture](android-lifecycle-reference-capture-2026-11-14.json)
retains valid energy but a failed post-idle incoming probe. The frozen harness
stopped before the C5 phone arm and verified restoration; no comparison exists.

## Validation

| Boundary | Required Evidence |
| --- | --- |
| Native AutoNAT idle | Repeated polls past a due timer start no probe while paused; cached state is not pending work |
| Native AutoNAT drain | Pausing with an owned request preserves it through failure/completion and does not start a retry |
| Native AutoNAT serving | A paused server accepts a real encrypted probe and completes its dial-back/reply |
| Native resume | Unpause starts eligible work through normal scheduling without clearing confidence or throttles |
| Native DHT idle | Due provider/value jobs remain retained without new automatic queries |
| Native DHT foreground | Explicit queries complete and inbound requests still work while background jobs are paused |
| Native DHT resume | Deferred jobs resume under existing query/capacity/per-poll bounds |
| Runtime gates | Passive capability, missing/stale incoming owner, unpaused peer, pairing, and serving roles prevent entry |
| Mixed peers | Dormant peer is not a recovery target when another peer remains missing |
| Control isolation | Per-peer cancellation preserves exact control owners and their original deadlines |
| Compatibility | Existing default API behaviour, wire formats, minimal JSON, and native Nix options remain unchanged |
| Packaging | Shared Linux/Android runtime and patched native sources match evaluated package sources |
| Energy | Fresh matched replicated paired/reversed cellular app mAh/hour and modem evidence remain required |

Unit and local native socket checks do not prove extended dormancy, public-WAN
availability, mobile roaming, or the measured 10x target.

## Implementation

| Surface | Implementation |
| --- | --- |
| AutoNAT | `set_client_probes_paused` pauses future starts; one native waker resumes polling |
| DHT | `set_background_jobs_paused` retains jobs/store and leaves foreground queries and serving active |
| Runtime | Reconcile the current gate before each Swarm poll; no new timer, request ledger, or user setting |
| Targeted discovery | Skip paused peers even when another peer needs recovery; cancel only ordinary data-query owners |
| Existing work | Keep native terminal ownership and failure history; active control and incoming repair remain explicit foreground work |

## Focused Evidence

| Check | Result |
| --- | --- |
| AutoNAT native tests | Five pass, including two new paused-client/server and due-timer/resume regressions |
| AutoNAT state | Due paused timer starts no probe; resume wakes polling; owned failure drains without retry; public status/confidence survive toggles |
| DHT background | Due jobs remain stored; resume obeys the existing two-query ceiling and one-new-query-per-poll bound |
| DHT foreground | Real encrypted TCP value lookup completes while both endpoints' background jobs are paused |
| Runtime gates | Three unit tests pass: preparation versus commitment, activation/incoming/serving/pairing gates, control ownership, and mixed-peer recovery |
| Real incoming owner | Fresh relayed control completes with quiet scheduling applied; payload demand, retiring owner, and stale epoch resume scheduling |
| Connection callbacks | Preparation/dormancy do not create data queries or cooldown state; active peers retain LAN-first connection-loss recovery and ordinary failed-dial discovery |

- The relay fixture injects the capability and maintenance-preparation state.
- Its reservation, connection ownership, circuit, and control exchange use real sockets.
- This is not bilateral idle negotiation or first IP payload recovery through an activated daemon.
- AutoNAT's first public-status transition can retain confidence zero; the tests preserve its existing semantics.

```sh
cargo test --offline --locked --test autonat_work -- --test-threads=2
cargo test --offline --locked --lib paused_background_jobs -- --test-threads=2
cargo test --offline --locked --lib quiet_discovery -- --test-threads=2
```

## Verification Record

| Check | Result |
| --- | --- |
| Workspace/all targets | 2,095 passed; zero failed; 50 existing/opt-in exclusions |
| Connection callbacks | Six cases pass: active, preparing, and dormant peers under close and failed-dial events |
| Cold control compatibility | Explicit native public-client namespace case passes with the new native APIs |
| Clippy | Correctness, suspicious, and performance groups pass; advisory warnings remain |
| Formatting | Workspace Rust and both touched native modules pass |
| Package sources | Linux and Android ARM64/x86-64 match runtime, all three native patches, and lockfile byte-for-byte |
| Test inventory | All 41 Rust integration/support files match the Linux source |
| Build resources | Cached Nix tools, one existing approximately 7 GiB target, two jobs, incremental compilation off |
| Full Nix/APK builds | Not run; the earlier offline dry run required 493 uncached derivations including bootstrap compilers |
| Operational acceptance | Production capability remains unadvertised; no APK/service deployment or new energy measurement |

The initial callback test assumed active connection loss immediately starts a
public query. Its corrected assertion preserves the existing LAN-first grace;
active failed dials still start discovery. Production LAN-first behavior was
not changed to satisfy the test.

These executable checks are not a formal proof or a battery comparison.
