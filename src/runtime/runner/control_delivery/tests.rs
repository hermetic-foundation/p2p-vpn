use super::*;
use crate::runtime::runner::tests::{
    config_with_peer, dispatch_membership_sync_test_event, membership_sync_test_node,
};

fn fixture() -> (P2pNode, Forwarder, Libp2pPeerId) {
    let identity = NodeIdentity::generate_ed25519().unwrap();
    let peer = Libp2pPeerId::random();
    let forwarder = Forwarder::from_config(&config_with_peer(&identity, peer)).unwrap();
    (membership_sync_test_node(identity), forwarder, peer)
}

fn capabilities() -> ControlCapabilities {
    ControlCapabilities::local("lab", None, 1280)
}

#[tokio::test]
async fn changes_coalesce_without_dialing_or_renewing_payload_demand() {
    let (mut node, forwarder, peer) = fixture();
    let overlay = PeerId::from_libp2p(peer);
    let before = forwarder.application_demand(overlay).unwrap();
    assert!(forwarder.grant_data_path_quiet(overlay, before.version.unwrap()));
    let mut syncs = MembershipRecordSyncs::default();
    let metrics = RuntimeMetrics::default();
    let now = Instant::now();
    for mtu in 1_000..2_000 {
        let mut latest = capabilities();
        latest.effective_mtu = mtu;
        syncs.queue_capabilities(latest, &forwarder);
        syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
        assert!(syncs.pending.is_empty());
        assert_eq!(syncs.capabilities.peers.len(), 1);
        assert!(syncs.occupied_slots() <= MAX_CONCURRENT_MEMBERSHIP_SYNCS);
    }
    assert_eq!(
        node.swarm
            .network_info()
            .connection_counters()
            .num_pending(),
        0
    );
    assert_eq!(
        syncs.capabilities.current.as_ref().unwrap().effective_mtu,
        1999
    );
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        now + PEER_RECOVERY_LAN_FIRST_GRACE,
    );
    assert_eq!(syncs.pending.len(), 1);
    assert!(node.swarm.behaviour().control.has_pending_requests(&peer));
    assert_eq!(forwarder.application_demand(overlay), Some(before));
    assert!(forwarder.data_path_is_dormant(overlay));
}

#[tokio::test]
async fn expired_round_keeps_native_slot_and_ticks_never_reopen_it() {
    let (mut node, forwarder, peer) = fixture();
    let mut syncs = MembershipRecordSyncs::default();
    let metrics = RuntimeMetrics::default();
    let now = Instant::now();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        now + PEER_RECOVERY_LAN_FIRST_GRACE,
    );
    let id = *syncs.pending.keys().next().unwrap();
    for tick in 0..1000 {
        syncs.drive_capabilities(
            &mut node.swarm,
            &forwarder,
            &metrics,
            now + DELIVERY_WINDOW + Duration::from_secs(tick * 5),
        );
        assert_eq!(syncs.pending.len(), 1);
        assert!(matches!(
            syncs.pending[&id],
            PendingControlSync::Retired { .. }
        ));
    }
    assert!(syncs.capabilities.peers[&peer].exhausted);
    assert_eq!(
        metrics
            .snapshot(crate::queue::QueueStats::default())
            .control_requests_sent,
        1
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::Behaviour(BehaviourEvent::Control(
                event @ request_response::Event::OutboundFailure { .. },
            )) = node.swarm.select_next_some().await
            {
                assert!(matches!(
                    syncs.finish_capability_event(
                        &event,
                        &forwarder,
                        &capabilities(),
                        &[],
                        true,
                        Instant::now()
                    ),
                    Completion::Discard
                ));
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(syncs.pending.is_empty());
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        now + DELIVERY_WINDOW + Duration::from_secs(5001),
    );
    assert!(syncs.pending.is_empty());
}

