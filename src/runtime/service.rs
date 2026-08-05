use std::{io, time::Duration};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{StreamProtocol, request_response};
use serde::{Deserialize, Serialize};

use crate::{
    runtime::control::{ControlCapabilities, membership_tag_matches},
    runtime::packet::PACKET_PROTOCOL,
    runtime::packet_plane::{PACKET_PLANE_DATAGRAM_OVERHEAD_LEN, PACKET_PLANE_MAX_PAYLOAD_LEN},
    wire::{HEADER_LEN, MAX_PAYLOAD_LEN, WIRE_VERSION},
};

pub const SERVICE_PROTOCOL: &str = "/p2p-vpn/service/1";
const MAX_SERVICE_MESSAGE_LEN: usize = 4096;

#[derive(Clone, Debug, Default)]
pub struct ServiceCodec;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceStatusRequest {
    pub network_name: String,
    #[serde(default)]
    pub membership_tag: Option<String>,
    pub nonce: u64,
}

impl ServiceStatusRequest {
    #[must_use]
    pub fn local(network_name: &str, membership_tag: Option<String>, nonce: u64) -> Self {
        Self {
            network_name: network_name.to_owned(),
            membership_tag,
            nonce,
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ServiceStatusResponse {
    pub network_name: String,
    #[serde(default)]
    pub membership_tag: Option<String>,
    pub nonce: u64,
    pub wire_version: u8,
    pub packet_protocol: String,
    pub packet_header_len: usize,
    #[serde(default)]
    pub max_packet_payload_len: Option<usize>,
    #[serde(default)]
    pub packet_plane_datagram_overhead_len: Option<usize>,
    #[serde(default)]
    pub packet_plane_max_payload_len: Option<usize>,
    pub effective_mtu: u16,
    pub supports_quic_datagrams: bool,
    #[serde(default)]
    pub supports_native_quic_datagrams: bool,
    #[serde(default)]
    pub supports_owned_udp_packet_plane: bool,
    #[serde(default)]
    pub supports_owned_quic_packet_plane: bool,
    #[serde(default)]
    pub packet_plane_session_ttl_seconds: Option<u64>,
    #[serde(default)]
    pub packet_plane_replay_windows_per_session: Option<usize>,
    #[serde(default)]
    pub selected_path: Option<String>,
    #[serde(default)]
    pub selected_path_score: Option<i32>,
    #[serde(default)]
    pub selected_path_mtu: Option<u16>,
    #[serde(default)]
    pub selected_path_rtt_ms: Option<u16>,
}

impl ServiceStatusResponse {
    #[must_use]
    pub fn local(
        network_name: &str,
        membership_tag: Option<String>,
        nonce: u64,
        effective_mtu: u16,
    ) -> Self {
        Self {
            network_name: network_name.to_owned(),
            membership_tag,
            nonce,
            wire_version: WIRE_VERSION,
            packet_protocol: PACKET_PROTOCOL.to_owned(),
            packet_header_len: HEADER_LEN,
            max_packet_payload_len: Some(MAX_PAYLOAD_LEN),
            packet_plane_datagram_overhead_len: Some(PACKET_PLANE_DATAGRAM_OVERHEAD_LEN),
            packet_plane_max_payload_len: Some(PACKET_PLANE_MAX_PAYLOAD_LEN),
            effective_mtu,
            supports_quic_datagrams: false,
            supports_native_quic_datagrams: false,
            supports_owned_udp_packet_plane: false,
            supports_owned_quic_packet_plane: false,
            packet_plane_session_ttl_seconds: None,
            packet_plane_replay_windows_per_session: None,
            selected_path: None,
            selected_path_score: None,
            selected_path_mtu: None,
            selected_path_rtt_ms: None,
        }
    }

    #[must_use]
    pub fn with_packet_data_plane_capabilities(
        mut self,
        capabilities: &ControlCapabilities,
    ) -> Self {
        self.supports_native_quic_datagrams = capabilities.supports_native_quic_datagrams;
        self.supports_owned_udp_packet_plane = capabilities.supports_owned_udp_packet_plane;
        self.supports_owned_quic_packet_plane = capabilities.supports_owned_quic_packet_plane;
        self.supports_quic_datagrams = capabilities.supports_quic_datagrams
            || capabilities.supports_native_quic_datagrams
            || capabilities.supports_owned_udp_packet_plane
            || capabilities.supports_owned_quic_packet_plane;
        self
    }

    #[must_use]
    pub const fn with_packet_plane_session_ttl_seconds(mut self, session_ttl_seconds: u64) -> Self {
        self.packet_plane_session_ttl_seconds = Some(session_ttl_seconds);
        self
    }

    #[must_use]
    pub const fn with_packet_plane_replay_windows_per_session(
        mut self,
        replay_windows_per_session: usize,
    ) -> Self {
        self.packet_plane_replay_windows_per_session = Some(replay_windows_per_session);
        self
    }

    #[must_use]
    pub fn with_selected_path(
        mut self,
        path: String,
        score: i32,
        mtu: u16,
        rtt_ms: Option<u16>,
    ) -> Self {
        self.selected_path = Some(path);
        self.selected_path_score = Some(score);
        self.selected_path_mtu = Some(mtu);
        self.selected_path_rtt_ms = rtt_ms;
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ServiceRequest {
    Status(ServiceStatusRequest),
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ServiceResponse {
    Status(ServiceStatusResponse),
    Rejected(ServiceRejectionReason),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ServiceRejectionReason {
    UnauthorizedPeer,
    WrongNetwork,
    MembershipMismatch,
}

#[must_use]
pub fn validate_status_request(
    request: &ServiceStatusRequest,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) -> Option<ServiceRejectionReason> {
    if request.network_name != expected_network {
        return Some(ServiceRejectionReason::WrongNetwork);
    }
    if !membership_tag_matches(
        request.membership_tag.as_deref(),
        expected_membership_tag,
        previous_membership_tags,
    ) {
        return Some(ServiceRejectionReason::MembershipMismatch);
    }

    None
}

#[must_use]
pub fn validate_status_response(
    response: &ServiceStatusResponse,
    expected_network: &str,
    expected_membership_tag: Option<&str>,
    previous_membership_tags: &[String],
) -> Option<ServiceRejectionReason> {
    if response.network_name != expected_network {
        return Some(ServiceRejectionReason::WrongNetwork);
    }
    if !membership_tag_matches(
        response.membership_tag.as_deref(),
        expected_membership_tag,
        previous_membership_tags,
    ) {
        return Some(ServiceRejectionReason::MembershipMismatch);
    }

    None
}

#[async_trait]
impl request_response::Codec for ServiceCodec {
    type Protocol = StreamProtocol;
    type Request = ServiceRequest;
    type Response = ServiceResponse;

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
pub fn behaviour(max_concurrent_streams: usize) -> request_response::Behaviour<ServiceCodec> {
    let protocols = [(
        StreamProtocol::new(SERVICE_PROTOCOL),
        request_response::ProtocolSupport::Full,
    )];
    let config = request_response::Config::default().with_request_timeout(Duration::from_secs(10));

    request_response::Behaviour::with_codec(
        ServiceCodec,
        protocols,
        config.with_max_concurrent_streams(max_concurrent_streams.max(1)),
    )
}

async fn read_json_message<T, M>(io: &mut T) -> io::Result<M>
where
    T: AsyncRead + Unpin + Send,
    M: for<'de> Deserialize<'de>,
{
    let mut length_bytes = [0; 2];
    io.read_exact(&mut length_bytes).await?;
    let length = usize::from(u16::from_be_bytes(length_bytes));
    if length > MAX_SERVICE_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("service message length {length} exceeds limit {MAX_SERVICE_MESSAGE_LEN}"),
        ));
    }

    let mut payload = vec![0; length];
    io.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

async fn write_json_message<T, M>(io: &mut T, message: &M) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    M: Serialize,
{
    let payload = serde_json::to_vec(message)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if payload.len() > MAX_SERVICE_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "service message length {} exceeds limit {MAX_SERVICE_MESSAGE_LEN}",
                payload.len()
            ),
        ));
    }
    let length = u16::try_from(payload.len())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    io.write_all(&length.to_be_bytes()).await?;
    io.write_all(&payload).await?;
    io.close().await
}

#[cfg(test)]
mod tests {
    use futures::io::Cursor;

    use super::*;

    #[tokio::test]
    async fn service_codec_round_trips_status_request() {
        let mut codec = ServiceCodec;
        let protocol = StreamProtocol::new(SERVICE_PROTOCOL);
        let request = ServiceRequest::Status(ServiceStatusRequest::local(
            "lab",
            Some("member".to_owned()),
            42,
        ));
        let mut written = Cursor::new(Vec::new());

        request_response::Codec::write_request(
            &mut codec,
            &protocol,
            &mut written,
            request.clone(),
        )
        .await
        .expect("write");

        written.set_position(0);
        let decoded = request_response::Codec::read_request(&mut codec, &protocol, &mut written)
            .await
            .expect("read");

        assert_eq!(decoded, request);
    }

    #[tokio::test]
    async fn service_codec_round_trips_rejection_response() {
        let mut codec = ServiceCodec;
        let protocol = StreamProtocol::new(SERVICE_PROTOCOL);
        let response = ServiceResponse::Rejected(ServiceRejectionReason::WrongNetwork);
        let mut written = Cursor::new(Vec::new());

        request_response::Codec::write_response(
            &mut codec,
            &protocol,
            &mut written,
            response.clone(),
        )
        .await
        .expect("write");

        written.set_position(0);
        let decoded = request_response::Codec::read_response(&mut codec, &protocol, &mut written)
            .await
            .expect("read");

        assert_eq!(decoded, response);
    }

    #[tokio::test]
    async fn service_codec_round_trips_status_response_with_packet_plane_ttl() {
        let mut codec = ServiceCodec;
        let protocol = StreamProtocol::new(SERVICE_PROTOCOL);
        let response = ServiceResponse::Status(
            ServiceStatusResponse::local("lab", Some("member".to_owned()), 42, 1280)
                .with_packet_plane_session_ttl_seconds(123)
                .with_packet_plane_replay_windows_per_session(456)
                .with_selected_path("direct_quic_datagram".to_owned(), 97, 1200, Some(31)),
        );
        let mut written = Cursor::new(Vec::new());

        request_response::Codec::write_response(
            &mut codec,
            &protocol,
            &mut written,
            response.clone(),
        )
        .await
        .expect("write");

        written.set_position(0);
        let decoded = request_response::Codec::read_response(&mut codec, &protocol, &mut written)
            .await
            .expect("read");

        assert_eq!(decoded, response);
    }

    #[test]
    fn status_response_decodes_missing_packet_plane_limits_as_unknown() {
        let decoded = serde_json::from_str::<ServiceStatusResponse>(
            r#"{"network_name":"lab","membership_tag":null,"nonce":42,"wire_version":1,"packet_protocol":"/p2p-vpn/packet/1","packet_header_len":25,"effective_mtu":1280,"supports_quic_datagrams":false}"#,
        )
        .expect("status response");

        assert_eq!(decoded.packet_plane_session_ttl_seconds, None);
        assert_eq!(decoded.packet_plane_replay_windows_per_session, None);
        assert_eq!(decoded.max_packet_payload_len, None);
        assert_eq!(decoded.packet_plane_datagram_overhead_len, None);
        assert_eq!(decoded.packet_plane_max_payload_len, None);
        assert_eq!(decoded.selected_path, None);
        assert_eq!(decoded.selected_path_score, None);
        assert_eq!(decoded.selected_path_mtu, None);
        assert_eq!(decoded.selected_path_rtt_ms, None);
        assert!(!decoded.supports_native_quic_datagrams);
        assert!(!decoded.supports_owned_udp_packet_plane);
        assert!(!decoded.supports_owned_quic_packet_plane);
    }

    #[test]
    fn status_response_inherits_packet_data_plane_capabilities() {
        let native = ControlCapabilities::local("lab", None, 1280).with_native_quic_datagrams(true);
        let status = ServiceStatusResponse::local("lab", None, 42, 1280)
            .with_packet_data_plane_capabilities(&native);
        assert!(status.supports_quic_datagrams);
        assert!(status.supports_native_quic_datagrams);
        assert!(!status.supports_owned_udp_packet_plane);

        let owned = ControlCapabilities::local("lab", None, 1280).with_owned_udp_packet_plane(true);
        let status = ServiceStatusResponse::local("lab", None, 42, 1280)
            .with_packet_data_plane_capabilities(&owned);
        assert!(status.supports_quic_datagrams);
        assert!(!status.supports_native_quic_datagrams);
        assert!(status.supports_owned_udp_packet_plane);
        assert!(!status.supports_owned_quic_packet_plane);

        let owned_quic = ControlCapabilities::local("lab", None, 1280)
            .with_owned_quic_packet_plane_certificate(vec![0x30, 0x01]);
        let status = ServiceStatusResponse::local("lab", None, 42, 1280)
            .with_packet_data_plane_capabilities(&owned_quic);
        assert!(status.supports_quic_datagrams);
        assert!(!status.supports_native_quic_datagrams);
        assert!(!status.supports_owned_udp_packet_plane);
        assert!(status.supports_owned_quic_packet_plane);
    }

    #[tokio::test]
    async fn service_codec_rejects_oversized_messages() {
        let mut codec = ServiceCodec;
        let protocol = StreamProtocol::new(SERVICE_PROTOCOL);
        let length = u16::try_from(MAX_SERVICE_MESSAGE_LEN + 1).expect("test length fits");
        let mut input = Cursor::new(length.to_be_bytes().to_vec());

        let error = request_response::Codec::read_request(&mut codec, &protocol, &mut input)
            .await
            .expect_err("oversized request should be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn status_request_validation_rejects_wrong_overlay_scope() {
        let mut request = ServiceStatusRequest::local("other", None, 1);
        assert_eq!(
            validate_status_request(&request, "lab", None, &[]),
            Some(ServiceRejectionReason::WrongNetwork)
        );

        request.network_name = "lab".to_owned();
        request.membership_tag = Some("wrong".to_owned());
        assert_eq!(
            validate_status_request(&request, "lab", Some("expected"), &[]),
            Some(ServiceRejectionReason::MembershipMismatch)
        );
    }

    #[test]
    fn status_response_validation_rejects_wrong_overlay_scope() {
        let mut response = ServiceStatusResponse::local("other", None, 1, 1280);
        assert_eq!(
            validate_status_response(&response, "lab", None, &[]),
            Some(ServiceRejectionReason::WrongNetwork)
        );

        response.network_name = "lab".to_owned();
        response.membership_tag = Some("wrong".to_owned());
        assert_eq!(
            validate_status_response(&response, "lab", Some("expected"), &[]),
            Some(ServiceRejectionReason::MembershipMismatch)
        );
    }

    #[test]
    fn status_validation_accepts_previous_membership_tag() {
        let request = ServiceStatusRequest::local("lab", Some("previous".to_owned()), 1);
        let response = ServiceStatusResponse::local("lab", Some("previous".to_owned()), 1, 1280);
        let previous = [String::from("previous")];

        assert_eq!(
            validate_status_request(&request, "lab", Some("current"), &previous),
            None
        );
        assert_eq!(
            validate_status_response(&response, "lab", Some("current"), &previous),
            None
        );
    }
}
