# Demand-Driven Connection Lifecycle

## Status And Scope

| Item | Status |
| --- | --- |
| Decision | Separate incoming reachability from active per-peer data paths |
| Runtime implementation | Incoming readiness, idle/wake, retirement, member/hostname updates, cold control recovery, and quiet discovery scheduling integrated; activation pending |
| Default configuration | Preserve minimal JSON and native NixOS workflows; no required new settings |
| Platforms | Shared Rust policy; Android and Linux must cooperate |
| Battery acceptance | Measured 10x idle-cellular efficiency with feature preservation; not yet demonstrated |

This document is the implementation contract, not evidence of deployed behavior.
The [combined isolation report](android-cellular-combined-isolation.md) reproduces
high idle cellular power but does not identify the exact remaining packet source.

- [Retirement ownership and fences](demand-lifecycle-retirement.md)
- [Control work and stream retirement](demand-lifecycle-control-work.md)
- [Bounded cold control discovery](demand-lifecycle-control-discovery.md)
- [Quiet discovery scheduling](demand-lifecycle-quiet-discovery.md)
- [Maintenance drain before idle](demand-lifecycle-drain.md)
- [Public relay idle validation](relay-idle-validation.md)

## Requirements

| ID | Required Behavior |
| --- | --- |
| R1 | A powered-on connected phone accepts incoming overlay traffic while its screen is off |
| R2 | Unused peer paths can become dormant without triggering repair traffic |
| R3 | Actual traffic in either direction wakes a peer; bounded first-packet latency is acceptable |
| R4 | Direct QUIC remains preferred for active data; TCP/streams and relay fallback remain available |
| R5 | Discover LAN first during startup, network changes, and demanded path establishment |
| R6 | Keep identities, memberships, DNS, pairing, network isolation, revocation, and always-on VPN intact |
| R7 | Do not require user-managed relays, manual routes, proprietary push, or extra minimal-config fields |
| R8 | Report missing relay reachability and legacy-peer limitations instead of silently claiming readiness |

## Existing Idle Work

| Owner | Current Scheduling | Planned Treatment |
| --- | --- | --- |
| Libp2p QUIC | Default 5-second keepalive, 10-second idle timeout | Close dormant overlay connections; audit infrastructure transport separately |
| Libp2p ping | 60-second interval; 20-second timeout | Retire unused negotiated paths; preserve the native incoming anchor |
| Packet-plane probes | 5-second timer across available peer sessions | Demand-gate; avoid probing unused backup transports |
| MTU probes | Datagram probes include MTU-sized payloads | Discovery on establishment/path change; not a permanent idle heartbeat |
| Peer recovery | Periodically seeks disconnected configured peers | Recover demanded peers; distinguish dormant from failed |
| DHT maintenance | Health-based suppression and background work | Demand-gate peer lookups; retain bounded address/provider lease refresh |
| AutoNAT | Library scheduling plus observed-address triggers | Coalesce/deduplicate; run for changed reachability, not every identify response |
| Membership sync | Compares snapshots before fetching pages | Preserve event-driven updates; do not turn idle state into membership loss |
| Android status polling | Local JNI snapshot polling | Do not classify local snapshots as network activity |

Sources: [host](../../src/runtime/p2p.rs), [runner](../../src/runtime/runner.rs),
[retention](../../src/runtime/runner/control_connection_retention.rs),
and the dependency versions recorded in `Cargo.lock`.

## Two Independent Lifecycles

### Network Reachability

| State | Meaning | Permitted Work |
| --- | --- | --- |
| Establishing | Acquire a useful incoming path | Bounded LAN discovery, public discovery, relay reservation |
| Available | Accepted live relay reservation or verified incoming LAN path | Essential lease refresh and bounded updates |
| Recovering | Incoming path was lost | Finite attempt round, LAN first, then alternate candidates |
| Backoff | Attempt round exhausted | Bounded cooldown; retry incoming reachability without a per-peer mesh storm |

