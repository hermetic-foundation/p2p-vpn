use std::{collections::VecDeque, num::NonZeroUsize, task::Context, time::Duration};

use futures::FutureExt;
use futures_timer::Delay;
use web_time::Instant;

use crate::{protocol::KadRequestMsg, QueryId};

/// Admission limits for requests waiting for an outbound connection substream.
#[derive(Clone, Copy, Debug)]
pub struct HandlerQueueLimits {
    pub(crate) requests: usize,
    pub(crate) bytes: usize,
}

impl HandlerQueueLimits {
    pub fn new(requests: NonZeroUsize, bytes: NonZeroUsize) -> Self {
        Self {
            requests: requests.get(),
            bytes: bytes.get(),
        }
    }
}

/// Pending payload accounting excludes active streams and allocator overhead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HandlerQueueUsage {
    pub pending_negotiations: usize,
    pub active_outbound_streams: usize,
    pub requests: usize,
    pub bytes: usize,
    pub queued_rejections: usize,
    pub rejected: u64,
    pub unreported_rejections: u64,
    pub expired: u64,
}

pub(super) struct PendingRequests {
    limits: Option<HandlerQueueLimits>,
    requests: VecDeque<(KadRequestMsg, QueryId, usize, Instant)>,
    rejected: VecDeque<QueryId>,
    usage: HandlerQueueUsage,
    timeout: Duration,
    timer: Delay,
}

impl PendingRequests {
    pub(super) fn new(limits: Option<HandlerQueueLimits>, timeout: Duration) -> Self {
        Self {
            limits,
            requests: VecDeque::new(),
            rejected: VecDeque::new(),
            usage: HandlerQueueUsage::default(),
            timeout,
            timer: Delay::new(timeout),
        }
    }

    pub(super) fn usage(&self) -> HandlerQueueUsage {
        HandlerQueueUsage {
            requests: self.requests.len(),
            queued_rejections: self.rejected.len(),
            ..self.usage
        }
    }

    pub(super) fn push(&mut self, msg: KadRequestMsg, query: QueryId) {
        let bytes = retained_bytes(&msg);
        if self.limits.is_some_and(|limits| {
            self.requests.len() >= limits.requests
                || bytes > limits.bytes.saturating_sub(self.usage.bytes)
        }) {
            self.usage.rejected = self.usage.rejected.saturating_add(1);
            let limit = self.limits.expect("checked limits").requests;
            if !self.rejected.contains(&query) {
                if self.rejected.len() < limit {
                    self.rejected.push_back(query);
                } else {
                    self.usage.unreported_rejections =
                        self.usage.unreported_rejections.saturating_add(1);
                }
            }
            return;
        }
        self.usage.bytes = self.usage.bytes.saturating_add(bytes);
        self.requests.push_back((msg, query, bytes, Instant::now()));
        if self.requests.len() == 1 {
            self.reset_timer();
        }
    }

    pub(super) fn pop(&mut self) -> Option<(KadRequestMsg, QueryId)> {
        let (msg, query, bytes, _) = self.requests.pop_front()?;
        self.usage.bytes -= bytes;
        self.reset_timer();
        Some((msg, query))
    }

    pub(super) fn pop_rejected(&mut self) -> Option<QueryId> {
        self.rejected.pop_front()
    }

    pub(super) fn poll_expired(&mut self, cx: &mut Context<'_>) -> Option<QueryId> {
        if self.limits.is_none() || self.requests.is_empty() {
            return None;
        }
        if self
            .requests
            .front()
            .is_some_and(|(_, _, _, at)| at.elapsed() >= self.timeout)
        {
            let (_, query) = self.pop().expect("front exists");
            self.usage.expired = self.usage.expired.saturating_add(1);
            return Some(query);
        }
        if self.timer.poll_unpin(cx).is_ready() {
            self.reset_timer();
            let _ = self.timer.poll_unpin(cx);
        }
        None
    }

    fn reset_timer(&mut self) {
        if let Some((_, _, _, at)) = self.requests.front() {
            self.timer
                .reset((*at + self.timeout).saturating_duration_since(Instant::now()));
        }
    }
}

fn retained_bytes(msg: &KadRequestMsg) -> usize {
    match msg {
        KadRequestMsg::Ping => 0,
        KadRequestMsg::FindNode { key } => key.capacity(),
        KadRequestMsg::GetProviders { key } | KadRequestMsg::GetValue { key } => key.as_ref().len(),
        KadRequestMsg::AddProvider { key, provider } => provider.multiaddrs.iter().fold(
            key.as_ref().len().saturating_add(
                provider
                    .multiaddrs
                    .capacity()
                    .saturating_mul(std::mem::size_of::<libp2p_core::Multiaddr>()),
            ),
            |size, address| size.saturating_add(address.len()),
        ),
        KadRequestMsg::PutValue { record } => record
            .key
            .as_ref()
            .len()
            .saturating_add(record.value.capacity()),
    }
}
