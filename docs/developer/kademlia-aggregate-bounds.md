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
| `vendor/libp2p-kad-0.48.0/src/handler.rs` | `pending_messages` is an unbounded `VecDeque` of request payloads and query IDs |
| `Handler::on_behaviour_event` | Five outbound request variants append directly to that queue |
| `Handler::poll` | A pending message advances only when fewer than 32 outbound streams are active |
| `pending_streams` | Tracks requested stream negotiations separately; retirement needs its own audit |
| `Behaviour::on_connection_handler_event` | `QueryError` fails that peer's attempt for the matching query, without closing the connection |

The 32-stream limit does not bound queued requests or their payloads. Query
cancellation removes unsent behaviour actions but cannot recall requests already
delivered to the handler. A stalled handler can therefore outlive query owners.

### Admission Design

Proposed, not implemented:

1. Apply count and retained-payload-byte limits before enqueueing requests.
2. Reject new requests at capacity without evicting admitted work.
3. Bound rejection bookkeeping independently of request storage.
4. Report tracked rejections through query errors; never close the shared connection solely for queue overload.
5. Let existing query deadlines retire rejections beyond the error-reporting budget; test this fallback explicitly.
6. Expire pending requests and release their accounting even when stream negotiation stalls.

Configure the limits through the shared controlled Kademlia constructor so
desktop, Android, and standalone pairing use the same policy. Preserve existing
wire messages and minimal JSON/Nix configuration.

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
