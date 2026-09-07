use std::{
    collections::{HashMap, HashSet},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use libp2p_core::Multiaddr;

/// Aggregate retained routing-entry generations and encoded address buffers.
#[derive(Clone, Copy, Debug)]
pub struct RoutingLimits {
    entries: usize,
    bytes: usize,
}

impl RoutingLimits {
    pub fn new(entries: NonZeroUsize, bytes: NonZeroUsize) -> Self {
        Self {
            entries: entries.get(),
            bytes: bytes.get(),
        }
    }
}

/// Includes pending entries and routing snapshots until their last owner drops.
/// Address buffers shared by snapshots are charged once; this is not RSS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoutingUsage {
    /// Retained entry generations, not just currently visible peers.
    pub entries: usize,
    pub address_bytes: usize,
    pub entry_rejections: u64,
    pub address_rejections: u64,
}

#[derive(Debug)]
struct State {
    limits: RoutingLimits,
    usage: RoutingUsage,
}

#[derive(Clone, Debug)]
pub(crate) struct RoutingBudget(Arc<Mutex<State>>);

impl RoutingBudget {
    pub(crate) fn new(limits: RoutingLimits) -> Self {
        Self(Arc::new(Mutex::new(State {
            limits,
            usage: RoutingUsage::default(),
        })))
    }

    pub(crate) fn usage(&self) -> RoutingUsage {
        self.0.lock().expect("routing budget poisoned").usage
    }
}

#[derive(Debug)]
struct Lease {
    budget: RoutingBudget,
    entries: usize,
    bytes: usize,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.budget.0.lock().expect("routing budget poisoned");
        state.usage.entries -= self.entries;
        state.usage.address_bytes -= self.bytes;
    }
}

#[derive(Clone)]
pub(crate) struct Reservations {
    entry: Arc<Lease>,
    addresses: HashMap<Multiaddr, Arc<Lease>>,
}

impl Reservations {
    pub(super) fn new(budget: &RoutingBudget, address: &Multiaddr) -> Option<Self> {
        Self::notification(budget, Some(address))
    }

    /// Callers normalize the owned address before reservation, so its lease key
    /// shares the charged buffer instead of allocating a second copy.
    pub(crate) fn notification(
        budget: &RoutingBudget,
        address: Option<&Multiaddr>,
    ) -> Option<Self> {
        let bytes = address.map_or(0, Multiaddr::len);
        let mut state = budget.0.lock().expect("routing budget poisoned");
        if state.usage.entries >= state.limits.entries {
            state.usage.entry_rejections = state.usage.entry_rejections.saturating_add(1);
            return None;
        }
        if bytes > state.limits.bytes.saturating_sub(state.usage.address_bytes) {
            state.usage.address_rejections = state.usage.address_rejections.saturating_add(1);
            return None;
        }
        state.usage.entries += 1;
        state.usage.address_bytes += bytes;
        drop(state);
        Some(Self {
            entry: Arc::new(Lease {
                budget: budget.clone(),
                entries: 1,
                bytes: 0,
            }),
            addresses: address
                .into_iter()
                .map(|address| {
                    (
                        address.clone(),
                        Arc::new(Lease {
                            budget: budget.clone(),
                            entries: 0,
                            bytes,
                        }),
                    )
                })
                .collect(),
        })
    }

    /// Reserve the entire change before altering admitted data. Unique removed
    /// buffers can fund replacements; snapshots keep shared buffers charged.
    pub(super) fn revise(&mut self, addresses: &[Multiaddr]) -> bool {
        let wanted: HashSet<_> = addresses.iter().collect();
        let added: Vec<_> = wanted
            .iter()
            .filter(|address| !self.addresses.contains_key(**address))
            .map(|address| (*address).clone())
            .collect();
        let removed: Vec<_> = self
            .addresses
            .keys()
            .filter(|address| !wanted.contains(address))
            .cloned()
            .collect();
        let credited: Vec<_> = removed
            .iter()
            .filter(|address| Arc::strong_count(&self.addresses[*address]) == 1)
            .collect();
        let credit: usize = credited
            .iter()
            .map(|address| self.addresses[*address].bytes)
            .sum();
        let debit = added.iter().map(Multiaddr::len).sum::<usize>();
        let budget = self.entry.budget.clone();
        // A new published version needs its own slot while older snapshots live.
        // Pure removal cannot create new buffers or publication events.
        let fork_entry = !added.is_empty() && Arc::strong_count(&self.entry) > 1;
        let mut state = budget.0.lock().expect("routing budget poisoned");
        if fork_entry && state.usage.entries >= state.limits.entries {
            state.usage.entry_rejections = state.usage.entry_rejections.saturating_add(1);
            return false;
        }
        let retained = state.usage.address_bytes - credit;
        if debit > state.limits.bytes.saturating_sub(retained) {
            state.usage.address_rejections = state.usage.address_rejections.saturating_add(1);
            return false;
        }
        state.usage.address_bytes = retained + debit;
        state.usage.entries += usize::from(fork_entry);
        // Zero credited leases before dropping them, outside the budget lock.
        for address in credited {
            Arc::get_mut(self.addresses.get_mut(address).expect("known lease"))
                .expect("credited lease remains unique")
                .bytes = 0;
        }
        drop(state);
        if fork_entry {
            self.entry = Arc::new(Lease {
                budget: budget.clone(),
                entries: 1,
                bytes: 0,
            });
        }
        for address in removed {
            self.addresses.remove(&address);
        }
        for address in added {
            let bytes = address.len();
            self.addresses.insert(
                address,
                Arc::new(Lease {
                    budget: budget.clone(),
                    entries: 0,
                    bytes,
                }),
            );
        }
        true
    }
}
