use super::*;

fn member_claim(fixture: &Fixture, sequence: u64, hostname: &str) -> SignedHostnameClaim {
    SignedHostnameClaim::issue(
        fixture.credentials.anchor().clone(),
        &fixture.member,
        fixture
            .state
            .snapshot()
            .payload
            .member(&fixture.member.peer_id)
            .unwrap()
            .incarnation,
        sequence,
        hostname,
    )
    .unwrap()
}

fn publisher_state(fixture: &Fixture) -> CooperativeMembershipState {
    let mut state = CooperativeMembershipState::restore(
        fixture.credentials.capability().unwrap(),
        fixture.member.peer_id.clone(),
        fixture.state.retained(),
    )
    .unwrap();
    let now = Instant::now();
    state.begin_resync(now, RESYNC_WINDOW).unwrap();
    state.finish_resync(now + RESYNC_WINDOW, WALL_NOW).unwrap();
    state
}

fn queue_owned_page(
    runtime: &mut CheckpointRuntime,
    node: &mut super::super::super::p2p::P2pNode,
    peer: PeerId,
    now: Instant,
) -> (request_response::OutboundRequestId, AbortHandle) {
    let pending = runtime.pending.as_ref().unwrap();
    let deadline = pending.deadline;
    let request = runtime
        .transfer
        .start_request(
            peer,
            pending.challenge.clone(),
            runtime.credentials.secret(),
            now,
            deadline,
        )
        .unwrap();
    let inventory = runtime
        .hostname_observations
        .get(&peer)
        .filter(|observation| observation.hint_present)
        .and_then(|observation| observation.inventory);
    let id = runtime.send_page_request(&mut node.swarm, peer, request, deadline, inventory);
    (id, runtime.requests[&id].cancellation.clone())
}

fn deliver_page_response(
    runtime: &mut CheckpointRuntime,
    node: &mut super::super::super::p2p::P2pNode,
    fixture: &Fixture,
    request_id: request_response::OutboundRequestId,
    response: CheckpointPageResponse,
    now: Instant,
) {
    runtime.handle_wire_event(
        &mut node.swarm,
        request_response::Event::Message {
            peer: fixture.member.peer_id.parse().unwrap(),
            connection_id: ConnectionId::new_unchecked(1),
            message: Message::Response {
                request_id,
                response,
            },
        },
        &fixture.local,
        now,
        WALL_NOW,
    );
}

async fn reconnect_hostname_nodes(
    local: &mut super::super::super::p2p::P2pNode,
    remote: &mut super::super::super::p2p::P2pNode,
) -> ConnectionId {
    use futures::StreamExt as _;
    use libp2p::swarm::{SwarmEvent, dial_opts::DialOpts};

    local
        .swarm
        .disconnect_peer_id(remote.local_peer_id)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut closed = [false; 2];
        while !closed.iter().all(|closed| *closed) {
            tokio::select! {
                event = local.swarm.select_next_some() => {
                    if matches!(event, SwarmEvent::ConnectionClosed { num_established: 0, .. }) {
                        closed[0] = true;
                    }
                }
                event = remote.swarm.select_next_some() => {
                    if matches!(event, SwarmEvent::ConnectionClosed { num_established: 0, .. }) {
                        closed[1] = true;
                    }
                }
            }
        }
        let address = remote.swarm.listeners().next().unwrap().clone();
        local
            .swarm
            .dial(
                DialOpts::peer_id(remote.local_peer_id)
                    .addresses(vec![address])
                    .build(),
            )
            .unwrap();
        let mut connection = None;
        let mut remote_connected = false;
        while connection.is_none() || !remote_connected {
            tokio::select! {
                event = local.swarm.select_next_some() => {
                    if let SwarmEvent::ConnectionEstablished { connection_id, .. } = event {
                        connection = Some(connection_id);
                    }
                }
                event = remote.swarm.select_next_some() => {
                    if matches!(event, SwarmEvent::ConnectionEstablished { .. }) {
                        remote_connected = true;
                    }
                }
            }
        }
        connection.unwrap()
    })
    .await
    .expect("native hostname reconnection")
}

#[tokio::test]
async fn page_leases_exclude_candidates_cancelled_owners_and_withdrawn_authority() {
    let fixture = Fixture::new("hostname-page-leases");
    let (mut runtime, mut forwarder) = fixture.participating();
    let mut node = test_node(&fixture.local);
    let peer = fixture.member.peer_id.parse().unwrap();
    let unknown = PeerId::random();
    let now = Instant::now();
    let capabilities = ControlCapabilities::local("lab", None, 1280)
        .with_checkpoint(&fixture.state)
        .unwrap();
    assert!(runtime.observe_capabilities(unknown, &capabilities, None, &[], now));
    assert_eq!(runtime.control_delivery_deadline(unknown, None, now), None);
    runtime.begin_resync(now).unwrap();
    assert_eq!(runtime.control_delivery_deadline(peer, None, now), None);
    let (id, cancellation) = queue_owned_page(&mut runtime, &mut node, peer, now);
    let (_, candidate_cancellation) = queue_owned_page(&mut runtime, &mut node, unknown, now);
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, now),
        Some(now + RESYNC_WINDOW)
    );
    assert_eq!(runtime.control_delivery_deadline(unknown, None, now), None);
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, now + RESYNC_WINDOW),
        None
    );
    assert!(
        !cancellation.is_aborted(),
        "lease inspection must not mutate native ownership"
    );
    cancellation.abort();
    assert_eq!(runtime.control_delivery_deadline(peer, None, now), None);
    assert!(runtime.requests.contains_key(&id));

    let mutation = runtime
        .state
        .sign_mutation_at(
            &fixture.local,
            MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
            WALL_NOW,
        )
        .unwrap();
    let mut candidate = runtime.state.clone();
    candidate.apply_mutation_at(&mutation, WALL_NOW).unwrap();
    runtime
        .persist_then_install(candidate, &fixture.store, &mut forwarder, WALL_NOW)
        .unwrap();
    assert!(candidate_cancellation.is_aborted());
    assert_eq!(runtime.requests.len(), 2);
    assert_eq!(runtime.control_delivery_deadline(peer, None, now), None);
    assert!(!forwarder.is_configured_transport_peer(peer));

    runtime.pending = None;
    runtime.retire_resync(now);
    let later = now + Duration::from_secs(1);
    runtime.begin_resync(later).unwrap();
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], later));
    let (_, revoked_cancellation) = queue_owned_page(&mut runtime, &mut node, peer, later);
    assert!(!revoked_cancellation.is_aborted());
    assert_eq!(runtime.control_delivery_deadline(peer, None, later), None);
    assert!(!forwarder.is_configured_transport_peer(peer));
}

