use std::{
    collections::{BTreeSet, HashMap},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use serde::{Deserialize, Serialize};

use crate::{
    PathKind, PeerId,
    config::{Config, ConfigError, RouteConfig, vpn_ip_host_route},
    dns::canonical_dns_label,
    membership::{SignedMembershipRecord, effective_membership_at},
    path::PathOrigin,
    route::{builtin_ipv4, builtin_ipv6},
};

pub const NETWORK_PEER_LIST_SCHEMA_VERSION: u8 = 1;
pub const NETWORK_PEER_SNAPSHOT_SCHEMA_VERSION: u8 = 1;
pub const MAX_NETWORK_PEER_SNAPSHOT_PEERS: usize = 128;
pub const MAX_NETWORK_PEER_SNAPSHOT_ENCODED_BYTES: usize = 128 * 1024;
pub const MAX_NETWORK_PEER_SNAPSHOT_HOSTNAMES: usize = 4;
pub const MAX_NETWORK_PEER_SNAPSHOT_IPV4: usize = 8;
pub const MAX_NETWORK_PEER_SNAPSHOT_IPV6: usize = 8;

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
        Self::from_config_with_hostname_records_at(
            config,
            member_records,
            &HashMap::new(),
            now_unix_seconds,
        )
    }

    pub fn from_config_with_hostname_records_at(
        config: &Config,
        member_records: &[SignedMembershipRecord],
        hostname_records: &HashMap<PeerId, String>,
        now_unix_seconds: u64,
    ) -> Result<Self, ConfigError> {
        let peers =
            network_peer_inventory_at(config, member_records, hostname_records, now_unix_seconds)?
                .into_iter()
                .map(|entry| entry.peer)
                .collect();

        Ok(Self {
            schema_version: NETWORK_PEER_LIST_SCHEMA_VERSION,
            network: config.network.name.clone(),
            peers,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NetworkPeerSnapshot {
    pub schema_version: u8,
    pub observed_at_unix_seconds: u64,
    pub total_peers: u32,
    pub returned_peers: u32,
    pub truncated: bool,
    pub peers: Vec<NetworkPeerSnapshotPeer>,
}

impl NetworkPeerSnapshot {
    pub fn from_config_at<F>(
        config: &Config,
        member_records: &[SignedMembershipRecord],
        now_unix_seconds: u64,
        runtime_state: F,
    ) -> Result<Self, ConfigError>
    where
        F: FnMut(PeerId, &NetworkPeer) -> NetworkPeerRuntimeState,
    {
        Self::from_config_with_hostname_records_at(
            config,
            member_records,
            &HashMap::new(),
            now_unix_seconds,
            runtime_state,
        )
    }

    pub fn from_config_with_hostname_records_at<F>(
        config: &Config,
        member_records: &[SignedMembershipRecord],
        hostname_records: &HashMap<PeerId, String>,
        now_unix_seconds: u64,
        mut runtime_state: F,
    ) -> Result<Self, ConfigError>
    where
        F: FnMut(PeerId, &NetworkPeer) -> NetworkPeerRuntimeState,
    {
        let mut inventory =
            network_peer_inventory_at(config, member_records, hostname_records, now_unix_seconds)?;
        let total_peers = u32::try_from(inventory.len()).unwrap_or(u32::MAX);
        let retained = inventory.len().min(MAX_NETWORK_PEER_SNAPSHOT_PEERS);
        let mut selected = inventory.drain(..retained).collect::<Vec<_>>();
        if retained > 0
            && !selected.iter().any(|entry| entry.peer.local)
            && let Some(local_index) = inventory.iter().position(|entry| entry.peer.local)
        {
            selected[retained - 1] = inventory.remove(local_index);
        }
        let peers = selected
            .into_iter()
            .map(|entry| {
                let state = if entry.peer.local {
                    NetworkPeerRuntimeState::local()
                } else {
                    runtime_state(entry.overlay_peer, &entry.peer)
                };
                NetworkPeerSnapshotPeer::from_inventory(entry, state)
            })
            .collect::<Vec<_>>();
        let returned_peers = u32::try_from(peers.len()).unwrap_or(u32::MAX);

        let mut snapshot = Self {
            schema_version: NETWORK_PEER_SNAPSHOT_SCHEMA_VERSION,
            observed_at_unix_seconds: now_unix_seconds,
            total_peers,
            returned_peers,
            truncated: total_peers > returned_peers,
            peers,
        };
        snapshot.enforce_encoded_size();
        Ok(snapshot)
    }

    fn enforce_encoded_size(&mut self) {
        while serde_json::to_vec(self)
            .is_ok_and(|encoded| encoded.len() > MAX_NETWORK_PEER_SNAPSHOT_ENCODED_BYTES)
        {
            let Some(index) = self.peers.iter().rposition(|peer| !peer.local) else {
                break;
            };
            self.peers.remove(index);
            self.returned_peers = u32::try_from(self.peers.len()).unwrap_or(u32::MAX);
            self.truncated = self.total_peers > self.returned_peers;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NetworkPeerSnapshotPeer {
    pub peer_id: String,
    pub hostnames: Vec<String>,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    pub local: bool,
    pub membership_sources: Vec<NetworkPeerMembershipSource>,
    pub connection_state: NetworkPeerConnectionState,
    pub selected_path: Option<NetworkPeerPathKind>,
    pub path_origin: Option<NetworkPeerPathOrigin>,
}

impl NetworkPeerSnapshotPeer {
    fn from_inventory(
        entry: NetworkPeerInventoryEntry,
        runtime_state: NetworkPeerRuntimeState,
    ) -> Self {
        let mut peer = entry.peer;
        peer.hostnames.truncate(MAX_NETWORK_PEER_SNAPSHOT_HOSTNAMES);
        peer.ipv4.truncate(MAX_NETWORK_PEER_SNAPSHOT_IPV4);
        peer.ipv6.truncate(MAX_NETWORK_PEER_SNAPSHOT_IPV6);
        Self {
            peer_id: peer.peer_id,
            hostnames: peer.hostnames,
            ipv4: peer.ipv4,
            ipv6: peer.ipv6,
            local: peer.local,
            membership_sources: entry.membership_sources,
            connection_state: runtime_state.connection_state,
            selected_path: runtime_state.selected_path,
            path_origin: runtime_state.path_origin,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPeerMembershipSource {
    LocalConfiguration,
    PeerConfiguration,
    SignedMembership,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPeerConnectionState {
    Local,
    Connected,
    Connecting,
    Recovering,
    Disconnected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPeerPathKind {
    DirectUdpDatagram,
    DirectQuicDatagram,
    DirectQuicStream,
    DirectTcpStream,
    CircuitRelay,
}

impl From<PathKind> for NetworkPeerPathKind {
    fn from(value: PathKind) -> Self {
        match value {
            PathKind::DirectUdpDatagram => Self::DirectUdpDatagram,
            PathKind::DirectQuicDatagram => Self::DirectQuicDatagram,
            PathKind::DirectQuicStream => Self::DirectQuicStream,
            PathKind::DirectTcpStream => Self::DirectTcpStream,
            PathKind::CircuitRelay => Self::CircuitRelay,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPeerPathOrigin {
    Unknown,
    Configured,
    Mdns,
    Kademlia,
    Identify,
    RelayCircuit,
    Dcutr,
    PacketPlaneNegotiation,
}

impl From<PathOrigin> for NetworkPeerPathOrigin {
    fn from(value: PathOrigin) -> Self {
        match value {
            PathOrigin::Unknown => Self::Unknown,
            PathOrigin::Configured => Self::Configured,
            PathOrigin::Mdns => Self::Mdns,
            PathOrigin::Kademlia => Self::Kademlia,
            PathOrigin::Identify => Self::Identify,
            PathOrigin::RelayCircuit => Self::RelayCircuit,
            PathOrigin::Dcutr => Self::Dcutr,
            PathOrigin::PacketPlaneNegotiation => Self::PacketPlaneNegotiation,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkPeerRuntimeState {
    connection_state: NetworkPeerConnectionState,
    selected_path: Option<NetworkPeerPathKind>,
    path_origin: Option<NetworkPeerPathOrigin>,
}

impl NetworkPeerRuntimeState {
    const fn local() -> Self {
        Self::without_path(NetworkPeerConnectionState::Local)
    }

    #[must_use]
    pub const fn connecting() -> Self {
        Self::without_path(NetworkPeerConnectionState::Connecting)
    }

    #[must_use]
    pub const fn recovering() -> Self {
        Self::without_path(NetworkPeerConnectionState::Recovering)
    }

    #[must_use]
    pub const fn disconnected() -> Self {
        Self::without_path(NetworkPeerConnectionState::Disconnected)
    }

    #[must_use]
    pub fn connected(path: PathKind, origin: PathOrigin) -> Self {
        Self {
            connection_state: NetworkPeerConnectionState::Connected,
            selected_path: Some(path.into()),
            path_origin: Some(origin.into()),
        }
    }

    const fn without_path(connection_state: NetworkPeerConnectionState) -> Self {
        Self {
            connection_state,
            selected_path: None,
            path_origin: None,
        }
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
    membership_sources: BTreeSet<NetworkPeerMembershipSource>,
}

impl NetworkPeerBuilder {
    fn new(peer: PeerId, peer_id: String) -> Self {
        Self {
            peer_id,
            hostnames: BTreeSet::new(),
            ipv4: BTreeSet::from([builtin_ipv4(peer)]),
            ipv6: BTreeSet::from([builtin_ipv6(peer)]),
            local: false,
            membership_sources: BTreeSet::new(),
        }
    }

    fn finish(self, overlay_peer: PeerId) -> NetworkPeerInventoryEntry {
        NetworkPeerInventoryEntry {
            overlay_peer,
            peer: NetworkPeer {
                peer_id: self.peer_id,
                hostnames: self.hostnames.into_iter().collect(),
                ipv4: self.ipv4.into_iter().collect(),
                ipv6: self.ipv6.into_iter().collect(),
                local: self.local,
            },
            membership_sources: self.membership_sources.into_iter().collect(),
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
}

#[derive(Debug)]
struct NetworkPeerInventoryEntry {
    overlay_peer: PeerId,
    peer: NetworkPeer,
    membership_sources: Vec<NetworkPeerMembershipSource>,
}

fn network_peer_inventory_at(
    config: &Config,
    member_records: &[SignedMembershipRecord],
    hostname_records: &HashMap<PeerId, String>,
    now_unix_seconds: u64,
) -> Result<Vec<NetworkPeerInventoryEntry>, ConfigError> {
    let local_peer = config.local_peer_id()?;
    let mut peers = HashMap::<PeerId, NetworkPeerBuilder>::new();

    let local = peer_entry(&mut peers, local_peer, config.local_peer()?);
    local.local = true;
    local
        .membership_sources
        .insert(NetworkPeerMembershipSource::LocalConfiguration);
    if !hostname_records.contains_key(&local_peer) {
        insert_hostname(local, config.network.dns.hostname.as_deref());
    }
    insert_vpn_ip(local, config.network.vpn_ip.as_deref())?;
    insert_host_routes(local, &config.network.routes)?;

    for configured in &config.peers {
        let peer = configured.peer_id()?;
        let entry = peer_entry(&mut peers, peer, configured.id.clone());
        entry
            .membership_sources
            .insert(NetworkPeerMembershipSource::PeerConfiguration);
        if !hostname_records.contains_key(&peer) {
            insert_hostname(entry, configured.name.as_deref());
        }
        insert_vpn_ip(entry, configured.vpn_ip.as_deref())?;
        insert_host_routes(entry, &configured.routes)?;
    }

    for member in effective_membership_at(member_records, &config.network.name, now_unix_seconds)?
        .overlay_members()
    {
        let entry = peer_entry(&mut peers, member.peer, member.transport_peer.to_string());
        entry.peer_id = member.transport_peer.to_string();
        entry
            .membership_sources
            .insert(NetworkPeerMembershipSource::SignedMembership);
        if !hostname_records.contains_key(&member.peer) {
            for hostname in &member.hostnames {
                insert_hostname(entry, Some(hostname));
            }
        }
        insert_host_routes(entry, &member.route_grants)?;
    }

    for (peer, hostname) in hostname_records {
        if let Some(entry) = peers.get_mut(peer) {
            insert_hostname(entry, Some(hostname));
        }
    }

    let mut peers = peers
        .into_iter()
        .map(|(overlay_peer, builder)| builder.finish(overlay_peer))
        .collect::<Vec<_>>();
    peers.sort_by(|left, right| {
        let left_name = left.peer.hostnames.first().map(String::as_str);
        let right_name = right.peer.hostnames.first().map(String::as_str);
        left_name
            .is_none()
            .cmp(&right_name.is_none())
            .then_with(|| left_name.cmp(&right_name))
            .then_with(|| left.peer.peer_id.cmp(&right.peer.peer_id))
    });
    Ok(peers)
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

        let configured_overlay = configured.peer_id.parse::<PeerId>().expect("overlay peer");
        let snapshot = NetworkPeerSnapshot::from_config_at(
            &config,
            &config.network.member_records,
            1_001,
            |peer, _| {
                if peer == configured_overlay {
                    NetworkPeerRuntimeState::connected(PathKind::DirectQuicStream, PathOrigin::Mdns)
                } else {
                    NetworkPeerRuntimeState::recovering()
                }
            },
        )
        .expect("peer snapshot");

        assert_eq!(
            snapshot.schema_version,
            NETWORK_PEER_SNAPSHOT_SCHEMA_VERSION
        );
        assert_eq!(snapshot.observed_at_unix_seconds, 1_001);
        assert_eq!(snapshot.total_peers, 3);
        assert_eq!(snapshot.returned_peers, 3);
        assert!(!snapshot.truncated);
        let local_snapshot = snapshot
            .peers
            .iter()
            .find(|peer| peer.peer_id == local.peer_id)
            .expect("local snapshot");
        assert_eq!(
            local_snapshot.membership_sources,
            [NetworkPeerMembershipSource::LocalConfiguration]
        );
        assert_eq!(
            local_snapshot.connection_state,
            NetworkPeerConnectionState::Local
        );
        let configured_snapshot = snapshot
            .peers
            .iter()
            .find(|peer| peer.peer_id == configured.peer_id)
            .expect("configured snapshot");
        assert_eq!(
            configured_snapshot.membership_sources,
            [NetworkPeerMembershipSource::PeerConfiguration]
        );
        assert_eq!(
            configured_snapshot.connection_state,
            NetworkPeerConnectionState::Connected
        );
        assert_eq!(
            configured_snapshot.selected_path,
            Some(NetworkPeerPathKind::DirectQuicStream)
        );
        assert_eq!(
            configured_snapshot.path_origin,
            Some(NetworkPeerPathOrigin::Mdns)
        );
        let signed_snapshot = snapshot
            .peers
            .iter()
            .find(|peer| peer.peer_id == signed.peer_id)
            .expect("signed snapshot");
        assert_eq!(
            signed_snapshot.membership_sources,
            [NetworkPeerMembershipSource::SignedMembership]
        );
        assert_eq!(
            signed_snapshot.connection_state,
            NetworkPeerConnectionState::Recovering
        );
        assert_eq!(signed_snapshot.selected_path, None);
    }

    #[test]
    fn snapshot_is_bounded_and_always_retains_the_local_peer() {
        let local = NodeIdentity::generate_ed25519().expect("local identity");
        let peers = (0..=MAX_NETWORK_PEER_SNAPSHOT_PEERS)
            .map(|index| {
                let peer = NodeIdentity::generate_ed25519().expect("remote identity");
                serde_json::json!({
                    "id": peer.peer_id,
                    "name": format!("remote-{index:03}")
                })
            })
            .collect::<Vec<_>>();
        let config: Config = serde_json::from_value(serde_json::json!({
            "network": {
                "name": "bounded",
                "private_key": local.private_key
            },
            "peers": peers
        }))
        .expect("config");

        let snapshot = NetworkPeerSnapshot::from_config_at(&config, &[], 10, |_, _| {
            NetworkPeerRuntimeState::disconnected()
        })
        .expect("peer snapshot");

        assert_eq!(
            snapshot.total_peers,
            u32::try_from(MAX_NETWORK_PEER_SNAPSHOT_PEERS + 2).expect("bounded count")
        );
        assert_eq!(
            snapshot.returned_peers,
            u32::try_from(MAX_NETWORK_PEER_SNAPSHOT_PEERS).expect("bounded count")
        );
        assert!(snapshot.truncated);
        assert!(snapshot.peers.iter().any(|peer| peer.local));
        assert_eq!(snapshot.peers.iter().filter(|peer| peer.local).count(), 1);
        assert!(
            serde_json::to_vec(&snapshot).expect("snapshot JSON").len()
                <= MAX_NETWORK_PEER_SNAPSHOT_ENCODED_BYTES
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
