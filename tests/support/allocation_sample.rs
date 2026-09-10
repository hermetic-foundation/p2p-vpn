use serde::Serialize;
use stats_alloc::{INSTRUMENTED_SYSTEM, Stats, StatsAlloc};
use std::{
    alloc::System,
    future::Future,
    io::Write as _,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[derive(Serialize)]
struct Sample {
    stage: &'static str,
    pid: u32,
    unix_millis: u128,
    elapsed_millis: u128,
    allocations: usize,
    deallocations: usize,
    reallocations: usize,
    bytes_allocated: usize,
    bytes_deallocated: usize,
    bytes_reallocated: isize,
    live_bytes: i128,
    live_blocks: i128,
}

impl Sample {
    fn new(stage: &'static str, started: Instant, stats: Stats) -> Self {
        Self {
            stage,
            pid: std::process::id(),
            unix_millis: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
            elapsed_millis: started.elapsed().as_millis(),
            allocations: stats.allocations,
            deallocations: stats.deallocations,
            reallocations: stats.reallocations,
            bytes_allocated: stats.bytes_allocated,
            bytes_deallocated: stats.bytes_deallocated,
            bytes_reallocated: stats.bytes_reallocated,
            live_bytes: stats.bytes_allocated as i128 - stats.bytes_deallocated as i128,
            live_blocks: stats.allocations as i128 - stats.deallocations as i128,
        }
    }
}

fn emit(stage: &'static str, started: Instant) {
    emit_record(b"allocation_sample ", stage, started);
}

fn emit_record(prefix: &[u8], stage: &'static str, started: Instant) {
    let sample = Sample::new(stage, started, ALLOCATOR.stats());
    let mut writer = std::io::stderr().lock();
    writer.write_all(prefix).unwrap();
    serde_json::to_writer(&mut writer, &sample).unwrap();
    writer.write_all(b"\n").unwrap();
}

pub async fn observe<F: Future>(future: F) -> F::Output {
    let started = Instant::now();
    let cadence = Duration::from_secs(5);
    let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + cadence, cadence);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tokio::pin!(future);
    emit("started", started);
    loop {
        tokio::select! {
            output = &mut future => {
                emit("returned", started);
                return output;
            }
            _ = interval.tick() => emit("periodic", started),
        }
    }
}

// The callback's entire runtime/configuration scope must end before it returns.
pub fn observe_child(run: impl FnOnce()) {
    let started = Instant::now();
    let prefix = b"allocation_lifecycle_sample ";
    emit_record(prefix, "before_child", started);
    #[cfg(feature = "allocation-sizes")]
    emit_sizes("before_child");
    run();
    emit_record(prefix, "child_returned", started);
    #[cfg(feature = "allocation-sizes")]
    emit_sizes("child_returned");
    std::thread::sleep(Duration::from_millis(100));
    emit_record(prefix, "child_settled", started);
    #[cfg(feature = "allocation-sizes")]
    emit_sizes("child_settled");
}

#[cfg(feature = "allocation-sizes")]
fn emit_sizes(stage: &'static str) {
    #[derive(Serialize)]
    struct Inventory<'a> {
        stage: &'static str,
        pid: u32,
        rows: &'a [(usize, isize)],
        oversized_blocks: isize,
        oversized_bytes: isize,
        total_blocks: i128,
        total_bytes: i128,
        failures: usize,
        coherent: bool,
        truncated: bool,
    }
    // Fixed scratch storage avoids allocating while inspecting the allocator.
    let mut rows = [(0, 0); 512];
    let mut count = 0;
    let mut truncated = false;
    let mut total_blocks = 0_i128;
    let mut total_bytes = 0_i128;
    let before = ALLOCATOR.stats();
    let sizes = ALLOCATOR.size_inventory();
    sizes.visit(|size, blocks| {
        total_blocks += blocks as i128;
        total_bytes += size as i128 * blocks as i128;
        if let Some(row) = rows.get_mut(count) {
            *row = (size, blocks);
            count += 1;
        } else {
            truncated = true;
        }
    });
    let (oversized_blocks, oversized_bytes) = sizes.oversized();
    total_blocks += oversized_blocks as i128;
    total_bytes += oversized_bytes as i128;
    let failures = sizes.failures();
    let after = ALLOCATOR.stats();
    let coherent = [before, after].into_iter().all(|stats| {
        stats.bytes_allocated as i128 - stats.bytes_deallocated as i128 == total_bytes
            && stats.allocations as i128 - stats.deallocations as i128 == total_blocks
    });
    let inventory = Inventory {
        stage,
        pid: std::process::id(),
        rows: &rows[..count],
        oversized_blocks,
        oversized_bytes,
        total_blocks,
        total_bytes,
        failures,
        coherent,
        truncated,
    };
    let mut writer = std::io::stderr().lock();
    writer.write_all(b"allocation_size_sample ").unwrap();
    serde_json::to_writer(&mut writer, &inventory).unwrap();
    writer.write_all(b"\n").unwrap();
    assert!(
        coherent && !truncated && failures == 0,
        "allocation size inventory is incomplete or inconsistent"
    );
}