#[tokio::test]
async fn old_challenge_slots_never_lease_a_replacement_round_even_before_native_cancellation() {
    let fixture = Fixture::new("hostname-challenge-lease");
    let (mut runtime, _) = fixture.participating();
    let mut node = test_node(&fixture.local);
    let peer = fixture.member.peer_id.parse().unwrap();
    let now = Instant::now();
    runtime.begin_resync(now).unwrap();
    let (old_id, old_cancellation) = queue_owned_page(&mut runtime, &mut node, peer, now);
    let old_challenge = runtime.challenge().unwrap().clone();
    // Isolate challenge correlation from the independent abort-handle guard.
    runtime.pending = None;
    let later = now + Duration::from_secs(1);
    runtime.begin_resync(later).unwrap();
    assert_ne!(runtime.challenge(), Some(&old_challenge));
    assert!(!old_cancellation.is_aborted());
    assert_eq!(runtime.control_delivery_deadline(peer, None, later), None);
    assert!(runtime.requests.contains_key(&old_id));
    let (fresh_id, fresh_cancellation) = queue_owned_page(&mut runtime, &mut node, peer, later);
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, later),
        Some(later + RESYNC_WINDOW)
    );
    runtime.retire_outbound(old_id, later);
    assert!(!fresh_cancellation.is_aborted());
    assert!(runtime.requests.contains_key(&fresh_id));
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, later),
        Some(later + RESYNC_WINDOW)
    );
}

#[tokio::test]
async fn continuation_pages_keep_the_original_deadline_and_abort_on_expiry() {
    use super::super::super::control::checkpoint::{
        CHECKPOINT_TRANSFER_VERSION, CheckpointPage, MAX_PAGE_BYTES,
    };

    let fixture = Fixture::new("hostname-page-continuation");
    let (mut runtime, mut forwarder) = fixture.participating();
    let mut node = test_node(&fixture.local);
    let peer = fixture.member.peer_id.parse().unwrap();
    let now = Instant::now();
    runtime.begin_resync(now).unwrap();
    let deadline = runtime.pending.as_ref().unwrap().deadline;
    let (id, _) = queue_owned_page(&mut runtime, &mut node, peer, now);
    let request = runtime.requests[&id].request.clone();
    runtime.handle_wire_event(
        &mut node.swarm,
        request_response::Event::Message {
            peer,
            connection_id: ConnectionId::new_unchecked(1),
            message: Message::Response {
                request_id: id,
                response: CheckpointPageResponse::Page(CheckpointPage {
                    version: CHECKPOINT_TRANSFER_VERSION,
                    challenge: request.challenge,
                    offer_digest: [209; 32],
                    cursor: 0,
                    total_bytes: u32::try_from(MAX_PAGE_BYTES * 2).unwrap(),
                    bytes: vec![1; MAX_PAGE_BYTES],
                }),
            },
        },
        &fixture.local,
        now + Duration::from_secs(1),
        WALL_NOW,
    );
    assert!(!runtime.requests.contains_key(&id));
    let (&next_id, next) = runtime.requests.iter().next().unwrap();
    assert_eq!(runtime.requests.len(), 1);
    assert_eq!(next.request.cursor, u32::try_from(MAX_PAGE_BYTES).unwrap());
    let cancellation = next.cancellation.clone();
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, now + Duration::from_secs(1)),
        Some(deadline)
    );
    runtime
        .drive_sync(&mut node.swarm, &PeerCapabilities::default(), deadline)
        .unwrap();
    assert!(cancellation.is_aborted());
    assert!(runtime.requests.contains_key(&next_id));
    assert_eq!(runtime.transfer.stats().buffered_bytes, 0);
    runtime
        .finish_due(&fixture.store, &mut forwarder, deadline, WALL_NOW)
        .unwrap();
    assert!(runtime.requests.contains_key(&next_id));
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, deadline),
        None
    );
}

#[tokio::test]
async fn cancelled_pages_release_slots_only_on_their_real_native_terminal_event() {
    use super::super::super::p2p::BehaviourEvent;
    use futures::StreamExt as _;
    use libp2p::swarm::SwarmEvent;

    let fixture = Fixture::new("hostname-page-cancellation");
    let (mut runtime, _) = fixture.participating();
    let mut node = test_node(&fixture.local);
    let peer = fixture.member.peer_id.parse().unwrap();
    let now = Instant::now();
    runtime.begin_resync(now).unwrap();
    let (id, cancellation) = queue_owned_page(&mut runtime, &mut node, peer, now);
    runtime.pending = None;
    runtime.retire_resync(now);
    assert!(cancellation.is_aborted());
    assert!(runtime.requests.contains_key(&id));
    assert_eq!(runtime.control_delivery_deadline(peer, None, now), None);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::Behaviour(BehaviourEvent::Checkpoint(
                event @ request_response::Event::OutboundFailure { request_id, .. },
            )) = node.swarm.select_next_some().await
            {
                assert_eq!(request_id, id);
                runtime.handle_wire_event(
                    &mut node.swarm,
                    event,
                    &fixture.local,
                    Instant::now(),
                    WALL_NOW,
                );
                break;
            }
        }
    })
    .await
    .expect("cancelled native page request must reach its terminal event");
    assert!(runtime.requests.is_empty());
}

#[tokio::test]
async fn mutation_request_leases_use_original_deadlines_and_not_dirty_or_retired_markers() {
    use super::super::super::checkpoint_handoff::ATTEMPT_WINDOW;

    let fixture = Fixture::new("hostname-mutation-lease");
    let (mut runtime, mut forwarder) = fixture.participating();
    let mut node = test_node(&fixture.local);
    let peer = fixture.member.peer_id.parse().unwrap();
    let now = Instant::now();
    runtime
        .apply_change_with_handoff(
            MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
            &fixture.local,
            &fixture.store,
            &mut forwarder,
            &[peer],
            now,
            WALL_NOW,
        )
        .unwrap();
    assert!(!forwarder.is_configured_transport_peer(peer));
    let deadline = runtime.handoff.as_ref().unwrap().deadline();
    assert_eq!(runtime.control_delivery_deadline(peer, None, now), None);
    runtime.drive_mutations(&mut node.swarm, now).unwrap();
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, now),
        Some(deadline)
    );
    assert_eq!(
        runtime.control_delivery_deadline(PeerId::random(), None, now),
        None
    );
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, deadline),
        None
    );
    runtime.finish_handoff_if_due(now + ATTEMPT_WINDOW);
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, now + ATTEMPT_WINDOW),
        None
    );
    assert_eq!(runtime.mutation_requests.len(), 1);
    let retry = now + ATTEMPT_WINDOW + Duration::from_secs(1);
    runtime.drive_mutations(&mut node.swarm, retry).unwrap();
    assert_eq!(runtime.mutation_requests.len(), 2);
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, retry),
        Some(deadline)
    );
    runtime.cancel_handoff();
    assert_eq!(runtime.control_delivery_deadline(peer, None, retry), None);
    assert_eq!(runtime.mutation_requests.len(), 2);
}

