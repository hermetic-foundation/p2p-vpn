use std::num::NonZeroUsize;

use crate::{
    addresses::normalized_address,
    behaviour::{AddProviderPhase, PutRecordPhase},
    AddressLimits, QueryInfo,
};

/// Per-query metadata ceilings. A pool-entry cap makes these aggregate bounds.
#[derive(Clone, Copy, Debug)]
pub struct QueryMetadataLimits {
    pub(crate) payload_bytes: usize,
    pub(crate) result_peers: usize,
    pub(crate) provider_addresses: AddressLimits,
}

impl QueryMetadataLimits {
    /// `payload_bytes` covers the input key and record value together.
    /// `result_peers` limits stored acknowledgement/cache entries, not quorum.
    /// Provider addresses have a separate count and encoded-size allowance.
    pub fn new(
        payload_bytes: NonZeroUsize,
        result_peers: NonZeroUsize,
        provider_addresses: AddressLimits,
    ) -> Self {
        Self {
            payload_bytes: payload_bytes.get(),
            result_peers: result_peers.get(),
            provider_addresses,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Kademlia query input needs {bytes} bytes, exceeding the {limit}-byte metadata limit")]
pub struct QueryInputTooLarge {
    pub bytes: usize,
    pub limit: usize,
}

/// Checked starts distinguish temporary pool exhaustion from an oversized input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum QueryStartError {
    #[error(transparent)]
    Capacity(#[from] super::QueryCapacityError),
    #[error(transparent)]
    InputTooLarge(#[from] QueryInputTooLarge),
}

/// Counts every retained query, including finished entries awaiting retirement.
/// Payload bytes exclude bookkeeping/container overhead and allocator RSS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryMetadataUsage {
    /// Input keys/values plus encoded provider addresses, excluding query-peer caches.
    pub payload_bytes: usize,
    /// Live acknowledgement/cache entries; reserved vector capacity may be larger.
    pub result_peers: usize,
    pub provider_addresses: usize,
    /// Conservative retained slots for partially consumed bootstrap iterators.
    pub bootstrap_target_slots: usize,
    /// Fixed-peer iterator allocation slots, including consumed positions.
    pub fixed_peer_slots: usize,
    pub rejected_inputs: u64,
}

impl QueryInfo {
    pub(super) fn input_bytes(&self) -> usize {
        match self {
            Self::Bootstrap { .. } => 0,
            Self::GetClosestPeers { key, .. } => key.len(),
            Self::GetRecord { key, .. }
            | Self::GetProviders { key, .. }
            | Self::AddProvider { key, .. } => key.as_ref().len(),
            Self::PutRecord { record, .. } => {
                record.key.as_ref().len().saturating_add(record.value.len())
            }
        }
    }

    pub(super) fn normalize_metadata(&mut self, limits: QueryMetadataLimits) {
        match self {
            Self::Bootstrap { .. } => {}
            Self::GetClosestPeers { key, .. } => {
                *key = std::mem::take(key).into_boxed_slice().into_vec()
            }
            Self::GetRecord { key, .. } | Self::GetProviders { key, .. } => {
                *key = crate::RecordKey::new(key)
            }
            Self::AddProvider { key, phase, .. } => {
                *key = crate::RecordKey::new(key);
                if let AddProviderPhase::AddProvider {
                    external_addresses, ..
                } = phase
                {
                    external_addresses.retain(|address| limits.provider_addresses.accepts(address));
                    external_addresses.truncate(limits.provider_addresses.count);
                    for address in external_addresses.iter_mut() {
                        *address = normalized_address(address);
                    }
                    *external_addresses = std::mem::take(external_addresses)
                        .into_boxed_slice()
                        .into_vec();
                }
            }
            Self::PutRecord { record, phase, .. } => {
                record.key = crate::RecordKey::new(&record.key);
                record.value = std::mem::take(&mut record.value)
                    .into_boxed_slice()
                    .into_vec();
                if let PutRecordPhase::PutRecord { success, .. } = phase {
                    success.truncate(limits.result_peers);
                    success.reserve_exact(limits.result_peers.saturating_sub(success.len()));
                }
            }
        }
    }

    pub(super) fn add_metadata_usage(&self, usage: &mut QueryMetadataUsage) {
        usage.payload_bytes += self.input_bytes();
        match self {
            Self::Bootstrap {
                remaining: Some(_), ..
            } => usage.bootstrap_target_slots += 256,
            Self::GetRecord {
                cache_candidates, ..
            } => usage.result_peers += cache_candidates.len(),
            Self::PutRecord {
                phase: PutRecordPhase::PutRecord { success, .. },
                ..
            } => usage.result_peers += success.len(),
            Self::AddProvider {
                phase:
                    AddProviderPhase::AddProvider {
                        external_addresses, ..
                    },
                ..
            } => {
                usage.provider_addresses += external_addresses.len();
                usage.payload_bytes += external_addresses
                    .iter()
                    .map(|address| address.len())
                    .sum::<usize>();
            }
            _ => {}
        }
    }
}
