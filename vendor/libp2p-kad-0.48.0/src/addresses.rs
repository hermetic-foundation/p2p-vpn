// Copyright 2019 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

use std::{collections::HashSet, fmt, num::NonZeroUsize};

use libp2p_core::{multiaddr::Protocol, Multiaddr};
use smallvec::SmallVec;

mod budget;
pub(crate) use budget::Reservations;
pub(crate) use budget::RoutingBudget;
pub use budget::{RoutingLimits, RoutingUsage};

/// A non-empty list of (unique) addresses of a peer in the routing table.
/// Every address must be a fully-qualified /p2p address.
#[derive(Clone)]
pub struct Addresses {
    addrs: SmallVec<[Multiaddr; 6]>,
    limits: AddressLimits,
    protected: HashSet<Multiaddr>,
    reservations: Option<Reservations>,
    normalize_buffers: bool,
}

/// Optional routing-address budgets. Unconfigured library users retain the
/// upstream behavior; applications can enable finite per-peer storage.
#[derive(Clone, Copy, Debug)]
pub struct AddressLimits {
    pub(crate) count: usize,
    bytes: usize,
}

impl Default for AddressLimits {
    fn default() -> Self {
        Self {
            count: usize::MAX,
            bytes: usize::MAX,
        }
    }
}

impl AddressLimits {
    /// Maximum retained addresses per peer and encoded bytes per address.
    pub fn new(count: NonZeroUsize, bytes: NonZeroUsize) -> Self {
        Self {
            count: count.get(),
            bytes: bytes.get(),
        }
    }

    pub(crate) fn accepts(&self, address: &Multiaddr) -> bool {
        address.len() <= self.bytes
    }
}

pub(crate) fn normalized_address(address: &Multiaddr) -> Multiaddr {
    Multiaddr::try_from(address.to_vec()).expect("existing multiaddress is valid")
}

#[allow(clippy::len_without_is_empty)]
impl Addresses {
    /// Creates a new list of addresses.
    pub fn new(addr: Multiaddr) -> Addresses {
        let mut addrs = SmallVec::new();
        addrs.push(addr);
        Addresses {
            addrs,
            limits: AddressLimits::default(),
            protected: HashSet::new(),
            reservations: None,
            normalize_buffers: false,
        }
    }

    pub(crate) fn with_limits(
        addr: Multiaddr,
        limits: AddressLimits,
        budget: Option<&RoutingBudget>,
    ) -> Option<Self> {
        if !limits.accepts(&addr) {
            return None;
        }
        let normalize_buffers =
            budget.is_some() || limits.count != usize::MAX || limits.bytes != usize::MAX;
        let addr = if normalize_buffers {
            normalized_address(&addr)
        } else {
            addr
        };
        let reservations = match budget {
            Some(budget) => Some(Reservations::new(budget, &addr)?),
            None => None,
        };
        let mut addresses = Self::new(addr);
        addresses.limits = limits;
        addresses.reservations = reservations;
        addresses.normalize_buffers = normalize_buffers;
        Some(addresses)
    }

    fn candidate(&self) -> Self {
        let mut candidate = self.clone();
        candidate.reservations = None;
        candidate
    }

    fn commit_candidate(&mut self, mut candidate: Self, incoming: Option<&Multiaddr>) -> bool {
        while !self
            .reservations
            .as_mut()
            .expect("budgeted collection")
            .revise(&candidate.addrs)
        {
            let Some(victim) = incoming.and_then(|address| candidate.eviction_candidate(address))
            else {
                return false;
            };
            candidate.addrs.remove(victim);
        }
        candidate.reservations = self.reservations.take();
        *self = candidate;
        true
    }

    pub(crate) fn protect(&mut self, addr: &Multiaddr) -> bool {
        let Some(retained) = self.addrs.iter().find(|address| *address == addr) else {
            return false;
        };
        self.protected.insert(retained.clone());
        true
    }

    pub(crate) fn is_protected(&self) -> bool {
        !self.protected.is_empty()
    }

    /// Gets a reference to the first address in the list.
    pub fn first(&self) -> &Multiaddr {
        &self.addrs[0]
    }

    /// Returns an iterator over the addresses.
    pub fn iter(&self) -> impl Iterator<Item = &Multiaddr> {
        self.addrs.iter()
    }

    /// Returns the number of addresses in the list.
    pub fn len(&self) -> usize {
        self.addrs.len()
    }

    /// Converts the addresses into a `Vec`.
    pub fn into_vec(self) -> Vec<Multiaddr> {
        self.addrs.into_vec()
    }

    /// Removes the given address from the list.
    ///
    /// Returns `Ok(())` if the address is either not in the list or was found and
    /// removed. Returns `Err(())` if the address is the last remaining address,
    /// which cannot be removed.
    ///
    /// An address should only be removed if is determined to be invalid or
    /// otherwise unreachable.
    #[allow(clippy::result_unit_err)]
    pub fn remove(&mut self, addr: &Multiaddr) -> Result<(), ()> {
        if self.reservations.is_some() {
            let mut candidate = self.candidate();
            candidate.remove(addr)?;
            assert!(
                self.commit_candidate(candidate, None),
                "removal cannot increase usage"
            );
            return Ok(());
        }
        if self.addrs.len() == 1 && self.addrs[0] == *addr {
            return Err(());
        }

        if let Some(pos) = self.addrs.iter().position(|a| a == addr) {
            self.addrs.remove(pos);
            self.protected.remove(addr);
            if self.addrs.len() <= self.addrs.inline_size() {
                self.addrs.shrink_to_fit();
            }
        }

        Ok(())
    }

