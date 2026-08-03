use std::{sync::Arc, time::Duration};

use futures::StreamExt as _;
use libp2p::{
    Multiaddr, PeerId as Libp2pPeerId, Swarm,
    core::ConnectedPoint,
    dcutr, identify, kad, mdns,
    multiaddr::Protocol,
    relay,
    request_response::{self, Message},
    swarm::SwarmEvent,
};
use tokio::sync::mpsc;

use crate::{
    PathKind, PeerId,
    config::{Config, DiscoveryConfig, QueueConfig},
    metrics::RuntimeMetrics,
    path::PathSet,
    queue::PeerQueues,
    runtime::{
        forward::{ForwardError, Forwarder},
        p2p::{Behaviour, BehaviourEvent, HostConfig, P2pBuildError, P2pNode, build_node},
        tun::{TunDevice, TunReader, TunRuntimeError, TunWriter},
    },
};

const TUN_READ_CHANNEL: usize = 1024;
const REDIAL_INTERVAL: Duration = Duration::from_secs(10);

pub async fn run_config(
    config: Config,
    device: TunDevice,
    metrics_interval: Option<Duration>,
) -> Result<(), RunnerError> {
    let identity = config.identity()?;
    let node = build_node(HostConfig {
        identity,
        network_name: config.network.name.clone(),
        mtu: config.effective_packet_mtu(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
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
        config.effective_packet_mtu(),
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
    let mut paths = PathSet::new();
    let mut metrics_tick = metrics_interval.map(tokio::time::interval);
    let mut redial_tick = tokio::time::interval(REDIAL_INTERVAL);
    redial_tick.tick().await;
    let discovery = node.discovery;

    if node.startup.mdns_enabled {
        eprintln!("mdns discovery enabled");
    }
    if node.startup.dcutr_enabled {
        eprintln!("dcutr hole punching enabled");
    }
    if node.startup.kademlia.bootstrap_started {
        eprintln!("kademlia bootstrap started");
    }
    if node.startup.kademlia.rendezvous_advertise_started {
        eprintln!("kademlia overlay provider advertisement started");
    }
    if node.startup.kademlia.rendezvous_lookup_started {
        eprintln!("kademlia overlay provider lookup started");
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
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &paths, &metrics);
            }
            event = node.swarm.select_next_some() => {
                handle_swarm_event(&mut node.swarm, &forwarder, &mut writer, &mut paths, &metrics, discovery, event)?;
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &paths, &metrics);
            }
            _ = redial_tick.tick() => {
                redial_configured_addresses(&mut node.swarm, &node.bootstrap_peer_addresses, &node.configured_peer_addresses, &metrics);
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

fn redial_configured_addresses(
    swarm: &mut Swarm<Behaviour>,
    bootstrap_addresses: &[(Libp2pPeerId, Multiaddr)],
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    metrics: &RuntimeMetrics,
) {
    let local_peer = *swarm.local_peer_id();
    let targets = pending_redial_targets(
        local_peer,
        bootstrap_addresses,
        configured_peer_addresses,
        |peer| swarm.is_connected(peer),
    );

    for _ in 0..targets.skipped_connected {
        metrics.record_redial_skipped_connected();
    }

    for (peer, address) in targets.addresses {
        metrics.record_redial_attempt();
        let dial_address = peer_dial_address(peer, address);
        if let Err(error) = swarm.dial(dial_address) {
            metrics.record_redial_failure();
            eprintln!("redial {peer} failed: {error}");
        }
    }
}

fn pending_redial_targets(
    local_peer: Libp2pPeerId,
    bootstrap_addresses: &[(Libp2pPeerId, Multiaddr)],
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    mut is_connected: impl FnMut(&Libp2pPeerId) -> bool,
) -> RedialTargets {
    let mut addresses = Vec::new();
    let mut skipped_connected = 0;
    for (peer, address) in bootstrap_addresses
        .iter()
        .chain(configured_peer_addresses.iter())
    {
        if *peer == local_peer {
            continue;
        }
        if is_connected(peer) {
            skipped_connected += 1;
            continue;
        }
        addresses.push((*peer, address.clone()));
    }
    RedialTargets {
        addresses,
        skipped_connected,
    }
}

#[derive(Debug, Eq, PartialEq)]
struct RedialTargets {
    addresses: Vec<(Libp2pPeerId, Multiaddr)>,
    skipped_connected: usize,
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
    paths: &PathSet,
    metrics: &RuntimeMetrics,
) {
    while let Some(packet) = queues.dequeue_ready(|peer| paths.has_healthy_path(peer)) {
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
    paths: &mut PathSet,
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
        SwarmEvent::Behaviour(event) => {
            handle_behaviour_event(swarm, forwarder, metrics, discovery, event);
        }
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => {
            record_path_established(paths, forwarder, peer_id, &endpoint);
            metrics.record_connection_established(endpoint.is_relayed());
            eprintln!("connection established with {peer_id} via {endpoint:?}");
        }
        SwarmEvent::ConnectionClosed {
            peer_id, endpoint, ..
        } => {
            record_path_closed(paths, forwarder, peer_id, &endpoint);
            eprintln!("connection closed with {peer_id} via {endpoint:?}");
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => match peer_id {
            Some(peer_id) => eprintln!("outgoing connection to {peer_id} failed: {error}"),
            None => eprintln!("outgoing connection failed: {error}"),
        },
        _ => {}
    }

    Ok(())
}

fn record_path_established(
    paths: &mut PathSet,
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    endpoint: &ConnectedPoint,
) {
    if !forwarder.is_configured_transport_peer(peer) {
        return;
    }

    paths.record_established(PeerId::from_libp2p(peer), path_kind_for_endpoint(endpoint));
}

fn record_path_closed(
    paths: &mut PathSet,
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    endpoint: &ConnectedPoint,
) {
    if !forwarder.is_configured_transport_peer(peer) {
        return;
    }

    paths.record_closed(PeerId::from_libp2p(peer), path_kind_for_endpoint(endpoint));
}

fn path_kind_for_endpoint(endpoint: &ConnectedPoint) -> PathKind {
    if endpoint.is_relayed() {
        return PathKind::CircuitRelay;
    }

    let address = match endpoint {
        ConnectedPoint::Dialer { address, .. } => address,
        ConnectedPoint::Listener { local_addr, .. } => local_addr,
    };

    if address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::Quic | Protocol::QuicV1))
    {
        PathKind::DirectQuicStream
    } else {
        PathKind::DirectTcpStream
    }
}

fn handle_behaviour_event(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    metrics: &RuntimeMetrics,
    discovery: DiscoveryConfig,
    event: BehaviourEvent,
) {
    match event {
        BehaviourEvent::Mdns(mdns::Event::Discovered(peers)) if discovery.mdns => {
            for (peer, address) in peers {
                learn_peer_address(swarm, forwarder, peer, address, discovery.kademlia);
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
                learn_peer_address(swarm, forwarder, peer_id, address, discovery.kademlia);
            }
        }
        BehaviourEvent::Identify(identify::Event::Error { peer_id, error, .. }) => {
            eprintln!("identify with {peer_id} failed: {error}");
        }
        BehaviourEvent::Kad(event) if discovery.kademlia => {
            handle_kademlia_event(swarm, forwarder, event);
        }
        BehaviourEvent::Relay(event) => handle_relay_event(metrics, &event),
        BehaviourEvent::RelayServer(event) => handle_relay_server_event(metrics, &event),
        BehaviourEvent::Dcutr(dcutr::Event {
            remote_peer_id,
            result,
        }) if discovery.dcutr => {
            metrics.record_dcutr_result(result.is_ok());
            eprintln!("dcutr hole-punch result with {remote_peer_id}: {result:?}");
        }
        _ => {}
    }
}

fn handle_kademlia_event(swarm: &mut Swarm<Behaviour>, forwarder: &Forwarder, event: kad::Event) {
    match event {
        kad::Event::OutboundQueryProgressed { result, .. } => {
            if let kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                providers,
                ..
            })) = &result
            {
                for provider in providers {
                    dial_configured_peer(swarm, forwarder, *provider);
                }
            }
            eprintln!("kademlia query progressed: {result:?}");
        }
        other => {
            eprintln!("kademlia event: {other:?}");
        }
    }
}

