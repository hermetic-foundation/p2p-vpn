use std::{sync::Arc, time::Duration};

use futures::StreamExt as _;
use libp2p::{
    Multiaddr, PeerId as Libp2pPeerId, Swarm, dcutr, identify, kad, mdns, relay,
    request_response::{self, Message},
    swarm::SwarmEvent,
};
use tokio::sync::mpsc;

use crate::{
    config::{Config, DiscoveryConfig, QueueConfig},
    metrics::RuntimeMetrics,
    queue::PeerQueues,
    runtime::{
        forward::{ForwardError, Forwarder},
        p2p::{Behaviour, BehaviourEvent, HostConfig, P2pBuildError, P2pNode, build_node},
        tun::{TunDevice, TunReader, TunRuntimeError, TunWriter},
    },
};

const TUN_READ_CHANNEL: usize = 1024;

pub async fn run_config(
    config: Config,
    device: TunDevice,
    metrics_interval: Option<Duration>,
) -> Result<(), RunnerError> {
    let identity = config.identity()?;
    let node = build_node(HostConfig {
        identity,
        mtu: config.interface.mtu,
        listen_addresses: config.listen_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
        relay_reservations: config.relay_reservation_multiaddrs()?,
        relay_server: config.network.relay.server,
        discovery: config.network.discovery,
    })?;
    let forwarder = Forwarder::from_config(&config)?;

    run_node(
        node,
        forwarder,
        device,
        config.interface.mtu,
        config.queue,
        metrics_interval,
    )
    .await
}

pub async fn run_node(
    mut node: P2pNode,
    mut forwarder: Forwarder,
    device: TunDevice,
    mtu: u16,
    queue_config: QueueConfig,
    metrics_interval: Option<Duration>,
) -> Result<(), RunnerError> {
    let (reader, mut writer) = device.split();
    let metrics = Arc::new(RuntimeMetrics::default());
    let mut tun_rx = spawn_tun_reader(reader, Arc::clone(&metrics), mtu);
    let mut queues = PeerQueues::new(
        queue_config.max_packets_per_peer,
        queue_config.max_bytes_per_peer,
    );
    let mut metrics_tick = metrics_interval.map(tokio::time::interval);
    let discovery = node.discovery;

    if node.startup.kad_bootstrap_started {
        eprintln!("kademlia bootstrap started");
    }
    if node.startup.relay_reservations_started > 0 {
        eprintln!(
            "relay reservation listeners started: {}",
            node.startup.relay_reservations_started
        );
    }
    if node.startup.relay_server_enabled {
        eprintln!("relay server enabled");
    }

    loop {
        tokio::select! {
            Some(packet) = tun_rx.recv() => {
                if let Err(error) = forwarder.enqueue_tun_packet(&mut queues, packet) {
                    metrics.record_outbound_drop();
                    eprintln!("dropping outbound packet: {error:?}");
                }
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &metrics);
            }
            event = node.swarm.select_next_some() => {
                handle_swarm_event(&mut node.swarm, &forwarder, &mut writer, &metrics, discovery, event)?;
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &metrics);
            }
            () = async {
                metrics_tick
                    .as_mut()
                    .expect("metrics interval is present")
                    .tick()
                    .await;
            }, if metrics_tick.is_some() => {
                print_metrics(&metrics, queues.total_stats());
            }
        }
    }
}

fn spawn_tun_reader(
    mut reader: TunReader,
    metrics: Arc<RuntimeMetrics>,
    mtu: u16,
) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel(TUN_READ_CHANNEL);
    std::thread::spawn(move || {
        let mut buffer = vec![0; usize::from(mtu)];
        loop {
            match reader.read_packet(&mut buffer) {
                Ok(length) => {
                    metrics.record_tun_read(length);
                    if tx.blocking_send(buffer[..length].to_vec()).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    eprintln!("failed to read TUN packet: {error:?}");
                    return;
                }
            }
        }
    });
    rx
}

