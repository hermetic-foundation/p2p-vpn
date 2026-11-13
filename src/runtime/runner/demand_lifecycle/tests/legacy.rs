//! Encrypted TCP events with virtual dispatcher/policy time. Holding a native
//! terminal event models delayed dispatch, not cancellation of a native request.

use super::*;
use request_response::ProtocolSupport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Unsupported {
    Protocol,
    Response,
}

impl Unsupported {
    fn support(self) -> ProtocolSupport {
        match self {
            Self::Protocol => ProtocolSupport::Outbound,
            Self::Response => ProtocolSupport::Full,
        }
    }

    fn matches(self, event: &WireEvent, expected: request_response::OutboundRequestId) -> bool {
        match event {
            WireEvent::OutboundFailure {
                request_id,
                error: request_response::OutboundFailure::UnsupportedProtocols,
                ..
            } => self == Self::Protocol && *request_id == expected,
            WireEvent::Message {
                message:
                    Message::Response {
                        request_id,
                        response,
                    },
                ..
            } => {
                self == Self::Response
                    && *request_id == expected
                    && matches!(
                        response.reply,
                        Reply::Rejected {
                            reason: RejectCode::Unsupported
                        }
                    )
            }
            _ => false,
        }
    }
}

async fn native_unsupported(
    a: &mut Swarm<WireBehaviour>,
    b: &mut Swarm<WireBehaviour>,
    expected: request_response::OutboundRequestId,
    kind: Unsupported,
) -> WireEvent {
    loop {
        tokio::select! {
            event = a.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                if kind.matches(&event, expected) {
                    return event;
                }
                if let WireEvent::OutboundFailure { request_id, error, .. } = &event {
                    assert_ne!(*request_id, expected, "unexpected native failure: {error:?}");
                }
            },
            event = b.select_next_some() => {
                if let SwarmEvent::Behaviour(WireEvent::Message {
                    peer,
                    message: Message::Request { request, channel, .. },
                    ..
                }) = event {
                    assert_eq!(kind, Unsupported::Response);
                    assert_eq!(peer, *a.local_peer_id());
                    assert_eq!(request.command, protocol::Command::Idle);
                    b.behaviour_mut().send_response(
                        channel,
                        Response::for_request(&request, Reply::Rejected {
                            reason: RejectCode::Unsupported,
                        }),
                    ).unwrap();
                }
            },
        }
    }
}

async fn crossed_idle_fallback(kind: Unsupported) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, a_fixture, mut b, b_fixture) = connected_with_support(
            "/ip4/127.0.0.1/tcp/0", protocol::PROTOCOL, kind.support(),
        ).await;
        let a_peer = *a.local_peer_id();
        let b_peer = *b.local_peer_id();
        let overlay = PeerId::from_libp2p(b_peer);
        let before = a_fixture.forwarder.application_demand(overlay);
        let b_before = b_fixture.forwarder.application_demand(PeerId::from_libp2p(a_peer));
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut a_owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        let mut b_owner = DemandLifecycle::new(epoch(2), 2, 2, now);
        a_owner.observe_peer(&a_fixture.context(), b_peer, now);
        b_owner.observe_peer(&b_fixture.context(), a_peer, now);
        a_owner.offer_idle(a.behaviour_mut(), &a_fixture.context(), b_peer, idle).unwrap();
        b_owner.offer_idle(b.behaviour_mut(), &b_fixture.context(), a_peer, idle).unwrap();
        let expected = a_owner.peers[&b_peer].outbound.as_ref().unwrap().id;
        let mut terminal = None;
        let mut held_reply = None;
        let mut reply_sent = false;
        while !reply_sent
            || (kind == Unsupported::Protocol && terminal.is_none())
            || (kind == Unsupported::Response && held_reply.is_none())
        {
            tokio::select! {
                event = a.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    if kind.matches(&event, expected) {
                        assert!(terminal.replace(event).is_none());
                        continue;
                    }
                    let flushed = matches!(&event, WireEvent::ResponseSent { peer, .. } if *peer == b_peer);
                    a_owner.handle_event(a.behaviour_mut(), &a_fixture.context(), event, idle);
                    if flushed {
                        reply_sent = true;
                        assert_eq!(a_owner.phase(b_peer), Phase::Dormant);
                        assert!(a_fixture.forwarder.data_path_is_dormant(overlay));
                    }
                },
                event = b.select_next_some() => if let SwarmEvent::Behaviour(event) = event {
                    match event {
                        WireEvent::Message {
                            peer,
                            message: Message::Request { request, channel, .. },
                            ..
                        } => {
                            assert_eq!(kind, Unsupported::Response);
                            assert_eq!(peer, a_peer);
                            assert_eq!(request.command, protocol::Command::Idle);
                            assert!(held_reply.replace((request, channel)).is_none());
                        }
                        event => b_owner.handle_event(
                            b.behaviour_mut(), &b_fixture.context(), event, idle,
                        ),
                    }
                },
            }
        }
        assert!(reply_sent);
        assert_eq!(a_owner.phase(b_peer), Phase::Dormant);
        assert!(a_fixture.forwarder.data_path_is_dormant(overlay));
        assert_eq!(a_owner.snapshot().outbound, 1);
        assert_eq!(a_owner.peers[&b_peer].outbound.as_ref().unwrap().id, expected);
        assert_eq!(a_fixture.forwarder.application_demand(overlay), before);
        let terminal = match kind {
            Unsupported::Protocol => terminal.unwrap(),
            Unsupported::Response => {
                // A's actual reply flush must commit quiet before B releases this channel.
                let (request, channel) = held_reply.take().unwrap();
                b.behaviour_mut().send_response(
                    channel,
                    Response::for_request(&request, Reply::Rejected {
                        reason: RejectCode::Unsupported,
                    }),
                ).unwrap();
                native_unsupported(&mut a, &mut b, expected, kind).await
            }
        };
        assert!(kind.matches(&terminal, expected));
        a_owner.handle_event(a.behaviour_mut(), &a_fixture.context(), terminal, idle);
        assert_eq!(a_owner.phase(b_peer), Phase::Legacy);
        assert!(!a_fixture.forwarder.data_path_is_dormant(overlay));
        assert_eq!(a_fixture.forwarder.application_demand(overlay), before);
        assert_eq!(
            b_fixture.forwarder.application_demand(PeerId::from_libp2p(a_peer)),
            b_before,
        );
        assert_eq!(
            a_owner.peers[&b_peer].policy.as_ref().unwrap().last_demand(),
            None,
        );
        assert_eq!(a_owner.snapshot().outbound, 0);
        a_owner.drive(a.behaviour_mut(), &a_fixture.context(), idle + IDLE_AFTER);
        assert_eq!(a_owner.phase(b_peer), Phase::Legacy);
        assert_eq!(a_owner.snapshot().outbound, 0);
        assert_eq!(a_fixture.forwarder.application_demand(overlay), before);
    }).await.unwrap_or_else(|_| panic!("crossed idle fallback timed out: {kind:?}"));
}