fn handle_relay_server_event(metrics: &RuntimeMetrics, event: &relay::Event) {
    match event {
        relay::Event::ReservationReqAccepted {
            src_peer_id,
            renewed,
        } => {
            metrics.record_relay_server_reservation_accepted();
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
            metrics.record_relay_server_circuit_accepted();
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

fn handle_relay_event(metrics: &RuntimeMetrics, event: &relay::client::Event) {
    match event {
        relay::client::Event::ReservationReqAccepted {
            relay_peer_id,
            renewal,
            ..
        } => {
            metrics.record_relay_reservation_accepted();
            eprintln!("relay reservation accepted by {relay_peer_id} renewal={renewal}");
        }
        relay::client::Event::OutboundCircuitEstablished { relay_peer_id, .. } => {
            metrics.record_relay_outbound_circuit_established();
            eprintln!("outbound relay circuit established via {relay_peer_id}");
        }
        relay::client::Event::InboundCircuitEstablished { src_peer_id, .. } => {
            metrics.record_relay_inbound_circuit_established();
            eprintln!("inbound relay circuit established from {src_peer_id}");
        }
    }
}

fn learn_peer_address(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    address: Multiaddr,
    add_to_kademlia: bool,
) {
    if peer == *swarm.local_peer_id() {
        return;
    }
    if !forwarder.is_configured_transport_peer(peer) {
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

fn dial_configured_peer(swarm: &mut Swarm<Behaviour>, forwarder: &Forwarder, peer: Libp2pPeerId) {
    if peer == *swarm.local_peer_id()
        || !forwarder.is_configured_transport_peer(peer)
        || swarm.is_connected(&peer)
    {
        return;
    }

    if let Err(error) = swarm.dial(peer) {
        eprintln!("dial discovered provider {peer} failed: {error}");
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
    use libp2p::{
        core::{Endpoint, transport::PortUse},
        identity::Keypair,
    };

    use super::*;

    fn peer_id() -> Libp2pPeerId {
        Keypair::generate_ed25519().public().to_peer_id()
    }

    #[test]
    fn redial_targets_skip_self_and_connected_peers() {
        let local = peer_id();
        let connected = peer_id();
        let disconnected = peer_id();
        let bootstrap_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let peer_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");
        let local_address: Multiaddr = "/ip4/127.0.0.1/tcp/4003".parse().expect("address");

        let targets = pending_redial_targets(
            local,
            &[(connected, bootstrap_address)],
            &[(disconnected, peer_address.clone()), (local, local_address)],
            |peer| *peer == connected,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![(disconnected, peer_address)],
                skipped_connected: 1,
            }
        );
    }

    #[test]
    fn redial_targets_include_bootstrap_and_configured_peer_addresses() {
        let local = peer_id();
        let bootstrap = peer_id();
        let configured = peer_id();
        let bootstrap_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let peer_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");

        let targets = pending_redial_targets(
            local,
            &[(bootstrap, bootstrap_address.clone())],
            &[(configured, peer_address.clone())],
            |_| false,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![(bootstrap, bootstrap_address), (configured, peer_address)],
                skipped_connected: 0,
            }
        );
    }

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

    #[test]
    fn endpoint_path_kind_prefers_relay_then_quic_then_tcp() {
        let relay: Multiaddr =
            "/ip4/127.0.0.1/tcp/4001/p2p/12D3KooWLSJY9r3syVF7eh1b5CAJSmQkHdHu1QMUGNXk7Nzd4y6f/p2p-circuit/p2p/12D3KooWBCGXBm96czaYf6X41Hd2mD879WoF5Jyi8YUxi2Tiz3aT"
                .parse()
                .expect("relay address");
        let quic: Multiaddr = "/ip4/127.0.0.1/udp/4001/quic-v1"
            .parse()
            .expect("quic address");
        let tcp: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("tcp address");

        assert_eq!(
            path_kind_for_endpoint(&ConnectedPoint::Dialer {
                address: relay,
                role_override: Endpoint::Dialer,
                port_use: PortUse::Reuse,
            }),
            PathKind::CircuitRelay
        );
        assert_eq!(
            path_kind_for_endpoint(&ConnectedPoint::Dialer {
                address: quic,
                role_override: Endpoint::Dialer,
                port_use: PortUse::Reuse,
            }),
            PathKind::DirectQuicStream
        );
        assert_eq!(
            path_kind_for_endpoint(&ConnectedPoint::Listener {
                local_addr: tcp,
                send_back_addr: "/ip4/127.0.0.1/tcp/5000".parse().expect("send back"),
            }),
            PathKind::DirectTcpStream
        );
    }

    #[test]
    fn relay_client_events_update_path_metrics() {
        let metrics = RuntimeMetrics::default();
        let relay_peer_id = peer_id();
        let src_peer_id = peer_id();

        handle_relay_event(
            &metrics,
            &relay::client::Event::ReservationReqAccepted {
                relay_peer_id,
                renewal: false,
                limit: None,
            },
        );
        handle_relay_event(
            &metrics,
            &relay::client::Event::OutboundCircuitEstablished {
                relay_peer_id,
                limit: None,
            },
        );
        handle_relay_event(
            &metrics,
            &relay::client::Event::InboundCircuitEstablished {
                src_peer_id,
                limit: None,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.relay_reservations_accepted, 1);
        assert_eq!(snapshot.relay_outbound_circuits_established, 1);
        assert_eq!(snapshot.relay_inbound_circuits_established, 1);
    }

    #[test]
    fn relay_server_events_update_path_metrics() {
        let metrics = RuntimeMetrics::default();
        let src_peer_id = peer_id();
        let dst_peer_id = peer_id();

        handle_relay_server_event(
            &metrics,
            &relay::Event::ReservationReqAccepted {
                src_peer_id,
                renewed: false,
            },
        );
        handle_relay_server_event(
            &metrics,
            &relay::Event::CircuitReqAccepted {
                src_peer_id,
                dst_peer_id,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.relay_server_reservations_accepted, 1);
        assert_eq!(snapshot.relay_server_circuits_accepted, 1);
    }
}
