use std::sync::{Arc, Mutex};

use super::HandlerQueueUsage;

/// Resource usage across all observed connection handlers belonging to one DHT.
///
/// Snapshots are refreshed after handler events and polls. Pending payload bytes
/// exclude active streams and allocator overhead; this is not RSS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HandlerResourceUsage {
    /// Currently live handlers, including handlers with no queued work.
    pub handlers: usize,
    /// Gauges are totals across live handlers. Event counters are cumulative,
    /// include closed handlers, and saturate at `u64::MAX`.
    pub usage: HandlerQueueUsage,
    /// Largest reported pending request count on any one handler, including closed handlers.
    pub peak_pending_requests_per_handler: usize,
    /// Largest reported pending payload byte count on any one handler, including closed handlers.
    pub peak_pending_bytes_per_handler: usize,
    /// Largest reported pending negotiation count on any one handler, including closed handlers.
    pub peak_pending_negotiations_per_handler: usize,
    /// Largest reported inbound stream count on any one handler, including closed handlers.
    pub peak_inbound_streams_per_handler: usize,
    /// Largest reported outbound stream count on any one handler, including closed handlers.
    pub peak_outbound_streams_per_handler: usize,
    /// Largest reported queued rejection count on any one handler, including closed handlers.
    pub peak_queued_rejections_per_handler: usize,
}

impl HandlerResourceUsage {
    fn replace_gauges(&mut self, previous: HandlerQueueUsage, current: HandlerQueueUsage) {
        self.usage.active_inbound_streams = self.usage.active_inbound_streams
            - previous.active_inbound_streams
            + current.active_inbound_streams;
        self.usage.pending_negotiations = self.usage.pending_negotiations
            - previous.pending_negotiations
            + current.pending_negotiations;
        self.usage.active_outbound_streams = self.usage.active_outbound_streams
            - previous.active_outbound_streams
            + current.active_outbound_streams;
        self.usage.requests = self.usage.requests - previous.requests + current.requests;
        self.usage.bytes = self.usage.bytes - previous.bytes + current.bytes;
        self.usage.queued_rejections =
            self.usage.queued_rejections - previous.queued_rejections + current.queued_rejections;
    }
}

/// One fixed-size shared aggregate per DHT, with no per-connection registry.
#[derive(Clone, Default)]
pub(crate) struct HandlerResources(Arc<Mutex<HandlerResourceUsage>>);

impl HandlerResources {
    pub(crate) fn usage(&self) -> HandlerResourceUsage {
        *self.0.lock().expect("handler resources poisoned")
    }

    pub(super) fn register(&self) -> HandlerResourceTracker {
        self.0.lock().expect("handler resources poisoned").handlers += 1;
        HandlerResourceTracker {
            resources: self.clone(),
            previous: HandlerQueueUsage::default(),
        }
    }
}

pub(super) struct HandlerResourceTracker {
    resources: HandlerResources,
    previous: HandlerQueueUsage,
}

impl HandlerResourceTracker {
    pub(super) fn update(&mut self, current: HandlerQueueUsage) {
        if self.previous == current {
            return;
        }
        let mut state = self.resources.0.lock().expect("handler resources poisoned");
        state.replace_gauges(self.previous, current);
        state.usage.inbound_rejections = state.usage.inbound_rejections.saturating_add(
            current
                .inbound_rejections
                .saturating_sub(self.previous.inbound_rejections),
        );
        state.usage.inbound_replacements = state.usage.inbound_replacements.saturating_add(
            current
                .inbound_replacements
                .saturating_sub(self.previous.inbound_replacements),
        );
        state.usage.inbound_expired = state.usage.inbound_expired.saturating_add(
            current
                .inbound_expired
                .saturating_sub(self.previous.inbound_expired),
        );
        state.usage.rejected = state
            .usage
            .rejected
            .saturating_add(current.rejected.saturating_sub(self.previous.rejected));
        state.usage.unreported_rejections = state.usage.unreported_rejections.saturating_add(
            current
                .unreported_rejections
                .saturating_sub(self.previous.unreported_rejections),
        );
        state.usage.expired = state
            .usage
            .expired
            .saturating_add(current.expired.saturating_sub(self.previous.expired));
        state.peak_pending_requests_per_handler = state
            .peak_pending_requests_per_handler
            .max(current.requests);
        state.peak_pending_bytes_per_handler =
            state.peak_pending_bytes_per_handler.max(current.bytes);
        state.peak_pending_negotiations_per_handler = state
            .peak_pending_negotiations_per_handler
            .max(current.pending_negotiations);
        state.peak_inbound_streams_per_handler = state
            .peak_inbound_streams_per_handler
            .max(current.active_inbound_streams);
        state.peak_outbound_streams_per_handler = state
            .peak_outbound_streams_per_handler
            .max(current.active_outbound_streams);
        state.peak_queued_rejections_per_handler = state
            .peak_queued_rejections_per_handler
            .max(current.queued_rejections);
        self.previous = current;
    }
}

