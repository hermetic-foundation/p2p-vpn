use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use crate::PeerId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpCidr {
    address: IpAddr,
    prefix_len: u8,
}

impl IpCidr {
    pub fn new(address: IpAddr, prefix_len: u8) -> Result<Self, RouteError> {
        let max_prefix = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };

        if prefix_len > max_prefix {
            return Err(RouteError::InvalidPrefix {
                prefix_len,
                max_prefix,
            });
        }

        Ok(Self {
            address,
            prefix_len,
        })
    }

    #[must_use]
    pub const fn address(self) -> IpAddr {
        self.address
    }

    #[must_use]
    pub const fn prefix_len(self) -> u8 {
        self.prefix_len
    }

    #[must_use]
    pub fn contains(self, candidate: IpAddr) -> bool {
        match (self.address, candidate) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                prefix_matches(u32::from(network), u32::from(address), self.prefix_len)
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                prefix_matches(u128::from(network), u128::from(address), self.prefix_len)
            }
            _ => false,
        }
    }

    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        match (self.address, other.address) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => {
                self.contains(other.address) || other.contains(self.address)
            }
            _ => false,
        }
    }
}

impl fmt::Display for IpCidr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.address, self.prefix_len)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Route {
    pub owner: PeerId,
    pub prefix: IpCidr,
    pub metric: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteError {
    InvalidPrefix {
        prefix_len: u8,
        max_prefix: u8,
    },
    ConflictingOwnership {
        existing_owner: PeerId,
        new_owner: PeerId,
        existing_prefix: IpCidr,
        new_prefix: IpCidr,
    },
    UnauthorizedSource {
        peer: PeerId,
        source: IpAddr,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RouteTable {
    routes: Vec<Route>,
}

impl RouteTable {
    #[must_use]
    pub const fn new() -> Self {
        Self { routes: Vec::new() }
    }

    pub fn insert(&mut self, route: Route) {
        self.routes.push(route);
        self.sort_routes();
    }

    pub fn insert_authorized(&mut self, route: Route) -> Result<(), RouteError> {
        for existing in &self.routes {
            if existing.owner != route.owner && existing.prefix.overlaps(route.prefix) {
                return Err(RouteError::ConflictingOwnership {
                    existing_owner: existing.owner,
                    new_owner: route.owner,
                    existing_prefix: existing.prefix,
                    new_prefix: route.prefix,
                });
            }
        }

        self.routes.push(route);
        self.sort_routes();
        Ok(())
    }

    fn sort_routes(&mut self) {
        self.routes.sort_by(|left, right| {
            right
                .prefix
                .prefix_len()
                .cmp(&left.prefix.prefix_len())
                .then(left.metric.cmp(&right.metric))
                .then(left.owner.as_bytes().cmp(&right.owner.as_bytes()))
        });
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    #[must_use]
    pub fn routes(&self) -> &[Route] {
        &self.routes
    }

    #[must_use]
    pub fn resolve(&self, destination: IpAddr) -> Option<Route> {
        self.routes
            .iter()
            .copied()
            .find(|route| route.prefix.contains(destination))
    }

    pub fn authorize_source(&self, peer: PeerId, source: IpAddr) -> Result<(), RouteError> {
        if self
            .routes
            .iter()
            .any(|route| route.owner == peer && route.prefix.contains(source))
        {
            Ok(())
        } else {
            Err(RouteError::UnauthorizedSource { peer, source })
        }
    }
}

fn prefix_matches<T>(network: T, address: T, prefix_len: u8) -> bool
where
    T: Copy
        + From<u8>
        + std::ops::BitAnd<Output = T>
        + std::ops::Shl<u32, Output = T>
        + std::ops::Not<Output = T>
        + PartialEq,
{
    let width = u32::try_from(std::mem::size_of::<T>() * 8).expect("integer width fits u32");
    let host_bits = width - u32::from(prefix_len);
    let mask = if prefix_len == 0 {
        T::from(0)
    } else {
        !T::from(0) << host_bits
    };

    (network & mask) == (address & mask)
}

#[must_use]
pub fn builtin_ipv4(peer: PeerId) -> Ipv4Addr {
    let mut address = [100, 64, 0, 0];
    for (index, byte) in peer.as_bytes().iter().enumerate() {
        address[(index % 2) + 2] ^= byte;
    }
    Ipv4Addr::from(address)
}

#[must_use]
pub fn builtin_ipv6(peer: PeerId) -> Ipv6Addr {
    let mut address = *b"\xfd\0hyprspace\0\0\0\0\0";
    let net_id = net_id(peer);
    address[12..16].copy_from_slice(&net_id);
    Ipv6Addr::from(address)
}

#[must_use]
pub fn net_id(peer: PeerId) -> [u8; 4] {
    let mut id = [0xde, 0xad, 0xbe, 0xef];
    for (index, byte) in peer.as_bytes().iter().enumerate() {
        id[index % 4] ^= byte;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(seed: u8) -> PeerId {
        PeerId::from_bytes([seed; 32])
    }

    #[test]
    fn route_table_uses_longest_prefix_then_metric() {
        let mut table = RouteTable::new();
        table.insert(Route {
            owner: peer(1),
            prefix: IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0)), 8).unwrap(),
            metric: 10,
        });
        table.insert(Route {
            owner: peer(2),
            prefix: IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 4, 0, 0)), 16).unwrap(),
            metric: 100,
        });

