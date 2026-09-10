# Stopped-Process Inventory

## Frozen Calibration

| Property | Setting |
| --- | --- |
| Baseline | `9c6ffd09` |
| Binary | Cached namespace fixture SHA `ce9dd5655f18717a1f84f6bdfb848d879d5845f2fe426795182945856e85d486` |
| Observer | GDB all-stop memory read; no inferior function calls or allocator breakpoints |
| Layout | Exact x86-64 binary only; global `INSTRUMENTED_SYSTEM` |
| Inventory | 65537 signed 64-bit counters at offset zero |
| Aggregate fields | Oversized blocks/bytes and failures at `0x80008`, `0x80010`, `0x80018` |
| Stats | Six operation counters at `0x80020` through `0x80048` |
| Read size | `0x80050` bytes per observation |
| Bounds | 128 snapshots, 512 nonzero sizes, 256 KiB log |
| Calibration | Existing allocation/grow/shrink/free test; five stats checkpoints |
| Watchdog | 15 seconds plus two-second kill grace |

Getter and stats disassembly establish these binary-specific offsets. Never
reuse them for another executable without validating its layout. Read only
allocator metadata; do not print raw addresses or allocation payloads.

## Gates

- Require normal calibration exit, five snapshots and exact size/counter totals.
- Expected byte changes from baseline: 0, 1024, 2048, 512, 0; block changes:
  0, 1, 1, 1, 0. Failed-allocation and oversized counters must remain zero.
- Stop-the-world does not make an interrupted multi-step allocator update
  transactional. Mark inconsistent live samples unusable, not zero activity.

The intended next use is the existing one-cycle reconnect diagnostic, not a new
ten-cycle campaign. First establish that this readout matches existing counters.

## Calibration Result

Two fresh calibration processes passed. All ten observations match exact-size
totals and operation counters; each follows the expected 0/1024/2048/512/0-byte
and 0/1/1/1/0-block changes. Oversized and failed-allocation counters remain zero.

## Direct Admission

Before any live reconnect attachment, run two unchanged direct fixtures with
the existing namespace wrapper. Read the inventory at each of the three
`emit_sizes` entries and compare every row with the subsequently emitted native
size inventory. Require exact equality and normal fixture/inferior exits.

Use two workers, graceful shutdown and the existing 90-second fixture watchdog;
outer watchdog remains 100 seconds plus two-second kill grace, with 1 MiB logs.
All other snapshot and row bounds remain unchanged. No concurrent builds.

## Direct Result

Both unchanged fixtures passed. All six stopped-process inventories match every
row and the total bytes/blocks of the native inventories. All are coherent, with
zero failed allocations and normal inferior exits.

| Result | First run | Repeat |
| --- | ---: | ---: |
| Duration under debugger | 40.57 seconds | 15.43 seconds |
| Inventory checkpoints | 3 | 3 |
| Exact native-row matches | 3 | 3 |
| Missing/incoherent checkpoints | 0 | 0 |

No performance conclusion follows from these debugger-instrumented durations.
The unchanged graceful-shutdown, traffic, selected-path and cleanup gates passed.

## Evidence And Limits

[Results](frozen-inventory-results.json) preserve both observer scripts, complete
commands, calibration rows, paired native/stopped inventories and artifact hashes.

- Snapshot reads do not execute code inside the daemon or track each allocation.
  They identify size changes, not allocating-stack owners by themselves.
- A live process can be stopped between counter updates. Retain coherence flags;
  do not silently count invalid snapshots as stable or empty.
- Only documentation and external observer files changed. Workspace, Android and
  Nix builds were not repeated; cached calibration and direct fixtures passed.

## Status

Calibration and direct correspondence complete. Next: attach this reader to one
existing reconnect smoke fixture while retaining its original process identities
and resource gates. Live-growth attribution and the final audit remain open.
