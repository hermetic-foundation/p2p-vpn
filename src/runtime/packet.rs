use std::{collections::HashSet, io, time::Duration};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{PeerId as Libp2pPeerId, StreamProtocol, request_response};

use crate::{
    config::Config,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketResponse {
    Accepted,
}

impl PacketResponse {
    const ACCEPTED_BYTE: u8 = 1;
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
        let mut response = [0];
        io.read_exact(&mut response).await?;
        match response[0] {
            PacketResponse::ACCEPTED_BYTE => Ok(PacketResponse::Accepted),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown packet response {other}"),
            )),
        }
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
        io.write_all(&request.encode()).await?;
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
        match response {
            PacketResponse::Accepted => io.write_all(&[PacketResponse::ACCEPTED_BYTE]).await?,
        }
        io.close().await
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

    #[must_use]
    pub fn allows(&self, peer: &Libp2pPeerId) -> bool {
        self.peers.contains(peer)
    }
}

#[must_use]
pub fn behaviour(mtu: u16) -> request_response::Behaviour<PacketCodec> {
    let max_payload_len = usize::from(mtu).min(MAX_PAYLOAD_LEN);
    let protocols = [(
        StreamProtocol::new(PACKET_PROTOCOL),
        request_response::ProtocolSupport::Full,
    )];
    let config = request_response::Config::default().with_request_timeout(Duration::from_secs(10));

    request_response::Behaviour::with_codec(
        PacketCodec::new(max_payload_len),
        protocols,
        config.with_max_concurrent_streams(256),
    )
}

async fn read_futures_frame<R>(reader: &mut R, max_payload_len: usize) -> io::Result<Frame>
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

fn invalid_data(error: crate::wire::DecodeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}"))
}

#[cfg(test)]
mod tests {
    use futures::io::Cursor;
    use libp2p::identity::Keypair;

    use crate::{
        config::{Config, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig},
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

    #[test]
    fn authorized_peers_are_derived_from_libp2p_peer_ids() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let other = Keypair::generate_ed25519().public().to_peer_id();
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: Keypair::generate_ed25519()
                    .public()
                    .to_peer_id()
                    .to_string(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: Some("remote".to_owned()),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
            },
        };

        let authorized = AuthorizedPeers::from_config(&config);

        assert!(authorized.allows(&remote));
        assert!(!authorized.allows(&other));
    }
}
