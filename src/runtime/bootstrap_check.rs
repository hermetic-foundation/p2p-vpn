use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use libp2p::{PeerId as Libp2pPeerId, autonat, multiaddr::Protocol, relay, swarm::SwarmEvent};

use crate::{
    config::{Config, ConfigError},
    identity::IdentityError,
    runtime::p2p::{BehaviourEvent, HostConfig, P2pBuildError, P2pNode, build_node},
};

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
    pub autonat_probe_servers_registered: usize,
    pub autonat_status: BootstrapAutoNatStatus,
    pub kademlia: BootstrapKademliaCheck,
    pub peer_results: Vec<BootstrapPeerCheck>,
    pub relay_results: Vec<RelayReservationCheck>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BootstrapCheckRequirements {
    pub relay_reservations: bool,
    pub autonat_status: bool,
    pub dcutr_ready: bool,
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
            self.relayed_listen_addresses,
        );
        let relay_ok = !self.requirements.relay_reservations || relay_ready;
        let autonat_ok = !self.requirements.autonat_status
            || (self.autonat_probe_servers_registered > 0 && self.autonat_status.is_observed());
        let dcutr_ok = !self.requirements.dcutr_ready || self.dcutr.ready;

        (has_bootstrap_work
            || self.requirements.relay_reservations
            || self.requirements.autonat_status
            || self.requirements.dcutr_ready)
            && bootstrap_ok
            && relay_ok
            && autonat_ok
            && dcutr_ok
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
            format!("kademlia protocol: {}", self.kademlia_protocol),
            format!("ipfs compatible: {}", self.ipfs_compatible),
            format!("dcutr enabled: {}", self.dcutr.enabled),
            format!("dcutr ready: {}", self.dcutr.ready),
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
                "relay reservation: {} accepted {} address {}",
                relay.relay_peer_id, relay.accepted, relay.address
            ));
        }

        lines
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
    let poll = poll_bootstrap_events(
        &mut node,
        &bootstrap_peers,
        &relay_reservations,
        timeout,
        threshold,
        requirements,
    )
    .await;
    let dcutr_ready = node.startup.dcutr_enabled
        && relay_reservations_ready(
            relay_reservations.len(),
            poll.accepted_relay_reservations.len(),
            poll.relayed_listen_addresses.len(),
        );

    Ok(BootstrapCheckReport {
        threshold,
        requirements,
        kademlia_protocol: node.discovery.kademlia_protocol.clone(),
        ipfs_compatible: node.discovery.kademlia_protocol == "/ipfs/kad/1.0.0",
        dcutr: BootstrapDcutrCheck {
            enabled: node.startup.dcutr_enabled,
            ready: dcutr_ready,
        },
        configured_bootstrap_peers: bootstrap_peers.len(),
        connected_bootstrap_peers: poll.connected_bootstrap_peers.len(),
        dial_failures: poll.dial_failures.len(),
        configured_relay_reservations: relay_reservations.len(),
        accepted_relay_reservations: poll.accepted_relay_reservations.len(),
        relayed_listen_addresses: poll.relayed_listen_addresses.len(),
        autonat_probe_servers_registered: node.startup.autonat_servers_registered,
        autonat_status: poll.autonat_status,
        kademlia: BootstrapKademliaCheck {
            bootstrap_started: node.startup.kademlia.bootstrap_started,
            rendezvous_lookup_started: node.startup.kademlia.rendezvous_lookup_started,
            rendezvous_advertise_started: node.startup.kademlia.rendezvous_advertise_started,
        },
        peer_results: bootstrap_peers
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
                    .find_map(|(failed_peer, error)| {
                        (*failed_peer == peer_id).then(|| error.clone())
                    }),
            })
            .collect(),
        relay_results: relay_reservations
            .into_iter()
            .map(|(relay_peer_id, address)| RelayReservationCheck {
                relay_peer_id,
                address: address.to_string(),
                accepted: poll.accepted_relay_reservations.contains(&relay_peer_id),
            })
            .collect(),
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
    let deadline = Instant::now() + timeout.max(Duration::from_millis(1));

    while should_continue_polling(PollingStatus {
        threshold,
        configured_bootstrap_peers: bootstrap_peers.len(),
        connected_bootstrap_peers: result.connected_bootstrap_peers.len(),
        requirements,
        configured_relay_reservations: relay_reservations.len(),
        accepted_relay_reservations: result.accepted_relay_reservations.len(),
        relayed_listen_addresses: result.relayed_listen_addresses.len(),
        autonat_probe_servers_registered: node.startup.autonat_servers_registered,
        autonat_status: result.autonat_status,
        now: Instant::now(),
        deadline,
    }) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = tokio::time::timeout(remaining, node.swarm.select_next_some()).await else {
            break;
        };
        record_bootstrap_event(event, bootstrap_peers, relay_reservations, &mut result);
    }

    result
}

