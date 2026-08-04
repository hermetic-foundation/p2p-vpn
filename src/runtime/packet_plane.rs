use std::{io, net::SocketAddr};

use libp2p::identity::PublicKey;
use tokio::net::UdpSocket;

use crate::{PeerId, SessionId, identity::NodeIdentity, wire::WIRE_VERSION};

const HANDSHAKE_MAGIC: &[u8; 8] = b"p2pvpnH1";
const HANDSHAKE_SIGNING_DOMAIN: &[u8] = b"p2p-vpn packet-plane handshake v1";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PacketPlaneSnapshot {
    pub listeners: Vec<SocketAddr>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketPlaneHandshakeKind {
    Hello = 1,
    Accept = 2,
}

impl TryFrom<u8> for PacketPlaneHandshakeKind {
    type Error = PacketPlaneHandshakeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Accept),
            other => Err(PacketPlaneHandshakeError::UnknownKind(other)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketPlaneHandshake {
    pub kind: PacketPlaneHandshakeKind,
    pub network_name: String,
    pub public_key: Vec<u8>,
    pub session_id: SessionId,
    pub nonce: u64,
    pub mtu: u16,
    pub endpoint: SocketAddr,
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedPacketPlaneHandshake {
    pub kind: PacketPlaneHandshakeKind,
    pub peer: PeerId,
    pub session_id: SessionId,
    pub nonce: u64,
    pub mtu: u16,
    pub endpoint: SocketAddr,
}

impl PacketPlaneHandshake {
    pub fn signed(
        kind: PacketPlaneHandshakeKind,
        identity: &NodeIdentity,
        network_name: impl Into<String>,
        session_id: SessionId,
        nonce: u64,
        mtu: u16,
        endpoint: SocketAddr,
    ) -> Result<Self, PacketPlaneHandshakeError> {
        let network_name = network_name.into();
        let public_key = identity.public_key_protobuf()?;
        let signing_payload = handshake_signing_payload(
            kind,
            &network_name,
            &public_key,
            session_id,
            nonce,
            mtu,
            endpoint,
        )?;
        let signature = identity.sign(&signing_payload)?;
        Ok(Self {
            kind,
            network_name,
            public_key,
            session_id,
            nonce,
            mtu,
            endpoint,
            signature,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, PacketPlaneHandshakeError> {
        let network_name = self.network_name.as_bytes();
        let endpoint = self.endpoint.to_string();
        let endpoint = endpoint.as_bytes();
        let mut out = Vec::new();
        out.extend_from_slice(HANDSHAKE_MAGIC);
        out.push(WIRE_VERSION);
        out.push(self.kind as u8);
        out.extend_from_slice(&self.session_id.to_be_bytes());
        out.extend_from_slice(&self.nonce.to_be_bytes());
        out.extend_from_slice(&self.mtu.to_be_bytes());
        encode_len_prefixed(&mut out, network_name)?;
        encode_len_prefixed(&mut out, endpoint)?;
        encode_len_prefixed(&mut out, &self.public_key)?;
        encode_len_prefixed(&mut out, &self.signature)?;
        Ok(out)
    }

    pub fn decode(input: &[u8]) -> Result<Self, PacketPlaneHandshakeError> {
        let mut cursor = HandshakeCursor::new(input);
        cursor.take_magic(HANDSHAKE_MAGIC)?;
        let version = cursor.take_u8()?;
        if version != WIRE_VERSION {
            return Err(PacketPlaneHandshakeError::UnsupportedVersion(version));
        }
        let kind = PacketPlaneHandshakeKind::try_from(cursor.take_u8()?)?;
        let session_id = cursor.take_u32()?;
        let nonce = cursor.take_u64()?;
        let mtu = cursor.take_u16()?;
        let network_name = String::from_utf8(cursor.take_len_prefixed()?.to_vec())
            .map_err(|_| PacketPlaneHandshakeError::InvalidUtf8)?;
        let endpoint = std::str::from_utf8(cursor.take_len_prefixed()?)
            .map_err(|_| PacketPlaneHandshakeError::InvalidUtf8)?
            .parse()
            .map_err(PacketPlaneHandshakeError::InvalidEndpoint)?;
        let public_key = cursor.take_len_prefixed()?.to_vec();
        let signature = cursor.take_len_prefixed()?.to_vec();
        cursor.finish()?;

        Ok(Self {
            kind,
            network_name,
            public_key,
            session_id,
            nonce,
            mtu,
            endpoint,
            signature,
        })
    }

    pub fn verify(
        &self,
        expected_network: &str,
        expected_peer: Option<PeerId>,
    ) -> Result<VerifiedPacketPlaneHandshake, PacketPlaneHandshakeError> {
        if self.network_name != expected_network {
            return Err(PacketPlaneHandshakeError::WrongNetwork);
        }
        if self.mtu == 0 {
            return Err(PacketPlaneHandshakeError::InvalidMtu);
        }
        let public_key = PublicKey::try_decode_protobuf(&self.public_key)
            .map_err(PacketPlaneHandshakeError::PublicKey)?;
        let peer = PeerId::from_libp2p(public_key.to_peer_id());
        if expected_peer.is_some_and(|expected| expected != peer) {
            return Err(PacketPlaneHandshakeError::UnexpectedPeer);
        }
        let signing_payload = handshake_signing_payload(
            self.kind,
            &self.network_name,
            &self.public_key,
            self.session_id,
            self.nonce,
            self.mtu,
            self.endpoint,
        )?;
        if !public_key.verify(&signing_payload, &self.signature) {
            return Err(PacketPlaneHandshakeError::InvalidSignature);
        }

        Ok(VerifiedPacketPlaneHandshake {
            kind: self.kind,
            peer,
            session_id: self.session_id,
            nonce: self.nonce,
            mtu: self.mtu,
            endpoint: self.endpoint,
        })
    }
}

#[derive(Debug, Default)]
pub struct PacketPlaneRuntime {
    sockets: Vec<UdpSocket>,
    listeners: Vec<SocketAddr>,
}

#[derive(Debug)]
pub enum PacketPlaneHandshakeError {
    Identity(crate::identity::IdentityError),
    InvalidMagic,
    Truncated { actual: usize, expected: usize },
    TrailingBytes { remaining: usize },
    UnsupportedVersion(u8),
    UnknownKind(u8),
    InvalidUtf8,
    InvalidEndpoint(std::net::AddrParseError),
    PublicKey(libp2p::identity::DecodingError),
    WrongNetwork,
    UnexpectedPeer,
    InvalidMtu,
    InvalidSignature,
    FieldTooLarge { actual: usize, max: usize },
}

fn handshake_signing_payload(
    kind: PacketPlaneHandshakeKind,
    network_name: &str,
    public_key: &[u8],
    session_id: SessionId,
    nonce: u64,
    mtu: u16,
    endpoint: SocketAddr,
) -> Result<Vec<u8>, PacketPlaneHandshakeError> {
    let endpoint = endpoint.to_string();
    let mut out = Vec::new();
    out.extend_from_slice(HANDSHAKE_SIGNING_DOMAIN);
    out.push(WIRE_VERSION);
    out.push(kind as u8);
    out.extend_from_slice(&session_id.to_be_bytes());
    out.extend_from_slice(&nonce.to_be_bytes());
    out.extend_from_slice(&mtu.to_be_bytes());
    encode_len_prefixed(&mut out, network_name.as_bytes())?;
    encode_len_prefixed(&mut out, endpoint.as_bytes())?;
    encode_len_prefixed(&mut out, public_key)?;
    Ok(out)
}

fn encode_len_prefixed(out: &mut Vec<u8>, value: &[u8]) -> Result<(), PacketPlaneHandshakeError> {
    let len = u16::try_from(value.len()).map_err(|_| PacketPlaneHandshakeError::FieldTooLarge {
        actual: value.len(),
        max: usize::from(u16::MAX),
    })?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

impl From<crate::identity::IdentityError> for PacketPlaneHandshakeError {
    fn from(error: crate::identity::IdentityError) -> Self {
        Self::Identity(error)
    }
}

struct HandshakeCursor<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> HandshakeCursor<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn take_magic(&mut self, expected: &[u8]) -> Result<(), PacketPlaneHandshakeError> {
        let bytes = self.take(expected.len())?;
        if bytes == expected {
            Ok(())
        } else {
            Err(PacketPlaneHandshakeError::InvalidMagic)
        }
    }

    fn take_u8(&mut self) -> Result<u8, PacketPlaneHandshakeError> {
        Ok(self.take(1)?[0])
    }

    fn take_u16(&mut self) -> Result<u16, PacketPlaneHandshakeError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("fixed slice length"),
        ))
    }

    fn take_u32(&mut self) -> Result<u32, PacketPlaneHandshakeError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("fixed slice length"),
        ))
    }

    fn take_u64(&mut self) -> Result<u64, PacketPlaneHandshakeError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("fixed slice length"),
        ))
    }

    fn take_len_prefixed(&mut self) -> Result<&'a [u8], PacketPlaneHandshakeError> {
        let len = usize::from(self.take_u16()?);
        self.take(len)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], PacketPlaneHandshakeError> {
        let end = self.offset.saturating_add(len);
        let Some(bytes) = self.input.get(self.offset..end) else {
            return Err(PacketPlaneHandshakeError::Truncated {
                actual: self.input.len().saturating_sub(self.offset),
                expected: len,
            });
        };
        self.offset = end;
        Ok(bytes)
    }

    fn finish(&self) -> Result<(), PacketPlaneHandshakeError> {
        let remaining = self.input.len().saturating_sub(self.offset);
        if remaining == 0 {
            Ok(())
        } else {
            Err(PacketPlaneHandshakeError::TrailingBytes { remaining })
        }
    }
}

