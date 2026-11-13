# Lifecycle Dispatcher

## Deployment Boundary

| Surface | Current State |
| --- | --- |
| Handler | Registered in the shared libp2p host; dispatched by the Linux/Android runner |
| Quiet capability | Still false and omitted from normal capability messages |
| Readiness | Native incoming-anchor, queue, in-flight, routing, codec, and transport owners |
| Automatic driver | Existing tick and capability events drive idle/wake; no separate timer or disconnected lifecycle dial |
| Maintenance | Generation-bound preparation drains probes; only committed quiet permits retirement |
| Transport timing | Native ping is 60 seconds; QUIC keepalive/idle remain the compatible 5/10 seconds |
| Energy result | None from this step; no device deployment or battery comparison |

The [wire protocol](demand-lifecycle-wire.md) and
[lifecycle design](demand-driven-lifecycle.md) remain the contracts.
Registration alone does not enable dormancy or prove incoming reachability.

## Ownership And Bounds

| Resource | Bound And Lifetime |
| --- | --- |
| Peer records | 256, or the larger initial explicitly configured peer count |
| Admission | Currently authorized overlay peer with recorded validated capabilities; never public infrastructure peers alone |
| Pending outbound | At most the configured control-stream limit per instance; one per peer |
| Pending inbound commits | Separately at most the control-stream limit per instance; one per peer |
| Payload allocation | Existing 2 KiB codec bound; retained requests stay within the same bound |
| Request budget | Four requests/second per peer and 64/second per instance; shared by incoming requests and outgoing negotiation |
| Busy retry | No new idle offer for 45 seconds after an offer; expiry never schedules another request itself |
| Semantic expiry | Ten seconds using monotonic time; no timer-created discovery or recovery |

Budgets are ceilings, not scheduled traffic. A disconnected peer does not cause
`send_request` to create an automatic libp2p dial. Demanded path establishment
belongs to the separate bounded LAN/direct/relay recovery owner.

### Transport Slots

1. Reserve a slot when submitting an outbound libp2p request.
2. Invalidate the semantic ticket on new demand, epoch replacement, expiry, or revocation.
3. Keep the transport slot until its matching authenticated response or outbound-failure event.
4. Match both peer and request ID before releasing the slot.

The lifecycle dispatcher retains native ownership until terminal delivery.
Clearing a semantic map entry cannot cancel a pending dial/stream queue. Even
the patched native cancellation API requires waiting for its terminal event.

Bounded transport tombstones prevent repeated cancellation from bypassing the
instance limit. Control capability and checkpoint-page owners use the new API;
the lifecycle dispatcher's existing native-slot policy remains unchanged.

Revocation immediately removes peer policy and pending commit authority.
Only the bounded peer/request-ID transport tombstone may outlive that removal;
it grants no route, member, wake, or idle authority.

## Commit Checks

| Boundary | Checks |
| --- | --- |
| Incoming request | Current connection epoch, configured peer authority, recorded capabilities, rate budget, exact scoped wire validation |
| Outbound response | Retained request ID, authenticated peer, current connection, authorization, both epochs, original scope and reply correlation |
| Idle reply sent | Exact reply ID and connection; revalidate the original request under current membership; current readiness and model ticket |
| Capability refresh | Reset policy only when effective lifecycle epochs change; do not refill the peer request budget |
| Unsupported peer | Protocol refusal or explicit unsupported reply selects Legacy for that capability epoch; ordinary refresh cannot restart negotiation |
| Excess request | Drop without generating another response; retain bounded diagnostic counters |

New application demand cancels pending quiet permission immediately. It cannot
recycle a libp2p request slot prematurely. Maintenance and capability observation
do not renew the model's application-demand timestamp.

Retained native flights keep their original bounded request and underlay epoch.
An expired semantic ticket cannot authorize a late idle/wake acceptance. A valid
unsupported result can still select Legacy for its current capability epoch,
including after a crossed offer committed quiet; fallback clears that permission.

## Diagnostics

- Startup emits `event=peer_lifecycle_policy mode=passive` while control-delivery and activation gates remain.
- Successful quiet commitment emits `peer_lifecycle_dormant`; a queued demand announcement emits `peer_lifecycle_wake_queued`.
- Dispatcher snapshots retain peer/flight/reply counts and dropped/rate-limited totals.
- CLI peer-state presentation and operational transition metrics remain part of activation work.

## Runtime Integration Contract

### Native Readiness

| Fact | Authoritative Owner |
| --- | --- |
| Incoming access | Current native reservation, owned live listener, usable underlay epoch; not cached capabilities |
| Payload work | Destination queue and application in-flight ownership |
| Protocol work | Native request/reply queues, active codecs, pending replies, and terminal notifications |
| Shared routing work | Primary/public-pairing DHT owners, AutoNAT probes, and pending native transports |
| Usable data path | Existing supported healthy path and actual packet-plane session |

- Borrow native owners; do not copy requests into another registry.
- Exclude only the lifecycle behaviour from idle-readiness work, so an idle exchange cannot block itself.
- Keep lifecycle work included in physical retirement fences until its transport events drain.
- A synchronous callback must recheck readiness at reply/ACK commitment, not reuse an earlier idle decision.
- Pending DHT/AutoNAT work also protects a dormant overlay transport from retirement.

