use std::{collections::HashSet, io, time::Duration};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{PeerId as Libp2pPeerId, StreamProtocol, request_response};

use crate::{
    config::{Config, ConfigError},
    wire::{Frame, HEADER_LEN, Header, MAX_PAYLOAD_LEN},
};

pub const PACKET_PROTOCOL: &str = "/p2p-vpn/packet/1";

#[derive(Clone, Debug, Default)]
pub struct PacketCodec {
    max_payload_len: usize,
}

impl PacketCodec {
    #[must_use]
    pub const fn new(max_payload_len: usize) -> Self {
        Self { max_payload_len }
    }

    #[must_use]
    pub const fn max_payload_len(&self) -> usize {
        self.max_payload_len
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketResponse {
    Accepted,
    Rejected(PacketRejectionReason),
}

impl PacketResponse {
    const ACCEPTED_BYTE: u8 = 1;
    const REJECTED_BYTE: u8 = 2;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketRejectionReason {
    MalformedPacket,
    PacketTooLarge,
    Replay,
    UnauthorizedPeer,
    UnauthorizedSource,
    UnauthorizedDestination,
    UnexpectedPayload,
    RateLimited,
}

impl PacketRejectionReason {
    const MALFORMED_PACKET_BYTE: u8 = 1;
    const PACKET_TOO_LARGE_BYTE: u8 = 2;
    const REPLAY_BYTE: u8 = 3;
    const UNAUTHORIZED_PEER_BYTE: u8 = 4;
    const UNAUTHORIZED_SOURCE_BYTE: u8 = 5;
    const UNAUTHORIZED_DESTINATION_BYTE: u8 = 6;
    const UNEXPECTED_PAYLOAD_BYTE: u8 = 7;
    const RATE_LIMITED_BYTE: u8 = 8;

    fn encode(self) -> u8 {
        match self {
            Self::MalformedPacket => Self::MALFORMED_PACKET_BYTE,
            Self::PacketTooLarge => Self::PACKET_TOO_LARGE_BYTE,
            Self::Replay => Self::REPLAY_BYTE,
            Self::UnauthorizedPeer => Self::UNAUTHORIZED_PEER_BYTE,
            Self::UnauthorizedSource => Self::UNAUTHORIZED_SOURCE_BYTE,
            Self::UnauthorizedDestination => Self::UNAUTHORIZED_DESTINATION_BYTE,
            Self::UnexpectedPayload => Self::UNEXPECTED_PAYLOAD_BYTE,
            Self::RateLimited => Self::RATE_LIMITED_BYTE,
        }
    }

    fn decode(byte: u8) -> io::Result<Self> {
        match byte {
            Self::MALFORMED_PACKET_BYTE => Ok(Self::MalformedPacket),
            Self::PACKET_TOO_LARGE_BYTE => Ok(Self::PacketTooLarge),
            Self::REPLAY_BYTE => Ok(Self::Replay),
            Self::UNAUTHORIZED_PEER_BYTE => Ok(Self::UnauthorizedPeer),
            Self::UNAUTHORIZED_SOURCE_BYTE => Ok(Self::UnauthorizedSource),
            Self::UNAUTHORIZED_DESTINATION_BYTE => Ok(Self::UnauthorizedDestination),
            Self::UNEXPECTED_PAYLOAD_BYTE => Ok(Self::UnexpectedPayload),
            Self::RATE_LIMITED_BYTE => Ok(Self::RateLimited),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown packet rejection reason {other}"),
            )),
        }
    }
}

#[async_trait]
impl request_response::Codec for PacketCodec {
    type Protocol = StreamProtocol;
    type Request = Frame;
    type Response = PacketResponse;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_futures_frame(io, self.max_payload_len).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        read_futures_response(io).await
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
        write_futures_frame(io, &request).await?;
        io.close().await
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
        write_futures_response(io, response).await
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AuthorizedPeers {
    peers: HashSet<Libp2pPeerId>,
}

impl AuthorizedPeers {
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let peers = config
            .peers
            .iter()
            .filter_map(|peer| peer.id.parse::<Libp2pPeerId>().ok())
            .collect();
        Self { peers }
    }