fn drain_outbound_queue(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    queues: &mut PeerQueues,
    metrics: &RuntimeMetrics,
) {
    while let Some(packet) = queues.dequeue() {
        if let Err(error) = forwarder.send_queued_packet(swarm, &packet) {
            metrics.record_outbound_drop();
            eprintln!("dropping queued outbound packet: {error:?}");
        } else {
            metrics.record_outbound_sent();
        }
    }
}

fn handle_swarm_event(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    writer: &mut TunWriter,
    metrics: &RuntimeMetrics,
    discovery: DiscoveryConfig,
    event: SwarmEvent<BehaviourEvent>,
) -> Result<(), RunnerError> {
    match event {
        SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        })) => match forwarder.accept_inbound_packet(peer, &request) {
            Ok(packet) => {
                writer.write_packet(packet)?;
                metrics.record_tun_write(packet.len());
                metrics.record_inbound_accepted();
                Forwarder::send_packet_response(swarm, channel)
                    .map_err(|_| RunnerError::PacketResponseDropped)?;
            }
            Err(error) => {
                metrics.record_inbound_drop();
                eprintln!("dropping inbound packet from {peer}: {error:?}");
            }
        },
        SwarmEvent::Behaviour(BehaviourEvent::Packet(
            request_response::Event::OutboundFailure { peer, error, .. },
        )) => {
            metrics.record_outbound_failure();
            eprintln!("packet request to {peer} failed: {error}");
        }
        SwarmEvent::Behaviour(BehaviourEvent::Packet(
            request_response::Event::InboundFailure { peer, error, .. },
        )) => {
            metrics.record_inbound_failure();
            eprintln!("packet request from {peer} failed: {error}");
        }
        SwarmEvent::Behaviour(event) => handle_behaviour_event(swarm, discovery, event),
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => {
            eprintln!("connection established with {peer_id} via {endpoint:?}");
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => match peer_id {
            Some(peer_id) => eprintln!("outgoing connection to {peer_id} failed: {error}"),
            None => eprintln!("outgoing connection failed: {error}"),
        },
        _ => {}
    }

    Ok(())
}

fn handle_behaviour_event(
    swarm: &mut Swarm<Behaviour>,
    discovery: DiscoveryConfig,
    event: BehaviourEvent,
) {
    match event {
        BehaviourEvent::Mdns(mdns::Event::Discovered(peers)) if discovery.mdns => {
            for (peer, address) in peers {
                learn_peer_address(swarm, peer, address, discovery.kademlia);
            }
        }
        BehaviourEvent::Mdns(mdns::Event::Expired(peers))
            if discovery.mdns && discovery.kademlia =>
        {
            for (peer, address) in peers {
                swarm.behaviour_mut().kad.remove_address(&peer, &address);
            }
        }
        BehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. }) => {
            for address in info.listen_addrs {
                learn_peer_address(swarm, peer_id, address, discovery.kademlia);
            }
        }
        BehaviourEvent::Identify(identify::Event::Error { peer_id, error, .. }) => {
            eprintln!("identify with {peer_id} failed: {error}");
        }
        BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed { result, .. })
            if discovery.kademlia =>
        {
            eprintln!("kademlia query progressed: {result:?}");
        }
        BehaviourEvent::Relay(event) => handle_relay_event(&event),
        BehaviourEvent::RelayServer(event) => handle_relay_server_event(&event),
        BehaviourEvent::Dcutr(dcutr::Event {
            remote_peer_id,
            result,
        }) if discovery.dcutr => {
            eprintln!("dcutr hole-punch result with {remote_peer_id}: {result:?}");
        }
        _ => {}
    }
}

