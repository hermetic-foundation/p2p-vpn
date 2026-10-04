use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hickory_proto::{
    op::{Message, Query},
    rr::{Name, RData, RecordType},
};
use p2p_vpn::{
    hostname::issue_hostname_record_at,
    membership::{
        MembershipRecordIssueOptions, MembershipRecordSubject, MembershipRole,
        SignedMembershipRecord, issue_named_membership_record_for_subject_at,
    },
};
use serde_json::{Value, json};
use std::{
    net::UdpSocket,
    os::unix::{
        fs::{FileTypeExt as _, OpenOptionsExt as _},
        net::UnixStream,
        process::CommandExt as _,
    },
    time::{SystemTime, UNIX_EPOCH},
};

pub const TEST_NAME: &str = "tun_namespace_checkpoint_migration_compacts_and_recovers";
const NETWORK: &str = "checkpoint-lab";
const BRIDGE: &str = "br-checkpoint";
const ROLES: [&str; 3] = ["a", "b", "c"];
const DNS_NAME_ENV: &str = "P2P_VPN_CHECKPOINT_DNS_NAME";
const DNS_MARKER: &str = "checkpoint_dns_probe ";
const ALIAS: &str = "10.43.1.1";

pub fn run() {
    let temp = init_namespace_temp_dir(&env::temp_dir().join("p2p-vpn-checkpoint"), TEST_NAME);
    let identities = distinct_identities();
    initialize_legacy(&temp, &identities);
    run_command("ip", &["link", "add", BRIDGE, "type", "bridge"]);
    run_command("ip", &["link", "set", BRIDGE, "up"]);
    let mut nodes: [Option<NamespaceChild>; 3] = std::array::from_fn(|_| None);
    for index in (0..3).rev() {
        nodes[index] = Some(start_node(&temp, index, &identities[index]));
        wait_for_daemon_running(&temp, ROLES[index]);
    }
    for role in ROLES {
        wait_for_daemon_running(&temp, role);
        wait_for_peer_ready(&temp, role);
    }
    assert_mesh(&nodes, &temp, &[0, 1, 2]);
    assert_names(&nodes, &[0, 1, 2], &[0, 1, 2]);
    ping(node_pid(&nodes, 1), ALIAS, true);
    eprintln!("checkpoint_migration phase=legacy packets=pass dns=pass");

    install_common_migration(&temp);
    for role in ROLES {
        wait_checkpoint(&temp, role, "participating", 3);
        assert_authority(&temp, role, &identities, &[0, 1, 2]);
        assert_inventory(&temp, role, &identities, &[0, 1, 2]);
    }
    assert_mesh(&nodes, &temp, &[0, 1, 2]);
    assert_names(&nodes, &[0, 1, 2], &[0, 1, 2]);
    ping(node_pid(&nodes, 2), ALIAS, true);
    eprintln!("checkpoint_migration phase=installed packets=pass dns=pass active=3");

    // B is the only online member. C keeps its admission despite A's departure.
    drop(nodes[0].take());
    drop(nodes[2].take());
    let before = authority_revision(&temp, "b");
    let socket = socket_string(&temp, "b");
    pair_cli_json(
        "sole survivor removes creator",
        &[
            "membership",
            "revoke",
            &identities[0].peer_id,
            "--socket",
            &socket,
            "--format",
            "json",
        ],
    );
    wait_checkpoint(&temp, "b", "participating", 2);
    assert!(authority_revision(&temp, "b") > before);
    assert_authority(&temp, "b", &identities, &[1, 2]);
    assert_inventory(&temp, "b", &identities, &[1, 2]);
    assert_names(&nodes, &[1], &[1, 2]);
    assert_removed(&temp, node_pid(&nodes, 1), "b", &identities[0]);
    eprintln!("checkpoint_migration phase=sole_survivor active=2 removed_creator=pass");

    nodes[2] = Some(start_node(&temp, 2, &identities[2]));
    wait_checkpoint_gate(&temp, "c");
    ping(node_pid(&nodes, 2), "10.42.0.2", false);
    wait_checkpoint(&temp, "c", "participating", 2);
    assert_authority(&temp, "c", &identities, &[1, 2]);
    assert_inventory(&temp, "c", &identities, &[1, 2]);
    assert_mesh(&nodes, &temp, &[1, 2]);
    assert_names(&nodes, &[1, 2], &[1, 2]);
    assert_removed(&temp, node_pid(&nodes, 2), "c", &identities[0]);
    eprintln!("checkpoint_migration phase=offline_catchup packets=pass dns=pass active=2");

    nodes[0] = Some(start_node(&temp, 0, &identities[0]));
    wait_checkpoint_gate(&temp, "a");
    ping(node_pid(&nodes, 0), "10.42.0.2", false);
    wait_checkpoint(&temp, "a", "excluded", 2);
    assert_authority(&temp, "a", &identities, &[1, 2]);
    ping(node_pid(&nodes, 0), "10.42.0.2", false);
    for role in ["b", "c"] {
        assert_inventory(&temp, role, &identities, &[1, 2]);
    }
    assert_mesh(&nodes, &temp, &[1, 2]);
    eprintln!("checkpoint_migration phase=removed_stale_restart access=denied resurrection=absent");

    drop(nodes[1].take());
    nodes[1] = Some(start_node(&temp, 1, &identities[1]));
    wait_checkpoint_gate(&temp, "b");
    wait_checkpoint(&temp, "b", "participating", 2);
    assert_mesh(&nodes, &temp, &[1, 2]);
    assert_names(&nodes, &[1, 2], &[1, 2]);
    for index in [1, 2] {
        assert_authority(&temp, ROLES[index], &identities, &[1, 2]);
        assert_removed(&temp, node_pid(&nodes, index), ROLES[index], &identities[0]);
    }
    let bytes: Vec<u64> = ROLES
        .iter()
        .map(|role| {
            fs::metadata(authority_path(&temp, role))
                .expect("authority size")
                .len()
        })
        .collect();
    assert!(bytes.iter().all(|size| *size < 64 * 1024));
    eprintln!(
        "checkpoint_migration phase=survivor_restart packets=pass dns=pass authority_bytes={bytes:?}"
    );
    drop(nodes);
    cleanup_temp_dir(temp);
}

