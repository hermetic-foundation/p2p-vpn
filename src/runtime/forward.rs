use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use libp2p::{PeerId as Libp2pPeerId, Swarm, request_response};

use crate::{
    PeerId, Sequence, SessionId,
    config::{Config, ConfigError},
    queue::{EnqueueError, Packet, PeerQueues},
    route::{RouteError, RouteTable, builtin_ipv4, builtin_ipv6},
    runtime::{
        p2p::Behaviour,
        packet::{AuthorizedPeers, PacketResponse},
    },
    wire::{Frame, FrameError, PayloadType},
};

#[derive(Debug)]
pub struct Forwarder {
    local_peer: PeerId,
    routes: RouteTable,
    peers: HashMap<PeerId, Libp2pPeerId>,
    authorized_peers: AuthorizedPeers,
    replay_windows: HashMap<(PeerId, SessionId), ReplayWindow>,
    session_id: SessionId,
    next_sequence: Sequence,
    mtu: usize,
}

const REPLAY_WINDOW_BITS: u64 = 64;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ReplayWindow {
    highest: Option<Sequence>,
    seen: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplayAcceptError {
    Duplicate,
    TooOld,
}

impl ReplayWindow {
    fn accept(&mut self, sequence: Sequence) -> Result<(), ReplayAcceptError> {
        let Some(highest) = self.highest else {
            self.highest = Some(sequence);
            self.seen = 1;
            return Ok(());
        };

        if sequence > highest {
            let shift = sequence - highest;
            self.seen = if shift >= REPLAY_WINDOW_BITS {
                1
            } else {
                (self.seen << shift) | 1
            };
            self.highest = Some(sequence);
            return Ok(());
        }

        let offset = highest - sequence;
        if offset >= REPLAY_WINDOW_BITS {
            return Err(ReplayAcceptError::TooOld);
        }
        let bit = 1_u64 << offset;
        if self.seen & bit != 0 {
            return Err(ReplayAcceptError::Duplicate);
        }

        self.seen |= bit;
        Ok(())
    }
}

impl Forwarder {
    pub fn from_config(config: &Config) -> Result<Self, ForwardError> {
        let peers = config
            .peers
            .iter()
            .filter_map(|peer| {
                let libp2p_peer = peer.id.parse::<Libp2pPeerId>().ok()?;
                Some((PeerId::from_libp2p(libp2p_peer), libp2p_peer))
            })
            .collect();

        let local_peer = config.local_peer_id()?;

        Ok(Self {
            local_peer,
            routes: config.compile_routes()?,
            peers,
            authorized_peers: AuthorizedPeers::from_config(config),
            replay_windows: HashMap::new(),
            session_id: session_id_for_peer(local_peer),
            next_sequence: 0,
            mtu: usize::from(config.effective_packet_mtu()),
        })
    }

    #[must_use]
    pub const fn mtu(&self) -> usize {
        self.mtu
    }

    pub fn send_tun_packet(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        packet: Vec<u8>,
    ) -> Result<request_response::OutboundRequestId, ForwardError> {
        let packet = self.prepare_tun_packet(packet)?;
        self.send_queued_packet(swarm, &packet)
    }

    pub fn enqueue_tun_packet(
        &mut self,
        queues: &mut PeerQueues,
        packet: Vec<u8>,
    ) -> Result<(), ForwardError> {
        let packet = self.prepare_tun_packet(packet)?;
        Ok(queues.enqueue(packet)?)
    }

    pub fn send_queued_packet(
        &self,
        swarm: &mut Swarm<Behaviour>,
        packet: &Packet,
    ) -> Result<request_response::OutboundRequestId, ForwardError> {
        self.send_queued_packet_with_mtu(swarm, packet, self.mtu_u16())
    }

    pub fn send_queued_packet_with_mtu(
        &self,
        swarm: &mut Swarm<Behaviour>,
        packet: &Packet,
        peer_mtu: u16,
    ) -> Result<request_response::OutboundRequestId, ForwardError> {
        let max = self.mtu.min(usize::from(peer_mtu));
        if packet.payload().len() > max {
            return Err(ForwardError::PacketTooLarge {
                actual: packet.payload().len(),
                max,
            });
        }

        let peer = self
            .peers
            .get(&packet.peer())
            .ok_or(ForwardError::NoTransportPeer(packet.peer()))?;
        let frame = self.packet_frame(packet)?;

        Ok(swarm.behaviour_mut().packet.send_request(peer, frame))
    }

    #[must_use]
    pub fn is_configured_transport_peer(&self, peer: Libp2pPeerId) -> bool {
        self.peers.values().any(|configured| *configured == peer)
    }

    fn packet_frame(&self, packet: &Packet) -> Result<Frame, ForwardError> {
        Ok(Frame::packet(
            self.session_id,
            packet.sequence(),
            packet.payload().to_vec(),
        )?)
    }

    fn mtu_u16(&self) -> u16 {
        u16::try_from(self.mtu).unwrap_or(u16::MAX)
    }

    fn prepare_tun_packet(&mut self, packet: Vec<u8>) -> Result<Packet, ForwardError> {
        if packet.len() > self.mtu {
            return Err(ForwardError::PacketTooLarge {
                actual: packet.len(),
                max: self.mtu,
            });
        }

        let source = packet_source(&packet)?;
        self.authorize_local_source(source)?;

        let destination = packet_destination(&packet)?;
        let route = self
            .routes
            .resolve(destination)
            .ok_or(ForwardError::NoRoute(destination))?;
        if !self.peers.contains_key(&route.owner) {
            return Err(ForwardError::NoTransportPeer(route.owner));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);

        Ok(Packet::new(route.owner, sequence, packet))
    }

    fn authorize_local_source(&self, source: IpAddr) -> Result<(), ForwardError> {
        match source {
            IpAddr::V4(address) if address == builtin_ipv4(self.local_peer) => Ok(()),
            IpAddr::V6(address) if address == builtin_ipv6(self.local_peer) => Ok(()),
            _ => Err(ForwardError::UnauthorizedLocalSource { source }),
        }
    }

    pub fn accept_inbound_packet<'a>(
        &mut self,
        peer: Libp2pPeerId,
        frame: &'a Frame,
    ) -> Result<&'a [u8], ForwardError> {
        if !self.authorized_peers.allows(&peer) {
            return Err(ForwardError::UnauthorizedPeer(peer));
        }
        if frame.header.payload_type != PayloadType::IpPacket {
            return Err(ForwardError::UnexpectedPayload(frame.header.payload_type));
        }
        if usize::from(frame.header.payload_len) != frame.payload.len() {
            return Err(ForwardError::PayloadLengthMismatch {
                header: frame.header.payload_len,
                actual: frame.payload.len(),
            });
        }
        if frame.payload.len() > self.mtu {
            return Err(ForwardError::PacketTooLarge {
                actual: frame.payload.len(),
                max: self.mtu,
            });
        }

        let overlay_peer = PeerId::from_libp2p(peer);
        let source = packet_source(&frame.payload)?;
        self.routes.authorize_source(overlay_peer, source)?;
        let destination = packet_destination(&frame.payload)?;
        self.authorize_local_destination(destination)?;
        self.accept_sequence(overlay_peer, frame.header.session_id, frame.header.sequence)?;

        Ok(&frame.payload)
    }

    fn accept_sequence(
        &mut self,
        peer: PeerId,
        session_id: SessionId,
        sequence: Sequence,
    ) -> Result<(), ForwardError> {
        let window = self.replay_windows.entry((peer, session_id)).or_default();
        window.accept(sequence).map_err(|error| match error {
            ReplayAcceptError::Duplicate => ForwardError::ReplayedPacket {
                peer,
                session_id,
                sequence,
            },
            ReplayAcceptError::TooOld => ForwardError::PacketOutsideReplayWindow {
                peer,
                session_id,
                sequence,
            },
        })
    }

    fn authorize_local_destination(&self, destination: IpAddr) -> Result<(), ForwardError> {
        match destination {
            IpAddr::V4(address) if address == builtin_ipv4(self.local_peer) => Ok(()),
            IpAddr::V6(address) if address == builtin_ipv6(self.local_peer) => Ok(()),
            _ => Err(ForwardError::UnauthorizedLocalDestination { destination }),
        }
    }

    pub fn send_packet_response(
        swarm: &mut Swarm<Behaviour>,
        channel: request_response::ResponseChannel<PacketResponse>,
        response: PacketResponse,
    ) -> Result<(), PacketResponse> {
        swarm
            .behaviour_mut()
            .packet
            .send_response(channel, response)
    }
}

