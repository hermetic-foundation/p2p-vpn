# Daemon 336-Byte Ownership

## Frozen Diagnostic

| Setting | Value |
| --- | --- |
| Baseline | `6bc0d8a9` |
| Fixture | `tun_namespace_ping_crosses_two_node_overlay` |
| Binary | `/tmp/p2p-vpn-review-target/debug/deps/tun_namespace-92e26a895f071361` |
| SHA-256 | `ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486` |
| Observer | Cached GDB; native 336-byte allocator conditions from constructor diagnostic |
| Start / checkpoints | Node A child entry / three existing size-inventory emissions |
| Workload | Original direct fixture; graceful shutdown; two Tokio workers |
| Isolation | Existing fresh user/PID/network namespace fixture; no Internet route |
| Quotas | 4096 allocations, 64 sites, 24 frames/site, 240 characters/frame |
| Logs | 1 MiB per file; no raw allocation addresses or arguments |
| Watchdogs | Original 90-second fixture, 100-second outer; two-second kill grace |
| Repeats | Two fresh fixtures, unchanged limits |

An owned PATH wrapper replaces only node A's `unshare --net` invocation with
`unshare --net` plus GDB. Other invocations delegate unchanged. GDB and its
inferior share that namespace; the fixture's namespace handle refers to GDB.

No process-resource comparison uses the wrapper PID. Original traffic, selected
path, shutdown acknowledgement, exit and TUN/control cleanup assertions remain.
No builds overlap tracing; this is not a timing benchmark.

## Acceptance

- Require three checkpoint live sets, normal inferior exit, no allocator failure,
  no quota breach and the unchanged fixture's success.
- Compare allocating stacks and surviving IDs with the constructor trace.
  A matching size alone is not attribution.
- Preserve failed admissions. Do not increase deadlines or rescue the runtime.

## Excluded Admission

The first command mistakenly set `P2P_VPN_TOKIO_WORKER_THREADS`, which Tokio
does not consume. `TOKIO_WORKER_THREADS` was unset, so it used its default
worker count rather than the frozen two-worker configuration.

- Outer log: `/tmp/p2p-vpn-daemon-336-1.log`; fixture passed in 16.43 seconds.
- Artifacts: `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.c0ee0bae9a52c4dd`.
- Preserved as excluded evidence; not a matched two-worker result. Correct only
  the environment-variable name for the two planned repetitions.

## Results

Both corrected two-worker runs passed the original direct-traffic and graceful
shutdown assertions. Three trace checkpoints and a normal inferior exit were
captured in each run, with no quota breach or allocation failure.

| Measurement | First valid run | Repeat |
| --- | ---: | ---: |
| Duration under debugger | 16.28 seconds | 55.85 seconds |
| Selected allocations / frees before exit | 69 / 5 | 79 / 15 |
| Distinct allocating sites | 6 | 7 |
| Selected blocks before child | 0 | 0 |
| Selected blocks after runtime drop / after 100 ms | 65 / 65 | 65 / 65 |
| Selected blocks not freed before process exit | 64 | 64 |
| All-size post-child residual | 40801 bytes / 90 blocks | Same |

No deadline, quota or assertion changed. The slower repeat remains recorded;
these debugger-instrumented durations are not a latency comparison.

## Attribution

- Opaque IDs 1-64 come from Tokio signal-registry watch channels. No matching
  frees occur before exit; their stacks match the constructor-control owners.
- ID 65 comes from the thread-local RNG. It survives runtime drop and settling,
  then is freed before the inferior exits.
- All other selected-size allocations are absent at both shutdown checkpoints.
  Their sites include control-message channel nodes and libp2p ping futures.

This attributes the 65 x 336 = 21840 bytes in the direct-daemon residual through
actual allocation lifetimes. The remaining residual is 18961 bytes / 25 blocks;
neither those owners nor all live reconnect-growth increments are resolved here.

## Evidence

[Results](daemon-336-results.json) preserve the complete command, wrapper, GDB
script, stack sites, live sets, lifecycle counters and hashes of capture files.
The excluded run is recorded separately from the two accepted repetitions.

No production or test source changed. Both cached fixture executions passed;
workspace, Android and Nix builds were not repeated for documentation-only work.

## Status

336-byte daemon correspondence complete. Remaining-size and reconnect-growth
attribution, followed by the final acceptance audit, remain open.
