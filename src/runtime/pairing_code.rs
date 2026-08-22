use std::{io, time::Duration};

use async_trait::async_trait;
use futures::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use libp2p::{StreamProtocol, request_response};
use serde::{Deserialize, Serialize};

use crate::{
    pairing::{PairingRequest, PairingResponse},
    pairing_code::{PairingCodeChallenge, PairingCodeHello},
};

pub const PAIRING_CODE_PROTOCOL: &str = "/p2p-vpn/pairing-code/1";
const MAX_PAIRING_CODE_MESSAGE_LEN: usize = 64 * 1024;

#[derive(Clone, Debug, Default)]
pub struct PairingCodeCodec;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingCodeRequest {
    Hello { hello: Box<PairingCodeHello> },
    Grant { request: Box<PairingRequest> },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingCodeRejectionReason {
    Unavailable,
    InvalidRequest,
    RateLimited,
    Busy,
    UserRejected,
    Expired,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PairingCodeResponse {
    Challenge {
        challenge: Box<PairingCodeChallenge>,
    },
    Accepted {
        response: Box<PairingResponse>,
    },
    Rejected {
        reason: PairingCodeRejectionReason,
    },
}

#[async_trait]
impl request_response::Codec for PairingCodeCodec {
    type Protocol = StreamProtocol;
    type Request = PairingCodeRequest;
    type Response = PairingCodeResponse;

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
pub fn behaviour(max_concurrent_streams: usize) -> request_response::Behaviour<PairingCodeCodec> {
    let protocols = [(
        StreamProtocol::new(PAIRING_CODE_PROTOCOL),
        request_response::ProtocolSupport::Full,
    )];
    let config = request_response::Config::default().with_request_timeout(Duration::from_secs(10));

    request_response::Behaviour::with_codec(
        PairingCodeCodec,
        protocols,
        config.with_max_concurrent_streams(max_concurrent_streams.max(1)),
    )
}

async fn read_json_message<T, M>(io: &mut T) -> io::Result<M>
where
    T: AsyncRead + Unpin + Send,
    M: for<'de> Deserialize<'de>,
{
    let mut length = [0; 4];
    io.read_exact(&mut length).await?;
    let length = usize::try_from(u32::from_be_bytes(length)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "pairing code message length does not fit this platform",
        )
    })?;
    if length > MAX_PAIRING_CODE_MESSAGE_LEN {
        return Err(message_too_large(length));
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
    if payload.len() > MAX_PAIRING_CODE_MESSAGE_LEN {
        return Err(message_too_large(payload.len()));
    }
    let length = u32::try_from(payload.len()).map_err(|_| message_too_large(payload.len()))?;
    io.write_all(&length.to_be_bytes()).await?;
    io.write_all(&payload).await?;
    io.close().await
}

fn message_too_large(length: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "pairing code message length {length} exceeds limit {MAX_PAIRING_CODE_MESSAGE_LEN}"
        ),
    )
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use futures::io::Cursor;

    use crate::pairing_code::{PairingCodeChallengePayload, PairingCodeHelloPayload};

    use super::*;

    fn hello() -> PairingCodeHello {
        PairingCodeHello {
            payload: PairingCodeHelloPayload {
                version: 1,
                network_name: "runners".to_owned(),
                locator: "locator".to_owned(),
                inviter_peer: "inviter".to_owned(),
                joiner_peer: "joiner".to_owned(),
                joiner_public_key: "public-key".to_owned(),
                issued_at_unix_seconds: 1_000,
                spake_message: "spake-a".to_owned(),
            },
            signature: "signature-a".to_owned(),
        }
    }

    fn challenge() -> PairingCodeChallenge {
        PairingCodeChallenge {
            payload: PairingCodeChallengePayload {
                version: 1,
                network_name: "runners".to_owned(),
                locator: "locator".to_owned(),
                inviter_peer: "inviter".to_owned(),
                inviter_public_key: "public-key".to_owned(),
                joiner_peer: "joiner".to_owned(),
                issued_at_unix_seconds: 1_001,
                expires_at_unix_seconds: 1_601,
                spake_message: "spake-b".to_owned(),
                nonce: "nonce".to_owned(),
                encrypted_offer: "ciphertext".to_owned(),
            },
            signature: "signature-b".to_owned(),
        }
    }

    #[tokio::test]
    async fn pairing_code_codec_round_trips_messages() {
        let protocol = StreamProtocol::new(PAIRING_CODE_PROTOCOL);
        let request = PairingCodeRequest::Hello {
            hello: Box::new(hello()),
        };
        let response = PairingCodeResponse::Challenge {
            challenge: Box::new(challenge()),
        };
        let mut codec = PairingCodeCodec;
        let mut request_io = Cursor::new(Vec::new());
        let mut response_io = Cursor::new(Vec::new());

        request_response::Codec::write_request(
            &mut codec,
            &protocol,
            &mut request_io,
            request.clone(),
        )
        .await
        .expect("request write");
        request_response::Codec::write_response(
            &mut codec,
            &protocol,
            &mut response_io,
            response.clone(),
        )
        .await
        .expect("response write");
        request_io.set_position(0);
        response_io.set_position(0);

        assert_eq!(
            request_response::Codec::read_request(&mut codec, &protocol, &mut request_io)
                .await
                .expect("request read"),
            request
        );
        assert_eq!(
            request_response::Codec::read_response(&mut codec, &protocol, &mut response_io)
                .await
                .expect("response read"),
            response
        );
    }

    #[tokio::test]
    async fn pairing_code_codec_rejects_oversized_messages() {
        let mut codec = PairingCodeCodec;
        let protocol = StreamProtocol::new(PAIRING_CODE_PROTOCOL);
        let length = u32::try_from(MAX_PAIRING_CODE_MESSAGE_LEN + 1).expect("test length");
        let mut input = Cursor::new(length.to_be_bytes().to_vec());

        let error = request_response::Codec::read_request(&mut codec, &protocol, &mut input)
            .await
            .expect_err("oversized request");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
