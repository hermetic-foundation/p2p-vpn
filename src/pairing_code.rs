use std::{fmt, str::FromStr};

use base32::Alphabet;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use chacha20poly1305::{
    ChaCha20Poly1305, Nonce,
    aead::{Aead as _, KeyInit as _, Payload},
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac as _};
use libp2p::{PeerId as Libp2pPeerId, identity::PublicKey};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use sha2_010::{Digest as _, Sha256};
use spake2::{Ed25519Group, Identity as SpakeIdentity, Password, Spake2};

use crate::{
    config::Config,
    identity::NodeIdentity,
    pairing::{
        PairingCodeAuthentication, PairingError, PairingOffer, PairingOfferOptions, PairingRequest,
        export_code_pairing_offer_at,
    },
};

pub const PAIRING_CODE_VERSION: u8 = 1;
pub const PAIRING_CODE_V2_VERSION: u8 = 2;
pub const PAIRING_CODE_ENTROPY_BITS: usize = 80;

const PAIRING_CODE_BYTES: usize = PAIRING_CODE_ENTROPY_BITS / 8;
const PAIRING_CODE_CHARACTERS: usize = 16;
const PAIRING_CODE_GROUP_CHARACTERS: usize = 4;
const PAIRING_CODE_NONCE_BYTES: usize = 12;
const PAIRING_CODE_SESSION_KEY_BYTES: usize = 32;
const PAIRING_CODE_HELLO_MAX_AGE_SECONDS: u64 = 120;
const PAIRING_CODE_CLOCK_SKEW_SECONDS: u64 = 60;

const LOCATOR_DOMAIN: &[u8] = b"p2p-vpn pairing code locator v1\n";
const HELLO_SIGNING_DOMAIN: &[u8] = b"p2p-vpn pairing code hello v1\n";
const CHALLENGE_SIGNING_DOMAIN: &[u8] = b"p2p-vpn pairing code challenge v1\n";
const SPAKE_IDENTITY_DOMAIN: &[u8] = b"p2p-vpn pairing code spake2 v1\n";
const KEY_DERIVATION_DOMAIN: &[u8] = b"p2p-vpn pairing code keys v1\n";
const CHALLENGE_AAD_DOMAIN: &[u8] = b"p2p-vpn pairing code encrypted offer v1\n";
const REQUEST_CONFIRMATION_DOMAIN: &[u8] = b"p2p-vpn pairing code request confirmation v1\n";
const REQUEST_TRANSCRIPT_DOMAIN: &[u8] = b"p2p-vpn pairing code transcript v1\n";

const LOCATOR_V2_DOMAIN: &[u8] = b"p2p-vpn pairing code locator v2\n";
const HELLO_V2_SIGNING_DOMAIN: &[u8] = b"p2p-vpn pairing code hello v2\n";
const CHALLENGE_V2_SIGNING_DOMAIN: &[u8] = b"p2p-vpn pairing code challenge v2\n";
const SPAKE_V2_IDENTITY_DOMAIN: &[u8] = b"p2p-vpn pairing code spake2 v2\n";
const KEY_DERIVATION_V2_DOMAIN: &[u8] = b"p2p-vpn pairing code keys v2\n";
const CHALLENGE_V2_AAD_DOMAIN: &[u8] = b"p2p-vpn pairing code encrypted offer v2\n";
const REQUEST_V2_CONFIRMATION_DOMAIN: &[u8] = b"p2p-vpn pairing code request confirmation v2\n";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Eq, PartialEq)]
pub struct PairingCode([u8; PAIRING_CODE_BYTES]);

impl PairingCode {
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0_u8; PAIRING_CODE_BYTES];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn locator(&self, network_name: &str) -> Result<String, PairingCodeError> {
        if network_name.is_empty() {
            return Err(PairingCodeError::EmptyNetworkName);
        }

        let mut hasher = Sha256::new();
        hasher.update(LOCATOR_DOMAIN);
        hash_length_prefixed(&mut hasher, network_name.as_bytes());
        hash_length_prefixed(&mut hasher, &self.0);
        Ok(URL_SAFE_NO_PAD.encode(hasher.finalize()))
    }

    /// Returns the v2 rendezvous locator, which intentionally does not depend on an overlay
    /// profile or network name.
    #[must_use]
    pub fn global_locator(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(LOCATOR_V2_DOMAIN);
        hash_length_prefixed(&mut hasher, &self.0);
        URL_SAFE_NO_PAD.encode(hasher.finalize())
    }

    fn password(&self) -> Password {
        Password::new(self.0)
    }

    fn canonical_characters(&self) -> String {
        base32::encode(Alphabet::Crockford, &self.0)
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingCode([REDACTED])")
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, character) in self.canonical_characters().chars().enumerate() {
            if index > 0 && index % PAIRING_CODE_GROUP_CHARACTERS == 0 {
                formatter.write_str("-")?;
            }
            write!(formatter, "{character}")?;
        }
        Ok(())
    }
}

impl FromStr for PairingCode {
    type Err = PairingCodeError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let mut compact = String::with_capacity(PAIRING_CODE_CHARACTERS);
        for character in input.chars() {
            if character == '-' || character.is_ascii_whitespace() {
                continue;
            }
            if !character.is_ascii_alphanumeric() {
                return Err(PairingCodeError::InvalidCodeCharacter(character));
            }
            compact.push(character.to_ascii_uppercase());
        }
        if compact.len() != PAIRING_CODE_CHARACTERS {
            return Err(PairingCodeError::InvalidCodeLength {
                actual: compact.len(),
                expected: PAIRING_CODE_CHARACTERS,
            });
        }

