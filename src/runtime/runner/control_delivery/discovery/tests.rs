use super::*;
use crate::runtime::runner::tests::{config_with_peer, membership_sync_test_node};

struct Fixture {
    node: P2pNode,
    remote: NodeIdentity,
    peer: Libp2pPeerId,
    forwarder: Forwarder,
    syncs: MembershipRecordSyncs,
    discovered: DiscoveredPeerAddresses,
    epochs: ConnectionEpochs,
    metrics: RuntimeMetrics,
    capabilities: ControlCapabilities,
    membership: OverlayMembership,
    tun: TunRuntimeConfig,
    paths: PathSet,
    infrastructure: InfrastructurePeers,
    auto_relay: AutoRelayState,
    pairing: CodePairingSessions,
    maintenance: KademliaMaintenance,
    now: Instant,
}

impl Fixture {
    fn new(public: bool) -> Self {
        let local = NodeIdentity::generate_ed25519().unwrap();
        let remote = NodeIdentity::generate_ed25519().unwrap();
        let peer = remote.peer_id.parse().unwrap();
        let mut config = config_with_peer(&local, peer);
        config.network.membership_key =
            Some(base64::engine::general_purpose::STANDARD.encode([7; 32]));
        let capabilities =
            ControlCapabilities::local("lab", config.membership_tag().unwrap(), 1280);
        let forwarder = Forwarder::from_config(&config).unwrap();
        let overlay = PeerId::from_libp2p(peer);
        let demand = forwarder.application_demand(overlay).unwrap();
        assert!(forwarder.grant_data_path_quiet(overlay, demand.version.unwrap()));
        let mut node = membership_sync_test_node(local);
        node.discovery.kademlia = true;
        node.membership_tag = capabilities.membership_tag.clone();
        node.bootstrap_peer_addresses.clear();
        let protocol = if public {
            PUBLIC_IPFS_KADEMLIA_PROTOCOL
        } else {
            crate::config::PRIVATE_KADEMLIA_PROTOCOL
        };
        node.swarm.behaviour_mut().kad = kad::Behaviour::with_config(
            node.local_peer_id,
            kad::store::MemoryStore::new(node.local_peer_id),
            crate::runtime::p2p::controlled_kademlia_config(
                libp2p::StreamProtocol::try_from_owned(protocol.to_owned()).unwrap(),
            ),
        );
        node.swarm
            .behaviour_mut()
            .kad
            .set_mode(Some(kad::Mode::Client));
        let now = Instant::now();
        Self {
            node,
            remote,
            peer,
            forwarder,
            syncs: MembershipRecordSyncs::default(),
            discovered: DiscoveredPeerAddresses::default(),
            epochs: ConnectionEpochs::default(),
            metrics: RuntimeMetrics::default(),
            capabilities,
            membership: OverlayMembership::from_config(&config).unwrap(),
            tun: TunRuntimeConfig::from_config(&config).unwrap(),
            paths: PathSet::new(),
            infrastructure: InfrastructurePeers::default(),
            auto_relay: AutoRelayState::default(),
            pairing: CodePairingSessions::new(),
            maintenance: KademliaMaintenance::new(now),
            now,
        }
    }

    fn drive(&mut self, elapsed: u64) {
        self.syncs
            .queue_capabilities(self.capabilities.clone(), &self.forwarder);
        super::super::drive(
            &mut self.syncs,
            &mut self.node,
            &self.forwarder,
            &mut self.epochs,
            &mut self.discovered,
            &self.metrics,
            self.now + Duration::from_secs(elapsed),
        );
    }

    fn query(&self) -> (kad::QueryId, crate::runtime::recovery_queries::ControlQuery) {
        self.discovered
            .recovery_queries
            .control_queries()
            .next()
            .unwrap()
    }

    fn provider_key(&self) -> kad::RecordKey {
        let key = crate::runtime::p2p::kademlia_rendezvous_key(
            "lab",
            self.capabilities.membership_tag.as_deref(),
        );
        crate::runtime::p2p::kademlia_provider_wire_key(&self.node.swarm.behaviour().kad, &key)
    }

