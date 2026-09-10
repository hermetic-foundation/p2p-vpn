# Smaller Retained Allocations

## Frozen Diagnostic

| Setting | Value |
| --- | --- |
| Baseline | `ea3d9741` |
| Selected sizes | 13, 14, 70, 112, 352, 368, 1024 bytes |
| Inventory residual in selected sizes | 1953 bytes / 7 blocks |
| Fixture and observer | Same direct two-node graceful-shutdown fixture and native GDB wrapper as [large trace](daemon-large-review.md) |
| Binary SHA-256 | `ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486` |
| Workers | `TOKIO_WORKER_THREADS=2` |
| Checkpoints | Three existing size-inventory emissions |
| Isolation | Fresh user/PID/network namespaces; no Internet route |
| Quotas | 4096 allocations, 64 sites, 24 frames/site, 240 characters/frame |
| Watchdogs / logs | Original 90-second fixture; 100-second outer plus two-second kill grace; 1 MiB/file |
| Repetition | Two fresh fixtures, no concurrent builds |

Change only selected sizes. Follow reallocations and frees. Require unchanged
traffic/path/shutdown/cleanup gates, all checkpoints, normal inferior exit and
no quota breach or allocator failure. Preserve failures without deadline changes.

## Interpretation

Attribute retained blocks by allocating stack and live ID. These traces do not
establish performance or whole-heap boundedness. The wrapper's namespace PID
is GDB, not the inferior, so do not use it for process-resource comparisons.

## Failed Admission

The seven-size trace exceeded its 64-site quota during startup. It captured only
one checkpoint and no normal inferior exit. The fixture failed in 32.61 seconds;
the trace also reports out-of-scope return breakpoints. No ownership conclusion
uses this incomplete capture.

- Outer log: `/tmp/p2p-vpn-daemon-small-1.log`.
- Artifacts: `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.2f9ac2f0600e04e7`.

## Split Admission

Before retrying, split the call-site-heavy 70- and 112-byte sizes from the other
five sizes. The next two runs select 13, 14, 352, 368 and 1024 bytes, covering
1771 bytes / five residual blocks. All quotas, watchdogs and validity gates stay
unchanged; out-of-scope returns still invalidate a trace.

## Return-Tracking Failure

The five-size admission reached all checkpoints and the daemon exited normally,
but two return breakpoints went out of scope. GDB correctly rejected the trace;
the outer fixture failed its child-exit gate in 55.95 seconds.

- Outer log: `/tmp/p2p-vpn-daemon-small-2.log`.
- Artifacts: `/tmp/p2p-vpn-tun_namespace_ping_crosses_two_node_overlay-1.1b6fa49c7bb5398f`.
- Do not accept the partial owner list as complete attribution.

The next single diagnostic adds opaque allocation ID, size and site to the
existing out-of-scope error. It keeps the five sizes and all quotas/deadlines.
Its purpose is to locate missed returns, not waive that validity gate.

## Status

The third diagnostic also failed: missed returns include resize events and it
eventually reported a duplicate live address. Preserve it as observer failure,
not evidence of allocator corruption in p2p-vpn.

Before another daemon run, run the existing allocator calibration under the same
five-size observer, with a 15-second watchdog and 256 KiB log cap. It exercises
zeroed allocation, growth, shrink and free in a single test thread. No source
or validity-gate change is needed; this control expects zero daemon checkpoints.

Three invalid daemon admissions preserved. Observer return tracking requires
diagnosis before proceeding with matched ownership repetitions.

## Calibration Result

The existing calibration passed under the same observer in 0.02 seconds. Its
single allocation was followed through growth and shrink, then freed. The final
live set and failure list are empty. No test source or allocator was changed.

This does not reproduce the daemon's missed returns or prove concurrency is the
cause. It narrows the next investigation to observer behavior under daemon
conditions rather than the basic single-thread allocation lifecycle.

## Evidence And Next Step

[Results](daemon-small-results.json) preserve the three invalid captures, exact
observer variants, calibration result and raw-artifact hashes. None contributes
to the accepted retained-byte attribution totals.

- Investigate return tracking before another small-block daemon admission.
- Keep live reconnect growth and Crossbeam reclamation review in scope;
  a partial small-allocation trace must not substitute for either.
- No production code changed. Workspace, Android and Nix builds were not rerun
  for these documentation-only diagnostics.
