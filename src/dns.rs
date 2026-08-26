use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use serde::{Deserialize, Serialize};

use crate::{
    PeerId,
    config::{Config, ConfigError, RouteConfig},
    route::{builtin_ipv4, builtin_ipv6},
};

pub const DNS_PRIVATE_SUFFIX: &str = "p2p-vpn.internal";
pub const DEFAULT_DNS_TTL_SECONDS: u32 = 30;
pub const MAX_DNS_TTL_SECONDS: u32 = 300;
pub const MAX_DNS_RECORD_SETS: usize = 1_024;
pub const MAX_DNS_ADDRESSES_PER_PEER: usize = 256;
pub const MAX_DNS_ADDRESSES: usize = 4_096;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct DnsConfig {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    pub listen: SocketAddr,
    pub ttl_seconds: u32,
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hostname: None,
            listen: default_dns_listen(),
            ttl_seconds: DEFAULT_DNS_TTL_SECONDS,
        }
    }
}

impl DnsConfig {
    #[must_use]
    pub fn is_disabled(&self) -> bool {
        self == &Self::default()
    }

    pub fn validate(&self, network_name: &str) -> Result<(), DnsValidationError> {
        if !self.enabled {
            return Ok(());
        }

        let hostname = self
            .hostname
            .as_deref()
            .ok_or(DnsValidationError::MissingHostname)?;
        canonical_dns_label(hostname).map_err(DnsValidationError::InvalidHostname)?;
        canonical_dns_label(network_name).map_err(DnsValidationError::InvalidNetworkName)?;
        if !self.listen.ip().is_loopback() {
            return Err(DnsValidationError::NonLoopbackListener(self.listen));
        }
        if self.ttl_seconds == 0 || self.ttl_seconds > MAX_DNS_TTL_SECONDS {
            return Err(DnsValidationError::InvalidTtl {
                value: self.ttl_seconds,
                max: MAX_DNS_TTL_SECONDS,
            });
        }

        Ok(())
    }
}

