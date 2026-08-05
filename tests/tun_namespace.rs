use std::{
    env, fs,
    fs::File,
    io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Child, Command, Output},
    thread,
    time::{Duration, Instant},
};

use futures::StreamExt as _;
use libp2p::{Multiaddr, identify, multiaddr::Protocol, relay, swarm::SwarmEvent};
use p2p_vpn::{
    config::{
        Config, DiscoveryConfig, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig,
        RelayConfig, RelayResourceConfig, ResourceConfig, RouteConfig,
    },
    identity::NodeIdentity,
    invite::{
        InviteExportOptions, InviteImportOptions, export_signed_invite_at, import_invite_config_at,
    },
    runtime::{
        control_socket::query_state,
        forward::Forwarder,
        p2p::{BehaviourEvent, HostConfig, build_node},
        packet_plane::{PacketPlaneQuicRuntime, PacketPlaneRuntime},
        runner,
        tun::{TunDevice, TunRuntimeConfig},
    },
};

const CHILD_ENV: &str = "P2P_VPN_TUN_E2E_MODE";
const KEEP_TEMP_ENV: &str = "P2P_VPN_TUN_E2E_KEEP_TEMP";
const DIRECT_TEST_NAME: &str = "tun_namespace_ping_crosses_two_node_overlay";
const DIRECT_QUIC_TEST_NAME: &str = "tun_namespace_ping_crosses_owned_quic_packet_plane";
const MDNS_TEST_NAME: &str = "tun_namespace_ping_crosses_mdns_discovered_overlay";
const RELAY_TEST_NAME: &str = "tun_namespace_ping_crosses_relay_overlay";
const INVITE_RELAY_TEST_NAME: &str = "tun_namespace_invite_import_crosses_relay_overlay";
const RELAY_PROMOTION_TEST_NAME: &str = "tun_namespace_relay_overlay_promotes_to_direct_path";
const DHT_TEST_NAME: &str = "tun_namespace_ping_crosses_dht_discovered_overlay";
const NETWORK_NAME: &str = "tun-e2e";
const NODE_A_LOCAL_ROUTE_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 41, 0, 9);

#[test]
#[ignore = "requires Linux user and network namespaces plus /dev/net/tun"]
fn tun_namespace_ping_crosses_two_node_overlay() {
    match env::var(CHILD_ENV).as_deref() {
        Ok("orchestrator") => run_direct_orchestrator(DIRECT_TEST_NAME),
        Ok("node") => run_node_child(),
        _ => reexec_orchestrator(DIRECT_TEST_NAME),
    }
}

#[test]
#[ignore = "requires Linux user and network namespaces plus /dev/net/tun"]
fn tun_namespace_ping_crosses_owned_quic_packet_plane() {
    match env::var(CHILD_ENV).as_deref() {
        Ok("orchestrator") => run_direct_orchestrator(DIRECT_QUIC_TEST_NAME),
        Ok("node") => run_node_child(),
        _ => reexec_orchestrator(DIRECT_QUIC_TEST_NAME),
    }
}

#[test]
#[ignore = "requires Linux user and network namespaces plus /dev/net/tun"]
fn tun_namespace_ping_crosses_mdns_discovered_overlay() {
    match env::var(CHILD_ENV).as_deref() {
        Ok("orchestrator") => run_mdns_orchestrator(),
        Ok("node") => run_node_child(),
        _ => reexec_orchestrator(MDNS_TEST_NAME),
    }
}

#[test]
#[ignore = "requires Linux user and network namespaces plus /dev/net/tun"]
fn tun_namespace_ping_crosses_relay_overlay() {
    match env::var(CHILD_ENV).as_deref() {
        Ok("orchestrator") => run_relay_orchestrator(),
        Ok("node") => run_node_child(),
        _ => reexec_orchestrator(RELAY_TEST_NAME),
    }
}

#[test]
#[ignore = "requires Linux user and network namespaces plus /dev/net/tun"]
fn tun_namespace_invite_import_crosses_relay_overlay() {
    match env::var(CHILD_ENV).as_deref() {
        Ok("orchestrator") => run_invite_relay_orchestrator(),
        Ok("node") => run_node_child(),
        _ => reexec_orchestrator(INVITE_RELAY_TEST_NAME),
    }
}

#[test]
#[ignore = "requires Linux user and network namespaces plus /dev/net/tun"]
fn tun_namespace_relay_overlay_promotes_to_direct_path() {
    match env::var(CHILD_ENV).as_deref() {
        Ok("orchestrator") => run_relay_promotion_orchestrator(),
        Ok("node") => run_node_child(),
        _ => reexec_orchestrator(RELAY_PROMOTION_TEST_NAME),
    }
}

#[test]
#[ignore = "requires Linux user and network namespaces plus /dev/net/tun"]
fn tun_namespace_ping_crosses_dht_discovered_overlay() {
    match env::var(CHILD_ENV).as_deref() {
        Ok("orchestrator") => run_dht_orchestrator(),
        Ok("node") => run_node_child(),
        _ => reexec_orchestrator(DHT_TEST_NAME),
    }
}

