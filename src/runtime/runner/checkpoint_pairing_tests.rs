mod checkpoint_joiner {
    use super::*;
    use crate::membership::checkpoint::{MembershipChange, MembershipSyncState};
    use crate::runtime::pairing_sessions::CheckpointJoinerOwnership;

    struct JoinFixture {
        directory: PathBuf,
        identity: NodeIdentity,
        config: Config,
        forwarder: Forwarder,
        membership: OverlayMembership,
        tun: TunRuntimeConfig,
        owner: Option<CheckpointRuntime>,
        store: MembershipStateStore,
        pairing_store: PairingStateStore,
        sessions: CodePairingSessions,
        operation: String,
        capabilities: ControlCapabilities,
    }

    impl JoinFixture {
        fn new(inviter: &CheckpointApprovalFixture) -> Self {
            let identity = inviter.joiner.clone();
            let mut config = config_with_peer(&identity, inviter.local.peer_id.parse().unwrap());
            config.peers.clear();
            config.network.dns.hostname = Some("joining-device".into());
            let path = test_pairing_state_path(&format!("checkpoint-join-{}", identity.peer_id));
            let directory = path.parent().unwrap().to_path_buf();
            let store = MembershipStateStore::new(directory.join("membership-state.json"));
            // Ordinary fresh daemon startup has already written an empty legacy envelope.
            store.save("lab", &identity.peer_id, &[], &[]).unwrap();
            let pairing_store = PairingStateStore::encrypted(
                &path,
                &identity.private_key,
                "lab",
                &identity.peer_id,
            )
            .unwrap();
            let mut sessions = CodePairingSessions::new();
            let started = sessions
                .join(
                    "lab",
                    crate::pairing_code::PairingCode::generate(),
                    Some("10.42.0.2".into()),
                    vec![],
                    600,
                    current_unix_seconds_lossy(),
                    Instant::now(),
                )
                .unwrap();
            sessions
                .set_pending_submission(
                    &started.operation_id,
                    inviter.local.peer_id.parse().unwrap(),
                    inviter.approval.request.clone(),
                    inviter.offer.clone(),
                    inviter.approval.transcript_sha256.clone(),
                    Instant::now(),
                )
                .unwrap();
            sessions
                .set_remote_pending(
                    &started.operation_id,
                    inviter.local.peer_id.parse().unwrap(),
                    inviter.offer.clone(),
                    inviter.approval.transcript_sha256.clone(),
                    inviter.approval.ticket.clone(),
                    Instant::now(),
                )
                .unwrap();
            let forwarder = Forwarder::from_config(&config).unwrap();
            let membership = OverlayMembership::from_config(&config).unwrap();
            let tun = TunRuntimeConfig::from_config(&config).unwrap();
            Self {
                directory,
                identity,
                config,
                forwarder,
                membership,
                tun,
                owner: None,
                store,
                pairing_store,
                sessions,
                operation: started.operation_id,
                capabilities: ControlCapabilities::local("lab", None, 1280),
            }
        }

        fn prepare(&mut self, inviter: &CheckpointApprovalFixture) {
            let response = inviter
                .sessions
                .open_completion(&inviter.approval.operation_id)
                .unwrap()
                .clone();
            self.sessions
                .prepare_enrollment(
                    "lab",
                    PairingEnrollmentPreparation {
                        operation_id: self.operation.clone(),
                        role: PairingEnrollmentRole::Joiner,
                        approval_id: None,
                        offer: Some(inviter.offer.clone()),
                        response,
                        transcript_sha256: inviter.approval.transcript_sha256.clone(),
                        membership_key_preconfigured: Some(false),
                    },
                )
                .unwrap();
            let ownership = if self.owner.as_ref().is_some_and(|owner| {
                !owner.enrollment_pending()
                    && owner
                        .state()
                        .snapshot()
                        .payload
                        .member(&self.identity.peer_id)
                        .is_some()
            }) {
                CheckpointJoinerOwnership::Repair
            } else {
                CheckpointJoinerOwnership::Fresh
            };
            self.sessions
                .record_checkpoint_joiner_ownership(&self.operation, ownership)
                .unwrap();
            self.sessions
                .record_tun_cleanup(
                    &self.operation,
                    super::super::super::tun::PairingTunCleanup::capture(&self.tun, &self.tun)
                        .unwrap(),
                )
                .unwrap();
            persist_code_pairing_sessions(Some(&self.pairing_store), &self.sessions, "lab")
                .unwrap();
            self.stage(current_unix_seconds_lossy());
        }

        fn stage(&mut self, wall: u64) {
            checkpoint_pairing::stage_prepared(
                &mut self.owner,
                &self.store,
                &self.sessions,
                &mut self.forwarder,
                &mut self.membership,
                &self.identity,
                wall,
            )
            .unwrap();
            self.assert_gated();
        }

        fn assert_gated(&self) {
            assert!(self.owner.as_ref().unwrap().pairing_activation_blocked());
            assert_eq!(self.forwarder.configured_transport_peers().count(), 0);
            assert!(self.forwarder.authorized_routes().is_empty());
        }

        fn select(&mut self, inviter: &CheckpointApprovalFixture) {
            let now = Instant::now();
            let owner = self.owner.as_mut().unwrap();
            owner.begin_resync(now).unwrap();
            let challenge = owner.challenge().unwrap().clone();
            let offer = inviter
                .checkpoint
                .as_ref()
                .unwrap()
                .state()
                .make_offer_at(challenge, &inviter.local, current_unix_seconds_lossy())
                .unwrap();
            owner
                .collect_offer(&offer, inviter.local.peer_id.parse().unwrap(), now)
                .unwrap();
            owner
                .finish_due(
                    &self.store,
                    &mut self.forwarder,
                    now + super::super::super::checkpoint_runtime::RESYNC_WINDOW,
                    current_unix_seconds_lossy(),
                )
                .unwrap();
            self.assert_gated();
        }

        fn restart(&mut self, wall: u64) {
            self.sessions = CodePairingSessions::restore_persisted(
                &self.pairing_store.load().unwrap().unwrap(),
                "lab",
                wall,
                Instant::now(),
            )
            .unwrap();
            let Some(PersistedAuthority::Checkpoint(loaded)) = self
                .store
                .load_authority("lab", &self.identity.peer_id, None, None)
                .unwrap()
            else {
                panic!("protected checkpoint")
            };
            self.owner = Some(
                CheckpointRuntime::restore("lab".into(), &self.identity.peer_id, *loaded).unwrap(),
            );
            self.forwarder = Forwarder::from_checkpoint_config(
                &self.config,
                self.owner.as_ref().unwrap().state(),
                self.owner.as_ref().unwrap().anchor(),
                wall,
            )
            .unwrap();
            self.membership = OverlayMembership::from_config(self.forwarder.config()).unwrap();
            self.stage(wall);
        }

        fn finalize(&mut self, routes: &mut dyn TunRouteController) -> Result<bool, RunnerError> {
            checkpoint_pairing::finalize_joiner(
                self.owner.as_mut().unwrap(),
                &mut self.sessions,
                Some(&self.pairing_store),
                &mut self.forwarder,
                &mut self.membership,
                &mut self.tun,
                routes,
                &self.identity,
                current_unix_seconds_lossy(),
            )
        }

        fn accept_wire(
            &mut self,
            node: &mut P2pNode,
            peer: Libp2pPeerId,
            request_id: request_response::OutboundRequestId,
            response: PairingCodeResponse,
        ) {
            struct UnusedPackets;
            impl super::super::super::tun::PacketRead for UnusedPackets {
                fn read_packet(&mut self, _: &mut [u8]) -> io::Result<usize> {
                    panic!("no packet reads during pairing")
                }
            }
            impl super::super::super::tun::PacketWrite for UnusedPackets {
                fn write_packet(&mut self, _: &[u8]) -> io::Result<usize> {
                    panic!("no packet writes during pairing")
                }
            }
            let (_, mut writer) = PacketIo::new(UnusedPackets, UnusedPackets).split();
            handle_pairing_code_response(
                &mut node.swarm,
                &mut SwarmEventContext {
                    checkpoint_pairing: Some(CheckpointPairingContext {
                        runtime: &mut self.owner,
                        store: &self.store,
                    }),
                    forwarder: &mut self.forwarder,
                    membership: &mut self.membership,
                    tun_runtime: &mut self.tun,
                    route_controller: &mut PreconfiguredTunRoutes,
                    infrastructure_peers: &mut InfrastructurePeers::default(),
                    routing_infrastructure_peers: &mut RoutingInfrastructurePeers::default(),
                    writer: &mut writer,
                    paths: &mut PathSet::new(),
                    peer_capabilities: &mut PeerCapabilities::default(),
                    relay_readiness: &mut RelayReadiness::default(),
                    auto_relay: &mut AutoRelayState::default(),
                    public_discovery_backoff: &mut PublicDiscoveryBackoff::default(),
                    public_discovery_holdoff_active: false,
                    relay_addresses: &[],
                    configured_peer_addresses: &[],
                    configured_relay_reservation_listeners: &mut HashSet::new(),
                    retiring_configured_relay_reservation_listeners: &mut HashSet::new(),
                    relay_server_enabled: false,
                    discovered_peer_addresses: &mut DiscoveredPeerAddresses::default(),
                    packet_in_flight: &mut PacketInFlight::new(1),
                    inbound_packet_rate_limiters: &mut PeerRateLimiters::new(1),
                    pairing_request_rate_limiters: &mut PeerRateLimiters::new(1),
                    membership_page_rate_limiters: &mut PeerRateLimiters::new(1),
                    membership_record_syncs: &mut MembershipRecordSyncs::default(),
                    pairing_handshake_rate_limiter: &mut GlobalRateLimiter::new(1, Instant::now()),
                    metrics: &RuntimeMetrics::default(),
                    local_capabilities: &mut self.capabilities,
                    persistent_packet_endpoint_candidates: &[],
                    persistent_packet_plane_quic_endpoint_candidates: &[],
                    previous_membership_tags: &[],
                    discovery: &node.discovery,
                    identity: &self.identity,
                    packet_plane: &mut PacketPlaneRuntime::disabled(),
                    packet_plane_quic: None,
                    packet_plane_negotiator: &mut PacketPlaneNegotiator::default(),
                    path_probe_tracker: &mut PathProbeTracker::default(),
                    packet_plane_session_ttl: Duration::from_secs(60),
                    packet_plane_replay_windows_per_session: 1,
                    pairing_replay_tokens: &mut PairingReplayTokens::default(),
                    code_pairing_sessions: &mut self.sessions,
                    pairing_state_store: Some(&self.pairing_store),
                    active_connections: &mut HashMap::new(),
                    connection_epochs: &mut ConnectionEpochs::default(),
                    membership_probe_connections: &mut MembershipProbeConnections::default(),
                    kademlia_maintenance: &mut KademliaMaintenance::new(Instant::now()),
                },
                peer,
                Some(PairingTransport::Direct),
                request_id,
                response,
            )
            .unwrap();
        }

        fn cancel_authority(&mut self) {
            self.sessions.cancel(&self.operation).unwrap();
            persist_code_pairing_sessions(Some(&self.pairing_store), &self.sessions, "lab")
                .unwrap();
            let entry = self.sessions.enrollment(&self.operation).unwrap().clone();
            checkpoint_pairing::cleanup_joiner_abort(
                self.owner.as_mut(),
                &entry,
                Some(&self.store),
                &mut self.sessions,
                Some(&self.pairing_store),
                &mut self.forwarder,
                &mut self.membership,
                &self.identity,
                &[],
            )
            .unwrap();
        }

        fn cleanup(&mut self, fail: bool) -> Result<(), RunnerError> {
            cleanup_pending_pairing_aborts_with(
                &mut self.sessions,
                Some(&self.pairing_store),
                "lab",
                &self.identity.peer_id,
                &self.tun,
                |_, _, _| {
                    if fail {
                        Err(io::Error::other("injected kernel cleanup failure").into())
                    } else {
                        Ok(())
                    }
                },
            )?;
            checkpoint_pairing::release_idle_barrier(
                self.owner.as_mut().unwrap(),
                &self.sessions,
                &mut self.forwarder,
                &mut self.membership,
                current_unix_seconds_lossy(),
            )
        }
    }

    impl Drop for JoinFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    fn approved() -> CheckpointApprovalFixture {
        let mut inviter = CheckpointApprovalFixture::new();
        assert!(matches!(
            inviter
                .approve(false, false, &mut PreconfiguredTunRoutes)
                .outcome,
            crate::runtime::control_socket::PairRpcOutcome::Ok { .. }
        ));
        inviter
    }

    fn formed_via_rpc() -> CheckpointApprovalFixture {
        let mut inviter = CheckpointApprovalFixture::new();
        fs::remove_file(inviter.directory.join("membership-state.json")).unwrap();
        let mut config = inviter.forwarder.config().clone();
        config.network.membership_key = None;
        inviter.forwarder = Forwarder::from_config(&config).unwrap();
        inviter.membership = OverlayMembership::from_config(&config).unwrap();
        inviter.tun = TunRuntimeConfig::from_config(&config).unwrap();
        inviter.checkpoint = None;
        inviter.sessions = CodePairingSessions::new();
        inviter.capabilities = ControlCapabilities::local("lab", None, 1280);
        inviter
            .checkpoint_store
            .save("lab", &inviter.local.peer_id, &[], &[])
            .unwrap();
        let operation = crate::runtime::pairing_sessions::fresh_pairing_operation_id();
        let response = handle_pair_rpc_request(
            &mut inviter.node.swarm,
            PairRpcRequest::PairOpen {
                operation_id: operation.clone(),
                expires_in_seconds: 600,
            },
            &mut inviter.sessions,
            Some(&inviter.pairing_store),
            &mut inviter.forwarder,
            &mut inviter.membership,
            &mut inviter.tun,
            &mut PreconfiguredTunRoutes,
            &mut inviter.capabilities,
            &inviter.local,
            &mut inviter.consumed,
            &mut inviter.promoted,
            &RuntimeMetrics::default(),
            Some(CheckpointPairingContext {
                runtime: &mut inviter.checkpoint,
                store: &inviter.checkpoint_store,
            }),
        );
        assert!(
            matches!(
                response.outcome,
                crate::runtime::control_socket::PairRpcOutcome::Ok { .. }
            ),
            "{response:?}"
        );
        assert!(inviter.checkpoint.is_some());
        assert!(inviter.capabilities.membership_tag.is_some());
        assert_eq!(inviter.forwarder.configured_transport_peers().count(), 0);
        let now = Instant::now();
        inviter
            .checkpoint
            .as_mut()
            .unwrap()
            .begin_resync(now)
            .unwrap();
        inviter
            .checkpoint
            .as_mut()
            .unwrap()
            .finish_due(
                &inviter.checkpoint_store,
                &mut inviter.forwarder,
                now + super::super::super::checkpoint_runtime::RESYNC_WINDOW,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        inviter.offer = export_code_pairing_offer_at(
            inviter.forwarder.config(),
            PairingOfferOptions::default(),
            current_unix_seconds_lossy(),
        )
        .unwrap();
        let request = build_pairing_request_at(
            &inviter.offer,
            PairingRequestOptions {
                identity: inviter.joiner.clone(),
                requested_vpn_ip: Some("10.42.0.2".into()),
                requested_routes: vec![],
            },
            current_unix_seconds_lossy(),
        )
        .unwrap();
        inviter.approval = PendingApproval::new(
            operation,
            inviter.joiner.peer_id.parse().unwrap(),
            current_unix_seconds_lossy() + 600,
            request,
        )
        .unwrap();
        inviter
            .sessions
            .set_pending_approval(inviter.approval.clone())
            .unwrap();
        assert!(matches!(
            inviter
                .approve(false, false, &mut PreconfiguredTunRoutes)
                .outcome,
            crate::runtime::control_socket::PairRpcOutcome::Ok { .. }
        ));
        inviter
    }

    #[tokio::test]
    async fn checkpoint_joiner_normal_rpc_open_accepted_tcp_full_fetch_and_completion() {
        let mut inviter = formed_via_rpc();
        let mut joined = JoinFixture::new(&inviter);
        let mut node = membership_sync_test_node(joined.identity.clone());
        let peer = inviter.local.peer_id.parse().unwrap();
        inviter
            .node
            .swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        timeout(Duration::from_secs(10), async {
            let address = loop {
                if let SwarmEvent::NewListenAddr { address, .. } = inviter.node.swarm.select_next_some().await { break address; }
            };
            node.swarm.dial(DialOpts::peer_id(peer).addresses(vec![address]).build()).unwrap();
            loop {
                tokio::select! {
                    event = node.swarm.select_next_some() => { if matches!(event, SwarmEvent::ConnectionEstablished { .. }) { break; } },
                    _ = inviter.node.swarm.select_next_some() => (),
                }
            }
        }).await.unwrap();
        let acceptance = inviter
            .sessions
            .open_completion(&inviter.approval.operation_id)
            .unwrap()
            .clone();
        let (request_id, response) = receive_pairing_acceptance_with_retry(
            &mut node,
            &mut inviter.node,
            &mut joined.sessions,
            &PairingCodeRequest::Poll {
                ticket: inviter.approval.ticket.clone(),
            },
            &acceptance,
        )
        .await;
        joined.accept_wire(&mut node, peer, request_id, response);
        joined.assert_gated();
        assert!(joined.owner.as_ref().unwrap().enrollment_pending());
        assert_eq!(
            joined.capabilities.membership_tag,
            inviter.capabilities.membership_tag
        );
        assert!(joined.sessions.join_completion(&joined.operation).is_none());
        let now = Instant::now();
        timeout(Duration::from_secs(10), async {
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            loop {
                tokio::select! {
                    _ = tick.tick() => { joined.owner.as_mut().unwrap().drive_sync(&mut node.swarm, &PeerCapabilities::default(), Instant::now()).unwrap(); },
                    event = node.swarm.select_next_some() => match event {
                        SwarmEvent::Behaviour(BehaviourEvent::Checkpoint(event)) => joined.owner.as_mut().unwrap().handle_wire_event(&mut node.swarm, event, &joined.identity, Instant::now(), current_unix_seconds_lossy()),
                        SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message { peer, message: Message::Response { response: ControlResponse::CapabilitiesAccepted(capabilities), .. }, .. })) => {
                            let tag = joined.capabilities.membership_tag.clone();
                            assert!(joined.owner.as_mut().unwrap().observe_capabilities(peer, &capabilities, tag.as_deref(), &[], Instant::now()));
                        }
                        _ => (),
                    },
                    event = inviter.node.swarm.select_next_some() => match event {
                        SwarmEvent::Behaviour(BehaviourEvent::Checkpoint(event)) => inviter.checkpoint.as_mut().unwrap().handle_wire_event(&mut inviter.node.swarm, event, &inviter.local, Instant::now(), current_unix_seconds_lossy()),
                        SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message { message: Message::Request { request: ControlRequest::Capabilities(capabilities), channel, .. }, .. })) => {
                            assert_eq!(capabilities.membership_tag, inviter.capabilities.membership_tag);
                            inviter.node.swarm.behaviour_mut().control.send_response(channel, ControlResponse::CapabilitiesAccepted(inviter.capabilities.clone())).unwrap();
                        }
                        _ => (),
                    },
                }
                let mut status = vec![];
                joined.owner.as_ref().unwrap().extend_status_lines(&mut status);
                if status.iter().any(|line| line == "checkpoint_offers_accepted 1") { break; }
            }
        }).await.expect("normal Accepted must enable authenticated paged catch-up over TCP/Noise");
        joined.assert_gated();
        joined
            .owner
            .as_mut()
            .unwrap()
            .finish_due(
                &joined.store,
                &mut joined.forwarder,
                now + super::super::super::checkpoint_runtime::RESYNC_WINDOW,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        joined.assert_gated();
        assert!(!joined.owner.as_ref().unwrap().enrollment_pending());
        assert!(joined.finalize(&mut PreconfiguredTunRoutes).unwrap());
        assert!(joined.sessions.join_completion(&joined.operation).is_some());
        assert!(joined.forwarder.is_configured_transport_peer(peer));
        assert!(joined.forwarder.member_records().is_empty());
        assert!(
            joined
                .sessions
                .enrollment(&joined.operation)
                .is_some_and(|entry| entry.state == PairingEnrollmentState::Applied)
        );
        let artifacts = pairing_rpc_completion_artifacts(
            &joined.sessions,
            &joined.operation,
            "lab",
            &joined.identity.peer_id,
            &[],
            Some(&joined.pairing_store),
        );
        assert!(
            artifacts.is_err(),
            "checkpoint credentials must not be exported as legacy configuration"
        );
    }

    #[tokio::test]
    async fn checkpoint_joiner_open_requires_durable_intent_and_preserves_legacy_networks() {
        let inviter = approved();
        for (failure, established) in [(true, false), (false, false), (false, true)] {
            let mut joined = JoinFixture::new(&inviter);
            joined.sessions = CodePairingSessions::new();
            if established {
                joined.config.peers.push(PeerConfig {
                    id: inviter.local.peer_id.clone(),
                    name: None,
                    ip: None,
                    vpn_ip: None,
                    addresses: vec![],
                    routes: vec![],
                });
                joined.forwarder = Forwarder::from_config(&joined.config).unwrap();
                joined.membership = OverlayMembership::from_config(&joined.config).unwrap();
                joined.tun = TunRuntimeConfig::from_config(&joined.config).unwrap();
            }
            let prior = fs::read(joined.directory.join("membership-state.json")).unwrap();
            let bad = PairingStateStore::new(joined.directory.join("missing/pairing.json"));
            let mut node = membership_sync_test_node(joined.identity.clone());
            let result = handle_pair_rpc_request(
                &mut node.swarm,
                PairRpcRequest::PairOpen {
                    operation_id: crate::runtime::pairing_sessions::fresh_pairing_operation_id(),
                    expires_in_seconds: 600,
                },
                &mut joined.sessions,
                Some(if failure { &bad } else { &joined.pairing_store }),
                &mut joined.forwarder,
                &mut joined.membership,
                &mut joined.tun,
                &mut PreconfiguredTunRoutes,
                &mut joined.capabilities,
                &joined.identity,
                &mut HashSet::new(),
                &mut None,
                &RuntimeMetrics::default(),
                Some(CheckpointPairingContext {
                    runtime: &mut joined.owner,
                    store: &joined.store,
                }),
            );
            if failure {
                assert!(matches!(
                    result.outcome,
                    crate::runtime::control_socket::PairRpcOutcome::Error { .. }
                ));
                assert!(joined.owner.is_none());
                assert_eq!(
                    fs::read(joined.directory.join("membership-state.json")).unwrap(),
                    prior
                );
            } else if established {
                assert!(matches!(
                    result.outcome,
                    crate::runtime::control_socket::PairRpcOutcome::Ok { .. }
                ));
                assert!(joined.owner.is_none());
                assert_eq!(
                    fs::read(joined.directory.join("membership-state.json")).unwrap(),
                    prior
                );
                assert!(
                    joined
                        .forwarder
                        .is_configured_transport_peer(inviter.local.peer_id.parse().unwrap())
                );
            } else {
                assert!(matches!(
                    result.outcome,
                    crate::runtime::control_socket::PairRpcOutcome::Ok { .. }
                ));
                assert_eq!(
                    joined
                        .owner
                        .as_ref()
                        .unwrap()
                        .state()
                        .snapshot()
                        .payload
                        .members
                        .len(),
                    1
                );
                assert_eq!(
                    joined.owner.as_ref().unwrap().state().sync_state(),
                    MembershipSyncState::ResyncRequired
                );
                assert!(joined.capabilities.membership_tag.is_some());
                assert_eq!(joined.forwarder.configured_transport_peers().count(), 0);
            }
        }
    }

    #[tokio::test]
    async fn checkpoint_joiner_floor_and_expired_protected_restart_remain_gated() {
        let inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        joined.prepare(&inviter);
        let now = Instant::now();
        joined.owner.as_mut().unwrap().begin_resync(now).unwrap();
        joined
            .owner
            .as_mut()
            .unwrap()
            .finish_due(
                &joined.store,
                &mut joined.forwarder,
                now + super::super::super::checkpoint_runtime::RESYNC_WINDOW,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        assert!(joined.owner.as_ref().unwrap().enrollment_pending());
        joined.assert_gated();
        let expired = inviter.offer.payload.expires_at_unix_seconds + 60;
        joined.restart(expired);
        assert!(joined.owner.as_ref().unwrap().enrollment_pending());
        joined.select(&inviter);
        assert!(!joined.owner.as_ref().unwrap().enrollment_pending());
        assert!(joined.finalize(&mut PreconfiguredTunRoutes).unwrap());
        assert!(!joined.owner.as_ref().unwrap().pairing_activation_blocked());
        assert!(joined.sessions.join_completion(&joined.operation).is_some());
        assert!(
            joined
                .forwarder
                .is_configured_transport_peer(inviter.local.peer_id.parse().unwrap())
        );
        assert_eq!(
            joined.forwarder.config().network.dns.hostname.as_deref(),
            Some("joining-device")
        );
        assert!(
            joined
                .tun
                .additional_addresses
                .iter()
                .any(|address| address.to_string() == "10.42.0.2/32")
        );
    }

    #[tokio::test]
    async fn checkpoint_joiner_fresh_cancel_removes_only_owned_incarnation() {
        let inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        joined.prepare(&inviter);
        joined.sessions.cancel(&joined.operation).unwrap();
        assert!(
            joined
                .finalize(&mut PreconfiguredTunRoutes)
                .is_ok_and(|done| !done)
        );
        joined.select(&inviter);
        joined.cancel_authority();
        assert_eq!(
            joined.owner.as_ref().unwrap().state().sync_state(),
            MembershipSyncState::Excluded
        );
        assert!(
            joined
                .owner
                .as_ref()
                .unwrap()
                .state()
                .snapshot()
                .payload
                .member(&joined.identity.peer_id)
                .is_none()
        );
        joined.assert_gated();
        assert!(joined.cleanup(true).is_err());
        joined.restart(current_unix_seconds_lossy());
        joined.assert_gated();
        joined.cleanup(false).unwrap();
        assert!(joined.sessions.enrollment(&joined.operation).is_none());
        assert_eq!(joined.forwarder.configured_transport_peers().count(), 0);
    }

    #[tokio::test]
    async fn checkpoint_joiner_pending_floor_is_fresh_and_duplicate_acceptance_preserves_ownership()
    {
        let mut inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        let initial = inviter
            .sessions
            .open_completion(&inviter.approval.operation_id)
            .unwrap()
            .clone();
        joined.owner = Some(
            CheckpointRuntime::stage_pairing_enrollment_from_solo(
                &joined.config,
                &joined.identity,
                &inviter.offer,
                &initial,
                &joined.store,
                current_unix_seconds_lossy(),
            )
            .unwrap(),
        );
        assert!(joined.owner.as_ref().unwrap().enrollment_pending());
        assert!(joined.sessions.enrollment(&joined.operation).is_none());
        inviter
            .checkpoint
            .as_mut()
            .unwrap()
            .apply_change(
                MembershipChange::SetPolicy(crate::membership::checkpoint::SnapshotPolicy {
                    max_active_members: 255,
                    route_grants_enabled: true,
                }),
                &inviter.local,
                &inviter.checkpoint_store,
                &mut inviter.forwarder,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        let publisher = inviter.checkpoint.as_ref().unwrap();
        let grant = publisher
            .pairing_grant_for(
                publisher.state(),
                &joined.identity.peer_id,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        let minimum = grant.minimum;
        assert!(minimum > initial.payload.checkpoint.as_ref().unwrap().minimum);
        let response = crate::pairing::build_checkpoint_pairing_response_at(
            inviter.forwarder.config(),
            &inviter.offer,
            crate::pairing::PairingResponseOptions {
                joiner_peer: joined.identity.peer_id.clone(),
                assigned_vpn_ip: initial.payload.assigned_vpn_ip.clone(),
                membership_key: None,
                member_records: vec![],
                expires_in_seconds: 600,
            },
            grant,
            current_unix_seconds_lossy(),
        )
        .unwrap();
        let mut node = membership_sync_test_node(joined.identity.clone());
        let peer = inviter.local.peer_id.parse().unwrap();
        for duplicate in [false, true] {
            let request_id = node.swarm.behaviour_mut().pairing_code.send_request(
                &peer,
                PairingCodeRequest::Poll {
                    ticket: inviter.approval.ticket.clone(),
                },
            );
            joined
                .sessions
                .insert_outbound_poll(
                    request_id,
                    OutboundPairing {
                        operation_id: joined.operation.clone(),
                        peer,
                        offer: inviter.offer.clone(),
                        transcript_sha256: inviter.approval.transcript_sha256.clone(),
                    },
                )
                .unwrap();
            joined.accept_wire(
                &mut node,
                peer,
                request_id,
                PairingCodeResponse::Accepted {
                    response: Box::new(response.clone()),
                },
            );
            assert_eq!(
                joined
                    .sessions
                    .enrollment(&joined.operation)
                    .unwrap()
                    .checkpoint_joiner_ownership,
                Some(CheckpointJoinerOwnership::Fresh),
                "a provisional seed is not established membership, duplicate={duplicate}",
            );
            joined.assert_gated();
            if !duplicate {
                let Some(PersistedAuthority::Checkpoint(loaded)) = joined
                    .store
                    .load_authority("lab", &joined.identity.peer_id, None, None)
                    .unwrap()
                else {
                    panic!("pending checkpoint")
                };
                assert_eq!(loaded.enrollment_floor, Some(minimum));
                joined.select(&inviter);
                assert!(!joined.owner.as_ref().unwrap().enrollment_pending());
            }
        }
        joined.cancel_authority();
        assert_eq!(
            joined.owner.as_ref().unwrap().state().sync_state(),
            MembershipSyncState::Excluded
        );
        assert!(
            joined
                .owner
                .as_ref()
                .unwrap()
                .state()
                .snapshot()
                .payload
                .member(&joined.identity.peer_id)
                .is_none()
        );
    }

    #[tokio::test]
    async fn checkpoint_joiner_repair_cancel_and_released_restart_preserve_authority() {
        let inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        joined.prepare(&inviter);
        joined.select(&inviter);
        // A ready protected authority exists before this repair transaction is created.
        let before = joined.owner.as_ref().unwrap().state().snapshot().clone();
        joined
            .sessions
            .record_checkpoint_joiner_ownership(&joined.operation, CheckpointJoinerOwnership::Fresh)
            .unwrap();
        // Reconstruct a separate protected repair instead of altering immutable ownership.
        joined.sessions = CodePairingSessions::new();
        let started = joined
            .sessions
            .join(
                "lab",
                crate::pairing_code::PairingCode::generate(),
                None,
                vec![],
                600,
                current_unix_seconds_lossy(),
                Instant::now(),
            )
            .unwrap();
        joined.operation = started.operation_id;
        joined
            .sessions
            .set_remote_pending(
                &joined.operation,
                inviter.local.peer_id.parse().unwrap(),
                inviter.offer.clone(),
                inviter.approval.transcript_sha256.clone(),
                inviter.approval.ticket.clone(),
                Instant::now(),
            )
            .unwrap();
        joined.prepare(&inviter);
        assert_eq!(
            joined
                .sessions
                .enrollment(&joined.operation)
                .unwrap()
                .checkpoint_joiner_ownership,
            Some(CheckpointJoinerOwnership::Repair)
        );
        joined.cancel_authority();
        assert_eq!(*joined.owner.as_ref().unwrap().state().snapshot(), before);
        assert!(joined.cleanup(true).is_err());
        joined.restart(current_unix_seconds_lossy());
        joined.assert_gated();
        let now = Instant::now();
        joined.owner.as_mut().unwrap().begin_resync(now).unwrap();
        joined
            .owner
            .as_mut()
            .unwrap()
            .finish_due(
                &joined.store,
                &mut joined.forwarder,
                now + super::super::super::checkpoint_runtime::RESYNC_WINDOW,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        joined.assert_gated();
        joined.cleanup(false).unwrap();
        assert_eq!(*joined.owner.as_ref().unwrap().state().snapshot(), before);
        assert!(
            joined
                .forwarder
                .is_configured_transport_peer(inviter.local.peer_id.parse().unwrap())
        );
    }

    #[tokio::test]
    async fn checkpoint_joiner_visible_failed_completion_blocks_cancel_until_reconfirmed() {
        let inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        joined.prepare(&inviter);
        joined.select(&inviter);
        let result = joined.sessions.finish_checkpoint_join_with::<RunnerError>(
            &joined.operation,
            "lab",
            current_unix_seconds_lossy(),
            |bytes| {
                joined.pairing_store.save(bytes)?;
                Err(io::Error::other("injected failure after visible replacement").into())
            },
            || joined.pairing_store.load().map_err(Into::into),
        );
        assert!(result.is_err());
        assert!(joined.sessions.checkpoint_completion_uncertain());
        assert!(joined.sessions.cancel(&joined.operation).is_err());
        assert!(
            persist_code_pairing_sessions(Some(&joined.pairing_store), &joined.sessions, "lab")
                .is_err()
        );
        joined.assert_gated();
        assert!(joined.finalize(&mut PreconfiguredTunRoutes).unwrap());
        assert!(joined.sessions.join_completion(&joined.operation).is_some());
        assert!(!joined.sessions.checkpoint_completion_uncertain());
        joined.sessions.cancel(&joined.operation).unwrap();
        assert!(
            joined
                .sessions
                .enrollment(&joined.operation)
                .is_some_and(|entry| entry.state == PairingEnrollmentState::Applied)
        );
        assert!(
            joined
                .owner
                .as_ref()
                .unwrap()
                .state()
                .snapshot()
                .payload
                .member(&joined.identity.peer_id)
                .is_some()
        );
    }

    #[tokio::test]
    async fn checkpoint_joiner_prewrite_failure_and_unknown_visibility_preserve_gate() {
        let inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        joined.prepare(&inviter);
        joined.select(&inviter);
        let result = joined.sessions.finish_checkpoint_join_with::<RunnerError>(
            &joined.operation,
            "lab",
            current_unix_seconds_lossy(),
            |_| Err(io::Error::other("before rename").into()),
            || joined.pairing_store.load().map_err(Into::into),
        );
        assert!(result.is_err());
        assert!(!joined.sessions.checkpoint_completion_uncertain());
        assert_eq!(
            joined.sessions.enrollment(&joined.operation).unwrap().state,
            PairingEnrollmentState::Prepared
        );
        let result = joined.sessions.finish_checkpoint_join_with::<RunnerError>(
            &joined.operation,
            "lab",
            current_unix_seconds_lossy(),
            |_| Err(io::Error::other("ambiguous write").into()),
            || Err(io::Error::other("read unavailable").into()),
        );
        assert!(result.is_err());
        assert!(joined.sessions.cancel(&joined.operation).is_err());
        assert!(joined.finalize(&mut PreconfiguredTunRoutes).unwrap());
        assert!(joined.sessions.join_completion(&joined.operation).is_some());
    }

    #[tokio::test]
    async fn checkpoint_joiner_removed_admission_is_rejected_without_activation() {
        let mut inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        joined.prepare(&inviter);
        inviter
            .checkpoint
            .as_mut()
            .unwrap()
            .apply_change(
                MembershipChange::RemoveMember(joined.identity.peer_id.clone()),
                &inviter.local,
                &inviter.checkpoint_store,
                &mut inviter.forwarder,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        joined.select(&inviter);
        assert_eq!(
            joined.owner.as_ref().unwrap().state().sync_state(),
            MembershipSyncState::Excluded
        );
        assert!(joined.finalize(&mut PreconfiguredTunRoutes).is_err());
        joined.assert_gated();
        assert_eq!(
            joined.sessions.enrollment(&joined.operation).unwrap().state,
            PairingEnrollmentState::Aborting
        );
        assert_eq!(
            pairing_rpc_status(
                &joined.sessions,
                &joined.operation,
                "lab",
                &joined.identity.peer_id
            )
            .unwrap()
            .phase,
            PairRpcPhase::Failed
        );
        assert!(joined.sessions.join_completion(&joined.operation).is_none());
        joined.restart(current_unix_seconds_lossy());
        joined.assert_gated();
    }

    #[tokio::test]
    async fn checkpoint_joiner_kernel_failure_retains_prepared_across_restart() {
        let inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        joined.prepare(&inviter);
        joined.select(&inviter);
        struct FailingRoutes;
        impl TunRouteController for FailingRoutes {
            fn reconcile(
                &mut self,
                _: &TunRuntimeConfig,
                _: &TunRuntimeConfig,
                _: &TunRouteUpdate,
            ) -> Result<(), RunnerError> {
                Err(io::Error::other("injected kernel finalization failure").into())
            }
        }
        assert!(joined.finalize(&mut FailingRoutes).is_err());
        assert_eq!(
            joined.sessions.enrollment(&joined.operation).unwrap().state,
            PairingEnrollmentState::Prepared
        );
        assert!(
            joined
                .sessions
                .enrollment(&joined.operation)
                .unwrap()
                .tun_cleanup
                .is_some()
        );
        joined.assert_gated();
        joined.restart(current_unix_seconds_lossy());
        assert!(!joined.finalize(&mut PreconfiguredTunRoutes).unwrap());
        joined.select(&inviter);
        assert!(joined.finalize(&mut PreconfiguredTunRoutes).unwrap());
    }

    #[tokio::test]
    async fn checkpoint_joiner_daemon_released_cleanup_stays_gated_after_resync() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        joined.prepare(&inviter);
        joined.select(&inviter);
        joined.sessions = CodePairingSessions::new();
        joined.operation = joined
            .sessions
            .join(
                "lab",
                crate::pairing_code::PairingCode::generate(),
                None,
                vec![],
                600,
                current_unix_seconds_lossy(),
                Instant::now(),
            )
            .unwrap()
            .operation_id;
        joined
            .sessions
            .set_remote_pending(
                &joined.operation,
                inviter.local.peer_id.parse().unwrap(),
                inviter.offer.clone(),
                inviter.approval.transcript_sha256.clone(),
                inviter.approval.ticket.clone(),
                Instant::now(),
            )
            .unwrap();
        joined.prepare(&inviter);
        joined.cancel_authority();
        assert!(joined.cleanup(true).is_err());
        let failures = Arc::new(AtomicUsize::new(0));
        struct CleanupFailure(Arc<AtomicUsize>);
        impl TunRouteController for CleanupFailure {
            fn reconcile(
                &mut self,
                _: &TunRuntimeConfig,
                _: &TunRuntimeConfig,
                update: &TunRouteUpdate,
            ) -> Result<(), RunnerError> {
                if update.is_abort_cleanup() {
                    self.0.fetch_add(1, Ordering::SeqCst);
                    return Err(io::Error::other("held cleanup failure").into());
                }
                Ok(())
            }
        }
        struct EmptyPackets;
        impl super::super::super::tun::PacketRead for EmptyPackets {
            fn read_packet(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
        }
        impl super::super::super::tun::PacketWrite for EmptyPackets {
            fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
                Ok(packet.len())
            }
        }
        let mut config = joined.config.clone();
        config.network.discovery = DiscoveryConfig {
            mdns: false,
            kademlia: false,
            autonat: false,
            dcutr: false,
            kademlia_provider_advertisement: false,
            ..DiscoveryConfig::default()
        };
        config.network.relay.auto.max_reservations = 0;
        config.network.packet_plane.listen.clear();
        config.network.packet_plane.quic_listen.clear();
        let (control, receiver) = super::super::super::control_socket::runtime_control_channel();
        let platform = RuntimePlatform::new(
            PacketIo::new(EmptyPackets, EmptyPackets),
            CleanupFailure(failures.clone()),
        )
        .with_control(receiver);
        let daemon = tokio::spawn(run_config_until_with_runtime_platform(
            config,
            platform,
            None,
            None,
            Some(joined.pairing_store.path().to_path_buf()),
            Some(joined.directory.join("membership-state.json")),
            std::future::pending(),
        ));
        let result = timeout(Duration::from_secs(25), async {
            loop {
                assert!(
                    control.network_peers().await.unwrap().peers.is_empty(),
                    "Released repair must not restore packet grants before cleanup"
                );
                let state = control.state().await.unwrap();
                if state
                    .iter()
                    .any(|line| line == "checkpoint_sync_state participating")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert!(failures.load(Ordering::SeqCst) > 0);
            assert!(control.network_peers().await.unwrap().peers.is_empty());
            control.shutdown().await.unwrap();
        })
        .await;
        if result.is_err() {
            daemon.abort();
        }
        let finished = daemon.await;
        result
            .expect("actual daemon startup must keep the transaction barrier across local resync");
        finished.unwrap().unwrap();
    }

    #[tokio::test]
    async fn checkpoint_joiner_tun_signed_alias_restore_remove_and_retry() {
        let mut inviter = approved();
        let mut joined = JoinFixture::new(&inviter);
        joined.prepare(&inviter);
        joined.select(&inviter);
        joined.finalize(&mut PreconfiguredTunRoutes).unwrap();
        let installed = joined.tun.clone();
        joined
            .owner
            .as_mut()
            .unwrap()
            .set_pairing_activation_blocked(
                true,
                &mut joined.forwarder,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        let mut commands = Vec::new();
        sync_live_tun_routes_with(&joined.forwarder, &mut joined.tun, |command| {
            commands.push(command.clone());
            Ok(())
        })
        .unwrap();
        assert!(joined.tun.additional_addresses.is_empty());
        assert!(!commands.is_empty());
        joined
            .owner
            .as_mut()
            .unwrap()
            .set_pairing_activation_blocked(
                false,
                &mut joined.forwarder,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        assert!(
            sync_live_tun_routes_with(&joined.forwarder, &mut joined.tun, |_| Err(
                io::Error::other("injected alias restore failure").into()
            ))
            .is_err()
        );
        assert!(joined.tun.additional_addresses.is_empty());
        sync_live_tun_routes_with(&joined.forwarder, &mut joined.tun, |_| Ok(())).unwrap();
        assert_eq!(joined.tun, installed);
        inviter
            .checkpoint
            .as_mut()
            .unwrap()
            .apply_change(
                MembershipChange::RemoveMember(joined.identity.peer_id.clone()),
                &inviter.local,
                &inviter.checkpoint_store,
                &mut inviter.forwarder,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        // Removal from a winning branch also erases local TUN aliases after restart/catch-up.
        joined
            .owner
            .as_mut()
            .unwrap()
            .set_pairing_activation_blocked(
                true,
                &mut joined.forwarder,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        joined.select(&inviter);
        joined
            .owner
            .as_mut()
            .unwrap()
            .set_pairing_activation_blocked(
                false,
                &mut joined.forwarder,
                current_unix_seconds_lossy(),
            )
            .unwrap();
        sync_live_tun_routes_with(&joined.forwarder, &mut joined.tun, |_| Ok(())).unwrap();
        assert!(joined.tun.additional_addresses.is_empty());
    }
}
