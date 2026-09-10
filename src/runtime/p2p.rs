use std::{
    collections::HashSet,
    error::Error,
    num::{NonZeroU8, NonZeroUsize},
    time::Duration,
};

use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder, allow_block_list, autonat,
    connection_limits,
    core::transport::ListenerId,
    dcutr, dns, identify,
    identity::Keypair,
    kad, mdns,
    multiaddr::Protocol,
    noise, ping, relay, request_response,
    swarm::{ConnectionId, NetworkBehaviour, behaviour::toggle::Toggle, dial_opts::DialOpts},
    tcp, yamux,
};

use crate::{
    config::{
        DiscoveryConfig, PUBLIC_IPFS_KADEMLIA_PROTOCOL, RelayResourceConfig, ResourceConfig,
        public_ipfs_bootstrap_peer_configs,
    },
    identity::{IdentityError, NodeIdentity},
    runtime::{
        control::{self, ControlCodec},
        packet::{self, PacketCodec},
        pairing::{self, PairingCodec},
        pairing_code::{self, PairingCodeCodec, PairingCodeV2Codec},
        pinned_packet_stream,
        service::{self, ServiceCodec},
    },
};

#[cfg(test)]
mod public_provider_tests;

const PROTOCOL_VERSION: &str = "/p2p-vpn/0.1.0";
const CONNECTION_PING_INTERVAL: Duration = Duration::from_secs(15);
const CONNECTION_PING_TIMEOUT: Duration = Duration::from_secs(20);
const SWARM_IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
const DIAL_CONCURRENCY_FACTOR: NonZeroU8 = NonZeroU8::MIN;
const KADEMLIA_QUERY_PARALLELISM: NonZeroUsize = NonZeroUsize::MIN;
const KADEMLIA_QUERY_POOL_CAPACITY: NonZeroUsize = NonZeroUsize::new(32).unwrap();
const KADEMLIA_QUERY_CANDIDATES: usize = 256;
const KADEMLIA_QUERY_ADDRESS_BYTES: usize = 256 * 1024;

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub(crate) connection_retention: super::connection_retention::Behaviour,
    pub connection_limits: connection_limits::Behaviour,
    pub blocked_peers: allow_block_list::Behaviour<allow_block_list::BlockedPeers>,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub kad: kad::Behaviour<kad::store::MemoryStore>,
    pub pairing_kad: Toggle<kad::Behaviour<kad::store::MemoryStore>>,
    pub relay: relay::client::Behaviour,
    pub relay_server: Toggle<relay::Behaviour>,
    pub dcutr: Toggle<dcutr::Behaviour>,
    pub autonat: Toggle<autonat::Behaviour>,
    pub mdns: Toggle<mdns::tokio::Behaviour>,
    pub pairing_mdns: Toggle<mdns::tokio::Behaviour>,
    pub control: request_response::Behaviour<ControlCodec>,
    // SelectUpgrade gives the first matching handler priority. Keep this before
    // pinned_packet_stream to preserve the default inbound Packet event contract.
    pub packet: request_response::Behaviour<PacketCodec>,
    pub pairing: request_response::Behaviour<PairingCodec>,
    pub pairing_code: request_response::Behaviour<PairingCodeCodec>,
    pub pairing_code_v2: request_response::Behaviour<PairingCodeV2Codec>,
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
    build_node_with_dial_observer(config, |_| {})
}