#[tokio::test]
async fn mutation_ack_leases_require_the_exact_native_connection_and_keep_original_expiry() {
    use super::super::super::p2p::BehaviourEvent;
    use futures::StreamExt as _;
    use libp2p::swarm::SwarmEvent;

    let fixture = Fixture::new("hostname-ack-lease");
    let (mut runtime, mut forwarder) = fixture.participating();
    let (mut node, mut remote, connections) = connected_mutation_nodes(&fixture).await;
    let peer = remote.local_peer_id;
    let mutation = publisher_state(&fixture)
        .sign_mutation_at(
            &fixture.member,
            MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
            WALL_NOW,
        )
        .unwrap();
    let request = CheckpointMutationRequest::new(mutation).unwrap();
    let event = receive_mutation_request(&mut remote, &mut node, request.clone()).await;
    let request_response::Event::Message {
        peer: sender,
        connection_id,
        message:
            Message::Request {
                request_id,
                channel,
                ..
            },
    } = event
    else {
        unreachable!("native mutation request");
    };
    assert_eq!(sender, peer);
    assert_eq!(connection_id, connections[0]);
    let now = Instant::now();
    let outcome =
        runtime.incoming_mutation_outcome(&request, peer, &fixture.store, &mut forwarder, WALL_NOW);
    assert!(matches!(outcome, MutationOutcome::Applied(_)));
    assert!(runtime.track_mutation_response(
        request_id,
        peer,
        connection_id,
        &request,
        outcome,
        now
    ));
    let deadline = now + MUTATION_REQUEST_TIMEOUT;
    assert!(!forwarder.is_configured_transport_peer(peer));
    assert_eq!(
        runtime.control_delivery_deadline(peer, Some(connection_id), now),
        Some(deadline)
    );
    assert_eq!(runtime.control_delivery_deadline(peer, None, now), None);
    assert_eq!(
        runtime.control_delivery_deadline(peer, Some(ConnectionId::new_unchecked(usize::MAX)), now),
        None
    );
    assert_eq!(
        runtime.control_delivery_deadline(PeerId::random(), Some(connection_id), now),
        None
    );
    assert_eq!(
        runtime.control_delivery_deadline(peer, Some(connection_id), deadline),
        None
    );
    assert_eq!(runtime.mutation_responses.len(), 1);
    assert!(runtime.track_mutation_response(
        request_id,
        peer,
        connection_id,
        &request,
        outcome,
        now + Duration::from_secs(1)
    ));
    assert_eq!(
        runtime.control_delivery_deadline(peer, Some(connection_id), now),
        Some(deadline)
    );
    let response = CheckpointMutationResponse::for_request(&request, outcome).unwrap();
    node.swarm
        .behaviour_mut()
        .checkpoint_mutation
        .send_response(channel, response)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                event = node.swarm.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::CheckpointMutation(
                        event @ request_response::Event::ResponseSent { request_id: id, .. },
                    )) = event {
                        assert_eq!(id, request_id);
                        runtime.handle_mutation_event(&mut node.swarm, event, &fixture.store, &mut forwarder, Instant::now(), WALL_NOW);
                        break;
                    }
                }
                _ = remote.swarm.select_next_some() => (),
            }
        }
    }).await.expect("native mutation ACK must release its exact control owner");
    assert_eq!(
        runtime.control_delivery_deadline(peer, Some(connection_id), Instant::now()),
        None
    );
}

#[test]
fn hostname_hint_observations_are_scoped_deduplicated_and_deadline_bound() {
    let fixture = Fixture::new("hostname-hint-bounds");
    let (mut runtime, forwarder) = fixture.participating();
    let peer = fixture.member.peer_id.parse().unwrap();
    let now = Instant::now();
    let mut capabilities = ControlCapabilities::local("lab", None, 1280)
        .with_checkpoint(&fixture.state)
        .unwrap();
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], now));
    assert!(runtime.hostname_observations[&peer].dirty_until.is_none());

    capabilities.checkpoint_hostname_inventory = Some([101; 32]);
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], now));
    let deadline = runtime.hostname_observations[&peer].dirty_until.unwrap();
    assert_eq!(deadline, now + HOSTNAME_RETRY_WINDOW);
    for offset in 1..4 {
        assert!(runtime.observe_capabilities(
            peer,
            &capabilities,
            None,
            &[],
            now + Duration::from_secs(offset)
        ));
        assert_eq!(
            runtime.hostname_observations[&peer].dirty_until,
            Some(deadline)
        );
    }
    capabilities.checkpoint_hostname_inventory = Some([102; 32]);
    assert!(runtime.observe_capabilities(
        peer,
        &capabilities,
        None,
        &[],
        now + Duration::from_secs(4)
    ));
    assert_eq!(
        runtime.hostname_observations[&peer].dirty_until,
        Some(deadline)
    );
    runtime.cleanup_hostname_observations(deadline);
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], deadline));
    assert!(runtime.hostname_observations[&peer].dirty_until.is_none());

    for invalid in ["network", "anchor", "tag"] {
        let mut foreign = capabilities.clone();
        match invalid {
            "network" => foreign.network_name = "other".into(),
            "anchor" => {
                foreign.checkpoint.as_mut().unwrap().anchor = NetworkAnchor::new([103; 32]).unwrap()
            }
            "tag" => foreign.membership_tag = Some("foreign".into()),
            _ => unreachable!(),
        }
        assert!(!runtime.observe_capabilities(peer, &foreign, None, &[], deadline));
        assert_eq!(
            runtime.hostname_observations[&peer].inventory,
            Some([102; 32])
        );
    }
    let unknown = PeerId::random();
    assert!(runtime.observe_capabilities(unknown, &capabilities, None, &[], deadline));
    assert!(!runtime.hostname_observations.contains_key(&unknown));
    assert!(!forwarder.is_configured_transport_peer(unknown));
    assert_eq!(runtime.hostname_observations.len(), 1);
    assert!(runtime.pending.is_none());

    runtime.hostname_observations.clear();
    capabilities
        .checkpoint
        .as_mut()
        .unwrap()
        .active_member_count -= 1;
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], deadline));
    assert!(runtime.hostname_observations.is_empty());
    assert_eq!(
        runtime.state().hostname_claims(),
        fixture.state.hostname_claims()
    );
}

