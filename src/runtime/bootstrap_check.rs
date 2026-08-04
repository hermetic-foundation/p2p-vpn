use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use libp2p::{
    Multiaddr, PeerId as Libp2pPeerId, autonat, dcutr, multiaddr::Protocol, relay,
    swarm::SwarmEvent,
};

use crate::{
    config::{
        Config, ConfigError, DiscoveryConfig, InterfaceConfig, NetworkConfig, PacketPlaneConfig,
        PeerConfig, QueueConfig, RelayConfig, ResourceConfig,
    },
    identity::{IdentityError, NodeIdentity},
    runtime::p2p::{BehaviourEvent, HostConfig, P2pBuildError, P2pNode, build_node},
};

pub const PUBLIC_RELAY_CANDIDATE_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapCheckThreshold {
    Any,
    All,
}

#[derive(Debug)]
pub struct BootstrapCheckReport {
    pub threshold: BootstrapCheckThreshold,
    pub requirements: BootstrapCheckRequirements,
    pub kademlia_protocol: String,
    pub ipfs_compatible: bool,
    pub dcutr: BootstrapDcutrCheck,
    pub configured_bootstrap_peers: usize,
    pub connected_bootstrap_peers: usize,
    pub dial_failures: usize,
    pub configured_relay_reservations: usize,
    pub accepted_relay_reservations: usize,
    pub relayed_listen_addresses: usize,
    pub configured_relayed_peer_circuits: usize,
    pub connected_relayed_peer_circuits: usize,
    pub autonat_probe_servers_registered: usize,
    pub autonat_status: BootstrapAutoNatStatus,
    pub kademlia: BootstrapKademliaCheck,
    pub peer_results: Vec<BootstrapPeerCheck>,
    pub relay_results: Vec<RelayReservationCheck>,
    pub relayed_peer_results: Vec<RelayedPeerCircuitCheck>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BootstrapCheckRequirements {
    pub relay_reservations: bool,
    pub autonat_status: bool,
    pub dcutr_ready: bool,
    pub dcutr_success: bool,
    pub relayed_peer_circuits: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BootstrapAutoNatStatus {
    #[default]
    Unknown,
    Public,
    Private,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BootstrapDcutrCheck {
    pub enabled: bool,
    pub ready: bool,
    pub successes: usize,
    pub failures: usize,
}

#[derive(Debug, Default)]
pub struct BootstrapKademliaCheck {
    pub bootstrap_started: bool,
    pub rendezvous_lookup_started: bool,
    pub rendezvous_advertise_started: bool,
}

impl BootstrapCheckReport {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        let has_bootstrap_work = self.configured_bootstrap_peers > 0;
        let bootstrap_ok = !has_bootstrap_work
            || match self.threshold {
                BootstrapCheckThreshold::Any => self.connected_bootstrap_peers > 0,
                BootstrapCheckThreshold::All => {
                    self.connected_bootstrap_peers == self.configured_bootstrap_peers
                }
            };
        let relay_ready = relay_reservations_ready(
            self.configured_relay_reservations,
            self.accepted_relay_reservations,
            self.relays_with_listen_addresses(),
        );
        let relay_ok = !self.requirements.relay_reservations || relay_ready;
        let autonat_ok = !self.requirements.autonat_status
            || (self.autonat_probe_servers_registered > 0 && self.autonat_status.is_observed());
        let dcutr_ok = !self.requirements.dcutr_ready || self.dcutr.ready;
        let dcutr_success_ok =
            !self.requirements.dcutr_success || (self.dcutr.enabled && self.dcutr.successes > 0);

        let relayed_peer_circuits_ok = !self.requirements.relayed_peer_circuits
            || (self.configured_relayed_peer_circuits > 0
                && self.connected_relayed_peer_circuits == self.configured_relayed_peer_circuits);

        (has_bootstrap_work
            || self.requirements.relay_reservations
            || self.requirements.autonat_status
            || self.requirements.dcutr_ready
            || self.requirements.dcutr_success
            || self.requirements.relayed_peer_circuits)
            && bootstrap_ok
            && relay_ok
            && autonat_ok
            && dcutr_ok
            && dcutr_success_ok
            && relayed_peer_circuits_ok
    }

    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!(
                "bootstrap check: {}",
                if self.succeeded() { "ok" } else { "failed" }
            ),
            format!("success threshold: {}", self.threshold.as_str()),
            format!(
                "require relay reservations: {}",
                self.requirements.relay_reservations
            ),
            format!(
                "require autonat status: {}",
                self.requirements.autonat_status
            ),
            format!("require dcutr ready: {}", self.requirements.dcutr_ready),
            format!("require dcutr success: {}", self.requirements.dcutr_success),
            format!(
                "require relayed peer circuits: {}",
                self.requirements.relayed_peer_circuits
            ),
            format!("kademlia protocol: {}", self.kademlia_protocol),
            format!("ipfs compatible: {}", self.ipfs_compatible),
            format!("dcutr enabled: {}", self.dcutr.enabled),
            format!("dcutr ready: {}", self.dcutr.ready),
            format!("dcutr successes: {}", self.dcutr.successes),
            format!("dcutr failures: {}", self.dcutr.failures),
            format!(
                "kademlia bootstrap started: {}",
                self.kademlia.bootstrap_started
            ),
            format!(
                "kademlia rendezvous lookup started: {}",
                self.kademlia.rendezvous_lookup_started
            ),
            format!(
                "kademlia rendezvous advertise started: {}",
                self.kademlia.rendezvous_advertise_started
            ),
            format!(
                "autonat probe servers registered: {}",
                self.autonat_probe_servers_registered
            ),
            format!("autonat status: {}", self.autonat_status.as_str()),
            format!(
                "bootstrap peers: {} connected {} dial_failures {}",
                self.configured_bootstrap_peers, self.connected_bootstrap_peers, self.dial_failures
            ),
            format!(
                "relay reservations: {} accepted {} relayed_listen_addresses {}",
                self.configured_relay_reservations,
                self.accepted_relay_reservations,
                self.relayed_listen_addresses
            ),
            format!(
                "relayed peer circuits: {} connected {}",
                self.configured_relayed_peer_circuits, self.connected_relayed_peer_circuits
            ),
        ];

        for peer in &self.peer_results {
            lines.push(format!(
                "bootstrap peer: {} connected {} dial_failures {} last_error {} address {}",
                peer.peer_id,
                peer.connected,
                peer.dial_failures,
                peer.last_error.as_deref().unwrap_or("none"),
                peer.address
            ));
        }

        for relay in &self.relay_results {
            lines.push(format!(
                "relay reservation: {} accepted {} relayed_listen_address {} address {}",
                relay.relay_peer_id, relay.accepted, relay.relayed_listen_address, relay.address
            ));
        }

        for peer in &self.relayed_peer_results {
            lines.push(format!(
                "relayed peer circuit: {} connected {} outbound_circuit {} dial_failures {} last_error {} address {}",
                peer.peer_id,
                peer.connected,
                peer.outbound_circuit,
                peer.dial_failures,
                peer.last_error.as_deref().unwrap_or("none"),
                peer.address
            ));
        }

        lines
    }

    fn relays_with_listen_addresses(&self) -> usize {
        self.relay_results
            .iter()
            .filter(|relay| relay.relayed_listen_address)
            .count()
    }
}

