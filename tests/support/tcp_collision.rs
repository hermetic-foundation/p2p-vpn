use super::*;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use serde_json::json;

pub const TEST_NAME: &str = "tun_namespace_tcp_simultaneous_dial_diagnostic";

pub fn run() {
    for fresh_port in [false, true] {
        for repetition in 0..3 {
            let temp =
                init_namespace_temp_dir(&env::temp_dir().join("p2p-vpn-rm1-tcp-v2"), TEST_NAME);
            fs::write(
                temp.join("fresh-port"),
                if fresh_port { "true" } else { "false" },
            )
            .unwrap();
            let identities = [
                NodeIdentity::generate_ed25519().unwrap(),
                NodeIdentity::generate_ed25519().unwrap(),
            ];
            let nodes = ["a", "b"].map(|role| {
                let index = usize::from(role == "b");
                spawn_node(
                    TEST_NAME,
                    role,
                    &identities[index],
                    Some(&identities[1 - index]),
                    None,
                    &temp,
                    &temp.join("start"),
                )
            });
            for node in &nodes {
                wait_for_child_namespace(node.id());
            }
            configure_underlay(nodes[0].id(), nodes[1].id());
            for (node, interface) in nodes.iter().zip(["veth-a", "veth-b"]) {
                ns_command(
                    node.id(),
                    "tc",
                    &[
                        "qdisc", "add", "dev", interface, "root", "netem", "delay", "100ms",
                    ],
                );
            }
            fs::write(temp.join("start"), b"ready").unwrap();
            for role in ["a", "b"] {
                wait_for_file(&temp.join(format!("ready-{role}")));
            }
            fs::write(temp.join("dial"), b"ready").unwrap();
            for role in ["a", "b"] {
                let path = temp.join(format!("result-{role}.json"));
                wait_for_file(&path);
                let result: serde_json::Value =
                    serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
                eprintln!(
                    "tcp_collision_v2 fresh_port={fresh_port} repetition={repetition} role={role} result={result}"
                );
            }
            drop(nodes);
            eprintln!("tcp_collision_v2 artifacts={}", temp.display());
        }
    }
}

pub fn run_node() {
    let role = required_env("P2P_VPN_TUN_E2E_ROLE");
    let temp = PathBuf::from(required_env("P2P_VPN_TUN_E2E_TEMP"));
    wait_for_file(&temp.join("start"));
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let local = NodeIdentity { peer_id: required_env("P2P_VPN_TUN_E2E_LOCAL_PEER"), private_key: required_env("P2P_VPN_TUN_E2E_LOCAL_KEY") };
        let remote = required_env("P2P_VPN_TUN_E2E_REMOTE_PEER").parse().unwrap();
        let last = if role == "a" { 1 } else { 2 };
        let mut node = build_node(&HostConfig {
            identity: local, network_name: "tcp-collision".to_owned(), membership_tag: None, mtu: 1280,
            max_concurrent_control_streams: 64, max_concurrent_packet_streams: 256,
            listen_addresses: vec![format!("/ip4/10.250.0.{last}/tcp/4001").parse().unwrap()],
            external_addresses: Vec::new(), bootstrap_peers: Vec::new(), known_peers: Vec::new(), relay_reservations: Vec::new(), relay_server: false,
            relay_resources: RelayResourceConfig::default(), resources: ResourceConfig::default(),
            discovery: DiscoveryConfig { mdns: false, kademlia: false, autonat: false, dcutr: false, ..DiscoveryConfig::default() },
        }).unwrap();
        while !matches!(node.swarm.next().await, Some(SwarmEvent::NewListenAddr { .. })) {}
        fs::write(temp.join(format!("ready-{role}")), b"ready").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !temp.join("dial").exists() { tokio::time::sleep(Duration::from_millis(1)).await; }
        }).await.expect("dial rendezvous");
        let options = DialOpts::peer_id(remote).condition(PeerCondition::NotDialing)
            .addresses(vec![format!("/ip4/10.250.0.{}/tcp/4001", 3-last).parse().unwrap()]);
        let options = if fs::read_to_string(temp.join("fresh-port")).unwrap() == "true" { options.allocate_new_port() } else { options };
        node.swarm.dial(options.build()).unwrap();
        let started = Instant::now();
        let mut established = Vec::new();
        let mut errors = Vec::new();
        let mut incoming_errors = 0;
        let deadline = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                event = node.swarm.next() => match event.unwrap() {
                    SwarmEvent::ConnectionEstablished { peer_id, endpoint, .. } => {
                        assert_eq!(peer_id, remote);
                        established.push(json!({"elapsed_seconds":started.elapsed().as_secs_f64(),"endpoint":format!("{endpoint:?}")}));
                    }
                    SwarmEvent::OutgoingConnectionError { error, .. } => errors.push(json!({"elapsed_seconds":started.elapsed().as_secs_f64(),"error":error.to_string()})),
                    SwarmEvent::IncomingConnectionError { .. } => incoming_errors += 1,
                    _ => {}
                }
            }
            assert!(established.len() <= 2 && errors.len() <= 1 && incoming_errors <= 2);
        }
        let result = json!({"established":established,"outgoing_errors":errors,"incoming_errors":incoming_errors,"connected":node.swarm.is_connected(&remote)});
        let bytes = serde_json::to_vec(&result).unwrap();
        assert!(bytes.len() <= 8192);
        let temporary = temp.join(format!("result-{role}.tmp"));
        fs::write(&temporary, bytes).unwrap();
        fs::rename(temporary, temp.join(format!("result-{role}.json"))).unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
    });
}