        let decoded = base32::decode(Alphabet::Crockford, &compact)
            .ok_or(PairingCodeError::InvalidCodeEncoding)?;
        let bytes = decoded.try_into().map_err(|bytes: Vec<u8>| {
            PairingCodeError::InvalidCodeByteLength {
                actual: bytes.len(),
                expected: PAIRING_CODE_BYTES,
            }
        })?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingCodeHello {
    pub payload: PairingCodeHelloPayload,
    pub signature: String,
}

impl PairingCodeHello {
    pub fn verify_for_transport_peer_at(
        &self,
        transport_peer: Libp2pPeerId,
        now_unix_seconds: u64,
    ) -> Result<(), PairingCodeError> {
        validate_version(self.payload.version)?;
        validate_network_name(&self.payload.network_name)?;
        validate_locator(&self.payload.locator)?;
        validate_hello_time(self.payload.issued_at_unix_seconds, now_unix_seconds)?;
        verify_identity_signature(
            &self.payload.joiner_peer,
            &self.payload.joiner_public_key,
            &self.signature,
            &signing_message(HELLO_SIGNING_DOMAIN, &self.payload)?,
        )?;
        if self.payload.joiner_peer != transport_peer.to_string() {
            return Err(PairingCodeError::TransportPeerMismatch {
                expected: self.payload.joiner_peer.clone(),
                actual: transport_peer.to_string(),
            });
        }
        decode_spake_message(&self.payload.spake_message)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingCodeHelloPayload {
    pub version: u8,
    pub network_name: String,
    pub locator: String,
    pub inviter_peer: String,
    pub joiner_peer: String,
    pub joiner_public_key: String,
    pub issued_at_unix_seconds: u64,
    pub spake_message: String,
}

pub struct PendingPairingCodeHello {
    state: Spake2<Ed25519Group>,
    payload: PairingCodeHelloPayload,
}

impl fmt::Debug for PendingPairingCodeHello {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingPairingCodeHello")
            .field("network_name", &self.payload.network_name)
            .field("locator", &self.payload.locator)
            .field("inviter_peer", &self.payload.inviter_peer)
            .field("joiner_peer", &self.payload.joiner_peer)
            .field("state", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingCodeChallenge {
    pub payload: PairingCodeChallengePayload,
    pub signature: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingCodeChallengePayload {
    pub version: u8,
    pub network_name: String,
    pub locator: String,
    pub inviter_peer: String,
    pub inviter_public_key: String,
    pub joiner_peer: String,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub spake_message: String,
    pub nonce: String,
    pub encrypted_offer: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingCodeHelloV2 {
    pub payload: PairingCodeHelloV2Payload,
    pub signature: String,
}

impl PairingCodeHelloV2 {
    pub fn verify_for_transport_peer_at(
        &self,
        transport_peer: Libp2pPeerId,
        now_unix_seconds: u64,
    ) -> Result<(), PairingCodeError> {
        validate_v2_version(self.payload.version)?;
        validate_locator(&self.payload.locator)?;
        validate_hello_time(self.payload.issued_at_unix_seconds, now_unix_seconds)?;
        self.payload.inviter_peer.parse::<Libp2pPeerId>()?;
        verify_identity_signature(
            &self.payload.joiner_peer,
            &self.payload.joiner_public_key,
            &self.signature,
            &signing_message(HELLO_V2_SIGNING_DOMAIN, &self.payload)?,
        )?;
        if self.payload.joiner_peer != transport_peer.to_string() {
            return Err(PairingCodeError::TransportPeerMismatch {
                expected: self.payload.joiner_peer.clone(),
                actual: transport_peer.to_string(),
            });
        }
        decode_spake_message(&self.payload.spake_message)?;
        Ok(())
    }
}

/// Profile-free v2 hello. The network name is deliberately absent until the encrypted offer is
/// authenticated and opened.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingCodeHelloV2Payload {
    pub version: u8,
    pub locator: String,
    pub inviter_peer: String,
    pub joiner_peer: String,
    pub joiner_public_key: String,
    pub issued_at_unix_seconds: u64,
    pub spake_message: String,
}

pub struct PendingPairingCodeHelloV2 {
    state: Spake2<Ed25519Group>,
    payload: PairingCodeHelloV2Payload,
}

impl fmt::Debug for PendingPairingCodeHelloV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingPairingCodeHelloV2")
            .field("locator", &self.payload.locator)
            .field("inviter_peer", &self.payload.inviter_peer)
            .field("joiner_peer", &self.payload.joiner_peer)
            .field("state", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingCodeChallengeV2 {
    pub payload: PairingCodeChallengeV2Payload,
    pub signature: String,
}

/// Profile-free v2 challenge. Network membership data exists only in `encrypted_offer`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingCodeChallengeV2Payload {
    pub version: u8,
    pub locator: String,
    pub inviter_peer: String,
    pub inviter_public_key: String,
    pub joiner_peer: String,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub spake_message: String,
    pub nonce: String,
    pub encrypted_offer: String,
}

#[derive(Clone, Eq, PartialEq)]
pub struct PairingCodeSession {
    protocol_version: u8,
    network_name: String,
    locator: String,
    inviter_peer: String,
    joiner_peer: String,
    rendezvous_token: String,
    expires_at_unix_seconds: u64,
    confirmation_key: [u8; PAIRING_CODE_SESSION_KEY_BYTES],
}

impl PairingCodeSession {
    #[must_use]
    pub const fn protocol_version(&self) -> u8 {
        self.protocol_version
    }

    #[must_use]
    pub fn network_name(&self) -> &str {
        &self.network_name
    }

    #[must_use]
    pub fn locator(&self) -> &str {
        &self.locator
    }

    #[must_use]
    pub fn inviter_peer(&self) -> &str {
        &self.inviter_peer
    }

    #[must_use]
    pub fn joiner_peer(&self) -> &str {
        &self.joiner_peer
    }

    #[must_use]
    pub fn rendezvous_token(&self) -> &str {
        &self.rendezvous_token
    }

    #[must_use]
    pub const fn expires_at_unix_seconds(&self) -> u64 {
        self.expires_at_unix_seconds
    }
}

impl fmt::Debug for PairingCodeSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingCodeSession")
            .field("protocol_version", &self.protocol_version)
            .field("network_name", &self.network_name)
            .field("locator", &self.locator)
            .field("inviter_peer", &self.inviter_peer)
            .field("joiner_peer", &self.joiner_peer)
            .field("rendezvous_token", &"[REDACTED]")
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .field("confirmation_key", &"[REDACTED]")
            .finish()
    }
}

pub fn start_pairing_code_hello_at(
    code: &PairingCode,
    network_name: &str,
    identity: &NodeIdentity,
    inviter_peer: Libp2pPeerId,
    issued_at_unix_seconds: u64,
) -> Result<(PairingCodeHello, PendingPairingCodeHello), PairingCodeError> {
    validate_network_name(network_name)?;
    let joiner_peer = identity.peer_id.parse::<Libp2pPeerId>()?;
    let locator = code.locator(network_name)?;
    let (state, message) = Spake2::<Ed25519Group>::start_a(
        &code.password(),
        &spake_identity(network_name, "joiner", joiner_peer),
        &spake_identity(network_name, "inviter", inviter_peer),
    );
    let payload = PairingCodeHelloPayload {
        version: PAIRING_CODE_VERSION,
        network_name: network_name.to_owned(),
        locator,
        inviter_peer: inviter_peer.to_string(),
        joiner_peer: joiner_peer.to_string(),
        joiner_public_key: STANDARD.encode(identity.public_key_protobuf()?),
        issued_at_unix_seconds,
        spake_message: STANDARD.encode(message),
    };
    let signature =
        STANDARD.encode(identity.sign(&signing_message(HELLO_SIGNING_DOMAIN, &payload)?)?);
    let hello = PairingCodeHello {
        payload: payload.clone(),
        signature,
    };
    let pending = PendingPairingCodeHello { state, payload };
    Ok((hello, pending))
}

pub fn start_pairing_code_hello_v2_at(
    code: &PairingCode,
    identity: &NodeIdentity,
    inviter_peer: Libp2pPeerId,
    issued_at_unix_seconds: u64,
) -> Result<(PairingCodeHelloV2, PendingPairingCodeHelloV2), PairingCodeError> {
    let joiner_peer = identity.peer_id.parse::<Libp2pPeerId>()?;
    let locator = code.global_locator();
    let (state, message) = Spake2::<Ed25519Group>::start_a(
        &code.password(),
        &spake_identity_v2(&locator, "joiner", inviter_peer, joiner_peer),
        &spake_identity_v2(&locator, "inviter", inviter_peer, joiner_peer),
    );
    let payload = PairingCodeHelloV2Payload {
        version: PAIRING_CODE_V2_VERSION,
        locator,
        inviter_peer: inviter_peer.to_string(),
        joiner_peer: joiner_peer.to_string(),
        joiner_public_key: STANDARD.encode(identity.public_key_protobuf()?),
        issued_at_unix_seconds,
        spake_message: STANDARD.encode(message),
    };
    let signature =
        STANDARD.encode(identity.sign(&signing_message(HELLO_V2_SIGNING_DOMAIN, &payload)?)?);
    let hello = PairingCodeHelloV2 {
        payload: payload.clone(),
        signature,
    };
    let pending = PendingPairingCodeHelloV2 { state, payload };
    Ok((hello, pending))
}

pub fn answer_pairing_code_hello_at(
    config: &Config,
    code: &PairingCode,
    hello: &PairingCodeHello,
    transport_peer: Libp2pPeerId,
    offer_options: PairingOfferOptions,
    issued_at_unix_seconds: u64,
) -> Result<(PairingCodeChallenge, PairingCodeSession), PairingCodeError> {
    hello.verify_for_transport_peer_at(transport_peer, issued_at_unix_seconds)?;
    if hello.payload.network_name != config.network.name {
        return Err(PairingCodeError::NetworkMismatch {
            expected: config.network.name.clone(),
            actual: hello.payload.network_name.clone(),
        });
    }
    let expected_locator = code.locator(&config.network.name)?;
    if hello.payload.locator != expected_locator {
        return Err(PairingCodeError::LocatorMismatch);
    }
    let identity = NodeIdentity::from_private_key(
        config
            .network
            .private_key
            .as_deref()
            .ok_or(PairingCodeError::MissingPrivateKey)?,
    )?;
    let inviter_peer = identity.peer_id.parse::<Libp2pPeerId>()?;
    if hello.payload.inviter_peer != inviter_peer.to_string() {
        return Err(PairingCodeError::InviterMismatch {
            expected: inviter_peer.to_string(),
            actual: hello.payload.inviter_peer.clone(),
        });
    }

    let (state, response_message) = Spake2::<Ed25519Group>::start_b(
        &code.password(),
        &spake_identity(&config.network.name, "joiner", transport_peer),
        &spake_identity(&config.network.name, "inviter", inviter_peer),
    );
    let shared_secret = state.finish(&decode_spake_message(&hello.payload.spake_message)?)?;
    let keys = derive_session_keys(
        &shared_secret,
        &config.network.name,
        &expected_locator,
        inviter_peer,
        transport_peer,
    )?;
    let offer = export_code_pairing_offer_at(config, offer_options, issued_at_unix_seconds)?;
    let mut nonce = [0_u8; PAIRING_CODE_NONCE_BYTES];
    OsRng.fill_bytes(&mut nonce);
    let mut payload = PairingCodeChallengePayload {
        version: PAIRING_CODE_VERSION,
        network_name: config.network.name.clone(),
        locator: expected_locator,
        inviter_peer: inviter_peer.to_string(),
        inviter_public_key: STANDARD.encode(identity.public_key_protobuf()?),
        joiner_peer: transport_peer.to_string(),
        issued_at_unix_seconds,
        expires_at_unix_seconds: offer.payload.expires_at_unix_seconds,
        spake_message: STANDARD.encode(response_message),
        nonce: URL_SAFE_NO_PAD.encode(nonce),
        encrypted_offer: String::new(),
    };
    let aad = challenge_aad(&hello.payload, &payload)?;
    payload.encrypted_offer = URL_SAFE_NO_PAD.encode(
        ChaCha20Poly1305::new((&keys.offer_key).into())
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &serde_json::to_vec(&offer)?,
                    aad: &aad,
                },
            )
            .map_err(|_| PairingCodeError::OfferEncryption)?,
    );
    let signature =
        STANDARD.encode(identity.sign(&signing_message(CHALLENGE_SIGNING_DOMAIN, &payload)?)?);
    let challenge = PairingCodeChallenge { payload, signature };
    let session = pairing_code_session(&challenge, &offer, keys.confirmation_key);
    Ok((challenge, session))
}

pub fn answer_pairing_code_hello_v2_at(
    config: &Config,
    code: &PairingCode,
    hello: &PairingCodeHelloV2,
    transport_peer: Libp2pPeerId,
    offer_options: PairingOfferOptions,
    issued_at_unix_seconds: u64,
) -> Result<(PairingCodeChallengeV2, PairingCodeSession), PairingCodeError> {
    hello.verify_for_transport_peer_at(transport_peer, issued_at_unix_seconds)?;
    let expected_locator = code.global_locator();
    if hello.payload.locator != expected_locator {
        return Err(PairingCodeError::LocatorMismatch);
    }
    let identity = NodeIdentity::from_private_key(
        config
            .network
            .private_key
            .as_deref()
            .ok_or(PairingCodeError::MissingPrivateKey)?,
    )?;
    let inviter_peer = identity.peer_id.parse::<Libp2pPeerId>()?;
    if hello.payload.inviter_peer != inviter_peer.to_string() {
        return Err(PairingCodeError::InviterMismatch {
            expected: inviter_peer.to_string(),
            actual: hello.payload.inviter_peer.clone(),
        });
    }

    let (state, response_message) = Spake2::<Ed25519Group>::start_b(
        &code.password(),
        &spake_identity_v2(&expected_locator, "joiner", inviter_peer, transport_peer),
        &spake_identity_v2(&expected_locator, "inviter", inviter_peer, transport_peer),
    );
    let shared_secret = state.finish(&decode_spake_message(&hello.payload.spake_message)?)?;
    let keys = derive_session_keys_v2(
        &shared_secret,
        &expected_locator,
        inviter_peer,
        transport_peer,
    )?;
    let offer = export_code_pairing_offer_at(config, offer_options, issued_at_unix_seconds)?;
    let mut nonce = [0_u8; PAIRING_CODE_NONCE_BYTES];
    OsRng.fill_bytes(&mut nonce);
    let mut payload = PairingCodeChallengeV2Payload {
        version: PAIRING_CODE_V2_VERSION,
        locator: expected_locator,
        inviter_peer: inviter_peer.to_string(),
        inviter_public_key: STANDARD.encode(identity.public_key_protobuf()?),
        joiner_peer: transport_peer.to_string(),
        issued_at_unix_seconds,
        expires_at_unix_seconds: offer.payload.expires_at_unix_seconds,
        spake_message: STANDARD.encode(response_message),
        nonce: URL_SAFE_NO_PAD.encode(nonce),
        encrypted_offer: String::new(),
    };
    let aad = challenge_v2_aad(&hello.payload, &payload)?;
    payload.encrypted_offer = URL_SAFE_NO_PAD.encode(
        ChaCha20Poly1305::new((&keys.offer_key).into())
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &serde_json::to_vec(&offer)?,
                    aad: &aad,
                },
            )
            .map_err(|_| PairingCodeError::OfferEncryption)?,
    );
    let signature =
        STANDARD.encode(identity.sign(&signing_message(CHALLENGE_V2_SIGNING_DOMAIN, &payload)?)?);
    let challenge = PairingCodeChallengeV2 { payload, signature };
    let session = pairing_code_session_v2(&challenge, &offer, keys.confirmation_key);
    Ok((challenge, session))
}