### Automatic Driver

| Event | Action |
| --- | --- |
| Existing maintenance tick | Reconcile authorized records; offer idle only to connected supported peers after normal eligibility |
| Payload invalidates quiet | Retain one coalesced wake announcement; use the normal bounded path-recovery owner |
| Control path absent | Do not make the lifecycle behaviour dial or queue a disconnected request |
| Wake retry | Preserve the original application timestamp/version; obey the same bounded request slots and backoff |
| Stale demand | Stop announcements after demand expires unless actual application work remains |
| Dormant/legacy peer | No periodic idle/wake exchange merely to retain its state |

- Reuse the existing tick; add no timer, new configuration field, or maintenance-created application demand.
- [Drain maintenance](demand-lifecycle-drain.md) after normal idle eligibility; preparation never grants physical retirement.
- A valid payload or authenticated wake still overrides quiet immediately.
- Keep unsupported-protocol fallback sticky for the validated capability epoch.
- Native readiness and a working driver alone are insufficient for production advertisement.

### Control Delivery Gate

The [control-work contract](demand-lifecycle-control-work.md) requires bounded
event delivery to disconnected dormant members before quiet support is enabled.
Cached membership and DNS must remain authorized without retaining every socket.

## Remaining Activation Work

1. Validate implemented bounded cold control discovery through public-WAN and extended dormancy scenarios.
2. Preserve native ownership while validating the implemented quiet DHT/AutoNAT scheduling gates.
3. Complete broader offline-peer retry policy and prove first-payload recovery through the activated runner.
4. Verify extended dormancy, roaming, mixed-version operation, and feature-preservation gates before production advertisement.
5. Run matched, replicated cellular comparisons; no source or socket test establishes 10x savings.

Loopback tests use virtual idle time and fixture-provided readiness. They are
not evidence of real path retirement, long-idle public reachability, or the
10x battery target.

The runtime adapter additionally checks real native reservation ownership in
the incoming-anchor regressions. Those local socket tests reject retiring and
stale-epoch anchors; they do not replace the remaining integrated/device gates.

## Validation

| Check | Evidence |
| --- | --- |
| Focused lifecycle tests | 94 pass: 34 policy, 23 protocol, 10 activity, and 27 dispatcher/fixture tests |
| Idle commit interruption | Eight cases: demand, revocation, peer restart, underlay change, retirement, incoming-path loss, new work, and membership rotation |
| Queue ownership | Repeated cancellation, expiry, and revocation cannot recycle an outstanding transport slot |
| Admission | Unknown peers retain no state; table overflow is bounded; peer/network budgets and monotonic refill pass |
| Compatibility | Unsupported protocol and explicit unsupported reply remain Legacy across unchanged capability refreshes |
| Wire flood | Five incoming requests with eight available stream slots: four bounded rejections and one application-level drop |
| Maintenance drain | Preparation is not quiet; pending work drains normally; payload resumes maintenance immediately |
| Legacy races | Crossed idle offers clear quiet on fallback; delayed current-epoch unsupported results survive semantic expiry |
| Disconnected membership | Shared runner reconnects a retained TCP member, persists departure, drains ACKs, and preserves survivor governance |
| Dormant control delivery | 18 coalescing, deadline, quarantine, native-slot, LAN reconnect, and connection-ID regressions pass |
| Checkpoint-only publication | Seven new regressions pass, including real gated TCP callbacks/replies, finite attempts, revocation, and expiry |
| Checkpoint ownership | 58 tests pass, including finite hostname catch-up and cancelled/expired control leases |
| Workspace/all-targets | 2,095 passed; zero failures; 50 existing exclusions, including opt-in namespace/resource scenarios |
| Quiet discovery | Native serving/foreground requests survive pause; real relay ownership, demand/epoch resume, and six callback states pass |
| Cold control discovery | Seven driver regressions and three native namespace runs pass; no demand renewal or foreign-provider dials |
| Clippy | Required correctness, suspicious, and performance groups pass; advisory and staged-unused API warnings remain |
| Formatting and packaging | Workspace and vendored Rust formatting pass; evaluated Linux/Android sources include identical runtime and patched dependency |
| Unrun operational gates | Full Nix/Android package builds, NixOS VMs, live device/public-relay tests, long dormancy, and energy comparisons |

The flood fixture keeps the transport cap above its request count. Otherwise a
lower-level stream refusal could masquerade as evidence that the application
rate limiter worked.

Validation used cached Nix Cargo/Rust/Clippy 1.97.1, two build jobs, and the
existing approximately 7.0 GiB build directory. No SDK/toolchain download or
device change was needed. These manual checks are not a full-flake pass.

The first broad run of the control-delivery milestone exposed two reconnect
fixture failures. Fixtures now retain their native control owner and accurately
model handler/connection event ordering. Slot-retention guards remain intact.

The paging fixture also waits for real cancellation terminal events before
reusing slots. Focused reruns and the final full suite passed.

Commands:

```sh
cargo test --offline --locked --lib runtime::runner::demand_lifecycle
cargo test --offline --locked --workspace --all-targets -- --test-threads=2 --quiet
cargo clippy --offline --locked --workspace --all-targets -- \
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
```
