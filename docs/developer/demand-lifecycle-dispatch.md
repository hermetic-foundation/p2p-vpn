# Lifecycle Dispatcher

## Deployment Boundary

| Surface | Current State |
| --- | --- |
| Handler | Registered in the shared libp2p host; dispatched by the Linux/Android runner |
| Quiet capability | Still false and omitted from normal capability messages |
| Readiness | Production adapter conservatively refuses quiet admission |
| Maintenance | Existing probes, retention, discovery, and keepalives remain unchanged |
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

Libp2p does not expose application-request cancellation here. Clearing a map
entry cannot cancel its pending dial/stream queue. Keeping bounded transport
tombstones prevents repeated cancellation from bypassing the instance limit.

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

## Diagnostics

- Startup emits `event=peer_lifecycle_policy mode=passive` with the pending-integration reason.
- Dispatcher snapshots retain peer/flight/reply counts and dropped/rate-limited totals.
- CLI peer-state presentation and operational transition metrics remain part of activation work.

## Remaining Activation Work

1. Finish real queue/in-flight/service readiness; [payload demand tracking](demand-lifecycle-activity.md) is implemented.
2. Finish physical retirement and remaining connection/discovery/NAT policy; probe, hello and retention gates respect committed quiet permission.
3. Supply actual incoming-path and outstanding-work readiness instead of the conservative adapter.
4. Prove a quiet public relay reservation and first inbound access before advertising quiet support.
5. Run extended dormancy, roaming, compatibility, feature, and matched cellular energy gates.

Loopback tests use virtual idle time and fixture-provided readiness. They are
not evidence of real path retirement, long-idle public reachability, or the
10x battery target.

## Validation

| Check | Evidence |
| --- | --- |
| Focused dispatcher tests | 12 pass, including real authenticated TCP/QUIC idle and reverse wake |
| Idle commit interruption | Eight cases: demand, revocation, peer restart, underlay change, retirement, incoming-path loss, new work, and membership rotation |
| Queue ownership | Repeated cancellation, expiry, and revocation cannot recycle an outstanding transport slot |
| Admission | Unknown peers retain no state; table overflow is bounded; peer/network budgets and monotonic refill pass |
| Compatibility | Unsupported protocol and explicit unsupported reply remain Legacy across unchanged capability refreshes |
| Wire flood | Five incoming requests with eight available stream slots: four bounded rejections and one application-level drop |
| Workspace/all-targets | 1,910 passed; 48 existing exclusions, including opt-in namespace/resource scenarios |
| Clippy | Required correctness, suspicious, and performance groups pass; advisory and staged-unused API warnings remain |
| Formatting and packaging | Changed Rust files pass rustfmt; evaluated Nix package source includes identical dispatcher sources/tests |
| Unrun operational gates | Full Nix/Android package builds, NixOS VMs, live device/public-relay tests, long dormancy, and energy comparisons |

The flood fixture keeps the transport cap above its request count. Otherwise a
lower-level stream refusal could masquerade as evidence that the application
rate limiter worked.

Validation used cached Nix Cargo/Rust/Clippy 1.97.1, two build jobs, and the
existing approximately 4 GiB build directory. No SDK/toolchain download or
device change was needed. These manual checks are not a full-flake pass.

Commands:

```sh
cargo test --offline --locked --lib runtime::runner::demand_lifecycle
cargo test --offline --locked --workspace --all-targets -- --test-threads=2 --quiet
cargo clippy --offline --locked --workspace --all-targets -- \
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
```