pub fn open_pairing_code_challenge_at(
    pending: PendingPairingCodeHello,
    challenge: &PairingCodeChallenge,
    transport_peer: Libp2pPeerId,
    now_unix_seconds: u64,
) -> Result<(PairingOffer, PairingCodeSession), PairingCodeError> {
    validate_version(challenge.payload.version)?;
    validate_network_name(&challenge.payload.network_name)?;
    validate_locator(&challenge.payload.locator)?;
    validate_hello_time(challenge.payload.issued_at_unix_seconds, now_unix_seconds)?;
    if challenge.payload.network_name != pending.payload.network_name {
        return Err(PairingCodeError::NetworkMismatch {
            expected: pending.payload.network_name,
            actual: challenge.payload.network_name.clone(),
        });
    }
    if challenge.payload.locator != pending.payload.locator {
        return Err(PairingCodeError::LocatorMismatch);
    }
    if challenge.payload.inviter_peer != pending.payload.inviter_peer {
        return Err(PairingCodeError::InviterMismatch {
            expected: pending.payload.inviter_peer,
            actual: challenge.payload.inviter_peer.clone(),
        });
    }
    if challenge.payload.inviter_peer != transport_peer.to_string() {
        return Err(PairingCodeError::TransportPeerMismatch {
            expected: challenge.payload.inviter_peer.clone(),
            actual: transport_peer.to_string(),
        });
    }
    if challenge.payload.joiner_peer != pending.payload.joiner_peer {
        return Err(PairingCodeError::JoinerMismatch {
            expected: pending.payload.joiner_peer,
            actual: challenge.payload.joiner_peer.clone(),
        });
    }
    if challenge.payload.expires_at_unix_seconds <= challenge.payload.issued_at_unix_seconds {
        return Err(PairingCodeError::InvalidExpiry);
    }
    if now_unix_seconds > challenge.payload.expires_at_unix_seconds {
        return Err(PairingCodeError::Expired {
            expired_at: challenge.payload.expires_at_unix_seconds,
            now: now_unix_seconds,
        });
    }
    verify_identity_signature(
        &challenge.payload.inviter_peer,
        &challenge.payload.inviter_public_key,
        &challenge.signature,
        &signing_message(CHALLENGE_SIGNING_DOMAIN, &challenge.payload)?,
    )?;

    let shared_secret = pending
        .state
        .finish(&decode_spake_message(&challenge.payload.spake_message)?)?;
    let inviter_peer = challenge.payload.inviter_peer.parse::<Libp2pPeerId>()?;
    let joiner_peer = challenge.payload.joiner_peer.parse::<Libp2pPeerId>()?;
    let keys = derive_session_keys(
        &shared_secret,
        &challenge.payload.network_name,
        &challenge.payload.locator,
        inviter_peer,
        joiner_peer,
    )?;
    let nonce = URL_SAFE_NO_PAD.decode(&challenge.payload.nonce)?;
    if nonce.len() != PAIRING_CODE_NONCE_BYTES {
        return Err(PairingCodeError::InvalidNonceLength {
            actual: nonce.len(),
            expected: PAIRING_CODE_NONCE_BYTES,
        });
    }
    let ciphertext = URL_SAFE_NO_PAD.decode(&challenge.payload.encrypted_offer)?;
    let aad = challenge_aad(&pending.payload, &challenge.payload)?;
    let offer_bytes = ChaCha20Poly1305::new((&keys.offer_key).into())
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| PairingCodeError::OfferDecryption)?;
    let offer: PairingOffer = serde_json::from_slice(&offer_bytes)?;
    offer.verify_at(now_unix_seconds)?;
    if offer.payload.network_name != challenge.payload.network_name {
        return Err(PairingCodeError::NetworkMismatch {
            expected: challenge.payload.network_name.clone(),
            actual: offer.payload.network_name.clone(),
        });
    }
    if offer.payload.inviter_peer != challenge.payload.inviter_peer {
        return Err(PairingCodeError::InviterMismatch {
            expected: challenge.payload.inviter_peer.clone(),
            actual: offer.payload.inviter_peer.clone(),
        });
    }
    if offer.payload.expires_at_unix_seconds != challenge.payload.expires_at_unix_seconds {
        return Err(PairingCodeError::InvalidExpiry);
    }
    if offer.payload.issued_at_unix_seconds != challenge.payload.issued_at_unix_seconds {
        return Err(PairingCodeError::InvalidExpiry);
    }
    let session = pairing_code_session(challenge, &offer, keys.confirmation_key);
    Ok((offer, session))
}