impl BootstrapCheckThreshold {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
        }
    }
}

impl BootstrapAutoNatStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Public => "public",
            Self::Private => "private",
        }
    }

    const fn is_observed(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

#[derive(Debug)]
pub struct BootstrapPeerCheck {
    pub peer_id: Libp2pPeerId,
    pub address: String,
    pub connected: bool,
    pub dial_failures: usize,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub struct RelayReservationCheck {
    pub relay_peer_id: Libp2pPeerId,
    pub address: String,
    pub accepted: bool,
    pub relayed_listen_address: bool,
}

#[derive(Debug)]
pub struct RelayedPeerCircuitCheck {
    pub peer_id: Libp2pPeerId,
    pub address: String,
    pub connected: bool,
    pub outbound_circuit: bool,
    pub dial_failures: usize,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicRelayProbeMode {
    RelayedPeerCircuit,
    DcutrSuccess,
}

#[derive(Debug)]
pub struct PublicRelayProbeReport {
    pub mode: PublicRelayProbeMode,
    pub candidates: Vec<PublicRelayCandidateReport>,
}

#[derive(Debug)]
pub struct PublicRelayCandidateReport {
    pub address: String,
    pub succeeded: bool,
    pub error: Option<String>,
    pub bootstrap: Option<BootstrapCheckReport>,
}

impl PublicRelayProbeReport {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.candidates.iter().any(|candidate| candidate.succeeded)
    }

    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let succeeded = self
            .candidates
            .iter()
            .filter(|candidate| candidate.succeeded)
            .count();
        let mut lines = vec![
            format!(
                "public relay probe: {}",
                if self.succeeded() { "ok" } else { "failed" }
            ),
            format!("public relay probe mode: {}", self.mode.as_str()),
            format!(
                "public relay candidates: {} succeeded {}",
                self.candidates.len(),
                succeeded
            ),
        ];

        for candidate in &self.candidates {
            lines.push(format!(
                "public relay candidate: {} succeeded {} error {}",
                candidate.address,
                candidate.succeeded,
                candidate.error.as_deref().unwrap_or("none")
            ));
            if candidate.succeeded
                && let Some(report) = &candidate.bootstrap
            {
                for line in report.lines() {
                    lines.push(format!("public relay candidate detail: {line}"));
                }
            }
        }

        lines
    }
}

impl PublicRelayProbeMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::RelayedPeerCircuit => "relayed_peer_circuit",
            Self::DcutrSuccess => "dcutr_success",
        }
    }
}

pub async fn check_config_bootstrap(
    config: &Config,
    timeout: Duration,
    threshold: BootstrapCheckThreshold,
    requirements: BootstrapCheckRequirements,
) -> Result<BootstrapCheckReport, BootstrapCheckError> {
    config.validate_runtime()?;
    let mut node = build_node(&bootstrap_check_host_config(config)?)?;
    let bootstrap_peers = node.bootstrap_peer_addresses.clone();
    let relay_reservations = node.relay_peer_addresses.clone();
    let relayed_peers = relayed_peer_addresses(&node.configured_peer_addresses);
    let poll = poll_bootstrap_events(
        &mut node,
        &bootstrap_peers,
        &relay_reservations,
        &relayed_peers,
        timeout,
        threshold,
        requirements,
    )
    .await;
    let dcutr_ready = node.startup.dcutr_enabled
        && relay_reservations_ready(
            relay_reservations.len(),
            poll.accepted_relay_reservations.len(),
            relays_with_listen_addresses(&relay_reservations, &poll.relayed_listen_addresses),
        );

    Ok(BootstrapCheckReport {
        threshold,
        requirements,
        kademlia_protocol: node.discovery.kademlia_protocol.clone(),
        ipfs_compatible: node.discovery.kademlia_protocol == "/ipfs/kad/1.0.0",
        dcutr: BootstrapDcutrCheck {
            enabled: node.startup.dcutr_enabled,
            ready: dcutr_ready,
            successes: poll.dcutr_successes,
            failures: poll.dcutr_failures,
        },
        configured_bootstrap_peers: bootstrap_peers.len(),
        connected_bootstrap_peers: poll.connected_bootstrap_peers.len(),
        dial_failures: poll.dial_failures.len(),
        configured_relay_reservations: relay_reservations.len(),
        accepted_relay_reservations: poll.accepted_relay_reservations.len(),
        relayed_listen_addresses: poll.relayed_listen_addresses.len(),
        configured_relayed_peer_circuits: relayed_peers.len(),
        connected_relayed_peer_circuits: poll.connected_relayed_peers.len(),
        autonat_probe_servers_registered: node.startup.autonat_servers_registered,
        autonat_status: poll.autonat_status,
        kademlia: BootstrapKademliaCheck {
            bootstrap_started: node.startup.kademlia.bootstrap_started,
            rendezvous_lookup_started: node.startup.kademlia.rendezvous_lookup_started,
            rendezvous_advertise_started: node.startup.kademlia.rendezvous_advertise_started,
        },
        peer_results: bootstrap_peer_results(bootstrap_peers, &poll),
        relay_results: relay_reservation_results(relay_reservations, &poll),
        relayed_peer_results: relayed_peer_results(relayed_peers, &poll),
    })
}

pub fn parse_public_relay_addresses(raw: &str) -> Result<Vec<Multiaddr>, String> {
    let mut addresses = Vec::new();
    for candidate in raw
        .split([',', ';', '\n'])
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
    {
        if addresses.len() == PUBLIC_RELAY_CANDIDATE_LIMIT {
            return Err(format!(
                "too many public relay candidates: maximum is {PUBLIC_RELAY_CANDIDATE_LIMIT}"
            ));
        }
        let address = candidate
            .parse::<Multiaddr>()
            .map_err(|error| format!("{candidate}: {error}"))?;
        if address_peer(&address).is_none() {
            return Err(format!("{candidate}: missing /p2p/RELAY"));
        }
        if relay_peer_from_relayed_address(&address).is_some() {
            return Err(format!(
                "{candidate}: relay candidate must be the relay's direct address, without /p2p-circuit"
            ));
        }
        addresses.push(address);
    }
    Ok(addresses)
}

pub async fn check_public_relay_candidates(
    relay_addresses: &[Multiaddr],
    mode: PublicRelayProbeMode,
    timeout: Duration,
) -> PublicRelayProbeReport {
    let mut candidates = Vec::new();
    for relay_address in relay_addresses {
        let result = match mode {
            PublicRelayProbeMode::RelayedPeerCircuit => {
                Box::pin(live_public_relayed_peer_circuit(relay_address, timeout)).await
            }
            PublicRelayProbeMode::DcutrSuccess => {
                Box::pin(live_public_dcutr_success(relay_address, timeout)).await
            }
        };
        let candidate = match result {
            Ok(report) => PublicRelayCandidateReport {
                address: relay_address.to_string(),
                succeeded: true,
                error: None,
                bootstrap: Some(report),
            },
            Err(error) => PublicRelayCandidateReport {
                address: relay_address.to_string(),
                succeeded: false,
                error: Some(error),
                bootstrap: None,
            },
        };
        let succeeded = candidate.succeeded;
        candidates.push(candidate);
        if succeeded {
            break;
        }
    }

    PublicRelayProbeReport { mode, candidates }
}

