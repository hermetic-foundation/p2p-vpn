pub mod config;
pub mod identity;
pub mod invite;
pub mod membership;
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

pub const OVERLAY_FRAGMENTATION_POLICY_LINE: &str = "overlay fragmentation: disabled";

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
    DirectUdpDatagram,
    DirectQuicDatagram,
    DirectQuicStream,
    DirectTcpStream,
    CircuitRelay,
}

impl PathKind {
    #[must_use]
    pub const fn default_score(self) -> u16 {
        match self {
            Self::DirectUdpDatagram => 95,
            Self::DirectQuicDatagram => 100,
            Self::DirectQuicStream => 75,
            Self::DirectTcpStream => 60,
            Self::CircuitRelay => 30,
        }
    }

    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::DirectUdpDatagram => "direct_udp_datagram",
            Self::DirectQuicDatagram => "direct_quic_datagram",
            Self::DirectQuicStream => "direct_quic_stream",
            Self::DirectTcpStream => "direct_tcp_stream",
            Self::CircuitRelay => "circuit_relay",
        }
    }

    #[must_use]
    pub fn from_wire_name(name: &str) -> Option<Self> {
        match name {
            "direct_udp_datagram" => Some(Self::DirectUdpDatagram),
            "direct_quic_datagram" => Some(Self::DirectQuicDatagram),
            "direct_quic_stream" => Some(Self::DirectQuicStream),
            "direct_tcp_stream" => Some(Self::DirectTcpStream),
            "circuit_relay" => Some(Self::CircuitRelay),
            _ => None,
        }
    }

    #[must_use]
    pub const fn requires_quic_datagrams(self) -> bool {
        matches!(self, Self::DirectUdpDatagram | Self::DirectQuicDatagram)
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

    #[test]
    fn path_kind_wire_names_round_trip() {
        for path in [
            PathKind::DirectUdpDatagram,
            PathKind::DirectQuicDatagram,
            PathKind::DirectQuicStream,
            PathKind::DirectTcpStream,
            PathKind::CircuitRelay,
        ] {
            assert_eq!(PathKind::from_wire_name(path.wire_name()), Some(path));
        }

        assert_eq!(PathKind::from_wire_name("unknown"), None);
        assert!(PathKind::DirectUdpDatagram.requires_quic_datagrams());
        assert!(PathKind::DirectQuicDatagram.requires_quic_datagrams());
        assert!(!PathKind::DirectQuicStream.requires_quic_datagrams());
    }
}