#[must_use]
pub fn session_id_for_peer(peer: PeerId) -> SessionId {
    let bytes = peer.as_bytes();
    let session_id = SessionId::from_be_bytes(bytes[..4].try_into().expect("fixed slice length"));
    session_id.max(1)
}

pub fn packet_source(packet: &[u8]) -> Result<IpAddr, ForwardError> {
    match ip_version(packet)? {
        4 => ipv4_endpoint(packet, 12),
        6 => ipv6_endpoint(packet, 8),
        version => Err(ForwardError::UnsupportedIpVersion(version)),
    }
}

pub fn packet_destination(packet: &[u8]) -> Result<IpAddr, ForwardError> {
    match ip_version(packet)? {
        4 => ipv4_endpoint(packet, 16),
        6 => ipv6_endpoint(packet, 24),
        version => Err(ForwardError::UnsupportedIpVersion(version)),
    }
}

fn ip_version(packet: &[u8]) -> Result<u8, ForwardError> {
    packet
        .first()
        .map(|byte| byte >> 4)
        .ok_or(ForwardError::TruncatedIpPacket {
            actual: 0,
            expected: 1,
        })
}

fn ipv4_endpoint(packet: &[u8], offset: usize) -> Result<IpAddr, ForwardError> {
    let expected = offset + 4;
    if packet.len() < expected {
        return Err(ForwardError::TruncatedIpPacket {
            actual: packet.len(),
            expected,
        });
    }

    Ok(IpAddr::V4(Ipv4Addr::from(
        <[u8; 4]>::try_from(&packet[offset..expected]).expect("fixed slice length"),
    )))
}

