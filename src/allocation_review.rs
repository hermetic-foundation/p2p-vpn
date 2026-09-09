use stats_alloc::{INSTRUMENTED_SYSTEM, Stats, StatsAlloc};
use std::alloc::System;

mod queue;

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

pub(crate) fn snapshot() -> Stats {
    ALLOCATOR.stats()
}

pub(crate) fn live_bytes(stats: Stats) -> i128 {
    // Reallocation growth/shrinkage is already included in these byte counters.
    i128::try_from(stats.bytes_allocated).unwrap()
        - i128::try_from(stats.bytes_deallocated).unwrap()
}

pub(crate) fn delta(before: Stats, after: Stats) -> serde_json::Value {
    let change = after - before;
    serde_json::json!({
        "allocations": change.allocations,
        "deallocations": change.deallocations,
        "reallocations": change.reallocations,
        "bytes_allocated": change.bytes_allocated,
        "bytes_deallocated": change.bytes_deallocated,
        "bytes_reallocated": change.bytes_reallocated,
        "live_bytes_delta": live_bytes(after) - live_bytes(before),
        "live_blocks_delta": i128::try_from(change.allocations).unwrap()
            - i128::try_from(change.deallocations).unwrap(),
    })
}

#[test]
#[ignore = "allocation calibration: run alone in a fresh process with --test-threads=1"]
fn allocator_counts_growth_shrinkage_and_drop_without_double_counting() {
    let before = snapshot();
    let mut bytes = Vec::<u8>::with_capacity(1024);
    bytes.resize(1024, 1);
    std::hint::black_box(&mut bytes);
    let allocated = snapshot();
    assert_eq!(live_bytes(allocated) - live_bytes(before), 1024);
    bytes.reserve_exact(1024);
    std::hint::black_box(&mut bytes);
    let grown = snapshot();
    assert_eq!(live_bytes(grown) - live_bytes(before), 2048);
    bytes.truncate(512);
    bytes.shrink_to_fit();
    std::hint::black_box(&mut bytes);
    let shrunk = snapshot();
    assert_eq!(live_bytes(shrunk) - live_bytes(before), 512);
    drop(bytes);
    let dropped = snapshot();
    assert_eq!(live_bytes(dropped), live_bytes(before));
    assert_eq!(dropped.allocations - before.allocations, 1);
    assert_eq!(dropped.deallocations - before.deallocations, 1);
    assert_eq!(dropped.reallocations - before.reallocations, 2);
}
