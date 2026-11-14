use super::*;

fn add_missing_peer(f: &mut Fixture) -> Libp2pPeerId {
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
    f.node.discovery.kademlia = true;
    missing
}

fn pending_dials(f: &Fixture) -> u32 {
    f.node
        .swarm
        .network_info()
        .connection_counters()
        .num_pending_outgoing()
}

#[tokio::test]
async fn closest_results_cache_paused_peers_without_automatic_dial_effects() {
    for timed_out in [false, true] {
        for paused in [false, true] {
            for lan_first in [false, true] {
                let mut f = Fixture::new();
                let missing = add_missing_peer(&mut f);
                if paused {
                    f.pause();
                } else {
                    f.local = ControlCapabilities::local("lab", None, 1280);
                }
                if lan_first {
                    assert!(f.discovered.start_lan_first_recovery(missing, f.now));
                }
                let before = f.forwarder.application_demand(PeerId::from_libp2p(f.peer));
                let address: Multiaddr = "/ip4/11.252.0.2/tcp/4001".parse().unwrap();
                let other_address: Multiaddr = "/ip4/11.252.0.3/tcp/4001".parse().unwrap();
                let peers = vec![
                    kad::PeerInfo {
                        peer_id: f.peer,
                        addrs: vec![address.clone()],
                    },
                    kad::PeerInfo {
                        peer_id: missing,
                        addrs: vec![other_address.clone()],
                    },
                ];
                let result = kad::QueryResult::GetClosestPeers(if timed_out {
                    Err(kad::GetClosestPeersError::Timeout {
                        key: missing.to_bytes(),
                        peers,
                    })
                } else {
                    Ok(kad::GetClosestPeersOk {
                        key: missing.to_bytes(),
                        peers,
                    })
                });
                let metrics = RuntimeMetrics::default();
                let mut epochs = ConnectionEpochs::default();
                // Observe admitted Swarm dials without polling transport futures.
                assert_eq!(pending_dials(&f), 0);
                handle_kademlia_closest_peer_result(
                    &mut f.node.swarm,
                    &mut epochs,
                    &f.forwarder,
                    &mut InfrastructurePeers::default(),
                    &mut AutoRelayState::default(),
                    &mut f.discovered,
                    &PathSet::new(),
                    &metrics,
                    &f.node.discovery,
                    false,
                    &result,
                );

                let expected = u32::from(!paused) + u32::from(!lan_first);
                assert_eq!(pending_dials(&f), expected);
                assert_eq!(epochs.connections.len(), expected as usize);
                assert!(f.discovered.as_vec().contains(&(f.peer, address)));
                assert!(f.discovered.as_vec().contains(&(missing, other_address)));
                assert!(
                    !f.node
                        .swarm
                        .behaviour_mut()
                        .kad
                        .addresses_of_peer(f.peer)
                        .is_empty()
                );
                assert_eq!(
                    f.discovered
                        .recovery_dial_attempts
                        .keys()
                        .any(|(peer, _)| *peer == f.peer),
                    !paused
                );
                assert!(f.discovered.recovery_queries.state(&f.peer).is_none());
                assert_eq!(
                    metrics
                        .snapshot(crate::queue::QueueStats::default())
                        .discovered_address_dial_attempts,
                    u64::from(expected)
                );
                assert_eq!(
                    f.forwarder.application_demand(PeerId::from_libp2p(f.peer)),
                    before
                );
            }
        }
    }
}

