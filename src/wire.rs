use crate::{Sequence, SessionId};

pub const WIRE_VERSION: u8 = 1;
pub const HEADER_LEN: usize = 17;
pub const MAX_PAYLOAD_LEN: usize = u16::MAX as usize;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PayloadType {
    IpPacket = 0,
    Keepalive = 1,
    PathProbe = 2,
}

impl TryFrom<u8> for PayloadType {
    type Error = DecodeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::IpPacket),
            1 => Ok(Self::Keepalive),
            2 => Ok(Self::PathProbe),
            other => Err(DecodeError::UnknownPayloadType(other)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    pub version: u8,
    pub flags: u8,
    pub payload_type: PayloadType,
    pub session_id: SessionId,
    pub sequence: Sequence,
    pub payload_len: u16,
}

impl Header {
    #[must_use]
    pub const fn new(
        payload_type: PayloadType,
        session_id: SessionId,
        sequence: Sequence,
        payload_len: u16,
    ) -> Self {
        Self {
            version: WIRE_VERSION,
            flags: 0,
            payload_type,
            session_id,
            sequence,
            payload_len,
        }
    }

    #[must_use]
    pub fn encode(self) -> [u8; HEADER_LEN] {
        let mut out = [0; HEADER_LEN];
        out[0] = self.version;
        out[1] = self.flags;
        out[2] = self.payload_type as u8;
        out[3..7].copy_from_slice(&self.session_id.to_be_bytes());
        out[7..15].copy_from_slice(&self.sequence.to_be_bytes());
        out[15..17].copy_from_slice(&self.payload_len.to_be_bytes());
        out
    }

    pub fn decode(input: &[u8]) -> Result<Self, DecodeError> {
        if input.len() < HEADER_LEN {
            return Err(DecodeError::Truncated {
                actual: input.len(),
                expected: HEADER_LEN,
            });
        }

        let version = input[0];
        if version != WIRE_VERSION {
            return Err(DecodeError::UnsupportedVersion(version));
        }

        Ok(Self {
            version,
            flags: input[1],
            payload_type: PayloadType::try_from(input[2])?,
            session_id: u32::from_be_bytes(input[3..7].try_into().expect("fixed slice length")),
            sequence: u64::from_be_bytes(input[7..15].try_into().expect("fixed slice length")),
            payload_len: u16::from_be_bytes(input[15..17].try_into().expect("fixed slice length")),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Truncated { actual: usize, expected: usize },
    UnsupportedVersion(u8),
    UnknownPayloadType(u8),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub header: Header,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(
        payload_type: PayloadType,
        session_id: SessionId,
        sequence: Sequence,
        payload: Vec<u8>,
    ) -> Result<Self, FrameError> {
        let payload_len =
            u16::try_from(payload.len()).map_err(|_| FrameError::PayloadTooLarge {
                actual: payload.len(),
                max: MAX_PAYLOAD_LEN,
            })?;

        Ok(Self {
            header: Header::new(payload_type, session_id, sequence, payload_len),
            payload,
        })
    }

    pub fn packet(
        session_id: SessionId,
        sequence: Sequence,
        payload: Vec<u8>,
    ) -> Result<Self, FrameError> {
        Self::new(PayloadType::IpPacket, session_id, sequence, payload)
    }

    pub fn keepalive(session_id: SessionId, sequence: Sequence) -> Result<Self, FrameError> {
        Self::new(PayloadType::Keepalive, session_id, sequence, Vec::new())
    }

    pub fn path_probe(
        session_id: SessionId,
        sequence: Sequence,
        payload: Vec<u8>,
    ) -> Result<Self, FrameError> {
        Self::new(PayloadType::PathProbe, session_id, sequence, payload)
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.header.encode());
        out.extend_from_slice(&self.payload);
        out
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    PayloadTooLarge { actual: usize, max: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let header = Header::new(PayloadType::IpPacket, 42, 99, 1280);

        assert_eq!(Header::decode(&header.encode()), Ok(header));
    }

    #[test]
    fn decode_rejects_unknown_versions() {
        let mut bytes = Header::new(PayloadType::Keepalive, 0, 0, 0).encode();
        bytes[0] = 99;

        assert_eq!(
            Header::decode(&bytes),
            Err(DecodeError::UnsupportedVersion(99))
        );
    }

    #[test]
    fn decode_rejects_short_headers() {
        assert_eq!(
            Header::decode(&[0; HEADER_LEN - 1]),
            Err(DecodeError::Truncated {
                actual: HEADER_LEN - 1,
                expected: HEADER_LEN
            })
        );
    }

    #[test]
    fn frame_rejects_payloads_that_cannot_fit_header_len() {
        let error = Frame::packet(1, 1, vec![0; MAX_PAYLOAD_LEN + 1])
            .expect_err("payload length should be bounded");

        assert_eq!(
            error,
            FrameError::PayloadTooLarge {
                actual: MAX_PAYLOAD_LEN + 1,
                max: MAX_PAYLOAD_LEN
            }
        );
    }

    #[test]
    fn keepalive_frame_has_no_payload() {
        let frame = Frame::keepalive(7, 42).expect("frame");

        assert_eq!(frame.header.payload_type, PayloadType::Keepalive);
        assert_eq!(frame.header.session_id, 7);
        assert_eq!(frame.header.sequence, 42);
        assert_eq!(frame.header.payload_len, 0);
        assert!(frame.payload.is_empty());
    }

    #[test]
    fn path_probe_frame_carries_probe_payload() {
        let frame = Frame::path_probe(7, 42, b"probe".to_vec()).expect("frame");

        assert_eq!(frame.header.payload_type, PayloadType::PathProbe);
        assert_eq!(frame.header.payload_len, 5);
        assert_eq!(frame.payload, b"probe");
    }
}
