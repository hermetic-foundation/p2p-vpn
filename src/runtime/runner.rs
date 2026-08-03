use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use libp2p::{
    Multiaddr, PeerId as Libp2pPeerId, Swarm, autonat,
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
    metrics::{AutoNatReachability, PacketDropReason, RuntimeMetrics},
    path::{PathSet, PathTransportSupport},
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
const KADEMLIA_REFRESH_INTERVAL: Duration = Duration::from_mins(1);
const DISCOVERED_ADDRESS_TTL: Duration = Duration::from_mins(10);
const MIN_QUEUE_EXPIRY_INTERVAL: Duration = Duration::from_millis(10);
const LOCAL_QUIC_DATAGRAMS_SUPPORTED: bool = false;

pub async fn run_config(
    config: Config,
    device: TunDevice,
    metrics_interval: Option<Duration>,
) -> Result<(), RunnerError> {
    let identity = config.identity()?;
    let node = build_node(&HostConfig {
        identity,
        network_name: config.network.name.clone(),
        membership_tag: config.membership_tag()?,
        mtu: config.effective_packet_mtu(),
        max_concurrent_control_streams: config.resources.control_stream_limit(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
        listen_addresses: config.listen_multiaddrs()?,
        external_addresses: config.external_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
        relay_reservations: config.relay_reservation_multiaddrs()?,
        relay_server: config.network.relay.server,
        relay_resources: config.network.relay.resources,
        resources: config.resources,
        discovery: config.network.discovery.clone(),
    })?;
    let forwarder = Forwarder::from_config(&config)?;
    let membership = OverlayMembership::from_config(&config)?;

    Box::pin(run_node(
        node,
        forwarder,
        membership,
        device,
        config.effective_packet_mtu(),
        config.queue,
        metrics_interval,
    ))
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
    let kademlia_rendezvous_key = node.kademlia_rendezvous_key.clone();
    let mut kademlia_refresh_tick = kademlia_rendezvous_key
        .as_ref()
        .map(|_| tokio::time::interval(KADEMLIA_REFRESH_INTERVAL));
    let mut queue_expiry_tick =
        tokio::time::interval(queue_expiry_interval(queue_config.max_packet_age()));
    let local_capabilities =
        ControlCapabilities::local(&node.network_name, node.membership_tag.clone(), mtu)
            .with_advertised_routes(forwarder.local_advertised_routes());
    redial_tick.tick().await;
    if let Some(tick) = &mut kademlia_refresh_tick {
        tick.tick().await;
    }
    queue_expiry_tick.tick().await;
    let discovery = node.discovery.clone();

    log_startup_status(node.startup);

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
                        discovery: &discovery,
                    },
                    event,
                )?;
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &paths, &peer_capabilities, &metrics);
            }
            _ = redial_tick.tick() => {
                handle_redial_tick(&mut node, &mut discovered_peer_addresses, &metrics);
            }
            () = async {
                kademlia_refresh_tick
                    .as_mut()
                    .expect("kademlia refresh interval is present")
                    .tick()
                    .await;
            }, if kademlia_refresh_tick.is_some() => {
                refresh_kademlia_rendezvous(
                    &mut node.swarm,
                    kademlia_rendezvous_key
                        .as_ref()
                        .expect("kademlia rendezvous key is present"),
                    &metrics,
                );
            }
            _ = queue_expiry_tick.tick() => {
                expire_outbound_queue(&mut queues, &metrics);
            }
            () = async {
                metrics_tick
                    .as_mut()
                    .expect("metrics interval is present")
                    .tick()
                    .await;
            }, if metrics_tick.is_some() => {
                print_metrics(
                    &metrics,
                    queues.total_stats(),
                    runtime_path_stats(&forwarder, &paths, &peer_capabilities),
                );
            }
        }
    }
}

fn handle_redial_tick(
    node: &mut P2pNode,
    discovered_peer_addresses: &mut DiscoveredPeerAddresses,
    metrics: &RuntimeMetrics,
) {
    expire_discovered_peer_addresses(discovered_peer_addresses, metrics);
    let discovered_addresses = discovered_peer_addresses.as_vec();
    redial_known_addresses(
        &mut node.swarm,
        &node.bootstrap_peer_addresses,
        &node.relay_peer_addresses,
        &node.configured_peer_addresses,
        &discovered_addresses,
        metrics,
    );
}

fn log_startup_status(startup: crate::runtime::p2p::StartupStatus) {
    if startup.mdns_enabled {
        eprintln!("mdns discovery enabled");
    }
    if startup.external_addresses_configured > 0 {
        eprintln!(
            "external addresses configured: {}",
            startup.external_addresses_configured
        );
    }
    if startup.dcutr_enabled {
        eprintln!("dcutr hole punching enabled");
    }
    if startup.autonat_enabled {
        eprintln!("autonat reachability probing enabled");
    }
    if startup.autonat_servers_registered > 0 {
        eprintln!(
            "autonat probe servers registered: {}",
            startup.autonat_servers_registered
        );
    }
    if startup.kademlia.bootstrap_started {
        eprintln!("kademlia bootstrap started");
    }
    if startup.kademlia.rendezvous_advertise_started {
        eprintln!("kademlia overlay provider advertisement started");
    }
    if startup.kademlia.rendezvous_lookup_started {
        eprintln!("kademlia overlay provider lookup started");
    }
    if startup.relay_reservations_started > 0 {
        eprintln!(
            "relay reservation listeners started: {}",
            startup.relay_reservations_started
        );
    }
    if startup.relay_server_enabled {
        eprintln!("relay server enabled");
    }
}