#[tokio::test]
async fn crossed_idle_flush_then_unsupported_protocol_revokes_quiet_without_demand() {
    crossed_idle_fallback(Unsupported::Protocol).await;
}

#[tokio::test]
async fn crossed_idle_flush_then_unsupported_response_revokes_quiet_without_demand() {
    crossed_idle_fallback(Unsupported::Response).await;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Change {
    None,
    RemoteEpoch,
    Underlay,
    MembershipTag,
}

async fn late_unsupported(kind: Unsupported, change: Change) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (mut a, mut fixture, mut b, _) =
            connected_with_support("/ip4/127.0.0.1/tcp/0", protocol::PROTOCOL, kind.support())
                .await;
        let peer = *b.local_peer_id();
        let overlay = PeerId::from_libp2p(peer);
        let before = fixture.forwarder.application_demand(overlay);
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut owner = DemandLifecycle::new(epoch(1), 2, 2, now);
        owner.observe_peer(&fixture.context(), peer, now);
        owner
            .offer_idle(a.behaviour_mut(), &fixture.context(), peer, idle)
            .unwrap();
        let expected = owner.peers[&peer].outbound.as_ref().unwrap().id;
        let terminal = native_unsupported(&mut a, &mut b, expected, kind).await;
        assert!(kind.matches(&terminal, expected));
        assert!(!a.behaviour().is_pending_outbound(&peer, &expected));
        assert_eq!(owner.snapshot().outbound, 1);

        // Only dispatcher time advances: the encrypted native exchange already completed.
        let expired_at = idle + protocol::REQUEST_TIMEOUT + Duration::from_secs(1);
        owner.reconcile(&fixture.context(), expired_at);
        assert!(owner.peers[&peer].outbound.is_none());
        assert_eq!(owner.snapshot().outbound, 1);
        match change {
            Change::None => {}
            Change::RemoteEpoch => fixture.peers.record(
                overlay,
                ControlCapabilities::local("demand-test", None, 1280)
                    .with_demand_lifecycle_epoch(epoch(3)),
            ),
            Change::Underlay => {
                fixture.connections.advance();
            }
            Change::MembershipTag => {
                let tag = "a".repeat(64);
                fixture.local.membership_tag = Some(tag.clone());
                fixture.peers.record(
                    overlay,
                    ControlCapabilities::local("demand-test", Some(tag), 1280)
                        .with_demand_lifecycle_epoch(epoch(2)),
                );
            }
        }
        owner.observe_peer(&fixture.context(), peer, expired_at);
        owner.handle_event(a.behaviour_mut(), &fixture.context(), terminal, expired_at);
        assert_eq!(
            owner.phase(peer),
            if change == Change::None {
                Phase::Legacy
            } else {
                Phase::Active
            },
            "kind={kind:?}, change={change:?}",
        );
        assert_eq!(owner.snapshot().outbound, 0);
        assert!(!fixture.forwarder.data_path_is_dormant(overlay));
        assert_eq!(fixture.forwarder.application_demand(overlay), before);
        assert_eq!(
            owner.peers[&peer].policy.as_ref().unwrap().last_demand(),
            None,
        );
        if change == Change::None {
            owner.observe_peer(&fixture.context(), peer, expired_at + IDLE_AFTER);
            owner.drive(
                a.behaviour_mut(),
                &fixture.context(),
                expired_at + IDLE_AFTER,
            );
            assert_eq!(owner.phase(peer), Phase::Legacy);
            assert_eq!(owner.snapshot().outbound, 0);
            assert_eq!(fixture.forwarder.application_demand(overlay), before);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("late unsupported event timed out: {kind:?}, {change:?}"));
}

#[tokio::test]
async fn late_current_epoch_unsupported_events_enter_legacy_after_semantic_expiry() {
    for kind in [Unsupported::Protocol, Unsupported::Response] {
        late_unsupported(kind, Change::None).await;
    }
}

#[tokio::test]
async fn late_old_epoch_underlay_or_scope_events_do_not_disable_current_epoch() {
    for kind in [Unsupported::Protocol, Unsupported::Response] {
        for change in [Change::RemoteEpoch, Change::Underlay, Change::MembershipTag] {
            late_unsupported(kind, change).await;
        }
    }
}
