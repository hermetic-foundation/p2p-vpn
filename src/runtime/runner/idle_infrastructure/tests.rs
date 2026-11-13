use super::*;

struct Fixture {
    peer: Libp2pPeerId,
    id: ConnectionId,
    node: P2pNode,
    forwarder: Forwarder,
    infrastructure: InfrastructurePeers,
    routing: RoutingInfrastructurePeers,
    queues: PeerQueues,
    in_flight: PacketInFlight,
    epochs: ConnectionEpochs,
    connections: HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    readiness: RelayReadiness,
    auto_relay: AutoRelayState,
    pairing: CodePairingSessions,
    probes: MembershipProbeConnections,
    quiet: bool,
    now: Instant,
}

impl Fixture {
    fn new() -> Self {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let config: Config = serde_json::from_value(serde_json::json!({
            "network": {"name":"idle-infrastructure", "private_key":identity.private_key},
        }))
        .unwrap();
        let mut node = super::super::tests::membership_sync_test_node(identity);
        let local = node.local_peer_id;
        node.swarm.behaviour_mut().kad = kad::Behaviour::with_config(
            local,
            kad::store::MemoryStore::new(local),
            crate::runtime::p2p::controlled_kademlia_config(libp2p::StreamProtocol::new(
                PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            )),
        );
        node.swarm.behaviour_mut().pairing_kad = None.into();
        let peer = Libp2pPeerId::random();
        let id = ConnectionId::new_unchecked(601);
        let address: Multiaddr = format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .unwrap();
        let mut infrastructure = InfrastructurePeers::default();
        infrastructure.insert(peer, address.clone());
        let now = Instant::now();
        let mut epochs = ConnectionEpochs::default();
        epochs.record_established_at(id, now.checked_sub(INITIAL_GRACE).unwrap());
        Self {
            peer,
            id,
            node,
            forwarder: Forwarder::from_config(&config).unwrap(),
            infrastructure,
            routing: RoutingInfrastructurePeers::default(),
            queues: PeerQueues::new(4, 4096),
            in_flight: PacketInFlight::new(4),
            epochs,
            connections: HashMap::from([(
                (peer, id),
                ConnectedPoint::Dialer {
                    address,
                    role_override: libp2p::core::Endpoint::Dialer,
                    port_use: libp2p::core::transport::PortUse::New,
                },
            )]),
            readiness: RelayReadiness::default(),
            auto_relay: AutoRelayState::default(),
            pairing: CodePairingSessions::new(),
            probes: MembershipProbeConnections::default(),
            quiet: true,
            now,
        }
    }

    fn context(&mut self) -> RetirementContext<'_> {
        RetirementContext {
            node: &mut self.node,
            forwarder: &self.forwarder,
            infrastructure: &self.infrastructure,
            routing: &self.routing,
            queues: &self.queues,
            in_flight: &self.in_flight,
            epochs: &mut self.epochs,
            connections: &self.connections,
            readiness: &self.readiness,
            auto_relay: &self.auto_relay,
            pairing: &self.pairing,
            probes: &self.probes,
            discovery_quiet: self.quiet,
            now: self.now,
        }
    }

    fn eligible(&mut self) -> bool {
        let (peer, id) = (self.peer, self.id);
        eligible(&self.context(), peer, id)
    }
}

#[tokio::test]
async fn retirement_rejects_protected_roles_and_noncurrent_connections() {
    for scenario in [
        "young",
        "stale",
        "retiring",
        "unestablished",
        "unknown",
        "explicit",
        "overlay",
        "pending",
        "probe",
        "circuit",
        "serving",
        "not_quiet",
    ] {
        let mut f = Fixture::new();
        assert!(f.eligible());
        match scenario {
            "young" => {
                f.epochs.established.insert(f.id, f.now);
            }
            "stale" => {
                f.epochs.advance();
            }
            "retiring" => {
                f.epochs.mark_retiring(f.id);
            }
            "unestablished" => {
                f.epochs.established.remove(&f.id);
            }
            "unknown" => {
                f.infrastructure.remove(f.peer);
            }
            "explicit" => {
                f.node
                    .relay_peer_addresses
                    .push((f.peer, Multiaddr::empty()));
            }
            "overlay" => {
                let mut config = f.forwarder.config().clone();
                config.peers.push(
                    serde_json::from_value(serde_json::json!({"id":f.peer.to_string()})).unwrap(),
                );
                f.forwarder = Forwarder::from_config(&config).unwrap();
            }
            "pending" => {
                f.auto_relay.record_candidate(
                    f.peer,
                    f.connections[&(f.peer, f.id)].get_remote_address().clone(),
                );
                f.auto_relay.next_reservation_targets(f.now);
            }
            "probe" => {
                f.probes.admit(f.peer, f.id, false, f.now);
            }
            "circuit" => {
                let peer = Libp2pPeerId::random();
                let address = f.connections[&(f.peer, f.id)]
                    .get_remote_address()
                    .clone()
                    .with(Protocol::P2pCircuit)
                    .with(Protocol::P2p(peer));
                f.connections.insert(
                    (peer, ConnectionId::new_unchecked(602)),
                    ConnectedPoint::Dialer {
                        address,
                        role_override: libp2p::core::Endpoint::Dialer,
                        port_use: libp2p::core::transport::PortUse::New,
                    },
                );
            }
            "serving" => f.node.startup.relay_server_enabled = true,
            "not_quiet" => f.quiet = false,
            _ => unreachable!(),
        }
        assert!(!f.eligible(), "{scenario}");
    }
}

