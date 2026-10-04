#[tokio::test]
async fn pairing_relay_survives_identify_with_a_full_routing_pool() {
    for owned in [false, true] {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let mut node = pairing_test_node(&identity);
        let config: Config = serde_json::from_value(serde_json::json!({
            "network": {"name": "lab", "private_key": identity.private_key}
        }))
        .unwrap();
        let mut forwarder = Forwarder::from_config(&config).unwrap();
        let initial_routes = forwarder.authorized_routes().to_vec();
        let mut membership = OverlayMembership::from_config(&config).unwrap();
        let relay_key = Keypair::generate_ed25519();
        let relay = relay_key.public().to_peer_id();
        let inviter = peer_id();
        let relay_address: Multiaddr = "/ip4/135.181.230.175/tcp/4001".parse().unwrap();
        let circuit = relay_address
            .clone()
            .with(Protocol::P2p(relay))
            .with(Protocol::P2pCircuit)
            .with(Protocol::P2p(inviter));
        public_pairing_kad_mut(node.swarm.behaviour_mut()).add_address(&inviter, circuit);
        let mut sessions = CodePairingSessions::new();
        let now = Instant::now();
        let started = sessions
            .join(
                "lab",
                crate::pairing_code::PairingCode::generate(),
                None,
                Vec::new(),
                600,
                current_unix_seconds_lossy(),
                now,
            )
            .unwrap();
        let metrics = RuntimeMetrics::default();
        send_pairing_code_hello(
            &mut node.swarm,
            &mut sessions,
            &identity,
            "lab",
            inviter,
            PairingDiscoveryStage::Public,
            &metrics,
            now,
        );
        assert!(sessions.uses_pairing_relay(relay));
        if !owned {
            sessions.release_peer_attempt(&started.operation_id, inviter, now);
        }
        let mut routing = RoutingInfrastructurePeers::default();
        for _ in 0..KADEMLIA_ROUTING_PEER_CAPACITY {
            assert_eq!(
                routing.admit(peer_id()),
                RoutingInfrastructureAdmission::Admitted
            );
        }
        assert_eq!(
            routing.admit(relay),
            RoutingInfrastructureAdmission::AtCapacity
        );
        let mut infrastructure = InfrastructurePeers::default();
        let mut probes = MembershipProbeConnections::default();
        handle_identify_received(
            &mut node.swarm,
            &mut BehaviourEventContext {
                forwarder: &mut forwarder,
                membership: &mut membership,
                tun_runtime: &mut TunRuntimeConfig::from_config(&config).unwrap(),
                route_controller: &mut PreconfiguredTunRoutes,
                infrastructure_peers: &mut infrastructure,
                routing_infrastructure_peers: &mut routing,
                relay_readiness: &mut RelayReadiness::default(),
                auto_relay: &mut AutoRelayState::default(),
                paths: &PathSet::new(),
                relay_addresses: &[],
                configured_peer_addresses: &[],
                relay_server_enabled: false,
                discovered_peer_addresses: &mut DiscoveredPeerAddresses::default(),
                metrics: &metrics,
                discovery: &node.discovery,
                local_capabilities: &mut ControlCapabilities::local("lab", None, 1280),
                previous_membership_tags: &[],
                identity: &identity,
                code_pairing_sessions: &mut sessions,
                membership_probe_connections: &mut probes,
                connection_epochs: &mut ConnectionEpochs::default(),
                active_connections: &HashMap::new(),
                public_discovery_quiet: false,
                kademlia_maintenance: &mut KademliaMaintenance::new(now),
            },
            relay,
            ConnectionId::new_unchecked(1),
            identify::Info {
                public_key: relay_key.public(),
                protocol_version: "ipfs/0.1.0".to_owned(),
                agent_version: "pairing-relay-test".to_owned(),
                listen_addrs: vec![relay_address.clone()],
                protocols: vec![
                    libp2p::StreamProtocol::new(PUBLIC_IPFS_KADEMLIA_PROTOCOL),
                    libp2p::StreamProtocol::new("/libp2p/circuit/relay/0.2.0/hop"),
                ],
                observed_addr: relay_address,
                signed_peer_record: None,
            },
        );
        assert_eq!(infrastructure.contains(relay), owned);
        assert_eq!(routing.len(), KADEMLIA_ROUTING_PEER_CAPACITY);
        assert!(!membership.allows(relay));
        assert!(!forwarder.is_configured_transport_peer(relay));
        assert_eq!(forwarder.authorized_routes(), initial_routes);
    }
}
