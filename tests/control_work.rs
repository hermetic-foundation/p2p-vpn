//! Inspect native request ownership without replacing libp2p's request engine.
//! Codec counters are test instrumentation, not energy measurements.
use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
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
    write_probe: Option<Arc<WriteProbe>>,
}

#[derive(Default)]
struct WriteProbe {
    entered: AtomicUsize,
    active: AtomicUsize,
    bytes: AtomicUsize,
    blocked: AtomicBool,
    pending: AtomicBool,
    waker: futures::task::AtomicWaker,
}

impl WriteProbe {
    fn release(&self) {
        self.blocked.store(false, Ordering::Release);
        self.waker.wake();
    }
}

struct ProbedWrite<'a, T> {
    io: &'a mut T,
    probe: Arc<WriteProbe>,
}

impl<T> Drop for ProbedWrite<'_, T> {
    fn drop(&mut self) {
        self.probe.active.fetch_sub(1, Ordering::AcqRel);
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for ProbedWrite<'_, T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.probe.waker.register(cx.waker());
        if self.probe.blocked.load(Ordering::Acquire) {
            self.probe.pending.store(true, Ordering::Release);
            return Poll::Pending;
        }
        let result = Pin::new(&mut *self.io).poll_write(cx, bytes);
        if let Poll::Ready(Ok(written)) = &result {
            self.probe.bytes.fetch_add(*written, Ordering::AcqRel);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.io).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut *self.io).poll_close(cx)
    }
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
        if let Some(probe) = &self.write_probe {
            probe.entered.fetch_add(1, Ordering::AcqRel);
            probe.active.fetch_add(1, Ordering::AcqRel);
            let mut io = ProbedWrite {
                io,
                probe: Arc::clone(probe),
            };
            return self.inner.write_request(protocol, &mut io, request).await;
        }
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
    rpc_with_probes(timeout, None, None)
}

