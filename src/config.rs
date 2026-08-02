use std::{
    fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
    str::FromStr,
};

use serde::Deserialize;

use crate::{
    PathKind, PeerId,
    identity::NodeIdentity,
    path::PathSet,
    route::{IpCidr, Route, RouteError, RouteTable, builtin_ipv4, builtin_ipv6},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Config {
    pub network: NetworkConfig,
    #[serde(default = "default_interface")]
    pub interface: InterfaceConfig,
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
    #[serde(default = "default_queue")]
    pub queue: QueueConfig,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn compile_routes(&self) -> Result<RouteTable, ConfigError> {
        let mut table = RouteTable::new();
        for peer in &self.peers {
            let owner = peer.peer_id()?;
            table.insert(Route {
                owner,
                prefix: IpCidr::new(IpAddr::V4(builtin_ipv4(owner)), 32)?,
                metric: 0,
            });
            table.insert(Route {
                owner,
                prefix: IpCidr::new(IpAddr::V6(builtin_ipv6(owner)), 128)?,
                metric: 0,
            });

            for route in &peer.routes {
                table.insert(Route {
                    owner,
                    prefix: route.prefix()?,
                    metric: route.metric,
                });
            }
        }

        Ok(table)
    }

    pub fn local_peer_id(&self) -> Result<PeerId, ConfigError> {
        PeerId::from_str(&self.network.local_peer).map_err(ConfigError::PeerId)
    }

    pub fn identity(&self) -> Result<NodeIdentity, ConfigError> {
        let private_key = self
            .network
            .private_key
            .clone()
            .ok_or(ConfigError::MissingPrivateKey)?;
        let identity = NodeIdentity::from_private_key(&private_key)?;
        if identity.peer_id != self.network.local_peer {
            return Err(ConfigError::IdentityPeerMismatch {
                expected: self.network.local_peer.clone(),
                actual: identity.peer_id,
            });
        }

        Ok(identity)
    }

    pub fn listen_multiaddrs(&self) -> Result<Vec<libp2p::Multiaddr>, ConfigError> {
        parse_multiaddrs(&self.network.listen_addresses)
    }

    pub fn bootstrap_multiaddrs(
        &self,
    ) -> Result<Vec<(libp2p::PeerId, libp2p::Multiaddr)>, ConfigError> {
        self.network
            .bootstrap_peers
            .iter()
            .map(BootstrapPeerConfig::peer_address)
            .collect()
    }

    pub fn peer_multiaddrs(&self) -> Result<Vec<(libp2p::PeerId, libp2p::Multiaddr)>, ConfigError> {
        self.peers
            .iter()
            .flat_map(|peer| peer.addresses.iter().map(|address| (&peer.id, address)))
            .map(|(peer, address)| parse_peer_address(peer, address))
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct NetworkConfig {
    pub name: String,
    pub local_peer: String,
    #[serde(default)]
    pub private_key: Option<String>,
    #[serde(default)]
    pub listen_addresses: Vec<String>,
    #[serde(default)]
    pub bootstrap_peers: Vec<BootstrapPeerConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct BootstrapPeerConfig {
    pub id: String,
    pub address: String,
}

impl BootstrapPeerConfig {
    pub fn peer_address(&self) -> Result<(libp2p::PeerId, libp2p::Multiaddr), ConfigError> {
        parse_peer_address(&self.id, &self.address)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct InterfaceConfig {
    pub name: String,
    pub mtu: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PeerConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub addresses: Vec<String>,
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
}

impl PeerConfig {
    pub fn peer_id(&self) -> Result<PeerId, ConfigError> {
        PeerId::from_str(&self.id).map_err(ConfigError::PeerId)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct RouteConfig {
    pub prefix: String,
    #[serde(default)]
    pub metric: u16,
}

impl RouteConfig {
    pub fn prefix(&self) -> Result<IpCidr, ConfigError> {
        parse_cidr(&self.prefix).map_err(ConfigError::RoutePrefix)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub struct QueueConfig {
    pub max_packets_per_peer: usize,
    pub max_bytes_per_peer: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDefaults {
    pub preferred_path: PathKind,
    pub fallback_paths: [PathKind; 3],
    pub initial_mtu: u16,
}

impl Default for RuntimeDefaults {
    fn default() -> Self {
        Self {
            preferred_path: PathKind::DirectQuicDatagram,
            fallback_paths: [
                PathKind::DirectQuicStream,
                PathKind::DirectTcpStream,
                PathKind::CircuitRelay,
            ],
            initial_mtu: 1_280,
        }
    }
}

#[must_use]
pub fn empty_path_state() -> PathSet {
    PathSet::new()
}

#[derive(Debug)]
pub enum ConfigError {
    Io(io::Error),
    Json(serde_json::Error),
    Identity(crate::identity::IdentityError),
    MissingPrivateKey,
    IdentityPeerMismatch { expected: String, actual: String },
    PeerId(crate::PeerIdParseError),
    Libp2pPeerId(libp2p::identity::ParseError),
    Multiaddr(libp2p::multiaddr::Error),
    RoutePrefix(RoutePrefixError),
    Route(RouteError),
}

impl From<io::Error> for ConfigError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ConfigError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<RouteError> for ConfigError {
    fn from(error: RouteError) -> Self {
        Self::Route(error)
    }
}

impl From<crate::identity::IdentityError> for ConfigError {
    fn from(error: crate::identity::IdentityError) -> Self {
        Self::Identity(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RoutePrefixError {
    MissingSlash,
    InvalidAddress(String),
    InvalidPrefix(String),
    InvalidPrefixLength(RouteError),
}

fn default_queue() -> QueueConfig {
    QueueConfig {
        max_packets_per_peer: 256,
        max_bytes_per_peer: 512 * 1_024,
    }
}

fn default_interface() -> InterfaceConfig {
    InterfaceConfig {
        name: "hs0".to_owned(),
        mtu: 1_280,
    }
}

fn parse_cidr(input: &str) -> Result<IpCidr, RoutePrefixError> {
    let (address, prefix) = input
        .split_once('/')
        .ok_or(RoutePrefixError::MissingSlash)?;
    let address = address
        .parse::<IpAddr>()
        .map_err(|_| RoutePrefixError::InvalidAddress(address.to_owned()))?;
    let prefix_len = prefix
        .parse::<u8>()
        .map_err(|_| RoutePrefixError::InvalidPrefix(prefix.to_owned()))?;

    IpCidr::new(normalize_address(address, prefix_len), prefix_len)
        .map_err(RoutePrefixError::InvalidPrefixLength)
}

fn parse_multiaddrs(input: &[String]) -> Result<Vec<libp2p::Multiaddr>, ConfigError> {
    input
        .iter()
        .map(|address| address.parse().map_err(ConfigError::Multiaddr))
        .collect()
}

fn parse_peer_address(
    peer: &str,
    address: &str,
) -> Result<(libp2p::PeerId, libp2p::Multiaddr), ConfigError> {
    Ok((
        peer.parse().map_err(ConfigError::Libp2pPeerId)?,
        address.parse().map_err(ConfigError::Multiaddr)?,
    ))
}

fn normalize_address(address: IpAddr, prefix_len: u8) -> IpAddr {
    match address {
        IpAddr::V4(address) => {
            let mask = ipv4_mask(prefix_len);
            IpAddr::V4(Ipv4Addr::from(u32::from(address) & mask))
        }
        IpAddr::V6(address) => {
            let mask = ipv6_mask(prefix_len);
            IpAddr::V6(Ipv6Addr::from(u128::from(address) & mask))
        }
    }
}

fn ipv4_mask(prefix_len: u8) -> u32 {
    if prefix_len == 0 {
        0
    } else {
        u32::MAX
            .checked_shl(32 - u32::from(prefix_len))
            .unwrap_or(0)
    }
}

fn ipv6_mask(prefix_len: u8) -> u128 {
    if prefix_len == 0 {
        0
    } else {
        u128::MAX
            .checked_shl(128 - u32::from(prefix_len))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_compiles_builtin_and_advertised_routes() {
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                listen_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: "0101010101010101010101010101010101010101010101010101010101010101".to_owned(),
                name: Some("one".to_owned()),
                addresses: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.42.7.99/24".to_owned(),
                    metric: 50,
                }],
            }],
            queue: default_queue(),
        };

        let owner = config.peers[0].peer_id().expect("valid peer");
        let table = config.compile_routes().expect("routes should compile");

        assert_eq!(
            table
                .resolve(IpAddr::V4(Ipv4Addr::new(10, 42, 7, 1)))
                .map(|route| route.owner),
            Some(owner)
        );
        assert_eq!(
            table.authorize_source(owner, IpAddr::V4(builtin_ipv4(owner))),
            Ok(())
        );
    }

    #[test]
    fn config_parses_libp2p_addresses() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let remote = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key.clone()),
                listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: remote.to_string(),
                    address: "/ip4/127.0.0.1/udp/4001/quic-v1".to_owned(),
                }],
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: Some("remote".to_owned()),
                addresses: vec!["/ip4/127.0.0.1/tcp/4001".to_owned()],
                routes: Vec::new(),
            }],
            queue: default_queue(),
        };

        assert_eq!(config.identity().expect("identity"), identity);
        assert_eq!(config.listen_multiaddrs().expect("listen").len(), 1);
        assert_eq!(config.bootstrap_multiaddrs().expect("bootstrap").len(), 1);
        assert_eq!(config.peer_multiaddrs().expect("peer addresses").len(), 1);
    }
}
