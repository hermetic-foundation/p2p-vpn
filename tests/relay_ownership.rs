//! Real TCP/QUIC relay ownership; no production service or device changes.
use std::time::Duration;

use futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder,
    core::ConnectedPoint,
    multiaddr::Protocol,
    noise, relay, request_response,
    swarm::{
        ConnectionId, NetworkBehaviour, SwarmEvent,
        behaviour::toggle::Toggle,
        dial_opts::{DialOpts, PeerCondition},
    },
    tcp, yamux,
};
use p2p_vpn::{
    runtime::packet::{PacketCodec, PacketResponse},
    wire::Frame,
};

const DEADLINE: Duration = Duration::from_secs(15);

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay: relay::client::Behaviour,
    server: Toggle<relay::Behaviour>,
    packet: request_response::Behaviour<PacketCodec>,
}

fn host(server: bool) -> Swarm<Behaviour> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_quic()
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .unwrap()
        .with_behaviour(|key, relay| Behaviour {
            relay,
            server: server
                .then(|| {
                    let config = relay::Config {
                        reservation_duration: Duration::from_secs(8),
                        ..relay::Config::default()
                    };
                    relay::Behaviour::new(key.public().to_peer_id(), config)
                })
                .into(),
            packet: request_response::Behaviour::with_codec(
                PacketCodec::new(1280),
                [(
                    StreamProtocol::new("/p2p-vpn/relay-ownership-test/1"),
                    request_response::ProtocolSupport::Full,
                )],
                request_response::Config::default()
                    .with_request_timeout(DEADLINE)
                    .with_max_concurrent_streams(4),
            ),
        })
        .unwrap()
        .with_swarm_config(|config| config.with_idle_connection_timeout(Duration::from_mins(1)))
        .build()
}

async fn server() -> (Swarm<Behaviour>, Multiaddr, Multiaddr) {
    let mut server = host(true);
    server
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    server
        .listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
        .unwrap();
    let mut tcp = None;
    let mut quic = None;
    tokio::time::timeout(DEADLINE, async {
        while tcp.is_none() || quic.is_none() {
            if let SwarmEvent::NewListenAddr { address, .. } = server.select_next_some().await {
                let address = address.with(Protocol::P2p(*server.local_peer_id()));
                server.add_external_address(address.clone());
                if address.iter().any(|p| matches!(p, Protocol::Tcp(_))) {
                    tcp = Some(address);
                } else if address.iter().any(|p| p == Protocol::QuicV1) {
                    quic = Some(address);
                }
            }
        }
    })
    .await
    .expect("server listen deadline");
    (server, tcp.unwrap(), quic.unwrap())
}

async fn connect(
    client: &mut Swarm<Behaviour>,
    server: &mut Swarm<Behaviour>,
    address: Multiaddr,
) -> ConnectionId {
    let peer = *server.local_peer_id();
    let opts = DialOpts::peer_id(peer)
        .condition(PeerCondition::Always)
        .addresses(vec![address.clone()])
        .build();
    let expected = opts.connection_id();
    client.dial(opts).unwrap();
    tokio::time::timeout(DEADLINE, async {
        loop {
            tokio::select! {
                _ = server.select_next_some() => {},
                event = client.select_next_some() => match event {
                    SwarmEvent::ConnectionEstablished {peer_id, connection_id, endpoint, ..}
                        if connection_id == expected => {
                        assert_eq!(peer_id, peer);
                        assert!(matches!(endpoint, ConnectedPoint::Dialer {address: actual, ..} if actual == address));
                        return connection_id;
                    }
                    SwarmEvent::OutgoingConnectionError {connection_id, error, ..}
                        if connection_id == expected => panic!("dial failed: {error}"),
                    _ => {}
                }
            }
        }
    }).await.expect("direct connection deadline")
}

fn owners(client: &Swarm<Behaviour>, peer: PeerId) -> Vec<ConnectionId> {
    client
        .behaviour()
        .relay
        .confirmed_reservation_connections(peer)
        .collect()
}

async fn await_reservation(
    client: &mut Swarm<Behaviour>,
    server: &mut Swarm<Behaviour>,
    connections: [ConnectionId; 2],
    renewal: bool,
) {
    let peer = *server.local_peer_id();
    let mut listening = renewal;
    let mut accepted = false;
    tokio::time::timeout(DEADLINE, async {
        while !listening || !accepted {
            tokio::select! {
                _ = server.select_next_some() => {},
                event = client.select_next_some() => match event {
                    SwarmEvent::NewListenAddr {address, ..}
                        if address.iter().any(|p| p == Protocol::P2pCircuit)
                            && address.iter().any(|p| matches!(p, Protocol::Tcp(_))) => {
                        listening = true;
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Relay(relay::client::Event::ReservationReqAccepted {
                        relay_peer_id, renewal: actual, ..
                    })) if actual == renewal => {
                        assert_eq!(relay_peer_id, peer);
                        accepted = true;
                    }
                    SwarmEvent::ConnectionClosed {connection_id, ..} if connections.contains(&connection_id) => {
                        panic!("reservation selection or renewal closed an existing direct connection");
                    }
                    _ => {}
                }
            }
        }
    }).await.expect("reservation/renewal deadline");
}