#[tokio::test]
async fn revocation_keeps_inert_native_ownership_until_real_failure() {
    let (mut node, mut forwarder, peer) = fixture();
    let now = Instant::now();
    let metrics = RuntimeMetrics::default();
    let mut syncs = MembershipRecordSyncs::default();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        now + PEER_RECOVERY_LAN_FIRST_GRACE,
    );
    let mut config = forwarder.config().clone();
    config.peers.clear();
    forwarder
        .try_commit_reconfigure(
            forwarder
                .prepare_reconfigure(config, current_unix_seconds_lossy())
                .unwrap(),
        )
        .unwrap();
    assert_eq!(syncs.reconcile_authorization(&forwarder, &metrics), 1);
    assert!(syncs.capabilities.peers.is_empty());
    assert_eq!(syncs.pending.len(), 1);
    assert!(syncs.active_by_peer.is_empty());
    assert!(node.swarm.behaviour().control.has_pending_requests(&peer));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::Behaviour(BehaviourEvent::Control(
                event @ request_response::Event::OutboundFailure { .. },
            )) = node.swarm.select_next_some().await
            {
                syncs.finish_capability_event(
                    &event,
                    &forwarder,
                    &capabilities(),
                    &[],
                    true,
                    Instant::now(),
                );
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(syncs.pending.is_empty());
}

#[tokio::test]
async fn native_failure_backoff_survives_a_new_generation() {
    let (mut node, forwarder, peer) = fixture();
    let now = Instant::now();
    let metrics = RuntimeMetrics::default();
    let mut syncs = MembershipRecordSyncs::default();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        now + PEER_RECOVERY_LAN_FIRST_GRACE,
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::Behaviour(BehaviourEvent::Control(
                event @ request_response::Event::OutboundFailure { .. },
            )) = node.swarm.select_next_some().await
            {
                syncs.finish_capability_event(
                    &event,
                    &forwarder,
                    &capabilities(),
                    &[],
                    true,
                    now + PEER_RECOVERY_LAN_FIRST_GRACE,
                );
                break;
            }
        }
    })
    .await
    .unwrap();
    let retry = syncs.capabilities.peers[&peer].retry_at.unwrap();
    let mut latest = capabilities();
    latest.effective_mtu = 1200;
    syncs.queue_capabilities(latest, &forwarder);
    assert_eq!(syncs.capabilities.peers[&peer].retry_at, Some(retry));
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        retry - Duration::from_nanos(1),
    );
    assert!(syncs.pending.is_empty());
    assert!(!syncs.capabilities.rounds.contains_key(&peer));
}

#[tokio::test]
async fn generation_exhaustion_does_not_alias_acknowledged_work() {
    let (mut node, forwarder, peer) = fixture();
    let mut syncs = MembershipRecordSyncs::default();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.capabilities.generation = u64::MAX;
    syncs
        .capabilities
        .peers
        .get_mut(&peer)
        .unwrap()
        .acknowledged = u64::MAX;
    let mut changed = capabilities();
    changed.effective_mtu = 1200;
    syncs.queue_capabilities(changed, &forwarder);
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &RuntimeMetrics::default(),
        Instant::now(),
    );
    assert!(syncs.capabilities.exhausted);
    assert!(syncs.pending.is_empty());
}

#[tokio::test]
async fn capability_rounds_and_record_pages_share_four_native_slots() {
    let local = NodeIdentity::generate_ed25519().unwrap();
    let mut addresses = HashSet::new();
    let peers = (0..8)
        .map(|_| {
            loop {
                let peer = Libp2pPeerId::random();
                if addresses.insert(crate::route::builtin_ipv4(PeerId::from_libp2p(peer))) {
                    break peer;
                }
            }
        })
        .collect::<Vec<_>>();
    let mut config = config_with_peer(&local, peers[0]);
    for peer in &peers[1..] {
        config.peers.extend(config_with_peer(&local, *peer).peers);
    }
    let forwarder = Forwarder::from_config(&config).unwrap();
    let mut node = membership_sync_test_node(local);
    let metrics = RuntimeMetrics::default();
    let now = Instant::now();
    let mut syncs = MembershipRecordSyncs::default();
    send_membership_record_sync_request(
        &mut node.swarm,
        &mut syncs,
        &metrics,
        PendingMembershipRecordSync::first(peers[0], &capabilities(), &capabilities()),
    );
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
    assert_eq!(syncs.pending.len(), 1);
    assert_eq!(syncs.capabilities.rounds.len(), 3);
    assert_eq!(syncs.occupied_slots(), MAX_CONCURRENT_MEMBERSHIP_SYNCS);
    assert!(!syncs.can_start(peers[7], "another-snapshot", now));
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        now + PEER_RECOVERY_LAN_FIRST_GRACE,
    );
    assert_eq!(syncs.pending.len(), MAX_CONCURRENT_MEMBERSHIP_SYNCS);
    assert_eq!(
        syncs
            .pending
            .values()
            .filter(|work| work.records().is_some())
            .count(),
        1
    );
    for mtu in 1200..1300 {
        let mut changed = capabilities();
        changed.effective_mtu = mtu;
        syncs.queue_capabilities(changed, &forwarder);
        syncs.drive_capabilities(
            &mut node.swarm,
            &forwarder,
            &metrics,
            now + PEER_RECOVERY_LAN_FIRST_GRACE,
        );
        assert_eq!(syncs.pending.len(), MAX_CONCURRENT_MEMBERSHIP_SYNCS);
        assert_eq!(syncs.occupied_slots(), MAX_CONCURRENT_MEMBERSHIP_SYNCS);
    }
}

