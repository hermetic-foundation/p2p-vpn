use std::{
    fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
    str::FromStr,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{
    PathKind, PeerId,
    identity::NodeIdentity,
    path::PathSet,
    route::{IpCidr, Route, RouteError, RouteTable, builtin_ipv4, builtin_ipv6},
    wire::MAX_PAYLOAD_LEN,
};

use libp2p::multiaddr::Protocol;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Config {
    pub network: NetworkConfig,
    #[serde(default = "default_interface")]
    pub interface: InterfaceConfig,
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
    #[serde(default = "default_queue")]
    pub queue: QueueConfig,
    #[serde(default)]
    pub resources: ResourceConfig,
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
            table.insert_authorized(Route {
                owner,
                prefix: IpCidr::new(IpAddr::V4(builtin_ipv4(owner)), 32)?,
                metric: 0,
            })?;
            table.insert_authorized(Route {
                owner,
                prefix: IpCidr::new(IpAddr::V6(builtin_ipv6(owner)), 128)?,
                metric: 0,
            })?;

            for route in &peer.routes {
                table.insert_authorized(Route {
                    owner,
                    prefix: route.prefix()?,
                    metric: route.metric,
                })?;
            }
        }

        Ok(table)
    }

    pub fn validate_runtime(&self) -> Result<(), ConfigError> {
        self.identity()?;
        self.compile_routes()?;
        self.listen_multiaddrs()?;
        self.external_multiaddrs()?;
        self.bootstrap_multiaddrs()?;
        self.peer_multiaddrs()?;
        self.relay_reservation_multiaddrs()?;
        Ok(())
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

    pub fn external_multiaddrs(&self) -> Result<Vec<libp2p::Multiaddr>, ConfigError> {
        parse_multiaddrs(&self.network.external_addresses)
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

    pub fn relay_reservation_multiaddrs(&self) -> Result<Vec<libp2p::Multiaddr>, ConfigError> {
        parse_relay_reservation_multiaddrs(&self.network.relay.reservations)
    }

    #[must_use]
    pub fn effective_packet_mtu(&self) -> u16 {
        effective_packet_mtu(self.interface.mtu)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkConfig {
    pub name: String,
    pub local_peer: String,
    #[serde(default)]
    pub private_key: Option<String>,
    #[serde(default)]
    pub listen_addresses: Vec<String>,
    #[serde(default)]
    pub external_addresses: Vec<String>,
    #[serde(default)]
    pub bootstrap_peers: Vec<BootstrapPeerConfig>,
    #[serde(default = "default_discovery")]
    pub discovery: DiscoveryConfig,
    #[serde(default)]
    pub relay: RelayConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BootstrapPeerConfig {
    pub id: String,
    pub address: String,
}

impl BootstrapPeerConfig {
    pub fn peer_address(&self) -> Result<(libp2p::PeerId, libp2p::Multiaddr), ConfigError> {
        parse_peer_address(&self.id, &self.address)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiscoveryConfig {
    #[serde(default = "default_true")]
    pub mdns: bool,
    #[serde(default = "default_true")]
    pub kademlia: bool,
    #[serde(default = "default_true")]
    pub dcutr: bool,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        default_discovery()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RelayConfig {
    #[serde(default)]
    pub server: bool,
    #[serde(default)]
    pub reservations: Vec<String>,
    #[serde(default)]
    pub resources: RelayResourceConfig,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RelayResourceConfig {
    #[serde(default = "default_max_relay_reservations")]
    pub max_reservations: usize,
    #[serde(default = "default_max_relay_reservations_per_peer")]
    pub max_reservations_per_peer: usize,
    #[serde(default = "default_relay_reservation_duration_secs")]
    pub reservation_duration_secs: u64,
    #[serde(default = "default_max_relay_circuits")]
    pub max_circuits: usize,
    #[serde(default = "default_max_relay_circuits_per_peer")]
    pub max_circuits_per_peer: usize,
    #[serde(default = "default_relay_max_circuit_duration_secs")]
    pub max_circuit_duration_secs: u64,
    #[serde(default = "default_relay_max_circuit_bytes")]
    pub max_circuit_bytes: u64,
}

impl Default for RelayResourceConfig {
    fn default() -> Self {
        Self {
            max_reservations: default_max_relay_reservations(),
            max_reservations_per_peer: default_max_relay_reservations_per_peer(),
            reservation_duration_secs: default_relay_reservation_duration_secs(),
            max_circuits: default_max_relay_circuits(),
            max_circuits_per_peer: default_max_relay_circuits_per_peer(),
            max_circuit_duration_secs: default_relay_max_circuit_duration_secs(),
            max_circuit_bytes: default_relay_max_circuit_bytes(),
        }
    }
}

impl RelayResourceConfig {
    #[must_use]
    pub fn to_libp2p_config(self) -> libp2p::relay::Config {
        libp2p::relay::Config {
            max_reservations: self.max_reservations,
            max_reservations_per_peer: self.max_reservations_per_peer,
            reservation_duration: Duration::from_secs(self.reservation_duration_secs),
            max_circuits: self.max_circuits,
            max_circuits_per_peer: self.max_circuits_per_peer,
            max_circuit_duration: Duration::from_secs(self.max_circuit_duration_secs),
            max_circuit_bytes: self.max_circuit_bytes,
            ..libp2p::relay::Config::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InterfaceConfig {
    pub name: String,
    pub mtu: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QueueConfig {
    pub max_packets_per_peer: usize,
    pub max_bytes_per_peer: usize,
    #[serde(default = "default_max_packet_age_millis")]
    pub max_packet_age_millis: u64,
}

impl QueueConfig {
    #[must_use]
    pub fn max_packet_age(self) -> std::time::Duration {
        std::time::Duration::from_millis(self.max_packet_age_millis.max(1))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceConfig {
    #[serde(default = "default_max_concurrent_packet_streams")]
    pub max_concurrent_packet_streams: usize,
    #[serde(default = "default_max_concurrent_control_streams")]
    pub max_concurrent_control_streams: usize,
    #[serde(default = "default_max_pending_incoming_connections")]
    pub max_pending_incoming_connections: u32,
    #[serde(default = "default_max_pending_outgoing_connections")]
    pub max_pending_outgoing_connections: u32,
    #[serde(default = "default_max_established_incoming_connections")]
    pub max_established_incoming_connections: u32,
    #[serde(default = "default_max_established_outgoing_connections")]
    pub max_established_outgoing_connections: u32,
    #[serde(default = "default_max_established_connections_per_peer")]
    pub max_established_connections_per_peer: u32,
    #[serde(default = "default_max_established_connections")]
    pub max_established_connections: u32,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        default_resources()
    }
}

impl ResourceConfig {
    #[must_use]
    pub fn packet_stream_limit(self) -> usize {
        self.max_concurrent_packet_streams.max(1)
    }

    #[must_use]
    pub fn control_stream_limit(self) -> usize {
        self.max_concurrent_control_streams.max(1)
    }

    #[must_use]
    pub fn to_connection_limits(self) -> libp2p::connection_limits::ConnectionLimits {
        libp2p::connection_limits::ConnectionLimits::default()
            .with_max_pending_incoming(Some(self.max_pending_incoming_connections))
            .with_max_pending_outgoing(Some(self.max_pending_outgoing_connections))
            .with_max_established_incoming(Some(self.max_established_incoming_connections))
            .with_max_established_outgoing(Some(self.max_established_outgoing_connections))
            .with_max_established_per_peer(Some(self.max_established_connections_per_peer))
            .with_max_established(Some(self.max_established_connections))
    }
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
            preferred_path: PathKind::DirectQuicStream,
            fallback_paths: [
                PathKind::DirectTcpStream,
                PathKind::CircuitRelay,
                PathKind::DirectQuicDatagram,
            ],
            initial_mtu: 1_280,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitPeer {
    pub id: String,
    pub address: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitConfigTemplate {
    pub identity: NodeIdentity,
    pub network_name: String,
    pub interface_name: String,
    pub mtu: u16,
    pub listen_addresses: Vec<String>,
    pub external_addresses: Vec<String>,
    pub bootstrap_peers: Vec<InitPeer>,
    pub peers: Vec<InitPeer>,
    pub discovery: DiscoveryConfig,
    pub relay: RelayConfig,
}

impl InitConfigTemplate {
    #[must_use]
    pub fn into_config(self) -> Config {
        let mut peers: Vec<PeerConfig> = Vec::new();
        for peer in self.peers {
            upsert_peer(&mut peers, peer);
        }

        Config {
            network: NetworkConfig {
                name: self.network_name,
                local_peer: self.identity.peer_id,
                private_key: Some(self.identity.private_key),
                listen_addresses: self.listen_addresses,
                external_addresses: self.external_addresses,
                bootstrap_peers: self
                    .bootstrap_peers
                    .into_iter()
                    .filter_map(|peer| {
                        peer.address.map(|address| BootstrapPeerConfig {
                            id: peer.id,
                            address,
                        })
                    })
                    .collect(),
                discovery: self.discovery,
                relay: self.relay,
            },
            interface: InterfaceConfig {
                name: self.interface_name,
                mtu: self.mtu,
            },
            peers,
            queue: default_queue(),
            resources: default_resources(),
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
    Address(AddressValidationError),
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddressValidationError {
    PeerIdMismatch {
        expected: String,
        actual: String,
        address: String,
    },
    MissingRelayCircuit {
        address: String,
    },
    MissingRelayPeer {
        address: String,
    },
    UnexpectedRelayTarget {
        address: String,
    },
}

fn default_queue() -> QueueConfig {
    QueueConfig {
        max_packets_per_peer: 256,
        max_bytes_per_peer: 512 * 1_024,
        max_packet_age_millis: default_max_packet_age_millis(),
    }
}

const fn default_max_packet_age_millis() -> u64 {
    1_000
}

const fn default_resources() -> ResourceConfig {
    ResourceConfig {
        max_concurrent_packet_streams: default_max_concurrent_packet_streams(),
        max_concurrent_control_streams: default_max_concurrent_control_streams(),
        max_pending_incoming_connections: default_max_pending_incoming_connections(),
        max_pending_outgoing_connections: default_max_pending_outgoing_connections(),
        max_established_incoming_connections: default_max_established_incoming_connections(),
        max_established_outgoing_connections: default_max_established_outgoing_connections(),
        max_established_connections_per_peer: default_max_established_connections_per_peer(),
        max_established_connections: default_max_established_connections(),
    }
}

const fn default_max_concurrent_packet_streams() -> usize {
    256
}

const fn default_max_concurrent_control_streams() -> usize {
    64
}

const fn default_max_pending_incoming_connections() -> u32 {
    64
}

const fn default_max_pending_outgoing_connections() -> u32 {
    64
}

const fn default_max_established_incoming_connections() -> u32 {
    256
}

const fn default_max_established_outgoing_connections() -> u32 {
    256
}

const fn default_max_established_connections_per_peer() -> u32 {
    8
}

const fn default_max_established_connections() -> u32 {
    512
}

const fn default_true() -> bool {
    true
}

const fn default_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        mdns: true,
        kademlia: true,
        dcutr: true,
    }
}

const fn default_max_relay_reservations() -> usize {
    128
}

const fn default_max_relay_reservations_per_peer() -> usize {
    4
}

const fn default_relay_reservation_duration_secs() -> u64 {
    60 * 60
}

const fn default_max_relay_circuits() -> usize {
    16
}

const fn default_max_relay_circuits_per_peer() -> usize {
    4
}

const fn default_relay_max_circuit_duration_secs() -> u64 {
    2 * 60
}

const fn default_relay_max_circuit_bytes() -> u64 {
    1 << 17
}

fn default_interface() -> InterfaceConfig {
    InterfaceConfig {
        name: "hs0".to_owned(),
        mtu: 1_280,
    }
}

#[must_use]
pub fn effective_packet_mtu(configured_mtu: u16) -> u16 {
    configured_mtu.min(u16::try_from(MAX_PAYLOAD_LEN).expect("wire payload length fits u16"))
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
    let peer = peer.parse().map_err(ConfigError::Libp2pPeerId)?;
    let address = address.parse().map_err(ConfigError::Multiaddr)?;
    validate_peer_multiaddr(peer, &address)?;
    Ok((peer, address))
}

fn parse_relay_reservation_multiaddrs(
    input: &[String],
) -> Result<Vec<libp2p::Multiaddr>, ConfigError> {
    input
        .iter()
        .map(|address| {
            let multiaddr = address.parse().map_err(ConfigError::Multiaddr)?;
            validate_relay_reservation_multiaddr(&multiaddr)?;
            Ok(multiaddr)
        })
        .collect()
}

fn validate_peer_multiaddr(
    expected: libp2p::PeerId,
    address: &libp2p::Multiaddr,
) -> Result<(), ConfigError> {
    let Some(actual) = peer_address_target(address) else {
        return Ok(());
    };
    if actual == expected {
        return Ok(());
    }

    Err(ConfigError::Address(
        AddressValidationError::PeerIdMismatch {
            expected: expected.to_string(),
            actual: actual.to_string(),
            address: address.to_string(),
        },
    ))
}

fn validate_relay_reservation_multiaddr(address: &libp2p::Multiaddr) -> Result<(), ConfigError> {
    let mut relay_peer = None;
    let mut saw_circuit = false;
    for protocol in address {
        match protocol {
            Protocol::P2p(_) if saw_circuit => {
                return Err(ConfigError::Address(
                    AddressValidationError::UnexpectedRelayTarget {
                        address: address.to_string(),
                    },
                ));
            }
            Protocol::P2p(peer) => relay_peer = Some(peer),
            Protocol::P2pCircuit if relay_peer.is_some() => saw_circuit = true,
            Protocol::P2pCircuit => {
                return Err(ConfigError::Address(
                    AddressValidationError::MissingRelayPeer {
                        address: address.to_string(),
                    },
                ));
            }
            _ => {}
        }
    }

    if saw_circuit {
        Ok(())
    } else {
        Err(ConfigError::Address(
            AddressValidationError::MissingRelayCircuit {
                address: address.to_string(),
            },
        ))
    }
}

fn peer_address_target(address: &libp2p::Multiaddr) -> Option<libp2p::PeerId> {
    let mut direct_target = None;
    let mut relayed_target = None;
    let mut after_circuit = false;

    for protocol in address {
        match protocol {
            Protocol::P2p(peer) if after_circuit => relayed_target = Some(peer),
            Protocol::P2p(peer) => direct_target = Some(peer),
            Protocol::P2pCircuit => after_circuit = true,
            _ => {}
        }
    }

    if after_circuit {
        relayed_target
    } else {
        direct_target
    }
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

fn upsert_peer(peers: &mut Vec<PeerConfig>, peer: InitPeer) {
    if let Some(existing) = peers.iter_mut().find(|existing| existing.id == peer.id) {
        if let Some(address) = peer.address
            && !existing.addresses.contains(&address)
        {
            existing.addresses.push(address);
        }
        return;
    }

    peers.push(PeerConfig {
        id: peer.id,
        name: None,
        addresses: peer.address.into_iter().collect(),
        routes: Vec::new(),
    });
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
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
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
            resources: default_resources(),
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
    fn effective_packet_mtu_is_capped_by_wire_payload_length() {
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: default_queue(),
            resources: default_resources(),
        };

        assert_eq!(config.effective_packet_mtu(), 1280);

        config.interface.mtu = u16::MAX;
        assert_eq!(
            config.effective_packet_mtu(),
            u16::try_from(MAX_PAYLOAD_LEN).expect("wire payload length fits u16")
        );
    }

    #[test]
    fn config_rejects_cross_peer_route_overlap() {
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![
                PeerConfig {
                    id: "0100000000000000000000000000000000000000000000000000000000000000"
                        .to_owned(),
                    name: Some("one".to_owned()),
                    addresses: Vec::new(),
                    routes: vec![RouteConfig {
                        prefix: "10.42.0.0/16".to_owned(),
                        metric: 50,
                    }],
                },
                PeerConfig {
                    id: "0200000000000000000000000000000000000000000000000000000000000000"
                        .to_owned(),
                    name: Some("two".to_owned()),
                    addresses: Vec::new(),
                    routes: vec![RouteConfig {
                        prefix: "10.42.9.0/24".to_owned(),
                        metric: 10,
                    }],
                },
            ],
            queue: default_queue(),
            resources: default_resources(),
        };

        assert!(matches!(
            config.compile_routes(),
            Err(ConfigError::Route(RouteError::ConflictingOwnership { .. }))
        ));

        config.peers[1].routes[0].prefix = "10.43.9.0/24".to_owned();
        assert!(config.compile_routes().is_ok());
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
                external_addresses: vec!["/ip4/203.0.113.10/udp/4001/quic-v1".to_owned()],
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: remote.to_string(),
                    address: "/ip4/127.0.0.1/udp/4001/quic-v1".to_owned(),
                }],
                discovery: DiscoveryConfig {
                    mdns: false,
                    kademlia: true,
                    dcutr: true,
                },
                relay: RelayConfig {
                    server: true,
                    reservations: vec![format!("/ip4/127.0.0.1/tcp/4001/p2p/{remote}/p2p-circuit")],
                    resources: RelayResourceConfig::default(),
                },
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
            resources: default_resources(),
        };

        assert_eq!(config.identity().expect("identity"), identity);
        assert_eq!(config.listen_multiaddrs().expect("listen").len(), 1);
        assert_eq!(config.external_multiaddrs().expect("external").len(), 1);
        assert_eq!(config.bootstrap_multiaddrs().expect("bootstrap").len(), 1);
        assert_eq!(config.peer_multiaddrs().expect("peer addresses").len(), 1);
        assert_eq!(
            config
                .relay_reservation_multiaddrs()
                .expect("relay reservations")
                .len(),
            1
        );
        assert!(config.network.relay.server);
        assert!(!config.network.discovery.mdns);
        assert_eq!(config.resources.packet_stream_limit(), 256);
    }

    #[test]
    fn runtime_validation_requires_matching_private_key() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let other = NodeIdentity::generate_ed25519().expect("other identity");
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id,
                private_key: Some(other.private_key),
                listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: default_queue(),
            resources: default_resources(),
        };

        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::IdentityPeerMismatch { .. })
        ));
    }

    #[test]
    fn runtime_validation_checks_transport_addresses() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let remote = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                listen_addresses: vec!["not-a-multiaddr".to_owned()],
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: default_queue(),
            resources: default_resources(),
        };

        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Multiaddr(_))
        ));

        config.network.listen_addresses = Vec::new();
        config.network.external_addresses = vec!["not-a-multiaddr".to_owned()];
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Multiaddr(_))
        ));

        config.network.external_addresses = Vec::new();
        config.peers[0].addresses = vec!["not-a-multiaddr".to_owned()];
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Multiaddr(_))
        ));
    }

    #[test]
    fn runtime_validation_rejects_peer_address_identity_mismatch() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let configured = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let other = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: configured.to_string(),
                    address: format!("/ip4/127.0.0.1/tcp/4001/p2p/{other}"),
                }],
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: configured.to_string(),
                name: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: default_queue(),
            resources: default_resources(),
        };

        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Address(
                AddressValidationError::PeerIdMismatch { .. }
            ))
        ));

        config.network.bootstrap_peers.clear();
        config.peers[0].addresses = vec![format!(
            "/ip4/127.0.0.1/tcp/4001/p2p/{other}/p2p-circuit/p2p/{other}"
        )];
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Address(
                AddressValidationError::PeerIdMismatch { .. }
            ))
        ));

        config.peers[0].addresses =
            vec![format!("/ip4/127.0.0.1/tcp/4001/p2p/{other}/p2p-circuit")];
        assert!(config.validate_runtime().is_ok());
    }

    #[test]
    fn runtime_validation_requires_relay_reservation_shape() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let relay = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig {
                    server: false,
                    reservations: vec!["/ip4/127.0.0.1/tcp/4001".to_owned()],
                    resources: RelayResourceConfig::default(),
                },
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: default_queue(),
            resources: default_resources(),
        };

        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Address(
                AddressValidationError::MissingRelayCircuit { .. }
            ))
        ));

        config.network.relay.reservations = vec!["/ip4/127.0.0.1/tcp/4001/p2p-circuit".to_owned()];
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Address(
                AddressValidationError::MissingRelayPeer { .. }
            ))
        ));

        config.network.relay.reservations = vec![format!(
            "/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{relay}"
        )];
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Address(
                AddressValidationError::UnexpectedRelayTarget { .. }
            ))
        ));

        config.network.relay.reservations =
            vec![format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit")];
        assert!(config.validate_runtime().is_ok());
    }

    #[test]
    fn init_config_template_generates_loadable_runtime_config() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let remote = libp2p::identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id();
        let config = InitConfigTemplate {
            identity: identity.clone(),
            network_name: "lab".to_owned(),
            interface_name: "hs-lab".to_owned(),
            mtu: 1_400,
            listen_addresses: vec![
                "/ip4/0.0.0.0/tcp/0".to_owned(),
                "/ip4/0.0.0.0/udp/0/quic-v1".to_owned(),
            ],
            external_addresses: vec!["/dns4/node-a.example.net/udp/4001/quic-v1".to_owned()],
            bootstrap_peers: vec![InitPeer {
                id: remote.to_string(),
                address: Some("/ip4/127.0.0.1/tcp/4001".to_owned()),
            }],
            peers: vec![
                InitPeer {
                    id: remote.to_string(),
                    address: Some("/ip4/127.0.0.1/tcp/4001".to_owned()),
                },
                InitPeer {
                    id: remote.to_string(),
                    address: Some("/ip4/127.0.0.1/udp/4001/quic-v1".to_owned()),
                },
                InitPeer {
                    id: remote.to_string(),
                    address: Some("/ip4/127.0.0.1/tcp/4001".to_owned()),
                },
            ],
            discovery: DiscoveryConfig {
                mdns: true,
                kademlia: false,
                dcutr: true,
            },
            relay: RelayConfig {
                server: true,
                reservations: vec![format!("/ip4/127.0.0.1/tcp/4002/p2p/{remote}/p2p-circuit")],
                resources: RelayResourceConfig::default(),
            },
        }
        .into_config();
        let rendered = serde_json::to_string_pretty(&config).expect("rendered config");
        let decoded = serde_json::from_str::<Config>(&rendered).expect("decoded config");

        assert_eq!(decoded.network.local_peer, identity.peer_id);
        assert_eq!(
            decoded.network.private_key.as_deref(),
            Some(identity.private_key.as_str())
        );
        assert_eq!(decoded.interface.name, "hs-lab");
        assert_eq!(decoded.interface.mtu, 1_400);
        assert_eq!(decoded.network.external_addresses.len(), 1);
        assert_eq!(decoded.network.bootstrap_peers.len(), 1);
        assert!(decoded.network.relay.server);
        assert_eq!(
            decoded.network.relay.resources,
            RelayResourceConfig::default()
        );
        assert!(!decoded.network.discovery.kademlia);
        assert_eq!(decoded.peers.len(), 1);
        assert_eq!(decoded.peers[0].addresses.len(), 2);
        assert!(decoded.identity().is_ok());
        assert!(decoded.listen_multiaddrs().is_ok());
        assert!(decoded.bootstrap_multiaddrs().is_ok());
        assert_eq!(decoded.peer_multiaddrs().expect("peer addresses").len(), 2);
        assert!(decoded.relay_reservation_multiaddrs().is_ok());
    }

    #[test]
    fn relay_resource_config_defaults_when_missing() {
        let config = serde_json::from_str::<Config>(
            r#"{
              "network": {
                "name": "dev",
                "local_peer": "0000000000000000000000000000000000000000000000000000000000000000",
                "relay": {
                  "server": true,
                  "reservations": []
                }
              },
              "interface": {
                "name": "hs0",
                "mtu": 1280
              }
            }"#,
        )
        .expect("config");

        assert!(config.network.relay.server);
        assert_eq!(
            config.network.relay.resources,
            RelayResourceConfig::default()
        );
    }

    #[test]
    fn relay_resource_config_maps_to_libp2p_relay_limits() {
        let resources = RelayResourceConfig {
            max_reservations: 17,
            max_reservations_per_peer: 3,
            reservation_duration_secs: 45,
            max_circuits: 19,
            max_circuits_per_peer: 5,
            max_circuit_duration_secs: 23,
            max_circuit_bytes: 4096,
        };

        let relay = resources.to_libp2p_config();

        assert_eq!(relay.max_reservations, 17);
        assert_eq!(relay.max_reservations_per_peer, 3);
        assert_eq!(
            relay.reservation_duration,
            std::time::Duration::from_secs(45)
        );
        assert_eq!(relay.max_circuits, 19);
        assert_eq!(relay.max_circuits_per_peer, 5);
        assert_eq!(
            relay.max_circuit_duration,
            std::time::Duration::from_secs(23)
        );
        assert_eq!(relay.max_circuit_bytes, 4096);
    }

    #[test]
    fn resource_config_defaults_and_clamps_stream_limits() {
        let config = serde_json::from_str::<Config>(
            r#"{
              "network": {
                "name": "dev",
                "local_peer": "0000000000000000000000000000000000000000000000000000000000000000"
              },
              "interface": {
                "name": "hs0",
                "mtu": 1280
              }
            }"#,
        )
        .expect("config");

        assert_eq!(config.resources.packet_stream_limit(), 256);
        assert_eq!(config.resources.control_stream_limit(), 64);
        assert_eq!(config.resources.max_pending_incoming_connections, 64);
        assert_eq!(config.resources.max_pending_outgoing_connections, 64);
        assert_eq!(config.resources.max_established_incoming_connections, 256);
        assert_eq!(config.resources.max_established_outgoing_connections, 256);
        assert_eq!(config.resources.max_established_connections_per_peer, 8);
        assert_eq!(config.resources.max_established_connections, 512);
        assert_eq!(config.queue.max_packet_age_millis, 1_000);

        let config = serde_json::from_str::<Config>(
            r#"{
              "network": {
                "name": "dev",
                "local_peer": "0000000000000000000000000000000000000000000000000000000000000000"
              },
              "interface": {
                "name": "hs0",
                "mtu": 1280
              },
              "resources": {
                "max_concurrent_control_streams": 0,
                "max_concurrent_packet_streams": 0,
                "max_pending_incoming_connections": 3,
                "max_pending_outgoing_connections": 4,
                "max_established_incoming_connections": 5,
                "max_established_outgoing_connections": 6,
                "max_established_connections_per_peer": 7,
                "max_established_connections": 8
              },
              "queue": {
                "max_packets_per_peer": 256,
                "max_bytes_per_peer": 524288,
                "max_packet_age_millis": 0
              }
            }"#,
        )
        .expect("config");

        assert_eq!(config.resources.packet_stream_limit(), 1);
        assert_eq!(config.resources.control_stream_limit(), 1);
        assert_eq!(config.resources.max_pending_incoming_connections, 3);
        assert_eq!(config.resources.max_pending_outgoing_connections, 4);
        assert_eq!(config.resources.max_established_incoming_connections, 5);
        assert_eq!(config.resources.max_established_outgoing_connections, 6);
        assert_eq!(config.resources.max_established_connections_per_peer, 7);
        assert_eq!(config.resources.max_established_connections, 8);
        assert_eq!(
            config.queue.max_packet_age(),
            std::time::Duration::from_millis(1)
        );
    }
}