fn rpc_with_probes(
    timeout: Duration,
    gate: Option<Arc<Gate>>,
    write_probe: Option<Arc<WriteProbe>>,
) -> request_response::Behaviour<GateCodec> {
    request_response::Behaviour::with_codec(
        GateCodec {
            inner: PacketCodec::new(1280),
            gate,
            write_probe,
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
    host_with_probes(timeout, gate, None)
}

fn host_with_probes(
    timeout: Duration,
    gate: Option<Arc<Gate>>,
    write_probe: Option<Arc<WriteProbe>>,
) -> Swarm<Behaviour> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|_| Behaviour {
            rpc: rpc_with_probes(timeout, gate, write_probe),
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
    connect_hosts(host(timeout, None), host(timeout, gate)).await
}

async fn connect_hosts(
    mut a: Swarm<Behaviour>,
    mut b: Swarm<Behaviour>,
) -> (Swarm<Behaviour>, Swarm<Behaviour>) {
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
    assert!(!wire.has_any_pending_requests());
    assert!(!wire.has_pending_connection_requests());
    let request = wire.send_request(&peer, frame(1));
    for _ in 0..1000 {
        assert!(wire.has_pending_requests(&peer));
        assert!(wire.has_any_pending_requests());
        assert!(wire.has_pending_connection_requests());
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
    assert!(!wire.has_pending_connection_requests());
    assert!(wire.has_any_pending_requests());
    assert!(
        wire.has_pending_requests(&peer),
        "terminal notification has not been delivered"
    );
    assert!(matches!(NetworkBehaviour::poll(&mut wire, &mut cx),
        std::task::Poll::Ready(ToSwarm::GenerateEvent(request_response::Event::OutboundFailure {peer:owner, request_id, ..}))
        if owner == peer && request_id == request));
    assert!(!wire.has_pending_requests(&peer));
    assert!(!wire.has_any_pending_requests());
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
    assert!(b.behaviour().rpc.has_any_pending_requests());
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
    assert!(!a.behaviour().rpc.has_pending_connection_requests());
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
        assert!(b.behaviour().rpc.has_any_pending_requests());
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
    assert!(!a.behaviour().rpc.has_any_pending_requests());
    assert!(!b.behaviour().rpc.has_any_pending_requests());
}

#[tokio::test]
async fn expired_deadline_before_late_tcp_connection_never_invokes_codec() {
    for already_expired in [false, true] {
        let probe = Arc::new(WriteProbe::default());
        let inbound_gate = Arc::new(Gate {
            entered: AtomicBool::new(false),
            release: tokio::sync::Semaphore::new(0),
        });
        let mut a = host_with_probes(Duration::from_secs(5), None, Some(Arc::clone(&probe)));
        let mut b = host(Duration::from_secs(5), Some(Arc::clone(&inbound_gate)));
        let peer = *b.local_peer_id();
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
        a.add_peer_address(peer, address);
        let deadline = if already_expired {
            Instant::now() - Duration::from_secs(1)
        } else {
            Instant::now() + Duration::from_millis(50)
        };
        let request = a
            .behaviour_mut()
            .rpc
            .send_request_with_deadline(&peer, frame(1), deadline);
        assert!(a.behaviour().rpc.has_pending_connection_requests());
        tokio::time::sleep(
            deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20),
        )
        .await;
        assert!(a.behaviour().rpc.is_pending_outbound(&peer, &request));
        assert!(a.behaviour().rpc.has_pending_requests(&peer));
        expect_outbound_timeout(&mut a, &mut b, request).await;
        assert!(
            a.is_connected(&peer),
            "expiry is delivered after a real TCP dial"
        );

        let settle = tokio::time::sleep(Duration::from_millis(100));
        tokio::pin!(settle);
        loop {
            tokio::select! {
                () = &mut settle => break,
                _ = a.select_next_some() => {},
                _ = b.select_next_some() => {},
            }
        }
        assert_eq!(probe.entered.load(Ordering::Acquire), 0);
        assert_eq!(probe.bytes.load(Ordering::Acquire), 0);
        assert!(
            !inbound_gate.entered.load(Ordering::Acquire),
            "no substream requested"
        );
    }
}

#[tokio::test]
async fn deadline_wakes_pending_tcp_write_and_releases_native_owner() {
    pending_write_deadline(Duration::from_secs(5), Duration::from_millis(700), false).await;
}

#[tokio::test]
async fn late_pending_write_wakeup_cannot_write_expired_body() {
    pending_write_deadline(Duration::from_secs(5), Duration::from_millis(700), true).await;
}

#[tokio::test]
async fn deadline_does_not_extend_shorter_native_write_timeout() {
    pending_write_deadline(Duration::from_millis(500), Duration::from_secs(5), false).await;
}

async fn pending_write_deadline(timeout: Duration, lifetime: Duration, late_wakeup: bool) {
    let probe = Arc::new(WriteProbe::default());
    probe.blocked.store(true, Ordering::Release);
    let (mut a, mut b) = connect_hosts(
        host_with_probes(timeout, None, Some(Arc::clone(&probe))),
        host(Duration::from_secs(5), None),
    )
    .await;
    let peer = *b.local_peer_id();
    let deadline = Instant::now() + lifetime;
    let request = a
        .behaviour_mut()
        .rpc
        .send_request_with_deadline(&peer, frame(1), deadline);
    wait_for_pending_write(&mut a, &mut b, &probe).await;
    assert!(a.behaviour().rpc.has_pending_requests(&peer));
    assert!(a.behaviour().rpc.is_pending_outbound(&peer, &request));
    assert_eq!(probe.active.load(Ordering::Acquire), 1);
    if late_wakeup {
        // Stop polling the swarms, then make the write ready only after expiry.
        tokio::time::sleep(
            deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(20),
        )
        .await;
        probe.release();
        assert!(a.behaviour().rpc.has_pending_requests(&peer));
    }
    // No harness tick or gate wakeup: only the native operation timer can finish this write.
    expect_outbound_timeout(&mut a, &mut b, request).await;
    if timeout < lifetime {
        assert!(Instant::now() < deadline, "native timeout must still win");
    } else {
        assert!(Instant::now() >= deadline);
    }
    assert_eq!(probe.entered.load(Ordering::Acquire), 1);
    assert_eq!(
        probe.active.load(Ordering::Acquire),
        0,
        "write future dropped"
    );
    assert_eq!(probe.bytes.load(Ordering::Acquire), 0);

    probe.release();
    let normal = a.behaviour_mut().rpc.send_request(&peer, frame(2));
    expect_round_trip(&mut a, &mut b, normal).await;
    assert_eq!(
        probe.entered.load(Ordering::Acquire),
        2,
        "old request cannot restart"
    );
    assert!(probe.bytes.load(Ordering::Acquire) > 0);
}

async fn wait_for_pending_write(
    a: &mut Swarm<Behaviour>,
    b: &mut Swarm<Behaviour>,
    probe: &WriteProbe,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !probe.pending.load(Ordering::Acquire) {
            tokio::select! {
                event = a.select_next_some() => {
                    assert!(!matches!(event, SwarmEvent::Behaviour(BehaviourEvent::Rpc(
                        request_response::Event::OutboundFailure { .. }
                    ))), "write failed before reaching Pending: {event:?}");
                }
                _ = b.select_next_some() => {},
                () = tokio::time::sleep(Duration::from_millis(1)) => {},
            }
        }
    })
    .await
    .unwrap();
}

