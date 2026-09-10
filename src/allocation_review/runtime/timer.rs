use crate::allocation_review::{delta, snapshot};
use std::time::{Duration, Instant};

#[test]
#[ignore = "allocation attribution: fresh process, one test thread"]
fn measure_global_timer_high_water() {
    const WIDTHS: [usize; 14] = [1, 4, 5, 8, 9, 16, 1, 16, 1, 16, 1, 16, 1, 16];
    let mut rows = Vec::with_capacity(WIDTHS.len());
    futures::executor::block_on(futures_timer::Delay::new(Duration::from_millis(1)));
    std::thread::sleep(Duration::from_millis(100));
    let baseline = snapshot();
    let started = Instant::now();
    for width in WIDTHS {
        let before = snapshot();
        futures::executor::block_on(futures::future::join_all(
            (0..width).map(|_| futures_timer::Delay::new(Duration::from_millis(100))),
        ));
        let completed = snapshot();
        std::thread::sleep(Duration::from_millis(100));
        let settled = snapshot();
        rows.push((width, before, completed, settled));
    }
    let elapsed_millis = started.elapsed().as_millis();
    // Serialize only after all allocation checkpoints have been captured.
    let rows: Vec<_> = rows
        .into_iter()
        .map(|(width, before, completed, settled)| {
            serde_json::json!({
                "concurrent_delays": width,
                "completed": delta(before, completed),
                "settled": delta(before, settled),
                "since_baseline": delta(baseline, settled),
            })
        })
        .collect();
    eprintln!(
        "global_timer_high_water_sample {}",
        serde_json::json!({
            "schema_version": 1,
            "elapsed_millis": elapsed_millis,
            "delay_millis": 100,
            "settle_millis": 100,
            "rows": rows,
        })
    );
}
