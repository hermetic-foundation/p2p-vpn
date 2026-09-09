# Resource Analysis

## Scope

Analysis of the frozen version-3 collection: 48 outcomes, 24 pairs, four workloads,
two profiles and three repetitions. This completes analysis, not the final phase-3
report, workstream acceptance review or a production-readiness assessment.

- [Compressed machine-readable results](kademlia-resource-analysis.json.gz): all per-run values, catalog entries, comparisons and repetition statistics.
- [Audited collection index](kademlia-resource-campaign-v3.json): provenance, artifacts, outcomes and observation hashes.
- [Method and commands](kademlia-resource-measurements.md): frozen timings, validity rules, reproduction and retention.

## Eligibility

| Workload | Public Profile | Private Profile |
| --- | --- | --- |
| Idle | Three eligible pairs for healthy windows | Three; some startup metrics have fewer |
| Traffic | Three eligible pairs | Three end-to-end pairs; transport differs |
| Recovery | At most two; one current run censored | None; three current runs censored |
| Pressure/release | Absolute results only; delivered counts differ | Absolute results only; delivered counts differ |

- Every metric carries its actual eligible pair count; the table is not a guarantee that every window qualifies.
- Retain 218 sampling-gap events and 56 unavailable parsed CPU windows. Three truncated runs lack complete process-window summaries.
- Failed/censored pairs have no efficiency deltas. Current-only metrics never receive baseline comparisons.
- Pressure's unequal delivery is not hidden by equal offered rates or successful final boundary probes.

## Selected Results

Endpoint A, three eligible pairs per row. CPU is percent of one logical core;
CPU deltas are percentage points. RSS is mean sampled KiB, not peak allocation.
The machine-readable artifact includes endpoint B and infrastructure separately.

| Profile / Window | Metric | Baseline Median | Current Median | Paired Delta Median [Range] |
| --- | --- | ---: | ---: | ---: |
| Public / idle | CPU | 0.0433 | 0.0367 | -0.0033 [-0.0067, ~0] |
| Private / idle | CPU | 0.0767 | 0.0467 | -0.0167 [-0.0333, -0.0133] |
| Public / traffic | CPU | 1.4500 | 1.5055 | +0.0278 [+0.0222, +0.0555] |
| Private / traffic | CPU | 6.7110 | 1.8667 | -4.8444 [-4.9111, -3.7556] |
| Public / idle | RSS | 20034.7 | 20523.8 | +569.2 [+489.1, +640.9] |
| Private / idle | RSS | 21184.9 | 21953.9 | +769.0 [+197.0, +863.5] |
| Public / traffic | RSS | 20007.1 | 20603.1 | +597.4 [+324.5, +630.4] |
| Private / traffic | RSS | 21225.4 | 21794.8 | +681.8 [+443.1, +743.7] |

- Median paired delta need not equal the difference between independently calculated subject medians.
- Current RSS is higher in these selected windows. No allocation trace establishes which structures account for the increase.
- Current exposes additional diagnostic state; capture serialization contributes measurement overhead. This dataset does not isolate that overhead from daemon work.
- Tiny idle CPU percentages magnify tick quantization and relative percentage changes. Prefer absolute values and ranges over headline percentages.
- No universal improvement is established: public traffic CPU rose slightly while private traffic CPU fell substantially under different selected transports.

## Attribution and Anomalies

| Observation | Investigation / Interpretation | Follow-Up |
| --- | --- | --- |
| Private traffic CPU reduction | All three baseline A windows used TCP stream fallback; all three current A windows used packet-plane datagrams | Do not attribute the reduction solely to DHT controls; isolate transport in any future attribution experiment |
| Datagram counter named `quic` | Both packet-plane datagram backends increment this legacy counter in the send path | Do not infer QUIC from its name; consider clearer instrumentation separately |
| Three private recovery truncations | Complete-record checks reached the frozen 16 MiB observation cap; current has additional diagnostic fields | Preserve censoring; design bounded compact observations before any separately authorized replacement campaign |
| Public recovery repetition 3 | First direct success at 360.12 seconds of a 375-second stage; confirmation arrived during post-recovery | Investigate late path promotion separately; do not relabel the run successful |
| Invalid failure-window intervals | Sequential probes during disconnection can exceed the five-second observation cadence | Keep frozen validity thresholds and unavailable results; decouple probing in future tooling |

- Transport choice is a behavioral outcome under identical external inputs. Published deltas are end-to-end observations, not fixed-transport microbenchmarks.
- Packet counter deltas span captured timestamps; the first capture may miss preload events. Use the generator's full report for offered/delivered workload totals.
- No runtime changes, timer changes or replacement measurements were introduced during analysis.

## Retention and Settling

- All 12 current endpoint post-pressure final captures have zero primary query-pool owners, pending RPCs and application recovery queries.
- Primary handlers remain at one to four in those captures. Open handlers are retained connection resources, not automatically leaked work.
- These current-only gauges have no baseline equivalent. Zero at the final capture does not establish the exact retirement time between samples.
- Stage outcomes retain first success and five-consecutive-success confirmation times; inspect them alongside coverage and censoring.
- Short observation windows, sampled peaks and RSS plateaus cannot establish long-term leak freedom or allocator release. No such claim is made.

## Metric Semantics

| Category | Meaning / Availability |
| --- | --- |
| Application counters | Common event counters; lookup calls, connection events and selected dial attempts are not a complete libp2p RPC census |
| Internal DHT counters | Current-only query phases, RPC requests, admissions, completions, retirements and rejections; primary/pairing roles remain separate |
| Owner gauges | Current-only retained counts/bytes; sample means/extrema, never cumulative event rates |
| Process resources | CPU tick deltas; sampled RSS and owned socket counts; namespace TCP totals explicitly not process-owned |
| Infrastructure separation | Helper process is separate; relay-specific application counters are labeled; internal DHT peer classes cannot be inferred where absent |

- Internal query phases are not application lookup calls. Retirement includes more than successful completion. Do not sum overlapping event categories.
- Gauge means are sample means; rates divide valid count deltas by valid elapsed time. Missing values and zero baselines remain explicit.
- Raw control captures and private configs are excluded. Per-metric catalog entries identify source definitions and availability.

## Reproduction and Validation

1. Build the `resource_measurement` test with the cached Nix toolchain.
2. Run `resource_dataset_analysis` using the command in the method document and a new output path.
3. Compare the uncompressed result against the published artifact; do not rerun subjects.

```bash
gzip -dc docs/developer/kademlia-resource-analysis.json.gz | sha256sum
```

Expected uncompressed SHA-256:
`2deb18d7ab47db67c8a2a0ae24551ccd66083fbf8cdc146b071cd1e85cf9cf73`.

- Artifact uses deterministic `gzip -n -9`. Raw JSON is retained locally; the compressed publication is under one MiB.
- Two final full-dataset passes took 54.27/54.59 seconds and produced byte-identical JSON, including the transport-context counters.
- Measurement tests, required Clippy groups, formatting and cached Nix source checks passed. Full-dataset audit/aggregation was executed without new measurements.
- Runtime/vendor code is unchanged; full workspace and Android builds were not repeated for analysis-only tooling.
- Final reporting and full phase-3 acceptance remain a separate goal, including decisions on the explicitly recorded follow-up investigations.