async fn expect_outbound_timeout(
    a: &mut Swarm<Behaviour>,
    b: &mut Swarm<Behaviour>,
    expected: request_response::OutboundRequestId,
) {
    expect_outbound_failure(a, b, expected, false).await;
}

async fn expect_outbound_failure(
    a: &mut Swarm<Behaviour>,
    b: &mut Swarm<Behaviour>,
    expected: request_response::OutboundRequestId,
    cancelled: bool,
) {
    let peer = *b.local_peer_id();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            assert!(a.behaviour().rpc.has_pending_requests(&peer));
            tokio::select! {
                event = a.select_next_some() => match event {
                    SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::OutboundFailure {
                        peer: owner, request_id, error, ..
                    })) => {
                        assert_eq!(owner, peer);
                        assert_eq!(request_id, expected);
                        if cancelled {
                            assert!(matches!(error, request_response::OutboundFailure::Io(ref io)
                                if io.kind() == io::ErrorKind::Interrupted), "{error:?}");
                        } else {
                            assert!(matches!(error, request_response::OutboundFailure::Timeout), "{error:?}");
                        }
                        break;
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::Message { .. })) => {
                        panic!("expired request unexpectedly succeeded");
                    }
                    _ => {}
                },
                event = b.select_next_some() => {
                    assert!(!matches!(event, SwarmEvent::Behaviour(BehaviourEvent::Rpc(
                        request_response::Event::Message { message: request_response::Message::Request { .. }, .. }
                    ))), "expired body reached the peer");
                }
            }
        }
    })
    .await
    .expect("native deadline or cancellation must wake Pending I/O before the longer timeout");
    assert!(!a.behaviour().rpc.is_pending_outbound(&peer, &expected));
    assert!(!a.behaviour().rpc.has_pending_requests(&peer));
    assert!(!a.behaviour().rpc.has_any_pending_requests());
}

async fn expect_round_trip(
    a: &mut Swarm<Behaviour>,
    b: &mut Swarm<Behaviour>,
    expected: request_response::OutboundRequestId,
) {
    let (mut answered, mut sent) = (false, false);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !answered || !sent {
            tokio::select! {
                event = a.select_next_some() => match event {
                    SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::Message {
                        message: request_response::Message::Response { request_id, response }, ..
                    })) => {
                        assert_eq!(request_id, expected);
                        assert_eq!(response, PacketResponse::Accepted);
                        answered = true;
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::OutboundFailure { .. })) => {
                        panic!("unexpected native failure: {event:?}");
                    }
                    _ => {}
                },
                event = b.select_next_some() => match event {
                    SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::Message {
                        message: request_response::Message::Request { channel, .. }, ..
                    })) => {
                        b.behaviour_mut().rpc.send_response(channel, PacketResponse::Accepted).unwrap();
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::ResponseSent { .. })) => sent = true,
                    _ => {}
                }
            }
        }
    })
    .await
    .unwrap();
    assert!(!a.behaviour().rpc.has_any_pending_requests());
}

