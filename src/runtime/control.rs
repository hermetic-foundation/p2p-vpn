use std::{collections::HashMap, io, time::Duration};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{StreamProtocol, request_response};
use serde::{Deserialize, Serialize};

use crate::{
    PathKind, PeerId,
    runtime::packet::PACKET_PROTOCOL,
    wire::{HEADER_LEN, WIRE_VERSION},
};

pub const CONTROL_PROTOCOL: &str = "/p2p-vpn/control/1";
const MAX_CONTROL_MESSAGE_LEN: usize = 4096;

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
    pub packet_endpoint_candidates: Vec<String>,
}

impl ControlCapabilities {
    #[must_use]
    pub fn local(network_name: &str, membership_tag: Option<String>, effective_mtu: u16) -> Self {
        let preferred_path = PathKind::DirectQuicStream;
        Self {
            network_name: network_name.to_owned(),
            membership_tag,
            advertised_routes: Vec::new(),
            wire_version: WIRE_VERSION,
            packet_protocol: PACKET_PROTOCOL.to_owned(),
            packet_header_len: HEADER_LEN,
            effective_mtu,
            preferred_path: preferred_path.wire_name().to_owned(),
            supports_quic_datagrams: false,
            packet_endpoint_candidates: Vec::new(),
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ControlRequest {
    Capabilities(ControlCapabilities),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ControlResponse {
    CapabilitiesAccepted(ControlCapabilities),
    CapabilitiesRejected(ControlRejectionReason),
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
    if preferred_path.requires_quic_datagrams() && !capabilities.supports_quic_datagrams {
        return Some(ControlRejectionReason::UnsupportedPreferredPath);
    }
    if capabilities
        .packet_endpoint_candidates
        .iter()
        .any(|endpoint| endpoint.parse::<std::net::SocketAddr>().is_err())
    {
        return Some(ControlRejectionReason::UnsupportedPreferredPath);
    }

    None
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
        assert!(capabilities.packet_endpoint_candidates.is_empty());
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

        let mut peer_capabilities = ControlCapabilities::local("lab", None, 1280);
        peer_capabilities.supports_quic_datagrams = true;
        capabilities.record(peer, peer_capabilities);

        assert!(capabilities.supports_quic_datagrams_for(peer));
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

        capabilities.supports_quic_datagrams = true;
        assert_eq!(validate_capabilities(&capabilities, "lab", None, &[]), None);

        capabilities.packet_endpoint_candidates = vec!["not-a-socket".to_owned()];
        assert_eq!(
            validate_capabilities(&capabilities, "lab", None, &[]),
            Some(ControlRejectionReason::UnsupportedPreferredPath)
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
}
