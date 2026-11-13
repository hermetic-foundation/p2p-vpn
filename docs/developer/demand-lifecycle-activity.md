# Application Demand And Quiet Fencing

## Recording Contract

| Event | Demand Record |
| --- | --- |
| New authorized TUN packet | Destination peer, after source/destination/size validation |
| Queue backpressure after valid admission | Still application demand; not a promise of delivery |
| Accepted inbound IP frame | Authenticated origin peer, after source/destination/size and applicable replay checks |
| Explicit application request or accepted wake | Authorized target/origin peer |
| Queued retransmission | No new activity |
| Probe, keepalive, ACK, capability, membership or DNS-cache maintenance | No new activity |
| Automatic service-status request on connection establishment | No new activity; bounded control work |
| Malformed, spoofed, unauthorized or replay-rejected packet | No new activity |

Record at the shared forwarding boundary so all packet backends and Android
use the same rule. Preserve the immutable inbound stream validation API;
recording must not require a new global lock or per-packet heap allocation.

## Per-Peer State

| Field | Purpose |
| --- | --- |
| Monotonic timestamp | Last observed application demand; never wall-clock time |
| Generation | Advances on each accepted application event, even at the same clock tick |
| Admission identifier | Changes when a record is removed/re-added; prevents zero-generation ticket reuse |
| Quiet bit | Permission committed by validated bilateral lifecycle negotiation |
| Ownership | Exactly one record per currently authorized forwarding peer |

The quiet bit and generation share an atomic word. An application event clears
quiet and advances the generation. A quiet grant uses compare-and-swap against
the generation that the dispatcher checked; stale permission cannot win later.

Generation exhaustion fails open for traffic and closed for quiescence: keep
forwarding, but never grant quiet using a reused or wrapped generation.

## Dispatcher And Forwarder

1. Observe actual forwarding activity before evaluating an idle request or acknowledgement.
2. Invalidate semantic idle tickets on new application generations; retain transport slots until terminal events.
3. Commit model permission only after normal authentication, scope, readiness and delivery checks.
4. Grant the forwarding quiet bit only if its observed generation still matches.
5. If activity raced the grant, remain active and preserve the newer demand.

A packet clears quiet immediately at the forwarding boundary. It need not wait
for a five-second maintenance tick or status poll. Control refreshes do not
invent a new application timestamp.

## Authorization Changes

- Preserve demand timestamps for unchanged authorized peers across harmless refreshes.
- Prune removed peers; a removed and re-added peer does not inherit an old quiet grant.
- Invalidate quiet grants on changed forwarding authority or membership incarnations.
- Keep staged/failed authorization transactions from changing live demand state.
- Each instance owns independent records; no global cross-network activity lease.

## Activation Boundary

Use the forwarding quiet bit to suppress peer probes, negotiation retries,
retention and background data-path recovery. Do not report dormant as a healthy
transport; it is a separate policy state.

These gates are not permission to advertise quiet support yet. Incoming-path
readiness, transport retirement, public-relay evidence, feature checks and
matched cellular energy measurements remain required activation gates.

## Implemented Boundaries

| Component | Current Behavior |
| --- | --- |
| Forwarder | New TUN admission and accepted IPv4/IPv6 frames record demand; queued retries and maintenance do not |
| Dispatcher | Reads forwarding generations before idle decisions; both idle commits grant a fenced quiet bit |
| Wake | Accepted authenticated wake and explicit application demand clear quiet immediately |
| Probes | Committed quiet skips outgoing probes and discards expired probe recovery work |
| Packet-plane negotiation | Quiet blocks periodic retry and hello initiation |
| Retention/recovery | Quiet releases selected control retention and skips background peer redial; configured infrastructure roles remain eligible |
| Discovery | Dormant is sufficient for peer-maintenance suppression, but is not reported as a healthy transport |
| Production | Still passive: capability advertisement, real readiness and physical retirement remain pending |

Automatic service-status requests are also sent by the runtime, not only by
human diagnostics. Treating every accepted status request as application
activity would recreate a maintenance-driven lease.

## Validation

| Check | Result |
| --- | --- |
| `cargo test --lib demand` | 22 passed: forwarding admission, retries, backpressure, authorization, dispatcher races and retention |
| Lifecycle/codec/activity scope | 63 passed, including the existing exhaustive model traces |
| Probe gate | Validated outbound IP admission clears the gate; suppressed probe attempts resume |
| Primitive concurrency | An old grant racing 10,000 same-timestamp application records cannot survive accepted data |
| Record lifetime | Removed/re-added unused peers cannot reuse zero-generation tickets; counter exhaustion never authorizes quiet |
| Workspace/all-targets | 1,925 passed; 48 existing exclusions |
| Clippy/formatting | Required correctness, suspicious and performance groups pass; changed Rust files pass rustfmt |
| Packaging | Evaluated Nix source includes identical activity and forwarding-test sources |

Checks used cached Nix Cargo/Rust/Clippy 1.97.1, two build jobs and the existing
build directory. Full Nix/Android builds, VMs, public-relay/device dormancy and
energy comparisons were not run for this still-unadvertised stage.

The concurrency check is executable coverage, not a formal memory-model proof.
No new wakeup task, network timer or required configuration field was added.

## Legacy Membership Tracking

The C2 device preflight reached idle eligibility without entering maintenance
pause. The legacy record-merge path updates forwarding authorization directly,
but did not reconcile its application-demand records.

| Required Correction | Invariant |
| --- | --- |
| Restored or newly paired member | Admit a demand record for every newly authorized packet peer |
| Removed member | Remove its record so stale versions cannot grant quiet after re-admission |
| Effective authorization changes | Revoke quiet permission consistently with transactional reconfiguration |
| Duplicate/metadata-only record refresh | Preserve demand versions and quiet permission when forwarding authority is unchanged |
| Bounds | Reuse the authorized-peer table and existing demand owner; no independent discovery timer or peer registry |

Cover persisted restore and live merge, real payload-demand recording,
duplicate refresh, revocation, and re-admission before another device candidate.
The failed [C2 preflight](android-lifecycle-reconnect-build-2026-11-13.json)
is not an energy measurement or a feature-preservation success.

The merge path now reconciles demand records on effective authorization changes.
New members receive tracking; removed records are discarded; changed authority
revokes quiet, while duplicate/metadata-only refreshes preserve activity state.

| Verification | Result |
| --- | --- |
| Regression before correction | Fails because a learned, authorized peer has no demand record |
| Forwarding/membership scope after correction | 79 passed; zero failed; one existing opt-in exclusion |
| Demand/dispatcher scope | 55 pass, including signed live merge and persisted restore with no explicit peers |
| Payload | Real TUN admission records demand and clears quiet/preparation for the newly learned peer |
| Revocation/re-admission | Removed versions cannot grant quiet after a new membership-epoch admission |
| Compatibility | Same-epoch sequence increments do not undo revocation; the test uses a new epoch rather than changing that policy |
| Device | The cached C2 APK predates this correction; rebuild both endpoints before repeating preflight |

The full 2,124-test workspace result belongs to the earlier reconnect candidate.
This subsequent correction has focused coverage; its full feature-enabled
workspace, Android build and operational/energy gates still need repeating.
