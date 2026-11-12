//! Transport-only evidence. No TUN, production service, or phone settings change.
use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, StreamExt};
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder, Transport,
    core::{ConnectedPoint, upgrade},
    identify,
    identity::Keypair,
    multiaddr::Protocol,
    noise, ping, relay, request_response,
    swarm::{NetworkBehaviour, SwarmEvent, behaviour::toggle::Toggle},
    tcp, yamux,
};
use p2p_vpn::{
    config::Config, identity::NodeIdentity, runtime::bootstrap_check::scan_public_relay_candidates,
};
use serde::Serialize;
use serde_json::{Value, json};

const DAY: Duration = Duration::from_hours(24);
const SETUP: Duration = Duration::from_secs(20);
const INCOMING: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Traffic {
    reads: AtomicU64,
    writes: AtomicU64,
    read_bytes: AtomicU64,
    write_bytes: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct Counts {
    reads: u64,
    writes: u64,
    read_bytes: u64,
    write_bytes: u64,
}

impl Counts {
    fn since(self, before: Self) -> Self {
        Self {
            reads: self.reads - before.reads,
            writes: self.writes - before.writes,
            read_bytes: self.read_bytes - before.read_bytes,
            write_bytes: self.write_bytes - before.write_bytes,
        }
    }
}

impl Traffic {
    fn snapshot(&self) -> Counts {
        Counts {
            reads: self.reads.load(Ordering::Relaxed),
            writes: self.writes.load(Ordering::Relaxed),
            read_bytes: self.read_bytes.load(Ordering::Relaxed),
            write_bytes: self.write_bytes.load(Ordering::Relaxed),
        }
    }
}

struct Counted<T> {
    io: T,
    traffic: Arc<Traffic>,
}

impl<T: AsyncRead + Unpin> AsyncRead for Counted<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.io).poll_read(cx, buffer);
        if let Poll::Ready(Ok(size)) = &result
            && *size > 0
        {
            this.traffic.reads.fetch_add(1, Ordering::Relaxed);
            this.traffic
                .read_bytes
                .fetch_add(*size as u64, Ordering::Relaxed);
        }
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Counted<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.io).poll_write(cx, buffer);
        if let Poll::Ready(Ok(size)) = &result
            && *size > 0
        {
            this.traffic.writes.fetch_add(1, Ordering::Relaxed);
            this.traffic
                .write_bytes
                .fetch_add(*size as u64, Ordering::Relaxed);
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().io).poll_close(cx)
    }
}

#[derive(Clone, Default)]
struct Echo;

#[async_trait]
impl request_response::Codec for Echo {
    type Protocol = StreamProtocol;
    type Request = [u8; 16];
    type Response = [u8; 16];

    async fn read_request<T>(&mut self, _: &StreamProtocol, io: &mut T) -> io::Result<[u8; 16]>
    where
        T: AsyncRead + Unpin + Send,
    {
        let mut nonce = [0; 16];
        io.read_exact(&mut nonce).await?;
        Ok(nonce)
    }

    async fn read_response<T>(&mut self, p: &StreamProtocol, io: &mut T) -> io::Result<[u8; 16]>
    where
        T: AsyncRead + Unpin + Send,
    {
        self.read_request(p, io).await
    }

    async fn write_request<T>(
        &mut self,
        _: &StreamProtocol,
        io: &mut T,
        nonce: [u8; 16],
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        io.write_all(&nonce).await?;
        io.close().await
    }

    async fn write_response<T>(
        &mut self,
        p: &StreamProtocol,
        io: &mut T,
        nonce: [u8; 16],
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        self.write_request(p, io, nonce).await
    }
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay: relay::client::Behaviour,
    server: Toggle<relay::Behaviour>,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    echo: request_response::Behaviour<Echo>,
}

fn host(server: bool) -> (Swarm<Behaviour>, Arc<Traffic>) {
    host_with_options(server, DAY, None)
}

