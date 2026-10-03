use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use libp2p::{Multiaddr, PeerId, multiaddr::Protocol};

pub(super) const MAX_DISCOVERED_ADDRESS_BYTES: usize = 2048;
pub(super) const MAX_DISCOVERED_ADDRESSES_PER_PEER: usize = 64;
const MAX_DISCOVERED_ADDRESSES: usize = 4096;
const MAX_INFRASTRUCTURE_ADDRESSES: usize = 512;

#[derive(Debug, Eq, PartialEq)]
struct Entry {
    peer: PeerId,
    address: Multiaddr,
    overlay: bool,
    category: u8,
    last_seen: Instant,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct AddressRetention {
    entries: Vec<Entry>,
    protected: HashSet<(PeerId, Multiaddr)>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Admission {
    RejectedTooLarge,
    Retained { evicted: Vec<(PeerId, Multiaddr)> },
}

impl AddressRetention {
    pub(super) fn overlay_peers(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.entries
            .iter()
            .filter(|entry| entry.overlay)
            .map(|entry| entry.peer)
    }

    /// Forget addresses and static protection together when their peer loses retention eligibility.
    pub(super) fn retain_peers(
        &mut self,
        mut eligible: impl FnMut(PeerId) -> bool,
    ) -> Vec<(PeerId, Multiaddr)> {
        let peers = self
            .entries
            .iter()
            .map(|entry| entry.peer)
            .chain(self.protected.iter().map(|(peer, _)| *peer))
            .collect::<HashSet<_>>();
        let removed_peers = peers
            .into_iter()
            .filter(|peer| !eligible(*peer))
            .collect::<HashSet<_>>();
        let mut removed = HashSet::new();
        self.entries.retain(|entry| {
            if removed_peers.contains(&entry.peer) {
                removed.insert((entry.peer, entry.address.clone()));
                false
            } else {
                true
            }
        });
        self.protected.retain(|(peer, address)| {
            if removed_peers.contains(peer) {
                removed.insert((*peer, address.clone()));
                false
            } else {
                true
            }
        });
        removed.into_iter().collect()
    }

    pub(super) fn protect(&mut self, peer: PeerId, address: Multiaddr) {
        self.protected.insert((peer, canonical(peer, address)));
    }

    pub(super) fn is_protected(&self, peer: PeerId, address: &Multiaddr) -> bool {
        self.protected
            .contains(&(peer, canonical(peer, address.clone())))
    }

    pub(super) fn admit(
        &mut self,
        peer: PeerId,
        address: Multiaddr,
        overlay: bool,
        now: Instant,
    ) -> Admission {
        let address = canonical(peer, address);
        if address.len() > MAX_DISCOVERED_ADDRESS_BYTES {
            return Admission::RejectedTooLarge;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.peer == peer && entry.address == address)
        {
            if self.entries[index].overlay == overlay {
                self.entries[index].last_seen = now;
                return Admission::Retained {
                    evicted: Vec::new(),
                };
            }
            self.entries.remove(index);
        }
        let peer_count = self
            .entries
            .iter()
            .filter(|entry| entry.peer == peer)
            .count();
        let address_category = category(&address);
        let victim = if peer_count >= MAX_DISCOVERED_ADDRESSES_PER_PEER {
            // Rotate within the incoming category first so address churn cannot
            // displace every LAN or relay alternative of the same peer.
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.peer == peer)
                .min_by_key(|(_, entry)| (entry.category != address_category, entry.last_seen))
                .map(|(index, _)| index)
        } else {
            None
        };
        let mut evicted = Vec::new();
        if let Some(index) = victim {
            let entry = self.entries.remove(index);
            evicted.push((entry.peer, entry.address));
        }
        if self
            .entries
            .iter()
            .filter(|entry| entry.overlay == overlay)
            .count()
            >= if overlay {
                MAX_DISCOVERED_ADDRESSES
            } else {
                MAX_INFRASTRUCTURE_ADDRESSES
            }
        {
            // Prefer this peer's oldest entry over taking another peer's budget.
            if let Some((index, _)) = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry.overlay == overlay)
                .min_by_key(|(_, entry)| (entry.peer != peer, entry.last_seen))
            {
                let entry = self.entries.remove(index);
                evicted.push((entry.peer, entry.address));
            }
        }
        self.entries.push(Entry {
            peer,
            address,
            overlay,
            category: address_category,
            last_seen: now,
        });
        Admission::Retained { evicted }
    }

