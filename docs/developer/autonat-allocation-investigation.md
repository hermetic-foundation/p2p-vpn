# AutoNAT Allocation Investigation

## Status

Existing connected captures narrow one recurring allocation step to intervals
containing AutoNAT refusals. This is temporal correlation, not causal ownership
or proof that all retained growth is bounded. No runtime fix is justified yet.

## Evidence

Analysis uses the two [connected allocation captures](connected-allocation-results.md),
without new runtime execution. The portable [correlation data](autonat-allocation-correlation.json)
retains source/log hashes, matching steps and every interval with AutoNAT events.

| Exact sampled increase | Intervals | With matching AutoNAT event | Without AutoNAT event |
| --- | ---: | ---: | ---: |
| 1,460 bytes | 7 | 7 outbound refusal intervals | 0 |
| 52 bytes | 8 | 5 inbound refusal intervals | 3 |

There are 76 intervals containing AutoNAT events across the four daemon series.
Most do not have either exact net increase. Concurrent allocations/deallocations
can mask a component's contribution; no unique allocator signature is established.

### Repeat Final Minute

| Node | Monotonic interval, ms | Net bytes / blocks | Logged event |
| --- | --- | ---: | --- |
| A | 835,000 to 840,001 | 52 / 1 | Inbound refusal |
| A | 845,002 to 850,003 | 1,460 / 3 | Outbound request and refusal |
| B | 840,001 to 845,001 | 1,460 / 3 | Outbound request and refusal |
| B | 845,001 to 850,000 | 52 / 1 | Inbound refusal |

The paired ordering differs, but each daemon ends 1,512 bytes/four blocks higher.
Both RSS series remain flat. These AutoNAT probes concern the connected fixture
peer; they are not evidence of contact with public infrastructure.

## Source Findings

| Component | Retained owner candidate | Cleanup visible in source |
| --- | --- | --- |
| AutoNAT client | `ongoing_outbound` map and throttled-server vector | Response removes request; server selection drains expired throttle entries |
| AutoNAT server | `ongoing_inbound` map and throttled-client vector | Refusal branch does not insert either owner |
| AutoNAT behaviour | `pending_actions` deque | Poll removes queued actions; empty capacity can remain |
| Request-response connection | Inbound/outbound pending-response sets | Completion removes IDs; connection close drops connection storage |
| Request-response behaviour | `pending_events` deque | Empty queue shrinks only when capacity exceeds 100 |

Sources are the locked cached `libp2p-autonat` 0.15.0 and
`libp2p-request-response` 0.29.0 implementations. Exact source hashes are in the
portable evidence; no dependency or cached source was modified.

The refusal path rules out a successful inbound dial-back allocation as the
immediate explanation for these events. It does not rule out request-response
sets, handler state, transport buffers or other concurrent work.

## Extraction

For each node log, retain allocation samples and AutoNAT event lines:

```sh
rg '^(allocation_sample |level=.*event=autonat_)' node-a.log
```

1. Parse each `allocation_sample` suffix as JSON.
2. Subtract adjacent live-byte/block values; retain their monotonic times.
3. Attach AutoNAT lines appearing between those samples in log order.
4. Retain all AutoNAT intervals and all exact 52/1,460-byte steps, including counterexamples.

Events have ordering but no independent monotonic timestamp here. The association
is only with an approximately five-second interval, not an exact allocation time.
The original sampler's concurrent counter-load limitations still apply.

## Next Isolation

- Exercise AutoNAT refusals with connected local swarms, separating first use from repeated requests on the same connection.
- Measure requested bytes before/after completion, connection retirement and full swarm/runtime drop.
- Include an otherwise identical no-probe control; check actual refusal events instead of assuming the timer fired.
- Distinguish behaviour storage from handler/transport storage before proposing a capacity or cleanup change.
- Freeze event counts, budgets and teardown assertions before the diagnostic capture; preserve normal security defaults in production.

The [isolated ownership matrix](autonat-ownership-review.md) now shows a plateau
after first use on one connection. Exact daemon owners and a 224-byte diagnostic
teardown residual remain open, along with pressure and broader platform coverage.
