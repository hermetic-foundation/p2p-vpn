# Aggregate Bounds Acceptance Audit

## Scope

This audit covers phase 1, **Aggregate Resource Bounds**, of the
[resource-limits workstream](kademlia-resource-plan.md).
Phase 1 implementation and verification are complete. This is not
production-readiness acceptance. Subsequent phase status and the overall
disposition are in the [workstream audit](kademlia-workstream-acceptance.md).

| Included | Not Completed By This Phase |
| --- | --- |
| Handler pending work and inbound stream retention | Sustained recovery and healthy settling |
| Aggregate routing entries, addresses, pending entries, and snapshots | Comparable sockets, dial/query rates, CPU, and RSS measurements |
| All retained query phases, metadata, jobs, and queued work | Original workstream's final system acceptance |
| Deterministic overload/recovery and packaging integration | Physical-host, phone, or public-WAN deployment acceptance |

## Final Audit Corrections

The audit started from `b0915ce5`, after the behaviour queue checkpoint.
Existing green tests did not establish these additional ownership properties.

| Finding | Correction | Regression |
| --- | --- | --- |
| Cancel-then-push inbound replacement exceeds 32 retained slots | Replace an idle slot in place and wake its existing scheduler task | `replacement_burst_stays_bounded_and_wakes_replaced_idle_slots` |
| Dropped responses leave inbound requests waiting indefinitely | Apply the substream deadline to inbound waiting, sending, and idle states | `stalled_inbound_streams_expire_without_socket_activity_and_readmit` |
| Inbound/protocol activity can precede pending-request expiry indefinitely | Check outbound pending expiry before other handler events | Existing handler expiry tests and source ordering audit |
| Fixed-peer iterator can inherit oversized caller capacity through collection | Normalize the collected vector before iterator ownership | `metadata_fixed_peer_backing_is_normalized_and_released` |
| Short multiaddresses can retain oversized shared backing allocations | Detach accepted buffers in routing, query caches, provider metadata, and raw notifications | Metadata backing tests and `routing_only_budget_detaches_raw_notification_backing` |
| AutoNAT owns queries even with ordinary DHT maintenance disabled | Deliver owned terminal events and expire ownership on the unconditional redial tick | Three `autonat_query_*_without_kademlia_maintenance` tests |

The legacy inbound control demonstrates 160 retained slots after 128 arrivals
against a nominal 32-slot ceiling. The corrected owner stays at 32 through 512
replacement attempts and admits new work after dispatch or expiry.

### Published Corrections

| Revision On `main` | Change |
| --- | --- |
| `77145d89854e` | Bound and expire retained inbound Kademlia streams |
| `5784e4e45830` | Normalize retained Kademlia buffer capacity |
| `cc9c839cb56d` | Retire AutoNAT lookups without periodic DHT maintenance |

## Owning Layers

### Connection Handlers

| Owner | Bound | Retirement |
| --- | --- | --- |
| Outbound waiting requests | 64 requests / 256 KiB per handler | Dispatch, expiry, handler drop |
| Rejection IDs | 64 per handler | Report or handler drop; excess IDs are discarded |
| Outbound request tasks | 32 per handler | Response, error, timeout, handler drop |
| Negotiation senders | 32 per handler, counted even after task expiry | Ordered swarm callback or handler drop |
| Inbound stream states | 32 per handler, including stalled or reusable streams | EOF, error, in-place replacement, idle/request expiry, handler drop |
| Inbound timers and wakeups | One timer and scheduler waker per retained slot | Same lifetime as its slot |

The waiting-payload counter does not count active streams. Outbound tasks can
retain up to one admitted request each. Inbound response payloads arrive through
the separately bounded behaviour queue; codec frames retain the existing 16 KiB
wire limit. Transport/multiplexer buffers are not a handler-pending byte metric.

Pending expiry and inbound expiry use the configured substream timeout, ten
seconds by default. Inbound request activity refreshes its deadline; stalled
responses and idle streams do not. Expiry drops a DHT stream, not its connection.

Source: [handler](../../vendor/libp2p-kad-0.48.0/src/handler.rs),
[pending owner](../../vendor/libp2p-kad-0.48.0/src/handler/pending.rs),
[inbound owner](../../vendor/libp2p-kad-0.48.0/src/handler/inbound.rs).

