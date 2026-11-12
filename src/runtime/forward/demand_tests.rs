use super::*;
use crate::{
    identity::NodeIdentity,
    route::{builtin_ipv4, builtin_ipv6},
};

fn fixture() -> (Config, Libp2pPeerId, Forwarder) {
    let local = NodeIdentity::generate_ed25519().unwrap();
    let remote: Libp2pPeerId = NodeIdentity::generate_ed25519()
        .unwrap()
        .peer_id
        .parse()
        .unwrap();
    let config: Config = serde_json::from_value(serde_json::json!({
        "network": {"name":"demand-activity", "private_key":local.private_key},
        "peers":[{"id":remote.to_string()}],
    }))
    .unwrap();
    let forwarder = Forwarder::from_config(&config).unwrap();
    (config, remote, forwarder)
}

fn packet4(source: Ipv4Addr, target: Ipv4Addr) -> Vec<u8> {
    let mut bytes = vec![0; 20];
    bytes[0] = 0x45;
    bytes[12..16].copy_from_slice(&source.octets());
    bytes[16..20].copy_from_slice(&target.octets());
    bytes
}

fn packet6(source: Ipv6Addr, target: Ipv6Addr) -> Vec<u8> {
    let mut bytes = vec![0; 40];
    bytes[0] = 0x60;
    bytes[8..24].copy_from_slice(&source.octets());
    bytes[24..40].copy_from_slice(&target.octets());
    bytes
}

#[test]
fn demand_records_new_tun_admission_but_not_queued_frame_retries() {
    let (config, remote, mut forwarder) = fixture();
    let peer = PeerId::from_libp2p(remote);
    let local = config.local_peer_id().unwrap();
    let before = forwarder.application_demand(peer).unwrap();
    assert_eq!(before.last_demand, None);
    assert!(forwarder.grant_data_path_quiet(peer, before.version.unwrap()));
    let mut queues = PeerQueues::new(4, 4096);
    forwarder
        .enqueue_tun_packet(
            &mut queues,
            packet4(builtin_ipv4(local), builtin_ipv4(peer)),
        )
        .unwrap();
    let admitted = forwarder.application_demand(peer).unwrap();
    assert!(admitted.last_demand.is_some());
    assert_ne!(admitted.version, before.version);
    assert!(!forwarder.data_path_is_dormant(peer));
    let queued = queues.dequeue().unwrap();
    for _ in 0..100 {
        forwarder
            .queued_packet_frame_with_mtu(&queued, 1280)
            .unwrap();
    }
    assert_eq!(forwarder.application_demand(peer), Some(admitted));
}

#[test]
fn demand_includes_valid_backpressured_attempts_but_not_rejected_ip() {
    let (config, remote, mut forwarder) = fixture();
    let peer = PeerId::from_libp2p(remote);
    let local = config.local_peer_id().unwrap();
    let mut queues = PeerQueues::new(1, 4096);
    let packet = packet4(builtin_ipv4(local), builtin_ipv4(peer));
    forwarder
        .enqueue_tun_packet(&mut queues, packet.clone())
        .unwrap();
    let first = forwarder.application_demand(peer).unwrap();
    assert!(matches!(
        forwarder.enqueue_tun_packet(&mut queues, packet),
        Err(ForwardError::Enqueue(_))
    ));
    let dropped = forwarder.application_demand(peer).unwrap();
    assert_ne!(first.version, dropped.version);
    for invalid in [
        vec![0; 1290],
        packet4("192.0.2.7".parse().unwrap(), builtin_ipv4(peer)),
        packet4(builtin_ipv4(local), "203.0.113.7".parse().unwrap()),
    ] {
        assert!(forwarder.enqueue_tun_packet(&mut queues, invalid).is_err());
    }
    assert_eq!(forwarder.application_demand(peer), Some(dropped));
}

