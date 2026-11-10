use super::*;

fn metric(lines: &[String], name: &str) -> u64 {
    lines
        .iter()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
        .unwrap_or(0)
}

async fn path_stage(
    name: &str,
    path: &str,
    counter: &str,
    a: &mut Node,
    b: &mut Node,
    sequence: &mut u64,
) -> serde_json::Value {
    let started = Instant::now();
    let mut consecutive = 0;
    let mut attempts = 0;
    let mut first = None;
    while started.elapsed() < Duration::from_secs(120) {
        let before_a = metric(&a.control.status().await.unwrap(), counter);
        let before_b = metric(&b.control.status().await.unwrap(), counter);
        *sequence += 1;
        attempts += 1;
        let delivered = probe(a, b, *sequence, Duration::from_millis(500)).await;
        let paths_a = a.control.paths().await.unwrap();
        let paths_b = b.control.paths().await.unwrap();
        let selected = |lines: &[String]| {
            lines.iter().any(|line| {
                line.starts_with("peer selected path: ") && line.contains(&format!(" {path} "))
            })
        };
        let metrics_a = a.control.status().await.unwrap();
        let metrics_b = b.control.status().await.unwrap();
        if delivered
            && selected(&paths_a)
            && selected(&paths_b)
            && metric(&metrics_a, counter) > before_a
            && metric(&metrics_b, counter) > before_b
        {
            if first.is_none() {
                first = Some(started.elapsed().as_secs_f64());
            }
            consecutive += 1;
            if consecutive == 10 {
                let result = serde_json::json!({"stage":name,"required_path":path,"first_bidirectional_on_required_path_seconds":first,"qualified_seconds":started.elapsed().as_secs_f64(),"probe_pairs_sent":attempts,"consecutive_fresh_bidirectional_pairs":consecutive,"a_paths":paths_a,"b_paths":paths_b,"a_metrics":metrics_a,"b_metrics":metrics_b,"a_state":a.control.state().await.unwrap(),"b_state":b.control.state().await.unwrap()});
                write_evidence(name, &result);
                eprintln!(
                    "RECOVERY {name}: {path} first={:.3}s qualified={:.3}s attempts={attempts}",
                    first.unwrap(),
                    started.elapsed().as_secs_f64()
                );
                return result;
            }
        } else {
            consecutive = 0;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    write_evidence(
        name,
        &serde_json::json!({"stage":name,"failed":true,"required_path":path,"a_state":a.control.state().await.unwrap(),"b_state":b.control.state().await.unwrap(),"a_metrics":a.control.status().await.unwrap(),"b_metrics":b.control.status().await.unwrap()}),
    );
    panic!("{name}: no sustained fresh bidirectional delivery on {path} within 120 seconds");
}

#[test]
fn direct_link_loss_uses_relay_bridge_and_returns_to_direct() {
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
        if builtin_ipv4(PeerId::from_libp2p(candidate.peer_id.parse().unwrap()))
            != builtin_ipv4(PeerId::from_libp2p(alice.peer_id.parse().unwrap()))
        {
            break candidate;
        }
    };
    let bridge = NodeIdentity::generate_ed25519().unwrap();
    let bridge_address = free_address();
    let backend = free_address();
    let alice_listener = free_address();
    let direct = Proxy::new(Ipv4Addr::LOCALHOST, backend, Link::Up).await;
    let relay_base = format!(
        "/ip4/127.0.0.1/tcp/{}/p2p/{}/p2p-circuit",
        bridge_address.port(),
        bridge.peer_id
    );
    let bob_relay = format!("{relay_base}/p2p/{}", bob.peer_id);
    let mut bridge_config = config(&bridge, &alice, bridge_address, vec![]);
    bridge_config.peers.clear();
    bridge_config.network.relay.server = true;
    let mut relay = Node::start(bridge_config);
    let mut a_config = config(
        &alice,
        &bob,
        alice_listener,
        vec![
            format!("/ip4/127.0.0.1/tcp/{}", direct.address.port()),
            bob_relay.clone(),
        ],
    );
    let mut b_config = config(&bob, &alice, backend, vec![]);
    a_config.network.name = "windows-relay-recovery-test".to_owned();
    b_config.network.name = "windows-relay-recovery-test".to_owned();
    b_config.network.relay.reservations = vec![relay_base];
    b_config.network.external_addresses = vec![bob_relay];
    let mut b = Node::start(b_config);
    let mut a = Node::start(a_config);
    let mut sequence = 2_000_000;
    let mut stages = Vec::new();
    stages.push(
        path_stage(
            "bridge_initial_direct",
            "direct_tcp_stream",
            "outbound_direct_tcp_stream_fallback_packets",
            &mut a,
            &mut b,
            &mut sequence,
        )
        .await,
    );
    // Ensure the WAN bridge is prepared before the direct path is lost.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if metric(
                &b.control.status().await.unwrap(),
                "relay_reservations_accepted",
            ) > 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("bridge reservation must exist before the direct outage");
    direct.set(Link::Down);
    stages.push(
        path_stage(
            "bridge_after_direct_link_loss",
            "circuit_relay",
            "outbound_relay_stream_fallback_packets",
            &mut a,
            &mut b,
            &mut sequence,
        )
        .await,
    );
    direct.set(Link::Up);
    stages.push(
        path_stage(
            "bridge_after_direct_link_return",
            "direct_tcp_stream",
            "outbound_direct_tcp_stream_fallback_packets",
            &mut a,
            &mut b,
            &mut sequence,
        )
        .await,
    );
    write_evidence(
        "relay-report",
        &serde_json::json!({"test":"direct_link_loss_uses_relay_bridge_and_returns_to_direct","physical_adapter_changed":false,"public_internet_traversed":false,"real_circuit_relay_protocol":true,"manual_network_changed_calls":0,"packet_io":"in-process TUN packet channels","stages":stages,"relay_metrics":relay.control.status().await.unwrap(),"direct_proxy":direct.snapshot()}),
    );
    for node in [&mut a, &mut b, &mut relay] {
        let _ = node.shutdown.take().unwrap().send(());
    }
    for node in [a, b, relay] {
        tokio::time::timeout(Duration::from_secs(15), node.daemon)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    direct.set(Link::Stop);
    direct.task.await.unwrap();
}
