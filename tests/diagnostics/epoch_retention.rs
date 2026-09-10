use crossbeam_epoch::{Collector, default_collector};
use stats_alloc::{INSTRUMENTED_SYSTEM, StatsAlloc};
use std::{
    alloc::System,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;
static RELEASED: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Default)]
struct Sample {
    bytes: i128,
    blocks: i128,
    released: usize,
}

fn sample() -> Sample {
    let stats = ALLOCATOR.stats();
    Sample {
        bytes: stats.bytes_allocated as i128 - stats.bytes_deallocated as i128,
        blocks: stats.allocations as i128 - stats.deallocations as i128,
        released: RELEASED.load(Ordering::SeqCst),
    }
}

fn retire_three(collector: &Collector) {
    let handles: [_; 3] = std::array::from_fn(|_| collector.register());
    for handle in &handles {
        let payload = Box::new([17_u8; 64]);
        std::hint::black_box(&payload);
        handle.pin().defer(move || {
            drop(payload);
            RELEASED.fetch_add(1, Ordering::SeqCst);
        });
    }
    drop(handles);
}

fn main() {
    let mode = std::env::args().nth(1).expect("private, idle or active");
    assert!(matches!(mode.as_str(), "private" | "idle" | "active"));
    let cycles = std::env::args()
        .nth(2)
        .map(|value| value.parse::<usize>().expect("cycle count"))
        .unwrap_or(10);
    assert!(matches!(cycles, 10 | 32), "expected 10 or 32 cycles");
    // The persistent handle is outside the measured cycles in both global modes.
    let anchor = (mode != "private").then(|| default_collector().register());
    let baseline = sample();
    let mut rows = [(Sample::default(), Sample::default(), Sample::default()); 32];
    for (index, row) in rows.iter_mut().take(cycles).enumerate() {
        if mode == "private" {
            let collector = Collector::new();
            retire_three(&collector);
            drop(collector);
        } else {
            retire_three(default_collector());
        }
        let retired = sample();
        std::thread::sleep(Duration::from_millis(100));
        let idle = sample();
        if mode == "active" {
            for _ in 0..1024 {
                drop(anchor.as_ref().unwrap().pin());
            }
        }
        let active = sample();
        *row = (retired, idle, active);
        assert_eq!(retired.bytes, idle.bytes, "idle changed requested bytes");
        assert_eq!(retired.blocks, idle.blocks, "idle changed requested blocks");
        assert_eq!(retired.released, idle.released, "idle reclaimed a payload");
        if mode == "private" {
            assert_eq!(active.bytes, baseline.bytes, "collector retained bytes");
            assert_eq!(active.blocks, baseline.blocks, "collector retained blocks");
            assert_eq!(active.released, (index + 1) * 3);
        }
        if mode == "active" {
            assert_eq!(
                active.released,
                (index + 1) * 3,
                "pins did not reclaim payloads"
            );
        }
    }
    // Formatting starts only after all measured snapshots.
    for (index, row) in rows.iter().take(cycles).enumerate() {
        for (stage, value) in [("retired", row.0), ("idle", row.1), ("active", row.2)] {
            println!(
                "{{\"mode\":\"{}\",\"cycle\":{},\"stage\":\"{}\",\"bytes\":{},\"blocks\":{},\"released\":{}}}",
                mode,
                index + 1,
                stage,
                value.bytes - baseline.bytes,
                value.blocks - baseline.blocks,
                value.released,
            );
        }
    }
}
