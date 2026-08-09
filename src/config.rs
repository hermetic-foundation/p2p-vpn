use std::{
    fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{
    PathKind, PeerId,
    identity::NodeIdentity,
    membership::{
        EffectiveMembership, MembershipRecordError, MembershipRole, SignedMembershipRecord,
        effective_membership_at, validate_membership_records_at,
    },
    path::PathSet,
    route::{IpCidr, Route, RouteError, RouteTable, builtin_ipv4, builtin_ipv6},
    runtime::packet_plane::PACKET_PLANE_MAX_PAYLOAD_LEN,
    wire::MAX_PAYLOAD_LEN,
};

use libp2p::multiaddr::Protocol;

pub const PRIVATE_KADEMLIA_PROTOCOL: &str = "/p2p-vpn/kad/1";
pub const PUBLIC_IPFS_KADEMLIA_PROTOCOL: &str = "/ipfs/kad/1.0.0";
pub const PUBLIC_IPFS_BOOTSTRAP_PEERS: &[(&str, &str)] = &[
    (
        "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    ),
    (
        "QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    ),
    (
        "QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5Zhtx6BHJX9KiKNN6tpvbUcqanj75Nb",
    ),
    (
        "QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
        "/dnsaddr/bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
    ),
    (
        "QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
        "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
    ),
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Config {
    pub network: NetworkConfig,
    #[serde(
        default = "default_interface",
        skip_serializing_if = "is_default_interface"
    )]
    pub interface: InterfaceConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<PeerConfig>,
    #[serde(default = "default_queue", skip_serializing_if = "is_default_queue")]
    pub queue: QueueConfig,
    #[serde(default, skip_serializing_if = "is_default_resources")]
    pub resources: ResourceConfig,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn compile_routes(&self) -> Result<RouteTable, ConfigError> {
        self.compile_routes_with_member_records(&self.network.member_records)
    }

    pub fn compile_routes_with_member_records(
        &self,
        member_records: &[SignedMembershipRecord],
    ) -> Result<RouteTable, ConfigError> {
        let mut table = RouteTable::new();
        let local_peer = self.local_peer_id()?;
        table.insert_authorized(Route {
            owner: local_peer,
            prefix: IpCidr::new(IpAddr::V4(builtin_ipv4(local_peer)), 32)?,
            metric: 0,
        })?;
        table.insert_authorized(Route {
            owner: local_peer,
            prefix: IpCidr::new(IpAddr::V6(builtin_ipv6(local_peer)), 128)?,
            metric: 0,
        })?;
        if let Some(vpn_ip) = &self.network.vpn_ip {
            table.insert_authorized(Route {
                owner: local_peer,
                prefix: vpn_ip_host_route(vpn_ip)?,
                metric: 0,
            })?;
        }
        for route in &self.network.routes {
            table.insert_authorized(Route {
                owner: local_peer,
                prefix: route.prefix()?,
                metric: route.metric,
            })?;
        }

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

            if let Some(vpn_ip) = &peer.vpn_ip {
                table.insert_authorized(Route {
                    owner,
                    prefix: vpn_ip_host_route(vpn_ip)?,
                    metric: 0,
                })?;
            }
            for route in &peer.routes {
                table.insert_authorized(Route {
                    owner,
                    prefix: route.prefix()?,
                    metric: route.metric,
                })?;
            }
        }

        let effective_membership =
            effective_membership_at(member_records, &self.network.name, current_unix_seconds()?)?;
        for member in effective_membership.overlay_members() {
            table.insert_authorized(Route {
                owner: member.peer,
                prefix: IpCidr::new(IpAddr::V4(builtin_ipv4(member.peer)), 32)?,
                metric: 0,
            })?;
            table.insert_authorized(Route {
                owner: member.peer,
                prefix: IpCidr::new(IpAddr::V6(builtin_ipv6(member.peer)), 128)?,
                metric: 0,
            })?;

            if member.has_role(MembershipRole::RouteAuthority) {
                for route in &member.route_grants {
                    table.insert_authorized(Route {
                        owner: member.peer,
                        prefix: route.prefix()?,
                        metric: route.metric,
                    })?;
                }
            }
        }

        Ok(table)
    }

    pub fn validate_runtime(&self) -> Result<(), ConfigError> {
        self.identity()?;
        self.membership_key_bytes()?;
        self.previous_membership_tags()?;
        self.validate_membership_records()?;
        self.compile_routes()?;
        self.validate_interface()?;
        self.validate_resources()?;
        self.listen_multiaddrs()?;
        self.external_multiaddrs()?;
        self.bootstrap_multiaddrs()?;
        self.peer_multiaddrs()?;
        self.relay_reservation_multiaddrs()?;
        self.packet_plane_listen_addrs()?;
        self.packet_plane_external_endpoints()?;
        self.packet_plane_quic_listen_addrs()?;
        self.packet_plane_quic_external_endpoints()?;
        self.validate_packet_plane()?;
        self.validate_peer_reachability()?;
        self.validate_discovery()?;
        validate_kademlia_protocol(&self.network.discovery.kademlia_protocol)?;
        Ok(())
    }

    fn validate_interface(&self) -> Result<(), ConfigError> {
        if self.interface.mtu == 0 {
            return Err(ConfigError::Interface(InterfaceValidationError::ZeroMtu));
        }

        Ok(())
    }

    fn validate_peer_reachability(&self) -> Result<(), ConfigError> {
        if self.network.discovery.mdns || self.network.discovery.kademlia {
            return Ok(());
        }

        if let Some(peer) = self.peers.iter().find(|peer| peer.addresses.is_empty()) {
            return Err(ConfigError::Address(
                AddressValidationError::UnreachablePeer {
                    peer: peer.id.clone(),
                },
            ));
        }

        Ok(())
    }

    fn validate_discovery(&self) -> Result<(), ConfigError> {
        if !self.network.discovery.kademlia
            && self.network.discovery.kademlia_provider_advertisement
        {
            return Err(ConfigError::Discovery(
                DiscoveryValidationError::ProviderAdvertisementWithoutKademlia,
            ));
        }

        Ok(())
    }

    fn validate_resources(&self) -> Result<(), ConfigError> {
        if self.queue.max_packets_per_peer == 0 {
            return Err(ConfigError::Resource(
                ResourceValidationError::EmptyQueuePackets,
            ));
        }
        if self.queue.max_bytes_per_peer == 0 {
            return Err(ConfigError::Resource(
                ResourceValidationError::EmptyQueueBytes,
            ));
        }
        if self.resources.max_concurrent_control_streams == 0 {
            return Err(ConfigError::Resource(
                ResourceValidationError::NoConcurrentControlStreams,
            ));
        }
        if self.resources.max_concurrent_packet_streams == 0 {
            return Err(ConfigError::Resource(
                ResourceValidationError::NoConcurrentPacketStreams,
            ));
        }
        if self.resources.max_pending_incoming_connections == 0 {
            return Err(ConfigError::Resource(
                ResourceValidationError::NoPendingIncomingConnections,
            ));
        }
        if self.resources.max_pending_outgoing_connections == 0 {
            return Err(ConfigError::Resource(
                ResourceValidationError::NoPendingOutgoingConnections,
            ));
        }
        if self.resources.max_established_connections_per_peer == 0 {
            return Err(ConfigError::Resource(
                ResourceValidationError::NoEstablishedConnectionsPerPeer,
            ));
        }
        if self.resources.max_established_connections == 0 {
            return Err(ConfigError::Resource(
                ResourceValidationError::NoEstablishedConnections,
            ));
        }
        if self.resources.max_inbound_packets_per_peer_per_second == 0 {
            return Err(ConfigError::Resource(
                ResourceValidationError::NoInboundPacketsPerPeerPerSecond,
            ));
        }
        validate_auto_relay_policy(self.network.relay.auto).map_err(ConfigError::Resource)?;
        if self.network.relay.server {
            validate_relay_server_resources(self.network.relay.resources)
                .map_err(ConfigError::Resource)?;
        }

        Ok(())
    }

    fn validate_packet_plane(&self) -> Result<(), ConfigError> {
        if self.network.packet_plane.session_ttl_seconds == 0 {
            return Err(ConfigError::PacketPlane(
                PacketPlaneValidationError::NoSessionTtl,
            ));
        }
        if self.network.packet_plane.max_replay_windows_per_session == 0 {
            return Err(ConfigError::PacketPlane(
                PacketPlaneValidationError::NoReplayWindows,
            ));
        }
        if self.network.packet_plane.quic_listen.len() > 1 {
            return Err(ConfigError::PacketPlane(
                PacketPlaneValidationError::TooManyQuicListeners {
                    actual: self.network.packet_plane.quic_listen.len(),
                    max: 1,
                },
            ));
        }

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

    pub fn membership_key_bytes(&self) -> Result<Option<Vec<u8>>, ConfigError> {
        self.network
            .membership_key
            .as_deref()
            .map(decode_membership_key)
            .transpose()
    }

    pub fn membership_tag(&self) -> Result<Option<String>, ConfigError> {
        let Some(key) = self.membership_key_bytes()? else {
            return Ok(None);
        };
        Ok(Some(membership_tag(&self.network.name, &key)))
    }

    pub fn previous_membership_tags(&self) -> Result<Vec<String>, ConfigError> {
        if self.network.previous_membership_tags.is_empty() {
            return Ok(Vec::new());
        }
        if self.network.membership_key.is_none() {
            return Err(ConfigError::PreviousMembershipTagsWithoutMembershipKey);
        }
        for tag in &self.network.previous_membership_tags {
            validate_membership_tag(tag)?;
        }

        Ok(self.network.previous_membership_tags.clone())
    }

    pub fn validate_membership_records(&self) -> Result<(), ConfigError> {
        validate_membership_records_at(
            &self.network.member_records,
            &self.network.name,
            current_unix_seconds()?,
        )?;
        Ok(())
    }

    pub fn effective_membership(&self) -> Result<EffectiveMembership, ConfigError> {
        Ok(effective_membership_at(
            &self.network.member_records,
            &self.network.name,
            current_unix_seconds()?,
        )?)
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

    pub fn effective_bootstrap_multiaddrs(
        &self,
    ) -> Result<Vec<(libp2p::PeerId, libp2p::Multiaddr)>, ConfigError> {
        let mut peers = self.bootstrap_multiaddrs()?;
        if self.uses_public_ipfs_bootstrap_defaults() {
            for peer in public_ipfs_bootstrap_peer_configs() {
                let address = peer.peer_address()?;
                if peers.iter().any(|existing| existing == &address) {
                    continue;
                }
                peers.push(address);
            }
        }
        Ok(peers)
    }

    pub fn uses_public_ipfs_bootstrap_defaults(&self) -> bool {
        self.network.discovery.kademlia
            && self.network.discovery.kademlia_protocol == PUBLIC_IPFS_KADEMLIA_PROTOCOL
    }

    pub fn peer_multiaddrs(&self) -> Result<Vec<(libp2p::PeerId, libp2p::Multiaddr)>, ConfigError> {
        self.peers
            .iter()
            .map(PeerConfig::peer_addresses)
            .collect::<Result<Vec<_>, _>>()
            .map(|addresses| addresses.into_iter().flatten().collect())
    }

    pub fn peer_address_count(&self) -> Result<usize, ConfigError> {
        self.peers
            .iter()
            .map(PeerConfig::peer_addresses)
            .try_fold(0_usize, |count, addresses| {
                addresses.map(|addresses| count + addresses.len())
            })
    }

    pub fn relay_reservation_multiaddrs(&self) -> Result<Vec<libp2p::Multiaddr>, ConfigError> {
        parse_relay_reservation_multiaddrs(&self.network.relay.reservations)
    }

    pub fn packet_plane_listen_addrs(&self) -> Result<Vec<SocketAddr>, ConfigError> {
        parse_socket_addrs(&self.network.packet_plane.listen)
    }

    pub fn packet_plane_quic_listen_addrs(&self) -> Result<Vec<SocketAddr>, ConfigError> {
        parse_socket_addrs(&self.network.packet_plane.quic_listen)
    }

    pub fn packet_plane_external_endpoints(&self) -> Result<Vec<String>, ConfigError> {
        parse_packet_plane_endpoint_candidates(&self.network.packet_plane.external_endpoints)
    }

    pub fn packet_plane_quic_external_endpoints(&self) -> Result<Vec<String>, ConfigError> {
        parse_packet_plane_endpoint_candidates(&self.network.packet_plane.quic_external_endpoints)
    }

    pub fn packet_plane_endpoint_candidates(&self) -> Result<Vec<String>, ConfigError> {
        self.packet_plane_external_endpoints()
    }

    pub fn packet_plane_quic_endpoint_candidates(&self) -> Result<Vec<String>, ConfigError> {
        self.packet_plane_quic_external_endpoints()
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_membership_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_records: Vec<SignedMembershipRecord>,
    #[serde(default, alias = "vpnIp", skip_serializing_if = "Option::is_none")]
    pub vpn_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteConfig>,
    #[serde(
        default = "default_listen_addresses",
        skip_serializing_if = "is_default_listen_addresses"
    )]
    pub listen_addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bootstrap_peers: Vec<BootstrapPeerConfig>,
    #[serde(
        default = "default_discovery",
        skip_serializing_if = "is_default_discovery"
    )]
    pub discovery: DiscoveryConfig,
    #[serde(default, skip_serializing_if = "is_default_relay")]
    pub relay: RelayConfig,
    #[serde(default, skip_serializing_if = "is_default_packet_plane")]
    pub packet_plane: PacketPlaneConfig,
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

#[must_use]
pub fn public_ipfs_bootstrap_peer_configs() -> Vec<BootstrapPeerConfig> {
    PUBLIC_IPFS_BOOTSTRAP_PEERS
        .iter()
        .map(|(id, address)| BootstrapPeerConfig {
            id: (*id).to_owned(),
            address: (*address).to_owned(),
        })
        .collect()
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DiscoveryConfig {
    #[serde(default = "default_true")]
    pub mdns: bool,
    #[serde(default = "default_true")]
    pub kademlia: bool,
    #[serde(default = "default_true")]
    pub kademlia_provider_advertisement: bool,
    #[serde(default = "default_kademlia_protocol")]
    pub kademlia_protocol: String,
    #[serde(default = "default_true")]
    pub dcutr: bool,
    #[serde(default = "default_true")]
    pub autonat: bool,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        default_discovery()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RelayConfig {
    #[serde(default, skip_serializing_if = "is_false")]
    pub server: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reservations: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default_auto_relay")]
    pub auto: AutoRelayConfig,
    #[serde(default, skip_serializing_if = "is_default_relay_resources")]
    pub resources: RelayResourceConfig,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AutoRelayConfig {
    #[serde(default = "default_auto_relay_max_candidates")]
    pub max_candidates: usize,
    #[serde(default = "default_auto_relay_max_reservations")]
    pub max_reservations: usize,
    #[serde(default = "default_auto_relay_retry_interval_seconds")]
    pub retry_interval_seconds: u64,
}

impl Default for AutoRelayConfig {
    fn default() -> Self {
        Self {
            max_candidates: default_auto_relay_max_candidates(),
            max_reservations: default_auto_relay_max_reservations(),
            retry_interval_seconds: default_auto_relay_retry_interval_seconds(),
        }
    }
}

impl AutoRelayConfig {
    #[must_use]
    pub fn retry_interval(self) -> Duration {
        Duration::from_secs(self.retry_interval_seconds.max(1))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PacketPlaneConfig {
    #[serde(
        default = "default_packet_plane_listen",
        skip_serializing_if = "is_default_packet_plane_listen"
    )]
    pub listen: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_endpoints: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quic_listen: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quic_external_endpoints: Vec<String>,
    #[serde(default = "default_packet_plane_session_ttl_seconds")]
    pub session_ttl_seconds: u64,
    #[serde(default = "default_packet_plane_replay_windows_per_session")]
    pub max_replay_windows_per_session: usize,
}

impl Default for PacketPlaneConfig {
    fn default() -> Self {
        Self {
            listen: default_packet_plane_listen(),
            external_endpoints: Vec::new(),
            quic_listen: Vec::new(),
            quic_external_endpoints: Vec::new(),
            session_ttl_seconds: default_packet_plane_session_ttl_seconds(),
            max_replay_windows_per_session: default_packet_plane_replay_windows_per_session(),
        }
    }
}

impl PacketPlaneConfig {
    #[must_use]
    pub fn session_ttl(&self) -> Duration {
        Duration::from_secs(self.session_ttl_seconds.max(1))
    }

    #[must_use]
    pub fn replay_window_limit(&self) -> usize {
        self.max_replay_windows_per_session.max(1)
    }
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
    #[serde(default = "default_interface_name")]
    pub name: String,
    #[serde(default = "default_mtu")]
    pub mtu: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PeerConfig {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, alias = "vpnIp", skip_serializing_if = "Option::is_none")]
    pub vpn_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteConfig>,
}

impl PeerConfig {
    pub fn peer_id(&self) -> Result<PeerId, ConfigError> {
        PeerId::from_str(&self.id).map_err(ConfigError::PeerId)
    }

    pub fn peer_addresses(&self) -> Result<Vec<(libp2p::PeerId, libp2p::Multiaddr)>, ConfigError> {
        self.addresses
            .iter()
            .map(|address| parse_peer_address(&self.id, address))
            .chain(self.ip.iter().map(|ip| parse_peer_ip(&self.id, ip)))
            .collect()
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

impl Default for QueueConfig {
    fn default() -> Self {
        default_queue()
    }
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
    #[serde(default = "default_max_inbound_packets_per_peer_per_second")]
    pub max_inbound_packets_per_peer_per_second: u32,
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
    pub const fn inbound_packet_rate_limit(self) -> u32 {
        self.max_inbound_packets_per_peer_per_second
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
    pub vpn_ip: Option<String>,
    pub routes: Vec<RouteConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InitConfigTemplate {
    pub identity: NodeIdentity,
    pub network_name: String,
    pub membership_key: Option<String>,
    pub vpn_ip: Option<String>,
    pub local_routes: Vec<RouteConfig>,
    pub interface_name: String,
    pub mtu: u16,
    pub listen_addresses: Vec<String>,
    pub external_addresses: Vec<String>,
    pub packet_plane: PacketPlaneConfig,
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
                membership_key: self.membership_key,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: self.vpn_ip,
                routes: self.local_routes,
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
                packet_plane: self.packet_plane,
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

fn current_unix_seconds() -> Result<u64, ConfigError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ConfigError::SystemTimeBeforeEpoch)?
        .as_secs())
}

#[derive(Debug)]
pub enum ConfigError {
    Io(io::Error),
    Json(serde_json::Error),
    Identity(crate::identity::IdentityError),
    MissingPrivateKey,
    IdentityPeerMismatch { expected: String, actual: String },
    MembershipKey(MembershipKeyError),
    PreviousMembershipTag(MembershipTagError),
    PreviousMembershipTagsWithoutMembershipKey,
    PeerId(crate::PeerIdParseError),
    Libp2pPeerId(libp2p::identity::ParseError),
    Multiaddr(libp2p::multiaddr::Error),
    SocketAddr(std::net::AddrParseError),
    KademliaProtocol(String),
    Address(AddressValidationError),
    Interface(InterfaceValidationError),
    Resource(ResourceValidationError),
    Discovery(DiscoveryValidationError),
    PacketPlane(PacketPlaneValidationError),
    RoutePrefix(RoutePrefixError),
    Route(RouteError),
    MembershipRecord(MembershipRecordError),
    SystemTimeBeforeEpoch,
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

impl From<MembershipRecordError> for ConfigError {
    fn from(error: MembershipRecordError) -> Self {
        Self::MembershipRecord(error)
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
pub enum MembershipTagError {
    InvalidLength { actual: usize },
    InvalidBase64(String),
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
    InvalidPeerIp {
        peer: String,
        ip: String,
    },
    UnreachablePeer {
        peer: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterfaceValidationError {
    ZeroMtu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceValidationError {
    EmptyQueuePackets,
    EmptyQueueBytes,
    NoConcurrentControlStreams,
    NoConcurrentPacketStreams,
    NoPendingIncomingConnections,
    NoPendingOutgoingConnections,
    NoEstablishedConnectionsPerPeer,
    NoEstablishedConnections,
    NoInboundPacketsPerPeerPerSecond,
    RelayServerNoReservations,
    RelayServerNoReservationsPerPeer,
    RelayServerNoReservationDuration,
    RelayServerNoCircuits,
    RelayServerNoCircuitsPerPeer,
    RelayServerNoCircuitDuration,
    RelayServerNoCircuitBytes,
    NoAutoRelayRetryInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryValidationError {
    ProviderAdvertisementWithoutKademlia,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PacketPlaneValidationError {
    NoSessionTtl,
    NoReplayWindows,
    TooManyQuicListeners { actual: usize, max: usize },
    InvalidEndpoint(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MembershipKeyError {
    InvalidBase64,
    TooShort { actual: usize, minimum: usize },
}

const MIN_MEMBERSHIP_KEY_LEN: usize = 32;
const DEFAULT_DIRECT_TCP_PORT: u16 = 4001;

fn default_queue() -> QueueConfig {
    QueueConfig {
        max_packets_per_peer: 256,
        max_bytes_per_peer: 512 * 1_024,
        max_packet_age_millis: default_max_packet_age_millis(),
    }
}

const fn default_max_packet_age_millis() -> u64 {
    3_000
}

const fn default_resources() -> ResourceConfig {
    ResourceConfig {
        max_concurrent_packet_streams: default_max_concurrent_packet_streams(),
        max_concurrent_control_streams: default_max_concurrent_control_streams(),
        max_inbound_packets_per_peer_per_second: default_max_inbound_packets_per_peer_per_second(),
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

const fn default_max_inbound_packets_per_peer_per_second() -> u32 {
    4096
}

fn default_packet_plane_listen() -> Vec<String> {
    vec!["0.0.0.0:0".to_owned()]
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

#[must_use]
pub const fn default_packet_plane_session_ttl_seconds() -> u64 {
    10 * 60
}

#[must_use]
pub const fn default_packet_plane_replay_windows_per_session() -> usize {
    1024
}

fn default_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        mdns: true,
        kademlia: true,
        kademlia_provider_advertisement: true,
        kademlia_protocol: default_kademlia_protocol(),
        dcutr: true,
        autonat: true,
    }
}

fn default_kademlia_protocol() -> String {
    PUBLIC_IPFS_KADEMLIA_PROTOCOL.to_owned()
}

pub fn default_listen_addresses() -> Vec<String> {
    vec![format!("/ip4/0.0.0.0/tcp/{DEFAULT_DIRECT_TCP_PORT}")]
}

fn default_interface_name() -> String {
    "pv0".to_owned()
}

const fn default_mtu() -> u16 {
    1_280
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

#[must_use]
pub const fn default_auto_relay_max_candidates() -> usize {
    16
}

#[must_use]
pub const fn default_auto_relay_max_reservations() -> usize {
    2
}

#[must_use]
pub const fn default_auto_relay_retry_interval_seconds() -> u64 {
    30
}

fn validate_auto_relay_policy(auto: AutoRelayConfig) -> Result<(), ResourceValidationError> {
    if auto.retry_interval_seconds == 0 {
        return Err(ResourceValidationError::NoAutoRelayRetryInterval);
    }

    Ok(())
}

fn validate_relay_server_resources(
    resources: RelayResourceConfig,
) -> Result<(), ResourceValidationError> {
    if resources.max_reservations == 0 {
        return Err(ResourceValidationError::RelayServerNoReservations);
    }
    if resources.max_reservations_per_peer == 0 {
        return Err(ResourceValidationError::RelayServerNoReservationsPerPeer);
    }
    if resources.reservation_duration_secs == 0 {
        return Err(ResourceValidationError::RelayServerNoReservationDuration);
    }
    if resources.max_circuits == 0 {
        return Err(ResourceValidationError::RelayServerNoCircuits);
    }
    if resources.max_circuits_per_peer == 0 {
        return Err(ResourceValidationError::RelayServerNoCircuitsPerPeer);
    }
    if resources.max_circuit_duration_secs == 0 {
        return Err(ResourceValidationError::RelayServerNoCircuitDuration);
    }
    if resources.max_circuit_bytes == 0 {
        return Err(ResourceValidationError::RelayServerNoCircuitBytes);
    }

    Ok(())
}

fn default_interface() -> InterfaceConfig {
    InterfaceConfig {
        name: default_interface_name(),
        mtu: default_mtu(),
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_default_interface(value: &InterfaceConfig) -> bool {
    *value == default_interface()
}

fn is_default_queue(value: &QueueConfig) -> bool {
    *value == default_queue()
}

fn is_default_resources(value: &ResourceConfig) -> bool {
    *value == default_resources()
}

fn is_default_discovery(value: &DiscoveryConfig) -> bool {
    *value == default_discovery()
}

fn is_default_relay(value: &RelayConfig) -> bool {
    *value == RelayConfig::default()
}

fn is_default_auto_relay(value: &AutoRelayConfig) -> bool {
    *value == AutoRelayConfig::default()
}

fn is_default_relay_resources(value: &RelayResourceConfig) -> bool {
    *value == RelayResourceConfig::default()
}

fn is_default_packet_plane(value: &PacketPlaneConfig) -> bool {
    *value == PacketPlaneConfig::default()
}

fn is_default_packet_plane_listen(value: &[String]) -> bool {
    value == default_packet_plane_listen().as_slice()
}

fn is_default_listen_addresses(value: &[String]) -> bool {
    value == default_listen_addresses().as_slice()
}

fn decode_membership_key(input: &str) -> Result<Vec<u8>, ConfigError> {
    let key = base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|_| ConfigError::MembershipKey(MembershipKeyError::InvalidBase64))?;
    if key.len() < MIN_MEMBERSHIP_KEY_LEN {
        return Err(ConfigError::MembershipKey(MembershipKeyError::TooShort {
            actual: key.len(),
            minimum: MIN_MEMBERSHIP_KEY_LEN,
        }));
    }
    Ok(key)
}

fn validate_membership_tag(input: &str) -> Result<(), ConfigError> {
    let tag = base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|_| {
            ConfigError::PreviousMembershipTag(MembershipTagError::InvalidBase64(input.to_owned()))
        })?;
    if tag.len() != 32 {
        return Err(ConfigError::PreviousMembershipTag(
            MembershipTagError::InvalidLength { actual: tag.len() },
        ));
    }

    Ok(())
}

#[must_use]
pub fn membership_tag(network_name: &str, membership_key: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"p2p-vpn membership tag v1");
    hasher.update(network_name.as_bytes());
    hasher.update([0]);
    hasher.update(membership_key);
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

#[must_use]
pub fn effective_packet_mtu(configured_mtu: u16) -> u16 {
    configured_mtu.min(
        u16::try_from(MAX_PAYLOAD_LEN.min(PACKET_PLANE_MAX_PAYLOAD_LEN))
            .expect("packet payload length fits u16"),
    )
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

fn vpn_ip_host_route(input: &str) -> Result<IpCidr, ConfigError> {
    if input.contains('/') {
        return parse_cidr(input).map_err(ConfigError::RoutePrefix);
    }

    let address = input.parse::<IpAddr>().map_err(|_| {
        ConfigError::RoutePrefix(RoutePrefixError::InvalidAddress(input.to_owned()))
    })?;
    let prefix_len = match address {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };

    IpCidr::new(address, prefix_len).map_err(ConfigError::Route)
}

fn parse_multiaddrs(input: &[String]) -> Result<Vec<libp2p::Multiaddr>, ConfigError> {
    input
        .iter()
        .map(|address| address.parse().map_err(ConfigError::Multiaddr))
        .collect()
}

fn parse_socket_addrs(input: &[String]) -> Result<Vec<SocketAddr>, ConfigError> {
    input
        .iter()
        .map(|address| address.parse().map_err(ConfigError::SocketAddr))
        .collect()
}

fn parse_packet_plane_endpoint_candidates(input: &[String]) -> Result<Vec<String>, ConfigError> {
    input
        .iter()
        .map(|endpoint| {
            if validate_packet_plane_endpoint_candidate(endpoint) {
                Ok(endpoint.clone())
            } else {
                Err(ConfigError::PacketPlane(
                    PacketPlaneValidationError::InvalidEndpoint(endpoint.clone()),
                ))
            }
        })
        .collect()
}

#[must_use]
pub fn validate_packet_plane_endpoint_candidate(endpoint: &str) -> bool {
    if endpoint.parse::<SocketAddr>().is_ok() {
        return true;
    }
    let Some((host, port)) = endpoint.rsplit_once(':') else {
        return false;
    };
    if !valid_packet_plane_dns_host(host) {
        return false;
    }
    port.parse::<u16>().is_ok()
}

fn valid_packet_plane_dns_host(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.len() > 253 || host.contains(':') {
        return false;
    }
    host.split('.').all(|label| {
        let bytes = label.as_bytes();
        !bytes.is_empty()
            && bytes.len() <= 63
            && bytes[0].is_ascii_alphanumeric()
            && bytes[bytes.len() - 1].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    })
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

fn parse_peer_ip(peer: &str, ip: &str) -> Result<(libp2p::PeerId, libp2p::Multiaddr), ConfigError> {
    let peer_id = peer.parse().map_err(ConfigError::Libp2pPeerId)?;
    let address = match IpAddr::from_str(ip) {
        Ok(IpAddr::V4(address)) => libp2p::Multiaddr::empty()
            .with(Protocol::Ip4(address))
            .with(Protocol::Tcp(DEFAULT_DIRECT_TCP_PORT))
            .with(Protocol::P2p(peer_id)),
        Ok(IpAddr::V6(address)) => libp2p::Multiaddr::empty()
            .with(Protocol::Ip6(address))
            .with(Protocol::Tcp(DEFAULT_DIRECT_TCP_PORT))
            .with(Protocol::P2p(peer_id)),
        Err(_) => {
            return Err(ConfigError::Address(
                AddressValidationError::InvalidPeerIp {
                    peer: peer.to_owned(),
                    ip: ip.to_owned(),
                },
            ));
        }
    };

    Ok((peer_id, address))
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

fn validate_kademlia_protocol(protocol: &str) -> Result<(), ConfigError> {
    if protocol.starts_with('/') {
        Ok(())
    } else {
        Err(ConfigError::KademliaProtocol(protocol.to_owned()))
    }
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
        if existing.vpn_ip.is_none() {
            existing.vpn_ip = peer.vpn_ip;
        }
        for route in peer.routes {
            if !existing.routes.contains(&route) {
                existing.routes.push(route);
            }
        }
        return;
    }

    peers.push(PeerConfig {
        id: peer.id,
        name: None,
        ip: None,
        vpn_ip: peer.vpn_ip,
        addresses: peer.address.into_iter().collect(),
        routes: peer.routes,
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
    fn omitted_listen_addresses_default_to_direct_tcp_listener() {
        let config: Config = serde_json::from_str(
            r#"{
                "network": {
                    "name": "lab",
                    "local_peer": "0000000000000000000000000000000000000000000000000000000000000000"
                },
                "peers": [
                    { "id": "1111111111111111111111111111111111111111111111111111111111111111" }
                ]
            }"#,
        )
        .expect("minimal config");

        assert_eq!(
            config.network.listen_addresses,
            vec!["/ip4/0.0.0.0/tcp/4001".to_owned()]
        );
    }

    #[test]
    fn minimal_public_config_gets_runtime_bootstrap_defaults() {
        let config: Config = serde_json::from_str(
            r#"{
                "network": {
                    "name": "lab",
                    "local_peer": "0000000000000000000000000000000000000000000000000000000000000000"
                },
                "peers": [
                    { "id": "1111111111111111111111111111111111111111111111111111111111111111" }
                ]
            }"#,
        )
        .expect("minimal config");

        assert!(config.network.bootstrap_peers.is_empty());
        assert!(config.uses_public_ipfs_bootstrap_defaults());
        assert_eq!(
            config
                .effective_bootstrap_multiaddrs()
                .expect("effective bootstrap")
                .len(),
            PUBLIC_IPFS_BOOTSTRAP_PEERS.len()
        );
    }

    #[test]
    fn omitted_packet_plane_defaults_to_auto_udp_listener() {
        let config: Config = serde_json::from_str(
            r#"{
                "network": {
                    "name": "lab",
                    "local_peer": "0000000000000000000000000000000000000000000000000000000000000000"
                },
                "peers": [
                    { "id": "1111111111111111111111111111111111111111111111111111111111111111" }
                ]
            }"#,
        )
        .expect("minimal config");

        assert_eq!(
            config.network.packet_plane.listen,
            vec!["0.0.0.0:0".to_owned()]
        );
        assert_eq!(
            config.packet_plane_listen_addrs().expect("packet plane"),
            vec!["0.0.0.0:0".parse().expect("socket")]
        );
    }

    #[test]
    fn explicit_empty_packet_plane_listen_disables_udp_packet_plane() {
        let config: Config = serde_json::from_str(
            r#"{
                "network": {
                    "name": "lab",
                    "local_peer": "0000000000000000000000000000000000000000000000000000000000000000",
                    "packet_plane": {
                        "listen": []
                    }
                },
                "peers": [
                    { "id": "1111111111111111111111111111111111111111111111111111111111111111" }
                ]
            }"#,
        )
        .expect("minimal config");

        assert!(config.network.packet_plane.listen.is_empty());
        assert!(
            config
                .packet_plane_listen_addrs()
                .expect("packet plane")
                .is_empty()
        );
    }

    #[test]
    fn explicit_empty_listen_addresses_disable_listening() {
        let config: Config = serde_json::from_str(
            r#"{
                "network": {
                    "name": "lab",
                    "local_peer": "0000000000000000000000000000000000000000000000000000000000000000",
                    "listen_addresses": []
                },
                "peers": [
                    { "id": "1111111111111111111111111111111111111111111111111111111111111111" }
                ]
            }"#,
        )
        .expect("minimal config");

        assert!(config.network.listen_addresses.is_empty());
    }

    #[test]
    fn partial_interface_config_uses_default_mtu() {
        let config: Config = serde_json::from_str(
            r#"{
                "network": {
                    "name": "lab",
                    "local_peer": "0000000000000000000000000000000000000000000000000000000000000000"
                },
                "interface": { "name": "pvpnA" }
            }"#,
        )
        .expect("partial interface config");

        assert_eq!(config.interface.name, "pvpnA");
        assert_eq!(config.interface.mtu, 1280);
    }

    #[test]
    fn peer_ip_synthesizes_default_direct_tcp_multiaddr() {
        let config: Config = serde_json::from_str(
            r#"{
                "network": {
                    "name": "lab",
                    "local_peer": "0000000000000000000000000000000000000000000000000000000000000000"
                },
                "peers": [
                    {
                        "id": "12D3KooWP7jte6xTZJeG2bSDpxbsxHoyFZjHgBxyzvGkd3UGdheB",
                        "ip": "192.168.0.203"
                    }
                ]
            }"#,
        )
        .expect("minimal config");

        let addresses = config.peer_multiaddrs().expect("peer addresses");

        assert_eq!(addresses.len(), 1);
        assert_eq!(
            addresses[0].1.to_string(),
            "/ip4/192.168.0.203/tcp/4001/p2p/12D3KooWP7jte6xTZJeG2bSDpxbsxHoyFZjHgBxyzvGkd3UGdheB"
        );
        assert_eq!(config.peer_address_count().expect("address count"), 1);
    }

    #[test]
    fn peer_ip_rejects_invalid_ip_literals() {
        let config: Config = serde_json::from_str(
            r#"{
                "network": {
                    "name": "lab",
                    "local_peer": "0000000000000000000000000000000000000000000000000000000000000000"
                },
                "peers": [
                    {
                        "id": "12D3KooWP7jte6xTZJeG2bSDpxbsxHoyFZjHgBxyzvGkd3UGdheB",
                        "ip": "node-b.local"
                    }
                ]
            }"#,
        )
        .expect("minimal config");

        assert!(matches!(
            config.peer_multiaddrs(),
            Err(ConfigError::Address(
                AddressValidationError::InvalidPeerIp { .. }
            ))
        ));
    }

    #[test]
    fn membership_keys_are_validated_and_tagged() {
        let key = base64::engine::general_purpose::STANDARD.encode([7_u8; 32]);
        let mut config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                membership_key: Some(key),
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: default_queue(),
            resources: default_resources(),
        };

        let first = config.membership_tag().expect("membership tag");
        config.network.name = "prod".to_owned();
        let second = config.membership_tag().expect("membership tag");

        assert!(first.is_some());
        assert_ne!(first, second);
    }

    #[test]
    fn membership_key_rejects_invalid_or_short_material() {
        assert!(matches!(
            decode_membership_key("not-base64"),
            Err(ConfigError::MembershipKey(
                MembershipKeyError::InvalidBase64
            ))
        ));
        assert!(matches!(
            decode_membership_key(&base64::engine::general_purpose::STANDARD.encode([1_u8; 8])),
            Err(ConfigError::MembershipKey(MembershipKeyError::TooShort {
                actual: 8,
                minimum: MIN_MEMBERSHIP_KEY_LEN
            }))
        ));
    }

    #[test]
    fn previous_membership_tags_require_current_membership_key() {
        let mut config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: vec![
                    base64::engine::general_purpose::STANDARD.encode([9_u8; 32]),
                ],
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
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
            config.previous_membership_tags(),
            Err(ConfigError::PreviousMembershipTagsWithoutMembershipKey)
        ));

        config.network.membership_key =
            Some(base64::engine::general_purpose::STANDARD.encode([7_u8; 32]));
        assert_eq!(
            config.previous_membership_tags().expect("previous tags"),
            config.network.previous_membership_tags
        );
    }

    #[test]
    fn previous_membership_tags_reject_invalid_tag_encoding() {
        let mut config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                membership_key: Some(base64::engine::general_purpose::STANDARD.encode([7_u8; 32])),
                previous_membership_tags: vec!["not-base64".to_owned()],
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
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
            config.previous_membership_tags(),
            Err(ConfigError::PreviousMembershipTag(
                MembershipTagError::InvalidBase64(tag)
            )) if tag == "not-base64"
        ));

        config.network.previous_membership_tags =
            vec![base64::engine::general_purpose::STANDARD.encode([9_u8; 8])];
        assert!(matches!(
            config.previous_membership_tags(),
            Err(ConfigError::PreviousMembershipTag(
                MembershipTagError::InvalidLength { actual: 8 }
            ))
        ));
    }

    fn runtime_config_for_identity(identity: NodeIdentity) -> Config {
        Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: identity.peer_id,
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: default_queue(),
            resources: default_resources(),
        }
    }

    fn signed_member_record_for(
        issuer: &NodeIdentity,
        member: NodeIdentity,
        network_name: &str,
    ) -> crate::membership::SignedMembershipRecord {
        signed_member_record_with_roles(
            issuer,
            member,
            network_name,
            vec![crate::membership::MembershipRole::OverlayMember],
            Vec::new(),
            1,
        )
    }

    fn signed_member_record_with_roles(
        issuer: &NodeIdentity,
        member: NodeIdentity,
        network_name: &str,
        roles: Vec<crate::membership::MembershipRole>,
        route_grants: Vec<RouteConfig>,
        sequence: u64,
    ) -> crate::membership::SignedMembershipRecord {
        crate::membership::issue_membership_record_at(
            issuer,
            crate::membership::MembershipRecordOptions {
                network_name: network_name.to_owned(),
                member,
                membership_epoch: 1,
                sequence,
                roles,
                route_grants,
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("membership record")
    }

    #[test]
    fn runtime_validation_accepts_valid_member_records() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let mut config = runtime_config_for_identity(member.clone());
        config.network.member_records = vec![signed_member_record_for(&issuer, member, "lab")];

        config.validate_runtime().expect("runtime config");
    }

    #[test]
    fn runtime_validation_rejects_tampered_member_records() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let mut config = runtime_config_for_identity(member.clone());
        let mut record = signed_member_record_for(&issuer, member, "lab");
        record.payload.sequence += 1;
        config.network.member_records = vec![record];

        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::MembershipRecord(
                crate::membership::MembershipRecordError::InvalidSignature
            ))
        ));
    }

    #[test]
    fn runtime_validation_rejects_wrong_network_member_records() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let mut config = runtime_config_for_identity(member.clone());
        config.network.member_records = vec![signed_member_record_for(&issuer, member, "other")];

        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::MembershipRecord(
                crate::membership::MembershipRecordError::NetworkMismatch { .. }
            ))
        ));
    }

    #[test]
    fn compile_routes_includes_member_record_overlay_builtin_routes() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = PeerId::from_str(&member.peer_id).expect("member peer");
        let mut config =
            runtime_config_for_identity(NodeIdentity::generate_ed25519().expect("local"));
        config.network.member_records = vec![signed_member_record_for(&issuer, member, "lab")];

        let routes = config.compile_routes().expect("routes");

        assert!(routes.authorizes_route(
            member_peer,
            IpCidr::new(IpAddr::V4(builtin_ipv4(member_peer)), 32).expect("cidr")
        ));
        assert!(routes.authorizes_route(
            member_peer,
            IpCidr::new(IpAddr::V6(builtin_ipv6(member_peer)), 128).expect("cidr")
        ));
    }

    #[test]
    fn compile_routes_includes_member_record_route_grants() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = PeerId::from_str(&member.peer_id).expect("member peer");
        let mut config =
            runtime_config_for_identity(NodeIdentity::generate_ed25519().expect("local"));
        config.network.member_records = vec![signed_member_record_with_roles(
            &issuer,
            member,
            "lab",
            vec![
                crate::membership::MembershipRole::OverlayMember,
                crate::membership::MembershipRole::RouteAuthority,
            ],
            vec![RouteConfig {
                prefix: "10.77.0.0/24".to_owned(),
                metric: 44,
            }],
            1,
        )];

        let routes = config.compile_routes().expect("routes");

        assert!(routes.authorizes_route(
            member_peer,
            IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 77, 0, 0)), 24).expect("cidr")
        ));
    }

    #[test]
    fn compile_routes_omits_revoked_member_record_routes() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = PeerId::from_str(&member.peer_id).expect("member peer");
        let grant = signed_member_record_with_roles(
            &issuer,
            member.clone(),
            "lab",
            vec![
                crate::membership::MembershipRole::OverlayMember,
                crate::membership::MembershipRole::RouteAuthority,
            ],
            vec![RouteConfig {
                prefix: "10.77.0.0/24".to_owned(),
                metric: 44,
            }],
            1,
        );
        let revocation = crate::membership::issue_membership_record_for_subject_at(
            &issuer,
            crate::membership::MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: crate::membership::MembershipRecordSubject::from_identity(&member)
                    .expect("member subject"),
                membership_epoch: 1,
                sequence: 2,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("revocation");
        let mut config =
            runtime_config_for_identity(NodeIdentity::generate_ed25519().expect("local"));
        config.network.member_records = vec![grant, revocation];

        let routes = config.compile_routes().expect("routes");

        assert!(!routes.authorizes_route(
            member_peer,
            IpCidr::new(IpAddr::V4(builtin_ipv4(member_peer)), 32).expect("cidr")
        ));
        assert!(!routes.authorizes_route(
            member_peer,
            IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 77, 0, 0)), 24).expect("cidr")
        ));
    }

    #[test]
    fn compile_routes_rejects_conflicting_member_record_grants() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let mut config =
            runtime_config_for_identity(NodeIdentity::generate_ed25519().expect("local"));
        config.network.routes = vec![RouteConfig {
            prefix: "10.77.0.0/24".to_owned(),
            metric: 1,
        }];
        config.network.member_records = vec![signed_member_record_with_roles(
            &issuer,
            member,
            "lab",
            vec![
                crate::membership::MembershipRole::OverlayMember,
                crate::membership::MembershipRole::RouteAuthority,
            ],
            vec![RouteConfig {
                prefix: "10.77.0.0/24".to_owned(),
                metric: 44,
            }],
            1,
        )];

        assert!(matches!(
            config.compile_routes(),
            Err(ConfigError::Route(RouteError::ConflictingOwnership { .. }))
        ));
    }

    #[test]
    fn config_vpn_ip_shortcuts_compile_to_host_routes() {
        let config: Config = serde_json::from_str(
            r#"{
              "network": {
                "name": "dev",
                "local_peer": "0000000000000000000000000000000000000000000000000000000000000000",
                "vpn_ip": "10.44.0.1"
              },
              "peers": [
                {
                  "id": "0100000000000000000000000000000000000000000000000000000000000000",
                  "vpnIp": "fd00::2"
                }
              ]
            }"#,
        )
        .expect("config");

        let local_peer = config.local_peer_id().expect("local peer");
        let remote_peer = config.peers[0].peer_id().expect("remote peer");
        let routes = config.compile_routes().expect("routes");

        assert!(routes.authorizes_route(
            local_peer,
            IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 44, 0, 1)), 32).expect("cidr")
        ));
        assert!(routes.authorizes_route(
            remote_peer,
            IpCidr::new("fd00::2".parse::<IpAddr>().expect("ip"), 128).expect("cidr")
        ));
    }

    #[test]
    fn config_compiles_builtin_and_advertised_routes() {
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 75,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: "0100000000000000000000000000000000000000000000000000000000000000".to_owned(),
                name: Some("one".to_owned()),
                ip: None,
                vpn_ip: None,
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
        let local = config.local_peer_id().expect("valid local peer");
        let table = config.compile_routes().expect("routes should compile");

        assert_eq!(
            table
                .resolve(IpAddr::V4(Ipv4Addr::new(10, 41, 0, 1)))
                .map(|route| route.owner),
            Some(local)
        );
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
    fn config_rejects_local_and_peer_route_overlap() {
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: vec![RouteConfig {
                    prefix: "10.42.0.0/16".to_owned(),
                    metric: 50,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: "0100000000000000000000000000000000000000000000000000000000000000".to_owned(),
                name: Some("one".to_owned()),
                ip: None,
                vpn_ip: None,
                addresses: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.42.9.0/24".to_owned(),
                    metric: 10,
                }],
            }],
            queue: default_queue(),
            resources: default_resources(),
        };

        assert!(matches!(
            config.compile_routes(),
            Err(ConfigError::Route(RouteError::ConflictingOwnership { .. }))
        ));

        config.peers[0].routes[0].prefix = "10.43.9.0/24".to_owned();
        assert!(config.compile_routes().is_ok());
    }

    #[test]
    fn effective_packet_mtu_is_capped_by_packet_plane_payload_length() {
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
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
            u16::try_from(PACKET_PLANE_MAX_PAYLOAD_LEN).expect("packet payload length fits u16")
        );
    }

    #[test]
    fn discovery_config_defaults_to_public_ipfs_kademlia_protocol() {
        let discovery = serde_json::from_str::<DiscoveryConfig>("{}").expect("discovery");

        assert_eq!(discovery, DiscoveryConfig::default());
        assert_eq!(discovery.kademlia_protocol, PUBLIC_IPFS_KADEMLIA_PROTOCOL);
    }

    #[test]
    fn runtime_validation_rejects_invalid_kademlia_protocol() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig {
                    kademlia_protocol: "ipfs/kad/1.0.0".to_owned(),
                    ..DiscoveryConfig::default()
                },
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
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
            Err(ConfigError::KademliaProtocol(protocol)) if protocol == "ipfs/kad/1.0.0"
        ));
    }

    #[test]
    fn runtime_validation_rejects_provider_advertisement_without_kademlia() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig {
                    mdns: true,
                    kademlia: false,
                    kademlia_provider_advertisement: true,
                    kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
                    dcutr: true,
                    autonat: true,
                },
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
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
            Err(ConfigError::Discovery(
                DiscoveryValidationError::ProviderAdvertisementWithoutKademlia
            ))
        ));
    }

    #[test]
    fn runtime_validation_rejects_addressless_peers_without_discovery() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig {
                    mdns: false,
                    kademlia: false,
                    kademlia_provider_advertisement: false,
                    kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
                    dcutr: true,
                    autonat: true,
                },
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.peer_id.clone(),
                name: Some("remote".to_owned()),
                ip: None,
                vpn_ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: default_queue(),
            resources: default_resources(),
        };

        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Address(
                AddressValidationError::UnreachablePeer { peer }
            )) if peer == remote.peer_id
        ));

        config.peers[0].addresses = vec![format!("/ip4/127.0.0.1/tcp/4001/p2p/{}", remote.peer_id)];
        assert!(config.validate_runtime().is_ok());
    }

    #[test]
    fn runtime_validation_allows_addressless_peers_with_discovery() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig {
                    mdns: true,
                    kademlia: false,
                    kademlia_provider_advertisement: false,
                    kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
                    dcutr: true,
                    autonat: true,
                },
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.peer_id,
                name: Some("remote".to_owned()),
                ip: None,
                vpn_ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: default_queue(),
            resources: default_resources(),
        };

        assert!(config.validate_runtime().is_ok());

        config.network.discovery.mdns = false;
        config.network.discovery.kademlia = true;
        assert!(config.validate_runtime().is_ok());
    }

    #[test]
    fn config_rejects_cross_peer_route_overlap() {
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: "0000000000000000000000000000000000000000000000000000000000000000"
                    .to_owned(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
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
                    ip: None,
                    vpn_ip: None,
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
                    ip: None,
                    vpn_ip: None,
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
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
                external_addresses: vec!["/ip4/203.0.113.10/udp/4001/quic-v1".to_owned()],
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: remote.to_string(),
                    address: "/ip4/127.0.0.1/udp/4001/quic-v1".to_owned(),
                }],
                discovery: DiscoveryConfig {
                    mdns: false,
                    kademlia: true,
                    kademlia_provider_advertisement: true,
                    kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
                    dcutr: true,
                    autonat: true,
                },
                relay: RelayConfig {
                    server: true,
                    reservations: vec![format!("/ip4/127.0.0.1/tcp/4001/p2p/{remote}/p2p-circuit")],
                    auto: AutoRelayConfig::default(),
                    resources: RelayResourceConfig::default(),
                },
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: Some("remote".to_owned()),
                ip: None,
                vpn_ip: None,
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
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
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
    fn runtime_validation_rejects_zero_interface_mtu() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 0,
            },
            peers: Vec::new(),
            queue: default_queue(),
            resources: default_resources(),
        };

        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Interface(InterfaceValidationError::ZeroMtu))
        ));
    }

    #[test]
    fn packet_plane_config_parses_listener_and_endpoint_candidates() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    listen: vec!["0.0.0.0:51820".to_owned()],
                    external_endpoints: vec!["203.0.113.10:51820".to_owned()],
                    quic_listen: vec!["0.0.0.0:51821".to_owned()],
                    quic_external_endpoints: vec!["203.0.113.10:51821".to_owned()],
                    session_ttl_seconds: default_packet_plane_session_ttl_seconds(),
                    max_replay_windows_per_session: default_packet_plane_replay_windows_per_session(
                    ),
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

        assert_eq!(
            config.packet_plane_listen_addrs().expect("listen addrs"),
            vec!["0.0.0.0:51820".parse().expect("socket")]
        );
        assert_eq!(
            config
                .packet_plane_quic_listen_addrs()
                .expect("quic listen addrs"),
            vec!["0.0.0.0:51821".parse().expect("socket")]
        );
        assert_eq!(
            config
                .packet_plane_endpoint_candidates()
                .expect("endpoint candidates"),
            vec!["203.0.113.10:51820"]
        );
        assert_eq!(
            config
                .packet_plane_quic_endpoint_candidates()
                .expect("quic endpoint candidates"),
            vec!["203.0.113.10:51821"]
        );
        assert!(config.validate_runtime().is_ok());
    }

    #[test]
    fn packet_plane_config_accepts_dns_endpoint_candidates() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    listen: vec!["0.0.0.0:51820".to_owned()],
                    external_endpoints: vec!["vpn-a.example.net:51820".to_owned()],
                    quic_listen: Vec::new(),
                    quic_external_endpoints: Vec::new(),
                    session_ttl_seconds: default_packet_plane_session_ttl_seconds(),
                    max_replay_windows_per_session: default_packet_plane_replay_windows_per_session(
                    ),
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

        assert_eq!(
            config
                .packet_plane_endpoint_candidates()
                .expect("endpoint candidates"),
            vec!["vpn-a.example.net:51820"]
        );
        assert!(config.validate_runtime().is_ok());
    }

    #[test]
    fn packet_plane_config_rejects_endpoint_candidates_without_ports() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    listen: Vec::new(),
                    external_endpoints: vec!["vpn-a.example.net".to_owned()],
                    quic_listen: Vec::new(),
                    quic_external_endpoints: Vec::new(),
                    session_ttl_seconds: default_packet_plane_session_ttl_seconds(),
                    max_replay_windows_per_session: default_packet_plane_replay_windows_per_session(
                    ),
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
            Err(ConfigError::PacketPlane(
                PacketPlaneValidationError::InvalidEndpoint(endpoint)
            )) if endpoint == "vpn-a.example.net"
        ));

        config.network.packet_plane.external_endpoints = vec!["vpn_a.example.net:51820".to_owned()];
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::PacketPlane(
                PacketPlaneValidationError::InvalidEndpoint(endpoint)
            )) if endpoint == "vpn_a.example.net:51820"
        ));
    }

    #[test]
    fn packet_plane_config_defaults_session_ttl_and_replay_limit_for_existing_configs() {
        let decoded = serde_json::from_str::<PacketPlaneConfig>(
            r#"{"listen":["0.0.0.0:51820"],"external_endpoints":["203.0.113.10:51820"]}"#,
        )
        .expect("packet plane config");

        assert_eq!(
            decoded.session_ttl_seconds,
            default_packet_plane_session_ttl_seconds()
        );
        assert_eq!(
            decoded.session_ttl(),
            Duration::from_secs(default_packet_plane_session_ttl_seconds())
        );
        assert_eq!(
            decoded.max_replay_windows_per_session,
            default_packet_plane_replay_windows_per_session()
        );
        assert_eq!(
            decoded.replay_window_limit(),
            default_packet_plane_replay_windows_per_session()
        );
    }

    #[test]
    fn runtime_validation_rejects_zero_packet_plane_session_ttl() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    session_ttl_seconds: 0,
                    ..PacketPlaneConfig::default()
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
            Err(ConfigError::PacketPlane(
                PacketPlaneValidationError::NoSessionTtl
            ))
        ));
    }

    #[test]
    fn runtime_validation_rejects_zero_packet_plane_replay_windows() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    max_replay_windows_per_session: 0,
                    ..PacketPlaneConfig::default()
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
            Err(ConfigError::PacketPlane(
                PacketPlaneValidationError::NoReplayWindows
            ))
        ));
    }

    #[test]
    fn runtime_validation_rejects_multiple_packet_plane_quic_listeners() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig {
                    quic_listen: vec!["127.0.0.1:51821".to_owned(), "127.0.0.1:51822".to_owned()],
                    ..PacketPlaneConfig::default()
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
            Err(ConfigError::PacketPlane(
                PacketPlaneValidationError::TooManyQuicListeners { actual: 2, max: 1 }
            ))
        ));
    }

    #[test]
    fn runtime_validation_rejects_empty_packet_queues() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: default_queue(),
            resources: default_resources(),
        };

        config.queue.max_packets_per_peer = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::EmptyQueuePackets
            ))
        ));

        config.queue.max_packets_per_peer = 256;
        config.queue.max_bytes_per_peer = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::EmptyQueueBytes
            ))
        ));
    }

    #[test]
    fn runtime_validation_rejects_zero_resource_capacity() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: default_queue(),
            resources: default_resources(),
        };

        config.resources.max_concurrent_control_streams = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::NoConcurrentControlStreams
            ))
        ));

        config.resources.max_concurrent_control_streams = 64;
        config.resources.max_concurrent_packet_streams = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::NoConcurrentPacketStreams
            ))
        ));

        config.resources.max_concurrent_packet_streams = 256;
        config.resources.max_pending_incoming_connections = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::NoPendingIncomingConnections
            ))
        ));

        config.resources.max_pending_incoming_connections = 64;
        config.resources.max_pending_outgoing_connections = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::NoPendingOutgoingConnections
            ))
        ));

        config.resources.max_pending_outgoing_connections = 64;
        config.resources.max_established_connections_per_peer = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::NoEstablishedConnectionsPerPeer
            ))
        ));

        config.resources.max_established_connections_per_peer = 8;
        config.resources.max_established_connections = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::NoEstablishedConnections
            ))
        ));

        config.resources.max_established_connections = 512;
        config.resources.max_inbound_packets_per_peer_per_second = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::NoInboundPacketsPerPeerPerSecond
            ))
        ));
    }

    #[test]
    fn runtime_validation_rejects_non_operational_relay_server_limits() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let mut config = Config {
            network: NetworkConfig {
                name: "dev".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig {
                    server: true,
                    reservations: Vec::new(),
                    auto: AutoRelayConfig::default(),
                    resources: RelayResourceConfig::default(),
                },
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: default_queue(),
            resources: default_resources(),
        };

        config.network.relay.resources.max_reservations = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::RelayServerNoReservations
            ))
        ));

        config.network.relay.resources = RelayResourceConfig::default();
        config.network.relay.resources.max_reservations_per_peer = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::RelayServerNoReservationsPerPeer
            ))
        ));

        config.network.relay.resources = RelayResourceConfig::default();
        config.network.relay.resources.reservation_duration_secs = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::RelayServerNoReservationDuration
            ))
        ));

        config.network.relay.resources = RelayResourceConfig::default();
        config.network.relay.resources.max_circuits = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::RelayServerNoCircuits
            ))
        ));

        config.network.relay.resources = RelayResourceConfig::default();
        config.network.relay.resources.max_circuits_per_peer = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::RelayServerNoCircuitsPerPeer
            ))
        ));

        config.network.relay.resources = RelayResourceConfig::default();
        config.network.relay.resources.max_circuit_duration_secs = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::RelayServerNoCircuitDuration
            ))
        ));

        config.network.relay.resources = RelayResourceConfig::default();
        config.network.relay.resources.max_circuit_bytes = 0;
        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::RelayServerNoCircuitBytes
            ))
        ));

        config.network.relay.server = false;
        assert!(config.validate_runtime().is_ok());
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
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: vec!["not-a-multiaddr".to_owned()],
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                ip: None,
                vpn_ip: None,
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
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: configured.to_string(),
                    address: format!("/ip4/127.0.0.1/tcp/4001/p2p/{other}"),
                }],
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: configured.to_string(),
                name: None,
                ip: None,
                vpn_ip: None,
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
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig {
                    server: false,
                    reservations: vec!["/ip4/127.0.0.1/tcp/4001".to_owned()],
                    auto: AutoRelayConfig::default(),
                    resources: RelayResourceConfig::default(),
                },
                packet_plane: PacketPlaneConfig::default(),
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
            membership_key: None,
            vpn_ip: None,
            local_routes: vec![RouteConfig {
                prefix: "10.41.0.0/24".to_owned(),
                metric: 100,
            }],
            interface_name: "hs-lab".to_owned(),
            mtu: 1_400,
            listen_addresses: vec![
                "/ip4/0.0.0.0/tcp/0".to_owned(),
                "/ip4/0.0.0.0/udp/0/quic-v1".to_owned(),
            ],
            external_addresses: vec!["/dns4/node-a.example.net/udp/4001/quic-v1".to_owned()],
            packet_plane: PacketPlaneConfig {
                listen: vec!["0.0.0.0:51820".to_owned()],
                external_endpoints: vec!["203.0.113.10:51820".to_owned()],
                quic_listen: Vec::new(),
                quic_external_endpoints: Vec::new(),
                session_ttl_seconds: 120,
                max_replay_windows_per_session: 512,
            },
            bootstrap_peers: vec![InitPeer {
                id: remote.to_string(),
                address: Some("/ip4/127.0.0.1/tcp/4001".to_owned()),
                vpn_ip: None,
                routes: Vec::new(),
            }],
            peers: vec![
                InitPeer {
                    id: remote.to_string(),
                    address: Some("/ip4/127.0.0.1/tcp/4001".to_owned()),
                    vpn_ip: None,
                    routes: vec![RouteConfig {
                        prefix: "10.42.0.0/24".to_owned(),
                        metric: 100,
                    }],
                },
                InitPeer {
                    id: remote.to_string(),
                    address: Some("/ip4/127.0.0.1/udp/4001/quic-v1".to_owned()),
                    vpn_ip: None,
                    routes: vec![RouteConfig {
                        prefix: "fd00::/64".to_owned(),
                        metric: 100,
                    }],
                },
                InitPeer {
                    id: remote.to_string(),
                    address: Some("/ip4/127.0.0.1/tcp/4001".to_owned()),
                    vpn_ip: None,
                    routes: vec![RouteConfig {
                        prefix: "10.42.0.0/24".to_owned(),
                        metric: 100,
                    }],
                },
            ],
            discovery: DiscoveryConfig {
                mdns: true,
                kademlia: false,
                kademlia_provider_advertisement: false,
                kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
                dcutr: true,
                autonat: true,
            },
            relay: RelayConfig {
                server: true,
                reservations: vec![format!("/ip4/127.0.0.1/tcp/4002/p2p/{remote}/p2p-circuit")],
                auto: AutoRelayConfig::default(),
                resources: RelayResourceConfig {
                    max_reservations: 17,
                    max_reservations_per_peer: 3,
                    reservation_duration_secs: 45,
                    max_circuits: 19,
                    max_circuits_per_peer: 5,
                    max_circuit_duration_secs: 23,
                    max_circuit_bytes: 4096,
                },
            },
        }
        .into_config();
        let rendered = serde_json::to_string_pretty(&config).expect("rendered config");
        let decoded = serde_json::from_str::<Config>(&rendered).expect("decoded config");

        assert_generated_init_config(&decoded, &identity);
    }

    fn assert_generated_init_config(decoded: &Config, identity: &NodeIdentity) {
        assert_eq!(decoded.network.local_peer, identity.peer_id);
        assert_eq!(
            decoded.network.private_key.as_deref(),
            Some(identity.private_key.as_str())
        );
        assert_eq!(decoded.interface.name, "hs-lab");
        assert_eq!(decoded.interface.mtu, 1_400);
        assert_eq!(
            decoded.network.routes,
            vec![RouteConfig {
                prefix: "10.41.0.0/24".to_owned(),
                metric: 100,
            }]
        );
        assert_eq!(decoded.network.external_addresses.len(), 1);
        assert_eq!(
            decoded.network.packet_plane,
            PacketPlaneConfig {
                listen: vec!["0.0.0.0:51820".to_owned()],
                external_endpoints: vec!["203.0.113.10:51820".to_owned()],
                quic_listen: Vec::new(),
                quic_external_endpoints: Vec::new(),
                session_ttl_seconds: 120,
                max_replay_windows_per_session: 512,
            }
        );
        assert_eq!(decoded.network.bootstrap_peers.len(), 1);
        assert!(decoded.network.relay.server);
        assert_eq!(
            decoded.network.relay.resources,
            RelayResourceConfig {
                max_reservations: 17,
                max_reservations_per_peer: 3,
                reservation_duration_secs: 45,
                max_circuits: 19,
                max_circuits_per_peer: 5,
                max_circuit_duration_secs: 23,
                max_circuit_bytes: 4096,
            }
        );
        assert_eq!(decoded.queue, QueueConfig::default());
        assert_eq!(decoded.resources, ResourceConfig::default());
        assert!(!decoded.network.discovery.kademlia);
        assert_eq!(decoded.peers.len(), 1);
        assert_eq!(decoded.peers[0].addresses.len(), 2);
        assert_eq!(
            decoded.peers[0].routes,
            vec![
                RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 100,
                },
                RouteConfig {
                    prefix: "fd00::/64".to_owned(),
                    metric: 100,
                },
            ]
        );
        assert!(decoded.identity().is_ok());
        assert!(decoded.listen_multiaddrs().is_ok());
        assert!(decoded.bootstrap_multiaddrs().is_ok());
        assert_eq!(decoded.peer_multiaddrs().expect("peer addresses").len(), 2);
        assert!(decoded.compile_routes().is_ok());
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
        assert_eq!(config.network.relay.auto, AutoRelayConfig::default());
    }

    #[test]
    fn auto_relay_config_deserializes_custom_policy() {
        let config = serde_json::from_str::<Config>(
            r#"{
              "network": {
                "name": "dev",
                "local_peer": "0000000000000000000000000000000000000000000000000000000000000000",
                "relay": {
                  "auto": {
                    "max_candidates": 24,
                    "max_reservations": 3,
                    "retry_interval_seconds": 17
                  }
                }
              },
              "interface": {
                "name": "hs0",
                "mtu": 1280
              }
            }"#,
        )
        .expect("config");

        assert_eq!(
            config.network.relay.auto,
            AutoRelayConfig {
                max_candidates: 24,
                max_reservations: 3,
                retry_interval_seconds: 17,
            }
        );
    }

    #[test]
    fn auto_relay_config_rejects_zero_retry_interval() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let mut config = runtime_config_for_identity(identity);
        config.network.relay.auto.retry_interval_seconds = 0;

        assert!(matches!(
            config.validate_runtime(),
            Err(ConfigError::Resource(
                ResourceValidationError::NoAutoRelayRetryInterval
            ))
        ));
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
        assert_eq!(config.resources.inbound_packet_rate_limit(), 4096);
        assert_eq!(config.queue.max_packet_age_millis, 3_000);

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
                "max_established_connections": 8,
                "max_inbound_packets_per_peer_per_second": 9
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
        assert_eq!(config.resources.inbound_packet_rate_limit(), 9);
        assert_eq!(
            config.queue.max_packet_age(),
            std::time::Duration::from_millis(1)
        );
    }
}