#[tokio::test]
async fn old_acknowledgement_cannot_ack_new_generation_or_lost_authority() {
    let (_, mut forwarder, peer) = fixture();
    let mut syncs = MembershipRecordSyncs::default();
    let now = Instant::now();
    syncs.queue_capabilities(capabilities(), &forwarder);
    let old = CapabilityTicket {
        peer,
        generation: syncs.capabilities.generation,
        deadline: now + DELIVERY_WINDOW,
    };
    let mut changed = capabilities();
    changed.effective_mtu = 1200;
    syncs.queue_capabilities(changed, &forwarder);
    syncs.acknowledge_capabilities(old, &forwarder, true, now);
    assert_eq!(syncs.capabilities.peers[&peer].acknowledged, 0);
    let current = CapabilityTicket {
        generation: syncs.capabilities.generation,
        ..old
    };
    syncs.acknowledge_capabilities(current, &forwarder, false, now);
    assert_eq!(syncs.capabilities.peers[&peer].acknowledged, 0);
    syncs.acknowledge_capabilities(current, &forwarder, true, now + DELIVERY_WINDOW);
    assert_eq!(syncs.capabilities.peers[&peer].acknowledged, 0);
    let mut config = forwarder.config().clone();
    config.peers.clear();
    forwarder
        .try_commit_reconfigure(
            forwarder
                .prepare_reconfigure(config, current_unix_seconds_lossy())
                .unwrap(),
        )
        .unwrap();
    syncs.acknowledge_capabilities(current, &forwarder, true, now);
    assert_eq!(syncs.capabilities.peers[&peer].acknowledged, 0);
}

#[tokio::test]
async fn wrong_peer_cannot_release_a_native_slot_or_acknowledge_work() {
    let (mut node, forwarder, peer) = fixture();
    let mut syncs = MembershipRecordSyncs::default();
    let now = Instant::now();
    let metrics = RuntimeMetrics::default();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        now + PEER_RECOVERY_LAN_FIRST_GRACE,
    );
    let id = *syncs.pending.keys().next().unwrap();
    let event = request_response::Event::Message {
        peer: Libp2pPeerId::random(),
        connection_id: ConnectionId::new_unchecked(1),
        message: Message::Response {
            request_id: id,
            response: ControlResponse::CapabilitiesAccepted(capabilities()),
        },
    };
    assert!(matches!(
        syncs.finish_capability_event(
            &event,
            &forwarder,
            &capabilities(),
            &[],
            true,
            now + PEER_RECOVERY_LAN_FIRST_GRACE
        ),
        Completion::Discard
    ));
    assert_eq!(syncs.pending.len(), 1);
    assert_eq!(syncs.active_by_peer.get(&peer), Some(&id));
    assert_eq!(syncs.capabilities.peers[&peer].acknowledged, 0);
}

#[tokio::test]
async fn root_route_rejection_cannot_acknowledge_the_publication() {
    let (mut node, mut forwarder, peer) = fixture();
    let now = Instant::now();
    let metrics = RuntimeMetrics::default();
    let mut syncs = MembershipRecordSyncs::default();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        now + PEER_RECOVERY_LAN_FIRST_GRACE,
    );
    let id = *syncs.pending.keys().next().unwrap();
    let mut invalid = capabilities();
    invalid
        .advertised_routes
        .push(crate::runtime::control::ControlRoute::new(
            "10.123.0.0/16",
            1,
        ));
    assert!(crate::runtime::control::validate_capabilities(&invalid, "lab", None, &[]).is_none());
    let connection_id = ConnectionId::new_unchecked(1);
    let mut epochs = ConnectionEpochs::default();
    epochs.record_established(connection_id);
    dispatch_membership_sync_test_event(
        &mut node,
        &mut forwarder,
        &mut syncs,
        &metrics,
        &mut epochs,
        request_response::Event::Message {
            peer,
            connection_id,
            message: Message::Response {
                request_id: id,
                response: ControlResponse::CapabilitiesAccepted(invalid),
            },
        },
    )
    .await;
    assert_eq!(syncs.capabilities.peers[&peer].acknowledged, 0);
    assert!(syncs.pending.is_empty());
    assert!(syncs.capabilities.rounds.contains_key(&peer));
    assert_eq!(
        metrics
            .snapshot(crate::queue::QueueStats::default())
            .control_failures,
        1
    );
}