#[tokio::test]
async fn routing_and_bootstrap_metadata_do_not_pin_mature_idle_transports() {
    let mut f = Fixture::new();
    f.infrastructure.remove(f.peer);
    assert!(!f.eligible());
    f.routing.admit(f.peer);
    assert!(f.eligible());
    f.routing.remove(f.peer);
    f.node.bootstrap_peer_addresses.push((
        f.peer,
        f.connections[&(f.peer, f.id)].get_remote_address().clone(),
    ));
    assert!(f.eligible());
    assert_eq!(
        retire(&mut f.context()),
        0,
        "no verified anchor: do not retire anything"
    );
    assert!(!f.epochs.retiring.contains(&f.id));
}

#[tokio::test]
async fn all_native_query_owners_and_pending_dials_protect_shared_transports() {
    for primary in [false, true] {
        let mut f = Fixture::new();
        assert!(!wire_busy(&f.node));
        if !primary {
            let local = f.node.local_peer_id;
            f.node.swarm.behaviour_mut().pairing_kad = Some(kad::Behaviour::with_config(
                local,
                kad::store::MemoryStore::new(local),
                crate::runtime::p2p::controlled_kademlia_config(libp2p::StreamProtocol::new(
                    PUBLIC_IPFS_KADEMLIA_PROTOCOL,
                )),
            ))
            .into();
        }
        let kad = if primary {
            &mut f.node.swarm.behaviour_mut().kad
        } else {
            f.node.swarm.behaviour_mut().pairing_kad.as_mut().unwrap()
        };
        let id = kad.get_closest_peers(Libp2pPeerId::random());
        assert!(wire_busy(&f.node));
        let kad = if primary {
            &mut f.node.swarm.behaviour_mut().kad
        } else {
            f.node.swarm.behaviour_mut().pairing_kad.as_mut().unwrap()
        };
        kad.cancel_query(&id);
        assert!(!wire_busy(&f.node));
    }
    let mut f = Fixture::new();
    f.node
        .swarm
        .dial(
            DialOpts::peer_id(f.peer)
                .addresses(vec!["/ip4/127.0.0.1/tcp/9".parse().unwrap()])
                .build(),
        )
        .unwrap();
    assert!(
        f.node
            .swarm
            .network_info()
            .connection_counters()
            .num_pending()
            > 0
    );
    assert!(wire_busy(&f.node));
}

#[tokio::test]
async fn disconnected_protocol_work_protects_indirect_circuit_selection() {
    let mut f = Fixture::new();
    assert!(!wire_busy(&f.node));
    f.node.swarm.behaviour_mut().control.send_request(
        &Libp2pPeerId::random(),
        ControlRequest::Capabilities(ControlCapabilities::local(
            "idle-infrastructure",
            None,
            1280,
        )),
    );
    assert_eq!(
        f.node
            .swarm
            .network_info()
            .connection_counters()
            .num_pending(),
        0
    );
    assert!(f.node.swarm.behaviour().has_pending_work());
    assert!(wire_busy(&f.node));
}

#[tokio::test]
async fn native_autonat_work_blocks_retirement_before_a_swarm_dial() {
    let mut f = Fixture::new();
    let mut nat = autonat::Behaviour::new(
        f.node.local_peer_id,
        autonat::Config {
            boot_delay: Duration::ZERO,
            ..autonat::Config::default()
        },
    );
    nat.add_server(f.peer, None);
    nat.probe_address("/ip4/203.0.113.10/tcp/4001".parse().unwrap());
    assert!(!nat.has_pending_work(), "configuration alone is not work");
    // Poll the native owner only; do not submit its queued request to Swarm.
    let event = tokio::time::timeout(
        Duration::from_secs(2),
        futures::future::poll_fn(|cx| libp2p::swarm::NetworkBehaviour::poll(&mut nat, cx)),
    )
    .await
    .unwrap();
    assert!(matches!(
        event,
        libp2p::swarm::ToSwarm::GenerateEvent(autonat::Event::OutboundProbe(
            autonat::OutboundProbeEvent::Request { peer, .. }
        )) if peer == f.peer
    ));
    f.node.swarm.behaviour_mut().autonat = Some(nat).into();
    assert!(f.eligible());
    assert_eq!(
        f.node
            .swarm
            .network_info()
            .connection_counters()
            .num_pending(),
        0
    );
    assert!(f.node.swarm.behaviour().has_pending_work());
    assert!(wire_busy(&f.node));
}
