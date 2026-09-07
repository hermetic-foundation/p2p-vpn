# p2p-vpn Patch Record

## Provenance

| Field | Value |
| --- | --- |
| Crate | `libp2p-kad` |
| Upstream version | `0.48.0` |
| Registry archive | `libp2p-kad-0.48.0.crate` |
| SHA-256 | `13d3fd632a5872ec804d37e7413ceea20588f69d027a0fa3c46f82574f4dee60` |
| License | MIT; original notices retained in source files |
| Upstream revision | Retained in `.cargo_vcs_info.json` |

The archive checksum was verified against the original root Cargo lock before
import. All archive files are retained, including the upstream manifest, lock,
changelog, generated protocol source, and tests.

## Changes

| File | Patch |
| --- | --- |
| `src/behaviour.rs` | Bootstrap controls, routing/query admission, protected seeds, shared background-job scheduling, and provider/query cancellation. |
| `src/addresses.rs` | Bounded insertion/replacement, category-aware churn rotation, size rejection, and explicit seed protection. |
| `src/lib.rs` | Export optional address/query limits and query resource usage. |
| `src/query.rs` | Enforce deadlines and initial candidate admission; share retention accounting with iterative discovery; remove canceled queries. |
| `src/query/retained.rs` | Bound candidate identities and address bytes across learning, migration, failure, and result extraction. |
| `src/query/metadata.rs` | Bound query input and result metadata; normalize retained buffers; expose aggregate usage and typed admission errors. |
| `src/jobs.rs`, `src/jobs/bounded.rs` | Shared background admission, bounded key batches and skip bookkeeping, fresh record selection, and explicit removal from pending jobs. |
| `src/handler.rs`, `src/handler/pending.rs` | Opt-in request admission/expiry, bounded rejection reporting, and negotiation queue accounting. |
| `src/kbucket.rs`, `src/kbucket/bucket.rs` | Preserve protected seed peers during pending replacement; retain and recheck the probed victim identity. |
| `src/addresses/budget.rs` | Shared aggregate entry-generation and encoded-buffer reservations, including retained snapshots and deferred eviction storage. |
| `src/behaviour/queue.rs`, `src/behaviour/queue/payload.rs` | Bound unsent actions and intermediate results by count/bytes; charge routing notifications and release on dispatch/cancellation. |

The library configuration defaults are unchanged. p2p-vpn disables automatic and periodic bootstrap
explicitly so its scheduler owns bootstrap initiation. Explicit `bootstrap()` is
unchanged; DHT wire messages and protocol names are unchanged.

### Behaviour Queue

`set_behaviour_queue_limits()` enables aggregate admission for unsent actions
and intermediate results. Production uses 512 entries and 4 MiB per DHT.
`behaviour_queue_usage()` reports configured limits, current use, and rejections.

- Charge vector capacity, normalized keys, and encoded addresses; exclude allocator overhead.
- `RoutingUpdated` snapshots retain their separate aggregate routing reservations.
- Count addressless dials as fixed-size entries; discard excess reports/responses without closing connections.
- Rejected record/provider progress does not advance delivered-result bookkeeping.
- Return terminal results directly, after any already-admitted progress for that query.
- Check retirement before draining ordinary traffic; reject late progress after finish/expiry.
- Coalesce handler-mode changes outside the bounded queue and alternate their dispatch with normal work.

Queries now hand off one selected action before advancing another. Connected
outbound admission failure marks the peer failed instead of reporting dispatch
success. Cancellation still releases unsent work and preserves unrelated queries.

p2p-vpn opts into 64 retained routing addresses per peer and 2,048 encoded bytes
per address. Configured seeds count toward the same budget, survive churn, and
remain explicitly removable. Query caches have a separate opt-in budget of 256
candidate identities and 256 KiB of encoded addresses per phase, with the same
per-peer address limits. Rejected candidates never enter the query's iterator.

Admitted identities count until retirement, even after address failure. This
prevents repeated responses from replacing failed identities indefinitely.
Excess candidates are ignored, so heavily branching lookups can return fewer
results. Fixed-peer operations preserve their original quorum requirement.
`QueryRef::resource_usage()` exposes admission, encoded bytes, and rejection counts.

Whole-peer bucket eviction also preserves protected seeds. If all eligible
disconnected peers are protected, insertion fails at the existing bucket limit.
Pending replacement does not switch to another victim if the probed peer becomes
protected or reconnects; explicit removal still releases capacity.

`set_routing_limits()` enables aggregate routing reservations; p2p-vpn selects
512 retained entry generations and 2 MiB of encoded addresses per DHT. Snapshots
share existing buffer reservations but updates need another generation while
older snapshots live. This also bounds notification snapshot metadata growth.

Reservations retire with their last owner, including pending entries and deferred
eviction notifications. Address replacement credits only uniquely owned removed
buffers. Rejected admission preserves existing data; seed protection never
bypasses the limits. `routing_resource_usage()` exposes usage and rejection attempts.
Raw routing notifications share these limits and are dropped when admission fails;
`RoutingUpdated` snapshots already retain reservations for their data.

`set_query_pool_capacity()` adds opt-in retained-entry admission at the query pool.
`try_start_query()` checks capacity before invoking exactly one start operation.
`query_pool_usage()` and `query_is_retained()` include finished entries awaiting
retirement. Background jobs preserve pending work while capacity is unavailable.

