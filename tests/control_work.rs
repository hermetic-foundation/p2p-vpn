//! Inspect native request ownership without replacing libp2p's request engine.
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::Context,
    time::Duration,
};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncWrite, StreamExt};
use libp2p::{
    PeerId, StreamProtocol, Swarm, SwarmBuilder, noise, request_response,
    swarm::{DialError, FromSwarm, NetworkBehaviour, SwarmEvent, ToSwarm, behaviour::DialFailure},
    tcp, yamux,
};
use p2p_vpn::{
    runtime::packet::{PacketCodec, PacketResponse},
    wire::Frame,
};

#[derive(NetworkBehaviour)]
struct Behaviour {
    rpc: request_response::Behaviour<GateCodec>,
}

struct Gate {
    entered: AtomicBool,
    release: tokio::sync::Semaphore,
}

#[derive(Clone)]
struct GateCodec {
    inner: PacketCodec,
    gate: Option<Arc<Gate>>,
}

#[async_trait]
impl request_response::Codec for GateCodec {
    type Protocol = StreamProtocol;
    type Request = Frame;
    type Response = PacketResponse;

    async fn read_request<T>(&mut self, protocol: &StreamProtocol, io: &mut T) -> io::Result<Frame>
    where
        T: AsyncRead + Unpin + Send,
    {
        if let Some(gate) = &self.gate {
            gate.entered.store(true, Ordering::Release);
            let _permit = gate
                .release
                .acquire()
                .await
                .map_err(|_| io::ErrorKind::BrokenPipe)?;
        }
        self.inner.read_request(protocol, io).await
    }
    async fn read_response<T>(
        &mut self,
        protocol: &StreamProtocol,
        io: &mut T,
    ) -> io::Result<PacketResponse>
    where
        T: AsyncRead + Unpin + Send,
    {
        self.inner.read_response(protocol, io).await
    }
    async fn write_request<T>(
        &mut self,
        protocol: &StreamProtocol,
        io: &mut T,
        request: Frame,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        self.inner.write_request(protocol, io, request).await
    }
    async fn write_response<T>(
        &mut self,
        protocol: &StreamProtocol,
        io: &mut T,
        response: PacketResponse,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        self.inner.write_response(protocol, io, response).await
    }
}

fn rpc(timeout: Duration) -> request_response::Behaviour<GateCodec> {
    rpc_with_gate(timeout, None)
}

fn rpc_with_gate(
    timeout: Duration,
    gate: Option<Arc<Gate>>,
) -> request_response::Behaviour<GateCodec> {
    request_response::Behaviour::with_codec(
        GateCodec {
            inner: PacketCodec::new(1280),
            gate,
        },
        [(
            StreamProtocol::new("/p2p-vpn/control-work-test/1"),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(timeout)
            .with_max_concurrent_streams(8),
    )
}

fn frame(sequence: u64) -> Frame {
    Frame::packet(1, sequence, vec![0x45; 20]).unwrap()
}

fn host(timeout: Duration, gate: Option<Arc<Gate>>) -> Swarm<Behaviour> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|_| Behaviour {
            rpc: rpc_with_gate(timeout, gate),
        })
        .unwrap()
        .with_swarm_config(|config| config.with_idle_connection_timeout(Duration::from_secs(30)))
        .build()
}

async fn pair(timeout: Duration) -> (Swarm<Behaviour>, Swarm<Behaviour>) {
    pair_with_gate(timeout, None).await
}

async fn pair_with_gate(
    timeout: Duration,
    gate: Option<Arc<Gate>>,
) -> (Swarm<Behaviour>, Swarm<Behaviour>) {
    let mut a = host(timeout, None);
    let mut b = host(timeout, gate);
    b.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    let address = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = b.select_next_some().await {
                break address;
            }
        }
    })
    .await
    .unwrap();
    a.dial(address).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !a.is_connected(b.local_peer_id()) || !b.is_connected(a.local_peer_id()) {
            tokio::select! {_ = a.select_next_some() => {}, _ = b.select_next_some() => {}}
        }
    })
    .await
    .unwrap();
    (a, b)
}