pub fn run_node() {
    let temp = PathBuf::from(required_env("P2P_VPN_TUN_E2E_TEMP"));
    let role = required_env("P2P_VPN_TUN_E2E_ROLE");
    let start = PathBuf::from(required_env("P2P_VPN_TUN_E2E_START"));
    wait_for_file_with_timeout(&start, Duration::from_mins(1));
    // exec keeps NamespaceChild::id() as the actual daemon for stop/reap and nsenter.
    let error = Command::new(current_test_binary())
        .arg("up")
        .arg("--config")
        .arg(child_config_path(&temp, &role))
        .arg("--control-socket")
        .arg(node_control_socket(&temp, &role))
        .arg("--membership-state")
        .arg(authority_path(&temp, &role))
        .arg("--pairing-state")
        .arg(state_dir(&temp, &role).join("pairing-state.json"))
        .args(["--metrics-interval-seconds", "1"])
        .env_remove("P2P_VPN_TUN_E2E_LOCAL_KEY")
        .exec();
    panic!("execute native daemon: {error}");
}

pub fn run_dns_probe() {
    let name = required_env(DNS_NAME_ENV);
    let mut query = Message::new();
    query
        .set_id(42)
        .set_recursion_desired(true)
        .add_query(Query::query(
            Name::from_ascii(&name).expect("DNS name"),
            RecordType::A,
        ));
    let socket = UdpSocket::bind("127.0.0.1:0").expect("DNS client");
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("DNS timeout");
    socket.connect("127.0.0.1:53535").expect("DNS listener");
    socket
        .send(&query.to_vec().expect("encode DNS request"))
        .expect("DNS send");
    let mut bytes = [0_u8; 4_096];
    let length = socket.recv(&mut bytes).expect("DNS receive");
    let response = Message::from_vec(&bytes[..length]).expect("decode DNS response");
    assert_eq!(response.id(), 42);
    let addresses: Vec<String> = response
        .answers()
        .iter()
        .filter_map(|answer| {
            if let RData::A(address) = answer.data() {
                Some(address.0.to_string())
            } else {
                None
            }
        })
        .collect();
    println!(
        "{DNS_MARKER}{}",
        json!({
            "response_code": u16::from(response.response_code()),
            "authoritative": response.authoritative(), "addresses": addresses,
        })
    );
}