fn host_with_options(
    server: bool,
    ping_interval: Duration,
    lease: Option<Duration>,
) -> (Swarm<Behaviour>, Arc<Traffic>) {
    let key = Keypair::generate_ed25519();
    let traffic = Arc::new(Traffic::default());
    let counter = Arc::clone(&traffic);
    let swarm = SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_other_transport(move |key: &Keypair| {
            tcp::tokio::Transport::new(tcp::Config::default().nodelay(true))
                .map(move |io, _| Counted {
                    io,
                    traffic: Arc::clone(&counter),
                })
                .upgrade(upgrade::Version::V1Lazy)
                .authenticate(noise::Config::new(key).unwrap())
                .multiplex(yamux::Config::default())
        })
        .unwrap()
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .unwrap()
        .with_behaviour(|key, relay| Behaviour {
            relay,
            server: server
                .then(|| {
                    let mut config = relay::Config::default();
                    if let Some(lease) = lease {
                        config.reservation_duration = lease;
                    }
                    relay::Behaviour::new(key.public().to_peer_id(), config)
                })
                .into(),
            identify: identify::Behaviour::new(
                identify::Config::new("/p2p-vpn/relay-idle-test/1".into(), key.public())
                    .with_interval(DAY),
            ),
            ping: ping::Behaviour::new(ping::Config::new().with_interval(ping_interval)),
            echo: request_response::Behaviour::new(
                [(
                    StreamProtocol::new("/p2p-vpn/relay-idle-test/1"),
                    request_response::ProtocolSupport::Full,
                )],
                request_response::Config::default()
                    .with_request_timeout(INCOMING)
                    .with_max_concurrent_streams(4),
            ),
        })
        .unwrap()
        .with_swarm_config(|config| config.with_idle_connection_timeout(DAY))
        .build();
    (swarm, traffic)
}

fn plain_tcp(address: &Multiaddr) -> bool {
    matches!(
        address.iter().collect::<Vec<_>>().as_slice(),
        [
            Protocol::Ip4(_) | Protocol::Ip6(_),
            Protocol::Tcp(_),
            Protocol::P2p(_)
        ]
    )
}

fn observe(
    event: SwarmEvent<BehaviourEvent>,
    relay: PeerId,
    report: &mut Value,
) -> Result<(), String> {
    match event {
        SwarmEvent::Behaviour(BehaviourEvent::Relay(
            relay::client::Event::ReservationReqAccepted {
                relay_peer_id,
                renewal,
                limit,
            },
        )) => {
            report["reservations"].as_array_mut().unwrap().push(json!({
                "relay": relay_peer_id.to_string(), "renewal":renewal,
                "limit":format!("{limit:?}")
            }));
        }
        SwarmEvent::ConnectionClosed { peer_id, cause, .. } if peer_id == relay => {
            return Err(format!("relay connection closed: {cause:?}"));
        }
        SwarmEvent::ListenerError { error, .. } => return Err(format!("listener: {error}")),
        SwarmEvent::ListenerClosed { reason, .. } => {
            return Err(format!("listener closed: {reason:?}"));
        }
        SwarmEvent::OutgoingConnectionError { error, .. } => return Err(format!("dial: {error}")),
        _ => {}
    }
    Ok(())
}

async fn reserve(
    receiver: &mut Swarm<Behaviour>,
    address: Multiaddr,
    report: &mut Value,
) -> Result<(), String> {
    let Some(Protocol::P2p(relay)) = address.iter().last() else {
        return Err("missing relay identity".into());
    };
    let circuit = address
        .clone()
        .with(Protocol::P2pCircuit)
        .with(Protocol::P2p(*receiver.local_peer_id()));
    receiver
        .listen_on(address.with(Protocol::P2pCircuit))
        .map_err(|e| e.to_string())?;
    let deadline = tokio::time::sleep(SETUP);
    tokio::pin!(deadline);
    let mut listening = false;
    loop {
        tokio::select! {
            () = &mut deadline => return Err("reservation deadline".into()),
            event = receiver.select_next_some() => {
                if let SwarmEvent::NewListenAddr {address, ..} = &event
                    && address.iter().any(|p| p == Protocol::P2pCircuit)
                    && address.iter().any(|p| matches!(p, Protocol::Tcp(_)))
                {
                    report["advertised_tcp_circuit"] = json!(address.to_string());
                    report["circuit"] = json!(circuit.to_string());
                    listening = true;
                }
                observe(event, relay, report)?;
                if listening && !report["reservations"].as_array().unwrap().is_empty() { return Ok(()); }
            }
        }
    }
}

