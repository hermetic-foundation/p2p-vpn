use std::{cmp::Ordering, collections::HashMap, io, time::Duration};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{StreamProtocol, request_response};
use rustls::pki_types::CertificateDer;
use serde::{Deserialize, Serialize};
use sha2_010::{Digest as _, Sha256};

use crate::{
    PathKind, PeerId,
    config::validate_packet_plane_endpoint_candidate,
    hostname::SignedHostnameRecord,
    membership::{MAX_MEMBERSHIP_RECORDS, SignedMembershipRecord},
    runtime::packet::PACKET_PROTOCOL,
    wire::{HEADER_LEN, WIRE_VERSION},
};

pub const CONTROL_PROTOCOL: &str = "/p2p-vpn/control/1";
const MAX_CONTROL_MESSAGE_LEN: usize = 16_384;
pub const MAX_CONTROL_MEMBERSHIP_RECORDS: usize = 8;
pub const MAX_CONTROL_HOSTNAME_RECORDS: usize = 16;
pub const MAX_CONTROL_DIRECT_ADDRESS_CANDIDATES: usize = 32;
pub const MAX_OWNED_QUIC_PACKET_PLANE_CERTIFICATE_DER_LEN: usize = 2048;
pub const MEMBERSHIP_RECORD_PAGE_VERSION: u8 = 1;

const MEMBERSHIP_RECORD_SNAPSHOT_DOMAIN: &[u8] = b"p2p-vpn membership snapshot v1\n";
const MEMBERSHIP_RECORD_SNAPSHOT_PREFIX: &str = "sha256:";
const SHA256_LEN: usize = 32;

#[derive(Clone, Debug, Default)]
pub struct ControlCodec;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlRoute {
    pub prefix: String,
    pub metric: u16,
}

