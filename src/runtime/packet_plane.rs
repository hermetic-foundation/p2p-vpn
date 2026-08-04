use std::{
    collections::HashMap,
    fmt, io,
    net::SocketAddr,
    time::{Duration, Instant},
};

use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use hkdf::Hkdf;
use libp2p::identity::PublicKey;
use rand_core::OsRng;
use sha2_010::{Digest as _, Sha256 as HkdfSha256};
use tokio::net::UdpSocket;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

use crate::{
    PeerId, Sequence, SessionId,
    identity::NodeIdentity,
    wire::{Frame, HEADER_LEN, Header, MAX_PAYLOAD_LEN, WIRE_VERSION},
};

const DATAGRAM_MAGIC: &[u8; 8] = b"p2pvpnD1";
const HANDSHAKE_MAGIC: &[u8; 8] = b"p2pvpnH1";
const HANDSHAKE_SIGNING_DOMAIN: &[u8] = b"p2p-vpn packet-plane handshake v1";
const SESSION_KDF_DOMAIN: &[u8] = b"p2p-vpn packet-plane session keys v1";
const DATAGRAM_HEADER_LEN: usize = 24;
const AEAD_TAG_LEN: usize = 16;
const MAX_UDP_DATAGRAM_LEN: usize = 65_535;
const PACKET_PLANE_REPLAY_WINDOW_BITS: u64 = 64;
pub const PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN: usize = 32;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PacketPlaneSnapshot {
    pub listeners: Vec<SocketAddr>,
    pub sessions: Vec<PacketPlaneSessionSnapshot>,
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
    pub ephemeral_public_key: [u8; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN],
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
    pub ephemeral_public_key: [u8; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN],
    pub session_id: SessionId,
    pub nonce: u64,
    pub mtu: u16,
    pub endpoint: SocketAddr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketPlaneHandshakeParams {
    pub network_name: String,
    pub session_id: SessionId,
    pub nonce: u64,
    pub mtu: u16,
    pub ephemeral_public_key: [u8; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN],
    pub endpoint: SocketAddr,
}

#[derive(Clone, Eq, PartialEq)]
pub struct PacketPlaneEphemeralSecret([u8; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN]);

impl PacketPlaneEphemeralSecret {
    #[must_use]
    pub fn generate() -> Self {
        Self(StaticSecret::random_from_rng(OsRng).to_bytes())
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn public_key(&self) -> [u8; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN] {
        X25519PublicKey::from(&StaticSecret::from(self.0)).to_bytes()
    }

    fn shared_secret(
        &self,
        remote_public_key: [u8; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN],
    ) -> Result<[u8; 32], PacketPlaneSessionError> {
        let secret = StaticSecret::from(self.0);
        let shared = secret
            .diffie_hellman(&X25519PublicKey::from(remote_public_key))
            .to_bytes();
        if shared == [0; 32] {
            Err(PacketPlaneSessionError::InvalidSharedSecret)
        } else {
            Ok(shared)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketPlaneSessionRole {
    Initiator,
    Responder,
}

#[derive(Clone, Eq, PartialEq)]
pub struct PacketPlaneSessionKeys {
    pub seal: PacketPlaneCipher,
    pub open: PacketPlaneCipher,
}

#[derive(Clone, Eq, PartialEq)]
pub struct PacketPlaneCipher {
    key: [u8; 32],
}

impl fmt::Debug for PacketPlaneEphemeralSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PacketPlaneEphemeralSecret")
            .field(&"<redacted>")
            .finish()
    }
}

impl fmt::Debug for PacketPlaneSessionKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PacketPlaneSessionKeys")
            .field("seal", &self.seal)
            .field("open", &self.open)
            .finish()
    }
}

impl fmt::Debug for PacketPlaneCipher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PacketPlaneCipher")
            .field(&"<redacted>")
            .finish()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum PacketPlaneSessionError {
    LocalHandshakeKind {
        role: PacketPlaneSessionRole,
        kind: PacketPlaneHandshakeKind,
    },
    RemoteHandshakeKind {
        role: PacketPlaneSessionRole,
        kind: PacketPlaneHandshakeKind,
    },
    InvalidSharedSecret,
    KeyDerivation,
}

#[derive(Debug, Eq, PartialEq)]
pub enum PacketPlaneDatagramError {
    Encrypt,
    Decrypt,
    InvalidMagic,
    Truncated {
        actual: usize,
        expected: usize,
    },
    UnsupportedVersion(u8),
    CiphertextTooLarge {
        actual: usize,
        max: usize,
    },
    PayloadTooLarge {
        actual: usize,
        max: usize,
    },
    FrameDecode(crate::wire::DecodeError),
    FrameLengthMismatch {
        header_len: u16,
        payload_len: usize,
    },
    HeaderMismatch {
        outer_session_id: SessionId,
        outer_sequence: Sequence,
        inner_session_id: SessionId,
        inner_sequence: Sequence,
    },
    ReplayedDatagram {
        session_id: SessionId,
        sequence: Sequence,
    },
    DatagramOutsideReplayWindow {
        session_id: SessionId,
        sequence: Sequence,
    },
    TrailingBytes {
        remaining: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketPlaneReceivedFrame {
    pub frame: Frame,
    pub peer: Option<PeerId>,
    pub remote_addr: SocketAddr,
    pub local_addr: SocketAddr,
}

#[derive(Debug)]
pub enum PacketPlaneIoError {
    NoListener {
        index: usize,
    },
    NoSession {
        peer: PeerId,
    },
    NoSessions,
    UnknownEndpoint {
        actual: SocketAddr,
    },
    UnexpectedEndpoint {
        peer: PeerId,
        expected: SocketAddr,
        actual: SocketAddr,
    },
    Io(io::Error),
    Datagram(PacketPlaneDatagramError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketPlaneSessionSnapshot {
    pub peer: PeerId,
    pub endpoint: SocketAddr,
    pub mtu: u16,
    pub role: PacketPlaneSessionRole,
    pub local_session_id: SessionId,
    pub remote_session_id: SessionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketPlaneSession {
    peer: PeerId,
    endpoint: SocketAddr,
    mtu: u16,
    role: PacketPlaneSessionRole,
    local_session_id: SessionId,
    remote_session_id: SessionId,
    established_at: Instant,
    keys: PacketPlaneSessionKeys,
    replay_windows: HashMap<SessionId, PacketPlaneReplayWindow>,
}

impl PacketPlaneSession {
    #[must_use]
    pub const fn snapshot(&self) -> PacketPlaneSessionSnapshot {
        PacketPlaneSessionSnapshot {
            peer: self.peer,
            endpoint: self.endpoint,
            mtu: self.mtu,
            role: self.role,
            local_session_id: self.local_session_id,
            remote_session_id: self.remote_session_id,
        }
    }

    fn accept_datagram(&mut self, frame: &Frame) -> Result<(), PacketPlaneDatagramError> {
        let window = self
            .replay_windows
            .entry(frame.header.session_id)
            .or_default();
        window
            .accept(frame.header.sequence)
            .map_err(|error| match error {
                PacketPlaneReplayAcceptError::Duplicate => {
                    PacketPlaneDatagramError::ReplayedDatagram {
                        session_id: frame.header.session_id,
                        sequence: frame.header.sequence,
                    }
                }
                PacketPlaneReplayAcceptError::TooOld => {
                    PacketPlaneDatagramError::DatagramOutsideReplayWindow {
                        session_id: frame.header.session_id,
                        sequence: frame.header.sequence,
                    }
                }
            })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PacketPlaneReplayWindow {
    highest: Option<Sequence>,
    seen: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PacketPlaneReplayAcceptError {
    Duplicate,
    TooOld,
}

impl PacketPlaneReplayWindow {
    fn accept(&mut self, sequence: Sequence) -> Result<(), PacketPlaneReplayAcceptError> {
        let Some(highest) = self.highest else {
            self.highest = Some(sequence);
            self.seen = 1;
            return Ok(());
        };

        if sequence > highest {
            let shift = sequence - highest;
            self.seen = if shift >= PACKET_PLANE_REPLAY_WINDOW_BITS {
                1
            } else {
                (self.seen << shift) | 1
            };
            self.highest = Some(sequence);
            return Ok(());
        }

        let offset = highest - sequence;
        if offset >= PACKET_PLANE_REPLAY_WINDOW_BITS {
            return Err(PacketPlaneReplayAcceptError::TooOld);
        }
        let bit = 1_u64 << offset;
        if self.seen & bit != 0 {
            return Err(PacketPlaneReplayAcceptError::Duplicate);
        }

        self.seen |= bit;
        Ok(())
    }
}

impl PacketPlaneSessionKeys {
    pub fn derive(
        role: PacketPlaneSessionRole,
        local_secret: &PacketPlaneEphemeralSecret,
        local: &VerifiedPacketPlaneHandshake,
        remote: &VerifiedPacketPlaneHandshake,
    ) -> Result<Self, PacketPlaneSessionError> {
        validate_session_kinds(role, local.kind, remote.kind)?;
        let shared_secret = local_secret.shared_secret(remote.ephemeral_public_key)?;
        let (hello, accept) = match role {
            PacketPlaneSessionRole::Initiator => (local, remote),
            PacketPlaneSessionRole::Responder => (remote, local),
        };
        let hello_to_accept =
            derive_directional_key(&shared_secret, hello, accept, b"hello-to-accept")?;
        let accept_to_hello =
            derive_directional_key(&shared_secret, hello, accept, b"accept-to-hello")?;
        let (seal, open) = match role {
            PacketPlaneSessionRole::Initiator => (hello_to_accept, accept_to_hello),
            PacketPlaneSessionRole::Responder => (accept_to_hello, hello_to_accept),
        };

        Ok(Self {
            seal: PacketPlaneCipher { key: seal },
            open: PacketPlaneCipher { key: open },
        })
    }
}

impl PacketPlaneCipher {
    pub fn seal_frame(&self, frame: &Frame) -> Result<Vec<u8>, PacketPlaneDatagramError> {
        let payload_len = frame.payload.len();
        if payload_len != usize::from(frame.header.payload_len) {
            return Err(PacketPlaneDatagramError::FrameLengthMismatch {
                header_len: frame.header.payload_len,
                payload_len,
            });
        }

        let plaintext = frame.encode();
        let ciphertext_len = u16::try_from(plaintext.len() + AEAD_TAG_LEN).map_err(|_| {
            PacketPlaneDatagramError::CiphertextTooLarge {
                actual: plaintext.len() + AEAD_TAG_LEN,
                max: usize::from(u16::MAX),
            }
        })?;
        let mut out = encode_datagram_header(
            frame.header.session_id,
            frame.header.sequence,
            ciphertext_len,
        );
        let nonce = datagram_nonce(frame.header.session_id, frame.header.sequence);
        let ciphertext = ChaCha20Poly1305::new_from_slice(&self.key)
            .expect("ChaCha20-Poly1305 key length")
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: &out,
                },
            )
            .map_err(|_| PacketPlaneDatagramError::Encrypt)?;
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    pub fn open_frame(
        &self,
        datagram: &[u8],
        max_payload_len: usize,
    ) -> Result<Frame, PacketPlaneDatagramError> {
        let (outer, ciphertext) = decode_datagram_header(datagram)?;
        let nonce = datagram_nonce(outer.session_id, outer.sequence);
        let plaintext = ChaCha20Poly1305::new_from_slice(&self.key)
            .expect("ChaCha20-Poly1305 key length")
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad: &datagram[..DATAGRAM_HEADER_LEN],
                },
            )
            .map_err(|_| PacketPlaneDatagramError::Decrypt)?;
        let frame = decode_plain_frame(&plaintext, max_payload_len)?;
        if frame.header.session_id != outer.session_id || frame.header.sequence != outer.sequence {
            return Err(PacketPlaneDatagramError::HeaderMismatch {
                outer_session_id: outer.session_id,
                outer_sequence: outer.sequence,
                inner_session_id: frame.header.session_id,
                inner_sequence: frame.header.sequence,
            });
        }
        Ok(frame)
    }
}

impl PacketPlaneHandshake {
    pub fn signed(
        kind: PacketPlaneHandshakeKind,
        identity: &NodeIdentity,
        params: PacketPlaneHandshakeParams,
    ) -> Result<Self, PacketPlaneHandshakeError> {
        let public_key = identity.public_key_protobuf()?;
        let signing_payload = handshake_signing_payload(
            kind,
            HandshakeSigningFields {
                network_name: &params.network_name,
                public_key: &public_key,
                session_id: params.session_id,
                nonce: params.nonce,
                mtu: params.mtu,
                ephemeral_public_key: params.ephemeral_public_key,
                endpoint: params.endpoint,
            },
        )?;
        let signature = identity.sign(&signing_payload)?;
        Ok(Self {
            kind,
            network_name: params.network_name,
            public_key,
            ephemeral_public_key: params.ephemeral_public_key,
            session_id: params.session_id,
            nonce: params.nonce,
            mtu: params.mtu,
            endpoint: params.endpoint,
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
        out.extend_from_slice(&self.ephemeral_public_key);
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
        let ephemeral_public_key = cursor.take_x25519_public_key()?;
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
            ephemeral_public_key,
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
            HandshakeSigningFields {
                network_name: &self.network_name,
                public_key: &self.public_key,
                session_id: self.session_id,
                nonce: self.nonce,
                mtu: self.mtu,
                ephemeral_public_key: self.ephemeral_public_key,
                endpoint: self.endpoint,
            },
        )?;
        if !public_key.verify(&signing_payload, &self.signature) {
            return Err(PacketPlaneHandshakeError::InvalidSignature);
        }

        Ok(VerifiedPacketPlaneHandshake {
            kind: self.kind,
            peer,
            ephemeral_public_key: self.ephemeral_public_key,
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
    sessions: HashMap<PeerId, PacketPlaneSession>,
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

#[derive(Clone, Copy, Debug)]
struct HandshakeSigningFields<'a> {
    network_name: &'a str,
    public_key: &'a [u8],
    session_id: SessionId,
    nonce: u64,
    mtu: u16,
    ephemeral_public_key: [u8; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN],
    endpoint: SocketAddr,
}

fn handshake_signing_payload(
    kind: PacketPlaneHandshakeKind,
    fields: HandshakeSigningFields<'_>,
) -> Result<Vec<u8>, PacketPlaneHandshakeError> {
    let endpoint = fields.endpoint.to_string();
    let mut out = Vec::new();
    out.extend_from_slice(HANDSHAKE_SIGNING_DOMAIN);
    out.push(WIRE_VERSION);
    out.push(kind as u8);
    out.extend_from_slice(&fields.session_id.to_be_bytes());
    out.extend_from_slice(&fields.nonce.to_be_bytes());
    out.extend_from_slice(&fields.mtu.to_be_bytes());
    out.extend_from_slice(&fields.ephemeral_public_key);
    encode_len_prefixed(&mut out, fields.network_name.as_bytes())?;
    encode_len_prefixed(&mut out, endpoint.as_bytes())?;
    encode_len_prefixed(&mut out, fields.public_key)?;
    Ok(out)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PacketPlaneDatagramHeader {
    session_id: SessionId,
    sequence: Sequence,
    ciphertext_len: u16,
}

fn validate_session_kinds(
    role: PacketPlaneSessionRole,
    local: PacketPlaneHandshakeKind,
    remote: PacketPlaneHandshakeKind,
) -> Result<(), PacketPlaneSessionError> {
    let (expected_local, expected_remote) = match role {
        PacketPlaneSessionRole::Initiator => (
            PacketPlaneHandshakeKind::Hello,
            PacketPlaneHandshakeKind::Accept,
        ),
        PacketPlaneSessionRole::Responder => (
            PacketPlaneHandshakeKind::Accept,
            PacketPlaneHandshakeKind::Hello,
        ),
    };
    if local != expected_local {
        return Err(PacketPlaneSessionError::LocalHandshakeKind { role, kind: local });
    }
    if remote != expected_remote {
        return Err(PacketPlaneSessionError::RemoteHandshakeKind { role, kind: remote });
    }
    Ok(())
}

fn derive_directional_key(
    shared_secret: &[u8; 32],
    hello: &VerifiedPacketPlaneHandshake,
    accept: &VerifiedPacketPlaneHandshake,
    direction: &[u8],
) -> Result<[u8; 32], PacketPlaneSessionError> {
    let transcript_hash = packet_plane_transcript_hash(hello, accept);
    let hkdf = Hkdf::<HkdfSha256>::new(Some(&transcript_hash), shared_secret);
    let mut key = [0; 32];
    hkdf.expand(direction, &mut key)
        .map_err(|_| PacketPlaneSessionError::KeyDerivation)?;
    Ok(key)
}

fn packet_plane_transcript_hash(
    hello: &VerifiedPacketPlaneHandshake,
    accept: &VerifiedPacketPlaneHandshake,
) -> [u8; 32] {
    let mut hash = HkdfSha256::new();
    hash.update(SESSION_KDF_DOMAIN);
    hash_verified_handshake(&mut hash, hello);
    hash_verified_handshake(&mut hash, accept);
    hash.finalize().into()
}

fn hash_verified_handshake(hash: &mut HkdfSha256, handshake: &VerifiedPacketPlaneHandshake) {
    hash.update([handshake.kind as u8]);
    hash.update(handshake.peer.as_bytes());
    hash.update(handshake.ephemeral_public_key);
    hash.update(handshake.session_id.to_be_bytes());
    hash.update(handshake.nonce.to_be_bytes());
    hash.update(handshake.mtu.to_be_bytes());
    hash.update(handshake.endpoint.to_string().as_bytes());
}

fn encode_datagram_header(
    session_id: SessionId,
    sequence: Sequence,
    ciphertext_len: u16,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(DATAGRAM_HEADER_LEN);
    out.extend_from_slice(DATAGRAM_MAGIC);
    out.push(WIRE_VERSION);
    out.push(0);
    out.extend_from_slice(&session_id.to_be_bytes());
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(&ciphertext_len.to_be_bytes());
    out
}

fn decode_datagram_header(
    datagram: &[u8],
) -> Result<(PacketPlaneDatagramHeader, &[u8]), PacketPlaneDatagramError> {
    if datagram.len() < DATAGRAM_HEADER_LEN {
        return Err(PacketPlaneDatagramError::Truncated {
            actual: datagram.len(),
            expected: DATAGRAM_HEADER_LEN,
        });
    }
    if &datagram[..DATAGRAM_MAGIC.len()] != DATAGRAM_MAGIC {
        return Err(PacketPlaneDatagramError::InvalidMagic);
    }
    let version = datagram[8];
    if version != WIRE_VERSION {
        return Err(PacketPlaneDatagramError::UnsupportedVersion(version));
    }
    let session_id =
        SessionId::from_be_bytes(datagram[10..14].try_into().expect("fixed slice length"));
    let sequence =
        Sequence::from_be_bytes(datagram[14..22].try_into().expect("fixed slice length"));
    let ciphertext_len =
        u16::from_be_bytes(datagram[22..24].try_into().expect("fixed slice length"));
    let actual_ciphertext_len = datagram.len() - DATAGRAM_HEADER_LEN;
    if actual_ciphertext_len < usize::from(ciphertext_len) {
        return Err(PacketPlaneDatagramError::Truncated {
            actual: actual_ciphertext_len,
            expected: usize::from(ciphertext_len),
        });
    }
    if actual_ciphertext_len > usize::from(ciphertext_len) {
        return Err(PacketPlaneDatagramError::TrailingBytes {
            remaining: actual_ciphertext_len - usize::from(ciphertext_len),
        });
    }
    Ok((
        PacketPlaneDatagramHeader {
            session_id,
            sequence,
            ciphertext_len,
        },
        &datagram[DATAGRAM_HEADER_LEN..],
    ))
}

fn datagram_nonce(session_id: SessionId, sequence: Sequence) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[..4].copy_from_slice(&session_id.to_be_bytes());
    nonce[4..].copy_from_slice(&sequence.to_be_bytes());
    nonce
}

fn decode_plain_frame(
    plaintext: &[u8],
    max_payload_len: usize,
) -> Result<Frame, PacketPlaneDatagramError> {
    if plaintext.len() < HEADER_LEN {
        return Err(PacketPlaneDatagramError::Truncated {
            actual: plaintext.len(),
            expected: HEADER_LEN,
        });
    }
    let header =
        Header::decode(&plaintext[..HEADER_LEN]).map_err(PacketPlaneDatagramError::FrameDecode)?;
    let payload_len = usize::from(header.payload_len);
    if payload_len > max_payload_len.min(MAX_PAYLOAD_LEN) {
        return Err(PacketPlaneDatagramError::PayloadTooLarge {
            actual: payload_len,
            max: max_payload_len.min(MAX_PAYLOAD_LEN),
        });
    }
    let expected_len = HEADER_LEN + payload_len;
    if plaintext.len() < expected_len {
        return Err(PacketPlaneDatagramError::FrameLengthMismatch {
            header_len: header.payload_len,
            payload_len: plaintext.len().saturating_sub(HEADER_LEN),
        });
    }
    if plaintext.len() > expected_len {
        return Err(PacketPlaneDatagramError::TrailingBytes {
            remaining: plaintext.len() - expected_len,
        });
    }
    Ok(Frame {
        header,
        payload: plaintext[HEADER_LEN..].to_vec(),
    })
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

    fn take_x25519_public_key(
        &mut self,
    ) -> Result<[u8; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN], PacketPlaneHandshakeError> {
        Ok(self
            .take(PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN)?
            .try_into()
            .expect("fixed slice length"))
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
    pub fn disabled() -> Self {
        Self {
            sockets: Vec::new(),
            listeners: Vec::new(),
            sessions: HashMap::new(),
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

        Ok(Self {
            sockets,
            listeners,
            sessions: HashMap::new(),
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> PacketPlaneSnapshot {
        let mut sessions = self
            .sessions
            .values()
            .map(PacketPlaneSession::snapshot)
            .collect::<Vec<_>>();
        sessions.sort_by_key(|session| session.peer.to_string());
        PacketPlaneSnapshot {
            listeners: self.listeners.clone(),
            sessions,
        }
    }

    #[must_use]
    pub fn listener_count(&self) -> usize {
        self.sockets.len()
    }

    #[must_use]
    pub fn primary_listener(&self) -> Option<SocketAddr> {
        self.listeners.first().copied()
    }

    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    #[must_use]
    pub fn session_for(&self, peer: PeerId) -> Option<&PacketPlaneSession> {
        self.sessions.get(&peer)
    }

    #[must_use]
    pub fn has_session(&self, peer: PeerId) -> bool {
        self.sessions.contains_key(&peer)
    }

    #[must_use]
    pub fn session_mtu_for(&self, peer: PeerId) -> Option<u16> {
        self.sessions.get(&peer).map(|session| session.mtu)
    }

    #[must_use]
    pub fn can_receive(&self) -> bool {
        !self.sockets.is_empty() && !self.sessions.is_empty()
    }

    pub fn establish_session(
        &mut self,
        role: PacketPlaneSessionRole,
        local_secret: &PacketPlaneEphemeralSecret,
        local: &VerifiedPacketPlaneHandshake,
        remote: &VerifiedPacketPlaneHandshake,
    ) -> Result<PacketPlaneSessionSnapshot, PacketPlaneSessionError> {
        self.establish_session_at(role, local_secret, local, remote, Instant::now())
    }

    fn establish_session_at(
        &mut self,
        role: PacketPlaneSessionRole,
        local_secret: &PacketPlaneEphemeralSecret,
        local: &VerifiedPacketPlaneHandshake,
        remote: &VerifiedPacketPlaneHandshake,
        established_at: Instant,
    ) -> Result<PacketPlaneSessionSnapshot, PacketPlaneSessionError> {
        let keys = PacketPlaneSessionKeys::derive(role, local_secret, local, remote)?;
        let session = PacketPlaneSession {
            peer: remote.peer,
            endpoint: remote.endpoint,
            mtu: local.mtu.min(remote.mtu),
            role,
            local_session_id: local.session_id,
            remote_session_id: remote.session_id,
            established_at,
            keys,
            replay_windows: HashMap::new(),
        };
        let snapshot = session.snapshot();
        self.sessions.insert(remote.peer, session);
        Ok(snapshot)
    }

    #[cfg(test)]
    pub(crate) fn establish_test_session_at(
        &mut self,
        role: PacketPlaneSessionRole,
        local_secret: &PacketPlaneEphemeralSecret,
        local: &VerifiedPacketPlaneHandshake,
        remote: &VerifiedPacketPlaneHandshake,
        established_at: Instant,
    ) -> Result<PacketPlaneSessionSnapshot, PacketPlaneSessionError> {
        self.establish_session_at(role, local_secret, local, remote, established_at)
    }

    pub fn expire_sessions(&mut self, max_age: Duration) -> Vec<PacketPlaneSessionSnapshot> {
        self.expire_sessions_at(Instant::now(), max_age)
    }

    fn expire_sessions_at(
        &mut self,
        now: Instant,
        max_age: Duration,
    ) -> Vec<PacketPlaneSessionSnapshot> {
        let expired_peers = self
            .sessions
            .iter()
            .filter_map(|(peer, session)| {
                (now.saturating_duration_since(session.established_at) >= max_age).then_some(*peer)
            })
            .collect::<Vec<_>>();
        let mut expired = expired_peers
            .into_iter()
            .filter_map(|peer| self.sessions.remove(&peer))
            .map(|session| session.snapshot())
            .collect::<Vec<_>>();
        expired.sort_by_key(|session| session.peer.to_string());
        expired
    }

    pub async fn send_frame_to(
        &self,
        endpoint: SocketAddr,
        cipher: &PacketPlaneCipher,
        frame: &Frame,
    ) -> Result<usize, PacketPlaneIoError> {
        self.send_frame_from(0, endpoint, cipher, frame).await
    }

    pub async fn send_frame_from(
        &self,
        listener_index: usize,
        endpoint: SocketAddr,
        cipher: &PacketPlaneCipher,
        frame: &Frame,
    ) -> Result<usize, PacketPlaneIoError> {
        let socket = self
            .sockets
            .get(listener_index)
            .ok_or(PacketPlaneIoError::NoListener {
                index: listener_index,
            })?;
        let datagram = cipher.seal_frame(frame)?;
        Ok(socket.send_to(&datagram, endpoint).await?)
    }

    pub async fn send_frame_to_peer(
        &self,
        peer: PeerId,
        frame: &Frame,
    ) -> Result<usize, PacketPlaneIoError> {
        self.send_frame_to_peer_from(0, peer, frame).await
    }

    pub async fn send_frame_to_peer_from(
        &self,
        listener_index: usize,
        peer: PeerId,
        frame: &Frame,
    ) -> Result<usize, PacketPlaneIoError> {
        let session = self
            .sessions
            .get(&peer)
            .ok_or(PacketPlaneIoError::NoSession { peer })?;
        let payload_len = frame.payload.len();
        if payload_len > usize::from(session.mtu) {
            return Err(PacketPlaneIoError::Datagram(
                PacketPlaneDatagramError::PayloadTooLarge {
                    actual: payload_len,
                    max: usize::from(session.mtu),
                },
            ));
        }
        self.send_frame_from(listener_index, session.endpoint, &session.keys.seal, frame)
            .await
    }

    pub async fn recv_frame(
        &self,
        cipher: &PacketPlaneCipher,
        max_payload_len: usize,
    ) -> Result<PacketPlaneReceivedFrame, PacketPlaneIoError> {
        self.recv_frame_on(0, cipher, max_payload_len).await
    }

    pub async fn recv_frame_on(
        &self,
        listener_index: usize,
        cipher: &PacketPlaneCipher,
        max_payload_len: usize,
    ) -> Result<PacketPlaneReceivedFrame, PacketPlaneIoError> {
        let socket = self
            .sockets
            .get(listener_index)
            .ok_or(PacketPlaneIoError::NoListener {
                index: listener_index,
            })?;
        let mut datagram = vec![0; MAX_UDP_DATAGRAM_LEN];
        let (len, remote_addr) = socket.recv_from(&mut datagram).await?;
        datagram.truncate(len);
        let frame = cipher.open_frame(&datagram, max_payload_len)?;
        Ok(PacketPlaneReceivedFrame {
            frame,
            peer: None,
            remote_addr,
            local_addr: socket.local_addr()?,
        })
    }

    pub async fn recv_frame_from_peer(
        &mut self,
        peer: PeerId,
    ) -> Result<PacketPlaneReceivedFrame, PacketPlaneIoError> {
        self.recv_frame_from_peer_on(0, peer).await
    }

    pub async fn recv_frame_from_peer_on(
        &mut self,
        listener_index: usize,
        peer: PeerId,
    ) -> Result<PacketPlaneReceivedFrame, PacketPlaneIoError> {
        let session = self
            .sessions
            .get_mut(&peer)
            .ok_or(PacketPlaneIoError::NoSession { peer })?;
        let socket = self
            .sockets
            .get(listener_index)
            .ok_or(PacketPlaneIoError::NoListener {
                index: listener_index,
            })?;
        let mut datagram = vec![0; MAX_UDP_DATAGRAM_LEN];
        let (len, remote_addr) = socket.recv_from(&mut datagram).await?;
        if remote_addr != session.endpoint {
            return Err(PacketPlaneIoError::UnexpectedEndpoint {
                peer,
                expected: session.endpoint,
                actual: remote_addr,
            });
        }
        datagram.truncate(len);
        let frame = session
            .keys
            .open
            .open_frame(&datagram, usize::from(session.mtu))?;
        session.accept_datagram(&frame)?;
        Ok(PacketPlaneReceivedFrame {
            frame,
            peer: Some(peer),
            remote_addr,
            local_addr: socket.local_addr()?,
        })
    }

    pub async fn recv_frame_from_session(
        &mut self,
    ) -> Result<PacketPlaneReceivedFrame, PacketPlaneIoError> {
        self.recv_frame_from_session_on(0).await
    }

    pub async fn recv_frame_from_session_on(
        &mut self,
        listener_index: usize,
    ) -> Result<PacketPlaneReceivedFrame, PacketPlaneIoError> {
        if self.sessions.is_empty() {
            return Err(PacketPlaneIoError::NoSessions);
        }
        let socket = self
            .sockets
            .get(listener_index)
            .ok_or(PacketPlaneIoError::NoListener {
                index: listener_index,
            })?;
        let mut datagram = vec![0; MAX_UDP_DATAGRAM_LEN];
        let (len, remote_addr) = socket.recv_from(&mut datagram).await?;
        let Some(session) = self
            .sessions
            .values_mut()
            .find(|session| session.endpoint == remote_addr)
        else {
            return Err(PacketPlaneIoError::UnknownEndpoint {
                actual: remote_addr,
            });
        };
        datagram.truncate(len);
        let frame = session
            .keys
            .open
            .open_frame(&datagram, usize::from(session.mtu))?;
        session.accept_datagram(&frame)?;
        Ok(PacketPlaneReceivedFrame {
            frame,
            peer: Some(session.peer),
            remote_addr,
            local_addr: socket.local_addr()?,
        })
    }
}

impl From<io::Error> for PacketPlaneIoError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<PacketPlaneDatagramError> for PacketPlaneIoError {
    fn from(error: PacketPlaneDatagramError) -> Self {
        Self::Datagram(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, timeout};

    fn test_secret(byte: u8) -> PacketPlaneEphemeralSecret {
        PacketPlaneEphemeralSecret::from_bytes([byte; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN])
    }

    fn signed_test_handshake(
        kind: PacketPlaneHandshakeKind,
        identity: &NodeIdentity,
        secret: &PacketPlaneEphemeralSecret,
        session_id: SessionId,
        nonce: u64,
    ) -> PacketPlaneHandshake {
        signed_test_handshake_with_endpoint(
            kind,
            identity,
            secret,
            session_id,
            nonce,
            1280,
            "127.0.0.1:51820".parse().expect("endpoint"),
        )
    }

    fn signed_test_handshake_with_endpoint(
        kind: PacketPlaneHandshakeKind,
        identity: &NodeIdentity,
        secret: &PacketPlaneEphemeralSecret,
        session_id: SessionId,
        nonce: u64,
        mtu: u16,
        endpoint: SocketAddr,
    ) -> PacketPlaneHandshake {
        PacketPlaneHandshake::signed(
            kind,
            identity,
            PacketPlaneHandshakeParams {
                network_name: "lab".to_owned(),
                session_id,
                nonce,
                mtu,
                ephemeral_public_key: secret.public_key(),
                endpoint,
            },
        )
        .expect("signed handshake")
    }

    fn verified_session_pair() -> (
        PacketPlaneEphemeralSecret,
        PacketPlaneEphemeralSecret,
        VerifiedPacketPlaneHandshake,
        VerifiedPacketPlaneHandshake,
    ) {
        let initiator_identity = NodeIdentity::generate_ed25519().expect("initiator identity");
        let responder_identity = NodeIdentity::generate_ed25519().expect("responder identity");
        let initiator_secret = test_secret(7);
        let responder_secret = test_secret(9);
        let hello = signed_test_handshake(
            PacketPlaneHandshakeKind::Hello,
            &initiator_identity,
            &initiator_secret,
            11,
            101,
        )
        .verify("lab", None)
        .expect("verified hello");
        let accept = signed_test_handshake(
            PacketPlaneHandshakeKind::Accept,
            &responder_identity,
            &responder_secret,
            13,
            103,
        )
        .verify("lab", None)
        .expect("verified accept");

        (initiator_secret, responder_secret, hello, accept)
    }

    fn verified_session_pair_with_endpoints(
        initiator_endpoint: SocketAddr,
        responder_endpoint: SocketAddr,
        mtu: u16,
    ) -> (
        PacketPlaneEphemeralSecret,
        PacketPlaneEphemeralSecret,
        VerifiedPacketPlaneHandshake,
        VerifiedPacketPlaneHandshake,
    ) {
        let initiator_identity = NodeIdentity::generate_ed25519().expect("initiator identity");
        let responder_identity = NodeIdentity::generate_ed25519().expect("responder identity");
        let initiator_secret = test_secret(7);
        let responder_secret = test_secret(9);
        let hello = signed_test_handshake_with_endpoint(
            PacketPlaneHandshakeKind::Hello,
            &initiator_identity,
            &initiator_secret,
            11,
            101,
            mtu,
            initiator_endpoint,
        )
        .verify("lab", None)
        .expect("verified hello");
        let accept = signed_test_handshake_with_endpoint(
            PacketPlaneHandshakeKind::Accept,
            &responder_identity,
            &responder_secret,
            13,
            103,
            mtu,
            responder_endpoint,
        )
        .verify("lab", None)
        .expect("verified accept");

        (initiator_secret, responder_secret, hello, accept)
    }

    fn session_key_pair() -> (PacketPlaneSessionKeys, PacketPlaneSessionKeys) {
        let (initiator_secret, responder_secret, hello, accept) = verified_session_pair();
        let initiator_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Initiator,
            &initiator_secret,
            &hello,
            &accept,
        )
        .expect("initiator keys");
        let responder_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Responder,
            &responder_secret,
            &accept,
            &hello,
        )
        .expect("responder keys");
        (initiator_keys, responder_keys)
    }

    #[test]
    fn handshake_round_trips_and_verifies_signature() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let secret = test_secret(7);
        let endpoint = "127.0.0.1:51820".parse().expect("endpoint");
        let expected_peer =
            PeerId::from_libp2p(identity.public_key().expect("public key").to_peer_id());

        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            PacketPlaneHandshakeParams {
                network_name: "lab".to_owned(),
                session_id: 7,
                nonce: 99,
                mtu: 1280,
                ephemeral_public_key: secret.public_key(),
                endpoint,
            },
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
        assert_eq!(verified.ephemeral_public_key, secret.public_key());
        assert_eq!(verified.endpoint, endpoint);
    }

    #[test]
    fn handshake_rejects_wrong_network() {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        let secret = test_secret(7);
        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Accept,
            &identity,
            PacketPlaneHandshakeParams {
                network_name: "lab".to_owned(),
                session_id: 7,
                nonce: 99,
                mtu: 1280,
                ephemeral_public_key: secret.public_key(),
                endpoint: "127.0.0.1:51820".parse().expect("endpoint"),
            },
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
        let secret = test_secret(7);
        let other = NodeIdentity::generate_ed25519().expect("other identity");
        let other_peer = PeerId::from_libp2p(other.public_key().expect("public key").to_peer_id());
        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            PacketPlaneHandshakeParams {
                network_name: "lab".to_owned(),
                session_id: 7,
                nonce: 99,
                mtu: 1280,
                ephemeral_public_key: secret.public_key(),
                endpoint: "127.0.0.1:51820".parse().expect("endpoint"),
            },
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
        let secret = test_secret(7);
        let mut handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            PacketPlaneHandshakeParams {
                network_name: "lab".to_owned(),
                session_id: 7,
                nonce: 99,
                mtu: 1280,
                ephemeral_public_key: secret.public_key(),
                endpoint: "127.0.0.1:51820".parse().expect("endpoint"),
            },
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
        let secret = test_secret(7);
        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            PacketPlaneHandshakeParams {
                network_name: "lab".to_owned(),
                session_id: 7,
                nonce: 99,
                mtu: 1280,
                ephemeral_public_key: secret.public_key(),
                endpoint: "127.0.0.1:51820".parse().expect("endpoint"),
            },
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
        let secret = test_secret(7);
        let handshake = PacketPlaneHandshake::signed(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            PacketPlaneHandshakeParams {
                network_name: "lab".to_owned(),
                session_id: 7,
                nonce: 99,
                mtu: 1280,
                ephemeral_public_key: secret.public_key(),
                endpoint: "127.0.0.1:51820".parse().expect("endpoint"),
            },
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
            ephemeral_public_key: [0; PACKET_PLANE_EPHEMERAL_PUBLIC_KEY_LEN],
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

    #[test]
    fn packet_plane_session_keys_encrypt_between_verified_handshakes() {
        let (initiator_secret, responder_secret, hello, accept) = verified_session_pair();
        let initiator_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Initiator,
            &initiator_secret,
            &hello,
            &accept,
        )
        .expect("initiator keys");
        let responder_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Responder,
            &responder_secret,
            &accept,
            &hello,
        )
        .expect("responder keys");
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");

        let datagram = initiator_keys
            .seal
            .seal_frame(&frame)
            .expect("sealed frame");
        let opened = responder_keys
            .open
            .open_frame(&datagram, 1280)
            .expect("opened frame");

        assert_eq!(opened, frame);
        assert_ne!(&datagram[DATAGRAM_HEADER_LEN..], frame.encode().as_slice());
    }

    #[test]
    fn packet_plane_session_keys_are_directional() {
        let (initiator_secret, responder_secret, hello, accept) = verified_session_pair();
        let initiator_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Initiator,
            &initiator_secret,
            &hello,
            &accept,
        )
        .expect("initiator keys");
        let responder_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Responder,
            &responder_secret,
            &accept,
            &hello,
        )
        .expect("responder keys");
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");
        let datagram = initiator_keys
            .seal
            .seal_frame(&frame)
            .expect("sealed frame");

        assert_eq!(
            responder_keys.open.open_frame(&datagram, 1280),
            Ok(frame.clone())
        );
        assert_eq!(
            initiator_keys.open.open_frame(&datagram, 1280),
            Err(PacketPlaneDatagramError::Decrypt)
        );
    }

    #[test]
    fn packet_plane_datagram_rejects_tampering() {
        let (initiator_secret, responder_secret, hello, accept) = verified_session_pair();
        let initiator_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Initiator,
            &initiator_secret,
            &hello,
            &accept,
        )
        .expect("initiator keys");
        let responder_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Responder,
            &responder_secret,
            &accept,
            &hello,
        )
        .expect("responder keys");
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");
        let mut datagram = initiator_keys
            .seal
            .seal_frame(&frame)
            .expect("sealed frame");
        let last = datagram.last_mut().expect("ciphertext byte");
        *last ^= 1;

        assert_eq!(
            responder_keys.open.open_frame(&datagram, 1280),
            Err(PacketPlaneDatagramError::Decrypt)
        );
    }

    #[test]
    fn packet_plane_datagram_rejects_payload_above_configured_mtu() {
        let (initiator_secret, responder_secret, hello, accept) = verified_session_pair();
        let initiator_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Initiator,
            &initiator_secret,
            &hello,
            &accept,
        )
        .expect("initiator keys");
        let responder_keys = PacketPlaneSessionKeys::derive(
            PacketPlaneSessionRole::Responder,
            &responder_secret,
            &accept,
            &hello,
        )
        .expect("responder keys");
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");
        let datagram = initiator_keys
            .seal
            .seal_frame(&frame)
            .expect("sealed frame");

        assert_eq!(
            responder_keys.open.open_frame(&datagram, 4),
            Err(PacketPlaneDatagramError::PayloadTooLarge { actual: 20, max: 4 })
        );
    }

    #[test]
    fn packet_plane_session_rejects_wrong_handshake_roles() {
        let (initiator_secret, _responder_secret, hello, accept) = verified_session_pair();

        assert_eq!(
            PacketPlaneSessionKeys::derive(
                PacketPlaneSessionRole::Initiator,
                &initiator_secret,
                &accept,
                &hello,
            ),
            Err(PacketPlaneSessionError::LocalHandshakeKind {
                role: PacketPlaneSessionRole::Initiator,
                kind: PacketPlaneHandshakeKind::Accept
            })
        );
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

    #[tokio::test]
    async fn udp_runtime_sends_encrypted_frame_between_bound_listeners() {
        let sender = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("sender bind");
        let receiver = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("receiver bind");
        let (initiator_keys, responder_keys) = session_key_pair();
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");
        let receiver_addr = receiver.snapshot().listeners[0];
        let sender_addr = sender.snapshot().listeners[0];

        let sent = sender
            .send_frame_to(receiver_addr, &initiator_keys.seal, &frame)
            .await
            .expect("sent frame");
        let inbound = timeout(
            Duration::from_secs(1),
            receiver.recv_frame(&responder_keys.open, 1280),
        )
        .await
        .expect("receive should not time out")
        .expect("received frame");

        assert!(sent > 0);
        assert_eq!(inbound.frame, frame);
        assert_eq!(inbound.remote_addr, sender_addr);
        assert_eq!(inbound.local_addr, receiver_addr);
    }

    #[tokio::test]
    async fn udp_runtime_rejects_datagram_with_wrong_direction_key() {
        let sender = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("sender bind");
        let receiver = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("receiver bind");
        let (initiator_keys, _responder_keys) = session_key_pair();
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");
        let receiver_addr = receiver.snapshot().listeners[0];

        sender
            .send_frame_to(receiver_addr, &initiator_keys.seal, &frame)
            .await
            .expect("sent frame");
        let error = timeout(
            Duration::from_secs(1),
            receiver.recv_frame(&initiator_keys.open, 1280),
        )
        .await
        .expect("receive should not time out")
        .expect_err("wrong direction key should fail");

        assert!(matches!(
            error,
            PacketPlaneIoError::Datagram(PacketPlaneDatagramError::Decrypt)
        ));
    }

    #[tokio::test]
    async fn udp_runtime_reports_missing_listener_for_send_and_receive() {
        let runtime = PacketPlaneRuntime::disabled();
        let (initiator_keys, _responder_keys) = session_key_pair();
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");
        let endpoint = "127.0.0.1:51820".parse().expect("endpoint");

        assert!(matches!(
            runtime
                .send_frame_to(endpoint, &initiator_keys.seal, &frame)
                .await,
            Err(PacketPlaneIoError::NoListener { index: 0 })
        ));
        assert!(matches!(
            runtime.recv_frame(&initiator_keys.open, 1280).await,
            Err(PacketPlaneIoError::NoListener { index: 0 })
        ));
    }

    #[tokio::test]
    async fn runtime_establishes_peer_sessions_from_verified_handshakes() {
        let sender = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("sender bind");
        let receiver = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("receiver bind");
        let sender_addr = sender.snapshot().listeners[0];
        let receiver_addr = receiver.snapshot().listeners[0];
        let (initiator_secret, _responder_secret, hello, accept) =
            verified_session_pair_with_endpoints(sender_addr, receiver_addr, 1200);
        let mut runtime = PacketPlaneRuntime::disabled();

        let session = runtime
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                &initiator_secret,
                &hello,
                &accept,
            )
            .expect("establish session");
        let snapshot = runtime.snapshot();

        assert_eq!(runtime.session_count(), 1);
        assert_eq!(session.peer, accept.peer);
        assert_eq!(session.endpoint, receiver_addr);
        assert_eq!(session.mtu, 1200);
        assert_eq!(session.role, PacketPlaneSessionRole::Initiator);
        assert_eq!(session.local_session_id, hello.session_id);
        assert_eq!(session.remote_session_id, accept.session_id);
        assert_eq!(snapshot.sessions, vec![session]);
        assert!(runtime.session_for(accept.peer).is_some());
    }

    #[test]
    fn runtime_expires_sessions_at_configured_age() {
        let now = Instant::now();
        let ttl = Duration::from_mins(1);
        let (initiator_secret, _responder_secret, hello, accept) = verified_session_pair();
        let mut runtime = PacketPlaneRuntime::disabled();

        let session = runtime
            .establish_session_at(
                PacketPlaneSessionRole::Initiator,
                &initiator_secret,
                &hello,
                &accept,
                now.checked_sub(ttl).expect("established time"),
            )
            .expect("establish session");

        assert_eq!(runtime.expire_sessions_at(now, ttl), vec![session]);
        assert_eq!(runtime.session_count(), 0);
        assert!(!runtime.has_session(accept.peer));
    }

    #[test]
    fn runtime_keeps_sessions_younger_than_configured_age() {
        let now = Instant::now();
        let ttl = Duration::from_mins(1);
        let (initiator_secret, _responder_secret, hello, accept) = verified_session_pair();
        let mut runtime = PacketPlaneRuntime::disabled();

        runtime
            .establish_session_at(
                PacketPlaneSessionRole::Initiator,
                &initiator_secret,
                &hello,
                &accept,
                now.checked_sub(Duration::from_secs(59))
                    .expect("established time"),
            )
            .expect("establish session");

        assert!(runtime.expire_sessions_at(now, ttl).is_empty());
        assert_eq!(runtime.session_count(), 1);
        assert!(runtime.has_session(accept.peer));
    }

    #[tokio::test]
    async fn udp_runtime_sends_encrypted_frame_to_registered_peer() {
        let mut sender = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("sender bind");
        let mut receiver = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("receiver bind");
        let sender_addr = sender.snapshot().listeners[0];
        let receiver_addr = receiver.snapshot().listeners[0];
        let (initiator_secret, responder_secret, hello, accept) =
            verified_session_pair_with_endpoints(sender_addr, receiver_addr, 1280);
        sender
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                &initiator_secret,
                &hello,
                &accept,
            )
            .expect("sender session");
        receiver
            .establish_session(
                PacketPlaneSessionRole::Responder,
                &responder_secret,
                &accept,
                &hello,
            )
            .expect("receiver session");
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");

        let sent = sender
            .send_frame_to_peer(accept.peer, &frame)
            .await
            .expect("sent frame");
        let inbound = timeout(
            Duration::from_secs(1),
            receiver.recv_frame_from_peer(hello.peer),
        )
        .await
        .expect("receive should not time out")
        .expect("received frame");

        assert!(sent > 0);
        assert_eq!(inbound.peer, Some(hello.peer));
        assert_eq!(inbound.frame, frame);
        assert_eq!(inbound.remote_addr, sender_addr);
        assert_eq!(inbound.local_addr, receiver_addr);
    }

    #[tokio::test]
    async fn udp_runtime_receives_frame_from_registered_session_endpoint() {
        let mut sender = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("sender bind");
        let mut receiver = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("receiver bind");
        let sender_addr = sender.snapshot().listeners[0];
        let receiver_addr = receiver.snapshot().listeners[0];
        let (initiator_secret, responder_secret, hello, accept) =
            verified_session_pair_with_endpoints(sender_addr, receiver_addr, 1280);
        sender
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                &initiator_secret,
                &hello,
                &accept,
            )
            .expect("sender session");
        receiver
            .establish_session(
                PacketPlaneSessionRole::Responder,
                &responder_secret,
                &accept,
                &hello,
            )
            .expect("receiver session");
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");

        sender
            .send_frame_to_peer(accept.peer, &frame)
            .await
            .expect("sent frame");
        let inbound = timeout(Duration::from_secs(1), receiver.recv_frame_from_session())
            .await
            .expect("receive should not time out")
            .expect("received frame");

        assert_eq!(inbound.peer, Some(hello.peer));
        assert_eq!(inbound.frame, frame);
        assert_eq!(inbound.remote_addr, sender_addr);
        assert_eq!(inbound.local_addr, receiver_addr);
    }

    #[tokio::test]
    async fn udp_runtime_rejects_replayed_registered_session_datagram() {
        let mut sender = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("sender bind");
        let mut receiver = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("receiver bind");
        let sender_addr = sender.snapshot().listeners[0];
        let receiver_addr = receiver.snapshot().listeners[0];
        let (initiator_secret, responder_secret, hello, accept) =
            verified_session_pair_with_endpoints(sender_addr, receiver_addr, 1280);
        sender
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                &initiator_secret,
                &hello,
                &accept,
            )
            .expect("sender session");
        receiver
            .establish_session(
                PacketPlaneSessionRole::Responder,
                &responder_secret,
                &accept,
                &hello,
            )
            .expect("receiver session");
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");
        let datagram = sender
            .sessions
            .get(&accept.peer)
            .expect("sender session")
            .keys
            .seal
            .seal_frame(&frame)
            .expect("sealed datagram");

        sender
            .sockets
            .first()
            .expect("sender socket")
            .send_to(&datagram, receiver_addr)
            .await
            .expect("sent first datagram");
        let inbound = timeout(
            Duration::from_secs(1),
            receiver.recv_frame_from_peer(hello.peer),
        )
        .await
        .expect("receive should not time out")
        .expect("first datagram accepted");

        assert_eq!(inbound.frame, frame);

        sender
            .sockets
            .first()
            .expect("sender socket")
            .send_to(&datagram, receiver_addr)
            .await
            .expect("sent replayed datagram");
        let error = timeout(
            Duration::from_secs(1),
            receiver.recv_frame_from_peer(hello.peer),
        )
        .await
        .expect("receive should not time out")
        .expect_err("duplicate datagram should be rejected");

        assert!(matches!(
            error,
            PacketPlaneIoError::Datagram(PacketPlaneDatagramError::ReplayedDatagram {
                session_id: 77,
                sequence: 42
            })
        ));
    }

    #[tokio::test]
    async fn udp_runtime_rejects_unknown_peer_sessions() {
        let mut runtime = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("runtime bind");
        let unknown = PeerId::from_bytes([9; 32]);
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");

        assert!(matches!(
            runtime.send_frame_to_peer(unknown, &frame).await,
            Err(PacketPlaneIoError::NoSession { peer }) if peer == unknown
        ));
        assert!(matches!(
            runtime.recv_frame_from_peer(unknown).await,
            Err(PacketPlaneIoError::NoSession { peer }) if peer == unknown
        ));
    }

    #[tokio::test]
    async fn udp_runtime_rejects_registered_peer_payload_above_session_mtu() {
        let mut sender = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("sender bind");
        let receiver = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("receiver bind");
        let sender_addr = sender.snapshot().listeners[0];
        let receiver_addr = receiver.snapshot().listeners[0];
        let (initiator_secret, _responder_secret, hello, accept) =
            verified_session_pair_with_endpoints(sender_addr, receiver_addr, 4);
        sender
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                &initiator_secret,
                &hello,
                &accept,
            )
            .expect("sender session");
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");

        assert!(matches!(
            sender.send_frame_to_peer(accept.peer, &frame).await,
            Err(PacketPlaneIoError::Datagram(
                PacketPlaneDatagramError::PayloadTooLarge { actual: 20, max: 4 }
            ))
        ));
    }

    #[tokio::test]
    async fn udp_runtime_rejects_registered_peer_endpoint_mismatch() {
        let mut sender = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("sender bind");
        let mut receiver = PacketPlaneRuntime::bind(vec!["127.0.0.1:0".parse().expect("socket")])
            .await
            .expect("receiver bind");
        let sender_addr = sender.snapshot().listeners[0];
        let receiver_addr = receiver.snapshot().listeners[0];
        let expected_sender_addr = "127.0.0.1:9".parse().expect("expected sender endpoint");
        let (initiator_secret, responder_secret, hello, accept) =
            verified_session_pair_with_endpoints(sender_addr, receiver_addr, 1280);
        let (_ignored_initiator_secret, _ignored_responder_secret, mismatched_hello, _accept) =
            verified_session_pair_with_endpoints(expected_sender_addr, receiver_addr, 1280);
        sender
            .establish_session(
                PacketPlaneSessionRole::Initiator,
                &initiator_secret,
                &hello,
                &accept,
            )
            .expect("sender session");
        receiver
            .establish_session(
                PacketPlaneSessionRole::Responder,
                &responder_secret,
                &accept,
                &mismatched_hello,
            )
            .expect("receiver session");
        let frame = Frame::packet(77, 42, vec![0x45; 20]).expect("frame");

        sender
            .send_frame_to_peer(accept.peer, &frame)
            .await
            .expect("sent frame");
        let error = timeout(
            Duration::from_secs(1),
            receiver.recv_frame_from_peer(mismatched_hello.peer),
        )
        .await
        .expect("receive should not time out")
        .expect_err("endpoint mismatch should fail");

        assert!(matches!(
            error,
            PacketPlaneIoError::UnexpectedEndpoint {
                peer,
                expected,
                actual
            } if peer == mismatched_hello.peer
                && expected == expected_sender_addr
                && actual == sender_addr
        ));
    }
}
