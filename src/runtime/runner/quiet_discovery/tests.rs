use super::*;
use crate::runtime::runner::tests::{config_with_peer, membership_sync_test_node};

struct Fixture {
    node: P2pNode,
    forwarder: Forwarder,
    local: ControlCapabilities,
    peer: Libp2pPeerId,
    pairing: CodePairingSessions,
    discovered: DiscoveredPeerAddresses,
    now: Instant,
}

impl Fixture {
    fn new() -> Self {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let local = PeerId::from_libp2p(identity.peer_id.parse().unwrap());
        let peer = std::iter::repeat_with(Libp2pPeerId::random)
            .take(1024)
            .find(|peer| {
                crate::route::builtin_ipv4(PeerId::from_libp2p(*peer))
                    != crate::route::builtin_ipv4(local)
            })
            .unwrap();
        let forwarder = Forwarder::from_config(&config_with_peer(&identity, peer)).unwrap();
        let mut node = membership_sync_test_node(identity);
        node.bootstrap_peer_addresses.clear();
        let id = node.local_peer_id;
        node.swarm.behaviour_mut().kad = kad::Behaviour::with_config(
            id,
            kad::store::MemoryStore::new(id),
            crate::runtime::p2p::controlled_kademlia_config(libp2p::StreamProtocol::new(
                PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            )),
        );
        node.swarm
            .behaviour_mut()
            .kad
            .set_mode(Some(kad::Mode::Client));
        node.swarm.behaviour_mut().autonat =
            Some(autonat::Behaviour::new(id, autonat::Config::default())).into();
        Self {
            node,
            forwarder,
            local: ControlCapabilities::local("lab", None, 1280).with_demand_lifecycle_epoch(
                crate::runtime::lifecycle::Epoch::new([7; 16]).unwrap(),
            ),
            peer,
            pairing: CodePairingSessions::new(),
            discovered: DiscoveredPeerAddresses::default(),
            now: Instant::now(),
        }
    }

    fn pause(&self) {
        let peer = PeerId::from_libp2p(self.peer);
        assert!(
            self.forwarder.pause_data_path_maintenance(
                peer,
                self.forwarder
                    .application_demand(peer)
                    .unwrap()
                    .version
                    .unwrap()
            )
        );
    }

    fn eligible(&self) -> bool {
        eligible(&self.forwarder, &self.local, true, &self.pairing, false)
    }

    fn reconcile(&mut self) {
        reconcile(Context {
            node: &mut self.node,
            forwarder: &self.forwarder,
            local: &self.local,
            relay: &AutoRelayState::default(),
            readiness: &RelayReadiness::default(),
            epochs: &ConnectionEpochs::default(),
            connections: &HashMap::new(),
            pairing: &self.pairing,
            discovered: &mut self.discovered,
            now: self.now,
        });
    }
}

#[tokio::test]
async fn quiet_discovery_requires_activation_current_pause_incoming_and_nonserving_role() {
    let mut f = Fixture::new();
    assert!(!f.eligible());
    f.pause();
    assert!(
        !f.forwarder
            .data_path_is_dormant(PeerId::from_libp2p(f.peer)),
        "preparation is not quiet commitment"
    );
    assert!(f.eligible());
    assert!(!eligible(
        &f.forwarder,
        &ControlCapabilities::local("lab", None, 1280),
        true,
        &f.pairing,
        false
    ));
    assert!(!eligible(&f.forwarder, &f.local, false, &f.pairing, false));
    assert!(!eligible(&f.forwarder, &f.local, true, &f.pairing, true));
    assert!(
        f.forwarder
            .record_application_demand(PeerId::from_libp2p(f.peer), f.now)
    );
    assert!(!f.eligible(), "payload immediately invalidates the pause");
    f.pause();
    let invitation = f
        .pairing
        .open("lab", 60, current_unix_seconds_lossy(), f.now)
        .unwrap();
    assert!(!f.eligible());
    f.pairing = CodePairingSessions::new();
    f.pairing
        .join(
            "lab",
            invitation.code.parse().unwrap(),
            None,
            Vec::new(),
            60,
            current_unix_seconds_lossy(),
            f.now,
        )
        .unwrap();
    assert!(!f.eligible());
}

