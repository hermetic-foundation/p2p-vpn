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
pub struct ControlCapabilities {
    pub network_name: String,
    pub wire_version: u8,
    pub packet_protocol: String,
    pub packet_header_len: usize,
    pub effective_mtu: u16,
    pub preferred_path: String,
    pub supports_quic_datagrams: bool,
}

impl ControlCapabilities {
    #[must_use]
    pub fn local(network_name: &str, effective_mtu: u16) -> Self {
        let preferred_path = PathKind::DirectQuicStream;
        Self {
            network_name: network_name.to_owned(),
            wire_version: WIRE_VERSION,
            packet_protocol: PACKET_PROTOCOL.to_owned(),
            packet_header_len: HEADER_LEN,
            effective_mtu,
            preferred_path: preferred_path.wire_name().to_owned(),
            supports_quic_datagrams: false,
        }
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
    UnsupportedWireVersion,
    UnsupportedPacketProtocol,
    UnsupportedPacketHeaderLength,
    InvalidEffectiveMtu,
    UnsupportedPreferredPath,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PeerCapabilities {
    peers: HashMap<PeerId, ControlCapabilities>,
}

impl PeerCapabilities {
    pub fn record(&mut self, peer: PeerId, capabilities: ControlCapabilities) {
        self.peers.insert(peer, capabilities);
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
) -> Option<ControlRejectionReason> {
    if capabilities.network_name != expected_network {
        return Some(ControlRejectionReason::WrongNetwork);
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

    None
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
        let request = ControlRequest::Capabilities(ControlCapabilities::local("lab", 1280));
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
        let capabilities = ControlCapabilities::local("lab", 1420);

        assert_eq!(capabilities.network_name, "lab");
        assert_eq!(capabilities.wire_version, WIRE_VERSION);
        assert_eq!(capabilities.packet_protocol, PACKET_PROTOCOL);
        assert_eq!(capabilities.packet_header_len, HEADER_LEN);
        assert_eq!(capabilities.effective_mtu, 1420);
        assert_eq!(capabilities.preferred_path, "direct_quic_stream");
        assert!(!capabilities.supports_quic_datagrams);
    }

    #[test]
    fn peer_capabilities_bound_effective_peer_mtu() {
        let peer = PeerId::from_bytes([1; 32]);
        let mut capabilities = PeerCapabilities::default();

        assert_eq!(capabilities.effective_mtu_for(peer, 1280), 1280);

        capabilities.record(peer, ControlCapabilities::local("lab", 1200));
        assert_eq!(capabilities.effective_mtu_for(peer, 1280), 1200);

        capabilities.record(peer, ControlCapabilities::local("lab", 1420));
        assert_eq!(capabilities.effective_mtu_for(peer, 1280), 1280);
    }

    #[test]
    fn peer_capabilities_report_quic_datagram_support() {
        let peer = PeerId::from_bytes([1; 32]);
        let mut capabilities = PeerCapabilities::default();

        assert!(!capabilities.supports_quic_datagrams_for(peer));

        let mut peer_capabilities = ControlCapabilities::local("lab", 1280);
        peer_capabilities.supports_quic_datagrams = true;
        capabilities.record(peer, peer_capabilities);

        assert!(capabilities.supports_quic_datagrams_for(peer));
    }

    #[test]
    fn capability_validation_rejects_incompatible_protocol_surfaces() {
        let mut capabilities = ControlCapabilities::local("lab", 1280);
        assert_eq!(validate_capabilities(&capabilities, "lab"), None);

        assert_eq!(
            validate_capabilities(&capabilities, "prod"),
            Some(ControlRejectionReason::WrongNetwork)
        );

        capabilities.wire_version = WIRE_VERSION.saturating_add(1);
        assert_eq!(
            validate_capabilities(&capabilities, "lab"),
            Some(ControlRejectionReason::UnsupportedWireVersion)
        );

        capabilities = ControlCapabilities::local("lab", 1280);
        capabilities.packet_protocol = "/different/packet/1".to_owned();
        assert_eq!(
            validate_capabilities(&capabilities, "lab"),
            Some(ControlRejectionReason::UnsupportedPacketProtocol)
        );

        capabilities = ControlCapabilities::local("lab", 1280);
        capabilities.packet_header_len = HEADER_LEN + 1;
        assert_eq!(
            validate_capabilities(&capabilities, "lab"),
            Some(ControlRejectionReason::UnsupportedPacketHeaderLength)
        );

        capabilities = ControlCapabilities::local("lab", 0);
        assert_eq!(
            validate_capabilities(&capabilities, "lab"),
            Some(ControlRejectionReason::InvalidEffectiveMtu)
        );

        capabilities = ControlCapabilities::local("lab", 1280);
        capabilities.preferred_path = "not_a_path".to_owned();
        assert_eq!(
            validate_capabilities(&capabilities, "lab"),
            Some(ControlRejectionReason::UnsupportedPreferredPath)
        );

        capabilities = ControlCapabilities::local("lab", 1280);
        capabilities.preferred_path = PathKind::DirectQuicDatagram.wire_name().to_owned();
        capabilities.supports_quic_datagrams = false;
        assert_eq!(
            validate_capabilities(&capabilities, "lab"),
            Some(ControlRejectionReason::UnsupportedPreferredPath)
        );

        capabilities.supports_quic_datagrams = true;
        assert_eq!(validate_capabilities(&capabilities, "lab"), None);
    }
}
