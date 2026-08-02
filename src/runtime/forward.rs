use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use libp2p::{PeerId as Libp2pPeerId, Swarm, request_response};

use crate::{
    PeerId, Sequence, SessionId,
    config::{Config, ConfigError},
    route::{RouteError, RouteTable},
    runtime::{
        p2p::Behaviour,
        packet::{AuthorizedPeers, PacketResponse},
    },
    wire::{Frame, FrameError, MAX_PAYLOAD_LEN, PayloadType},
};

#[derive(Debug)]
pub struct Forwarder {
    routes: RouteTable,
    peers: HashMap<PeerId, Libp2pPeerId>,
    authorized_peers: AuthorizedPeers,
    session_id: SessionId,
    next_sequence: Sequence,
    mtu: usize,
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

        Ok(Self {
            routes: config.compile_routes()?,
            peers,
            authorized_peers: AuthorizedPeers::from_config(config),
            session_id: 0,
            next_sequence: 0,
            mtu: usize::from(config.interface.mtu).min(MAX_PAYLOAD_LEN),
        })
    }

    pub fn send_tun_packet(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        packet: Vec<u8>,
    ) -> Result<request_response::OutboundRequestId, ForwardError> {
        if packet.len() > self.mtu {
            return Err(ForwardError::PacketTooLarge {
                actual: packet.len(),
                max: self.mtu,
            });
        }

        let destination = packet_destination(&packet)?;
        let route = self
            .routes
            .resolve(destination)
            .ok_or(ForwardError::NoRoute(destination))?;
        let peer = self
            .peers
            .get(&route.owner)
            .ok_or(ForwardError::NoTransportPeer(route.owner))?;
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let frame = Frame::packet(self.session_id, sequence, packet)?;

        Ok(swarm.behaviour_mut().packet.send_request(peer, frame))
    }

    pub fn accept_inbound_packet<'a>(
        &self,
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

        let source = packet_source(&frame.payload)?;
        self.routes
            .authorize_source(PeerId::from_libp2p(peer), source)?;

        Ok(&frame.payload)
    }

    pub fn send_packet_response(
        swarm: &mut Swarm<Behaviour>,
        channel: request_response::ResponseChannel<PacketResponse>,
    ) -> Result<(), PacketResponse> {
        swarm
            .behaviour_mut()
            .packet
            .send_response(channel, PacketResponse::Accepted)
    }
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
    NoRoute(IpAddr),
    NoTransportPeer(PeerId),
    PacketTooLarge { actual: usize, max: usize },
    TruncatedIpPacket { actual: usize, expected: usize },
    UnsupportedIpVersion(u8),
    UnauthorizedPeer(Libp2pPeerId),
    UnexpectedPayload(PayloadType),
    PayloadLengthMismatch { header: u16, actual: usize },
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

#[cfg(test)]
mod tests {
    use libp2p::identity::Keypair;

    use crate::{
        config::{InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig},
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
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: Some("remote".to_owned()),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
            },
        }
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
        let forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), Ipv4Addr::new(100, 64, 9, 9));
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
        let forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let packet = ipv4_packet(Ipv4Addr::new(198, 51, 100, 1), Ipv4Addr::new(100, 64, 9, 9));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::Route(RouteError::UnauthorizedSource { .. }))
        ));
    }

    #[tokio::test]
    async fn outbound_packet_resolves_destination_to_libp2p_peer() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut config = config_for(remote);
        config.network.local_peer = local_identity.peer_id.clone();
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut node = build_node(HostConfig {
            identity: local_identity,
            mtu: 1280,
            listen_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
        })
        .expect("node");
        let packet = ipv4_packet(Ipv4Addr::new(100, 64, 9, 9), builtin_ipv4(remote_overlay));

        let request_id = forwarder
            .send_tun_packet(&mut node.swarm, packet)
            .expect("request id");

        assert_ne!(format!("{request_id:?}"), "");
    }

    #[test]
    fn inbound_packet_rejects_payload_length_mismatch() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
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
        let forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let packet = ipv6_packet(builtin_ipv6(remote_overlay), Ipv6Addr::LOCALHOST);
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_packet(other, &frame),
            Err(ForwardError::UnauthorizedPeer(peer)) if peer == other
        ));
    }
}