#[tokio::test]
async fn failed_hostname_hints_retry_three_rounds_without_renewing_the_original_window() {
    let fixture = Fixture::new("hostname-finite-retries");
    let (mut runtime, mut forwarder) = fixture.participating();
    let (mut node, remote, connections) = connected_mutation_nodes(&fixture).await;
    let peer = remote.local_peer_id;
    let now = Instant::now();
    let mut capabilities = ControlCapabilities::local("lab", None, 1280)
        .with_checkpoint(&fixture.state)
        .unwrap();
    capabilities.checkpoint_hostname_inventory = Some([211; 32]);
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], now));
    runtime.hostname_connection_available(peer, connections[0], now);
    let window_deadline = now + HOSTNAME_RETRY_WINDOW;
    for round in 0..u32::from(MAX_HOSTNAME_RESYNC_ROUNDS) {
        let round_start = now + RESYNC_WINDOW * round;
        assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], round_start));
        runtime
            .drive_sync(&mut node.swarm, &PeerCapabilities::default(), round_start)
            .unwrap();
        let deadline = runtime.pending.as_ref().unwrap().deadline;
        assert_eq!(deadline, round_start + RESYNC_WINDOW);
        assert!(deadline <= window_deadline);
        assert_eq!(runtime.requests.len(), 1);
        let (&id, owner) = runtime.requests.iter().next().unwrap();
        let request = owner.request.clone();
        assert_eq!(owner.hostname_inventory, Some([211; 32]));
        assert_eq!(
            runtime.hostname_observations[&peer].dirty_until,
            Some(window_deadline)
        );
        assert_eq!(
            u32::from(runtime.hostname_observations[&peer].rounds_started),
            round + 1
        );
        deliver_page_response(
            &mut runtime,
            &mut node,
            &fixture,
            id,
            CheckpointPageResponse::rejected(&request, CheckpointRejection::Busy),
            round_start + Duration::from_secs(1),
        );
        assert!(runtime.requests.is_empty());
        assert!(!runtime.hostname_observations[&peer].consumed);
        runtime
            .drive_sync(
                &mut node.swarm,
                &PeerCapabilities::default(),
                round_start + Duration::from_secs(2),
            )
            .unwrap();
        assert!(
            runtime.requests.is_empty(),
            "failure must wait for the next round"
        );
        runtime
            .finish_due(&fixture.store, &mut forwarder, deadline, WALL_NOW)
            .unwrap();
    }
    assert_eq!(runtime.transfer_failures, 3);
    assert_eq!(runtime.offers_accepted, 0);
    for offset in [45, 60, 600, 3_600] {
        let later = now + Duration::from_secs(offset);
        assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], later));
        runtime
            .drive_sync(&mut node.swarm, &PeerCapabilities::default(), later)
            .unwrap();
        assert!(runtime.pending.is_none());
        assert!(runtime.requests.is_empty());
        assert!(!runtime.hostname_observations[&peer].consumed);
        assert!(runtime.hostname_observations[&peer].dirty_until.is_none());
    }
    let reconnected_at = now + Duration::from_secs(3_601);
    runtime.hostname_connection_available(peer, connections[0], reconnected_at);
    assert!(runtime.hostname_observations[&peer].dirty_until.is_none());
    // Inject a fresh connection policy event; socket reconnects are covered below.
    let new_connection = ConnectionId::new_unchecked(usize::MAX);
    runtime.hostname_connection_available(peer, new_connection, reconnected_at);
    assert_eq!(runtime.hostname_observations[&peer].rounds_started, 0);
    let renewed_deadline = reconnected_at + HOSTNAME_RETRY_WINDOW;
    assert_eq!(
        runtime.hostname_observations[&peer].dirty_until,
        Some(renewed_deadline)
    );
    runtime
        .drive_sync(
            &mut node.swarm,
            &PeerCapabilities::default(),
            reconnected_at,
        )
        .unwrap();
    assert_eq!(runtime.requests.len(), 1);
    assert_eq!(
        runtime.pending.as_ref().unwrap().deadline,
        reconnected_at + RESYNC_WINDOW
    );
    runtime.hostname_connection_available(
        peer,
        new_connection,
        reconnected_at + Duration::from_secs(1),
    );
    assert_eq!(
        runtime.hostname_observations[&peer].dirty_until,
        Some(renewed_deadline)
    );
}

#[tokio::test]
async fn late_hostname_round_and_native_lease_stop_at_the_original_window_deadline() {
    for existing_round in [false, true] {
        let fixture = Fixture::new(if existing_round {
            "hostname-clamped-existing-round"
        } else {
            "hostname-clamped-round"
        });
        let (mut runtime, mut forwarder) = fixture.participating();
        let (mut node, remote, connections) = connected_mutation_nodes(&fixture).await;
        let peer = remote.local_peer_id;
        let now = Instant::now();
        let mut capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_checkpoint(&fixture.state)
            .unwrap();
        capabilities.checkpoint_hostname_inventory = Some([214; 32]);
        assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], now));
        runtime.hostname_connection_available(peer, connections[0], now);
        if existing_round {
            runtime.begin_resync(now + Duration::from_secs(35)).unwrap();
        }
        let late = now + Duration::from_secs(40);
        runtime
            .drive_sync(&mut node.swarm, &PeerCapabilities::default(), late)
            .unwrap();
        let deadline = now + HOSTNAME_RETRY_WINDOW;
        let round_deadline = if existing_round {
            now + Duration::from_secs(50)
        } else {
            deadline
        };
        assert_eq!(runtime.pending.as_ref().unwrap().deadline, round_deadline);
        assert_eq!(
            runtime.control_delivery_deadline(peer, None, late),
            Some(deadline)
        );
        let (&id, owner) = runtime.requests.iter().next().unwrap();
        assert_eq!(owner.deadline, deadline);
        let cancellation = owner.cancellation.clone();
        assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], deadline));
        runtime.hostname_connection_available(peer, connections[0], deadline);
        runtime
            .drive_sync(&mut node.swarm, &PeerCapabilities::default(), deadline)
            .unwrap();
        assert!(cancellation.is_aborted());
        assert!(runtime.requests.contains_key(&id));
        assert_eq!(runtime.requests.len(), 1);
        assert_eq!(
            runtime.control_delivery_deadline(peer, None, deadline),
            None
        );
        runtime
            .finish_due(&fixture.store, &mut forwarder, round_deadline, WALL_NOW)
            .unwrap();
        runtime
            .drive_sync(
                &mut node.swarm,
                &PeerCapabilities::default(),
                round_deadline,
            )
            .unwrap();
        assert!(runtime.pending.is_none());
        assert_eq!(runtime.requests.len(), 1);
    }
}

#[test]
fn hostname_connection_rearms_only_unconsumed_current_work_on_a_new_id() {
    let fixture = Fixture::new("hostname-connection-retry");
    let (mut runtime, forwarder) = fixture.participating();
    let peer = fixture.member.peer_id.parse().unwrap();
    let demand_peer = crate::PeerId::from_libp2p(peer);
    let demand = forwarder.application_demand(demand_peer).unwrap();
    assert!(forwarder.grant_data_path_quiet(demand_peer, demand.version.unwrap()));
    let now = Instant::now();
    let mut capabilities = ControlCapabilities::local("lab", None, 1280)
        .with_checkpoint(&fixture.state)
        .unwrap();
    capabilities.checkpoint_hostname_inventory = Some([212; 32]);
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], now));
    let first = ConnectionId::new_unchecked(101);
    let second = ConnectionId::new_unchecked(102);
    runtime.hostname_connection_available(peer, first, now);
    runtime.hostname_connection_available(peer, first, now + HOSTNAME_RETRY_WINDOW);
    assert!(runtime.hostname_observations[&peer].dirty_until.is_none());
    let reconnected_at = now + Duration::from_secs(60);
    runtime.hostname_connection_available(peer, second, reconnected_at);
    let renewed_deadline = reconnected_at + HOSTNAME_RETRY_WINDOW;
    assert_eq!(
        runtime.hostname_observations[&peer].dirty_until,
        Some(renewed_deadline)
    );
    assert_eq!(runtime.hostname_observations[&peer].rounds_started, 0);
    for connection in [second, first] {
        runtime.hostname_connection_available(
            peer,
            connection,
            reconnected_at + Duration::from_secs(1),
        );
        assert_eq!(
            runtime.hostname_observations[&peer].dirty_until,
            Some(renewed_deadline)
        );
    }
    let third = ConnectionId::new_unchecked(103);
    runtime.hostname_connection_available(peer, second, renewed_deadline);
    assert!(runtime.hostname_observations[&peer].dirty_until.is_none());
    runtime.pairing_activation_blocked = true;
    runtime.hostname_connection_available(peer, third, renewed_deadline);
    assert!(runtime.hostname_observations[&peer].dirty_until.is_none());
    runtime.pairing_activation_blocked = false;
    let participating = runtime.state.clone();
    runtime.state = CooperativeMembershipState::restore(
        fixture.credentials.capability().unwrap(),
        fixture.local.peer_id.clone(),
        runtime.state.retained(),
    )
    .unwrap();
    runtime.hostname_connection_available(peer, third, renewed_deadline);
    assert!(runtime.hostname_observations[&peer].dirty_until.is_none());
    runtime.state = participating;
    let unknown = PeerId::random();
    runtime.hostname_connection_available(unknown, third, renewed_deadline);
    assert!(!runtime.hostname_observations.contains_key(&unknown));
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, renewed_deadline),
        None
    );
    assert_eq!(forwarder.application_demand(demand_peer), Some(demand));
    assert!(forwarder.data_path_is_dormant(demand_peer));

    let mutation = runtime
        .state
        .sign_mutation_at(
            &fixture.local,
            MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
            WALL_NOW,
        )
        .unwrap();
    runtime
        .state
        .apply_mutation_at(&mutation, WALL_NOW)
        .unwrap();
    runtime.hostname_connection_available(peer, third, renewed_deadline);
    assert!(runtime.hostname_observations.is_empty());
}

