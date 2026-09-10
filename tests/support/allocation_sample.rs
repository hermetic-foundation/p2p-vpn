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
    let sample = Sample::new(stage, started, ALLOCATOR.stats());
    let mut writer = std::io::stderr().lock();
    writer.write_all(b"allocation_sample ").unwrap();
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