#[test]
fn daemon_snapshot_capture_records_missing_control_sockets() {
    let temp_dir = env::temp_dir().join(format!(
        "p2p-vpn-daemon-snapshot-test-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&temp_dir).expect("create temp dir");

    capture_daemon_snapshots(&temp_dir, &["a"]);

    let state =
        fs::read_to_string(temp_dir.join("daemon-state-a.txt")).expect("daemon state snapshot");
    let status =
        fs::read_to_string(temp_dir.join("daemon-status-a.txt")).expect("daemon status snapshot");
    let summary = daemon_snapshot_summary(&temp_dir, &["a"]);

    assert!(state.contains("socket missing:"));
    assert!(status.contains("socket missing:"));
    assert!(summary.contains("daemon-state-a.txt"));
    assert!(summary.contains("socket missing:"));

    let _ = fs::remove_dir_all(temp_dir);
}

fn reexec_orchestrator(test_name: &str) {
    let current_exe = env::current_exe().expect("current test binary");
    let timeout = if test_name == RELAY_PROMOTION_TEST_NAME {
        Duration::from_secs(150)
    } else {
        Duration::from_secs(90)
    };
    let output = command_output(
        "unshare",
        &[
            "--user",
            "--map-root-user",
            "--mount",
            "--net",
            current_exe.to_str().expect("test binary path is utf-8"),
            "--ignored",
            test_name,
            "--exact",
            "--nocapture",
        ],
        &[(CHILD_ENV, "orchestrator")],
        timeout,
    )
    .expect("failed to execute unshare");

    assert_output_success("unshare tun e2e orchestrator", &output);
}

fn run_direct_orchestrator(test_name: &str) {
    let identity_a = NodeIdentity::generate_ed25519().expect("node A identity");
    let identity_b = NodeIdentity::generate_ed25519().expect("node B identity");
    let temp_dir = env::temp_dir().join(format!("p2p-vpn-{test_name}-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    let start_a = temp_dir.join("start-a");
    let start_b = temp_dir.join("start-b");

    let config_b = if test_name == DIRECT_QUIC_TEST_NAME {
        direct_quic_overlay_config("b", &identity_b, &identity_a)
    } else {
        direct_overlay_config("b", &identity_b, &identity_a)
    };
    let address_b = TunRuntimeConfig::from_config(&config_b)
        .expect("node B TUN config")
        .addresses
        .ipv4;

    let mut node_a = spawn_node(
        test_name,
        "a",
        &identity_a,
        Some(&identity_b),
        None,
        &temp_dir,
        &start_a,
    );
    let mut node_b = spawn_node(
        test_name,
        "b",
        &identity_b,
        Some(&identity_a),
        None,
        &temp_dir,
        &start_b,
    );
    wait_for_child_namespace(node_a.id());
    wait_for_child_namespace(node_b.id());
    configure_underlay(node_a.id(), node_b.id());
    ns_command(node_a.id(), "ping", &["-c", "1", "-W", "2", "10.250.0.2"]);

    fs::write(&start_b, b"start").expect("write node B start file");
    wait_for_file(&temp_dir.join("ready-b"));
    wait_for_daemon_running(&temp_dir, "b");
    fs::write(&start_a, b"start").expect("write node A start file");
    wait_for_file(&temp_dir.join("ready-a"));
    if test_name == DIRECT_QUIC_TEST_NAME {
        wait_for_peer_ready(&temp_dir, "a");
        wait_for_peer_ready(&temp_dir, "b");
        wait_for_owned_quic_packet_plane_sessions(&temp_dir);
    } else {
        wait_for_daemon_running(&temp_dir, "a");
        wait_for_daemon_running(&temp_dir, "b");
    }
    let host_ping = ping_from_namespace(node_a.id(), "hse2ea", address_b);
    let routed_ping = ping_from_namespace(node_b.id(), "hse2eb", NODE_A_LOCAL_ROUTE_ADDRESS);
    let initiator_addresses = ns_command_output(node_a.id(), "ip", &["addr", "show"]);
    let initiator_routes = ns_command_output(node_a.id(), "ip", &["route", "show", "table", "all"]);
    let responder_addresses = ns_command_output(node_b.id(), "ip", &["addr", "show"]);
    let responder_routes = ns_command_output(node_b.id(), "ip", &["route", "show", "table", "all"]);
    if test_name == DIRECT_QUIC_TEST_NAME {
        wait_for_owned_quic_packet_plane_datagrams(&temp_dir);
    } else {
        wait_for_packet_plane_datagrams(&temp_dir);
    }

    stop_child(&mut node_a);
    stop_child(&mut node_b);
    assert_ping_success(
        "overlay host ping",
        &host_ping,
        &temp_dir,
        &initiator_addresses,
        &initiator_routes,
        &responder_addresses,
        &responder_routes,
    );
    assert_ping_success(
        "overlay routed-prefix ping",
        &routed_ping,
        &temp_dir,
        &initiator_addresses,
        &initiator_routes,
        &responder_addresses,
        &responder_routes,
    );
    let initiator_log = read_log(&temp_dir.join("node-a.log"));
    let responder_log = read_log(&temp_dir.join("node-b.log"));
    if test_name == DIRECT_QUIC_TEST_NAME {
        assert_owned_quic_packet_plane_datagrams_used("node A", &initiator_log, &responder_log);
        assert_owned_quic_packet_plane_datagrams_used("node B", &responder_log, &initiator_log);
    } else {
        assert_packet_plane_datagrams_used("node A", &initiator_log, &responder_log);
        assert_packet_plane_datagrams_used("node B", &responder_log, &initiator_log);
    }
    cleanup_temp_dir(temp_dir);
}

fn run_mdns_orchestrator() {
    let identity_a = NodeIdentity::generate_ed25519().expect("node A identity");
    let identity_b = NodeIdentity::generate_ed25519().expect("node B identity");
    let temp_dir = env::temp_dir().join(format!("p2p-vpn-mdns-tun-e2e-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    let start_a = temp_dir.join("start-a");
    let start_b = temp_dir.join("start-b");

    let config_b = mdns_overlay_config("b", &identity_b, &identity_a);
    let address_b = TunRuntimeConfig::from_config(&config_b)
        .expect("node B TUN config")
        .addresses
        .ipv4;

    let mut node_a = spawn_node(
        MDNS_TEST_NAME,
        "a",
        &identity_a,
        Some(&identity_b),
        None,
        &temp_dir,
        &start_a,
    );
    let mut node_b = spawn_node(
        MDNS_TEST_NAME,
        "b",
        &identity_b,
        Some(&identity_a),
        None,
        &temp_dir,
        &start_b,
    );
    wait_for_child_namespace(node_a.id());
    wait_for_child_namespace(node_b.id());
    configure_underlay(node_a.id(), node_b.id());
    ns_command(node_a.id(), "ping", &["-c", "1", "-W", "2", "10.250.0.2"]);

    fs::write(&start_b, b"start").expect("write node B start file");
    wait_for_file(&temp_dir.join("ready-b"));
    wait_for_daemon_running(&temp_dir, "b");
    fs::write(&start_a, b"start").expect("write node A start file");
    wait_for_file(&temp_dir.join("ready-a"));
    wait_for_packet_plane_sessions(&temp_dir, "a");
    wait_for_packet_plane_sessions(&temp_dir, "b");

    let host_ping = ping_from_namespace(node_a.id(), "hse2ea", address_b);
    let initiator_addresses = ns_command_output(node_a.id(), "ip", &["addr", "show"]);
    let initiator_routes = ns_command_output(node_a.id(), "ip", &["route", "show", "table", "all"]);
    let responder_addresses = ns_command_output(node_b.id(), "ip", &["addr", "show"]);
    let responder_routes = ns_command_output(node_b.id(), "ip", &["route", "show", "table", "all"]);
    wait_for_packet_plane_datagrams(&temp_dir);

    stop_child(&mut node_a);
    stop_child(&mut node_b);
    assert_ping_success(
        "mDNS-discovered overlay host ping",
        &host_ping,
        &temp_dir,
        &initiator_addresses,
        &initiator_routes,
        &responder_addresses,
        &responder_routes,
    );
    let initiator_log = read_log(&temp_dir.join("node-a.log"));
    let responder_log = read_log(&temp_dir.join("node-b.log"));
    assert!(
        initiator_log.contains("control capabilities accepted")
            && initiator_log.contains("discovered_address_dial_attempts 1"),
        "node A did not discover and validate node B through mDNS\nnode-a log:\n{initiator_log}\nnode-b log:\n{responder_log}",
    );
    assert_packet_plane_datagrams_used("node A", &initiator_log, &responder_log);
    assert_packet_plane_datagrams_used("node B", &responder_log, &initiator_log);
    cleanup_temp_dir(temp_dir);
}

fn run_relay_orchestrator() {
    let identity_relay = NodeIdentity::generate_ed25519().expect("relay identity");
    let identity_a = NodeIdentity::generate_ed25519().expect("node A identity");
    let identity_b = NodeIdentity::generate_ed25519().expect("node B identity");
    let temp_dir = env::temp_dir().join(format!("p2p-vpn-relay-tun-e2e-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    let start_relay = temp_dir.join("start-relay");
    let start_a = temp_dir.join("start-a");
    let start_b = temp_dir.join("start-b");

    let config_b = relay_overlay_config("b", &identity_b, &identity_a, &identity_relay);
    let address_b = TunRuntimeConfig::from_config(&config_b)
        .expect("node B TUN config")
        .addresses
        .ipv4;

    let mut relay = spawn_node(
        RELAY_TEST_NAME,
        "relay",
        &identity_relay,
        None,
        None,
        &temp_dir,
        &start_relay,
    );
    let mut node_b = spawn_node(
        RELAY_TEST_NAME,
        "b",
        &identity_b,
        Some(&identity_a),
        Some(&identity_relay),
        &temp_dir,
        &start_b,
    );
    let mut node_a = spawn_node(
        RELAY_TEST_NAME,
        "a",
        &identity_a,
        Some(&identity_b),
        Some(&identity_relay),
        &temp_dir,
        &start_a,
    );
    wait_for_child_namespace(relay.id());
    wait_for_child_namespace(node_a.id());
    wait_for_child_namespace(node_b.id());
    configure_relay_underlay(relay.id(), node_a.id(), node_b.id());
    ns_command(node_a.id(), "ping", &["-c", "1", "-W", "2", "10.251.0.254"]);
    ns_command(node_b.id(), "ping", &["-c", "1", "-W", "2", "10.251.0.254"]);

    fs::write(&start_relay, b"start").expect("write relay start file");
    wait_for_file(&temp_dir.join("ready-relay"));
    fs::write(&start_b, b"start").expect("write node B start file");
    wait_for_file(&temp_dir.join("ready-b"));
    wait_for_daemon_running(&temp_dir, "b");
    fs::write(&start_a, b"start").expect("write node A start file");
    wait_for_file(&temp_dir.join("ready-a"));
    wait_for_peer_ready(&temp_dir, "a");
    wait_for_peer_ready(&temp_dir, "b");

    let host_ping = ping_from_namespace(node_a.id(), "hse2ea", address_b);
    let initiator_addresses = ns_command_output(node_a.id(), "ip", &["addr", "show"]);
    let initiator_routes = ns_command_output(node_a.id(), "ip", &["route", "show", "table", "all"]);
    let responder_addresses = ns_command_output(node_b.id(), "ip", &["addr", "show"]);
    let responder_routes = ns_command_output(node_b.id(), "ip", &["route", "show", "table", "all"]);

    stop_child(&mut node_a);
    stop_child(&mut node_b);
    stop_child(&mut relay);
    assert_ping_success(
        "relayed overlay host ping",
        &host_ping,
        &temp_dir,
        &initiator_addresses,
        &initiator_routes,
        &responder_addresses,
        &responder_routes,
    );
    let relay_log = read_log(&temp_dir.join("node-relay.log"));
    assert!(
        relay_log.contains("CircuitReqAccepted"),
        "relay did not accept a circuit\nrelay log:\n{relay_log}\nnode-a log:\n{}\nnode-b log:\n{}",
        read_log(&temp_dir.join("node-a.log")),
        read_log(&temp_dir.join("node-b.log"))
    );
    cleanup_temp_dir(temp_dir);
}

fn run_invite_relay_orchestrator() {
    let identity_relay = NodeIdentity::generate_ed25519().expect("relay identity");
    let identity_a = NodeIdentity::generate_ed25519().expect("node A identity");
    let identity_b = NodeIdentity::generate_ed25519().expect("node B identity");
    let temp_dir = env::temp_dir().join(format!(
        "p2p-vpn-invite-relay-tun-e2e-{}",
        std::process::id()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    let start_relay = temp_dir.join("start-relay");
    let start_a = temp_dir.join("start-a");
    let start_b = temp_dir.join("start-b");

    let config_a = invite_relay_overlay_config("a", &identity_a, &identity_b, &identity_relay);
    let config_b = invite_relay_overlay_config("b", &identity_a, &identity_b, &identity_relay);
    let address_b = TunRuntimeConfig::from_config(&config_b)
        .expect("node B TUN config")
        .addresses
        .ipv4;
    write_child_config(&temp_dir, "a", &config_a);
    write_child_config(&temp_dir, "b", &config_b);

    let mut relay = spawn_node(
        INVITE_RELAY_TEST_NAME,
        "relay",
        &identity_relay,
        None,
        None,
        &temp_dir,
        &start_relay,
    );
    let mut node_b = spawn_node(
        INVITE_RELAY_TEST_NAME,
        "b",
        &identity_b,
        Some(&identity_a),
        Some(&identity_relay),
        &temp_dir,
        &start_b,
    );
    let mut node_a = spawn_node(
        INVITE_RELAY_TEST_NAME,
        "a",
        &identity_a,
        Some(&identity_b),
        Some(&identity_relay),
        &temp_dir,
        &start_a,
    );
    wait_for_child_namespace(relay.id());
    wait_for_child_namespace(node_a.id());
    wait_for_child_namespace(node_b.id());
    configure_relay_underlay(relay.id(), node_a.id(), node_b.id());
    ns_command(node_a.id(), "ping", &["-c", "1", "-W", "2", "10.251.0.254"]);
    ns_command(node_b.id(), "ping", &["-c", "1", "-W", "2", "10.251.0.254"]);

    fs::write(&start_relay, b"start").expect("write relay start file");
    wait_for_file(&temp_dir.join("ready-relay"));
    fs::write(&start_b, b"start").expect("write node B start file");
    wait_for_file(&temp_dir.join("ready-b"));
    wait_for_daemon_running(&temp_dir, "b");
    fs::write(&start_a, b"start").expect("write node A start file");
    wait_for_file(&temp_dir.join("ready-a"));
    wait_for_peer_ready(&temp_dir, "a");
    wait_for_peer_ready(&temp_dir, "b");

    let host_ping = ping_from_namespace(node_a.id(), "hse2ea", address_b);
    let initiator_addresses = ns_command_output(node_a.id(), "ip", &["addr", "show"]);
    let initiator_routes = ns_command_output(node_a.id(), "ip", &["route", "show", "table", "all"]);
    let responder_addresses = ns_command_output(node_b.id(), "ip", &["addr", "show"]);
    let responder_routes = ns_command_output(node_b.id(), "ip", &["route", "show", "table", "all"]);

    stop_child(&mut node_a);
    stop_child(&mut node_b);
    stop_child(&mut relay);
    assert_ping_success(
        "invite-imported relayed overlay host ping",
        &host_ping,
        &temp_dir,
        &initiator_addresses,
        &initiator_routes,
        &responder_addresses,
        &responder_routes,
    );
    let relay_log = read_log(&temp_dir.join("node-relay.log"));
    let initiator_log = read_log(&temp_dir.join("node-a.log"));
    let responder_log = read_log(&temp_dir.join("node-b.log"));
    assert!(
        relay_log.contains("CircuitReqAccepted"),
        "relay did not accept a circuit for invite-imported config\nrelay log:\n{relay_log}\nnode-a log:\n{initiator_log}\nnode-b log:\n{responder_log}",
    );
    assert!(
        initiator_log.contains("control capabilities accepted"),
        "invite-imported node did not exchange accepted capabilities\nnode-a log:\n{initiator_log}\nnode-b log:\n{responder_log}",
    );
    cleanup_temp_dir(temp_dir);
}

fn run_relay_promotion_orchestrator() {
    let identity_relay = NodeIdentity::generate_ed25519().expect("relay identity");
    let identity_a = NodeIdentity::generate_ed25519().expect("node A identity");
    let identity_b = NodeIdentity::generate_ed25519().expect("node B identity");
    let temp_dir = env::temp_dir().join(format!(
        "p2p-vpn-relay-promotion-tun-e2e-{}",
        std::process::id()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    let start_relay = temp_dir.join("start-relay");
    let start_a = temp_dir.join("start-a");
    let start_b = temp_dir.join("start-b");

    let config_b = relay_promotion_overlay_config("b", &identity_b, &identity_a, &identity_relay);
    let address_b = TunRuntimeConfig::from_config(&config_b)
        .expect("node B TUN config")
        .addresses
        .ipv4;

    let mut relay = spawn_node(
        RELAY_PROMOTION_TEST_NAME,
        "relay",
        &identity_relay,
        None,
        None,
        &temp_dir,
        &start_relay,
    );
    let mut node_b = spawn_node(
        RELAY_PROMOTION_TEST_NAME,
        "b",
        &identity_b,
        Some(&identity_a),
        Some(&identity_relay),
        &temp_dir,
        &start_b,
    );
    let mut node_a = spawn_node(
        RELAY_PROMOTION_TEST_NAME,
        "a",
        &identity_a,
        Some(&identity_b),
        Some(&identity_relay),
        &temp_dir,
        &start_a,
    );
    wait_for_child_namespace(relay.id());
    wait_for_child_namespace(node_a.id());
    wait_for_child_namespace(node_b.id());
    configure_relay_underlay(relay.id(), node_a.id(), node_b.id());
    ns_command(node_a.id(), "ping", &["-c", "1", "-W", "2", "10.251.0.254"]);
    ns_command(node_b.id(), "ping", &["-c", "1", "-W", "2", "10.251.0.254"]);

    fs::write(&start_relay, b"start").expect("write relay start file");
    wait_for_file(&temp_dir.join("ready-relay"));
    fs::write(&start_b, b"start").expect("write node B start file");
    wait_for_file(&temp_dir.join("ready-b"));
    wait_for_daemon_running(&temp_dir, "b");
    fs::write(&start_a, b"start").expect("write node A start file");
    wait_for_file(&temp_dir.join("ready-a"));
    wait_for_packet_plane_sessions(&temp_dir, "a");
    wait_for_packet_plane_sessions(&temp_dir, "b");
    wait_for_direct_promotion(&temp_dir, "a");

    let host_ping = ping_from_namespace(node_a.id(), "hse2ea", address_b);
    let initiator_addresses = ns_command_output(node_a.id(), "ip", &["addr", "show"]);
    let initiator_routes = ns_command_output(node_a.id(), "ip", &["route", "show", "table", "all"]);
    let responder_addresses = ns_command_output(node_b.id(), "ip", &["addr", "show"]);
    let responder_routes = ns_command_output(node_b.id(), "ip", &["route", "show", "table", "all"]);
    wait_for_packet_plane_datagrams(&temp_dir);

    stop_child(&mut node_a);
    stop_child(&mut node_b);
    stop_child(&mut relay);
    assert_ping_success(
        "relay overlay direct-promotion host ping",
        &host_ping,
        &temp_dir,
        &initiator_addresses,
        &initiator_routes,
        &responder_addresses,
        &responder_routes,
    );
    let relay_log = read_log(&temp_dir.join("node-relay.log"));
    let initiator_log = read_log(&temp_dir.join("node-a.log"));
    let responder_log = read_log(&temp_dir.join("node-b.log"));
    assert!(
        relay_log.contains("CircuitReqAccepted"),
        "relay did not accept a circuit\nrelay log:\n{relay_log}\nnode-a log:\n{initiator_log}\nnode-b log:\n{responder_log}",
    );
    assert!(
        initiator_log.contains("event=dcutr_enabled")
            && initiator_log.contains("event=autonat_enabled")
            && initiator_log.contains("event=path_promoted_to_direct")
            && initiator_log.contains("previous_path=circuit_relay")
            && initiator_log.contains("current_path=direct_tcp_stream")
            && initiator_log.contains("event=dcutr_hole_punch_result")
            && initiator_log.contains("success=true"),
        "node A did not promote from relay to a direct path with NAT traversal enabled\nnode-a log:\n{initiator_log}\nnode-b log:\n{responder_log}\nrelay log:\n{relay_log}",
    );
    assert_packet_plane_datagrams_used("node A", &initiator_log, &responder_log);
    assert_packet_plane_datagrams_used("node B", &responder_log, &initiator_log);
    cleanup_temp_dir(temp_dir);
}

fn run_dht_orchestrator() {
    let identity_bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
    let identity_a = NodeIdentity::generate_ed25519().expect("node A identity");
    let identity_b = NodeIdentity::generate_ed25519().expect("node B identity");
    let temp_dir = env::temp_dir().join(format!("p2p-vpn-dht-tun-e2e-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    let start_bootstrap = temp_dir.join("start-bootstrap");
    let start_a = temp_dir.join("start-a");
    let start_b = temp_dir.join("start-b");

    let config_b = dht_overlay_config("b", &identity_b, &identity_a, &identity_bootstrap);
    let address_b = TunRuntimeConfig::from_config(&config_b)
        .expect("node B TUN config")
        .addresses
        .ipv4;

    let mut bootstrap = spawn_node(
        DHT_TEST_NAME,
        "bootstrap",
        &identity_bootstrap,
        None,
        None,
        &temp_dir,
        &start_bootstrap,
    );
    let mut node_b = spawn_node(
        DHT_TEST_NAME,
        "b",
        &identity_b,
        Some(&identity_a),
        Some(&identity_bootstrap),
        &temp_dir,
        &start_b,
    );
    let mut node_a = spawn_node(
        DHT_TEST_NAME,
        "a",
        &identity_a,
        Some(&identity_b),
        Some(&identity_bootstrap),
        &temp_dir,
        &start_a,
    );
    wait_for_child_namespace(bootstrap.id());
    wait_for_child_namespace(node_a.id());
    wait_for_child_namespace(node_b.id());
    configure_three_node_underlay(bootstrap.id(), node_a.id(), node_b.id(), "dht", "10.252.0");
    ns_command(node_a.id(), "ping", &["-c", "1", "-W", "2", "10.252.0.254"]);
    ns_command(node_b.id(), "ping", &["-c", "1", "-W", "2", "10.252.0.254"]);

    fs::write(&start_bootstrap, b"start").expect("write bootstrap start file");
    wait_for_file(&temp_dir.join("ready-bootstrap"));
    fs::write(&start_b, b"start").expect("write node B start file");
    wait_for_file(&temp_dir.join("ready-b"));
    wait_for_daemon_running(&temp_dir, "b");
    fs::write(&start_a, b"start").expect("write node A start file");
    wait_for_file(&temp_dir.join("ready-a"));
    wait_for_packet_plane_sessions(&temp_dir, "a");
    wait_for_packet_plane_sessions(&temp_dir, "b");

    let host_ping = ping_from_namespace(node_a.id(), "hse2ea", address_b);
    let initiator_addresses = ns_command_output(node_a.id(), "ip", &["addr", "show"]);
    let initiator_routes = ns_command_output(node_a.id(), "ip", &["route", "show", "table", "all"]);
    let responder_addresses = ns_command_output(node_b.id(), "ip", &["addr", "show"]);
    let responder_routes = ns_command_output(node_b.id(), "ip", &["route", "show", "table", "all"]);
    wait_for_packet_plane_datagrams(&temp_dir);

    stop_child(&mut node_a);
    stop_child(&mut node_b);
    stop_child(&mut bootstrap);
    assert_ping_success(
        "DHT-discovered overlay host ping",
        &host_ping,
        &temp_dir,
        &initiator_addresses,
        &initiator_routes,
        &responder_addresses,
        &responder_routes,
    );
    let initiator_log = read_log(&temp_dir.join("node-a.log"));
    let responder_log = read_log(&temp_dir.join("node-b.log"));
    let bootstrap_log = read_log(&temp_dir.join("node-bootstrap.log"));
    assert!(
        initiator_log.contains("kademlia query progressed")
            && initiator_log.contains("control capabilities accepted"),
        "node A did not discover and validate node B through Kademlia\nnode-a log:\n{initiator_log}\nnode-b log:\n{responder_log}\nbootstrap log:\n{bootstrap_log}",
    );
    assert_packet_plane_datagrams_used("node A", &initiator_log, &responder_log);
    assert_packet_plane_datagrams_used("node B", &responder_log, &initiator_log);
    cleanup_temp_dir(temp_dir);
}

fn assert_ping_success(
    context: &str,
    ping: &Output,
    temp_dir: &Path,
    initiator_addresses: &Output,
    initiator_routes: &Output,
    responder_addresses: &Output,
    responder_routes: &Output,
) {
    if !ping.status.success() {
        capture_daemon_snapshots(temp_dir, &["a", "b"]);
    }
    assert!(
        ping.status.success(),
        "{context} failed with {}\nstdout:\n{}\nstderr:\n{}\nnode-a ip addr:\n{}\nnode-a routes:\n{}\nnode-b ip addr:\n{}\nnode-b routes:\n{}\ndaemon snapshots:\n{}\nnode-a log:\n{}\nnode-b log:\n{}",
        ping.status,
        String::from_utf8_lossy(&ping.stdout),
        String::from_utf8_lossy(&ping.stderr),
        String::from_utf8_lossy(&initiator_addresses.stdout),
        String::from_utf8_lossy(&initiator_routes.stdout),
        String::from_utf8_lossy(&responder_addresses.stdout),
        String::from_utf8_lossy(&responder_routes.stdout),
        daemon_snapshot_summary(temp_dir, &["a", "b"]),
        read_log(&temp_dir.join("node-a.log")),
        read_log(&temp_dir.join("node-b.log")),
    );
}

fn cleanup_temp_dir(temp_dir: PathBuf) {
    if keep_temp_artifacts() {
        eprintln!(
            "preserving namespace E2E artifacts at {}",
            temp_dir.display()
        );
    } else {
        let _ = fs::remove_dir_all(temp_dir);
    }
}

fn keep_temp_artifacts() -> bool {
    env::var(KEEP_TEMP_ENV)
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
}

fn assert_packet_plane_datagrams_used(context: &str, log: &str, peer_log: &str) {
    assert!(
        packet_plane_datagrams_used(log),
        "{context} did not negotiate and use packet-plane datagrams\n{context} log tail:\n{}\npeer log tail:\n{}",
        log_tail(log, 80),
        log_tail(peer_log, 80),
    );
}

fn assert_owned_quic_packet_plane_datagrams_used(context: &str, log: &str, peer_log: &str) {
    assert!(
        owned_quic_packet_plane_datagrams_used(log),
        "{context} did not negotiate and use owned QUIC packet-plane datagrams\n{context} log tail:\n{}\npeer log tail:\n{}",
        log_tail(log, 100),
        log_tail(peer_log, 100),
    );
}

fn wait_for_owned_quic_packet_plane_sessions(temp_dir: &Path) -> (String, String) {
    let node_a_path = temp_dir.join("node-a.log");
    let node_b_path = temp_dir.join("node-b.log");
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let initiator_log = read_log(&node_a_path);
        let responder_log = read_log(&node_b_path);
        if owned_quic_packet_plane_session_established(&initiator_log)
            && owned_quic_packet_plane_session_established(&responder_log)
        {
            return (initiator_log, responder_log);
        }
        if Instant::now() >= deadline {
            return (initiator_log, responder_log);
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_packet_plane_datagrams(temp_dir: &Path) -> (String, String) {
    let node_a_path = temp_dir.join("node-a.log");
    let node_b_path = temp_dir.join("node-b.log");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let initiator_log = read_log(&node_a_path);
        let responder_log = read_log(&node_b_path);
        if packet_plane_datagrams_used(&initiator_log)
            && packet_plane_datagrams_used(&responder_log)
        {
            return (initiator_log, responder_log);
        }
        if Instant::now() >= deadline {
            return (initiator_log, responder_log);
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_owned_quic_packet_plane_datagrams(temp_dir: &Path) -> (String, String) {
    let node_a_path = temp_dir.join("node-a.log");
    let node_b_path = temp_dir.join("node-b.log");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let initiator_log = read_log(&node_a_path);
        let responder_log = read_log(&node_b_path);
        if owned_quic_packet_plane_datagrams_used(&initiator_log)
            && owned_quic_packet_plane_datagrams_used(&responder_log)
        {
            return (initiator_log, responder_log);
        }
        if Instant::now() >= deadline {
            return (initiator_log, responder_log);
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_daemon_running(temp_dir: &Path, role: &str) {
    wait_for_daemon_state(
        temp_dir,
        role,
        Duration::from_secs(10),
        "daemon running",
        |lines| lines.iter().any(|line| line == "daemon state: running"),
    );
}

fn wait_for_peer_ready(temp_dir: &Path, role: &str) {
    wait_for_daemon_state(
        temp_dir,
        role,
        Duration::from_secs(15),
        "validated peer with supported path",
        |lines| {
            state_colon_count(lines, "validated peers").is_some_and(|count| count >= 1)
                && state_metric_count(lines, "peers_with_supported_path")
                    .is_some_and(|count| count >= 1)
        },
    );
}

fn wait_for_packet_plane_sessions(temp_dir: &Path, role: &str) {
    wait_for_daemon_state(
        temp_dir,
        role,
        Duration::from_secs(15),
        "validated peer with packet-plane session",
        |lines| {
            state_colon_count(lines, "validated peers").is_some_and(|count| count >= 1)
                && state_metric_count(lines, "peers_with_supported_path")
                    .is_some_and(|count| count >= 1)
                && state_metric_count(lines, "packet_plane_sessions")
                    .is_some_and(|count| count >= 1)
        },
    );
}

fn wait_for_direct_promotion(temp_dir: &Path, role: &str) {
    wait_for_daemon_state(
        temp_dir,
        role,
        Duration::from_secs(30),
        "relay path promoted to direct path",
        |lines| {
            state_metric_count(lines, "dcutr_successes").is_some_and(|count| count >= 1)
                && state_metric_count(lines, "path_promotions_to_direct")
                    .is_some_and(|count| count >= 1)
                && state_metric_count(lines, "healthy_direct_tcp_stream_paths")
                    .is_some_and(|count| count >= 1)
        },
    );
}

fn wait_for_daemon_state<F>(
    temp_dir: &Path,
    role: &str,
    timeout: Duration,
    context: &str,
    mut predicate: F,
) -> Vec<String>
where
    F: FnMut(&[String]) -> bool,
{
    let socket = node_control_socket(temp_dir, role);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime for control socket polling");
    let deadline = Instant::now() + timeout;
    let mut last_lines = Vec::new();
    let mut last_error = None;

    loop {
        match runtime.block_on(query_state(&socket, Duration::from_millis(500))) {
            Ok(lines) => {
                if predicate(&lines) {
                    return lines;
                }
                last_lines = lines;
            }
            Err(error) => {
                last_error = Some(format!("{error:?}"));
            }
        }
        if Instant::now() >= deadline {
            capture_daemon_snapshots(temp_dir, &[role]);
            let log = read_log(&temp_dir.join(format!("node-{role}.log")));
            panic!(
                "timed out waiting for {context} on node {role} via {}\nlast_error: {}\nlast_state:\n{}\ndaemon snapshots:\n{}\nnode log tail:\n{}",
                socket.display(),
                last_error.unwrap_or_else(|| "none".to_owned()),
                last_lines.join("\n"),
                daemon_snapshot_summary(temp_dir, &[role]),
                log_tail(&log, 100),
            );
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn capture_daemon_snapshots(temp_dir: &Path, roles: &[&str]) {
    for role in roles {
        capture_daemon_snapshots_for_role(temp_dir, role);
    }
}

fn capture_daemon_snapshots_for_role(temp_dir: &Path, role: &str) {
    let socket = node_control_socket(temp_dir, role);
    for (command, artifact) in DAEMON_SNAPSHOT_COMMANDS {
        let artifact_path = temp_dir.join(format!("{artifact}-{role}.txt"));
        let output = if socket.exists() {
            let socket_arg = socket.to_string_lossy().into_owned();
            command_output(
                current_test_binary(),
                &[*command, "--socket", &socket_arg, "--timeout-seconds", "1"],
                &[],
                Duration::from_secs(3),
            )
            .map_or_else(
                |error| format!("failed to execute {command}: {error}\n"),
                |output| format_snapshot_output(&output),
            )
        } else {
            format!("socket missing: {}\n", socket.display())
        };
        let _ = fs::write(artifact_path, output);
    }
}

fn daemon_snapshot_summary(temp_dir: &Path, roles: &[&str]) -> String {
    let mut lines = Vec::new();
    for role in roles {
        for (_, artifact) in DAEMON_SNAPSHOT_COMMANDS {
            let path = temp_dir.join(format!("{artifact}-{role}.txt"));
            let summary = if path.exists() {
                let body = fs::read_to_string(&path)
                    .unwrap_or_else(|error| format!("failed to read snapshot: {error}"));
                format!("{}:\n{}", path.display(), log_tail(&body, 40))
            } else {
                format!("{}: not captured", path.display())
            };
            lines.push(summary);
        }
    }
    lines.join("\n")
}

fn format_snapshot_output(output: &Output) -> String {
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn current_test_binary() -> &'static str {
    env!("CARGO_BIN_EXE_p2p-vpn")
}

const DAEMON_SNAPSHOT_COMMANDS: &[(&str, &str)] = &[
    ("daemon-status", "daemon-status"),
    ("daemon-state", "daemon-state"),
    ("daemon-peers", "daemon-peers"),
    ("daemon-routes", "daemon-routes"),
    ("daemon-paths", "daemon-paths"),
    ("daemon-mtu", "daemon-mtu"),
    ("daemon-capabilities", "daemon-capabilities"),
];

fn node_control_socket(temp_dir: &Path, role: &str) -> PathBuf {
    temp_dir.join(format!("control-{role}.sock"))
}

fn state_colon_count(lines: &[String], prefix: &str) -> Option<usize> {
    let needle = format!("{prefix}: ");
    lines
        .iter()
        .find_map(|line| line.strip_prefix(&needle)?.parse().ok())
}

fn state_metric_count(lines: &[String], name: &str) -> Option<usize> {
    lines.iter().find_map(|line| {
        let (candidate, value) = line.split_once(' ')?;
        if candidate == name {
            value.parse().ok()
        } else {
            None
        }
    })
}

fn packet_plane_datagrams_used(log: &str) -> bool {
    log.contains("event=packet_plane_session_established")
        && log_metric_positive(log, "path_healthy_direct_quic_datagram_paths")
        && log_metric_positive(log, "outbound_quic_datagram_packets")
        && log_metric_positive(log, "inbound_accepted_packets")
}

fn owned_quic_packet_plane_datagrams_used(log: &str) -> bool {
    owned_quic_packet_plane_session_established(log)
        && log.contains("event=packet_plane_quic_listening")
        && (log_metric_positive(log, "outbound_quic_datagram_packets")
            || log_metric_positive(log, "inbound_accepted_packets"))
}

fn owned_quic_packet_plane_session_established(log: &str) -> bool {
    log.contains("event=packet_plane_session_established")
        && log.contains("backend=owned_quic")
        && log_metric_positive(log, "path_healthy_direct_quic_datagram_paths")
}

fn log_metric_positive(log: &str, metric: &str) -> bool {
    log.lines().any(|line| {
        line.trim_start()
            .strip_prefix(metric)
            .and_then(|value| value.trim().parse::<u64>().ok())
            .is_some_and(|value| value > 0)
    })
}

fn log_tail(log: &str, lines: usize) -> String {
    let mut tail = log.lines().rev().take(lines).collect::<Vec<_>>();
    tail.reverse();
    tail.join("\n")
}

fn spawn_node(
    test_name: &str,
    role: &str,
    local: &NodeIdentity,
    remote: Option<&NodeIdentity>,
    relay: Option<&NodeIdentity>,
    temp_dir: &Path,
    start_file: &Path,
) -> Child {
    let current_exe = env::current_exe().expect("current test binary");
    let log = File::create(temp_dir.join(format!("node-{role}.log"))).expect("create node log");
    let log_err = log.try_clone().expect("clone node log");
    let mut command = Command::new("unshare");
    command
        .args([
            "--net",
            current_exe.to_str().expect("test binary path is utf-8"),
            "--ignored",
            test_name,
            "--exact",
            "--nocapture",
        ])
        .env(CHILD_ENV, "node")
        .env("P2P_VPN_TUN_E2E_ROLE", role)
        .env("P2P_VPN_TUN_E2E_LOCAL_PEER", &local.peer_id)
        .env("P2P_VPN_TUN_E2E_LOCAL_KEY", &local.private_key)
        .env("P2P_VPN_TUN_E2E_TEMP", temp_dir)
        .env("P2P_VPN_TUN_E2E_START", start_file);
    if let Some(remote) = remote {
        command.env("P2P_VPN_TUN_E2E_REMOTE_PEER", &remote.peer_id);
    }
    if let Some(relay) = relay {
        command.env("P2P_VPN_TUN_E2E_RELAY_PEER", &relay.peer_id);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .spawn()
        .expect("spawn node namespace")
}

fn wait_for_child_namespace(pid: u32) {
    let namespace = PathBuf::from(format!("/proc/{pid}/ns/net"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if namespace.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("child namespace for pid {pid} did not appear");
}

fn configure_underlay(pid_a: u32, pid_b: u32) {
    run_command(
        "ip",
        &[
            "link", "add", "veth-a", "type", "veth", "peer", "name", "veth-b",
        ],
    );
    run_command(
        "ip",
        &["link", "set", "veth-a", "netns", &pid_a.to_string()],
    );
    run_command(
        "ip",
        &["link", "set", "veth-b", "netns", &pid_b.to_string()],
    );
    ns_command(pid_a, "ip", &["link", "set", "lo", "up"]);
    ns_command(pid_b, "ip", &["link", "set", "lo", "up"]);
    configure_sysctls(pid_a);
    configure_sysctls(pid_b);
    ns_command(
        pid_a,
        "ip",
        &["addr", "add", "10.250.0.1/24", "dev", "veth-a"],
    );
    ns_command(
        pid_b,
        "ip",
        &["addr", "add", "10.250.0.2/24", "dev", "veth-b"],
    );
    ns_command(pid_a, "ip", &["link", "set", "veth-a", "up"]);
    ns_command(pid_b, "ip", &["link", "set", "veth-b", "up"]);
}

fn configure_relay_underlay(pid_relay: u32, pid_a: u32, pid_b: u32) {
    configure_three_node_underlay(pid_relay, pid_a, pid_b, "relay", "10.251.0");
}

fn configure_three_node_underlay(pid_infra: u32, pid_a: u32, pid_b: u32, name: &str, prefix: &str) {
    let bridge = format!("br-{name}");
    run_command("ip", &["link", "add", &bridge, "type", "bridge"]);
    run_command("ip", &["link", "set", &bridge, "up"]);
    attach_veth_to_bridge(pid_infra, name, &format!("{prefix}.254/24"), &bridge);
    attach_veth_to_bridge(pid_a, "a", &format!("{prefix}.1/24"), &bridge);
    attach_veth_to_bridge(pid_b, "b", &format!("{prefix}.2/24"), &bridge);
}

fn attach_veth_to_bridge(pid: u32, suffix: &str, address: &str, bridge: &str) {
    let host = format!("veth-{suffix}-host");
    let child = format!("veth-{suffix}");
    run_command(
        "ip",
        &["link", "add", &host, "type", "veth", "peer", "name", &child],
    );
    run_command("ip", &["link", "set", &host, "master", bridge]);
    run_command("ip", &["link", "set", &host, "up"]);
    run_command("ip", &["link", "set", &child, "netns", &pid.to_string()]);
    ns_command(pid, "ip", &["link", "set", "lo", "up"]);
    configure_sysctls(pid);
    ns_command(pid, "ip", &["addr", "add", address, "dev", &child]);
    ns_command(pid, "ip", &["link", "set", &child, "up"]);
}

fn configure_sysctls(pid: u32) {
    ns_command(pid, "sysctl", &["-w", "net.ipv4.conf.all.rp_filter=0"]);
    ns_command(pid, "sysctl", &["-w", "net.ipv4.conf.default.rp_filter=0"]);
    ns_command(pid, "sysctl", &["-w", "net.ipv4.icmp_echo_ignore_all=0"]);
}

fn configure_tun_sysctls(interface: &str) {
    run_command(
        "sysctl",
        &["-w", &format!("net.ipv4.conf.{interface}.rp_filter=0")],
    );
    run_command(
        "sysctl",
        &["-w", &format!("net.ipv4.conf.{interface}.accept_local=1")],
    );
}

fn run_node_child() {
    let role = required_env("P2P_VPN_TUN_E2E_ROLE");
    let local = NodeIdentity {
        peer_id: required_env("P2P_VPN_TUN_E2E_LOCAL_PEER"),
        private_key: required_env("P2P_VPN_TUN_E2E_LOCAL_KEY"),
    };
    let start_file = PathBuf::from(required_env("P2P_VPN_TUN_E2E_START"));
    let temp_dir = PathBuf::from(required_env("P2P_VPN_TUN_E2E_TEMP"));
    wait_for_file(&start_file);

    if role == "relay" {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(run_relay_child(local, temp_dir.join("ready-relay")))
            .expect("relay runtime");
        return;
    }
    if role == "bootstrap" {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(run_bootstrap_child(local, temp_dir.join("ready-bootstrap")))
            .expect("bootstrap runtime");
        return;
    }

    let config = if is_invite_relay_test_child() {
        read_child_config(&temp_dir, &role)
    } else {
        let remote = NodeIdentity {
            peer_id: required_env("P2P_VPN_TUN_E2E_REMOTE_PEER"),
            private_key: String::new(),
        };
        let relay = env::var("P2P_VPN_TUN_E2E_RELAY_PEER")
            .ok()
            .map(|peer_id| NodeIdentity {
                peer_id,
                private_key: String::new(),
            });
        if let Some(infra) = relay.as_ref() {
            match role.as_str() {
                "a" | "b" if is_dht_test_child() => {
                    dht_overlay_config(&role, &local, &remote, infra)
                }
                "a" | "b" if is_relay_promotion_test_child() => {
                    relay_promotion_overlay_config(&role, &local, &remote, infra)
                }
                "a" | "b" => relay_overlay_config(&role, &local, &remote, infra),
                other => panic!("unknown node role {other}"),
            }
        } else if is_direct_quic_test_child() {
            direct_quic_overlay_config(&role, &local, &remote)
        } else if is_mdns_test_child() {
            mdns_overlay_config(&role, &local, &remote)
        } else {
            direct_overlay_config(&role, &local, &remote)
        }
    };
    let interface = config.interface.name.clone();
    let runtime = TunRuntimeConfig::from_config(&config).expect("TUN config");
    let effective_mtu = runtime.mtu;
    let device = open_and_configure_tun(&runtime);
    if role == "a" {
        run_command(
            "ip",
            &[
                "addr",
                "add",
                &format!("{NODE_A_LOCAL_ROUTE_ADDRESS}/32"),
                "dev",
                &interface,
            ],
        );
    }
    configure_tun_sysctls(&interface);

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(run_ready_node(
            config,
            device,
            effective_mtu,
            temp_dir.join(format!("ready-{role}")),
            node_control_socket(&temp_dir, &role),
        ))
        .expect("node runtime");
}

fn direct_overlay_config(role: &str, local: &NodeIdentity, remote: &NodeIdentity) -> Config {
    let (name, interface, listen, packet_endpoint, remote_address, local_routes, peer_routes) =
        match role {
            "a" => (
                NETWORK_NAME,
                "hse2ea",
                "/ip4/10.250.0.1/tcp/42101",
                "10.250.0.1:43101",
                Some("/ip4/10.250.0.2/tcp/42102"),
                vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 100,
                }],
                Vec::new(),
            ),
            "b" => (
                NETWORK_NAME,
                "hse2eb",
                "/ip4/10.250.0.2/tcp/42102",
                "10.250.0.2:43102",
                None,
                Vec::new(),
                vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 100,
                }],
            ),
            other => panic!("unknown node role {other}"),
        };
    let mut config = node_config(
        name,
        interface,
        local,
        listen,
        local_routes,
        peer_config(remote, remote_address, peer_routes),
    );
    config.network.packet_plane.listen = vec![packet_endpoint.to_owned()];
    config.network.packet_plane.external_endpoints = vec![packet_endpoint.to_owned()];
    config
}

fn direct_quic_overlay_config(role: &str, local: &NodeIdentity, remote: &NodeIdentity) -> Config {
    let mut config = direct_overlay_config(role, local, remote);
    config.network.discovery = relay_test_discovery();
    config.network.packet_plane.listen = Vec::new();
    config.network.packet_plane.external_endpoints = Vec::new();
    let packet_endpoint = match role {
        "a" => "10.250.0.1:44101",
        "b" => "10.250.0.2:44102",
        other => panic!("unknown owned QUIC node role {other}"),
    };
    config.network.packet_plane.quic_listen = vec![packet_endpoint.to_owned()];
    config.network.packet_plane.quic_external_endpoints = vec![packet_endpoint.to_owned()];
    config
}

fn enable_test_packet_plane(config: &mut Config, endpoint: &str) {
    config.network.packet_plane.listen = vec![endpoint.to_owned()];
    config.network.packet_plane.external_endpoints = vec![endpoint.to_owned()];
}

fn relay_overlay_config(
    role: &str,
    local: &NodeIdentity,
    remote: &NodeIdentity,
    relay: &NodeIdentity,
) -> Config {
    let relay_base = format!(
        "/ip4/10.251.0.254/tcp/42200/p2p/{}/p2p-circuit",
        relay.peer_id
    );
    let (interface, listen, remote_address, local_routes, peer_routes, relay_reservations) =
        match role {
            "a" => (
                "hse2ea",
                "/ip4/10.251.0.1/tcp/42201",
                Some(format!("{relay_base}/p2p/{}", remote.peer_id)),
                vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 100,
                }],
                Vec::new(),
                Vec::new(),
            ),
            "b" => (
                "hse2eb",
                "/ip4/10.251.0.2/tcp/42202",
                None,
                Vec::new(),
                vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 100,
                }],
                vec![relay_base],
            ),
            other => panic!("unknown relay node role {other}"),
        };
    let mut config = node_config(
        NETWORK_NAME,
        interface,
        local,
        listen,
        local_routes,
        peer_config(remote, remote_address.as_deref(), peer_routes),
    );
    config.network.discovery = relay_test_discovery();
    config.network.relay.reservations = relay_reservations;
    config
}

fn relay_promotion_overlay_config(
    role: &str,
    local: &NodeIdentity,
    remote: &NodeIdentity,
    relay: &NodeIdentity,
) -> Config {
    let mut config = relay_overlay_config(role, local, remote, relay);
    config.network.discovery = relay_promotion_test_discovery();
    let packet_endpoint = match role {
        "a" => "10.251.0.1:43201",
        "b" => "10.251.0.2:43202",
        other => panic!("unknown relay promotion node role {other}"),
    };
    enable_test_packet_plane(&mut config, packet_endpoint);
    config
}

fn invite_relay_overlay_config(
    role: &str,
    joining_node: &NodeIdentity,
    source_node: &NodeIdentity,
    relay: &NodeIdentity,
) -> Config {
    let relay_base = format!(
        "/ip4/10.251.0.254/tcp/42200/p2p/{}/p2p-circuit",
        relay.peer_id
    );
    let source_relayed_address = format!("{relay_base}/p2p/{}", source_node.peer_id);
    let joining_relayed_address = format!("{relay_base}/p2p/{}", joining_node.peer_id);
    let mut inviter_config = relay_overlay_config("b", source_node, joining_node, relay);
    inviter_config.network.external_addresses = vec![source_relayed_address];
    inviter_config.peers[0].addresses = vec![joining_relayed_address];
    let mut invite_export_config = inviter_config.clone();
    invite_export_config.network.listen_addresses = Vec::new();
    let invite =
        export_signed_invite_at(&invite_export_config, InviteExportOptions::default(), 1_000)
            .expect("relay-assisted invite");

    match role {
        "a" => {
            let mut config = import_invite_config_at(
                &invite,
                InviteImportOptions {
                    identity: joining_node.clone(),
                    interface_name: "hse2ea".to_owned(),
                    mtu: 1280,
                    local_routes: vec![RouteConfig {
                        prefix: "10.41.0.0/24".to_owned(),
                        metric: 100,
                    }],
                    peer_name: Some("inviter".to_owned()),
                },
                1_000,
            )
            .expect("import relay-assisted invite");
            assert!(!config.network.relay.reservations.is_empty());
            config.network.listen_addresses = vec!["/ip4/10.251.0.1/tcp/42201".to_owned()];
            config.network.relay.reservations = Vec::new();
            config.validate_runtime().expect("invited runtime config");
            config
        }
        "b" => {
            inviter_config
                .validate_runtime()
                .expect("inviter runtime config");
            inviter_config
        }
        other => panic!("unknown invite relay node role {other}"),
    }
}

fn write_child_config(temp_dir: &Path, role: &str, config: &Config) {
    let bytes = serde_json::to_vec(config).expect("serialize child config");
    fs::write(child_config_path(temp_dir, role), bytes).expect("write child config");
}

fn read_child_config(temp_dir: &Path, role: &str) -> Config {
    let bytes = fs::read(child_config_path(temp_dir, role)).expect("read child config");
    let config = serde_json::from_slice::<Config>(&bytes).expect("parse child config");
    config.validate_runtime().expect("valid child config");
    config
}

fn child_config_path(temp_dir: &Path, role: &str) -> PathBuf {
    temp_dir.join(format!("config-{role}.json"))
}

fn mdns_overlay_config(role: &str, local: &NodeIdentity, remote: &NodeIdentity) -> Config {
    let (interface, listen, packet_endpoint, local_routes, peer_routes) = match role {
        "a" => (
            "hse2ea",
            "/ip4/10.250.0.1/tcp/42101",
            "10.250.0.1:43101",
            vec![RouteConfig {
                prefix: "10.41.0.0/24".to_owned(),
                metric: 100,
            }],
            Vec::new(),
        ),
        "b" => (
            "hse2eb",
            "/ip4/10.250.0.2/tcp/42102",
            "10.250.0.2:43102",
            Vec::new(),
            vec![RouteConfig {
                prefix: "10.41.0.0/24".to_owned(),
                metric: 100,
            }],
        ),
        other => panic!("unknown mDNS node role {other}"),
    };
    let mut config = node_config(
        NETWORK_NAME,
        interface,
        local,
        listen,
        local_routes,
        peer_config(remote, None, peer_routes),
    );
    config.network.discovery = mdns_test_discovery();
    enable_test_packet_plane(&mut config, packet_endpoint);
    config
}

fn dht_overlay_config(
    role: &str,
    local: &NodeIdentity,
    remote: &NodeIdentity,
    bootstrap: &NodeIdentity,
) -> Config {
    let (interface, listen, packet_endpoint, local_routes, peer_routes) = match role {
        "a" => (
            "hse2ea",
            "/ip4/10.252.0.1/tcp/42301",
            "10.252.0.1:43301",
            vec![RouteConfig {
                prefix: "10.41.0.0/24".to_owned(),
                metric: 100,
            }],
            Vec::new(),
        ),
        "b" => (
            "hse2eb",
            "/ip4/10.252.0.2/tcp/42302",
            "10.252.0.2:43302",
            Vec::new(),
            vec![RouteConfig {
                prefix: "10.41.0.0/24".to_owned(),
                metric: 100,
            }],
        ),
        other => panic!("unknown DHT node role {other}"),
    };
    let mut config = node_config(
        NETWORK_NAME,
        interface,
        local,
        listen,
        local_routes,
        peer_config(remote, None, peer_routes),
    );
    config.network.bootstrap_peers = vec![p2p_vpn::config::BootstrapPeerConfig {
        id: bootstrap.peer_id.clone(),
        address: format!("/ip4/10.252.0.254/tcp/42300/p2p/{}", bootstrap.peer_id),
    }];
    config.network.discovery = dht_test_discovery();
    enable_test_packet_plane(&mut config, packet_endpoint);
    config
}

async fn run_ready_node(
    config: Config,
    device: TunDevice,
    mtu: u16,
    ready_file: PathBuf,
    control_socket: PathBuf,
) -> Result<(), runner::RunnerError> {
    let mut node = build_node(&HostConfig {
        identity: config.identity()?,
        network_name: config.network.name.clone(),
        membership_tag: config.membership_tag()?,
        mtu: config.effective_packet_mtu(),
        max_concurrent_control_streams: config.resources.control_stream_limit(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
        listen_addresses: config.listen_multiaddrs()?,
        external_addresses: config.external_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
        relay_reservations: config.relay_reservation_multiaddrs()?,
        relay_server: config.network.relay.server,
        relay_resources: config.network.relay.resources,
        resources: config.resources,
        discovery: config.network.discovery.clone(),
    })?;
    wait_for_node_ready(&mut node, &config).await;
    fs::write(ready_file, b"ready").expect("write ready file");
    node.packet_endpoint_candidates = config.packet_plane_endpoint_candidates()?;
    let packet_plane = PacketPlaneRuntime::bind_with_replay_window_limit(
        config.packet_plane_listen_addrs()?,
        config.network.packet_plane.replay_window_limit(),
    )
    .await
    .map_err(runner::RunnerError::PacketPlane)?;
    let packet_plane_quic = match config.packet_plane_quic_listen_addrs()?.as_slice() {
        [] => None,
        [listen_addr] => Some(
            PacketPlaneQuicRuntime::bind_with_replay_window_limit(
                *listen_addr,
                config.network.packet_plane.replay_window_limit(),
            )
            .map_err(runner::RunnerError::PacketPlaneQuic)?,
        ),
        _ => unreachable!("namespace test configs use at most one owned QUIC listener"),
    };
    let forwarder = Forwarder::from_config(&config)?;
    let membership = runner::OverlayMembership::from_config(&config)?;
    Box::pin(runner::run_node_until(
        node,
        forwarder,
        membership,
        Vec::new(),
        device,
        mtu,
        config.queue,
        config.resources,
        Some(Duration::from_secs(1)),
        Some(control_socket),
        packet_plane,
        packet_plane_quic,
        config.packet_plane_quic_endpoint_candidates()?,
        config.network.packet_plane.session_ttl(),
        config.network.packet_plane.replay_window_limit(),
        std::future::pending::<runner::ShutdownReason>(),
    ))
    .await
}

async fn run_relay_child(
    identity: NodeIdentity,
    ready_file: PathBuf,
) -> Result<(), runner::RunnerError> {
    let config = relay_config(&identity);
    let mut node = build_node(&HostConfig {
        identity: config.identity()?,
        network_name: config.network.name.clone(),
        membership_tag: config.membership_tag()?,
        mtu: config.effective_packet_mtu(),
        max_concurrent_control_streams: config.resources.control_stream_limit(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
        listen_addresses: config.listen_multiaddrs()?,
        external_addresses: config.external_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
        relay_reservations: config.relay_reservation_multiaddrs()?,
        relay_server: config.network.relay.server,
        relay_resources: config.network.relay.resources,
        resources: config.resources,
        discovery: config.network.discovery.clone(),
    })?;
    wait_for_listen_address(&mut node).await;
    fs::write(ready_file, b"ready").expect("write relay ready file");
    while let Some(event) = node.swarm.next().await {
        eprintln!("{event:?}");
    }
    Ok(())
}

async fn run_bootstrap_child(
    identity: NodeIdentity,
    ready_file: PathBuf,
) -> Result<(), runner::RunnerError> {
    let config = bootstrap_config(&identity);
    let mut node = build_node(&HostConfig {
        identity: config.identity()?,
        network_name: config.network.name.clone(),
        membership_tag: config.membership_tag()?,
        mtu: config.effective_packet_mtu(),
        max_concurrent_control_streams: config.resources.control_stream_limit(),
        max_concurrent_packet_streams: config.resources.packet_stream_limit(),
        listen_addresses: config.listen_multiaddrs()?,
        external_addresses: config.external_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
        relay_reservations: config.relay_reservation_multiaddrs()?,
        relay_server: config.network.relay.server,
        relay_resources: config.network.relay.resources,
        resources: config.resources,
        discovery: config.network.discovery.clone(),
    })?;
    wait_for_listen_address(&mut node).await;
    fs::write(ready_file, b"ready").expect("write bootstrap ready file");
    while let Some(event) = node.swarm.next().await {
        if let SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
            peer_id,
            info,
            ..
        })) = &event
        {
            for address in &info.listen_addrs {
                node.swarm
                    .behaviour_mut()
                    .kad
                    .add_address(peer_id, address.clone());
            }
        }
        eprintln!("{event:?}");
    }
    Ok(())
}

async fn wait_for_node_ready(node: &mut p2p_vpn::runtime::p2p::P2pNode, config: &Config) {
    let expected_relayed_addresses = expected_relayed_addresses(node, config);
    if expected_relayed_addresses.is_empty() {
        wait_for_listen_address(node).await;
        return;
    }

    let mut physical_listen = false;
    let mut relay_reservation = false;
    let mut relayed_listen = false;
    while !(physical_listen && relay_reservation && relayed_listen) {
        match node.swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                eprintln!("observed listen address {address}");
                if expected_relayed_addresses
                    .iter()
                    .any(|expected| expected == &address)
                {
                    relayed_listen = true;
                } else {
                    physical_listen = true;
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Relay(
                relay::client::Event::ReservationReqAccepted {
                    relay_peer_id,
                    renewal,
                    ..
                },
            )) => {
                eprintln!("relay reservation accepted by {relay_peer_id} renewal={renewal}");
                relay_reservation = true;
            }
            event => {
                eprintln!("readiness event {event:?}");
            }
        }
    }
}

fn expected_relayed_addresses(
    node: &p2p_vpn::runtime::p2p::P2pNode,
    config: &Config,
) -> Vec<Multiaddr> {
    config
        .relay_reservation_multiaddrs()
        .expect("valid relay reservation addresses")
        .into_iter()
        .map(|address| address.with(Protocol::P2p(node.local_peer_id)))
        .collect()
}

async fn wait_for_listen_address(node: &mut p2p_vpn::runtime::p2p::P2pNode) {
    loop {
        if let SwarmEvent::NewListenAddr { .. } = node.swarm.select_next_some().await {
            return;
        }
    }
}

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("start file {} did not appear", path.display());
}

fn node_config(
    name: &str,
    interface: &str,
    identity: &NodeIdentity,
    listen_address: &str,
    local_routes: Vec<RouteConfig>,
    peer: PeerConfig,
) -> Config {
    Config {
        network: NetworkConfig {
            name: name.to_owned(),
            local_peer: identity.peer_id.clone(),
            private_key: Some(identity.private_key.clone()),
            membership_key: None,
            previous_membership_tags: Vec::new(),
            member_records: Vec::new(),
            routes: local_routes,
            listen_addresses: vec![listen_address.to_owned()],
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            discovery: p2p_vpn::config::DiscoveryConfig::default(),
            relay: p2p_vpn::config::RelayConfig::default(),
            packet_plane: p2p_vpn::config::PacketPlaneConfig::default(),
        },
        interface: InterfaceConfig {
            name: interface.to_owned(),
            mtu: 1280,
        },
        peers: vec![peer],
        queue: QueueConfig {
            max_packets_per_peer: 64,
            max_bytes_per_peer: 128 * 1024,
            max_packet_age_millis: 1_000,
        },
        resources: ResourceConfig::default(),
    }
}

fn relay_config(identity: &NodeIdentity) -> Config {
    Config {
        network: NetworkConfig {
            name: NETWORK_NAME.to_owned(),
            local_peer: identity.peer_id.clone(),
            private_key: Some(identity.private_key.clone()),
            membership_key: None,
            previous_membership_tags: Vec::new(),
            member_records: Vec::new(),
            routes: Vec::new(),
            listen_addresses: vec!["/ip4/10.251.0.254/tcp/42200".to_owned()],
            external_addresses: vec!["/ip4/10.251.0.254/tcp/42200".to_owned()],
            bootstrap_peers: Vec::new(),
            discovery: relay_test_discovery(),
            relay: RelayConfig {
                server: true,
                reservations: Vec::new(),
                resources: RelayResourceConfig::default(),
            },
            packet_plane: p2p_vpn::config::PacketPlaneConfig::default(),
        },
        interface: InterfaceConfig {
            name: "unused-relay".to_owned(),
            mtu: 1280,
        },
        peers: Vec::new(),
        queue: QueueConfig {
            max_packets_per_peer: 64,
            max_bytes_per_peer: 128 * 1024,
            max_packet_age_millis: 1_000,
        },
        resources: ResourceConfig::default(),
    }
}

fn bootstrap_config(identity: &NodeIdentity) -> Config {
    Config {
        network: NetworkConfig {
            name: NETWORK_NAME.to_owned(),
            local_peer: identity.peer_id.clone(),
            private_key: Some(identity.private_key.clone()),
            membership_key: None,
            previous_membership_tags: Vec::new(),
            member_records: Vec::new(),
            routes: Vec::new(),
            listen_addresses: vec!["/ip4/10.252.0.254/tcp/42300".to_owned()],
            external_addresses: vec!["/ip4/10.252.0.254/tcp/42300".to_owned()],
            bootstrap_peers: Vec::new(),
            discovery: dht_bootstrap_discovery(),
            relay: RelayConfig::default(),
            packet_plane: p2p_vpn::config::PacketPlaneConfig::default(),
        },
        interface: InterfaceConfig {
            name: "unused-bootstrap".to_owned(),
            mtu: 1280,
        },
        peers: Vec::new(),
        queue: QueueConfig {
            max_packets_per_peer: 64,
            max_bytes_per_peer: 128 * 1024,
            max_packet_age_millis: 1_000,
        },
        resources: ResourceConfig::default(),
    }
}

fn relay_test_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        mdns: false,
        kademlia: false,
        kademlia_provider_advertisement: false,
        kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
        dcutr: false,
        autonat: false,
    }
}

fn relay_promotion_test_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        dcutr: true,
        autonat: true,
        ..relay_test_discovery()
    }
}

fn dht_test_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        mdns: false,
        kademlia: true,
        kademlia_provider_advertisement: true,
        kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
        dcutr: false,
        autonat: false,
    }
}

fn dht_bootstrap_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        mdns: false,
        kademlia: true,
        kademlia_provider_advertisement: false,
        kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
        dcutr: false,
        autonat: false,
    }
}

fn mdns_test_discovery() -> DiscoveryConfig {
    DiscoveryConfig {
        mdns: true,
        kademlia: false,
        kademlia_provider_advertisement: false,
        kademlia_protocol: "/p2p-vpn/kad/1".to_owned(),
        dcutr: false,
        autonat: false,
    }
}

fn is_dht_test_child() -> bool {
    env::args().any(|argument| argument == DHT_TEST_NAME)
}

fn is_mdns_test_child() -> bool {
    env::args().any(|argument| argument == MDNS_TEST_NAME)
}

fn is_direct_quic_test_child() -> bool {
    env::args().any(|argument| argument == DIRECT_QUIC_TEST_NAME)
}

fn is_relay_promotion_test_child() -> bool {
    env::args().any(|argument| argument == RELAY_PROMOTION_TEST_NAME)
}

fn is_invite_relay_test_child() -> bool {
    env::args().any(|argument| argument == INVITE_RELAY_TEST_NAME)
}

fn peer_config(
    identity: &NodeIdentity,
    address: Option<&str>,
    routes: Vec<RouteConfig>,
) -> PeerConfig {
    PeerConfig {
        id: identity.peer_id.clone(),
        name: None,
        addresses: address.into_iter().map(str::to_owned).collect(),
        routes,
    }
}

fn open_and_configure_tun(config: &TunRuntimeConfig) -> TunDevice {
    let device = TunDevice::open(config).expect("open TUN device");
    for command in config.route_commands() {
        let status = command.execute().expect("execute ip command");
        assert!(status.success(), "`{command}` exited with {status}");
    }
    device
}

fn ping_from_namespace(pid: u32, interface: &str, destination: Ipv4Addr) -> Output {
    let destination = destination.to_string();
    ns_command_output(
        pid,
        "ping",
        &["-c", "5", "-W", "2", "-I", interface, &destination],
    )
}

fn ns_command(pid: u32, program: &str, args: &[&str]) {
    let output = ns_command_output(pid, program, args);
    assert_output_success("nsenter", &output);
}

fn ns_command_output(pid: u32, program: &str, args: &[&str]) -> Output {
    let pid = pid.to_string();
    let mut nsenter_args = vec!["-t", pid.as_str(), "-n", program];
    nsenter_args.extend_from_slice(args);
    command_output("nsenter", &nsenter_args, &[], Duration::from_secs(20)).expect("execute nsenter")
}

fn run_command(program: &str, args: &[&str]) {
    let output = command_output(program, args, &[], Duration::from_secs(20))
        .unwrap_or_else(|error| panic!("failed to execute `{program}`: {error}"));
    assert_output_success(program, &output);
}

fn command_output(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    timeout: Duration,
) -> io::Result<Output> {
    let mut command = Command::new(program);
    command.args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait_with_output();
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| format!("failed to read log: {error}"))
}

fn assert_output_success(context: &str, output: &Output) {
    assert!(
        output.status.success(),
        "`{context}` exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