#[test]
fn hostname_connection_before_first_hint_also_deduplicates_its_connection_id() {
    let fixture = Fixture::new("hostname-connection-before-hint");
    let (mut runtime, _) = fixture.participating();
    let peer = fixture.member.peer_id.parse().unwrap();
    let now = Instant::now();
    let first = ConnectionId::new_unchecked(201);
    runtime.hostname_connection_available(peer, first, now);
    assert!(runtime.hostname_observations[&peer].inventory.is_none());
    assert!(!runtime.hostname_observations[&peer].hint_present);
    let mut capabilities = ControlCapabilities::local("lab", None, 1280)
        .with_checkpoint(&fixture.state)
        .unwrap();
    capabilities.checkpoint_hostname_inventory = Some([215; 32]);
    let advertised_at = now + Duration::from_secs(1);
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], advertised_at));
    let deadline = advertised_at + HOSTNAME_RETRY_WINDOW;
    assert_eq!(
        runtime.hostname_observations[&peer].dirty_until,
        Some(deadline)
    );
    assert!(!runtime.hostname_observations[&peer].consumed);
    runtime.hostname_connection_available(peer, first, deadline);
    assert!(runtime.hostname_observations[&peer].dirty_until.is_none());
    let second = ConnectionId::new_unchecked(202);
    runtime.hostname_connection_available(peer, second, deadline);
    assert_eq!(
        runtime.hostname_observations[&peer].dirty_until,
        Some(deadline + HOSTNAME_RETRY_WINDOW)
    );
}

#[tokio::test]
async fn newer_hostname_hint_during_paging_is_not_consumed_by_the_older_offer() {
    use super::super::super::control::checkpoint::{
        CHECKPOINT_TRANSFER_VERSION, CheckpointPage, MAX_PAGE_BYTES,
    };
    use sha2::{Digest as _, Sha256};

    let fixture = Fixture::new("hostname-paging-inventory");
    let (mut runtime, mut forwarder) = fixture.participating();
    let (mut node, remote, _) = connected_mutation_nodes(&fixture).await;
    let peer = remote.local_peer_id;
    let now = Instant::now();
    let mut publisher = publisher_state(&fixture);
    publisher
        .merge_hostname_claims(&[member_claim(&fixture, 1, "paged-name")])
        .unwrap();
    let latest_inventory = super::super::super::control::checkpoint_hostname_inventory(&publisher);
    let mut capabilities = ControlCapabilities::local("lab", None, 1280)
        .with_checkpoint(&fixture.state)
        .unwrap();
    capabilities.checkpoint_hostname_inventory = Some([213; 32]);
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], now));
    runtime
        .drive_sync(&mut node.swarm, &PeerCapabilities::default(), now)
        .unwrap();
    let (&first_id, first) = runtime.requests.iter().next().unwrap();
    let challenge = first.request.challenge.clone();
    let offer = publisher
        .make_offer_at(challenge.clone(), &fixture.member, WALL_NOW)
        .unwrap();
    let mut bytes = serde_json::to_vec(&offer).unwrap();
    assert!(bytes.len() < MAX_PAGE_BYTES);
    // Legal JSON whitespace forces real assembly/paging without a larger roster.
    bytes.resize(MAX_PAGE_BYTES * 2, b' ');
    let mut digest = Sha256::new();
    digest.update(b"p2p-vpn checkpoint transfer offer bytes v1\n");
    digest.update(&bytes);
    let digest: [u8; 32] = digest.finalize().into();
    for (index, chunk) in bytes.chunks(MAX_PAGE_BYTES).enumerate() {
        let id = if index == 0 {
            first_id
        } else {
            capabilities.checkpoint_hostname_inventory = Some(latest_inventory);
            assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], now));
            let (&id, owner) = runtime.requests.iter().next().unwrap();
            assert_eq!(owner.hostname_inventory, Some([213; 32]));
            id
        };
        deliver_page_response(
            &mut runtime,
            &mut node,
            &fixture,
            id,
            CheckpointPageResponse::Page(CheckpointPage {
                version: CHECKPOINT_TRANSFER_VERSION,
                challenge: challenge.clone(),
                offer_digest: digest,
                cursor: u32::try_from(index * MAX_PAGE_BYTES).unwrap(),
                total_bytes: u32::try_from(bytes.len()).unwrap(),
                bytes: chunk.to_vec(),
            }),
            now,
        );
    }
    assert_eq!(runtime.offers_accepted, 1);
    assert!(runtime.requests.is_empty());
    assert!(!runtime.hostname_observations[&peer].consumed);
    assert_eq!(
        runtime.hostname_observations[&peer].inventory,
        Some(latest_inventory)
    );
    assert_eq!(
        runtime.hostname_observations[&peer].dirty_until,
        Some(now + HOSTNAME_RETRY_WINDOW)
    );
    let later = now + RESYNC_WINDOW;
    runtime
        .finish_due(&fixture.store, &mut forwarder, later, WALL_NOW)
        .unwrap();
    assert_eq!(
        super::super::super::control::checkpoint_hostname_inventory(runtime.state()),
        latest_inventory
    );
    runtime
        .drive_sync(&mut node.swarm, &PeerCapabilities::default(), later)
        .unwrap();
    let (&id, owner) = runtime.requests.iter().next().unwrap();
    assert_eq!(owner.hostname_inventory, Some(latest_inventory));
    let request = owner.request.clone();
    let mut transfer = CheckpointSync::new(
        peer,
        fixture.credentials.anchor().clone(),
        CheckpointSyncLimits::default(),
    )
    .unwrap();
    let response = transfer
        .respond_at(
            node.local_peer_id,
            &request,
            &publisher,
            &fixture.member,
            fixture.credentials.secret(),
            later,
            WALL_NOW,
        )
        .unwrap();
    deliver_page_response(&mut runtime, &mut node, &fixture, id, response, later);
    assert_eq!(runtime.offers_accepted, 2);
    assert!(runtime.hostname_observations[&peer].consumed);
    assert!(runtime.hostname_observations[&peer].dirty_until.is_none());
}

