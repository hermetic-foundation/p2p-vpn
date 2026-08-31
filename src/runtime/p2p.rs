use std::{collections::HashSet, error::Error, num::NonZeroU8, time::Duration};

use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder, allow_block_list, autonat, connection_limits,
    core::transport::ListenerId,
    dcutr, dns, identify,
    identity::Keypair,
    kad, mdns,
    multiaddr::Protocol,
    noise, ping, relay, request_response,
    swarm::{NetworkBehaviour, behaviour::toggle::Toggle},
    tcp, yamux,
};

use crate::{
    config::{DiscoveryConfig, RelayResourceConfig, ResourceConfig},
    identity::{IdentityError, NodeIdentity},
    runtime::{
        control::{self, ControlCodec},
        packet::{self, PacketCodec},
        pairing::{self, PairingCodec},
        pairing_code::{self, PairingCodeCodec},
        pinned_packet_stream,
        service::{self, ServiceCodec},
    },
};

const PROTOCOL_VERSION: &str = "/p2p-vpn/0.1.0";
const CONNECTION_PING_INTERVAL: Duration = Duration::from_secs(15);
const CONNECTION_PING_TIMEOUT: Duration = Duration::from_secs(20);
const SWARM_IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
const DIAL_CONCURRENCY_FACTOR: NonZeroU8 = NonZeroU8::MIN;

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub connection_limits: connection_limits::Behaviour,
    pub blocked_peers: allow_block_list::Behaviour<allow_block_list::BlockedPeers>,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub kad: kad::Behaviour<kad::store::MemoryStore>,
    pub relay: relay::client::Behaviour,
    pub relay_server: Toggle<relay::Behaviour>,
    pub dcutr: Toggle<dcutr::Behaviour>,
    pub autonat: Toggle<autonat::Behaviour>,
    pub mdns: Toggle<mdns::tokio::Behaviour>,
    pub control: request_response::Behaviour<ControlCodec>,
    pub packet: request_response::Behaviour<PacketCodec>,
    pub pairing: request_response::Behaviour<PairingCodec>,
    pub pairing_code: request_response::Behaviour<PairingCodeCodec>,
    pub pinned_packet_stream: pinned_packet_stream::Behaviour,
    pub service: request_response::Behaviour<ServiceCodec>,
}

pub struct P2pNode {
    pub local_peer_id: PeerId,
    pub identity: NodeIdentity,
    pub network_name: String,
    pub membership_tag: Option<String>,
    pub swarm: Swarm<Behaviour>,
    pub discovery: DiscoveryConfig,
    pub kademlia_rendezvous_key: Option<kad::RecordKey>,
    pub kademlia_membership_records_key: Option<kad::RecordKey>,
    pub bootstrap_peer_addresses: Vec<(PeerId, Multiaddr)>,
    pub relay_peer_addresses: Vec<(PeerId, Multiaddr)>,
    pub relay_reservation_addresses: Vec<Multiaddr>,
    pub configured_relay_reservation_listeners: HashSet<ListenerId>,
    pub retiring_configured_relay_reservation_listeners: HashSet<ListenerId>,
    pub configured_peer_addresses: Vec<(PeerId, Multiaddr)>,
    pub configured_external_addresses: Vec<Multiaddr>,
    pub packet_endpoint_candidates: Vec<String>,
    pub startup: StartupStatus,
}