- Target one accepted automatic relay reservation per network instance during idle.
- Keep candidate addresses as metadata; do not keep every candidate connection open.
- Permit one additional reservation during bounded make-before-break handover.
- Preserve explicit user relay overrides and disclose their energy implications.
- Multiple network identities remain isolated; do not silently share authorization across instances.

The incoming path is an exception to peer demand gating. Its loss must trigger
automatic bounded recovery even when no local application is sending packets.
An unavailable public relay is not something an idle policy can guarantee away.

### Per-Peer Data Paths

| State | Entry | Exit |
| --- | --- | --- |
| Dormant | No current application demand; both peers agreed to quiet | Queued payload, accepted incoming payload, or authenticated wake |
| Establishing | Demand exists and no usable path exists | Usable path, exhausted attempt round, cancellation, or revocation |
| Active | Recent demand or outstanding application work | Idle eligibility, path failure, or revocation |
| Quiescing | Idle offer is awaiting acknowledgement | Matching acknowledgement, new demand, timeout, or changed reachability |
| Recovering | A demanded path failed | LAN/direct/relay success or bounded failure cooldown |
| Legacy | Remote lifecycle support absent or unavailable | Renegotiation on a new authenticated connection |

Dormant is not Offline, Unhealthy, or Revoked. Connection/session expiration
must not delete a peer, route authorization, DNS record, or network membership.

## Demand Definition

| Event | Renews Application Activity? | Other Action |
| --- | --- | --- |
| Authorized outbound TUN payload | Yes | Queue within bounded byte/age limits; establish a path |
| Accepted authenticated inbound payload | Yes | Deliver; permit a reply path |
| Explicit application/service request | Yes | Track outstanding work until completion/cancellation |
| User-initiated peer diagnostic | Bounded explicit demand | End when the diagnostic finishes |
| Path probe, pong, keepalive, ACK, transport ping | No | Update transport evidence only |
| Identify, capability refresh, DHT, NAT check, relay renewal | No | Separate control budget |
| DNS lookup answered from local authenticated membership | No | Do not wake the entire network |
| Membership/revocation/hostname update | No permanent activity | Deliver bounded authenticated control work |
| Unknown/unauthorized incoming packet | No | Apply existing validation/rate limits |

Default activity window: 45 seconds after actual payload demand. This is an
initial policy value, not an energy result. Pending payloads, in-flight service
work, or blocked queue delivery prevent quiescence even if the timer expires.