#[test]
fn hostname_hint_never_authorizes_unsigned_foreign_or_old_incarnation_claims() {
    let fixture = Fixture::new("hostname-hint-authority");
    let (mut runtime, mut forwarder) = fixture.participating();
    let peer = fixture.member.peer_id.parse().unwrap();
    let rank = runtime.state().snapshot().payload.rank().unwrap();
    let now = Instant::now();
    runtime
        .begin_resync_until(now, now + RESYNC_WINDOW, Some(rank))
        .unwrap();
    let publisher = publisher_state(&fixture);
    let offer = publisher
        .make_offer_at(
            runtime.challenge().unwrap().clone(),
            &fixture.member,
            WALL_NOW,
        )
        .unwrap();
    for invalid in ["signature", "anchor", "incarnation"] {
        let mut claim = member_claim(&fixture, 100, "untrusted");
        match invalid {
            "signature" => claim.signature.clear(),
            "anchor" => {
                claim = SignedHostnameClaim::issue(
                    NetworkAnchor::new([104; 32]).unwrap(),
                    &fixture.member,
                    claim.payload.incarnation,
                    100,
                    "untrusted",
                )
                .unwrap()
            }
            "incarnation" => {
                claim = SignedHostnameClaim::issue(
                    fixture.credentials.anchor().clone(),
                    &fixture.member,
                    [105; 32],
                    100,
                    "untrusted",
                )
                .unwrap()
            }
            _ => unreachable!(),
        }
        assert!(
            runtime
                .state
                .clone()
                .merge_hostname_claims(&[claim.clone()])
                .is_err()
        );
        let mut invalid_offer = offer.clone();
        invalid_offer.payload.hostname_claims = vec![claim];
        assert!(runtime.collect_offer(&invalid_offer, peer, now).is_err());
    }
    assert!(
        runtime
            .collect_offer(&offer, PeerId::random(), now)
            .is_err()
    );
    assert!(
        runtime
            .collect_offer(&offer, peer, now + RESYNC_WINDOW)
            .is_err()
    );
    runtime
        .finish_due(
            &fixture.store,
            &mut forwarder,
            now + RESYNC_WINDOW,
            WALL_NOW,
        )
        .unwrap();
    assert!(runtime.state().hostname_claims().is_empty());
    assert_eq!(runtime.state().snapshot().payload.rank().unwrap(), rank);
}

#[tokio::test]
async fn hostname_round_expiry_keeps_native_ids_inert_until_exact_terminal_events() {
    let fixture = Fixture::new("hostname-native-retirement");
    let (mut runtime, mut forwarder) = fixture.participating();
    let mut node = test_node(&fixture.local);
    let peer = fixture.member.peer_id.parse().unwrap();
    let rank = runtime.state().snapshot().payload.rank().unwrap();
    let now = Instant::now();
    runtime
        .begin_resync_until(now, now + RESYNC_WINDOW, Some(rank))
        .unwrap();
    let old = runtime
        .transfer
        .start_request(
            peer,
            runtime.challenge().unwrap().clone(),
            runtime.credentials.secret(),
            now,
            now + RESYNC_WINDOW,
        )
        .unwrap();
    let (old_id, old_cancellation) = node
        .swarm
        .behaviour_mut()
        .checkpoint
        .send_request_cancellable(&peer, old.clone(), now + RESYNC_WINDOW);
    runtime.requests.insert(
        old_id,
        PendingCheckpointRequest {
            peer,
            request: old.clone(),
            hostname_inventory: None,
            deadline: now + RESYNC_WINDOW,
            cancellation: old_cancellation.clone(),
        },
    );
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, now),
        Some(now + RESYNC_WINDOW)
    );
    runtime
        .finish_due(
            &fixture.store,
            &mut forwarder,
            now + RESYNC_WINDOW,
            WALL_NOW,
        )
        .unwrap();
    assert!(runtime.requests.contains_key(&old_id));
    assert!(old_cancellation.is_aborted());
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, now + RESYNC_WINDOW),
        None
    );
    assert_eq!(runtime.transfer.stats().buffered_bytes, 0);
    let later = now + RESYNC_WINDOW + Duration::from_secs(1);
    runtime
        .begin_resync_until(later, later + RESYNC_WINDOW, Some(rank))
        .unwrap();
    let fresh = runtime
        .transfer
        .start_request(
            peer,
            runtime.challenge().unwrap().clone(),
            runtime.credentials.secret(),
            later,
            later + RESYNC_WINDOW,
        )
        .unwrap();
    let (fresh_id, fresh_cancellation) = node
        .swarm
        .behaviour_mut()
        .checkpoint
        .send_request_cancellable(&peer, fresh.clone(), later + RESYNC_WINDOW);
    runtime.requests.insert(
        fresh_id,
        PendingCheckpointRequest {
            peer,
            request: fresh.clone(),
            hostname_inventory: None,
            deadline: later + RESYNC_WINDOW,
            cancellation: fresh_cancellation.clone(),
        },
    );
    let response = |peer| request_response::Event::Message {
        peer,
        connection_id: ConnectionId::new_unchecked(1),
        message: Message::Response {
            request_id: old_id,
            response: CheckpointPageResponse::rejected(&old, CheckpointRejection::Expired),
        },
    };
    runtime.handle_wire_event(
        &mut node.swarm,
        response(PeerId::random()),
        &fixture.local,
        later,
        WALL_NOW,
    );
    assert_eq!(runtime.requests.len(), 2);
    runtime.handle_wire_event(
        &mut node.swarm,
        response(peer),
        &fixture.local,
        later,
        WALL_NOW,
    );
    assert_eq!(runtime.requests.len(), 1);
    assert!(runtime.requests.contains_key(&fresh_id));
    assert_eq!(runtime.challenge(), Some(&fresh.challenge));
    assert_eq!(runtime.offers_accepted, 0);
    assert!(!fresh_cancellation.is_aborted());
    assert_eq!(
        runtime.control_delivery_deadline(peer, None, later),
        Some(later + RESYNC_WINDOW)
    );

    runtime.discard_wire_event(
        request_response::Event::OutboundFailure {
            peer: PeerId::random(),
            connection_id: ConnectionId::new_unchecked(1),
            request_id: fresh_id,
            error: request_response::OutboundFailure::ConnectionClosed,
        },
        later,
    );
    assert!(runtime.requests.contains_key(&fresh_id));
    assert!(!fresh_cancellation.is_aborted());
    runtime.discard_wire_event(
        request_response::Event::OutboundFailure {
            peer,
            connection_id: ConnectionId::new_unchecked(1),
            request_id: fresh_id,
            error: request_response::OutboundFailure::ConnectionClosed,
        },
        later,
    );
    assert!(runtime.requests.is_empty());
    assert!(fresh_cancellation.is_aborted());
    assert_eq!(runtime.control_delivery_deadline(peer, None, later), None);
}

#[tokio::test]
async fn hinted_idle_peers_skip_general_rounds_but_missing_hints_keep_legacy_fetches() {
    let fixture = Fixture::new("hostname-legacy-scheduling");
    let (mut runtime, mut forwarder) = fixture.participating();
    let (mut node, remote, _) = connected_mutation_nodes(&fixture).await;
    let now = Instant::now();
    let peer = remote.local_peer_id;
    let mut capabilities = ControlCapabilities::local("lab", None, 1280)
        .with_checkpoint(&fixture.state)
        .unwrap();
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], now));
    runtime.begin_resync(now).unwrap();
    runtime
        .drive_sync(&mut node.swarm, &PeerCapabilities::default(), now)
        .unwrap();
    assert!(runtime.requests.is_empty());
    runtime
        .finish_due(
            &fixture.store,
            &mut forwarder,
            now + RESYNC_WINDOW,
            WALL_NOW,
        )
        .unwrap();

    let later = now + Duration::from_secs(120);
    capabilities.checkpoint_hostname_inventory = None;
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], later));
    runtime
        .drive_sync(&mut node.swarm, &PeerCapabilities::default(), later)
        .unwrap();
    assert!(runtime.pending.as_ref().unwrap().hostname_rank.is_none());
    assert_eq!(runtime.requests.len(), 1);
}