fn bootstrap_peer_results(
    bootstrap_peers: Vec<(Libp2pPeerId, libp2p::Multiaddr)>,
    poll: &BootstrapPollResult,
) -> Vec<BootstrapPeerCheck> {
    bootstrap_peers
        .into_iter()
        .map(|(peer_id, address)| BootstrapPeerCheck {
            peer_id,
            address: address.to_string(),
            connected: poll.connected_bootstrap_peers.contains(&peer_id),
            dial_failures: poll
                .dial_failures
                .iter()
                .filter(|(failed_peer, _)| *failed_peer == peer_id)
                .count(),
            last_error: poll
                .dial_failures
                .iter()
                .rev()
                .find_map(|(failed_peer, error)| (*failed_peer == peer_id).then(|| error.clone())),
        })
        .collect()
}

fn relay_reservation_results(
    relay_reservations: Vec<(Libp2pPeerId, libp2p::Multiaddr)>,
    poll: &BootstrapPollResult,
) -> Vec<RelayReservationCheck> {
    relay_reservations
        .into_iter()
        .map(|(relay_peer_id, address)| RelayReservationCheck {
            relay_peer_id,
            address: address.to_string(),
            accepted: poll.accepted_relay_reservations.contains(&relay_peer_id),
            relayed_listen_address: poll.relayed_listen_addresses.contains_key(&relay_peer_id),
        })
        .collect()
}

fn relayed_peer_results(
    relayed_peers: Vec<(Libp2pPeerId, libp2p::Multiaddr)>,
    poll: &BootstrapPollResult,
) -> Vec<RelayedPeerCircuitCheck> {
    relayed_peers
        .into_iter()
        .map(|(peer_id, address)| {
            let relay_peer = relay_peer_from_relayed_address(&address);
            RelayedPeerCircuitCheck {
                peer_id,
                address: address.to_string(),
                connected: poll.connected_relayed_peers.contains(&peer_id),
                outbound_circuit: relay_peer.is_some_and(|relay| {
                    poll.outbound_circuit_relays.contains(&relay)
                        || poll.connected_relayed_peers.contains(&peer_id)
                }),
                dial_failures: poll
                    .relayed_peer_dial_failures
                    .iter()
                    .filter(|(failed_peer, _)| *failed_peer == peer_id)
                    .count(),
                last_error: poll.relayed_peer_dial_failures.iter().rev().find_map(
                    |(failed_peer, error)| (*failed_peer == peer_id).then(|| error.clone()),
                ),
            }
        })
        .collect()
}

async fn live_public_relayed_peer_circuit(
    relay_address: &Multiaddr,
    timeout: Duration,
) -> Result<BootstrapCheckReport, String> {
    let relay_peer = address_peer(relay_address)
        .ok_or_else(|| "live relay multiaddr must include /p2p/RELAY".to_owned())?;
    let relay_reservation = relay_address.to_owned().with(Protocol::P2pCircuit);
    let discovery = relay_probe_discovery();
    let mut listener_node = build_node(&HostConfig {
        identity: NodeIdentity::generate_ed25519().map_err(|error| format!("{error:?}"))?,
        network_name: "lab".to_owned(),
        membership_tag: None,
        mtu: 1280,
        max_concurrent_control_streams: 64,
        max_concurrent_packet_streams: 256,
        listen_addresses: Vec::new(),
        external_addresses: Vec::new(),
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: vec![relay_reservation.clone()],
        relay_server: false,
        relay_resources: crate::config::RelayResourceConfig::default(),
        resources: ResourceConfig::default(),
        discovery: discovery.clone(),
    })
    .map_err(|error| format!("{error:?}"))?;
    let listener_peer = listener_node.local_peer_id;
    let relayed_target_address = relay_reservation.with(Protocol::P2p(listener_peer));

    tokio::time::timeout(
        timeout,
        wait_for_external_relay_reservation(
            &mut listener_node,
            relayed_target_address.clone(),
            relay_peer,
        ),
    )
    .await
    .map_err(|_| "relay reservation timed out".to_owned())?;

    let _listener_task = tokio::spawn(async move {
        loop {
            let _ = listener_node.swarm.select_next_some().await;
        }
    });

    let config = relay_probe_config_with_relayed_peer(listener_peer, &relayed_target_address)?;
    let report = check_config_bootstrap(
        &config,
        timeout,
        BootstrapCheckThreshold::Any,
        BootstrapCheckRequirements {
            relay_reservations: false,
            autonat_status: false,
            dcutr_ready: false,
            dcutr_success: false,
            relayed_peer_circuits: true,
        },
    )
    .await
    .map_err(|error| format!("{error:?}"))?;

    if report.succeeded() {
        Ok(report)
    } else {
        Err(format!("{report:?}"))
    }
}

async fn live_public_dcutr_success(
    relay_address: &Multiaddr,
    timeout: Duration,
) -> Result<BootstrapCheckReport, String> {
    let relay_peer = address_peer(relay_address)
        .ok_or_else(|| "live relay multiaddr must include /p2p/RELAY".to_owned())?;
    let relay_reservation = relay_address.to_owned().with(Protocol::P2pCircuit);
    let discovery = dcutr_probe_discovery();
    let mut listener_node = build_node(&HostConfig {
        identity: NodeIdentity::generate_ed25519().map_err(|error| format!("{error:?}"))?,
        network_name: "lab".to_owned(),
        membership_tag: None,
        mtu: 1280,
        max_concurrent_control_streams: 64,
        max_concurrent_packet_streams: 256,
        listen_addresses: vec!["/ip4/0.0.0.0/tcp/0".parse().expect("listen address")],
        external_addresses: Vec::new(),
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: vec![relay_reservation.clone()],
        relay_server: false,
        relay_resources: crate::config::RelayResourceConfig::default(),
        resources: ResourceConfig::default(),
        discovery,
    })
    .map_err(|error| format!("{error:?}"))?;
    let listener_peer = listener_node.local_peer_id;
    let relayed_target_address = relay_reservation.with(Protocol::P2p(listener_peer));

    tokio::time::timeout(
        timeout,
        wait_for_external_relay_reservation(
            &mut listener_node,
            relayed_target_address.clone(),
            relay_peer,
        ),
    )
    .await
    .map_err(|_| "relay reservation timed out".to_owned())?;

    let _listener_task = tokio::spawn(async move {
        loop {
            let _ = listener_node.swarm.select_next_some().await;
        }
    });

    let mut config = relay_probe_config_with_relayed_peer_discovery(
        listener_peer,
        &relayed_target_address,
        dcutr_probe_discovery(),
    )?;
    config.network.listen_addresses = vec!["/ip4/0.0.0.0/tcp/0".to_owned()];
    let report = check_config_bootstrap(
        &config,
        timeout,
        BootstrapCheckThreshold::Any,
        BootstrapCheckRequirements {
            relay_reservations: false,
            autonat_status: false,
            dcutr_ready: false,
            dcutr_success: true,
            relayed_peer_circuits: true,
        },
    )
    .await
    .map_err(|error| format!("{error:?}"))?;

    if report.succeeded() {
        Ok(report)
    } else {
        Err(format!("{report:?}"))
    }
}