fn queue_expiry_interval(max_packet_age: Duration) -> Duration {
    max_packet_age.clamp(MIN_QUEUE_EXPIRY_INTERVAL, REDIAL_INTERVAL)
}

fn expire_outbound_queue(queues: &mut PeerQueues, metrics: &RuntimeMetrics) {
    let expired_before = queues.total_stats().expired_packets;
    queues.drop_expired(std::time::Instant::now());
    let expired_after = queues.total_stats().expired_packets;
    metrics.record_outbound_queue_expired(expired_after.saturating_sub(expired_before));
}

fn expire_discovered_peer_addresses(
    discovered_peer_addresses: &mut DiscoveredPeerAddresses,
    metrics: &RuntimeMetrics,
) {
    let expired = discovered_peer_addresses.drop_expired(Instant::now(), DISCOVERED_ADDRESS_TTL);
    metrics.record_discovered_address_expired(expired);
}

fn refresh_kademlia_rendezvous(
    swarm: &mut Swarm<Behaviour>,
    rendezvous_key: &kad::RecordKey,
    metrics: &RuntimeMetrics,
) {
    match swarm
        .behaviour_mut()
        .kad
        .start_providing(rendezvous_key.clone())
    {
        Ok(_) => metrics.record_kademlia_provider_advertisement(),
        Err(error) => {
            metrics.record_kademlia_provider_advertisement_failure();
            eprintln!("kademlia provider advertisement refresh failed: {error:?}");
        }
    }

    swarm
        .behaviour_mut()
        .kad
        .get_providers(rendezvous_key.clone());
    metrics.record_kademlia_provider_lookup();

    match swarm.behaviour_mut().kad.bootstrap() {
        Ok(_) => metrics.record_kademlia_bootstrap_refresh(),
        Err(error) => {
            metrics.record_kademlia_bootstrap_failure();
            eprintln!("kademlia bootstrap refresh failed: {error:?}");
        }
    }
}

