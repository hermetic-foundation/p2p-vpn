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

## Routing Owner Audit

| Owner / Mutation | Implication For Aggregate Enforcement |
| --- | --- |
| `KBucketsTable::buckets` | Count all present entries, not only application-selected routing peers |
| `KBucket::pending` | One additional retained candidate per bucket; include its addresses before promotion |
| `KBucketsTable::applied_pending` | Retains inserted snapshots and evicted nodes until consumed; promotion alone does not retire all storage |
| `entry`, `iter`, `bucket`, and closest iterators | Can apply pending replacements during otherwise read-like operations |
| `Behaviour::add_address` | Updates present/pending entries or inserts a new entry |
| `Behaviour::connection_updated` | Independently learns addresses and inserts connected peers |
| `Behaviour::on_address_change` | Replaces an address, or adds the new endpoint while retaining a protected seed |
| `remove_address`, `remove_peer`, and failed-address handling | Must release accounting without losing the last usable recovery address accidentally |

### Protected Seed Enforcement

The initial regression reproduced whole-peer seed eviction during pending
replacement. Address protection previously applied only to address rotation.

Bucket selection now skips protected values. A pending candidate retains the
selected victim's key; promotion rechecks that the same victim is disconnected
and unprotected. It does not silently choose another, unprobed peer.

An all-protected bucket rejects new candidates at the existing capacity.
Explicit removal remains available. These changes protect seeds within bucket
capacity; they do not establish aggregate routing count or byte limits.

The real-behaviour regression covers five seeded scenarios: one protected peer,
an all-protected bucket, late protection with and without another eligible peer,
and ordinary unprotected replacement. It checks the probe target, retained peers,
bucket capacity, and admission after explicit removal.

| Seed Checkpoint | Result |
| --- | --- |
| Before-fix reproduction | Failed: protected seed evicted |
| Expanded regression and workspace | 1,275 passed; 22 opt-in tests ignored |
| DHT, forced-relay pairing, peerless code namespaces | Passed |
| Owned QUIC and relay/direct network-move namespaces | Both passed |
| Android x86_64 native library | Compiled offline; four existing warnings |
| Nix source integration | Passed with cached tool overrides |
| Formatting and Clippy correctness/suspicious/perf | Passed; existing style warnings remain |

Logs use `/tmp/p2p-vpn-kad-seed-` with suffixes `before.log`, `workspace.log`,
`dht.log`, `relay.log`, `code.log`, `quic.log`, `move.log`, `android.log`,
`nix.log`, and `clippy.log`.
No new formal model, full Nix package, APK, ARM64, or device validation is claimed.

### Required Routing Regressions

1. Fill present and pending entries together; assert entry and encoded-address budgets.
2. Promote pending entries through lookup and iteration; account for deferred eviction notifications.
3. Update a peer at the global byte ceiling; retain fresh LAN/public/relay alternatives within bounds.
4. Preserve a disconnected protected seed during pending replacement: verified at bucket capacity; aggregate budget coverage remains open.
5. Remove entries and drain deferred notifications; prove capacity becomes available again.
6. Exercise explicit address addition, connection discovery, and address migration through the same admission policy.

### Aggregate Routing Admission

| Limit | Value Per DHT | Rationale |
| --- | ---: | --- |
| Retained entry generations | 512 | Room for normal populated buckets plus pending work and short-lived snapshots |
| Encoded address buffers | 2 MiB | Aggregate storage bound independent of per-peer address limits |
| Per-peer addresses / per-address bytes | 64 / 2,048 | Existing limits also bound metadata and each admission attempt |

Main discovery, separate public pairing, and standalone pairing use these defaults.
No JSON or Nix configuration additions are required. The vendored library's
`set_routing_limits` remains opt-in; wire formats are unchanged.

| Transition | Accounting |
| --- | --- |
| Create present or pending entry | Reserve an entry generation and its first address before insertion |
| Clone a routing snapshot | Share reservations; existing buffers remain charged once |
| Add data while old snapshots live | Reserve another entry generation, bounding snapshot metadata as well as buffers |
| Apply pending replacement | Keep the evicted entry charged until deferred notifications release it |
| Remove an address or peer | Release each reservation after its last owner drops |
| Queue a raw routing notification | Reserve from the same budget until dispatch or removal; excess reports are dropped |
| Migrate / rotate an address | Credit only uniquely owned removed buffers; snapshots cannot be credited prematurely |
| Admission fails | Preserve previously admitted entries and alternatives; do not close connections |

