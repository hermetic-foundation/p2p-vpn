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
| `src/jobs.rs` | Document the shared background batch default; remove stopped providers from pending snapshots. |
| `src/handler.rs`, `src/handler/pending.rs` | Opt-in request admission/expiry, bounded rejection reporting, and negotiation queue accounting. |

The library configuration defaults are unchanged. p2p-vpn disables automatic and periodic bootstrap
explicitly so its scheduler owns bootstrap initiation. Explicit `bootstrap()` is
unchanged; DHT wire messages and protocol names are unchanged.

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

Provider and record jobs share background admission capacity and alternate first
access. Defaults remain a 100-query ceiling and batch size ten, but the batch is
now shared across both jobs. p2p-vpn selects a ceiling of two and batch size one.
Foreground API calls count against admission but are not capped by this setting.

Expired queries stop issuing new requests when the pool next examines them,
even if uncontacted candidates remain. Already queued or dispatched requests are
not canceled by this check. Multi-stage operations retain their existing timeout
semantics; this does not establish an aggregate operation-lifetime bound.

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
