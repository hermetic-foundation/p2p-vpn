use std::{
    collections::{BTreeSet, HashMap},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use serde::{Deserialize, Serialize};

use crate::{
    PeerId,
    config::{Config, ConfigError, RouteConfig, vpn_ip_host_route},
    dns::canonical_dns_label,
    membership::{SignedMembershipRecord, effective_membership_at},
    route::{builtin_ipv4, builtin_ipv6},
};

pub const NETWORK_PEER_LIST_SCHEMA_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkPeerList {
    pub schema_version: u8,
    pub network: String,
    pub peers: Vec<NetworkPeer>,
}

impl NetworkPeerList {
    pub fn from_config_at(
        config: &Config,
        member_records: &[SignedMembershipRecord],
        now_unix_seconds: u64,
    ) -> Result<Self, ConfigError> {
        let local_peer = config.local_peer_id()?;
        let mut peers = HashMap::<PeerId, NetworkPeerBuilder>::new();

        let local = peer_entry(&mut peers, local_peer, config.local_peer()?);
        local.local = true;
        insert_hostname(local, config.network.dns.hostname.as_deref());
        insert_vpn_ip(local, config.network.vpn_ip.as_deref())?;
        insert_host_routes(local, &config.network.routes)?;

        for configured in &config.peers {
            let peer = configured.peer_id()?;
            let entry = peer_entry(&mut peers, peer, configured.id.clone());
            insert_hostname(entry, configured.name.as_deref());
            insert_vpn_ip(entry, configured.vpn_ip.as_deref())?;
            insert_host_routes(entry, &configured.routes)?;
        }

        for member in
            effective_membership_at(member_records, &config.network.name, now_unix_seconds)?
                .overlay_members()
        {
            let entry = peer_entry(&mut peers, member.peer, member.transport_peer.to_string());
            entry.peer_id = member.transport_peer.to_string();
            for hostname in &member.hostnames {
                insert_hostname(entry, Some(hostname));
            }
            insert_host_routes(entry, &member.route_grants)?;
        }

        let mut peers = peers
            .into_values()
            .map(NetworkPeerBuilder::finish)
            .collect::<Vec<_>>();
        peers.sort_by(|left, right| {
            let left_name = left.hostnames.first().map(String::as_str);
            let right_name = right.hostnames.first().map(String::as_str);
            left_name
                .is_none()
                .cmp(&right_name.is_none())
                .then_with(|| left_name.cmp(&right_name))
                .then_with(|| left.peer_id.cmp(&right.peer_id))
        });

        Ok(Self {
            schema_version: NETWORK_PEER_LIST_SCHEMA_VERSION,
            network: config.network.name.clone(),
            peers,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkPeer {
    pub peer_id: String,
    pub hostnames: Vec<String>,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    pub local: bool,
}

#[derive(Debug)]
struct NetworkPeerBuilder {
    peer_id: String,
    hostnames: BTreeSet<String>,
    ipv4: BTreeSet<Ipv4Addr>,
    ipv6: BTreeSet<Ipv6Addr>,
    local: bool,
}

impl NetworkPeerBuilder {
    fn new(peer: PeerId, peer_id: String) -> Self {
        Self {
            peer_id,
            hostnames: BTreeSet::new(),
            ipv4: BTreeSet::from([builtin_ipv4(peer)]),
            ipv6: BTreeSet::from([builtin_ipv6(peer)]),
            local: false,
        }
    }

    fn insert_ip(&mut self, address: IpAddr) {
        match address {
            IpAddr::V4(address) => {
                self.ipv4.insert(address);
            }
            IpAddr::V6(address) => {
                self.ipv6.insert(address);
            }
        }
    }

    fn finish(self) -> NetworkPeer {
        NetworkPeer {
            peer_id: self.peer_id,
            hostnames: self.hostnames.into_iter().collect(),
            ipv4: self.ipv4.into_iter().collect(),
            ipv6: self.ipv6.into_iter().collect(),
            local: self.local,
        }
    }
}

fn peer_entry(
    peers: &mut HashMap<PeerId, NetworkPeerBuilder>,
    peer: PeerId,
    peer_id: String,
) -> &mut NetworkPeerBuilder {
    peers
        .entry(peer)
        .or_insert_with(|| NetworkPeerBuilder::new(peer, peer_id))
}

fn insert_hostname(entry: &mut NetworkPeerBuilder, hostname: Option<&str>) {
    if let Some(hostname) = hostname.and_then(|hostname| canonical_dns_label(hostname).ok()) {
        entry.hostnames.insert(hostname);
    }
}

fn insert_vpn_ip(entry: &mut NetworkPeerBuilder, vpn_ip: Option<&str>) -> Result<(), ConfigError> {
    if let Some(vpn_ip) = vpn_ip {
        entry.insert_ip(vpn_ip_host_route(vpn_ip)?.address());
    }
    Ok(())
}

fn insert_host_routes(
    entry: &mut NetworkPeerBuilder,
    routes: &[RouteConfig],
) -> Result<(), ConfigError> {
    for route in routes {
        let prefix = route.prefix()?;
        if matches!(prefix.address(), IpAddr::V4(_)) && prefix.prefix_len() == 32
            || matches!(prefix.address(), IpAddr::V6(_)) && prefix.prefix_len() == 128
        {
            entry.insert_ip(prefix.address());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::NodeIdentity,
        membership::{
            MembershipRecordIssueOptions, MembershipRecordSubject, MembershipRole,
            issue_named_membership_record_for_subject_at,
        },
    };

    #[test]
    fn inventory_includes_local_static_and_signed_members() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let configured = NodeIdentity::generate_ed25519().expect("configured identity");
        let signed = NodeIdentity::generate_ed25519().expect("signed identity");
        let mut config: Config = serde_json::from_value(serde_json::json!({
            "network": {
                "name": "runners",
                "private_key": local.private_key.clone(),
                "dns": {
                    "enabled": true,
                    "hostname": "local-runner"
                }
            },
            "peers": [{
                "id": configured.peer_id.clone(),
                "name": "configured-runner",
                "vpn_ip": "10.42.0.2"
            }]
        }))
        .expect("config");
        let signed_record = issue_named_membership_record_for_subject_at(
            &local,
            MembershipRecordIssueOptions {
                network_name: "runners".to_owned(),
                member: MembershipRecordSubject::from_identity(&signed).expect("subject"),
                membership_epoch: 1,
                sequence: 1,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: vec![RouteConfig {
                    prefix: "10.42.0.3/32".to_owned(),
                    metric: 0,
                }],
                expires_at_unix_seconds: None,
            },
            Some("signed-runner"),
            1_000,
        )
        .expect("signed membership record");
        config.network.member_records.push(signed_record);

        let inventory =
            NetworkPeerList::from_config_at(&config, &config.network.member_records, 1_001)
                .expect("peer inventory");

        assert_eq!(inventory.schema_version, NETWORK_PEER_LIST_SCHEMA_VERSION);
        assert_eq!(inventory.network, "runners");
        assert_eq!(inventory.peers.len(), 3);
        assert_eq!(
            inventory
                .peers
                .iter()
                .map(|peer| peer.hostnames.clone())
                .collect::<Vec<_>>(),
            vec![
                vec!["configured-runner".to_owned()],
                vec!["local-runner".to_owned()],
                vec!["signed-runner".to_owned()],
            ]
        );

        let local_peer = inventory
            .peers
            .iter()
            .find(|peer| peer.peer_id == local.peer_id)
            .expect("local peer");
        assert!(local_peer.local);
        assert!(
            local_peer
                .ipv4
                .contains(&builtin_ipv4(local.peer_id.parse().expect("peer")))
        );

        let configured_peer = inventory
            .peers
            .iter()
            .find(|peer| peer.peer_id == configured.peer_id)
            .expect("configured peer");
        assert!(
            configured_peer
                .ipv4
                .contains(&"10.42.0.2".parse().expect("IPv4"))
        );

        let signed_peer = inventory
            .peers
            .iter()
            .find(|peer| peer.peer_id == signed.peer_id)
            .expect("signed peer");
        assert!(
            signed_peer
                .ipv4
                .contains(&"10.42.0.3".parse().expect("IPv4"))
        );
    }

    #[test]
    fn inventory_works_when_overlay_dns_is_disabled() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let config: Config = serde_json::from_value(serde_json::json!({
            "network": {
                "name": "lab",
                "private_key": local.private_key.clone()
            },
            "peers": []
        }))
        .expect("config");

        let inventory = NetworkPeerList::from_config_at(&config, &[], 1).expect("peer inventory");

        assert_eq!(inventory.peers.len(), 1);
        assert_eq!(inventory.peers[0].peer_id, local.peer_id);
        assert!(inventory.peers[0].hostnames.is_empty());
        assert!(inventory.peers[0].local);
    }

    #[test]
    fn inventory_omits_non_overlay_and_expired_membership_records() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let routing_peer = NodeIdentity::generate_ed25519().expect("routing identity");
        let expired_peer = NodeIdentity::generate_ed25519().expect("expired identity");
        let mut config: Config = serde_json::from_value(serde_json::json!({
            "network": {
                "name": "lab",
                "private_key": local.private_key.clone()
            },
            "peers": []
        }))
        .expect("config");
        config.network.member_records = vec![
            issue_named_membership_record_for_subject_at(
                &local,
                MembershipRecordIssueOptions {
                    network_name: "lab".to_owned(),
                    member: MembershipRecordSubject::from_identity(&routing_peer)
                        .expect("routing subject"),
                    membership_epoch: 1,
                    sequence: 1,
                    revoked: false,
                    roles: vec![MembershipRole::RouteAuthority],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                Some("routing-only"),
                1_000,
            )
            .expect("routing record"),
            issue_named_membership_record_for_subject_at(
                &local,
                MembershipRecordIssueOptions {
                    network_name: "lab".to_owned(),
                    member: MembershipRecordSubject::from_identity(&expired_peer)
                        .expect("expired subject"),
                    membership_epoch: 1,
                    sequence: 1,
                    revoked: false,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: Some(1_001),
                },
                Some("expired"),
                1_000,
            )
            .expect("expired record"),
        ];

        let inventory =
            NetworkPeerList::from_config_at(&config, &config.network.member_records, 1_001)
                .expect("peer inventory");

        assert_eq!(inventory.peers.len(), 1);
        assert_eq!(inventory.peers[0].peer_id, local.peer_id);
    }
}