#[test]
fn child_observation_runs_the_callback_once() {
    let calls = std::cell::Cell::new(0);
    observe_child(|| calls.set(calls.get() + 1));
    assert_eq!(calls.get(), 1);
}

#[test]
fn serialization_preserves_signed_live_counts_without_double_counting_reallocation() {
    let sample = Sample::new(
        "test",
        Instant::now(),
        Stats {
            allocations: 1,
            deallocations: 2,
            bytes_allocated: 100,
            bytes_deallocated: 120,
            bytes_reallocated: 30,
            ..Stats::default()
        },
    );
    let value = serde_json::to_value(sample).unwrap();
    assert_eq!(value["live_bytes"], -20);
    assert_eq!(value["live_blocks"], -1);
    assert_eq!(value["bytes_reallocated"], 30);
}

#[tokio::test]
async fn observation_returns_the_wrapped_future_output() {
    assert_eq!(observe(async { 17 }).await, 17);
}

#[test]
#[ignore = "allocation calibration: run alone in a fresh process with one test thread"]
fn allocator_calibration() {
    let before = ALLOCATOR.stats();
    let mut bytes = vec![0_u8; 1024];
    std::hint::black_box(&mut bytes);
    let allocated = ALLOCATOR.stats();
    bytes.reserve_exact(1024);
    std::hint::black_box(&mut bytes);
    let grown = ALLOCATOR.stats();
    bytes.truncate(512);
    bytes.shrink_to_fit();
    std::hint::black_box(&mut bytes);
    let shrunk = ALLOCATOR.stats();
    drop(bytes);
    let dropped = ALLOCATOR.stats();
    let live = |stats: Stats| stats.bytes_allocated as i128 - stats.bytes_deallocated as i128;
    assert_eq!(live(allocated) - live(before), 1024);
    assert_eq!(live(grown) - live(before), 2048);
    assert_eq!(live(shrunk) - live(before), 512);
    assert_eq!(live(dropped), live(before));
}

#[cfg(feature = "allocation-sizes")]
#[test]
#[ignore = "size calibration: run alone in a fresh process with one test thread"]
fn size_inventory_calibration() {
    let count = |wanted| {
        let mut result = 0;
        ALLOCATOR.size_inventory().visit(|size, blocks| {
            if size == wanted {
                result = blocks;
            }
        });
        result
    };
    let initial = (
        count(1024),
        count(2048),
        count(512),
        ALLOCATOR.size_inventory().oversized(),
    );
    let mut bytes = vec![0_u8; 1024];
    std::hint::black_box(&mut bytes);
    assert_eq!(count(1024), initial.0 + 1);
    bytes.reserve_exact(1024);
    assert_eq!(count(1024), initial.0);
    assert_eq!(count(2048), initial.1 + 1);
    bytes.truncate(512);
    bytes.shrink_to_fit();
    assert_eq!(count(2048), initial.1);
    assert_eq!(count(512), initial.2 + 1);
    drop(bytes);
    assert_eq!(count(512), initial.2);
    let mut large = vec![0_u8; 65_537];
    std::hint::black_box(&mut large);
    assert_eq!(
        ALLOCATOR.size_inventory().oversized(),
        (initial.3.0 + 1, initial.3.1 + 65_537)
    );
    drop(large);
    assert_eq!(ALLOCATOR.size_inventory().oversized(), initial.3);
    assert_eq!(ALLOCATOR.size_inventory().failures(), 0);
}