async fn idle(
    receiver: &mut Swarm<Behaviour>,
    traffic: &Traffic,
    relay: PeerId,
    duration: Duration,
    report: &mut Value,
    report_file: Option<&str>,
) -> Result<(), String> {
    let started = Instant::now();
    let before = traffic.snapshot();
    let mut previous = before;
    let mut sample = tokio::time::interval(Duration::from_secs(5));
    sample.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    sample.tick().await;
    let deadline = tokio::time::sleep(duration);
    tokio::pin!(deadline);
    let result = loop {
        tokio::select! {
            () = &mut deadline => break Ok(()),
            _ = sample.tick() => {
                let current = traffic.snapshot();
                report["idle_samples"].as_array_mut().unwrap().push(json!({
                    "elapsed_seconds":started.elapsed().as_secs_f64(),
                    "delta":current.since(previous),
                }));
                previous = current;
                if let Some(path) = report_file {
                    std::fs::write(path, serde_json::to_vec_pretty(report).unwrap()).map_err(|e| e.to_string())?;
                }
            }
            event = receiver.select_next_some() => {
                if let Err(error) = observe(event, relay, report) { break Err(error); }
            }
        }
    };
    let current = traffic.snapshot();
    report["idle_samples"].as_array_mut().unwrap().push(json!({
        "elapsed_seconds":started.elapsed().as_secs_f64(), "delta":current.since(previous),
    }));
    report["idle_elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
    report["idle_traffic"] = json!(current.since(before));
    result
}

async fn incoming(receiver: &mut Swarm<Behaviour>, report: &mut Value) -> Result<(), String> {
    let (mut sender, _) = host(false);
    let sender_peer = *sender.local_peer_id();
    let receiver_peer = *receiver.local_peer_id();
    let mut circuit: Multiaddr = report["circuit"].as_str().unwrap().parse().unwrap();
    if circuit.iter().last() != Some(Protocol::P2p(receiver_peer)) {
        circuit.push(Protocol::P2p(receiver_peer));
    }
    sender.dial(circuit).map_err(|e| e.to_string())?;
    let nonce = [0x7b; 16];
    let mut request = None;
    let mut receiver_relayed = false;
    let mut sender_relayed = false;
    let mut request_seen = false;
    let started = Instant::now();
    let deadline = tokio::time::sleep(INCOMING);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => return Err("fresh incoming echo deadline".into()),
            event = receiver.select_next_some() => match event {
                SwarmEvent::ConnectionEstablished {peer_id, endpoint, ..} if peer_id == sender_peer => {
                    receiver_relayed = relayed(&endpoint);
                    report["receiver_endpoint"] = json!(format!("{endpoint:?}"));
                }
                SwarmEvent::Behaviour(BehaviourEvent::Echo(request_response::Event::Message {peer, message:request_response::Message::Request {request, channel, ..}, ..})) => {
                    if peer != sender_peer || request != nonce {return Err("unexpected request identity/nonce".into());}
                    request_seen = true;
                    receiver.behaviour_mut().echo.send_response(channel, request).map_err(|_| "echo channel closed")?;
                }
                SwarmEvent::ListenerClosed {reason, ..} => return Err(format!("incoming listener closed: {reason:?}")),
                _ => {}
            },
            event = sender.select_next_some() => match event {
                SwarmEvent::ConnectionEstablished {peer_id, endpoint, ..} if peer_id == receiver_peer => {
                    sender_relayed = relayed(&endpoint);
                    report["sender_endpoint"] = json!(format!("{endpoint:?}"));
                    request = Some(sender.behaviour_mut().echo.send_request(&receiver_peer, nonce));
                }
                SwarmEvent::Behaviour(BehaviourEvent::Echo(request_response::Event::Message {peer, message:request_response::Message::Response {request_id, response}, ..})) => {
                    if peer != receiver_peer || Some(request_id) != request || response != nonce || !request_seen || !sender_relayed || !receiver_relayed {
                        return Err("incoming identity/nonce/relay correlation failed".into());
                    }
                    report["incoming_elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
                    report["sender"] = json!(sender_peer.to_string());
                    report["receiver"] = json!(receiver_peer.to_string());
                    return Ok(());
                }
                SwarmEvent::Behaviour(BehaviourEvent::Echo(request_response::Event::OutboundFailure {error, ..})) => return Err(format!("echo failed: {error}")),
                SwarmEvent::OutgoingConnectionError {error, ..} => return Err(format!("incoming dial failed: {error}")),
                _ => {}
            }
        }
    }
}