### Per-DHT Storage

| Owner | Aggregate Ceiling | Release / Admission Rule |
| --- | --- | --- |
| Routing generations | 512 entries / 2 MiB encoded addresses | Shared leases cover present, pending, snapshots, and deferred eviction |
| Query pool | 32 retained phases | Finished entries remain charged until retirement; canceled entries drop immediately |
| Query candidate ledger | 8,192 identities across 32 phases | Failed candidates remain counted until phase retirement |
| Query address cache | 8 MiB encoded addresses | At most 256 peers and 64 addresses per peer per phase |
| Query input keys and values | 8 MiB | At most 256 KiB per retained query |
| Provider metadata | 4 MiB encoded addresses / 2,048 address slots | At most 64 addresses per query, each at most 2,048 bytes |
| Fixed-peer iterator backing | 8,192 allocated peer slots | Normalized capacity includes consumed iterator positions |
| Bootstrap target iterator | 8,192 target slots | At most 256 per retained bootstrap phase, including consumed positions |
| Acknowledgement/cache reporting | At most 8,192 peer slots | Reporting cap never lowers required quorum |
| Pending RPC payload | 256 live requests / 1 MiB | Shared reservations drop on failure, handoff, cancellation, or retirement |
| Background key batches | 128 keys / 2 MiB | Both jobs share foreground query admission; keys release on pop/removal/pass end |
| Background cursors/boundaries | Four keys / 1 MiB | Replaced during paging; released at pass end |
| Background skip hints | 64 keys / 1 MiB | Current/next-pass flags share one bounded map |
| Behaviour event/action queue | 512 entries / 4 MiB charged payload | Dispatch/cancellation release bytes; excess work is dropped |
| Mode-change backlog | One cursor and two flags | Latest mode replaces previous intent; no per-change event list |

These component payload ceilings sum conservatively to **31 MiB per DHT**.
This is not a process-memory ceiling or measured peak. Containers, active
handlers, codec/transport buffers, record storage, and caller-owned copies are
additional. Some categories overlap or cannot reach their maxima together.

### Capacity After Churn

- Pool, peer-ledger, peer-address, and skip maps have bounded admitted entry counts.
- Hash-table capacity may remain after deletion; it is not reported as live payload bytes.
- Per-query pending-RPC vectors can retain their high-water capacity after clear: conservatively 8,192 slots across the pool.
- Per-peer address vectors retain at most their configured 64 slots; phase retirement drops them.
- Fixed iterators report allocated slots, not only their unconsumed length.
- Queue slots are reserved at construction and cannot grow past configured admission.
- Record values, keys, and accepted multiaddress buffers cannot retain arbitrary caller spare capacity.

Container overhead remains additional to the payload accounting above. The
bounded element counts constrain container growth under the standard collections;
these counters are not allocator instrumentation or a portable RSS promise.

Source: [query pool](../../vendor/libp2p-kad-0.48.0/src/query.rs),
[query owners](../../vendor/libp2p-kad-0.48.0/src/query),
[background jobs](../../vendor/libp2p-kad-0.48.0/src/jobs.rs),
[routing leases](../../vendor/libp2p-kad-0.48.0/src/addresses/budget.rs),
[behaviour queue](../../vendor/libp2p-kad-0.48.0/src/behaviour/queue.rs).

## Production Producers

Every production payload-bearing start uses a typed checked API. Bootstrap uses
the capacity-checked closure API. Caller IDs are recorded only after admission;
the vendor pool independently enforces its cap on legacy starts and transitions.

| Producer | Application Ownership / Retirement |
| --- | --- |
| Periodic discovery, relay lookup, membership publication | One maintenance owner; terminal completion, expiry, preemption, suppression, or reset |
| AutoNAT-triggered relay lookup | Same owner; cleanup also works without the periodic maintenance timer |
| Address publication | One publication ID; terminal cleanup and cancellation before replacement/expiry |
| Targeted peer recovery | One concurrent lookup; terminal cleanup, expiry, revocation, suppression, or reset |
| Daemon code-pairing lookup | One join query; terminal cleanup and cancellation when its operation changes |
| Pairing provider publication v1/v2 | One ID per version; result cleanup or operation cancellation/stop-providing |
| Standalone code pairing | Checked bootstrap and provider lookup; result cleanup or dedicated swarm drop |
| CLI pairing acceptance | Checked provider/record/closest/bootstrap starts; operation-local swarm lifetime |
| Bootstrap diagnostics | Checked closest/get/put starts; operation-local swarm lifetime |
| Library bootstrap / replication / publication | Same retained query pool; jobs consume work only after admission |