#[tokio::test]
async fn quiet_discovery_reconciles_real_readiness_and_preserves_control_ownership() {
    let mut f = Fixture::new();
    f.pause();
    let before = f.forwarder.application_demand(PeerId::from_libp2p(f.peer));
    f.node
        .swarm
        .behaviour_mut()
        .kad
        .set_background_jobs_paused(true);
    f.node
        .swarm
        .behaviour_mut()
        .autonat
        .as_mut()
        .unwrap()
        .set_client_probes_paused(true);
    assert!(f.discovered.recovery_queries.should_query(f.peer, f.now));
    let data = f.node.swarm.behaviour_mut().kad.get_closest_peers(f.peer);
    f.discovered.recovery_queries.record(f.peer, [data], f.now);
    f.reconcile();
    assert!(
        !f.node.swarm.behaviour().kad.background_jobs_paused(),
        "no native incoming owner exists"
    );
    assert!(
        !f.node
            .swarm
            .behaviour()
            .autonat
            .as_ref()
            .unwrap()
            .client_probes_paused()
    );
    assert!(!f.node.swarm.behaviour().kad.query_is_retained(&data));
    assert!(f.discovered.recovery_queries.can_continue_control(f.peer));
    let control = f.node.swarm.behaviour_mut().kad.get_closest_peers(f.peer);
    let scope = crate::runtime::recovery_queries::ControlQueryScope {
        peer: f.peer,
        generation: 1,
        deadline: f.now + Duration::from_secs(45),
    };
    f.discovered
        .recovery_queries
        .record_control(control, scope, f.now);
    f.reconcile();
    assert!(f.node.swarm.behaviour().kad.query_is_retained(&control));
    assert_eq!(
        f.discovered
            .recovery_queries
            .control_query(&control)
            .unwrap()
            .scope,
        scope
    );
    assert_eq!(
        f.forwarder.application_demand(PeerId::from_libp2p(f.peer)),
        before
    );
}

#[tokio::test]
async fn quiet_discovery_mixed_peer_recovery_does_not_repair_a_paused_peer() {
    let mut f = Fixture::new();
    let occupied = std::iter::once(f.forwarder.config().local_peer_id().unwrap())
        .chain(f.forwarder.configured_overlay_peers())
        .map(crate::route::builtin_ipv4)
        .collect::<HashSet<_>>();
    let missing = std::iter::repeat_with(Libp2pPeerId::random)
        .take(1024)
        .find(|peer| !occupied.contains(&crate::route::builtin_ipv4(PeerId::from_libp2p(*peer))))
        .unwrap();
    let mut config = f.forwarder.config().clone();
    config.peers.push(crate::config::PeerConfig {
        id: missing.to_string(),
        name: None,
        ip: None,
        vpn_ip: None,
        addresses: Vec::new(),
        routes: Vec::new(),
    });
    f.forwarder = Forwarder::from_config(&config).unwrap();
    f.pause();
    f.node.discovery.kademlia = true;
    let mut epochs = ConnectionEpochs::default();
    let mut maintenance = KademliaMaintenance::new(f.now);
    let mut readiness = RelayReadiness::default();
    let mut retries = ConfiguredRelayReservationRetries::default();
    for round in 0..2 {
        handle_redial_tick(
            &mut f.node,
            &mut epochs,
            &f.forwarder,
            &mut maintenance,
            &mut f.discovered,
            &PathSet::new(),
            &mut readiness,
            &mut retries,
            &PublicDiscoveryBackoff::default(),
            false,
            &RuntimeMetrics::default(),
        );
        assert!(
            f.discovered
                .recovery_queries
                .control_queries()
                .next()
                .is_none()
        );
        if round == 0 {
            let queries = f
                .node
                .swarm
                .behaviour()
                .kad
                .iter_queries()
                .map(|query| (query.id(), query.info().clone()))
                .collect::<Vec<_>>();
            assert_eq!(queries.len(), 1);
            assert!(
                matches!(&queries[0].1, kad::QueryInfo::GetClosestPeers { key, .. } if *key == missing.to_bytes())
            );
            f.discovered.recovery_queries.finished(queries[0].0, f.now);
            f.node.swarm.behaviour_mut().kad.cancel_query(&queries[0].0);
        } else {
            assert!(
                !f.discovered.recovery_queries.has_pending(),
                "the missing peer is cooling down; a paused peer must not occupy its slot"
            );
        }
    }
}
