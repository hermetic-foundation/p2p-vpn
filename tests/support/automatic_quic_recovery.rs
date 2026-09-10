use super::*;

pub(super) const TEST_NAME: &str = "tun_namespace_minimal_quic_recovers_after_packet_block";
pub(super) const STARTUP_TEST_NAME: &str =
    "tun_namespace_minimal_quic_blocked_from_startup_recovers";
pub(super) const MOVEMENT_TEST_NAME: &str = "tun_namespace_minimal_quic_follows_lan_address_change";

pub(super) fn movement_identities() -> (NodeIdentity, NodeIdentity) {
    let roles = env::var("P2P_VPN_MOVEMENT_INITIATORS").unwrap_or_else(|_| "ab".to_owned());
    assert!(matches!(roles.as_str(), "aa" | "ab" | "ba" | "bb"));
    // First character selects TCP's preferred initiator; second selects packet negotiation.
    for _ in 0..256 {
        let a = NodeIdentity::generate_ed25519().expect("node A identity");
        let b = NodeIdentity::generate_ed25519().expect("node B identity");
        let a_id = a.peer_id.parse::<libp2p::PeerId>().unwrap();
        let b_id = b.peer_id.parse::<libp2p::PeerId>().unwrap();
        let tcp_a = a_id.to_bytes() < b_id.to_bytes();
        let packet_a = p2p_vpn::PeerId::from_libp2p(a_id).as_bytes()
            < p2p_vpn::PeerId::from_libp2p(b_id).as_bytes();
        if tcp_a == (roles.as_bytes()[0] == b'a') && packet_a == (roles.as_bytes()[1] == b'a') {
            eprintln!(
                "movement initiators: TCP={}, packet={}",
                &roles[..1],
                &roles[1..]
            );
            return (a, b);
        }
    }
    panic!("could not generate requested initiator roles {roles}");
}

pub(super) fn move_address_and_return(
    temp_dir: &Path,
    node_a: u32,
    node_b: u32,
    address_b: Ipv4Addr,
) {
    for (stage, old, new) in [
        ("lan-address-changed", "10.250.0.2", "10.250.0.3"),
        ("lan-address-returned", "10.250.0.3", "10.250.0.2"),
    ] {
        ns_command(
            node_b,
            "ip",
            &["addr", "del", &format!("{old}/24"), "dev", "veth-b"],
        );
        ns_command(
            node_b,
            "ip",
            &["addr", "add", &format!("{new}/24"), "dev", "veth-b"],
        );
        let endpoint = format!("endpoint {new}:");
        let lines = wait_for_daemon_state(temp_dir, "a", Duration::from_secs(60), stage, |lines| {
            lines.iter().any(|line| {
                line.starts_with("packet_plane_quic_connection ") && line.contains(&endpoint)
            })
        });
        fs::write(temp_dir.join(format!("{stage}-a.txt")), lines.join("\n"))
            .expect("save changed endpoint");
        for role in ["a", "b"] {
            wait_for_selected_path(temp_dir, role, "direct_quic_datagram");
        }
        let before = payload_counts(temp_dir, PacketPlaneDatagramEvidence::OwnedQuic);
        traffic(temp_dir, node_a, address_b, stage);
        assert_payload_growth(temp_dir, PacketPlaneDatagramEvidence::OwnedQuic, before);
    }
}

pub(super) fn block_before_discovery(temp_dir: &Path, node_b: u32) -> String {
    let lines = state(temp_dir, "b");
    assert_eq!(
        state_metric_count(&lines, "packet_plane_quic_sessions"),
        Some(0)
    );
    let endpoint = lines
        .iter()
        .find_map(|line| line.strip_prefix("packet_plane_quic_listener "))
        .expect("QUIC listener")
        .parse::<std::net::SocketAddr>()
        .expect("socket");
    let port = endpoint.port().to_string();
    ns_command(
        node_b,
        "iptables",
        &["-I", "INPUT", "-p", "udp", "--dport", &port, "-j", "DROP"],
    );
    fs::write(temp_dir.join("startup-block-b.txt"), lines.join("\n")).expect("save state");
    port
}

pub(super) fn finish_startup_block(
    temp_dir: &Path,
    node_a: u32,
    node_b: u32,
    address_b: Ipv4Addr,
    port: &str,
) {
    for role in ["a", "b"] {
        wait_for_selected_path(temp_dir, role, "direct_udp_datagram");
        let lines = state(temp_dir, role);
        assert_eq!(
            state_metric_count(&lines, "packet_plane_quic_sessions"),
            Some(0)
        );
        fs::write(
            temp_dir.join(format!("startup-fallback-{role}.txt")),
            lines.join("\n"),
        )
        .expect("save state");
    }
    assert_eq!(
        payload_counts(temp_dir, PacketPlaneDatagramEvidence::OwnedQuic),
        [0, 0]
    );
    let udp_before = payload_counts(temp_dir, PacketPlaneDatagramEvidence::OwnedUdp);
    traffic(temp_dir, node_a, address_b, "startup-udp-fallback");
    assert_payload_growth(temp_dir, PacketPlaneDatagramEvidence::OwnedUdp, udp_before);
    ns_command(
        node_b,
        "iptables",
        &["-D", "INPUT", "-p", "udp", "--dport", port, "-j", "DROP"],
    );
    for role in ["a", "b"] {
        wait_for_selected_path(temp_dir, role, "direct_quic_datagram");
    }
    let quic_before = payload_counts(temp_dir, PacketPlaneDatagramEvidence::OwnedQuic);
    traffic(temp_dir, node_a, address_b, "startup-quic-restored");
    assert_payload_growth(
        temp_dir,
        PacketPlaneDatagramEvidence::OwnedQuic,
        quic_before,
    );
}

