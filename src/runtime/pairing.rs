use std::{io, time::Duration};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{StreamProtocol, request_response};
use serde::{Deserialize, Serialize};

use crate::pairing::{MAX_PAIRING_MESSAGE_LEN, PairingRequest, PairingResponse};

pub const PAIRING_PROTOCOL: &str = "/p2p-vpn/pairing/1";

#[derive(Clone, Debug, Default)]
pub struct PairingCodec;

#[async_trait]
impl request_response::Codec for PairingCodec {
    type Protocol = StreamProtocol;
    type Request = PairingRequest;
    type Response = PairingResponse;

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
pub fn behaviour(max_concurrent_streams: usize) -> request_response::Behaviour<PairingCodec> {
    let protocols = [(
        StreamProtocol::new(PAIRING_PROTOCOL),
        request_response::ProtocolSupport::Full,
    )];
    let config = request_response::Config::default().with_request_timeout(Duration::from_secs(10));

    request_response::Behaviour::with_codec(
        PairingCodec,
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
    if length > MAX_PAIRING_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pairing message length {length} exceeds limit {MAX_PAIRING_MESSAGE_LEN}"),
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
                "pairing message length {} exceeds encodable limit {}",
                payload.len(),
                u16::MAX
            ),
        )
    })?;
    if usize::from(length) > MAX_PAIRING_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "pairing message length {} exceeds limit {MAX_PAIRING_MESSAGE_LEN}",
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
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use futures::io::Cursor;
    use libp2p::StreamProtocol;

    use crate::{
        config::{
            Config, DiscoveryConfig, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig,
            RelayConfig, ResourceConfig,
        },
        identity::NodeIdentity,
        pairing::{
            PairingOfferOptions, PairingRequestOptions, PairingResponseOptions,
            build_pairing_request_at, build_pairing_response_at, export_pairing_offer_at,
        },
    };

    use super::*;

    fn config() -> Config {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: identity.peer_id,
                private_key: Some(identity.private_key),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.42.0.1".to_owned()),
                routes: Vec::new(),
                listen_addresses: vec!["/ip4/127.0.0.1/tcp/0".to_owned()],
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::<PeerConfig>::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        }
    }

    #[tokio::test]
    async fn pairing_codec_round_trips_request_and_response() {
        let inviter_config = config();
        let offer = export_pairing_offer_at(&inviter_config, PairingOfferOptions::default(), 1_000)
            .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let request = build_pairing_request_at(
            &offer,
            PairingRequestOptions {
                identity: joiner.clone(),
                requested_vpn_ip: Some("10.42.0.2".to_owned()),
                requested_routes: Vec::new(),
            },
            1_001,
        )
        .expect("request");
        let response = build_pairing_response_at(
            &inviter_config,
            &offer,
            PairingResponseOptions {
                joiner_peer: joiner.peer_id,
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                membership_key: Some(STANDARD.encode([9_u8; 32])),
                member_records: Vec::new(),
                expires_in_seconds: 300,
            },
            1_002,
        )
        .expect("response");
        let protocol = StreamProtocol::new(PAIRING_PROTOCOL);
        let mut codec = PairingCodec;
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
    async fn pairing_codec_rejects_oversized_messages() {
        let mut codec = PairingCodec;
        let protocol = StreamProtocol::new(PAIRING_PROTOCOL);
        let length = u16::try_from(MAX_PAIRING_MESSAGE_LEN + 1).expect("test length fits");
        let mut input = Cursor::new(length.to_be_bytes().to_vec());

        let error = request_response::Codec::read_request(&mut codec, &protocol, &mut input)
            .await
            .expect_err("oversized pairing message should fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