#[tokio::test]
async fn hostname_expiry_cannot_recycle_a_full_native_budget_or_survive_authority_loss() {
    let fixture = Fixture::new("hostname-native-budget");
    let (mut runtime, mut forwarder) = fixture.participating();
    let (mut node, remote, _) = connected_mutation_nodes(&fixture).await;
    let peer = remote.local_peer_id;
    let now = Instant::now();
    let rank = runtime.state().snapshot().payload.rank().unwrap();
    runtime
        .begin_resync_until(now, now + RESYNC_WINDOW, Some(rank))
        .unwrap();
    let old = runtime
        .transfer
        .start_request(
            peer,
            runtime.challenge().unwrap().clone(),
            runtime.credentials.secret(),
            now,
            now + RESYNC_WINDOW,
        )
        .unwrap();
    for _ in 0..MAX_TRANSFER_SESSIONS {
        let (id, cancellation) = node
            .swarm
            .behaviour_mut()
            .checkpoint
            .send_request_cancellable(&peer, old.clone(), now + RESYNC_WINDOW);
        runtime.requests.insert(
            id,
            PendingCheckpointRequest {
                peer,
                request: old.clone(),
                hostname_inventory: None,
                deadline: now + RESYNC_WINDOW,
                cancellation,
            },
        );
    }
    runtime
        .finish_due(
            &fixture.store,
            &mut forwarder,
            now + RESYNC_WINDOW,
            WALL_NOW,
        )
        .unwrap();
    assert!(
        runtime
            .requests
            .values()
            .all(|owner| owner.cancellation.is_aborted())
    );
    let later = now + Duration::from_secs(120);
    let mut capabilities = ControlCapabilities::local("lab", None, 1280)
        .with_checkpoint(&fixture.state)
        .unwrap();
    capabilities.checkpoint_hostname_inventory = Some([202; 32]);
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], later));
    runtime
        .drive_sync(&mut node.swarm, &PeerCapabilities::default(), later)
        .unwrap();
    assert_eq!(runtime.requests.len(), MAX_TRANSFER_SESSIONS);
    assert!(runtime.attempted.is_empty());
    runtime
        .finish_due(
            &fixture.store,
            &mut forwarder,
            later + RESYNC_WINDOW,
            WALL_NOW,
        )
        .unwrap();
    assert_eq!(runtime.requests.len(), MAX_TRANSFER_SESSIONS);
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], later + RESYNC_WINDOW));
    runtime
        .drive_sync(
            &mut node.swarm,
            &PeerCapabilities::default(),
            later + RESYNC_WINDOW,
        )
        .unwrap();
    assert!(runtime.pending.is_some());
    assert!(runtime.attempted.is_empty());
    runtime
        .finish_due(
            &fixture.store,
            &mut forwarder,
            later + RESYNC_WINDOW * 2,
            WALL_NOW,
        )
        .unwrap();
    runtime
        .drive_sync(
            &mut node.swarm,
            &PeerCapabilities::default(),
            later + HOSTNAME_RETRY_WINDOW,
        )
        .unwrap();
    assert!(runtime.pending.is_none());

    let revoked_at = later + Duration::from_secs(120);
    capabilities.checkpoint_hostname_inventory = Some([203; 32]);
    assert!(runtime.observe_capabilities(peer, &capabilities, None, &[], revoked_at));
    runtime
        .drive_sync(&mut node.swarm, &PeerCapabilities::default(), revoked_at)
        .unwrap();
    assert!(runtime.pending.is_some());
    let mutation = runtime
        .state
        .sign_mutation_at(
            &fixture.local,
            MembershipChange::RemoveMember(fixture.member.peer_id.clone()),
            WALL_NOW,
        )
        .unwrap();
    let mut candidate = runtime.state.clone();
    candidate.apply_mutation_at(&mutation, WALL_NOW).unwrap();
    runtime
        .persist_then_install(candidate, &fixture.store, &mut forwarder, WALL_NOW)
        .unwrap();
    assert!(
        runtime
            .finish_due(
                &fixture.store,
                &mut forwarder,
                revoked_at + RESYNC_WINDOW,
                WALL_NOW
            )
            .unwrap()
            .is_none()
    );
    runtime.cleanup_hostname_observations(revoked_at + RESYNC_WINDOW);
    assert!(runtime.hostname_observations.is_empty());
    assert_eq!(runtime.requests.len(), MAX_TRANSFER_SESSIONS);
    assert!(!forwarder.is_configured_transport_peer(peer));
    assert!(
        runtime
            .state()
            .snapshot()
            .payload
            .member(&fixture.member.peer_id)
            .is_none()
    );
}