#[tokio::test]
async fn disconnect_tombstone_cannot_bypass_four_slot_limit() {
    let local = NodeIdentity::generate_ed25519().unwrap();
    let mut config = config_with_peer(&local, Libp2pPeerId::random());
    for _ in 1..4 {
        config
            .peers
            .extend(config_with_peer(&local, Libp2pPeerId::random()).peers);
    }
    let forwarder = Forwarder::from_config(&config).unwrap();
    let peers = forwarder.configured_transport_peers().collect::<Vec<_>>();
    let mut node = membership_sync_test_node(local);
    let mut syncs = MembershipRecordSyncs::default();
    let now = Instant::now();
    let metrics = RuntimeMetrics::default();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &metrics,
        now + PEER_RECOVERY_LAN_FIRST_GRACE,
    );
    assert_eq!(syncs.pending.len(), MAX_CONCURRENT_MEMBERSHIP_SYNCS);
    syncs.remove_peer(peers[0]);
    for offset in 1..5 {
        syncs.drive_capabilities(
            &mut node.swarm,
            &forwarder,
            &metrics,
            now + PEER_RECOVERY_LAN_FIRST_GRACE + RETRY_DELAY * offset,
        );
        assert_eq!(syncs.pending.len(), MAX_CONCURRENT_MEMBERSHIP_SYNCS);
        assert_eq!(syncs.occupied_slots(), MAX_CONCURRENT_MEMBERSHIP_SYNCS);
    }
}

#[tokio::test]
async fn stale_owner_is_not_checkpoint_advertisement_authority() {
    let (mut node, forwarder, peer) = fixture();
    let now = Instant::now();
    let metrics = RuntimeMetrics::default();
    let mut syncs = MembershipRecordSyncs::default();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
    let send = now + PEER_RECOVERY_LAN_FIRST_GRACE;
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, send);
    let id = *syncs.pending.keys().next().unwrap();
    assert!(syncs.capability_response_may_advertise(peer, id, &forwarder, send));
    assert!(!syncs.capability_response_may_advertise(Libp2pPeerId::random(), id, &forwarder, send));
    let mut updated = capabilities();
    updated.effective_mtu = 1200;
    syncs.queue_capabilities(updated, &forwarder);
    assert!(!syncs.capability_response_may_advertise(peer, id, &forwarder, send));
    assert!(matches!(
        syncs.pending[&id],
        PendingControlSync::Retired { .. }
    ));
    assert!(
        syncs
            .control_delivery_deadline(peer, &forwarder, send)
            .is_none()
    );
}

#[tokio::test]
async fn control_probe_lease_preserves_quarantine_history_and_original_deadlines() {
    let (mut node, _, peer) = fixture();
    let mut probes = MembershipProbeConnections::default();
    let now = Instant::now();
    quarantine_membership_probe_peer(&mut node.swarm, &mut probes, peer, now, "test");
    let original = probes.quarantines[&peer];
    let deadline = now + MEMBERSHIP_PROBE_QUARANTINE_BASE / 2;
    reconcile_control_probe_leases(
        &mut node.swarm,
        &mut probes,
        |_| false,
        |_, _| Some(deadline),
        now,
    );
    assert!(!node.swarm.behaviour_mut().blocked_peers.unblock_peer(peer));
    assert!(!probes.is_quarantined(peer, now));
    let connection = ConnectionId::new_unchecked(7);
    assert_eq!(
        probes.admit(peer, connection, false, now),
        MembershipProbeAdmission::Admitted
    );
    assert_eq!(probes.by_peer[&peer].expires_at, deadline);
    for seconds in 1..10 {
        reconcile_control_probe_leases(
            &mut node.swarm,
            &mut probes,
            |_| false,
            |_, _| Some(deadline),
            now + Duration::from_secs(seconds),
        );
        let current = probes.quarantines[&peer];
        assert_eq!(current.retry_at, original.retry_at);
        assert_eq!(current.last_failure, original.last_failure);
        assert_eq!(current.consecutive_failures, original.consecutive_failures);
        assert_eq!(current.control_until, Some(deadline));
    }
    reconcile_control_probe_leases(
        &mut node.swarm,
        &mut probes,
        |_| false,
        |_, _| Some(deadline),
        deadline,
    );
    assert!(probes.is_quarantined(peer, deadline));
    assert!(!node.swarm.behaviour_mut().blocked_peers.block_peer(peer));
    assert_eq!(probes.quarantines[&peer].retry_at, original.retry_at);
    probes.release_ready_quarantines(original.retry_at);
    let later = probes.quarantine(peer, original.retry_at);
    assert_eq!(later.consecutive_failures, 2);
}