#[tokio::test]
async fn legacy_send_apis_allow_pending_writes_without_deadline() {
    for with_addresses in [false, true] {
        let probe = Arc::new(WriteProbe::default());
        probe.blocked.store(true, Ordering::Release);
        let mut a = host_with_probes(Duration::from_secs(5), None, Some(Arc::clone(&probe)));
        let mut b = host(Duration::from_secs(5), None);
        let peer = *b.local_peer_id();
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
        let request = if with_addresses {
            a.behaviour_mut()
                .rpc
                .send_request_with_addresses(&peer, frame(1), vec![address])
        } else {
            a.add_peer_address(peer, address);
            a.behaviour_mut().rpc.send_request(&peer, frame(1))
        };
        wait_for_pending_write(&mut a, &mut b, &probe).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(a.behaviour().rpc.is_pending_outbound(&peer, &request));
        assert_eq!(probe.active.load(Ordering::Acquire), 1);
        assert_eq!(probe.bytes.load(Ordering::Acquire), 0);
        probe.release();
        expect_round_trip(&mut a, &mut b, request).await;
        assert_eq!(probe.entered.load(Ordering::Acquire), 1);
        assert_eq!(probe.active.load(Ordering::Acquire), 0);
        assert!(probe.bytes.load(Ordering::Acquire) > 0);
    }
}

#[tokio::test]
async fn deadline_bounds_response_wait_after_successful_tcp_write() {
    response_wait_expiry(false).await;
}

#[tokio::test]
async fn cancellation_bounds_response_wait_after_successful_tcp_write() {
    response_wait_expiry(true).await;
}

async fn response_wait_expiry(cancelled: bool) {
    let (mut a, mut b) = pair(Duration::from_secs(5)).await;
    let peer = *b.local_peer_id();
    let deadline = Instant::now()
        + if cancelled {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(700)
        };
    let (request, handle) = if cancelled {
        let (request, handle) =
            a.behaviour_mut()
                .rpc
                .send_request_cancellable(&peer, frame(1), deadline);
        (request, Some(handle))
    } else {
        (
            a.behaviour_mut()
                .rpc
                .send_request_with_deadline(&peer, frame(1), deadline),
            None,
        )
    };
    let channel = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            tokio::select! {
                event = a.select_next_some() => {
                    assert!(!matches!(event, SwarmEvent::Behaviour(BehaviourEvent::Rpc(
                        request_response::Event::OutboundFailure { .. }
                    ))), "request failed before response wait");
                }
                event = b.select_next_some() => {
                    if let SwarmEvent::Behaviour(BehaviourEvent::Rpc(request_response::Event::Message {
                        message: request_response::Message::Request { channel, .. }, ..
                    })) = event {
                        break channel;
                    }
                }
            }
        }
    }).await.unwrap();
    if let Some(handle) = handle {
        handle.abort();
    }
    expect_outbound_failure(&mut a, &mut b, request, cancelled).await;
    if cancelled {
        assert!(Instant::now() < deadline);
    } else {
        assert!(Instant::now() >= deadline);
    }
    drop(channel);
}

#[derive(Default)]
struct WakeCount(AtomicUsize);

impl futures::task::ArcWake for WakeCount {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.0.fetch_add(1, Ordering::AcqRel);
    }
}