async fn fresh_incoming(
    receiver: &mut Swarm<Behaviour>,
    server: &mut Swarm<Behaviour>,
    relay_address: Multiaddr,
) {
    let mut sender = host(false);
    let sender_peer = *sender.local_peer_id();
    let receiver_peer = *receiver.local_peer_id();
    let frame = Frame::packet(1, 41, vec![0x45; 20]).unwrap();
    let mut request = None;
    let mut seen = false;
    let mut receiver_relayed = false;
    sender
        .dial(
            relay_address
                .with(Protocol::P2pCircuit)
                .with(Protocol::P2p(receiver_peer)),
        )
        .unwrap();
    tokio::time::timeout(DEADLINE, async {
        loop {
            tokio::select! {
                _ = server.select_next_some() => {},
                event = receiver.select_next_some() => match event {
                    SwarmEvent::ConnectionEstablished {peer_id, connection_id, endpoint, ..} if peer_id == sender_peer => {
                        assert!(endpoint.is_relayed());
                        receiver_relayed = true;
                        assert!(!receiver.behaviour_mut().relay.prefer_relay_connection(sender_peer, connection_id),
                            "inner circuit cannot be a direct relay preference");
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
                        peer, message: request_response::Message::Request {request: incoming, channel, ..}, ..
                    })) => {
                        assert_eq!(peer, sender_peer);
                        assert_eq!(incoming, frame);
                        seen = true;
                        receiver.behaviour_mut().packet.send_response(channel, PacketResponse::Accepted).unwrap();
                    }
                    _ => {}
                },
                event = sender.select_next_some() => match event {
                    SwarmEvent::ConnectionEstablished {peer_id, endpoint, ..} if peer_id == receiver_peer => {
                        assert!(endpoint.is_relayed());
                        request = Some(sender.behaviour_mut().packet.send_request(&receiver_peer, frame.clone()));
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
                        peer, message: request_response::Message::Response {request_id, response}, ..
                    })) => {
                        assert_eq!(peer, receiver_peer);
                        assert_eq!(Some(request_id), request);
                        assert_eq!(response, PacketResponse::Accepted);
                        assert!(seen && receiver_relayed);
                        return;
                    }
                    SwarmEvent::OutgoingConnectionError {error, ..} => panic!("circuit dial failed: {error}"),
                    SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::OutboundFailure {error, ..})) => {
                        panic!("incoming packet failed: {error}");
                    }
                    _ => {}
                }
            }
        }
    }).await.expect("fresh incoming packet deadline");
}

#[tokio::test]
async fn tcp_preference_owns_reservation_renewal_and_fresh_incoming() {
    scenario(true).await;
}

#[tokio::test]
async fn default_first_connection_and_existing_reservation_ownership_are_preserved() {
    scenario(false).await;
}

async fn scenario(prefer_tcp: bool) {
    let (mut server, tcp_address, quic_address) = server().await;
    let peer = *server.local_peer_id();
    let mut client = host(false);
    assert!(owners(&client, peer).is_empty());
    let quic = connect(&mut client, &mut server, quic_address).await;
    let tcp = connect(&mut client, &mut server, tcp_address.clone()).await;
    let unknown = DialOpts::peer_id(peer).build().connection_id();
    assert!(
        !client
            .behaviour_mut()
            .relay
            .prefer_relay_connection(PeerId::random(), tcp)
    );
    assert!(
        !client
            .behaviour_mut()
            .relay
            .prefer_relay_connection(peer, unknown)
    );
    if prefer_tcp {
        assert!(
            client
                .behaviour_mut()
                .relay
                .prefer_relay_connection(peer, tcp)
        );
    }
    let owner = if prefer_tcp { tcp } else { quic };
    let other = if prefer_tcp { quic } else { tcp };
    client
        .listen_on(tcp_address.clone().with(Protocol::P2pCircuit))
        .unwrap();
    assert!(owners(&client, peer).is_empty(), "request is not confirmed");
    await_reservation(&mut client, &mut server, [tcp, quic], false).await;
    assert_eq!(owners(&client, peer), [owner]);
    assert!(owners(&client, PeerId::random()).is_empty());
    assert!(
        client
            .behaviour_mut()
            .relay
            .prefer_relay_connection(peer, other)
    );
    assert_eq!(
        owners(&client, peer),
        [owner],
        "preference cannot migrate a lease"
    );

    await_reservation(&mut client, &mut server, [tcp, quic], true).await;
    assert_eq!(
        owners(&client, peer),
        [owner],
        "native renewal keeps ownership"
    );
    fresh_incoming(&mut client, &mut server, tcp_address).await;
    assert!(client.close_connection(owner));
    tokio::time::timeout(DEADLINE, async {
        loop {
            tokio::select! {
                _ = server.select_next_some() => {},
                event = client.select_next_some() => {
                    if let SwarmEvent::ConnectionClosed {connection_id, ..} = event
                        && connection_id == owner
                    {
                        break;
                    }
                }
            }
        }
    })
    .await
    .expect("owner closure deadline");
    assert!(owners(&client, peer).is_empty());
    assert!(
        !client
            .behaviour_mut()
            .relay
            .prefer_relay_connection(peer, owner)
    );
    assert!(
        client
            .behaviour_mut()
            .relay
            .prefer_relay_connection(peer, other)
    );
    assert!(client.is_connected(&peer));
}
