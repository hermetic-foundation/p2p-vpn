# Native-Filtered Allocation Review

## Result

The one-cycle daemon diagnostic passes the original recovery and resource gates.
It identifies the AutoNAT allocations matching a 1,512-byte/four-block step:
two 704-byte handler deques and two 52-byte request-ID tables.

This explains the combined outbound/inbound step in this capture. It does not
retroactively assign every historical step or prove all retained growth bounded.
The [manifest](connected-allocation-native-results.json) preserves commands and hashes.

## Observer Validation

GDB now applies native size conditions before executing Python command lists.
The prior Python `stop()` callback checked every allocation and deallocation.
The new capture retains the same one-second status-query deadline.

| Isolated filter | Allocations / matching frees | Result |
| --- | ---: | --- |
| 52 bytes | 7 / 7 | Passed |
| 52 or 1,408 bytes | 7 / 7 | Passed; no 1,408-byte allocation |
| 52 or 704 bytes | 9 / 9 | Passed; two 704-byte allocations |

Each isolated test completed ten requests and both sets of ten refusal events.
The 704-byte blocks were released on connection teardown. No allocator or
dependency was replaced, and no protocol behavior was suppressed.

## Daemon Capture

| Property | Result |
| --- | --- |
| Fixture | Existing one-cycle churn smoke; two isolated peers; no Internet route |
| Duration / exit | 189.40 seconds / zero |
| Watchdogs | Original 240-second internal / 250-second external |
| Recovery and final traffic | Strict 5/5 each direction; original process identities |
| Outage / recovery / settle runtime rows | 12 / 16 / 24; all complete, no errors |
| Maximum status-query time by phase | 126 / 118 / 117 ms, rounded |
| Traced allocations / frees | 1,004 / 992 |
| Distinct stack keys / live snapshots | 44 / 29 |
| Trace bounds | 1 MiB; 4,096 allocations; 64 sites; 24 frames/site |
| Temporary storage before run | 8,262,100 KiB, below 10 GiB |

The earlier observer-overhead failure remains documented separately in
[connected allocation sites](connected-allocation-sites.md). This is not a matched
CPU comparison; debugger timing must not be treated as production performance.

## Attribution

Times are seconds since debugger attachment. The captured process is node A.

| ID | Time | Requested bytes | Owner |
| --- | ---: | ---: | --- |
| 495 | 63.652 | 52 | AutoNAT inbound pending-request set |
| 496 | 63.890 | 52 | AutoNAT outbound pending-request set |
| 497 | 63.896 | 704 | Handler `pending_outbound` deque |
| 498 | 63.897 | 704 | Handler `requested_outbound` deque |

The outbound set plus two deques totals 1,460 bytes across three blocks.
The inbound table contributes another 52 bytes and one block.

Daemon snapshots at 100.087 and 110.086 seconds show a net increase from
2,533,180 to 2,534,692 bytes and from 1,539 to 1,543 blocks. The intermediate
105.023-second snapshot also contains transient allocations and is retained.

Locked request-response handler source places the deques on each connection.
One receives outgoing requests; the other holds requests awaiting negotiation.
Normal pops retain capacity, and the pending queue shrinks only above its threshold.

The isolated trace observes both deque frees during teardown. The full daemon's
final sampled set retains these blocks until fixture termination; that SIGKILL
is not evidence of graceful runtime release.

## Reproduction And Limits

1. Verify the manifest's cached tool and binary hashes; check the storage cap.
2. Launch a fresh owned fixture using the referenced command.
3. Verify node A's executable, role, start identity and both PID mappings.
4. Replace historical PIDs in the recorded debugger command only after verification.
5. Keep all watchdogs, quotas and traffic assertions unchanged; retain failures.

- The trace covers selected sizes after attachment, not the whole heap.
- Untested reallocation branches must not be treated as validated profiler functionality.
- No physical machines, services, personal flake or public-network campaign was touched.
- Full reconnect-growth attribution, packet-pressure ownership and the final audit remain open.