    /// Adds a new address to the end of the list.
    ///
    /// Returns true if the address was added, false otherwise (i.e. if the
    /// address is already in the list or rejected by the configured limits).
    /// At capacity, rotates the oldest unprotected address, preferring the
    /// incoming address's category, then a category with multiple addresses.
    pub fn insert(&mut self, addr: Multiaddr) -> bool {
        if self.reservations.is_some() {
            let mut candidate = self.candidate();
            let inserted = candidate.insert(addr.clone());
            return self.commit_candidate(candidate, Some(&addr)) && inserted;
        }
        if !self.limits.accepts(&addr) {
            return false;
        }
        if let Some(index) = self.addrs.iter().position(|a| *a == addr) {
            // Refresh recency only for bounded collections, preserving the
            // upstream ordering contract when limits are not configured.
            if self.limits.count != usize::MAX {
                let address = self.addrs.remove(index);
                self.addrs.push(address);
            }
            return false;
        }
        if self.addrs.len() >= self.limits.count {
            let Some(victim) = self.eviction_candidate(&addr) else {
                return false;
            };
            self.addrs.remove(victim);
        }
        self.addrs.push(if self.normalize_buffers {
            normalized_address(&addr)
        } else {
            addr
        });
        true
    }

    fn eviction_candidate(&self, incoming: &Multiaddr) -> Option<usize> {
        let category = address_category(incoming);
        let mut category_counts = [0_usize; 3];
        for address in &self.addrs {
            category_counts[usize::from(address_category(address))] += 1;
        }
        self.addrs
            .iter()
            .enumerate()
            .filter(|(_, address)| *address != incoming && !self.protected.contains(*address))
            .min_by_key(|(_, address)| {
                let candidate = address_category(address);
                (
                    candidate != category,
                    category_counts[usize::from(candidate)] == 1,
                )
            })
            .map(|(index, _)| index)
    }

    /// Replaces an old address, or learns the new address while keeping a protected seed.
    ///
    /// Returns true if the old address was found and the new address is retained.
    /// Bounded collections refresh recency and collapse duplicate replacements.
    pub fn replace(&mut self, old: &Multiaddr, new: &Multiaddr) -> bool {
        if self.reservations.is_some() {
            let mut candidate = self.candidate();
            if !candidate.replace(old, new) {
                return false;
            }
            return self.commit_candidate(candidate, Some(new));
        }
        if !self.limits.accepts(new) {
            return false;
        }
        let Some(index) = self.addrs.iter().position(|a| a == old) else {
            return false;
        };
        if old == new {
            return true;
        }
        if self.protected.contains(old) {
            // A configured seed remains available even if one connection to
            // it migrates; the new endpoint is learned without replacing it.
            return self.insert(new.clone()) || self.addrs.contains(new);
        }
        if self.limits.count == usize::MAX {
            self.addrs[index] = if self.normalize_buffers {
                normalized_address(new)
            } else {
                new.clone()
            };
        } else {
            self.addrs.remove(index);
            self.insert(new.clone());
        }
        true
    }
}

fn address_category(address: &Multiaddr) -> u8 {
    if address.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
        return 2;
    }
    if address.iter().any(|p| match p {
        Protocol::Ip4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        Protocol::Ip6(ip) => ip.is_unique_local() || ip.is_loopback() || ip.is_unicast_link_local(),
        _ => false,
    }) {
        0
    } else {
        1
    }
}

impl fmt::Debug for Addresses {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.addrs.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_one_address_when_removing_different_one_returns_ok() {
        let mut addresses = make_addresses([tcp_addr(1234)]);

        let result = addresses.remove(&tcp_addr(4321));

        assert!(result.is_ok());
        assert_eq!(
            addresses.into_vec(),
            vec![tcp_addr(1234)],
            "`Addresses` to not change because we tried to remove a non-present address"
        );
    }

    #[test]
    fn given_one_address_when_removing_correct_one_returns_err() {
        let mut addresses = make_addresses([tcp_addr(1234)]);

        let result = addresses.remove(&tcp_addr(1234));

        assert!(result.is_err());
        assert_eq!(
            addresses.into_vec(),
            vec![tcp_addr(1234)],
            "`Addresses` to not be empty because it would have been the last address to be removed"
        );
    }

    #[test]
    fn given_many_addresses_when_removing_different_one_does_not_remove_and_returns_ok() {
        let mut addresses = make_addresses([tcp_addr(1234), tcp_addr(4321)]);

        let result = addresses.remove(&tcp_addr(5678));

        assert!(result.is_ok());
        assert_eq!(
            addresses.into_vec(),
            vec![tcp_addr(1234), tcp_addr(4321)],
            "`Addresses` to not change because we tried to remove a non-present address"
        );
    }

    #[test]
    fn given_many_addresses_when_removing_correct_one_removes_and_returns_ok() {
        let mut addresses = make_addresses([tcp_addr(1234), tcp_addr(4321)]);

        let result = addresses.remove(&tcp_addr(1234));

        assert!(result.is_ok());
        assert_eq!(
            addresses.into_vec(),
            vec![tcp_addr(4321)],
            "`Addresses to no longer contain address was present and then removed`"
        );
    }

    /// Helper function to easily initialize Addresses struct with multiple addresses.
    fn make_addresses(addresses: impl IntoIterator<Item = Multiaddr>) -> Addresses {
        Addresses {
            addrs: SmallVec::from_iter(addresses),
            limits: AddressLimits::default(),
            protected: HashSet::new(),
            reservations: None,
            normalize_buffers: false,
        }
    }

    /// Helper function to create a tcp Multiaddr with a specific port
    fn tcp_addr(port: u16) -> Multiaddr {
        format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap()
    }
}