#[tokio::test]
async fn providers_skip_paused_peers_and_preserve_legacy_lan_first_dial_effects() {
    for paused in [false, true] {
        for lan_first in [false, true] {
            let mut f = Fixture::new();
            let missing = add_missing_peer(&mut f);
            if paused {
                f.pause();
            } else {
                f.local = ControlCapabilities::local("lab", None, 1280);
            }
            let before = f.forwarder.application_demand(PeerId::from_libp2p(f.peer));
            let address: Multiaddr = "/ip4/11.252.0.2/tcp/4001".parse().unwrap();
            for peer in [f.peer, missing] {
                f.node
                    .swarm
                    .behaviour_mut()
                    .kad
                    .add_address(&peer, address.clone());
            }
            if lan_first {
                assert!(f.discovered.start_lan_first_recovery(missing, f.now));
            }
            let metrics = RuntimeMetrics::default();
            let mut epochs = ConnectionEpochs::default();
            dial_kademlia_providers(
                &mut f.node.swarm,
                &mut epochs,
                &f.forwarder,
                &metrics,
                &HashSet::from([f.peer, missing]),
                |peer| !f.discovered.lan_first_recovery_active(peer, f.now),
            );

            let expected = u32::from(!paused) + u32::from(!lan_first);
            assert_eq!(pending_dials(&f), expected);
            assert_eq!(epochs.connections.len(), expected as usize);
            assert!(
                !f.node
                    .swarm
                    .behaviour_mut()
                    .kad
                    .addresses_of_peer(f.peer)
                    .is_empty()
            );
            let snapshot = metrics.snapshot(crate::queue::QueueStats::default());
            assert_eq!(
                snapshot.kademlia_provider_dial_attempts,
                u64::from(expected)
            );
            assert_eq!(snapshot.kademlia_provider_dial_failures, 0);
            assert_eq!(snapshot.kademlia_providers_ignored, 2 - u64::from(expected));
            assert!(f.discovered.recovery_dial_attempts.is_empty());
            assert!(f.discovered.recovery_queries.state(&f.peer).is_none());
            assert_eq!(
                f.forwarder.application_demand(PeerId::from_libp2p(f.peer)),
                before
            );
        }
    }
}

#[tokio::test]
async fn learned_addresses_cache_without_automatic_dial_effects_or_cooldown_until_demand() {
    for source in [
        DiscoveredPeerAddressSource::PublicDiscovery,
        DiscoveredPeerAddressSource::AuthenticatedPeerRecord,
        DiscoveredPeerAddressSource::AuthenticatedControlHint,
        DiscoveredPeerAddressSource::AuthenticatedPeerIdentify,
        DiscoveredPeerAddressSource::LocalDiscovery,
    ] {
        for lan_promotion in [false, true] {
            let mut f = Fixture::new();
            f.node.discovery.kademlia = true;
            f.pause();
            let peer = PeerId::from_libp2p(f.peer);
            let before = f.forwarder.application_demand(peer);
            let address: Multiaddr = "/ip4/11.252.0.2/tcp/4001".parse().unwrap();
            let mut epochs = ConnectionEpochs::default();
            let metrics = RuntimeMetrics::default();
            for demanded in [false, true] {
                if demanded {
                    assert!(f.forwarder.record_application_demand(peer, f.now));
                }
                if lan_promotion {
                    learn_peer_address_with_policy(
                        &mut f.node.swarm,
                        &mut epochs,
                        &f.forwarder,
                        &mut f.discovered,
                        &PathSet::new(),
                        &metrics,
                        f.peer,
                        address.clone(),
                        &f.node.discovery,
                        source,
                        true,
                    );
                } else {
                    learn_peer_address(
                        &mut f.node.swarm,
                        &mut epochs,
                        &f.forwarder,
                        &mut f.discovered,
                        &PathSet::new(),
                        &metrics,
                        f.peer,
                        address.clone(),
                        &f.node.discovery,
                        source,
                    );
                }

                assert_eq!(pending_dials(&f), u32::from(demanded));
                assert_eq!(epochs.connections.len(), usize::from(demanded));
                assert_eq!(f.discovered.as_vec(), vec![(f.peer, address.clone())]);
                assert!(
                    !f.node
                        .swarm
                        .behaviour_mut()
                        .kad
                        .addresses_of_peer(f.peer)
                        .is_empty()
                );
                assert_eq!(
                    f.discovered.recovery_dial_attempts.len(),
                    usize::from(demanded)
                );
                assert!(f.discovered.recovery_queries.state(&f.peer).is_none());
                assert_eq!(
                    metrics
                        .snapshot(crate::queue::QueueStats::default())
                        .discovered_address_dial_attempts,
                    u64::from(demanded)
                );
                if !demanded {
                    assert_eq!(f.forwarder.application_demand(peer), before);
                    assert!(f.forwarder.data_path_maintenance_is_paused(peer));
                }
            }
        }
    }
}