async fn wait_for_external_relay_reservation(
    listener: &mut P2pNode,
    relayed_address: Multiaddr,
    relay_peer: Libp2pPeerId,
) {
    let mut listen_addr_reported = false;
    let mut reservation_accepted = false;

    loop {
        match listener.swarm.select_next_some().await {
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

        if listen_addr_reported && reservation_accepted {
            return;
        }
    }
}

fn address_peer(address: &Multiaddr) -> Option<Libp2pPeerId> {
    address.iter().find_map(|protocol| match protocol {
        Protocol::P2p(peer) => Some(peer),
        _ => None,
    })
}

fn relay_probe_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        mdns: false,
        kademlia: false,
        kademlia_provider_advertisement: false,
        kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
        dcutr: false,
        autonat: false,
    }
}

fn dcutr_probe_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        dcutr: true,
        autonat: true,
        ..relay_probe_discovery()
    }
}

fn relay_probe_config_with_relayed_peer(
    peer: Libp2pPeerId,
    address: &Multiaddr,
) -> Result<Config, String> {
    relay_probe_config_with_relayed_peer_discovery(peer, address, relay_probe_discovery())
}

fn relay_probe_config_with_relayed_peer_discovery(
    peer: Libp2pPeerId,
    address: &Multiaddr,
    discovery: DiscoveryConfig,
) -> Result<Config, String> {
    let identity = NodeIdentity::generate_ed25519().map_err(|error| format!("{error:?}"))?;
    Ok(Config {
        network: NetworkConfig {
            name: "lab".to_owned(),
            local_peer: identity.peer_id.clone(),
            private_key: Some(identity.private_key),
            membership_key: None,
            previous_membership_tags: Vec::new(),
            routes: Vec::new(),
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            discovery,
            relay: RelayConfig::default(),
            packet_plane: PacketPlaneConfig::default(),
        },
        interface: InterfaceConfig {
            name: "hs0".to_owned(),
            mtu: 1280,
        },
        peers: vec![PeerConfig {
            id: peer.to_string(),
            name: Some("relay-probe-listener".to_owned()),
            addresses: vec![address.to_string()],
            routes: Vec::new(),
        }],
        queue: QueueConfig::default(),
        resources: ResourceConfig::default(),
    })
}

fn bootstrap_check_host_config(config: &Config) -> Result<HostConfig, BootstrapCheckError> {
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

async fn poll_bootstrap_events(
    node: &mut P2pNode,
    bootstrap_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    relay_reservations: &[(Libp2pPeerId, libp2p::Multiaddr)],
    relayed_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    timeout: Duration,
    threshold: BootstrapCheckThreshold,
    requirements: BootstrapCheckRequirements,
) -> BootstrapPollResult {
    let mut result = BootstrapPollResult {
        connected_bootstrap_peers: bootstrap_peers
            .iter()
            .filter_map(|(peer, _)| node.swarm.is_connected(peer).then_some(*peer))
            .collect(),
        ..BootstrapPollResult::default()
    };
    if requirements.relayed_peer_circuits || requirements.dcutr_success {
        dial_relayed_peer_targets(node, relayed_peers, &mut result);
    }
    let deadline = Instant::now() + timeout.max(Duration::from_millis(1));

    while should_continue_polling(PollingStatus {
        threshold,
        configured_bootstrap_peers: bootstrap_peers.len(),
        connected_bootstrap_peers: result.connected_bootstrap_peers.len(),
        requirements,
        configured_relay_reservations: relay_reservations.len(),
        accepted_relay_reservations: result.accepted_relay_reservations.len(),
        relayed_listen_addresses: result.relayed_listen_addresses.len(),
        configured_relayed_peer_circuits: relayed_peers.len(),
        connected_relayed_peer_circuits: result.connected_relayed_peers.len(),
        dcutr_successes: result.dcutr_successes,
        autonat_probe_servers_registered: node.startup.autonat_servers_registered,
        autonat_status: result.autonat_status,
        now: Instant::now(),
        deadline,
    }) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = tokio::time::timeout(remaining, node.swarm.select_next_some()).await else {
            break;
        };
        record_bootstrap_event(
            event,
            bootstrap_peers,
            relay_reservations,
            relayed_peers,
            &mut result,
        );
    }

    result
}

fn dial_relayed_peer_targets(
    node: &mut P2pNode,
    relayed_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    result: &mut BootstrapPollResult,
) {
    for (peer, address) in relayed_peers {
        let dial_address = match peer_dial_address(*peer, address.clone()) {
            Ok(address) => address,
            Err(address) => {
                result
                    .relayed_peer_dial_failures
                    .push((*peer, format!("address lacks valid /p2p/{peer}: {address}")));
                continue;
            }
        };

        if let Err(error) = node.swarm.dial(dial_address) {
            result
                .relayed_peer_dial_failures
                .push((*peer, format!("{error:?}")));
        }
    }
}

fn record_bootstrap_event(
    event: SwarmEvent<BehaviourEvent>,
    bootstrap_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    relay_reservations: &[(Libp2pPeerId, libp2p::Multiaddr)],
    relayed_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    result: &mut BootstrapPollResult,
) {
    match event {
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => {
            if bootstrap_peers.iter().any(|(peer, _)| *peer == peer_id) {
                result.connected_bootstrap_peers.insert(peer_id);
            }
            if endpoint.is_relayed() && relayed_peers.iter().any(|(peer, _)| *peer == peer_id) {
                result.connected_relayed_peers.insert(peer_id);
            }
        }
        SwarmEvent::OutgoingConnectionError {
            peer_id: Some(peer_id),
            error,
            ..
        } => {
            if bootstrap_peers.iter().any(|(peer, _)| *peer == peer_id) {
                result.dial_failures.push((peer_id, format!("{error:?}")));
            }
            if relayed_peers.iter().any(|(peer, _)| *peer == peer_id) {
                result
                    .relayed_peer_dial_failures
                    .push((peer_id, format!("{error:?}")));
            }
        }
        SwarmEvent::Behaviour(BehaviourEvent::Relay(
            relay::client::Event::ReservationReqAccepted {
                relay_peer_id,
                renewal: false,
                ..
            },
        )) if relay_reservations
            .iter()
            .any(|(peer, _)| *peer == relay_peer_id) =>
        {
            result.accepted_relay_reservations.insert(relay_peer_id);
        }
        SwarmEvent::Behaviour(BehaviourEvent::Relay(
            relay::client::Event::OutboundCircuitEstablished { relay_peer_id, .. },
        )) => {
            result.outbound_circuit_relays.insert(relay_peer_id);
        }
        SwarmEvent::Behaviour(BehaviourEvent::Dcutr(dcutr::Event { result: event, .. })) => {
            if event.is_ok() {
                result.dcutr_successes += 1;
            } else {
                result.dcutr_failures += 1;
            }
        }
        SwarmEvent::NewListenAddr { address, .. } => {
            if let Some(relay_peer) = relay_peer_from_relayed_address(&address)
                && relay_reservations
                    .iter()
                    .any(|(peer, _)| *peer == relay_peer)
            {
                result.relayed_listen_addresses.insert(relay_peer, address);
            }
        }
        SwarmEvent::Behaviour(BehaviourEvent::Autonat(autonat::Event::StatusChanged {
            new,
            ..
        })) => {
            result.autonat_status = BootstrapAutoNatStatus::from_nat_status(&new);
        }
        _ => {}
    }
}