Legacy starts rejected by this opt-in cap return an unretained ID without a
completion event; local result events and bootstrap suppression are not retained
for that ID. No rejection queue is created. The application constructor enables
32 retained queries per DHT and application starts use checked admission.
Queued result/action bounds remain unfinished; background storage is bounded below.

`set_query_metadata_limits()` bounds input key/value bytes, stored result peers,
and provider addresses. Production selects 256 KiB input, 256 result peers,
and 64 addresses of at most 2,048 encoded bytes per query. Bootstrap refresh
iterators retain at most 256 fixed-size targets, including consumed slots.

Together with the 32-entry pool cap, input and publication-address payloads
have a conservative 12 MiB aggregate ceiling per DHT. Query-peer caches,
pending RPCs, queued results, container overhead, and allocator RSS are separate.
`query_metadata_usage()` includes finished entries awaiting retirement.

Typed `try_get_*`, `try_put_*`, and `try_start_providing` methods distinguish
`QueryStartError::Capacity` from `InputTooLarge` before side effects. The generic
`try_start_query` remains capacity-only. Legacy oversized starts return an
unretained ID without a completion event; metadata admission defaults remain unlimited.

Stored success lists are bounded independently of quorum. Only distinct
accepted responses increment success statistics; duplicate/unsolicited responses
cannot satisfy quorum. Error reporting may contain a truncated success list,
but the original quorum requirement is preserved.

Pending connection RPCs now use shared reservations across the query pool.
`set_pending_rpc_limits()` is opt-in upstream; p2p-vpn sets 256 requests and
1 MiB per DHT. Failure, cancellation, retirement, and handler handoff release
reservations. `pending_rpc_usage()` exposes aggregate usage and rejections.

Provider requests awaiting connection no longer complete before handoff.
`AddProviderError::NoPeersReached` reports an all-failed publication phase;
it is a local API addition, not a wire change. Handler dispatch still does not
prove remote receipt. Queued result/action bounds remain unfinished.

Provider and record jobs share background admission capacity and alternate first
access. Defaults remain a 100-query ceiling and batch size ten, but the batch is
now shared across both jobs. p2p-vpn selects a ceiling of two and batch size one.
Foreground API calls count against admission but are not capped by this setting.

`set_background_job_limits()` replaces full snapshots with ordered key batches.
Production selects 64 keys / 1 MiB per job and a 256 KiB input ceiling. Each
job also retains at most two bounded cursor/boundary keys. The record job's
current/next-pass skip map shares a 64-key / 1 MiB budget.

Together these owners retain at most 4 MiB of key payload per DHT, excluding
containers, transient copies, record-store data, and separately admitted queries.
`background_job_usage()` reports aggregate keys, bytes, and rejection attempts;
unconfigured legacy snapshots are explicitly outside that report.

Byte-limited pages preserve the first excluded key so later short keys cannot
starve earlier large keys. Values and expiry are read at selection time; no
record-value or provider-address snapshots are retained. Oversized inputs remain
in the store but are not cloned into jobs or submitted as queries.

Full skip maps discard advisory hints, not records. Explicit local record
removal clears pending keys, skip bookkeeping, and legacy record snapshots.
Provider removal clears its pending batch too. Store scans borrow entries;
bounded batches do not imply constant-time scans or an RSS bound.

Expired queries stop issuing new requests, even if uncontacted candidates remain.
One timer per query pool wakes it for the earliest started query deadline;
idle connections and dropped overload reports no longer postpone retirement.
The timer is removed when the last query retires or is canceled.

Already queued or dispatched handler requests are not recalled by this check.
Multi-stage operations retain their existing per-phase timeout semantics;
this does not establish an aggregate operation-lifetime bound.

`stop_providing()` now retires matching provider queries and removes the key
from pending republication snapshots. Unlike upstream, it silently discards
matching queued results; callers must release their query IDs. p2p-vpn's pairing
cleanup already does so.

The additive `cancel_query()` API removes a query and its unsent behaviour
actions without advancing phases or emitting completion. Unsent dials needed by
another query remain queued. Already dispatched dials, handler requests, and
remote provider records are not recalled; remote records expire normally.

Canceling a retained bootstrap query also releases its active-bootstrap count.
Automatic bootstrap stays suppressed while another bootstrap remains active,
then resumes according to configuration. Repeated cancellation is a no-op.

`set_handler_queue_limits()` bounds pending requests by count and retained
payload bytes. Requests expire after the substream timeout. Rejection IDs are
bounded by the same count limit; excess reports defer to query deadlines.
Overload never requests closure of the shared connection.

p2p-vpn selects 64 waiting requests and 256 KiB per handler. The existing
32-stream limit also caps FIFO negotiation entries, including entries whose
stream tasks timed out before the swarm returned their upgrade callbacks.
`pending_request_usage()` exposes queue, rejection, expiry, and negotiation counts.

## Build Integration

- The root `[patch.crates-io]` selects this source for the entire Cargo dependency graph.
- Desktop and Android Nix source filesets include this directory.
- The vendored package is excluded from root workspace membership, not from compilation.
- Application regressions exercise both shared-public and separate-public-pairing DHTs.

## Maintenance

1. Verify a replacement archive against its registry checksum before import.
2. Compare all local changes with the pristine archive; do not update generated protocol files casually.
3. Reapply only required fixes and update this record.
4. Run discovery, pairing, recovery, source-parity, and native-target checks.

Aggregate routing storage, active-query limits, and sustained measurements remain
tracked in `docs/developer/kademlia-resource-plan.md` at the repo root.