fn ipv6_endpoint(packet: &[u8], offset: usize) -> Result<IpAddr, ForwardError> {
    let expected = offset + 16;
    if packet.len() < expected {
        return Err(ForwardError::TruncatedIpPacket {
            actual: packet.len(),
            expected,
        });
    }

    Ok(IpAddr::V6(Ipv6Addr::from(
        <[u8; 16]>::try_from(&packet[offset..expected]).expect("fixed slice length"),
    )))
}

#[derive(Debug)]
pub enum ForwardError {
    Config(ConfigError),
    Route(RouteError),
    Frame(FrameError),
    Enqueue(EnqueueError),
    NoRoute(IpAddr),
    NoTransportPeer(PeerId),
    PacketTooLarge {
        actual: usize,
        max: usize,
    },
    UnauthorizedLocalSource {
        source: IpAddr,
    },
    UnauthorizedLocalDestination {
        destination: IpAddr,
    },
    TruncatedIpPacket {
        actual: usize,
        expected: usize,
    },
    UnsupportedIpVersion(u8),
    UnauthorizedPeer(Libp2pPeerId),
    UnexpectedPayload(PayloadType),
    PayloadLengthMismatch {
        header: u16,
        actual: usize,
    },
    ReplayedPacket {
        peer: PeerId,
        session_id: SessionId,
        sequence: Sequence,
    },
    PacketOutsideReplayWindow {
        peer: PeerId,
        session_id: SessionId,
        sequence: Sequence,
    },
}

impl From<ConfigError> for ForwardError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<RouteError> for ForwardError {
    fn from(error: RouteError) -> Self {
        Self::Route(error)
    }
}

impl From<FrameError> for ForwardError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<EnqueueError> for ForwardError {
    fn from(error: EnqueueError) -> Self {
        Self::Enqueue(error)
    }
}

#[cfg(test)]
mod tests {
    use libp2p::identity::Keypair;

    use crate::{
        config::{InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig, ResourceConfig},
        route::{builtin_ipv4, builtin_ipv6},
        runtime::p2p::{HostConfig, build_node},
        wire::Header,
    };

    use super::*;