impl ControlRoute {
    #[must_use]
    pub fn new(prefix: impl Into<String>, metric: u16) -> Self {
        Self {
            prefix: prefix.into(),
            metric,
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ControlCapabilities {
    pub network_name: String,
    #[serde(default)]
    pub membership_tag: Option<String>,
    #[serde(default)]
    pub advertised_routes: Vec<ControlRoute>,
    pub wire_version: u8,
    pub packet_protocol: String,
    pub packet_header_len: usize,
    pub effective_mtu: u16,
    pub preferred_path: String,
    pub supports_quic_datagrams: bool,
    #[serde(default)]
    pub supports_native_quic_datagrams: bool,
    #[serde(default)]
    pub supports_owned_udp_packet_plane: bool,
    #[serde(default)]
    pub supports_owned_quic_packet_plane: bool,
    #[serde(default)]
    pub owned_quic_packet_plane_certificate_der: Option<Vec<u8>>,
    #[serde(default)]
    pub owned_quic_packet_endpoint_candidates: Vec<String>,
    #[serde(default)]
    pub packet_endpoint_candidates: Vec<String>,
    #[serde(default)]
    pub direct_address_candidates: Vec<String>,
    #[serde(default)]
    pub member_records: Vec<SignedMembershipRecord>,
    #[serde(default)]
    pub hostname_records: Vec<SignedHostnameRecord>,
    #[serde(default)]
    pub supports_membership_record_pages: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub membership_records_snapshot: Option<String>,
    #[serde(default)]
    pub membership_record_count: u16,
}

impl ControlCapabilities {
    #[must_use]
    pub fn local(network_name: &str, membership_tag: Option<String>, effective_mtu: u16) -> Self {
        Self {
            network_name: network_name.to_owned(),
            membership_tag,
            advertised_routes: Vec::new(),
            wire_version: WIRE_VERSION,
            packet_protocol: PACKET_PROTOCOL.to_owned(),
            packet_header_len: HEADER_LEN,
            effective_mtu,
            preferred_path: PathKind::DirectQuicStream.wire_name().to_owned(),
            supports_quic_datagrams: false,
            supports_native_quic_datagrams: false,
            supports_owned_udp_packet_plane: false,
            supports_owned_quic_packet_plane: false,
            owned_quic_packet_plane_certificate_der: None,
            owned_quic_packet_endpoint_candidates: Vec::new(),
            packet_endpoint_candidates: Vec::new(),
            direct_address_candidates: Vec::new(),
            member_records: Vec::new(),
            hostname_records: Vec::new(),
            supports_membership_record_pages: false,
            membership_records_snapshot: None,
            membership_record_count: 0,
        }
    }

    #[must_use]
    pub fn with_advertised_routes(mut self, routes: Vec<ControlRoute>) -> Self {
        self.advertised_routes = routes;
        self
    }

    #[must_use]
    pub fn with_packet_endpoint_candidates(mut self, endpoints: Vec<String>) -> Self {
        self.packet_endpoint_candidates = endpoints;
        self
    }

    #[must_use]
    pub fn with_direct_address_candidates(mut self, addresses: Vec<String>) -> Self {
        self.direct_address_candidates = addresses;
        self
    }

    #[must_use]
    pub fn with_member_records(mut self, records: Vec<SignedMembershipRecord>) -> Self {
        self.member_records = records;
        self
    }

    #[must_use]
    pub fn with_hostname_records(mut self, records: Vec<SignedHostnameRecord>) -> Self {
        self.hostname_records = records;
        self
    }

    #[must_use]
    pub fn with_membership_record_inventory(mut self, records: &[SignedMembershipRecord]) -> Self {
        self.supports_membership_record_pages = true;
        self.membership_records_snapshot = Some(membership_records_snapshot(records));
        self.membership_record_count = u16::try_from(records.len()).unwrap_or(u16::MAX);
        self
    }

    #[must_use]
    pub fn with_owned_udp_packet_plane(mut self, supported: bool) -> Self {
        self.supports_owned_udp_packet_plane = supported;
        self.supports_quic_datagrams = self.supports_native_quic_datagrams
            || self.supports_owned_quic_packet_plane
            || supported;
        self.refresh_preferred_path();
        self
    }

    #[must_use]
    pub fn with_owned_quic_packet_plane(mut self, supported: bool) -> Self {
        self.supports_owned_quic_packet_plane = supported;
        if !supported {
            self.owned_quic_packet_plane_certificate_der = None;
        }
        self.supports_quic_datagrams = self.supports_native_quic_datagrams
            || self.supports_owned_udp_packet_plane
            || supported;
        self.refresh_preferred_path();
        self
    }

    #[must_use]
    pub fn with_owned_quic_packet_plane_certificate(mut self, certificate_der: Vec<u8>) -> Self {
        self.supports_owned_quic_packet_plane = true;
        self.owned_quic_packet_plane_certificate_der = Some(certificate_der);
        self.supports_quic_datagrams = true;
        self.refresh_preferred_path();
        self
    }

    #[must_use]
    pub fn with_owned_quic_packet_endpoint_candidates(mut self, endpoints: Vec<String>) -> Self {
        self.owned_quic_packet_endpoint_candidates = endpoints;
        self
    }

    #[must_use]
    pub fn with_native_quic_datagrams(mut self, supported: bool) -> Self {
        self.supports_native_quic_datagrams = supported;
        self.supports_quic_datagrams = supported
            || self.supports_owned_udp_packet_plane
            || self.supports_owned_quic_packet_plane;
        self.refresh_preferred_path();
        self
    }

    #[must_use]
    pub const fn supports_datagram_packet_path(&self) -> bool {
        self.supports_quic_datagrams
            || self.supports_native_quic_datagrams
            || self.supports_owned_udp_packet_plane
            || self.supports_owned_quic_packet_plane
    }

    fn refresh_preferred_path(&mut self) {
        let preferred_path =
            if self.supports_native_quic_datagrams || self.supports_owned_quic_packet_plane {
                PathKind::DirectQuicDatagram.wire_name()
            } else if self.supports_owned_udp_packet_plane {
                PathKind::DirectUdpDatagram.wire_name()
            } else {
                PathKind::DirectQuicStream.wire_name()
            };
        preferred_path.clone_into(&mut self.preferred_path);
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[allow(clippy::large_enum_variant)]
pub enum ControlRequest {
    Capabilities(ControlCapabilities),
    PacketPlaneHello(Vec<u8>),
    MembershipRecords(MembershipRecordsRequest),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[allow(clippy::large_enum_variant)]
pub enum ControlResponse {
    CapabilitiesAccepted(ControlCapabilities),
    CapabilitiesRejected(ControlRejectionReason),
    PacketPlaneAccepted(Vec<u8>),
    PacketPlaneRejected(ControlRejectionReason),
    MembershipRecordsPage(MembershipRecordsPage),
    MembershipRecordsRejected(MembershipRecordsRejectionReason),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MembershipRecordsRequest {
    pub version: u8,
    pub network_name: String,
    #[serde(default)]
    pub membership_tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    pub cursor: u16,
}

impl MembershipRecordsRequest {
    #[must_use]
    pub fn first(network_name: &str, membership_tag: Option<String>) -> Self {
        Self {
            version: MEMBERSHIP_RECORD_PAGE_VERSION,
            network_name: network_name.to_owned(),
            membership_tag,
            snapshot: None,
            cursor: 0,
        }
    }

    #[must_use]
    pub fn next(&self, snapshot: String, cursor: u16) -> Self {
        Self {
            version: MEMBERSHIP_RECORD_PAGE_VERSION,
            network_name: self.network_name.clone(),
            membership_tag: self.membership_tag.clone(),
            snapshot: Some(snapshot),
            cursor,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MembershipRecordsPage {
    pub version: u8,
    pub network_name: String,
    #[serde(default)]
    pub membership_tag: Option<String>,
    pub snapshot: String,
    pub cursor: u16,
    pub total_records: u16,
    pub records: Vec<SignedMembershipRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u16>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MembershipRecordsRejectionReason {
    UnauthorizedPeer,
    RateLimited,
    WrongNetwork,
    MembershipMismatch,
    UnsupportedVersion,
    InvalidSnapshot,
    InvalidCursor,
    SnapshotChanged,
    PageTooLarge,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ControlRejectionReason {
    UnauthorizedPeer,
    WrongNetwork,
    MembershipMismatch,
    UnsupportedWireVersion,
    UnsupportedPacketProtocol,
    UnsupportedPacketHeaderLength,
    InvalidEffectiveMtu,
    UnsupportedPreferredPath,
    UnauthorizedRouteAdvertisement,
    InvalidOwnedQuicCertificate,
    InvalidMembershipRecord,
    InvalidHostnameRecord,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PeerCapabilities {
    peers: HashMap<PeerId, ControlCapabilities>,
}

impl PeerCapabilities {
    pub fn record(&mut self, peer: PeerId, capabilities: ControlCapabilities) {
        self.peers.insert(peer, capabilities);
    }

    pub fn remove(&mut self, peer: PeerId) {
        self.peers.remove(&peer);
    }

    #[must_use]
    pub fn contains(&self, peer: PeerId) -> bool {
        self.peers.contains_key(&peer)
    }

    #[must_use]
    pub fn get(&self, peer: PeerId) -> Option<&ControlCapabilities> {
        self.peers.get(&peer)
    }

    #[must_use]
    pub fn effective_mtu_for(&self, peer: PeerId, fallback_mtu: u16) -> u16 {
        self.peers
            .get(&peer)
            .map_or(fallback_mtu, |capabilities| capabilities.effective_mtu)
            .min(fallback_mtu)
    }

    #[must_use]
    pub fn supports_quic_datagrams_for(&self, peer: PeerId) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(|capabilities| capabilities.supports_quic_datagrams)
    }

    #[must_use]
    pub fn supports_native_quic_datagrams_for(&self, peer: PeerId) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(|capabilities| capabilities.supports_native_quic_datagrams)
    }

    #[must_use]
    pub fn supports_owned_udp_packet_plane_for(&self, peer: PeerId) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(|capabilities| capabilities.supports_owned_udp_packet_plane)
    }

    #[must_use]
    pub fn supports_owned_quic_packet_plane_for(&self, peer: PeerId) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(|capabilities| capabilities.supports_owned_quic_packet_plane)
    }

    #[must_use]
    pub fn owned_quic_packet_plane_certificate_for(&self, peer: PeerId) -> Option<&[u8]> {
        self.peers.get(&peer).and_then(|capabilities| {
            capabilities
                .owned_quic_packet_plane_certificate_der
                .as_deref()
        })
    }

    #[must_use]
    pub fn supports_datagram_packet_path_for(&self, peer: PeerId) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(ControlCapabilities::supports_datagram_packet_path)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
}

#[must_use]
pub fn validate_capabilities(
    capabilities: &ControlCapabilities,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) -> Option<ControlRejectionReason> {
    if capabilities.network_name != expected_network {
        return Some(ControlRejectionReason::WrongNetwork);
    }
    if !membership_tag_matches(
        capabilities.membership_tag.as_deref(),
        expected_membership_tag,
        previous_membership_tags,
    ) {
        return Some(ControlRejectionReason::MembershipMismatch);
    }
    if capabilities.wire_version != WIRE_VERSION {
        return Some(ControlRejectionReason::UnsupportedWireVersion);
    }
    if capabilities.packet_protocol != PACKET_PROTOCOL {
        return Some(ControlRejectionReason::UnsupportedPacketProtocol);
    }
    if capabilities.packet_header_len != HEADER_LEN {
        return Some(ControlRejectionReason::UnsupportedPacketHeaderLength);
    }
    if capabilities.effective_mtu == 0 {
        return Some(ControlRejectionReason::InvalidEffectiveMtu);
    }
    let Some(preferred_path) = PathKind::from_wire_name(&capabilities.preferred_path) else {
        return Some(ControlRejectionReason::UnsupportedPreferredPath);
    };
    if preferred_path.requires_quic_datagrams() && !capabilities.supports_datagram_packet_path() {
        return Some(ControlRejectionReason::UnsupportedPreferredPath);
    }
    if capabilities
        .packet_endpoint_candidates
        .iter()
        .any(|endpoint| !validate_packet_plane_endpoint_candidate(endpoint))
    {
        return Some(ControlRejectionReason::UnsupportedPreferredPath);
    }
    if capabilities
        .owned_quic_packet_endpoint_candidates
        .iter()
        .any(|endpoint| !validate_packet_plane_endpoint_candidate(endpoint))
    {
        return Some(ControlRejectionReason::UnsupportedPreferredPath);
    }
    if capabilities.member_records.len() > MAX_CONTROL_MEMBERSHIP_RECORDS {
        return Some(ControlRejectionReason::InvalidMembershipRecord);
    }
    if capabilities.hostname_records.len() > MAX_CONTROL_HOSTNAME_RECORDS {
        return Some(ControlRejectionReason::InvalidHostnameRecord);
    }
    if capabilities.supports_membership_record_pages {
        if usize::from(capabilities.membership_record_count) > MAX_MEMBERSHIP_RECORDS
            || capabilities
                .membership_records_snapshot
                .as_deref()
                .is_none_or(|snapshot| !is_membership_records_snapshot(snapshot))
        {
            return Some(ControlRejectionReason::InvalidMembershipRecord);
        }
    } else if capabilities.membership_records_snapshot.is_some()
        || capabilities.membership_record_count != 0
    {
        return Some(ControlRejectionReason::InvalidMembershipRecord);
    }
    if capabilities.direct_address_candidates.len() > MAX_CONTROL_DIRECT_ADDRESS_CANDIDATES
        || capabilities
            .direct_address_candidates
            .iter()
            .any(|address| address.parse::<libp2p::Multiaddr>().is_err())
    {
        return Some(ControlRejectionReason::UnsupportedPreferredPath);
    }
    if capabilities.supports_owned_quic_packet_plane {
        let Some(certificate) = capabilities
            .owned_quic_packet_plane_certificate_der
            .as_deref()
        else {
            return Some(ControlRejectionReason::InvalidOwnedQuicCertificate);
        };
        if certificate.is_empty()
            || certificate.len() > MAX_OWNED_QUIC_PACKET_PLANE_CERTIFICATE_DER_LEN
            || !validate_owned_quic_certificate_der(certificate)
        {
            return Some(ControlRejectionReason::InvalidOwnedQuicCertificate);
        }
        if capabilities
            .owned_quic_packet_endpoint_candidates
            .is_empty()
        {
            return Some(ControlRejectionReason::UnsupportedPreferredPath);
        }
    } else if capabilities
        .owned_quic_packet_plane_certificate_der
        .as_deref()
        .is_some_and(|certificate| !certificate.is_empty())
    {
        return Some(ControlRejectionReason::InvalidOwnedQuicCertificate);
    }

    None
}

fn validate_owned_quic_certificate_der(certificate: &[u8]) -> bool {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(certificate.to_vec()))
        .is_ok()
}

#[must_use]
pub fn membership_tag_matches(
    actual: Option<&str>,
    expected_current: Option<&str>,
    previous_membership_tags: &[String],
) -> bool {
    match expected_current {
        None => actual.is_none(),
        Some(current) => {
            actual == Some(current)
                || actual.is_some_and(|actual| {
                    previous_membership_tags
                        .iter()
                        .any(|previous| previous == actual)
                })
        }
    }
}

#[must_use]
pub fn accepted_capabilities_response(capabilities: &ControlCapabilities) -> ControlResponse {
    ControlResponse::CapabilitiesAccepted(capabilities.clone())
}

#[must_use]
pub const fn rejected_capabilities_response(reason: ControlRejectionReason) -> ControlResponse {
    ControlResponse::CapabilitiesRejected(reason)
}

#[must_use]
pub fn membership_records_snapshot(records: &[SignedMembershipRecord]) -> String {
    let mut encoded = records
        .iter()
        .map(|record| {
            (
                serde_json::to_vec(record)
                    .expect("membership records contain only JSON-serializable fields"),
                record,
            )
        })
        .collect::<Vec<_>>();
    encoded.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));

    let mut digest = Sha256::new();
    digest.update(MEMBERSHIP_RECORD_SNAPSHOT_DOMAIN);
    for (record, _) in encoded {
        let length = u32::try_from(record.len()).expect("membership record length is bounded");
        digest.update(length.to_be_bytes());
        digest.update(record);
    }
    format!(
        "{MEMBERSHIP_RECORD_SNAPSHOT_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(digest.finalize())
    )
}

#[must_use]
pub fn is_membership_records_snapshot(snapshot: &str) -> bool {
    let Some(encoded) = snapshot.strip_prefix(MEMBERSHIP_RECORD_SNAPSHOT_PREFIX) else {
        return false;
    };
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    decoded.len() == SHA256_LEN && URL_SAFE_NO_PAD.encode(decoded) == encoded
}

pub fn build_membership_records_page(
    request: &MembershipRecordsRequest,
    records: &[SignedMembershipRecord],
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) -> Result<MembershipRecordsPage, MembershipRecordsRejectionReason> {
    if records.len() > MAX_MEMBERSHIP_RECORDS {
        return Err(MembershipRecordsRejectionReason::PageTooLarge);
    }
    build_membership_records_page_for_snapshot(
        request,
        records,
        &membership_records_snapshot(records),
        expected_network,
        expected_membership_tag,
        previous_membership_tags,
    )
}

pub fn build_membership_records_page_for_snapshot(
    request: &MembershipRecordsRequest,
    records: &[SignedMembershipRecord],
    current_snapshot: &str,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) -> Result<MembershipRecordsPage, MembershipRecordsRejectionReason> {
    if request.version != MEMBERSHIP_RECORD_PAGE_VERSION {
        return Err(MembershipRecordsRejectionReason::UnsupportedVersion);
    }
    if request.network_name != expected_network {
        return Err(MembershipRecordsRejectionReason::WrongNetwork);
    }
    if !membership_tag_matches(
        request.membership_tag.as_deref(),
        expected_membership_tag,
        previous_membership_tags,
    ) {
        return Err(MembershipRecordsRejectionReason::MembershipMismatch);
    }
    if records.len() > MAX_MEMBERSHIP_RECORDS {
        return Err(MembershipRecordsRejectionReason::PageTooLarge);
    }
    if !is_membership_records_snapshot(current_snapshot) {
        return Err(MembershipRecordsRejectionReason::InvalidSnapshot);
    }
    if request
        .snapshot
        .as_deref()
        .is_some_and(|snapshot| !is_membership_records_snapshot(snapshot))
    {
        return Err(MembershipRecordsRejectionReason::InvalidSnapshot);
    }
    if request.snapshot.is_none() && request.cursor != 0 {
        return Err(MembershipRecordsRejectionReason::InvalidCursor);
    }

    let mut sorted = records.iter().collect::<Vec<_>>();
    sorted.sort_unstable_by(|left, right| compare_membership_records(left, right));
    if request
        .snapshot
        .as_deref()
        .is_some_and(|expected| expected != current_snapshot)
    {
        return Err(MembershipRecordsRejectionReason::SnapshotChanged);
    }

    let total_records = u16::try_from(sorted.len()).expect("membership record count is bounded");
    if request.cursor > total_records {
        return Err(MembershipRecordsRejectionReason::InvalidCursor);
    }
    let mut page = MembershipRecordsPage {
        version: MEMBERSHIP_RECORD_PAGE_VERSION,
        network_name: expected_network.to_owned(),
        membership_tag: expected_membership_tag.map(str::to_owned),
        snapshot: current_snapshot.to_owned(),
        cursor: request.cursor,
        total_records,
        records: Vec::new(),
        next_cursor: None,
    };
    let start = usize::from(request.cursor);
    for record in sorted
        .into_iter()
        .skip(start)
        .take(MAX_CONTROL_MEMBERSHIP_RECORDS)
    {
        page.records.push(record.clone());
        let next = start + page.records.len();
        page.next_cursor = (next < usize::from(total_records))
            .then(|| u16::try_from(next).expect("membership page cursor is bounded"));
        let response = ControlResponse::MembershipRecordsPage(page.clone());
        let encoded = serde_json::to_vec(&response)
            .expect("control responses contain only JSON-serializable fields");
        if encoded.len() > MAX_CONTROL_MESSAGE_LEN {
            page.records.pop();
            let next = start + page.records.len();
            page.next_cursor = (next < usize::from(total_records))
                .then(|| u16::try_from(next).expect("membership page cursor is bounded"));
            break;
        }
    }
    if start < usize::from(total_records) && page.records.is_empty() {
        return Err(MembershipRecordsRejectionReason::PageTooLarge);
    }
    let encoded = serde_json::to_vec(&ControlResponse::MembershipRecordsPage(page.clone()))
        .expect("control responses contain only JSON-serializable fields");
    if encoded.len() > MAX_CONTROL_MESSAGE_LEN {
        return Err(MembershipRecordsRejectionReason::PageTooLarge);
    }

    Ok(page)
}

fn compare_membership_records(
    left: &SignedMembershipRecord,
    right: &SignedMembershipRecord,
) -> Ordering {
    left.payload
        .issuer_peer
        .cmp(&right.payload.issuer_peer)
        .then_with(|| left.payload.member_peer.cmp(&right.payload.member_peer))
        .then_with(|| {
            left.payload
                .membership_epoch
                .cmp(&right.payload.membership_epoch)
        })
        .then_with(|| left.payload.sequence.cmp(&right.payload.sequence))
        .then_with(|| {
            let left = serde_json::to_vec(left)
                .expect("membership records contain only JSON-serializable fields");
            let right = serde_json::to_vec(right)
                .expect("membership records contain only JSON-serializable fields");
            left.cmp(&right)
        })
}

pub fn validate_membership_records_page(
    page: &MembershipRecordsPage,
    request: &MembershipRecordsRequest,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) -> Result<(), MembershipRecordsRejectionReason> {
    if page.version != MEMBERSHIP_RECORD_PAGE_VERSION {
        return Err(MembershipRecordsRejectionReason::UnsupportedVersion);
    }
    if page.network_name != expected_network {
        return Err(MembershipRecordsRejectionReason::WrongNetwork);
    }
    if !membership_tag_matches(
        page.membership_tag.as_deref(),
        expected_membership_tag,
        previous_membership_tags,
    ) {
        return Err(MembershipRecordsRejectionReason::MembershipMismatch);
    }
    if !is_membership_records_snapshot(&page.snapshot) {
        return Err(MembershipRecordsRejectionReason::InvalidSnapshot);
    }
    if request
        .snapshot
        .as_deref()
        .is_some_and(|snapshot| snapshot != page.snapshot)
    {
        return Err(MembershipRecordsRejectionReason::SnapshotChanged);
    }
    if usize::from(page.total_records) > MAX_MEMBERSHIP_RECORDS
        || page.records.len() > MAX_CONTROL_MEMBERSHIP_RECORDS
        || page.cursor != request.cursor
    {
        return Err(MembershipRecordsRejectionReason::InvalidCursor);
    }
    let end = usize::from(page.cursor).saturating_add(page.records.len());
    let total = usize::from(page.total_records);
    if end > total || page.records.is_empty() && end < total {
        return Err(MembershipRecordsRejectionReason::InvalidCursor);
    }
    let expected_next =
        (end < total).then(|| u16::try_from(end).expect("membership page cursor is bounded"));
    if page.next_cursor != expected_next {
        return Err(MembershipRecordsRejectionReason::InvalidCursor);
    }

    Ok(())
}

#[async_trait]
impl request_response::Codec for ControlCodec {
    type Protocol = StreamProtocol;
    type Request = ControlRequest;
    type Response = ControlResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_json_message(io).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_json_message(io).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        request: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_json_message(io, &request).await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        response: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        write_json_message(io, &response).await
    }
}

#[must_use]
pub fn behaviour(max_concurrent_streams: usize) -> request_response::Behaviour<ControlCodec> {
    let protocols = [(
        StreamProtocol::new(CONTROL_PROTOCOL),
        request_response::ProtocolSupport::Full,
    )];
    let config = request_response::Config::default().with_request_timeout(Duration::from_secs(10));

    request_response::Behaviour::with_codec(
        ControlCodec,
        protocols,
        config.with_max_concurrent_streams(max_concurrent_streams.max(1)),
    )
}

async fn read_json_message<T, M>(io: &mut T) -> io::Result<M>
where
    T: AsyncRead + Unpin + Send,
    M: for<'de> Deserialize<'de>,
{
    let mut length = [0; 2];
    io.read_exact(&mut length).await?;
    let length = usize::from(u16::from_be_bytes(length));
    if length > MAX_CONTROL_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("control message length {length} exceeds limit {MAX_CONTROL_MESSAGE_LEN}"),
        ));
    }

    let mut payload = vec![0; length];
    io.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).map_err(invalid_data)
}

async fn write_json_message<T, M>(io: &mut T, message: &M) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    M: Serialize,
{
    let payload = serde_json::to_vec(message).map_err(invalid_data)?;
    let length = u16::try_from(payload.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "control message length {} exceeds encodable limit {}",
                payload.len(),
                u16::MAX
            ),
        )
    })?;
    if usize::from(length) > MAX_CONTROL_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "control message length {} exceeds limit {MAX_CONTROL_MESSAGE_LEN}",
                payload.len()
            ),
        ));
    }

    io.write_all(&length.to_be_bytes()).await?;
    io.write_all(&payload).await?;
    io.close().await
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use futures::io::Cursor;

    use super::*;

    fn membership_record(sequence: u64) -> SignedMembershipRecord {
        SignedMembershipRecord {
            payload: crate::membership::MembershipRecordPayload {
                version: crate::membership::MEMBERSHIP_RECORD_VERSION,
                network_name: "lab".to_owned(),
                member_peer: format!("member-{sequence}"),
                member_public_key: format!("member-key-{sequence}"),
                issuer_peer: "issuer".to_owned(),
                issuer_public_key: "issuer-key".to_owned(),
                membership_epoch: 1,
                sequence,
                revoked: false,
                hostname: None,
                roles: vec![crate::membership::MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                issued_at_unix_seconds: 1,
                expires_at_unix_seconds: None,
            },
            signature: format!("signature-{sequence}"),
        }
    }

    fn test_owned_quic_certificate_der() -> Vec<u8> {
        rcgen::generate_simple_self_signed(vec!["p2p-vpn-packet-plane".to_owned()])
            .expect("test certificate")
            .cert
            .der()
            .to_vec()
    }

    #[tokio::test]
    async fn control_codec_round_trips_capabilities() {
        let mut codec = ControlCodec;
        let protocol = StreamProtocol::new(CONTROL_PROTOCOL);
        let request = ControlRequest::Capabilities(
            ControlCapabilities::local("lab", None, 1280)
                .with_advertised_routes(vec![ControlRoute::new("100.64.1.2/32", 0)]),
        );
        let mut written = Cursor::new(Vec::new());

        request_response::Codec::write_request(
            &mut codec,
            &protocol,
            &mut written,
            request.clone(),
        )
        .await
        .expect("write request");

        written.set_position(0);
        let decoded = request_response::Codec::read_request(&mut codec, &protocol, &mut written)
            .await
            .expect("read request");

        assert_eq!(decoded, request);
    }

    #[test]
    fn capabilities_decode_missing_advertised_routes_as_empty() {
        let payload = serde_json::json!({
            "network_name": "lab",
            "wire_version": WIRE_VERSION,
            "packet_protocol": PACKET_PROTOCOL,
            "packet_header_len": HEADER_LEN,
            "effective_mtu": 1280,
            "preferred_path": "direct_quic_stream",
            "supports_quic_datagrams": false
        });

        let capabilities: ControlCapabilities =
            serde_json::from_value(payload).expect("capabilities decode");

        assert!(capabilities.advertised_routes.is_empty());
        assert!(capabilities.packet_endpoint_candidates.is_empty());
        assert!(capabilities.member_records.is_empty());
        assert!(capabilities.hostname_records.is_empty());
        assert!(!capabilities.supports_membership_record_pages);
        assert_eq!(capabilities.membership_records_snapshot, None);
        assert_eq!(capabilities.membership_record_count, 0);
    }

    #[test]
    fn capabilities_reject_too_many_member_records() {
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.member_records = vec![
            crate::membership::SignedMembershipRecord {
                payload: crate::membership::MembershipRecordPayload {
                    version: crate::membership::MEMBERSHIP_RECORD_VERSION,
                    network_name: "lab".to_owned(),
                    member_peer: String::new(),
                    member_public_key: String::new(),
                    issuer_peer: String::new(),
                    issuer_public_key: String::new(),
                    membership_epoch: 1,
                    sequence: 1,
                    revoked: false,
                    hostname: None,
                    roles: vec![crate::membership::MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    issued_at_unix_seconds: 1,
                    expires_at_unix_seconds: None,
                },
                signature: String::new(),
            };
            MAX_CONTROL_MEMBERSHIP_RECORDS + 1
        ];

        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::InvalidMembershipRecord)
        );
    }

    #[test]
    fn capabilities_reject_too_many_hostname_records() {
        let identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let record = crate::hostname::issue_hostname_record_at(&identity, "lab", "host", 1, 1_000)
            .expect("hostname record");
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.hostname_records = vec![record; MAX_CONTROL_HOSTNAME_RECORDS + 1];

        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::InvalidHostnameRecord)
        );
    }

    #[tokio::test]
    async fn control_codec_round_trips_capability_rejection() {
        let mut codec = ControlCodec;
        let protocol = StreamProtocol::new(CONTROL_PROTOCOL);
        let response =
            ControlResponse::CapabilitiesRejected(ControlRejectionReason::UnauthorizedPeer);
        let mut written = Cursor::new(Vec::new());

        request_response::Codec::write_response(
            &mut codec,
            &protocol,
            &mut written,
            response.clone(),
        )
        .await
        .expect("write response");

        written.set_position(0);
        let decoded = request_response::Codec::read_response(&mut codec, &protocol, &mut written)
            .await
            .expect("read response");

        assert_eq!(decoded, response);
    }

    #[tokio::test]
    async fn control_codec_round_trips_packet_plane_negotiation() {
        let mut codec = ControlCodec;
        let protocol = StreamProtocol::new(CONTROL_PROTOCOL);
        let request = ControlRequest::PacketPlaneHello(vec![1, 2, 3, 4]);
        let response = ControlResponse::PacketPlaneAccepted(vec![5, 6, 7, 8]);
        let mut written_request = Cursor::new(Vec::new());
        let mut written_response = Cursor::new(Vec::new());

        request_response::Codec::write_request(
            &mut codec,
            &protocol,
            &mut written_request,
            request.clone(),
        )
        .await
        .expect("write request");
        request_response::Codec::write_response(
            &mut codec,
            &protocol,
            &mut written_response,
            response.clone(),
        )
        .await
        .expect("write response");

        written_request.set_position(0);
        written_response.set_position(0);
        let decoded_request =
            request_response::Codec::read_request(&mut codec, &protocol, &mut written_request)
                .await
                .expect("read request");
        let decoded_response =
            request_response::Codec::read_response(&mut codec, &protocol, &mut written_response)
                .await
                .expect("read response");

        assert_eq!(decoded_request, request);
        assert_eq!(decoded_response, response);
    }

    #[tokio::test]
    async fn control_codec_round_trips_membership_record_page() {
        let mut codec = ControlCodec;
        let protocol = StreamProtocol::new(CONTROL_PROTOCOL);
        let request = MembershipRecordsRequest::first("lab", Some("tag".to_owned()));
        let page = build_membership_records_page(
            &request,
            &[membership_record(1)],
            "lab",
            Some("tag"),
            &[],
        )
        .expect("membership page");
        let response = ControlResponse::MembershipRecordsPage(page);
        let mut written_request = Cursor::new(Vec::new());
        let mut written_response = Cursor::new(Vec::new());

        request_response::Codec::write_request(
            &mut codec,
            &protocol,
            &mut written_request,
            ControlRequest::MembershipRecords(request.clone()),
        )
        .await
        .expect("write request");
        request_response::Codec::write_response(
            &mut codec,
            &protocol,
            &mut written_response,
            response.clone(),
        )
        .await
        .expect("write response");

        written_request.set_position(0);
        written_response.set_position(0);
        let decoded_request =
            request_response::Codec::read_request(&mut codec, &protocol, &mut written_request)
                .await
                .expect("read request");
        let decoded_response =
            request_response::Codec::read_response(&mut codec, &protocol, &mut written_response)
                .await
                .expect("read response");

        assert_eq!(decoded_request, ControlRequest::MembershipRecords(request));
        assert_eq!(decoded_response, response);
    }

    #[test]
    fn membership_snapshot_is_stable_across_record_order() {
        let first = membership_record(1);
        let second = membership_record(2);

        let forward = membership_records_snapshot(&[first.clone(), second.clone()]);
        let reverse = membership_records_snapshot(&[second, first]);

        assert_eq!(forward, reverse);
        assert!(is_membership_records_snapshot(&forward));
        assert!(!is_membership_records_snapshot("sha256:not-base64"));
    }

    #[test]
    fn membership_inventory_enables_paged_capability_advertisement() {
        let records = vec![membership_record(1), membership_record(2)];
        let capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_membership_record_inventory(&records);
        let expected_snapshot = membership_records_snapshot(&records);

        assert!(capabilities.supports_membership_record_pages);
        assert_eq!(capabilities.membership_record_count, 2);
        assert_eq!(
            capabilities.membership_records_snapshot.as_deref(),
            Some(expected_snapshot.as_str())
        );
    }

    #[test]
    fn membership_pages_are_bounded_and_cursor_validated() {
        let records = (0..10).map(membership_record).collect::<Vec<_>>();
        let first_request = MembershipRecordsRequest::first("lab", None);
        let first = build_membership_records_page(&first_request, &records, "lab", None, &[])
            .expect("first page");

        assert_eq!(first.cursor, 0);
        assert_eq!(first.total_records, 10);
        assert_eq!(first.records.len(), MAX_CONTROL_MEMBERSHIP_RECORDS);
        assert_eq!(first.next_cursor, Some(8));
        validate_membership_records_page(&first, &first_request, "lab", None, &[])
            .expect("valid first page");

        let second_request = first_request.next(first.snapshot.clone(), 8);
        let second = build_membership_records_page(&second_request, &records, "lab", None, &[])
            .expect("second page");
        assert_eq!(second.records.len(), 2);
        assert_eq!(second.next_cursor, None);
        validate_membership_records_page(&second, &second_request, "lab", None, &[])
            .expect("valid second page");

        let invalid_request = MembershipRecordsRequest {
            cursor: 1,
            ..MembershipRecordsRequest::first("lab", None)
        };
        assert_eq!(
            build_membership_records_page(&invalid_request, &records, "lab", None, &[]),
            Err(MembershipRecordsRejectionReason::InvalidCursor)
        );
    }

    #[test]
    fn membership_pages_respect_encoded_frame_size() {
        let mut first = membership_record(1);
        first.signature = "a".repeat(9_000);
        let mut second = membership_record(2);
        second.signature = "b".repeat(9_000);
        let request = MembershipRecordsRequest::first("lab", None);

        let page = build_membership_records_page(&request, &[first, second], "lab", None, &[])
            .expect("size-bounded page");
        let encoded = serde_json::to_vec(&ControlResponse::MembershipRecordsPage(page.clone()))
            .expect("encoded page");

        assert_eq!(page.records.len(), 1);
        assert_eq!(page.next_cursor, Some(1));
        assert!(encoded.len() <= MAX_CONTROL_MESSAGE_LEN);
    }

    #[test]
    fn cached_membership_pages_cover_the_maximum_bounded_inventory() {
        let records = (0..MAX_MEMBERSHIP_RECORDS)
            .map(|sequence| {
                let mut record = membership_record(u64::try_from(sequence).expect("sequence"));
                record.signature = "x".repeat(10_000);
                record
            })
            .collect::<Vec<_>>();
        let snapshot = membership_records_snapshot(&records);
        let mut request = MembershipRecordsRequest::first("lab", None);
        let mut records_received = 0;
        let mut pages = 0;

        loop {
            let page = build_membership_records_page_for_snapshot(
                &request,
                &records,
                &snapshot,
                "lab",
                None,
                &[],
            )
            .expect("bounded page");
            let encoded = serde_json::to_vec(&ControlResponse::MembershipRecordsPage(page.clone()))
                .expect("encoded page");
            assert!(encoded.len() <= MAX_CONTROL_MESSAGE_LEN);
            assert_eq!(page.snapshot, snapshot);
            records_received += page.records.len();
            pages += 1;
            let Some(next_cursor) = page.next_cursor else {
                break;
            };
            request = request.next(snapshot.clone(), next_cursor);
        }

        assert_eq!(records_received, MAX_MEMBERSHIP_RECORDS);
        assert_eq!(pages, MAX_MEMBERSHIP_RECORDS);
    }

    #[test]
    fn membership_page_detects_snapshot_changes() {
        let records = vec![membership_record(1)];
        let request = MembershipRecordsRequest {
            snapshot: Some(membership_records_snapshot(&[membership_record(2)])),
            ..MembershipRecordsRequest::first("lab", None)
        };

        assert_eq!(
            build_membership_records_page(&request, &records, "lab", None, &[]),
            Err(MembershipRecordsRejectionReason::SnapshotChanged)
        );
    }

    #[tokio::test]
    async fn control_codec_rejects_oversized_messages() {
        let mut codec = ControlCodec;
        let protocol = StreamProtocol::new(CONTROL_PROTOCOL);
        let length = u16::try_from(MAX_CONTROL_MESSAGE_LEN + 1).expect("test length fits");
        let mut input = Cursor::new(length.to_be_bytes().to_vec());

        let error = request_response::Codec::read_request(&mut codec, &protocol, &mut input)
            .await
            .expect_err("oversized control message should fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn local_capabilities_describe_packet_surface() {
        let capabilities = ControlCapabilities::local("lab", None, 1420);

        assert_eq!(capabilities.network_name, "lab");
        assert_eq!(capabilities.wire_version, WIRE_VERSION);
        assert_eq!(capabilities.packet_protocol, PACKET_PROTOCOL);
        assert_eq!(capabilities.packet_header_len, HEADER_LEN);
        assert_eq!(capabilities.effective_mtu, 1420);
        assert_eq!(capabilities.preferred_path, "direct_quic_stream");
        assert!(capabilities.advertised_routes.is_empty());
        assert!(!capabilities.supports_quic_datagrams);
        assert!(!capabilities.supports_native_quic_datagrams);
        assert!(!capabilities.supports_owned_udp_packet_plane);
        assert!(!capabilities.supports_owned_quic_packet_plane);
        assert_eq!(capabilities.owned_quic_packet_plane_certificate_der, None);
        assert!(capabilities.packet_endpoint_candidates.is_empty());
        assert!(!capabilities.supports_membership_record_pages);
        assert_eq!(capabilities.membership_record_count, 0);
        assert_eq!(capabilities.membership_records_snapshot, None);
    }

    #[test]
    fn local_capabilities_advertise_packet_datagram_backends() {
        let owned = ControlCapabilities::local("lab", None, 1420).with_owned_udp_packet_plane(true);
        assert!(owned.supports_quic_datagrams);
        assert!(!owned.supports_native_quic_datagrams);
        assert!(owned.supports_owned_udp_packet_plane);
        assert!(!owned.supports_owned_quic_packet_plane);
        assert_eq!(owned.preferred_path, "direct_udp_datagram");

        let certificate_der = test_owned_quic_certificate_der();
        let owned_quic = ControlCapabilities::local("lab", None, 1420)
            .with_owned_quic_packet_plane_certificate(certificate_der.clone())
            .with_owned_quic_packet_endpoint_candidates(vec!["203.0.113.10:51821".to_owned()]);
        assert!(owned_quic.supports_quic_datagrams);
        assert!(!owned_quic.supports_native_quic_datagrams);
        assert!(!owned_quic.supports_owned_udp_packet_plane);
        assert!(owned_quic.supports_owned_quic_packet_plane);
        assert_eq!(owned_quic.preferred_path, "direct_quic_datagram");
        assert_eq!(
            owned_quic
                .owned_quic_packet_plane_certificate_der
                .as_deref(),
            Some(certificate_der.as_slice())
        );

        let native = ControlCapabilities::local("lab", None, 1420).with_native_quic_datagrams(true);
        assert!(native.supports_quic_datagrams);
        assert!(native.supports_native_quic_datagrams);
        assert!(!native.supports_owned_udp_packet_plane);
        assert!(!native.supports_owned_quic_packet_plane);
        assert_eq!(native.preferred_path, "direct_quic_datagram");
    }

    #[test]
    fn local_capabilities_can_advertise_owned_packet_endpoints() {
        let capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_packet_endpoint_candidates(vec!["203.0.113.10:51820".to_owned()]);

        assert_eq!(
            capabilities.packet_endpoint_candidates,
            vec!["203.0.113.10:51820"]
        );
        assert_eq!(validate_capabilities(&capabilities, "lab", None, &[]), None);
    }

    #[test]
    fn peer_capabilities_bound_effective_peer_mtu() {
        let peer = PeerId::from_bytes([1; 32]);
        let mut capabilities = PeerCapabilities::default();

        assert_eq!(capabilities.effective_mtu_for(peer, 1280), 1280);

        capabilities.record(peer, ControlCapabilities::local("lab", None, 1200));
        assert_eq!(capabilities.effective_mtu_for(peer, 1280), 1200);

        capabilities.record(peer, ControlCapabilities::local("lab", None, 1420));
        assert_eq!(capabilities.effective_mtu_for(peer, 1280), 1280);
    }

    #[test]
    fn peer_capabilities_report_quic_datagram_support() {
        let peer = PeerId::from_bytes([1; 32]);
        let mut capabilities = PeerCapabilities::default();

        assert!(!capabilities.supports_quic_datagrams_for(peer));
        assert!(!capabilities.supports_native_quic_datagrams_for(peer));
        assert!(!capabilities.supports_owned_udp_packet_plane_for(peer));
        assert!(!capabilities.supports_owned_quic_packet_plane_for(peer));
        assert_eq!(
            capabilities.owned_quic_packet_plane_certificate_for(peer),
            None
        );
        assert!(!capabilities.supports_datagram_packet_path_for(peer));

        let mut peer_capabilities = ControlCapabilities::local("lab", None, 1280);
        peer_capabilities.supports_quic_datagrams = true;
        capabilities.record(peer, peer_capabilities);

        assert!(capabilities.supports_quic_datagrams_for(peer));
        assert!(!capabilities.supports_native_quic_datagrams_for(peer));
        assert!(!capabilities.supports_owned_udp_packet_plane_for(peer));
        assert!(!capabilities.supports_owned_quic_packet_plane_for(peer));
        assert!(capabilities.supports_datagram_packet_path_for(peer));

        capabilities.record(
            peer,
            ControlCapabilities::local("lab", None, 1280).with_owned_udp_packet_plane(true),
        );

        assert!(capabilities.supports_quic_datagrams_for(peer));
        assert!(!capabilities.supports_native_quic_datagrams_for(peer));
        assert!(capabilities.supports_owned_udp_packet_plane_for(peer));
        assert!(!capabilities.supports_owned_quic_packet_plane_for(peer));
        assert!(capabilities.supports_datagram_packet_path_for(peer));

        capabilities.record(
            peer,
            ControlCapabilities::local("lab", None, 1280)
                .with_owned_quic_packet_plane_certificate(test_owned_quic_certificate_der())
                .with_owned_quic_packet_endpoint_candidates(vec!["203.0.113.10:51821".to_owned()]),
        );

        assert!(capabilities.supports_quic_datagrams_for(peer));
        assert!(!capabilities.supports_native_quic_datagrams_for(peer));
        assert!(!capabilities.supports_owned_udp_packet_plane_for(peer));
        assert!(capabilities.supports_owned_quic_packet_plane_for(peer));
        assert!(
            capabilities
                .owned_quic_packet_plane_certificate_for(peer)
                .is_some_and(
                    |certificate| certificate.len() > 64 && certificate.starts_with(&[0x30])
                )
        );
        assert!(capabilities.supports_datagram_packet_path_for(peer));

        capabilities.record(
            peer,
            ControlCapabilities::local("lab", None, 1280).with_native_quic_datagrams(true),
        );

        assert!(capabilities.supports_quic_datagrams_for(peer));
        assert!(capabilities.supports_native_quic_datagrams_for(peer));
        assert!(!capabilities.supports_owned_udp_packet_plane_for(peer));
        assert!(!capabilities.supports_owned_quic_packet_plane_for(peer));
        assert!(capabilities.supports_datagram_packet_path_for(peer));
    }

    #[test]
    fn peer_capabilities_can_be_invalidated() {
        let peer = PeerId::from_bytes([1; 32]);
        let mut capabilities = PeerCapabilities::default();

        capabilities.record(peer, ControlCapabilities::local("lab", None, 1280));
        assert!(capabilities.contains(peer));

        capabilities.remove(peer);
        assert!(!capabilities.contains(peer));
    }

    #[test]
    fn capability_validation_rejects_incompatible_protocol_surfaces() {
        let mut capabilities = ControlCapabilities::local("lab", None, 1280);
        assert_eq!(validate_capabilities(&capabilities, "lab", None, &[]), None);

        assert_eq!(
            validate_capabilities(&capabilities, "prod", None, &[]),
            Some(ControlRejectionReason::WrongNetwork)
        );
        assert_eq!(
            validate_capabilities(&capabilities, "lab", Some("tag-a"), &[]),
            Some(ControlRejectionReason::MembershipMismatch)
        );

        capabilities.membership_tag = Some("tag-a".to_owned());
        assert_eq!(
            validate_capabilities(&capabilities, "lab", Some("tag-a"), &[]),
            None
        );
        assert_eq!(
            validate_capabilities(&capabilities, "lab", Some("tag-b"), &[]),
            Some(ControlRejectionReason::MembershipMismatch)
        );
        capabilities.membership_tag = None;

        capabilities.wire_version = WIRE_VERSION.saturating_add(1);
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::UnsupportedWireVersion)
        );

        capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.packet_protocol = "/different/packet/1".to_owned();
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::UnsupportedPacketProtocol)
        );

        capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.packet_header_len = HEADER_LEN + 1;
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::UnsupportedPacketHeaderLength)
        );

        capabilities = ControlCapabilities::local("lab", None, 0);
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::InvalidEffectiveMtu)
        );

        capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.preferred_path = "not_a_path".to_owned();
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::UnsupportedPreferredPath)
        );

        capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        capabilities.supports_quic_datagrams = false;
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::UnsupportedPreferredPath)
        );

        capabilities = capabilities.with_owned_udp_packet_plane(true);
        assert_eq!(validate_capabilities(&capabilities, "lab", None, &[]), None);

        capabilities.packet_endpoint_candidates = vec!["vpn-a.example.net:51820".to_owned()];
        assert_eq!(validate_capabilities(&capabilities, "lab", None, &[]), None);

        capabilities.packet_endpoint_candidates = vec!["not-a-socket".to_owned()];
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::UnsupportedPreferredPath)
        );
    }

    #[test]
    fn capability_validation_rejects_invalid_owned_quic_certificates() {
        let mut capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_owned_quic_packet_plane_certificate(test_owned_quic_certificate_der())
            .with_owned_quic_packet_endpoint_candidates(vec!["203.0.113.10:51821".to_owned()]);
        assert_eq!(validate_capabilities(&capabilities, "lab", None, &[]), None);

        capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_owned_quic_packet_plane_certificate(test_owned_quic_certificate_der());
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::UnsupportedPreferredPath)
        );

        capabilities =
            ControlCapabilities::local("lab", None, 1280).with_owned_quic_packet_plane(true);
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::InvalidOwnedQuicCertificate)
        );

        capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_owned_quic_packet_plane_certificate(Vec::new());
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::InvalidOwnedQuicCertificate)
        );

        capabilities = ControlCapabilities::local("lab", None, 1280)
            .with_owned_quic_packet_plane_certificate(vec![0x30, 0x01])
            .with_owned_quic_packet_endpoint_candidates(vec!["203.0.113.10:51821".to_owned()]);
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::InvalidOwnedQuicCertificate)
        );

        capabilities =
            ControlCapabilities::local("lab", None, 1280).with_owned_quic_packet_plane_certificate(
                vec![0x30; MAX_OWNED_QUIC_PACKET_PLANE_CERTIFICATE_DER_LEN + 1],
            );
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::InvalidOwnedQuicCertificate)
        );

        capabilities = ControlCapabilities::local("lab", None, 1280);
        capabilities.owned_quic_packet_plane_certificate_der = Some(vec![0x30, 0x01]);
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::InvalidOwnedQuicCertificate)
        );
    }

    #[test]
    fn capability_validation_accepts_previous_membership_tag() {
        let capabilities = ControlCapabilities::local("lab", Some("previous-tag".to_owned()), 1280);

        assert_eq!(
            validate_capabilities(
                &capabilities,
                "lab",
                Some("current-tag"),
                &[String::from("previous-tag")]
            ),
            None
        );
    }

    #[test]
    fn capabilities_decode_legacy_json_without_owned_quic_certificate() {
        let decoded: ControlCapabilities = serde_json::from_str(
            r#"{"network_name":"lab","membership_tag":null,"advertised_routes":[],"wire_version":1,"packet_protocol":"/p2p-vpn/packet/1","packet_header_len":25,"effective_mtu":1280,"preferred_path":"direct_quic_stream","supports_quic_datagrams":false,"packet_endpoint_candidates":[]}"#,
        )
        .expect("capabilities");

        assert!(!decoded.supports_native_quic_datagrams);
        assert!(!decoded.supports_owned_udp_packet_plane);
        assert!(!decoded.supports_owned_quic_packet_plane);
        assert_eq!(decoded.owned_quic_packet_plane_certificate_der, None);
    }
}
