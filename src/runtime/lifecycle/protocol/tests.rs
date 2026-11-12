use std::{cell::Cell, time::Instant};

use futures::{StreamExt, io::Cursor};
use libp2p::{Swarm, SwarmBuilder, request_response::Codec as _, swarm::SwarmEvent};

use super::*;
use crate::runtime::{
    control::{
        self, ControlCapabilities, ControlCodec, ControlRequest, ControlResponse, PeerCapabilities,
    },
    lifecycle::{IDLE_AFTER, PeerLifecycle, Phase, Readiness, Rejection},
};

fn epoch(value: u8) -> Epoch {
    Epoch::new([value; 16]).unwrap()
}

fn scope() -> Scope {
    Scope {
        network_name: "lab".to_owned(),
        membership_tag: None,
    }
}

fn request() -> Request {
    Request::idle(
        scope(),
        IdleOffer {
            sender_epoch: epoch(1),
            receiver_epoch: epoch(2),
            revision: 1,
        },
    )
}

fn session(peer: PeerId) -> Session<'static> {
    Session {
        peer,
        local_epoch: epoch(2),
        remote_epoch: epoch(1),
        network_name: "lab",
        membership_tag: None,
        previous_membership_tags: &[],
    }
}

fn frame(value: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap();
    let mut output = u16::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    output.extend(body);
    output
}

async fn roundtrip_request(value: Request) {
    let mut bytes = Cursor::new(Vec::new());
    let protocol = StreamProtocol::new(PROTOCOL);
    LifecycleCodec
        .write_request(&protocol, &mut bytes, value.clone())
        .await
        .unwrap();
    assert!(bytes.get_ref().len() <= MAX_MESSAGE_BYTES + 2);
    bytes.set_position(0);
    assert_eq!(
        LifecycleCodec
            .read_request(&protocol, &mut bytes)
            .await
            .unwrap(),
        value
    );
}

async fn roundtrip_response(value: Response) {
    let mut bytes = Cursor::new(Vec::new());
    let protocol = StreamProtocol::new(PROTOCOL);
    LifecycleCodec
        .write_response(&protocol, &mut bytes, value.clone())
        .await
        .unwrap();
    assert!(bytes.get_ref().len() <= MAX_MESSAGE_BYTES + 2);
    bytes.set_position(0);
    assert_eq!(
        LifecycleCodec
            .read_response(&protocol, &mut bytes)
            .await
            .unwrap(),
        value
    );
}

#[tokio::test]
async fn request_commands_roundtrip() {
    roundtrip_request(request()).await;
    roundtrip_request(Request {
        command: Command::Wake,
        ..request()
    })
    .await;
}

#[tokio::test]
async fn accepted_and_all_rejected_responses_roundtrip() {
    for reply in [
        Reply::IdleAccepted {
            responder_revision: 0,
        },
        Reply::IdleAccepted {
            responder_revision: u64::MAX,
        },
        Reply::WakeAccepted {},
    ] {
        roundtrip_response(Response::for_request(&request(), reply)).await;
    }
    for reason in [
        RejectCode::Busy,
        RejectCode::NoIncomingPath,
        RejectCode::NotIdle,
        RejectCode::Unsupported,
        RejectCode::Stale,
        RejectCode::RateLimited,
        RejectCode::Unauthorized,
    ] {
        roundtrip_response(Response::for_request(
            &request(),
            Reply::Rejected { reason },
        ))
        .await;
    }
}

#[tokio::test]
async fn maximum_scope_and_integer_values_fit_the_frame_even_when_json_escaped() {
    let value = Request {
        scope: Scope {
            network_name: "\0".repeat(MAX_NETWORK_NAME_BYTES),
            membership_tag: Some("\u{0001}".repeat(MAX_MEMBERSHIP_TAG_BYTES)),
        },
        intent: Intent {
            sender_epoch: [255; 16],
            receiver_epoch: [255; 16],
            revision: u64::MAX,
        },
        ..request()
    };
    roundtrip_request(value.clone()).await;
    roundtrip_response(Response::for_request(
        &value,
        Reply::IdleAccepted {
            responder_revision: u64::MAX,
        },
    ))
    .await;
}