    fn config_for(remote: Libp2pPeerId) -> Config {
        Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: Keypair::generate_ed25519()
                    .public()
                    .to_peer_id()
                    .to_string(),
                private_key: None,
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: Some("remote".to_owned()),
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        }
    }

    fn local_ipv4(config: &Config) -> Ipv4Addr {
        builtin_ipv4(config.local_peer_id().expect("local peer id"))
    }

    fn local_ipv6(config: &Config) -> Ipv6Addr {
        builtin_ipv6(config.local_peer_id().expect("local peer id"))
    }

    fn ipv4_packet(source: Ipv4Addr, destination: Ipv4Addr) -> Vec<u8> {
        let mut packet = vec![0; 20];
        packet[0] = 0x45;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        packet
    }

    fn ipv6_packet(source: Ipv6Addr, destination: Ipv6Addr) -> Vec<u8> {
        let mut packet = vec![0; 40];
        packet[0] = 0x60;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        packet
    }

    #[test]
    fn session_id_is_derived_from_local_peer_and_never_zero() {
        assert_eq!(session_id_for_peer(PeerId::from_bytes([0; 32])), 1);
        assert_eq!(
            session_id_for_peer(PeerId::from_bytes([
                0x12, 0x34, 0x56, 0x78, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0
            ])),
            0x1234_5678
        );
    }

    #[test]
    fn packet_endpoints_parse_ipv4_and_ipv6() {
        let source4 = Ipv4Addr::new(100, 64, 1, 2);
        let destination4 = Ipv4Addr::new(100, 64, 3, 4);
        let source6 = Ipv6Addr::LOCALHOST;
        let destination6 = Ipv6Addr::UNSPECIFIED;

        assert_eq!(
            packet_source(&ipv4_packet(source4, destination4)).expect("source"),
            IpAddr::V4(source4)
        );
        assert_eq!(
            packet_destination(&ipv4_packet(source4, destination4)).expect("destination"),
            IpAddr::V4(destination4)
        );
        assert_eq!(
            packet_source(&ipv6_packet(source6, destination6)).expect("source"),
            IpAddr::V6(source6)
        );
        assert_eq!(
            packet_destination(&ipv6_packet(source6, destination6)).expect("destination"),
            IpAddr::V6(destination6)
        );
    }

    #[test]
    fn inbound_packet_must_match_peer_route_ownership() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert_eq!(
            forwarder
                .accept_inbound_packet(remote, &frame)
                .expect("packet accepted"),
            frame.payload.as_slice()
        );
    }

    #[test]
    fn inbound_packet_rejects_source_spoofing() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let packet = ipv4_packet(Ipv4Addr::new(198, 51, 100, 1), Ipv4Addr::new(100, 64, 9, 9));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::Route(RouteError::UnauthorizedSource { .. }))
        ));
    }

    #[test]
    fn inbound_packet_must_target_local_overlay_address() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), Ipv4Addr::new(100, 64, 9, 9));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::UnauthorizedLocalDestination {
                destination: IpAddr::V4(destination)
            }) if destination == Ipv4Addr::new(100, 64, 9, 9)
        ));
    }

    #[test]
    fn inbound_ipv6_packet_can_target_local_overlay_address() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv6_packet(builtin_ipv6(remote_overlay), local_ipv6(&config));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert_eq!(
            forwarder
                .accept_inbound_packet(remote, &frame)
                .expect("packet accepted"),
            frame.payload.as_slice()
        );
    }

    #[test]
    fn inbound_packet_rejects_duplicate_sequence_in_session() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let frame = Frame::packet(7, 42, packet).expect("frame");

        forwarder
            .accept_inbound_packet(remote, &frame)
            .expect("first packet accepted");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::ReplayedPacket {
                peer,
                session_id: 7,
                sequence: 42
            }) if peer == remote_overlay
        ));
    }

    #[test]
    fn inbound_packet_accepts_out_of_order_sequence_inside_replay_window() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let later = Frame::packet(7, 42, packet.clone()).expect("later frame");
        let earlier = Frame::packet(7, 41, packet).expect("earlier frame");

        forwarder
            .accept_inbound_packet(remote, &later)
            .expect("later packet accepted");
        forwarder
            .accept_inbound_packet(remote, &earlier)
            .expect("earlier packet accepted");
    }

    #[test]
    fn inbound_packet_rejects_sequence_outside_replay_window() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let current = Frame::packet(7, 100, packet.clone()).expect("current frame");
        let too_old = Frame::packet(7, 36, packet).expect("old frame");

        forwarder
            .accept_inbound_packet(remote, &current)
            .expect("current packet accepted");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &too_old),
            Err(ForwardError::PacketOutsideReplayWindow {
                peer,
                session_id: 7,
                sequence: 36
            }) if peer == remote_overlay
        ));
    }

    #[test]
    fn inbound_packet_tracks_replay_windows_per_session() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let first_session = Frame::packet(7, 42, packet.clone()).expect("first session frame");
        let second_session = Frame::packet(8, 42, packet).expect("second session frame");

        forwarder
            .accept_inbound_packet(remote, &first_session)
            .expect("first session accepted");
        forwarder
            .accept_inbound_packet(remote, &second_session)
            .expect("second session accepted");
    }

    #[tokio::test]
    async fn outbound_packet_resolves_destination_to_libp2p_peer() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut config = config_for(remote);
        config.network.local_peer = local_identity.peer_id.clone();
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
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
            discovery: crate::config::DiscoveryConfig::default(),
        })
        .expect("node");
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));

        let request_id = forwarder
            .send_tun_packet(&mut node.swarm, packet)
            .expect("request id");

        assert_ne!(format!("{request_id:?}"), "");
    }

    #[test]
    fn outbound_packet_can_be_enqueued_before_send() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(1, 1280);
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));

        forwarder
            .enqueue_tun_packet(&mut queues, packet)
            .expect("queued");

        let queued_packet = queues.dequeue().expect("queued packet");
        assert_eq!(queued_packet.peer(), remote_overlay);
        assert_eq!(queued_packet.sequence(), 0);
    }

    #[test]
    fn outbound_frame_carries_local_session_and_packet_sequence() {
        let local = PeerId::from_bytes([
            0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0,
        ]);
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut config = config_for(remote);
        config.network.local_peer = local.to_string();
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));
        let mut queues = PeerQueues::new(1, 1280);

        forwarder
            .enqueue_tun_packet(&mut queues, packet)
            .expect("queued");
        let queued_packet = queues.dequeue().expect("queued packet");
        let frame = forwarder.packet_frame(&queued_packet).expect("frame");

        assert_eq!(frame.header.session_id, 0xdead_beef);
        assert_eq!(frame.header.sequence, 0);
        assert_eq!(frame.header.payload_len, 20);
    }

    #[test]
    fn outbound_packet_reports_queue_backpressure() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(1, 1280);
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));

        forwarder
            .enqueue_tun_packet(&mut queues, packet.clone())
            .expect("first packet queued");

        assert!(matches!(
            forwarder.enqueue_tun_packet(&mut queues, packet),
            Err(ForwardError::Enqueue(EnqueueError::QueueFull { .. }))
        ));
    }

    #[tokio::test]
    async fn queued_packet_respects_peer_advertised_mtu() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let mut config = config_for(remote);
        config.network.local_peer = local_identity.peer_id.clone();
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(1, 1280);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
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
            discovery: crate::config::DiscoveryConfig::default(),
        })
        .expect("node");
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));
        forwarder
            .enqueue_tun_packet(&mut queues, packet)
            .expect("queued");
        let queued_packet = queues.dequeue().expect("queued packet");

        assert!(matches!(
            forwarder.send_queued_packet_with_mtu(&mut node.swarm, &queued_packet, 19),
            Err(ForwardError::PacketTooLarge {
                actual: 20,
                max: 19
            })
        ));
    }

    #[test]
    fn outbound_packet_rejects_local_source_spoofing() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let mut queues = PeerQueues::new(1, 1280);
        let packet = ipv4_packet(Ipv4Addr::new(198, 51, 100, 1), builtin_ipv4(remote_overlay));

        assert!(matches!(
            forwarder.enqueue_tun_packet(&mut queues, packet),
            Err(ForwardError::UnauthorizedLocalSource {
                source: IpAddr::V4(source)
            }) if source == Ipv4Addr::new(198, 51, 100, 1)
        ));
    }

    #[test]
    fn forwarder_uses_effective_packet_mtu() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut config = config_for(remote);
        config.interface.mtu = u16::MAX;
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert_eq!(forwarder.mtu(), usize::from(config.effective_packet_mtu()));
    }

    #[test]
    fn inbound_packet_rejects_payload_length_mismatch() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let mut frame = Frame::packet(0, 1, vec![0x45; 20]).expect("frame");
        frame.header = Header::new(PayloadType::IpPacket, 0, 1, 19);

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::PayloadLengthMismatch { .. })
        ));
    }

    #[test]
    fn inbound_packet_rejects_unknown_peer() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let other = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let packet = ipv6_packet(builtin_ipv6(remote_overlay), Ipv6Addr::LOCALHOST);
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_packet(other, &frame),
            Err(ForwardError::UnauthorizedPeer(peer)) if peer == other
        ));
    }
}
