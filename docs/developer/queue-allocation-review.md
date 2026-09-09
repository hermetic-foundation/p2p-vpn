# Queue Allocation Review

## Status

All four planned captures passed. This isolates queue
ownership and complements [live pressure measurements](sustained-pressure-results.md).
It does not attribute transport, native-library or whole-daemon RSS.

## Frozen Workload

| Setting | Value |
| --- | --- |
| Test | `allocation_review::queue::measure_packet_queue_allocations` |
| Allocator | Calibrated `allocation-review` library-test allocator |
| Peers | Four deterministic fixture IDs, no network connections |
| Packet size | 1028 requested payload bytes, matching S4's inner packet size |
| Packet-limited case | Four packets / 8192 bytes per peer |
| Byte-limited case | Sixteen packets / 4096 bytes per peer; three payloads fit |
| Sequence | Packets, bytes, bytes, packets; fresh process per capture |
| Cycles | Ten on the same `PeerQueues` owner |
| Cycle stages | Fill, reject one excess packet per peer, drain, refill one per peer, expire, retire peers |
| Expiry | Injected timestamp two seconds after enqueue; one-second TTL |
| Final stage | Drop owner before creating JSON report |
| Watchdog / log | 60 seconds / 256 KiB per capture |
| Builds | Complete before captures; one executable hash for all four |

Observer storage is reserved before accounting begins. Preserve the first cycle
and any reusable container capacity; peer retirement need not release the outer
hash-table allocation. Final owner teardown is measured separately.

## Checks

- Verify exact admitted packet/byte counts and rejection at the configured bound.
- Require empty payload queues after drain and expiry.
- Compare deallocated bytes with drained/expired payload totals.
- Attribute post-retirement capacity and check whether it grows across cycles.
- Inspect final live bytes and blocks; positive retention requires investigation.
- Keep raw results even when a logical assertion or watchdog fails.

## Command

```bash
P2P_VPN_REVIEW_QUEUE_LIMIT=packets timeout 60 "$TEST_BINARY" \
  allocation_review::queue::measure_packet_queue_allocations \
  --ignored --exact --test-threads=1 --nocapture
```

Use `bytes` for the other case. Preserve the `queue_allocation_sample` JSON,
full log, executable hash and source revision. No no-leak conclusion follows
from this test alone, even if every queue-owned allocation is released.

## Results

| Mode | Captures / Cycles | Drain Bytes Per Cycle | Expiry Bytes Per Cycle | Post-Retirement Storage | Final Live Bytes / Blocks |
| --- | --- | --- | --- | --- | --- |
| Packets | 2 / 20 | 16448 | 4112 | 1368 bytes, constant | 0 / 0 |
| Bytes | 2 / 20 | 12336 | 4112 | 1368 bytes, constant | 0 / 0 |

Drain deallocations equal four peers times admitted packets times 1028 bytes.
Expiry deallocations equal four refilled payloads. Peer retirement drops each
inner queue; the outer map/ready containers remain owned until final teardown.
Their measured 1368-byte residual did not increase across any cycle.

- [Raw samples and log hashes](queue-allocation-samples.json).
- Executable SHA-256: `65fea175f92a655b23d0693f4bc44bfd5687c324426fb5269267b45ec618624a`.
- Calibration passed on the same binary before all four captures.
- Each capture finished below the 60-second watchdog, with no overlapping builds.
- No production change is justified by these samples.

These results account for queue-owned payload and container allocations only.
They do not explain S4's full process RSS growth or measure simultaneous network
instances.

## Validation

| Gate | Result |
| --- | --- |
| Same-binary allocator calibration | Passed |
| Frozen queue captures | Four passed; 40 cycles |
| Locked offline workspace | 1497 passed, 37 ignored |
| Feature-enabled workspace Clippy | Required correctness, suspicious and performance groups passed |
| Rust formatting | Changed files passed |
| Cached evaluated Nix source check | Passed; new test source identical in Linux/Android inputs |
| Storage before builds | 8701760 KiB, below 10 GiB |

Logs use `/tmp/p2p-vpn-queue-allocation-*.log`. Source-check artifacts are in
`/tmp/p2p-vpn-queue-allocation-source.6pSZ0bFb`; assertions ran with cached Cargo
outside a Nix build sandbox. No Android cross-build or device test was run for
this test-only module. No existing Lean/TLA+/Alloy model files were found.
