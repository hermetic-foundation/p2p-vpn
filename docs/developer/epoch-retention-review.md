# Epoch Reclamation Control

## Frozen Workload

| Setting | Value |
| --- | --- |
| Baseline | `a58da953` |
| Source | `tests/diagnostics/epoch_retention.rs` |
| Dependencies | Cached Crossbeam epoch 0.9.20 and instrumented stats allocator rlibs |
| Modes | Private collector drop; global collector idle; global collector ordinary pin activity |
| Cycles | Ten; three new handles and three deferred 64-byte payloads per cycle |
| Observation | After handle retirement, after 100 ms idle, after optional activity |
| Activity | Exactly 1024 ordinary pin/unpin operations; no explicit flush |
| Repeats | Two fresh processes per mode |
| Limits | 15-second watchdog plus two-second grace; 256 KiB log per process |
| Isolation | Standalone control with no networking or VPN runtime; no overlapping builds |

Both global modes create one persistent handle before the baseline. Measured
rows use fixed stack storage; formatting occurs after the final snapshot.
Compare requested bytes/blocks and independently counted payload destructors.

## Gates

- All private-collector cycles must release all payloads and restore baseline
  requested bytes/blocks.
- Idle alone must not change counters or invoke payload destructors.
- Active cycles must reclaim all retired payloads through ordinary pin activity.
- Record global-mode storage trends; do not impose a plateau inferred from data.

This control tests dependency reclamation mechanics. It is not a replacement for
daemon growth measurements and does not authorize adding production flushing.

## Status

The initial six ten-cycle runs passed their gates. Private collectors released
everything. Global idle storage plateaued after cycle two, but active-mode
bookkeeping grew while every payload was reclaimed. Retain this distinction.

## Bag-Capacity Follow-Up

Source inspection shows a 64-callback local bag. Removing retired local handles
and queue nodes can defer their destruction into the collecting handle's bag;
pinning collects global bags but does not flush a partially filled local bag.

Before execution, add a fixed 32-cycle active-mode follow-up, repeated twice.
This crosses several 64-callback bag thresholds for three retired handles per
cycle. Retain the original 15-second watchdog, 100 ms idle checks and 1024 pins.
Do not add flushing or change payload-reclamation assertions.

The helper accepts only 10 or 32 cycles. The ten-cycle raw evidence and original
source/binary remain recorded; the longer control is a new source-motivated
workload, not a deadline extension or a relaxed failed gate.

## Results

All six initial ten-cycle runs and both 32-cycle active runs passed. Repetitions
are byte-for-byte identical. The final helper also reproduces each ten-cycle
mode's original output exactly.

| Mode | Payload release | Requested-storage result |
| --- | --- | --- |
| Private collector | All three per cycle | Exact baseline bytes/blocks after every drop |
| Global, idle between cycles | One cycle behind after cycle two | 26448 bytes / 15 blocks from cycles 2-10 |
| Global, active, first ten cycles | All three per cycle | Bookkeeping grows by 13128 bytes / six blocks per cycle |
| Global, active, 32 cycles | All 96 payloads per run | Storage falls at cycles 11, 22 and 32 |

Every 100 ms idle observation left byte/block/destructor counters unchanged.
The idle-mode plateau includes collection triggered by the next cycle's new
handle activity; sleeping alone did not collect anything.

### Bag Turnover

| Point | Requested bytes / blocks over baseline |
| --- | ---: |
| Cycle 10 | 131280 / 60 |
| Cycle 11 | 6216 / 3 |
| Cycle 21, observed maximum | 137496 / 63 |
| Cycle 22 | 12896 / 6 |
| Cycle 31 | 131048 / 60 |
| Cycle 32 | 6216 / 3 |

Three local registrations and three sealed-bag queue nodes total 13128 bytes,
matching the per-cycle bookkeeping increment. Their destruction can itself be
deferred into the persistent collecting handle's 64-callback bag.

The source-derived bag threshold and repeated storage drops support bounded
batching in this fixed control. Ten apparently linear cycles were too short to
observe the first turnover. This is not an arbitrary-thread-count memory bound.

## Validation And Reproduction

- The helper uses only safe dependency APIs, fixed-size measurement storage and
  bounded loops. No production flush, cache bypass or allocator patch was added.
- Changed-file rustfmt and required Clippy groups pass. All eleven bounded
  executions pass; original and final ten-cycle outputs match exactly.
- This standalone diagnostic is compiled explicitly, not auto-discovered by
  Cargo. No dependency lock, production source or runtime interface changed.
- Workspace, Android and Nix package builds were not rerun for the standalone
  control. Cached rlibs are recorded; this is not a hermetic package-build claim.

[Results](epoch-retention-results.json) preserve the build/run commands, both
source versions, binary/dependency/log hashes and all measured rows. Use `private`,
`idle` or `active` for `MODE`, and only `10` or `32` for `CYCLES`.

## Remaining Scope

Crossbeam batching is now demonstrated independently of RSS and idle waits.
Full-daemon reconnect increments still need correspondence checks; the remaining
734 bytes of direct residual and final acceptance audit remain open.
