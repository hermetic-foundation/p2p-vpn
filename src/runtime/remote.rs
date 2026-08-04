use std::time::Duration;

use futures::StreamExt as _;
use libp2p::{
    PeerId as Libp2pPeerId,
    request_response::{self, Message, OutboundRequestId},
    swarm::SwarmEvent,
};

use crate::{
    config::{Config, ConfigError},
    identity::IdentityError,
    runtime::{
        control::{
            ControlCapabilities, ControlRejectionReason, ControlRequest, ControlResponse,
            accepted_capabilities_response, rejected_capabilities_response, validate_capabilities,
        },
        forward::Forwarder,
        p2p::{BehaviourEvent, HostConfig, P2pBuildError, build_node},
        service::{
            ServiceRejectionReason, ServiceRequest, ServiceResponse, ServiceStatusRequest,
            ServiceStatusResponse, validate_status_request, validate_status_response,
        },
    },
};

const REMOTE_STATUS_NONCE: u64 = 0x7076_706e_7374_6174;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemotePeerStatus {
    pub peer: Libp2pPeerId,
    pub capabilities: ControlCapabilities,
    pub service: ServiceStatusResponse,
}

pub async fn query_peer_status(
    config: &Config,
    peer: Libp2pPeerId,
    timeout: Duration,
) -> Result<RemotePeerStatus, RemoteQueryError> {
    let mut node = build_node(&remote_query_host_config(config)?)?;
    node.packet_endpoint_candidates = config.packet_plane_endpoint_candidates()?;
    let forwarder = Forwarder::from_config(config)?;
    if !forwarder.is_configured_transport_peer(peer) {
        return Err(RemoteQueryError::UnconfiguredPeer(peer));
    }
    let previous_membership_tags = config.previous_membership_tags()?;

    let local_capabilities = ControlCapabilities::local(
        &node.network_name,
        node.membership_tag.clone(),
        config.effective_packet_mtu(),
    )
    .with_packet_endpoint_candidates(node.packet_endpoint_candidates.clone())
    .with_advertised_routes(forwarder.local_advertised_routes());
    let expected_network = local_capabilities.network_name.clone();
    let expected_membership_tag = local_capabilities.membership_tag.clone();
    let packet_plane_session_ttl = config.network.packet_plane.session_ttl();
    let packet_plane_replay_windows_per_session = config.network.packet_plane.replay_window_limit();

    let query = async move {
        let mut control_request = None;
        let mut service_request = None;
        let mut capabilities = None;
        let mut service = None;

        loop {
            if control_request.is_none() && node.swarm.is_connected(&peer) {
                control_request = Some(send_capabilities_request(
                    &mut node.swarm,
                    peer,
                    &local_capabilities,
                ));
            }
            if service_request.is_none() && node.swarm.is_connected(&peer) {
                service_request = Some(send_status_request(
                    &mut node.swarm,
                    peer,
                    &local_capabilities,
                ));
            }

            if let (Some(capabilities), Some(service)) = (capabilities.clone(), service.clone()) {
                return Ok(RemotePeerStatus {
                    peer,
                    capabilities,
                    service,
                });
            }

            match node.swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Control(event)) => {
                    handle_control_event(
                        &mut node.swarm,
                        &forwarder,
                        &local_capabilities,
                        &expected_network,
                        expected_membership_tag.as_deref(),
                        &previous_membership_tags,
                        peer,
                        control_request,
                        event,
                        &mut capabilities,
                    )?;
                }
                SwarmEvent::Behaviour(BehaviourEvent::Service(event)) => {
                    handle_service_event(
                        &mut node.swarm,
                        &forwarder,
                        &local_capabilities,
                        &expected_network,
                        expected_membership_tag.as_deref(),
                        &previous_membership_tags,
                        peer,
                        service_request,
                        REMOTE_STATUS_NONCE,
                        packet_plane_session_ttl,
                        packet_plane_replay_windows_per_session,
                        event,
                        &mut service,
                    )?;
                }
                _ => {}
            }
        }
    };

    tokio::time::timeout(timeout, query)
        .await
        .map_err(|_| RemoteQueryError::TimedOut)?
}

fn remote_query_host_config(config: &Config) -> Result<HostConfig, RemoteQueryError> {
    Ok(HostConfig {
        identity: config.identity()?,
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
    })
}

