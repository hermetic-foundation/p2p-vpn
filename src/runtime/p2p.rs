use std::error::Error;

use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder, dcutr, identify,
    identity::Keypair,
    kad, mdns,
    multiaddr::Protocol,
    noise, ping, relay, request_response,
    swarm::{NetworkBehaviour, behaviour::toggle::Toggle},
    tcp, yamux,
};

use crate::{
    config::DiscoveryConfig,
    identity::{IdentityError, NodeIdentity},
    runtime::{
        control::{self, ControlCodec},
        packet::{self, PacketCodec},
    },
};

const PROTOCOL_VERSION: &str = "/p2p-vpn/0.1.0";

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub kad: kad::Behaviour<kad::store::MemoryStore>,
    pub relay: relay::client::Behaviour,
    pub relay_server: Toggle<relay::Behaviour>,
    pub dcutr: Toggle<dcutr::Behaviour>,
    pub mdns: Toggle<mdns::tokio::Behaviour>,
    pub control: request_response::Behaviour<ControlCodec>,
    pub packet: request_response::Behaviour<PacketCodec>,
}

pub struct P2pNode {
    pub local_peer_id: PeerId,
    pub swarm: Swarm<Behaviour>,
    pub discovery: DiscoveryConfig,
    pub bootstrap_peer_addresses: Vec<(PeerId, Multiaddr)>,
    pub configured_peer_addresses: Vec<(PeerId, Multiaddr)>,
    pub startup: StartupStatus,
}

pub struct HostConfig {
    pub identity: NodeIdentity,
    pub network_name: String,
    pub mtu: u16,
    pub max_concurrent_control_streams: usize,
    pub max_concurrent_packet_streams: usize,
    pub listen_addresses: Vec<Multiaddr>,
    pub bootstrap_peers: Vec<(PeerId, Multiaddr)>,
    pub known_peers: Vec<(PeerId, Multiaddr)>,
    pub relay_reservations: Vec<Multiaddr>,
    pub relay_server: bool,
    pub discovery: DiscoveryConfig,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StartupStatus {
    pub mdns_enabled: bool,
    pub dcutr_enabled: bool,
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
    let configured_peer_addresses = config.known_peers.clone();

    let discovery = config.discovery;
    let relay_server = config.relay_server;
    let mtu = config.mtu;
    let control_streams = config.max_concurrent_control_streams;
    let packet_streams = config.max_concurrent_packet_streams;

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_quic()
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(
            |keypair, relay| -> Result<Behaviour, Box<dyn Error + Send + Sync>> {
                let local_peer_id = keypair.public().to_peer_id();
                let store = kad::store::MemoryStore::new(local_peer_id);
                let kad_config = kad::Config::new(libp2p::StreamProtocol::new("/p2p-vpn/kad/1"));
                let mut kad = kad::Behaviour::with_config(local_peer_id, store, kad_config);
                if discovery.kademlia {
                    kad.set_mode(Some(kad::Mode::Server));
                } else {
                    kad.set_mode(Some(kad::Mode::Client));
                }
                let mdns = if discovery.mdns {
                    Some(mdns::tokio::Behaviour::new(
                        mdns::Config::default(),
                        local_peer_id,
                    )?)
                } else {
                    None
                };

                Ok(Behaviour {
                    identify: identify::Behaviour::new(identify::Config::new(
                        PROTOCOL_VERSION.to_owned(),
                        keypair.public(),
                    )),
                    ping: ping::Behaviour::default(),
                    kad,
                    relay,
                    relay_server: relay_server
                        .then(|| relay::Behaviour::new(local_peer_id, relay::Config::default()))
                        .into(),
                    dcutr: discovery
                        .dcutr
                        .then(|| dcutr::Behaviour::new(local_peer_id))
                        .into(),
                    mdns: mdns.into(),
                    control: control::behaviour(control_streams),
                    packet: packet::behaviour(mtu, packet_streams),
                })
            },
        )?
        .build();

    let relay_reservations_started = config.relay_reservations.len();
    install_listeners_and_dials(&mut swarm, local_peer_id, config)?;
    let kademlia = start_kademlia(&mut swarm, config)?;

    Ok(P2pNode {
        local_peer_id,
        swarm,
        discovery: config.discovery,
        bootstrap_peer_addresses,
        configured_peer_addresses,
        startup: StartupStatus {
            mdns_enabled: config.discovery.mdns,
            dcutr_enabled: config.discovery.dcutr,
            kademlia,
            relay_reservations_started,
            relay_server_enabled: config.relay_server,
        },
    })
}

fn install_listeners_and_dials(
    swarm: &mut Swarm<Behaviour>,
    local_peer_id: PeerId,
    config: &HostConfig,
) -> Result<(), P2pBuildError> {
    for address in &config.listen_addresses {
        swarm.listen_on(address.clone())?;
    }

    for address in &config.relay_reservations {
        swarm.listen_on(peer_dial_address(local_peer_id, address.clone())?)?;
    }

    for (peer, address) in config
        .bootstrap_peers
        .iter()
        .chain(config.known_peers.iter())
    {
        swarm.behaviour_mut().kad.add_address(peer, address.clone());
        let dial_address = peer_dial_address(*peer, address.clone())?;
        swarm.dial(dial_address)?;
    }

    Ok(())
}

fn start_kademlia(
    swarm: &mut Swarm<Behaviour>,
    config: &HostConfig,
) -> Result<KademliaStartupStatus, P2pBuildError> {
    if !config.discovery.kademlia {
        return Ok(KademliaStartupStatus::default());
    }

    let rendezvous_key = kademlia_rendezvous_key(&config.network_name);
    swarm
        .behaviour_mut()
        .kad
        .start_providing(rendezvous_key.clone())?;
    swarm.behaviour_mut().kad.get_providers(rendezvous_key);

    Ok(KademliaStartupStatus {
        bootstrap_started: swarm.behaviour_mut().kad.bootstrap().is_ok(),
        rendezvous_advertise_started: true,
        rendezvous_lookup_started: true,
    })
}

#[must_use]
pub fn kademlia_rendezvous_key(network_name: &str) -> kad::RecordKey {
    kad::RecordKey::new(&format!("/p2p-vpn/{network_name}/providers/1"))
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

    use futures::StreamExt as _;
    use libp2p::{
        multiaddr::Protocol,
        request_response::{self, Message},
        swarm::SwarmEvent,
    };

    use crate::{
        runtime::control::{ControlCapabilities, ControlRequest, ControlResponse},
        wire::{Frame, PayloadType},
    };

    use super::*;

    #[tokio::test]
    async fn build_node_uses_configured_identity() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let expected_peer_id = identity.peer_id.parse::<PeerId>().expect("peer id");

        let node = build_node(&HostConfig {
            identity,
            network_name: "lab".to_owned(),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            discovery: DiscoveryConfig::default(),
        })
        .expect("node should build");

        assert_eq!(node.local_peer_id, expected_peer_id);
        assert!(node.startup.mdns_enabled);
        assert!(node.startup.dcutr_enabled);
        assert!(node.swarm.behaviour().mdns.is_enabled());
        assert!(node.swarm.behaviour().dcutr.is_enabled());
        assert!(!node.startup.relay_server_enabled);
        assert!(!node.swarm.behaviour().relay_server.is_enabled());
    }

