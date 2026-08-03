use std::{collections::HashSet, sync::Arc, time::Duration};

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
    config::{Config, ConfigError, DiscoveryConfig, QueueConfig},
    metrics::{PacketDropReason, RuntimeMetrics},
    path::PathSet,
    queue::PeerQueues,
    route::RouteError,
    runtime::{
        control::{
            ControlCapabilities, ControlRejectionReason, ControlRequest, ControlResponse,
            PeerCapabilities, accepted_capabilities_response, rejected_capabilities_response,
            validate_capabilities,
        },
        forward::{ForwardError, Forwarder},
        p2p::{Behaviour, BehaviourEvent, HostConfig, P2pBuildError, P2pNode, build_node},
        packet::{PacketRejectionReason, PacketResponse},
        tun::{TunDevice, TunReader, TunRuntimeError, TunWriter},
    },
};

const TUN_READ_CHANNEL: usize = 1024;
const REDIAL_INTERVAL: Duration = Duration::from_secs(10);
const MIN_QUEUE_EXPIRY_INTERVAL: Duration = Duration::from_millis(10);

pub async fn run_config(
    config: Config,
    device: TunDevice,
    metrics_interval: Option<Duration>,
) -> Result<(), RunnerError> {
    let identity = config.identity()?;
    let node = build_node(&HostConfig {
        identity,
        network_name: config.network.name.clone(),
        mtu: config.effective_packet_mtu(),
        max_concurrent_control_streams: config.resources.control_stream_limit(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
        listen_addresses: config.listen_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
        relay_reservations: config.relay_reservation_multiaddrs()?,
        relay_server: config.network.relay.server,
        relay_resources: config.network.relay.resources,
        discovery: config.network.discovery,
    })?;
    let forwarder = Forwarder::from_config(&config)?;
    let membership = OverlayMembership::from_config(&config)?;

    run_node(
        node,
        forwarder,
        membership,
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
    membership: OverlayMembership,
    device: TunDevice,
    mtu: u16,
    queue_config: QueueConfig,
    metrics_interval: Option<Duration>,
) -> Result<(), RunnerError> {
    let (reader, mut writer) = device.split();
    let metrics = Arc::new(RuntimeMetrics::default());
    let mut tun_rx = spawn_tun_reader(reader, Arc::clone(&metrics), mtu);
    let mut queues = PeerQueues::with_packet_ttl(
        queue_config.max_packets_per_peer,
        queue_config.max_bytes_per_peer,
        queue_config.max_packet_age(),
    );
    let mut paths = PathSet::new();
    let mut peer_capabilities = PeerCapabilities::default();
    let mut discovered_peer_addresses = DiscoveredPeerAddresses::default();
    let mut metrics_tick = metrics_interval.map(tokio::time::interval);
    let mut redial_tick = tokio::time::interval(REDIAL_INTERVAL);
    let mut queue_expiry_tick =
        tokio::time::interval(queue_expiry_interval(queue_config.max_packet_age()));
    let local_capabilities = ControlCapabilities::local(mtu);
    redial_tick.tick().await;
    queue_expiry_tick.tick().await;
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
                    metrics.record_outbound_drop(outbound_drop_reason(&error));
                    eprintln!("dropping outbound packet: {error:?}");
                }
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &paths, &peer_capabilities, &metrics);
            }
            event = node.swarm.select_next_some() => {
                handle_swarm_event(
                    &mut node.swarm,
                    SwarmEventContext {
                        forwarder: &mut forwarder,
                        membership: &membership,
                        writer: &mut writer,
                        paths: &mut paths,
                        peer_capabilities: &mut peer_capabilities,
                        discovered_peer_addresses: &mut discovered_peer_addresses,
                        metrics: &metrics,
                        local_capabilities: &local_capabilities,
                        discovery,
                    },
                    event,
                )?;
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &paths, &peer_capabilities, &metrics);
            }
            _ = redial_tick.tick() => {
                redial_known_addresses(
                    &mut node.swarm,
                    &node.bootstrap_peer_addresses,
                    &node.configured_peer_addresses,
                    discovered_peer_addresses.as_slice(),
                    &metrics,
                );
            }
            _ = queue_expiry_tick.tick() => {
                expire_outbound_queue(&mut queues);
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

fn queue_expiry_interval(max_packet_age: Duration) -> Duration {
    max_packet_age.clamp(MIN_QUEUE_EXPIRY_INTERVAL, REDIAL_INTERVAL)
}

fn expire_outbound_queue(queues: &mut PeerQueues) {
    queues.drop_expired(std::time::Instant::now());
}

fn redial_known_addresses(
    swarm: &mut Swarm<Behaviour>,
    bootstrap_addresses: &[(Libp2pPeerId, Multiaddr)],
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    metrics: &RuntimeMetrics,
) {
    let local_peer = *swarm.local_peer_id();
    let targets = pending_redial_targets(
        local_peer,
        bootstrap_addresses,
        configured_peer_addresses,
        discovered_peer_addresses,
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
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    mut is_connected: impl FnMut(&Libp2pPeerId) -> bool,
) -> RedialTargets {
    let mut addresses = Vec::new();
    let mut skipped_connected = 0;
    let mut seen = HashSet::new();
    for (peer, address) in bootstrap_addresses
        .iter()
        .chain(configured_peer_addresses.iter())
        .chain(discovered_peer_addresses.iter())
    {
        if *peer == local_peer {
            continue;
        }
        if is_connected(peer) {
            skipped_connected += 1;
            continue;
        }
        if !seen.insert((*peer, address.clone())) {
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

#[derive(Debug, Default, Eq, PartialEq)]
struct DiscoveredPeerAddresses {
    addresses: Vec<(Libp2pPeerId, Multiaddr)>,
}

impl DiscoveredPeerAddresses {
    fn insert(&mut self, peer: Libp2pPeerId, address: Multiaddr) {
        let entry = (peer, address);
        if !self.addresses.contains(&entry) {
            self.addresses.push(entry);
        }
    }

    fn remove(&mut self, peer: Libp2pPeerId, address: &Multiaddr) {
        self.addresses
            .retain(|(candidate_peer, candidate_address)| {
                *candidate_peer != peer || candidate_address != address
            });
    }

    fn as_slice(&self) -> &[(Libp2pPeerId, Multiaddr)] {
        &self.addresses
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlayMembership {
    peers: HashSet<Libp2pPeerId>,
}

impl OverlayMembership {
    pub fn from_config(config: &Config) -> Result<Self, ConfigError> {
        let mut peers = HashSet::new();
        peers.insert(
            config
                .network
                .local_peer
                .parse()
                .map_err(ConfigError::Libp2pPeerId)?,
        );

        for peer in &config.peers {
            peers.insert(peer.id.parse().map_err(ConfigError::Libp2pPeerId)?);
        }

        for peer in &config.network.bootstrap_peers {
            peers.insert(peer.id.parse().map_err(ConfigError::Libp2pPeerId)?);
        }

        for (_, address) in config
            .bootstrap_multiaddrs()?
            .into_iter()
            .chain(config.peer_multiaddrs()?)
        {
            if let Some(peer) = relay_peer_from_relayed_address(&address) {
                peers.insert(peer);
            }
        }

        for address in config.relay_reservation_multiaddrs()? {
            if let Some(peer) = relay_peer_from_relayed_address(&address) {
                peers.insert(peer);
            }
        }

        Ok(Self { peers })
    }

    #[must_use]
    pub fn allows(&self, peer: Libp2pPeerId) -> bool {
        self.peers.contains(&peer)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

fn relay_peer_from_relayed_address(address: &Multiaddr) -> Option<Libp2pPeerId> {
    let mut relay_peer = None;
    for protocol in address {
        match protocol {
            Protocol::P2p(peer) => relay_peer = Some(peer),
            Protocol::P2pCircuit => return relay_peer,
            _ => {}
        }
    }

    None
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
    peer_capabilities: &PeerCapabilities,
    metrics: &RuntimeMetrics,
) {
    expire_outbound_queue(queues);
    while let Some(packet) = queues.dequeue_ready(|peer| paths.has_healthy_path(peer)) {
        let peer_mtu = peer_capabilities.effective_mtu_for(
            packet.peer(),
            u16::try_from(forwarder.mtu()).unwrap_or(u16::MAX),
        );
        if let Err(error) = forwarder.send_queued_packet_with_mtu(swarm, &packet, peer_mtu) {
            metrics.record_outbound_drop(outbound_drop_reason(&error));
            eprintln!("dropping queued outbound packet: {error:?}");
        } else {
            metrics.record_outbound_sent();
        }
    }
}

struct SwarmEventContext<'a> {
    forwarder: &'a mut Forwarder,
    membership: &'a OverlayMembership,
    writer: &'a mut TunWriter,
    paths: &'a mut PathSet,
    peer_capabilities: &'a mut PeerCapabilities,
    discovered_peer_addresses: &'a mut DiscoveredPeerAddresses,
    metrics: &'a RuntimeMetrics,
    local_capabilities: &'a ControlCapabilities,
    discovery: DiscoveryConfig,
}

fn handle_swarm_event(
    swarm: &mut Swarm<Behaviour>,
    mut context: SwarmEventContext<'_>,
    event: SwarmEvent<BehaviourEvent>,
) -> Result<(), RunnerError> {
    match event {
        SwarmEvent::Behaviour(BehaviourEvent::Control(event)) => {
            handle_control_event(swarm, &mut context, event)?;
        }
        SwarmEvent::Behaviour(BehaviourEvent::Packet(event)) => {
            handle_packet_event(swarm, &mut context, event)?;
        }
        SwarmEvent::Behaviour(event) => {
            handle_behaviour_event(
                swarm,
                context.forwarder,
                context.discovered_peer_addresses,
                context.metrics,
                context.discovery,
                event,
            );
        }
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => {
            if !authorize_established_connection(context.membership, context.metrics, peer_id) {
                eprintln!("disconnecting unauthorized peer {peer_id}");
                if swarm.disconnect_peer_id(peer_id).is_err() {
                    eprintln!("unauthorized peer {peer_id} was already disconnected");
                }
                return Ok(());
            }
            record_path_established(context.paths, context.forwarder, peer_id, &endpoint);
            send_control_capabilities(
                swarm,
                context.forwarder,
                peer_id,
                context.local_capabilities,
                context.metrics,
            );
            context
                .metrics
                .record_connection_established(endpoint.is_relayed());
            eprintln!("connection established with {peer_id} via {endpoint:?}");
        }
        SwarmEvent::ConnectionClosed {
            peer_id, endpoint, ..
        } => {
            record_path_closed(context.paths, context.forwarder, peer_id, &endpoint);
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

fn handle_control_event(
    swarm: &mut Swarm<Behaviour>,
    context: &mut SwarmEventContext<'_>,
    event: request_response::Event<ControlRequest, ControlResponse>,
) -> Result<(), RunnerError> {
    match event {
        request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        } => {
            handle_control_request(swarm, context, peer, request, channel)?;
        }
        request_response::Event::Message {
            peer,
            message: Message::Response { response, .. },
            ..
        } => {
            handle_control_response(
                context.forwarder,
                context.peer_capabilities,
                context.metrics,
                peer,
                response,
            );
        }
        request_response::Event::OutboundFailure { peer, error, .. } => {
            context.metrics.record_control_failure();
            eprintln!("control request to {peer} failed: {error}");
        }
        request_response::Event::InboundFailure { peer, error, .. } => {
            context.metrics.record_control_failure();
            eprintln!("control request from {peer} failed: {error}");
        }
        request_response::Event::ResponseSent { .. } => {}
    }

    Ok(())
}

fn handle_packet_event(
    swarm: &mut Swarm<Behaviour>,
    context: &mut SwarmEventContext<'_>,
    event: request_response::Event<crate::wire::Frame, crate::runtime::packet::PacketResponse>,
) -> Result<(), RunnerError> {
    match event {
        request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        } => match context.forwarder.accept_inbound_packet(peer, &request) {
            Ok(packet) => {
                context.writer.write_packet(packet)?;
                context.metrics.record_tun_write(packet.len());
                context.metrics.record_inbound_accepted();
                Forwarder::send_packet_response(swarm, channel, PacketResponse::Accepted)
                    .map_err(|_| RunnerError::PacketResponseDropped)?;
            }
            Err(error) => {
                let drop_reason = inbound_drop_reason(&error);
                context.metrics.record_inbound_drop(drop_reason);
                eprintln!("dropping inbound packet from {peer}: {error:?}");
                Forwarder::send_packet_response(
                    swarm,
                    channel,
                    PacketResponse::Rejected(packet_rejection_reason(drop_reason)),
                )
                .map_err(|_| RunnerError::PacketResponseDropped)?;
            }
        },
        request_response::Event::Message {
            peer,
            message: Message::Response { response, .. },
            ..
        } => match response {
            PacketResponse::Accepted => {}
            PacketResponse::Rejected(reason) => {
                context.metrics.record_outbound_failure();
                eprintln!("packet request to {peer} was rejected: {reason:?}");
            }
        },
        request_response::Event::OutboundFailure { peer, error, .. } => {
            context.metrics.record_outbound_failure();
            eprintln!("packet request to {peer} failed: {error}");
        }
        request_response::Event::InboundFailure { peer, error, .. } => {
            context.metrics.record_inbound_failure();
            eprintln!("packet request from {peer} failed: {error}");
        }
        request_response::Event::ResponseSent { .. } => {}
    }

    Ok(())
}

fn handle_control_request(
    swarm: &mut Swarm<Behaviour>,
    context: &mut SwarmEventContext<'_>,
    peer: Libp2pPeerId,
    request: ControlRequest,
    channel: request_response::ResponseChannel<ControlResponse>,
) -> Result<(), RunnerError> {
    match request {
        ControlRequest::Capabilities(capabilities) => {
            context.metrics.record_control_request_received();
            eprintln!("control capabilities from {peer}: {capabilities:?}");
            let response = capability_response_for_peer(
                context.forwarder,
                peer,
                &capabilities,
                context.local_capabilities,
            );
            match &response {
                ControlResponse::CapabilitiesAccepted(_) => {
                    record_peer_capabilities(
                        context.forwarder,
                        context.peer_capabilities,
                        peer,
                        capabilities.clone(),
                    );
                }
                ControlResponse::CapabilitiesRejected(reason) => {
                    context.metrics.record_control_failure();
                    eprintln!("rejecting control capabilities from {peer}: {reason:?}");
                }
            }
            swarm
                .behaviour_mut()
                .control
                .send_response(channel, response)
                .map_err(|_| RunnerError::ControlResponseDropped)?;
        }
    }

    Ok(())
}

fn handle_control_response(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    response: ControlResponse,
) {
    match response {
        ControlResponse::CapabilitiesAccepted(capabilities) => {
            if let Some(reason) = validate_capabilities(&capabilities) {
                metrics.record_control_failure();
                eprintln!("ignoring incompatible control acceptance from {peer}: {reason:?}");
            } else {
                record_peer_capabilities(forwarder, peer_capabilities, peer, capabilities.clone());
                metrics.record_control_response_received();
                eprintln!("control capabilities accepted by {peer}: {capabilities:?}");
            }
        }
        ControlResponse::CapabilitiesRejected(reason) => {
            metrics.record_control_failure();
            eprintln!("control capabilities rejected by {peer}: {reason:?}");
        }
    }
}

fn capability_response_for_peer(
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    capabilities: &ControlCapabilities,
    local_capabilities: &ControlCapabilities,
) -> ControlResponse {
    if !forwarder.is_configured_transport_peer(peer) {
        return rejected_capabilities_response(ControlRejectionReason::UnauthorizedPeer);
    }

    if let Some(reason) = validate_capabilities(capabilities) {
        return rejected_capabilities_response(reason);
    }

    accepted_capabilities_response(local_capabilities)
}

fn record_peer_capabilities(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    peer: Libp2pPeerId,
    capabilities: ControlCapabilities,
) {
    if forwarder.is_configured_transport_peer(peer) {
        peer_capabilities.record(PeerId::from_libp2p(peer), capabilities);
    }
}

fn send_control_capabilities(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    local_capabilities: &ControlCapabilities,
    metrics: &RuntimeMetrics,
) {
    if !forwarder.is_configured_transport_peer(peer) {
        return;
    }

    swarm.behaviour_mut().control.send_request(
        &peer,
        ControlRequest::Capabilities(local_capabilities.clone()),
    );
    metrics.record_control_request_sent();
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

fn authorize_established_connection(
    membership: &OverlayMembership,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
) -> bool {
    if membership.allows(peer) {
        return true;
    }

    metrics.record_unauthorized_connection_dropped();
    false
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
    discovered_peer_addresses: &mut DiscoveredPeerAddresses,
    metrics: &RuntimeMetrics,
    discovery: DiscoveryConfig,
    event: BehaviourEvent,
) {
    match event {
        BehaviourEvent::Mdns(mdns::Event::Discovered(peers)) if discovery.mdns => {
            for (peer, address) in peers {
                learn_peer_address(
                    swarm,
                    forwarder,
                    discovered_peer_addresses,
                    peer,
                    address,
                    discovery.kademlia,
                );
            }
        }
        BehaviourEvent::Mdns(mdns::Event::Expired(peers)) if discovery.mdns => {
            for (peer, address) in peers {
                discovered_peer_addresses.remove(peer, &address);
                if discovery.kademlia {
                    swarm.behaviour_mut().kad.remove_address(&peer, &address);
                }
            }
        }
        BehaviourEvent::Identify(identify::Event::Received { peer_id, info, .. }) => {
            for address in info.listen_addrs {
                learn_peer_address(
                    swarm,
                    forwarder,
                    discovered_peer_addresses,
                    peer_id,
                    address,
                    discovery.kademlia,
                );
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
    discovered_peer_addresses: &mut DiscoveredPeerAddresses,
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

    discovered_peer_addresses.insert(peer, address.clone());

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

fn outbound_drop_reason(error: &ForwardError) -> PacketDropReason {
    match error {
        ForwardError::NoRoute(_) => PacketDropReason::NoRoute,
        ForwardError::NoTransportPeer(_) => PacketDropReason::NoTransportPeer,
        ForwardError::PacketTooLarge { .. } => PacketDropReason::PacketTooLarge,
        ForwardError::UnauthorizedLocalSource { .. } => PacketDropReason::UnauthorizedSource,
        ForwardError::TruncatedIpPacket { .. }
        | ForwardError::UnsupportedIpVersion(_)
        | ForwardError::PayloadLengthMismatch { .. }
        | ForwardError::UnexpectedPayload(_)
        | ForwardError::Frame(_)
        | ForwardError::Config(_)
        | ForwardError::Route(_)
        | ForwardError::UnauthorizedPeer(_)
        | ForwardError::UnauthorizedLocalDestination { .. }
        | ForwardError::ReplayedPacket { .. }
        | ForwardError::PacketOutsideReplayWindow { .. } => PacketDropReason::MalformedPacket,
        ForwardError::Enqueue(crate::queue::EnqueueError::PacketTooLarge { .. }) => {
            PacketDropReason::PacketTooLarge
        }
        ForwardError::Enqueue(crate::queue::EnqueueError::QueueFull { .. }) => {
            PacketDropReason::QueueFull
        }
    }
}

fn inbound_drop_reason(error: &ForwardError) -> PacketDropReason {
    match error {
        ForwardError::UnauthorizedPeer(_) => PacketDropReason::UnauthorizedPeer,
        ForwardError::Route(RouteError::UnauthorizedSource { .. }) => {
            PacketDropReason::UnauthorizedSource
        }
        ForwardError::UnauthorizedLocalDestination { .. } => {
            PacketDropReason::UnauthorizedDestination
        }
        ForwardError::PacketTooLarge { .. } => PacketDropReason::PacketTooLarge,
        ForwardError::UnexpectedPayload(_) => PacketDropReason::UnexpectedPayload,
        ForwardError::ReplayedPacket { .. } | ForwardError::PacketOutsideReplayWindow { .. } => {
            PacketDropReason::Replay
        }
        ForwardError::PayloadLengthMismatch { .. }
        | ForwardError::TruncatedIpPacket { .. }
        | ForwardError::UnsupportedIpVersion(_)
        | ForwardError::Frame(_)
        | ForwardError::Config(_)
        | ForwardError::Route(_)
        | ForwardError::NoRoute(_)
        | ForwardError::NoTransportPeer(_)
        | ForwardError::Enqueue(_)
        | ForwardError::UnauthorizedLocalSource { .. } => PacketDropReason::MalformedPacket,
    }
}

fn packet_rejection_reason(reason: PacketDropReason) -> PacketRejectionReason {
    match reason {
        PacketDropReason::PacketTooLarge | PacketDropReason::QueueFull => {
            PacketRejectionReason::PacketTooLarge
        }
        PacketDropReason::Replay => PacketRejectionReason::Replay,
        PacketDropReason::UnauthorizedPeer | PacketDropReason::NoTransportPeer => {
            PacketRejectionReason::UnauthorizedPeer
        }
        PacketDropReason::UnauthorizedSource => PacketRejectionReason::UnauthorizedSource,
        PacketDropReason::UnauthorizedDestination => PacketRejectionReason::UnauthorizedDestination,
        PacketDropReason::UnexpectedPayload => PacketRejectionReason::UnexpectedPayload,
        PacketDropReason::MalformedPacket | PacketDropReason::NoRoute => {
            PacketRejectionReason::MalformedPacket
        }
    }
}

#[derive(Debug)]
pub enum RunnerError {
    Config(crate::config::ConfigError),
    P2p(P2pBuildError),
    Forward(ForwardError),
    Tun(TunRuntimeError),
    PacketResponseDropped,
    ControlResponseDropped,
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
    use std::{collections::HashSet, net::Ipv4Addr};

    use libp2p::{
        core::{Endpoint, transport::PortUse},
        identity::Keypair,
    };

    use crate::{
        config::{
            BootstrapPeerConfig, Config, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig,
            RelayConfig, ResourceConfig,
        },
        route::builtin_ipv4,
    };

    use super::*;

    fn peer_id() -> Libp2pPeerId {
        Keypair::generate_ed25519().public().to_peer_id()
    }

    fn ipv4_packet(source: Ipv4Addr, destination: Ipv4Addr) -> Vec<u8> {
        let mut packet = vec![0; 20];
        packet[0] = 0x45;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        packet
    }

    #[test]
    fn queue_expiry_interval_is_bounded_by_ttl_and_redial_interval() {
        assert_eq!(
            queue_expiry_interval(Duration::from_millis(1)),
            MIN_QUEUE_EXPIRY_INTERVAL
        );
        assert_eq!(
            queue_expiry_interval(Duration::from_millis(250)),
            Duration::from_millis(250)
        );
        assert_eq!(
            queue_expiry_interval(Duration::from_secs(11)),
            REDIAL_INTERVAL
        );
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
            &[],
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
            &[],
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
    fn redial_targets_include_discovered_peer_addresses() {
        let local = peer_id();
        let configured = peer_id();
        let discovered = peer_id();
        let configured_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let discovered_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");

        let targets = pending_redial_targets(
            local,
            &[],
            &[(configured, configured_address.clone())],
            &[(discovered, discovered_address.clone())],
            |_| false,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![
                    (configured, configured_address),
                    (discovered, discovered_address),
                ],
                skipped_connected: 0,
            }
        );
    }

    #[test]
    fn redial_targets_deduplicate_discovered_addresses() {
        let local = peer_id();
        let peer = peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        let targets = pending_redial_targets(
            local,
            &[(peer, address.clone())],
            &[],
            &[(peer, address.clone())],
            |_| false,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![(peer, address)],
                skipped_connected: 0,
            }
        );
    }

    #[test]
    fn discovered_peer_addresses_are_unique_and_expirable() {
        let peer = peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let other_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");
        let mut discovered = DiscoveredPeerAddresses::default();

        discovered.insert(peer, address.clone());
        discovered.insert(peer, address.clone());
        discovered.insert(peer, other_address.clone());

        assert_eq!(
            discovered.as_slice(),
            &[(peer, address.clone()), (peer, other_address.clone())]
        );

        discovered.remove(peer, &address);

        assert_eq!(discovered.as_slice(), &[(peer, other_address)]);
    }

    #[test]
    fn overlay_membership_includes_local_peers_bootstrap_and_relay_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let local = local_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("local peer");
        let configured = peer_id();
        let bootstrap = peer_id();
        let relay = peer_id();
        let peer_address_relay = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                listen_addresses: Vec::new(),
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: bootstrap.to_string(),
                    address: "/ip4/127.0.0.1/tcp/4001".to_owned(),
                }],
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig {
                    server: false,
                    reservations: vec![format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay}/p2p-circuit")],
                    resources: crate::config::RelayResourceConfig::default(),
                },
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: configured.to_string(),
                name: None,
                addresses: vec![format!(
                    "/ip4/127.0.0.1/tcp/4003/p2p/{peer_address_relay}/p2p-circuit/p2p/{configured}"
                )],
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };

        let membership = OverlayMembership::from_config(&config).expect("membership");

        assert_eq!(membership.len(), 5);
        assert!(membership.allows(local));
        assert!(membership.allows(configured));
        assert!(membership.allows(bootstrap));
        assert!(membership.allows(relay));
        assert!(membership.allows(peer_address_relay));
        assert!(!membership.allows(peer_id()));
    }

    #[test]
    fn relay_peer_is_extracted_from_reservation_address() {
        let relay = peer_id();
        let target = peer_id();
        let reservation: Multiaddr = format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay}/p2p-circuit")
            .parse()
            .expect("reservation");
        let target_address: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay}/p2p-circuit/p2p/{target}")
                .parse()
                .expect("target address");

        assert_eq!(relay_peer_from_relayed_address(&reservation), Some(relay));
        assert_eq!(
            relay_peer_from_relayed_address(&target_address),
            Some(relay)
        );
    }

    #[test]
    fn unauthorized_connections_are_rejected_and_counted() {
        let allowed = peer_id();
        let rejected = peer_id();
        let membership = OverlayMembership {
            peers: HashSet::from([allowed]),
        };
        let metrics = RuntimeMetrics::default();

        assert!(authorize_established_connection(
            &membership,
            &metrics,
            allowed
        ));
        assert!(!authorize_established_connection(
            &membership,
            &metrics,
            rejected
        ));

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.unauthorized_connections_dropped, 1);
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
    fn packet_drop_reasons_map_to_packet_rejections() {
        assert_eq!(
            packet_rejection_reason(PacketDropReason::MalformedPacket),
            PacketRejectionReason::MalformedPacket
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::PacketTooLarge),
            PacketRejectionReason::PacketTooLarge
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::QueueFull),
            PacketRejectionReason::PacketTooLarge
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::Replay),
            PacketRejectionReason::Replay
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::UnauthorizedPeer),
            PacketRejectionReason::UnauthorizedPeer
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::NoTransportPeer),
            PacketRejectionReason::UnauthorizedPeer
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::UnauthorizedSource),
            PacketRejectionReason::UnauthorizedSource
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::UnauthorizedDestination),
            PacketRejectionReason::UnauthorizedDestination
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::UnexpectedPayload),
            PacketRejectionReason::UnexpectedPayload
        );
        assert_eq!(
            packet_rejection_reason(PacketDropReason::NoRoute),
            PacketRejectionReason::MalformedPacket
        );
    }

    #[test]
    fn capability_response_accepts_configured_compatible_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                listen_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let local_capabilities = ControlCapabilities::local(1280);

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &ControlCapabilities::local(1200),
                &local_capabilities,
            ),
            ControlResponse::CapabilitiesAccepted(local_capabilities)
        );
    }

    #[test]
    fn capability_response_rejects_unconfigured_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let unconfigured = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                listen_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: configured.to_string(),
                name: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                unconfigured,
                &ControlCapabilities::local(1280),
                &ControlCapabilities::local(1280),
            ),
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::UnauthorizedPeer)
        );
    }

    #[test]
    fn capability_response_rejects_incompatible_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                listen_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut capabilities = ControlCapabilities::local(1280);
        capabilities.packet_protocol = "/other/packet/1".to_owned();

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &capabilities,
                &ControlCapabilities::local(1280),
            ),
            ControlResponse::CapabilitiesRejected(
                ControlRejectionReason::UnsupportedPacketProtocol
            )
        );
    }

    #[tokio::test]
    async fn drain_outbound_queue_respects_peer_capability_mtu() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_overlay = local_identity
            .peer_id
            .parse::<PeerId>()
            .expect("local overlay peer");
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                listen_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("node");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(4, 4096);
        forwarder
            .enqueue_tun_packet(
                &mut queues,
                ipv4_packet(builtin_ipv4(local_overlay), builtin_ipv4(remote_overlay)),
            )
            .expect("queued");
        let mut paths = PathSet::new();
        paths.record_established(remote_overlay, PathKind::DirectTcpStream);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(remote_overlay, ControlCapabilities::local(19));
        let metrics = RuntimeMetrics::default();

        drain_outbound_queue(
            &mut node.swarm,
            &forwarder,
            &mut queues,
            &paths,
            &peer_capabilities,
            &metrics,
        );

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_sent_packets, 0);
        assert_eq!(snapshot.outbound_dropped_packets, 1);
        assert_eq!(snapshot.outbound_drop_packet_too_large_packets, 1);
        assert_eq!(snapshot.queue.queued_packets, 0);
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
