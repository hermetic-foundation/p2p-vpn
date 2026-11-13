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
fn demand_tracks_legacy_membership_restore_merge_revoke_and_readmission() {
    use crate::membership::{
        MembershipRecordIssueOptions, MembershipRecordOptions, MembershipRecordSubject,
        MembershipRole, issue_membership_record_at, issue_membership_record_for_subject_at,
    };

    for restored in [false, true] {
        let issuer = NodeIdentity::generate_ed25519().unwrap();
        let local = PeerId::from_libp2p(issuer.peer_id.parse().unwrap());
        let member = std::iter::repeat_with(|| NodeIdentity::generate_ed25519().unwrap())
            .take(1024)
            .find(|member| {
                builtin_ipv4(PeerId::from_libp2p(member.peer_id.parse().unwrap()))
                    != builtin_ipv4(local)
            })
            .unwrap();
        let remote: Libp2pPeerId = member.peer_id.parse().unwrap();
        let peer = PeerId::from_libp2p(remote);
        let config: Config = serde_json::from_value(serde_json::json!({
            "network": {"name":"membership-demand", "private_key":issuer.private_key},
            "peers":[],
        }))
        .unwrap();
        let mut forwarder = Forwarder::from_config(&config).unwrap();
        let grant = |sequence, membership_epoch| {
            issue_membership_record_at(
                &issuer,
                MembershipRecordOptions {
                    network_name: "membership-demand".to_owned(),
                    member: member.clone(),
                    membership_epoch,
                    sequence,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1000 + sequence,
            )
            .unwrap()
        };
        let record = grant(1, 1);
        assert!(forwarder.application_demand(peer).is_none());
        if restored {
            forwarder
                .restore_persisted_membership_records(std::slice::from_ref(&record), 1002)
                .unwrap();
        } else {
            forwarder
                .merge_membership_records(std::slice::from_ref(&record), 1002)
                .unwrap();
        }
        assert!(forwarder.is_configured_transport_peer(remote));
        let admitted = forwarder
            .application_demand(peer)
            .expect("learned packet peer must have demand tracking");
        assert_eq!(admitted.last_demand, None);
        assert!(forwarder.pause_data_path_maintenance(peer, admitted.version.unwrap()));
        assert!(forwarder.grant_data_path_quiet(peer, admitted.version.unwrap()));
        let mut queues = PeerQueues::new(2, 4096);
        forwarder
            .enqueue_tun_packet(
                &mut queues,
                packet4(builtin_ipv4(local), builtin_ipv4(peer)),
            )
            .unwrap();
        let demanded = forwarder.application_demand(peer).unwrap();
        assert!(demanded.last_demand.is_some());
        assert_ne!(admitted.version, demanded.version);
        assert!(!forwarder.data_path_maintenance_is_paused(peer));
        assert!(forwarder.grant_data_path_quiet(peer, demanded.version.unwrap()));
        for refresh in [record, grant(2, 1)] {
            forwarder
                .merge_membership_records(&[refresh], 1003)
                .unwrap();
            assert_eq!(forwarder.application_demand(peer), Some(demanded));
            assert!(forwarder.data_path_is_dormant(peer));
        }
        let revoked = issue_membership_record_for_subject_at(
            &issuer,
            MembershipRecordIssueOptions {
                network_name: "membership-demand".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).unwrap(),
                membership_epoch: 1,
                sequence: 3,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1003,
        )
        .unwrap();
        forwarder
            .merge_membership_records(&[revoked], 1004)
            .unwrap();
        assert!(!forwarder.is_configured_transport_peer(remote));
        assert!(forwarder.application_demand(peer).is_none());
        assert!(!forwarder.grant_data_path_quiet(peer, demanded.version.unwrap()));
        forwarder
            .merge_membership_records(&[grant(4, 2)], 1005)
            .unwrap();
        assert!(forwarder.is_configured_transport_peer(remote));
        let readmitted = forwarder.application_demand(peer).unwrap();
        assert_eq!(readmitted.last_demand, None);
        assert_ne!(readmitted.version, admitted.version);
        assert!(!forwarder.grant_data_path_quiet(peer, admitted.version.unwrap()));
        assert!(!forwarder.grant_data_path_quiet(peer, demanded.version.unwrap()));
    }
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
fn only_first_negotiated_wake_packet_gets_fixed_recovery_retention() {
    let (config, remote, mut forwarder) = fixture();
    let peer = PeerId::from_libp2p(remote);
    let local = config.local_peer_id().unwrap();
    let bytes = packet4(builtin_ipv4(local), builtin_ipv4(peer));
    let mut queues = PeerQueues::with_packet_ttl(2, 4096, Duration::from_secs(3));
    let before = forwarder.application_demand(peer).unwrap();
    assert!(forwarder.grant_data_path_quiet(peer, before.version.unwrap()));
    forwarder
        .enqueue_tun_packet(&mut queues, bytes.clone())
        .unwrap();
    let demand = forwarder.application_demand(peer).unwrap();
    let first = queues.dequeue().unwrap();
    let original_time = first.enqueued_at();
    let expiry = original_time + crate::queue::FIRST_WAKE_PACKET_RETENTION;
    assert!(!first.is_expired(
        original_time + Duration::from_secs(15),
        Duration::from_secs(3)
    ));
    assert!(!first.is_expired(expiry, Duration::from_secs(3)));
    assert!(first.is_expired(expiry + Duration::from_nanos(1), Duration::from_secs(3)));
    assert!(
        !first.is_expired(expiry + Duration::from_secs(1), Duration::from_secs(60)),
        "an explicitly longer ordinary age remains unchanged"
    );
    for _ in 0..100 {
        forwarder
            .queued_packet_frame_with_mtu(&first, 1280)
            .unwrap();
        queues.enqueue(first.clone()).unwrap();
        assert_eq!(queues.dequeue().unwrap().enqueued_at(), original_time);
    }
    assert_eq!(forwarder.application_demand(peer), Some(demand));

    forwarder.enqueue_tun_packet(&mut queues, bytes).unwrap();
    let next = queues.dequeue().unwrap();
    assert!(next.is_expired(
        next.enqueued_at() + Duration::from_secs(4),
        Duration::from_secs(3)
    ));
    queues.enqueue(first).unwrap();
    queues.enqueue(next).unwrap();
    queues.drop_expired(original_time + Duration::from_secs(4));
    assert_eq!(queues.total_stats().queued_packets, 1);
    assert_eq!(queues.total_stats().expired_packets, 1);
    queues.drop_expired(expiry + Duration::from_nanos(1));
    assert_eq!(queues.total_stats().queued_packets, 0);
    assert_eq!(queues.total_stats().expired_packets, 2);
}

#[test]
fn idle_preparation_and_legacy_admission_keep_ordinary_packet_age() {
    let (config, remote, mut forwarder) = fixture();
    let peer = PeerId::from_libp2p(remote);
    let local = config.local_peer_id().unwrap();
    let mut queues = PeerQueues::new(2, 4096);
    for preparing in [false, true] {
        if preparing {
            let version = forwarder.application_demand(peer).unwrap().version.unwrap();
            assert!(forwarder.pause_data_path_maintenance(peer, version));
        }
        forwarder
            .enqueue_tun_packet(
                &mut queues,
                packet4(builtin_ipv4(local), builtin_ipv4(peer)),
            )
            .unwrap();
        let packet = queues.dequeue().unwrap();
        assert!(packet.is_expired(
            packet.enqueued_at() + Duration::from_secs(4),
            Duration::from_secs(3)
        ));
    }
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