pub struct HostConfig {
    pub identity: NodeIdentity,
    pub network_name: String,
    pub membership_tag: Option<String>,
    pub mtu: u16,
    pub max_concurrent_control_streams: usize,
    pub max_concurrent_packet_streams: usize,
    pub listen_addresses: Vec<Multiaddr>,
    pub external_addresses: Vec<Multiaddr>,
    pub bootstrap_peers: Vec<(PeerId, Multiaddr)>,
    pub known_peers: Vec<(PeerId, Multiaddr)>,
    pub relay_reservations: Vec<Multiaddr>,
    pub relay_server: bool,
    pub relay_resources: RelayResourceConfig,
    pub resources: ResourceConfig,
    pub discovery: DiscoveryConfig,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StartupStatus {
    pub mdns_enabled: bool,
    pub dcutr_enabled: bool,
    pub autonat_enabled: bool,
    pub autonat_servers_registered: usize,
    pub external_addresses_configured: usize,
    pub kademlia: KademliaStartupStatus,
    pub relay_reservations_started: usize,
    pub relay_server_enabled: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KademliaStartupStatus {
    pub bootstrap_started: bool,
    pub rendezvous_advertise_started: bool,
    pub rendezvous_lookup_started: bool,
}

pub fn build_node(config: &HostConfig) -> Result<P2pNode, P2pBuildError> {
    let keypair = decode_keypair(&config.identity.private_key)?;
    let local_peer_id = keypair.public().to_peer_id();
    let bootstrap_peer_addresses = config.bootstrap_peers.clone();
    let relay_peer_addresses = relay_peer_addresses_from_reservations(&config.relay_reservations);
    let relay_reservation_addresses = config.relay_reservations.clone();
    let configured_peer_addresses = config.known_peers.clone();
    let configured_external_addresses = config.external_addresses.clone();

    let discovery = config.discovery.clone();
    let behaviour_discovery = discovery.clone();
    let relay_server = config.relay_server;
    let relay_resources = config.relay_resources;
    let resources = config.resources;
    let mtu = config.mtu;
    let control_streams = config.max_concurrent_control_streams;
    let packet_streams = config.max_concurrent_packet_streams;
    let kademlia_protocol = kademlia_stream_protocol(&discovery.kademlia_protocol)?;

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_dns_config(dns::ResolverConfig::default(), dns::ResolverOpts::default())
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(
            |keypair, relay| -> Result<Behaviour, Box<dyn Error + Send + Sync>> {
                let local_peer_id = keypair.public().to_peer_id();
                let store = kad::store::MemoryStore::new(local_peer_id);
                let kad_config = kad::Config::new(kademlia_protocol);
                let mut kad = kad::Behaviour::with_config(local_peer_id, store, kad_config);
                if behaviour_discovery.kademlia {
                    kad.set_mode(Some(kad::Mode::Server));
                } else {
                    kad.set_mode(Some(kad::Mode::Client));
                }
                let mdns = if behaviour_discovery.mdns {
                    Some(mdns::tokio::Behaviour::new(
                        mdns::Config::default(),
                        local_peer_id,
                    )?)
                } else {
                    None
                };

                Ok(Behaviour {
                    connection_limits: connection_limits::Behaviour::new(
                        resources.to_connection_limits(),
                    ),
                    blocked_peers: allow_block_list::Behaviour::default(),
                    identify: identify::Behaviour::new(
                        identify::Config::new(PROTOCOL_VERSION.to_owned(), keypair.public())
                            .with_hide_listen_addrs(true),
                    ),
                    ping: ping::Behaviour::new(
                        ping::Config::new()
                            .with_interval(CONNECTION_PING_INTERVAL)
                            .with_timeout(CONNECTION_PING_TIMEOUT),
                    ),
                    kad,
                    relay,
                    relay_server: relay_server
                        .then(|| {
                            relay::Behaviour::new(local_peer_id, relay_resources.to_libp2p_config())
                        })
                        .into(),
                    dcutr: behaviour_discovery
                        .dcutr
                        .then(|| dcutr::Behaviour::new(local_peer_id))
                        .into(),
                    autonat: behaviour_discovery
                        .autonat
                        .then(|| autonat::Behaviour::new(local_peer_id, autonat::Config::default()))
                        .into(),
                    mdns: mdns.into(),
                    control: control::behaviour(control_streams),
                    packet: packet::behaviour(mtu, packet_streams),
                    pairing: pairing::behaviour(control_streams),
                    pairing_code: pairing_code::behaviour(control_streams),
                    pinned_packet_stream: pinned_packet_stream::Behaviour::new(usize::from(mtu)),
                    service: service::behaviour(control_streams),
                })
            },
        )?
        .with_swarm_config(|config| {
            config
                .with_dial_concurrency_factor(DIAL_CONCURRENCY_FACTOR)
                .with_idle_connection_timeout(SWARM_IDLE_CONNECTION_TIMEOUT)
        })
        .build();

    let relay_reservations_started = config.relay_reservations.len();
    let configured_relay_reservation_listeners = install_listeners_and_dials(&mut swarm, config)?;
    let autonat_servers_registered = register_autonat_servers(&mut swarm, config);
    let (kademlia_rendezvous_key, kademlia_membership_records_key, kademlia) =
        start_configured_kademlia(&mut swarm, config)?;

    Ok(P2pNode {
        local_peer_id,
        identity: config.identity.clone(),
        network_name: config.network_name.clone(),
        membership_tag: config.membership_tag.clone(),
        swarm,
        discovery,
        kademlia_rendezvous_key,
        kademlia_membership_records_key,
        bootstrap_peer_addresses,
        relay_peer_addresses,
        relay_reservation_addresses,
        configured_relay_reservation_listeners,
        retiring_configured_relay_reservation_listeners: HashSet::new(),
        configured_peer_addresses,
        configured_external_addresses,
        packet_endpoint_candidates: Vec::new(),
        startup: startup_status(
            config,
            kademlia,
            autonat_servers_registered,
            relay_reservations_started,
        ),
    })
}

fn startup_status(
    config: &HostConfig,
    kademlia: KademliaStartupStatus,
    autonat_servers_registered: usize,
    relay_reservations_started: usize,
) -> StartupStatus {
    StartupStatus {
        mdns_enabled: config.discovery.mdns,
        dcutr_enabled: config.discovery.dcutr,
        autonat_enabled: config.discovery.autonat,
        autonat_servers_registered,
        external_addresses_configured: config.external_addresses.len(),
        kademlia,
        relay_reservations_started,
        relay_server_enabled: config.relay_server,
    }
}

fn start_configured_kademlia(
    swarm: &mut Swarm<Behaviour>,
    config: &HostConfig,
) -> Result<
    (
        Option<kad::RecordKey>,
        Option<kad::RecordKey>,
        KademliaStartupStatus,
    ),
    P2pBuildError,
> {
    let rendezvous_key = config
        .discovery
        .kademlia
        .then(|| kademlia_rendezvous_key(&config.network_name, config.membership_tag.as_deref()));
    let membership_records_key = config.discovery.kademlia.then(|| {
        kademlia_membership_records_key(&config.network_name, config.membership_tag.as_deref())
    });
    let startup = start_kademlia(
        swarm,
        rendezvous_key.as_ref(),
        config.discovery.kademlia_provider_advertisement,
    )?;
    Ok((rendezvous_key, membership_records_key, startup))
}

fn register_autonat_servers(swarm: &mut Swarm<Behaviour>, config: &HostConfig) -> usize {
    let Some(autonat) = swarm.behaviour_mut().autonat.as_mut() else {
        return 0;
    };
    let mut registered = 0;
    for (peer, address) in autonat_server_addresses(config) {
        autonat.add_server(peer, Some(address));
        registered += 1;
    }

    registered
}

fn autonat_server_addresses(config: &HostConfig) -> Vec<(PeerId, Multiaddr)> {
    let mut addresses = Vec::new();
    let relay_peers = relay_peer_addresses_from_reservations(&config.relay_reservations);
    for (peer, address) in config
        .bootstrap_peers
        .iter()
        .chain(config.known_peers.iter())
        .chain(relay_peers.iter())
    {
        let entry = (*peer, address.clone());
        if !addresses.contains(&entry) {
            addresses.push(entry);
        }
    }

    addresses
}

fn install_listeners_and_dials(
    swarm: &mut Swarm<Behaviour>,
    config: &HostConfig,
) -> Result<HashSet<ListenerId>, P2pBuildError> {
    for address in &config.listen_addresses {
        swarm.listen_on(address.clone())?;
    }

    for address in &config.external_addresses {
        swarm.add_external_address(address.clone());
    }

    let configured_relay_reservation_listeners = config
        .relay_reservations
        .iter()
        .map(|address| swarm.listen_on(relay_reservation_listen_address(address.clone())))
        .collect::<Result<HashSet<_>, _>>()?;

    for (peer, address) in &config.bootstrap_peers {
        if should_seed_kademlia_address_book(&config.discovery, address) {
            swarm.behaviour_mut().kad.add_address(peer, address.clone());
        }
        let dial_address = peer_dial_address(*peer, address.clone())?;
        swarm.dial(dial_address)?;
    }

    for (peer, address) in &config.known_peers {
        if should_seed_kademlia_address_book(&config.discovery, address) {
            swarm.behaviour_mut().kad.add_address(peer, address.clone());
        }
        if is_relayed_address(address) {
            continue;
        }

        let dial_address = peer_dial_address(*peer, address.clone())?;
        swarm.dial(dial_address)?;
    }

    Ok(configured_relay_reservation_listeners)
}

fn should_seed_kademlia_address_book(discovery: &DiscoveryConfig, address: &Multiaddr) -> bool {
    discovery.kademlia || !is_relayed_address(address)
}

fn relay_reservation_listen_address(address: Multiaddr) -> Multiaddr {
    address
}

fn is_relayed_address(address: &Multiaddr) -> bool {
    address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
}

fn relay_peer_addresses_from_reservations(reservations: &[Multiaddr]) -> Vec<(PeerId, Multiaddr)> {
    reservations
        .iter()
        .filter_map(relay_peer_address_from_reservation)
        .collect()
}

fn relay_peer_address_from_reservation(reservation: &Multiaddr) -> Option<(PeerId, Multiaddr)> {
    let mut relay_address = Multiaddr::empty();
    let mut relay_peer = None;

    for protocol in reservation {
        if matches!(protocol, Protocol::P2pCircuit) {
            break;
        }
        if let Protocol::P2p(peer) = protocol {
            relay_peer = Some(peer);
        }
        relay_address.push(protocol);
    }

    relay_peer.map(|peer| (peer, relay_address))
}

fn start_kademlia(
    _swarm: &mut Swarm<Behaviour>,
    rendezvous_key: Option<&kad::RecordKey>,
    _advertise_provider: bool,
) -> Result<KademliaStartupStatus, P2pBuildError> {
    let Some(_rendezvous_key) = rendezvous_key else {
        return Ok(KademliaStartupStatus::default());
    };

    Ok(KademliaStartupStatus {
        bootstrap_started: false,
        rendezvous_advertise_started: false,
        rendezvous_lookup_started: false,
    })
}

#[must_use]
pub fn kademlia_rendezvous_key(network_name: &str, membership_tag: Option<&str>) -> kad::RecordKey {
    let key = membership_tag.map_or_else(
        || format!("/p2p-vpn/{network_name}/providers/1"),
        |membership_tag| format!("/p2p-vpn/{network_name}/members/{membership_tag}/providers/1"),
    );
    kad::RecordKey::new(&key)
}

#[must_use]
pub fn kademlia_membership_records_key(
    network_name: &str,
    membership_tag: Option<&str>,
) -> kad::RecordKey {
    let key = membership_tag.map_or_else(
        || format!("/p2p-vpn/{network_name}/membership-records/1"),
        |membership_tag| {
            format!("/p2p-vpn/{network_name}/members/{membership_tag}/membership-records/1")
        },
    );
    kad::RecordKey::new(&key)
}

#[must_use]
pub fn kademlia_peer_addresses_key(
    network_name: &str,
    membership_tag: Option<&str>,
    peer: PeerId,
) -> kad::RecordKey {
    let key = membership_tag.map_or_else(
        || format!("/p2p-vpn/{network_name}/peer-addresses/{peer}/1"),
        |membership_tag| {
            format!("/p2p-vpn/{network_name}/members/{membership_tag}/peer-addresses/{peer}/1")
        },
    );
    kad::RecordKey::new(&key)
}

#[must_use]
pub fn kademlia_pairing_code_key(locator: &str) -> kad::RecordKey {
    kad::RecordKey::new(&format!("/p2p-vpn/pairing-code/{locator}/providers/1"))
}

fn kademlia_stream_protocol(protocol: &str) -> Result<libp2p::StreamProtocol, P2pBuildError> {
    libp2p::StreamProtocol::try_from_owned(protocol.to_owned())
        .map_err(|_| P2pBuildError::InvalidKademliaProtocol(protocol.to_owned()))
}

fn decode_keypair(encoded: &str) -> Result<Keypair, IdentityError> {
    let identity = NodeIdentity::from_private_key(encoded)?;
    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        identity.private_key,
    )?;
    Ok(Keypair::from_protobuf_encoding(&bytes)?)
}

fn peer_dial_address(peer: PeerId, address: Multiaddr) -> Result<Multiaddr, P2pBuildError> {
    if address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2p(address_peer) if address_peer == peer))
    {
        return Ok(address);
    }

    address
        .with_p2p(peer)
        .map_err(|address| P2pBuildError::InvalidP2pAddress(Box::new(address)))
}