fn redial_known_addresses(
    swarm: &mut Swarm<Behaviour>,
    bootstrap_addresses: &[(Libp2pPeerId, Multiaddr)],
    relay_addresses: &[(Libp2pPeerId, Multiaddr)],
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    metrics: &RuntimeMetrics,
) {
    let local_peer = *swarm.local_peer_id();
    let targets = pending_redial_targets(
        local_peer,
        bootstrap_addresses,
        relay_addresses,
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
    relay_addresses: &[(Libp2pPeerId, Multiaddr)],
    configured_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    discovered_peer_addresses: &[(Libp2pPeerId, Multiaddr)],
    mut is_connected: impl FnMut(&Libp2pPeerId) -> bool,
) -> RedialTargets {
    let mut addresses = Vec::new();
    let mut skipped_connected = 0;
    let mut seen = HashSet::new();
    for (peer, address) in bootstrap_addresses
        .iter()
        .chain(relay_addresses.iter())
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
    addresses: Vec<DiscoveredPeerAddress>,
}

#[derive(Debug, Eq, PartialEq)]
struct DiscoveredPeerAddress {
    peer: Libp2pPeerId,
    address: Multiaddr,
    last_seen: Instant,
}

impl DiscoveredPeerAddresses {
    fn insert(&mut self, peer: Libp2pPeerId, address: Multiaddr) {
        self.insert_at(peer, address, Instant::now());
    }

    fn insert_at(&mut self, peer: Libp2pPeerId, address: Multiaddr, now: Instant) {
        if let Some(entry) = self
            .addresses
            .iter_mut()
            .find(|entry| entry.peer == peer && entry.address == address)
        {
            entry.last_seen = now;
            return;
        }

        self.addresses.push(DiscoveredPeerAddress {
            peer,
            address,
            last_seen: now,
        });
    }

    fn remove(&mut self, peer: Libp2pPeerId, address: &Multiaddr) {
        self.addresses
            .retain(|entry| entry.peer != peer || &entry.address != address);
    }

    fn drop_expired(&mut self, now: Instant, max_age: Duration) -> u64 {
        let mut expired = 0_u64;
        self.addresses.retain(|entry| {
            let keep = now.saturating_duration_since(entry.last_seen) <= max_age;
            if !keep {
                expired = expired.saturating_add(1);
            }
            keep
        });
        expired
    }

    fn as_vec(&self) -> Vec<(Libp2pPeerId, Multiaddr)> {
        self.addresses
            .iter()
            .map(|entry| (entry.peer, entry.address.clone()))
            .collect()
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
    expire_outbound_queue(queues, metrics);
    while let Some(packet) = queues.dequeue_ready(|peer| {
        peer_capabilities.contains(peer)
            && paths.has_supported_path(peer, packet_transport_support(peer_capabilities, peer))
    }) {
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
    if queues.total_stats().queued_packets > 0 {
        metrics.record_outbound_queue_blocked_no_supported_path();
    }
}

fn packet_transport_support(
    peer_capabilities: &PeerCapabilities,
    peer: PeerId,
) -> PathTransportSupport {
    PathTransportSupport {
        quic_datagrams: LOCAL_QUIC_DATAGRAMS_SUPPORTED
            && peer_capabilities.supports_quic_datagrams_for(peer),
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
    discovery: &'a DiscoveryConfig,
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
            peer_id,
            endpoint,
            num_established,
            ..
        } => {
            if !authorize_established_connection(context.membership, context.metrics, peer_id) {
                eprintln!("disconnecting unauthorized peer {peer_id}");
                if swarm.disconnect_peer_id(peer_id).is_err() {
                    eprintln!("unauthorized peer {peer_id} was already disconnected");
                }
                return Ok(());
            }
            invalidate_peer_capabilities_on_first_connection(
                context.forwarder,
                context.peer_capabilities,
                peer_id,
                num_established.get(),
            );
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
            peer_id,
            endpoint,
            num_established,
            ..
        } => {
            record_path_closed(context.paths, context.forwarder, peer_id, &endpoint);
            invalidate_peer_capabilities_when_disconnected(
                context.forwarder,
                context.peer_capabilities,
                peer_id,
                num_established,
            );
            eprintln!("connection closed with {peer_id} via {endpoint:?}");
        }
        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            handle_outgoing_connection_error(context.metrics, peer_id, &error);
        }
        SwarmEvent::NewExternalAddrCandidate { address } => {
            context.metrics.record_external_address_candidate();
            eprintln!("external address candidate observed: {address}");
        }
        SwarmEvent::ExternalAddrConfirmed { address } => {
            context.metrics.record_external_address_confirmed();
            eprintln!("external address confirmed: {address}");
        }
        SwarmEvent::ExternalAddrExpired { address } => {
            context.metrics.record_external_address_expired();
            eprintln!("external address expired: {address}");
        }
        _ => {}
    }

    Ok(())
}

fn handle_outgoing_connection_error(
    metrics: &RuntimeMetrics,
    peer_id: Option<Libp2pPeerId>,
    error: &impl std::fmt::Display,
) {
    metrics.record_outgoing_connection_error();
    match peer_id {
        Some(peer_id) => eprintln!("outgoing connection to {peer_id} failed: {error}"),
        None => eprintln!("outgoing connection failed: {error}"),
    }
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
                &context.local_capabilities.network_name,
                context.local_capabilities.membership_tag.as_deref(),
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
                context
                    .metrics
                    .record_outbound_drop(packet_rejection_drop_reason(reason));
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
                    context.metrics.record_control_capability_accept();
                }
                ControlResponse::CapabilitiesRejected(reason) => {
                    context.metrics.record_control_capability_rejection(*reason);
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
    expected_network: &str,
    expected_membership_tag: Option<&str>,
) {
    metrics.record_control_response_received();
    match response {
        ControlResponse::CapabilitiesAccepted(capabilities) => {
            if let Some(reason) = validate_peer_capabilities(
                forwarder,
                peer,
                &capabilities,
                expected_network,
                expected_membership_tag,
            ) {
                metrics.record_control_capability_rejection(reason);
                metrics.record_control_failure();
                eprintln!("ignoring incompatible control acceptance from {peer}: {reason:?}");
            } else {
                record_peer_capabilities(forwarder, peer_capabilities, peer, capabilities.clone());
                metrics.record_control_capability_accept();
                eprintln!("control capabilities accepted by {peer}: {capabilities:?}");
            }
        }
        ControlResponse::CapabilitiesRejected(reason) => {
            metrics.record_control_capability_rejection(reason);
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

    if let Some(reason) = validate_capabilities(
        capabilities,
        &local_capabilities.network_name,
        local_capabilities.membership_tag.as_deref(),
    ) {
        return rejected_capabilities_response(reason);
    }

    if !forwarder.authorizes_advertised_routes(peer, &capabilities.advertised_routes) {
        return rejected_capabilities_response(
            ControlRejectionReason::UnauthorizedRouteAdvertisement,
        );
    }

    accepted_capabilities_response(local_capabilities)
}

fn validate_peer_capabilities(
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    capabilities: &ControlCapabilities,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
) -> Option<ControlRejectionReason> {
    if let Some(reason) =
        validate_capabilities(capabilities, expected_network, expected_membership_tag)
    {
        return Some(reason);
    }

    if !forwarder.authorizes_advertised_routes(peer, &capabilities.advertised_routes) {
        return Some(ControlRejectionReason::UnauthorizedRouteAdvertisement);
    }

    None
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

fn invalidate_peer_capabilities_on_first_connection(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    peer: Libp2pPeerId,
    established_connections: u32,
) {
    if established_connections == 1 {
        invalidate_peer_capabilities(forwarder, peer_capabilities, peer);
    }
}

fn invalidate_peer_capabilities_when_disconnected(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    peer: Libp2pPeerId,
    remaining_connections: u32,
) {
    if remaining_connections == 0 {
        invalidate_peer_capabilities(forwarder, peer_capabilities, peer);
    }
}

fn invalidate_peer_capabilities(
    forwarder: &Forwarder,
    peer_capabilities: &mut PeerCapabilities,
    peer: Libp2pPeerId,
) {
    if forwarder.is_configured_transport_peer(peer) {
        peer_capabilities.remove(PeerId::from_libp2p(peer));
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
    discovery: &DiscoveryConfig,
    event: BehaviourEvent,
) {
    match event {
        BehaviourEvent::Mdns(mdns::Event::Discovered(peers)) if discovery.mdns => {
            for (peer, address) in peers {
                learn_peer_address(
                    swarm,
                    forwarder,
                    discovered_peer_addresses,
                    metrics,
                    peer,
                    address,
                    discovery,
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
            let observed_addr = info.observed_addr.clone();
            for address in info.listen_addrs {
                learn_peer_address(
                    swarm,
                    forwarder,
                    discovered_peer_addresses,
                    metrics,
                    peer_id,
                    address,
                    discovery,
                );
            }
            if discovery.autonat && observed_addr.iter().next().is_some() {
                schedule_autonat_probe(swarm, metrics, &observed_addr);
            }
        }
        BehaviourEvent::Identify(identify::Event::Error { peer_id, error, .. }) => {
            eprintln!("identify with {peer_id} failed: {error}");
        }
        BehaviourEvent::Kad(event) if discovery.kademlia => {
            handle_kademlia_event(swarm, forwarder, metrics, event);
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
        BehaviourEvent::Autonat(event) if discovery.autonat => {
            handle_autonat_event(metrics, event);
        }
        _ => {}
    }
}

fn schedule_autonat_probe(
    swarm: &mut Swarm<Behaviour>,
    metrics: &RuntimeMetrics,
    address: &Multiaddr,
) -> bool {
    let Some(autonat) = swarm.behaviour_mut().autonat.as_mut() else {
        return false;
    };
    autonat.probe_address(address.clone());
    metrics.record_autonat_probe_scheduled();
    eprintln!("autonat probe scheduled for observed address: {address}");
    true
}

fn handle_autonat_event(metrics: &RuntimeMetrics, event: autonat::Event) {
    match event {
        autonat::Event::StatusChanged { old, new } => {
            metrics.record_autonat_status(autonat_reachability(&new));
            eprintln!("autonat status changed: {old:?} -> {new:?}");
        }
        autonat::Event::OutboundProbe(event) => {
            eprintln!("autonat outbound probe: {event:?}");
        }
        autonat::Event::InboundProbe(event) => {
            eprintln!("autonat inbound probe: {event:?}");
        }
    }
}

fn autonat_reachability(status: &autonat::NatStatus) -> AutoNatReachability {
    match status {
        autonat::NatStatus::Unknown => AutoNatReachability::Unknown,
        autonat::NatStatus::Public(_) => AutoNatReachability::Public,
        autonat::NatStatus::Private => AutoNatReachability::Private,
    }
}

fn handle_kademlia_event(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    metrics: &RuntimeMetrics,
    event: kad::Event,
) {
    match event {
        kad::Event::OutboundQueryProgressed { result, .. } => {
            if let kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                providers,
                ..
            })) = &result
            {
                dial_kademlia_providers(swarm, forwarder, metrics, providers);
            }
            eprintln!("kademlia query progressed: {result:?}");
        }
        other => {
            eprintln!("kademlia event: {other:?}");
        }
    }
}

fn dial_kademlia_providers(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    metrics: &RuntimeMetrics,
    providers: &HashSet<Libp2pPeerId>,
) {
    metrics.record_kademlia_providers_found(providers.len());
    for provider in providers {
        dial_configured_peer(swarm, forwarder, metrics, *provider);
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
            metrics.record_relay_server_reservation_denied();
            eprintln!("relay server denied reservation from {src_peer_id}: {status:?}");
        }
        relay::Event::ReservationClosed { src_peer_id } => {
            metrics.record_relay_server_reservation_closed();
            eprintln!("relay server reservation closed for {src_peer_id}");
        }
        relay::Event::ReservationTimedOut { src_peer_id } => {
            metrics.record_relay_server_reservation_timed_out();
            eprintln!("relay server reservation timed out for {src_peer_id}");
        }
        relay::Event::CircuitReqDenied {
            src_peer_id,
            dst_peer_id,
            status,
        } => {
            metrics.record_relay_server_circuit_denied();
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
            metrics.record_relay_server_circuit_closed();
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
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
    address: Multiaddr,
    discovery: &DiscoveryConfig,
) {
    if peer == *swarm.local_peer_id() {
        return;
    }
    if !forwarder.is_configured_transport_peer(peer) {
        return;
    }
    if !address_targets_peer(peer, &address) {
        metrics.record_discovered_address_rejected();
        eprintln!("rejecting discovered address for {peer} with mismatched target: {address}");
        return;
    }

    metrics.record_discovered_address_accepted();
    discovered_peer_addresses.insert(peer, address.clone());

    if discovery.kademlia {
        swarm
            .behaviour_mut()
            .kad
            .add_address(&peer, address.clone());
    }
    if discovery.autonat
        && let Some(autonat) = swarm.behaviour_mut().autonat.as_mut()
    {
        autonat.add_server(peer, Some(address.clone()));
    }

    if swarm.is_connected(&peer) {
        return;
    }

    let dial_address = peer_dial_address(peer, address);
    metrics.record_discovered_address_dial_attempt();
    if let Err(error) = swarm.dial(dial_address) {
        metrics.record_discovered_address_dial_failure();
        eprintln!("dial discovered peer {peer} failed: {error}");
    }
}

fn dial_configured_peer(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    metrics: &RuntimeMetrics,
    peer: Libp2pPeerId,
) {
    if peer == *swarm.local_peer_id()
        || !forwarder.is_configured_transport_peer(peer)
        || swarm.is_connected(&peer)
    {
        return;
    }

    metrics.record_kademlia_provider_dial_attempt();
    if let Err(error) = swarm.dial(peer) {
        metrics.record_kademlia_provider_dial_failure();
        eprintln!("dial discovered provider {peer} failed: {error}");
    }
}

fn peer_dial_address(peer: Libp2pPeerId, address: Multiaddr) -> Multiaddr {
    address.with_p2p(peer).unwrap_or_else(|address| address)
}

fn address_targets_peer(peer: Libp2pPeerId, address: &Multiaddr) -> bool {
    discovered_address_target(address).is_none_or(|target| target == peer)
}

fn discovered_address_target(address: &Multiaddr) -> Option<Libp2pPeerId> {
    let mut direct_target = None;
    let mut relayed_target = None;
    let mut after_circuit = false;

    for protocol in address {
        match protocol {
            Protocol::P2p(peer) if after_circuit => relayed_target = Some(peer),
            Protocol::P2p(peer) => direct_target = Some(peer),
            Protocol::P2pCircuit => after_circuit = true,
            _ => {}
        }
    }

    if after_circuit {
        relayed_target
    } else {
        direct_target
    }
}

fn print_metrics(
    metrics: &RuntimeMetrics,
    queue: crate::queue::QueueStats,
    path: crate::path::PathRuntimeStats,
) {
    eprintln!("metrics:");
    for line in metrics.snapshot_with_paths(queue, path).lines() {
        eprintln!("  {line}");
    }
}

fn runtime_path_stats(
    forwarder: &Forwarder,
    paths: &PathSet,
    peer_capabilities: &PeerCapabilities,
) -> crate::path::PathRuntimeStats {
    paths.runtime_stats_for_peers(forwarder.configured_overlay_peers(), |peer| {
        packet_transport_support(peer_capabilities, peer)
    })
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

fn packet_rejection_drop_reason(reason: PacketRejectionReason) -> PacketDropReason {
    match reason {
        PacketRejectionReason::MalformedPacket => PacketDropReason::MalformedPacket,
        PacketRejectionReason::PacketTooLarge => PacketDropReason::PacketTooLarge,
        PacketRejectionReason::Replay => PacketDropReason::Replay,
        PacketRejectionReason::UnauthorizedPeer => PacketDropReason::UnauthorizedPeer,
        PacketRejectionReason::UnauthorizedSource => PacketDropReason::UnauthorizedSource,
        PacketRejectionReason::UnauthorizedDestination => PacketDropReason::UnauthorizedDestination,
        PacketRejectionReason::UnexpectedPayload => PacketDropReason::UnexpectedPayload,
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
            RelayConfig, ResourceConfig, RouteConfig,
        },
        route::builtin_ipv4,
        runtime::control::ControlRoute,
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

    fn config_with_peer(
        local_identity: &crate::identity::NodeIdentity,
        peer: Libp2pPeerId,
    ) -> Config {
        Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: peer.to_string(),
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
        }
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
    fn expire_outbound_queue_records_expired_drops() {
        let now = std::time::Instant::now();
        let expired_at = now
            .checked_sub(Duration::from_millis(101))
            .expect("test instant should allow subtraction");
        let mut queues = PeerQueues::with_packet_ttl(4, 4096, Duration::from_millis(100));
        queues
            .enqueue(crate::queue::Packet::new_at(
                PeerId::from_bytes([1; 32]),
                1,
                vec![1],
                expired_at,
            ))
            .expect("expired packet");
        queues
            .enqueue(crate::queue::Packet::new_at(
                PeerId::from_bytes([2; 32]),
                2,
                vec![2],
                now,
            ))
            .expect("fresh packet");
        let metrics = RuntimeMetrics::default();

        expire_outbound_queue(&mut queues, &metrics);

        let snapshot = metrics.snapshot(queues.total_stats());
        assert_eq!(snapshot.outbound_dropped_packets, 1);
        assert_eq!(snapshot.outbound_drop_queue_expired_packets, 1);
        assert_eq!(snapshot.queue.queued_packets, 1);
        assert_eq!(snapshot.queue.expired_packets, 1);
    }

    #[tokio::test]
    async fn kademlia_refresh_records_lookup_advertisement_and_bootstrap_result() {
        let discovery = DiscoveryConfig {
            mdns: false,
            kademlia: true,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        };
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
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
        .expect("node");
        let metrics = RuntimeMetrics::default();
        let key = node.kademlia_rendezvous_key.clone().expect("kademlia key");

        refresh_kademlia_rendezvous(&mut node.swarm, &key, &metrics);

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.kademlia_provider_lookups, 1);
        assert_eq!(snapshot.kademlia_provider_advertisements, 1);
        assert_eq!(snapshot.kademlia_provider_advertisement_failures, 0);
        assert_eq!(snapshot.kademlia_bootstrap_refreshes, 0);
        assert_eq!(snapshot.kademlia_bootstrap_failures, 1);
    }

    #[tokio::test]
    async fn kademlia_provider_results_dial_configured_peers_and_count_failures() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let unconfigured = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key.clone()),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
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
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity {
                peer_id: local_identity.peer_id,
                private_key: local_identity.private_key,
            },
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
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let metrics = RuntimeMetrics::default();
        let providers = HashSet::from([configured, unconfigured]);

        dial_kademlia_providers(&mut node.swarm, &forwarder, &metrics, &providers);

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.kademlia_providers_found, 2);
        assert_eq!(snapshot.kademlia_provider_dial_attempts, 1);
        assert_eq!(snapshot.kademlia_provider_dial_failures, 1);
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
            &[],
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
        let relay = peer_id();
        let configured = peer_id();
        let bootstrap_address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let relay_address: Multiaddr = "/ip4/127.0.0.1/tcp/4002".parse().expect("address");
        let peer_address: Multiaddr = "/ip4/127.0.0.1/tcp/4003".parse().expect("address");

        let targets = pending_redial_targets(
            local,
            &[(bootstrap, bootstrap_address.clone())],
            &[(relay, relay_address.clone())],
            &[(configured, peer_address.clone())],
            &[],
            |_| false,
        );

        assert_eq!(
            targets,
            RedialTargets {
                addresses: vec![
                    (bootstrap, bootstrap_address),
                    (relay, relay_address),
                    (configured, peer_address),
                ],
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
        let now = Instant::now();
        let old = now.checked_sub(Duration::from_secs(30)).expect("old time");
        let mut discovered = DiscoveredPeerAddresses::default();

        discovered.insert_at(peer, address.clone(), old);
        discovered.insert_at(peer, address.clone(), now);
        discovered.insert_at(peer, other_address.clone(), old);

        assert_eq!(
            discovered.as_vec(),
            vec![(peer, address.clone()), (peer, other_address.clone())]
        );

        assert_eq!(discovered.drop_expired(now, Duration::from_secs(10)), 1);
        assert_eq!(discovered.as_vec(), vec![(peer, address.clone())]);

        discovered.remove(peer, &address);

        assert_eq!(discovered.as_vec(), Vec::new());
    }

    #[test]
    fn discovered_address_expiry_records_metrics() {
        let peer = peer_id();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let now = Instant::now();
        let mut discovered = DiscoveredPeerAddresses::default();
        let metrics = RuntimeMetrics::default();

        discovered.insert_at(
            peer,
            address,
            now.checked_sub(DISCOVERED_ADDRESS_TTL + Duration::from_secs(1))
                .expect("expired time"),
        );

        let expired = discovered.drop_expired(now, DISCOVERED_ADDRESS_TTL);
        metrics.record_discovered_address_expired(expired);

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_expired, 1);
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
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
    fn discovered_address_target_uses_direct_or_relayed_target_peer() {
        let peer = peer_id();
        let relay = peer_id();
        let other = peer_id();
        let direct_without_target: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");
        let direct_target: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .expect("address");
        let relayed_without_target: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit")
                .parse()
                .expect("address");
        let relayed_target: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{peer}")
                .parse()
                .expect("address");
        let relayed_other_target: Multiaddr =
            format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit/p2p/{other}")
                .parse()
                .expect("address");

        assert!(address_targets_peer(peer, &direct_without_target));
        assert!(address_targets_peer(peer, &direct_target));
        assert!(address_targets_peer(peer, &relayed_without_target));
        assert!(address_targets_peer(peer, &relayed_target));
        assert!(!address_targets_peer(peer, &relayed_other_target));
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
    fn outgoing_connection_errors_are_counted() {
        let metrics = RuntimeMetrics::default();

        handle_outgoing_connection_error(&metrics, Some(peer_id()), &"dial failed");
        handle_outgoing_connection_error(&metrics, None, &"peer id unavailable");

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outgoing_connection_errors, 2);
    }

    #[tokio::test]
    async fn schedule_autonat_probe_records_enabled_probe() {
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
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
        .expect("node");
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = "/ip4/203.0.113.10/tcp/4001".parse().expect("address");

        assert!(schedule_autonat_probe(&mut node.swarm, &metrics, &address));

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.autonat_probes_scheduled, 1);
    }

    #[tokio::test]
    async fn schedule_autonat_probe_is_noop_when_disabled() {
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
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
                autonat: false,
                ..DiscoveryConfig::default()
            },
        })
        .expect("node");
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = "/ip4/203.0.113.10/tcp/4001".parse().expect("address");

        assert!(!schedule_autonat_probe(&mut node.swarm, &metrics, &address));

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.autonat_probes_scheduled, 0);
    }

    #[tokio::test]
    async fn learn_peer_address_records_accepted_address_and_dial_attempt() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
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
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut discovered = DiscoveredPeerAddresses::default();
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        learn_peer_address(
            &mut node.swarm,
            &forwarder,
            &mut discovered,
            &metrics,
            configured,
            address.clone(),
            &DiscoveryConfig::default(),
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 1);
        assert_eq!(snapshot.discovered_address_dial_attempts, 1);
        assert_eq!(snapshot.discovered_address_dial_failures, 0);
        assert_eq!(snapshot.discovered_addresses_rejected, 0);
        assert_eq!(discovered.as_vec(), vec![(configured, address)]);
    }

    #[tokio::test]
    async fn learn_peer_address_ignores_unconfigured_peers_without_metrics() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let unconfigured = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
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
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut discovered = DiscoveredPeerAddresses::default();
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().expect("address");

        learn_peer_address(
            &mut node.swarm,
            &forwarder,
            &mut discovered,
            &metrics,
            unconfigured,
            address,
            &DiscoveryConfig::default(),
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 0);
        assert_eq!(snapshot.discovered_address_dial_attempts, 0);
        assert_eq!(snapshot.discovered_address_dial_failures, 0);
        assert_eq!(snapshot.discovered_addresses_rejected, 0);
        assert!(discovered.as_vec().is_empty());
    }

    #[tokio::test]
    async fn learn_peer_address_counts_mismatched_discovered_targets_as_rejected() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let configured = peer_id();
        let other = peer_id();
        let config = config_with_peer(&local_identity, configured);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
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
        .expect("node");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut discovered = DiscoveredPeerAddresses::default();
        let metrics = RuntimeMetrics::default();
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{other}")
            .parse()
            .expect("address");

        learn_peer_address(
            &mut node.swarm,
            &forwarder,
            &mut discovered,
            &metrics,
            configured,
            address,
            &DiscoveryConfig::default(),
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.discovered_addresses_accepted, 0);
        assert_eq!(snapshot.discovered_address_dial_attempts, 0);
        assert_eq!(snapshot.discovered_address_dial_failures, 0);
        assert_eq!(snapshot.discovered_addresses_rejected, 1);
        assert!(discovered.as_vec().is_empty());
    }

    #[test]
    fn autonat_status_changes_update_reachability_metrics() {
        let metrics = RuntimeMetrics::default();
        let public_address: Multiaddr = "/ip4/203.0.113.10/tcp/4001".parse().expect("address");

        handle_autonat_event(
            &metrics,
            autonat::Event::StatusChanged {
                old: autonat::NatStatus::Unknown,
                new: autonat::NatStatus::Public(public_address),
            },
        );
        handle_autonat_event(
            &metrics,
            autonat::Event::StatusChanged {
                old: autonat::NatStatus::Public(
                    "/ip4/203.0.113.10/tcp/4001".parse().expect("address"),
                ),
                new: autonat::NatStatus::Private,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.autonat_status_unknown, 0);
        assert_eq!(snapshot.autonat_status_public, 0);
        assert_eq!(snapshot.autonat_status_private, 1);
        assert_eq!(snapshot.autonat_status_changes_to_public, 1);
        assert_eq!(snapshot.autonat_status_changes_to_private, 1);
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
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::MalformedPacket),
            PacketDropReason::MalformedPacket
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::PacketTooLarge),
            PacketDropReason::PacketTooLarge
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::Replay),
            PacketDropReason::Replay
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::UnauthorizedPeer),
            PacketDropReason::UnauthorizedPeer
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::UnauthorizedSource),
            PacketDropReason::UnauthorizedSource
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::UnauthorizedDestination),
            PacketDropReason::UnauthorizedDestination
        );
        assert_eq!(
            packet_rejection_drop_reason(PacketRejectionReason::UnexpectedPayload),
            PacketDropReason::UnexpectedPayload
        );
    }

    #[test]
    fn rejected_packet_responses_update_outbound_drop_metrics() {
        let metrics = RuntimeMetrics::default();
        let reason = packet_rejection_drop_reason(PacketRejectionReason::PacketTooLarge);

        metrics.record_outbound_failure();
        metrics.record_outbound_drop(reason);

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.outbound_failures, 1);
        assert_eq!(snapshot.outbound_dropped_packets, 1);
        assert_eq!(snapshot.outbound_drop_packet_too_large_packets, 1);
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
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        let local_capabilities = ControlCapabilities::local("lab", None, 1280);

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &ControlCapabilities::local("lab", None, 1200),
                &local_capabilities,
            ),
            ControlResponse::CapabilitiesAccepted(local_capabilities)
        );
    }

    #[test]
    fn accepted_control_response_records_capability_metrics() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        let mut peer_capabilities = PeerCapabilities::default();
        let metrics = RuntimeMetrics::default();

        handle_control_response(
            &forwarder,
            &mut peer_capabilities,
            &metrics,
            remote,
            ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("lab", None, 1200)),
            "lab",
            None,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(peer_capabilities.len(), 1);
        assert_eq!(snapshot.control_capability_accepts, 1);
        assert_eq!(snapshot.control_responses_received, 1);
        assert_eq!(snapshot.control_failures, 0);
    }

    #[test]
    fn rejected_control_response_records_rejection_reason_metrics() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        let mut peer_capabilities = PeerCapabilities::default();
        let metrics = RuntimeMetrics::default();

        handle_control_response(
            &forwarder,
            &mut peer_capabilities,
            &metrics,
            remote,
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::WrongNetwork),
            "lab",
            None,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert!(peer_capabilities.is_empty());
        assert_eq!(snapshot.control_capability_accepts, 0);
        assert_eq!(snapshot.control_capability_rejections, 1);
        assert_eq!(snapshot.control_reject_wrong_network, 1);
        assert_eq!(snapshot.control_responses_received, 1);
        assert_eq!(snapshot.control_failures, 1);
    }

    #[test]
    fn incompatible_accepted_control_response_records_response_metrics() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        let mut peer_capabilities = PeerCapabilities::default();
        let metrics = RuntimeMetrics::default();

        handle_control_response(
            &forwarder,
            &mut peer_capabilities,
            &metrics,
            remote,
            ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("prod", None, 1200)),
            "lab",
            None,
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert!(peer_capabilities.is_empty());
        assert_eq!(snapshot.control_capability_accepts, 0);
        assert_eq!(snapshot.control_capability_rejections, 1);
        assert_eq!(snapshot.control_reject_wrong_network, 1);
        assert_eq!(snapshot.control_responses_received, 1);
        assert_eq!(snapshot.control_failures, 1);
    }

    #[test]
    fn first_connection_invalidates_stale_peer_capabilities() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );

        invalidate_peer_capabilities_on_first_connection(
            &forwarder,
            &mut peer_capabilities,
            remote,
            1,
        );

        assert!(!peer_capabilities.contains(remote_overlay));
    }

    #[test]
    fn peer_capabilities_survive_until_final_connection_closes() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            remote_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );

        invalidate_peer_capabilities_when_disconnected(
            &forwarder,
            &mut peer_capabilities,
            remote,
            1,
        );
        assert!(peer_capabilities.contains(remote_overlay));

        invalidate_peer_capabilities_when_disconnected(
            &forwarder,
            &mut peer_capabilities,
            remote,
            0,
        );
        assert!(!peer_capabilities.contains(remote_overlay));
    }

    #[test]
    fn capability_response_accepts_authorized_route_advertisements() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
                routes: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 100,
                }],
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let local_capabilities = ControlCapabilities::local("lab", None, 1280);
        let remote_capabilities = ControlCapabilities::local("lab", None, 1200)
            .with_advertised_routes(vec![
                ControlRoute::new(format!("{}/32", builtin_ipv4(remote_overlay)), 0),
                ControlRoute::new("10.42.0.0/24", 100),
            ]);

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &remote_capabilities,
                &local_capabilities,
            ),
            ControlResponse::CapabilitiesAccepted(local_capabilities)
        );
    }

    #[test]
    fn capability_response_rejects_unauthorized_route_advertisements() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        let remote_capabilities = ControlCapabilities::local("lab", None, 1200)
            .with_advertised_routes(vec![ControlRoute::new("10.42.0.0/24", 0)]);

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &remote_capabilities,
                &ControlCapabilities::local("lab", None, 1280),
            ),
            ControlResponse::CapabilitiesRejected(
                ControlRejectionReason::UnauthorizedRouteAdvertisement
            )
        );
    }

    #[test]
    fn capability_response_rejects_wrong_network() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &ControlCapabilities::local("prod", None, 1280),
                &ControlCapabilities::local("lab", None, 1280),
            ),
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::WrongNetwork)
        );
    }

    #[test]
    fn capability_response_rejects_wrong_membership_tag() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &ControlCapabilities::local("lab", Some("remote-tag".to_owned()), 1280),
                &ControlCapabilities::local("lab", Some("local-tag".to_owned()), 1280),
            ),
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::MembershipMismatch)
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
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
                &ControlCapabilities::local("lab", None, 1280),
                &ControlCapabilities::local("lab", None, 1280),
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
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.packet_protocol = "/other/packet/1".to_owned();

        assert_eq!(
            capability_response_for_peer(
                &forwarder,
                remote,
                &capabilities,
                &ControlCapabilities::local("lab", None, 1280),
            ),
            ControlResponse::CapabilitiesRejected(
                ControlRejectionReason::UnsupportedPacketProtocol
            )
        );
    }

    #[tokio::test]
    async fn drain_outbound_queue_waits_for_validated_peer_capabilities() {
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
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        let peer_capabilities = PeerCapabilities::default();
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
        assert_eq!(snapshot.outbound_dropped_packets, 0);
        assert_eq!(snapshot.outbound_queue_blocked_no_supported_path_events, 1);
        assert_eq!(snapshot.queue.queued_packets, 1);
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
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        peer_capabilities.record(remote_overlay, ControlCapabilities::local("lab", None, 19));
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

    #[tokio::test]
    async fn drain_outbound_queue_waits_when_only_datagram_path_is_unsupported() {
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
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
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
        paths.record_established(remote_overlay, PathKind::DirectQuicDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.supports_quic_datagrams = true;
        peer_capabilities.record(remote_overlay, capabilities);
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
        assert_eq!(snapshot.outbound_dropped_packets, 0);
        assert_eq!(snapshot.outbound_queue_blocked_no_supported_path_events, 1);
        assert_eq!(snapshot.queue.queued_packets, 1);
    }

    #[test]
    fn runtime_path_stats_report_supported_and_blocked_configured_peers() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let stream_peer = peer_id();
        let datagram_peer = peer_id();
        let stream_overlay = PeerId::from_libp2p(stream_peer);
        let datagram_overlay = PeerId::from_libp2p(datagram_peer);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local_identity.peer_id.clone(),
                private_key: Some(local_identity.private_key),
                membership_key: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![
                PeerConfig {
                    id: stream_peer.to_string(),
                    name: None,
                    addresses: Vec::new(),
                    routes: Vec::new(),
                },
                PeerConfig {
                    id: datagram_peer.to_string(),
                    name: None,
                    addresses: Vec::new(),
                    routes: Vec::new(),
                },
            ],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut paths = PathSet::new();
        paths.record_established(stream_overlay, PathKind::DirectQuicStream);
        paths.record_established(datagram_overlay, PathKind::DirectQuicDatagram);
        let mut peer_capabilities = PeerCapabilities::default();
        peer_capabilities.record(
            stream_overlay,
            ControlCapabilities::local("lab", None, 1280),
        );
        let mut datagram_capabilities = ControlCapabilities::local("lab", None, 1280);
        datagram_capabilities.supports_quic_datagrams = true;
        peer_capabilities.record(datagram_overlay, datagram_capabilities);

        let stats = runtime_path_stats(&forwarder, &paths, &peer_capabilities);

        assert_eq!(stats.healthy_direct_quic_datagram_paths, 1);
        assert_eq!(stats.healthy_direct_quic_stream_paths, 1);
        assert_eq!(stats.peers_with_supported_path, 1);
        assert_eq!(stats.peers_without_supported_path, 1);
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
            &relay::Event::ReservationReqDenied {
                src_peer_id,
                status: relay::StatusCode::ResourceLimitExceeded,
            },
        );
        handle_relay_server_event(&metrics, &relay::Event::ReservationClosed { src_peer_id });
        handle_relay_server_event(&metrics, &relay::Event::ReservationTimedOut { src_peer_id });
        handle_relay_server_event(
            &metrics,
            &relay::Event::CircuitReqDenied {
                src_peer_id,
                dst_peer_id,
                status: relay::StatusCode::NoReservation,
            },
        );
        handle_relay_server_event(
            &metrics,
            &relay::Event::CircuitReqAccepted {
                src_peer_id,
                dst_peer_id,
            },
        );
        handle_relay_server_event(
            &metrics,
            &relay::Event::CircuitClosed {
                src_peer_id,
                dst_peer_id,
                error: None,
            },
        );

        let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
        assert_eq!(snapshot.relay_server_reservations_accepted, 1);
        assert_eq!(snapshot.relay_server_reservations_denied, 1);
        assert_eq!(snapshot.relay_server_reservations_closed, 1);
        assert_eq!(snapshot.relay_server_reservations_timed_out, 1);
        assert_eq!(snapshot.relay_server_circuits_accepted, 1);
        assert_eq!(snapshot.relay_server_circuits_denied, 1);
        assert_eq!(snapshot.relay_server_circuits_closed, 1);
    }
}
