pub mod config;
pub mod identity;
pub mod metrics;
pub mod path;
pub mod queue;
pub mod route;
pub mod runtime;
pub mod wire;

use std::{fmt, str::FromStr};

use sha2::{Digest, Sha256};

pub type Sequence = u64;
pub type SessionId = u32;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PeerId([u8; 32]);

impl PeerId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    #[must_use]
    pub fn from_libp2p(peer_id: libp2p::PeerId) -> Self {
        let digest = Sha256::digest(peer_id.to_bytes());
        let mut bytes = [0; 32];
        bytes.copy_from_slice(&digest);
        Self(bytes)
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for PeerId {
    type Err = PeerIdParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if let Ok(peer_id) = input.parse::<libp2p::PeerId>() {
            return Ok(Self::from_libp2p(peer_id));
        }

        if input.len() != 64 {
            return Err(PeerIdParseError::InvalidLength {
                actual: input.len(),
            });
        }

        let mut bytes = [0; 32];
        for (index, chunk) in input.as_bytes().chunks_exact(2).enumerate() {
            let high = decode_hex(chunk[0])?;
            let low = decode_hex(chunk[1])?;
            bytes[index] = (high << 4) | low;
        }

        Ok(Self(bytes))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerIdParseError {
    InvalidLength { actual: usize },
    InvalidHex { byte: u8 },
}

fn decode_hex(byte: u8) -> Result<u8, PeerIdParseError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        other => Err(PeerIdParseError::InvalidHex { byte: other }),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathKind {
    DirectQuicDatagram,
    DirectQuicStream,
    DirectTcpStream,
    CircuitRelay,
}

impl PathKind {
    #[must_use]
    pub const fn default_score(self) -> u16 {
        match self {
            Self::DirectQuicDatagram => 100,
            Self::DirectQuicStream => 75,
            Self::DirectTcpStream => 60,
            Self::CircuitRelay => 30,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_round_trips_as_hex() {
        let peer = PeerId::from_bytes([0xab; 32]);

        assert_eq!(peer.to_string().parse::<PeerId>(), Ok(peer));
    }

    #[test]
    fn peer_id_parse_rejects_wrong_length() {
        assert_eq!(
            "abcd".parse::<PeerId>(),
            Err(PeerIdParseError::InvalidLength { actual: 4 })
        );
    }
}
