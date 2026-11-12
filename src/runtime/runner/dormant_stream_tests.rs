use super::*;
use crate::config::{RelayResourceConfig, ResourceConfig};

struct Fixture {
    peer: Libp2pPeerId,
    node: P2pNode,
    forwarder: Forwarder,
    queues: PeerQueues,
    in_flight: PacketInFlight,
    epochs: ConnectionEpochs,
    connections: HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    infrastructure: InfrastructurePeers,
    readiness: RelayReadiness,
    auto_relay: AutoRelayState,
    now: Instant,
}

fn node(identity: NodeIdentity) -> P2pNode {
    build_node(&HostConfig {
        identity,
        network_name: "dormant-stream-retirement".into(),
        membership_tag: None,
        mtu: 1280,
        max_concurrent_control_streams: 8,
        max_concurrent_packet_streams: 8,
        listen_addresses: Vec::new(),
        external_addresses: Vec::new(),
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: Vec::new(),
        relay_server: false,
        relay_resources: RelayResourceConfig::default(),
        resources: ResourceConfig::default(),
        discovery: DiscoveryConfig {
            mdns: false,
            kademlia: false,
            autonat: false,
            dcutr: false,
            ..DiscoveryConfig::default()
        },
    })
    .unwrap()
}

fn endpoint(address: &str) -> ConnectedPoint {
    ConnectedPoint::Dialer {
        address: address.parse().unwrap(),
        role_override: libp2p::core::Endpoint::Dialer,
        port_use: libp2p::core::transport::PortUse::New,
    }
}

impl Fixture {
    fn new() -> Self {
        Self::for_peer(Libp2pPeerId::random())
    }

    fn for_peer(peer: Libp2pPeerId) -> Self {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let config: Config = serde_json::from_value(serde_json::json!({
            "network":{"name":"dormant-stream-retirement", "private_key":identity.private_key},
            "peers":[{"id":peer.to_string()}],
        }))
        .unwrap();
        Self {
            peer,
            node: node(identity),
            forwarder: Forwarder::from_config(&config).unwrap(),
            queues: PeerQueues::new(4, 4096),
            in_flight: PacketInFlight::new(4),
            epochs: ConnectionEpochs::default(),
            connections: HashMap::new(),
            infrastructure: InfrastructurePeers::default(),
            readiness: RelayReadiness::default(),
            auto_relay: AutoRelayState::default(),
            now: Instant::now(),
        }
    }

    fn overlay(&self) -> PeerId {
        PeerId::from_libp2p(self.peer)
    }

    fn quiet(&self) {
        let version = self
            .forwarder
            .application_demand(self.overlay())
            .unwrap()
            .version
            .unwrap();
        assert!(
            self.forwarder
                .grant_data_path_quiet(self.overlay(), version)
        );
    }

    fn mature_connection(&mut self) -> ConnectionId {
        let id = ConnectionId::new_unchecked(601);
        self.epochs
            .record_established_at(id, self.now.checked_sub(INITIAL_CONTROL_GRACE).unwrap());
        self.connections
            .insert((self.peer, id), endpoint("/ip4/127.0.0.1/tcp/9001"));
        id
    }

    fn context(&mut self) -> StreamRetirementContext<'_> {
        StreamRetirementContext {
            node: &mut self.node,
            forwarder: &self.forwarder,
            queues: &self.queues,
            in_flight: &self.in_flight,
            epochs: &mut self.epochs,
            connections: &self.connections,
            infrastructure: &self.infrastructure,
            relay_readiness: &self.readiness,
            auto_relay: &self.auto_relay,
            now: self.now,
        }
    }

    fn eligible(&mut self, id: ConnectionId) -> bool {
        let peer = self.peer;
        can_retire_stream(&self.context(), peer, id)
    }
}

#[tokio::test]
async fn retirement_requires_quiet_and_empty_application_and_wire_work() {
    let mut f = Fixture::new();
    let id = f.mature_connection();
    assert!(!f.eligible(id));
    f.quiet();
    assert!(f.eligible(id));
    f.queues
        .enqueue(crate::queue::Packet::new(f.overlay(), 1, vec![1; 20]))
        .unwrap();
    assert!(!f.eligible(id));
    f.queues.dequeue().unwrap();
    assert!(f.eligible(id));
    let local = ControlCapabilities::local("dormant-stream-retirement", None, 1280);
    f.node
        .swarm
        .behaviour_mut()
        .control
        .send_request(&f.peer, ControlRequest::Capabilities(local));
    assert!(
        !f.eligible(id),
        "a queued control request must not be closed before it is polled"
    );
    let mut active = Fixture::new();
    let id = active.mature_connection();
    active.quiet();
    active
        .forwarder
        .record_application_demand(active.overlay(), active.now);
    assert!(!active.eligible(id));
}

#[tokio::test]
async fn initial_grace_stale_and_retiring_connections_are_fenced() {
    let mut f = Fixture::new();
    let id = f.mature_connection();
    f.quiet();
    f.epochs.established.insert(id, f.now);
    assert!(!f.eligible(id));
    f.now += INITIAL_CONTROL_GRACE / 2;
    f.epochs.record_established_at(id, f.now);
    assert!(!f.eligible(id));
    f.now += INITIAL_CONTROL_GRACE / 2;
    assert!(
        f.eligible(id),
        "duplicate establishment must not extend the grace forever"
    );
    f.epochs.mark_retiring(id);
    assert!(!f.eligible(id));
    f.epochs.retiring.clear();
    f.epochs.advance();
    assert!(!f.eligible(id));
    f.epochs.record_started(id);
    assert!(!f.epochs.is_current(id));
    assert!(!f.eligible(id), "late starts must not relabel stale paths");
    let pending = ConnectionId::new_unchecked(602);
    f.epochs.record_started(pending);
    f.connections
        .insert((f.peer, pending), endpoint("/ip4/127.0.0.1/tcp/9001"));
    assert!(
        !f.eligible(pending),
        "an unestablished attempt has no control grace evidence"
    );
}