#[derive(Debug, Default)]
struct BootstrapPollResult {
    connected_bootstrap_peers: HashSet<Libp2pPeerId>,
    dial_failures: Vec<(Libp2pPeerId, String)>,
    accepted_relay_reservations: HashSet<Libp2pPeerId>,
    relayed_listen_addresses: HashMap<Libp2pPeerId, libp2p::Multiaddr>,
    connected_relayed_peers: HashSet<Libp2pPeerId>,
    outbound_circuit_relays: HashSet<Libp2pPeerId>,
    relayed_peer_dial_failures: Vec<(Libp2pPeerId, String)>,
    dcutr_successes: usize,
    dcutr_failures: usize,
    autonat_status: BootstrapAutoNatStatus,
}

#[derive(Clone, Copy, Debug)]
struct PollingStatus {
    threshold: BootstrapCheckThreshold,
    configured_bootstrap_peers: usize,
    connected_bootstrap_peers: usize,
    requirements: BootstrapCheckRequirements,
    configured_relay_reservations: usize,
    accepted_relay_reservations: usize,
    relayed_listen_addresses: usize,
    configured_relayed_peer_circuits: usize,
    connected_relayed_peer_circuits: usize,
    dcutr_successes: usize,
    autonat_probe_servers_registered: usize,
    autonat_status: BootstrapAutoNatStatus,
    now: Instant,
    deadline: Instant,
}

fn should_continue_polling(status: PollingStatus) -> bool {
    if (status.configured_bootstrap_peers == 0
        && !status.requirements.relay_reservations
        && !status.requirements.autonat_status
        && !status.requirements.dcutr_ready
        && !status.requirements.dcutr_success
        && !status.requirements.relayed_peer_circuits)
        || status.now >= status.deadline
    {
        return false;
    }

    let bootstrap_waiting = status.configured_bootstrap_peers > 0
        && match status.threshold {
            BootstrapCheckThreshold::Any => status.connected_bootstrap_peers == 0,
            BootstrapCheckThreshold::All => {
                status.connected_bootstrap_peers < status.configured_bootstrap_peers
            }
        };
    let relay_waiting = (status.requirements.relay_reservations || status.requirements.dcutr_ready)
        && status.configured_relay_reservations > 0
        && (status.accepted_relay_reservations < status.configured_relay_reservations
            || status.relayed_listen_addresses < status.configured_relay_reservations);
    let autonat_waiting = status.requirements.autonat_status
        && status.autonat_probe_servers_registered > 0
        && !status.autonat_status.is_observed();
    let relayed_peer_waiting = status.requirements.relayed_peer_circuits
        && status.configured_relayed_peer_circuits > 0
        && status.connected_relayed_peer_circuits < status.configured_relayed_peer_circuits;
    let dcutr_success_waiting = status.requirements.dcutr_success && status.dcutr_successes == 0;

    bootstrap_waiting
        || relay_waiting
        || autonat_waiting
        || relayed_peer_waiting
        || dcutr_success_waiting
}

const fn relay_reservations_ready(
    configured_relay_reservations: usize,
    accepted_relay_reservations: usize,
    relays_with_listen_addresses: usize,
) -> bool {
    configured_relay_reservations > 0
        && accepted_relay_reservations == configured_relay_reservations
        && relays_with_listen_addresses == configured_relay_reservations
}

fn relays_with_listen_addresses(
    relay_reservations: &[(Libp2pPeerId, libp2p::Multiaddr)],
    relayed_listen_addresses: &HashMap<Libp2pPeerId, libp2p::Multiaddr>,
) -> usize {
    relay_reservations
        .iter()
        .filter(|(relay, _)| relayed_listen_addresses.contains_key(relay))
        .count()
}

impl BootstrapAutoNatStatus {
    fn from_nat_status(status: &autonat::NatStatus) -> Self {
        match status {
            autonat::NatStatus::Unknown => Self::Unknown,
            autonat::NatStatus::Public(_) => Self::Public,
            autonat::NatStatus::Private => Self::Private,
        }
    }
}

fn relay_peer_from_relayed_address(address: &libp2p::Multiaddr) -> Option<Libp2pPeerId> {
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

fn relayed_peer_addresses(
    configured_peer_addresses: &[(Libp2pPeerId, libp2p::Multiaddr)],
) -> Vec<(Libp2pPeerId, libp2p::Multiaddr)> {
    configured_peer_addresses
        .iter()
        .filter(|(_, address)| relay_peer_from_relayed_address(address).is_some())
        .cloned()
        .collect()
}

fn peer_dial_address(
    peer: Libp2pPeerId,
    address: libp2p::Multiaddr,
) -> Result<libp2p::Multiaddr, libp2p::Multiaddr> {
    if address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2p(address_peer) if address_peer == peer))
    {
        return Ok(address);
    }

    address.with_p2p(peer)
}

#[derive(Debug)]
pub enum BootstrapCheckError {
    Config(ConfigError),
    Identity(IdentityError),
    Build(P2pBuildError),
}

impl From<ConfigError> for BootstrapCheckError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<IdentityError> for BootstrapCheckError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<P2pBuildError> for BootstrapCheckError {
    fn from(error: P2pBuildError) -> Self {
        Self::Build(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{env, time::Duration};

    use futures::StreamExt as _;
    use libp2p::{Multiaddr, PeerId as Libp2pPeerId, identity::Keypair, swarm::SwarmEvent};

    use super::*;
    use crate::{
        config::{
            BootstrapPeerConfig, Config, DiscoveryConfig, InterfaceConfig, NetworkConfig,
            PacketPlaneConfig, PeerConfig, QueueConfig, RelayConfig, ResourceConfig,
        },
        identity::NodeIdentity,
        runtime::p2p::{HostConfig, build_node},
    };

    const LIVE_RELAY_MULTIADDR_ENV: &str = "P2P_VPN_LIVE_RELAY_MULTIADDR";
    const LIVE_RELAY_MULTIADDRS_ENV: &str = "P2P_VPN_LIVE_RELAY_MULTIADDRS";

    #[tokio::test]
    async fn bootstrap_check_connects_to_configured_bootstrap_peer() {
        let mut bootstrap = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("bootstrap identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("bootstrap node");
        let bootstrap_peer = bootstrap.local_peer_id;
        let bootstrap_address = next_listen_address(&mut bootstrap).await;
        let _bootstrap_task = tokio::spawn(async move {
            loop {
                let _ = bootstrap.swarm.select_next_some().await;
            }
        });
        let config = config_with_bootstrap_peer(bootstrap_peer, &bootstrap_address);

        let report = check_config_bootstrap(
            &config,
            Duration::from_secs(5),
            BootstrapCheckThreshold::Any,
            BootstrapCheckRequirements::default(),
        )
        .await
        .expect("bootstrap check");

        assert!(report.succeeded());
        assert_eq!(report.configured_bootstrap_peers, 1);
        assert_eq!(report.connected_bootstrap_peers, 1);
        assert!(report.lines().contains(&"bootstrap check: ok".to_owned()));
    }

    #[tokio::test]
    async fn bootstrap_check_can_require_relay_reservation_acceptance() {
        let mut relay_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: DiscoveryConfig::default(),
        })
        .expect("relay node");
        let relay_peer = relay_node.local_peer_id;
        let relay_address = next_listen_address(&mut relay_node).await;
        relay_node.swarm.add_external_address(relay_address.clone());
        let relay_reservation = relay_address
            .clone()
            .with_p2p(relay_peer)
            .expect("relay address")
            .with(Protocol::P2pCircuit);
        let _relay_task = tokio::spawn(async move {
            loop {
                let _ = relay_node.swarm.select_next_some().await;
            }
        });
        let config = config_with_relay_reservation(&relay_reservation);

        let report = check_config_bootstrap(
            &config,
            Duration::from_secs(5),
            BootstrapCheckThreshold::Any,
            BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: false,
                dcutr_ready: true,
                dcutr_success: false,
                relayed_peer_circuits: false,
            },
        )
        .await
        .expect("bootstrap check");

        assert!(report.succeeded(), "{report:?}");
        assert_eq!(report.configured_bootstrap_peers, 0);
        assert_eq!(report.configured_relay_reservations, 1);
        assert_eq!(report.accepted_relay_reservations, 1);
        assert_eq!(report.relayed_listen_addresses, 1);
        assert!(report.dcutr.enabled);
        assert!(report.dcutr.ready);
        assert!(report.lines().contains(&"bootstrap check: ok".to_owned()));
    }

