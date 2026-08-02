use std::sync::atomic::{AtomicU64, Ordering};

use crate::queue::QueueStats;

#[derive(Debug, Default)]
pub struct RuntimeMetrics {
    tun_read_packets: AtomicU64,
    tun_read_bytes: AtomicU64,
    tun_write_packets: AtomicU64,
    tun_write_bytes: AtomicU64,
    outbound_sent_packets: AtomicU64,
    inbound_accepted_packets: AtomicU64,
    outbound_dropped_packets: AtomicU64,
    inbound_dropped_packets: AtomicU64,
    outbound_failures: AtomicU64,
    inbound_failures: AtomicU64,
}

impl RuntimeMetrics {
    pub fn record_tun_read(&self, bytes: usize) {
        self.tun_read_packets.fetch_add(1, Ordering::Relaxed);
        self.tun_read_bytes
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn record_tun_write(&self, bytes: usize) {
        self.tun_write_packets.fetch_add(1, Ordering::Relaxed);
        self.tun_write_bytes
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn record_outbound_sent(&self) {
        self.outbound_sent_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inbound_accepted(&self) {
        self.inbound_accepted_packets
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_drop(&self) {
        self.outbound_dropped_packets
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inbound_drop(&self) {
        self.inbound_dropped_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_failure(&self) {
        self.outbound_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inbound_failure(&self) {
        self.inbound_failures.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self, queue: QueueStats) -> RuntimeSnapshot {
        RuntimeSnapshot {
            tun_read_packets: self.tun_read_packets.load(Ordering::Relaxed),
            tun_read_bytes: self.tun_read_bytes.load(Ordering::Relaxed),
            tun_write_packets: self.tun_write_packets.load(Ordering::Relaxed),
            tun_write_bytes: self.tun_write_bytes.load(Ordering::Relaxed),
            outbound_sent_packets: self.outbound_sent_packets.load(Ordering::Relaxed),
            inbound_accepted_packets: self.inbound_accepted_packets.load(Ordering::Relaxed),
            outbound_dropped_packets: self.outbound_dropped_packets.load(Ordering::Relaxed),
            inbound_dropped_packets: self.inbound_dropped_packets.load(Ordering::Relaxed),
            outbound_failures: self.outbound_failures.load(Ordering::Relaxed),
            inbound_failures: self.inbound_failures.load(Ordering::Relaxed),
            queue,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub tun_read_packets: u64,
    pub tun_read_bytes: u64,
    pub tun_write_packets: u64,
    pub tun_write_bytes: u64,
    pub outbound_sent_packets: u64,
    pub inbound_accepted_packets: u64,
    pub outbound_dropped_packets: u64,
    pub inbound_dropped_packets: u64,
    pub outbound_failures: u64,
    pub inbound_failures: u64,
    pub queue: QueueStats,
}

impl RuntimeSnapshot {
    #[must_use]
    pub fn lines(self) -> Vec<String> {
        vec![
            format!("tun_read_packets {}", self.tun_read_packets),
            format!("tun_read_bytes {}", self.tun_read_bytes),
            format!("tun_write_packets {}", self.tun_write_packets),
            format!("tun_write_bytes {}", self.tun_write_bytes),
            format!("outbound_sent_packets {}", self.outbound_sent_packets),
            format!("inbound_accepted_packets {}", self.inbound_accepted_packets),
            format!("outbound_dropped_packets {}", self.outbound_dropped_packets),
            format!("inbound_dropped_packets {}", self.inbound_dropped_packets),
            format!("outbound_failures {}", self.outbound_failures),
            format!("inbound_failures {}", self.inbound_failures),
            format!("queue_queued_packets {}", self.queue.queued_packets),
            format!("queue_queued_bytes {}", self.queue.queued_bytes),
            format!("queue_dropped_packets {}", self.queue.dropped_packets),
            format!("queue_dropped_bytes {}", self.queue.dropped_bytes),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_snapshot_reports_runtime_and_queue_counters() {
        let metrics = RuntimeMetrics::default();
        metrics.record_tun_read(20);
        metrics.record_tun_write(40);
        metrics.record_outbound_sent();
        metrics.record_inbound_accepted();
        metrics.record_outbound_drop();
        metrics.record_inbound_drop();
        metrics.record_outbound_failure();
        metrics.record_inbound_failure();

        let snapshot = metrics.snapshot(QueueStats {
            queued_packets: 2,
            queued_bytes: 80,
            dropped_packets: 3,
            dropped_bytes: 120,
        });

        assert_eq!(snapshot.tun_read_packets, 1);
        assert_eq!(snapshot.tun_read_bytes, 20);
        assert_eq!(snapshot.tun_write_packets, 1);
        assert_eq!(snapshot.tun_write_bytes, 40);
        assert_eq!(snapshot.outbound_sent_packets, 1);
        assert_eq!(snapshot.inbound_accepted_packets, 1);
        assert_eq!(snapshot.outbound_dropped_packets, 1);
        assert_eq!(snapshot.inbound_dropped_packets, 1);
        assert_eq!(snapshot.outbound_failures, 1);
        assert_eq!(snapshot.inbound_failures, 1);
        assert!(
            snapshot
                .lines()
                .contains(&"queue_queued_packets 2".to_owned())
        );
    }
}