#[tokio::test]
async fn absence_of_a_native_owner_never_suspends_probe_quarantine() {
    let (mut node, _, peer) = fixture();
    let now = Instant::now();
    let mut probes = MembershipProbeConnections::default();
    quarantine_membership_probe_peer(&mut node.swarm, &mut probes, peer, now, "test");
    let original = probes.quarantines[&peer];
    for tick in 0..10 {
        let at = now + Duration::from_secs(tick);
        reconcile_control_probe_leases(&mut node.swarm, &mut probes, |_| false, |_, _| None, at);
        assert!(probes.is_quarantined(peer, at));
        assert_eq!(probes.quarantines[&peer], original);
        assert!(!node.swarm.behaviour_mut().blocked_peers.block_peer(peer));
    }
}

#[tokio::test]
async fn real_application_demand_can_retry_but_maintenance_cannot_renew_the_round() {
    let (mut node, forwarder, peer) = fixture();
    let now = Instant::now();
    let mut syncs = MembershipRecordSyncs::default();
    let metrics = RuntimeMetrics::default();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, now + DELIVERY_WINDOW);
    assert!(syncs.capabilities.peers[&peer].exhausted);
    for _ in 0..1000 {
        syncs.request_capabilities(peer, &forwarder);
        assert!(syncs.capabilities.peers[&peer].exhausted);
    }
    let demanded = now + DELIVERY_WINDOW;
    forwarder.record_application_demand(PeerId::from_libp2p(peer), demanded);
    let snapshot = forwarder.application_demand(PeerId::from_libp2p(peer));
    syncs.request_capabilities(peer, &forwarder);
    assert!(!syncs.capabilities.peers[&peer].exhausted);
    syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, demanded);
    let deadline = syncs.capabilities.rounds[&peer].deadline;
    for _ in 0..1000 {
        syncs.request_capabilities(peer, &forwarder);
        assert_eq!(syncs.capabilities.rounds[&peer].deadline, deadline);
        assert_eq!(
            forwarder.application_demand(PeerId::from_libp2p(peer)),
            snapshot
        );
    }
}

