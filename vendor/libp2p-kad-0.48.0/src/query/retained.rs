use std::{collections::HashSet, num::NonZeroUsize};

use fnv::FnvHashMap;
use libp2p_core::Multiaddr;
use libp2p_identity::PeerId;
use smallvec::SmallVec;

use crate::{addresses::normalized_address, AddressLimits};

/// Optional per-query candidate and encoded-address budgets.
///
/// Once a candidate is admitted it counts until the query is retired, including
/// after failure. Excess candidates are ignored, not queued for later admission.
/// This bounds search work and can reduce results in a heavily branching lookup.
#[derive(Clone, Copy, Debug)]
pub struct QueryLimits {
    pub(crate) candidates: usize,
    addresses: AddressLimits,
    bytes: usize,
}

impl QueryLimits {
    /// Budgets include initial candidates and all subsequently discovered peers.
    /// Address bytes count encoded multiaddresses, not allocator overhead.
    pub fn new(candidates: NonZeroUsize, addresses: AddressLimits, bytes: NonZeroUsize) -> Self {
        Self {
            candidates: candidates.get(),
            addresses,
            bytes: bytes.get(),
        }
    }
}

/// Current retained query resources, excluding the routing table and wire buffers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueryResourceUsage {
    /// Distinct identities admitted over this query phase's lifetime.
    pub candidates: usize,
    /// Currently retained encoded multiaddress bytes.
    pub address_bytes: usize,
    /// Reports rejected by candidate, address-count, or address-byte budgets.
    pub rejected: usize,
}

pub(crate) struct RetainedPeers {
    limits: Option<QueryLimits>,
    admitted: HashSet<PeerId>,
    addresses: FnvHashMap<PeerId, SmallVec<[Multiaddr; 8]>>,
    usage: QueryResourceUsage,
}

impl RetainedPeers {
    pub(super) fn new(limits: Option<QueryLimits>) -> Self {
        Self {
            limits,
            admitted: HashSet::new(),
            addresses: FnvHashMap::default(),
            usage: QueryResourceUsage::default(),
        }
    }

    pub(super) fn admit(&mut self, peer: PeerId) -> bool {
        let Some(limits) = self.limits else {
            return true;
        };
        if self.admitted.contains(&peer) {
            return true;
        }
        if self.admitted.len() >= limits.candidates {
            self.usage.rejected = self.usage.rejected.saturating_add(1);
            return false;
        }
        self.admitted.insert(peer);
        self.usage.candidates = self.admitted.len();
        true
    }

    pub(crate) fn learn(&mut self, peer: PeerId, addresses: &[Multiaddr]) -> bool {
        if !self.admit(peer) {
            return false;
        }
        let Some(limits) = self.limits else {
            self.addresses
                .insert(peer, addresses.iter().cloned().collect());
            return true;
        };
        if let Some(old) = self.addresses.remove(&peer) {
            self.usage.address_bytes -= old.iter().map(Multiaddr::len).sum::<usize>();
        }
        let mut retained = SmallVec::<[Multiaddr; 8]>::new();
        for address in addresses {
            if retained.contains(address) {
                continue;
            }
            if retained.len() >= limits.addresses.count
                || !limits.addresses.accepts(address)
                || address.len() > limits.bytes.saturating_sub(self.usage.address_bytes)
            {
                self.usage.rejected = self.usage.rejected.saturating_add(1);
                continue;
            }
            self.usage.address_bytes += address.len();
            retained.push(normalized_address(address));
        }
        self.addresses.insert(peer, retained);
        true
    }

    pub(crate) fn get(&self, peer: &PeerId) -> Option<&SmallVec<[Multiaddr; 8]>> {
        self.addresses.get(peer)
    }

    pub(super) fn remove(&mut self, peer: &PeerId) -> Option<SmallVec<[Multiaddr; 8]>> {
        let removed = self.addresses.remove(peer)?;
        if self.limits.is_some() {
            self.usage.address_bytes -= removed.iter().map(Multiaddr::len).sum::<usize>();
        }
        Some(removed)
    }

    pub(crate) fn address_failed(&mut self, peer: &PeerId, address: &Multiaddr) {
        if let Some(addresses) = self.addresses.get_mut(peer) {
            addresses.retain(|candidate| {
                if candidate != address {
                    return true;
                }
                if self.limits.is_some() {
                    self.usage.address_bytes -= candidate.len();
                }
                false
            });
        }
    }

    pub(crate) fn replace(&mut self, peer: &PeerId, old: &Multiaddr, new: &Multiaddr) {
        let Some(addresses) = self.addresses.get_mut(peer) else {
            return;
        };
        let Some(index) = addresses.iter().position(|address| address == old) else {
            return;
        };
        if let Some(limits) = self.limits {
            let duplicate = addresses.contains(new);
            let remaining = self.usage.address_bytes - old.len();
            if !limits.addresses.accepts(new)
                || (!duplicate && new.len() > limits.bytes.saturating_sub(remaining))
            {
                self.usage.rejected = self.usage.rejected.saturating_add(1);
                return;
            }
            if old == new {
                return;
            }
            if duplicate {
                addresses.remove(index);
                self.usage.address_bytes = remaining;
            } else {
                addresses[index] = normalized_address(new);
                self.usage.address_bytes = remaining + new.len();
            }
        } else {
            for address in addresses.iter_mut().filter(|address| *address == old) {
                *address = new.clone();
            }
        }
    }

    pub(crate) fn usage(&self) -> Option<QueryResourceUsage> {
        self.limits.map(|_| self.usage)
    }
}