fn distinct_identities() -> [NodeIdentity; 4] {
    let mut addresses = std::collections::HashSet::new();
    std::array::from_fn(|_| {
        loop {
            let identity = NodeIdentity::generate_ed25519().expect("fixture identity");
            let peer = p2p_vpn::PeerId::from_libp2p(identity.peer_id.parse().expect("peer ID"));
            if addresses.insert(p2p_vpn::route::builtin_ipv4(peer)) {
                break identity;
            }
        }
    })
}

fn initialize_legacy(temp: &Path, identities: &[NodeIdentity; 4]) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let mut records: Vec<SignedMembershipRecord> = identities
        .iter()
        .enumerate()
        .map(|(index, subject)| {
            let issuer = &identities[usize::from(index >= 2)];
            legacy_record(issuer, subject, index, false, now - 60)
        })
        .collect();
    records.push(legacy_record(
        &identities[1],
        &identities[3],
        3,
        true,
        now - 30,
    ));
    let names: Vec<_> = identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            issue_hostname_record_at(
                identity,
                NETWORK,
                &format!("node-{}", ["a", "b", "c", "d"][index]),
                2,
                now - 10,
            )
            .expect("self-signed current hostname")
        })
        .collect();
    for (index, role) in ROLES.iter().enumerate() {
        let directory = create_private_namespace_dir(&temp.join(format!("state-{role}")))
            .expect("protected state directory");
        fs::rename(directory, state_dir(temp, role)).expect("name state directory");
        let identity = &identities[index];
        let bytes = serde_json::to_vec(&json!({
            "version": 2, "network_name": NETWORK, "local_peer": identity.peer_id,
            "records": records, "hostname_records": names,
        }))
        .expect("legacy authority encoding");
        write_private(&authority_path(temp, role), &bytes);
        let peers: Vec<_> = identities[..3]
            .iter()
            .enumerate()
            .filter(|(remote, _)| *remote != index)
            .map(|(remote, peer)| {
                json!({
                    "id": peer.peer_id, "name": format!("node-{}", ROLES[remote]),
                    "vpn_ip": format!("10.42.0.{}", remote + 1),
                    "addresses": if index < remote {
                        vec![format!("/ip4/10.254.0.{}/tcp/4001/p2p/{}", remote + 1, peer.peer_id)]
                    } else { vec![] },
                    "routes": if remote == 0 { vec![alias_route()] } else { vec![] },
                })
            })
            .collect();
        let mut config: Config = serde_json::from_value(json!({
            "network": {
                "name": NETWORK, "local_peer": identity.peer_id, "private_key": identity.private_key,
                "membership_key": STANDARD.encode([7_u8; 32]),
                "vpn_ip": format!("10.42.0.{}", index + 1),
                "listen_addresses": [format!("/ip4/10.254.0.{}/tcp/4001", index + 1)],
                "dns": {"enabled": true, "hostname": format!("node-{role}"), "listen": "127.0.0.1:53535", "ttl_seconds": 1},
                "routes": if index == 0 { vec![alias_route()] } else { vec![] },
            },
            "interface": {"name": "pv0", "mtu": 1280}, "peers": peers,
        })).expect("fixture config");
        config.network.discovery = mdns_test_discovery();
        config.network.relay.auto.max_candidates = 0;
        config.network.relay.auto.max_reservations = 0;
        config.network.packet_plane.listen.clear();
        config.network.packet_plane.quic_listen.clear();
        config.validate_runtime().expect("valid fixture config");
        write_private(
            &child_config_path(temp, role),
            &serde_json::to_vec(&config).expect("config encoding"),
        );
    }
}

fn alias_route() -> RouteConfig {
    RouteConfig {
        prefix: format!("{ALIAS}/32"),
        metric: 7,
    }
}

fn legacy_record(
    issuer: &NodeIdentity,
    subject: &NodeIdentity,
    index: usize,
    revoked: bool,
    now: u64,
) -> SignedMembershipRecord {
    issue_named_membership_record_for_subject_at(
        issuer,
        MembershipRecordIssueOptions {
            network_name: NETWORK.to_owned(),
            member: MembershipRecordSubject::from_identity(subject).expect("subject"),
            membership_epoch: 1,
            sequence: if revoked { 2 } else { 1 },
            revoked,
            roles: if revoked {
                vec![]
            } else {
                vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ]
            },
            route_grants: if revoked {
                vec![]
            } else {
                let mut routes = vec![RouteConfig {
                    prefix: format!("10.42.0.{}/32", index + 1),
                    metric: 0,
                }];
                if index == 0 {
                    routes.push(alias_route());
                }
                routes
            },
            expires_at_unix_seconds: None,
        },
        (!revoked)
            .then(|| format!("old-{}", ["a", "b", "c", "d"][index]))
            .as_deref(),
        now,
    )
    .expect("legacy membership signature")
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .expect("create owner-only fixture file");
    file.write_all(bytes).expect("write fixture file");
    file.sync_all().expect("sync fixture file");
}

