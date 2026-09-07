# Kademlia Aggregate Bounds

## Status

Active phase; aggregate enforcement is not complete.
Starting revision: `77146fe3`. Existing per-peer, per-query, scheduler, and
cancellation fixes remain in place.

## Acceptance Gates

| Owner | Required Bound | Required Verification |
| --- | --- | --- |
| Connection handler | Pending request count, retained payload bytes, rejection bookkeeping, retirement | Saturate count and bytes; release capacity; preserve unrelated traffic |
| Routing table | Aggregate present and pending entries and retained address storage | Churn at capacity; protect configured seeds within the budget; retain fresh alternatives |
| Query pool | All retained query state, including finished entries and every producer | Foreground/background saturation, cancellation, phase transitions, resumed admission |

Bounds must hold at the owning layer. Running-query iterators exclude finished
entries, and application routing-peer counters do not cover internal Kademlia
storage. Neither is sufficient evidence of an aggregate bound.

## Handler Audit

| Source | Observation |
| --- | --- |
| Original `handler.rs` | `pending_messages` was an unbounded `VecDeque` of request payloads and query IDs |
| `Handler::on_behaviour_event` | Five outbound request variants now use the bounded pending-request owner |
| `Handler::poll` | A pending message advances only when fewer than 32 outbound streams are active |
| `pending_streams` | FIFO negotiation entries now count against the 32-stream admission limit, even after task timeout |
| `Behaviour::on_connection_handler_event` | `QueryError` fails that peer's attempt for the matching query, without closing the connection |

The original 32-stream limit did not bound queued requests or their payloads. Query
cancellation removes unsent behaviour actions but cannot recall requests already
delivered to the handler. A stalled handler can therefore outlive query owners.

### Implemented Admission

| Limit | Value | Rationale |
| --- | ---: | --- |
| Waiting requests | 64 per handler | Two batches of the existing 32 active streams |
| Retained waiting payload | 256 KiB | Bounded allowance for keys, records, and provider addresses |
| Rejection IDs | 64 per handler | Reporting cannot become a second unbounded queue |
| Pending negotiations | 32 per handler | Preserve FIFO callback association without accumulating expired-task entries |
| Queue residence | Ten seconds by default | Reuses the existing configurable substream timeout |

Admission rejects new requests, preserving admitted payloads. Tracked rejection
IDs emit `QueryError` with `WouldBlock`; additional IDs are discarded and their
queries retire through existing deadlines. No overload action closes a connection.

Expiry checks the actual deadline before polling the timer. It releases payload
accounting and reports `TimedOut`. Negotiation callbacks release their FIFO
entries; canceled receiver entries cannot be removed early without misassociation.

`HandlerQueueUsage` exposes pending requests, payload bytes, rejection counts,
expiry, negotiations, and active outbound streams. Payload accounting includes
vector capacities where available, key/address lengths, and address-vector slots;
it is not allocator-level RSS accounting or an aggregate process-memory bound.

The shared controlled constructor enables limits for desktop, Android, and
standalone pairing. Library request limits remain opt-in; negotiation entries
always share the existing stream ceiling. Wire messages and minimal configs
are unchanged.

### Regression Coverage

Normal p2p-vpn tests construct the real libp2p handler through its behaviour API.
They cover count/byte saturation, oversized requests, bounded rejection reports,
query-deadline fallback, cancellation with handler-owned work, stalled
negotiations, expiry, and resumed admission.

Root Cargo cannot run vendored crate unit tests because the crate is excluded
from workspace membership and needs dev-dependencies. This limitation is not
reported as an upstream test-suite pass. Log:
`/tmp/p2p-vpn-kad-handler-test-build.log`.

### Handler Checkpoint Evidence

| Check | Result |
| --- | --- |
| Offline workspace tests | 1,274 passed; 22 opt-in tests ignored |
| Namespace DHT discovery | Passed |
| Namespace peerless code and forced-relay pairing | Both passed |
| Namespace owned QUIC packet plane | Passed |
| Namespace relay/direct network move | Passed |
| Android x86_64 native library | Compiled offline with cached NDK; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tool overrides |
| Root and changed vendored Rust formatting | Passed |
| Workspace Clippy correctness, suspicious, and perf groups | Passed; existing style warnings remain |

Logs use `/tmp/p2p-vpn-kad-handler-` with suffixes
`workspace-verified.log`, `dht.log`, `code.log`, `relay.log`, `quic.log`,
`move.log`, `android.log`, `nix-verified.log`, and `clippy-verified.log`.

The namespace checks verify normal VPN operation, not overload while sharing
a connection. That combined saturation test remains required. Full Nix package,
APK, ARM64, and physical-device builds/deployments were not performed.

### Failure Modes To Test

| Scenario | Required Outcome |
| --- | --- |
| One oversized request | Reject without retaining its payload |
| Many small requests | Stop at the count ceiling |
| Fewer large requests | Stop at the byte ceiling |
| Rejection flood without polling | Error bookkeeping stays bounded |
| Stream negotiation stalls | Pending requests expire; accounting is released |
| Capacity released | Later requests are admitted |
| Query canceled while handler retains work | No permanent retained request or accounting leak |
| Connection carries VPN streams | Kademlia overload does not reset unrelated application traffic |

## Implementation Order

1. Handler admission, expiry, and bounded rejection reporting.
2. Aggregate routing-entry and address-storage admission.
3. Aggregate query admission across all producers and phases.
4. Combined saturation and recovery regressions; final phase-1 evidence audit.

Use focused checks during iteration. Run broader validation for each coherent
code change before publishing; documentation-only changes do not require
rebuilding unchanged binaries.

## Validation And Limits

- Reuse cached tools; at most two Cargo jobs and 10 GiB task temporary storage.
- Cap build-related downloads at 10 Mbps; do not deploy physical devices or personal flakes.
- Verify workspace tests, relevant namespace checks, formatting, Clippy, Nix source integration, and affected Android native compilation.
- Record failed reproductions separately from passing validation.
- Do not treat deterministic tests as sustained performance or physical-WAN evidence.

Long-running settling tests, comparable before/after process measurements, and
final acceptance remain separate phases in the
[workstream plan](kademlia-resource-plan.md#remaining-phases).
