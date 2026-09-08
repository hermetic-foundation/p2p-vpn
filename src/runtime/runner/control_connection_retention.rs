use super::*;

/// Prefer the same transport at both endpoints; connection IDs are process-local.
pub(super) fn should_retain_control_connection(
    peer: Libp2pPeerId,
    connection: ConnectionId,
    forwarder: &Forwarder,
    peer_capabilities: &PeerCapabilities,
    paths: &PathSet,
    active_connections: &HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
) -> bool {
    if !forwarder.is_configured_transport_peer(peer)
        || !peer_capabilities.contains(PeerId::from_libp2p(peer))
    {
        return false;
    }
    let Some(endpoint) = active_connections.get(&(peer, connection)) else {
        return false;
    };
    let eligible = |path: &crate::path::PathCandidate| {
        path.healthy
            && path.established_connections > 0
            && !path.kind.requires_quic_datagrams()
            && path
                .latest_connection_id
                .is_some_and(|id| active_connections.contains_key(&(peer, id)))
    };
    let preference = |kind| match kind {
        PathKind::DirectQuicStream => 0,
        PathKind::DirectTcpStream => 1,
        _ => 2,
    };
    let kind = path_kind_for_endpoint(endpoint);
    let preferred = paths
        .candidates_for(PeerId::from_libp2p(peer))
        .filter(eligible)
        .map(|path| preference(path.kind))
        .min();
    preferred == Some(preference(kind))
        && paths.candidates_for(PeerId::from_libp2p(peer)).any(|path| {
            eligible(&path)
                && path.kind == kind
                && path.relay_peer == relay_peer_for_endpoint(endpoint)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        peer: Libp2pPeerId,
        forwarder: Forwarder,
        capabilities: PeerCapabilities,
        paths: PathSet,
        connections: HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    }

    impl Fixture {
        fn new() -> Self {
            Self::for_peer(Libp2pPeerId::random())
        }

        fn for_peer(peer: Libp2pPeerId) -> Self {
            let identity = NodeIdentity::generate_ed25519().unwrap();
            let config: Config = serde_json::from_value(serde_json::json!({
                "network": {"name": "retention-selection", "private_key": identity.private_key},
                "peers": [{"id": peer.to_string()}]
            }))
            .unwrap();
            let mut fixture = Self {
                peer,
                forwarder: Forwarder::from_config(&config).unwrap(),
                capabilities: PeerCapabilities::default(),
                paths: PathSet::new(),
                connections: HashMap::new(),
            };
            fixture.validate(peer);
            fixture
        }

        fn validate(&mut self, peer: Libp2pPeerId) {
            self.capabilities.record(
                PeerId::from_libp2p(peer),
                ControlCapabilities::local("retention-selection", None, 1280),
            );
        }

        fn establish(&mut self, peer: Libp2pPeerId, kind: PathKind, id: usize) -> ConnectionId {
            let connection = ConnectionId::new_unchecked(id);
            self.paths.record_established_with_details(
                PeerId::from_libp2p(peer),
                kind,
                None,
                Some(1280),
                PathOrigin::Configured,
                PathConnectionRole::Dialer,
                kind == PathKind::CircuitRelay,
                Some(connection),
                Some(1),
            );
            let address = match kind {
                PathKind::CircuitRelay => "/ip4/127.0.0.1/tcp/4001/p2p-circuit",
                PathKind::DirectQuicStream | PathKind::DirectQuicDatagram => {
                    "/ip4/127.0.0.1/udp/4001/quic-v1"
                }
                _ => "/ip4/127.0.0.1/tcp/4001",
            };
            self.connections.insert(
                (peer, connection),
                ConnectedPoint::Dialer {
                    address: address.parse().unwrap(),
                    role_override: libp2p::core::Endpoint::Dialer,
                    port_use: libp2p::core::transport::PortUse::Reuse,
                },
            );
            connection
        }

        fn retains(&self, peer: Libp2pPeerId, connection: ConnectionId) -> bool {
            should_retain_control_connection(
                peer,
                connection,
                &self.forwarder,
                &self.capabilities,
                &self.paths,
                &self.connections,
            )
        }
    }

    #[test]
    fn requires_authorization_and_validation_and_excludes_infrastructure() {
        let mut fixture = Fixture::new();
        let peer = fixture.peer;
        let connection = fixture.establish(peer, PathKind::DirectTcpStream, 1);
        assert!(fixture.retains(peer, connection));
        fixture.capabilities.remove(PeerId::from_libp2p(peer));
        assert!(!fixture.retains(peer, connection));
        fixture.validate(peer);
        assert!(fixture.retains(peer, connection));

        let bootstrap = Libp2pPeerId::random();
        let relay = Libp2pPeerId::random();
        let unrelated = Libp2pPeerId::random();
        let mut config = fixture.forwarder.config().clone();
        config
            .network
            .bootstrap_peers
            .push(crate::config::BootstrapPeerConfig {
                id: bootstrap.to_string(),
                address: "/ip4/127.0.0.1/tcp/4001".to_owned(),
            });
        config
            .network
            .relay
            .reservations
            .push(format!("/ip4/127.0.0.1/tcp/4001/p2p/{relay}/p2p-circuit"));
        fixture.forwarder = Forwarder::from_config(&config).unwrap();
        for (index, excluded) in [bootstrap, relay, unrelated].into_iter().enumerate() {
            fixture.validate(excluded);
            let id = fixture.establish(excluded, PathKind::DirectTcpStream, index + 2);
            assert!(!fixture.retains(excluded, id));
        }
        assert!(fixture.retains(peer, connection));
    }

    #[test]
    fn excludes_unhealthy_unestablished_missing_id_and_non_live_paths() {
        let mut fixture = Fixture::new();
        let peer = fixture.peer;
        let connection = fixture.establish(peer, PathKind::DirectTcpStream, 1);
        let candidate = fixture
            .paths
            .candidates_for(PeerId::from_libp2p(peer))
            .next()
            .unwrap();
        assert!(fixture.retains(peer, connection));
        let mut unhealthy = candidate;
        unhealthy.healthy = false;
        let mut unestablished = candidate;
        unestablished.established_connections = 0;
        let mut missing_id = candidate;
        missing_id.latest_connection_id = None;
        for excluded in [unhealthy, unestablished, missing_id] {
            fixture.paths.upsert(excluded);
            assert!(!fixture.retains(peer, connection), "{excluded:?}");
        }
        fixture.paths.upsert(candidate);
        let endpoint = fixture.connections.remove(&(peer, connection)).unwrap();
        assert!(!fixture.retains(peer, connection));
        fixture
            .connections
            .insert((Libp2pPeerId::random(), connection), endpoint);
        assert!(!fixture.retains(peer, connection));
    }

    #[test]
    fn excludes_datagram_paths_even_with_live_connection_ids() {
        for kind in [PathKind::DirectUdpDatagram, PathKind::DirectQuicDatagram] {
            let mut fixture = Fixture::new();
            let peer = fixture.peer;
            let connection = fixture.establish(peer, kind, 1);
            assert!(!fixture.retains(peer, connection), "{kind:?}");
        }
    }

    #[test]
    fn prefers_quic_then_tcp_and_falls_back_to_live_relay() {
        let mut fixture = Fixture::new();
        let peer = fixture.peer;
        let relay = fixture.establish(peer, PathKind::CircuitRelay, 1);
        let tcp = fixture.establish(peer, PathKind::DirectTcpStream, 2);
        let quic = fixture.establish(peer, PathKind::DirectQuicStream, 3);
        assert!(!fixture.retains(peer, relay));
        assert!(!fixture.retains(peer, tcp));
        assert!(fixture.retains(peer, quic));
        fixture
            .paths
            .mark_unhealthy(PeerId::from_libp2p(peer), PathKind::DirectQuicStream);
        assert!(!fixture.retains(peer, quic));
        assert!(fixture.retains(peer, tcp));
        fixture.connections.remove(&(peer, tcp));
        assert!(!fixture.retains(peer, tcp));
        assert!(fixture.retains(peer, relay));
    }

    #[test]
    fn opposing_endpoint_connection_id_order_selects_the_same_transport() {
        for (tcp_id, quic_id) in [(10, 11), (21, 20)] {
            let mut fixture = Fixture::new();
            let peer = fixture.peer;
            let tcp = fixture.establish(peer, PathKind::DirectTcpStream, tcp_id);
            let quic = fixture.establish(peer, PathKind::DirectQuicStream, quic_id);
            assert!(!fixture.retains(peer, tcp));
            assert!(fixture.retains(peer, quic));
        }
    }

    #[tokio::test]
    async fn mixed_tcp_and_quic_endpoints_keep_the_same_live_connection() {
        use futures::StreamExt as _;
        let swarm = || {
            libp2p::SwarmBuilder::with_new_identity()
                .with_tokio()
                .with_tcp(
                    libp2p::tcp::Config::default(),
                    libp2p::noise::Config::new,
                    libp2p::yamux::Config::default,
                )
                .unwrap()
                .with_quic()
                .with_behaviour(|_| crate::runtime::connection_retention::Behaviour::default())
                .unwrap()
                .with_swarm_config(|config| {
                    config.with_idle_connection_timeout(Duration::from_millis(200))
                })
                .build()
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut a = swarm();
            let mut b = swarm();
            let mut fixtures = [
                Fixture::for_peer(*b.local_peer_id()),
                Fixture::for_peer(*a.local_peer_id()),
            ];
            a.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
                .unwrap();
            b.listen_on("/ip4/127.0.0.1/udp/0/quic-v1".parse().unwrap())
                .unwrap();
            let a_address = loop {
                if let SwarmEvent::NewListenAddr { address, .. } = a.select_next_some().await {
                    break address;
                }
            };
            let b_address = loop {
                if let SwarmEvent::NewListenAddr { address, .. } = b.select_next_some().await {
                    break address;
                }
            };
            // Opposite outbound transports allocate local IDs before inbound acceptance.
            a.dial(b_address).unwrap();
            b.dial(a_address).unwrap();
            while fixtures.iter().any(|fixture| fixture.connections.len() < 2) {
                let (side, event) = tokio::select! {
                    event = a.select_next_some() => (0, event),
                    event = b.select_next_some() => (1, event),
                };
                if let SwarmEvent::ConnectionEstablished {
                    peer_id,
                    connection_id,
                    endpoint,
                    ..
                } = event
                {
                    let fixture = &mut fixtures[side];
                    fixture.paths.record_established_with_details(
                        PeerId::from_libp2p(peer_id),
                        path_kind_for_endpoint(&endpoint),
                        None,
                        Some(1280),
                        PathOrigin::Configured,
                        path_connection_role_for_endpoint(&endpoint),
                        false,
                        Some(connection_id),
                        Some(1),
                    );
                    fixture
                        .connections
                        .insert((peer_id, connection_id), endpoint);
                }
                a.behaviour_mut()
                    .retain_connections(|peer, id| fixtures[0].retains(peer, id));
                b.behaviour_mut()
                    .retain_connections(|peer, id| fixtures[1].retains(peer, id));
            }
            for fixture in &fixtures {
                for ((peer, id), endpoint) in &fixture.connections {
                    assert_eq!(
                        fixture.retains(*peer, *id),
                        path_kind_for_endpoint(endpoint) == PathKind::DirectQuicStream
                    );
                }
            }
            let dwell = tokio::time::sleep(Duration::from_secs(1));
            tokio::pin!(dwell);
            let mut closed_tcp = [false; 2];
            loop {
                let (side, event) = tokio::select! {
                    () = &mut dwell => break,
                    event = a.select_next_some() => (0, event),
                    event = b.select_next_some() => (1, event),
                };
                if let SwarmEvent::ConnectionClosed { endpoint, .. } = event {
                    assert_eq!(path_kind_for_endpoint(&endpoint), PathKind::DirectTcpStream);
                    closed_tcp[side] = true;
                }
            }
            assert_eq!(closed_tcp, [true; 2]);
            assert!(a.is_connected(b.local_peer_id()));
            assert!(b.is_connected(a.local_peer_id()));
        })
        .await
        .expect("mixed transport retention deadline");
    }

    #[test]
    fn retains_same_transport_connections_and_selects_independently_per_peer() {
        let mut fixture = Fixture::new();
        let peer = fixture.peer;
        let old = fixture.establish(peer, PathKind::DirectTcpStream, 1);
        let latest = fixture.establish(peer, PathKind::DirectTcpStream, 2);
        assert!(fixture.retains(peer, old));
        assert!(fixture.retains(peer, latest));
        let other = Libp2pPeerId::random();
        let mut config = fixture.forwarder.config().clone();
        let mut other_config = config.peers[0].clone();
        other_config.id = other.to_string();
        config.peers.push(other_config);
        fixture.forwarder = Forwarder::from_config(&config).unwrap();
        fixture.validate(other);
        let other_id = fixture.establish(other, PathKind::CircuitRelay, 3);
        assert!(fixture.retains(other, other_id));
        assert!(fixture.retains(peer, latest));
        assert!(!fixture.retains(peer, other_id));
        assert!(!fixture.retains(other, latest));
    }

    #[test]
    fn releases_on_authorization_removal_despite_validated_healthy_live_path() {
        let mut fixture = Fixture::new();
        let peer = fixture.peer;
        let connection = fixture.establish(peer, PathKind::DirectTcpStream, 1);
        assert!(fixture.retains(peer, connection));
        let mut config = fixture.forwarder.config().clone();
        config.peers.clear();
        let update = fixture
            .forwarder
            .prepare_reconfigure(config, 1_000)
            .unwrap();
        fixture.forwarder.commit_reconfigure(update);
        assert!(fixture.capabilities.contains(PeerId::from_libp2p(peer)));
        assert!(fixture.paths.has_healthy_path(PeerId::from_libp2p(peer)));
        assert!(fixture.connections.contains_key(&(peer, connection)));
        assert!(!fixture.retains(peer, connection));
    }
}