fn relayed(endpoint: &ConnectedPoint) -> bool {
    match endpoint {
        ConnectedPoint::Dialer { address, .. } => address,
        ConnectedPoint::Listener { local_addr, .. } => local_addr,
    }
    .iter()
    .any(|p| p == Protocol::P2pCircuit)
}

fn report() -> Value {
    json!({"schema_version":1,"scope":"transport_only_not_energy_or_cellular",
        "started_unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        "client_ping_interval_seconds":DAY.as_secs(),"client_identify_interval_seconds":DAY.as_secs(),
        "reservations":[],"idle_samples":[],"attempts":[]})
}

#[tokio::test]
async fn local_relay_accepts_incoming_after_idle() {
    local_scenario(DAY, None).await;
}

#[tokio::test]
async fn remote_heartbeats_are_included_in_idle_traffic() {
    local_scenario(Duration::from_millis(200), None).await;
}

#[tokio::test]
async fn local_reservation_renewal_preserves_incoming() {
    local_scenario(DAY, Some(Duration::from_secs(8))).await;
}

async fn local_scenario(server_ping: Duration, lease: Option<Duration>) {
    let expect_traffic = server_ping < DAY || lease.is_some();
    let (mut server, _) = host_with_options(true, server_ping, lease);
    let peer = *server.local_peer_id();
    server
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    let address = loop {
        if let SwarmEvent::NewListenAddr { address, .. } = server.select_next_some().await {
            break address.with(Protocol::P2p(peer));
        }
    };
    server.add_external_address(address.clone());
    server.add_external_address(
        format!("/ip4/127.0.0.1/udp/1/quic-v1/p2p/{peer}")
            .parse()
            .unwrap(),
    );
    let server_task = tokio::spawn(async move { while server.next().await.is_some() {} });
    let (mut receiver, traffic) = host(false);
    let mut report = report();
    reserve(&mut receiver, address, &mut report).await.unwrap();
    let duration = Duration::from_secs(if server_ping < DAY { 2 } else { 12 });
    idle(
        &mut receiver,
        &traffic,
        peer,
        Duration::from_secs(1),
        &mut report,
        None,
    )
    .await
    .unwrap();
    idle(&mut receiver, &traffic, peer, duration, &mut report, None)
        .await
        .unwrap();
    incoming(&mut receiver, &mut report).await.unwrap();
    assert!(report["idle_elapsed_seconds"].as_f64().unwrap() >= duration.as_secs_f64());
    assert!(report["circuit"].as_str().unwrap().contains("/tcp/"));
    assert!(!report["circuit"].as_str().unwrap().contains("/quic-v1"));
    let renewals = report["reservations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["renewal"] == true)
        .count();
    assert_eq!(renewals > 0, lease.is_some());
    assert_eq!(
        report["idle_traffic"]["read_bytes"].as_u64().unwrap() > 0,
        expect_traffic
    );
    assert_eq!(
        report["idle_traffic"]["write_bytes"].as_u64().unwrap() > 0,
        expect_traffic
    );
    server_task.abort();
    let _ = server_task.await;
}

#[tokio::test]
async fn counters_exclude_eof_empty_io_and_poll_attempts() {
    let traffic = Arc::new(Traffic::default());
    let mut io = Counted {
        io: futures::io::Cursor::new(vec![1, 2, 3]),
        traffic: Arc::clone(&traffic),
    };
    let mut bytes = [0; 4];
    assert_eq!(io.read(&mut bytes).await.unwrap(), 3);
    assert_eq!(io.read(&mut bytes).await.unwrap(), 0);
    assert_eq!(io.write(&[]).await.unwrap(), 0);
    assert_eq!(io.write(&[4, 5]).await.unwrap(), 2);
    let snapshot = traffic.snapshot();
    assert_eq!(
        (
            snapshot.reads,
            snapshot.read_bytes,
            snapshot.writes,
            snapshot.write_bytes
        ),
        (1, 3, 1, 2)
    );
}

#[test]
fn candidates_reject_quic_wrappers_and_relay_circuits() {
    let peer = PeerId::random();
    assert!(plain_tcp(
        &format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}")
            .parse()
            .unwrap()
    ));
    for address in [
        format!("/ip4/127.0.0.1/udp/4001/quic-v1/p2p/{peer}"),
        format!("/ip4/127.0.0.1/tcp/4001/ws/p2p/{peer}"),
        format!("/ip4/127.0.0.1/tcp/4001/p2p/{peer}/p2p-circuit"),
        "/ip4/127.0.0.1/tcp/4001".into(),
    ] {
        assert!(!plain_tcp(&address.parse().unwrap()));
    }
}

