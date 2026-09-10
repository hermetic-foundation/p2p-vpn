use std::{
    fmt,
    sync::atomic::{AtomicIsize, AtomicUsize, Ordering},
};

/// Opt-in, fixed-storage inventory. It performs no allocation, locking or stack tracing.
pub struct SizeInventory {
    counts: [AtomicIsize; Self::MAX_EXACT_SIZE + 1],
    oversized_blocks: AtomicIsize,
    oversized_bytes: AtomicIsize,
    failures: AtomicUsize,
}

impl SizeInventory {
    /// Largest size with its own exact-size counter. Larger allocations are aggregated.
    pub const MAX_EXACT_SIZE: usize = 65_536;

    pub(crate) const fn new() -> Self {
        Self {
            counts: [const { AtomicIsize::new(0) }; Self::MAX_EXACT_SIZE + 1],
            oversized_blocks: AtomicIsize::new(0),
            oversized_bytes: AtomicIsize::new(0),
            failures: AtomicUsize::new(0),
        }
    }

    /// Visit nonzero live block counts by exact requested size, without allocating.
    /// Concurrent activity can make this view inconsistent; consumers must verify totals.
    pub fn visit(&self, mut visit: impl FnMut(usize, isize)) {
        for (size, count) in self.counts.iter().enumerate() {
            let count = count.load(Ordering::SeqCst);
            if count != 0 {
                visit(size, count);
            }
        }
    }

    /// Live blocks and requested bytes above the exact-size range.
    pub fn oversized(&self) -> (isize, isize) {
        (
            self.oversized_blocks.load(Ordering::SeqCst),
            self.oversized_bytes.load(Ordering::SeqCst),
        )
    }

    /// Failed allocations/reallocations. The original operation counters count requests;
    /// this successful-allocation inventory must not be equated with them after failure.
    pub fn failures(&self) -> usize {
        self.failures.load(Ordering::SeqCst)
    }

    fn record(&self, size: usize, change: isize) {
        if let Some(count) = self.counts.get(size) {
            count.fetch_add(change, Ordering::SeqCst);
        } else {
            self.oversized_blocks.fetch_add(change, Ordering::SeqCst);
            self.oversized_bytes
                .fetch_add((size as isize) * change, Ordering::SeqCst);
        }
    }

    pub(crate) fn allocated(&self, size: usize, success: bool) {
        if success {
            self.record(size, 1);
        } else {
            self.failures.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub(crate) fn released(&self, size: usize) {
        self.record(size, -1);
    }

    pub(crate) fn resized(&self, old: usize, new: usize, success: bool) {
        if success {
            self.released(old);
            self.record(new, 1);
        } else {
            self.failures.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Default for SizeInventory {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SizeInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SizeInventory")
            .field("oversized", &self.oversized())
            .field("failures", &self.failures())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_large_resize_and_failure_accounting() {
        let sizes = SizeInventory::new();
        sizes.allocated(1024, true);
        sizes.resized(1024, 2048, false);
        let mut entries = Vec::new();
        sizes.visit(|size, count| entries.push((size, count)));
        assert_eq!(entries, [(1024, 1)]);
        assert_eq!(sizes.failures(), 1);
        sizes.resized(1024, 65_537, true);
        assert_eq!(sizes.oversized(), (1, 65_537));
        sizes.resized(65_537, 512, true);
        assert_eq!(sizes.oversized(), (0, 0));
        sizes.released(512);
        sizes.allocated(1, false);
        sizes.visit(|_, _| panic!("all successful allocations were released"));
        assert_eq!(sizes.failures(), 2);
    }
}