fn default_dns_listen() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DnsValidationError {
    MissingHostname,
    InvalidHostname(DnsNameError),
    InvalidPeerHostname {
        peer: String,
        hostname: String,
        error: DnsNameError,
    },
    InvalidNetworkName(DnsNameError),
    NonLoopbackListener(SocketAddr),
    InvalidTtl {
        value: u32,
        max: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsNameError {
    Empty,
    TooLong,
    LeadingHyphen,
    TrailingHyphen,
    InvalidCharacter,
}

pub fn canonical_dns_label(label: &str) -> Result<String, DnsNameError> {
    if label.is_empty() {
        return Err(DnsNameError::Empty);
    }
    if label.len() > 63 {
        return Err(DnsNameError::TooLong);
    }
    if label.starts_with('-') {
        return Err(DnsNameError::LeadingHyphen);
    }
    if label.ends_with('-') {
        return Err(DnsNameError::TrailingHyphen);
    }
    if !label
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(DnsNameError::InvalidCharacter);
    }

    Ok(label.to_ascii_lowercase())
}

pub fn zone_name(network_name: &str) -> Result<String, DnsNameError> {
    Ok(format!(
        "{}.{}.",
        canonical_dns_label(network_name)?,
        DNS_PRIVATE_SUFFIX
    ))
}

pub fn fully_qualified_name(label: &str, zone: &str) -> Result<String, DnsNameError> {
    Ok(format!("{}.{}", canonical_dns_label(label)?, zone))
}

#[must_use]
pub fn peer_fallback_label(peer: PeerId) -> String {
    format!(
        "peer-{}",
        base32::encode(base32::Alphabet::Crockford, &peer.as_bytes()).to_ascii_lowercase()
    )
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DnsNameSource {
    LocalConfiguration,
    PeerConfiguration,
    SignedMembership,
    PeerIdFallback,
}

impl DnsNameSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalConfiguration => "local_configuration",
            Self::PeerConfiguration => "peer_configuration",
            Self::SignedMembership => "signed_membership",
            Self::PeerIdFallback => "peer_id_fallback",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DnsRecordSet {
    pub label: String,
    pub fqdn: String,
    pub peer: PeerId,
    pub transport_peer: String,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    pub sources: Vec<DnsNameSource>,
    pub fallback: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DnsNameConflict {
    pub label: String,
    pub fqdn: String,
    pub peers: Vec<PeerId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DnsZone {
    network_name: String,
    zone: String,
    ttl_seconds: u32,
    next_refresh_unix_seconds: Option<u64>,
    records: Vec<DnsRecordSet>,
    conflicts: Vec<DnsNameConflict>,
    reverse: BTreeMap<String, String>,
}

impl DnsZone {
    #[allow(clippy::too_many_lines)]
    pub fn from_config_at(
        config: &Config,
        member_records: &[crate::membership::SignedMembershipRecord],
        now_unix_seconds: u64,
    ) -> Result<Self, DnsZoneError> {
        config
            .network
            .dns
            .validate(&config.network.name)
            .map_err(DnsZoneError::Validation)?;
        if !config.network.dns.enabled {
            return Err(DnsZoneError::Disabled);
        }

        let zone = zone_name(&config.network.name).map_err(DnsZoneError::Name)?;
        let local_peer = config.local_peer_id().map_err(DnsZoneError::Config)?;
        let local_transport_peer = config.local_peer().map_err(DnsZoneError::Config)?;
        let mut peers = HashMap::<PeerId, PeerAddresses>::new();
        peers.insert(
            local_peer,
            PeerAddresses::new(local_transport_peer, local_peer),
        );
        if let Some(vpn_ip) = config.network.vpn_ip.as_deref() {
            peers
                .get_mut(&local_peer)
                .expect("local DNS peer exists")
                .insert_ip(parse_explicit_ip(vpn_ip)?);
        }
        add_host_routes(
            peers.get_mut(&local_peer).expect("local DNS peer exists"),
            &config.network.routes,
        )?;

        let mut names = HashMap::<String, HashMap<PeerId, BTreeSet<DnsNameSource>>>::new();
        insert_name(
            &mut names,
            config
                .network
                .dns
                .hostname
                .as_deref()
                .expect("enabled DNS has a validated hostname"),
            local_peer,
            DnsNameSource::LocalConfiguration,
        )?;

        for peer in &config.peers {
            let overlay_peer = peer.peer_id().map_err(DnsZoneError::Config)?;
            let addresses = peers
                .entry(overlay_peer)
                .or_insert_with(|| PeerAddresses::new(peer.id.clone(), overlay_peer));
            if let Some(vpn_ip) = peer.vpn_ip.as_deref() {
                addresses.insert_ip(parse_explicit_ip(vpn_ip)?);
            }
            add_host_routes(addresses, &peer.routes)?;
            if let Some(name) = peer.name.as_deref() {
                insert_name(
                    &mut names,
                    name,
                    overlay_peer,
                    DnsNameSource::PeerConfiguration,
                )?;
            }
        }

        let effective = crate::membership::effective_membership_at(
            member_records,
            &config.network.name,
            now_unix_seconds,
        )
        .map_err(DnsZoneError::Membership)?;
        for member in effective.overlay_members() {
            let addresses = peers.entry(member.peer).or_insert_with(|| {
                PeerAddresses::new(member.transport_peer.to_string(), member.peer)
            });
            add_host_routes(addresses, &member.route_grants)?;
            for hostname in &member.hostnames {
                insert_name(
                    &mut names,
                    hostname,
                    member.peer,
                    DnsNameSource::SignedMembership,
                )?;
            }
        }

        let mut address_count = 0_usize;
        for (peer, addresses) in &peers {
            let peer_address_count = addresses.ipv4.len().saturating_add(addresses.ipv6.len());
            if peer_address_count > MAX_DNS_ADDRESSES_PER_PEER {
                return Err(DnsZoneError::TooManyPeerAddresses {
                    peer: *peer,
                    actual: peer_address_count,
                    max: MAX_DNS_ADDRESSES_PER_PEER,
                });
            }
            address_count = address_count.saturating_add(peer_address_count);
        }
        if address_count > MAX_DNS_ADDRESSES {
            return Err(DnsZoneError::TooManyAddresses {
                actual: address_count,
                max: MAX_DNS_ADDRESSES,
            });
        }

        for peer in peers.keys().copied().collect::<Vec<_>>() {
            insert_name(
                &mut names,
                &peer_fallback_label(peer),
                peer,
                DnsNameSource::PeerIdFallback,
            )?;
        }

        let mut records = Vec::new();
        let mut conflicts = Vec::new();
        for (label, owners) in names {
            let fqdn = fully_qualified_name(&label, &zone).map_err(DnsZoneError::Name)?;
            if owners.len() > 1 {
                let mut conflicting_peers = owners.into_keys().collect::<Vec<_>>();
                conflicting_peers.sort_by_key(ToString::to_string);
                conflicts.push(DnsNameConflict {
                    label,
                    fqdn,
                    peers: conflicting_peers,
                });
                continue;
            }
            let (peer, sources) = owners.into_iter().next().expect("one DNS name owner");
            let addresses = peers.get(&peer).expect("DNS name owner has addresses");
            let sources = sources.into_iter().collect::<Vec<_>>();
            records.push(DnsRecordSet {
                label,
                fqdn,
                peer,
                transport_peer: addresses.transport_peer.clone(),
                ipv4: addresses.ipv4.iter().copied().collect(),
                ipv6: addresses.ipv6.iter().copied().collect(),
                fallback: sources == [DnsNameSource::PeerIdFallback],
                sources,
            });
        }
        if records.len() > MAX_DNS_RECORD_SETS {
            return Err(DnsZoneError::TooManyRecordSets {
                actual: records.len(),
                max: MAX_DNS_RECORD_SETS,
            });
        }
        records.sort_by(|left, right| left.fqdn.cmp(&right.fqdn));
        conflicts.sort_by(|left, right| left.fqdn.cmp(&right.fqdn));
        let reverse = build_reverse_records(&records);
        let next_refresh_unix_seconds = member_records
            .iter()
            .filter_map(|record| record.payload.expires_at_unix_seconds)
            .filter(|expires_at| *expires_at > now_unix_seconds)
            .min();

        Ok(Self {
            network_name: config.network.name.clone(),
            zone,
            ttl_seconds: config.network.dns.ttl_seconds,
            next_refresh_unix_seconds,
            records,
            conflicts,
            reverse,
        })
    }

    #[must_use]
    pub fn network_name(&self) -> &str {
        &self.network_name
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.zone
    }

    #[must_use]
    pub const fn ttl_seconds(&self) -> u32 {
        self.ttl_seconds
    }

    #[must_use]
    pub const fn next_refresh_unix_seconds(&self) -> Option<u64> {
        self.next_refresh_unix_seconds
    }

    pub fn records(&self) -> impl Iterator<Item = &DnsRecordSet> {
        self.records.iter()
    }

    pub fn conflicts(&self) -> impl Iterator<Item = &DnsNameConflict> {
        self.conflicts.iter()
    }

    #[must_use]
    pub fn conflict(&self, fqdn: &str) -> Option<&DnsNameConflict> {
        let canonical = canonical_fqdn(fqdn);
        self.conflicts
            .binary_search_by(|conflict| conflict.fqdn.cmp(&canonical))
            .ok()
            .map(|index| &self.conflicts[index])
    }

    #[must_use]
    pub fn record(&self, fqdn: &str) -> Option<&DnsRecordSet> {
        let canonical = canonical_fqdn(fqdn);
        self.records
            .binary_search_by(|record| record.fqdn.cmp(&canonical))
            .ok()
            .map(|index| &self.records[index])
    }

    pub fn qualify(&self, label: &str) -> Result<String, DnsNameError> {
        fully_qualified_name(label, &self.zone)
    }

    #[must_use]
    pub fn reverse_target(&self, address: IpAddr) -> Option<&str> {
        self.reverse.get(&reverse_name(address)).map(String::as_str)
    }

    #[must_use]
    pub fn reverse_target_name(&self, owner: &str) -> Option<&str> {
        self.reverse.get(&canonical_fqdn(owner)).map(String::as_str)
    }

    pub fn reverse_records(&self) -> impl Iterator<Item = (&str, &str)> {
        self.reverse
            .iter()
            .map(|(owner, target)| (owner.as_str(), target.as_str()))
    }
}

#[derive(Debug)]
pub enum DnsZoneError {
    Disabled,
    Validation(DnsValidationError),
    Name(DnsNameError),
    Config(ConfigError),
    Membership(crate::membership::MembershipRecordError),
    InvalidExplicitIp(String),
    TooManyRecordSets {
        actual: usize,
        max: usize,
    },
    TooManyPeerAddresses {
        peer: PeerId,
        actual: usize,
        max: usize,
    },
    TooManyAddresses {
        actual: usize,
        max: usize,
    },
}

#[derive(Clone, Debug)]
struct PeerAddresses {
    transport_peer: String,
    ipv4: BTreeSet<Ipv4Addr>,
    ipv6: BTreeSet<Ipv6Addr>,
}

impl PeerAddresses {
    fn new(transport_peer: String, peer: PeerId) -> Self {
        Self {
            transport_peer,
            ipv4: BTreeSet::from([builtin_ipv4(peer)]),
            ipv6: BTreeSet::from([builtin_ipv6(peer)]),
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

fn insert_name(
    names: &mut HashMap<String, HashMap<PeerId, BTreeSet<DnsNameSource>>>,
    label: &str,
    peer: PeerId,
    source: DnsNameSource,
) -> Result<(), DnsZoneError> {
    let label = canonical_dns_label(label).map_err(DnsZoneError::Name)?;
    names
        .entry(label)
        .or_default()
        .entry(peer)
        .or_default()
        .insert(source);
    Ok(())
}

fn parse_explicit_ip(input: &str) -> Result<IpAddr, DnsZoneError> {
    let input = input.split_once('/').map_or(input, |(address, _)| address);
    input
        .parse()
        .map_err(|_| DnsZoneError::InvalidExplicitIp(input.to_owned()))
}

fn add_host_routes(
    addresses: &mut PeerAddresses,
    routes: &[RouteConfig],
) -> Result<(), DnsZoneError> {
    for route in routes {
        let prefix = route.prefix().map_err(DnsZoneError::Config)?;
        if matches!(prefix.address(), IpAddr::V4(_)) && prefix.prefix_len() == 32
            || matches!(prefix.address(), IpAddr::V6(_)) && prefix.prefix_len() == 128
        {
            addresses.insert_ip(prefix.address());
        }
    }
    Ok(())
}

fn build_reverse_records(records: &[DnsRecordSet]) -> BTreeMap<String, String> {
    let mut preferred = HashMap::<PeerId, &DnsRecordSet>::new();
    for record in records {
        preferred
            .entry(record.peer)
            .and_modify(|current| {
                if current.fallback && !record.fallback {
                    *current = record;
                }
            })
            .or_insert(record);
    }

    let mut reverse = BTreeMap::new();
    for record in preferred.into_values() {
        for address in &record.ipv4 {
            reverse.insert(reverse_name(IpAddr::V4(*address)), record.fqdn.clone());
        }
        for address in &record.ipv6 {
            reverse.insert(reverse_name(IpAddr::V6(*address)), record.fqdn.clone());
        }
    }
    reverse
}

#[must_use]
pub fn reverse_name(address: IpAddr) -> String {
    match address {
        IpAddr::V4(address) => {
            let [a, b, c, d] = address.octets();
            format!("{d}.{c}.{b}.{a}.in-addr.arpa.")
        }
        IpAddr::V6(address) => {
            let mut name = String::with_capacity(72);
            for byte in address.octets().iter().rev() {
                use std::fmt::Write as _;
                write!(name, "{:x}.{:x}.", byte & 0x0f, byte >> 4)
                    .expect("writing to a string cannot fail");
            }
            name.push_str("ip6.arpa.");
            name
        }
    }
}

fn canonical_fqdn(name: &str) -> String {
    let mut canonical = name.to_ascii_lowercase();
    if !canonical.ends_with('.') {
        canonical.push('.');
    }
    canonical
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::*;
    use crate::{
        config::{
            DiscoveryConfig, InterfaceConfig, NetworkConfig, PacketPlaneConfig, QueueConfig,
            RelayConfig, ResourceConfig,
        },
        identity::NodeIdentity,
        membership::{
            MembershipRecordIssueOptions, MembershipRecordSubject, MembershipRole,
            issue_named_membership_record_for_subject_at,
        },
    };

    fn config_with_dns(identity: &NodeIdentity, hostname: &str) -> Config {
        Config {
            network: NetworkConfig {
                dns: DnsConfig {
                    enabled: true,
                    hostname: Some(hostname.to_owned()),
                    ..DnsConfig::default()
                },
                name: "runners".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig {
                    mdns: false,
                    kademlia: false,
                    kademlia_provider_advertisement: false,
                    kademlia_protocol: crate::config::PRIVATE_KADEMLIA_PROTOCOL.to_owned(),
                    dcutr: false,
                    autonat: false,
                },
                relay: RelayConfig::default(),
                packet_plane: PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1_280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        }
    }

    #[test]
    fn canonical_names_use_private_network_zone() {
        assert_eq!(
            zone_name("Monarchic-Runners"),
            Ok("monarchic-runners.p2p-vpn.internal.".to_owned())
        );
        assert_eq!(
            fully_qualified_name("MIDI-DESKTOP-1", "monarchic-runners.p2p-vpn.internal."),
            Ok("midi-desktop-1.monarchic-runners.p2p-vpn.internal.".to_owned())
        );
    }

    #[test]
    fn invalid_labels_are_rejected() {
        assert_eq!(canonical_dns_label(""), Err(DnsNameError::Empty));
        assert_eq!(
            canonical_dns_label("not valid"),
            Err(DnsNameError::InvalidCharacter)
        );
        assert_eq!(
            canonical_dns_label("-leading"),
            Err(DnsNameError::LeadingHyphen)
        );
        assert_eq!(
            canonical_dns_label("trailing-"),
            Err(DnsNameError::TrailingHyphen)
        );
    }

    #[test]
    fn fallback_labels_encode_the_complete_overlay_peer_id() {
        let first = peer_fallback_label(PeerId::from_bytes([0; 32]));
        let second = peer_fallback_label(PeerId::from_bytes([0; 32]));

        assert!(first.starts_with("peer-"));
        assert!(first.len() <= 63);
        assert_eq!(first, second);
        assert_ne!(first, peer_fallback_label(PeerId::from_bytes([1; 32])));
    }

    #[test]
    fn zone_uses_transitive_signed_names_and_overlay_addresses() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let mut config = config_with_dns(&root, "root");
        let record = issue_named_membership_record_for_subject_at(
            &root,
            MembershipRecordIssueOptions {
                network_name: "runners".to_owned(),
                member: MembershipRecordSubject::from_identity(&member).expect("subject"),
                membership_epoch: 1,
                sequence: 1,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: vec![RouteConfig {
                    prefix: "10.42.0.2/32".to_owned(),
                    metric: 0,
                }],
                expires_at_unix_seconds: None,
            },
            Some("worker-1"),
            1_000,
        )
        .expect("membership record");
        config.network.member_records.push(record);

        let zone = DnsZone::from_config_at(&config, &config.network.member_records, 1_001)
            .expect("DNS zone");
        let record = zone
            .record("worker-1.runners.p2p-vpn.internal")
            .expect("signed DNS record");

        assert_eq!(
            record.peer,
            PeerId::from_str(&member.peer_id).expect("peer")
        );
        assert!(record.ipv4.contains(&"10.42.0.2".parse().expect("IPv4")));
        assert_eq!(record.sources, vec![DnsNameSource::SignedMembership]);
        assert!(
            zone.record(&format!(
                "{}.{}",
                peer_fallback_label(record.peer),
                zone.name()
            ))
            .is_some()
        );
    }

    #[test]
    fn ambiguous_friendly_names_do_not_resolve_but_fallbacks_do() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let first = NodeIdentity::generate_ed25519().expect("first");
        let second = NodeIdentity::generate_ed25519().expect("second");
        let mut config = config_with_dns(&root, "root");
        for (sequence, member) in [(1, &first), (2, &second)] {
            config.network.member_records.push(
                issue_named_membership_record_for_subject_at(
                    &root,
                    MembershipRecordIssueOptions {
                        network_name: "runners".to_owned(),
                        member: MembershipRecordSubject::from_identity(member).expect("subject"),
                        membership_epoch: 1,
                        sequence,
                        revoked: false,
                        roles: vec![MembershipRole::OverlayMember],
                        route_grants: Vec::new(),
                        expires_at_unix_seconds: None,
                    },
                    Some("worker"),
                    1_000,
                )
                .expect("membership record"),
            );
        }

        let zone = DnsZone::from_config_at(&config, &config.network.member_records, 1_001)
            .expect("DNS zone");

        assert!(zone.record("worker.runners.p2p-vpn.internal").is_none());
        assert_eq!(zone.conflicts().count(), 1);
        for identity in [&first, &second] {
            let peer = PeerId::from_str(&identity.peer_id).expect("peer");
            assert!(
                zone.record(&format!("{}.{}", peer_fallback_label(peer), zone.name()))
                    .is_some()
            );
        }
    }

    #[test]
    fn expired_and_revoked_names_are_absent() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let mut config = config_with_dns(&root, "root");
        let subject = MembershipRecordSubject::from_identity(&member).expect("subject");
        config.network.member_records = vec![
            issue_named_membership_record_for_subject_at(
                &root,
                MembershipRecordIssueOptions {
                    network_name: "runners".to_owned(),
                    member: subject.clone(),
                    membership_epoch: 1,
                    sequence: 1,
                    revoked: false,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: Some(1_010),
                },
                Some("ephemeral"),
                1_000,
            )
            .expect("expiring record"),
        ];
        let before = DnsZone::from_config_at(&config, &config.network.member_records, 1_009)
            .expect("zone before expiry");
        assert_eq!(before.next_refresh_unix_seconds(), Some(1_010));
        assert!(
            before
                .record("ephemeral.runners.p2p-vpn.internal")
                .is_some()
        );
        let after = DnsZone::from_config_at(&config, &config.network.member_records, 1_011)
            .expect("zone after expiry");
        assert_eq!(after.next_refresh_unix_seconds(), None);
        assert!(after.record("ephemeral.runners.p2p-vpn.internal").is_none());

        config.network.member_records.push(
            issue_named_membership_record_for_subject_at(
                &root,
                MembershipRecordIssueOptions {
                    network_name: "runners".to_owned(),
                    member: subject,
                    membership_epoch: 1,
                    sequence: 2,
                    revoked: true,
                    roles: Vec::new(),
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                None,
                1_005,
            )
            .expect("revocation"),
        );
        let revoked = DnsZone::from_config_at(&config, &config.network.member_records, 1_006)
            .expect("zone after revocation");
        assert!(
            revoked
                .record("ephemeral.runners.p2p-vpn.internal")
                .is_none()
        );
    }

    #[test]
    fn reverse_records_prefer_friendly_names() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let config = config_with_dns(&root, "root");
        let zone = DnsZone::from_config_at(&config, &[], 1_000).expect("DNS zone");
        let address = builtin_ipv4(config.local_peer_id().expect("peer"));

        assert_eq!(
            zone.reverse_target(IpAddr::V4(address)),
            Some("root.runners.p2p-vpn.internal.")
        );
        assert_eq!(
            reverse_name(IpAddr::V4(address)),
            format!(
                "{}.{}.{}.100.in-addr.arpa.",
                address.octets()[3],
                address.octets()[2],
                address.octets()[1]
            )
        );
    }

    #[test]
    fn zone_rejects_a_peer_address_set_that_cannot_fit_a_bounded_response() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let mut config = config_with_dns(&identity, "worker-1");
        config.network.routes = (0_u16..u16::try_from(MAX_DNS_ADDRESSES_PER_PEER).unwrap())
            .map(|suffix| RouteConfig {
                prefix: format!("10.1.{}.{}/32", suffix / 256, suffix % 256),
                metric: 100,
            })
            .collect();

        assert!(matches!(
            DnsZone::from_config_at(&config, &[], 1_000),
            Err(DnsZoneError::TooManyPeerAddresses { .. })
        ));
    }
}