fn record_bootstrap_event(
    event: SwarmEvent<BehaviourEvent>,
    bootstrap_peers: &[(Libp2pPeerId, libp2p::Multiaddr)],
    relay_reservations: &[(Libp2pPeerId, libp2p::Multiaddr)],
    result: &mut BootstrapPollResult,
) {
    match event {
        SwarmEvent::ConnectionEstablished { peer_id, .. } => {
            if bootstrap_peers.iter().any(|(peer, _)| *peer == peer_id) {
                result.connected_bootstrap_peers.insert(peer_id);
            }
        }
        SwarmEvent::OutgoingConnectionError {
            peer_id: Some(peer_id),
            error,
            ..
        } if bootstrap_peers.iter().any(|(peer, _)| *peer == peer_id) => {
            result.dial_failures.push((peer_id, format!("{error:?}")));
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
        SwarmEvent::NewListenAddr { address, .. } if is_relayed_address(&address) => {
            result.relayed_listen_addresses.insert(address);
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
    relayed_listen_addresses: HashSet<libp2p::Multiaddr>,
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
    autonat_probe_servers_registered: usize,
    autonat_status: BootstrapAutoNatStatus,
    now: Instant,
    deadline: Instant,
}

fn should_continue_polling(status: PollingStatus) -> bool {
    if (status.configured_bootstrap_peers == 0
        && !status.requirements.relay_reservations
        && !status.requirements.autonat_status
        && !status.requirements.dcutr_ready)
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

    bootstrap_waiting || relay_waiting || autonat_waiting
}

const fn relay_reservations_ready(
    configured_relay_reservations: usize,
    accepted_relay_reservations: usize,
    relayed_listen_addresses: usize,
) -> bool {
    configured_relay_reservations > 0
        && accepted_relay_reservations == configured_relay_reservations
        && relayed_listen_addresses >= configured_relay_reservations
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

fn is_relayed_address(address: &libp2p::Multiaddr) -> bool {
    address
        .iter()
        .any(|protocol| matches!(protocol, Protocol::P2pCircuit))
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
    use std::time::Duration;

    use futures::StreamExt as _;
    use libp2p::{Multiaddr, PeerId as Libp2pPeerId, identity::Keypair, swarm::SwarmEvent};

    use super::*;
    use crate::{
        config::{
            BootstrapPeerConfig, Config, DiscoveryConfig, InterfaceConfig, NetworkConfig,
            PacketPlaneConfig, QueueConfig, RelayConfig, ResourceConfig,
        },
        identity::NodeIdentity,
        runtime::p2p::{HostConfig, build_node},
    };

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

    #[test]
    fn bootstrap_check_lines_report_ipfs_compatible_thresholds() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let report = BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::All,
            requirements: BootstrapCheckRequirements {
                relay_reservations: true,
                autonat_status: true,
                dcutr_ready: true,
            },
            kademlia_protocol: "/ipfs/kad/1.0.0".to_owned(),
            ipfs_compatible: true,
            dcutr: BootstrapDcutrCheck {
                enabled: true,
                ready: false,
            },
            configured_bootstrap_peers: 2,
            connected_bootstrap_peers: 1,
            dial_failures: 1,
            configured_relay_reservations: 1,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
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
            }],
        };

        let lines = report.lines();

        assert!(!report.succeeded());
        assert!(lines.contains(&"bootstrap check: failed".to_owned()));
        assert!(lines.contains(&"success threshold: all".to_owned()));
        assert!(lines.contains(&"require relay reservations: true".to_owned()));
        assert!(lines.contains(&"require autonat status: true".to_owned()));
        assert!(lines.contains(&"require dcutr ready: true".to_owned()));
        assert!(lines.contains(&"ipfs compatible: true".to_owned()));
        assert!(lines.contains(&"dcutr enabled: true".to_owned()));
        assert!(lines.contains(&"dcutr ready: false".to_owned()));
        assert!(
            lines.contains(
                &"relay reservations: 1 accepted 0 relayed_listen_addresses 0".to_owned()
            )
        );
        assert!(lines.contains(&"autonat probe servers registered: 2".to_owned()));
        assert!(lines.contains(&"autonat status: private".to_owned()));
        assert!(lines.iter().any(|line| line.contains("last_error none")));
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

    async fn next_listen_address(node: &mut crate::runtime::p2p::P2pNode) -> Multiaddr {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = node.swarm.select_next_some().await {
                return address;
            }
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
            },
            kademlia_protocol: "/p2p-vpn/kad/1.0.0".to_owned(),
            ipfs_compatible: false,
            dcutr: BootstrapDcutrCheck {
                enabled: false,
                ready: false,
            },
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations: 0,
            accepted_relay_reservations: 0,
            relayed_listen_addresses: 0,
            autonat_probe_servers_registered,
            autonat_status,
            kademlia: BootstrapKademliaCheck::default(),
            peer_results: Vec::new(),
            relay_results: Vec::new(),
        }
    }

    fn dcutr_report(
        dcutr_enabled: bool,
        configured_relay_reservations: usize,
        accepted_relay_reservations: usize,
        relayed_listen_addresses: usize,
    ) -> BootstrapCheckReport {
        BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::Any,
            requirements: BootstrapCheckRequirements {
                relay_reservations: false,
                autonat_status: false,
                dcutr_ready: true,
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
            },
            configured_bootstrap_peers: 0,
            connected_bootstrap_peers: 0,
            dial_failures: 0,
            configured_relay_reservations,
            accepted_relay_reservations,
            relayed_listen_addresses,
            autonat_probe_servers_registered: 0,
            autonat_status: BootstrapAutoNatStatus::Unknown,
            kademlia: BootstrapKademliaCheck::default(),
            peer_results: Vec::new(),
            relay_results: Vec::new(),
        }
    }
}