        let route = table
            .resolve(IpAddr::V4(Ipv4Addr::new(10, 4, 9, 1)))
            .expect("route should resolve");

        assert_eq!(route.owner, peer(2));
    }

    #[test]
    fn ipv4_prefix_matching_respects_ipv4_width() {
        let prefix = IpCidr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 1, 2)), 32).unwrap();

        assert!(prefix.contains(IpAddr::V4(Ipv4Addr::new(100, 64, 1, 2))));
        assert!(!prefix.contains(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1))));
    }

    #[test]
    fn route_table_authorizes_source_by_owner() {
        let mut table = RouteTable::new();
        table.insert(Route {
            owner: peer(7),
            prefix: IpCidr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 9, 1)), 32).unwrap(),
            metric: 0,
        });

        assert_eq!(
            table.authorize_source(peer(7), IpAddr::V4(Ipv4Addr::new(100, 64, 9, 1))),
            Ok(())
        );

        assert_eq!(
            table.authorize_source(peer(8), IpAddr::V4(Ipv4Addr::new(100, 64, 9, 1))),
            Err(RouteError::UnauthorizedSource {
                peer: peer(8),
                source: IpAddr::V4(Ipv4Addr::new(100, 64, 9, 1))
            })
        );
    }

    #[test]
    fn cidr_overlap_detects_more_specific_prefixes() {
        let aggregate = IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 0)), 16).unwrap();
        let more_specific = IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 9, 0)), 24).unwrap();
        let disjoint = IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 43, 0, 0)), 16).unwrap();

        assert!(aggregate.overlaps(more_specific));
        assert!(more_specific.overlaps(aggregate));
        assert!(!aggregate.overlaps(disjoint));
    }

    #[test]
    fn route_table_rejects_cross_peer_prefix_overlap() {
        let mut table = RouteTable::new();
        let aggregate = IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 0)), 16).unwrap();
        let hijack = IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 9, 0)), 24).unwrap();

        table
            .insert_authorized(Route {
                owner: peer(7),
                prefix: aggregate,
                metric: 0,
            })
            .expect("aggregate route");

        assert_eq!(
            table.insert_authorized(Route {
                owner: peer(8),
                prefix: hijack,
                metric: 0,
            }),
            Err(RouteError::ConflictingOwnership {
                existing_owner: peer(7),
                new_owner: peer(8),
                existing_prefix: aggregate,
                new_prefix: hijack,
            })
        );
    }

    #[test]
    fn route_table_allows_same_peer_prefix_overlap() {
        let mut table = RouteTable::new();

        table
            .insert_authorized(Route {
                owner: peer(7),
                prefix: IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 0)), 16).unwrap(),
                metric: 10,
            })
            .expect("aggregate route");
        table
            .insert_authorized(Route {
                owner: peer(7),
                prefix: IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 9, 0)), 24).unwrap(),
                metric: 0,
            })
            .expect("same owner more-specific route");
    }

    #[test]
    fn generated_ipv4_is_in_carrier_grade_nat_range() {
        let address = builtin_ipv4(peer(1)).octets();
        assert_eq!(&address[0..2], &[100, 64]);
    }
}