At the byte ceiling, replacement prefers unprotected addresses in the incoming
category, then duplicated categories. Protected seeds never fund replacement.
If no valid replacement fits, the update fails without discarding existing data.

Snapshots may delay admission after a peer disappears from the visible table.
Draining notifications releases capacity automatically. Holding snapshots in a
consumer intentionally keeps their reservations alive.

`Behaviour::routing_resource_usage()` reports retained generations, encoded bytes,
and rejection-attempt counters. It includes pending and deferred routing storage;
it is not a visible-peer count or RSS estimate. Raw addresses transferred into
query/transport owners are governed by those owners' separate limits.

The behaviour queue charges raw `RoutablePeer`, `PendingRoutablePeer`,
`UnroutablePeer`, and `NewExternalAddrOfPeer` notifications too. Otherwise a full
table could move rejected addresses into an unbounded reporting queue. Existing
`RoutingUpdated` snapshots already carry their reservations.

### Aggregate Routing Regressions

- Count saturation across peers, retained snapshots, removal, and resumed admission.
- Snapshot-generation saturation for repeated updates to one peer; removal still works at capacity.
- Byte saturation with 99 address replacements, seed/LAN/relay preservation, migration, and oversized-update rollback.
- Pending admission under count/byte ceilings, lazy promotion, deferred eviction retention, and final release.
- Raw notification admission at capacity, bounded rejection, and release on dispatch.

### Routing Checkpoint Evidence

| Check | Result |
| --- | --- |
| Five aggregate routing regressions and workspace | 1,280 passed; 22 opt-in tests ignored |
| Internal connection learning, shared and separate DHTs | Passed: 65 connections each; 64 addresses retained |
| Namespace DHT, forced relay pairing, peerless code, and owned QUIC | Passed |
| Namespace relay/direct network move | Passed |
| Android x86_64 native library | Built offline; four existing warnings |
| Nix desktop / Android source parity | Passed with cached tool overrides, including both new modules |
| Root / changed vendor formatting | Passed |
| Workspace Clippy correctness, suspicious, and perf groups | Passed; style warnings remain |

Logs use `/tmp/p2p-vpn-kad-aggregate-routing-`: `workspace-queue.log`,
`internal-queue.log`, `dht-queue.log`, `relay-queue.log`, `code-queue.log`,
`quic-queue.log`, `move-queue.log`, `android-queue.log`, `nix-queue.log`, and `clippy.log`.

The initial queue adapter lacked its batch-insertion method; that compile error
is fixed. `queue-check.log` and `workspace-verified.log` record the failed
intermediate builds, not acceptance evidence.

Full upstream vendor unit tests, full Nix packaging, ARM64 compilation, APKs,
and physical deployments were not repeated. No RSS or sustained settling claim
is made. Total query-pool limits and combined overload tests remain open;
the phase is not complete.

## Query-Pool Admission Foundation

The vendored pool now supports an opt-in retained-entry cap. Both iterative and
fixed-phase construction enforce it. `query_pool_usage()` counts finished entries
awaiting polling; `query_is_retained(id)` does not hide them like `query(id)` does.

### Checked Starts

```rust
config.set_query_pool_capacity(NonZeroUsize::new(32).unwrap());
let lookup = kad.try_start_query(|kad| kad.get_record(key))?;
let bootstrap = kad.try_start_query(|kad| kad.bootstrap())??;
```

The closure must start exactly one query. Capacity errors occur before it runs,
preserving local storage and caller ownership. The closure's return value keeps
existing store/bootstrap errors separate from admission failure.

Legacy starts remain available. When a configured cap rejects one, its returned
ID is not retained and has no completion event. No rejection-result backlog is
created. Use checked starts when enabling the cap; this is a new opt-in contract,
not a change to unconfigured library behavior.

### Supporting Ownership Rules

- Local record/provider results are not queued for rejected IDs.
- Rejected bootstraps do not increment the active-bootstrap suppression counter.
- Background jobs use the smaller of their allowance and the pool cap before taking work.
- Cancellation releases capacity immediately; phase transitions reuse the retiring query's slot.

### Admission Regression Coverage

| Scenario | Verified Behavior |
| --- | --- |
| Finished entry at capacity | Remains counted until retirement or cancellation |
| Rejected checked start | Closure is not called; no local side effects |
| Repeated legacy starts | Rejected IDs are unretained; no local-result backlog |
| Bootstrap and publication phases | Reuse capacity while an unrelated query remains retained |
| Background jobs at capacity | Preserve pending records and resume after foreground retirement |
| Rejected bootstrap | Does not leave automatic bootstrap suppression active |