fn send_capabilities_request(
    swarm: &mut libp2p::Swarm<crate::runtime::p2p::Behaviour>,
    peer: Libp2pPeerId,
    local_capabilities: &ControlCapabilities,
) -> OutboundRequestId {
    swarm.behaviour_mut().control.send_request(
        &peer,
        ControlRequest::Capabilities(local_capabilities.clone()),
    )
}

fn send_status_request(
    swarm: &mut libp2p::Swarm<crate::runtime::p2p::Behaviour>,
    peer: Libp2pPeerId,
    local_capabilities: &ControlCapabilities,
) -> OutboundRequestId {
    swarm.behaviour_mut().service.send_request(
        &peer,
        ServiceRequest::Status(ServiceStatusRequest::local(
            &local_capabilities.network_name,
            local_capabilities.membership_tag.clone(),
            REMOTE_STATUS_NONCE,
        )),
    )
}

#[allow(clippy::too_many_arguments)]
fn handle_control_event(
    swarm: &mut libp2p::Swarm<crate::runtime::p2p::Behaviour>,
    forwarder: &Forwarder,
    local_capabilities: &ControlCapabilities,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
    target_peer: Libp2pPeerId,
    expected_request: Option<OutboundRequestId>,
    event: request_response::Event<ControlRequest, ControlResponse>,
    capabilities: &mut Option<ControlCapabilities>,
) -> Result<(), RemoteQueryError> {
    match event {
        request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        } => {
            let response = inbound_capability_response(
                forwarder,
                peer,
                request,
                local_capabilities,
                previous_membership_tags,
            );
            swarm
                .behaviour_mut()
                .control
                .send_response(channel, response)
                .map_err(|_| RemoteQueryError::ResponseDropped)?;
        }
        request_response::Event::Message {
            peer,
            message:
                Message::Response {
                    request_id,
                    response,
                },
            ..
        } if peer == target_peer && Some(request_id) == expected_request => match response {
            ControlResponse::CapabilitiesAccepted(remote) => {
                if let Some(reason) = validate_capabilities(
                    &remote,
                    expected_network,
                    expected_membership_tag,
                    previous_membership_tags,
                ) {
                    return Err(RemoteQueryError::RejectedCapabilities(reason));
                }
                if !forwarder.authorizes_advertised_routes(peer, &remote.advertised_routes) {
                    return Err(RemoteQueryError::RejectedCapabilities(
                        ControlRejectionReason::UnauthorizedRouteAdvertisement,
                    ));
                }
                *capabilities = Some(remote);
            }
            ControlResponse::CapabilitiesRejected(reason) => {
                return Err(RemoteQueryError::RejectedCapabilities(reason));
            }
            ControlResponse::PacketPlaneAccepted(_) | ControlResponse::PacketPlaneRejected(_) => {
                return Err(RemoteQueryError::ControlFailure(
                    "unexpected packet-plane negotiation response".to_owned(),
                ));
            }
        },
        request_response::Event::OutboundFailure { peer, error, .. } if peer == target_peer => {
            return Err(RemoteQueryError::ControlFailure(error.to_string()));
        }
        request_response::Event::InboundFailure { error, .. } => {
            return Err(RemoteQueryError::ControlFailure(error.to_string()));
        }
        _ => {}
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_service_event(
    swarm: &mut libp2p::Swarm<crate::runtime::p2p::Behaviour>,
    forwarder: &Forwarder,
    local_capabilities: &ControlCapabilities,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
    target_peer: Libp2pPeerId,
    expected_request: Option<OutboundRequestId>,
    expected_nonce: u64,
    packet_plane_session_ttl: Duration,
    packet_plane_replay_windows_per_session: usize,
    event: request_response::Event<ServiceRequest, ServiceResponse>,
    service: &mut Option<ServiceStatusResponse>,
) -> Result<(), RemoteQueryError> {
    match event {
        request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        } => {
            let response = inbound_service_response(
                forwarder,
                peer,
                request,
                local_capabilities,
                previous_membership_tags,
                packet_plane_session_ttl,
                packet_plane_replay_windows_per_session,
            );
            swarm
                .behaviour_mut()
                .service
                .send_response(channel, response)
                .map_err(|_| RemoteQueryError::ResponseDropped)?;
        }
        request_response::Event::Message {
            peer,
            message:
                Message::Response {
                    request_id,
                    response,
                },
            ..
        } if peer == target_peer && Some(request_id) == expected_request => match response {
            ServiceResponse::Status(remote) => {
                if let Some(reason) = validate_status_response(
                    &remote,
                    expected_network,
                    expected_membership_tag,
                    previous_membership_tags,
                ) {
                    return Err(RemoteQueryError::RejectedServiceStatus(reason));
                }
                if remote.nonce != expected_nonce {
                    return Err(RemoteQueryError::ServiceNonceMismatch {
                        expected: expected_nonce,
                        actual: remote.nonce,
                    });
                }
                *service = Some(remote);
            }
            ServiceResponse::Rejected(reason) => {
                return Err(RemoteQueryError::RejectedServiceStatus(reason));
            }
        },
        request_response::Event::OutboundFailure { peer, error, .. } if peer == target_peer => {
            return Err(RemoteQueryError::ServiceFailure(error.to_string()));
        }
        request_response::Event::InboundFailure { error, .. } => {
            return Err(RemoteQueryError::ServiceFailure(error.to_string()));
        }
        _ => {}
    }

    Ok(())
}