#[derive(Debug)]
pub enum P2pBuildError {
    Identity(IdentityError),
    Noise(libp2p::noise::Error),
    Transport(libp2p::TransportError<std::io::Error>),
    Behaviour(libp2p::BehaviourBuilderError),
    Listen(libp2p::TransportError<std::io::Error>),
    Dial(libp2p::swarm::DialError),
    KadStore(kad::store::Error),
    Multiaddr(libp2p::multiaddr::Error),
    InvalidP2pAddress(Box<Multiaddr>),
    InvalidKademliaProtocol(String),
}

impl From<IdentityError> for P2pBuildError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<libp2p::TransportError<std::io::Error>> for P2pBuildError {
    fn from(error: libp2p::TransportError<std::io::Error>) -> Self {
        Self::Transport(error)
    }
}

impl From<libp2p::noise::Error> for P2pBuildError {
    fn from(error: libp2p::noise::Error) -> Self {
        Self::Noise(error)
    }
}

impl From<libp2p::BehaviourBuilderError> for P2pBuildError {
    fn from(error: libp2p::BehaviourBuilderError) -> Self {
        Self::Behaviour(error)
    }
}

impl From<libp2p::swarm::DialError> for P2pBuildError {
    fn from(error: libp2p::swarm::DialError) -> Self {
        Self::Dial(error)
    }
}

impl From<kad::store::Error> for P2pBuildError {
    fn from(error: kad::store::Error) -> Self {
        Self::KadStore(error)
    }
}

impl From<libp2p::multiaddr::Error> for P2pBuildError {
    fn from(error: libp2p::multiaddr::Error) -> Self {
        Self::Multiaddr(error)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use base64::Engine as _;
    use futures::StreamExt as _;
    use libp2p::{
        multiaddr::Protocol,
        request_response::{self, Message},
        swarm::SwarmEvent,
    };

    use crate::{
        config::{Config, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig},
        pairing::{
            PairingOfferOptions, PairingRequest, PairingRequestOptions, PairingResponseOptions,
            build_pairing_request_at, build_pairing_response_at, export_pairing_offer_at,
        },
        runtime::control::{ControlCapabilities, ControlRequest, ControlResponse},
        runtime::pinned_packet_stream,
        runtime::service::{
            ServiceRequest, ServiceResponse, ServiceStatusRequest, ServiceStatusResponse,
        },
        wire::{Frame, PayloadType},
    };

    use super::*;

    fn pairing_config(identity: NodeIdentity) -> Config {
        Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: identity.peer_id,
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.42.0.1".to_owned()),
                routes: Vec::new(),
                listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::<PeerConfig>::new(),
            queue: QueueConfig::default(),
            resources: crate::config::ResourceConfig::default(),
        }
    }