#[test]
fn cancelled_disconnected_queue_wakes_and_releases_exact_native_owner() {
    for dial_polled in [false, true] {
        let peer = PeerId::random();
        let mut wire = rpc(Duration::from_secs(5));
        let (request, handle) =
            wire.send_request_cancellable(&peer, frame(1), Instant::now() + Duration::from_secs(5));
        let wakes = Arc::new(WakeCount::default());
        let waker = futures::task::waker(Arc::clone(&wakes));
        let mut cx = Context::from_waker(&waker);
        let dial_id = if dial_polled {
            let Poll::Ready(ToSwarm::Dial { opts }) = NetworkBehaviour::poll(&mut wire, &mut cx)
            else {
                panic!("expected dial");
            };
            assert!(NetworkBehaviour::poll(&mut wire, &mut cx).is_pending());
            Some(opts.connection_id())
        } else {
            None
        };
        let wake_count = wakes.0.load(Ordering::Acquire);
        handle.abort();
        if dial_polled {
            assert!(
                wakes.0.load(Ordering::Acquire) > wake_count,
                "abort must wake idle queue"
            );
        }
        assert!(wire.is_pending_outbound(&peer, &request));
        assert!(wire.has_pending_requests(&peer));
        let Poll::Ready(ToSwarm::GenerateEvent(request_response::Event::OutboundFailure {
            peer: owner,
            request_id,
            connection_id,
            error,
        })) = NetworkBehaviour::poll(&mut wire, &mut cx)
        else {
            panic!("cancelled native queue must emit a terminal failure, not dial or stay pending");
        };
        assert_eq!(owner, peer);
        assert_eq!(request_id, request);
        if let Some(dial_id) = dial_id {
            assert_eq!(connection_id, dial_id);
        }
        assert!(
            matches!(error, request_response::OutboundFailure::Io(ref io)
            if io.kind() == io::ErrorKind::Interrupted)
        );
        assert!(!wire.is_pending_outbound(&peer, &request));
        assert!(!wire.has_pending_requests(&peer));
        assert!(!wire.has_any_pending_requests());
        assert!(!wire.has_pending_connection_requests());
        handle.abort();
        assert!(
            NetworkBehaviour::poll(&mut wire, &mut cx).is_pending(),
            "no duplicate terminal event"
        );
    }
}

#[test]
fn cancelled_queue_keeps_terminal_fence_and_unrelated_legacy_work() {
    let peer = PeerId::random();
    let unrelated = PeerId::random();
    let mut wire = rpc(Duration::from_secs(5));
    let (request, handle) =
        wire.send_request_cancellable(&peer, frame(1), Instant::now() + Duration::from_secs(5));
    let legacy = wire.send_request(&unrelated, frame(2));
    handle.abort();
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    assert!(matches!(
        NetworkBehaviour::poll(&mut wire, &mut cx),
        Poll::Ready(ToSwarm::Dial { .. })
    ));
    assert!(
        !wire.is_pending_outbound(&peer, &request),
        "cancelled queue entry is gone"
    );
    assert!(
        wire.has_pending_requests(&peer),
        "undelivered failure still fences its peer"
    );
    assert!(wire.is_pending_outbound(&unrelated, &legacy));
    assert!(matches!(NetworkBehaviour::poll(&mut wire, &mut cx),
        Poll::Ready(ToSwarm::GenerateEvent(request_response::Event::OutboundFailure { request_id, .. }))
        if request_id == request));
    assert!(!wire.has_pending_requests(&peer));
    assert!(wire.has_pending_requests(&unrelated));
}