    pub fn try_from_config(config: &Config) -> Result<Self, ConfigError> {
        let mut authorized = Self::from_config(config);
        for member in config.effective_membership()?.overlay_members() {
            authorized.peers.insert(member.transport_peer);
        }
        Ok(authorized)
    }

    #[must_use]
    pub fn allows(&self, peer: &Libp2pPeerId) -> bool {
        self.peers.contains(peer)
    }

    pub fn insert(&mut self, peer: Libp2pPeerId) {
        self.peers.insert(peer);
    }
}

#[must_use]
pub fn behaviour(
    mtu: u16,
    max_concurrent_streams: usize,
) -> request_response::Behaviour<PacketCodec> {
    let max_payload_len = usize::from(mtu).min(MAX_PAYLOAD_LEN);
    let protocols = [(
        StreamProtocol::new(PACKET_PROTOCOL),
        request_response::ProtocolSupport::Full,
    )];
    let config = request_response::Config::default().with_request_timeout(Duration::from_secs(10));

    request_response::Behaviour::with_codec(
        PacketCodec::new(max_payload_len),
        protocols,
        config.with_max_concurrent_streams(max_concurrent_streams.max(1)),
    )
}

pub(crate) async fn read_futures_frame<R>(
    reader: &mut R,
    max_payload_len: usize,
) -> io::Result<Frame>
where
    R: AsyncRead + Unpin + Send,
{
    let mut header_bytes = [0; HEADER_LEN];
    reader.read_exact(&mut header_bytes).await?;
    let header = Header::decode(&header_bytes).map_err(invalid_data)?;
    let payload_len = usize::from(header.payload_len);

    if payload_len > max_payload_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("payload length {payload_len} exceeds limit {max_payload_len}"),
        ));
    }

    let mut payload = vec![0; payload_len];
    reader.read_exact(&mut payload).await?;
    Ok(Frame { header, payload })
}

pub(crate) async fn write_futures_frame<W>(writer: &mut W, frame: &Frame) -> io::Result<()>
where
    W: AsyncWrite + Unpin + Send,
{
    writer.write_all(&frame.encode()).await
}

pub(crate) async fn read_futures_response<R>(reader: &mut R) -> io::Result<PacketResponse>
where
    R: AsyncRead + Unpin + Send,
{
    let mut status = [0];
    reader.read_exact(&mut status).await?;
    match status[0] {
        PacketResponse::ACCEPTED_BYTE => Ok(PacketResponse::Accepted),
        PacketResponse::REJECTED_BYTE => {
            let mut reason = [0];
            reader.read_exact(&mut reason).await?;
            Ok(PacketResponse::Rejected(PacketRejectionReason::decode(
                reason[0],
            )?))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown packet response {other}"),
        )),
    }
}

pub(crate) async fn write_futures_response<W>(
    writer: &mut W,
    response: PacketResponse,
) -> io::Result<()>
where
    W: AsyncWrite + Unpin + Send,
{
    match response {
        PacketResponse::Accepted => writer.write_all(&[PacketResponse::ACCEPTED_BYTE]).await?,
        PacketResponse::Rejected(reason) => {
            writer
                .write_all(&[PacketResponse::REJECTED_BYTE, reason.encode()])
                .await?;
        }
    }
    writer.close().await
}

fn invalid_data(error: crate::wire::DecodeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}"))
}

#[cfg(test)]
mod tests {
    use futures::io::Cursor;
    use libp2p::identity::Keypair;

    use crate::{
        config::{Config, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig, ResourceConfig},
        identity::NodeIdentity,
        membership::{MembershipRecordOptions, MembershipRole, issue_membership_record_at},
        wire::PayloadType,
    };

    use super::*;