fn state_dir(temp: &Path, role: &str) -> PathBuf {
    temp.join(format!("state-{role}"))
}
fn authority_path(temp: &Path, role: &str) -> PathBuf {
    state_dir(temp, role).join("membership-state.json")
}
fn socket_string(temp: &Path, role: &str) -> String {
    node_control_socket(temp, role)
        .to_string_lossy()
        .into_owned()
}
fn node_pid(nodes: &[Option<NamespaceChild>; 3], index: usize) -> u32 {
    nodes[index].as_ref().expect("online node").id()
}

fn start_node(temp: &Path, index: usize, identity: &NodeIdentity) -> NamespaceChild {
    let role = ROLES[index];
    // Model systemd's ephemeral RuntimeDirectory, without touching protected authority.
    let socket = node_control_socket(temp, role);
    if socket.exists() {
        assert!(
            fs::symlink_metadata(&socket)
                .expect("socket metadata")
                .file_type()
                .is_socket()
        );
        let error = UnixStream::connect(&socket).expect_err("old daemon must already be stopped");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        fs::remove_file(socket).expect("retire stopped daemon's runtime socket");
    }
    let start = temp.join(format!("start-{role}"));
    if start.exists() {
        fs::remove_file(&start).expect("reset start rendezvous");
    }
    let node = spawn_node(TEST_NAME, role, identity, None, None, temp, &start);
    wait_for_child_namespace(node.id());
    attach_veth_to_bridge(
        node.id(),
        role,
        &format!("10.254.0.{}/24", index + 1),
        BRIDGE,
    );
    fs::write(start, b"ready").expect("start daemon after underlay readiness");
    node
}

fn migration(temp: &Path, role: &str, action: &str, fingerprint: Option<&str>) -> Value {
    let socket = socket_string(temp, role);
    let mut args = vec![
        "membership",
        "checkpoint",
        action,
        "--socket",
        &socket,
        "--format",
        "json",
    ];
    if let Some(id) = fingerprint {
        args.extend(["--accept-id", id]);
    }
    pair_cli_json("checkpoint migration command", &args)
}

fn install_common_migration(temp: &Path) {
    // Transfer the identical protected handoff before installation retires its source.
    let prepared = migration(temp, "a", "prepare", None);
    assert_eq!(prepared["active_members"], 3);
    assert_eq!(prepared["current_hostnames"], 3);
    let fingerprint = prepared["migration_id"].as_str().expect("migration ID");
    let source = PathBuf::from(prepared["artifact_path"].as_str().expect("artifact path"));
    for role in ["b", "c"] {
        let status = migration(temp, role, "inspect", None);
        let target = PathBuf::from(
            status["artifact_path"]
                .as_str()
                .expect("recipient artifact"),
        );
        assert!(
            !target.exists(),
            "fresh recipient must not own another handoff"
        );
        write_private(&target, &fs::read(&source).expect("protected source"));
        let received = migration(temp, role, "inspect", None);
        assert_eq!(received["migration_id"], fingerprint);
    }
    for role in ROLES {
        let installed = migration(temp, role, "install", Some(fingerprint));
        assert_eq!(installed["phase"], "installed");
        assert_eq!(installed["active_members"], 3);
        assert!(
            !authority_path(temp, role)
                .with_extension("migration.json")
                .exists()
        );
    }
}

fn wait_checkpoint(temp: &Path, role: &str, state: &str, members: usize) {
    let expected = format!("checkpoint_sync_state {state}");
    wait_for_daemon_state(temp, role, Duration::from_secs(40), &expected, |lines| {
        lines.contains(&expected)
            && state_metric_count(lines, "checkpoint_active_members") == Some(members)
    });
}

fn wait_checkpoint_gate(temp: &Path, role: &str) {
    wait_for_daemon_state(
        temp,
        role,
        Duration::from_secs(10),
        "checkpoint restart gate",
        |lines| {
            lines.iter().any(|line| {
                [
                    "checkpoint_sync_state resync_required",
                    "checkpoint_sync_state resyncing",
                ]
                .contains(&line.as_str())
            })
        },
    );
}

