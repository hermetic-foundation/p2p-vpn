use super::*;

#[path = "connection_recovery_lifecycle_quic.rs"]
mod quic_trials;

fn lifecycle_line<'a>(state: &'a [String], peer: &str) -> &'a str {
    let peer = PeerId::from_libp2p(peer.parse().expect("transport peer ID"));
    state
        .iter()
        .find(|line| line.starts_with(&format!("peer lifecycle state: {peer} ")))
        .map(String::as_str)
        .expect("configured peer must have lifecycle diagnostics")
}

async fn state(node: &Node) -> Vec<String> {
    tokio::time::timeout(Duration::from_secs(5), node.control.state())
        .await
        .expect("runtime state must remain responsive")
        .expect("runtime must still be running")
}

async fn await_quiet(a: &Node, b: &Node, peers: [&str; 2], stage: &str) {
    let started = Instant::now();
    loop {
        let states = [state(a).await, state(b).await];
        let quiet = states.iter().zip(peers).all(|(lines, peer)| {
            lifecycle_line(lines, peer).ends_with("maintenance_paused true quiet true")
                && lines.iter().any(|line| line == "connected_overlay_peers 0")
                && lines.iter().any(|line| line == "packet_plane_sessions 0")
                && lines
                    .iter()
                    .any(|line| line == "packet_plane_quic_sessions 0")
        });
        if quiet {
            write_evidence(
                stage,
                &serde_json::json!({"elapsed_seconds":started.elapsed().as_secs_f64(),"states":states}),
            );
            eprintln!(
                "LIFECYCLE {stage}: both negotiated quiet with overlay transports retired after {:.3}s",
                started.elapsed().as_secs_f64()
            );
            return;
        }
        if started.elapsed() >= Duration::from_secs(120) {
            write_evidence(stage, &serde_json::json!({"failed":true,"states":states}));
            panic!(
                "{stage}: bilateral quiet and actual overlay retirement not reached: {states:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn first_packet(sender: &Node, receiver: &mut Node, sequence: u64, stage: &str) {
    let expected = packet(sender.ip, receiver.ip, sequence);
    let started = Instant::now();
    sender.input.send(Some(expected.clone())).unwrap();
    // Observe the documented 30-second retention budget plus scheduling margin.
    // Packet expiry itself is checked at the exact boundary in queue tests.
    let received = tokio::time::timeout(Duration::from_secs(35), async {
        while let Some(received) = receiver.output.recv().await {
            if received == expected {
                return true;
            }
        }
        false
    })
    .await;
    let states = [state(sender).await, state(receiver).await];
    write_evidence(
        stage,
        &serde_json::json!({"delivered":received.as_ref().is_ok_and(|value|*value),"single_packet_sent":true,"destination_application_traffic":false,"elapsed_seconds":started.elapsed().as_secs_f64(),"states":states,
            "sender_metrics":sender.control.status().await.unwrap(),"receiver_metrics":receiver.control.status().await.unwrap()}),
    );
    assert!(
        received.unwrap_or(false),
        "{stage}: single IP packet was not delivered: {states:?}"
    );
    assert!(
        states.iter().all(|lines| lines.iter().any(|line| {
            line.starts_with("peer lifecycle state: ") && line.ends_with("quiet false")
        })),
        "accepted application payload must revoke quiet on both endpoints"
    );
    eprintln!(
        "LIFECYCLE {stage}: single packet delivered in {:.3}s",
        started.elapsed().as_secs_f64()
    );
}

#[test]
fn activated_runners_deliver_first_ip_after_bilateral_dormancy() {
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
    let (mut a, mut b, relay, peers) = start_pair([true, true], None);
    let mut sequence = 3_000_000;
    stage(
        "lifecycle_initial",
        &mut a,
        &mut b,
        &mut sequence,
        Duration::from_secs(45),
    )
    .await;
    await_quiet(&a, &b, [&peers[0], &peers[1]], "lifecycle_first_idle").await;
    first_packet(&a, &mut b, 3_100_000, "lifecycle_first_inbound_b").await;
    await_quiet(&a, &b, [&peers[0], &peers[1]], "lifecycle_second_idle").await;
    first_packet(&b, &mut a, 3_200_000, "lifecycle_first_inbound_a").await;
    write_evidence(
        "lifecycle-report",
        &serde_json::json!({
            "test":"activated_runners_deliver_first_ip_after_bilateral_dormancy",
            "real_runtime_negotiation":true,"injected_readiness":false,"injected_quiet":false,
            "virtual_time":false,"public_internet_traversed":false,
            "a_metrics":a.control.status().await.unwrap(),"b_metrics":b.control.status().await.unwrap(),
        }),
    );
    stop_pair([a, b, relay]).await;
}

fn start_pair(
    enabled: [bool; 2],
    quic: Option<[(SocketAddr, SocketAddr); 2]>,
) -> (Node, Node, Node, [String; 2]) {
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
    let relay_address = free_address();
    let base = format!(
        "/ip4/127.0.0.1/tcp/{}/p2p/{}/p2p-circuit",
        relay_address.port(),
        bridge.peer_id
    );
    let mut bridge_config = config(&bridge, &alice, relay_address, vec![]);
    bridge_config.peers.clear();
    bridge_config.network.relay.server = true;
    let relay = Node::start(bridge_config);
    let make_config = |local: &NodeIdentity, remote: &NodeIdentity, index: usize| {
        let mut config = config(
            local,
            remote,
            free_address(),
            vec![format!("{base}/p2p/{}", remote.peer_id)],
        );
        config.network.relay.reservations = vec![base.clone()];
        config.network.external_addresses = vec![format!("{base}/p2p/{}", local.peer_id)];
        if let Some(endpoints) = quic {
            let (listener, external) = endpoints[index];
            config.network.packet_plane.quic_listen = vec![listener.to_string()];
            config.network.packet_plane.quic_external_endpoints = vec![external.to_string()];
        }
        config
    };
    (
        Node::start_with_lifecycle(make_config(&alice, &bob, 0), enabled[0]),
        Node::start_with_lifecycle(make_config(&bob, &alice, 1), enabled[1]),
        relay,
        [bob.peer_id, alice.peer_id],
    )
}

async fn stop_pair(mut nodes: [Node; 3]) {
    for node in &mut nodes {
        let _ = node.shutdown.take().unwrap().send(());
    }
    for node in nodes {
        tokio::time::timeout(Duration::from_secs(15), node.daemon)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[test]
fn mixed_enabled_passive_runners_keep_legacy_reachability() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let (mut a, mut b, relay, peers) = start_pair([true, false], None);
                    let mut sequence = 3_300_000;
                    stage("lifecycle_mixed_initial", &mut a, &mut b, &mut sequence, Duration::from_secs(45)).await;
                    let started = Instant::now();
                    while started.elapsed() < Duration::from_secs(50) {
                        for (node, peer, supported) in [(&a, &peers[0], false), (&b, &peers[1], true)] {
                            let lines = state(node).await;
                            assert!(lifecycle_line(&lines, peer).ends_with(&format!(
                                "remote_supported {supported} maintenance_paused false quiet false"
                            )), "mixed peer cannot grant quiet: {lines:?}");
                        }
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                    assert!(probe(&mut a, &mut b, 3_400_000, Duration::from_secs(3)).await,
                        "mixed-version traffic must survive idle without a test-side reconnect");
                    write_evidence("lifecycle-mixed-report", &serde_json::json!({
                        "a_state":state(&a).await,"b_state":state(&b).await,
                        "idle_seconds":started.elapsed().as_secs_f64(),"bidirectional_delivery":true,
                    }));
                    stop_pair([a, b, relay]).await;
                });
        })
        .unwrap()
        .join()
        .unwrap();
}