#[tokio::test]
async fn infrastructure_and_inner_circuit_dependencies_are_preserved() {
    for role in [
        "bootstrap",
        "relay",
        "infrastructure",
        "reservation",
        "listener",
        "pending_auto",
        "accepted_auto",
        "circuit",
        "server",
    ] {
        let mut f = Fixture::new();
        let id = f.mature_connection();
        f.quiet();
        assert!(f.eligible(id));
        let address = "/ip4/127.0.0.1/tcp/9001".parse().unwrap();
        match role {
            "bootstrap" => f.node.bootstrap_peer_addresses.push((f.peer, address)),
            "relay" => f.node.relay_peer_addresses.push((f.peer, address)),
            "infrastructure" => {
                f.infrastructure.insert(f.peer, address);
            }
            "reservation" => {
                f.readiness.accepted_reservations.insert(f.peer);
            }
            "listener" => {
                f.readiness
                    .relayed_listen_addresses
                    .insert(f.peer, Vec::new());
            }
            "pending_auto" => {
                f.auto_relay.pending_reservations.insert(
                    f.peer,
                    PendingAutoRelayReservation {
                        address,
                        expires_at: f.now + Duration::from_secs(30),
                    },
                );
            }
            "accepted_auto" => {
                f.auto_relay.accepted_reservation_peers.insert(f.peer);
            }
            "circuit" => {
                let other = Libp2pPeerId::random();
                f.connections.insert(
                    (other, ConnectionId::new_unchecked(602)),
                    endpoint(&format!(
                        "/ip4/127.0.0.1/tcp/9001/p2p/{}/p2p-circuit/p2p/{other}",
                        f.peer
                    )),
                );
            }
            "server" => f.node.startup.relay_server_enabled = true,
            _ => unreachable!(),
        }
        assert!(
            !f.eligible(id),
            "role {role} is infrastructure, not a disposable data path"
        );
    }
}

#[tokio::test]
async fn dormant_metadata_survives_disconnect_and_initial_reconnect_but_not_demanded_failure() {
    let f = Fixture::new();
    let peer = f.overlay();
    let mut caps = PeerCapabilities::default();
    let capabilities = ControlCapabilities::local("dormant-stream-retirement", None, 1280);
    caps.record(peer, capabilities.clone());
    f.quiet();
    invalidate_peer_capabilities_when_disconnected(&f.forwarder, &mut caps, None, None, f.peer, 0);
    assert!(caps.contains(peer));
    invalidate_peer_capabilities_on_first_connection(&f.forwarder, &mut caps, f.peer, 1);
    assert!(caps.contains(peer));
    f.forwarder.record_application_demand(peer, f.now);
    invalidate_peer_capabilities_when_disconnected(&f.forwarder, &mut caps, None, None, f.peer, 0);
    assert!(!caps.contains(peer));
    caps.record(peer, capabilities);
    invalidate_peer_capabilities_on_first_connection(&f.forwarder, &mut caps, f.peer, 1);
    assert!(!caps.contains(peer));
}

#[tokio::test]
async fn real_tcp_retirement_is_idempotent_and_demand_allows_a_fresh_connection() {
    let remote_identity = NodeIdentity::generate_ed25519().unwrap();
    let peer = remote_identity.peer_id.parse().unwrap();
    let mut f = Fixture::for_peer(peer);
    let mut remote = node(remote_identity);
    remote
        .swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    let address = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = remote.swarm.select_next_some().await
            {
                break address;
            }
        }
    })
    .await
    .unwrap();
    let mut previous = None;
    for quiet in [true, false] {
        f.node.swarm.dial(address.clone()).unwrap();
        let id = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                tokio::select! {
                    _ = remote.swarm.select_next_some() => {},
                    event = f.node.swarm.select_next_some() => {
                        if let SwarmEvent::ConnectionEstablished {peer_id,connection_id,endpoint,..} = event {
                            assert_eq!(peer_id,peer);
                            f.connections.insert((peer,connection_id),endpoint);
                            f.epochs.record_established_at(connection_id,f.now.checked_sub(INITIAL_CONTROL_GRACE).unwrap());
                            break connection_id;
                        }
                    }
                }
            }
        }).await.unwrap();
        assert_ne!(Some(id), previous);
        if quiet {
            f.quiet();
        }
        assert_eq!(retire_streams(&mut f.context()), usize::from(quiet));
        assert_eq!(retire_streams(&mut f.context()), 0);
        assert!(
            f.forwarder
                .transport_peer_for_overlay(f.overlay())
                .is_some()
        );
        if quiet {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        _ = remote.swarm.select_next_some() => {},
                        event = f.node.swarm.select_next_some() => {
                            if let SwarmEvent::ConnectionClosed {connection_id,..} = event {
                                assert_eq!(connection_id,id);
                                f.epochs.remove(id);
                                f.connections.remove(&(peer,id));
                                break;
                            }
                        }
                    }
                }
            })
            .await
            .unwrap();
            assert!(!f.node.swarm.is_connected(&peer));
            f.forwarder.record_application_demand(f.overlay(), f.now);
        } else {
            assert!(f.node.swarm.is_connected(&peer));
        }
        previous = Some(id);
    }
}