#[tokio::test]
async fn page_deadline_cancels_buffers_but_keeps_native_ownership_until_completion() {
    let (mut node, forwarder, peer) = fixture();
    let mut syncs = MembershipRecordSyncs::default();
    let metrics = RuntimeMetrics::default();
    let request = PendingMembershipRecordSync::first(peer, &capabilities(), &capabilities());
    let deadline = request.deadline;
    send_membership_record_sync_request(&mut node.swarm, &mut syncs, &metrics, request);
    let id = *syncs.pending.keys().next().unwrap();
    syncs.expire_control_work(deadline, &metrics);
    assert!(matches!(
        syncs.pending[&id],
        PendingControlSync::Retired { .. }
    ));
    assert_eq!(
        syncs.history.retry_deadline(peer),
        Some(deadline + MEMBERSHIP_SYNC_RETRY_DELAY)
    );
    assert!(syncs.active_by_peer.is_empty());
    assert!(
        syncs
            .control_delivery_deadline(peer, &forwarder, deadline)
            .is_none()
    );
    for _ in 0..1000 {
        syncs.expire_control_work(deadline, &metrics);
    }
    assert_eq!(
        metrics
            .snapshot(crate::queue::QueueStats::default())
            .membership_record_sync_failures,
        1
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::Behaviour(BehaviourEvent::Control(
                event @ request_response::Event::OutboundFailure { .. },
            )) = node.swarm.select_next_some().await
            {
                assert!(matches!(
                    syncs.finish_capability_event(
                        &event,
                        &forwarder,
                        &capabilities(),
                        &[],
                        true,
                        deadline
                    ),
                    Completion::Discard
                ));
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(syncs.pending.is_empty());
}

#[tokio::test]
async fn established_control_peer_precedes_cold_lan_reservations() {
    use libp2p::swarm::NetworkBehaviour;
    let local = NodeIdentity::generate_ed25519().unwrap();
    let peer = Libp2pPeerId::random();
    let mut config = config_with_peer(&local, peer);
    for _ in 0..8 {
        config
            .peers
            .extend(config_with_peer(&local, Libp2pPeerId::random()).peers);
    }
    let forwarder = Forwarder::from_config(&config).unwrap();
    let mut node = membership_sync_test_node(local);
    let _handler = node
        .swarm
        .behaviour_mut()
        .control
        .handle_established_outbound_connection(
            ConnectionId::new_unchecked(1),
            peer,
            &"/ip4/127.0.0.1/tcp/4001".parse().unwrap(),
            libp2p::core::Endpoint::Dialer,
            libp2p::core::transport::PortUse::New,
        )
        .unwrap();
    let mut syncs = MembershipRecordSyncs::default();
    syncs.queue_capabilities(capabilities(), &forwarder);
    syncs.drive_capabilities(
        &mut node.swarm,
        &forwarder,
        &RuntimeMetrics::default(),
        Instant::now(),
    );
    assert_eq!(syncs.pending.len(), 1);
    assert!(syncs.active_by_peer.contains_key(&peer));
    assert_eq!(
        syncs.capabilities.rounds.len(),
        MAX_CONCURRENT_MEMBERSHIP_SYNCS
    );
    assert_eq!(syncs.occupied_slots(), MAX_CONCURRENT_MEMBERSHIP_SYNCS);
}

#[tokio::test]
async fn retained_lan_edge_delivers_latest_capabilities_through_root_control_handler() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let local = NodeIdentity::generate_ed25519().unwrap();
        let remote = NodeIdentity::generate_ed25519().unwrap();
        let peer = remote.peer_id.parse().unwrap();
        let mut node = membership_sync_test_node(local.clone());
        let mut receiver = membership_sync_test_node(remote);
        let mut forwarder = Forwarder::from_config(&config_with_peer(&local, peer)).unwrap();
        receiver.swarm.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
        let address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = receiver.swarm.select_next_some().await { break address; }
        };
        node.configured_peer_addresses.push((peer, address.clone()));
        node.swarm.add_peer_address(peer, address);
        let overlay = PeerId::from_libp2p(peer);
        let before = forwarder.application_demand(overlay).unwrap();
        forwarder.grant_data_path_quiet(overlay, before.version.unwrap());
        let mut syncs = MembershipRecordSyncs::default();
        let metrics = RuntimeMetrics::default();
        let mut epochs = ConnectionEpochs::default();
        let mut discovered = DiscoveredPeerAddresses::default();
        let mut latest = capabilities();
        latest.effective_mtu = 1234;
        syncs.queue_capabilities(latest, &forwarder);
        drive(&mut syncs, &mut node, &forwarder, &mut epochs, &mut discovered, &metrics, Instant::now());
        assert!(syncs.pending.is_empty(), "LAN discovery precedes a disconnected RPC");
        let mut received = false;
        while syncs.capabilities.peers[&peer].acknowledged != syncs.capabilities.generation {
            tokio::select! {
                event = node.swarm.select_next_some() => match event {
                    SwarmEvent::ConnectionEstablished { connection_id, .. } => {
                        epochs.record_established(connection_id);
                        syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, Instant::now());
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Control(event)) => {
                        dispatch_membership_sync_test_event(&mut node, &mut forwarder,
                            &mut syncs, &metrics, &mut epochs, event).await;
                    }
                    _ => (),
                },
                event = receiver.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                        message: Message::Request { request: ControlRequest::Capabilities(capabilities), channel, .. }, ..
                    })) = event {
                        assert_eq!(capabilities.effective_mtu, 1234);
                        received = true;
                        receiver.swarm.behaviour_mut().control.send_response(channel,
                            ControlResponse::CapabilitiesAccepted(super::tests::capabilities())).unwrap();
                    }
                },
            }
        }
        assert!(received);
        assert!(syncs.pending.is_empty());
        assert_eq!(forwarder.application_demand(overlay), Some(before));
        assert!(forwarder.data_path_is_dormant(overlay));
        for _ in 0..1000 {
            syncs.drive_capabilities(&mut node.swarm, &forwarder, &metrics, Instant::now());
            assert!(syncs.pending.is_empty());
        }
    }).await.expect("native LAN-first capability delivery");
}