#[test]
fn demand_records_valid_inbound_ipv4_ipv6_and_not_rejected_replays() {
    let (config, remote, mut forwarder) = fixture();
    let peer = PeerId::from_libp2p(remote);
    let local = config.local_peer_id().unwrap();
    let v4 = Frame::packet(10, 1, packet4(builtin_ipv4(peer), builtin_ipv4(local))).unwrap();
    let v6 = Frame::packet(11, 1, packet6(builtin_ipv6(peer), builtin_ipv6(local))).unwrap();
    forwarder.accept_inbound_stream_packet(remote, &v4).unwrap();
    let stream = forwarder.application_demand(peer).unwrap();
    forwarder.accept_inbound_packet(remote, &v6).unwrap();
    let datagram = forwarder.application_demand(peer).unwrap();
    assert_ne!(stream.version, datagram.version);
    assert!(forwarder.accept_inbound_packet(remote, &v6).is_err());
    let spoofed = Frame::packet(
        12,
        1,
        packet4("192.0.2.1".parse().unwrap(), builtin_ipv4(local)),
    )
    .unwrap();
    assert!(
        forwarder
            .accept_inbound_stream_packet(remote, &spoofed)
            .is_err()
    );
    assert!(
        forwarder
            .accept_inbound_stream_packet(Libp2pPeerId::random(), &v4)
            .is_err()
    );
    assert_eq!(forwarder.application_demand(peer), Some(datagram));
}

#[test]
fn demand_ignores_inbound_and_outbound_maintenance_and_keeps_quiet() {
    let (_, remote, mut forwarder) = fixture();
    let peer = PeerId::from_libp2p(remote);
    let before = forwarder.application_demand(peer).unwrap();
    assert!(forwarder.grant_data_path_quiet(peer, before.version.unwrap()));
    let keepalive = Frame::keepalive(20, 1).unwrap();
    let probe = Frame::path_probe(21, 1, b"probe".to_vec()).unwrap();
    forwarder
        .accept_inbound_control_frame(remote, &keepalive, PayloadType::Keepalive)
        .unwrap();
    forwarder
        .accept_inbound_control_frame(remote, &probe, PayloadType::PathProbe)
        .unwrap();
    forwarder
        .validate_inbound_packet_plane_control_frame(remote, &probe, PayloadType::PathProbe)
        .unwrap();
    forwarder.path_probe_frame_with_mtu(1280, b"probe").unwrap();
    assert_eq!(forwarder.application_demand(peer), Some(before));
    assert!(forwarder.data_path_is_dormant(peer));
}

#[test]
fn demand_reconfiguration_preserves_activity_but_revokes_changed_authority() {
    let (mut config, remote, mut forwarder) = fixture();
    let peer = PeerId::from_libp2p(remote);
    forwarder.record_application_demand(peer, Instant::now());
    let activity = forwarder.application_demand(peer).unwrap();
    assert!(forwarder.grant_data_path_quiet(peer, activity.version.unwrap()));
    config.peers[0].name = Some("renamed".into());
    let metadata = forwarder
        .prepare_reconfigure(config.clone(), 1_000)
        .unwrap();
    assert_eq!(forwarder.application_demand(peer), Some(activity));
    assert!(forwarder.data_path_is_dormant(peer));
    forwarder.try_commit_reconfigure(metadata).unwrap();
    assert_eq!(forwarder.application_demand(peer), Some(activity));
    assert!(forwarder.data_path_is_dormant(peer));

    config.peers[0].routes.push(RouteConfig {
        prefix: "10.77.0.0/24".into(),
        metric: 10,
    });
    let changed = forwarder
        .prepare_reconfigure(config.clone(), 1_000)
        .unwrap();
    assert!(forwarder.data_path_is_dormant(peer));
    forwarder.try_commit_reconfigure(changed).unwrap();
    assert_eq!(forwarder.application_demand(peer), Some(activity));
    assert!(!forwarder.data_path_is_dormant(peer));

    let mut removed = config.clone();
    removed.peers.clear();
    let changed = forwarder.prepare_reconfigure(removed, 1_000).unwrap();
    forwarder.try_commit_reconfigure(changed).unwrap();
    assert_eq!(forwarder.application_demand(peer), None);
    let changed = forwarder.prepare_reconfigure(config, 1_000).unwrap();
    forwarder.try_commit_reconfigure(changed).unwrap();
    assert_eq!(
        forwarder.application_demand(peer).unwrap().last_demand,
        None
    );
    assert!(!forwarder.grant_data_path_quiet(peer, activity.version.unwrap()));
}