All four `query_pool_capacity` tests passed with the offline cached toolchain.
These are deterministic behaviour tests, not production-cap activation evidence.
Log: `/tmp/p2p-vpn-kad-pool-admission-focused-final.log`.

### Foundation Validation

| Check | Result |
| --- | --- |
| Offline workspace tests | 1,284 passed; 22 opt-in tests ignored |
| Owned QUIC packet-plane namespace | Passed |
| Relay/direct network-move namespace | Passed |
| Workspace Clippy correctness, suspicious, and perf groups | Passed; existing style warnings remain |
| Root Rust formatting | Passed |

Logs use `/tmp/p2p-vpn-kad-pool-admission-` with suffixes
`workspace-final.log`, `quic.log`, `move.log`, and `clippy-final.log`.
Normal namespace recovery does not prove recovery under aggregate overload.

### Production Integration

The shared production constructor enables a 32-entry cap per DHT instance.
It includes finished entries awaiting retirement. A separate public-pairing DHT
has its own budget; no JSON or Nix override is required.

The cap leaves headroom above the small scheduled foreground/background sets
while limiting retained query addresses to 8 MiB per DHT (32 times 256 KiB).
That calculation excludes pending RPCs, metadata, and queued results: this is
not yet a total query-memory bound.

Standalone pairing uses checked starts: rejected provider lookups retry at the next lookup interval
without retaining a query ID or consuming the public-lookup attempt budget.

Targeted peer recovery also uses checked starts. Capacity rejection preserves
retry eligibility and does not record a nonexistent query or start its cooldown.
Already admitted unrelated foreground queries remain retained.

Address publication returns capacity rejection separately from no-address or
encoding failures. Rejection preserves the pending update, respects the existing
retry interval, and regenerates the current address snapshot on retry.

Maintenance provider, membership, bootstrap, and relay queries now use checked
starts, including AutoNAT-triggered relay discovery. Query-start counters only
advance after admission; existing store/bootstrap failures remain distinct.

Diagnostic relay scans and membership checks use checked starts. Their existing
started flags reflect admission; membership publication reports capacity failure
through its existing error field without writing a local record.

Daemon pairing provider publication uses checked starts. Capacity rejection
schedules a retry without consuming the publication-attempt budget or retaining
a query; real publication failures keep their existing backoff and counters.
Rejected join lookups remain eligible for a later driver tick without owning a
query or incrementing lookup counters.

The CLI pairing-accept path also uses checked starts for provider, address-record,
closest-peer, and bootstrap discovery. Diagnostics distinguish rejected starts
from launched queries, including provider-result-triggered closest-peer queries.

The caller audit now covers `src/main.rs` as well as `src/runtime`. Maintenance
rotation has a saturation/release regression for relay and provider work.
Membership publication has a signed-record saturation/release regression.
Pairing provider drivers cover both versions with shared and separate public
DHTs, including retry delay, local-store effects, and resumed admission.

Focused logs: `/tmp/p2p-vpn-kad-provider-driver-saturation.log` and
`/tmp/p2p-vpn-kad-membership-saturation.log`. Both passed; these tests use
explicit test caps and do not prove production activation.

Pool-entry count alone does not bound queued query results, query metadata,
or pending RPC payloads. Those owners and combined
overload/recovery tests remain required before completing this goal.

### Production Admission Evidence

| Check | Result |
| --- | --- |
| Offline workspace tests | 1,293 passed; 22 opt-in tests ignored |
| Namespace DHT discovery, peerless pairing, forced-relay pairing, owned QUIC | All four passed |
| Namespace relay/direct network move | Passed |
| Android x86_64 native library | Compiled offline; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tool overrides |
| Root Rust formatting and whitespace | Passed |
| Workspace Clippy correctness, suspicious, and perf groups | Passed; style warnings remain |

Logs use `/tmp/p2p-vpn-kad-production-cap-` with suffixes `workspace.log`,
`namespace.log`, `move.log`, `android.log`, `nix.log`, and `clippy.log`.

The first activation run exposed a handler-test setup conflict: its 64 requests
were stopped by the new 32-query limit. That test now explicitly allows 64 queries
to exercise handler negotiation/expiry; constructor tests assert production caps
of 32 for primary, separate public-pairing, and standalone pairing DHTs.

No full Nix package, APK, ARM64 build, or physical deployment was performed.
Normal namespace tests do not prove VPN continuity during aggregate overload.

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
