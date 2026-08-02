use std::error::Error;

use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder, dcutr, identify, identity::Keypair, kad, mdns, noise,
    ping, relay, request_response, swarm::NetworkBehaviour, tcp, yamux,
};

use crate::{
    identity::{IdentityError, NodeIdentity},
    runtime::packet::{self, PacketCodec},
};

const PROTOCOL_VERSION: &str = "/p2p-vpn/0.1.0";

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub kad: kad::Behaviour<kad::store::MemoryStore>,
    pub relay: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
    pub mdns: mdns::tokio::Behaviour,
    pub packet: request_response::Behaviour<PacketCodec>,
}

pub struct P2pNode {
    pub local_peer_id: PeerId,
    pub swarm: Swarm<Behaviour>,
}

pub struct HostConfig {
    pub identity: NodeIdentity,
    pub mtu: u16,
    pub listen_addresses: Vec<Multiaddr>,
    pub bootstrap_peers: Vec<(PeerId, Multiaddr)>,
}

pub fn build_node(config: HostConfig) -> Result<P2pNode, P2pBuildError> {
    let keypair = decode_keypair(&config.identity.private_key)?;
    let local_peer_id = keypair.public().to_peer_id();

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
                kad.set_mode(Some(kad::Mode::Client));

                Ok(Behaviour {
                    identify: identify::Behaviour::new(identify::Config::new(
                        PROTOCOL_VERSION.to_owned(),
                        keypair.public(),
                    )),
                    ping: ping::Behaviour::default(),
                    kad,
                    relay,
                    dcutr: dcutr::Behaviour::new(local_peer_id),
                    mdns: mdns::tokio::Behaviour::new(mdns::Config::default(), local_peer_id)?,
                    packet: packet::behaviour(config.mtu),
                })
            },
        )?
        .build();

    for address in config.listen_addresses {
        swarm.listen_on(address)?;
    }

    for (peer, address) in config.bootstrap_peers {
        swarm
            .behaviour_mut()
            .kad
            .add_address(&peer, address.clone());
        let dial_address = address
            .with_p2p(peer)
            .map_err(|address| P2pBuildError::InvalidP2pAddress(Box::new(address)))?;
        swarm.dial(dial_address)?;
    }

    Ok(P2pNode {
        local_peer_id,
        swarm,
    })
}

fn decode_keypair(encoded: &str) -> Result<Keypair, IdentityError> {
    let identity = NodeIdentity::from_private_key(encoded)?;
    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        identity.private_key,
    )?;
    Ok(Keypair::from_protobuf_encoding(&bytes)?)
}

#[derive(Debug)]
pub enum P2pBuildError {
    Identity(IdentityError),
    Noise(libp2p::noise::Error),
    Transport(libp2p::TransportError<std::io::Error>),
    Behaviour(libp2p::BehaviourBuilderError),
    Listen(libp2p::TransportError<std::io::Error>),
    Dial(libp2p::swarm::DialError),
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

impl From<libp2p::multiaddr::Error> for P2pBuildError {
    fn from(error: libp2p::multiaddr::Error) -> Self {
        Self::Multiaddr(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn build_node_uses_configured_identity() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let expected_peer_id = identity.peer_id.parse::<PeerId>().expect("peer id");

        let node = build_node(HostConfig {
            identity,
            mtu: 1280,
            listen_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
        })
        .expect("node should build");

        assert_eq!(node.local_peer_id, expected_peer_id);
    }
}