impl PacketPlaneRuntime {
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            sockets: Vec::new(),
            listeners: Vec::new(),
        }
    }

    pub async fn bind(listen_addrs: Vec<SocketAddr>) -> Result<Self, io::Error> {
        let mut sockets = Vec::with_capacity(listen_addrs.len());
        let mut listeners = Vec::with_capacity(listen_addrs.len());

        for address in listen_addrs {
            let socket = UdpSocket::bind(address).await?;
            listeners.push(socket.local_addr()?);
            sockets.push(socket);
        }

        Ok(Self { sockets, listeners })
    }

    #[must_use]
    pub fn snapshot(&self) -> PacketPlaneSnapshot {
        PacketPlaneSnapshot {
            listeners: self.listeners.clone(),
        }
    }

    #[must_use]
    pub fn listener_count(&self) -> usize {
        self.sockets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_round_trips_and_verifies_signature() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let endpoint = "127.0.0.1:51820".parse().expect("endpoint");
        let expected_peer =
            PeerId::from_libp2p(identity.public_key().expect("public key").to_peer_id());

        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            "lab",
            7,
            99,
            1280,
            endpoint,
        )
        .expect("signed handshake");

        let encoded = handshake.encode().expect("encoded handshake");
        let decoded = PacketPlaneHandshake::decode(&encoded).expect("decoded handshake");
        let verified = decoded
            .verify("lab", Some(expected_peer))
            .expect("verified handshake");

        assert_eq!(decoded, handshake);
        assert_eq!(verified.kind, PacketPlaneHandshakeKind::Hello);
        assert_eq!(verified.peer, expected_peer);
        assert_eq!(verified.session_id, 7);
        assert_eq!(verified.nonce, 99);
        assert_eq!(verified.mtu, 1280);
        assert_eq!(verified.endpoint, endpoint);
    }

    #[test]
    fn handshake_rejects_wrong_network() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Accept,
            &identity,
            "lab",
            7,
            99,
            1280,
            "127.0.0.1:51820".parse().expect("endpoint"),
        )
        .expect("signed handshake");

        let error = handshake
            .verify("prod", None)
            .expect_err("network mismatch should fail");

        assert!(matches!(error, PacketPlaneHandshakeError::WrongNetwork));
    }

    #[test]
    fn handshake_rejects_unexpected_peer() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let other = NodeIdentity::generate_ed25519().expect("other identity");
        let other_peer = PeerId::from_libp2p(other.public_key().expect("public key").to_peer_id());
        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            "lab",
            7,
            99,
            1280,
            "127.0.0.1:51820".parse().expect("endpoint"),
        )
        .expect("signed handshake");

        let error = handshake
            .verify("lab", Some(other_peer))
            .expect_err("peer mismatch should fail");

        assert!(matches!(error, PacketPlaneHandshakeError::UnexpectedPeer));
    }

    #[test]
    fn handshake_rejects_tampered_signature_payload() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let mut handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            "lab",
            7,
            99,
            1280,
            "127.0.0.1:51820".parse().expect("endpoint"),
        )
        .expect("signed handshake");
        handshake.mtu = 1400;

        let error = handshake
            .verify("lab", None)
            .expect_err("tampered payload should fail");

        assert!(matches!(error, PacketPlaneHandshakeError::InvalidSignature));
    }

    #[test]
    fn handshake_decode_rejects_bad_magic() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            "lab",
            7,
            99,
            1280,
            "127.0.0.1:51820".parse().expect("endpoint"),
        )
        .expect("signed handshake");
        let mut encoded = handshake.encode().expect("encoded handshake");
        encoded[0] = b'x';

        let error = PacketPlaneHandshake::decode(&encoded).expect_err("bad magic should fail");

        assert!(matches!(error, PacketPlaneHandshakeError::InvalidMagic));
    }

    #[test]
    fn handshake_decode_rejects_trailing_bytes() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            "lab",
            7,
            99,
            1280,
            "127.0.0.1:51820".parse().expect("endpoint"),
        )
        .expect("signed handshake");
        let mut encoded = handshake.encode().expect("encoded handshake");
        encoded.push(0);

        let error = PacketPlaneHandshake::decode(&encoded).expect_err("trailing bytes should fail");

        assert!(matches!(
            error,
            PacketPlaneHandshakeError::TrailingBytes { remaining: 1 }
        ));
    }

    #[test]
    fn handshake_encode_rejects_oversized_fields() {
        let handshake = PacketPlaneHandshake {
            kind: PacketPlaneHandshakeKind::Hello,
            network_name: "x".repeat(usize::from(u16::MAX) + 1),
            public_key: Vec::new(),
            session_id: 7,
            nonce: 99,
            mtu: 1280,
            endpoint: "127.0.0.1:51820".parse().expect("endpoint"),
            signature: Vec::new(),
        };

        let error = handshake.encode().expect_err("oversized field should fail");

        assert!(matches!(
            error,
            PacketPlaneHandshakeError::FieldTooLarge { actual, max }
                if actual == usize::from(u16::MAX) + 1 && max == usize::from(u16::MAX)
        ));
    }

    #[tokio::test]
    async fn binds_configured_udp_listeners() {
        let runtime = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("packet plane bind");

        let snapshot = runtime.snapshot();

        assert_eq!(runtime.listener_count(), 1);
        assert_eq!(snapshot.listeners.len(), 1);
        assert!(snapshot.listeners[0].port() > 0);
    }
}
