# Retained 336-Byte Allocations

## Question

Four direct-daemon inventories retain 65 blocks of 336 bytes. Trace allocation
lifetimes in the existing ordered initialization diagnostic before attempting
any attribution of the full daemon residual.

## Frozen Trace

| Setting | Value |
| --- | --- |
| Baseline | `6fa5026b` |
| Test | `allocation_review::runtime::initialization::measure_runtime_initialization_components` |
| Executable | `/tmp/p2p-vpn-review-target/debug/deps/p2p_vpn-99d2efd11d633eff` |
| SHA-256 | `d7a100d7ca985cfcc1e7794e1fd82ca6b0894999705e6ea11035daeb5436f78c` |
| Workload | Existing ten ordered Tokio/Noise/QUIC-config/node construction-drop cycles |
| Isolation | Fresh user/network namespace; only loopback enabled |
| Trace start | Test entry, before initialization |
| Filter | Native GDB conditions on 336-byte alloc/alloc-zeroed/free/realloc events |
| Tracking | Opaque IDs; follow reallocations and matching frees; retain old owner on failed resize |
| Snapshots | Existing 122 allocator checkpoints; no production instrumentation added |
| Limits | 1024 allocations, 32 stack sites, 24 frames/site, 240 characters/frame |
| Watchdog / log | 30 seconds plus two-second kill grace; 256 KiB |
| Repetition | Two fresh processes, same command and executable |

No debugger arguments or allocation addresses are printed. Failed allocations,
trace quotas, missing snapshots, nonzero inferior exits or truncated logs invalidate
the trace. No build or other measurement overlaps it.

## Interpretation

Require matching allocation/free lifetimes and allocating stacks, not size
coincidence. Process-global retention in this constructor control is not yet
proof of corresponding owners in a full VPN daemon.

Tokio's signal registry is a source-derived candidate. It may explain only part
of the 65 blocks; retain additional sites and negative evidence explicitly.

## Results

Both fresh processes passed the unchanged initialization test. Each trace contains
122 checkpoints, two allocating sites, 65 successful tracked allocations and one
matching free. Neither exceeded a quota or reported a failed allocation.

| Observation | First run | Repeat |
| --- | --- | --- |
| Test duration under debugger | 6.94 seconds | 6.93 seconds |
| Log bytes / 262144 limit | 143339 | 143340 |
| Checkpoints 1-3 | No tracked allocations | Same |
| Checkpoints 4-12 | 64 signal-registry blocks | Same |
| Checkpoints 13-122 | Registry plus one RNG block | Same |
| Inferior exit | Normal; RNG freed, 64 registry blocks live | Same |

### Owners

| Site | Bytes | Lifetime evidence |
| --- | --- | --- |
| Tokio signal-registry watch channels | 64 x 336 = 21504 | Created in first Tokio stage; identical IDs survive all ten cycles, with no frees before exit |
| Thread-local RNG initialized through Quinn server configuration | 336 | Created in first node stage; survives all checkpoints, freed before process exit |

The first stack passes through `EventInfo::default`, `OsStorage::default` and
`globals_init`. Tokio 1.53.1 stores this registry in `OnceLock<Globals>` and
constructs its Linux signal entries using `SIGRTMAX()`.

The second stack passes through `rand::rngs::thread::THREAD_RNG_KEY`,
`quinn_proto::config::ServerConfig::with_crypto` and `build_node`. Its opaque
allocation ID is absent from the final live set; the free count is exactly one.

### Limits

- This control demonstrates bounded process-global and thread-local ownership.
  It does not prove the corresponding full-daemon allocation identities.
- The matching 21840-byte size total is supporting evidence, not a substitute
  for tracing the daemon through its own shutdown checkpoints.
- Debugger durations are not CPU or throughput measurements. Stack frames and
  names retain the frozen truncation limits; owner-identifying frames are present.
- No production code changed. Workspace, Android and Nix builds were not rerun
  for this documentation-only attribution; both cached diagnostic tests passed.

## Reproduction

[Machine-readable results](retained-336-results.json) retain the complete command,
allocating stacks, checkpoint counts, final live IDs and source/log hashes.
Redirect the command to a fresh log path for each repetition.

## Status

Constructor attribution complete. Full-daemon correspondence and remaining
reconnect-growth ownership remain open; no runtime workaround is justified here.