    fn dispatch(&mut self, id: kad::QueryId, result: kad::QueryResult, last: bool) {
        self.dispatch_native(kad::Event::OutboundQueryProgressed {
            id,
            result,
            stats: kad::QueryStats::empty(),
            step: kad::ProgressStep {
                count: std::num::NonZeroUsize::MIN,
                last,
            },
        });
    }

    fn dispatch_native(&mut self, event: kad::Event) {
        handle_kademlia_event_at(
            &mut self.node.swarm,
            KademliaEventContext {
                connection_epochs: &mut self.epochs,
                forwarder: &mut self.forwarder,
                membership_record_syncs: &mut self.syncs,
                membership: &mut self.membership,
                tun_runtime: &mut self.tun,
                route_controller: &mut PreconfiguredTunRoutes,
                infrastructure_peers: &mut self.infrastructure,
                auto_relay: &mut self.auto_relay,
                discovered_peer_addresses: &mut self.discovered,
                paths: &self.paths,
                local_capabilities: &mut self.capabilities,
                previous_membership_tags: &[],
                metrics: &self.metrics,
                discovery: &self.node.discovery,
                identity: &self.node.identity,
                code_pairing_sessions: &mut self.pairing,
                public_discovery_quiet: true,
                kademlia_maintenance: &mut self.maintenance,
            },
            event,
            self.now + Duration::from_secs(15),
        );
    }
}

#[tokio::test]
async fn control_discovery_waits_for_lan_and_survives_quiet_cancellation() {
    let mut fixture = Fixture::new(true);
    let before = fixture
        .forwarder
        .application_demand(PeerId::from_libp2p(fixture.peer));
    fixture.drive(0);
    fixture.drive(14);
    assert!(!fixture.discovered.recovery_queries.has_pending());
    fixture.drive(15);
    let (id, owner) = fixture.query();
    assert_eq!(owner.scope.peer, fixture.peer);
    assert_eq!(owner.scope.deadline, fixture.now + DELIVERY_WINDOW);
    assert!(
        matches!(fixture.node.swarm.behaviour().kad.query(&id).unwrap().info(),
        kad::QueryInfo::GetProviders { key, .. } if key == &fixture.provider_key())
    );
    assert_eq!(
        fixture.discovered.cancel_recovery_discovery_queries(
            &mut fixture.node.swarm.behaviour_mut().kad,
            fixture.now
        ),
        0
    );
    assert!(fixture.node.swarm.behaviour().kad.query_is_retained(&id));
    assert!(!fixture.discovered.should_query_recovery_discovery_at(
        Libp2pPeerId::random(),
        fixture.now + Duration::from_secs(15)
    ));
    assert!(fixture.syncs.pending.is_empty());
    assert_eq!(
        fixture
            .forwarder
            .application_demand(PeerId::from_libp2p(fixture.peer)),
        before
    );
    assert!(
        fixture
            .forwarder
            .data_path_is_dormant(PeerId::from_libp2p(fixture.peer))
    );
}

#[tokio::test]
async fn control_discovery_fallback_is_two_stages_inside_the_original_round() {
    let mut fixture = Fixture::new(true);
    fixture.drive(0);
    fixture.drive(15);
    let (first, owner) = fixture.query();
    fixture.drive(25);
    let (second, fallback) = fixture.query();
    assert_ne!(first, second);
    assert_eq!(owner.scope, fallback.scope);
    assert_eq!(fallback.expires_at, fixture.now + Duration::from_secs(35));
    assert!(
        matches!(fixture.node.swarm.behaviour().kad.query(&second).unwrap().info(),
        kad::QueryInfo::GetClosestPeers { key, .. } if key == &fixture.peer.to_bytes())
    );
    fixture.drive(35);
    assert!(!fixture.discovered.recovery_queries.has_pending());
    for tick in 45..1000 {
        fixture.drive(tick);
    }
    assert_eq!(
        fixture
            .metrics
            .snapshot(crate::queue::QueueStats::default())
            .kademlia_provider_lookups,
        2
    );
    assert_eq!(
        fixture
            .node
            .swarm
            .behaviour()
            .kad
            .query_pool_usage()
            .retained,
        0
    );
}