impl Drop for HandlerResourceTracker {
    fn drop(&mut self) {
        let mut state = self.resources.0.lock().expect("handler resources poisoned");
        state.handlers -= 1;
        state.replace_gauges(self.previous, HandlerQueueUsage::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_usage() -> HandlerQueueUsage {
        HandlerQueueUsage {
            active_inbound_streams: 2,
            inbound_rejections: 3,
            inbound_replacements: 5,
            inbound_expired: 7,
            pending_negotiations: 11,
            active_outbound_streams: 13,
            requests: 17,
            bytes: 19,
            queued_rejections: 23,
            rejected: 29,
            unreported_rejections: 31,
            expired: 37,
        }
    }

    fn unit_usage() -> HandlerQueueUsage {
        HandlerQueueUsage {
            active_inbound_streams: 1,
            inbound_rejections: 1,
            inbound_replacements: 1,
            inbound_expired: 1,
            pending_negotiations: 1,
            active_outbound_streams: 1,
            requests: 1,
            bytes: 1,
            queued_rejections: 1,
            rejected: 1,
            unreported_rejections: 1,
            expired: 1,
        }
    }

    #[test]
    fn idle_handlers_register_and_drop_without_usage() {
        let resources = HandlerResources::default();
        assert_eq!(resources.usage(), HandlerResourceUsage::default());
        let first = resources.register();
        let second = resources.register();
        assert_eq!(
            resources.usage(),
            HandlerResourceUsage {
                handlers: 2,
                ..HandlerResourceUsage::default()
            }
        );
        drop(first);
        assert_eq!(resources.usage().handlers, 1);
        drop(second);
        assert_eq!(resources.usage(), HandlerResourceUsage::default());
    }

    #[test]
    fn aggregate_live_gauges_but_keep_all_counters_after_drop() {
        let resources = HandlerResources::default();
        let mut first = resources.register();
        let mut second = resources.register();
        first.update(sample_usage());
        second.update(unit_usage());
        let aggregate = resources.usage();
        assert_eq!(aggregate.handlers, 2);
        assert_eq!(
            aggregate.usage,
            HandlerQueueUsage {
                active_inbound_streams: 3,
                inbound_rejections: 4,
                inbound_replacements: 6,
                inbound_expired: 8,
                pending_negotiations: 12,
                active_outbound_streams: 14,
                requests: 18,
                bytes: 20,
                queued_rejections: 24,
                rejected: 30,
                unreported_rejections: 32,
                expired: 38,
            }
        );
        drop(first);
        assert_eq!(
            resources.usage(),
            HandlerResourceUsage {
                handlers: 1,
                usage: HandlerQueueUsage {
                    active_inbound_streams: 1,
                    pending_negotiations: 1,
                    active_outbound_streams: 1,
                    requests: 1,
                    bytes: 1,
                    queued_rejections: 1,
                    ..aggregate.usage
                },
                ..aggregate
            }
        );
        drop(second);
        assert_eq!(
            resources.usage(),
            HandlerResourceUsage {
                handlers: 0,
                usage: HandlerQueueUsage {
                    active_inbound_streams: 0,
                    pending_negotiations: 0,
                    active_outbound_streams: 0,
                    requests: 0,
                    bytes: 0,
                    queued_rejections: 0,
                    ..aggregate.usage
                },
                ..aggregate
            }
        );
    }

    #[test]
    fn updates_replace_gauges_and_count_only_new_events() {
        let resources = HandlerResources::default();
        let mut tracker = resources.register();
        tracker.update(sample_usage());
        let current = HandlerQueueUsage {
            active_inbound_streams: 1,
            inbound_rejections: 9,
            inbound_replacements: 11,
            inbound_expired: 13,
            pending_negotiations: 4,
            active_outbound_streams: 2,
            requests: 3,
            bytes: 100,
            queued_rejections: 4,
            rejected: 41,
            unreported_rejections: 43,
            expired: 47,
        };
        tracker.update(current);
        let snapshot = resources.usage();
        assert_eq!(snapshot.usage, current);
        for _ in 0..1_000 {
            tracker.update(current);
            assert_eq!(resources.usage(), snapshot);
        }
    }

    #[test]
    fn peaks_are_per_handler_and_survive_drain_and_drop() {
        let resources = HandlerResources::default();
        let mut first = resources.register();
        let mut second = resources.register();
        first.update(sample_usage());
        second.update(HandlerQueueUsage {
            active_inbound_streams: 3,
            pending_negotiations: 10,
            active_outbound_streams: 14,
            requests: 16,
            bytes: 20,
            queued_rejections: 22,
            ..HandlerQueueUsage::default()
        });
        let peaks = HandlerResourceUsage {
            peak_pending_requests_per_handler: 17,
            peak_pending_bytes_per_handler: 20,
            peak_pending_negotiations_per_handler: 11,
            peak_inbound_streams_per_handler: 3,
            peak_outbound_streams_per_handler: 14,
            peak_queued_rejections_per_handler: 23,
            ..resources.usage()
        };
        assert_eq!(resources.usage(), peaks);
        second.update(HandlerQueueUsage::default());
        drop(first);
        drop(second);
        assert_eq!(
            resources.usage(),
            HandlerResourceUsage {
                handlers: 0,
                usage: HandlerQueueUsage {
                    active_inbound_streams: 0,
                    pending_negotiations: 0,
                    active_outbound_streams: 0,
                    requests: 0,
                    bytes: 0,
                    queued_rejections: 0,
                    ..sample_usage()
                },
                ..peaks
            }
        );
    }

    #[test]
    fn registration_churn_keeps_one_shared_state_and_no_retired_trackers() {
        let resources = HandlerResources::default();
        let allocation = Arc::as_ptr(&resources.0);
        for count in 1..=10_000_u64 {
            let mut tracker = resources.register();
            tracker.update(unit_usage());
            assert!(Arc::ptr_eq(&resources.0, &tracker.resources.0));
            assert_eq!(Arc::strong_count(&resources.0), 2);
            assert_eq!(resources.usage().handlers, 1);
            drop(tracker);
            assert_eq!(Arc::strong_count(&resources.0), 1);
            assert_eq!(Arc::as_ptr(&resources.0), allocation);
            assert_eq!(
                resources.usage(),
                HandlerResourceUsage {
                    handlers: 0,
                    usage: HandlerQueueUsage {
                        inbound_rejections: count,
                        inbound_replacements: count,
                        inbound_expired: count,
                        rejected: count,
                        unreported_rejections: count,
                        expired: count,
                        ..HandlerQueueUsage::default()
                    },
                    peak_pending_requests_per_handler: 1,
                    peak_pending_bytes_per_handler: 1,
                    peak_pending_negotiations_per_handler: 1,
                    peak_inbound_streams_per_handler: 1,
                    peak_outbound_streams_per_handler: 1,
                    peak_queued_rejections_per_handler: 1,
                }
            );
        }
    }

    #[test]
    fn clones_share_usage_but_separate_dht_observers_are_isolated() {
        let first = HandlerResources::default();
        let first_clone = first.clone();
        let second = HandlerResources::default();
        let mut first_tracker = first_clone.register();
        first_tracker.update(sample_usage());
        assert_eq!(first.usage(), first_clone.usage());
        assert_eq!(second.usage(), HandlerResourceUsage::default());
        let first_snapshot = first.usage();
        let mut second_tracker = second.register();
        second_tracker.update(unit_usage());
        assert_eq!(first.usage(), first_snapshot);
        let second_snapshot = second.usage();
        drop(first_tracker);
        assert_eq!(first.usage().handlers, 0);
        assert_eq!(first.usage(), first_clone.usage());
        assert_eq!(second.usage(), second_snapshot);
    }

    #[test]
    fn tracker_can_outlive_observer_and_releases_the_shared_allocation() {
        let resources = HandlerResources::default();
        let allocation = Arc::downgrade(&resources.0);
        let mut tracker = resources.register();
        drop(resources);
        tracker.update(sample_usage());
        assert_eq!(allocation.strong_count(), 1);
        drop(tracker);
        assert!(allocation.upgrade().is_none());
    }

    #[test]
    fn cumulative_counters_saturate_and_never_wrap_on_update_or_drop() {
        let resources = HandlerResources::default();
        let mut first = resources.register();
        let mut second = resources.register();
        let saturated = HandlerQueueUsage {
            inbound_rejections: u64::MAX,
            inbound_replacements: u64::MAX,
            inbound_expired: u64::MAX,
            rejected: u64::MAX,
            unreported_rejections: u64::MAX,
            expired: u64::MAX,
            ..HandlerQueueUsage::default()
        };
        first.update(saturated);
        second.update(unit_usage());
        drop(first);
        second.update(sample_usage());
        drop(second);
        assert_eq!(resources.usage().handlers, 0);
        assert_eq!(resources.usage().usage, saturated);
    }
}
