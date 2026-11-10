use super::*;
use tokio::net::UdpSocket;

struct UdpProxy {
    address: SocketAddr,
    mode: watch::Sender<Link>,
    forwarded: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    task: JoinHandle<()>,
}
impl UdpProxy {
    async fn new(front: std::net::UdpSocket, backend: SocketAddr) -> Self {
        front.set_nonblocking(true).unwrap();
        let front = Arc::new(UdpSocket::from_std(front).unwrap());
        let address = front.local_addr().unwrap();
        let (mode, mut changes) = watch::channel(Link::Up);
        let forwarded = Arc::new(AtomicU64::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let (sent, lost) = (Arc::clone(&forwarded), Arc::clone(&dropped));
        let task = tokio::spawn(async move {
            let mut bridges = std::collections::HashMap::<SocketAddr, Arc<UdpSocket>>::new();
            let mut replies = tokio::task::JoinSet::new();
            let mut buffer = vec![0_u8; 65535];
            loop {
                tokio::select! {
                    result = changes.changed() => { if result.is_err() || *changes.borrow() == Link::Stop { break; } }
                    result = front.recv_from(&mut buffer) => {
                        let (length, client) = result.unwrap();
                        if *changes.borrow() != Link::Up { lost.fetch_add(1,Ordering::Relaxed); continue; }
                        if let std::collections::hash_map::Entry::Vacant(entry) = bridges.entry(client) {
                            let bridge = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
                            bridge.connect(backend).await.unwrap();
                            entry.insert(Arc::clone(&bridge));
                            let front = Arc::clone(&front);
                            let (sent,lost) = (Arc::clone(&sent),Arc::clone(&lost));
                            let mode = changes.clone();
                            replies.spawn(async move {
                                let mut response=vec![0_u8;65535];
                                while let Ok(length)=bridge.recv(&mut response).await {
                                    if *mode.borrow() == Link::Up {
                                        if front.send_to(&response[..length],client).await.is_ok() { sent.fetch_add(1,Ordering::Relaxed); }
                                    } else { lost.fetch_add(1,Ordering::Relaxed); }
                                }
                            });
                        }
                        if bridges[&client].send(&buffer[..length]).await.is_ok() { sent.fetch_add(1,Ordering::Relaxed); }
                    }
                }
            }
            replies.abort_all();
            while replies.join_next().await.is_some() {}
        });
        Self {
            address,
            mode,
            forwarded,
            dropped,
            task,
        }
    }
    fn set(&self, mode: Link) {
        self.mode.send_replace(mode);
    }
    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"address":self.address.to_string(),"forwarded_datagrams":self.forwarded.load(Ordering::Relaxed),"dropped_datagrams":self.dropped.load(Ordering::Relaxed)})
    }
}

fn metric(lines: &[String], name: &str) -> u64 {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
        .unwrap_or(0)
}

async fn quic_stage(
    name: &str,
    a: &mut Node,
    b: &mut Node,
    sequence: &mut u64,
) -> serde_json::Value {
    let start = Instant::now();
    let mut consecutive = 0;
    let mut attempts = 0;
    let mut first = None;
    while start.elapsed() < Duration::from_secs(90) {
        let before_a = metric(
            &a.control.status().await.unwrap(),
            "outbound_owned_quic_datagram_packets",
        );
        let before_b = metric(
            &b.control.status().await.unwrap(),
            "outbound_owned_quic_datagram_packets",
        );
        *sequence += 1;
        attempts += 1;
        let delivered = probe(a, b, *sequence, Duration::from_millis(500)).await;
        let after_a = metric(
            &a.control.status().await.unwrap(),
            "outbound_owned_quic_datagram_packets",
        );
        let after_b = metric(
            &b.control.status().await.unwrap(),
            "outbound_owned_quic_datagram_packets",
        );
        if delivered && after_a > before_a && after_b > before_b {
            if first.is_none() {
                first = Some(start.elapsed().as_secs_f64());
            }
            consecutive += 1;
            if consecutive == 10 {
                let result = serde_json::json!({"stage":name,"first_bidirectional_quic_seconds":first,"qualified_seconds":start.elapsed().as_secs_f64(),"probe_pairs_sent":attempts,"consecutive_fresh_quic_pairs":consecutive,
                    "a_state":a.control.state().await.unwrap(),"b_state":b.control.state().await.unwrap(),"a_metrics":a.control.status().await.unwrap(),"b_metrics":b.control.status().await.unwrap()});
                eprintln!(
                    "RECOVERY {name}: first QUIC={:.3}s qualified={:.3}s attempts={attempts}",
                    first.unwrap(),
                    start.elapsed().as_secs_f64()
                );
                write_evidence(name, &result);
                return result;
            }
        } else {
            consecutive = 0;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    write_evidence(
        name,
        &serde_json::json!({"stage":name,"failed":true,"a_state":a.control.state().await.unwrap(),"b_state":b.control.state().await.unwrap(),"a_metrics":a.control.status().await.unwrap(),"b_metrics":b.control.status().await.unwrap()}),
    );
    panic!("{name}: owned QUIC did not recover within 90 seconds");
}

#[test]
fn datagram_blackhole_recovers_through_fallback_and_alternate_endpoint() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(scenario());
        })
        .unwrap()
        .join()
        .unwrap();
}