#[tokio::test]
#[ignore = "explicit opt-in: bounded public relay traffic and a real idle wait"]
async fn public_relay_accepts_incoming_after_idle() {
    let seconds: u64 = std::env::var("P2P_VPN_RELAY_IDLE_SECONDS")
        .expect("explicit idle duration required")
        .parse()
        .unwrap();
    assert!((1..=3600).contains(&seconds));
    let path = std::env::var("P2P_VPN_RELAY_IDLE_REPORT").expect("report path required");
    let mut report = report();
    report["requested_idle_seconds"] = json!(seconds);
    let result = Box::pin(public_scenario(
        Duration::from_secs(seconds),
        &mut report,
        &path,
    ))
    .await;
    report["succeeded"] = json!(result.is_ok());
    if let Err(error) = &result {
        report["failure"] = json!(error);
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    eprintln!("RELAY_IDLE report={path} result={result:?}");
    result.unwrap();
}

async fn public_scenario(duration: Duration, report: &mut Value, path: &str) -> Result<(), String> {
    let identity = NodeIdentity::generate_ed25519().map_err(|e| format!("{e:?}"))?;
    let config: Config = serde_json::from_value(json!({"network": {
        "name":"relay-idle-validation", "private_key":identity.private_key,
        "listen_addresses":["/ip4/0.0.0.0/tcp/0", "/ip4/0.0.0.0/udp/0/quic-v1"],
        "discovery":{"mdns":false}
    }}))
    .map_err(|e| e.to_string())?;
    let scan = scan_public_relay_candidates(&config, Duration::from_secs(45), 64)
        .await
        .map_err(|e| format!("{e:?}"))?;
    report["discovery"] = json!({"scanned_peers":scan.scanned_peers,
        "identified_peers":scan.identified_peers,"relay_capable_peers":scan.relay_capable_peers,
        "candidate_count":scan.candidates.len(),"dial_failures":scan.dial_failures});
    let candidates = scan
        .candidates
        .iter()
        .filter_map(|c| c.address.parse::<Multiaddr>().ok())
        .filter(plain_tcp)
        .take(8);
    for address in candidates {
        let Some(Protocol::P2p(relay)) = address.iter().last() else {
            unreachable!();
        };
        let (mut receiver, traffic) = host(false);
        let mut attempt = self::report();
        let started = Instant::now();
        attempt["address"] = json!(address.to_string());
        let result = reserve(&mut receiver, address, &mut attempt).await;
        attempt["setup_seconds"] = json!(started.elapsed().as_secs_f64());
        attempt["setup_error"] = json!(result.as_ref().err());
        report["attempts"]
            .as_array_mut()
            .unwrap()
            .push(attempt.clone());
        std::fs::write(path, serde_json::to_vec_pretty(report).unwrap())
            .map_err(|e| e.to_string())?;
        if result.is_err() {
            continue;
        }
        report["selected_address"] = attempt["address"].clone();
        report["circuit"] = attempt["circuit"].clone();
        report["reservations"] = attempt["reservations"].clone();
        // Settle the initial identify and ping exchange before starting the window.
        idle(
            &mut receiver,
            &traffic,
            relay,
            Duration::from_secs(10),
            &mut attempt,
            None,
        )
        .await?;
        report["setup_and_settle_traffic"] = json!(traffic.snapshot());
        eprintln!(
            "RELAY_IDLE reserved={} beginning_real_idle_seconds={}",
            report["selected_address"],
            duration.as_secs()
        );
        idle(&mut receiver, &traffic, relay, duration, report, Some(path)).await?;
        eprintln!(
            "RELAY_IDLE elapsed={} traffic={} starting_fresh_sender",
            report["idle_elapsed_seconds"], report["idle_traffic"]
        );
        return incoming(&mut receiver, report).await;
    }
    Err("no plain-TCP public relay accepted a reservation within the bounded scan/attempts".into())
}
