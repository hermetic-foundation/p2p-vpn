use p2p_vpn::{
    PeerId,
    config::Config,
    dns::DnsZone,
    identity::NodeIdentity,
    membership::{
        MembershipRecordIssueOptions, MembershipRecordSubject, MembershipRole,
        issue_membership_record_for_subject_at,
    },
    network_peer::{NetworkPeerList, NetworkPeerMembershipState},
    runtime::{forward::Forwarder, packet::AuthorizedPeers, runner::OverlayMembership},
};

#[test]
fn authorization_consumers_agree_on_remote_revocation_and_local_resignation() {
    let local = NodeIdentity::generate_ed25519().expect("local");
    let remote = NodeIdentity::generate_ed25519().expect("remote");
    let static_peer = NodeIdentity::generate_ed25519().expect("static peer");
    let issue = |member: &NodeIdentity, sequence, revoked| {
        issue_membership_record_for_subject_at(
            &local,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(member).expect("subject"),
                membership_epoch: 1,
                sequence,
                revoked,
                roles: if revoked {
                    Vec::new()
                } else {
                    vec![MembershipRole::OverlayMember]
                },
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000 + sequence,
        )
        .expect("signed event")
    };
    for (departed, remote_allowed, static_allowed) in [
        (None, true, true),
        (Some(&remote), false, true),
        (Some(&local), false, false),
    ] {
        let mut config: Config = serde_json::from_value(serde_json::json!({
            "network": {
                "name": "lab",
                "private_key": local.private_key,
                "dns": { "enabled": true, "hostname": "local" }
            },
            "peers": [
                { "id": remote.peer_id, "name": "remote" },
                { "id": static_peer.peer_id, "name": "static" }
            ]
        }))
        .expect("minimal config");
        config.network.member_records = vec![issue(&local, 1, false), issue(&remote, 2, false)];
        if let Some(departed) = departed {
            config.network.member_records.push(issue(departed, 3, true));
        }
        let routes = config.compile_routes().expect("routes");
        let packets = AuthorizedPeers::try_from_config(&config).expect("packet admission");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");
        let runtime = OverlayMembership::from_config(&config).expect("runtime membership");
        let dns =
            DnsZone::from_config_at(&config, &config.network.member_records, 2_000).expect("DNS");
        let inventory =
            NetworkPeerList::from_config_at(&config, &config.network.member_records, 2_000)
                .expect("audit inventory");

        for (identity, hostname, allowed) in [
            (&remote, "remote", remote_allowed),
            (&static_peer, "static", static_allowed),
        ] {
            let transport_peer = identity.peer_id.parse().expect("transport peer");
            let peer = PeerId::from_libp2p(transport_peer);
            assert_eq!(
                routes.routes_for(peer).count() > 0,
                allowed,
                "routes: {hostname}"
            );
            assert_eq!(
                packets.allows(&transport_peer),
                allowed,
                "packets: {hostname}"
            );
            assert_eq!(
                forwarder.is_configured_transport_peer(transport_peer),
                allowed,
                "forwarder: {hostname}"
            );
            assert_eq!(
                runtime.allows(transport_peer),
                allowed,
                "runtime: {hostname}"
            );
            assert_eq!(
                dns.record(&dns.qualify(hostname).expect("name")).is_some(),
                allowed,
                "DNS: {hostname}"
            );
        }
        assert!(dns.record(&dns.qualify("local").expect("name")).is_some());
        let remote_audit = inventory
            .peers
            .iter()
            .find(|peer| peer.peer_id == remote.peer_id)
            .expect("remote audit retained");
        assert_eq!(
            remote_audit.membership.as_ref().expect("membership").state,
            if departed.is_some_and(|peer| peer.peer_id == remote.peer_id) {
                NetworkPeerMembershipState::Revoked
            } else {
                NetworkPeerMembershipState::Active
            },
            "local departure must not erase surviving network members",
        );
    }
}