#[tokio::test]
async fn control_discovery_clamps_late_queries_and_supersession_purges_native_work() {
    let mut fixture = Fixture::new(true);
    fixture.drive(0);
    fixture.drive(40);
    let (id, owner) = fixture.query();
    assert_eq!(owner.expires_at, fixture.now + DELIVERY_WINDOW);
    fixture.capabilities.effective_mtu = 1234;
    fixture.drive(41);
    assert!(!fixture.node.swarm.behaviour().kad.query_is_retained(&id));
    assert!(
        fixture
            .discovered
            .recovery_queries
            .control_query(&id)
            .is_none()
    );
    assert!(
        fixture
            .discovered
            .recovery_queries
            .control_queries()
            .next()
            .is_none(),
        "new generations retain failure cooldown"
    );
}

#[tokio::test]
async fn control_discovery_ignores_foreign_providers_keys_and_private_public_hints() {
    let mut fixture = Fixture::new(true);
    fixture.drive(0);
    fixture.drive(15);
    let (id, _) = fixture.query();
    fixture
        .node
        .swarm
        .behaviour_mut()
        .kad
        .add_address(&fixture.peer, "/ip4/127.0.0.1/tcp/4001".parse().unwrap());
    for (key, providers) in [
        (
            kad::RecordKey::new(&b"wrong-scope"),
            HashSet::from([fixture.peer]),
        ),
        (
            fixture.provider_key(),
            HashSet::from([Libp2pPeerId::random()]),
        ),
        (fixture.provider_key(), HashSet::from([fixture.peer])),
    ] {
        fixture.dispatch(
            id,
            kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                key,
                providers,
            })),
            false,
        );
        assert!(fixture.discovered.as_vec().is_empty());
        assert!(fixture.syncs.pending.is_empty());
    }
    assert!(fixture.node.swarm.behaviour().kad.query_is_retained(&id));
}

#[tokio::test]
async fn control_discovery_provider_result_caches_only_its_target_without_global_dials() {
    let mut fixture = Fixture::new(true);
    fixture.drive(0);
    fixture.drive(15);
    let (id, _) = fixture.query();
    let address: Multiaddr = "/ip4/11.252.0.2/tcp/4001".parse().unwrap();
    fixture
        .node
        .swarm
        .behaviour_mut()
        .kad
        .add_address(&fixture.peer, address.clone());
    let other = Libp2pPeerId::random();
    fixture.dispatch(
        id,
        kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
            key: fixture.provider_key(),
            providers: HashSet::from([fixture.peer, other]),
        })),
        false,
    );
    assert_eq!(
        fixture.discovered.peer_ids().collect::<Vec<_>>(),
        vec![fixture.peer]
    );
    assert_eq!(fixture.syncs.pending.len(), 1);
    assert!(
        fixture
            .node
            .swarm
            .behaviour()
            .control
            .has_pending_requests(&fixture.peer)
    );
    assert!(
        !fixture
            .node
            .swarm
            .behaviour()
            .control
            .has_pending_requests(&other)
    );
    assert!(fixture.infrastructure.peers.is_empty());
    assert!(!fixture.discovered.recovery_queries.has_pending());
    assert!(!fixture.node.swarm.behaviour().kad.query_is_retained(&id));
    remove_retained_discovery_address(
        &mut fixture.node.swarm,
        &mut fixture.discovered,
        fixture.peer,
        &address,
    );
    use libp2p::swarm::NetworkBehaviour as _;
    let remaining = fixture
        .node
        .swarm
        .behaviour_mut()
        .control
        .handle_pending_outbound_connection(
            ConnectionId::new_unchecked(1),
            Some(fixture.peer),
            &[],
            libp2p::core::Endpoint::Dialer,
        )
        .unwrap();
    assert!(
        remaining.is_empty(),
        "address eviction reaches the native control cache"
    );
}