fn handle_relay_server_event(event: &relay::Event) {
    match event {
        relay::Event::ReservationReqAccepted {
            src_peer_id,
            renewed,
        } => {
            eprintln!("relay server accepted reservation from {src_peer_id} renewed={renewed}");
        }
        relay::Event::ReservationReqDenied {
            src_peer_id,
            status,
        } => {
            eprintln!("relay server denied reservation from {src_peer_id}: {status:?}");
        }
        relay::Event::ReservationClosed { src_peer_id } => {
            eprintln!("relay server reservation closed for {src_peer_id}");
        }
        relay::Event::ReservationTimedOut { src_peer_id } => {
            eprintln!("relay server reservation timed out for {src_peer_id}");
        }
        relay::Event::CircuitReqDenied {
            src_peer_id,
            dst_peer_id,
            status,
        } => {
            eprintln!("relay server denied circuit {src_peer_id} -> {dst_peer_id}: {status:?}");
        }
        relay::Event::CircuitReqAccepted {
            src_peer_id,
            dst_peer_id,
        } => {
            eprintln!("relay server accepted circuit {src_peer_id} -> {dst_peer_id}");
        }
        relay::Event::CircuitClosed {
            src_peer_id,
            dst_peer_id,
            error,
        } => {
            eprintln!("relay server circuit closed {src_peer_id} -> {dst_peer_id}: {error:?}");
        }
        _ => {}
    }
}

fn handle_relay_event(event: &relay::client::Event) {
    match event {
        relay::client::Event::ReservationReqAccepted {
            relay_peer_id,
            renewal,
            ..
        } => {
            eprintln!("relay reservation accepted by {relay_peer_id} renewal={renewal}");
        }
        relay::client::Event::OutboundCircuitEstablished { relay_peer_id, .. } => {
            eprintln!("outbound relay circuit established via {relay_peer_id}");
        }
        relay::client::Event::InboundCircuitEstablished { src_peer_id, .. } => {
            eprintln!("inbound relay circuit established from {src_peer_id}");
        }
    }
}

fn learn_peer_address(
    swarm: &mut Swarm<Behaviour>,
    peer: Libp2pPeerId,
    address: Multiaddr,
    add_to_kademlia: bool,
) {
    if peer == *swarm.local_peer_id() {
        return;
    }

    if add_to_kademlia {
        swarm
            .behaviour_mut()
            .kad
            .add_address(&peer, address.clone());
    }

    if swarm.is_connected(&peer) {
        return;
    }

    let dial_address = peer_dial_address(peer, address);
    if let Err(error) = swarm.dial(dial_address) {
        eprintln!("dial discovered peer {peer} failed: {error}");
    }
}

fn peer_dial_address(peer: Libp2pPeerId, address: Multiaddr) -> Multiaddr {
    address.with_p2p(peer).unwrap_or_else(|address| address)
}

fn print_metrics(metrics: &RuntimeMetrics, queue: crate::queue::QueueStats) {
    eprintln!("metrics:");
    for line in metrics.snapshot(queue).lines() {
        eprintln!("  {line}");
    }
}

#[derive(Debug)]
pub enum RunnerError {
    Config(crate::config::ConfigError),
    P2p(P2pBuildError),
    Forward(ForwardError),
    Tun(TunRuntimeError),
    PacketResponseDropped,
}

impl From<crate::config::ConfigError> for RunnerError {
    fn from(error: crate::config::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<P2pBuildError> for RunnerError {
    fn from(error: P2pBuildError) -> Self {
        Self::P2p(error)
    }
}

impl From<ForwardError> for RunnerError {
    fn from(error: ForwardError) -> Self {
        Self::Forward(error)
    }
}

impl From<TunRuntimeError> for RunnerError {
    fn from(error: TunRuntimeError) -> Self {
        Self::Tun(error)
    }
}

#[cfg(test)]
mod tests {
    use libp2p::identity::Keypair;

    use super::*;

    #[test]
    fn peer_dial_address_appends_p2p_component() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        let dial = peer_dial_address(peer, address);

        assert!(dial.to_string().ends_with(&format!("/p2p/{peer}")));
    }

    #[test]
    fn peer_dial_address_preserves_existing_p2p_address() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("address");

        assert_eq!(peer_dial_address(peer, address.clone()), address);
    }
}
