//! Native AutoNAT ownership must outlive request delivery and pending Swarm dials.
use std::time::Duration;

use futures::{StreamExt as _, future::poll_fn};
use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder, autonat, noise,
    swarm::{DialError, FromSwarm, NetworkBehaviour, SwarmEvent, ToSwarm, behaviour::DialFailure},
    tcp, yamux,
};

const DEADLINE: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_secs(3600);

fn config(boot_delay: Duration, retry_interval: Duration) -> autonat::Config {
    autonat::Config {
        boot_delay,
        retry_interval,
        refresh_interval: QUIET,
        timeout: Duration::from_secs(30),
        use_connected: false,
        only_global_ips: false,
        ..autonat::Config::default()
    }
}

fn host(config: autonat::Config) -> Swarm<autonat::Behaviour> {
    SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|key| autonat::Behaviour::new(key.public().to_peer_id(), config))
        .unwrap()
        .with_swarm_config(|config| config.with_idle_connection_timeout(QUIET))
        .build()
}

async fn listen(swarm: &mut Swarm<autonat::Behaviour>) -> Multiaddr {
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    loop {
        if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
            return address;
        }
    }
}

#[track_caller]
fn assert_no_pending_transports(swarm: &Swarm<autonat::Behaviour>) {
    assert_eq!(swarm.network_info().connection_counters().num_pending(), 0);
}

#[test]
fn cached_servers_and_future_probe_timers_are_not_pending_work() {
    let mut native = autonat::Behaviour::new(PeerId::random(), config(QUIET, QUIET));
    assert!(!native.has_pending_work());
    native.add_server(
        PeerId::random(),
        Some("/ip4/127.0.0.1/tcp/9".parse().unwrap()),
    );
    native.probe_address("/ip4/127.0.0.1/tcp/10".parse().unwrap());
    for _ in 0..1000 {
        assert!(!native.has_pending_work());
    }
}

#[tokio::test]
async fn disconnected_probe_and_undelivered_failure_remain_busy() {
    tokio::time::timeout(DEADLINE, async {
        let peer = PeerId::random();
        let mut native = autonat::Behaviour::new(PeerId::random(), config(Duration::ZERO, QUIET));
        native.add_server(peer, Some("/ip4/127.0.0.1/tcp/9".parse().unwrap()));
        native.probe_address("/ip4/127.0.0.1/tcp/10".parse().unwrap());
        assert!(!native.has_pending_work());

        let request = poll_fn(|cx| NetworkBehaviour::poll(&mut native, cx)).await;
        assert!(matches!(
            request,
            ToSwarm::GenerateEvent(autonat::Event::OutboundProbe(
                autonat::OutboundProbeEvent::Request { peer: owner, .. }
            )) if owner == peer
        ));
        assert!(native.has_pending_work());

        let ToSwarm::Dial { opts } = poll_fn(|cx| NetworkBehaviour::poll(&mut native, cx)).await
        else {
            panic!("expected the native disconnected request to select a dial");
        };
        NetworkBehaviour::on_swarm_event(
            &mut native,
            FromSwarm::DialFailure(DialFailure {
                peer_id: Some(peer),
                connection_id: opts.connection_id(),
                error: &DialError::NoAddresses,
            }),
        );
        assert!(native.has_pending_work(), "failure has not been delivered");

        let failure = poll_fn(|cx| NetworkBehaviour::poll(&mut native, cx)).await;
        assert!(matches!(
            failure,
            ToSwarm::GenerateEvent(autonat::Event::OutboundProbe(
                autonat::OutboundProbeEvent::Error {
                    peer: Some(owner),
                    error: autonat::OutboundProbeError::OutboundRequest(_),
                    ..
                }
            )) if owner == peer
        ));
        assert!(
            !native.has_pending_work(),
            "a scheduled retry is not live work"
        );
    })
    .await
    .expect("disconnected AutoNAT work deadline");
}