fn authority(temp: &Path, role: &str) -> Value {
    serde_json::from_slice(&fs::read(authority_path(temp, role)).expect("read protected authority"))
        .expect("decode protected authority")
}

fn authority_revision(temp: &Path, role: &str) -> u64 {
    authority(temp, role)["retained"]["snapshot"]["payload"]["authority_revision"]
        .as_u64()
        .expect("revision")
}

fn assert_authority(temp: &Path, role: &str, identities: &[NodeIdentity; 4], active: &[usize]) {
    let value = authority(temp, role);
    assert_eq!(value["version"], 3);
    let payload = &value["retained"]["snapshot"]["payload"];
    let members = payload["members"].as_array().expect("active roster");
    assert_eq!(members.len(), active.len());
    assert_eq!(payload["policy"]["route_grants_enabled"], true);
    for index in active {
        let member = members
            .iter()
            .find(|member| member["subject"]["peer_id"] == identities[*index].peer_id)
            .expect("retained active member");
        assert_eq!(
            member["subject"]["public_key"],
            STANDARD.encode(
                identities[*index]
                    .public_key_protobuf()
                    .expect("member key")
            )
        );
        assert_eq!(
            member["roles"],
            json!(["overlay_member", "route_authority"])
        );
        assert!(
            member["route_grants"]
                .as_array()
                .expect("grants")
                .iter()
                .any(|route| { route["prefix"] == format!("10.42.0.{}/32", index + 1) })
        );
        if *index == 0 {
            assert!(
                member["route_grants"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|route| {
                        route["prefix"] == format!("{ALIAS}/32") && route["metric"] == 7
                    })
            );
        }
    }
    let bytes = fs::read(authority_path(temp, role)).expect("authority bytes");
    let text = std::str::from_utf8(&bytes).expect("authority UTF-8");
    let retained = serde_json::to_string(&value["retained"]).expect("retained authority");
    for (index, identity) in identities.iter().enumerate() {
        if !active.contains(&index) {
            assert!(
                !retained.contains(&identity.peer_id),
                "removed identity retained in authority on {role}"
            );
        }
    }
    for field in [
        "issuer_peer",
        "issuer_public_key",
        "inviter",
        "revoked",
        "records",
    ] {
        assert!(
            !text.contains(&format!("\"{field}\"")),
            "legacy field {field} retained"
        );
    }
    assert!(!text.contains("old-"), "obsolete admission label retained");
    let files: Vec<_> = fs::read_dir(state_dir(temp, role))
        .expect("state files")
        .map(|entry| entry.expect("state entry").file_name())
        .collect();
    assert!(
        files
            .iter()
            .all(|file| file == "membership-state.json" || file == "pairing-state.json"),
        "unexpected retained state files on {role}: {files:?}"
    );
}

fn assert_inventory(temp: &Path, role: &str, identities: &[NodeIdentity; 4], active: &[usize]) {
    let socket = socket_string(temp, role);
    let value = pair_cli_json(
        "peer inventory",
        &["peers", "--socket", &socket, "--format", "json"],
    );
    let peers = value["peers"].as_array().expect("peer list");
    assert_eq!(peers.len(), active.len());
    for index in active {
        assert!(
            peers
                .iter()
                .any(|peer| peer["peer_id"] == identities[*index].peer_id)
        );
    }
}

fn ping(pid: u32, target: &str, success: bool) {
    let output = ns_command_output(
        pid,
        "ping",
        &["-I", "pv0", "-c", "2", "-W", "1", "-i", "0.1", target],
    );
    if success {
        assert_output_success("checkpoint overlay ping", &output);
    } else {
        assert!(
            !output.status.success(),
            "gated/removed overlay unexpectedly transmitted packets"
        );
    }
}

