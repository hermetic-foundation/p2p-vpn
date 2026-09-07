use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

use futures::{stream::SelectAll, FutureExt, Stream, StreamExt};
use futures_timer::Delay;
use web_time::Instant;

pub(super) struct InboundStreams<S> {
    streams: SelectAll<RetainedStream<S>>,
    limit: usize,
    timeout: Duration,
    expired: Arc<AtomicU64>,
    rejected: u64,
    replaced: u64,
}

impl<S: Stream + Unpin> InboundStreams<S> {
    pub(super) fn new(limit: usize, timeout: Duration) -> Self {
        Self {
            streams: SelectAll::new(),
            limit,
            timeout,
            expired: Arc::default(),
            rejected: 0,
            replaced: 0,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.streams.len()
    }
    pub(super) fn expired(&self) -> u64 {
        self.expired.load(Ordering::Relaxed)
    }
    pub(super) fn rejected(&self) -> u64 {
        self.rejected
    }
    pub(super) fn replaced(&self) -> u64 {
        self.replaced
    }

    pub(super) fn push(&mut self, stream: S, reusable: impl Fn(&S) -> bool) {
        if self.streams.len() < self.limit {
            self.streams.push(RetainedStream::new(
                stream,
                self.timeout,
                self.expired.clone(),
            ));
        } else if let Some(slot) = self.streams.iter_mut().find(|slot| reusable(&slot.stream)) {
            // Replace the existing slot, not its state followed by another allocation.
            // SelectAll only polls notified members, so wake the old slot's task.
            let waker = slot.waker.take();
            *slot = RetainedStream::new(stream, self.timeout, self.expired.clone());
            self.replaced = self.replaced.saturating_add(1);
            if let Some(waker) = waker {
                waker.wake();
            }
        } else {
            self.rejected = self.rejected.saturating_add(1);
        }
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = &mut S> {
        self.streams.iter_mut().map(|slot| &mut slot.stream)
    }

    pub(super) fn poll_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<S::Item>> {
        self.streams.poll_next_unpin(cx)
    }
}

struct RetainedStream<S> {
    stream: S,
    timeout: Duration,
    deadline: Instant,
    timer: Delay,
    waker: Option<Waker>,
    expired: Arc<AtomicU64>,
}

impl<S> RetainedStream<S> {
    fn new(stream: S, timeout: Duration, expired: Arc<AtomicU64>) -> Self {
        Self {
            stream,
            timeout,
            deadline: Instant::now() + timeout,
            timer: Delay::new(timeout),
            waker: None,
            expired,
        }
    }
}

impl<S: Stream + Unpin> Stream for RetainedStream<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        this.waker = Some(cx.waker().clone());
        if Instant::now() >= this.deadline || this.timer.poll_unpin(cx).is_ready() {
            let _ = this
                .expired
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                    Some(n.saturating_add(1))
                });
            return Poll::Ready(None);
        }
        let result = this.stream.poll_next_unpin(cx);
        if let Poll::Ready(Some(_)) = result {
            this.deadline = Instant::now() + this.timeout;
            this.timer.reset(this.timeout);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::task::{waker, ArcWake};
    use std::sync::atomic::AtomicUsize;

    struct Probe {
        value: Option<u64>,
        reusable: bool,
        pending: bool,
    }
    impl Stream for Probe {
        type Item = u64;
        fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<u64>> {
            let this = self.get_mut();
            if this.pending {
                Poll::Pending
            } else {
                Poll::Ready(this.value.take())
            }
        }
    }

    #[derive(Default)]
    struct WakeCount(AtomicUsize);
    impl ArcWake for WakeCount {
        fn wake_by_ref(arc: &Arc<Self>) {
            arc.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn legacy_cancel_then_push_control_exceeds_the_nominal_limit() {
        let mut streams = SelectAll::new();
        for _ in 0..32 {
            streams.push(Probe {
                value: None,
                reusable: true,
                pending: true,
            });
        }
        for _ in 0..128 {
            if streams.len() == 32 {
                let reusable = streams.iter_mut().find(|s| s.reusable).unwrap();
                reusable.pending = false;
                reusable.value = None;
            }
            streams.push(Probe {
                value: None,
                reusable: false,
                pending: true,
            });
        }
        assert_eq!(
            streams.len(),
            160,
            "canceling a stream does not synchronously remove its SelectAll slot"
        );
    }

    #[test]
    fn inbound_activity_refreshes_deadline_but_expiry_precedes_ready_input() {
        let expired = Arc::new(AtomicU64::new(0));
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut stream = RetainedStream::new(
            Probe {
                value: Some(1),
                reusable: false,
                pending: false,
            },
            Duration::from_secs(10),
            expired.clone(),
        );
        stream.deadline = Instant::now() + Duration::from_secs(1);
        let previous = stream.deadline;
        assert_eq!(stream.poll_next_unpin(&mut cx), Poll::Ready(Some(1)));
        assert!(stream.deadline > previous);
        stream.stream.value = Some(2);
        stream.deadline = Instant::now();
        assert_eq!(stream.poll_next_unpin(&mut cx), Poll::Ready(None));
        assert_eq!(expired.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn replacement_burst_stays_bounded_and_wakes_replaced_idle_slots() {
        let wake = Arc::new(WakeCount::default());
        let waker = waker(wake.clone());
        let mut cx = Context::from_waker(&waker);
        let mut streams = InboundStreams::new(32, Duration::from_secs(10));
        for _ in 0..32 {
            streams.push(
                Probe {
                    value: None,
                    reusable: true,
                    pending: true,
                },
                |s| s.reusable,
            );
        }
        assert!(streams.poll_next(&mut cx).is_pending());
        let before = wake.0.load(Ordering::Relaxed);
        for value in 0..512 {
            streams.push(
                Probe {
                    value: Some(value),
                    reusable: false,
                    pending: false,
                },
                |s| s.reusable,
            );
            assert_eq!(streams.len(), 32);
        }
        assert_eq!(streams.replaced(), 32);
        assert_eq!(streams.rejected(), 480);
        assert!(wake.0.load(Ordering::Relaxed) > before);
        assert_eq!(streams.iter_mut().count(), 32);
        let mut values = Vec::new();
        loop {
            match streams.poll_next(&mut cx) {
                Poll::Ready(Some(value)) => values.push(value),
                Poll::Ready(None) => break,
                Poll::Pending => panic!("replacement lost its wakeup"),
            }
        }
        values.sort_unstable();
        assert_eq!(values, (0..32).collect::<Vec<_>>());
        assert_eq!(streams.len(), 0);
        streams.push(
            Probe {
                value: Some(99),
                reusable: false,
                pending: false,
            },
            |_| false,
        );
        assert_eq!(streams.poll_next(&mut cx), Poll::Ready(Some(99)));
        assert_eq!(streams.expired(), 0);
    }

    #[test]
    fn stalled_inbound_streams_expire_without_socket_activity_and_readmit() {
        let wake = Arc::new(WakeCount::default());
        let waker = waker(wake.clone());
        let mut cx = Context::from_waker(&waker);
        let mut streams = InboundStreams::new(2, Duration::from_millis(30));
        for _ in 0..2 {
            streams.push(
                Probe {
                    value: None,
                    reusable: false,
                    pending: true,
                },
                |_| false,
            );
        }
        assert!(streams.poll_next(&mut cx).is_pending());
        let before = wake.0.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(80));
        assert!(wake.0.load(Ordering::Relaxed) > before);
        assert_eq!(streams.poll_next(&mut cx), Poll::Ready(None));
        assert_eq!(streams.expired(), 2);
        assert_eq!(streams.len(), 0);
        streams.push(
            Probe {
                value: Some(1),
                reusable: false,
                pending: false,
            },
            |_| false,
        );
        assert_eq!(streams.poll_next(&mut cx), Poll::Ready(Some(1)));
    }
}