    #[tokio::test]
    async fn packet_codec_round_trips_frame() {
        let mut codec = PacketCodec::new(1280);
        let protocol = StreamProtocol::new(PACKET_PROTOCOL);
        let frame = Frame::packet(1, 2, vec![0x45, 0, 0, 20]).expect("frame");
        let mut written = Cursor::new(Vec::new());

        request_response::Codec::write_request(&mut codec, &protocol, &mut written, frame.clone())
            .await
            .expect("write");

        written.set_position(0);
        let decoded = request_response::Codec::read_request(&mut codec, &protocol, &mut written)
            .await
            .expect("read");

        assert_eq!(decoded, frame);
    }

    #[tokio::test]
    async fn packet_codec_rejects_oversized_request() {
        let mut codec = PacketCodec::new(4);
        let protocol = StreamProtocol::new(PACKET_PROTOCOL);
        let header = Header::new(PayloadType::IpPacket, 1, 2, 8).encode();
        let mut input = Cursor::new(header.to_vec());

        let error = request_response::Codec::read_request(&mut codec, &protocol, &mut input)
            .await
            .expect_err("oversized request should be invalid");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn packet_codec_round_trips_rejection_response() {
        for reason in [
            PacketRejectionReason::MalformedPacket,
            PacketRejectionReason::PacketTooLarge,
            PacketRejectionReason::Replay,
            PacketRejectionReason::UnauthorizedPeer,
            PacketRejectionReason::UnauthorizedSource,
            PacketRejectionReason::UnauthorizedDestination,
            PacketRejectionReason::UnexpectedPayload,
            PacketRejectionReason::RateLimited,
        ] {
            let mut codec = PacketCodec::new(1280);
            let protocol = StreamProtocol::new(PACKET_PROTOCOL);
            let response = PacketResponse::Rejected(reason);
            let mut written = Cursor::new(Vec::new());

            request_response::Codec::write_response(&mut codec, &protocol, &mut written, response)
                .await
                .expect("write");

            written.set_position(0);
            let decoded =
                request_response::Codec::read_response(&mut codec, &protocol, &mut written)
                    .await
                    .expect("read");

            assert_eq!(decoded, response);
        }
    }

    #[tokio::test]
    async fn packet_codec_rejects_unknown_rejection_reason() {
        let mut codec = PacketCodec::new(1280);
        let protocol = StreamProtocol::new(PACKET_PROTOCOL);
        let mut input = Cursor::new(vec![PacketResponse::REJECTED_BYTE, 99]);

        let error = request_response::Codec::read_response(&mut codec, &protocol, &mut input)
            .await
            .expect_err("unknown rejection reason should be invalid");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn packet_codec_reports_configured_payload_limit() {
        let codec = PacketCodec::new(1280);

        assert_eq!(codec.max_payload_len(), 1280);
    }

    #[test]
    fn authorized_peers_are_derived_from_libp2p_peer_ids() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let other = Keypair::generate_ed25519().public().to_peer_id();
        let config = Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: Keypair::generate_ed25519()
                    .public()
                    .to_peer_id()
                    .to_string(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: Some("remote".to_owned()),
                ip: None,
                vpn_ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };

        let authorized = AuthorizedPeers::from_config(&config);

        assert!(authorized.allows(&remote));
        assert!(!authorized.allows(&other));
    }

    #[test]
    fn authorized_peers_include_member_record_overlay_members() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let remote = member.peer_id.parse().expect("member peer");
        let other = Keypair::generate_ed25519().public().to_peer_id();
        let mut config = Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: Keypair::generate_ed25519()
                    .public()
                    .to_peer_id()
                    .to_string(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };
        config.network.member_records = vec![
            issue_membership_record_at(
                &issuer,
                MembershipRecordOptions {
                    network_name: "lab".to_owned(),
                    member,
                    membership_epoch: 1,
                    sequence: 1,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1_000,
            )
            .expect("member record"),
        ];

        let authorized = AuthorizedPeers::try_from_config(&config).expect("authorized peers");

        assert!(authorized.allows(&remote));
        assert!(!authorized.allows(&other));
    }
}
