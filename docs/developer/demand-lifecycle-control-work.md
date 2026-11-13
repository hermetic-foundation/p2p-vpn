# Dormant Control Work And Stream Retirement

## Status

Implemented behind committed quiet permission; production lifecycle support
remains unadvertised. This is not a deployed battery-efficiency result.

## Contract

Control maintenance is not payload demand, but unfinished control work must
finish or fail through its normal protocol owner before a connection retires.
Do not invent a second request ledger or treat semantic cancellation as completion.

| Work | Retirement Fence |
| --- | --- |
| Queued outbound request | Busy before the behaviour is polled or a connection exists |
| Transmitted request | Busy until response/failure ownership drains |
| Inbound request/reply channel | Busy while a reply is being prepared, queued, or written |
| Active codec I/O | Busy before an inbound request has finished decoding |
| Queued terminal notification | Busy until the behaviour delivers the completion/failure event |
| Lifecycle exchange | Same wire fence; never let the idle exchange fence its own readiness |
| Queued/in-flight IP payload | Existing application-work fence; maintenance does not renew demand |

## Dependency Boundary

- Pin the existing `libp2p-request-response` 0.29.0 source through a local patch.
- Add `Behaviour::has_pending_requests(peer)`, a read-only aggregate query.
- Include pending requests, response channels, and queued peer event delivery.
- Include queued connection selection across destinations before native pending-dial counters increase.
- Connected requests for unrelated peers do not globally pin every dormant connection.
- Share one native worker-count gauge per connection, so undecoded inbound I/O is visible without a request ledger.
- Preserve all current behaviour types, request IDs, codecs, stream limits, and wire protocols.
- Query existing ownership in place; allocate no additional per-request bookkeeping.
- Include the patched source in Linux, Android, and source-parity Nix checks.
- Pinned packet streams share their existing semaphore budget with the behaviour for the same I/O fence.

## Retirement Preconditions

1. The current authorized peer has committed quiet permission.
2. No queued/in-flight payload or request-response work remains for that peer.
3. The connection belongs to the current underlay epoch and is not already retiring.
4. A fresh connection has had a bounded initial control-handshake grace period.
5. The connection is not needed for an incoming anchor, reservation setup, or another relay circuit.

No await occurs between the final check and requesting connection closure.
Late application demand clears quiet permission and may establish a fresh path.
Initial control grace is bounded transport work, not a renewable activity lease.
Unobserved requests can still race a close; existing bounded reconnect/retry
owners must handle this ordinary transport failure without losing membership state.

## Infrastructure Exceptions

| Role | Treatment |
| --- | --- |
| Explicit bootstrap/relay peer | Preserve its infrastructure connection |
| Automatic relay infrastructure | Preserve until its network-level owner releases it |
| Pending/accepted reservation or relay listener | Preserve the incoming path and lease work |
| Underlay carrying another circuit | Preserve the relay dependency; do not confuse inner and outer connections |
| Local relay-server mode | Do not retire client connections without server-side reservation/circuit accounting |

These exceptions are safety fences, not the final idle footprint policy.
The incoming-anchor owner must separately bound and release unused infrastructure.

## Dormant Metadata

- Intentional disconnect does not revoke membership, routes, DNS, or cached addresses.
- Retain the last validated capabilities during authorized dormancy, including initial reconnection.
- Cached capabilities are historical metadata, not proof of a healthy connection or live packet session.
- Current secure identity, authorization, network/tag, epochs, and frame checks still apply.
- Fresh capability exchange remains mandatory protocol work and can replace stale epoch/version information.
- Authorization changes or actual demanded path failure keep the existing invalidation behavior.

## Validation

| Layer | Required Cases |
| --- | --- |
| Aggregate query | Disconnected queues, concurrent requests, reply delivery, omission, timeout, dial failure, unrelated peers |
| Retirement | Demand/work fences, initial grace, stale/retiring IDs, repeated no-op, protected relay roles |
| Compatibility | Public behaviour types unchanged; legacy/non-dormant invalidation unchanged |
| Resource | No new request ledger; no dial/poll side effects from readiness inspection |
| Integration | Real encrypted TCP requests and replies; normal recovery/authorization regressions |

Production lifecycle advertisement stays disabled until real incoming-anchor
readiness and end-to-end wake handling are active and verified. No battery
savings are claimed from ownership tests alone.

## Event-Driven Delivery Contract

### Capability Changes

- Coalesce local membership, hostname, and incoming-endpoint changes per authorized recipient.
- Extend the existing bounded membership-sync owner; keep one latest dirty generation instead of a request ledger.
- Track native capability requests through exact peer/request completion, with deadlines, concurrency limits, and failure backoff.
- Use existing retained addresses and bounded LAN-first recovery; do not enqueue unlimited disconnected control requests.
- Recheck authority before dialing or sending. Control delivery must not renew application demand or clear quiet permission.

