pub mod queue;
pub mod route;
pub mod wire;

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