Shared public, separate public-pairing, and standalone constructors all use
`controlled_kademlia_config`. Separate DHTs have independent budgets. Android
delegates to the same runner and standalone pairing constructors; it introduces
no separate Kademlia query producer.

## Compatibility

| Contract | Evidence |
| --- | --- |
| Minimal JSON and Nix | No new required options; defaults activate in the shared constructor |
| LAN-first discovery | Existing scheduling and LAN discovery logic retained; workspace and namespace regressions |
| Public bootstrap and relay fallback | Same protocol names, bootstrap peers, and checked producer paths |
| Autonomous route recovery | Namespace relay/direct network-move regression |
| Fresh address publication | Existing coalescing and capacity-retry tests; provider jobs read fresh store values |
| Authorization and wire encoding | No authorization, membership format, DHT wire schema, or packet-format changes |
| Generic library opt-out | Unconfigured query/routing collections preserve unbounded mode; backed by comparison tests |
| Inbound timeout | Existing timeout setting now also expires stalled/idle inbound streams; peers may open another stream |

## Validation

Final code verification on 2026-09-07 covers all corrections above together.
Earlier checkpoint evidence is in [Aggregate Bounds](kademlia-aggregate-bounds.md).

| Check | Result |
| --- | --- |
| Offline locked workspace suite | 1,328 passed; 22 opt-in tests ignored |
| Inbound production-owner tests | Four passed, including the legacy overflow control and timer wakeup |
| Namespace DHT discovery, peerless pairing, forced-relay pairing, owned QUIC | Four passed; 63.72 seconds combined |
| Namespace relay/direct recovery after network move | Passed; 52.34 seconds |
| Clippy correctness, suspicious, and performance groups | Passed; nonfatal style warnings remain, including new test warnings |
| Android x86_64 native library | Compiled offline in 34.59 seconds; four existing warnings |
| Nix desktop/Android source parity | Passed with cached tool overrides and unchanged sandbox assertions |
| Root and changed vendored Rust formatting / whitespace | Passed; each source retains its own Rust edition |

Logs use `/tmp/p2p-vpn-kad-final-audit-` with `workspace.log`, `namespace.log`,
`move.log`, `clippy.log`, `android.log`, and `nix.log` suffixes. The final focused
inbound repeat is `/tmp/p2p-vpn-kad-inbound-final.log`.

Namespace durations are smoke-test observations, not comparable performance
measurements. Cached builds used at most two Cargo jobs and no downloads.
Readable task temporary paths totaled approximately 5.1 GiB, below the 10 GiB cap.

### Reproduction

Use the repository's [testing commands](testing.md) and locked dependencies.
This run reused cached Nix Rust, formatter, Clippy, and NDK executables because
the full default Nix tool closure would require hundreds of builds.

```sh
cargo test --offline --locked --workspace
cargo test --offline --locked --test kad_inbound_owner
cargo clippy --offline --locked --workspace --all-targets -- \
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
```

Run namespace gates individually with `--ignored --exact --test-threads=1`.
They require namespace/TUN privileges. Names:

- `tun_namespace_ping_crosses_dht_discovered_overlay`
- `tun_namespace_pair_accept_crosses_relayed_live_pairing_overlay`
- `tun_namespace_code_pairing_crosses_peerless_overlay`
- `tun_namespace_ping_crosses_owned_quic_packet_plane`
- `tun_namespace_recovers_relay_and_direct_after_network_move`

## Limits

- No physical machines, personal flakes, or phones were deployed.
- No full Nix package closure, APK, ARM64 native build, or public-WAN experiment is claimed.
- No existing Lean/formal model was found; tests and source-level invariant review provide the evidence here.
- The native Android gate compiles x86_64; the Nix gate verifies desktop and Android source inclusion.
- Sustained settling, comparable resource measurements, and original-workstream final acceptance remain separate phases.