pub fn open_pairing_code_challenge_v2_at(
    pending: PendingPairingCodeHelloV2,
    challenge: &PairingCodeChallengeV2,
    transport_peer: Libp2pPeerId,
    now_unix_seconds: u64,
) -> Result<(PairingOffer, PairingCodeSession), PairingCodeError> {
    validate_v2_version(challenge.payload.version)?;
    validate_locator(&challenge.payload.locator)?;
    validate_hello_time(challenge.payload.issued_at_unix_seconds, now_unix_seconds)?;
    if challenge.payload.locator != pending.payload.locator {
        return Err(PairingCodeError::LocatorMismatch);
    }
    if challenge.payload.inviter_peer != pending.payload.inviter_peer {
        return Err(PairingCodeError::InviterMismatch {
            expected: pending.payload.inviter_peer,
            actual: challenge.payload.inviter_peer.clone(),
        });
    }
    if challenge.payload.inviter_peer != transport_peer.to_string() {
        return Err(PairingCodeError::TransportPeerMismatch {
            expected: challenge.payload.inviter_peer.clone(),
            actual: transport_peer.to_string(),
        });
    }
    if challenge.payload.joiner_peer != pending.payload.joiner_peer {
        return Err(PairingCodeError::JoinerMismatch {
            expected: pending.payload.joiner_peer,
            actual: challenge.payload.joiner_peer.clone(),
        });
    }
    if challenge.payload.expires_at_unix_seconds <= challenge.payload.issued_at_unix_seconds {
        return Err(PairingCodeError::InvalidExpiry);
    }
    if now_unix_seconds > challenge.payload.expires_at_unix_seconds {
        return Err(PairingCodeError::Expired {
            expired_at: challenge.payload.expires_at_unix_seconds,
            now: now_unix_seconds,
        });
    }
    verify_identity_signature(
        &challenge.payload.inviter_peer,
        &challenge.payload.inviter_public_key,
        &challenge.signature,
        &signing_message(CHALLENGE_V2_SIGNING_DOMAIN, &challenge.payload)?,
    )?;

    let shared_secret = pending
        .state
        .finish(&decode_spake_message(&challenge.payload.spake_message)?)?;
    let inviter_peer = challenge.payload.inviter_peer.parse::<Libp2pPeerId>()?;
    let joiner_peer = challenge.payload.joiner_peer.parse::<Libp2pPeerId>()?;
    let keys = derive_session_keys_v2(
        &shared_secret,
        &challenge.payload.locator,
        inviter_peer,
        joiner_peer,
    )?;
    let nonce = URL_SAFE_NO_PAD.decode(&challenge.payload.nonce)?;
    if nonce.len() != PAIRING_CODE_NONCE_BYTES {
        return Err(PairingCodeError::InvalidNonceLength {
            actual: nonce.len(),
            expected: PAIRING_CODE_NONCE_BYTES,
        });
    }
    let ciphertext = URL_SAFE_NO_PAD.decode(&challenge.payload.encrypted_offer)?;
    let aad = challenge_v2_aad(&pending.payload, &challenge.payload)?;
    let offer_bytes = ChaCha20Poly1305::new((&keys.offer_key).into())
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| PairingCodeError::OfferDecryption)?;
    let offer: PairingOffer = serde_json::from_slice(&offer_bytes)?;
    offer.verify_at(now_unix_seconds)?;
    if offer.payload.inviter_peer != challenge.payload.inviter_peer {
        return Err(PairingCodeError::InviterMismatch {
            expected: challenge.payload.inviter_peer.clone(),
            actual: offer.payload.inviter_peer.clone(),
        });
    }
    if offer.payload.expires_at_unix_seconds != challenge.payload.expires_at_unix_seconds
        || offer.payload.issued_at_unix_seconds != challenge.payload.issued_at_unix_seconds
    {
        return Err(PairingCodeError::InvalidExpiry);
    }
    let session = pairing_code_session_v2(challenge, &offer, keys.confirmation_key);
    Ok((offer, session))
}

pub fn authenticate_pairing_request(
    request: &mut PairingRequest,
    session: &PairingCodeSession,
) -> Result<(), PairingCodeError> {
    validate_request_for_session(request, session)?;
    request.code_authentication = None;
    let confirmation = pairing_request_confirmation(request, session)?;
    request.code_authentication = Some(PairingCodeAuthentication {
        locator: session.locator.clone(),
        confirmation: STANDARD.encode(confirmation),
    });
    Ok(())
}

pub fn verify_pairing_request_code_authentication(
    request: &PairingRequest,
    session: &PairingCodeSession,
) -> Result<(), PairingCodeError> {
    validate_request_for_session(request, session)?;
    let authentication = request
        .code_authentication
        .as_ref()
        .ok_or(PairingCodeError::MissingCodeAuthentication)?;
    if authentication.locator != session.locator {
        return Err(PairingCodeError::LocatorMismatch);
    }
    let received = STANDARD.decode(&authentication.confirmation)?;
    let mut verifier = <HmacSha256 as hmac::Mac>::new_from_slice(&session.confirmation_key)
        .map_err(|_| PairingCodeError::InvalidSessionKey)?;
    verifier.update(&request_confirmation_message(request, session)?);
    verifier
        .verify_slice(&received)
        .map_err(|_| PairingCodeError::InvalidCodeConfirmation)
}

