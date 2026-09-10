# Connected Allocation Sites

## Status

The full-daemon debugger trace identifies connection-scoped 52-byte request sets.
The resource capture failed during settling. These are partial ownership findings,
not a passing sustained-resource result or proof that all growth is bounded.

## Captures

| Run | Workload result | Debugger coverage |
| --- | --- | --- |
| Smoke 1 | Passed in 189.45 seconds | Initial trace hit its quota; separate late trace covered settling |
| Smoke 2 | Failed in 185.80 seconds | Same-namespace trace covered pre-outage through settling; cleanup killed debugger |

Both used the existing one-cycle profile, two workers, cached allocation-review
executable and unchanged 240/250-second watchdogs. No builds, downloads, physical
hosts, public routes, runtime rescue or production changes were involved.

The [manifest](connected-allocation-sites.json) records commands, hashes,
limits, selected stacks and lifetime events. Raw logs and namespace reports
remain in the recorded local directories, including failed evidence.

## Observed Owners

Times below are relative to debugger attachment in smoke 2, not daemon startup.
The debugger tracked requested allocations of exactly 52 bytes in node A.

| Allocation | Owner | Observed lifetime |
| --- | --- | --- |
| 25 | AutoNAT inbound request-ID set | Allocated at 3.513 s; freed at 63.851 s |
| 26 | AutoNAT outbound request-ID set | Allocated at 3.754 s; freed at 63.851 s |
| 612, 613 | Replacement AutoNAT request-ID sets | Allocated at 93.568/93.757 s; present in final sampled set |
| 413 | Kademlia maintenance query-ID set | First-use capacity remained in sampled set |
| 417 | DCUtR connection handling | Present in final sampled set |
| 418, 419, 424, 434 | Control/service request-ID sets | Present in final sampled set |
| 443 | Kademlia address handling | Present in final sampled set |
| 453 | Packet request-ID set | Present in final sampled set |

The trace recorded 1,113 allocations, 1,103 matching frees and 43 distinct
stack keys. Thirty-four snapshots were captured. Many allocations were temporary
peer-ID copies or formatted metric strings, rather than retained request state.

The final sampled set contained ten blocks. This is **not** ten blocks of net
heap growth: allocations made before attachment, and their frees, are outside
the trace. Namespace cleanup does not demonstrate graceful release of these blocks.

## Source Cross-Check

Locked `libp2p-request-response` 0.29.0 stores pending inbound/outbound IDs in
per-connection hash sets. Completed requests remove IDs without shrinking capacity.
Connection closure removes the connection and consumes those sets.

The matched frees for IDs 25/26 fall within the daemon's connection-retirement
interval. Later requests allocate replacement tables. This supports retained
connection capacity for these objects, not an ever-growing history of requests.

It does not identify every historical 52-byte step or the 1,460-byte steps in
the ten-cycle captures. The Kademlia maintenance set also allocates 52 bytes;
allocation size alone is not a unique owner signature.

## Failed Gate

| Settling observation | Result |
| --- | --- |
| Node A status queries scheduled at 40, 45, 50, 55 seconds | Each exceeded the existing one-second timeout |
| Node B status query errors | Zero |
| Recovery traffic before settling | Strict 5/5 in both directions |
| Runtime counter completeness | Failed; report retained |
| Final post-settle traffic | Not reached |
| Debugger | Exit 137 during namespace cleanup; no complete end summary |

Node A's status-query duration increased from 0.65 seconds to the timeout during
settling. Breakpoint instrumentation is intrusive; this capture cannot establish
production timing behavior. Do not relax the query deadline to accept this result.

## Reproduction And Safety

1. Verify cached artifact hashes and the 10 GiB temporary-storage cap.
2. Launch a new owned fixture using the manifest's fixture command.
3. Verify node A's executable, start identity, role and namespace-local PID.
4. Adapt the recorded trace command to those freshly verified PIDs only.
5. Keep the recorded watchdog, file-size, allocation and site limits unchanged.
6. Retain failure logs; verify owned processes exit before another capture.

The recorded PIDs are historical. Never attach using them without revalidation.
The corrected namespace entry includes user, network, mount and PID namespaces
with preserved credentials; no sudo or global ptrace-policy changes were needed.

## Next Step

- Reduce callback work and validate observer overhead before another full trace.
- Reproduce the connection-capacity finding with complete runtime-counter coverage.
- Attribute the remaining larger reconnect allocations and packet-pressure retention.
- Keep the sustained-resource goal open until those findings and the final audit are complete.