#[tokio::test]
async fn control_discovery_private_records_keep_signature_key_and_target_checks() {
    for invalid in ["none", "signature", "network", "key", "transport"] {
        let mut fixture = Fixture::new(false);
        fixture.drive(0);
        fixture.drive(15);
        let (id, _) = fixture.query();
        let key = crate::runtime::p2p::kademlia_peer_addresses_key(
            "lab",
            fixture.capabilities.membership_tag.as_deref(),
            fixture.peer,
        );
        assert!(
            matches!(fixture.node.swarm.behaviour().kad.query(&id).unwrap().info(), kad::QueryInfo::GetRecord { key: actual, .. } if actual == &key)
        );
        let address: Multiaddr = if invalid == "transport" {
            "/ip4/127.0.0.1/tcp/4001/ws"
        } else {
            "/ip4/127.0.0.1/tcp/4001"
        }
        .parse()
        .unwrap();
        let mut value = encode_kademlia_peer_address_record(
            if invalid == "network" {
                "foreign"
            } else {
                "lab"
            },
            fixture.capabilities.membership_tag.as_deref(),
            &fixture.remote,
            vec![address],
            current_unix_seconds_lossy(),
        )
        .unwrap();
        if invalid == "signature" {
            let mut record: KademliaPeerAddressRecord = serde_json::from_slice(&value).unwrap();
            record.signature[0] ^= 1;
            value = serde_json::to_vec(&record).unwrap();
        }
        fixture.dispatch(
            id,
            kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(kad::PeerRecord {
                peer: None,
                record: kad::Record {
                    key: if invalid == "key" {
                        kad::RecordKey::new(&b"foreign")
                    } else {
                        key
                    },
                    value,
                    publisher: None,
                    expires: None,
                },
            }))),
            false,
        );
        assert_eq!(
            !fixture.discovered.as_vec().is_empty(),
            invalid == "none",
            "{invalid}"
        );
        assert!(
            fixture
                .forwarder
                .data_path_is_dormant(PeerId::from_libp2p(fixture.peer))
        );
    }
}

#[tokio::test]
async fn control_discovery_revocation_and_network_reset_cancel_native_ownership() {
    for revoked in [false, true] {
        let mut fixture = Fixture::new(true);
        fixture.drive(0);
        fixture.drive(15);
        let (id, _) = fixture.query();
        if revoked {
            let mut config = fixture.forwarder.config().clone();
            config.peers.clear();
            let update = fixture
                .forwarder
                .prepare_reconfigure(config, current_unix_seconds_lossy())
                .unwrap();
            fixture.forwarder.try_commit_reconfigure(update).unwrap();
            reconcile(
                &mut fixture.syncs,
                &mut fixture.node.swarm,
                &fixture.forwarder,
                &mut fixture.discovered,
                &fixture.metrics,
                fixture.now + Duration::from_secs(16),
            );
        } else {
            fixture
                .discovered
                .reset_recovery_backoff(&mut fixture.node.swarm.behaviour_mut().kad);
            fixture.syncs.retry_capabilities_after_network_change();
        }
        assert!(!fixture.node.swarm.behaviour().kad.query_is_retained(&id));
        assert!(!fixture.discovered.recovery_queries.has_pending());
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "isolated native public-provider lookup; requires user/network namespaces and iproute2"]
fn control_discovery_native_public_client_namespace() {
    use std::{env, fs, process::Command};
    const CHILD: &str = "P2P_VPN_CONTROL_DISCOVERY_CHILD";
    const PARENT: &str = "P2P_VPN_CONTROL_DISCOVERY_PARENT_NETNS";
    if env::var_os(CHILD).is_none() {
        let status = Command::new("unshare")
            .args(["--user", "--map-root-user", "--net", "--"])
            .arg(env::current_exe().unwrap())
            .args(["--exact", "runtime::runner::control_delivery::discovery::tests::control_discovery_native_public_client_namespace",
                "--ignored", "--nocapture"])
            .env(CHILD, "1")
            .env(PARENT, fs::read_link("/proc/self/ns/net").unwrap())
            .status().unwrap();
        assert!(
            status.success(),
            "isolated native control discovery child failed"
        );
        return;
    }
    assert_ne!(
        fs::read_link("/proc/self/ns/net").unwrap(),
        std::path::PathBuf::from(env::var_os(PARENT).unwrap()),
        "never configure addresses in the parent's network namespace"
    );
    assert!(
        Command::new("ip")
            .args(["link", "set", "lo", "up"])
            .status()
            .unwrap()
            .success()
    );
    for ip in ["11.252.0.1/32", "11.252.0.2/32"] {
        assert!(
            Command::new("ip")
                .args(["address", "add", ip, "dev", "lo"])
                .status()
                .unwrap()
                .success()
        );
    }
    let routes = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .unwrap();
    assert!(routes.status.success());
    assert!(
        routes.stdout.is_empty(),
        "fixture has no route to external networks"
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let stage = std::cell::Cell::new("initializing");
            tokio::time::timeout(Duration::from_secs(20), public_client_scenario(&stage))
                .await
                .unwrap_or_else(|_| {
                    panic!("native public-provider scenario stalled: {}", stage.get())
                });
        });
}

