use super::*;

#[test]
fn activated_quic_retires_and_recovers_through_relay() {
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

fn datagrams(proxies: &[quic::UdpProxy; 2]) -> u64 {
    proxies
        .iter()
        .map(|proxy| proxy.snapshot()["forwarded_datagrams"].as_u64().unwrap())
        .sum()
}

async fn quiet_interval(
    a: &Node,
    b: &Node,
    peers: [&str; 2],
    proxies: &[quic::UdpProxy; 2],
    stage: &str,
) {
    tokio::time::sleep(Duration::from_secs(5)).await;
    let before = datagrams(proxies);
    let probes = [
        quic::metric(
            &a.control.status().await.unwrap(),
            "outbound_path_probes_sent",
        ),
        quic::metric(
            &b.control.status().await.unwrap(),
            "outbound_path_probes_sent",
        ),
    ];
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(15) {
        for (node, peer) in [(a, peers[0]), (b, peers[1])] {
            let lines = state(node).await;
            assert!(lifecycle_line(&lines, peer).ends_with("maintenance_paused true quiet true"));
            assert!(lines.iter().any(|line| line == "connected_overlay_peers 0"));
            assert!(
                lines
                    .iter()
                    .any(|line| line == "packet_plane_quic_sessions 0")
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let after = datagrams(proxies);
    let final_probes = [
        quic::metric(
            &a.control.status().await.unwrap(),
            "outbound_path_probes_sent",
        ),
        quic::metric(
            &b.control.status().await.unwrap(),
            "outbound_path_probes_sent",
        ),
    ];
    write_evidence(
        stage,
        &serde_json::json!({
            "sample_seconds":started.elapsed().as_secs_f64(),"terminal_drain_seconds":5,
            "proxy_datagrams_before":before,"proxy_datagrams_after":after,
            "probes_before":probes,"probes_after":final_probes,
            "a_state":state(a).await,"b_state":state(b).await,
        }),
    );
    assert_eq!(
        after, before,
        "dormant packet planes must not generate keepalives or handshakes"
    );
    assert_eq!(
        final_probes, probes,
        "path probes must not wake dormant peers"
    );
    eprintln!(
        "LIFECYCLE {stage}: no proxied datagrams, probes, or recreated sessions for 15 seconds"
    );
}

async fn relay_wake(sender: &Node, destination: &mut Node, sequence: u64, stage: &str) {
    let before = sender.control.status().await.unwrap();
    first_packet(sender, destination, sequence, stage).await;
    let after = sender.control.status().await.unwrap();
    assert!(
        quic::metric(&after, "outbound_relay_stream_fallback_packets")
            > quic::metric(&before, "outbound_relay_stream_fallback_packets")
    );
    assert_eq!(
        quic::metric(&after, "outbound_owned_quic_datagram_packets"),
        quic::metric(&before, "outbound_owned_quic_datagram_packets"),
        "blackholed QUIC must not deliver the first wake packet"
    );
}

async fn scenario() {
    let guards =
        std::array::from_fn::<_, 2, _>(|_| std::net::UdpSocket::bind("127.0.0.1:0").unwrap());
    let backends = guards.each_ref().map(|socket| socket.local_addr().unwrap());
    let proxies = [
        quic::UdpProxy::new(
            std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
            backends[0],
        )
        .await,
        quic::UdpProxy::new(
            std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
            backends[1],
        )
        .await,
    ];
    let endpoints = std::array::from_fn(|index| {
        (
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, backends[index].port())),
            proxies[index].address,
        )
    });
    drop(guards);
    let (mut a, mut b, relay, peers) = start_pair([true, true], Some(endpoints));
    let mut sequence = 4_000_000;
    quic::quic_stage("lifecycle_quic_initial", &mut a, &mut b, &mut sequence).await;
    for (node, peer, proxy) in [(&a, &peers[0], &proxies[1]), (&b, &peers[1], &proxies[0])] {
        let capabilities = node.control.capabilities().await.unwrap();
        let peer = PeerId::from_libp2p(peer.parse().unwrap());
        let prefix = format!("remote capability owned quic packet endpoint candidate: {peer} ");
        let candidates = capabilities
            .iter()
            .filter_map(|line| line.strip_prefix(&prefix))
            .collect::<Vec<_>>();
        assert_eq!(
            candidates,
            [proxy.address.to_string()],
            "fixture must not advertise a fault-proxy bypass"
        );
    }
    let peer_refs = [&peers[0][..], &peers[1][..]];
    for (reverse, name, wake_sequence) in [
        (false, "lifecycle_quic_forward", 4_100_000),
        (true, "lifecycle_quic_reverse", 4_200_000),
    ] {
        await_quiet(&a, &b, peer_refs, &format!("{name}_idle")).await;
        quiet_interval(
            &a,
            &b,
            peer_refs,
            &proxies,
            &format!("{name}_quiet_interval"),
        )
        .await;
        for proxy in &proxies {
            proxy.set(Link::Blackhole);
        }
        if reverse {
            relay_wake(&b, &mut a, wake_sequence, &format!("{name}_first_packet")).await;
        } else {
            relay_wake(&a, &mut b, wake_sequence, &format!("{name}_first_packet")).await;
        }
        for proxy in &proxies {
            proxy.set(Link::Up);
        }
        quic::quic_stage(&format!("{name}_promotion"), &mut a, &mut b, &mut sequence).await;
    }
    write_evidence(
        "lifecycle-quic-report",
        &serde_json::json!({
            "real_negotiation":true,"virtual_time":false,"injected_quiet":false,
            "public_internet_traversed":false,"wildcard_listeners":true,
            "proxies":proxies.iter().map(quic::UdpProxy::snapshot).collect::<Vec<_>>(),
            "a_state":state(&a).await,"b_state":state(&b).await,
        }),
    );
    stop_pair([a, b, relay]).await;
    for proxy in &proxies {
        proxy.set(Link::Stop);
    }
    for proxy in proxies {
        proxy.task.await.unwrap();
    }
}