    #[tokio::test]
    async fn bootstrap_check_can_probe_relayed_peer_circuit() {
        let discovery = relay_test_discovery();
        let mut relay_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: discovery.clone(),
        })
        .expect("relay node");
        let relay_peer = relay_node.local_peer_id;
        let relay_address = next_listen_address(&mut relay_node).await;
        relay_node.swarm.add_external_address(relay_address.clone());
        let relay_reservation = relay_address
            .clone()
            .with_p2p(relay_peer)
            .expect("relay address")
            .with(Protocol::P2pCircuit);

        let mut listener_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("listener identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: vec![relay_reservation.clone()],
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery,
        })
        .expect("listener node");
        let listener_peer = listener_node.local_peer_id;
        let relayed_target_address = relay_reservation.clone().with(Protocol::P2p(listener_peer));

        tokio::time::timeout(
            Duration::from_secs(10),
            wait_for_relay_reservation(
                &mut relay_node,
                &mut listener_node,
                relayed_target_address.clone(),
                relay_peer,
            ),
        )
        .await
        .expect("relay reservation timed out");

        let _relay_task = tokio::spawn(async move {
            loop {
                let _ = relay_node.swarm.select_next_some().await;
            }
        });
        let _listener_task = tokio::spawn(async move {
            loop {
                let _ = listener_node.swarm.select_next_some().await;
            }
        });

        let config = config_with_relayed_peer(listener_peer, &relayed_target_address);
        let report = check_config_bootstrap(
            &config,
            Duration::from_secs(10),
            BootstrapCheckThreshold::Any,
            BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: false,
                dcutr_ready: false,
                dcutr_success: false,
                relayed_peer_circuits: true,
            },
        )
        .await
        .expect("bootstrap check");

        assert!(report.succeeded(), "{report:?}");
        assert_eq!(report.configured_relayed_peer_circuits, 1);
        assert_eq!(report.connected_relayed_peer_circuits, 1);
        assert_eq!(report.relayed_peer_results.len(), 1);
        assert!(report.relayed_peer_results[0].connected);
        assert!(report.relayed_peer_results[0].outbound_circuit);
    }

    #[tokio::test]
    async fn public_relay_probe_can_validate_local_relay_candidate() {
        let mut relay_node = build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().expect("relay identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".parse().expect("listen address")],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: true,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: relay_test_discovery(),
        })
        .expect("relay node");
        let relay_peer = relay_node.local_peer_id;
        let relay_address = next_listen_address(&mut relay_node)
            .await
            .with_p2p(relay_peer)
            .expect("relay address");
        relay_node.swarm.add_external_address(relay_address.clone());
        let _relay_task = tokio::spawn(async move {
            loop {
                let _ = relay_node.swarm.select_next_some().await;
            }
        });

        let report = check_public_relay_candidates(
            &[relay_address],
            PublicRelayProbeMode::RelayedPeerCircuit,
            Duration::from_secs(10),
        )
        .await;

        assert!(report.succeeded(), "{report:?}");
        assert_eq!(report.candidates.len(), 1);
        assert!(report.candidates[0].succeeded);
        assert!(
            report
                .lines()
                .contains(&"public relay probe: ok".to_owned())
        );
    }

    #[tokio::test]
    #[ignore = "requires P2P_VPN_LIVE_RELAY_MULTIADDRS or P2P_VPN_LIVE_RELAY_MULTIADDR for a reachable public libp2p relay"]
    async fn bootstrap_check_can_probe_live_public_relayed_peer_circuit() {
        let relay_addresses = live_relay_addresses();
        if relay_addresses.is_empty() {
            eprintln!(
                "skipping live public relay smoke: P2P_VPN_LIVE_RELAY_MULTIADDRS and P2P_VPN_LIVE_RELAY_MULTIADDR are not set"
            );
            return;
        }

        let mut failures = Vec::new();
        for relay_address in relay_addresses {
            let report = check_public_relay_candidates(
                std::slice::from_ref(&relay_address),
                PublicRelayProbeMode::RelayedPeerCircuit,
                Duration::from_secs(45),
            )
            .await;
            if report.succeeded() {
                eprintln!("live public relay circuit smoke passed through {relay_address}");
                return;
            }
            failures.push(report.lines().join("\n"));
        }

        panic!(
            "no live public relay candidate completed relayed peer circuit smoke:\n{}",
            failures.join("\n")
        );
    }

    #[tokio::test]
    #[ignore = "requires P2P_VPN_LIVE_RELAY_MULTIADDRS or P2P_VPN_LIVE_RELAY_MULTIADDR and a network path where DCUtR can complete"]
    async fn bootstrap_check_can_probe_live_public_dcutr_success() {
        let relay_addresses = live_relay_addresses();
        if relay_addresses.is_empty() {
            eprintln!(
                "skipping live public DCUtR smoke: P2P_VPN_LIVE_RELAY_MULTIADDRS and P2P_VPN_LIVE_RELAY_MULTIADDR are not set"
            );
            return;
        }

        let mut failures = Vec::new();
        for relay_address in relay_addresses {
            let report = check_public_relay_candidates(
                std::slice::from_ref(&relay_address),
                PublicRelayProbeMode::DcutrSuccess,
                Duration::from_secs(45),
            )
            .await;
            if report.succeeded() {
                eprintln!("live public DCUtR smoke passed through {relay_address}");
                return;
            }
            failures.push(report.lines().join("\n"));
        }

        panic!(
            "no live public relay candidate completed DCUtR smoke:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn bootstrap_check_lines_report_ipfs_compatible_thresholds() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let report = BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::All,
            requirements: BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: true,
                dcutr_ready: true,
                dcutr_success: true,
                relayed_peer_circuits: true,
            },
            kademlia_protocol: "/ipfs/kad/1.0.0".to_owned(),
            ipfs_compatible: true,
            dcutr: BootstrapDcutrCheck {
                enabled: true,
                ready: false,
                successes: 0,
                failures: 1,
            },
            configured_bootstrap_peers: 2,
            connected_bootstrap_peers: 1,
            dial_failures: 1,
            configured_relay_reservations: 1,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            configured_relayed_peer_circuits: 1,
            connected_relayed_peer_circuits: 0,
            autonat_probe_servers_registered: 2,
            autonat_status: BootstrapAutoNatStatus::Private,
            kademlia: BootstrapKademliaCheck {
                bootstrap_started: true,
                rendezvous_lookup_started: true,
                rendezvous_advertise_started: true,
            },
            peer_results: vec![BootstrapPeerCheck {
                peer_id: peer,
                address: "/dnsaddr/bootstrap.libp2p.io".to_owned(),
                connected: true,
                dial_failures: 0,
                last_error: None,
            }],
            relay_results: vec![RelayReservationCheck {
                relay_peer_id: peer,
                address: "/dns4/relay.example.net/tcp/4001".to_owned(),
                accepted: false,
                relayed_listen_address: false,
            }],
            relayed_peer_results: vec![RelayedPeerCircuitCheck {
                peer_id: peer,
                address: "/dns4/relay.example.net/tcp/4001/p2p/example/p2p-circuit".to_owned(),
                connected: false,
                outbound_circuit: false,
                dial_failures: 1,
                last_error: Some("dial failed".to_owned()),
            }],
        };

        let lines = report.lines();

        assert!(!report.succeeded());
        assert!(lines.contains(&"bootstrap check: failed".to_owned()));
        assert!(lines.contains(&"success threshold: all".to_owned()));
        assert!(lines.contains(&"require relay reservations: true".to_owned()));
        assert!(lines.contains(&"require autonat status: true".to_owned()));
        assert!(lines.contains(&"require dcutr ready: true".to_owned()));
        assert!(lines.contains(&"require dcutr success: true".to_owned()));
        assert!(lines.contains(&"require relayed peer circuits: true".to_owned()));
        assert!(lines.contains(&"ipfs compatible: true".to_owned()));
        assert!(lines.contains(&"dcutr enabled: true".to_owned()));
        assert!(lines.contains(&"dcutr ready: false".to_owned()));
        assert!(lines.contains(&"dcutr successes: 0".to_owned()));
        assert!(lines.contains(&"dcutr failures: 1".to_owned()));
        assert!(
            lines.contains(
                &"relay reservations: 1 accepted 0 relayed_listen_addresses 0".to_owned()
            )
        );
        assert!(lines.contains(&"autonat probe servers registered: 2".to_owned()));
        assert!(lines.contains(&"autonat status: private".to_owned()));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("accepted false relayed_listen_address false"))
        );
        assert!(lines.iter().any(|line| line.contains("last_error none")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("relayed peer circuit:")
                    && line.contains("connected false")
                    && line.contains("dial_failures 1"))
        );
    }

    #[test]
    fn live_relay_candidate_parser_accepts_multiple_addresses() {
        let relay_a = peer_id();
        let relay_b = peer_id();
        let raw = format!(
            "/dns4/relay-a.example.net/tcp/4001/p2p/{relay_a},\n/ip4/203.0.113.10/tcp/4001/p2p/{relay_b}"
        );

        let relays = parse_public_relay_addresses(&raw).expect("relay candidates");

        assert_eq!(relays.len(), 2);
        assert_eq!(address_peer(&relays[0]), Some(relay_a));
        assert_eq!(address_peer(&relays[1]), Some(relay_b));
        assert!(
            parse_public_relay_addresses("/dns4/relay.example.net/tcp/4001")
                .expect_err("missing peer id should fail")
                .contains("missing /p2p/RELAY")
        );
        assert!(
            parse_public_relay_addresses(&format!(
                "/dns4/relay.example.net/tcp/4001/p2p/{relay_a}/p2p-circuit"
            ))
            .expect_err("relayed address should fail")
            .contains("without /p2p-circuit")
        );

        let too_many = (0..=PUBLIC_RELAY_CANDIDATE_LIMIT)
            .map(|_| format!("/dns4/relay.example.net/tcp/4001/p2p/{}", peer_id()))
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            parse_public_relay_addresses(&too_many)
                .expect_err("candidate limit should fail")
                .contains("too many public relay candidates")
        );
    }

    #[test]
    fn bootstrap_check_can_require_observed_autonat_status() {
        assert!(autonat_report(1, BootstrapAutoNatStatus::Private).succeeded());
        assert!(autonat_report(1, BootstrapAutoNatStatus::Public).succeeded());
        assert!(!autonat_report(1, BootstrapAutoNatStatus::Unknown).succeeded());
        assert!(!autonat_report(0, BootstrapAutoNatStatus::Private).succeeded());
    }

    #[test]
    fn bootstrap_check_can_require_dcutr_ready_state() {
        assert!(dcutr_report(true, 1, 1, 1).succeeded());
        assert!(!dcutr_report(false, 1, 1, 1).succeeded());
        assert!(!dcutr_report(true, 1, 0, 1).succeeded());
        assert!(!dcutr_report(true, 1, 1, 0).succeeded());
        assert!(!dcutr_report(true, 0, 0, 0).succeeded());
    }

    #[test]
    fn bootstrap_check_can_require_dcutr_success_event() {
        assert!(dcutr_success_report(true, 1, 0).succeeded());
        assert!(!dcutr_success_report(true, 0, 0).succeeded());
        assert!(!dcutr_success_report(true, 0, 1).succeeded());
        assert!(!dcutr_success_report(false, 1, 0).succeeded());
    }

    #[test]
    fn bootstrap_check_can_require_relayed_peer_circuits() {
        assert!(relayed_peer_report(1, 1).succeeded());
        assert!(!relayed_peer_report(1, 0).succeeded());
        assert!(!relayed_peer_report(0, 0).succeeded());
    }

    #[test]
    fn relay_readiness_requires_each_relay_to_have_listen_address() {
        let relay_a = peer_id();
        let relay_b = peer_id();
        let report = relay_report(vec![
            RelayReservationCheck {
                relay_peer_id: relay_a,
                address: format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay_a}/p2p-circuit"),
                accepted: true,
                relayed_listen_address: true,
            },
            RelayReservationCheck {
                relay_peer_id: relay_b,
                address: format!("/ip4/127.0.0.1/tcp/4002/p2p/{relay_b}/p2p-circuit"),
                accepted: true,
                relayed_listen_address: false,
            },
        ]);

        assert_eq!(report.accepted_relay_reservations, 2);
        assert_eq!(report.relayed_listen_addresses, 2);
        assert!(!report.succeeded());
    }

    async fn next_listen_address(node: &mut crate::runtime::p2p::P2pNode) -> Multiaddr {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = node.swarm.select_next_some().await {
                return address;
            }
        }
    }

    async fn wait_for_relay_reservation(
        relay: &mut crate::runtime::p2p::P2pNode,
        listener: &mut crate::runtime::p2p::P2pNode,
        relayed_address: Multiaddr,
        relay_peer: Libp2pPeerId,
    ) {
        let mut listen_addr_reported = false;
        let mut reservation_accepted = false;

        loop {
            tokio::select! {
                event = relay.swarm.select_next_some() => {
                    let _ = event;
                }
                event = listener.swarm.select_next_some() => {
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

    fn live_relay_addresses() -> Vec<Multiaddr> {
        let raw = env::var(LIVE_RELAY_MULTIADDRS_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env::var(LIVE_RELAY_MULTIADDR_ENV).ok());
        let Some(raw) = raw else {
            return Vec::new();
        };

        parse_public_relay_addresses(&raw)
            .expect("live relay multiaddr candidates must parse and include /p2p/RELAY")
    }

    fn relay_test_discovery() -> DiscoveryConfig {
        DiscoveryConfig {
            mdns: false,
            kademlia: false,
            kademlia_provider_advertisement: false,
            kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
            dcutr: false,
            autonat: false,
        }
    }

    fn config_with_bootstrap_peer(peer: Libp2pPeerId, address: &Multiaddr) -> Config {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: vec![BootstrapPeerConfig {
                    id: peer.to_string(),
                    address: address.to_string(),
                }],
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        }
    }

    fn config_with_relay_reservation(reservation: &Multiaddr) -> Config {
        let mut config = config_with_bootstrap_peer(peer_id(), &"/memory/9".parse().expect("addr"));
        config.network.bootstrap_peers = Vec::new();
        config.network.relay.reservations = vec![reservation.to_string()];
        config
    }

    fn config_with_relayed_peer(peer: Libp2pPeerId, address: &Multiaddr) -> Config {
        config_with_relayed_peer_discovery(peer, address, relay_test_discovery())
    }

    fn config_with_relayed_peer_discovery(
        peer: Libp2pPeerId,
        address: &Multiaddr,
        discovery: DiscoveryConfig,
    ) -> Config {
        let mut config = config_with_bootstrap_peer(peer_id(), &"/memory/9".parse().expect("addr"));
        config.network.bootstrap_peers = Vec::new();
        config.network.discovery = discovery;
        config.peers = vec![PeerConfig {
            id: peer.to_string(),
            name: Some("listener".to_owned()),
            addresses: vec![address.to_string()],
            routes: Vec::new(),
        }];
        config
    }

    fn peer_id() -> Libp2pPeerId {
        Keypair::generate_ed25519().public().to_peer_id()
    }

    fn autonat_report(
        autonat_probe_servers_registered: usize,
        autonat_status: BootstrapAutoNatStatus,
    ) -> BootstrapCheckReport {
        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: true,
                dcutr_ready: false,
                dcutr_success: false,
                relayed_peer_circuits: false,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck {
                enabled: false,
                ready: false,
                successes: 0,
                failures: 0,
            },
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations: 0,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            configured_relayed_peer_circuits: 0,
            connected_relayed_peer_circuits: 0,
            autonat_probe_servers_registered,
            autonat_status,
            kademlia: BootstrapKademliaCheck::default(),
            peer_results: Vec::new(),
            relay_results: Vec::new(),
            relayed_peer_results: Vec::new(),
        }
    }

    fn relay_report(relay_results: Vec<RelayReservationCheck>) -> BootstrapCheckReport {
        let configured_relay_reservations = relay_results.len();
        let accepted_relay_reservations =
            relay_results.iter().filter(|relay| relay.accepted).count();
        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: false,
                dcutr_ready: false,
                dcutr_success: false,
                relayed_peer_circuits: false,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck::default(),
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations,
            accepted_relay_reservations,
            relayed_listen_addresses: configured_relay_reservations,
            configured_relayed_peer_circuits: 0,
            connected_relayed_peer_circuits: 0,
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            kademlia: BootstrapKademliaCheck::default(),
            peer_results: Vec::new(),
            relay_results,
            relayed_peer_results: Vec::new(),
        }
    }

    fn dcutr_report(
        dcutr_enabled: bool,
        configured_relay_reservations: usize,
        accepted_relay_reservations: usize,
        relayed_listen_addresses: usize,
    ) -> BootstrapCheckReport {
        let relay_results: Vec<_> = (0..configured_relay_reservations)
            .map(|index| {
                let relay_peer_id = peer_id();
                RelayReservationCheck {
                    relay_peer_id,
                    address: format!("/memory/{}/p2p/{relay_peer_id}/p2p-circuit", index + 10),
                    accepted: index < accepted_relay_reservations,
                    relayed_listen_address: index < relayed_listen_addresses,
                }
            })
            .collect();
        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: false,
                dcutr_ready: true,
                dcutr_success: false,
                relayed_peer_circuits: false,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck {
                enabled: dcutr_enabled,
                ready: dcutr_enabled
                    && relay_reservations_ready(
                        configured_relay_reservations,
                        accepted_relay_reservations,
                        relayed_listen_addresses,
                    ),
                successes: 0,
                failures: 0,
            },
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations,
            accepted_relay_reservations,
            relayed_listen_addresses,
            configured_relayed_peer_circuits: 0,
            connected_relayed_peer_circuits: 0,
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            kademlia: BootstrapKademliaCheck::default(),
            peer_results: Vec::new(),
            relay_results,
            relayed_peer_results: Vec::new(),
        }
    }

    fn dcutr_success_report(
        dcutr_enabled: bool,
        dcutr_successes: usize,
        dcutr_failures: usize,
    ) -> BootstrapCheckReport {
        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: false,
                dcutr_ready: false,
                dcutr_success: true,
                relayed_peer_circuits: false,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck {
                enabled: dcutr_enabled,
                ready: false,
                successes: dcutr_successes,
                failures: dcutr_failures,
            },
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations: 0,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            configured_relayed_peer_circuits: 0,
            connected_relayed_peer_circuits: 0,
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            kademlia: BootstrapKademliaCheck::default(),
            peer_results: Vec::new(),
            relay_results: Vec::new(),
            relayed_peer_results: Vec::new(),
        }
    }

    fn relayed_peer_report(
        configured_relayed_peer_circuits: usize,
        connected_relayed_peer_circuits: usize,
    ) -> BootstrapCheckReport {
        let relayed_peer_results: Vec<_> = (0..configured_relayed_peer_circuits)
            .map(|index| {
                let target_peer = peer_id();
                let relay_peer = peer_id();
                RelayedPeerCircuitCheck {
                    peer_id: target_peer,
                    address: format!(
                        "/memory/{}/p2p/{relay_peer}/p2p-circuit/p2p/{target_peer}",
                        index + 20
                    ),
                    connected: index < connected_relayed_peer_circuits,
                    outbound_circuit: index < connected_relayed_peer_circuits,
                    dial_failures: usize::from(index >= connected_relayed_peer_circuits),
                    last_error: (index >= connected_relayed_peer_circuits)
                        .then(|| "dial failed".to_owned()),
                }
            })
            .collect();

        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: false,
                dcutr_ready: false,
                dcutr_success: false,
                relayed_peer_circuits: true,
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck::default(),
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations: 0,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            configured_relayed_peer_circuits,
            connected_relayed_peer_circuits,
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            kademlia: BootstrapKademliaCheck::default(),
            peer_results: Vec::new(),
            relay_results: Vec::new(),
            relayed_peer_results,
        }
    }
}
