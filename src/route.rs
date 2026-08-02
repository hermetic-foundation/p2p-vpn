use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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
            (IpAddr::V4(network), IpAddr::V4(address)) => prefix_matches(
                u128::from(u32::from(network)),
                u128::from(u32::from(address)),
                self.prefix_len,
            ),
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                prefix_matches(u128::from(network), u128::from(address), self.prefix_len)
            }
            _ => false,
        }
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
    InvalidPrefix { prefix_len: u8, max_prefix: u8 },
    UnauthorizedSource { peer: PeerId, source: IpAddr },
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

fn prefix_matches(network: u128, address: u128, prefix_len: u8) -> bool {
    let host_bits = 128 - u32::from(prefix_len);
    let mask = if prefix_len == 0 {
        0
    } else {
        u128::MAX.checked_shl(host_bits).unwrap_or(0)
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
    fn generated_ipv4_is_in_carrier_grade_nat_range() {
        let address = builtin_ipv4(peer(1)).octets();
        assert_eq!(&address[0..2], &[100, 64]);
    }
}