pub(crate) fn build_node_with_dial_observer(
    config: &HostConfig,
    mut dial_started: impl FnMut(ConnectionId),
) -> Result<P2pNode, P2pBuildError> {
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
    let public_pairing_uses_primary_kad =
        discovery.kademlia_protocol == PUBLIC_IPFS_KADEMLIA_PROTOCOL;

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
                let kad_config = controlled_kademlia_config(kademlia_protocol);
                let mut kad = kad::Behaviour::with_config(local_peer_id, store, kad_config);
                if behaviour_discovery.kademlia
                    && behaviour_discovery.kademlia_protocol != PUBLIC_IPFS_KADEMLIA_PROTOCOL
                {
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
                let pairing_kad = if public_pairing_uses_primary_kad {
                    None
                } else {
                    let store = kad::store::MemoryStore::new(local_peer_id);
                    let config = controlled_kademlia_config(StreamProtocol::new(
                        PUBLIC_IPFS_KADEMLIA_PROTOCOL,
                    ));
                    let mut behaviour = kad::Behaviour::with_config(local_peer_id, store, config);
                    behaviour.set_mode(Some(kad::Mode::Client));
                    Some(behaviour)
                };

                Ok(Behaviour {
                    connection_retention: super::connection_retention::Behaviour::default(),
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
                    pairing_kad: pairing_kad.into(),
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
                    pairing_mdns: None.into(),
                    control: control::behaviour(control_streams),
                    packet: packet::behaviour(mtu, packet_streams),
                    pairing: pairing::behaviour(control_streams),
                    pairing_code: pairing_code::behaviour(control_streams),
                    pairing_code_v2: pairing_code::behaviour_v2(control_streams),
                    pinned_packet_stream: pinned_packet_stream::Behaviour::new(usize::from(mtu))
                        .with_max_concurrent_streams(packet_streams),
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
    let configured_relay_reservation_listeners =
        install_listeners_and_dials(&mut swarm, config, &mut dial_started)?;
    seed_public_pairing_kademlia(&mut swarm)?;
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
    dial_started: &mut impl FnMut(ConnectionId),
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
            swarm
                .behaviour_mut()
                .kad
                .add_protected_address(peer, address.clone());
        }
        let dial_address = peer_dial_address(*peer, address.clone())?;
        let options: DialOpts = dial_address.into();
        let connection_id = options.connection_id();
        swarm.dial(options)?;
        dial_started(connection_id);
    }

    for (peer, address) in &config.known_peers {
        if should_seed_kademlia_address_book(&config.discovery, address) {
            swarm
                .behaviour_mut()
                .kad
                .add_protected_address(peer, address.clone());
        }
        if is_relayed_address(address) {
            continue;
        }

        let dial_address = peer_dial_address(*peer, address.clone())?;
        let options: DialOpts = dial_address.into();
        let connection_id = options.connection_id();
        swarm.dial(options)?;
        dial_started(connection_id);
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

pub(super) fn controlled_kademlia_config(protocol: StreamProtocol) -> kad::Config {
    let mut config = kad::Config::new(protocol);
    let address_limits = kad::AddressLimits::new(
        NonZeroUsize::new(super::address_retention::MAX_DISCOVERED_ADDRESSES_PER_PEER).unwrap(),
        NonZeroUsize::new(super::address_retention::MAX_DISCOVERED_ADDRESS_BYTES).unwrap(),
    );
    config
        .set_parallelism(KADEMLIA_QUERY_PARALLELISM)
        .set_query_pool_capacity(KADEMLIA_QUERY_POOL_CAPACITY)
        .set_behaviour_queue_limits(kad::BehaviourQueueLimits::new(
            NonZeroUsize::new(512).unwrap(),
            NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
        ))
        .set_query_metadata_limits(Some(kad::QueryMetadataLimits::new(
            NonZeroUsize::new(256 * 1024).unwrap(),
            NonZeroUsize::new(KADEMLIA_QUERY_CANDIDATES).unwrap(),
            address_limits,
        )))
        .set_pending_rpc_limits(kad::PendingRpcLimits::new(
            NonZeroUsize::new(256).unwrap(),
            NonZeroUsize::new(1024 * 1024).unwrap(),
        ))
        .set_handler_queue_limits(kad::HandlerQueueLimits::new(
            NonZeroUsize::new(64).unwrap(),
            NonZeroUsize::new(256 * 1024).unwrap(),
        ))
        .set_routing_limits(kad::RoutingLimits::new(
            NonZeroUsize::new(512).unwrap(),
            NonZeroUsize::new(2 * 1024 * 1024).unwrap(),
        ))
        .set_background_query_limits(NonZeroUsize::new(2).unwrap(), NonZeroUsize::MIN)
        .set_background_job_limits(kad::BackgroundJobLimits::new(
            NonZeroUsize::new(64).unwrap(),
            NonZeroUsize::new(1024 * 1024).unwrap(),
            NonZeroUsize::new(256 * 1024).unwrap(),
        ))
        .set_address_limits(address_limits)
        .set_query_limits(kad::QueryLimits::new(
            NonZeroUsize::new(KADEMLIA_QUERY_CANDIDATES).unwrap(),
            address_limits,
            NonZeroUsize::new(KADEMLIA_QUERY_ADDRESS_BYTES).unwrap(),
        ))
        .set_periodic_bootstrap_interval(None)
        .set_automatic_bootstrap_throttle(None);
    config
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

pub(crate) fn kademlia_provider_wire_key(
    behaviour: &kad::Behaviour<kad::store::MemoryStore>,
    key: &kad::RecordKey,
) -> kad::RecordKey {
    use sha2::{Digest, Sha256};

    if key.as_ref().len() <= 80
        || !behaviour
            .protocol_names()
            .iter()
            .any(|protocol| protocol.as_ref() == PUBLIC_IPFS_KADEMLIA_PROTOCOL)
    {
        return key.clone();
    }
    // Keep legacy keys where supported; public servers reject longer provider keys.
    let digest = Sha256::digest(key.as_ref());
    let mut multihash = vec![0x12, 0x20];
    multihash.extend_from_slice(&digest);
    kad::RecordKey::new(&multihash)
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

#[must_use]
pub fn kademlia_pairing_code_v2_key(locator: &str) -> kad::RecordKey {
    kad::RecordKey::new(&format!("/p2p-vpn/pairing-code/{locator}/providers/2"))
}

#[must_use]
pub fn public_pairing_uses_primary_kad(behaviour: &Behaviour) -> bool {
    !behaviour.pairing_kad.is_enabled()
}

pub fn public_pairing_kad_mut(
    behaviour: &mut Behaviour,
) -> &mut kad::Behaviour<kad::store::MemoryStore> {
    if behaviour.pairing_kad.is_enabled() {
        behaviour
            .pairing_kad
            .as_mut()
            .expect("enabled public pairing Kademlia behaviour")
    } else {
        &mut behaviour.kad
    }
}

fn seed_public_pairing_kademlia(swarm: &mut Swarm<Behaviour>) -> Result<(), P2pBuildError> {
    for configured in public_ipfs_bootstrap_peer_configs() {
        let (peer, mut address) = configured.peer_address()?;
        if matches!(address.iter().last(), Some(Protocol::P2p(address_peer)) if address_peer == peer)
        {
            address.pop();
        }
        public_pairing_kad_mut(swarm.behaviour_mut()).add_protected_address(&peer, address);
    }
    Ok(())
}

fn kademlia_stream_protocol(protocol: &str) -> Result<libp2p::StreamProtocol, P2pBuildError> {
    libp2p::StreamProtocol::try_from_owned(protocol.to_owned())
        .map_err(|_| P2pBuildError::InvalidKademliaProtocol(protocol.to_owned()))
}

pub(crate) fn decode_keypair(encoded: &str) -> Result<Keypair, IdentityError> {
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
    Config(crate::config::ConfigError),
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

impl From<crate::config::ConfigError> for P2pBuildError {
    fn from(error: crate::config::ConfigError) -> Self {
        Self::Config(error)
    }
}

#[cfg(test)]
#[path = "p2p/metadata_tests.rs"]
mod metadata_tests;

#[cfg(test)]
#[path = "p2p/background_tests.rs"]
mod background_tests;

#[cfg(test)]
#[path = "p2p/queue_tests.rs"]
mod queue_tests;

#[cfg(test)]
mod tests {
    mod resource_tests {
        include!("p2p/resource_tests.rs");
    }

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
        runtime::service::{
            ServiceRequest, ServiceResponse, ServiceStatusRequest, ServiceStatusResponse,
        },
        runtime::{packet::PacketResponse, pinned_packet_stream},
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
        assert_eq!(
            node.swarm
                .behaviour()
                .kad
                .behaviour_queue_usage()
                .event_limit,
            Some(512)
        );
        assert_eq!(
            node.swarm
                .behaviour()
                .kad
                .behaviour_queue_usage()
                .byte_limit,
            Some(4 * 1024 * 1024)
        );
        assert_eq!(
            node.swarm
                .behaviour()
                .kad
                .background_job_usage()
                .bounded_jobs,
            2
        );
        assert!(!node.swarm.behaviour().mdns.is_enabled());
        assert!(!node.swarm.behaviour().pairing_mdns.is_enabled());
        assert!(!node.swarm.behaviour().dcutr.is_enabled());
        assert!(!node.swarm.behaviour().autonat.is_enabled());
        assert!(node.swarm.behaviour().pairing_kad.is_enabled());
    }

    #[tokio::test]
    async fn build_node_accepts_ipfs_compatible_kademlia_protocol() {
        let discovery = DiscoveryConfig {
            kademlia_protocol: "/ipfs/kad/1.0.0".to_owned(),
            ..DiscoveryConfig::default()
        };

        let mut node = build_node(&HostConfig {
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
        assert_eq!(node.swarm.behaviour().kad.mode(), kad::Mode::Client);
        assert!(!node.swarm.behaviour().pairing_kad.is_enabled());
        assert!(public_pairing_uses_primary_kad(node.swarm.behaviour()));
        assert_eq!(
            node.swarm.behaviour().kad.query_pool_usage().capacity,
            Some(32)
        );
        assert!(!node.startup.kademlia.rendezvous_advertise_started);
        assert!(!node.startup.kademlia.rendezvous_lookup_started);
        assert!(matches!(
            node.swarm
                .behaviour_mut()
                .kad
                .try_get_closest_peers(vec![0; 256 * 1024 + 1]),
            Err(kad::QueryStartError::InputTooLarge(_))
        ));
    }

    #[tokio::test]
    async fn build_node_serves_only_private_kademlia_protocols() {
        let discovery = DiscoveryConfig {
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            ..DiscoveryConfig::default()
        };

        let mut node = build_node(&HostConfig {
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
            discovery,
        })
        .expect("node should build");

        assert_eq!(node.swarm.behaviour().kad.mode(), kad::Mode::Server);
        assert!(node.swarm.behaviour().pairing_kad.is_enabled());
        assert_eq!(
            node.swarm.behaviour().kad.query_pool_usage().capacity,
            Some(32)
        );
        assert_eq!(
            node.swarm
                .behaviour()
                .pairing_kad
                .as_ref()
                .unwrap()
                .query_pool_usage()
                .capacity,
            Some(32)
        );
        let behaviour = node.swarm.behaviour_mut();
        for kad in [&mut behaviour.kad, behaviour.pairing_kad.as_mut().unwrap()] {
            assert_eq!(kad.background_job_usage().bounded_jobs, 2);
            assert_eq!(kad.behaviour_queue_usage().event_limit, Some(512));
            assert_eq!(
                kad.behaviour_queue_usage().byte_limit,
                Some(4 * 1024 * 1024)
            );
            assert!(matches!(
                kad.try_get_closest_peers(vec![0; 256 * 1024 + 1]),
                Err(kad::QueryStartError::InputTooLarge(_))
            ));
        }
    }

    #[test]
    fn pairing_code_v2_provider_key_is_versioned_and_distinct() {
        let locator = "global-locator";

        assert_eq!(
            kademlia_pairing_code_v2_key(locator).to_vec(),
            b"/p2p-vpn/pairing-code/global-locator/providers/2"
        );
        assert_ne!(
            kademlia_pairing_code_v2_key(locator),
            kademlia_pairing_code_key(locator)
        );
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
    async fn startup_dial_registration_observes_only_admitted_attempts() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address: Multiaddr = format!(
            "/ip4/127.0.0.1/tcp/{}",
            listener.local_addr().unwrap().port()
        )
        .parse()
        .unwrap();
        for bootstrap in [true, false] {
            for allowed in [false, true] {
                let peer = Keypair::generate_ed25519().public().to_peer_id();
                let target = vec![(peer, address.clone())];
                let config = HostConfig {
                    identity: NodeIdentity::generate_ed25519().unwrap(),
                    network_name: "registration".to_owned(),
                    membership_tag: None,
                    mtu: 1280,
                    max_concurrent_control_streams: 64,
                    max_concurrent_packet_streams: 256,
                    listen_addresses: Vec::new(),
                    external_addresses: Vec::new(),
                    bootstrap_peers: if bootstrap {
                        target.clone()
                    } else {
                        Vec::new()
                    },
                    known_peers: if bootstrap { Vec::new() } else { target },
                    relay_reservations: Vec::new(),
                    relay_server: false,
                    relay_resources: crate::config::RelayResourceConfig::default(),
                    resources: crate::config::ResourceConfig {
                        max_pending_outgoing_connections: u32::from(allowed),
                        ..crate::config::ResourceConfig::default()
                    },
                    discovery: DiscoveryConfig {
                        mdns: false,
                        dcutr: false,
                        autonat: false,
                        ..DiscoveryConfig::default()
                    },
                };
                let mut admitted = Vec::new();
                let node = build_node_with_dial_observer(&config, |id| admitted.push(id));
                assert_eq!(node.is_ok(), allowed);
                assert_eq!(
                    admitted.len(),
                    usize::from(allowed),
                    "startup admission was not registered correctly"
                );
            }
        }
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
    async fn provider_wire_keys_bound_public_keys_and_preserve_private_compatibility() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let public = kad::Behaviour::with_config(
            peer,
            kad::store::MemoryStore::new(peer),
            controlled_kademlia_config(StreamProtocol::new(PUBLIC_IPFS_KADEMLIA_PROTOCOL)),
        );
        let private = kad::Behaviour::with_config(
            peer,
            kad::store::MemoryStore::new(peer),
            controlled_kademlia_config(StreamProtocol::new("/p2p-vpn/kad/1")),
        );
        for len in [1, 79, 80, 81, 256] {
            let key = kad::RecordKey::new(&vec![b'x'; len]);
            assert_eq!(kademlia_provider_wire_key(&private, &key), key);
            let wire = kademlia_provider_wire_key(&public, &key);
            if len <= 80 {
                assert_eq!(wire, key);
            } else {
                assert_eq!(wire.as_ref().len(), 34);
                assert_eq!(&wire.as_ref()[..2], &[0x12, 0x20]);
                assert_eq!(kademlia_provider_wire_key(&public, &wire), wire);
            }
        }
        let mut keys = HashSet::new();
        for network in ["personal-devices".to_owned(), "n".repeat(128)] {
            for secret in [b"old".as_slice(), b"new".as_slice()] {
                let tag = crate::config::membership_tag(&network, secret);
                assert_eq!(tag.len(), 44);
                let key = kademlia_rendezvous_key(&network, Some(&tag));
                assert!(key.as_ref().len() > 80);
                let wire = kademlia_provider_wire_key(&public, &key);
                assert_eq!(wire.as_ref().len(), 34);
                assert!(keys.insert(wire));
            }
        }
    }

    #[tokio::test]
    async fn provider_wire_key_is_shared_by_publication_lookup_and_withdrawal() {
        use libp2p::kad::store::RecordStore as _;

        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let mut kad = kad::Behaviour::with_config(
            peer,
            kad::store::MemoryStore::new(peer),
            controlled_kademlia_config(StreamProtocol::new(PUBLIC_IPFS_KADEMLIA_PROTOCOL)),
        );
        let tag = crate::config::membership_tag("personal-devices", b"fixture");
        let logical = kademlia_rendezvous_key("personal-devices", Some(&tag));
        assert_eq!(logical.as_ref().len(), 90);
        let published = kademlia_provider_wire_key(&kad, &logical);
        let publish = kad.try_start_providing(published.clone()).unwrap().unwrap();
        assert!(
            matches!(kad.query(&publish).unwrap().info(), kad::QueryInfo::AddProvider { key, .. } if key == &published)
        );
        assert!(
            kad.store_mut()
                .provided()
                .any(|record| record.key == published)
        );
        let lookup_key = kademlia_provider_wire_key(&kad, &logical);
        let lookup = kad.try_get_providers(lookup_key).unwrap();
        assert!(
            matches!(kad.query(&lookup).unwrap().info(), kad::QueryInfo::GetProviders { key, .. } if key == &published)
        );
        let withdraw = kademlia_provider_wire_key(&kad, &logical);
        kad.stop_providing(&withdraw);
        assert_eq!(kad.store_mut().provided().count(), 0);
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
    async fn routing_updates_do_not_start_unowned_bootstrap_queries() {
        for separate in [false, true] {
            let mut node = build_node(&retention_diagnostic_config(separate)).unwrap();
            let kad = public_pairing_kad_mut(node.swarm.behaviour_mut());
            for seed in public_ipfs_bootstrap_peer_configs() {
                let (peer, _) = seed.peer_address().unwrap();
                kad.remove_peer(&peer);
            }
            kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
            let deadline = tokio::time::sleep(Duration::from_secs(1));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    () = &mut deadline => break,
                    event = node.swarm.select_next_some() => {
                        let event = match event {
                            SwarmEvent::Behaviour(BehaviourEvent::Kad(event)) if !separate => Some(event),
                            SwarmEvent::Behaviour(BehaviourEvent::PairingKad(event)) if separate => Some(event),
                            _ => None,
                        };
                        assert!(
                            !matches!(event, Some(kad::Event::OutboundQueryProgressed {
                                result: kad::QueryResult::Bootstrap(_), ..
                            })),
                            "routing insertion started bootstrap outside the runtime scheduler"
                        );
                    }
                }
            }
            let kad = public_pairing_kad_mut(node.swarm.behaviour_mut());
            assert_eq!(kad.iter_queries().count(), 0);
            let manual = kad
                .bootstrap()
                .expect("explicit bootstrap remains available");
            assert!(kad.query(&manual).is_some());
        }
    }

    #[test]
    fn expired_kademlia_query_does_not_dial_remaining_candidates() {
        check_kademlia_query_deadline(false);
    }

    #[test]
    fn explicitly_finished_kademlia_query_is_not_reclassified_as_timeout() {
        check_kademlia_query_deadline(true);
    }

    fn check_kademlia_query_deadline(finish: bool) {
        use std::task::{Context, Poll};

        use libp2p::swarm::{DialError, FromSwarm, NetworkBehaviour, ToSwarm};

        let local = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_query_timeout(Duration::from_millis(20));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        for index in 1..=3 {
            kad.add_address(
                &PeerId::random(),
                format!("/memory/{index}").parse().unwrap(),
            );
        }
        let query = kad.get_closest_peers(PeerId::random());
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let first = loop {
            match kad.poll(&mut cx) {
                Poll::Ready(ToSwarm::Dial { opts }) => break opts,
                Poll::Ready(ToSwarm::GenerateEvent(kad::Event::RoutingUpdated { .. })) => {}
                other => panic!("expected initial candidate dial, got {other:?}"),
            }
        };
        assert_eq!(
            kad.query_lifecycle_usage(),
            kad::QueryLifecycleUsage {
                admitted_phases: 1,
                requests: 1,
                ..Default::default()
            }
        );
        std::thread::sleep(Duration::from_millis(30));
        kad.on_swarm_event(FromSwarm::DialFailure(
            libp2p::swarm::behaviour::DialFailure {
                peer_id: first.get_peer_id(),
                connection_id: first.connection_id(),
                error: &DialError::NoAddresses,
            },
        ));
        if finish {
            kad.query_mut(&query).unwrap().finish();
        }
        assert_eq!(kad.query_lifecycle_usage().retired_phases, 0);
        assert_eq!(kad.query_resource_snapshot().bounded_queries, 1);
        let mut reported = false;
        for _ in 0..10 {
            match kad.poll(&mut cx) {
                Poll::Ready(ToSwarm::Dial { .. }) => {
                    panic!("expired query dialed another candidate")
                }
                Poll::Ready(ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
                    id,
                    result: kad::QueryResult::GetClosestPeers(result),
                    ..
                })) => {
                    assert_eq!(id, query);
                    assert_eq!(result.is_ok(), finish);
                    if !finish {
                        assert!(matches!(
                            result,
                            Err(kad::GetClosestPeersError::Timeout { .. })
                        ));
                    }
                    reported = true;
                }
                Poll::Pending => break,
                other => panic!("unexpected event after expiry: {other:?}"),
            }
        }
        assert!(
            reported,
            "query must report its result instead of silently disappearing"
        );
        assert!(kad.query(&query).is_none());
        assert_eq!(
            kad.query_lifecycle_usage(),
            kad::QueryLifecycleUsage {
                admitted_phases: 1,
                retired_phases: 1,
                completed_phases: u64::from(finish),
                timed_out_phases: u64::from(!finish),
                requests: 1,
                failures: 1,
                ..Default::default()
            }
        );
        assert_eq!(kad.query_resource_snapshot(), Default::default());
    }

    fn bounded_routing_test_dht() -> kad::Behaviour<kad::store::MemoryStore> {
        let local = libp2p::identity::Keypair::ed25519_from_bytes([0; 32])
            .unwrap()
            .public()
            .to_peer_id();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_address_limits(kad::AddressLimits::new(
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2048).unwrap(),
        ));
        kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config)
    }

    #[test]
    fn kademlia_fixed_query_initial_candidates_obey_budget() {
        use libp2p::swarm::{DialError, FromSwarm, NetworkBehaviour, ToSwarm};
        use std::task::{Context, Poll};

        for bounded in [false, true] {
            let mut kad = if bounded {
                bounded_routing_test_dht()
            } else {
                let local = PeerId::random();
                kad::Behaviour::new(local, kad::store::MemoryStore::new(local))
            };
            let peers = (0..512).map(|_| PeerId::random()).collect::<Vec<_>>();
            let query = kad.put_record_to(
                kad::Record::new(b"bounded".to_vec(), b"value".to_vec()),
                peers.into_iter(),
                kad::Quorum::All,
            );
            let expected = if bounded {
                KADEMLIA_QUERY_CANDIDATES
            } else {
                512
            };
            let usage = kad.query(&query).unwrap().resource_usage();
            assert_eq!(
                usage.map(|usage| usage.candidates),
                bounded.then_some(expected)
            );
            let waker = futures::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            let mut dials = 0;
            let mut finished = false;
            for _ in 0..1024 {
                match kad.poll(&mut cx) {
                    Poll::Ready(ToSwarm::Dial { opts }) => {
                        dials += 1;
                        assert!(dials <= expected);
                        kad.on_swarm_event(FromSwarm::DialFailure(
                            libp2p::swarm::behaviour::DialFailure {
                                peer_id: opts.get_peer_id(),
                                connection_id: opts.connection_id(),
                                error: &DialError::NoAddresses,
                            },
                        ));
                    }
                    Poll::Ready(ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
                        id,
                        result: kad::QueryResult::PutRecord(result),
                        stats,
                        ..
                    })) => {
                        assert_eq!(id, query);
                        assert!(result.is_err(), "a capped query must not lower its quorum");
                        assert_eq!(stats.num_requests() as usize, expected);
                        finished = true;
                        break;
                    }
                    other => panic!("unexpected fixed-query event: {other:?}"),
                }
            }
            assert!(finished);
            assert_eq!(dials, expected);
            assert!(kad.query(&query).is_none());
        }
    }

    #[test]
    fn kademlia_background_jobs_share_remaining_query_capacity() {
        use libp2p::{kad::store::RecordStore, swarm::NetworkBehaviour};
        use std::task::Context;

        let local = PeerId::random();
        let mut config = kad::Config::new(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config
            .set_periodic_bootstrap_interval(None)
            .set_automatic_bootstrap_throttle(None)
            .set_provider_publication_interval(Some(Duration::from_millis(1)))
            .set_replication_interval(Some(Duration::from_millis(1)))
            .set_publication_interval(Some(Duration::from_millis(1)));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
        for _ in 0..99 {
            kad.get_closest_peers(PeerId::random());
        }
        for index in 0..10 {
            let key = kad::RecordKey::new(&[index]);
            kad.store_mut()
                .add_provider(kad::ProviderRecord::new(key.clone(), local, Vec::new()))
                .unwrap();
            kad.store_mut().put(kad::Record::new(key, vec![1])).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
        let waker = futures::task::noop_waker();
        let _ = kad.poll(&mut Context::from_waker(&waker));
        assert!(
            kad.iter_queries().count() <= 100,
            "background jobs reused the same remaining capacity"
        );
    }

    #[test]
    fn handler_stalled_negotiations_expire_queued_requests_and_resume() {
        use libp2p::{
            core::{ConnectedPoint, Endpoint, transport::PortUse},
            swarm::{
                ConnectionHandler, ConnectionHandlerEvent, ConnectionId, NetworkBehaviour, ToSwarm,
            },
        };
        use std::task::Context;
        let local = PeerId::random();
        let remote = PeerId::random();
        let address: Multiaddr = "/memory/1".parse().unwrap();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        // Isolate handler retirement with 32 active and 32 waiting requests.
        config.set_query_pool_capacity(NonZeroUsize::new(64).unwrap());
        config.set_substreams_timeout(Duration::from_millis(100));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        kad.add_address(&remote, address.clone());
        for index in 0_u64..64 {
            kad.get_record(kad::RecordKey::new(&index.to_le_bytes()));
        }
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        for _ in 0..200 {
            let _ = kad.poll(&mut cx);
        }
        let connection = ConnectionId::new_unchecked(1);
        let mut handler = kad
            .handle_established_outbound_connection(
                connection,
                remote,
                &address,
                Endpoint::Dialer,
                PortUse::Reuse,
            )
            .unwrap();
        for _ in 0..32 {
            assert!(matches!(
                handler.poll(&mut cx),
                std::task::Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest { .. })
            ));
        }
        assert_eq!(handler.pending_request_usage().requests, 32);
        let canceled = kad
            .iter_queries()
            .map(|query| query.id())
            .collect::<Vec<_>>();
        assert_eq!(canceled.len(), 64);
        for query in canceled {
            assert!(kad.cancel_query(&query));
        }
        assert_eq!(handler.pending_request_usage().requests, 32);
        std::thread::sleep(Duration::from_millis(150));
        for _ in 0..200 {
            if let std::task::Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event)) =
                handler.poll(&mut cx)
            {
                kad.on_connection_handler_event(remote, connection, event);
            }
        }
        let usage = handler.pending_request_usage();
        assert_eq!(usage.requests, 0);
        assert_eq!(usage.bytes, 0);
        assert_eq!(usage.expired, 32);
        assert_eq!(usage.pending_negotiations, 32);
        assert_eq!(usage.active_outbound_streams, 0);
        assert_eq!(kad.handler_resource_usage().handlers, 1);
        assert_eq!(kad.handler_resource_usage().usage, usage);
        let endpoint = ConnectedPoint::Dialer {
            address: address.clone(),
            role_override: Endpoint::Dialer,
            port_use: PortUse::Reuse,
        };
        kad.on_swarm_event(libp2p::swarm::FromSwarm::ConnectionEstablished(
            libp2p::swarm::behaviour::ConnectionEstablished {
                peer_id: remote,
                connection_id: connection,
                endpoint: &endpoint,
                failed_addresses: &[],
                other_established: 0,
            },
        ));
        let query = kad.get_record(kad::RecordKey::new(&b"new"));
        for _ in 0..200 {
            if let std::task::Poll::Ready(ToSwarm::NotifyHandler { event, .. }) = kad.poll(&mut cx)
            {
                handler.on_behaviour_event(event);
            }
        }
        assert_eq!(handler.pending_request_usage().requests, 1);
        assert!(matches!(handler.poll(&mut cx), std::task::Poll::Pending));
        assert_eq!(handler.pending_request_usage().pending_negotiations, 32);
        for _ in 0..32 {
            handler.on_connection_event(libp2p::swarm::handler::ConnectionEvent::DialUpgradeError(
                libp2p::swarm::handler::DialUpgradeError {
                    info: (),
                    error: libp2p::swarm::StreamUpgradeError::Timeout,
                },
            ));
        }
        assert!(matches!(
            handler.poll(&mut cx),
            std::task::Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest { .. })
        ));
        assert_eq!(handler.pending_request_usage().requests, 0);
        assert_eq!(handler.pending_request_usage().pending_negotiations, 1);
        assert!(kad.query(&query).is_some());
    }

    #[test]
    fn handler_pending_requests_enforce_count_bytes_and_bounded_rejections() {
        use libp2p::{
            core::{Endpoint, transport::PortUse},
            swarm::{
                ConnectionHandler, ConnectionHandlerEvent, ConnectionId, NetworkBehaviour, ToSwarm,
            },
        };
        use std::task::Context;
        for (count, bytes, admitted, queued_errors, deferred) in
            [(4, 4096, 4, 4, 12), (64, 16, 2, 18, 0), (64, 4, 0, 20, 0)]
        {
            let local = PeerId::random();
            let remote = PeerId::random();
            let address: Multiaddr = "/memory/1".parse().unwrap();
            let mut config = controlled_kademlia_config(StreamProtocol::new(
                crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            ));
            config.set_handler_queue_limits(kad::HandlerQueueLimits::new(
                NonZeroUsize::new(count).unwrap(),
                NonZeroUsize::new(bytes).unwrap(),
            ));
            config.set_query_timeout(Duration::from_millis(100));
            let mut kad =
                kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
            kad.add_address(&remote, address.clone());
            let queries = (0_u64..20)
                .map(|index| kad.get_record(kad::RecordKey::new(&index.to_le_bytes())))
                .collect::<Vec<_>>();
            let waker = futures::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            for _ in 0..100 {
                let _ = kad.poll(&mut cx);
            }
            let connection = ConnectionId::new_unchecked(1);
            let mut handler = kad
                .handle_established_outbound_connection(
                    connection,
                    remote,
                    &address,
                    Endpoint::Dialer,
                    PortUse::Reuse,
                )
                .unwrap();
            let usage = handler.pending_request_usage();
            assert_eq!(usage.requests, admitted);
            assert_eq!(usage.bytes, admitted * 8);
            assert_eq!(usage.queued_rejections, queued_errors);
            assert_eq!(usage.rejected, 20 - admitted as u64);
            assert_eq!(usage.unreported_rejections, deferred);
            let mut streams = 0;
            for _ in 0..100 {
                match handler.poll(&mut cx) {
                    std::task::Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        ..
                    }) => streams += 1,
                    std::task::Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event)) => {
                        kad.on_connection_handler_event(remote, connection, event)
                    }
                    _ => {}
                }
            }
            assert_eq!(streams, admitted);
            assert_eq!(handler.pending_request_usage().bytes, 0);
            assert_eq!(handler.pending_request_usage().queued_rejections, 0);
            std::thread::sleep(Duration::from_millis(120));
            for _ in 0..100 {
                assert!(!matches!(
                    kad.poll(&mut cx),
                    std::task::Poll::Ready(ToSwarm::CloseConnection { .. })
                ));
            }
            for query in queries {
                assert!(
                    !kad.cancel_query(&query),
                    "query deadline did not retire deferred rejection"
                );
            }
        }
    }

    #[test]
    fn canceling_bootstrap_releases_automatic_bootstrap_suppression() {
        use libp2p::swarm::NetworkBehaviour;
        use std::task::Context;
        let local = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_periodic_bootstrap_interval(Some(Duration::from_millis(1)));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
        let first = kad.bootstrap().unwrap();
        let second = kad.bootstrap().unwrap();
        assert!(kad.cancel_query(&first));
        std::thread::sleep(Duration::from_millis(5));
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        for _ in 0..10 {
            let _ = kad.poll(&mut cx);
        }
        assert_eq!(kad.iter_queries().count(), 1);
        assert!(kad.query(&second).is_some());
        assert!(kad.cancel_query(&second));
        assert!(!kad.cancel_query(&second));
        for _ in 0..10 {
            let _ = kad.poll(&mut cx);
        }
        assert_eq!(
            kad.iter_queries().count(),
            1,
            "automatic bootstrap stayed suppressed after all explicit queries were canceled"
        );
    }

    #[test]
    fn stopping_provider_retires_queries_without_affecting_other_work() {
        let mut kad = bounded_routing_test_dht();
        kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
        let lookup = kad.get_providers(kad::RecordKey::new(&[0_u8]));
        let keep = kad.start_providing(kad::RecordKey::new(&b"keep")).unwrap();
        for index in 0..64 {
            let key = kad::RecordKey::new(&[index]);
            let stopped = kad.start_providing(key.clone()).unwrap();
            kad.stop_providing(&key);
            assert!(
                kad.query(&stopped).is_none(),
                "stopped provider query survived cancellation"
            );
            assert_eq!(kad.iter_queries().count(), 2);
            assert!(kad.query(&lookup).is_some());
            assert!(kad.query(&keep).is_some());
        }
    }

    #[test]
    fn stopping_provider_prevents_undispatched_dials() {
        check_provider_incremental_dial_cancellation(false);
    }

    #[test]
    fn stopping_provider_preserves_query_sharing_a_dispatched_peer() {
        check_provider_incremental_dial_cancellation(true);
    }

    fn check_provider_incremental_dial_cancellation(shared: bool) {
        use libp2p::swarm::{NetworkBehaviour, ToSwarm};
        use std::task::{Context, Poll};
        let local = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_parallelism(NonZeroUsize::new(2).unwrap());
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        for port in 1..=2 {
            kad.add_address(
                &PeerId::random(),
                format!("/memory/{port}").parse().unwrap(),
            );
        }
        let key = kad::RecordKey::new(&b"stopped");
        let query = kad.start_providing(key.clone()).unwrap();
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut dispatched = false;
        for _ in 0..10 {
            if matches!(kad.poll(&mut cx), Poll::Ready(ToSwarm::Dial { .. })) {
                dispatched = true;
                break;
            }
        }
        assert!(dispatched);
        assert_eq!(kad.query(&query).unwrap().stats().num_requests(), 1);
        assert_eq!(kad.pending_rpc_usage().requests, 1);
        let keep = shared.then(|| kad.get_closest_peers(PeerId::random()));
        if let Some(keep) = keep {
            for _ in 0..10 {
                let _ = kad.poll(&mut cx);
            }
            assert_eq!(kad.query(&keep).unwrap().stats().num_requests(), 2);
            assert_eq!(kad.query(&query).unwrap().stats().num_requests(), 2);
            assert_eq!(kad.pending_rpc_usage().requests, 4);
        }
        kad.stop_providing(&key);
        assert!(kad.query(&query).is_none());
        if let Some(keep) = keep {
            assert!(kad.query(&keep).is_some());
            assert_eq!(kad.pending_rpc_usage().requests, 2);
            assert!(matches!(kad.poll(&mut cx), Poll::Pending));
            assert!(kad.cancel_query(&keep));
            assert_eq!(kad.pending_rpc_usage().requests, 0);
        } else {
            assert!(
                matches!(kad.poll(&mut cx), Poll::Pending),
                "stopped query left unsent actions"
            );
            assert_eq!(kad.pending_rpc_usage().requests, 0);
        }
    }

    #[test]
    fn stopping_providers_clears_background_snapshot() {
        use libp2p::{kad::store::RecordStore, swarm::NetworkBehaviour};
        use std::task::{Context, Poll};
        let local = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_provider_publication_interval(Some(Duration::from_millis(1)));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
        let keys = (0..10)
            .map(|index| kad::RecordKey::new(&[index]))
            .collect::<Vec<_>>();
        for key in &keys {
            kad.store_mut()
                .add_provider(kad::ProviderRecord::new(key.clone(), local, Vec::new()))
                .unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let _ = kad.poll(&mut cx);
        assert_eq!(kad.iter_queries().count(), 1);
        for key in &keys {
            kad.stop_providing(key);
        }
        assert_eq!(kad.store_mut().provided().count(), 0);
        for _ in 0..20 {
            assert!(
                matches!(kad.poll(&mut cx), Poll::Pending),
                "stopped snapshot started another action"
            );
            assert_eq!(kad.iter_queries().count(), 0);
        }
    }

    #[test]
    fn kademlia_background_jobs_yield_to_foreground_and_both_resume() {
        exercise_background_query_admission(None);
    }

    #[test]
    fn query_pool_capacity_preserves_background_jobs_until_admission_resumes() {
        exercise_background_query_admission(NonZeroUsize::new(1));
    }

    fn exercise_background_query_admission(capacity: Option<NonZeroUsize>) {
        use libp2p::{
            kad::store::RecordStore,
            swarm::{DialError, FromSwarm, NetworkBehaviour, ToSwarm},
        };
        use std::{
            collections::HashSet,
            task::{Context, Poll},
        };

        let local = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config
            .set_provider_publication_interval(Some(Duration::from_millis(1)))
            .set_replication_interval(Some(Duration::from_millis(1)))
            .set_publication_interval(Some(Duration::from_millis(1)));
        if let Some(capacity) = capacity {
            config.set_query_pool_capacity(capacity);
        }
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
        let foreground = (0..capacity.map_or(2, NonZeroUsize::get))
            .map(|_| kad.get_closest_peers(PeerId::random()))
            .collect::<Vec<_>>();
        for index in 0..10 {
            let key = kad::RecordKey::new(&[index]);
            kad.store_mut()
                .add_provider(kad::ProviderRecord::new(key.clone(), local, Vec::new()))
                .unwrap();
            kad.store_mut().put(kad::Record::new(key, vec![1])).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        for _ in 0..10 {
            let _ = kad.poll(&mut cx);
            assert_eq!(
                kad.iter_queries()
                    .map(|query| query.id())
                    .collect::<HashSet<_>>(),
                foreground.iter().copied().collect()
            );
        }
        for query in &foreground {
            kad.query_mut(query).unwrap().finish();
        }
        let mut seen_queries = foreground.iter().copied().collect::<HashSet<_>>();
        let mut providers = HashSet::new();
        let mut records = HashSet::new();
        for _ in 0..500 {
            let action = kad.poll(&mut cx);
            let mut new_queries = 0;
            for query in kad.iter_queries() {
                new_queries += usize::from(seen_queries.insert(query.id()));
                match query.info() {
                    kad::QueryInfo::AddProvider { key, .. } => {
                        providers.insert(key.clone());
                    }
                    kad::QueryInfo::PutRecord { record, .. } => {
                        records.insert(record.key.clone());
                    }
                    _ => {}
                }
            }
            assert!(
                new_queries <= 1,
                "background jobs exceeded the per-poll allowance"
            );
            assert!(
                kad.query_pool_usage().retained <= capacity.map_or(2, NonZeroUsize::get),
                "background jobs exceeded the shared ceiling"
            );
            if let Poll::Ready(ToSwarm::Dial { opts }) = action {
                kad.on_swarm_event(FromSwarm::DialFailure(
                    libp2p::swarm::behaviour::DialFailure {
                        peer_id: opts.get_peer_id(),
                        connection_id: opts.connection_id(),
                        error: &DialError::NoAddresses,
                    },
                ));
            }
            if providers.len() == 10 && records.len() == 10 {
                break;
            }
        }
        assert_eq!(providers.len(), 10, "provider publication starved");
        assert_eq!(records.len(), 10, "record publication starved");
        assert_eq!(
            kad.query_pool_usage().rejected,
            0,
            "background jobs were consumed before capacity was available"
        );
    }

    #[test]
    fn pending_rpc_budget_is_aggregate_and_reusable_after_cancellation_and_handoff() {
        use libp2p::{
            core::{Endpoint, transport::PortUse},
            swarm::{ConnectionHandler, ConnectionId, NetworkBehaviour, ToSwarm},
        };
        fn drain(kad: &mut kad::Behaviour<kad::store::MemoryStore>) -> usize {
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            let mut failures = 0;
            for _ in 0..128 {
                match kad.poll(&mut cx) {
                    std::task::Poll::Pending => return failures,
                    std::task::Poll::Ready(ToSwarm::GenerateEvent(
                        kad::Event::OutboundQueryProgressed {
                            result: kad::QueryResult::PutRecord(Err(_)),
                            ..
                        },
                    )) => failures += 1,
                    _ => {}
                }
            }
            panic!("pending RPC test did not settle");
        }
        for (count, bytes, admitted) in [(1, 4096, 1), (8, 40, 1), (8, 16, 0)] {
            let local = PeerId::random();
            let mut config = controlled_kademlia_config(StreamProtocol::new(
                crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            ));
            config.set_pending_rpc_limits(kad::PendingRpcLimits::new(
                NonZeroUsize::new(count).unwrap(),
                NonZeroUsize::new(bytes).unwrap(),
            ));
            let mut kad =
                kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
            let queries = (0_u8..2)
                .map(|index| {
                    kad.put_record_to(
                        kad::Record::new(kad::RecordKey::new(&[index]), vec![42; 32]),
                        [PeerId::random()].into_iter(),
                        kad::Quorum::One,
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(drain(&mut kad), 2 - admitted);
            let usage = kad.pending_rpc_usage();
            assert_eq!(usage.requests, admitted);
            assert_eq!(usage.bytes, admitted * 33);
            if count == 1 {
                assert_eq!(usage.count_rejections, 1);
            } else {
                assert_eq!(usage.byte_rejections, (2 - admitted) as u64);
            }
            assert_eq!(
                queries
                    .iter()
                    .filter(|id| kad.query_is_retained(id))
                    .count(),
                admitted
            );
            for query in queries {
                kad.cancel_query(&query);
            }
            assert_eq!(kad.pending_rpc_usage().requests, 0);
            assert_eq!(kad.pending_rpc_usage().bytes, 0);
            let remote = PeerId::random();
            let query = kad.put_record_to(
                kad::Record::new(kad::RecordKey::new(&b"r"), vec![1]),
                [remote].into_iter(),
                kad::Quorum::One,
            );
            assert_eq!(drain(&mut kad), 0);
            assert_eq!(kad.pending_rpc_usage().requests, 1);
            assert_eq!(kad.pending_rpc_usage().bytes, 2);
            let mut handler = kad
                .handle_established_outbound_connection(
                    ConnectionId::new_unchecked(1),
                    remote,
                    &"/memory/1".parse().unwrap(),
                    Endpoint::Dialer,
                    PortUse::Reuse,
                )
                .unwrap();
            assert_eq!(kad.pending_rpc_usage().requests, 0);
            assert_eq!(kad.pending_rpc_usage().bytes, 0);
            assert_eq!(handler.pending_request_usage().requests, 1);
            assert!(kad.query_is_retained(&query));
            let waker = futures::task::noop_waker();
            assert!(
                handler
                    .poll(&mut std::task::Context::from_waker(&waker))
                    .is_ready()
            );
        }
    }

    #[test]
    fn pending_provider_rpc_waits_for_handoff_and_reports_capacity_failure() {
        use libp2p::{
            core::{Endpoint, transport::PortUse},
            swarm::{ConnectionHandler, ConnectionId, FromSwarm, NetworkBehaviour, ToSwarm},
        };
        type HandlerEvent = <<kad::Behaviour<kad::store::MemoryStore> as NetworkBehaviour>::ConnectionHandler as ConnectionHandler>::ToBehaviour;
        for reject in [false, true] {
            let local = PeerId::random();
            let remote = PeerId::random();
            let address: Multiaddr = "/memory/1".parse().unwrap();
            let mut config = controlled_kademlia_config(StreamProtocol::new(
                crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            ));
            config.set_pending_rpc_limits(kad::PendingRpcLimits::new(
                NonZeroUsize::MIN,
                NonZeroUsize::new(if reject { 2 } else { 4096 }).unwrap(),
            ));
            let mut kad =
                kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
            kad.add_address(&remote, address.clone());
            kad.on_swarm_event(FromSwarm::ExternalAddrConfirmed(
                libp2p::swarm::behaviour::ExternalAddrConfirmed { addr: &address },
            ));
            let query = kad.start_providing(kad::RecordKey::new(&b"p")).unwrap();
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            for _ in 0..32 {
                let _ = kad.poll(&mut cx);
                if kad.pending_rpc_usage().requests == 1 {
                    break;
                }
            }
            assert_eq!(kad.pending_rpc_usage().requests, 1);
            // The lookup response arrives before the publication connection exists.
            kad.on_connection_handler_event(
                remote,
                ConnectionId::new_unchecked(1),
                HandlerEvent::FindNodeRes {
                    closer_peers: vec![],
                    query_id: query,
                },
            );
            assert_eq!(kad.pending_rpc_usage().requests, 0);
            assert_eq!(
                kad.query_lifecycle_usage(),
                kad::QueryLifecycleUsage {
                    admitted_phases: 1,
                    requests: 1,
                    successes: 1,
                    ..Default::default()
                }
            );
            let mut result = None;
            for _ in 0..64 {
                if let std::task::Poll::Ready(ToSwarm::GenerateEvent(
                    kad::Event::OutboundQueryProgressed {
                        id,
                        result: kad::QueryResult::StartProviding(event),
                        ..
                    },
                )) = kad.poll(&mut cx)
                {
                    assert_eq!(id, query);
                    result = Some(event);
                }
            }
            if reject {
                assert!(matches!(
                    result,
                    Some(Err(kad::AddProviderError::NoPeersReached { .. }))
                ));
                assert_eq!(kad.pending_rpc_usage().requests, 0);
                assert_eq!(kad.pending_rpc_usage().byte_rejections, 1);
                assert!(!kad.query_is_retained(&query));
                assert_eq!(
                    kad.query_lifecycle_usage(),
                    kad::QueryLifecycleUsage {
                        admitted_phases: 2,
                        retired_phases: 2,
                        completed_phases: 2,
                        requests: 2,
                        successes: 1,
                        failures: 1,
                        ..Default::default()
                    }
                );
                continue;
            }
            assert!(
                result.is_none(),
                "unhanded provider request reported completion"
            );
            assert!(kad.query_is_retained(&query));
            assert_eq!(kad.pending_rpc_usage().requests, 1);
            assert_eq!(
                kad.query_lifecycle_usage(),
                kad::QueryLifecycleUsage {
                    admitted_phases: 2,
                    retired_phases: 1,
                    completed_phases: 1,
                    requests: 2,
                    successes: 1,
                    ..Default::default()
                }
            );
            let handler = kad
                .handle_established_outbound_connection(
                    ConnectionId::new_unchecked(2),
                    remote,
                    &address,
                    Endpoint::Dialer,
                    PortUse::Reuse,
                )
                .unwrap();
            assert_eq!(handler.pending_request_usage().requests, 1);
            assert_eq!(kad.pending_rpc_usage().requests, 0);
            for _ in 0..64 {
                if let std::task::Poll::Ready(ToSwarm::GenerateEvent(
                    kad::Event::OutboundQueryProgressed {
                        id,
                        result: kad::QueryResult::StartProviding(event),
                        ..
                    },
                )) = kad.poll(&mut cx)
                {
                    assert_eq!(id, query);
                    result = Some(event);
                }
            }
            assert!(matches!(result, Some(Ok(_))));
            assert_eq!(
                kad.query_lifecycle_usage(),
                kad::QueryLifecycleUsage {
                    admitted_phases: 2,
                    retired_phases: 2,
                    completed_phases: 2,
                    requests: 2,
                    successes: 2,
                    ..Default::default()
                }
            );
        }
    }

    #[test]
    fn pending_rpc_budget_retires_failed_finished_and_expired_work() {
        use libp2p::{
            core::{Endpoint, transport::PortUse},
            swarm::{ConnectionId, DialError, FromSwarm, NetworkBehaviour},
        };
        for retirement in ["dial_failure", "finished", "expired"] {
            let local = PeerId::random();
            let remote = PeerId::random();
            let mut config = controlled_kademlia_config(StreamProtocol::new(
                crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            ));
            config.set_query_timeout(Duration::from_millis(50));
            let mut kad =
                kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
            let query = kad.put_record_to(
                kad::Record::new(kad::RecordKey::new(&b"r"), vec![1]),
                [remote].into_iter(),
                kad::Quorum::One,
            );
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            let _ = kad.poll(&mut cx);
            assert_eq!(kad.pending_rpc_usage().requests, 1);
            if retirement == "dial_failure" {
                kad.on_swarm_event(FromSwarm::DialFailure(libp2p::swarm::DialFailure {
                    peer_id: Some(remote),
                    connection_id: ConnectionId::new_unchecked(1),
                    error: &DialError::NoAddresses,
                }));
            } else {
                if retirement == "finished" {
                    kad.query_mut(&query).unwrap().finish();
                } else {
                    std::thread::sleep(Duration::from_millis(75));
                }
                assert_eq!(kad.pending_rpc_usage().requests, 1);
                let handler = kad
                    .handle_established_outbound_connection(
                        ConnectionId::new_unchecked(1),
                        remote,
                        &"/memory/1".parse().unwrap(),
                        Endpoint::Dialer,
                        PortUse::Reuse,
                    )
                    .unwrap();
                assert_eq!(
                    handler.pending_request_usage().requests,
                    0,
                    "stale RPC handed off"
                );
            }
            assert_eq!(kad.pending_rpc_usage().requests, 0);
            assert_eq!(kad.pending_rpc_usage().bytes, 0);
            for _ in 0..32 {
                if kad.poll(&mut cx).is_pending() {
                    break;
                }
            }
            assert!(!kad.query_is_retained(&query));
        }
    }

    #[tokio::test]
    async fn pending_query_deadline_wakes_without_network_events() {
        use libp2p::swarm::{NetworkBehaviour, ToSwarm};
        let local = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_query_timeout(Duration::from_millis(50));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        let query = kad.put_record_to(
            kad::Record::new(vec![1], vec![1]),
            [PeerId::random()].into_iter(),
            kad::Quorum::One,
        );
        let first = futures::future::poll_fn(|cx| kad.poll(cx)).await;
        assert!(matches!(first, ToSwarm::Dial { .. }));
        assert_eq!(kad.pending_rpc_usage().requests, 1);
        // Intentionally never deliver the dial result or a network event.
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            futures::future::poll_fn(|cx| kad.poll(cx)),
        )
        .await
        .expect("idle query must wake itself at its deadline");
        assert!(
            matches!(result, ToSwarm::GenerateEvent(kad::Event::OutboundQueryProgressed {
            id, result: kad::QueryResult::PutRecord(Err(kad::PutRecordError::Timeout { .. })), ..
        }) if id == query)
        );
        assert_eq!(kad.query_pool_usage().retained, 0);
        assert_eq!(kad.pending_rpc_usage().requests, 0);
        assert_eq!(kad.pending_rpc_usage().bytes, 0);
        let next = kad
            .try_start_query(|kad| kad.get_closest_peers(PeerId::random()))
            .unwrap();
        assert!(kad.cancel_query(&next));
    }

    #[test]
    fn query_pool_capacity_counts_finished_entries_and_rejects_without_side_effects() {
        use libp2p::kad::store::RecordStore;
        let local = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_query_pool_capacity(NonZeroUsize::new(2).unwrap());
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
        let first = kad
            .try_start_query(|kad| kad.get_record(kad::RecordKey::new(&b"first")))
            .unwrap();
        let keep = kad
            .try_start_query(|kad| kad.get_closest_peers(PeerId::random()))
            .unwrap();
        let admitted = kad::QueryLifecycleUsage {
            admitted_phases: 2,
            ..Default::default()
        };
        assert_eq!(kad.query_lifecycle_usage(), admitted);
        let retained = kad.query_resource_snapshot();
        assert_eq!(retained.bounded_queries, 2);
        kad.query_mut(&first).unwrap().finish();
        assert!(kad.query(&first).is_none());
        assert!(kad.query_is_retained(&first));
        assert_eq!(kad.query_pool_usage().retained, 2);
        assert_eq!(kad.query_resource_snapshot(), retained);
        assert_eq!(kad.query_lifecycle_usage(), admitted);
        let attempted = std::cell::Cell::new(false);
        let rejected = kad.try_start_query(|_| attempted.set(true));
        assert_eq!(rejected, Err(kad::QueryCapacityError));
        assert!(!attempted.get());
        let key = kad::RecordKey::new(&b"local");
        kad.store_mut()
            .put(kad::Record::new(key.clone(), vec![1; 32]))
            .unwrap();
        kad.store_mut()
            .add_provider(kad::ProviderRecord::new(key.clone(), local, vec![]))
            .unwrap();
        let mut denied = HashSet::new();
        for _ in 0..100 {
            denied.insert(kad.get_record(key.clone()));
            denied.insert(kad.get_providers(key.clone()));
            assert_eq!(kad.query_pool_usage().retained, 2);
        }
        denied.insert(kad.put_record_to(
            kad::Record::new(key.clone(), vec![1; 32]),
            [PeerId::random()].into_iter(),
            kad::Quorum::One,
        ));
        assert_eq!(kad.query_pool_usage().retained, 2);
        assert!(denied.iter().all(|id| !kad.query_is_retained(id)));
        assert_eq!(kad.query_lifecycle_usage(), admitted);
        assert!(kad.cancel_query(&first));
        assert!(!kad.cancel_query(&first));
        assert_eq!(
            kad.query_lifecycle_usage(),
            kad::QueryLifecycleUsage {
                retired_phases: 1,
                cancelled_phases: 1,
                ..admitted
            }
        );
        assert_eq!(kad.query_pool_usage().retained, 1);
        let admitted = kad.try_start_query(|kad| kad.get_record(key)).unwrap();
        assert_eq!(kad.query_lifecycle_usage().admitted_phases, 3);
        assert!(kad.query_is_retained(&admitted));
        assert!(kad.query_is_retained(&keep));
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        for _ in 0..1000 {
            let action = kad.poll(&mut cx);
            if let std::task::Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(
                kad::Event::OutboundQueryProgressed { id, .. },
            )) = &action
            {
                assert!(
                    !denied.contains(id),
                    "rejected local results entered the event queue"
                );
            }
            if action.is_pending() {
                break;
            }
        }
        assert_eq!(kad.query_pool_usage().retained, 2);
        assert!(kad.query_pool_usage().rejected >= 202);
    }

    #[test]
    fn query_pool_capacity_is_reused_across_bootstrap_phases() {
        let peer_for = |seed| {
            libp2p::identity::Keypair::ed25519_from_bytes([seed; 32])
                .unwrap()
                .public()
                .to_peer_id()
        };
        let local = peer_for(0);
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_query_pool_capacity(NonZeroUsize::new(2).unwrap());
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        let near = (1..=255)
            .map(peer_for)
            .min_by_key(|peer| kad.kbucket(*peer).unwrap().range().0)
            .unwrap();
        kad.add_address(&near, "/memory/1".parse().unwrap());
        let keep = kad.get_record(kad::RecordKey::new(&b"keep"));
        let bootstrap = kad
            .try_start_query(kad::Behaviour::bootstrap)
            .unwrap()
            .unwrap();
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        let mut phases = 0;
        for _ in 0..1024 {
            if let Some(mut query) = kad.query_mut(&bootstrap) {
                query.finish();
            }
            if let std::task::Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(
                kad::Event::OutboundQueryProgressed { id, .. },
            )) = kad.poll(&mut cx)
                && id == bootstrap
            {
                phases += 1;
            }
            assert!(kad.query_pool_usage().retained <= 2);
            assert!(kad.query_is_retained(&keep));
            let lifecycle = kad.query_lifecycle_usage();
            assert_eq!(lifecycle.retired_phases, phases);
            assert_eq!(lifecycle.completed_phases, phases);
            assert_eq!(lifecycle.timed_out_phases, 0);
            assert_eq!(lifecycle.cancelled_phases, 0);
            assert_eq!(
                lifecycle.admitted_phases - lifecycle.retired_phases,
                kad.query_pool_usage().retained as u64
            );
            if !kad.query_is_retained(&bootstrap) {
                break;
            }
        }
        assert!(phases > 1, "test did not exercise a bootstrap continuation");
        assert!(!kad.query_is_retained(&bootstrap));
        assert_eq!(kad.query_pool_usage().retained, 1);
        assert_eq!(kad.query_pool_usage().rejected, 0);
        for provider in [false, true] {
            let before = kad.query_lifecycle_usage();
            let key = kad::RecordKey::new(&b"publish");
            let publish = kad
                .try_start_query(|kad| {
                    if provider {
                        kad.start_providing(key)
                    } else {
                        kad.put_record(kad::Record::new(key, vec![1]), kad::Quorum::One)
                    }
                })
                .unwrap()
                .unwrap();
            let mut completed = false;
            for _ in 0..1024 {
                if let Some(mut query) = kad.query_mut(&publish) {
                    query.finish();
                }
                if let std::task::Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(
                    kad::Event::OutboundQueryProgressed { id, step, .. },
                )) = kad.poll(&mut cx)
                    && id == publish
                    && step.last
                {
                    completed = true;
                }
                assert!(kad.query_pool_usage().retained <= 2);
                assert!(kad.query_is_retained(&keep));
                if !kad.query_is_retained(&publish) {
                    break;
                }
            }
            assert!(completed, "fixed publication phase failed to retire");
            assert_eq!(kad.query_pool_usage().retained, 1);
            assert_eq!(kad.query_pool_usage().rejected, 0);
            let after = kad.query_lifecycle_usage();
            assert_eq!(after.admitted_phases - before.admitted_phases, 2);
            assert_eq!(after.retired_phases - before.retired_phases, 2);
            assert_eq!(after.completed_phases - before.completed_phases, 2);
            assert_eq!(after.timed_out_phases, 0);
            assert_eq!(after.cancelled_phases, 0);
        }
        assert!(
            kad.try_start_query(|kad| kad.get_record(kad::RecordKey::new(&b"after")))
                .is_ok()
        );
    }

    #[test]
    fn query_pool_capacity_rejected_bootstrap_does_not_leak_suppression() {
        let local = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config
            .set_query_pool_capacity(NonZeroUsize::MIN)
            .set_periodic_bootstrap_interval(Some(Duration::from_millis(1)));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
        let first = kad
            .try_start_query(kad::Behaviour::bootstrap)
            .unwrap()
            .unwrap();
        let denied = kad.bootstrap().unwrap();
        assert!(!kad.query_is_retained(&denied));
        assert!(kad.cancel_query(&first));
        std::thread::sleep(Duration::from_millis(5));
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        for _ in 0..10 {
            let _ = kad.poll(&mut cx);
        }
        assert_eq!(
            kad.query_pool_usage().retained,
            1,
            "rejected bootstrap left automatic bootstrap suppressed"
        );
        assert!(
            kad.iter_queries()
                .all(|query| matches!(query.info(), kad::QueryInfo::Bootstrap { .. }))
        );
    }

    #[test]
    fn aggregate_routing_entries_retire_only_after_snapshots_release() {
        let local = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_routing_limits(kad::RoutingLimits::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
        ));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        let peers = [PeerId::random(), PeerId::random(), PeerId::random()];
        let address: Multiaddr = "/memory/1".parse().unwrap();
        for peer in &peers[..2] {
            assert_eq!(
                kad.add_address(peer, address.clone()),
                kad::RoutingUpdate::Success
            );
        }
        let snapshots = drain_routing_snapshots(&mut kad);
        assert_eq!(snapshots.len(), 2);
        assert_eq!(kad.routing_resource_usage().entries, 2);
        drop(kad.remove_peer(&peers[0]).unwrap());
        assert_eq!(
            kad.routing_resource_usage().entries,
            2,
            "snapshot released too early"
        );
        assert_eq!(
            kad.add_address(&peers[2], address.clone()),
            kad::RoutingUpdate::Failed
        );
        drop(snapshots);
        assert_eq!(kad.routing_resource_usage().entries, 1);
        assert_eq!(
            kad.add_address(&peers[2], address),
            kad::RoutingUpdate::Success
        );
        for peer in &peers[1..] {
            drop(kad.remove_peer(peer).unwrap());
        }
        drop(drain_routing_snapshots(&mut kad));
        let usage = kad.routing_resource_usage();
        assert_eq!(usage.entries, 0);
        assert_eq!(usage.address_bytes, 0);
        assert!(usage.entry_rejections > 0);
    }

    fn drain_routing_snapshots(
        kad: &mut kad::Behaviour<kad::store::MemoryStore>,
    ) -> Vec<kad::Addresses> {
        let waker = futures::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        let mut snapshots = Vec::new();
        for _ in 0..1000 {
            match kad.poll(&mut cx) {
                std::task::Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(
                    kad::Event::RoutingUpdated { addresses, .. },
                )) => snapshots.push(addresses),
                std::task::Poll::Pending => return snapshots,
                _ => {}
            }
        }
        panic!("routing event queue did not drain");
    }

    #[test]
    fn aggregate_routing_snapshot_generations_cannot_bypass_entry_limit() {
        let local = PeerId::random();
        let peer = PeerId::random();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_routing_limits(kad::RoutingLimits::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
        ));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        let address = |port| format!("/memory/{port}").parse::<Multiaddr>().unwrap();
        for port in 1..=2 {
            assert_eq!(
                kad.add_address(&peer, address(port)),
                kad::RoutingUpdate::Success
            );
        }
        assert_eq!(kad.routing_resource_usage().entries, 2);
        assert_eq!(
            kad.add_address(&peer, address(3)),
            kad::RoutingUpdate::Failed
        );
        let before = kad.routing_resource_usage().address_bytes;
        assert!(kad.remove_address(&peer, &address(1)).is_none());
        assert_eq!(
            kad.routing_resource_usage().address_bytes,
            before,
            "old snapshots still own the removed buffer"
        );
        drop(drain_routing_snapshots(&mut kad));
        assert_eq!(kad.routing_resource_usage().entries, 1);
        assert!(kad.routing_resource_usage().address_bytes < before);
        assert_eq!(
            kad.add_address(&peer, address(3)),
            kad::RoutingUpdate::Success
        );
        drop(kad.remove_peer(&peer).unwrap());
        drop(drain_routing_snapshots(&mut kad));
        assert_eq!(kad.routing_resource_usage().entries, 0);
        assert_eq!(kad.routing_resource_usage().address_bytes, 0);
    }

    #[test]
    fn aggregate_routing_bytes_preserve_seeds_and_fresh_alternatives() {
        let local = PeerId::random();
        let peer = PeerId::random();
        let seed: Multiaddr = "/ip4/11.1.1.1/tcp/4001".parse().unwrap();
        let lan: Multiaddr = "/ip4/192.168.1.2/tcp/4001".parse().unwrap();
        let relay: Multiaddr = format!(
            "/ip4/11.1.1.2/tcp/4001/p2p/{}/p2p-circuit",
            PeerId::random()
        )
        .parse()
        .unwrap();
        let wan = |port| {
            format!("/ip4/11.1.1.3/tcp/{port}")
                .parse::<Multiaddr>()
                .unwrap()
        };
        let limit = [seed.clone(), lan.clone(), relay.clone(), wan(1)]
            .into_iter()
            .map(|address| address.with_p2p(peer).unwrap().len())
            .sum();
        let mut config = controlled_kademlia_config(StreamProtocol::new(
            crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        ));
        config.set_routing_limits(kad::RoutingLimits::new(
            NonZeroUsize::new(16).unwrap(),
            NonZeroUsize::new(limit).unwrap(),
        ));
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        assert_eq!(
            kad.add_protected_address(&peer, seed.clone()),
            kad::RoutingUpdate::Success
        );
        for address in [lan.clone(), relay.clone(), wan(1)] {
            assert_eq!(kad.add_address(&peer, address), kad::RoutingUpdate::Success);
        }
        assert_eq!(kad.routing_resource_usage().address_bytes, limit);
        assert_eq!(
            kad.add_address(&peer, wan(2)),
            kad::RoutingUpdate::Failed,
            "snapshots still retain the old buffers"
        );
        drop(drain_routing_snapshots(&mut kad));
        for port in 2..=100 {
            assert_eq!(
                kad.add_address(&peer, wan(port)),
                kad::RoutingUpdate::Success
            );
            assert_eq!(kad.routing_resource_usage().address_bytes, limit);
            drop(drain_routing_snapshots(&mut kad));
        }
        let old = libp2p::core::ConnectedPoint::Dialer {
            address: wan(100).with_p2p(peer).unwrap(),
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        let new = libp2p::core::ConnectedPoint::Dialer {
            address: wan(101).with_p2p(peer).unwrap(),
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        kad.on_swarm_event(libp2p::swarm::FromSwarm::AddressChange(
            libp2p::swarm::behaviour::AddressChange {
                peer_id: peer,
                connection_id: libp2p::swarm::ConnectionId::new_unchecked(1),
                old: &old,
                new: &new,
            },
        ));
        assert_eq!(
            kad.add_address(
                &peer,
                Multiaddr::empty().with(Protocol::Dns("a".repeat(limit).into()))
            ),
            kad::RoutingUpdate::Failed,
            "an impossible admission must leave existing alternatives intact"
        );
        let retained = kad.remove_peer(&peer).unwrap().node.value;
        assert_eq!(retained.len(), 4);
        for address in [seed, lan, relay, wan(101)] {
            assert!(
                retained
                    .iter()
                    .any(|a| a == &address.clone().with_p2p(peer).unwrap())
            );
        }
        assert_eq!(kad.routing_resource_usage().address_bytes, limit);
        drop(retained);
        assert_eq!(kad.routing_resource_usage().entries, 0);
        assert_eq!(kad.routing_resource_usage().address_bytes, 0);
    }

    #[test]
    fn aggregate_routing_counts_pending_and_deferred_eviction_storage() {
        let peer_for = |seed| {
            libp2p::identity::Keypair::ed25519_from_bytes([seed; 32])
                .unwrap()
                .public()
                .to_peer_id()
        };
        for address_slots in [1, 2] {
            let local = peer_for(0);
            let first = peer_for(1);
            let address: Multiaddr = "/memory/1".parse().unwrap();
            let bytes = address.clone().with_p2p(first).unwrap().len();
            let mut config = controlled_kademlia_config(StreamProtocol::new(
                crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            ));
            config
                .set_kbucket_size(NonZeroUsize::MIN)
                .set_kbucket_pending_timeout(Duration::from_millis(100))
                .set_routing_limits(kad::RoutingLimits::new(
                    NonZeroUsize::new(2).unwrap(),
                    NonZeroUsize::new(bytes * address_slots).unwrap(),
                ));
            let mut kad =
                kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
            let range = kad.kbucket(first).unwrap().range();
            let candidate = (2..=255)
                .map(peer_for)
                .find(|peer| kad.kbucket(*peer).unwrap().range() == range)
                .unwrap();
            let other = (2..=255)
                .map(peer_for)
                .find(|peer| kad.kbucket(*peer).unwrap().range() != range)
                .unwrap();
            assert_eq!(
                kad.add_address(&first, address.clone()),
                kad::RoutingUpdate::Success
            );
            let endpoint = libp2p::core::ConnectedPoint::Dialer {
                address: address.clone(),
                role_override: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::Reuse,
            };
            kad.on_swarm_event(libp2p::swarm::FromSwarm::ConnectionEstablished(
                libp2p::swarm::behaviour::ConnectionEstablished {
                    peer_id: candidate,
                    connection_id: libp2p::swarm::ConnectionId::new_unchecked(1),
                    endpoint: &endpoint,
                    failed_addresses: &[],
                    other_established: 0,
                },
            ));
            assert_eq!(
                kad.add_protected_address(&candidate, address.clone()),
                if address_slots == 2 {
                    kad::RoutingUpdate::Pending
                } else {
                    kad::RoutingUpdate::Failed
                }
            );
            let usage = kad.routing_resource_usage();
            assert_eq!(usage.entries, address_slots);
            assert_eq!(usage.address_bytes, bytes * address_slots);
            assert_eq!(
                kad.kbucket(first).unwrap().has_pending(),
                address_slots == 2
            );
            assert_eq!(
                kad.add_address(&other, address.clone()),
                kad::RoutingUpdate::Failed
            );
            let initial_snapshots = drain_routing_snapshots(&mut kad);
            assert_eq!(initial_snapshots.len(), 1);
            drop(initial_snapshots);
            std::thread::sleep(Duration::from_millis(150));
            let present = kad
                .kbucket(first)
                .unwrap()
                .iter()
                .map(|entry| *entry.node.key.preimage())
                .collect::<Vec<_>>();
            assert_eq!(
                present,
                vec![if address_slots == 2 { candidate } else { first }]
            );
            // Lazy promotion still owns the evicted node until behaviour polling.
            assert_eq!(kad.routing_resource_usage().entries, address_slots);
            assert_eq!(
                kad.routing_resource_usage().address_bytes,
                bytes * address_slots
            );
            drop(drain_routing_snapshots(&mut kad));
            assert_eq!(kad.routing_resource_usage().entries, 1);
            assert_eq!(kad.routing_resource_usage().address_bytes, bytes);
            let removed = kad.remove_peer(&present[0]).unwrap();
            assert_eq!(kad.routing_resource_usage().entries, 1);
            drop(removed);
            assert_eq!(kad.routing_resource_usage().entries, 0);
            assert_eq!(kad.routing_resource_usage().address_bytes, 0);
            assert_eq!(
                kad.add_address(&other, address),
                kad::RoutingUpdate::Success
            );
        }
    }

    #[test]
    fn aggregate_routing_raw_notifications_share_admission_and_retire_on_dispatch() {
        let peer_for = |seed| {
            libp2p::identity::Keypair::ed25519_from_bytes([seed; 32])
                .unwrap()
                .public()
                .to_peer_id()
        };
        for slots in [2, 3] {
            let local = peer_for(0);
            let first = peer_for(1);
            let address: Multiaddr = "/memory/1".parse().unwrap();
            let bytes = address.clone().with_p2p(first).unwrap().len();
            let mut config = controlled_kademlia_config(StreamProtocol::new(
                crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            ));
            config
                .set_kbucket_size(NonZeroUsize::MIN)
                .set_kbucket_pending_timeout(Duration::from_millis(100))
                .set_routing_limits(kad::RoutingLimits::new(
                    NonZeroUsize::new(slots).unwrap(),
                    NonZeroUsize::new(bytes * slots).unwrap(),
                ));
            let mut kad =
                kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
            let range = kad.kbucket(first).unwrap().range();
            let candidate = (2..=255)
                .map(peer_for)
                .find(|peer| kad.kbucket(*peer).unwrap().range() == range)
                .unwrap();
            assert_eq!(
                kad.add_address(&first, address.clone()),
                kad::RoutingUpdate::Success
            );
            let endpoint = libp2p::core::ConnectedPoint::Dialer {
                address: address.clone(),
                role_override: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::Reuse,
            };
            kad.on_swarm_event(libp2p::swarm::FromSwarm::ConnectionEstablished(
                libp2p::swarm::behaviour::ConnectionEstablished {
                    peer_id: candidate,
                    connection_id: libp2p::swarm::ConnectionId::new_unchecked(1),
                    endpoint: &endpoint,
                    failed_addresses: &[],
                    other_established: 0,
                },
            ));
            assert_eq!(
                kad.add_address(&candidate, address),
                kad::RoutingUpdate::Pending
            );
            drop(drain_routing_snapshots(&mut kad));
            std::thread::sleep(Duration::from_millis(150));
            assert_eq!(kad.kbucket(first).unwrap().num_entries(), 1);
            assert_eq!(kad.routing_resource_usage().entries, 2);
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            let routing = kad.poll(&mut cx);
            assert!(
                matches!(&routing, std::task::Poll::Ready(libp2p::swarm::ToSwarm::GenerateEvent(kad::Event::RoutingUpdated { peer, .. })) if *peer == candidate)
            );
            assert_eq!(kad.routing_resource_usage().entries, slots - 1);
            drop(kad.remove_peer(&candidate).unwrap());
            drop(routing);
            assert_eq!(kad.routing_resource_usage().entries, slots - 2);
            let notification = kad.poll(&mut cx);
            if slots == 3 {
                assert!(
                    matches!(notification, std::task::Poll::Ready(libp2p::swarm::ToSwarm::NewExternalAddrOfPeer { peer_id, .. }) if peer_id == candidate)
                );
            } else {
                assert!(notification.is_pending());
                assert!(kad.routing_resource_usage().entry_rejections > 0);
            }
            assert_eq!(kad.routing_resource_usage().entries, 0);
            assert_eq!(kad.routing_resource_usage().address_bytes, 0);
        }
    }

    #[test]
    fn routing_pending_replacement_preserves_protected_peers() {
        let peer_for = |seed| {
            libp2p::identity::Keypair::ed25519_from_bytes([seed; 32])
                .unwrap()
                .public()
                .to_peer_id()
        };
        for (protect_first, protect_second, protect_after) in [
            (true, false, false),
            (true, true, false),
            (true, false, true),
            (false, false, false),
            (false, false, true),
        ] {
            let local = peer_for(0);
            let mut config = controlled_kademlia_config(StreamProtocol::new(
                crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            ));
            config
                .set_kbucket_size(NonZeroUsize::new(2).unwrap())
                .set_kbucket_pending_timeout(Duration::from_millis(100));
            let mut kad =
                kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
            let first = peer_for(1);
            let range = kad.kbucket(first).unwrap().range();
            let peers = (2..=255)
                .map(peer_for)
                .filter(|peer| kad.kbucket(*peer).unwrap().range() == range)
                .take(2)
                .collect::<Vec<_>>();
            assert_eq!(peers.len(), 2);
            let (second, candidate) = (peers[0], peers[1]);
            let address: Multiaddr = "/memory/1".parse().unwrap();
            assert_eq!(
                if protect_first {
                    kad.add_protected_address(&first, address.clone())
                } else {
                    kad.add_address(&first, address.clone())
                },
                kad::RoutingUpdate::Success
            );
            assert_eq!(
                if protect_second {
                    kad.add_protected_address(&second, address.clone())
                } else {
                    kad.add_address(&second, address.clone())
                },
                kad::RoutingUpdate::Success
            );
            let endpoint = libp2p::core::ConnectedPoint::Dialer {
                address: address.clone(),
                role_override: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::Reuse,
            };
            kad.on_swarm_event(libp2p::swarm::FromSwarm::ConnectionEstablished(
                libp2p::swarm::behaviour::ConnectionEstablished {
                    peer_id: candidate,
                    connection_id: libp2p::swarm::ConnectionId::new_unchecked(1),
                    endpoint: &endpoint,
                    failed_addresses: &[],
                    other_established: 0,
                },
            ));
            assert_eq!(
                kad.add_address(&candidate, address.clone()),
                if protect_second {
                    kad::RoutingUpdate::Failed
                } else {
                    kad::RoutingUpdate::Pending
                }
            );
            let victim = if protect_first { second } else { first };
            if protect_after {
                assert_eq!(
                    kad.add_protected_address(&victim, address.clone()),
                    kad::RoutingUpdate::Success
                );
            }
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            let mut probes = Vec::new();
            for _ in 0..100 {
                if let std::task::Poll::Ready(libp2p::swarm::ToSwarm::Dial { opts }) =
                    kad.poll(&mut cx)
                {
                    probes.push(opts.get_peer_id().unwrap());
                }
            }
            assert_eq!(probes, if protect_second { vec![] } else { vec![victim] });
            std::thread::sleep(Duration::from_millis(150));
            let retained = kad
                .kbucket(first)
                .unwrap()
                .iter()
                .map(|entry| *entry.node.key.preimage())
                .collect::<HashSet<_>>();
            if protect_first {
                assert!(retained.contains(&first), "protected seed was evicted");
            }
            assert_eq!(retained.len(), 2, "protection bypassed bucket capacity");
            let mut expected = HashSet::from([first, second]);
            if !protect_second && !protect_after {
                expected.remove(&victim);
                expected.insert(candidate);
            }
            assert_eq!(
                retained, expected,
                "replacement changed the selected victim"
            );
            if protect_second {
                assert!(kad.remove_peer(&first).is_some());
                assert_eq!(
                    kad.add_address(&candidate, address),
                    kad::RoutingUpdate::Success,
                    "explicit removal did not restore admission"
                );
            }
        }
    }

    #[test]
    fn routing_address_limits_preserve_seeds_lan_and_relay_under_churn() {
        let mut kad = bounded_routing_test_dht();
        let peer = PeerId::random();
        let seed: Multiaddr = "/ip4/11.1.1.1/tcp/4001".parse().unwrap();
        let lan: Multiaddr = "/ip4/192.168.1.2/tcp/4001".parse().unwrap();
        let relay: Multiaddr = format!(
            "/ip4/11.1.1.2/tcp/4001/p2p/{}/p2p-circuit",
            PeerId::random()
        )
        .parse()
        .unwrap();
        kad.add_protected_address(&peer, seed.clone());
        kad.add_address(&peer, lan.clone());
        kad.add_address(&peer, relay.clone());
        for port in 1..=100 {
            assert_eq!(
                kad.add_address(&peer, format!("/ip4/11.1.1.3/tcp/{port}").parse().unwrap()),
                kad::RoutingUpdate::Success
            );
        }
        let mut addresses = kad.remove_peer(&peer).unwrap().node.value;
        assert_eq!(addresses.len(), 4);
        for address in [&seed, &lan, &relay] {
            let address = address.clone().with_p2p(peer).unwrap();
            assert!(addresses.iter().any(|a| a == &address));
        }
        let seed = seed.with_p2p(peer).unwrap();
        let moved: Multiaddr = format!("/ip4/11.1.1.4/tcp/4001/p2p/{peer}")
            .parse()
            .unwrap();
        assert!(addresses.replace(&seed, &moved));
        assert!(addresses.iter().any(|a| a == &seed));
        assert!(addresses.iter().any(|a| a == &moved));
        assert_eq!(addresses.len(), 4);
        let oversized = Multiaddr::empty().with(Protocol::Dns("a".repeat(2048).into()));
        assert!(!addresses.insert(oversized.clone()));
        assert!(!addresses.replace(&moved, &oversized));
        assert!(addresses.iter().any(|a| a == &moved));
        assert_eq!(
            kad.add_address(&peer, oversized),
            kad::RoutingUpdate::Failed
        );
        assert!(kad.remove_peer(&peer).is_none());
        let lan = lan.with_p2p(peer).unwrap();
        assert!(addresses.replace(&moved, &lan));
        assert_eq!(addresses.len(), 3, "replacement must not retain duplicates");
    }

    #[test]
    fn protected_routing_addresses_cannot_exceed_or_bypass_limits() {
        let mut kad = bounded_routing_test_dht();
        let peer = PeerId::random();
        for port in 1..=4 {
            assert_eq!(
                kad.add_protected_address(&peer, format!("/memory/{port}").parse().unwrap()),
                kad::RoutingUpdate::Success
            );
        }
        assert_eq!(
            kad.add_address(&peer, "/memory/5".parse().unwrap()),
            kad::RoutingUpdate::Failed
        );
        assert_eq!(
            kad.add_protected_address(&peer, "/memory/6".parse().unwrap()),
            kad::RoutingUpdate::Failed
        );
        assert_eq!(
            kad.add_protected_address(&peer, "/memory/1".parse().unwrap()),
            kad::RoutingUpdate::Success
        );
        kad.remove_address(&peer, &"/memory/1".parse().unwrap());
        assert_eq!(
            kad.add_address(&peer, "/memory/5".parse().unwrap()),
            kad::RoutingUpdate::Success
        );
        let addresses = kad.remove_peer(&peer).unwrap().node.value;
        assert_eq!(addresses.len(), 4);
        assert!(!addresses.iter().any(|a| {
            a == &"/memory/1"
                .parse::<Multiaddr>()
                .unwrap()
                .with_p2p(peer)
                .unwrap()
        }));
    }

    #[test]
    fn routing_churn_preserves_singleton_alternatives_and_fresh_migrations() {
        let mut kad = bounded_routing_test_dht();
        let peer = PeerId::random();
        let lan = |port| {
            format!("/ip4/192.168.1.2/tcp/{port}")
                .parse::<Multiaddr>()
                .unwrap()
                .with_p2p(peer)
                .unwrap()
        };
        let relay = format!(
            "/ip4/11.1.1.2/tcp/4001/p2p/{}/p2p-circuit/p2p/{peer}",
            PeerId::random()
        )
        .parse::<Multiaddr>()
        .unwrap();
        kad.add_address(&peer, relay.clone());
        for port in 1..=3 {
            kad.add_address(&peer, lan(port));
        }
        let mut addresses = kad.remove_peer(&peer).unwrap().node.value;
        let wan = "/ip4/11.1.1.3/tcp/4001"
            .parse::<Multiaddr>()
            .unwrap()
            .with_p2p(peer)
            .unwrap();
        assert!(addresses.insert(wan));
        assert!(
            addresses.iter().any(|a| a == &relay),
            "a new category must not evict a singleton when alternatives exist"
        );
        assert!(addresses.replace(&lan(2), &lan(4)));
        assert!(addresses.insert(lan(5)));
        assert!(
            addresses.iter().any(|a| a == &lan(4)),
            "migration refreshes recency"
        );
        assert_eq!(addresses.len(), 4);
    }

    #[test]
    fn pending_routing_entries_enforce_address_limits_without_events() {
        let mut kad = bounded_routing_test_dht();
        let peer_for = |seed| {
            libp2p::identity::Keypair::ed25519_from_bytes([seed; 32])
                .unwrap()
                .public()
                .to_peer_id()
        };
        for seed in 1..=128 {
            kad.add_address(&peer_for(seed), "/memory/1".parse().unwrap());
        }
        let endpoint_for = |address| libp2p::core::ConnectedPoint::Dialer {
            address,
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        let endpoint = endpoint_for("/memory/1".parse().unwrap());
        let peer = (129..=255)
            .find_map(|seed| {
                let peer = peer_for(seed);
                kad.on_swarm_event(libp2p::swarm::FromSwarm::ConnectionEstablished(
                    libp2p::swarm::behaviour::ConnectionEstablished {
                        peer_id: peer,
                        connection_id: libp2p::swarm::ConnectionId::new_unchecked(usize::from(
                            seed,
                        )),
                        endpoint: &endpoint,
                        failed_addresses: &[],
                        other_established: 0,
                    },
                ));
                (kad.add_address(&peer, "/memory/1".parse().unwrap())
                    == kad::RoutingUpdate::Pending)
                    .then_some(peer)
            })
            .expect("connected identity creates a pending entry in a full bucket");
        let seed: Multiaddr = "/memory/1".parse().unwrap();
        assert_eq!(
            kad.add_protected_address(&peer, seed.clone()),
            kad::RoutingUpdate::Pending
        );
        for port in 2..=100 {
            assert_eq!(
                kad.add_address(&peer, format!("/memory/{port}").parse().unwrap()),
                kad::RoutingUpdate::Pending
            );
        }
        let old_address = "/memory/100"
            .parse::<Multiaddr>()
            .unwrap()
            .with_p2p(peer)
            .unwrap();
        let old_endpoint = endpoint_for(old_address.clone());
        let oversized_endpoint = endpoint_for(
            Multiaddr::empty()
                .with(Protocol::Dns("a".repeat(2048).into()))
                .with_p2p(peer)
                .unwrap(),
        );
        let changed_endpoint = endpoint_for(
            "/memory/101"
                .parse::<Multiaddr>()
                .unwrap()
                .with_p2p(peer)
                .unwrap(),
        );
        for new in [&oversized_endpoint, &changed_endpoint] {
            kad.on_swarm_event(libp2p::swarm::FromSwarm::AddressChange(
                libp2p::swarm::behaviour::AddressChange {
                    peer_id: peer,
                    connection_id: libp2p::swarm::ConnectionId::new_unchecked(999),
                    old: &old_endpoint,
                    new,
                },
            ));
        }
        let addresses = kad.remove_peer(&peer).unwrap().node.value;
        assert_eq!(addresses.len(), 4);
        assert!(
            addresses
                .iter()
                .any(|a| a == &seed.clone().with_p2p(peer).unwrap())
        );
        assert!(addresses.iter().any(|a| {
            a == &"/memory/101"
                .parse::<Multiaddr>()
                .unwrap()
                .with_p2p(peer)
                .unwrap()
        }));
        assert!(!addresses.iter().any(|a| a == &old_address));
        assert!(addresses.iter().all(|a| a.len() <= 2048));
    }

    #[tokio::test]
    #[ignore = "opt-in loopback regression for internal Kademlia address retention"]
    async fn internal_kademlia_connection_addresses_remain_bounded() {
        for separate in [false, true] {
            Box::pin(exercise_internal_kademlia_connection_address_retention(
                separate,
            ))
            .await;
        }
    }

    pub(super) fn retention_diagnostic_config(separate: bool) -> HostConfig {
        HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "retention-diagnostic".to_owned(),
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
                mdns: false,
                autonat: false,
                dcutr: false,
                kademlia_provider_advertisement: false,
                kademlia_protocol: if separate {
                    crate::config::PRIVATE_KADEMLIA_PROTOCOL.to_owned()
                } else {
                    crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL.to_owned()
                },
                ..DiscoveryConfig::default()
            },
        }
    }

    async fn exercise_internal_kademlia_connection_address_retention(separate: bool) {
        let mut listener = build_node(&retention_diagnostic_config(separate)).expect("listener");
        let mut dialer = build_node(&retention_diagnostic_config(separate)).expect("dialer");
        for node in [&mut listener, &mut dialer] {
            assert_eq!(node.swarm.behaviour().pairing_kad.is_enabled(), separate);
            for seed in public_ipfs_bootstrap_peer_configs() {
                let (peer, _) = seed.peer_address().expect("seed");
                public_pairing_kad_mut(node.swarm.behaviour_mut()).remove_peer(&peer);
            }
            public_pairing_kad_mut(node.swarm.behaviour_mut()).set_mode(Some(kad::Mode::Server));
        }
        let remote = listener.local_peer_id;
        let samples = super::super::address_retention::MAX_DISCOVERED_ADDRESSES_PER_PEER + 1;
        tokio::time::timeout(Duration::from_secs(30), async {
            for index in 1..=samples {
                listener
                    .swarm
                    .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
                    .unwrap();
                let address = next_listen_address(&mut listener.swarm).await;
                dialer
                    .swarm
                    .dial(address.with(Protocol::P2p(remote)))
                    .expect("dial loopback");
                loop {
                    tokio::select! {
                        event = listener.swarm.select_next_some() => { let _ = event; }
                        event = dialer.swarm.select_next_some() => {
                            let routing = match event {
                                SwarmEvent::Behaviour(BehaviourEvent::Kad(event)) if !separate => Some(event),
                                SwarmEvent::Behaviour(BehaviourEvent::PairingKad(event)) if separate => Some(event),
                                _ => None,
                            };
                            if let Some(kad::Event::RoutingUpdated { peer, addresses, .. }) = routing
                                && peer == remote && addresses.len() == index.min(samples - 1) {
                                break;
                            }
                        }
                    }
                }
                dialer.swarm.disconnect_peer_id(remote).expect("disconnect");
                loop {
                    tokio::select! {
                        event = listener.swarm.select_next_some() => { let _ = event; }
                        event = dialer.swarm.select_next_some() => {
                            if matches!(event, SwarmEvent::ConnectionClosed {
                                peer_id, num_established: 0, ..
                            } if peer_id == remote) {
                                break;
                            }
                        }
                    }
                }
            }
        })
        .await
        .expect("loopback retention diagnostic deadline");
        let retained = public_pairing_kad_mut(dialer.swarm.behaviour_mut())
            .kbuckets()
            .flat_map(|bucket| {
                bucket
                    .iter()
                    .map(|entry| entry.node.value.len())
                    .collect::<Vec<_>>()
            })
            .sum::<usize>();
        eprintln!(
            "internal_kademlia_retention separate={separate} connections={samples} addresses={retained}"
        );
        assert_eq!(retained, samples - 1);
    }

    #[tokio::test]
    async fn internal_kademlia_query_addresses_remain_bounded() {
        for (separate, small_budget) in [(false, false), (true, false), (false, true), (true, true)]
        {
            let mut source = build_node(&retention_diagnostic_config(separate)).unwrap();
            let mut client = build_node(&retention_diagnostic_config(separate)).unwrap();
            // Keep the responder deliberately unbounded so this diagnostic
            // still tests the client's independent query-cache admission.
            let mut source_config = controlled_kademlia_config(StreamProtocol::new(
                crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            ));
            source_config.set_address_limits(kad::AddressLimits::default());
            *public_pairing_kad_mut(source.swarm.behaviour_mut()) = kad::Behaviour::with_config(
                source.local_peer_id,
                kad::store::MemoryStore::new(source.local_peer_id),
                source_config,
            );
            if small_budget {
                let mut config = controlled_kademlia_config(StreamProtocol::new(
                    crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
                ));
                config.set_query_limits(kad::QueryLimits::new(
                    NonZeroUsize::new(2).unwrap(),
                    kad::AddressLimits::new(
                        NonZeroUsize::new(64).unwrap(),
                        NonZeroUsize::new(2048).unwrap(),
                    ),
                    NonZeroUsize::new(1024).unwrap(),
                ));
                *public_pairing_kad_mut(client.swarm.behaviour_mut()) = kad::Behaviour::with_config(
                    client.local_peer_id,
                    kad::store::MemoryStore::new(client.local_peer_id),
                    config,
                );
            }
            for node in [&mut source, &mut client] {
                for seed in public_ipfs_bootstrap_peer_configs() {
                    let (peer, _) = seed.peer_address().unwrap();
                    public_pairing_kad_mut(node.swarm.behaviour_mut()).remove_peer(&peer);
                }
                public_pairing_kad_mut(node.swarm.behaviour_mut())
                    .set_mode(Some(kad::Mode::Server));
            }
            source
                .swarm
                .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
                .unwrap();
            let address = next_listen_address(&mut source.swarm).await;
            let reported = PeerId::random();
            let count = super::super::address_retention::MAX_DISCOVERED_ADDRESSES_PER_PEER + 1;
            for index in 1..=count {
                // Unsupported memory addresses cannot dial unrelated local or public services.
                public_pairing_kad_mut(source.swarm.behaviour_mut())
                    .add_address(&reported, format!("/memory/{index}").parse().unwrap());
            }
            if small_budget {
                for index in 100..104 {
                    public_pairing_kad_mut(source.swarm.behaviour_mut()).add_address(
                        &PeerId::random(),
                        format!("/memory/{index}").parse().unwrap(),
                    );
                }
            }
            let kad = public_pairing_kad_mut(client.swarm.behaviour_mut());
            kad.add_address(&source.local_peer_id, address);
            let query = kad.get_closest_peers(reported);
            let retained = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    tokio::select! {
                        _ = source.swarm.select_next_some() => {}
                        _ = client.swarm.select_next_some() => {}
                    }
                    let addresses = public_pairing_kad_mut(client.swarm.behaviour_mut())
                        .handle_pending_outbound_connection(
                            libp2p::swarm::ConnectionId::new_unchecked(99_999),
                            Some(reported),
                            &[],
                            libp2p::core::Endpoint::Dialer,
                        )
                        .unwrap();
                    if !addresses.is_empty() {
                        break addresses;
                    }
                }
            })
            .await
            .expect("query retention diagnostic deadline");
            let kad = public_pairing_kad_mut(client.swarm.behaviour_mut());
            assert!(kad.kbuckets().all(|bucket| {
                bucket
                    .iter()
                    .all(|entry| entry.node.key.preimage() != &reported)
            }));
            let expected = if small_budget {
                1024 / retained[0].len()
            } else {
                count - 1
            };
            assert_eq!(retained.len(), expected);
            let encoded_bytes: usize = retained.iter().map(Multiaddr::len).sum();
            let usage = kad.query(&query).unwrap().resource_usage().unwrap();
            assert_eq!(usage.candidates, 2);
            assert_eq!(usage.address_bytes, encoded_bytes);
            assert!(usage.rejected >= count - expected);
            eprintln!(
                "internal_kademlia_query_retention separate={separate} small_budget={small_budget} addresses={} encoded_bytes={encoded_bytes} rejected={}",
                retained.len(),
                usage.rejected
            );
            let endpoint = |address| libp2p::core::ConnectedPoint::Dialer {
                address,
                role_override: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::Reuse,
            };
            let mut old = retained[0].clone();
            for index in 200..300 {
                let new = format!("/memory/{index}/p2p/{reported}")
                    .parse::<Multiaddr>()
                    .unwrap();
                kad.on_swarm_event(libp2p::swarm::FromSwarm::AddressChange(
                    libp2p::swarm::behaviour::AddressChange {
                        peer_id: reported,
                        connection_id: libp2p::swarm::ConnectionId::new_unchecked(99_999),
                        old: &endpoint(old),
                        new: &endpoint(new.clone()),
                    },
                ));
                assert_eq!(
                    kad.query(&query)
                        .unwrap()
                        .resource_usage()
                        .unwrap()
                        .address_bytes,
                    encoded_bytes
                );
                old = new;
            }
            let oversized = Multiaddr::empty().with(Protocol::Dns("a".repeat(2048).into()));
            kad.on_swarm_event(libp2p::swarm::FromSwarm::AddressChange(
                libp2p::swarm::behaviour::AddressChange {
                    peer_id: reported,
                    connection_id: libp2p::swarm::ConnectionId::new_unchecked(99_999),
                    old: &endpoint(old.clone()),
                    new: &endpoint(oversized),
                },
            ));
            let current = kad
                .handle_pending_outbound_connection(
                    libp2p::swarm::ConnectionId::new_unchecked(99_999),
                    Some(reported),
                    &[],
                    libp2p::core::Endpoint::Dialer,
                )
                .unwrap();
            assert!(current.contains(&old));
            assert_eq!(current.len(), expected);
            if small_budget {
                let over_budget = Multiaddr::empty().with(Protocol::Dns("a".repeat(100).into()));
                kad.on_swarm_event(libp2p::swarm::FromSwarm::AddressChange(
                    libp2p::swarm::behaviour::AddressChange {
                        peer_id: reported,
                        connection_id: libp2p::swarm::ConnectionId::new_unchecked(99_999),
                        old: &endpoint(old.clone()),
                        new: &endpoint(over_budget),
                    },
                ));
                assert_eq!(
                    kad.query(&query)
                        .unwrap()
                        .resource_usage()
                        .unwrap()
                        .address_bytes,
                    encoded_bytes
                );
            }
            kad.on_swarm_event(libp2p::swarm::FromSwarm::DialFailure(
                libp2p::swarm::behaviour::DialFailure {
                    peer_id: Some(reported),
                    connection_id: libp2p::swarm::ConnectionId::new_unchecked(99_999),
                    error: &libp2p::swarm::DialError::Transport(vec![(
                        old.clone(),
                        libp2p::core::transport::TransportError::Other(std::io::Error::other(
                            "injected failure",
                        )),
                    )]),
                },
            ));
            assert_eq!(
                kad.query(&query)
                    .unwrap()
                    .resource_usage()
                    .unwrap()
                    .address_bytes,
                encoded_bytes - old.len()
            );
            assert_eq!(
                kad.query(&query)
                    .unwrap()
                    .resource_usage()
                    .unwrap()
                    .candidates,
                2,
            );
            kad.on_swarm_event(libp2p::swarm::FromSwarm::AddressChange(
                libp2p::swarm::behaviour::AddressChange {
                    peer_id: reported,
                    connection_id: libp2p::swarm::ConnectionId::new_unchecked(99_999),
                    old: &endpoint(retained[1].clone()),
                    new: &endpoint(retained[2].clone()),
                },
            ));
            assert_eq!(
                kad.query(&query)
                    .unwrap()
                    .resource_usage()
                    .unwrap()
                    .address_bytes,
                encoded_bytes - old.len() - retained[1].len()
            );
            kad.query_mut(&query)
                .expect("query remains active")
                .finish();
            // Polling retires a finished query; no application routing-table admission occurs.
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    tokio::select! {
                        _ = source.swarm.select_next_some() => {}
                        _ = client.swarm.select_next_some() => {}
                    }
                    let kad = public_pairing_kad_mut(client.swarm.behaviour_mut());
                    if kad.query(&query).is_none() {
                        assert!(
                            kad.handle_pending_outbound_connection(
                                libp2p::swarm::ConnectionId::new_unchecked(99_999),
                                Some(reported),
                                &[],
                                libp2p::core::Endpoint::Dialer,
                            )
                            .unwrap()
                            .is_empty()
                        );
                        break;
                    }
                }
            })
            .await
            .expect("finished query cleanup deadline");
        }
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
    async fn pinned_overload_replies_without_resetting_tcp_or_quic_connection() {
        exercise_pinned_overload(1, Duration::ZERO).await;
    }

    #[tokio::test]
    #[ignore = "opt-in paced TCP/QUIC stream saturation and recovery exercise"]
    async fn sustained_pinned_overload_recovers_on_the_same_connection() {
        tokio::time::timeout(
            Duration::from_secs(180),
            exercise_pinned_overload(1_500, Duration::from_millis(20)),
        )
        .await
        .expect("sustained stream exercise exceeded its overall deadline");
    }

    async fn exercise_pinned_overload(rounds: usize, pace: Duration) {
        for address in ["/ip4/127.0.0.1/tcp/0", "/ip4/127.0.0.1/udp/0/quic-v1"] {
            let transport = address;
            let config = |limit, listen_addresses| HostConfig {
                identity: NodeIdentity::generate_ed25519().unwrap(),
                network_name: "bounded-streams".to_owned(),
                membership_tag: None,
                mtu: 1280,
                max_concurrent_control_streams: 64,
                max_concurrent_packet_streams: limit,
                listen_addresses,
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                known_peers: Vec::new(),
                relay_reservations: Vec::new(),
                relay_server: false,
                relay_resources: crate::config::RelayResourceConfig::default(),
                resources: crate::config::ResourceConfig::default(),
                discovery: DiscoveryConfig {
                    kademlia: false,
                    mdns: false,
                    autonat: false,
                    dcutr: false,
                    kademlia_provider_advertisement: false,
                    ..DiscoveryConfig::default()
                },
            };
            let mut listener = build_node(&config(2, vec![address.parse().unwrap()])).unwrap();
            let mut dialer = build_node(&config(1, Vec::new())).unwrap();
            // The default host also advertises request-response on this protocol.
            // Isolate the pinned receiver so this test exercises its own budget.
            for node in [&mut listener, &mut dialer] {
                for seed in public_ipfs_bootstrap_peer_configs() {
                    let (peer, _) = seed.peer_address().unwrap();
                    public_pairing_kad_mut(node.swarm.behaviour_mut()).remove_peer(&peer);
                }
                node.swarm.behaviour_mut().packet = request_response::Behaviour::with_codec(
                    packet::PacketCodec::new(1280),
                    [(
                        libp2p::StreamProtocol::new(packet::PACKET_PROTOCOL),
                        request_response::ProtocolSupport::Outbound,
                    )],
                    request_response::Config::default(),
                );
            }
            let address = next_listen_address(&mut listener.swarm).await;
            dialer
                .swarm
                .dial(address.with(Protocol::P2p(listener.local_peer_id)))
                .unwrap();
            let connection = next_connection_to_peer(
                &mut listener.swarm,
                &mut dialer.swarm,
                listener.local_peer_id,
            )
            .await;
            let frame = Frame::packet(1, 7, vec![0x45; 1024]).unwrap();
            let started = std::time::Instant::now();
            for _ in 0..rounds {
                let first = dialer
                    .swarm
                    .behaviour_mut()
                    .pinned_packet_stream
                    .send_request_on_connection(listener.local_peer_id, connection, frame.clone());
                tokio::time::timeout(Duration::from_secs(10), async {
                    let mut held_response = None;
                    let mut second = None;
                    let mut overload_seen = false;
                    loop {
                        let (at_listener, event) = tokio::select! {
                            event = listener.swarm.select_next_some() => (true, event),
                            event = dialer.swarm.select_next_some() => (false, event),
                        };
                        let SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(event)) =
                            event
                        else {
                            assert!(
                                !matches!(event, SwarmEvent::ConnectionClosed { .. }),
                                "connection closed during overload"
                            );
                            continue;
                        };
                        match event {
                            pinned_packet_stream::Event::InboundRequest {
                                peer,
                                connection_id,
                                request_id,
                                frame: received,
                            } => {
                                assert!(at_listener, "full dialer admitted an inbound packet");
                                assert_eq!(received, frame);
                                held_response =
                                    Some(pinned_packet_stream::Behaviour::response_channel(
                                        peer,
                                        connection_id,
                                        request_id,
                                    ));
                                second = Some(
                                    listener
                                        .swarm
                                        .behaviour_mut()
                                        .pinned_packet_stream
                                        .send_request_on_connection(
                                            peer,
                                            connection_id,
                                            frame.clone(),
                                        ),
                                );
                            }
                            pinned_packet_stream::Event::OutboundResponse {
                                request_id,
                                response,
                                ..
                            } if at_listener => {
                                assert_eq!(Some(request_id), second);
                                assert_eq!(
                                    response,
                                    PacketResponse::Rejected(
                                        super::super::packet::PacketRejectionReason::RateLimited
                                    )
                                );
                                overload_seen = true;
                                listener
                                    .swarm
                                    .behaviour_mut()
                                    .pinned_packet_stream
                                    .send_response(
                                        held_response.take().unwrap(),
                                        PacketResponse::Accepted,
                                    );
                            }
                            pinned_packet_stream::Event::OutboundResponse {
                                request_id,
                                response,
                                ..
                            } => {
                                assert_eq!(request_id, first);
                                assert_eq!(response, PacketResponse::Accepted);
                                assert!(overload_seen);
                                break;
                            }
                            pinned_packet_stream::Event::InboundFailure { error, .. }
                            | pinned_packet_stream::Event::OutboundFailure { error, .. } => {
                                panic!("overload reset a stream: {error:?}")
                            }
                            pinned_packet_stream::Event::ResponseSent { .. } => {}
                        }
                    }
                })
                .await
                .expect("overload exchange timed out");
                for node in [&listener, &dialer] {
                    assert_eq!(
                        node.swarm
                            .behaviour()
                            .pinned_packet_stream
                            .pending_outbound_count(),
                        0
                    );
                }
                assert!(listener.swarm.is_connected(&dialer.local_peer_id));
                assert!(dialer.swarm.is_connected(&listener.local_peer_id));
                if !pace.is_zero() {
                    tokio::time::sleep(pace).await;
                }
            }
            eprintln!(
                "pinned_overload_recovery transport={transport} rounds={rounds} elapsed_ms={} pending_outbound=0",
                started.elapsed().as_millis()
            );
        }
    }

    #[tokio::test]
    async fn kademlia_overload_preserves_shared_tcp_and_quic_packet_connections() {
        exercise_kademlia_packet_overload(false).await;
    }

    #[tokio::test]
    async fn kademlia_behaviour_queue_overload_preserves_tcp_and_quic_packets() {
        exercise_kademlia_packet_overload(true).await;
    }

    async fn exercise_kademlia_packet_overload(bound_behaviour_queue: bool) {
        for listen in ["/ip4/127.0.0.1/tcp/0", "/ip4/127.0.0.1/udp/0/quic-v1"] {
            let mut listener = build_node(&retention_diagnostic_config(false)).unwrap();
            let mut dialer = build_node(&retention_diagnostic_config(false)).unwrap();
            for node in [&mut listener, &mut dialer] {
                let mut config = controlled_kademlia_config(StreamProtocol::new(
                    crate::config::PUBLIC_IPFS_KADEMLIA_PROTOCOL,
                ));
                // Small handler payloads isolate overload from the wire/store limits.
                config.set_handler_queue_limits(kad::HandlerQueueLimits::new(
                    NonZeroUsize::new(4).unwrap(),
                    NonZeroUsize::new(512).unwrap(),
                ));
                if bound_behaviour_queue {
                    config.set_behaviour_queue_limits(kad::BehaviourQueueLimits::new(
                        NonZeroUsize::new(4).unwrap(),
                        NonZeroUsize::new(512).unwrap(),
                    ));
                }
                config.set_query_timeout(Duration::from_millis(200));
                node.swarm.behaviour_mut().kad = kad::Behaviour::with_config(
                    node.local_peer_id,
                    kad::store::MemoryStore::new(node.local_peer_id),
                    config,
                );
                node.swarm
                    .behaviour_mut()
                    .kad
                    .set_mode(Some(kad::Mode::Server));
            }
            listener.swarm.listen_on(listen.parse().unwrap()).unwrap();
            let address = next_listen_address(&mut listener.swarm).await;
            dialer
                .swarm
                .dial(address.with(Protocol::P2p(listener.local_peer_id)))
                .unwrap();
            let connection = tokio::time::timeout(
                Duration::from_secs(10),
                next_connection_to_peer(
                    &mut listener.swarm,
                    &mut dialer.swarm,
                    listener.local_peer_id,
                ),
            )
            .await
            .expect("loopback connection deadline");

            for (wave, overloaded) in [(0, true), (1, false), (2, true), (3, false)] {
                let count = if overloaded {
                    KADEMLIA_QUERY_POOL_CAPACITY.get()
                } else {
                    1
                };
                let kad = &mut dialer.swarm.behaviour_mut().kad;
                if bound_behaviour_queue && overloaded {
                    let before = kad.behaviour_queue_usage().rejected;
                    for _ in 0..32 {
                        kad.add_address(&PeerId::random(), "/memory/1".parse().unwrap());
                    }
                    assert_eq!(kad.behaviour_queue_usage().events, 4);
                    assert!(kad.behaviour_queue_usage().rejected > before);
                }
                let mut queries = (0..count)
                    .map(|index| {
                        kad.try_start_query(|kad| {
                            kad.put_record_to(
                                kad::Record::new(
                                    vec![wave, u8::try_from(index).unwrap()],
                                    vec![1; if overloaded { 513 } else { 1 }],
                                ),
                                [listener.local_peer_id].into_iter(),
                                kad::Quorum::One,
                            )
                        })
                        .unwrap()
                    })
                    .collect::<HashSet<_>>();
                assert_eq!(kad.query_pool_usage().retained, count);
                if overloaded {
                    assert!(
                        kad.try_start_query(|_| panic!("full pool admitted a query"))
                            .is_err()
                    );
                    let canceled = *queries.iter().next().unwrap();
                    assert!(kad.cancel_query(&canceled));
                    queries.remove(&canceled);
                    assert!(queries.iter().all(|id| kad.query_is_retained(id)));
                }
                tokio::time::timeout(
                    Duration::from_secs(2),
                    exchange_packets_during_kademlia_work(
                        &mut listener.swarm,
                        &mut dialer.swarm,
                        connection,
                        wave,
                        queries,
                        overloaded,
                    ),
                )
                .await
                .expect("VPN traffic or DHT failed to retire during overload/recovery");
                assert_eq!(dialer.swarm.behaviour().kad.query_pool_usage().retained, 0);
                assert_eq!(dialer.swarm.behaviour().kad.pending_rpc_usage().requests, 0);
                if bound_behaviour_queue {
                    for node in [&listener, &dialer] {
                        let usage = node.swarm.behaviour().kad.behaviour_queue_usage();
                        assert!(usage.events <= 4 && usage.bytes <= 512);
                    }
                }
                if !overloaded {
                    use libp2p::kad::store::RecordStore;
                    assert!(
                        listener
                            .swarm
                            .behaviour_mut()
                            .kad
                            .store_mut()
                            .get(&kad::RecordKey::new(&[wave, 0]))
                            .is_some()
                    );
                }
            }
        }
    }

    async fn exchange_packets_during_kademlia_work(
        listener: &mut Swarm<Behaviour>,
        dialer: &mut Swarm<Behaviour>,
        connection: libp2p::swarm::ConnectionId,
        wave: u8,
        mut queries: HashSet<kad::QueryId>,
        overloaded: bool,
    ) {
        let mut packets = std::collections::HashMap::new();
        for sequence in 0..8 {
            let frame = Frame::packet(u32::from(wave), sequence, vec![0x45; 1024]).unwrap();
            let id = dialer
                .behaviour_mut()
                .pinned_packet_stream
                .send_request_on_connection(*listener.local_peer_id(), connection, frame.clone());
            packets.insert(id, frame);
        }
        let mut received = HashSet::new();
        let deadline = tokio::time::sleep(Duration::from_secs(1));
        tokio::pin!(deadline);
        while !queries.is_empty() || !packets.is_empty() {
            let (at_listener, event) = tokio::select! {
                event = listener.select_next_some() => (true, event),
                event = dialer.select_next_some() => (false, event),
                () = &mut deadline => panic!("wave={wave} overloaded={overloaded} queries={} packets={} received={}", queries.len(), packets.len(), received.len()),
            };
            match event {
                SwarmEvent::ConnectionClosed { .. } => {
                    panic!("Kademlia overload reset a shared connection")
                }
                SwarmEvent::ConnectionEstablished {
                    num_established, ..
                } => {
                    assert!(at_listener, "dialer replaced the original connection");
                    assert_eq!(num_established.get(), 1);
                }
                SwarmEvent::Behaviour(BehaviourEvent::Packet(
                    request_response::Event::Message {
                        message:
                            Message::Request {
                                request, channel, ..
                            },
                        peer,
                        ..
                    },
                )) => {
                    assert!(at_listener);
                    assert_eq!(peer, *dialer.local_peer_id());
                    assert!(packets.values().any(|expected| expected == &request));
                    assert!(received.insert(request.header.sequence));
                    listener
                        .behaviour_mut()
                        .packet
                        .send_response(channel, PacketResponse::Accepted)
                        .unwrap();
                }
                SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
                    pinned_packet_stream::Event::OutboundResponse {
                        request_id,
                        connection_id,
                        response,
                        ..
                    },
                )) => {
                    assert!(!at_listener);
                    assert_eq!(connection_id, connection);
                    assert_eq!(response, PacketResponse::Accepted);
                    assert!(packets.remove(&request_id).is_some());
                }
                SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
                    pinned_packet_stream::Event::OutboundFailure { error, .. }
                    | pinned_packet_stream::Event::InboundFailure { error, .. },
                )) => panic!("VPN packet failed during Kademlia work: {error:?}"),
                SwarmEvent::Behaviour(BehaviourEvent::Kad(
                    kad::Event::OutboundQueryProgressed {
                        id,
                        result: kad::QueryResult::PutRecord(result),
                        stats,
                        ..
                    },
                )) if !at_listener => {
                    assert!(queries.remove(&id), "unowned or canceled DHT result");
                    assert_eq!(result.is_err(), overloaded);
                    if !matches!(result, Err(kad::PutRecordError::Timeout { .. })) {
                        assert_eq!(stats.num_failures(), u32::from(overloaded));
                    }
                    assert_eq!(stats.num_successes(), u32::from(!overloaded));
                }
                SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::InboundRequest {
                    request: kad::InboundRequest::PutRecord { .. },
                })) if at_listener => assert!(!overloaded, "rejected payload reached the peer"),
                _ => {}
            }
        }
        assert_eq!(received.len(), 8);
    }

    #[tokio::test]
    async fn default_tcp_and_quic_hosts_deliver_pinned_requests_to_packet_owner() {
        for listen in ["/ip4/127.0.0.1/tcp/0", "/ip4/127.0.0.1/udp/0/quic-v1"] {
            let mut listener = build_node(&HostConfig {
                identity: NodeIdentity::generate_ed25519().expect("listener identity"),
                network_name: "lab".to_owned(),
                membership_tag: None,
                mtu: 1280,
                max_concurrent_control_streams: 64,
                max_concurrent_packet_streams: 256,
                listen_addresses: vec![listen.parse().expect("listen address")],
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

    pub(super) async fn next_listen_address(swarm: &mut Swarm<Behaviour>) -> Multiaddr {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                return address;
            }
        }
    }

    pub(super) async fn next_connection_to_peer(
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
                            pinned_packet_stream::Event::InboundRequest { .. },
                        )) => {
                            panic!("default host changed its inbound Packet event owner");
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
                            message: Message::Request { request, channel, .. },
                            peer, ..
                        })) => {
                            assert_eq!(peer, *dialer.local_peer_id());
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
