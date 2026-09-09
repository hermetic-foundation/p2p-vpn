# Resource Measurement Acceptance

## Disposition

**Phase 3: complete with explicit evidence limitations.** This accepts the
measurement, analysis and reporting deliverables, not every subject's runtime
behavior. The subsequent [phase-4 audit](kademlia-workstream-acceptance.md)
defers overall acceptance for RM-1; production readiness remains open.

The original protocol permits failures, censoring and unavailable metrics.
All 24 prescribed pairs were executed and retained; acceptance does not require
turning them into 24 successful efficiency comparisons.

| Outcome | Count | Treatment |
| --- | ---: | --- |
| Recorded subject runs | 48 | All profiles, workloads and repetitions covered |
| Completed runs | 44 | Comparison eligibility still depends on coverage and useful work |
| Censored runs | 4 | Preserved; no paired efficiency claims |
| Required pairs | 24 | Three repetitions in each of eight cells |

## Evidence

| Artifact | Purpose |
| --- | --- |
| [Build manifest](kademlia-resource-builds.json) | Full source revisions, binary hashes and toolchain settings |
| [Collection index](kademlia-resource-campaign-v3.json) | Per-run provenance, observation hashes, stages, outcomes and missing captures |
| [Method](kademlia-resource-measurements.md) | Frozen protocol, versions, ordering, budgets, reproduction and retention |
| [Analysis notes](kademlia-resource-analysis.md) | Numeric results, metric semantics, uncertainty and attribution checks |
| [Compressed results](kademlia-resource-analysis.json.gz) | All per-run measurements, paired outcomes and eligible repetition statistics |

Baseline is `5ecb01ea`; current is `3b503ad2`. Version-3 harness integration is
`92ca4bc3`, identified by executable hashes in the campaign. The earlier
`2a1ff204` harness belongs to archived version-2 evidence, not this campaign.

## Acceptance Checklist

Statuses describe evidence fulfillment, not blanket runtime correctness.
"Met with limits" means the original requirement permits the documented missing
or censored evidence; it does not mean the unavailable conclusion is established.

| Requirement | Disposition | Inspected Evidence / Limitation |
| --- | --- | --- |
| Pre-workstream baseline and current selection | Met | Build manifest and pinned campaign binary verification |
| Revision, binary, toolchain and effective-input provenance | Met | Collection auditor checks executables, identities, configurations and worker results |
| Identical paired external conditions | Met | Frozen namespace topology, paired config hashes, common helper and generated pair identities |
| Four workloads, two DHT profiles, three paired repetitions | Met | 48 unique artifacts and 24 pairs; exact frozen plan checked by auditor |
| Frozen ordering, durations, rates and watchdogs | Met | Protocol module, controller plan and per-run stage records |
| CPU normalization, RSS, owned sockets and connections | Met with limits | Per-role process windows; 56 parsed CPU windows unavailable under frozen rules |
| Dial/query rates, admissions, completions and owners | Met with limits | Source-classified catalog; internal DHT metrics are current-only, not a fabricated baseline |
| Infrastructure versus overlay distinction | Met with limits | Helper process and relay counters separated; unobservable peer classes explicitly unspecified |
| Missing values, resets, gaps and process replacement | Met | Parser/window checks and regression tests; missing is distinct from zero |
| Useful work and recovery/settling evidence | Met with limits | Packet reports and stage confirmation times; failed deadlines and truncations retained |
| Per-run absolutes and comparable paired deltas | Met with limits | Outcome, window and useful-work gates; pressure deltas withheld for unequal delivery |
| Replication, counts, medians and ranges | Met with limits | Actual eligible counts per metric; no three-pair claim where fewer qualify |
| Regressions, anomalies and uncertainty | Met | Transport split, RSS increases, late recovery, instrumentation overhead and capture limits investigated and qualified |
| Bounded storage, redaction and retained failures | Met | Frozen caps, 7.627 GiB task usage at review start, redaction checks and private raw evidence retained |
| Scripted reproduction and applicable tests | Met | Re-audit/aggregation, deterministic output checks, measurement tests and cached Nix source check |
| Structured documentation and atomic publication | Met | Method, index, analysis, this checklist and verified Conventional Commits on main |

No original measurement requirement is waived. The following conclusions remain
unproved: equal-work pressure efficiency, successful recovery in every trial,
fixed-transport attribution, and long-term leak freedom.

## Results Summary

