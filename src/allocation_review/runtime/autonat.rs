use super::require_isolated_network;
use crate::allocation_review::{delta, snapshot};
use futures::StreamExt;
use libp2p::{Swarm, SwarmBuilder, autonat, noise, swarm::SwarmEvent, tcp, yamux};
use std::time::{Duration, Instant};

fn swarm(probe: bool) -> Swarm<autonat::Behaviour> {
    let interval = Duration::from_secs(if probe { 1 } else { 3600 });
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(move |key| {
            autonat::Behaviour::new(
                key.public().to_peer_id(),
                autonat::Config {
                    boot_delay: interval,
                    retry_interval: interval,
                    refresh_interval: interval,
                    throttle_server_period: Duration::ZERO,
                    use_connected: false,
                    ..Default::default()
                },
            )
        })
        .unwrap()
        .with_swarm_config(|config| config.with_idle_connection_timeout(Duration::from_secs(60)))
        .build()
}

#[test]
#[ignore = "allocation review: isolated network namespace, fresh process, one test thread"]
fn measure_autonat_refusal_ownership() {
    require_isolated_network();
    let probe = match std::env::var("P2P_VPN_REVIEW_AUTONAT_MODE").as_deref() {
        Ok("probe") => true,
        Ok("control") => false,
        other => panic!("expected explicit probe/control mode: {other:?}"),
    };
    // Initialize the process-global timer separately, as in the runtime baseline.
    futures::executor::block_on(futures_timer::Delay::new(Duration::from_millis(1)));
    let mut rows = Vec::with_capacity(16);
    let before = snapshot();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let counts = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(25), async {
            let mut client = swarm(probe);
            let mut server = swarm(false);
            server.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
            let address = loop {
                if let SwarmEvent::NewListenAddr { address, .. } = server.select_next_some().await {
                    break address;
                }
            };
            let server_id = *server.local_peer_id();
            let client_id = *client.local_peer_id();
            client.behaviour_mut().add_server(server_id, Some(address.clone()));
            client.listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap()).unwrap();
            client.dial(address).unwrap();
            while !(client.is_connected(&server_id) && server.is_connected(&client_id)) {
                tokio::select! {
                    _ = client.select_next_some() => {},
                    _ = server.select_next_some() => {},
                }
            }
            let started = Instant::now();
            rows.push(("connected", 0, started.elapsed().as_millis(), snapshot()));
            let mut requests = 0;
            let mut outbound = 0;
            let mut inbound = 0;
            for round in 1..=10 {
                let deadline = tokio::time::sleep(Duration::from_secs(1));
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        event = client.select_next_some() => match event {
                            SwarmEvent::Behaviour(autonat::Event::OutboundProbe(autonat::OutboundProbeEvent::Request { .. })) => requests += 1,
                            SwarmEvent::Behaviour(autonat::Event::OutboundProbe(autonat::OutboundProbeEvent::Error { error, .. })) => {
                                assert!(matches!(error, autonat::OutboundProbeError::Response(autonat::ResponseError::DialRefused)), "{error:?}");
                                outbound += 1;
                            },
                            SwarmEvent::ConnectionClosed { .. } => panic!("client connection closed during measurement"),
                            _ => {},
                        },
                        event = server.select_next_some() => match event {
                            SwarmEvent::Behaviour(autonat::Event::InboundProbe(autonat::InboundProbeEvent::Error { error, .. })) => {
                                assert!(matches!(error, autonat::InboundProbeError::Response(autonat::ResponseError::DialRefused)), "{error:?}");
                                inbound += 1;
                            },
                            SwarmEvent::ConnectionClosed { .. } => panic!("server connection closed during measurement"),
                            _ => {},
                        },
                        () = &mut deadline, if !probe => break,
                    }
                    if probe && outbound == round && inbound == round { break; }
                }
                // Poll both sides through response-sent cleanup before taking a snapshot.
                let settle = tokio::time::sleep(Duration::from_millis(100));
                tokio::pin!(settle);
                loop {
                    tokio::select! {
                        event = client.select_next_some() => assert!(
                            !matches!(event, SwarmEvent::Behaviour(_) | SwarmEvent::ConnectionClosed { .. }),
                            "unexpected client event during settling: {event:?}"
                        ),
                        event = server.select_next_some() => assert!(
                            !matches!(event, SwarmEvent::Behaviour(_) | SwarmEvent::ConnectionClosed { .. }),
                            "unexpected server event during settling: {event:?}"
                        ),
                        () = &mut settle => break,
                    }
                }
                rows.push(("round", round, started.elapsed().as_millis(), snapshot()));
            }
            assert_eq!((requests, outbound, inbound), if probe { (10, 10, 10) } else { (0, 0, 0) });
            client.disconnect_peer_id(server_id).unwrap();
            while client.is_connected(&server_id) || server.is_connected(&client_id) {
                tokio::select! {
                    _ = client.select_next_some() => {},
                    _ = server.select_next_some() => {},
                }
            }
            rows.push(("connections_closed", 10, started.elapsed().as_millis(), snapshot()));
            drop(client);
            drop(server);
            tokio::time::sleep(Duration::from_millis(100)).await;
            rows.push(("swarms_dropped", 10, started.elapsed().as_millis(), snapshot()));
            (requests, outbound, inbound)
        }).await.expect("fixed 25-second diagnostic deadline")
    });
    drop(runtime);
    let dropped = snapshot();
    let rows: Vec<_> = rows.into_iter().map(|(stage, round, elapsed_millis, stats)| {
        serde_json::json!({"stage":stage,"round":round,"elapsed_millis":elapsed_millis,"since_start":delta(before,stats)})
    }).collect();
    eprintln!(
        "autonat_ownership_sample {}",
        serde_json::json!({
            "schema_version":1,"mode":if probe {"probe"} else {"control"},
            "counts":counts,"rows":rows,"runtime_dropped":delta(before,dropped),
        })
    );
}