#[tokio::test]
async fn scoped_control_discovery_retains_dial_effects_for_a_paused_peer() {
    let mut f = Fixture::new();
    f.node.discovery.kademlia = true;
    f.pause();
    let peer = PeerId::from_libp2p(f.peer);
    let before = f.forwarder.application_demand(peer);
    let metrics = RuntimeMetrics::default();
    let mut epochs = ConnectionEpochs::default();
    let mut syncs = MembershipRecordSyncs::default();
    syncs.queue_capabilities(f.local.clone(), &f.forwarder);
    for elapsed in [0, 15] {
        control_delivery::drive(
            &mut syncs,
            &mut f.node,
            &f.forwarder,
            &mut epochs,
            &mut f.discovered,
            &metrics,
            f.now + Duration::from_secs(elapsed),
        );
    }
    let (id, owner) = f
        .discovered
        .recovery_queries
        .control_queries()
        .next()
        .unwrap();
    let address: Multiaddr = "/ip4/11.252.0.2/tcp/4001".parse().unwrap();
    f.node
        .swarm
        .behaviour_mut()
        .kad
        .add_address(&f.peer, address.clone());
    let key = crate::runtime::p2p::kademlia_rendezvous_key("lab", None);
    let key = crate::runtime::p2p::kademlia_provider_wire_key(&f.node.swarm.behaviour().kad, &key);
    let mut membership = OverlayMembership::from_config(f.forwarder.config()).unwrap();
    let mut tun = TunRuntimeConfig::from_config(f.forwarder.config()).unwrap();
    handle_kademlia_event_at(
        &mut f.node.swarm,
        KademliaEventContext {
            connection_epochs: &mut epochs,
            forwarder: &mut f.forwarder,
            membership_record_syncs: &mut syncs,
            membership: &mut membership,
            tun_runtime: &mut tun,
            route_controller: &mut PreconfiguredTunRoutes,
            infrastructure_peers: &mut InfrastructurePeers::default(),
            auto_relay: &mut AutoRelayState::default(),
            discovered_peer_addresses: &mut f.discovered,
            paths: &PathSet::new(),
            local_capabilities: &mut f.local,
            previous_membership_tags: &[],
            metrics: &metrics,
            discovery: &f.node.discovery,
            identity: &f.node.identity,
            code_pairing_sessions: &mut f.pairing,
            public_discovery_quiet: true,
            kademlia_maintenance: &mut KademliaMaintenance::new(f.now),
        },
        kad::Event::OutboundQueryProgressed {
            id,
            result: kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                key,
                providers: HashSet::from([f.peer]),
            })),
            stats: kad::QueryStats::empty(),
            step: kad::ProgressStep {
                count: std::num::NonZeroUsize::MIN,
                last: true,
            },
        },
        f.now + Duration::from_secs(15),
    );

    let mut cx = std::task::Context::from_waker(futures::task::noop_waker_ref());
    let std::task::Poll::Ready(libp2p::swarm::ToSwarm::Dial { opts }) =
        libp2p::swarm::NetworkBehaviour::poll(&mut f.node.swarm.behaviour_mut().control, &mut cx)
    else {
        panic!("scoped control discovery must still request its target dial");
    };
    assert_eq!(opts.get_peer_id(), Some(f.peer));
    assert_eq!(pending_dials(&f), 0);
    epochs.dial(&mut f.node.swarm, opts).unwrap();
    assert_eq!(pending_dials(&f), 1);
    assert!(
        f.discovered
            .as_vec()
            .iter()
            .any(|(cached, _)| *cached == f.peer)
    );
    assert!(f.discovered.recovery_dial_attempts.is_empty());
    assert!(!f.discovered.recovery_queries.has_pending());
    assert_eq!(owner.scope.deadline, f.now + Duration::from_secs(45));
    assert_eq!(f.forwarder.application_demand(peer), before);
    assert!(f.forwarder.data_path_maintenance_is_paused(peer));
}
