# Resource-Limits Final Acceptance

## Disposition

**Final audit complete; unconditional workstream acceptance deferred.**
Phases 1-3 retain their documented completion and limitations. The late direct
recovery recorded in phase 3 needs diagnosis before closing the overall recovery
gate. No runtime defect or permanent loss of reachability is inferred from it alone.

This is an evidence audit at `692a5fef`, not a deployment, new measurement
campaign, general reliability review or production-readiness certification.

## Requirements and Evidence

The requirements come from the [original retention review](kademlia-retention-review.md),
[workstream plan](kademlia-resource-plan.md), [aggregate gates](kademlia-aggregate-bounds.md),
[sustained protocol](kademlia-settling.md) and [measurement protocol](kademlia-resource-measurements.md).

| Status | Meaning |
| --- | --- |
| Satisfied | Inspected source and scoped verification establish the requirement |
| Limited | Evidence establishes only the stated boundary; excluded claims remain unproved |
| Unresolved | Conflicting or incomplete evidence prevents final acceptance |
| Not verified | No verification at the named scope; narrower checks are not substitutes |

### Retained Owners

The [phase-1 inventory](kademlia-final-ownership-audit.md#owning-layers) gives
individual ceilings. The shared [constructor](../../src/runtime/p2p.rs) enables
them for the primary DHT, separate pairing DHT and standalone pairing bootstrap.

| Requirement | Source / Owning Boundary | Inspected Regression Evidence | Status |
| --- | --- | --- | --- |
| Handler waiting count, bytes and rejection bookkeeping | [Pending requests](../../vendor/libp2p-kad-0.48.0/src/handler/pending.rs); 64 requests / 256 KiB per handler | `handler_pending_requests_enforce_count_bytes_and_bounded_rejections` | Satisfied |
| Negotiation retention, expiry and readmission | [Handler](../../vendor/libp2p-kad-0.48.0/src/handler.rs); 32 negotiations; pending expiry precedes inbound work | `handler_stalled_negotiations_expire_queued_requests_and_resume` | Satisfied |
| Inbound replacement and stalled responses | [Inbound owner](../../vendor/libp2p-kad-0.48.0/src/handler/inbound.rs); 32 retained slots | [Exact-source tests](../../tests/kad_inbound_owner.rs): legacy overflow control, replacement wakeup, expiry/readmission | Satisfied |
| Present, pending, deferred and snapshot routing storage | [Shared leases](../../vendor/libp2p-kad-0.48.0/src/addresses/budget.rs); 512 generations / 2 MiB | `aggregate_routing_*` tests include snapshot generations, pending storage and dispatch retirement | Satisfied |
| Protected seeds, address migration and useful alternatives | [Addresses](../../vendor/libp2p-kad-0.48.0/src/addresses.rs); seed protection cannot bypass admission | `routing_address_limits_preserve_seeds_lan_and_relay_under_churn`, pending-entry and migration tests | Satisfied |
| All retained query phases, including finished entries | [Query pool](../../vendor/libp2p-kad-0.48.0/src/query.rs); capacity counts the owning map, not active iteration | `query_pool_capacity_counts_finished_entries_and_rejects_without_side_effects`, bootstrap transition tests | Satisfied |
| Failed candidates and query-local address storage | [Retained peers](../../vendor/libp2p-kad-0.48.0/src/query/retained.rs); failed identities remain charged until retirement | Fixed initial candidates and real-loopback internal query-address regressions | Satisfied |
| Payloads, provider metadata, iterator backing and quorum | [Metadata](../../vendor/libp2p-kad-0.48.0/src/query/metadata.rs) and [tests](../../src/runtime/p2p/metadata_tests.rs) | Capacity normalization, finished ownership, phase transition and quorum-preservation tests | Satisfied |
| Aggregate pending RPCs and release | [Reservations](../../vendor/libp2p-kad-0.48.0/src/query/pending.rs); 256 requests / 1 MiB | `pending_rpc_budget_*`: cancellation, handoff, failure, completion and expiry | Satisfied |
| Background batches, cursors, skip hints and freshness | [Jobs](../../vendor/libp2p-kad-0.48.0/src/jobs.rs), [bounded owners](../../vendor/libp2p-kad-0.48.0/src/jobs/bounded.rs), [tests](../../src/runtime/p2p/background_tests.rs) | Count/byte limits, fresh values, removed/expired records and resumed admission | Satisfied |
| Unsent events, payloads, dial intents and mode updates | [Queue](../../vendor/libp2p-kad-0.48.0/src/behaviour/queue.rs), [tests](../../src/runtime/p2p/queue_tests.rs); 512 entries / 4 MiB | Count/byte rejection, unrelated results, retirement under inbound load, latest-mode preservation | Satisfied |
| Every production query producer obeys admission | [Runner](../../src/runtime/runner.rs), [bootstrap checks](../../src/runtime/bootstrap_check.rs), [standalone pairing](../../src/runtime/pairing_bootstrap.rs) | Typed checked starts; capacity-checked bootstrap; producer saturation and AutoNAT cleanup tests | Satisfied |

The conservative component payload sum is 31 MiB per DHT, **not an RSS ceiling**.
Container overhead, active handlers, record storage, transport buffers and caller
copies remain additional. Two DHTs have independent budgets, not one shared cap.

### Scheduling and Compatibility

| Requirement | Evidence Inspected | Status / Boundary |
| --- | --- | --- |
| Long failure backoff survives idle expiry | [Recovery owner](../../src/runtime/recovery_queries.rs), `sustained_failures_preserve_backoff_past_state_ttl` | Satisfied; expiry uses the later retry/last-query timestamp |
| No unowned periodic or insertion bootstrap | Shared constructor disables both triggers; `routing_updates_do_not_start_unowned_bootstrap_queries` | Satisfied; explicitly owned bootstrap remains available |
| Healthy suppression, released capacity and disabled-maintenance cleanup | [Timeline tests](../../src/runtime/runner/settling_timeline_tests.rs), AutoNAT terminal/expiry tests | Satisfied within explicit-time and live-loopback coverage |
| Kademlia overload preserves unrelated VPN packets | [Contention matrix](../../src/runtime/runner/kademlia_contention_tests.rs) saturates actual pools and drives transport | Satisfied for both DHT profiles over TCP and QUIC; resumed producers and final drain checked |
| Minimal configuration and LAN-first discovery | [Production-entry fixture](../../tests/support/recovery_soak.rs), unavailable initial infrastructure, ID-only overlay peers | Limited to isolated namespaces; bootstrap overrides replace public Internet access |
| Relay replacement, address changes and no management rescue | Five-cycle saved soaks; fixture checks process identity, configuration bytes, path and bidirectional packets | Satisfied for the recorded runs; not a physical-WAN claim |
| Direct-path recovery consistently meets the frozen fixture budget | Phase-2 soaks pass; phase-3 public recovery repetition 3 is not confirmed within 375 seconds | Unresolved: RM-1; harness and runtime contributions are not isolated |
| Quiet settling and session renewal | [Settling oracle](../../tests/support/recovery_settling.rs), final healthy windows, renewal regressions | Satisfied for recorded windows; no discovery-counter growth, bounded library work and query drain |
| Authorization, identity and protocol compatibility | Unchanged membership/wire formats; endpoint tests reject stale, unrelated and relayed authority | Limited source/regression review, not a complete security certification |
| Diagnostics reflect real owners | [Snapshots](../../src/runtime/kademlia_resources.rs), [resource tests](../../src/runtime/p2p/resource_tests.rs) | Satisfied; absent DHT differs from zero, retained differs from active, dial intent differs from socket |

Admission can reduce lookup results under pressure. Handler expiry now closes
idle/stalled DHT streams, not their shared connection. Provider cancellation does
not retract remote records; those expire normally. These are documented behavior
changes, not a claim that the vendored library is identical to upstream.

## Reused Runtime Evidence

`jj diff --from a25f78dd8723 --to 692a5fef` shows no changes in `src/`,
`vendor/`, `crates/` or `nix/android.nix`. Cargo adds only the already locked
`socket2` development dependency for measurement tooling.

Both raw `recovery-summary.json` files were inspected, not just prior prose.
Their binary SHA-256 is `b8583c257248f75a7d569379e5ae195226d23bee435ac23937dca9e153d06077`.

| Profile | Artifact Suffix | Outcome / Cycles | Runtime / Continuous Healthy |
| --- | --- | --- | --- |
| Private | `.e8d346a279ff2eca` | Passed / 5 | 1801.315s / 602.519s |
| Public | `.5b1966ccf4728177` | Passed / 5 | 1801.321s / 682.910s |

Directory prefix: `/tmp/p2p-vpn-tun_namespace_automatic_discovery_recovers_after_link_changes`.
The corresponding `/tmp/p2p-vpn-renewal-final-{private,public}.log` terminal
results pass. Historical failed soaks are retained, not substituted for these runs.

The inspected oracle checks unchanged daemons/configs, owner bounds, real packet
delivery, no stream fallback during healthy UDP gates, quiet counters and primary
query drain within 250 seconds. These checks do not observe every unsampled instant.

## Measurements and Follow-Ups

The [phase-3 report](kademlia-resource-final-report.md) accepts measurement
deliverables, not universal runtime success. Its 48 runs, 24 pairs, four censored
outcomes and eligibility exclusions remain authoritative and unchanged.

- Preserve all 218 sampling gaps and 56 unavailable parsed CPU windows; missing data is not zero activity.
- First direct success in the censored public run was at 360.12 seconds; confirmation arrived in post-recovery, not within its required stage.
- Final sampled query/RPC drain is retention evidence, not long-term leak freedom or allocator reclamation.

| Item | Final-Acceptance Decision | Reason / Next Acceptance Evidence |
| --- | --- | --- |
| RM-1: late direct promotion | Blocks unconditional recovery acceptance | Explain the observed delay and confirmation timing; reproduce any runtime defect, fix it with regression coverage, or demonstrate a measurement-induced cause |
| RM-2: private observation caps / probe cadence | Separate measurement-tool follow-up; may support RM-1 | Three 16 MiB truncations and sampling gaps limit evidence, not proof of runtime failure; use bounded compact capture and independent probes in a new declared protocol |
| RM-3: unequal pressure delivery | Separate experiment-design follow-up | All six pressure pairs have unequal delivered work; no paired efficiency deltas or post-hoc tolerance |
| RM-4: transport identity / causal comparison | Separate diagnostic and fixed-transport follow-up | Legacy datagram counters span backends; private TCP/datagram differences cannot isolate DHT cost |
| RM-5: RSS / instrumentation overhead | Separate attribution follow-up | Selected RSS increases are real observations, but allocation causes and diagnostic overhead were not isolated; no promised RSS target is declared met |
| RM-6: broader review / long-duration claims | This document completes the workstream audit only | Broader reliability, physical WAN, Android device acceptance and long-term leak claims still require their own evidence |

Phase 3 permits explicit censoring; that is why its measurement goal is complete.
Final recovery acceptance nevertheless needs reconciliation of the later failed
confirmation with earlier passing soaks. This is not a new public-network SLA or
a requirement to make every noisy observation into a successful comparison.

### Recommended Next Goal

**RM-1: diagnose late direct-path promotion.** Start with saved public repetition-3
logs, stage observations and timer/endpoint decisions. Separate first packet
success from the five-success confirmation oracle and its sequential probe cost.

1. Produce an evidence-backed timeline identifying where the delay occurs; do not infer permanent failure from the censored outcome.
2. If new diagnostics are necessary, predeclare one bounded isolated reproduction and preserve the original evidence and deadlines.
3. Regression-test any runtime fix; preserve LAN-first discovery, authorization, relay fallback, minimal configuration and resource bounds.
4. Reassess only the affected acceptance gate. Keep additional performance experiments and deployments in separate goals.

This is a proposed follow-up scope, not a newly started experiment or goal.

## Packaging and Verification

| Check | Result / Scope |
| --- | --- |
| Fresh offline locked workspace | 1,459 passed; 35 opt-in tests excluded; includes host-side Android tests and live TCP/QUIC contention |
| Exact-source handler owners | Eight observation tests and four inbound-owner tests passed |
| Opt-in connection-address regression | Passed separately in 3.01s for both DHT layouts; not included in the workspace passed count |
| Fresh Clippy / formatting | Correctness, suspicious and performance groups passed; advisory style warnings remain. Rust formatting passed |
| Documentation checks | Relative file links and whitespace passed; raw measurement artifacts unchanged |
| Cargo selection | One root path patch for `libp2p-kad 0.48.0`; vendored crate excluded from workspace membership, not compilation |
| Provenance / license | [Patch record](../../vendor/libp2p-kad-0.48.0/P2P-VPN.md), upstream checksum/revision and MIT notices retained |
| Fresh cached Nix source check | Passed: identical vendored sources in desktop, ARM64 and x86_64 Android source inputs; repository/packaged Cargo test target lists match |
| Android native compilation | Reused final x86_64/API 26 log: completed in 34.57s with four target warnings; affected runtime/native sources unchanged |
| Full Nix packages, ARM64 native, APK/device, physical WAN | Not verified by this workstream; source inclusion and host tests are not substitutes |
| Formal verification | No Lean sources or model found; no formal-proof claim |

Fresh logs: `/tmp/p2p-vpn-phase4-{workspace,owners,addresses,nix,clippy}.log`.
The source check uses cached tool overrides with unchanged assertions, not a full
default toolchain build. Original phase-2 namespace/native logs are reused explicitly.

Reproduce the fresh checks with the repository's Nix shell and cached dependencies:

```sh
cargo test --offline --locked --workspace -- --test-threads=2
cargo test --offline --locked --test kad_handler_resources --test kad_inbound_owner
cargo clippy --offline --locked --workspace --all-targets -- \
  -D clippy::correctness -D clippy::suspicious -D clippy::perf
cargo fmt --check
nix build --offline .#checks.x86_64-linux.rust-test-sources
```

Storage was 7.655 GiB before verification and 7.810 GiB afterward, below 10 GiB.
No new subject campaign,
raw-evidence deletion, physical-host change, downloads or threshold relaxation
was performed. Builds used at most two Cargo jobs.