The [activated runner contract](demand-lifecycle-activated-runner.md#first-wake-packet)
permits one first-wake IP packet to remain queued for a fixed 30 seconds after
negotiated quiet. Other packets retain their configured ordinary age limit.

Track application activity separately from transport activity. A heartbeat must
never keep its own activity lease alive. Use monotonic deadlines only.

## Bilateral Quiet Protocol

The [wire contract](demand-lifecycle-wire.md) defines capability fields, framing,
limits, identity/scope validation, and response correlation before implementation.

### Negotiation And Framing

| Item | Contract |
| --- | --- |
| Capability | Optional `supports_demand_lifecycle` boolean; absent means false |
| Protocol | Separate `/p2p-vpn/lifecycle/1` stream; do not add unknown enum variants to `/control/1` |
| Encoding | Bounded control serialization consistent with existing codecs; maximum 2 KiB per message |
| Authentication | Existing secure libp2p peer identity plus current network/member authorization |
| Epoch | Fresh random 128-bit local runtime epoch, tied to a validated capability exchange |
| Revision | Non-wrapping monotonic local revision; invalidate outstanding offers after new demand |
| Scope | Exactly one authorized peer in one instance; never accept another peer's claimed identity |
| Time | No remote wall clock; durations are bounded local monotonic observations |

Each message binds sender/receiver epochs, sender revision, network context,
and the idle offer it acknowledges where applicable. Unsupported versions,
oversized fields, invalid revisions, or unknown epochs cause bounded rejection.

### Exchange

1. Validate support and network authorization using the existing control exchange.
2. After 45 seconds without payload demand, offer `Idle` only when local queues/work are empty.
3. The receiver acknowledges only if it is also idle and an incoming path is available.
4. The sender commits only a matching acknowledgement while its demand revision is unchanged.
5. Both endpoints suppress maintenance toward that peer and may close its data paths.
6. New local demand cancels pending quiet state immediately; send payload/wake via an available path.

- An accepted incoming payload overrides a previous quiet agreement immediately.
- A remote idle request cannot force a locally active transfer into dormancy.
- Crossed idle offers and duplicate acknowledgements must be idempotent.
- Delayed acknowledgement after a new packet, restart, or network change must not close the new path.
- Closing a direct path is safe only after the peer has usable incoming-path information.
- No periodic per-peer idle-message exchange is allowed merely to maintain Dormant status.

Quiet permission lasts within the validated runtime epochs; it is invalidated
by relevant authorization, epoch, or incoming-path changes. Reconnect can
renegotiate support; stale state does not survive daemon restart as authority.

### Legacy Peers

| Remote Behavior | Local Behavior |
| --- | --- |
| Missing capability | Preserve compatible behavior; label `legacy` |
| Capability true but protocol unsupported | Fall back for the validated capability epoch; do not retry every timer tick |
| Quiet rejected because peer is busy | Continue Active; bounded retry after new idle eligibility |
| Negotiation timeout | Keep reachability; bounded retry/backoff, no close based on uncertain acknowledgement |
| Older peer continues sending probes | Preserve interoperability; expose compatibility traffic instead of claiming full savings |

Mixed-version compatibility is required, but its idle-energy cost is explicit.
The 10x acceptance topology requires all participating endpoints to support the
lifecycle; legacy fallback must still pass separate interoperability checks.

## Circuit Relay Audit

The [Circuit Relay v2 specification](https://github.com/libp2p/specs/blob/master/relay/circuit-v2.md)
requires an active relay connection for a reservation to remain valid. A
reservation admits future incoming circuits; it is not a permanent peer circuit.

| Constraint | Evidence And Design Consequence |
| --- | --- |
| Reservation expiry | Use the returned expiry; renew before it, never assume a universal duration |
| Rust client renewal | `libp2p-relay` 0.21.1 schedules renewal at 3/4 of the remaining lifetime |
| Reservation retention | The relay client marks a reserved connection retained; transport upkeep remains independent |
| Circuit duration/data cap | Honor returned limits; idle peers must not hold open and repeatedly recreate capped circuits |
| Local server defaults | One-hour reservation; two-minute circuit; 128 KiB circuit byte cap; not a public-server guarantee |
| QUIC inactivity | Default libp2p QUIC timeout is 10 seconds with 5-second keepalives; a 60-second keepalive alone breaks this contract |
| Candidate idle transport | Prefer TCP for the small infrastructure footprint when QUIC requires frequent upkeep; active peer data still prefers QUIC |
| Remote infrastructure pings | Public relays can originate traffic; measure it and select compatible candidates, not just suppress local sends |
| Public availability | Reservations may be refused or disappear; bounded failover and truthful readiness are mandatory |

Do not extend public-server timeouts by assumption or treat DHT records as a
live incoming channel. A public TCP reservation surviving long quiet periods
and subsequent inbound circuit establishment is a required operational gate.

## Discovery, Recovery, And Control

- Keep LAN listeners active. Coalesce initial/network-change LAN discovery into bounded bursts.
- For demand, try valid LAN candidates before WAN/direct/public relay work; a cached relay can carry first traffic while promotion runs.
- Limit attempts per round, per peer, and per network. Bound concurrency, elapsed time, queue memory, and cancellation.
- Back off offline unused peers instead of repeatedly crawling the public DHT for them.
- A network change invalidates old endpoint evidence and starts bounded incoming-path repair; it does not eagerly rebuild every peer path.
- Preserve private signed-address records: 30-minute TTL, 15-minute refresh.
- Public address updates use the existing provider key and coalesced 15-minute refresh owner, not custom public DHT values.
- Preserve native provider and relay renewal deadlines; do not add a duplicate periodic publisher.
- Address publication is coalesced control work; never suppress it indefinitely while advertising a stale incoming path.
- Membership and hostname changes propagate through bounded authenticated exchanges, including wake-on-control when necessary.
- Revocation takes effect locally immediately; any incoming wake/payload must recheck current authority.
- Pairing remains an explicit temporary workload; only live pairing operations may request their discovery budget.

## Transport And Android Integration

| Surface | Required Change |
| --- | --- |
| Retention | Retain demanded overlay paths and incoming infrastructure separately |
| Libp2p ping | Per-connection policy, not one global switch that disables all liveness checks |
| QUIC | Preserve active transport semantics; do not make incompatible timeout promises to public peers |
| Packet-plane sessions | Retire unused sessions without scheduling immediate hello retries |
| Probes | Test a selected demanded path; avoid permanent MTU-sized probing of backup paths |
| Queue recovery | A queued packet wakes only its destination; enforce normal expiry/drop counters |
| Android screen | Screen state is not demand; screen-off transfers and incoming access must work |
| Android always-on | VPN service stays enabled; do not disconnect the VPN to obtain a low reading |
| Underlay changes | Existing callbacks wake incoming-path recovery and invalidate stale negotiation |
| Multiple instances | Shared TUN remains active; every enabled network keeps isolated policy/state |

## Bounds And Observability

| Resource | Required Bound |
| --- | --- |
| Lifecycle records | At most one per authorized configured/discovered member within existing member limits |
| Pending offers | At most one outbound offer and one bounded inbound exchange per peer |
| Peer-supplied state | Fixed-size epochs/revisions and bounded codec; no unbounded per-message allocation |
| Retry state | One next deadline and bounded failure count; prune on revocation/config removal |
| Wake/control requests | Per-peer plus network-wide rate limits; admission before expensive work |
| Incoming reservations | One idle target plus bounded handover; explicit config overrides remain visible |
| Diagnostics | State transitions, demand age, quiet rejection, wake reason, anchor state, drops, and retry suppression |

Existing `connected_peers` remains a transport observation. Add explicit
available/dormant/recovering/legacy states rather than redefining that field
or making a DNS peer disappear when its transport closes.

Logs must omit private keys, pairing secrets, packet payloads, and private
raw battery reports. Idle signaling is not authority to change routes or members.

## Implementation Sequence

| Step | Deliverable | Acceptance |
| --- | --- | --- |
| 1 | Publish this contract | Structured review and checked references before runtime edits |
| 2 | Pure monotonic lifecycle model | Demand/probe separation, stale-ACK rejection, bilateral races, bounds |
| 3 | Versioned authenticated exchange | Capability compatibility, bounded codec, rate limits, epoch validation |
| 4 | Runtime/transport integration | Dormant does not trigger redial, probes, renegotiation, or retained peer connections |
| 5 | Incoming path policy | Public relay quiet-period admission and failure/replacement evidence |
| 6 | Android/Linux end-to-end | Screen-off inbound/outbound, active transfer, roaming, DNS, membership, legacy peers |
| 7 | Matched energy comparisons | Replicated paired/reversed-order runs; functionality gates remain intact |

The repository has no current Lean project. Model invariants can be checked
with exhaustive short Rust traces initially; this is not a formal proof of
the production runtime. Evaluate a focused proof once the model stabilizes.

### Implementation Progress

| Component | Evidence | Remaining Boundary |
| --- | --- | --- |
| Protocol contract | Published before runtime edits | Operational assumptions still require verification |
| [Policy model](../../src/runtime/lifecycle.rs) | Fixed-size state, epoch/revision-bound exchanges, and wake announcements that preserve application time | Shared runner driver is integrated; production advertisement remains gated |
| [Model tests](../../src/runtime/lifecycle/tests.rs) | 34 tests, including 111,111 short bilateral event sequences | These model checks are not a production-runtime proof |
| Idle delivery safety | Authenticated reply-sent/ACK commitment rechecks authority, epochs, demand, incoming path, and native work | Integrated first-payload and public-relay validation remain |
| [Wire exchange](demand-lifecycle-wire.md) | 23 tests: bounded codec, contextual authentication, epochs, scope, stale/revoked response rejection | Quiet capability remains unadvertised |
| [Runner dispatcher](demand-lifecycle-dispatch.md) | Native readiness, automatic idle/wake, bounded slots/rates, commit-time validation, and native-owned control quarantine leases | Integrated activation, broader offline-peer retry policy, and operational validation remain |
| [Control delivery](demand-lifecycle-control-delivery.md) | Coalesced member/checkpoint updates, gated-roster isolation, hostname catch-up, and scoped cold discovery without payload demand | Public-WAN and extended dormancy validation remain |
| [Application demand](demand-lifecycle-activity.md) | Shared forwarding hooks, atomic quiet fencing, transport retirement, and demand-gated probes/recovery | Broader offline-peer retry policy and first-payload/roaming validation remain |
| [Quiet discovery](demand-lifecycle-quiet-discovery.md) | Native AutoNAT client and DHT background gates preserve serving, owned requests, control, and incoming repair | Activated bilateral dormancy, public-WAN/roaming, and cellular energy remain unverified |
| TCP/QUIC loopback | Idle commits after real response-sent; either endpoint can initiate wake | Virtual idle clock and fixture readiness; not long-idle or public-relay evidence |
| Compatibility | Legacy defaults; dispatcher preserves unsupported fallback across ordinary capability refreshes | Operational peer-state presentation and full mixed-version deployment tests remain pending |
| Energy impact | Source/transport evidence only; no updated APK/service deployment in these integration checks | All measured improvement and feature-preservation gates remain open |

The model owns no heap allocations and retains at most one pending offer and
one pending reply. Table admission, revocation cleanup, attempt budgets, relay
selection, and transport effects remain responsibilities of the integration.

## Completion Gates

| Gate | Required Evidence |
| --- | --- |
| Idle access | First inbound and outbound payload after 30+ minutes of dormancy; no management intervention |
| Carrier reachability | Screen-off cellular incoming request after the quiet period; actual public relay reservation evidence |
| Relay failover | Remove selected relay and observe bounded automatic incoming-path recovery |
| Path promotion | Relayed first traffic can become direct QUIC; no forced TCP-only acceptance |
| Active workload | Long bidirectional transfers stay active and preserve delivery/throughput expectations |
| Network change | Repeated LAN/cellular transitions recover independently, with LAN-first promotion |
| Features | Pairing, membership/revocation, indirect DNS, instances, always-on, minimal JSON, and native Nix configuration |
| Compatibility | New/new quiet behavior; old/new retained interoperability with explicit legacy status |
| Safety | No unauthorized wake/state changes, stale idle ACK closes, unbounded memory, or retry storms |
| Energy | Fresh original baseline and each candidate under matched workload; at least 90% lower app-attributed idle mAh/hour |
| Corroboration | Modem-energy rails support the reduction; not just app attribution or fewer counters |
| Repetition | Paired/reversed-order valid trials; retain raw endpoints and every rejected attempt |
| Restoration | APK/profile/settings and all temporary host changes restored and verified after diagnostic runs |

Historical 295.273 mAh/hour is context, not a substitute for a contemporaneous
baseline. VPN-off is a reference control, never an acceptable optimization.
No completion claim is permitted from a dormant counter, green unit tests,
or a design document without the operational and energy evidence above.