#[tokio::test]
async fn cancellation_wakes_pending_tcp_write_and_drops_worker() {
    let probe = Arc::new(WriteProbe::default());
    probe.blocked.store(true, Ordering::Release);
    let (mut a, mut b) = connect_hosts(
        host_with_probes(Duration::from_secs(10), None, Some(Arc::clone(&probe))),
        host(Duration::from_secs(10), None),
    )
    .await;
    let peer = *b.local_peer_id();
    let (request, handle) = a.behaviour_mut().rpc.send_request_cancellable(
        &peer,
        frame(1),
        Instant::now() + Duration::from_secs(10),
    );
    wait_for_pending_write(&mut a, &mut b, &probe).await;
    assert_eq!(probe.active.load(Ordering::Acquire), 1);
    handle.abort();
    assert!(a.behaviour().rpc.has_pending_requests(&peer));
    expect_outbound_failure(&mut a, &mut b, request, true).await;
    assert_eq!(probe.entered.load(Ordering::Acquire), 1);
    assert_eq!(probe.active.load(Ordering::Acquire), 0);
    assert_eq!(probe.bytes.load(Ordering::Acquire), 0);

    probe.release();
    let request = a.behaviour_mut().rpc.send_request(&peer, frame(2));
    expect_round_trip(&mut a, &mut b, request).await;
    assert_eq!(probe.entered.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn cancellation_before_tcp_substream_does_not_invoke_codec() {
    let probe = Arc::new(WriteProbe::default());
    let (mut a, mut b) = connect_hosts(
        host_with_probes(Duration::from_secs(5), None, Some(Arc::clone(&probe))),
        host(Duration::from_secs(5), None),
    )
    .await;
    let peer = *b.local_peer_id();
    let (request, handle) = a.behaviour_mut().rpc.send_request_cancellable(
        &peer,
        frame(1),
        Instant::now() + Duration::from_secs(5),
    );
    handle.abort();
    expect_outbound_failure(&mut a, &mut b, request, true).await;
    assert_eq!(probe.entered.load(Ordering::Acquire), 0);
    assert_eq!(probe.bytes.load(Ordering::Acquire), 0);

    let request = a.behaviour_mut().rpc.send_request_with_deadline(
        &peer,
        frame(2),
        Instant::now() + Duration::from_secs(5),
    );
    expect_round_trip(&mut a, &mut b, request).await;
    assert_eq!(probe.entered.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn dropping_cancellation_handle_does_not_cancel_tcp_request() {
    let mut a = host(Duration::from_secs(5), None);
    let mut b = host(Duration::from_secs(5), None);
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
    a.add_peer_address(*b.local_peer_id(), address);
    let (request, handle) = a.behaviour_mut().rpc.send_request_cancellable(
        b.local_peer_id(),
        frame(1),
        Instant::now() + Duration::from_secs(5),
    );
    drop(handle);
    assert!(a.behaviour().rpc.has_pending_connection_requests());
    expect_round_trip(&mut a, &mut b, request).await;
}

#[test]
fn cancelled_negotiation_keeps_native_fifo_and_exact_terminal_ids() {
    use libp2p::swarm::{
        ConnectionHandler, ConnectionHandlerEvent, ConnectionId, StreamUpgradeError,
        handler::{ConnectionEvent, DialUpgradeError},
    };

    let peer = PeerId::random();
    let connection_id = ConnectionId::new_unchecked(1);
    let address = "/ip4/127.0.0.1/tcp/1".parse().unwrap();
    let mut wire = rpc(Duration::from_secs(5));
    let mut handler = NetworkBehaviour::handle_established_inbound_connection(
        &mut wire,
        connection_id,
        peer,
        &address,
        &address,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let (first, handle) = wire.send_request_cancellable(&peer, frame(1), deadline);
    let second = wire.send_request_with_deadline(&peer, frame(2), deadline);
    let mut cx = Context::from_waker(futures::task::noop_waker_ref());
    for _ in 0..2 {
        let Poll::Ready(ToSwarm::NotifyHandler { event, .. }) =
            NetworkBehaviour::poll(&mut wire, &mut cx)
        else {
            panic!("expected handler message");
        };
        handler.on_behaviour_event(event);
        assert!(matches!(
            handler.poll(&mut cx),
            Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest { .. })
        ));
    }
    handle.abort();
    assert!(
        handler.poll(&mut cx).is_pending(),
        "negotiation retains both FIFO entries"
    );
    for (expected, cancelled) in [(first, true), (second, false)] {
        handler.on_connection_event(ConnectionEvent::DialUpgradeError(DialUpgradeError {
            info: (),
            error: StreamUpgradeError::Timeout,
        }));
        let Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event)) = handler.poll(&mut cx)
        else {
            panic!("expected native terminal event");
        };
        NetworkBehaviour::on_connection_handler_event(&mut wire, peer, connection_id, event);
        assert!(
            wire.has_pending_requests(&peer),
            "terminal event is still undelivered"
        );
        let Poll::Ready(ToSwarm::GenerateEvent(request_response::Event::OutboundFailure {
            request_id,
            error,
            ..
        })) = NetworkBehaviour::poll(&mut wire, &mut cx)
        else {
            panic!("expected outbound failure");
        };
        assert_eq!(request_id, expected);
        if cancelled {
            assert!(
                matches!(error, request_response::OutboundFailure::Io(ref io) if io.kind() == io::ErrorKind::Interrupted)
            );
        } else {
            assert!(matches!(error, request_response::OutboundFailure::Timeout));
        }
    }
    assert!(!wire.has_any_pending_requests());
}