Endpoint A medians are shown for orientation; full results preserve endpoint B
and infrastructure separately. CPU is percent of one logical core. Do not sum
namespace TCP totals into process-owned connections.

| Healthy Idle Metric | Public Baseline / Current | Private Baseline / Current |
| --- | ---: | ---: |
| CPU | 0.0433 / 0.0367 | 0.0767 / 0.0467 |
| Mean sampled RSS, KiB | 20034.7 / 20523.8 | 21184.9 / 21953.9 |
| Mean owned socket inodes | 13.951 / 14.000 | 15.426 / 15.623 |
| Mean owned established TCP sockets | 0.951 / 1.000 | 2.426 / 2.623 |
| Application redial attempts/second | 0.010 / 0 | 0 / 0 |
| Provider lookup calls/second | 0 / 0 | 0 / 0 |

- These windows have three eligible pairs. A zero application lookup rate is not a census of every internal libp2p request.
- Public traffic CPU rose slightly: median paired change +0.0278 percentage points. Selected RSS windows rose by hundreds of KiB.
- Private traffic CPU fell, but all three baseline A windows used TCP streams while current used datagrams. This is not isolated evidence of cheaper DHT processing.
- The legacy datagram metric covers multiple backends. Its name does not independently identify QUIC.
- Current diagnostic serialization differs from baseline; its CPU/allocation contribution was not isolated. No causal allocation explanation is established for RSS increases.

## Recovery and Retention

- Three current private-recovery runs reached the 16 MiB observation cap. Partial windows are identified, not completed or interpolated.
- Public recovery repetition 3 first succeeded directly at 360.12 seconds of its 375-second stage. Confirmation arrived in post-recovery, outside the required window.
- Sequential failure probes contributed cadence gaps. Keep all 218 gap events and the frozen 7.5-second validity rule.
- All six pressure pairs delivered different packet counts between subjects. Their raw resource observations remain available, but efficiency deltas are withheld.
- All 12 current endpoint final post-pressure captures show zero primary query owners, pending RPCs and application recovery queries; handlers remain present.
- This is sampled retirement evidence, not proof that all allocated memory is released or that no long-term leak exists.

## Prioritized Follow-Ups

These are tracked decisions for subsequent work, not changes authorized or
performed by this report. Raw evidence and the frozen outcomes must survive any
later fix or replacement experiment.

| ID / Priority | Evidence and Scope | Acceptance for Subsequent Work |
| --- | --- | --- |
| RM-1 / P1 | Late direct-path promotion in public recovery repetition 3 | Explain the delay from a reproducible diagnostic; regression-test any fix and preserve relay fallback and autonomous recovery |
| RM-2 / P1 | Three private-recovery observation-cap truncations | Design compact bounded capture and independently scheduled probes; any new version must fit its declared budget and retain original censored artifacts |
| RM-3 / P2 | Pressure delivery differs despite equal offered load | Choose an explicitly declared useful-work comparison method; do not add a post-hoc tolerance to this dataset |
| RM-4 / P2 | Private-traffic transports differ; metric naming is ambiguous | Separate backend identity in diagnostics and predeclare fixed-transport tests before attributing CPU changes to DHT controls |
| RM-5 / P2 | Higher sampled RSS and unequal instrumentation | Attribute allocations/diagnostic overhead with suitable evidence before optimizing; retain functionality and resource bounds |
| RM-6 / P2 | Short retention windows and remaining broader acceptance | Complete phase-4 cross-platform/reliability review separately; longer-duration claims require their own evidence |

## Verification and Boundaries

- Fresh measurement tests and full saved-dataset audit/aggregation were run for this review. Existing tests cover parsing, invalid intervals, censoring and comparison gates.
- Published decompressed SHA-256: `2deb18d7ab47db67c8a2a0ae24551ccd66083fbf8cdc146b071cd1e85cf9cf73`.
- Artifact checks cover 48 runs, 24 pairs, unique observation hashes, four censored outcomes, unavailable deltas for censored/unequal-work pairs and redaction.
- Cached Nix source verification and formatting/whitespace checks apply. Runtime/vendor code is unchanged; full runtime, Android and physical-network tests were not rerun.
- No Lean model covers this measurement scheduler. Executable tests are not presented as formal verification.
- No raw artifacts were deleted, no subjects rerun, no physical hosts changed and no observation thresholds relaxed during this review.

Reproduction and exact per-metric definitions are linked above. Final acceptance
of the broader resource-limits workstream remains phase 4; this report is not a
production deployment recommendation.
