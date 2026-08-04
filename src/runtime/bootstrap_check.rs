use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use libp2p::{PeerId as Libp2pPeerId, swarm::SwarmEvent};

use crate::{
    config::{Config, ConfigError},
    identity::IdentityError,
    runtime::p2p::{HostConfig, P2pBuildError, build_node},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapCheckThreshold {
    Any,
    All,
}

#[derive(Debug)]
pub struct BootstrapCheckReport {
    pub threshold: BootstrapCheckThreshold,
    pub kademlia_protocol: String,
    pub ipfs_compatible: bool,
    pub configured_bootstrap_peers: usize,
    pub connected_bootstrap_peers: usize,
    pub dial_failures: usize,
    pub autonat_probe_servers_registered: usize,
    pub kademlia: BootstrapKademliaCheck,
    pub peer_results: Vec<BootstrapPeerCheck>,
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
        match self.threshold {
            BootstrapCheckThreshold::Any => self.connected_bootstrap_peers > 0,
            BootstrapCheckThreshold::All => {
                self.configured_bootstrap_peers > 0
                    && self.connected_bootstrap_peers == self.configured_bootstrap_peers
            }
        }
    }

    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!(
                "bootstrap check: {}",
                if self.succeeded() { "ok" } else { "failed" }
            ),
            format!("success threshold: {}", self.threshold.as_str()),
            format!("kademlia protocol: {}", self.kademlia_protocol),
            format!("ipfs compatible: {}", self.ipfs_compatible),
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
            format!(
                "bootstrap peers: {} connected {} dial_failures {}",
                self.configured_bootstrap_peers, self.connected_bootstrap_peers, self.dial_failures
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

#[derive(Debug)]
pub struct BootstrapPeerCheck {
    pub peer_id: Libp2pPeerId,
    pub address: String,
    pub connected: bool,
    pub dial_failures: usize,
    pub last_error: Option<String>,
}

pub async fn check_config_bootstrap(
    config: &Config,
    timeout: Duration,
    threshold: BootstrapCheckThreshold,
) -> Result<BootstrapCheckReport, BootstrapCheckError> {
    config.validate_runtime()?;
    let mut node = build_node(&HostConfig {
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
    })?;

    let bootstrap_peers = node.bootstrap_peer_addresses.clone();
    let mut connected = bootstrap_peers
        .iter()
        .filter_map(|(peer, _)| node.swarm.is_connected(peer).then_some(*peer))
        .collect::<HashSet<_>>();
    let mut dial_failures = Vec::new();
    let deadline = Instant::now() + timeout.max(Duration::from_millis(1));

    while should_continue_polling(
        threshold,
        bootstrap_peers.len(),
        connected.len(),
        Instant::now(),
        deadline,
    ) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = tokio::time::timeout(remaining, node.swarm.select_next_some()).await else {
            break;
        };
        match event {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                if bootstrap_peers.iter().any(|(peer, _)| *peer == peer_id) {
                    connected.insert(peer_id);
                }
            }
            SwarmEvent::OutgoingConnectionError {
                peer_id: Some(peer_id),
                error,
                ..
            } if bootstrap_peers.iter().any(|(peer, _)| *peer == peer_id) => {
                dial_failures.push((peer_id, format!("{error:?}")));
            }
            _ => {}
        }
    }

    Ok(BootstrapCheckReport {
        threshold,
        kademlia_protocol: node.discovery.kademlia_protocol.clone(),
        ipfs_compatible: node.discovery.kademlia_protocol == "/ipfs/kad/1.0.0",
        configured_bootstrap_peers: bootstrap_peers.len(),
        connected_bootstrap_peers: connected.len(),
        dial_failures: dial_failures.len(),
        autonat_probe_servers_registered: node.startup.autonat_servers_registered,
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
                connected: connected.contains(&peer_id),
                dial_failures: dial_failures
                    .iter()
                    .filter(|(failed_peer, _)| *failed_peer == peer_id)
                    .count(),
                last_error: dial_failures.iter().rev().find_map(|(failed_peer, error)| {
                    (*failed_peer == peer_id).then(|| error.clone())
                }),
            })
            .collect(),
    })
}

fn should_continue_polling(
    threshold: BootstrapCheckThreshold,
    configured_peers: usize,
    connected_peers: usize,
    now: Instant,
    deadline: Instant,
) -> bool {
    if configured_peers == 0 || now >= deadline {
        return false;
    }

    match threshold {
        BootstrapCheckThreshold::Any => connected_peers == 0,
        BootstrapCheckThreshold::All => connected_peers < configured_peers,
    }
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
        )
        .await
        .expect("bootstrap check");

        assert!(report.succeeded());
        assert_eq!(report.configured_bootstrap_peers, 1);
        assert_eq!(report.connected_bootstrap_peers, 1);
        assert!(report.lines().contains(&"bootstrap check: ok".to_owned()));
    }

    #[test]
    fn bootstrap_check_lines_report_ipfs_compatible_thresholds() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let report = BootstrapCheckReport {
            threshold: BootstrapCheckThreshold::All,
            kademlia_protocol: "/ipfs/kad/1.0.0".to_owned(),
            ipfs_compatible: true,
            configured_bootstrap_peers: 2,
            connected_bootstrap_peers: 1,
            dial_failures: 1,
            autonat_probe_servers_registered: 2,
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
        };

        let lines = report.lines();

        assert!(!report.succeeded());
        assert!(lines.contains(&"bootstrap check: failed".to_owned()));
        assert!(lines.contains(&"success threshold: all".to_owned()));
        assert!(lines.contains(&"ipfs compatible: true".to_owned()));
        assert!(lines.contains(&"autonat probe servers registered: 2".to_owned()));
        assert!(lines.iter().any(|line| line.contains("last_error none")));
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
}