pub fn pairing_request_transcript_sha256(
    request: &PairingRequest,
) -> Result<String, PairingCodeError> {
    let mut hasher = Sha256::new();
    hasher.update(REQUEST_TRANSCRIPT_DOMAIN);
    hash_length_prefixed(&mut hasher, &serde_json::to_vec(request)?);
    Ok(URL_SAFE_NO_PAD.encode(hasher.finalize()))
}

fn pairing_code_session(
    challenge: &PairingCodeChallenge,
    offer: &PairingOffer,
    confirmation_key: [u8; PAIRING_CODE_SESSION_KEY_BYTES],
) -> PairingCodeSession {
    PairingCodeSession {
        protocol_version: PAIRING_CODE_VERSION,
        network_name: challenge.payload.network_name.clone(),
        locator: challenge.payload.locator.clone(),
        inviter_peer: challenge.payload.inviter_peer.clone(),
        joiner_peer: challenge.payload.joiner_peer.clone(),
        rendezvous_token: offer.payload.rendezvous_token.clone(),
        expires_at_unix_seconds: offer.payload.expires_at_unix_seconds,
        confirmation_key,
    }
}

fn pairing_code_session_v2(
    challenge: &PairingCodeChallengeV2,
    offer: &PairingOffer,
    confirmation_key: [u8; PAIRING_CODE_SESSION_KEY_BYTES],
) -> PairingCodeSession {
    PairingCodeSession {
        protocol_version: PAIRING_CODE_V2_VERSION,
        network_name: offer.payload.network_name.clone(),
        locator: challenge.payload.locator.clone(),
        inviter_peer: challenge.payload.inviter_peer.clone(),
        joiner_peer: challenge.payload.joiner_peer.clone(),
        rendezvous_token: offer.payload.rendezvous_token.clone(),
        expires_at_unix_seconds: offer.payload.expires_at_unix_seconds,
        confirmation_key,
    }
}

fn validate_request_for_session(
    request: &PairingRequest,
    session: &PairingCodeSession,
) -> Result<(), PairingCodeError> {
    if request.payload.network_name != session.network_name {
        return Err(PairingCodeError::NetworkMismatch {
            expected: session.network_name.clone(),
            actual: request.payload.network_name.clone(),
        });
    }
    if request.payload.inviter_peer != session.inviter_peer {
        return Err(PairingCodeError::InviterMismatch {
            expected: session.inviter_peer.clone(),
            actual: request.payload.inviter_peer.clone(),
        });
    }
    if request.payload.joiner_peer != session.joiner_peer {
        return Err(PairingCodeError::JoinerMismatch {
            expected: session.joiner_peer.clone(),
            actual: request.payload.joiner_peer.clone(),
        });
    }
    if request.payload.rendezvous_token != session.rendezvous_token {
        return Err(PairingCodeError::RendezvousTokenMismatch);
    }
    Ok(())
}

fn pairing_request_confirmation(
    request: &PairingRequest,
    session: &PairingCodeSession,
) -> Result<Vec<u8>, PairingCodeError> {
    let mut mac = <HmacSha256 as hmac::Mac>::new_from_slice(&session.confirmation_key)
        .map_err(|_| PairingCodeError::InvalidSessionKey)?;
    mac.update(&request_confirmation_message(request, session)?);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn request_confirmation_message(
    request: &PairingRequest,
    session: &PairingCodeSession,
) -> Result<Vec<u8>, PairingCodeError> {
    #[derive(Serialize)]
    struct PairingRequestWithoutCodeAuthentication<'a> {
        offer: &'a Option<PairingOffer>,
        payload: &'a crate::pairing::PairingRequestPayload,
        signature: &'a str,
    }

    let mut message = match session.protocol_version {
        PAIRING_CODE_VERSION => REQUEST_CONFIRMATION_DOMAIN.to_vec(),
        PAIRING_CODE_V2_VERSION => REQUEST_V2_CONFIRMATION_DOMAIN.to_vec(),
        version => return Err(PairingCodeError::UnsupportedVersion(version)),
    };
    append_length_prefixed(&mut message, session.network_name.as_bytes());
    append_length_prefixed(&mut message, session.locator.as_bytes());
    append_length_prefixed(&mut message, session.inviter_peer.as_bytes());
    append_length_prefixed(&mut message, session.joiner_peer.as_bytes());
    append_length_prefixed(&mut message, session.rendezvous_token.as_bytes());
    message.extend(serde_json::to_vec(
        &PairingRequestWithoutCodeAuthentication {
            offer: &request.offer,
            payload: &request.payload,
            signature: &request.signature,
        },
    )?);
    Ok(message)
}

struct SessionKeys {
    offer_key: [u8; PAIRING_CODE_SESSION_KEY_BYTES],
    confirmation_key: [u8; PAIRING_CODE_SESSION_KEY_BYTES],
}

fn derive_session_keys(
    shared_secret: &[u8],
    network_name: &str,
    locator: &str,
    inviter_peer: Libp2pPeerId,
    joiner_peer: Libp2pPeerId,
) -> Result<SessionKeys, PairingCodeError> {
    let mut salt = KEY_DERIVATION_DOMAIN.to_vec();
    append_length_prefixed(&mut salt, network_name.as_bytes());
    append_length_prefixed(&mut salt, locator.as_bytes());
    append_length_prefixed(&mut salt, inviter_peer.to_bytes().as_slice());
    append_length_prefixed(&mut salt, joiner_peer.to_bytes().as_slice());
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared_secret);
    let mut offer_key = [0_u8; PAIRING_CODE_SESSION_KEY_BYTES];
    let mut confirmation_key = [0_u8; PAIRING_CODE_SESSION_KEY_BYTES];
    hkdf.expand(b"encrypted offer", &mut offer_key)?;
    hkdf.expand(b"pairing request confirmation", &mut confirmation_key)?;
    Ok(SessionKeys {
        offer_key,
        confirmation_key,
    })
}

fn derive_session_keys_v2(
    shared_secret: &[u8],
    locator: &str,
    inviter_peer: Libp2pPeerId,
    joiner_peer: Libp2pPeerId,
) -> Result<SessionKeys, PairingCodeError> {
    let mut salt = KEY_DERIVATION_V2_DOMAIN.to_vec();
    append_length_prefixed(&mut salt, &[PAIRING_CODE_V2_VERSION]);
    append_length_prefixed(&mut salt, locator.as_bytes());
    append_length_prefixed(&mut salt, inviter_peer.to_bytes().as_slice());
    append_length_prefixed(&mut salt, joiner_peer.to_bytes().as_slice());
    let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared_secret);
    let mut offer_key = [0_u8; PAIRING_CODE_SESSION_KEY_BYTES];
    let mut confirmation_key = [0_u8; PAIRING_CODE_SESSION_KEY_BYTES];
    hkdf.expand(b"encrypted offer", &mut offer_key)?;
    hkdf.expand(b"pairing request confirmation", &mut confirmation_key)?;
    Ok(SessionKeys {
        offer_key,
        confirmation_key,
    })
}