fn state(temp_dir: &Path, role: &str) -> Vec<String> {
    wait_for_daemon_state(
        temp_dir,
        role,
        Duration::from_secs(5),
        "QUIC recovery snapshot",
        |_| true,
    )
}

fn traffic(temp_dir: &Path, pid: u32, address: Ipv4Addr, stage: &str) {
    let output = ping_from_namespace(pid, "hse2ea", address);
    fs::write(temp_dir.join(format!("{stage}-ping.txt")), &output.stdout).expect("save ping");
    assert!(
        output.status.success()
            && recovery_soak::complete_ping(&String::from_utf8_lossy(&output.stdout)),
        "{stage} did not deliver all five packets: {output:?}"
    );
    let mtu = ["a", "b"]
        .into_iter()
        .map(|role| {
            state(temp_dir, role)
                .iter()
                .find_map(|line| {
                    line.strip_prefix("peer state: ")?
                        .split_once(" selected_path_mtu ")?
                        .1
                        .split_whitespace()
                        .next()?
                        .parse::<usize>()
                        .ok()
                })
                .expect("selected path MTU")
        })
        .min()
        .unwrap();
    assert!(mtu >= 576, "unexpected IPv4 path MTU {mtu}");
    let output = ns_command_output(
        pid,
        "ping",
        &[
            "-c",
            "5",
            "-W",
            "2",
            "-I",
            "hse2ea",
            "-M",
            "do",
            "-s",
            &(mtu - 28).to_string(),
            &address.to_string(),
        ],
    );
    fs::write(
        temp_dir.join(format!("{stage}-mtu-ping.txt")),
        &output.stdout,
    )
    .expect("save MTU ping");
    assert!(
        output.status.success()
            && recovery_soak::complete_ping(&String::from_utf8_lossy(&output.stdout)),
        "{stage} did not deliver all five {mtu}-byte IPv4 packets: {output:?}"
    );
}

fn payload_counts(temp_dir: &Path, backend: PacketPlaneDatagramEvidence) -> [usize; 2] {
    ["a", "b"].map(|role| {
        state_metric_count(&state(temp_dir, role), backend.payload_metric())
            .expect("backend payload counter")
    })
}

fn assert_payload_growth(
    temp_dir: &Path,
    backend: PacketPlaneDatagramEvidence,
    before: [usize; 2],
) {
    let after = payload_counts(temp_dir, backend);
    for (before, after) in before.into_iter().zip(after) {
        assert!(
            after >= before + 10,
            "{} did not send ten payloads: {before} -> {after}",
            backend.context()
        );
    }
}

pub(super) fn capture(temp_dir: &Path, node_a: u32, node_b: u32, address_b: Ipv4Addr) {
    let mut ports = Vec::new();
    for (role, pid) in [("a", node_a), ("b", node_b)] {
        let lines = state(temp_dir, role);
        fs::write(
            temp_dir.join(format!("before-block-{role}.txt")),
            lines.join("\n"),
        )
        .expect("save state");
        let endpoint = lines
            .iter()
            .find_map(|line| line.strip_prefix("packet_plane_quic_listener "))
            .expect("QUIC listener")
            .parse::<std::net::SocketAddr>()
            .expect("socket");
        let port = endpoint.port().to_string();
        ns_command(
            pid,
            "iptables",
            &["-I", "INPUT", "-p", "udp", "--dport", &port, "-j", "DROP"],
        );
        ports.push((pid, port));
    }
    for role in ["a", "b"] {
        wait_for_selected_path(temp_dir, role, "direct_udp_datagram");
    }
    let before_udp = payload_counts(temp_dir, PacketPlaneDatagramEvidence::OwnedUdp);
    traffic(temp_dir, node_a, address_b, "blocked-quic-udp-fallback");
    assert_payload_growth(temp_dir, PacketPlaneDatagramEvidence::OwnedUdp, before_udp);
    for role in ["a", "b"] {
        let lines = state(temp_dir, role);
        fs::write(
            temp_dir.join(format!("blocked-{role}.txt")),
            lines.join("\n"),
        )
        .expect("save state");
    }
    let before = payload_counts(temp_dir, PacketPlaneDatagramEvidence::OwnedQuic);
    for (pid, port) in ports {
        ns_command(
            pid,
            "iptables",
            &["-D", "INPUT", "-p", "udp", "--dport", &port, "-j", "DROP"],
        );
    }
    for role in ["a", "b"] {
        wait_for_selected_path(temp_dir, role, "direct_quic_datagram");
    }
    traffic(temp_dir, node_a, address_b, "restored-quic");
    assert_payload_growth(temp_dir, PacketPlaneDatagramEvidence::OwnedQuic, before);
    let after = state(temp_dir, "a");
    fs::write(temp_dir.join("restored-a.txt"), after.join("\n")).expect("save state");
}