#[tokio::test]
async fn length_prefix_is_checked_before_reading_the_payload() {
    for length in [0, u16::try_from(MAX_MESSAGE_BYTES + 1).unwrap(), u16::MAX] {
        let mut bytes = Cursor::new(length.to_be_bytes().to_vec());
        let error = LifecycleCodec
            .read_request(&StreamProtocol::new(PROTOCOL), &mut bytes)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(bytes.position(), 2);
    }
}

#[tokio::test]
async fn truncated_header_and_payload_are_rejected() {
    for input in [vec![0], vec![0, 5, b'{']] {
        let error = LifecycleCodec
            .read_request(&StreamProtocol::new(PROTOCOL), &mut Cursor::new(input))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
}

#[tokio::test]
async fn trailing_frames_and_json_junk_are_rejected() {
    let valid = frame(&serde_json::to_value(request()).unwrap());
    let mut extra = valid.clone();
    extra.push(0);
    let mut second_frame = valid;
    second_frame.extend(frame(&serde_json::to_value(request()).unwrap()));
    for input in [
        extra,
        second_frame,
        vec![0, 2, b'[', b']'],
        vec![0, 1, b'{'],
    ] {
        assert_eq!(
            LifecycleCodec
                .read_request(&StreamProtocol::new(PROTOCOL), &mut Cursor::new(input))
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

#[tokio::test]
async fn unknown_fields_are_rejected_at_every_object_level() {
    let original = serde_json::to_value(request()).unwrap();
    let mut root = original.clone();
    root["peer_id"] = "untrusted".into();
    let mut scope = original.clone();
    scope["scope"]["extra"] = true.into();
    let mut intent = original;
    intent["intent"]["extra"] = true.into();
    for value in [root, scope, intent] {
        assert_eq!(
            LifecycleCodec
                .read_request(
                    &StreamProtocol::new(PROTOCOL),
                    &mut Cursor::new(frame(&value))
                )
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    for reply in [
        serde_json::json!({"status":"wake_accepted","extra":true}),
        serde_json::json!({"status":"idle_accepted","responder_revision":0,"extra":true}),
        serde_json::json!({"status":"rejected","reason":"busy","extra":true}),
    ] {
        let mut value =
            serde_json::to_value(Response::for_request(&request(), Reply::WakeAccepted {}))
                .unwrap();
        value["reply"] = reply;
        assert!(
            LifecycleCodec
                .read_response(
                    &StreamProtocol::new(PROTOCOL),
                    &mut Cursor::new(frame(&value))
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn malformed_versions_intents_scopes_and_enum_values_are_rejected() {
    let mutations = [
        ("/version", serde_json::json!(2)),
        (
            "/intent/sender_epoch",
            serde_json::to_value([0_u8; 16]).unwrap(),
        ),
        (
            "/intent/receiver_epoch",
            serde_json::to_value([0_u8; 16]).unwrap(),
        ),
        ("/intent/revision", serde_json::json!(0)),
        ("/intent/revision", serde_json::json!(-1)),
        ("/intent/sender_epoch", serde_json::json!([1, 2])),
        ("/scope/network_name", serde_json::json!("")),
        (
            "/scope/network_name",
            serde_json::json!("x".repeat(MAX_NETWORK_NAME_BYTES + 1)),
        ),
        ("/scope/membership_tag", serde_json::json!("")),
        (
            "/scope/membership_tag",
            serde_json::json!("x".repeat(MAX_MEMBERSHIP_TAG_BYTES + 1)),
        ),
        ("/command", serde_json::json!("future_command")),
    ];
    for (pointer, replacement) in mutations {
        let mut value = serde_json::to_value(request()).unwrap();
        if pointer == "/scope/membership_tag" {
            value["scope"]["membership_tag"] = replacement;
        } else {
            *value.pointer_mut(pointer).unwrap() = replacement;
        }
        assert!(
            LifecycleCodec
                .read_request(
                    &StreamProtocol::new(PROTOCOL),
                    &mut Cursor::new(frame(&value))
                )
                .await
                .is_err(),
            "{pointer}"
        );
    }
}

#[tokio::test]
async fn invalid_outgoing_objects_write_no_bytes() {
    for value in [
        Request {
            version: 2,
            ..request()
        },
        Request {
            scope: Scope {
                network_name: "x".repeat(MAX_MESSAGE_BYTES * 2),
                membership_tag: None,
            },
            ..request()
        },
        Request {
            intent: Intent {
                revision: 0,
                ..request().intent
            },
            ..request()
        },
    ] {
        let mut bytes = Cursor::new(Vec::new());
        assert!(
            LifecycleCodec
                .write_request(&StreamProtocol::new(PROTOCOL), &mut bytes, value.clone())
                .await
                .is_err()
        );
        assert!(bytes.get_ref().is_empty());
        assert!(
            LifecycleCodec
                .write_response(
                    &StreamProtocol::new(PROTOCOL),
                    &mut bytes,
                    Response {
                        version: value.version,
                        scope: value.scope,
                        intent: value.intent,
                        reply: Reply::WakeAccepted {}
                    }
                )
                .await
                .is_err()
        );
        assert!(bytes.get_ref().is_empty());
    }
}

#[test]
fn scope_limits_are_utf8_bytes_not_character_counts() {
    assert!(supported_scope(&"\u{e9}".repeat(64), None));
    assert!(!supported_scope(&"\u{e9}".repeat(65), None));
    assert!(!supported_scope("lab", Some(&"\u{e9}".repeat(33))));
}

#[test]
fn current_transport_identity_and_authorization_are_both_required() {
    let peer = PeerId::random();
    let context = session(peer);
    assert_eq!(
        context.validate_request(
            PeerId::random(),
            |_| panic!("wrong peer must not reach lookup"),
            &request()
        ),
        Err(ValidationError::UnauthorizedPeer)
    );
    assert_eq!(
        context.validate_request(
            peer,
            |_| false,
            &Request {
                version: 2,
                ..request()
            }
        ),
        Err(ValidationError::UnauthorizedPeer)
    );
    let calls = Cell::new(0);
    let authorized = |actual| {
        assert_eq!(actual, peer);
        calls.set(calls.get() + 1);
        true
    };
    assert!(matches!(
        context.validate_request(peer, authorized, &request()),
        Ok(ValidatedRequest::Idle(_))
    ));
    assert_eq!(calls.get(), 1);
}

#[test]
fn wrong_network_and_membership_are_rejected_without_policy_input() {
    let peer = PeerId::random();
    let mut context = session(peer);
    let mut value = request();
    value.scope.network_name = "other".into();
    assert_eq!(
        context.validate_request(peer, |_| true, &value),
        Err(ValidationError::WrongNetwork)
    );
    value = request();
    value.scope.membership_tag = Some("unexpected".into());
    assert_eq!(
        context.validate_request(peer, |_| true, &value),
        Err(ValidationError::MembershipMismatch)
    );
    context.membership_tag = Some("current");
    assert_eq!(
        context.validate_request(peer, |_| true, &request()),
        Err(ValidationError::MembershipMismatch)
    );
    let previous = ["previous".to_owned()];
    context.previous_membership_tags = &previous;
    value.scope.membership_tag = Some("previous".into());
    assert!(context.validate_request(peer, |_| true, &value).is_ok());
}

#[test]
fn both_epoch_directions_are_checked_for_requests_and_responses() {
    let peer = PeerId::random();
    let context = session(peer);
    for intent in [
        Intent {
            sender_epoch: [3; 16],
            ..request().intent
        },
        Intent {
            receiver_epoch: [3; 16],
            ..request().intent
        },
        Intent {
            sender_epoch: [2; 16],
            receiver_epoch: [1; 16],
            ..request().intent
        },
    ] {
        assert_eq!(
            context.validate_request(
                peer,
                |_| true,
                &Request {
                    intent,
                    ..request()
                }
            ),
            Err(ValidationError::WrongEpoch)
        );
    }
    let sender = Session {
        local_epoch: epoch(1),
        remote_epoch: epoch(2),
        ..session(peer)
    };
    let reply = Response::for_request(
        &request(),
        Reply::IdleAccepted {
            responder_revision: 0,
        },
    );
    assert!(
        sender
            .validate_response(peer, |_| true, &request(), &reply)
            .is_ok()
    );
    let restarted = Session {
        local_epoch: epoch(4),
        ..sender
    };
    assert_eq!(
        restarted.validate_response(peer, |_| true, &request(), &reply),
        Err(ValidationError::WrongEpoch)
    );
}

#[test]
fn response_must_echo_scope_intent_and_command_kind_exactly() {
    let peer = PeerId::random();
    let context = Session {
        local_epoch: epoch(1),
        remote_epoch: epoch(2),
        ..session(peer)
    };
    let original = Response::for_request(
        &request(),
        Reply::IdleAccepted {
            responder_revision: 0,
        },
    );
    let mut changed_tag = original.clone();
    changed_tag.scope.membership_tag = Some("other".into());
    let mut changed_network = original.clone();
    changed_network.scope.network_name = "other".into();
    let mut changed_revision = original.clone();
    changed_revision.intent.revision += 1;
    let mut changed_epoch = original.clone();
    changed_epoch.intent.receiver_epoch = [3; 16];
    let mut changed_kind = original;
    changed_kind.reply = Reply::WakeAccepted {};
    for value in [
        changed_tag,
        changed_network,
        changed_revision,
        changed_epoch,
        changed_kind,
    ] {
        assert_eq!(
            context.validate_response(peer, |_| true, &request(), &value),
            Err(ValidationError::ResponseMismatch)
        );
    }
    let wake = Request {
        command: Command::Wake,
        ..request()
    };
    assert_eq!(
        context.validate_response(
            peer,
            |_| true,
            &wake,
            &Response::for_request(
                &wake,
                Reply::IdleAccepted {
                    responder_revision: 0
                }
            )
        ),
        Err(ValidationError::ResponseMismatch)
    );
}

#[test]
fn revocation_while_waiting_prevents_an_idle_ack_from_reaching_the_model() {
    let peer = PeerId::random();
    let context = Session {
        local_epoch: epoch(1),
        remote_epoch: epoch(2),
        ..session(peer)
    };
    let reply = Response::for_request(
        &request(),
        Reply::IdleAccepted {
            responder_revision: 0,
        },
    );
    assert_eq!(
        context.validate_response(peer, |_| false, &request(), &reply),
        Err(ValidationError::UnauthorizedPeer)
    );
}

#[test]
fn wake_and_rejection_responses_are_scoped_not_delivery_confirmations() {
    let peer = PeerId::random();
    let context = Session {
        local_epoch: epoch(1),
        remote_epoch: epoch(2),
        ..session(peer)
    };
    let value = Request {
        command: Command::Wake,
        ..request()
    };
    assert_eq!(
        context.validate_response(
            peer,
            |_| true,
            &value,
            &Response::for_request(&value, Reply::WakeAccepted {})
        ),
        Ok(ValidatedResponse::WakeAccepted)
    );
    assert_eq!(
        context.validate_response(
            peer,
            |_| true,
            &value,
            &Response::for_request(
                &value,
                Reply::Rejected {
                    reason: RejectCode::Busy
                }
            )
        ),
        Ok(ValidatedResponse::Rejected(RejectCode::Busy))
    );
    assert!(matches!(
        session(peer).validate_request(peer, |_| true, &value),
        Ok(ValidatedRequest::Wake(_))
    ));
}

#[test]
fn idle_response_constructor_cannot_acknowledge_another_offer() {
    let value = request();
    let offer = value.intent.offer().unwrap();
    let ack = IdleAck {
        offer,
        responder_revision: 0,
    };
    assert!(Response::idle_accepted(&value, ack).is_ok());
    assert_eq!(
        Response::idle_accepted(
            &Request {
                command: Command::Wake,
                ..value.clone()
            },
            ack
        ),
        Err(ValidationError::ResponseMismatch)
    );
    assert_eq!(
        Response::idle_accepted(
            &value,
            IdleAck {
                offer: IdleOffer {
                    revision: 2,
                    ..offer
                },
                ..ack
            }
        ),
        Err(ValidationError::ResponseMismatch)
    );
}

#[test]
fn capability_defaults_remain_legacy_and_leave_old_serialization_unchanged() {
    let value = ControlCapabilities::local("lab", None, 1280);
    let json = serde_json::to_value(&value).unwrap();
    assert!(json.get("supports_demand_lifecycle").is_none());
    assert!(json.get("demand_lifecycle_epoch").is_none());
    let old: ControlCapabilities = serde_json::from_value(json).unwrap();
    assert_eq!(old.demand_lifecycle_epoch(), None);
    let upgraded = value.with_demand_lifecycle_epoch(epoch(1));
    assert_eq!(
        serde_json::from_slice::<ControlCapabilities>(&serde_json::to_vec(&upgraded).unwrap())
            .unwrap()
            .demand_lifecycle_epoch(),
        Some(epoch(1))
    );
}

#[test]
fn contradictory_optional_capabilities_never_enable_quiet() {
    let mut value = ControlCapabilities::local("lab", None, 1280);
    value.supports_demand_lifecycle = true;
    assert_eq!(value.demand_lifecycle_epoch(), None);
    value.demand_lifecycle_epoch = Some([0; 16]);
    assert_eq!(value.demand_lifecycle_epoch(), None);
    value.demand_lifecycle_epoch = Some([1; 16]);
    value.supports_demand_lifecycle = false;
    assert_eq!(value.demand_lifecycle_epoch(), None);
    value = value.with_demand_lifecycle_epoch(epoch(1));
    let peer = crate::PeerId::from_bytes([1; 32]);
    let mut peers = PeerCapabilities::default();
    peers.record(peer, value);
    assert_eq!(peers.demand_lifecycle_epoch_for(peer), Some(epoch(1)));
    peers.remove(peer);
    assert_eq!(peers.demand_lifecycle_epoch_for(peer), None);
}

#[test]
fn unrepresentable_optional_scopes_stay_legacy_without_breaking_base_capabilities() {
    let value = ControlCapabilities::local(&"x".repeat(MAX_NETWORK_NAME_BYTES + 1), None, 1280)
        .with_demand_lifecycle_epoch(epoch(1));
    assert!(!value.supports_demand_lifecycle);
    assert!(value.demand_lifecycle_epoch.is_none());
    assert_eq!(
        crate::runtime::control::validate_capabilities(&value, &value.network_name, None, &[]),
        None
    );
}

fn swarm() -> Swarm<request_response::Behaviour<LifecycleCodec>> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            libp2p::tcp::Config::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .unwrap()
        .with_quic()
        .with_behaviour(|_| behaviour(4))
        .unwrap()
        .build()
}

async fn live_exchange(listen: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut a = swarm();
        let mut b = swarm();
        let a_peer = *a.local_peer_id();
        let b_peer = *b.local_peer_id();
        b.listen_on(listen.parse().unwrap()).unwrap();
        let address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = b.select_next_some().await { break address; }
        };
        a.dial(address).unwrap();
        let mut connected_a = false;
        let mut connected_b = false;
        while !connected_a || !connected_b {
            tokio::select! {
                event = a.select_next_some() => {
                    if let SwarmEvent::ConnectionEstablished { peer_id, .. } = event {
                        assert_eq!(peer_id, b_peer);
                        connected_a = true;
                    }
                }
                event = b.select_next_some() => {
                    if let SwarmEvent::ConnectionEstablished { peer_id, .. } = event {
                        assert_eq!(peer_id, a_peer);
                        connected_b = true;
                    }
                }
            }
        }
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let mut a_policy = PeerLifecycle::new(epoch(1), epoch(2), true, now);
        let mut b_policy = PeerLifecycle::new(epoch(2), epoch(1), true, now);
        let ready = Readiness { incoming_path_available: true, outstanding_work: false };
        let value = Request::idle(scope(), a_policy.offer_idle(idle, ready).unwrap());
        let request_id = a.behaviour_mut().send_request(&b_peer, value.clone());
        let mut pending_reply = None;
        let mut client_done = false;
        let mut server_done = false;
        while !client_done || !server_done {
            tokio::select! {
                event = b.select_next_some() => match event {
                    SwarmEvent::Behaviour(request_response::Event::Message { peer, message: request_response::Message::Request { request, channel, request_id }, .. }) => {
                        assert_eq!(peer, a_peer);
                        let context = session(a_peer);
                        let ValidatedRequest::Idle(offer) = context.validate_request(peer, |p| p == a_peer, &request).unwrap() else { panic!("expected idle"); };
                        let ack = b_policy.receive_idle(offer, idle, ready).unwrap();
                        assert_ne!(b_policy.phase(), Phase::Dormant);
                        b.behaviour_mut().send_response(channel, Response::idle_accepted(&request, ack).unwrap()).unwrap();
                        pending_reply = Some((request_id, ack));
                    }
                    SwarmEvent::Behaviour(request_response::Event::ResponseSent { peer, request_id, .. }) => {
                        assert_eq!(peer, a_peer);
                        let (expected, ack) = pending_reply.take().unwrap();
                        assert_eq!(expected, request_id);
                        b_policy.idle_reply_sent(ack, idle, ready).unwrap();
                        server_done = true;
                    }
                    SwarmEvent::Behaviour(request_response::Event::InboundFailure { error, .. }) => panic!("inbound failure: {error}"),
                    _ => {}
                },
                event = a.select_next_some() => match event {
                    SwarmEvent::Behaviour(request_response::Event::Message { peer, message: request_response::Message::Response { response, request_id: response_id }, .. }) => {
                        assert_eq!(peer, b_peer);
                        assert_eq!(request_id, response_id);
                        let context = Session { local_epoch: epoch(1), remote_epoch: epoch(2), ..session(b_peer) };
                        let ValidatedResponse::IdleAccepted(ack) = context.validate_response(peer, |p| p == b_peer, &value, &response).unwrap() else { panic!("expected idle ack"); };
                        a_policy.receive_idle_ack(ack, idle, ready).unwrap();
                        client_done = true;
                    }
                    SwarmEvent::Behaviour(request_response::Event::OutboundFailure { error, .. }) => panic!("outbound failure: {error}"),
                    _ => {}
                },
            }
        }
        assert_eq!(a_policy.phase(), Phase::Dormant);
        assert_eq!(b_policy.phase(), Phase::Dormant);
        assert_eq!(a_policy.last_demand(), None);
        assert_eq!(b_policy.last_demand(), None);

        // Reverse the initiator to exercise incoming demand, not only local wake.
        let active = idle + Duration::from_secs(1);
        let value = Request::wake(scope(), b_policy.wake(active, true).unwrap());
        let wake_id = b.behaviour_mut().send_request(&a_peer, value.clone());
        let mut client_done = false;
        let mut server_done = false;
        while !client_done || !server_done {
            tokio::select! {
                event = a.select_next_some() => match event {
                    SwarmEvent::Behaviour(request_response::Event::Message { peer, message: request_response::Message::Request { request, channel, .. }, .. }) => {
                        let context = Session { local_epoch: epoch(1), remote_epoch: epoch(2), ..session(b_peer) };
                        let ValidatedRequest::Wake(wake) = context.validate_request(peer, |p| p == b_peer, &request).unwrap() else { panic!("expected wake"); };
                        a_policy.receive_wake(wake, active, true).unwrap();
                        a.behaviour_mut().send_response(channel, Response::for_request(&request, Reply::WakeAccepted {})).unwrap();
                    }
                    SwarmEvent::Behaviour(request_response::Event::ResponseSent { peer, .. }) => {
                        assert_eq!(peer, b_peer);
                        server_done = true;
                    }
                    SwarmEvent::Behaviour(request_response::Event::InboundFailure { error, .. }) => panic!("wake inbound failure: {error}"),
                    _ => {}
                },
                event = b.select_next_some() => match event {
                    SwarmEvent::Behaviour(request_response::Event::Message { peer, message: request_response::Message::Response { response, request_id }, .. }) => {
                        assert_eq!(request_id, wake_id);
                        assert_eq!(session(a_peer).validate_response(peer, |p| p == a_peer, &value, &response).unwrap(), ValidatedResponse::WakeAccepted);
                        client_done = true;
                    }
                    SwarmEvent::Behaviour(request_response::Event::OutboundFailure { error, .. }) => panic!("wake outbound failure: {error}"),
                    _ => {}
                },
            }
        }
        assert_eq!(a_policy.phase(), Phase::Active);
        assert_eq!(b_policy.phase(), Phase::Active);
        assert_eq!(a_policy.last_demand(), Some(active));
        assert_eq!(b_policy.last_demand(), Some(active));
    }).await.expect("bounded lifecycle loopback exchange");
}

#[tokio::test]
async fn authenticated_tcp_exchange_commits_only_after_real_reply_delivery() {
    live_exchange("/ip4/127.0.0.1/tcp/0").await;
}

#[tokio::test]
async fn authenticated_quic_exchange_commits_only_after_real_reply_delivery() {
    live_exchange("/ip4/127.0.0.1/udp/0/quic-v1").await;
}

#[derive(libp2p::swarm::NetworkBehaviour)]
struct MixedBehaviour {
    lifecycle: request_response::Behaviour<LifecycleCodec>,
    control: request_response::Behaviour<ControlCodec>,
}

#[tokio::test]
async fn unsupported_optional_protocol_keeps_existing_control_usable() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut client = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .unwrap()
            .with_behaviour(|_| MixedBehaviour {
                lifecycle: behaviour(4),
                control: control::behaviour(4),
            })
            .unwrap()
            .build();
        // This endpoint intentionally has no lifecycle protocol handler.
        let mut server = SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .unwrap()
            .with_behaviour(|_| control::behaviour(4))
            .unwrap()
            .build();
        let client_peer = *client.local_peer_id();
        let server_peer = *server.local_peer_id();
        server.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
        let address = loop {
            if let SwarmEvent::NewListenAddr { address, .. } = server.select_next_some().await { break address; }
        };
        client.dial(address).unwrap();
        let mut client_connected = false;
        let mut server_connected = false;
        while !client_connected || !server_connected {
            tokio::select! {
                event = client.select_next_some() => if let SwarmEvent::ConnectionEstablished { peer_id, .. } = event {
                    assert_eq!(peer_id, server_peer);
                    client_connected = true;
                },
                event = server.select_next_some() => if let SwarmEvent::ConnectionEstablished { peer_id, .. } = event {
                    assert_eq!(peer_id, client_peer);
                    server_connected = true;
                },
            }
        }
        let now = Instant::now();
        let idle = now + IDLE_AFTER;
        let ready = Readiness { incoming_path_available: true, outstanding_work: false };
        let mut policy = PeerLifecycle::new(epoch(1), epoch(2), true, now);
        let value = Request::idle(scope(), policy.offer_idle(idle, ready).unwrap());
        let unsupported_id = client.behaviour_mut().lifecycle.send_request(&server_peer, value);
        loop {
            tokio::select! {
                event = client.select_next_some() => if let SwarmEvent::Behaviour(MixedBehaviourEvent::Lifecycle(request_response::Event::OutboundFailure { peer, request_id, error, .. })) = event {
                    assert_eq!(peer, server_peer);
                    assert_eq!(request_id, unsupported_id);
                    assert!(matches!(error, request_response::OutboundFailure::UnsupportedProtocols));
                    policy.use_legacy();
                    break;
                },
                _ = server.select_next_some() => {},
            }
        }
        assert_eq!(policy.phase(), Phase::Legacy);
        for tick in 1..=10 {
            assert_eq!(policy.offer_idle(idle + Duration::from_secs(tick), ready), Err(Rejection::Unsupported));
        }
        let capabilities = ControlCapabilities::local("lab", None, 1280).with_demand_lifecycle_epoch(epoch(1));
        let control_id = client.behaviour_mut().control.send_request(&server_peer, ControlRequest::Capabilities(capabilities.clone()));
        loop {
            tokio::select! {
                event = server.select_next_some() => if let SwarmEvent::Behaviour(request_response::Event::Message { peer, message: request_response::Message::Request { request: ControlRequest::Capabilities(received), channel, .. }, .. }) = event {
                    assert_eq!(peer, client_peer);
                    assert_eq!(received, capabilities);
                    server.behaviour_mut().send_response(channel, ControlResponse::CapabilitiesAccepted(ControlCapabilities::local("lab", None, 1280))).unwrap();
                },
                event = client.select_next_some() => match event {
                    SwarmEvent::Behaviour(MixedBehaviourEvent::Control(request_response::Event::Message { peer, message: request_response::Message::Response { request_id, response: ControlResponse::CapabilitiesAccepted(received) }, .. })) => {
                        assert_eq!(peer, server_peer);
                        assert_eq!(request_id, control_id);
                        assert_eq!(received.demand_lifecycle_epoch(), None);
                        assert_eq!(control::validate_capabilities(&received, "lab", None, &[]), None);
                        break;
                    }
                    SwarmEvent::Behaviour(MixedBehaviourEvent::Control(request_response::Event::OutboundFailure { error, .. })) => panic!("legacy control failed: {error}"),
                    SwarmEvent::Behaviour(MixedBehaviourEvent::Lifecycle(request_response::Event::OutboundFailure { error, .. })) => panic!("unexpected lifecycle retry: {error}"),
                    _ => {}
                },
            }
        }
    }).await.expect("bounded mixed-version control exchange");
}