#[tokio::test]
async fn established_tcp_probe_is_busy_before_dial_back_and_drains_after_completion() {
    tokio::time::timeout(DEADLINE, async {
        let mut client = host(config(Duration::ZERO, Duration::from_millis(50)));
        let mut server = host(config(QUIET, QUIET));
        let client_address = listen(&mut client).await;
        let server_address = listen(&mut server).await;
        let client_peer = *client.local_peer_id();
        let server_peer = *server.local_peer_id();
        client.dial(server_address).unwrap();
        while !client.is_connected(&server_peer) || !server.is_connected(&client_peer) {
            tokio::select! {
                _ = client.select_next_some() => {},
                _ = server.select_next_some() => {},
            }
        }
        assert_no_pending_transports(&client);
        assert_no_pending_transports(&server);
        assert!(!client.behaviour().has_pending_work());
        assert!(!server.behaviour().has_pending_work());

        // Registration happens only after TCP is established; no probe can open it.
        client.behaviour_mut().add_server(server_peer, None);
        client.behaviour_mut().probe_address(client_address);
        loop {
            tokio::select! {
                event = client.select_next_some() => {
                    if let SwarmEvent::Behaviour(autonat::Event::OutboundProbe(
                        autonat::OutboundProbeEvent::Request { peer, .. }
                    )) = event {
                        assert_eq!(peer, server_peer);
                        break;
                    }
                }
                _ = server.select_next_some() => {},
            }
        }
        client.behaviour_mut().remove_server(&server_peer);
        assert_no_pending_transports(&client);
        assert!(client.behaviour().has_pending_work());

        loop {
            tokio::select! {
                _ = client.select_next_some() => {},
                event = server.select_next_some() => {
                    if let SwarmEvent::Behaviour(autonat::Event::InboundProbe(
                        autonat::InboundProbeEvent::Request { peer, .. }
                    )) = event {
                        assert_eq!(peer, client_peer);
                        break;
                    }
                }
            }
        }
        // Native AutoNAT has queued the dial-back, but Swarm has not admitted it yet.
        for _ in 0..1000 {
            assert_no_pending_transports(&client);
            assert_no_pending_transports(&server);
            assert!(client.behaviour().has_pending_work());
            assert!(server.behaviour().has_pending_work());
        }

        finish_probe(&mut client, &mut server).await;
        assert_no_pending_transports(&client);
        assert_no_pending_transports(&server);
        assert!(client.is_connected(&server_peer));
        assert!(server.is_connected(&client_peer));
        assert!(!client.behaviour().has_pending_work());
        assert!(!server.behaviour().has_pending_work());
    })
    .await
    .expect("established TCP AutoNAT work deadline");
}

async fn finish_probe(
    client: &mut Swarm<autonat::Behaviour>,
    server: &mut Swarm<autonat::Behaviour>,
) {
    let client_peer = *client.local_peer_id();
    let server_peer = *server.local_peer_id();
    let (mut answered, mut dialed_back, mut status_delivered) = (false, false, false);
    // The reply can finish before both Swarms admit the dial-back transport.
    while !answered
        || !dialed_back
        || !status_delivered
        || client.behaviour().has_pending_work()
        || server.behaviour().has_pending_work()
        || client.network_info().connection_counters().num_pending() > 0
        || server.network_info().connection_counters().num_pending() > 0
    {
        tokio::select! {
            event = client.select_next_some() => match event {
                SwarmEvent::Behaviour(autonat::Event::OutboundProbe(
                    autonat::OutboundProbeEvent::Response { peer, .. }
                )) => {
                    assert_eq!(peer, server_peer);
                    assert!(client.behaviour().has_pending_work(), "status/address actions are still queued");
                    answered = true;
                }
                SwarmEvent::Behaviour(autonat::Event::StatusChanged {
                    new: autonat::NatStatus::Public(_), ..
                }) => status_delivered = true,
                SwarmEvent::Behaviour(autonat::Event::OutboundProbe(
                    autonat::OutboundProbeEvent::Error { peer: Some(peer), error, .. }
                )) if peer == server_peer => panic!("native probe failed: {error:?}"),
                SwarmEvent::ConnectionClosed { .. } => panic!("client TCP connection closed during probe"),
                _ => {},
            },
            event = server.select_next_some() => match event {
                SwarmEvent::Behaviour(autonat::Event::InboundProbe(
                    autonat::InboundProbeEvent::Response { peer, .. }
                )) => {
                    assert_eq!(peer, client_peer);
                    assert!(server.behaviour().has_pending_work(), "reply/terminal work has not drained");
                    dialed_back = true;
                }
                SwarmEvent::Behaviour(autonat::Event::InboundProbe(
                    autonat::InboundProbeEvent::Error { error, .. }
                )) => panic!("native dial-back failed: {error:?}"),
                SwarmEvent::ConnectionClosed { .. } => panic!("server TCP connection closed during probe"),
                _ => {},
            },
            () = tokio::time::sleep(Duration::from_millis(1)) => {},
        }
    }
}