    pub(super) fn expire(&mut self, now: Instant, ttl: Duration) -> Vec<(PeerId, Multiaddr)> {
        let mut expired = Vec::new();
        self.entries.retain(|entry| {
            if now.saturating_duration_since(entry.last_seen) <= ttl {
                return true;
            }
            expired.push((entry.peer, entry.address.clone()));
            false
        });
        expired
    }
}

pub(super) fn canonical(peer: PeerId, address: Multiaddr) -> Multiaddr {
    address.clone().with_p2p(peer).unwrap_or(address)
}

fn category(address: &Multiaddr) -> u8 {
    if address
        .iter()
        .any(|part| matches!(part, Protocol::P2pCircuit))
    {
        return 2;
    }
    if address.iter().any(|part| match part {
        Protocol::Ip4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        Protocol::Ip6(ip) => ip.is_unique_local() || ip.is_loopback() || ip.is_unicast_link_local(),
        _ => false,
    }) {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(port: usize) -> Multiaddr {
        format!("/ip4/11.252.0.2/tcp/{port}").parse().unwrap()
    }

    #[test]
    fn authority_pruning_forgets_removed_peers_and_static_protection_once() {
        let revoked = PeerId::random();
        let survivor = PeerId::random();
        let infrastructure = PeerId::random();
        let now = Instant::now();
        let mut book = AddressRetention::default();
        book.admit(revoked, address(4001), true, now);
        book.admit(revoked, address(4002), false, now);
        book.protect(revoked, address(4001));
        book.protect(revoked, address(4003));
        for peer in [survivor, infrastructure] {
            book.admit(peer, address(4004), peer == survivor, now);
            book.protect(peer, address(4004));
        }
        let mut checked = HashSet::new();
        let removed = book.retain_peers(|peer| {
            assert!(
                checked.insert(peer),
                "eligibility is evaluated once per retained peer"
            );
            peer != revoked
        });
        assert_eq!(checked.len(), 3);
        assert_eq!(removed.len(), 3);
        assert_eq!(
            removed.into_iter().collect::<HashSet<_>>(),
            [4001, 4002, 4003]
                .into_iter()
                .map(|port| (revoked, canonical(revoked, address(port))))
                .collect()
        );
        assert!(book.entries.iter().all(|entry| entry.peer != revoked));
        for port in [4001, 4002, 4003] {
            assert!(!book.is_protected(revoked, &address(port)));
        }
        assert_eq!(book.entries.len(), 2);
        assert_eq!(book.protected.len(), 2);
        for peer in [survivor, infrastructure] {
            assert!(book.is_protected(peer, &address(4004)));
        }
        assert!(book.retain_peers(|peer| peer != revoked).is_empty());
        // Re-admission starts without stale static protection or old addresses.
        assert_eq!(
            book.admit(revoked, address(4005), true, now),
            Admission::Retained { evicted: vec![] }
        );
        assert!(!book.is_protected(revoked, &address(4005)));
        assert_eq!(
            book.entries
                .iter()
                .filter(|entry| entry.peer == revoked)
                .count(),
            1
        );
    }

    #[test]
    fn pruning_protected_only_addresses_does_not_require_a_discovery_entry() {
        let peer = PeerId::random();
        let mut book = AddressRetention::default();
        book.protect(peer, address(4001));
        book.protect(peer, canonical(peer, address(4001)));
        assert_eq!(book.protected.len(), 1);
        assert_eq!(
            book.retain_peers(|_| false),
            vec![(peer, canonical(peer, address(4001)))]
        );
        assert!(book.protected.is_empty());
        assert!(book.entries.is_empty());
    }

    #[test]
    fn repeated_admission_and_pruning_retains_only_current_addresses() {
        let survivor = PeerId::random();
        let now = Instant::now();
        let mut book = AddressRetention::default();
        book.admit(survivor, address(4001), true, now);
        book.protect(survivor, address(4001));
        for cycle in 0..1_024 {
            let departing = PeerId::random();
            let candidate = address(4002 + cycle);
            book.admit(departing, candidate.clone(), true, now);
            book.protect(departing, candidate);
            assert_eq!(book.retain_peers(|peer| peer == survivor).len(), 1);
            assert_eq!(book.entries.len(), 1);
            assert_eq!(book.protected.len(), 1);
            assert_eq!(book.entries[0].peer, survivor);
        }
        assert!(book.is_protected(survivor, &address(4001)));
    }

    #[test]
    fn per_peer_rotation_preserves_lan_alternatives_and_refreshes() {
        let peer = PeerId::random();
        let now = Instant::now();
        let lan: Multiaddr = "/ip4/192.168.1.2/tcp/4001".parse().unwrap();
        let mut book = AddressRetention::default();
        book.admit(peer, lan.clone(), true, now);
        let relay = address(4002)
            .with(Protocol::P2p(PeerId::random()))
            .with(Protocol::P2pCircuit);
        book.admit(peer, relay.clone(), true, now);
        for port in 1..512 {
            book.admit(
                peer,
                address(port),
                true,
                now + Duration::from_secs(port as u64),
            );
        }
        assert_eq!(book.entries.len(), MAX_DISCOVERED_ADDRESSES_PER_PEER);
        assert!(
            book.entries
                .iter()
                .any(|entry| entry.address == canonical(peer, lan.clone()))
        );
        assert!(
            book.entries
                .iter()
                .any(|entry| entry.address == canonical(peer, relay.clone()))
        );
        assert_eq!(
            book.admit(peer, address(511), true, now + Duration::from_secs(600)),
            Admission::Retained {
                evicted: Vec::new()
            }
        );
        let expired = book.expire(now + Duration::from_secs(650), Duration::from_secs(100));
        assert_eq!(expired.len(), MAX_DISCOVERED_ADDRESSES_PER_PEER - 1);
        assert_eq!(book.entries.len(), 1);
    }

    #[test]
    fn global_limit_and_peer_budget_survive_many_peers() {
        let mut book = AddressRetention::default();
        let now = Instant::now();
        for _ in 0..MAX_DISCOVERED_ADDRESSES + 10 {
            book.admit(PeerId::random(), address(4001), true, now);
        }
        assert_eq!(book.entries.len(), MAX_DISCOVERED_ADDRESSES);
        let peer = book.entries.last().unwrap().peer;
        let victim = book.admit(peer, address(4002), true, now);
        assert!(
            matches!(victim, Admission::Retained { evicted } if evicted.len() == 1 && evicted[0].0 == peer)
        );
    }

    #[test]
    fn oversized_addresses_are_rejected_before_retention() {
        let mut book = AddressRetention::default();
        let peer = PeerId::random();
        let huge = Multiaddr::empty().with(Protocol::Dns(
            "a".repeat(MAX_DISCOVERED_ADDRESS_BYTES).into(),
        ));
        assert_eq!(
            book.admit(peer, huge, true, Instant::now()),
            Admission::RejectedTooLarge
        );
        assert!(book.entries.is_empty());
        book.protect(peer, address(4001));
        assert!(book.is_protected(peer, &canonical(peer, address(4001))));
    }

    #[test]
    fn infrastructure_churn_does_not_evict_overlay_addresses() {
        let mut book = AddressRetention::default();
        let now = Instant::now();
        let overlay = PeerId::random();
        book.admit(overlay, address(4001), true, now);
        for _ in 0..MAX_INFRASTRUCTURE_ADDRESSES + 10 {
            book.admit(PeerId::random(), address(4002), false, now);
        }
        assert_eq!(book.entries.len(), MAX_INFRASTRUCTURE_ADDRESSES + 1);
        assert!(book.entries.iter().any(|entry| entry.peer == overlay));
        let promoted = book.entries.last().unwrap().peer;
        book.admit(promoted, address(4002), true, now);
        assert_eq!(book.entries.iter().filter(|entry| entry.overlay).count(), 2);
    }

    #[test]
    fn entering_full_overlay_pool_enforces_both_limits() {
        let mut book = AddressRetention::default();
        let now = Instant::now();
        let peer = PeerId::random();
        for port in 1..=MAX_DISCOVERED_ADDRESSES_PER_PEER {
            book.admit(peer, address(port), false, now);
        }
        for _ in 0..MAX_DISCOVERED_ADDRESSES {
            book.admit(PeerId::random(), address(4001), true, now);
        }
        let admission = book.admit(peer, address(4002), true, now);
        assert!(matches!(admission, Admission::Retained { evicted } if evicted.len() == 2));
        assert_eq!(
            book.entries.iter().filter(|entry| entry.overlay).count(),
            MAX_DISCOVERED_ADDRESSES
        );
        assert_eq!(
            book.entries
                .iter()
                .filter(|entry| entry.peer == peer)
                .count(),
            MAX_DISCOVERED_ADDRESSES_PER_PEER
        );
    }
}