fn assert_mesh(nodes: &[Option<NamespaceChild>; 3], temp: &Path, active: &[usize]) {
    let remote_count = active.len() - 1;
    for index in active {
        wait_for_daemon_state(
            temp,
            ROLES[*index],
            Duration::from_secs(30),
            "complete active mesh paths",
            |lines| {
                state_colon_count(lines, "validated peers") == Some(remote_count)
                    && state_metric_count(lines, "peers_with_supported_path") == Some(remote_count)
                    && state_metric_count(lines, "connected_overlay_peers") == Some(remote_count)
            },
        );
    }
    for from in active {
        let output = ns_command_output(
            node_pid(nodes, *from),
            "ip",
            &["-j", "addr", "show", "dev", "pv0"],
        );
        assert_output_success("checkpoint TUN addresses", &output);
        let addresses: Vec<Value> =
            serde_json::from_slice(&output.stdout).expect("kernel addresses");
        let expected = format!("10.42.0.{}", from + 1);
        assert!(
            addresses.iter().any(|interface| {
                interface["addr_info"]
                    .as_array()
                    .expect("interface addresses")
                    .iter()
                    .any(|address| address["local"] == expected)
            }),
            "node {} missing restored alias {expected}; addresses={addresses:?}",
            ROLES[*from]
        );
        for to in active {
            if from != to {
                let target = format!("10.42.0.{}", to + 1);
                let output = ns_command_output(
                    node_pid(nodes, *from),
                    "ping",
                    &["-I", "pv0", "-c", "2", "-W", "1", "-i", "0.1", &target],
                );
                if !output.status.success() {
                    let roles: Vec<_> = active.iter().map(|index| ROLES[*index]).collect();
                    capture_daemon_snapshots(temp, &roles);
                    for index in [*from, *to] {
                        for args in [
                            &["-j", "addr", "show", "dev", "pv0"][..],
                            &["-j", "route", "show", "dev", "pv0"][..],
                        ] {
                            let state = ns_command_output(node_pid(nodes, index), "ip", args);
                            eprintln!(
                                "checkpoint_packet_failure node={} args={args:?} state={}",
                                ROLES[index],
                                String::from_utf8_lossy(&state.stdout)
                            );
                        }
                    }
                    eprintln!("{}", daemon_snapshot_summary(temp, &roles));
                }
                assert_output_success("checkpoint overlay mesh ping", &output);
            }
        }
    }
}

fn dns(pid: u32, hostname: &str) -> Value {
    let current = env::current_exe().expect("test executable");
    let name = format!("{hostname}.{NETWORK}.p2p-vpn.internal.");
    let output = command_output(
        "nsenter",
        &[
            "-t",
            &pid.to_string(),
            "-n",
            current.to_str().expect("test path"),
            "--ignored",
            TEST_NAME,
            "--exact",
            "--nocapture",
        ],
        &[(CHILD_ENV, "dns"), (DNS_NAME_ENV, &name)],
        Duration::from_secs(3),
    )
    .expect("DNS probe process");
    assert_output_success("namespace wire DNS", &output);
    let text = String::from_utf8(output.stdout).expect("DNS probe output");
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix(DNS_MARKER))
        .expect("marked DNS result");
    serde_json::from_str(line).expect("DNS result JSON")
}

fn assert_names(nodes: &[Option<NamespaceChild>; 3], online: &[usize], active: &[usize]) {
    for from in online {
        for to in active {
            let response = dns(node_pid(nodes, *from), &format!("node-{}", ROLES[*to]));
            assert_eq!(response["response_code"], 0);
            assert_eq!(response["authoritative"], true);
            assert!(
                response["addresses"]
                    .as_array()
                    .expect("DNS answers")
                    .iter()
                    .any(|address| { address == &format!("10.42.0.{}", to + 1) }),
                "current active name must resolve its preserved address"
            );
        }
    }
}

fn assert_removed(temp: &Path, pid: u32, role: &str, removed: &NodeIdentity) {
    let response = dns(pid, "node-a");
    assert_eq!(response["response_code"], 3);
    assert!(
        response["addresses"]
            .as_array()
            .expect("negative DNS answers")
            .is_empty()
    );
    let routes = ns_command_output(pid, "ip", &["-j", "route", "show", "dev", "pv0"]);
    assert_output_success("inspect overlay kernel routes", &routes);
    let routes: Vec<Value> =
        serde_json::from_slice(&routes.stdout).expect("structured kernel routes");
    for prefix in ["10.42.0.1", ALIAS] {
        assert!(
            !routes
                .iter()
                .any(|route| route["dst"] == prefix || route["dst"] == format!("{prefix}/32")),
            "removed route {prefix} retained on {role}"
        );
    }
    let socket = socket_string(temp, role);
    let peers = pair_cli_json(
        "daemon path inventory",
        &["daemon-peers", "--socket", &socket, "--format", "json"],
    );
    assert!(
        !serde_json::to_string(&peers)
            .expect("public inventory")
            .contains(&removed.peer_id),
        "removed connection/path retained on {role}"
    );
}
