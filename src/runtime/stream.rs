use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::wire::{Frame, HEADER_LEN, Header, MAX_PAYLOAD_LEN};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketStreamConfig {
    pub max_payload_len: usize,
}

impl Default for PacketStreamConfig {
    fn default() -> Self {
        Self {
            max_payload_len: MAX_PAYLOAD_LEN,
        }
    }
}

pub async fn write_frame<W>(writer: &mut W, frame: &Frame) -> Result<(), PacketStreamError>
where
    W: AsyncWrite + Unpin,
{
    let payload_len = frame.payload.len();
    if payload_len != usize::from(frame.header.payload_len) {
        return Err(PacketStreamError::LengthMismatch {
            header_len: frame.header.payload_len,
            payload_len,
        });
    }

    writer.write_all(&frame.header.encode()).await?;
    writer.write_all(&frame.payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R>(
    reader: &mut R,
    config: PacketStreamConfig,
) -> Result<Frame, PacketStreamError>
where
    R: AsyncRead + Unpin,
{
    let mut header_bytes = [0; HEADER_LEN];
    reader.read_exact(&mut header_bytes).await?;
    let header = Header::decode(&header_bytes)?;
    let payload_len = usize::from(header.payload_len);

    if payload_len > config.max_payload_len {
        return Err(PacketStreamError::PayloadTooLarge {
            actual: payload_len,
            max: config.max_payload_len,
        });
    }

    let mut payload = vec![0; payload_len];
    reader.read_exact(&mut payload).await?;
    Ok(Frame { header, payload })
}

#[derive(Debug)]
pub enum PacketStreamError {
    Io(std::io::Error),
    Decode(crate::wire::DecodeError),
    PayloadTooLarge { actual: usize, max: usize },
    LengthMismatch { header_len: u16, payload_len: usize },
}

impl From<std::io::Error> for PacketStreamError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::wire::DecodeError> for PacketStreamError {
    fn from(error: crate::wire::DecodeError) -> Self {
        Self::Decode(error)
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncWriteExt as _, duplex};

    use crate::wire::{DecodeError, PayloadType};

    use super::*;

    #[tokio::test]
    async fn frame_round_trips_over_async_stream() {
        let (mut client, mut server) = duplex(128);
        let frame = Frame::packet(7, 42, vec![0x45, 0, 0, 20]).expect("frame");

        write_frame(&mut client, &frame)
            .await
            .expect("write should succeed");
        let received = read_frame(&mut server, PacketStreamConfig::default())
            .await
            .expect("read should succeed");

        assert_eq!(received, frame);
    }

    #[tokio::test]
    async fn read_rejects_unknown_payload_type() {
        let (mut client, mut server) = duplex(128);
        let mut header = Header::new(PayloadType::IpPacket, 1, 1, 0).encode();
        header[2] = 255;

        client.write_all(&header).await.expect("write header");

        assert!(matches!(
            read_frame(&mut server, PacketStreamConfig::default()).await,
            Err(PacketStreamError::Decode(DecodeError::UnknownPayloadType(
                255
            )))
        ));
    }

    #[tokio::test]
    async fn read_rejects_payload_above_configured_limit() {
        let (mut client, mut server) = duplex(128);
        let header = Header::new(PayloadType::IpPacket, 1, 1, 10).encode();

        client.write_all(&header).await.expect("write header");

        assert!(matches!(
            read_frame(&mut server, PacketStreamConfig { max_payload_len: 4 }).await,
            Err(PacketStreamError::PayloadTooLarge { actual: 10, max: 4 })
        ));
    }

    #[tokio::test]
    async fn write_rejects_length_mismatch() {
        let (mut client, _server) = duplex(128);
        let mut frame = Frame::packet(1, 1, vec![1, 2, 3]).expect("frame");
        frame.header.payload_len = 2;

        assert!(matches!(
            write_frame(&mut client, &frame).await,
            Err(PacketStreamError::LengthMismatch {
                header_len: 2,
                payload_len: 3
            })
        ));
    }
}
