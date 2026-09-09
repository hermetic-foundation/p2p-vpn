use super::*;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};

pub const TEST_NAME: &str = "tun_namespace_tcp_simultaneous_dial_diagnostic";

pub fn run() {
    for args in [
        vec!["link", "set", "lo", "up"],
        vec![
            "qdisc", "add", "dev", "lo", "root", "netem", "delay", "25ms",
        ],
    ] {
        let tool = if args[0] == "link" { "ip" } else { "tc" };
        let output = command_output(tool, &args, &[], Duration::from_secs(2)).unwrap();
        assert_output_success("isolated loopback setup", &output);
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for fresh_port in [false, true] {
                for repetition in 0..3 {
                    let port = 24000 + repetition + u16::from(fresh_port) * 10;
                    let result = tokio::time::timeout(
                        Duration::from_secs(3),
                        pair(port, fresh_port),
                    )
                    .await;
                    eprintln!("tcp_collision fresh_port={fresh_port} repetition={repetition} result={result:?}");
                }
            }
        });
}

async fn pair(port: u16, fresh_port: bool) -> [String; 2] {
    let mut nodes = [1, 2].map(|last| {
        build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().unwrap(),
            network_name: "tcp-collision".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec![format!("/ip4/127.0.0.{last}/tcp/{port}").parse().unwrap()],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery: DiscoveryConfig {
                mdns: false,
                kademlia: false,
                autonat: false,
                dcutr: false,
                ..DiscoveryConfig::default()
            },
        })
        .unwrap()
    });
    for node in &mut nodes {
        loop {
            if matches!(
                node.swarm.next().await,
                Some(SwarmEvent::NewListenAddr { .. })
            ) {
                break;
            }
        }
    }
    for index in 0..2 {
        let remote = nodes[1 - index].local_peer_id;
        let address = format!("/ip4/127.0.0.{}/tcp/{port}", 2 - index)
            .parse()
            .unwrap();
        let options = DialOpts::peer_id(remote)
            .condition(PeerCondition::NotDialing)
            .addresses(vec![address]);
        let options = if fresh_port {
            options.allocate_new_port()
        } else {
            options
        };
        nodes[index].swarm.dial(options.build()).unwrap();
    }
    let [a, b] = &mut nodes;
    let mut results: [Option<String>; 2] = [None, None];
    while results.iter().any(Option::is_none) {
        let (side, event) = tokio::select! {
            event = a.swarm.next() => (0, event.unwrap()),
            event = b.swarm.next() => (1, event.unwrap()),
        };
        match event {
            SwarmEvent::ConnectionEstablished { endpoint, .. } => {
                results[side] = Some(format!("authenticated {endpoint:?}"));
            }
            SwarmEvent::OutgoingConnectionError { error, .. } => {
                results[side] = Some(format!("outgoing error {error}"));
            }
            _ => {}
        }
    }
    results.map(Option::unwrap)
}