#[test]
fn disconnected_queue_and_undelivered_failure_remain_busy() {
    let peer = PeerId::random();
    let unrelated = PeerId::random();
    let mut wire = rpc(Duration::from_secs(5));
    assert!(!wire.has_pending_requests(&peer));
    let request = wire.send_request(&peer, frame(1));
    for _ in 0..1000 {
        assert!(wire.has_pending_requests(&peer));
        assert!(!wire.has_pending_requests(&unrelated));
        assert!(wire.is_pending_outbound(&peer, &request));
    }
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    let std::task::Poll::Ready(ToSwarm::Dial { opts }) = NetworkBehaviour::poll(&mut wire, &mut cx)
    else {
        panic!("expected one dial");
    };
    NetworkBehaviour::on_swarm_event(
        &mut wire,
        FromSwarm::DialFailure(DialFailure {
            peer_id: Some(peer),
            connection_id: opts.connection_id(),
            error: &DialError::NoAddresses,
        }),
    );
    assert!(!wire.is_pending_outbound(&peer, &request));
    assert!(
        wire.has_pending_requests(&peer),
        "terminal notification has not been delivered"
    );
    assert!(matches!(NetworkBehaviour::poll(&mut wire, &mut cx),
        std::task::Poll::Ready(ToSwarm::GenerateEvent(request_response::Event::OutboundFailure {peer:owner, request_id, ..}))
        if owner == peer && request_id == request));
    assert!(!wire.has_pending_requests(&peer));
    assert!(NetworkBehaviour::poll(&mut wire, &mut cx).is_pending());
}

#[tokio::test]
async fn undecoded_inbound_codec_work_prevents_false_idleness() {
    let gate = Arc::new(Gate {
        entered: AtomicBool::new(false),
        release: tokio::sync::Semaphore::new(0),
    });
    let (mut a, mut b) = pair_with_gate(Duration::from_secs(5), Some(Arc::clone(&gate))).await;
    let peer_a = *a.local_peer_id();
    let peer_b = *b.local_peer_id();
    a.behaviour_mut().rpc.send_request(&peer_b, frame(1));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !gate.entered.load(Ordering::Acquire) {
            tokio::select! {
                _ = a.select_next_some() => {},
                _ = b.select_next_some() => {},
                () = tokio::time::sleep(Duration::from_millis(1)) => {},
            }
        }
    })
    .await
    .unwrap();
    assert!(
        b.behaviour().rpc.has_pending_requests(&peer_a),
        "no request has been decoded yet"
    );
    gate.release.add_permits(1);
    let (mut sent, mut answered) = (false, false);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !sent || !answered {
            tokio::select! {
                event = a.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::Message {message:request_response::Message::Response {..}, ..})) = event {answered = true;}
                }
                event = b.select_next_some() => match event {
                    SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::Message {message:request_response::Message::Request {channel, ..}, ..})) => {
                        b.behaviour_mut().rpc.send_response(channel, PacketResponse::Accepted).unwrap();
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::ResponseSent {..})) => {sent = true;}
                    _ => {}
                }
            }
        }
    }).await.unwrap();
    assert!(!b.behaviour().rpc.has_pending_requests(&peer_a));
    assert!(!a.behaviour().rpc.has_pending_requests(&peer_b));
}