#[tokio::test]
async fn hostname_hint_native_busy_retry_and_completed_noop_reconnections_stay_consumed() {
    use super::super::super::{
        control::{ControlRequest, ControlResponse},
        p2p::BehaviourEvent,
    };
    use futures::StreamExt as _;
    use libp2p::swarm::SwarmEvent;

    let mut fixture = Fixture::new("hostname-native-rename");
    let old_claim = member_claim(&fixture, 1, "before-rename");
    fixture.state.merge_hostname_claims(&[old_claim]).unwrap();
    fixture
        .store
        .save_checkpoint(
            "lab",
            &fixture.local.peer_id,
            &fixture.credentials,
            &fixture.state.retained(),
        )
        .unwrap();
    let (mut local, mut forwarder) = fixture.participating();
    let demand_peer = crate::PeerId::from_libp2p(fixture.member.peer_id.parse().unwrap());
    let demand = forwarder.application_demand(demand_peer).unwrap();
    assert!(forwarder.grant_data_path_quiet(demand_peer, demand.version.unwrap()));
    let remote_store = MembershipStateStore::new(fixture.directory.join("hostname-remote.json"));
    remote_store
        .save_checkpoint(
            "lab",
            &fixture.member.peer_id,
            &fixture.credentials,
            &fixture.state.retained(),
        )
        .unwrap();
    let Some(PersistedAuthority::Checkpoint(loaded)) = remote_store
        .load_authority(
            "lab",
            &fixture.member.peer_id,
            Some(fixture.credentials.anchor()),
            None,
        )
        .unwrap()
    else {
        panic!("remote checkpoint authority");
    };
    let mut remote =
        CheckpointRuntime::restore("lab".into(), &fixture.member.peer_id, *loaded).unwrap();
    remote.state = publisher_state(&fixture);
    let mut remote_config = fixture.config.clone();
    remote_config.network.local_peer = fixture.member.peer_id.clone();
    remote_config.network.private_key = Some(fixture.member.private_key.clone());
    remote_config.network.dns.hostname = Some("after-rename".into());
    remote_config.peers.clear();
    let mut remote_forwarder = Forwarder::from_checkpoint_config(
        &remote_config,
        remote.state(),
        remote.anchor(),
        WALL_NOW,
    )
    .unwrap();
    let rank = local.state().snapshot().payload.rank().unwrap();
    assert!(
        remote
            .reconcile_local_hostname(
                &fixture.member,
                &remote_store,
                &mut remote_forwarder,
                WALL_NOW
            )
            .unwrap()
    );
    assert_eq!(remote.state().snapshot().payload.rank().unwrap(), rank);
    let (mut local_node, mut remote_node, _) = connected_mutation_nodes(&fixture).await;
    let mut peers = PeerCapabilities::default();
    let start = Instant::now();

    for (index, phase) in [
        "rename",
        "duplicate",
        "noop",
        "noop-duplicate",
        "stale",
        "stale-duplicate",
    ]
    .into_iter()
    .enumerate()
    {
        let mut now = start + Duration::from_secs(u64::try_from(index).unwrap() * 180);
        if phase == "stale" {
            remote.state = publisher_state(&fixture);
        }
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        remote.decorate_capabilities(&mut capabilities).unwrap();
        assert!(!capabilities.supports_demand_lifecycle);
        if phase.starts_with("noop") {
            capabilities.checkpoint_hostname_inventory = Some([201; 32]);
        }
        let expected_tag = capabilities.membership_tag.clone();
        let before_offers = local.offers_accepted;
        let before_failures = local.transfer_failures;
        let before_disk = fs::read(fixture.path()).unwrap();
        let expect_offer = matches!(phase, "rename" | "noop" | "stale");
        let request_id = remote_node.swarm.behaviour_mut().control.send_request(
            &local_node.local_peer_id,
            ControlRequest::Capabilities(capabilities),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut observed = false;
            let mut acknowledged = false;
            let mut reject_once = phase == "rename";
            let mut retried = false;
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            loop {
                tokio::select! {
                    _ = tick.tick() => local.drive_sync(&mut local_node.swarm, &peers, now).unwrap(),
                    event = local_node.swarm.select_next_some() => match event {
                        SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                            peer, message: Message::Request { request: ControlRequest::Capabilities(capabilities), channel, .. }, ..
                        })) => {
                            assert!(local.observe_capabilities(peer, &capabilities, expected_tag.as_deref(), &[], now));
                            peers.record(crate::PeerId::from_libp2p(peer), capabilities);
                            local.drive_sync(&mut local_node.swarm, &peers, now).unwrap();
                            let mut reply = ControlCapabilities::local("lab", None, 1280);
                            local.decorate_capabilities(&mut reply).unwrap();
                            local_node.swarm.behaviour_mut().control.send_response(channel, ControlResponse::CapabilitiesAccepted(reply)).unwrap();
                            observed = true;
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::Checkpoint(event)) => local.handle_wire_event(&mut local_node.swarm, event, &fixture.local, now, WALL_NOW),
                        _ => (),
                    },
                    event = remote_node.swarm.select_next_some() => match event {
                        SwarmEvent::Behaviour(BehaviourEvent::Checkpoint(request_response::Event::Message {
                            message: Message::Request { request, channel, .. }, ..
                        })) if reject_once => {
                            reject_once = false;
                            remote_node.swarm.behaviour_mut().checkpoint.send_response(
                                channel,
                                CheckpointPageResponse::rejected(&request, CheckpointRejection::Busy),
                            ).unwrap();
                        }
                        SwarmEvent::Behaviour(BehaviourEvent::Checkpoint(event)) => remote.handle_wire_event(&mut remote_node.swarm, event, &fixture.member, now, WALL_NOW),
                        SwarmEvent::Behaviour(BehaviourEvent::Control(request_response::Event::Message {
                            message: Message::Response { request_id: id, response: ControlResponse::CapabilitiesAccepted(_) }, ..
                        })) if id == request_id => acknowledged = true,
                        _ => (),
                    },
                }
                if phase == "rename" && !retried && local.transfer_failures > before_failures {
                    let observation = &local.hostname_observations[&remote_node.local_peer_id];
                    assert!(!observation.consumed);
                    assert_eq!(observation.dirty_until, Some(now + HOSTNAME_RETRY_WINDOW));
                    assert_eq!(local.offers_accepted, before_offers);
                    // Advance policy time only; this is not a long-idle/energy measurement.
                    now += RESYNC_WINDOW;
                    local.finish_due(&fixture.store, &mut forwarder, now, WALL_NOW).unwrap();
                    local.drive_sync(&mut local_node.swarm, &peers, now).unwrap();
                    assert_eq!(local.requests.len(), 1);
                    retried = true;
                }
                if observed && acknowledged && (!expect_offer || local.offers_accepted > before_offers) {
                    break;
                }
            }
        }).await.expect("native hostname capability/transfer delivery");
        if expect_offer {
            assert!(local.hostname_observations[&remote_node.local_peer_id].consumed);
            assert!(local.pending.as_ref().unwrap().hostname_rank.is_some());
            local
                .finish_due(
                    &fixture.store,
                    &mut forwarder,
                    now + RESYNC_WINDOW,
                    WALL_NOW,
                )
                .unwrap()
                .unwrap();
        } else {
            assert_eq!(local.offers_accepted, before_offers);
            assert!(local.pending.is_none());
        }
        assert!(local.requests.is_empty());
        assert_eq!(local.state().snapshot().payload.rank().unwrap(), rank);
        assert_eq!(
            local.state().hostname_claims()[0].payload.hostname,
            "after-rename"
        );
        assert_eq!(local.state().hostname_claims()[0].payload.sequence, 2);
        assert_eq!(forwarder.application_demand(demand_peer), Some(demand));
        assert!(forwarder.data_path_is_dormant(demand_peer));
        if phase != "rename" {
            assert_eq!(fs::read(fixture.path()).unwrap(), before_disk);
        }
        if matches!(phase, "rename" | "noop") {
            let connection = reconnect_hostname_nodes(&mut local_node, &mut remote_node).await;
            let reconnected_at = now + Duration::from_secs(60);
            for _ in 0..2 {
                local.hostname_connection_available(
                    remote_node.local_peer_id,
                    connection,
                    reconnected_at,
                );
            }
            local
                .drive_sync(&mut local_node.swarm, &peers, reconnected_at)
                .unwrap();
            assert!(local.pending.is_none());
            assert!(local.requests.is_empty());
            assert!(local.hostname_observations[&remote_node.local_peer_id].consumed);
            assert!(
                local.hostname_observations[&remote_node.local_peer_id]
                    .dirty_until
                    .is_none()
            );
            assert_eq!(forwarder.application_demand(demand_peer), Some(demand));
            assert!(forwarder.data_path_is_dormant(demand_peer));
        }
        for offset in [60, 120] {
            local
                .drive_sync(
                    &mut local_node.swarm,
                    &peers,
                    now + Duration::from_secs(offset),
                )
                .unwrap();
            assert!(
                local.pending.is_none(),
                "unchanged hinted peers must stay idle: {phase}"
            );
            assert!(local.requests.is_empty());
        }
    }
    let peer = crate::PeerId::from_libp2p(remote_node.local_peer_id);
    assert_eq!(
        forwarder
            .effective_hostname_records()
            .unwrap()
            .get(&peer)
            .map(String::as_str),
        Some("after-rename")
    );
    assert_eq!(
        fixture.restored().0.state().hostname_claims()[0]
            .payload
            .hostname,
        "after-rename"
    );
}