fn challenge_aad(
    hello: &PairingCodeHelloPayload,
    challenge: &PairingCodeChallengePayload,
) -> Result<Vec<u8>, PairingCodeError> {
    #[derive(Serialize)]
    struct ChallengeAad<'a> {
        hello: &'a PairingCodeHelloPayload,
        version: u8,
        network_name: &'a str,
        locator: &'a str,
        inviter_peer: &'a str,
        inviter_public_key: &'a str,
        joiner_peer: &'a str,
        issued_at_unix_seconds: u64,
        expires_at_unix_seconds: u64,
        spake_message: &'a str,
        nonce: &'a str,
    }

    let mut aad = CHALLENGE_AAD_DOMAIN.to_vec();
    aad.extend(serde_json::to_vec(&ChallengeAad {
        hello,
        version: challenge.version,
        network_name: &challenge.network_name,
        locator: &challenge.locator,
        inviter_peer: &challenge.inviter_peer,
        inviter_public_key: &challenge.inviter_public_key,
        joiner_peer: &challenge.joiner_peer,
        issued_at_unix_seconds: challenge.issued_at_unix_seconds,
        expires_at_unix_seconds: challenge.expires_at_unix_seconds,
        spake_message: &challenge.spake_message,
        nonce: &challenge.nonce,
    })?);
    Ok(aad)
}

fn challenge_v2_aad(
    hello: &PairingCodeHelloV2Payload,
    challenge: &PairingCodeChallengeV2Payload,
) -> Result<Vec<u8>, PairingCodeError> {
    #[derive(Serialize)]
    struct ChallengeV2Aad<'a> {
        hello: &'a PairingCodeHelloV2Payload,
        version: u8,
        locator: &'a str,
        inviter_peer: &'a str,
        inviter_public_key: &'a str,
        joiner_peer: &'a str,
        issued_at_unix_seconds: u64,
        expires_at_unix_seconds: u64,
        spake_message: &'a str,
        nonce: &'a str,
    }

    let mut aad = CHALLENGE_V2_AAD_DOMAIN.to_vec();
    aad.extend(serde_json::to_vec(&ChallengeV2Aad {
        hello,
        version: challenge.version,
        locator: &challenge.locator,
        inviter_peer: &challenge.inviter_peer,
        inviter_public_key: &challenge.inviter_public_key,
        joiner_peer: &challenge.joiner_peer,
        issued_at_unix_seconds: challenge.issued_at_unix_seconds,
        expires_at_unix_seconds: challenge.expires_at_unix_seconds,
        spake_message: &challenge.spake_message,
        nonce: &challenge.nonce,
    })?);
    Ok(aad)
}

fn spake_identity(network_name: &str, role: &str, peer: Libp2pPeerId) -> SpakeIdentity {
    let mut identity = SPAKE_IDENTITY_DOMAIN.to_vec();
    append_length_prefixed(&mut identity, network_name.as_bytes());
    append_length_prefixed(&mut identity, role.as_bytes());
    append_length_prefixed(&mut identity, peer.to_bytes().as_slice());
    SpakeIdentity::new(&identity)
}

fn spake_identity_v2(
    locator: &str,
    role: &str,
    inviter_peer: Libp2pPeerId,
    joiner_peer: Libp2pPeerId,
) -> SpakeIdentity {
    let mut identity = SPAKE_V2_IDENTITY_DOMAIN.to_vec();
    append_length_prefixed(&mut identity, &[PAIRING_CODE_V2_VERSION]);
    append_length_prefixed(&mut identity, locator.as_bytes());
    append_length_prefixed(&mut identity, role.as_bytes());
    append_length_prefixed(&mut identity, inviter_peer.to_bytes().as_slice());
    append_length_prefixed(&mut identity, joiner_peer.to_bytes().as_slice());
    SpakeIdentity::new(&identity)
}

fn verify_identity_signature(
    claimed_peer: &str,
    encoded_public_key: &str,
    encoded_signature: &str,
    message: &[u8],
) -> Result<(), PairingCodeError> {
    let public_key = PublicKey::try_decode_protobuf(&STANDARD.decode(encoded_public_key)?)?;
    let peer = claimed_peer.parse::<Libp2pPeerId>()?;
    if public_key.to_peer_id() != peer {
        return Err(PairingCodeError::PublicKeyPeerMismatch {
            expected: claimed_peer.to_owned(),
            actual: public_key.to_peer_id().to_string(),
        });
    }
    let signature = STANDARD.decode(encoded_signature)?;
    if !public_key.verify(message, &signature) {
        return Err(PairingCodeError::InvalidSignature);
    }
    Ok(())
}

fn signing_message<T: Serialize>(domain: &[u8], payload: &T) -> Result<Vec<u8>, PairingCodeError> {
    let mut message = domain.to_vec();
    message.extend(serde_json::to_vec(payload)?);
    Ok(message)
}

fn decode_spake_message(encoded: &str) -> Result<Vec<u8>, PairingCodeError> {
    let message = STANDARD.decode(encoded)?;
    if message.len() != 33 {
        return Err(PairingCodeError::InvalidSpakeMessageLength {
            actual: message.len(),
            expected: 33,
        });
    }
    Ok(message)
}

fn validate_version(version: u8) -> Result<(), PairingCodeError> {
    if version == PAIRING_CODE_VERSION {
        Ok(())
    } else {
        Err(PairingCodeError::UnsupportedVersion(version))
    }
}

fn validate_v2_version(version: u8) -> Result<(), PairingCodeError> {
    if version == PAIRING_CODE_V2_VERSION {
        Ok(())
    } else {
        Err(PairingCodeError::UnsupportedVersion(version))
    }
}

fn validate_network_name(network_name: &str) -> Result<(), PairingCodeError> {
    if network_name.is_empty() {
        Err(PairingCodeError::EmptyNetworkName)
    } else {
        Ok(())
    }
}

fn validate_locator(locator: &str) -> Result<(), PairingCodeError> {
    let decoded = URL_SAFE_NO_PAD.decode(locator)?;
    if decoded.len() != 32 {
        return Err(PairingCodeError::InvalidLocatorLength {
            actual: decoded.len(),
            expected: 32,
        });
    }
    Ok(())
}

