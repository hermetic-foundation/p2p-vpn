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
let lookup = kad.try_get_record(key)?;
let bootstrap = kad.try_start_query(|kad| kad.bootstrap())??;
```

The closure must start exactly one query. Capacity errors occur before it runs,
preserving local storage and caller ownership. The closure's return value keeps
existing store/bootstrap errors separate from admission failure.

Payload-bearing operations use typed checked starts, such as `try_get_record`.
Those check metadata input limits too; `try_start_query` checks only capacity
and remains the wrapper for bootstrap, which has no variable-sized input.

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

## Pending Query RPC Bounds

Requests awaiting a peer connection share one budget across all queries in a
DHT. Reservations follow retained requests until cancellation, failure,
retirement, or handoff to a separately bounded connection handler.

| Limit | Default | Scope |
| --- | ---: | --- |
| Waiting RPCs | 256 | Aggregate per DHT, not per query |
| Waiting payload | 1 MiB | Keys, record-value capacities, provider addresses and vector slots |
| Lifetime | Parent query deadline | Failed, finished, and expired work cannot be handed off late |
| Rejection bookkeeping | Saturating counters | No rejected-payload or rejected-ID queue |

The request count allows eight waiting requests per retained query on average,
with headroom for fixed publication batches. The byte cap permits ordinary
record publication while preventing those batches from retaining unlimited copies.

`pending_rpc_usage()` reports retained requests, bytes, and count/byte rejection
totals. This is payload accounting, not allocator RSS. Container high-water
capacity and query metadata require separate accounting in the final audit.

Admission preserves existing requests and fails the rejected peer attempt.
Dial failure and query cancellation release reservations. Handler preload skips
finished/expired queries and transfers admitted live work out of this budget.

Disconnected provider requests remain pending until handoff. If all attempted
publication peers fail, `NoPeersReached` reports failure instead of success;
the runtime exposes `no_peers_reached`. This changes a local result enum, not
the wire protocol. Handoff is not remote acknowledgement or confirmed delivery.

Focused count/byte, cancellation, handoff, dial-failure, and late-handoff tests
passed in `/tmp/p2p-vpn-kad-pending-rpc-tests.log`. Provider handoff and capacity
failure passed in `/tmp/p2p-vpn-kad-pending-provider-test.log`.

### Pending RPC Checkpoint Evidence

| Check | Result |
| --- | --- |
| Offline workspace tests | 1,296 passed; 22 opt-in tests ignored |
| Namespace DHT, peerless pairing, forced-relay pairing, owned QUIC | All four passed |
| Namespace relay/direct network move | Passed |
| Android x86_64 native library | Compiled offline; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tool overrides |
| Root and changed vendored Rust formatting | Passed |
| Workspace Clippy correctness, suspicious, and perf groups | Passed; style warnings remain |

Logs use `/tmp/p2p-vpn-kad-pending-` with suffixes `workspace.log`,
`namespace.log`, `move.log`, `android.log`, `nix.log`, and `clippy.log`.
No full Nix package, APK, ARM64 build, formal model, or physical deployment
is claimed. Query metadata, queued results, and combined overload remain open.

## Idle Deadlines And Shared Traffic

The combined loopback test exposed missing deadline wakeups. With a four-ID
handler rejection queue and a 200 ms query timeout, all eight VPN frames arrived
but 27 unreported queries remained after one second without network activity.
Reproduction: `/tmp/p2p-vpn-kad-shared-overload-deadline-before.log`.

The query pool now arms one timer for its earliest started query deadline.
It reuses the timer while that deadline is unchanged and removes it when the
last query retires or is canceled. Deadline expiry does not depend on peer traffic.
Per-phase timeout values and wire formats are unchanged.

### Combined Regression

| Scenario | Assertion |
| --- | --- |
| TCP and QUIC loopback | Real production packet behaviours share a connection with Kademlia |
| Fill the 32-query pool | Extra checked start is rejected without running its closure |
| Cancel one admitted query | Other admitted queries remain owned; canceled work emits no result |
| Exceed handler payload and report budgets | Immediate errors or query deadlines retire all remaining work |
| Eight packet frames per wave | Every frame is acknowledged on the original connection, without duplicates |
| Small publication after overload | Remote record storage succeeds after query capacity is released |
| Repeat overload and recovery | Both cycles complete without replacing or closing the connection |

A separate regression abandons a dial and awaits the behaviour without injecting
any event. Its timer must emit a timeout and release pending-RPC bytes/count.
Both focused tests passed in `/tmp/p2p-vpn-kad-shared-overload-fixed.log`.

This exercises the VPN packet protocol, not TUN routing or sustained performance.
Handler limits are deliberately reduced to isolate payload/report saturation;
the production 32-query cap is retained.
Metadata and queued-result bounds remain required for goal completion.

### Idle Deadline Checkpoint Evidence

| Check | Result |
| --- | --- |
| Offline workspace tests | 1,298 passed; 22 opt-in tests ignored |
| Namespace DHT, peerless pairing, forced-relay pairing, owned QUIC | All four passed |
| Namespace relay/direct network move | Passed |
| Android x86_64 native library | Compiled offline; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tool overrides |
| Root and changed vendored Rust formatting | Passed |
| Workspace Clippy correctness, suspicious, and perf groups | Passed; style warnings remain |

Logs use `/tmp/p2p-vpn-kad-idle-deadline-` with suffixes `workspace.log`,
`namespace.log`, `move.log`, `android.log`, `nix.log`, and `clippy.log`.
No full Nix package, APK, ARM64 build, formal model, or physical deployment
was performed. No sustained performance or total process-memory claim is made.

## Query Metadata Bounds

The production 32-entry pool cap now combines with per-component metadata
ceilings. Both fixed and iterative admission paths enforce them, including
phase transitions and finished entries awaiting polling.

| Component | Per Query | Aggregate Per DHT |
| --- | ---: | ---: |
| Input key plus record value | 256 KiB | 8 MiB |
| Provider addresses | 64, each at most 2,048 encoded bytes | 2,048 address slots; 4 MiB encoded payload |
| Stored publication acknowledgements or cache candidates | 256 peer entries | 8,192 entries |
| Bootstrap refresh targets | At most 256 fixed-size targets | At most 8,192 slots, including consumed iterator storage |

These are conservative component maxima; not every query owns every component.
The 12 MiB input/address sum excludes candidate-address caches, pending RPCs,
queued results, container overhead, and allocator RSS. It is not a total
query-memory or process-memory claim.

The input ceiling provides headroom for existing control records. It does not
increase the existing wire-message or record-store size limits.

### Ownership And Overload

- Keys, record values, and provider vectors discard excess backing capacity on admission.
- Provider addresses are filtered before collection; cache insertion never exceeds its entry ceiling.
- Cancellation and retirement release metadata; marking a query finished alone does not release its slot.
- `query_metadata_usage()` includes finished entries, encoded payload, result entries, address slots, and rejected inputs.

Publication quorum uses distinct accepted acknowledgements, not the bounded
reporting list length. Duplicate or unsolicited responses cannot satisfy quorum.
Errors retain the original required quorum; their success list may be truncated.
Candidate admission limits can still prevent reaching an unusually large quorum.

### Checked Production Starts

| API / Result | Behavior |
| --- | --- |
| `try_get_closest_peers`, `try_get_n_closest_peers` | Check pool capacity and target-key size |
| `try_get_record`, `try_get_providers` | Check pool capacity and record-key size |
| `try_put_record`, `try_put_record_to` | Check combined key/value size before query admission or local store effects |
| `try_start_providing` | Check key size before retaining a local provider record |
| `QueryStartError::Capacity` | Temporary rejection; preserve existing retry eligibility |
| `QueryStartError::InputTooLarge` | Reject without retained query, payload, local store side effect, or completion backlog |

All payload-bearing production callers use the typed starts. Store failures
remain distinct inner results. Legacy methods remain available; when input
limits reject them, they return an unretained ID without a completion event.
Unconfigured vendored users retain unlimited metadata admission.

Address publication retries capacity exhaustion with a fresh snapshot. An
oversized snapshot reports failure and waits for a new address update instead
of repeatedly retrying the same invalid input. Bootstrap diagnostics distinguish
`query_input_too_large` from `query_capacity`.

### Regression Coverage

| Scenario | Required Assertion |
| --- | --- |
| Every typed and legacy input path | Oversized input creates no retained query, record, provider, or result |
| Full pool including finished work | Aggregate bytes stay bounded; cancellation permits immediate readmission |
| Caller vectors with large spare capacity | Retained key/value buffers are normalized |
| Duplicate acknowledgements | Do not inflate success count or satisfy quorum |
| Result list smaller than required quorum | Unique acknowledgements still succeed; partial completion still fails |
| Cache and provider-address churn | Entry and encoded-address limits hold across response and phase transitions |
| Shared, separate pairing, standalone constructors | Production input limits are enabled |
| Address-publication driver | Invalid snapshots do not enter a retry loop; new updates remain eligible |

Focused tests live in `src/runtime/p2p/metadata_tests.rs`, with constructor and
caller regressions alongside their existing runtime tests.

### Metadata Checkpoint Evidence

| Check | Result |
| --- | --- |
| Focused metadata and caller regressions | Seven passed |
| Offline workspace tests | 1,304 passed; 22 opt-in tests ignored |
| Namespace DHT, peerless pairing, forced-relay pairing, owned QUIC | All four passed |
| Namespace relay/direct network move | Passed |
| Android x86_64 native library | Compiled offline; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tool overrides, including the new metadata modules |
| Root and changed vendored Rust formatting / whitespace | Passed |
| Workspace Clippy correctness, suspicious, and perf groups | Passed; existing style warnings remain |

Logs use `/tmp/p2p-vpn-kad-metadata-` with suffixes `focused.log`,
`workspace.log`, `namespace.log`, `move.log`, `android.log`, `nix.log`,
and `clippy.log`. The final workspace run includes the quorum correction.
Readable project temporary paths total approximately 4.82 GiB; no downloads occurred.

The Nix check used the existing source-parity assertions with cached tool inputs,
not the full package closure. No full Nix package, ARM64 native build, APK,
formal model, physical deployment, or public-network measurement is claimed.

### Remaining Query Owners

| Owner | Remaining Work |
| --- | --- |
| Behaviour event/action queue | Bound query results and unsent actions without stranding terminal query ownership |
| Background record/provider jobs | Bounded key batches and skipped-key bookkeeping verified below |
| Final aggregate audit | Account for container high-water storage, cancellation, phase changes, and every producer |

Metadata enforcement does not close these owners. Phase 1 remains active.
Wire formats, authorization, minimal JSON/Nix configuration, and packet transport
are unchanged. Public-WAN settling and process measurements remain later phases.

## Background Job Storage

The original jobs cloned every stored record or provider into an owned snapshot.
The record job also accumulated skipped keys independently of query admission.
Production now enables `set_background_job_limits()` for both job owners.

| Owner | Production Ceiling | Aggregate Per DHT |
| --- | ---: | ---: |
| Pending key batch | 64 keys / 1 MiB per job | 128 keys / 2 MiB |
| Resume and page-boundary keys | Two keys per job, each at most 256 KiB | Four keys / 1 MiB |
| Replication skip map | 64 keys / 1 MiB | One map / 1 MiB |
| Record input selected for admission | Key plus value at most 256 KiB | Moves immediately into the separately bounded query pool |
| Snapshotted record values / provider addresses | None | None |

The retained key-payload ceiling is 4 MiB per DHT. This excludes container
overhead, transient selection copies, the record store itself, admitted queries,
and allocator RSS. Key buffers are normalized instead of retaining caller spare
capacity. Metadata slots are bounded independently by the counts above.

### Progress And Retirement

1. Check shared query admission before polling either job.
2. Scan borrowed store entries for the next bounded, ordered key prefix.
3. Fetch the current record only when its key reaches the front of the batch.
4. Reject oversized input before cloning its value; discard deleted or expired work.
5. Keep the cursor across batches; release batch/cursor storage when the pass ends.

The first excluded key bounds each page. A smaller later key cannot advance
the cursor past an earlier key that failed byte admission. New keys behind the
cursor remain eligible on the next periodic pass; stored values are read fresh.

Each poll discards at most 64 selected keys before yielding and waking itself.
Refilling a page scans the store, and provider selection scans local providers.
Production uses the existing bounded `MemoryStore`; scan cost still depends on
store size, so this is not an O(1) work or sustained-performance claim.

Current-pass and next-pass skip intentions share the same bounded map. A full
map drops new skip hints, not records; excess records may be replicated normally.
Local record removal and `stop_providing` remove matching pending job keys.
Already admitted query work is not recalled by local record removal.

`background_job_usage()` aggregates both jobs' keys, bytes, cursors, and skip
state, plus saturating rejection counters. `bounded_jobs` identifies activation;
the usage report does not cover unconfigured legacy snapshots.

### Compatibility And Coverage

- Generic library jobs retain snapshot behavior unless the new limits are enabled.
- Production constructors enable limits for shared, separate-pairing, and standalone DHTs.
- Minimal JSON/Nix, DHT wire messages, authorization, and packet transport are unchanged.
- Explicit local record removal also removes legacy pending snapshots, preventing stale republication.

| Regression | Evidence |
| --- | --- |
| Ordered variable-size keys | Both jobs cross count/byte boundaries without starving the large earlier key |
| Foreground saturation | No batch consumed until capacity is released; unrelated query stays retained |
| Fresh value / deletion / expiry | Bounded jobs read updates and discard obsolete work; legacy snapshot is a comparison control |
| Oversized key and combined key/value input | No query admitted; corrected store entries become eligible on the next pass |
| Inbound loopback churn | Skip count and byte ceilings hold; explicit removal releases capacity for a new hint |
| Constructor activation | Both background owners are bounded in each production DHT constructor |

### Background Job Checkpoint Evidence

| Check | Result |
| --- | --- |
| Focused background and constructor regressions | Ten passed |
| Offline workspace tests | 1,308 passed; 22 opt-in tests ignored |
| Namespace DHT, peerless pairing, forced-relay pairing, owned QUIC | All four passed |
| Namespace relay/direct network move | Passed |
| Android x86_64 native library | Compiled offline; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tool overrides, including the new job module |
| Root and changed vendored Rust formatting / whitespace | Passed |
| Workspace Clippy correctness, suspicious, and perf groups | Passed; nonfatal style warnings remain |

Logs use `/tmp/p2p-vpn-kad-jobs-` with suffixes `focused.log`, `workspace.log`,
`namespace.log`, `move.log`, `android.log`, `nix.log`, and `clippy.log`.
The workspace run includes current/next-pass skip bookkeeping. Readable project
temporary paths total approximately 4.74 GiB; no downloads occurred.

The source-parity check is not a full Nix package build. No ARM64 native build,
APK, formal model, physical deployment, or public-network measurement is claimed.
Query-result and action queues still require bounds; phase 1 is not complete.

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