fn inbound_capability_response(
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    request: ControlRequest,
    local_capabilities: &ControlCapabilities,
    previous_membership_tags: &[String],
) -> ControlResponse {
    match request {
        ControlRequest::Capabilities(capabilities) => {
            if !forwarder.is_configured_transport_peer(peer) {
                return rejected_capabilities_response(ControlRejectionReason::UnauthorizedPeer);
            }
            if let Some(reason) = validate_capabilities(
                &capabilities,
                &local_capabilities.network_name,
                local_capabilities.membership_tag.as_deref(),
                previous_membership_tags,
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
        ControlRequest::PacketPlaneHello(_) => {
            ControlResponse::PacketPlaneRejected(ControlRejectionReason::UnsupportedPreferredPath)
        }
    }
}

fn inbound_service_response(
    forwarder: &Forwarder,
    peer: Libp2pPeerId,
    request: ServiceRequest,
    local_capabilities: &ControlCapabilities,
    previous_membership_tags: &[String],
    packet_plane_session_ttl: Duration,
    packet_plane_replay_windows_per_session: usize,
) -> ServiceResponse {
    match request {
        ServiceRequest::Status(request) => {
            if !forwarder.is_configured_transport_peer(peer) {
                return ServiceResponse::Rejected(ServiceRejectionReason::UnauthorizedPeer);
            }
            if let Some(reason) = validate_status_request(
                &request,
                &local_capabilities.network_name,
                local_capabilities.membership_tag.as_deref(),
                previous_membership_tags,
            ) {
                return ServiceResponse::Rejected(reason);
            }
            ServiceResponse::Status(
                ServiceStatusResponse::local(
                    &local_capabilities.network_name,
                    local_capabilities.membership_tag.clone(),
                    request.nonce,
                    local_capabilities.effective_mtu,
                )
                .with_packet_plane_session_ttl_seconds(packet_plane_session_ttl.as_secs())
                .with_packet_plane_replay_windows_per_session(
                    packet_plane_replay_windows_per_session,
                ),
            )
        }
    }
}

#[derive(Debug)]
pub enum RemoteQueryError {
    Config(ConfigError),
    Identity(IdentityError),
    P2p(P2pBuildError),
    Forwarder(crate::runtime::forward::ForwardError),
    UnconfiguredPeer(Libp2pPeerId),
    TimedOut,
    ResponseDropped,
    RejectedCapabilities(ControlRejectionReason),
    RejectedServiceStatus(ServiceRejectionReason),
    ServiceNonceMismatch { expected: u64, actual: u64 },
    ControlFailure(String),
    ServiceFailure(String),
}

impl From<ConfigError> for RemoteQueryError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<IdentityError> for RemoteQueryError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<P2pBuildError> for RemoteQueryError {
    fn from(error: P2pBuildError) -> Self {
        Self::P2p(error)
    }
}

impl From<crate::runtime::forward::ForwardError> for RemoteQueryError {
    fn from(error: crate::runtime::forward::ForwardError) -> Self {
        Self::Forwarder(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{
            DiscoveryConfig, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig, RelayConfig,
            ResourceConfig,
        },
        identity::NodeIdentity,
        runtime::p2p::{Behaviour, P2pNode},
    };

    #[tokio::test]
    async fn query_peer_status_exchanges_live_control_and_service_status() {
        let listener_identity = NodeIdentity::generate_ed25519().expect("listener identity");
        let client_identity = NodeIdentity::generate_ed25519().expect("client identity");
        let client_peer = client_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("client peer id");
        let listener_peer = listener_identity
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("listener peer id");
        let mut listener_config = test_config(
            listener_identity.clone(),
            vec![PeerConfig {
                id: client_identity.peer_id.clone(),
                name: Some("client".to_owned()),
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
        );
        listener_config.network.packet_plane.session_ttl_seconds = 123;
        listener_config
            .network
            .packet_plane
            .max_replay_windows_per_session = 321;
        let mut listener = build_node(&HostConfig {
            identity: listener_identity,
            network_name: listener_config.network.name.clone(),
            membership_tag: None,
            mtu: listener_config.effective_packet_mtu(),
            max_concurrent_control_streams: listener_config.resources.control_stream_limit(),
            max_concurrent_packet_streams: listener_config.resources.packet_stream_limit(),
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: listener_config.network.relay.resources,
            resources: listener_config.resources,
            discovery: test_discovery(),
        })
        .expect("listener node");
        let listener_address = next_listen_address(&mut listener.swarm).await;
        let client_config = test_config(
            client_identity,
            vec![PeerConfig {
                id: listener_peer.to_string(),
                name: Some("listener".to_owned()),
                addresses: vec![listener_address.to_string()],
                routes: Vec::new(),
            }],
        );

        let status = Box::pin(tokio::time::timeout(Duration::from_secs(10), async {
            let mut server = Box::pin(serve_status_queries(listener, listener_config));
            let mut query = Box::pin(query_peer_status(
                &client_config,
                listener_peer,
                Duration::from_secs(10),
            ));

            tokio::select! {
                result = &mut query => result,
                result = &mut server => panic!("status server exited unexpectedly: {result:?}"),
            }
        }))
        .await
        .expect("remote status query timed out");

        let status = status.expect("remote status");
        assert_eq!(status.peer, listener_peer);
        assert_eq!(status.capabilities.network_name, "lab");
        assert_eq!(status.service.network_name, "lab");
        assert_eq!(status.service.effective_mtu, 1280);
        assert_eq!(status.service.packet_plane_session_ttl_seconds, Some(123));
        assert_eq!(
            status.service.packet_plane_replay_windows_per_session,
            Some(321)
        );
        assert!(status.capabilities.advertised_routes.len() >= 2);
        assert_ne!(client_peer, listener_peer);
    }

    async fn serve_status_queries(
        mut node: P2pNode,
        config: Config,
    ) -> Result<(), RemoteQueryError> {
        let forwarder = Forwarder::from_config(&config)?;
        let packet_plane_session_ttl = config.network.packet_plane.session_ttl();
        let packet_plane_replay_windows_per_session =
            config.network.packet_plane.replay_window_limit();
        let local_capabilities =
            ControlCapabilities::local(&node.network_name, None, config.effective_packet_mtu())
                .with_advertised_routes(forwarder.local_advertised_routes());

        loop {
            match node.swarm.select_next_some().await {
                SwarmEvent::Behaviour(BehaviourEvent::Control(
                    request_response::Event::Message {
                        peer,
                        message:
                            Message::Request {
                                request, channel, ..
                            },
                        ..
                    },
                )) => {
                    let response = inbound_capability_response(
                        &forwarder,
                        peer,
                        request,
                        &local_capabilities,
                        &[],
                    );
                    node.swarm
                        .behaviour_mut()
                        .control
                        .send_response(channel, response)
                        .map_err(|_| RemoteQueryError::ResponseDropped)?;
                }
                SwarmEvent::Behaviour(BehaviourEvent::Service(
                    request_response::Event::Message {
                        peer,
                        message:
                            Message::Request {
                                request, channel, ..
                            },
                        ..
                    },
                )) => {
                    let response = inbound_service_response(
                        &forwarder,
                        peer,
                        request,
                        &local_capabilities,
                        &[],
                        packet_plane_session_ttl,
                        packet_plane_replay_windows_per_session,
                    );
                    node.swarm
                        .behaviour_mut()
                        .service
                        .send_response(channel, response)
                        .map_err(|_| RemoteQueryError::ResponseDropped)?;
                }
                _ => {}
            }
        }
    }

    async fn next_listen_address(swarm: &mut libp2p::Swarm<Behaviour>) -> libp2p::Multiaddr {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                return address;
            }
        }
    }

    fn test_config(identity: NodeIdentity, peers: Vec<PeerConfig>) -> Config {
        Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: identity.peer_id,
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: test_discovery(),
                relay: RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers,
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        }
    }

    fn test_discovery() -> DiscoveryConfig {
        DiscoveryConfig {
            mdns: false,
            kademlia: false,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        }
    }
}