    #[tokio::test]
    async fn build_node_disables_optional_discovery_behaviours() {
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: false,
            dcutr: false,
        };

        let node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            discovery,
        })
        .expect("node should build");

        assert!(!node.startup.mdns_enabled);
        assert!(!node.startup.dcutr_enabled);
        assert!(!node.startup.kademlia.bootstrap_started);
        assert!(!node.startup.kademlia.rendezvous_advertise_started);
        assert!(!node.startup.kademlia.rendezvous_lookup_started);
        assert!(!node.swarm.behaviour().mdns.is_enabled());
        assert!(!node.swarm.behaviour().dcutr.is_enabled());
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
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            bootstrap_peers: vec![(relay, bootstrap_address)],
            known_peers: Vec::new(),
            relay_reservations: vec![relay_reservation],
            relay_server: true,
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");

        assert!(node.startup.kademlia.bootstrap_started);
        assert!(node.startup.kademlia.rendezvous_advertise_started);
        assert!(node.startup.kademlia.rendezvous_lookup_started);
        assert!(node.startup.mdns_enabled);
        assert!(node.startup.dcutr_enabled);
        assert_eq!(node.startup.relay_reservations_started, 1);
        assert!(node.startup.relay_server_enabled);
        assert!(node.swarm.behaviour().relay_server.is_enabled());
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
    fn kademlia_rendezvous_key_is_scoped_to_network_name() {
        assert_eq!(
            kademlia_rendezvous_key("lab").to_vec(),
            b"/p2p-vpn/lab/providers/1".to_vec()
        );
        assert_ne!(
            kademlia_rendezvous_key("lab").to_vec(),
            kademlia_rendezvous_key("prod").to_vec()
        );
    }

    #[tokio::test]
    async fn two_nodes_exchange_packet_request() {
        let mut listener = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            discovery: DiscoveryConfig::default(),
        })
        .expect("listener node");
        let listener_address = next_listen_address(&mut listener.swarm).await;

        let mut dialer = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("dialer identity"),
            network_name: "lab".to_owned(),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener.local_peer_id, listener_address)],
            relay_reservations: Vec::new(),
            relay_server: false,
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
    async fn two_nodes_exchange_control_capabilities() {
        let mut listener = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            discovery: DiscoveryConfig::default(),
        })
        .expect("listener node");
        let listener_address = next_listen_address(&mut listener.swarm).await;

        let mut dialer = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("dialer identity"),
            network_name: "lab".to_owned(),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener.local_peer_id, listener_address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            discovery: DiscoveryConfig::default(),
        })
        .expect("dialer node");
        let request = ControlRequest::Capabilities(ControlCapabilities::local(1280));
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
    async fn relayed_nodes_exchange_packet_request() {
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: false,
            dcutr: false,
        };
        let mut relay = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("relay listen")],
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            discovery,
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
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: vec![relayed_listener_address.clone()],
            relay_server: false,
            discovery,
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
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: vec![(listener_peer, relayed_target_address)],
            relay_reservations: Vec::new(),
            relay_server: false,
            discovery,
        })
        .expect("dialer node");
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

    async fn next_listen_address(swarm: &mut Swarm<Behaviour>) -> Multiaddr {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                return address;
            }
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
                                ControlResponse::CapabilitiesAccepted(ControlCapabilities::local(1280)),
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
                            ControlResponse::CapabilitiesAccepted(ControlCapabilities::local(1280))
                        );
                        return;
                    }
                }
            }
        }
    }
}