    #[tokio::test]
    async fn build_node_uses_configured_identity() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let expected_peer_id = identity.peer_id.parse::<PeerId>().expect("peer id");

        let node = build_node(&HostConfig {
            identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node should build");

        assert_eq!(node.local_peer_id, expected_peer_id);
        assert!(node.startup.mdns_enabled);
        assert!(node.startup.dcutr_enabled);
        assert!(node.startup.autonat_enabled);
        assert!(node.swarm.behaviour().mdns.is_enabled());
        assert!(node.swarm.behaviour().dcutr.is_enabled());
        assert!(node.swarm.behaviour().autonat.is_enabled());
        assert!(!node.startup.relay_server_enabled);
        assert!(!node.swarm.behaviour().relay_server.is_enabled());
    }

    #[tokio::test]
    async fn build_node_disables_optional_discovery_behaviours() {
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: false,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        };

        let node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("node should build");

        assert!(!node.startup.mdns_enabled);
        assert!(!node.startup.dcutr_enabled);
        assert!(!node.startup.autonat_enabled);
        assert_eq!(node.startup.autonat_servers_registered, 0);
        assert!(!node.startup.kademlia.bootstrap_started);
        assert!(!node.startup.kademlia.rendezvous_advertise_started);
        assert!(!node.startup.kademlia.rendezvous_lookup_started);
        assert!(!node.swarm.behaviour().mdns.is_enabled());
        assert!(!node.swarm.behaviour().dcutr.is_enabled());
        assert!(!node.swarm.behaviour().autonat.is_enabled());
    }

    #[tokio::test]
    async fn build_node_accepts_ipfs_compatible_kademlia_protocol() {
        let discovery = DiscoveryConfig {
            kademlia_protocol: "/ipfs/kad/1.0.0".to_owned(),
            ..DiscoveryConfig::default()
        };

        let node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("node should build");

        assert_eq!(node.discovery.kademlia_protocol, "/ipfs/kad/1.0.0");
        assert!(!node.startup.kademlia.rendezvous_advertise_started);
        assert!(!node.startup.kademlia.rendezvous_lookup_started);
    }

    #[tokio::test]
    async fn build_node_scopes_kademlia_rendezvous_to_membership_tag() {
        let membership_tag = "membership-tag".to_owned();
        let node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: Some(membership_tag.clone()),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node should build");

        assert_eq!(
            node.kademlia_rendezvous_key
                .expect("rendezvous key")
                .to_vec(),
            kademlia_rendezvous_key("lab", Some(&membership_tag)).to_vec()
        );
    }