async fn scenario() {
    let alice = NodeIdentity::generate_ed25519().unwrap();
    let bob = loop {
        let candidate = NodeIdentity::generate_ed25519().unwrap();
        if builtin_ipv4(PeerId::from_libp2p(alice.peer_id.parse().unwrap()))
            != builtin_ipv4(PeerId::from_libp2p(candidate.peer_id.parse().unwrap()))
        {
            break candidate;
        }
    };
    // Production endpoint ranking breaks equal-IP ties by port. Reserve all
    // four ports together and give the proxies the lower ports, so the intended
    // initial path is selected rather than accidentally bypassing injection.
    let mut sockets =
        std::array::from_fn::<_, 4, _>(|_| std::net::UdpSocket::bind("127.0.0.1:0").unwrap());
    sockets.sort_by_key(|socket| socket.local_addr().unwrap().port());
    let [front_a, front_b, back_a, back_b] = sockets;
    let guards = [back_a, back_b];
    let backends = guards.each_ref().map(|socket| socket.local_addr().unwrap());
    let proxies = [
        UdpProxy::new(front_a, backends[0]).await,
        UdpProxy::new(front_b, backends[1]).await,
    ];
    let tcp = [free_address(), free_address()];
    let mut configs = [
        config(
            &alice,
            &bob,
            tcp[0],
            vec![format!("/ip4/127.0.0.1/tcp/{}", tcp[1].port())],
        ),
        config(&bob, &alice, tcp[1], vec![]),
    ];
    for i in 0..2 {
        configs[i].network.packet_plane.quic_listen = vec![backends[i].to_string()];
        configs[i].network.packet_plane.quic_external_endpoints =
            vec![proxies[i].address.to_string()];
    }
    drop(guards);
    let [a_config, b_config] = configs;
    let (mut a, mut b) = (Node::start(a_config), Node::start(b_config));
    let mut sequence = 1_000_000;
    let mut stages = Vec::new();
    let forwarded_before = proxies
        .iter()
        .map(|p| p.forwarded.load(Ordering::Relaxed))
        .sum::<u64>();
    stages.push(quic_stage("quic_initial_proxied_path", &mut a, &mut b, &mut sequence).await);
    let forwarded = proxies
        .iter()
        .map(|p| p.forwarded.load(Ordering::Relaxed))
        .sum::<u64>();
    assert!(
        forwarded >= forwarded_before + 10,
        "QUIC did not traverse the fault-injection proxies"
    );
    for proxy in &proxies {
        proxy.set(Link::Blackhole);
    }
    stages.push(
        stage(
            "quic_blackhole_any_transport_recovery",
            &mut a,
            &mut b,
            &mut sequence,
            Duration::from_secs(60),
        )
        .await,
    );
    stages.push(
        quic_stage(
            "quic_alternate_endpoint_recovery",
            &mut a,
            &mut b,
            &mut sequence,
        )
        .await,
    );
    assert!(
        proxies
            .iter()
            .map(|p| p.dropped.load(Ordering::Relaxed))
            .sum::<u64>()
            > 0,
        "no real datagrams were dropped"
    );
    for proxy in &proxies {
        proxy.set(Link::Up);
    }
    let notification = a.control.network_changed().await.unwrap();
    stages.push(
        quic_stage(
            "quic_after_network_change_notification",
            &mut a,
            &mut b,
            &mut sequence,
        )
        .await,
    );
    write_evidence(
        "quic-report",
        &serde_json::json!({"test":"datagram_blackhole_recovers_through_fallback_and_alternate_endpoint","physical_adapter_changed":false,"packet_io":"in-process packet channels", "stages":stages,"explicit_notification":notification,"proxies":proxies.iter().map(UdpProxy::snapshot).collect::<Vec<_>>()}),
    );
    for node in [&mut a, &mut b] {
        let _ = node.shutdown.take().unwrap().send(());
    }
    for node in [a, b] {
        tokio::time::timeout(Duration::from_secs(15), node.daemon)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    for proxy in proxies {
        proxy.set(Link::Stop);
        proxy.task.await.unwrap();
    }
}
