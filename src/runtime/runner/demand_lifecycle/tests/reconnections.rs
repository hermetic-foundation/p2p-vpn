use super::*;

#[tokio::test]
async fn repeated_short_reconnections_preserve_idle_age_but_require_fresh_capabilities() {
    let peer = Libp2pPeerId::random();
    let overlay = PeerId::from_libp2p(peer);
    let mut f = Fixture::new(peer, 1, 2);
    let caps = f.peers.get(overlay).unwrap().clone();
    let now = Instant::now();
    let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
    let mut wire = wire_swarm();
    owner.observe_peer(&f.context(), peer, now);
    let before = f.forwarder.application_demand(overlay);
    for seconds in [20, 40, 60, 80] {
        let at = now + Duration::from_secs(seconds);
        invalidate_peer_capabilities_when_disconnected(
            &f.forwarder,
            &mut f.peers,
            None,
            None,
            peer,
            0,
        );
        owner.drive(wire.behaviour_mut(), &f.context(), at);
        assert_eq!(owner.snapshot().peers, 1);
        assert!(owner.peers[&peer].capabilities_suspended);
        assert!(f.context().session(peer, epoch(1)).is_none());
        owner.prepare_idle(&f.context(), peer, at);
        assert!(!f.forwarder.data_path_maintenance_is_paused(overlay));
        assert!(!f.forwarder.data_path_is_dormant(overlay));
        assert_eq!(owner.snapshot().outbound, 0);
        invalidate_peer_capabilities_on_first_connection(&f.forwarder, &mut f.peers, peer, 1);
        f.peers.record(overlay, caps.clone());
        owner.drive(
            wire.behaviour_mut(),
            &f.context(),
            at + Duration::from_secs(1),
        );
        assert_eq!(owner.peers[&peer].remote_epoch, Some(epoch(2)));
        assert_eq!(
            f.forwarder.data_path_maintenance_is_paused(overlay),
            seconds >= 60
        );
        assert!(!f.forwarder.data_path_is_dormant(overlay));
        assert_eq!(f.forwarder.application_demand(overlay), before);
        assert!(!wire.behaviour().has_any_pending_requests());
    }
}

#[tokio::test]
async fn demand_during_capability_absence_and_new_epochs_restart_the_proper_grace() {
    for new_epoch in [false, true] {
        let peer = Libp2pPeerId::random();
        let overlay = PeerId::from_libp2p(peer);
        let mut f = Fixture::new(peer, 1, 2);
        let now = Instant::now();
        let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        let mut wire = wire_swarm();
        owner.observe_peer(&f.context(), peer, now);
        f.peers.remove(overlay);
        let at = now + IDLE_AFTER * 2;
        owner.drive(wire.behaviour_mut(), &f.context(), at);
        if !new_epoch {
            assert!(f.forwarder.record_application_demand(overlay, at));
            owner.reconcile(&f.context(), at);
        }
        f.peers.record(
            overlay,
            ControlCapabilities::local("demand-test", None, 1280)
                .with_demand_lifecycle_epoch(epoch(if new_epoch { 3 } else { 2 })),
        );
        owner.drive(wire.behaviour_mut(), &f.context(), at);
        assert!(!f.forwarder.data_path_maintenance_is_paused(overlay));
        owner.drive(wire.behaviour_mut(), &f.context(), at + IDLE_AFTER);
        assert!(f.forwarder.data_path_maintenance_is_paused(overlay));
        assert!(!f.forwarder.data_path_is_dormant(overlay));
        assert_eq!(
            owner.peers[&peer].policy.as_ref().unwrap().last_demand(),
            (!new_epoch).then_some(at)
        );
    }
}

#[tokio::test]
async fn capability_revalidation_cannot_refill_request_budgets_or_wrap_session_fences() {
    let peer = Libp2pPeerId::random();
    let overlay = PeerId::from_libp2p(peer);
    let mut f = Fixture::new(peer, 1, 2);
    let caps = f.peers.get(overlay).unwrap().clone();
    let now = Instant::now();
    let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
    owner.observe_peer(&f.context(), peer, now);
    for _ in 0..PEER_REQUESTS_PER_SECOND {
        assert!(owner.allow_request(peer, now));
    }
    f.peers.remove(overlay);
    owner.reconcile(&f.context(), now);
    f.peers.record(overlay, caps);
    owner.observe_peer(&f.context(), peer, now);
    assert!(!owner.allow_request(peer, now));
    owner.peers.get_mut(&peer).unwrap().session_generation = u64::MAX - 1;
    f.peers.remove(overlay);
    owner.reconcile(&f.context(), now);
    f.peers.record(
        overlay,
        ControlCapabilities::local("demand-test", None, 1280).with_demand_lifecycle_epoch(epoch(3)),
    );
    assert!(!owner.admit_peer(&f.context(), peer, now));
    assert_eq!(owner.phase(peer), Phase::Legacy);
    assert_eq!(owner.peers[&peer].session_generation, u64::MAX);
    assert!(!f.forwarder.data_path_maintenance_is_paused(overlay));
}

#[tokio::test]
async fn same_epoch_revalidation_releases_but_does_not_commit_an_old_native_ack() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, mut af, mut b, bf) = connected("/ip4/127.0.0.1/tcp/0").await;
        let ap = *a.local_peer_id();
        let bp = *b.local_peer_id();
        let overlay = PeerId::from_libp2p(bp);
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut ao = DemandLifecycle::new(epoch(1), 2, 1, now);
        let mut bo = DemandLifecycle::new(epoch(2), 2, 1, now);
        ao.observe_peer(&af.context(), bp, now);
        bo.observe_peer(&bf.context(), ap, now);
        ao.offer_idle(a.behaviour_mut(), &af.context(), bp, idle).unwrap();
        let terminal = loop {
            tokio::select! {
                event = a.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    if matches!(&event, WireEvent::Message { message: Message::Response { .. }, .. }) {
                        break event;
                    }
                    ao.handle_event(a.behaviour_mut(), &af.context(), event, idle);
                },
                event = b.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    bo.handle_event(b.behaviour_mut(), &bf.context(), event, idle);
                },
            }
        };
        let caps = af.peers.get(overlay).unwrap().clone();
        af.peers.remove(overlay);
        ao.reconcile(&af.context(), idle);
        assert_eq!(ao.snapshot().outbound, 1);
        assert!(ao.peers[&bp].outbound.is_none());
        af.peers.record(overlay, caps);
        ao.observe_peer(&af.context(), bp, idle);
        ao.handle_event(a.behaviour_mut(), &af.context(), terminal, idle);
        assert_eq!(ao.snapshot().outbound, 0);
        assert_eq!(ao.phase(bp), Phase::Active);
        assert!(!af.forwarder.data_path_is_dormant(overlay));
    }).await.expect("native late ACK cannot cross capability absence");
}