fn validate_hello_time(
    issued_at_unix_seconds: u64,
    now_unix_seconds: u64,
) -> Result<(), PairingCodeError> {
    if issued_at_unix_seconds > now_unix_seconds.saturating_add(PAIRING_CODE_CLOCK_SKEW_SECONDS) {
        return Err(PairingCodeError::IssuedInFuture {
            issued_at: issued_at_unix_seconds,
            now: now_unix_seconds,
        });
    }
    if now_unix_seconds.saturating_sub(issued_at_unix_seconds) > PAIRING_CODE_HELLO_MAX_AGE_SECONDS
    {
        return Err(PairingCodeError::StaleHello {
            issued_at: issued_at_unix_seconds,
            now: now_unix_seconds,
        });
    }
    Ok(())
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn append_length_prefixed(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    output.extend(bytes);
}

#[derive(Debug)]
pub enum PairingCodeError {
    InvalidCodeCharacter(char),
    InvalidCodeLength { actual: usize, expected: usize },
    InvalidCodeByteLength { actual: usize, expected: usize },
    InvalidCodeEncoding,
    UnsupportedVersion(u8),
    EmptyNetworkName,
    InvalidLocatorLength { actual: usize, expected: usize },
    InvalidSpakeMessageLength { actual: usize, expected: usize },
    InvalidNonceLength { actual: usize, expected: usize },
    MissingPrivateKey,
    MissingCodeAuthentication,
    InvalidSessionKey,
    InvalidCodeConfirmation,
    InvalidSignature,
    InvalidExpiry,
    OfferEncryption,
    OfferDecryption,
    LocatorMismatch,
    RendezvousTokenMismatch,
    NetworkMismatch { expected: String, actual: String },
    InviterMismatch { expected: String, actual: String },
    JoinerMismatch { expected: String, actual: String },
    TransportPeerMismatch { expected: String, actual: String },
    PublicKeyPeerMismatch { expected: String, actual: String },
    IssuedInFuture { issued_at: u64, now: u64 },
    StaleHello { issued_at: u64, now: u64 },
    Expired { expired_at: u64, now: u64 },
    Base64(base64::DecodeError),
    Json(serde_json::Error),
    Identity(crate::identity::IdentityError),
    Pairing(PairingError),
    Libp2pIdentity(libp2p::identity::DecodingError),
    Libp2pPeerId(libp2p::identity::ParseError),
    Spake(spake2::Error),
    Hkdf(hkdf::InvalidLength),
}

impl From<base64::DecodeError> for PairingCodeError {
    fn from(error: base64::DecodeError) -> Self {
        Self::Base64(error)
    }
}

impl From<serde_json::Error> for PairingCodeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<crate::identity::IdentityError> for PairingCodeError {
    fn from(error: crate::identity::IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<PairingError> for PairingCodeError {
    fn from(error: PairingError) -> Self {
        Self::Pairing(error)
    }
}

impl From<libp2p::identity::DecodingError> for PairingCodeError {
    fn from(error: libp2p::identity::DecodingError) -> Self {
        Self::Libp2pIdentity(error)
    }
}

impl From<libp2p::identity::ParseError> for PairingCodeError {
    fn from(error: libp2p::identity::ParseError) -> Self {
        Self::Libp2pPeerId(error)
    }
}

impl From<spake2::Error> for PairingCodeError {
    fn from(error: spake2::Error) -> Self {
        Self::Spake(error)
    }
}

impl From<hkdf::InvalidLength> for PairingCodeError {
    fn from(error: hkdf::InvalidLength) -> Self {
        Self::Hkdf(error)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        config::{
            DiscoveryConfig, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig, RelayConfig,
            ResourceConfig,
        },
        pairing::{PairingRequestOptions, build_pairing_request_at},
    };

    use super::*;

    fn config(identity: &NodeIdentity) -> Config {
        Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "runners".to_owned(),
                local_peer: identity.peer_id.clone(),
                private_key: Some(identity.private_key.clone()),
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: vec!["/ip4/127.0.0.1/tcp/4001".to_owned()],
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1_280,
            },
            peers: Vec::<PeerConfig>::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        }
    }

    fn code(byte: u8) -> PairingCode {
        PairingCode([byte; PAIRING_CODE_BYTES])
    }

    fn exchange() -> (
        NodeIdentity,
        PairingCodeSession,
        PairingCodeSession,
        PairingOffer,
    ) {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let inviter_peer = inviter.peer_id.parse().expect("inviter peer");
        let joiner_peer = joiner.peer_id.parse().expect("joiner peer");
        let code = code(7);
        let (hello, pending) =
            start_pairing_code_hello_at(&code, "runners", &joiner, inviter_peer, 1_000)
                .expect("hello");
        let (challenge, inviter_session) = answer_pairing_code_hello_at(
            &config(&inviter),
            &code,
            &hello,
            joiner_peer,
            PairingOfferOptions::default(),
            1_001,
        )
        .expect("challenge");
        let (offer, joiner_session) =
            open_pairing_code_challenge_at(pending, &challenge, inviter_peer, 1_002)
                .expect("open challenge");
        (joiner, inviter_session, joiner_session, offer)
    }

    fn exchange_v2() -> (
        NodeIdentity,
        PairingCodeSession,
        PairingCodeSession,
        PairingOffer,
        PairingCodeHelloV2,
        PairingCodeChallengeV2,
    ) {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let inviter_peer = inviter.peer_id.parse().expect("inviter peer");
        let joiner_peer = joiner.peer_id.parse().expect("joiner peer");
        let code = code(9);
        let (hello, pending) =
            start_pairing_code_hello_v2_at(&code, &joiner, inviter_peer, 1_000).expect("hello");
        let (challenge, inviter_session) = answer_pairing_code_hello_v2_at(
            &config(&inviter),
            &code,
            &hello,
            joiner_peer,
            PairingOfferOptions::default(),
            1_001,
        )
        .expect("challenge");
        let (offer, joiner_session) =
            open_pairing_code_challenge_v2_at(pending, &challenge, inviter_peer, 1_002)
                .expect("open challenge");
        (
            joiner,
            inviter_session,
            joiner_session,
            offer,
            hello,
            challenge,
        )
    }

    #[test]
    fn pairing_code_round_trips_human_format() {
        let code = code(0xa5);
        let rendered = code.to_string();

        assert_eq!(rendered.len(), 19);
        assert_eq!(rendered.matches('-').count(), 3);
        assert_eq!(rendered.parse::<PairingCode>().expect("code"), code);
        assert_eq!(
            rendered
                .to_ascii_lowercase()
                .parse::<PairingCode>()
                .expect("lowercase"),
            code
        );
        assert_eq!(format!("{code:?}"), "PairingCode([REDACTED])");
    }

    #[test]
    fn pairing_code_locator_is_network_bound_and_does_not_contain_code() {
        let code = code(3);
        let runners = code.locator("runners").expect("locator");
        let lab = code.locator("lab").expect("locator");

        assert_ne!(runners, lab);
        assert!(!runners.contains(&code.to_string()));
        assert_eq!(URL_SAFE_NO_PAD.decode(runners).expect("locator").len(), 32);
    }

    #[test]
    fn pairing_code_v2_locator_is_profile_independent_and_domain_separated() {
        let code = code(3);
        let global = code.global_locator();

        assert_eq!(global, code.global_locator());
        assert_ne!(global, code.locator("runners").expect("v1 locator"));
        assert_ne!(global, code.locator("lab").expect("v1 locator"));
        assert!(!global.contains(&code.to_string()));
        assert_eq!(URL_SAFE_NO_PAD.decode(global).expect("locator").len(), 32);
    }

    #[test]
    fn pairing_code_v2_hides_network_until_offer_is_opened() {
        let (_, _, _, offer, hello, challenge) = exchange_v2();
        let hello_json = serde_json::to_string(&hello).expect("serialize hello");
        let challenge_json = serde_json::to_string(&challenge).expect("serialize challenge");

        assert_eq!(offer.payload.network_name, "runners");
        assert!(!hello_json.contains("network_name"));
        assert!(!hello_json.contains("runners"));
        assert!(!challenge_json.contains("network_name"));
        assert!(!challenge_json.contains("runners"));
    }

    #[test]
    fn pairing_code_v2_exchange_authenticates_existing_pairing_request() {
        let (joiner, inviter_session, joiner_session, offer, _, _) = exchange_v2();
        let mut request = build_pairing_request_at(
            &offer,
            PairingRequestOptions {
                identity: joiner,
                requested_vpn_ip: None,
                requested_routes: Vec::new(),
            },
            1_003,
        )
        .expect("request");

        authenticate_pairing_request(&mut request, &joiner_session).expect("authenticate");
        verify_pairing_request_code_authentication(&request, &inviter_session).expect("verify");
        assert_eq!(inviter_session, joiner_session);
        assert_eq!(joiner_session.protocol_version(), PAIRING_CODE_V2_VERSION);
        assert_eq!(joiner_session.network_name(), "runners");
    }

    #[test]
    fn pairing_code_v2_rejects_wrong_code_even_with_observed_locator() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let inviter_peer = inviter.peer_id.parse().expect("inviter peer");
        let joiner_peer = joiner.peer_id.parse().expect("joiner peer");
        let expected_code = code(1);
        let guessed_code = code(2);
        let (mut hello, mut pending) =
            start_pairing_code_hello_v2_at(&guessed_code, &joiner, inviter_peer, 1_000)
                .expect("hello");
        hello.payload.locator = expected_code.global_locator();
        pending.payload.locator.clone_from(&hello.payload.locator);
        hello.signature = STANDARD.encode(
            joiner
                .sign(
                    &signing_message(HELLO_V2_SIGNING_DOMAIN, &hello.payload)
                        .expect("signing message"),
                )
                .expect("signature"),
        );
        let (challenge, _) = answer_pairing_code_hello_v2_at(
            &config(&inviter),
            &expected_code,
            &hello,
            joiner_peer,
            PairingOfferOptions::default(),
            1_001,
        )
        .expect("challenge");

        let error = open_pairing_code_challenge_v2_at(pending, &challenge, inviter_peer, 1_002)
            .expect_err("wrong SPAKE2 password");

        assert!(matches!(error, PairingCodeError::OfferDecryption));
    }

    #[test]
    fn pairing_code_v2_challenge_binds_locator_and_transport_peer() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let other = NodeIdentity::generate_ed25519().expect("other");
        let inviter_peer = inviter.peer_id.parse().expect("inviter peer");
        let joiner_peer = joiner.peer_id.parse().expect("joiner peer");
        let other_peer = other.peer_id.parse().expect("other peer");
        let code = code(4);
        let (hello, pending) =
            start_pairing_code_hello_v2_at(&code, &joiner, inviter_peer, 1_000).expect("hello");
        let (challenge, _) = answer_pairing_code_hello_v2_at(
            &config(&inviter),
            &code,
            &hello,
            joiner_peer,
            PairingOfferOptions::default(),
            1_001,
        )
        .expect("challenge");

        let error = open_pairing_code_challenge_v2_at(pending, &challenge, other_peer, 1_002)
            .expect_err("transport mismatch");

        assert!(matches!(
            error,
            PairingCodeError::TransportPeerMismatch { .. }
        ));
    }

    #[test]
    fn pairing_code_exchange_authenticates_existing_pairing_request() {
        let (joiner, inviter_session, joiner_session, offer) = exchange();
        assert_eq!(
            offer.payload.acceptance_mode,
            crate::pairing::PairingAcceptanceMode::CodeApproval
        );
        let mut request = build_pairing_request_at(
            &offer,
            PairingRequestOptions {
                identity: joiner,
                requested_vpn_ip: None,
                requested_routes: Vec::new(),
            },
            1_003,
        )
        .expect("request");

        authenticate_pairing_request(&mut request, &joiner_session).expect("authenticate");
        verify_pairing_request_code_authentication(&request, &inviter_session).expect("verify");
        assert_eq!(inviter_session, joiner_session);
        assert!(request.code_authentication.is_some());
    }

    #[test]
    fn pairing_request_transcript_digest_is_stable_and_binds_authenticated_request() {
        let (joiner, _, joiner_session, offer) = exchange();
        let mut request = build_pairing_request_at(
            &offer,
            PairingRequestOptions {
                identity: joiner,
                requested_vpn_ip: None,
                requested_routes: Vec::new(),
            },
            1_003,
        )
        .expect("request");
        authenticate_pairing_request(&mut request, &joiner_session).expect("authenticate");

        let digest = pairing_request_transcript_sha256(&request).expect("transcript digest");
        let restored: PairingRequest = serde_json::from_slice(
            &serde_json::to_vec(&request).expect("serialize authenticated request"),
        )
        .expect("restore authenticated request");
        assert_eq!(
            pairing_request_transcript_sha256(&restored).expect("restored transcript digest"),
            digest
        );
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(&digest)
                .expect("decode digest")
                .len(),
            32
        );

        let mut tampered = request;
        tampered.payload.requested_vpn_ip = Some("10.42.0.9".to_owned());
        assert_ne!(
            pairing_request_transcript_sha256(&tampered).expect("tampered transcript digest"),
            digest
        );
    }

    #[test]
    fn pairing_code_exchange_rejects_wrong_code() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let inviter_peer = inviter.peer_id.parse().expect("inviter peer");
        let joiner_peer = joiner.peer_id.parse().expect("joiner peer");
        let (hello, _) =
            start_pairing_code_hello_at(&code(1), "runners", &joiner, inviter_peer, 1_000)
                .expect("hello");

        let error = answer_pairing_code_hello_at(
            &config(&inviter),
            &code(2),
            &hello,
            joiner_peer,
            PairingOfferOptions::default(),
            1_001,
        )
        .expect_err("wrong code");

        assert!(matches!(error, PairingCodeError::LocatorMismatch));
    }

    #[test]
    fn pairing_code_exchange_rejects_locator_observer_without_code() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let inviter_peer = inviter.peer_id.parse().expect("inviter peer");
        let joiner_peer = joiner.peer_id.parse().expect("joiner peer");
        let expected_code = code(1);
        let guessed_code = code(2);
        let (mut hello, mut pending) =
            start_pairing_code_hello_at(&guessed_code, "runners", &joiner, inviter_peer, 1_000)
                .expect("hello");
        hello.payload.locator = expected_code.locator("runners").expect("locator");
        pending.payload.locator.clone_from(&hello.payload.locator);
        hello.signature = STANDARD.encode(
            joiner
                .sign(
                    &signing_message(HELLO_SIGNING_DOMAIN, &hello.payload)
                        .expect("signing message"),
                )
                .expect("signature"),
        );
        let (challenge, _) = answer_pairing_code_hello_at(
            &config(&inviter),
            &expected_code,
            &hello,
            joiner_peer,
            PairingOfferOptions::default(),
            1_001,
        )
        .expect("challenge");

        let error = open_pairing_code_challenge_at(pending, &challenge, inviter_peer, 1_002)
            .expect_err("wrong SPAKE2 password");

        assert!(matches!(error, PairingCodeError::OfferDecryption));
    }

    #[test]
    fn pairing_code_confirmation_rejects_request_tampering() {
        let (joiner, inviter_session, joiner_session, offer) = exchange();
        let mut request = build_pairing_request_at(
            &offer,
            PairingRequestOptions {
                identity: joiner,
                requested_vpn_ip: None,
                requested_routes: Vec::new(),
            },
            1_003,
        )
        .expect("request");
        authenticate_pairing_request(&mut request, &joiner_session).expect("authenticate");
        request.payload.requested_vpn_ip = Some("10.42.0.9".to_owned());

        let error = verify_pairing_request_code_authentication(&request, &inviter_session)
            .expect_err("tampered request");

        assert!(matches!(error, PairingCodeError::InvalidCodeConfirmation));
    }

    #[test]
    fn pairing_code_hello_rejects_transport_identity_mismatch() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let other = NodeIdentity::generate_ed25519().expect("other");
        let inviter_peer = inviter.peer_id.parse().expect("inviter peer");
        let other_peer = other.peer_id.parse().expect("other peer");
        let (hello, _) =
            start_pairing_code_hello_at(&code(1), "runners", &joiner, inviter_peer, 1_000)
                .expect("hello");

        let error = hello
            .verify_for_transport_peer_at(other_peer, 1_001)
            .expect_err("transport mismatch");

        assert!(matches!(
            error,
            PairingCodeError::TransportPeerMismatch { .. }
        ));
    }

    #[test]
    fn pairing_code_challenge_rejects_expiry() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let inviter_peer = inviter.peer_id.parse().expect("inviter peer");
        let joiner_peer = joiner.peer_id.parse().expect("joiner peer");
        let code = code(7);
        let (hello, pending) =
            start_pairing_code_hello_at(&code, "runners", &joiner, inviter_peer, 1_000)
                .expect("hello");
        let (challenge, _) = answer_pairing_code_hello_at(
            &config(&inviter),
            &code,
            &hello,
            joiner_peer,
            PairingOfferOptions {
                expires_in_seconds: 2,
                rendezvous_token: None,
            },
            1_001,
        )
        .expect("challenge");

        let error = open_pairing_code_challenge_at(pending, &challenge, inviter_peer, 1_004)
            .expect_err("expired challenge");

        assert!(matches!(error, PairingCodeError::Expired { .. }));
    }
}
