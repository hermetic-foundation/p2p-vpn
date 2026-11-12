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

## Remaining Activation Gates

- Own and bound a live incoming anchor; keep essential renewal and automatic anchor recovery intact.
- Deliver membership, checkpoints, revocations, and hostname changes to disconnected dormant members on events.
- Do not rely only on connected-peer broadcasts, and do not turn control updates into permanent activity leases.
- Complete demand-driven LAN-first establishment/recovery and sparse infrastructure maintenance policy.
- Verify first incoming/outgoing VPN payloads, legacy peers, active transfers, and roaming with lifecycle support enabled.
- Measure fresh paired/reversed-order cellular energy only after the functional candidate is active.

## Verification Record

| Check | Result |
| --- | --- |
| Workspace/all-target tests | 1,948 passed; zero failures; 49 existing/opt-in ignores |
| Native work regressions | Five passed: queued failure, concurrent replies, omitted reply, timeout, undecoded codec I/O |
| Stream retirement regressions | Five passed: work/demand, initial grace, nine infrastructure roles, metadata retention, real TCP close/reconnect |
| Pinned stream regressions | Existing budget observed; original capacity retained across builder-limit changes; denied creation releases metadata |
| Clippy | Correctness, suspicious, and performance groups passed; other advisory warnings remain |
| Rust formatting | Changed repository Rust files passed; vendored source preserves upstream formatting |
| Linux package source | Current runtime and patched dependency match evaluated source byte-for-byte; test inventory matches |
| Android package sources | ARM64/x86-64 evaluate and include identical current runtime and patched dependency |
| Full Nix build/check | Not run: offline dry run requires 493 uncached derivations, including bootstrap toolchain |
| Nix formatting | Android source module passes; root flake already fails formatter checks on the unchanged baseline |
| Phone/APK/service deployment | None performed in this milestone |
| Energy acceptance | Unmeasured; no 10x savings claim |

Validation used the existing shared target directory and cached Nix Rust 1.97.1
tools. One final-suite attempt was interrupted mid-run with no remaining process;
its log was preserved and a subsequent full run completed successfully.

No new formal-verification project was introduced. These executable tests do
not prove the entire distributed lifecycle or replace the remaining device tests.