    #[tokio::test]
    async fn build_node_rejects_invalid_kademlia_protocol() {
        let result = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig {
                kademlia_protocol: "ipfs/kad/1.0.0".to_owned(),
                ..DiscoveryConfig::default()
            },
        });

        assert!(matches!(
            result,
            Err(P2pBuildError::InvalidKademliaProtocol(protocol))
                if protocol == "ipfs/kad/1.0.0"
        ));
    }

    #[tokio::test]
    async fn build_node_registers_configured_external_addresses() {
        let external_address: Multiaddr = "/ip4/203.0.113.10/udp/4001/quic-v1"
            .parse()
            .expect("external address");

        let node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: vec![external_address.clone()],
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node should build");

        assert_eq!(node.startup.external_addresses_configured, 1);
        assert!(
            node.swarm
                .external_addresses()
                .any(|address| address == &external_address)
        );
    }

    #[tokio::test]
    async fn build_node_enforces_configured_connection_limits_on_startup_dials() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let address = "/memory/9".parse().expect("peer address");

        let result = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(peer, address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig {
                max_pending_outgoing_connections: 0,
                ..crate::config::ResourceConfig::default()
            },
            discovery: DiscoveryConfig::default(),
        });

        assert!(matches!(result, Err(P2pBuildError::Dial(_))));
    }

    #[tokio::test]
    async fn build_node_defers_relayed_configured_peer_dials() {
        let relay = Keypair::generate_ed25519().public().to_peer_id();
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let address = format!("/memory/9/p2p/{relay}/p2p-circuit/p2p/{peer}")
            .parse()
            .expect("relayed peer address");

        let node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(peer, address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig {
                max_pending_outgoing_connections: 0,
                ..crate::config::ResourceConfig::default()
            },
            discovery: DiscoveryConfig::default(),
        })
        .expect("relayed configured peer should not be dialed at startup");

        assert_eq!(node.configured_peer_addresses.len(), 1);
    }

    #[tokio::test]
    async fn build_node_accepts_dns_peer_addresses_for_startup_dials() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let address = "/dns4/example.invalid/tcp/4001"
            .parse()
            .expect("dns address");

        let node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: vec![(peer, address)],
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node should build and queue DNS dial");

        assert_eq!(node.bootstrap_peer_addresses.len(), 1);
        assert!(!node.startup.kademlia.bootstrap_started);
    }

    #[tokio::test]
    async fn build_node_starts_bootstrap_and_relay_reservations() {
        let relay = Keypair::generate_ed25519().public().to_peer_id();
        let bootstrap_address: Multiaddr = "/memory/91".parse().expect("bootstrap address");
        let relay_reservation = bootstrap_address
            .clone()
            .with_p2p(relay)
            .expect("relay p2p address")
            .with(libp2p::multiaddr::Protocol::P2pCircuit);

        let node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: vec![(relay, bootstrap_address.clone())],
            known_peers: Vec::new(),
            relay_reservations: vec![relay_reservation.clone()],
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");

        assert!(!node.startup.kademlia.bootstrap_started);
        assert!(!node.startup.kademlia.rendezvous_advertise_started);
        assert!(!node.startup.kademlia.rendezvous_lookup_started);
        assert!(node.startup.mdns_enabled);
        assert!(node.startup.dcutr_enabled);
        assert!(node.startup.autonat_enabled);
        assert_eq!(node.startup.autonat_servers_registered, 2);
        assert_eq!(node.startup.relay_reservations_started, 1);
        assert_eq!(
            node.relay_peer_addresses,
            vec![(
                relay,
                bootstrap_address
                    .with_p2p(relay)
                    .expect("relay dial address")
            )]
        );
        assert!(node.startup.relay_server_enabled);
        assert!(node.swarm.behaviour().relay_server.is_enabled());
    }

    #[tokio::test]
    async fn build_node_registers_relay_reservations_as_autonat_servers() {
        let relay = Keypair::generate_ed25519().public().to_peer_id();
        let relay_address = "/memory/92"
            .parse::<Multiaddr>()
            .expect("relay base address")
            .with_p2p(relay)
            .expect("relay p2p address");
        let relay_reservation = relay_address
            .clone()
            .with(libp2p::multiaddr::Protocol::P2pCircuit);

        let node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: vec![relay_reservation],
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");

        assert_eq!(node.startup.autonat_servers_registered, 1);
        assert_eq!(node.relay_peer_addresses, vec![(relay, relay_address)]);
    }

    #[test]
    fn autonat_server_addresses_deduplicate_relay_infrastructure() {
        let relay = Keypair::generate_ed25519().public().to_peer_id();
        let relay_address = "/memory/93"
            .parse::<Multiaddr>()
            .expect("relay base address")
            .with_p2p(relay)
            .expect("relay p2p address");
        let relay_reservation = relay_address
            .clone()
            .with(libp2p::multiaddr::Protocol::P2pCircuit);
        let config = HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: vec![(relay, relay_address.clone())],
            known_peers: vec![(relay, relay_address.clone())],
            relay_reservations: vec![relay_reservation],
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        };

        assert_eq!(
            autonat_server_addresses(&config),
            vec![(relay, relay_address)]
        );
    }

    #[test]
    fn relay_peer_address_is_derived_from_reservation() {
        let relay = Keypair::generate_ed25519().public().to_peer_id();
        let target = Keypair::generate_ed25519().public().to_peer_id();
        let relay_address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}")
            .parse()
            .expect("relay address");
        let reservation: Multiaddr = relay_address
            .clone()
            .with(Protocol::P2pCircuit)
            .with(Protocol::P2p(target));

        assert_eq!(
            relay_peer_address_from_reservation(&reservation),
            Some((relay, relay_address))
        );
    }

    #[test]
    fn peer_dial_address_appends_missing_target_peer() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        let dial = peer_dial_address(peer, address).expect("dial address");

        assert!(dial.to_string().ends_with(&format!("/p2p/{peer}")));
    }

    #[test]
    fn peer_dial_address_preserves_full_relayed_target_address() {
        let relay = Keypair::generate_ed25519().public().to_peer_id();
        let target = Keypair::generate_ed25519().public().to_peer_id();
        let address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{target}")
                .parse()
                .expect("relayed address");

        let dial = peer_dial_address(target, address.clone()).expect("dial address");

        assert_eq!(dial, address);
    }

    #[test]
    fn peer_dial_address_appends_target_to_relay_reservation_address() {
        let relay = Keypair::generate_ed25519().public().to_peer_id();
        let target = Keypair::generate_ed25519().public().to_peer_id();
        let reservation: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit")
            .parse()
            .expect("reservation address");

        let listen_address = peer_dial_address(target, reservation).expect("listen address");

        assert_eq!(
            listen_address.to_string(),
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{target}")
        );
    }

    #[test]
    fn relay_reservation_listen_address_preserves_base_reservation_address() {
        let relay = Keypair::generate_ed25519().public().to_peer_id();
        let target = Keypair::generate_ed25519().public().to_peer_id();
        let reservation: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit")
            .parse()
            .expect("reservation address");

        let listen_address = relay_reservation_listen_address(reservation.clone());

        assert_eq!(listen_address, reservation);
        assert_ne!(
            listen_address.to_string(),
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{target}")
        );
    }

    #[test]
    fn relayed_known_addresses_seed_kademlia_only_when_discovery_is_enabled() {
        let relay = Keypair::generate_ed25519().public().to_peer_id();
        let target = Keypair::generate_ed25519().public().to_peer_id();
        let direct: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{target}")
            .parse()
            .expect("direct address");
        let relayed: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{target}")
                .parse()
                .expect("relayed address");
        let discovery_disabled = DiscoveryConfig {
            kademlia: false,
            kademlia_provider_advertisement: false,
            ..DiscoveryConfig::default()
        };

        assert!(should_seed_kademlia_address_book(
            &discovery_disabled,
            &direct
        ));
        assert!(!should_seed_kademlia_address_book(
            &discovery_disabled,
            &relayed
        ));
        assert!(should_seed_kademlia_address_book(
            &DiscoveryConfig::default(),
            &relayed
        ));
    }

    #[test]
    fn kademlia_rendezvous_key_is_scoped_to_network_name_without_membership() {
        assert_eq!(
            kademlia_rendezvous_key("lab", None).to_vec(),
            b"/p2p-vpn/lab/providers/1".to_vec()
        );
        assert_ne!(
            kademlia_rendezvous_key("lab", None).to_vec(),
            kademlia_rendezvous_key("prod", None).to_vec()
        );
    }

    #[test]
    fn kademlia_rendezvous_key_is_scoped_to_membership_tag_when_available() {
        let tag = "tag";

        assert_eq!(
            kademlia_rendezvous_key("lab", Some(tag)).to_vec(),
            b"/p2p-vpn/lab/members/tag/providers/1".to_vec()
        );
        assert_ne!(
            kademlia_rendezvous_key("lab", Some(tag)).to_vec(),
            kademlia_rendezvous_key("lab", Some("other")).to_vec()
        );
        assert_ne!(
            kademlia_rendezvous_key("lab", Some(tag)).to_vec(),
            kademlia_rendezvous_key("lab", None).to_vec()
        );
    }

    #[tokio::test]
    async fn two_nodes_exchange_packet_request() {
        let mut listener = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("listener node");
        let listener_address = next_listen_address(&mut listener.swarm).await;

        let mut dialer = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("dialer identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener.local_peer_id, listener_address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("dialer node");
        let frame = Frame::packet(1, 7, vec![0x45, 0, 0, 20]).expect("frame");
        let request_id = dialer
            .swarm
            .behaviour_mut()
            .packet
            .send_request(&listener.local_peer_id, frame.clone());

        tokio::time::timeout(
            Duration::from_secs(10),
            exchange_until_response(&mut listener.swarm, &mut dialer.swarm, frame, request_id),
        )
        .await
        .expect("packet exchange timed out");
    }

    #[tokio::test]
    async fn two_nodes_keep_idle_connection_alive_between_pings() {
        let discovery = relay_test_discovery();
        let mut listener = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("listener node");
        let listener_address = next_listen_address(&mut listener.swarm).await;
        let listener_peer = listener.local_peer_id;

        let mut dialer = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("dialer identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener_peer, listener_address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery,
        })
        .expect("dialer node");
        let dialer_peer = dialer.local_peer_id;
        next_connection_to_peer(&mut listener.swarm, &mut dialer.swarm, listener_peer).await;

        // The libp2p swarm default expires at 10 seconds, before the next 15-second ping.
        let idle_boundary = tokio::time::sleep(Duration::from_secs(17));
        tokio::pin!(idle_boundary);
        loop {
            tokio::select! {
                () = &mut idle_boundary => break,
                event = listener.swarm.select_next_some() => {
                    if matches!(event, SwarmEvent::ConnectionClosed { peer_id, .. } if peer_id == dialer_peer) {
                        panic!("listener connection closed before the keepalive ping");
                    }
                }
                event = dialer.swarm.select_next_some() => {
                    if matches!(event, SwarmEvent::ConnectionClosed { peer_id, .. } if peer_id == listener_peer) {
                        panic!("dialer connection closed before the keepalive ping");
                    }
                }
            }
        }

        assert!(listener.swarm.is_connected(&dialer_peer));
        assert!(dialer.swarm.is_connected(&listener_peer));
    }

    #[tokio::test]
    async fn two_quic_nodes_exchange_pinned_packet_stream_request() {
        let mut listener = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec![
                "/ip4/127.0.0.1/udp/0/quic-v1"
                    .parse()
                    .expect("listen address"),
            ],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("listener node");
        let listener_address = next_listen_address(&mut listener.swarm).await;

        let mut dialer = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("dialer identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener.local_peer_id, listener_address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("dialer node");
        let connection_id = next_connection_to_peer(
            &mut listener.swarm,
            &mut dialer.swarm,
            listener.local_peer_id,
        )
        .await;
        let frame = Frame::packet(1, 7, vec![0x45, 0, 0, 20]).expect("frame");
        let request_id = dialer
            .swarm
            .behaviour_mut()
            .pinned_packet_stream
            .send_request_on_connection(listener.local_peer_id, connection_id, frame.clone());

        tokio::time::timeout(
            Duration::from_secs(10),
            exchange_until_pinned_packet_stream_response(
                &mut listener.swarm,
                &mut dialer.swarm,
                frame,
                request_id,
            ),
        )
        .await
        .expect("pinned packet stream exchange timed out");
    }

    #[tokio::test]
    async fn two_nodes_exchange_control_capabilities() {
        let mut listener = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("listener node");
        let listener_address = next_listen_address(&mut listener.swarm).await;

        let mut dialer = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("dialer identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener.local_peer_id, listener_address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("dialer node");
        let request = ControlRequest::Capabilities(ControlCapabilities::local("lab", None, 1280));
        let request_id = dialer
            .swarm
            .behaviour_mut()
            .control
            .send_request(&listener.local_peer_id, request.clone());

        tokio::time::timeout(
            Duration::from_secs(10),
            exchange_until_control_response(
                &mut listener.swarm,
                &mut dialer.swarm,
                request,
                request_id,
            ),
        )
        .await
        .expect("control exchange timed out");
    }

    #[tokio::test]
    async fn two_nodes_exchange_pairing_request() {
        let listener_identity = NodeIdentity::generate_ed25519().expect("listener identity");
        let inviter_config = pairing_config(listener_identity.clone());
        let offer = export_pairing_offer_at(&inviter_config, PairingOfferOptions::default(), 1_000)
            .expect("offer");
        let mut listener = build_node(&HostConfig {
            identity: listener_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("listener node");
        let listener_address = next_listen_address(&mut listener.swarm).await;
        let joiner_identity = NodeIdentity::generate_ed25519().expect("joiner identity");

        let mut dialer = build_node(&HostConfig {
            identity: joiner_identity.clone(),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener.local_peer_id, listener_address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("dialer node");
        let request = build_pairing_request_at(
            &offer,
            PairingRequestOptions {
                identity: joiner_identity.clone(),
                requested_vpn_ip: Some("10.42.0.2".to_owned()),
                requested_routes: Vec::new(),
            },
            1_001,
        )
        .expect("request");
        let expected_response = build_pairing_response_at(
            &inviter_config,
            &offer,
            PairingResponseOptions {
                joiner_peer: joiner_identity.peer_id,
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                membership_key: Some(base64::engine::general_purpose::STANDARD.encode([9_u8; 32])),
                member_records: Vec::new(),
                expires_in_seconds: 300,
            },
            1_002,
        )
        .expect("response");
        let request_id = dialer
            .swarm
            .behaviour_mut()
            .pairing
            .send_request(&listener.local_peer_id, request.clone());

        tokio::time::timeout(
            Duration::from_secs(10),
            exchange_until_pairing_response(
                &mut listener.swarm,
                &mut dialer.swarm,
                request,
                expected_response,
                request_id,
            ),
        )
        .await
        .expect("pairing exchange timed out");
    }

    #[tokio::test]
    async fn two_nodes_exchange_service_status() {
        let mut listener = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("listener node");
        let listener_address = next_listen_address(&mut listener.swarm).await;

        let mut dialer = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("dialer identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener.local_peer_id, listener_address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("dialer node");
        let request = ServiceRequest::Status(ServiceStatusRequest::local("lab", None, 42));
        let request_id = dialer
            .swarm
            .behaviour_mut()
            .service
            .send_request(&listener.local_peer_id, request.clone());

        tokio::time::timeout(
            Duration::from_secs(10),
            exchange_until_service_response(
                &mut listener.swarm,
                &mut dialer.swarm,
                request,
                request_id,
            ),
        )
        .await
        .expect("service exchange timed out");
    }

    #[tokio::test]
    async fn relayed_nodes_exchange_packet_request() {
        let discovery = relay_test_discovery();
        let mut relay = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("relay listen")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("relay node");
        let relay_address = next_listen_address(&mut relay.swarm).await;
        relay.swarm.add_external_address(relay_address.clone());
        let relay_peer = relay.local_peer_id;
        let relayed_listener_address = relay_address
            .clone()
            .with_p2p(relay_peer)
            .expect("relay p2p address")
            .with(Protocol::P2pCircuit);

        let mut listener = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: vec![relayed_listener_address.clone()],
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("listener node");
        let listener_peer = listener.local_peer_id;
        let relayed_target_address = relayed_listener_address
            .clone()
            .with(Protocol::P2p(listener_peer));

        tokio::time::timeout(
            Duration::from_secs(10),
            wait_for_relay_reservation(
                &mut relay.swarm,
                &mut listener.swarm,
                relayed_target_address.clone(),
                relay_peer,
            ),
        )
        .await
        .expect("relay reservation timed out");

        let mut dialer = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("dialer identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener_peer, relayed_target_address.clone())],
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery,
        })
        .expect("dialer node");
        dialer
            .swarm
            .dial(relayed_target_address.clone())
            .expect("dial relayed listener");
        let frame = Frame::packet(2, 9, vec![0x45, 0, 0, 20]).expect("frame");
        let request_id = dialer
            .swarm
            .behaviour_mut()
            .packet
            .send_request(&listener_peer, frame.clone());

        tokio::time::timeout(
            Duration::from_secs(10),
            exchange_until_relayed_response(
                &mut relay.swarm,
                &mut listener.swarm,
                &mut dialer.swarm,
                frame,
                request_id,
            ),
        )
        .await
        .expect("relayed packet exchange timed out");
    }

    #[tokio::test]
    async fn two_relay_reserved_nodes_exchange_packet_request() {
        two_relay_reserved_nodes_exchange_packet_request_with_edge_listeners(Vec::new()).await;
    }

    #[tokio::test]
    async fn two_relay_reserved_nodes_exchange_packet_request_with_direct_edge_listeners() {
        two_relay_reserved_nodes_exchange_packet_request_with_edge_listeners(vec![
            "/ip4/127.0.0.1/tcp/0".parse().expect("node-a listen"),
            "/ip4/127.0.0.1/tcp/0".parse().expect("node-b listen"),
        ])
        .await;
    }

    async fn two_relay_reserved_nodes_exchange_packet_request_with_edge_listeners(
        edge_listeners: Vec<Multiaddr>,
    ) {
        let discovery = relay_test_discovery();
        let mut relay = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("relay listen")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("relay node");
        let relay_address = next_listen_address(&mut relay.swarm).await;
        relay.swarm.add_external_address(relay_address.clone());
        let relay_peer = relay.local_peer_id;
        let relay_reservation_address = relay_address
            .clone()
            .with_p2p(relay_peer)
            .expect("relay p2p address")
            .with(Protocol::P2pCircuit);

        let mut node_a = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("node-a identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: edge_listeners.iter().take(1).cloned().collect(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: vec![relay_reservation_address.clone()],
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("node-a");
        let node_a_peer = node_a.local_peer_id;
        let node_a_relayed_address = relay_reservation_address
            .clone()
            .with(Protocol::P2p(node_a_peer));

        let mut node_b = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("node-b identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: edge_listeners.iter().skip(1).take(1).cloned().collect(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(node_a_peer, node_a_relayed_address.clone())],
            relay_reservations: vec![relay_reservation_address.clone()],
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery,
        })
        .expect("node-b");
        let node_b_peer = node_b.local_peer_id;
        let node_b_relayed_address = relay_reservation_address.with(Protocol::P2p(node_b_peer));

        tokio::time::timeout(
            Duration::from_secs(10),
            wait_for_relay_reservation(
                &mut relay.swarm,
                &mut node_a.swarm,
                node_a_relayed_address.clone(),
                relay_peer,
            ),
        )
        .await
        .expect("node-a relay reservation timed out");
        tokio::time::timeout(
            Duration::from_secs(10),
            wait_for_relay_reservation(
                &mut relay.swarm,
                &mut node_b.swarm,
                node_b_relayed_address,
                relay_peer,
            ),
        )
        .await
        .expect("node-b relay reservation timed out");

        node_b
            .swarm
            .dial(node_a_relayed_address)
            .expect("dial node-a through relay");
        let frame = Frame::packet(2, 9, vec![0x45, 0, 0, 20]).expect("frame");
        let request_id = node_b
            .swarm
            .behaviour_mut()
            .packet
            .send_request(&node_a_peer, frame.clone());

        tokio::time::timeout(
            Duration::from_secs(10),
            exchange_until_relayed_response(
                &mut relay.swarm,
                &mut node_a.swarm,
                &mut node_b.swarm,
                frame,
                request_id,
            ),
        )
        .await
        .expect("relayed packet exchange timed out");
    }

    async fn next_listen_address(swarm: &mut Swarm<Behaviour>) -> Multiaddr {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                return address;
            }
        }
    }

    async fn next_connection_to_peer(
        listener: &mut Swarm<Behaviour>,
        dialer: &mut Swarm<Behaviour>,
        peer: PeerId,
    ) -> libp2p::swarm::ConnectionId {
        loop {
            tokio::select! {
                event = listener.select_next_some() => {
                    let _ = event;
                }
                event = dialer.select_next_some() => {
                    if let SwarmEvent::ConnectionEstablished {
                        peer_id,
                        connection_id,
                        ..
                    } = event
                        && peer_id == peer
                    {
                        return connection_id;
                    }
                }
            }
        }
    }

    fn relay_test_discovery() -> DiscoveryConfig {
        DiscoveryConfig {
            mdns: false,
            kademlia: false,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        }
    }

    async fn wait_for_relay_reservation(
        relay: &mut Swarm<Behaviour>,
        listener: &mut Swarm<Behaviour>,
        relayed_address: Multiaddr,
        relay_peer: PeerId,
    ) {
        let mut listen_addr_reported = false;
        let mut reservation_accepted = false;

        loop {
            tokio::select! {
                event = relay.select_next_some() => {
                    let _ = event;
                }
                event = listener.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(BehaviourEvent::Relay(
                            relay::client::Event::ReservationReqAccepted {
                                relay_peer_id,
                                renewal,
                                ..
                            },
                        )) if relay_peer_id == relay_peer && !renewal => {
                            reservation_accepted = true;
                        }
                        SwarmEvent::NewListenAddr { address, .. } if address == relayed_address => {
                            listen_addr_reported = true;
                        }
                        _ => {}
                    }
                }
            }

            if listen_addr_reported && reservation_accepted {
                return;
            }
        }
    }

    async fn exchange_until_relayed_response(
        relay: &mut Swarm<Behaviour>,
        listener: &mut Swarm<Behaviour>,
        dialer: &mut Swarm<Behaviour>,
        expected_frame: Frame,
        expected_request_id: request_response::OutboundRequestId,
    ) {
        loop {
            tokio::select! {
                event = relay.select_next_some() => {
                    let _ = event;
                }
                event = listener.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
                        message: Message::Request { request, channel, .. },
                        ..
                    })) = event {
                        assert_eq!(request, expected_frame);
                        listener
                            .behaviour_mut()
                            .packet
                            .send_response(channel, packet::PacketResponse::Accepted)
                            .expect("send response");
                    }
                }
                event = dialer.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Packet(
                        request_response::Event::Message {
                            message: Message::Response { request_id, response },
                            ..
                        },
                    )) = event
                    {
                        assert_eq!(request_id, expected_request_id);
                        assert_eq!(response, packet::PacketResponse::Accepted);
                        assert_eq!(expected_frame.header.payload_type, PayloadType::IpPacket);
                        return;
                    }
                }
            }
        }
    }

    async fn exchange_until_response(
        listener: &mut Swarm<Behaviour>,
        dialer: &mut Swarm<Behaviour>,
        expected_frame: Frame,
        expected_request_id: request_response::OutboundRequestId,
    ) {
        loop {
            tokio::select! {
                event = listener.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
                        message: Message::Request { request, channel, .. },
                        ..
                    })) = event {
                        assert_eq!(request, expected_frame);
                        listener
                            .behaviour_mut()
                            .packet
                            .send_response(channel, packet::PacketResponse::Accepted)
                            .expect("send response");
                    }
                }
                event = dialer.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
                        message: Message::Response { request_id, response },
                        ..
                    })) = event {
                        assert_eq!(request_id, expected_request_id);
                        assert_eq!(response, packet::PacketResponse::Accepted);
                        assert_eq!(expected_frame.header.payload_type, PayloadType::IpPacket);
                        return;
                    }
                }
            }
        }
    }

    async fn exchange_until_pinned_packet_stream_response(
        listener: &mut Swarm<Behaviour>,
        dialer: &mut Swarm<Behaviour>,
        expected_frame: Frame,
        expected_request_id: pinned_packet_stream::RequestId,
    ) {
        loop {
            tokio::select! {
                event = listener.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
                            pinned_packet_stream::Event::InboundRequest {
                                peer,
                                connection_id,
                                request_id,
                                frame,
                            },
                        )) => {
                            assert_eq!(peer, *dialer.local_peer_id());
                            assert_eq!(frame, expected_frame);
                            let channel = pinned_packet_stream::Behaviour::response_channel(
                                *dialer.local_peer_id(),
                                connection_id,
                                request_id,
                            );
                            listener
                                .behaviour_mut()
                                .pinned_packet_stream
                                .send_response(channel, packet::PacketResponse::Accepted);
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
                            message: Message::Request { request, channel, .. },
                            ..
                        })) => {
                            assert_eq!(request, expected_frame);
                            listener
                                .behaviour_mut()
                                .packet
                                .send_response(channel, packet::PacketResponse::Accepted)
                                .expect("send response");
                        }
                        _ => {}
                    }
                }
                event = dialer.select_next_some() => {
                    match event {
                        SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
                            pinned_packet_stream::Event::OutboundResponse {
                                request_id,
                                response,
                                ..
                            },
                        )) => {
                            assert_eq!(request_id, expected_request_id);
                            assert_eq!(response, packet::PacketResponse::Accepted);
                            assert_eq!(expected_frame.header.payload_type, PayloadType::IpPacket);
                            return;
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
                            pinned_packet_stream::Event::OutboundFailure {
                                error,
                                ..
                            },
                        )) => {
                            panic!("pinned packet stream outbound failure: {error:?}");
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    async fn exchange_until_control_response(
        listener: &mut Swarm<Behaviour>,
        dialer: &mut Swarm<Behaviour>,
        expected_request: ControlRequest,
        expected_request_id: request_response::OutboundRequestId,
    ) {
        loop {
            tokio::select! {
                event = listener.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                        message: Message::Request { request, channel, .. },
                        ..
                    })) = event {
                        assert_eq!(request, expected_request);
                        listener
                            .behaviour_mut()
                            .control
                            .send_response(
                                channel,
                                ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("lab", None, 1280)),
                            )
                            .expect("send response");
                    }
                }
                event = dialer.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                        message: Message::Response { request_id, response },
                        ..
                    })) = event {
                        assert_eq!(request_id, expected_request_id);
                        assert_eq!(
                            response,
                            ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("lab", None, 1280))
                        );
                        return;
                    }
                }
            }
        }
    }

    async fn exchange_until_pairing_response(
        listener: &mut Swarm<Behaviour>,
        dialer: &mut Swarm<Behaviour>,
        expected_request: PairingRequest,
        expected_response: crate::pairing::PairingResponse,
        expected_request_id: request_response::OutboundRequestId,
    ) {
        loop {
            tokio::select! {
                event = listener.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Pairing(request_response::Event::Message {
                        message: Message::Request { request, channel, .. },
                        ..
                    })) = event {
                        assert_eq!(request, expected_request);
                        listener
                            .behaviour_mut()
                            .pairing
                            .send_response(channel, expected_response.clone())
                            .expect("send response");
                    }
                }
                event = dialer.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Pairing(request_response::Event::Message {
                        message: Message::Response { request_id, response },
                        ..
                    })) = event {
                        assert_eq!(request_id, expected_request_id);
                        assert_eq!(response, expected_response);
                        return;
                    }
                }
            }
        }
    }

    async fn exchange_until_service_response(
        listener: &mut Swarm<Behaviour>,
        dialer: &mut Swarm<Behaviour>,
        expected_request: ServiceRequest,
        expected_request_id: request_response::OutboundRequestId,
    ) {
        loop {
            tokio::select! {
                event = listener.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Service(request_response::Event::Message {
                        message: Message::Request { request, channel, .. },
                        ..
                    })) = event {
                        assert_eq!(request, expected_request);
                        listener
                            .behaviour_mut()
                            .service
                            .send_response(
                                channel,
                                ServiceResponse::Status(ServiceStatusResponse::local("lab", None, 42, 1280)),
                            )
                            .expect("send response");
                    }
                }
                event = dialer.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Service(request_response::Event::Message {
                        message: Message::Response { request_id, response },
                        ..
                    })) = event {
                        assert_eq!(request_id, expected_request_id);
                        assert_eq!(
                            response,
                            ServiceResponse::Status(ServiceStatusResponse::local("lab", None, 42, 1280))
                        );
                        return;
                    }
                }
            }
        }
    }
}