#[cfg(target_os = "linux")]
async fn public_client_scenario(stage: &std::cell::Cell<&'static str>) {
    use crate::runtime::runner::tests::dispatch_membership_sync_test_event_with_capabilities;
    use libp2p::kad::store::RecordStore as _;

    fn public_node(identity: NodeIdentity) -> P2pNode {
        let mut node = membership_sync_test_node(identity);
        node.bootstrap_peer_addresses.clear();
        node.swarm.behaviour_mut().kad = kad::Behaviour::with_config(
            node.local_peer_id,
            kad::store::MemoryStore::new(node.local_peer_id),
            crate::runtime::p2p::controlled_kademlia_config(libp2p::StreamProtocol::new(
                PUBLIC_IPFS_KADEMLIA_PROTOCOL,
            )),
        );
        node
    }

    async fn listen(node: &mut P2pNode, ip: &str) -> Multiaddr {
        node.swarm
            .listen_on(format!("/ip4/{ip}/tcp/0").parse().unwrap())
            .unwrap();
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = node.swarm.select_next_some().await {
                return address;
            }
        }
    }
    let mut fixture = Fixture::new(true);
    let before = fixture
        .forwarder
        .application_demand(PeerId::from_libp2p(fixture.peer));
    let mut router = public_node(NodeIdentity::generate_ed25519().unwrap());
    router
        .swarm
        .behaviour_mut()
        .kad
        .set_mode(Some(kad::Mode::Server));
    stage.set("router listen");
    let router_address = listen(&mut router, "11.252.0.1").await;
    let mut publisher = public_node(fixture.remote.clone());
    publisher
        .swarm
        .behaviour_mut()
        .kad
        .set_mode(Some(kad::Mode::Client));
    stage.set("publisher listen");
    let publisher_address = listen(&mut publisher, "11.252.0.2").await;
    let provider_address = publisher_address.clone().with_p2p(fixture.peer).unwrap();
    publisher
        .swarm
        .add_external_address(publisher_address.clone());
    publisher
        .swarm
        .behaviour_mut()
        .kad
        .add_address(&router.local_peer_id, router_address.clone());
    let publication = start_kademlia_peer_address_record_publication(
        true,
        &mut publisher.swarm,
        "lab",
        fixture.capabilities.membership_tag.as_deref(),
        &fixture.remote,
    )
    .unwrap()
    .unwrap();
    let key = fixture.provider_key();
    let mut publication_complete = false;
    stage.set("provider publication");
    loop {
        tokio::select! {
            event = publisher.swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Kad(kad::Event::OutboundQueryProgressed {
                    id, result: kad::QueryResult::StartProviding(result), step, ..
                })) = event {
                    if id == publication && step.last {
                        result.unwrap(); publication_complete = true;
                        stage.set("routing peer stores provider");
                    }
                }
            }
            _ = router.swarm.select_next_some() => (),
        }
        if publication_complete
            && router
                .swarm
                .behaviour_mut()
                .kad
                .store_mut()
                .providers(&key)
                .iter()
                .any(|provider| {
                    provider.provider == fixture.peer
                        && provider.addresses.contains(&provider_address)
                })
        {
            break;
        }
    }
    let providers = router.swarm.behaviour_mut().kad.store_mut().providers(&key);
    assert!(
        providers
            .iter()
            .any(|provider| provider.provider == fixture.peer
                && provider.addresses.contains(&provider_address))
    );
    assert_eq!(publisher.swarm.behaviour().kad.mode(), kad::Mode::Client);
    assert!(
        !router
            .swarm
            .behaviour_mut()
            .kad
            .kbuckets()
            .any(|bucket| bucket
                .iter()
                .any(|entry| *entry.node.key.preimage() == fixture.peer)),
        "publisher is not a DHT routing server"
    );
    let unrelated = Libp2pPeerId::random();
    router
        .swarm
        .behaviour_mut()
        .kad
        .store_mut()
        .add_provider(kad::ProviderRecord {
            key,
            provider: unrelated,
            expires: None,
            addresses: vec!["/ip4/11.252.0.99/tcp/4001".parse().unwrap()],
        })
        .unwrap();
    fixture
        .node
        .swarm
        .behaviour_mut()
        .kad
        .add_address(&router.local_peer_id, router_address);
    assert!(
        fixture
            .node
            .swarm
            .behaviour_mut()
            .kad
            .addresses_of_peer(fixture.peer)
            .is_empty()
    );
    assert!(fixture.node.configured_peer_addresses.is_empty());
    fixture.drive(0);
    fixture.drive(15);
    let (query, _) = fixture.query();
    let mut received = 0;
    stage.set("provider lookup and control delivery");
    loop {
        tokio::select! {
            event = fixture.node.swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(BehaviourEvent::Kad(event)) => fixture.dispatch_native(event),
                SwarmEvent::ConnectionEstablished { peer_id, connection_id, .. } => {
                    fixture.epochs.record_established(connection_id);
                    if peer_id == fixture.peer {
                        fixture.syncs.control_connection_available(peer_id, connection_id);
                        fixture.syncs.drive_capabilities(&mut fixture.node.swarm, &fixture.forwarder, &fixture.metrics,
                            fixture.now + Duration::from_secs(15));
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Control(event)) => {
                    dispatch_membership_sync_test_event_with_capabilities(&mut fixture.node, &mut fixture.forwarder,
                        &mut fixture.syncs, &fixture.metrics, &mut fixture.epochs, &mut fixture.capabilities, event).await;
                }
                SwarmEvent::Dialing { peer_id: Some(peer), .. } => assert!(
                    [router.local_peer_id, fixture.peer].contains(&peer),
                    "only the isolated router and target may be dialed: {peer}"
                ),
                _ => (),
            },
            event = publisher.swarm.select_next_some() => {
                if let SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                    message: Message::Request { request: ControlRequest::Capabilities(capabilities), channel, .. }, ..
                })) = event {
                    assert!(crate::runtime::control::validate_capabilities(&capabilities, "lab",
                        fixture.capabilities.membership_tag.as_deref(), &[]).is_none());
                    received += 1;
                    stage.set("control acknowledgement");
                    publisher.swarm.behaviour_mut().control.send_response(channel,
                        ControlResponse::CapabilitiesAccepted(fixture.capabilities.clone())).unwrap();
                }
            }
            _ = router.swarm.select_next_some() => (),
        }
        if received > 0
            && fixture.syncs.pending.is_empty()
            && fixture.syncs.capabilities.peers[&fixture.peer].acknowledged
                == fixture.syncs.capabilities.generation
        {
            break;
        }
    }
    assert_eq!(received, 1);
    assert_eq!(
        fixture
            .metrics
            .snapshot(crate::queue::QueueStats::default())
            .kademlia_provider_lookups,
        1
    );
    assert!(!fixture.node.swarm.behaviour().kad.query_is_retained(&query));
    assert!(!fixture.discovered.recovery_queries.has_pending());
    assert_eq!(
        fixture.discovered.peer_ids().collect::<Vec<_>>(),
        vec![fixture.peer]
    );
    assert!(fixture.infrastructure.peers.is_empty());
    assert!(!fixture.node.swarm.is_connected(&unrelated));
    assert!(
        !fixture
            .node
            .swarm
            .behaviour_mut()
            .kad
            .kbuckets()
            .any(|bucket| bucket
                .iter()
                .any(|entry| *entry.node.key.preimage() == fixture.peer)),
        "control cache must not promote a client to DHT server"
    );
    assert_eq!(
        fixture
            .forwarder
            .application_demand(PeerId::from_libp2p(fixture.peer)),
        before
    );
    assert!(
        fixture
            .forwarder
            .data_path_is_dormant(PeerId::from_libp2p(fixture.peer))
    );
}