#[tokio::test]
async fn concurrent_requests_and_queued_replies_drain_without_false_idleness() {
    let (mut a, mut b) = pair(Duration::from_secs(5)).await;
    let peer_a = *a.local_peer_id();
    let peer_b = *b.local_peer_id();
    let unrelated = PeerId::random();
    let mut requests = Vec::new();
    for sequence in 1..=3 {
        a.behaviour_mut().rpc.send_request(&peer_b, frame(sequence));
    }
    assert!(a.behaviour().rpc.has_pending_requests(&peer_b));
    tokio::time::timeout(Duration::from_secs(5), async {
        while requests.len() < 3 {
            tokio::select! {
                _ = a.select_next_some() => {},
                event = b.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::Message {peer, message:request_response::Message::Request {request_id, channel, ..}, ..})) = event {
                        assert_eq!(peer, peer_a);
                        requests.push((request_id, channel));
                    }
                }
            }
        }
    }).await.unwrap();
    while let Some((inbound, channel)) = requests.pop() {
        assert!(b.behaviour().rpc.has_pending_requests(&peer_a));
        assert!(!b.behaviour().rpc.has_pending_requests(&unrelated));
        b.behaviour_mut()
            .rpc
            .send_response(channel, PacketResponse::Accepted)
            .unwrap();
        assert!(
            b.behaviour().rpc.has_pending_requests(&peer_a),
            "reply is only queued"
        );
        let (mut sent, mut answered) = (false, false);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !sent || !answered {
                tokio::select! {
                    event = a.select_next_some() => {
                        if let SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::Message {peer, message:request_response::Message::Response {response, ..}, ..})) = event {
                            assert_eq!(peer, peer_b);
                            assert_eq!(response, PacketResponse::Accepted);
                            answered = true;
                        }
                    }
                    event = b.select_next_some() => {
                        if let SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::ResponseSent {peer, request_id, ..})) = event {
                            assert_eq!(peer, peer_a);
                            assert_eq!(request_id, inbound);
                            sent = true;
                        }
                    }
                }
            }
        }).await.unwrap();
        assert_eq!(
            a.behaviour().rpc.has_pending_requests(&peer_b),
            !requests.is_empty()
        );
        assert_eq!(
            b.behaviour().rpc.has_pending_requests(&peer_a),
            !requests.is_empty()
        );
    }
}

#[tokio::test]
async fn omitted_response_releases_work_only_after_failure_delivery() {
    failed_reply(false).await;
}

#[tokio::test]
async fn expired_response_releases_work_only_after_timeout_delivery() {
    failed_reply(true).await;
}

async fn failed_reply(timeout: bool) {
    let (mut a, mut b) = pair(Duration::from_millis(500)).await;
    let peer_a = *a.local_peer_id();
    let peer_b = *b.local_peer_id();
    a.behaviour_mut().rpc.send_request(&peer_b, frame(1));
    let channel = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                _ = a.select_next_some() => {},
                event = b.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::Message {peer, message:request_response::Message::Request {channel, ..}, ..})) = event {
                        assert_eq!(peer, peer_a);
                        break channel;
                    }
                }
            }
        }
    }).await.unwrap();
    assert!(b.behaviour().rpc.has_pending_requests(&peer_a));
    let held = if timeout {
        Some(channel)
    } else {
        drop(channel);
        None
    };
    assert!(b.behaviour().rpc.has_pending_requests(&peer_a));
    let (mut outbound_failed, mut inbound_failed) = (false, false);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !outbound_failed || !inbound_failed {
            tokio::select! {
                event = a.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::OutboundFailure {peer, ..})) = event {
                        assert_eq!(peer, peer_b);
                        outbound_failed = true;
                    }
                }
                event = b.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::InboundFailure {peer, error, ..})) = event {
                        assert_eq!(peer, peer_a);
                        if timeout {assert!(matches!(error, request_response::InboundFailure::Timeout));}
                        else {assert!(matches!(error, request_response::InboundFailure::ResponseOmission));}
                        inbound_failed = true;
                    }
                }
            }
        }
    }).await.unwrap();
    drop(held);
    assert!(!a.behaviour().rpc.has_pending_requests(&peer_b));
    assert!(!b.behaviour().rpc.has_pending_requests(&peer_a));
}
