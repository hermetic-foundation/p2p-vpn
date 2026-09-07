use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use libp2p_identity::PeerId;

use crate::handler::HandlerIn;

/// Aggregate requests awaiting peer connections across a DHT's query pool.
#[derive(Clone, Copy, Debug)]
pub struct PendingRpcLimits {
    requests: usize,
    bytes: usize,
}

impl PendingRpcLimits {
    pub fn new(requests: NonZeroUsize, bytes: NonZeroUsize) -> Self {
        Self {
            requests: requests.get(),
            bytes: bytes.get(),
        }
    }
}

/// Retained payload accounting, not allocator overhead or connection-handler work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PendingRpcUsage {
    pub requests: usize,
    pub bytes: usize,
    pub count_rejections: u64,
    pub byte_rejections: u64,
}

#[derive(Clone, Debug)]
pub(super) struct Budget {
    limits: PendingRpcLimits,
    usage: Arc<Mutex<PendingRpcUsage>>,
}

impl Budget {
    pub(super) fn new(limits: PendingRpcLimits) -> Self {
        Self {
            limits,
            usage: Arc::default(),
        }
    }

    pub(super) fn usage(&self) -> PendingRpcUsage {
        *self.usage.lock().expect("pending RPC budget poisoned")
    }

    fn reserve(&self, bytes: usize) -> Option<Reservation> {
        let mut usage = self.usage.lock().expect("pending RPC budget poisoned");
        if usage.requests >= self.limits.requests {
            usage.count_rejections = usage.count_rejections.saturating_add(1);
            return None;
        }
        if bytes > self.limits.bytes.saturating_sub(usage.bytes) {
            usage.byte_rejections = usage.byte_rejections.saturating_add(1);
            return None;
        }
        usage.requests += 1;
        usage.bytes += bytes;
        Some(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
}

struct Reservation {
    budget: Budget,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut usage = self
            .budget
            .usage
            .lock()
            .expect("pending RPC budget poisoned");
        usage.requests -= 1;
        usage.bytes -= self.bytes;
    }
}

struct PendingRpc {
    peer: PeerId,
    request: HandlerIn,
    _reservation: Option<Reservation>,
}

pub(crate) struct PendingRpcs {
    budget: Option<Budget>,
    requests: Vec<PendingRpc>,
}

impl PendingRpcs {
    pub(super) fn new(budget: Option<Budget>) -> Self {
        Self {
            budget,
            requests: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, peer: PeerId, request: HandlerIn) -> bool {
        debug_assert!(!self.requests.iter().any(|rpc| rpc.peer == peer));
        let reservation = if let Some(budget) = &self.budget {
            let Some(reservation) = budget.reserve(retained_bytes(&request)) else {
                return false;
            };
            Some(reservation)
        } else {
            None
        };
        self.requests.push(PendingRpc {
            peer,
            request,
            _reservation: reservation,
        });
        true
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&PeerId, &HandlerIn)> {
        self.requests.iter().map(|rpc| (&rpc.peer, &rpc.request))
    }

    pub(crate) fn remove_peer(&mut self, peer: &PeerId) -> Option<HandlerIn> {
        let index = self.requests.iter().position(|rpc| &rpc.peer == peer)?;
        Some(self.requests.swap_remove(index).request)
    }

    pub(crate) fn clear(&mut self) {
        self.requests.clear();
    }
}

fn retained_bytes(request: &HandlerIn) -> usize {
    match request {
        HandlerIn::FindNodeReq { key, .. } => key.capacity(),
        HandlerIn::GetProvidersReq { key, .. } | HandlerIn::GetRecord { key, .. } => {
            key.as_ref().len()
        }
        HandlerIn::PutRecord { record, .. } => record
            .key
            .as_ref()
            .len()
            .saturating_add(record.value.capacity()),
        HandlerIn::AddProvider { key, provider, .. } => provider.multiaddrs.iter().fold(
            key.as_ref().len().saturating_add(
                provider
                    .multiaddrs
                    .capacity()
                    .saturating_mul(std::mem::size_of::<libp2p_core::Multiaddr>()),
            ),
            |size, address| size.saturating_add(address.len()),
        ),
        _ => unreachable!("only outbound requests wait for query connections"),
    }
}