### Checkpoint Mutations

| Boundary | Contract |
| --- | --- |
| Recipients | The pre-mutation active roster except self; connection presence is not membership |
| Removed recipient | Retain only existing control-delivery eligibility; never restore packet authorization |
| Delivery | Existing native mutation owner, deadline-bearing request codec, operation window, three attempts, and 32-flight bound |
| Acknowledgement | Preserve exact-base proof checks, scope/peer validation, and persist-before-ACK |
| Final departure | Survivors can acknowledge and continue governance even when initially disconnected |
| Offline member | Bounded failure remains explicit; no guarantee of delivery to an arbitrarily unavailable device |

### Catch-Up And Hostnames

- Trigger bounded checkpoint catch-up on actual control changes, including a hostname claim at unchanged authority rank.
- Deduplicate completed/no-op notifications; identical descriptors must not trigger endless fetch/broadcast rounds.
- Preserve paged membership pulls and their existing byte, restart, concurrency, and retry limits.
- Test disconnected delivery, authority loss during retry, unchanged application demand, and subsequent stream retirement.

## Implemented Checkpoint Delivery

- Mutation recipients come from the active roster before applying the change, including disconnected members.
- Native request IDs retain their slots after semantic expiry/cancellation; tombstones carry no mutation authority.
- Old terminal events cannot acknowledge or fail a newer handoff for the same peer.
- Removed recipients retain bounded control-only eligibility while waiting for a native slot.
- Shared-root regression starts with A-B-C only, reconnects retained A-C for departure, then verifies survivor governance.

## Remaining Activation Gates

- Keep verified incoming ownership, essential renewal, and automatic anchor recovery intact in the activated runtime.
- Finish dirty capability delivery and equal-authority hostname catch-up for disconnected dormant members.
- Add a bounded control-owner exception for old probe quarantine without erasing failure history or granting packets.
- Do not rely only on connected-peer broadcasts, and do not turn control updates into permanent activity leases.
- Complete demand-driven LAN-first establishment/recovery and sparse infrastructure maintenance policy.
- Verify first incoming/outgoing VPN payloads, legacy peers, active transfers, and roaming with lifecycle support enabled.
- Measure fresh paired/reversed-order cellular energy only after the functional candidate is active.

## Verification Record

| Check | Result |
| --- | --- |
| Workspace/all-target tests | 2,016 passed; zero failures; 49 existing/opt-in ignores |
| Native work regressions | Five passed: queued failure, concurrent replies, omitted reply, timeout, undecoded codec I/O |
| Stream retirement regressions | Six passed: work/demand, initial grace, pending transports, nine infrastructure roles, metadata retention, real TCP close/reconnect |
| Datagram retirement regressions | Five passed; local preparation alone cannot retire paths or reject incoming hello processing |
| Checkpoint native ownership | 41 focused tests passed, including four new disconnected-roster, slot-expiry, stale-event, and three-owner regressions |
| Shared-root delivery | A-B-C startup with retained A-C only; departure ACKs reach both survivors, which continue governance |
| Pinned stream regressions | Existing budget observed; original capacity retained across builder-limit changes; denied creation releases metadata |
| Clippy | Correctness, suspicious, and performance groups passed; other advisory warnings remain |
| Rust formatting | Changed repository Rust files passed; vendored source preserves upstream formatting |
| Linux package source | Current runtime and patched dependency match evaluated source byte-for-byte; test inventory matches |
| Android package sources | ARM64/x86-64 evaluate and include identical current runtime and patched dependency |
| Full Nix build/check | Not run: earlier offline dry run required 493 uncached derivations, including bootstrap toolchain |
| Nix formatting | Android source module passes; root flake already fails formatter checks on the unchanged baseline |
| Phone/APK/service deployment | None performed in this milestone |
| Energy acceptance | Unmeasured; no 10x savings claim |

Validation used the existing shared target directory and cached Nix Rust 1.97.1
tools, two build jobs, and disabled incremental compilation. The shared target
remained approximately 6.3 GiB; no new SDK, toolchain, or target was downloaded.

An initial broad integration run exposed five test-fixture failures. Synthetic
connection fixtures now isolate public DHT seed work, and storage-only churn
explicitly expires its bounded offline handoff. Native guards remain unchanged;
focused reruns and the final full workspace run passed.

No new formal-verification project was introduced. These executable tests do
not prove the entire distributed lifecycle or replace the remaining device tests.
