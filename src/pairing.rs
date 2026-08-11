use std::time::{SystemTime, UNIX_EPOCH};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use libp2p::{Multiaddr, PeerId as Libp2pPeerId, identity::PublicKey};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::{
    config::{
        BootstrapPeerConfig, Config, DiscoveryConfig, InterfaceConfig, PeerConfig, QueueConfig,
        RelayConfig, ResourceConfig, RouteConfig, public_ipfs_bootstrap_peer_configs,
    },
    identity::NodeIdentity,
    membership::{MembershipRole, SignedMembershipRecord, validate_membership_records_at},
    runtime::{control::CONTROL_PROTOCOL, packet::PACKET_PROTOCOL, service::SERVICE_PROTOCOL},
    wire::{HEADER_LEN, WIRE_VERSION},
};

pub const PAIRING_OFFER_VERSION: u8 = 1;
pub const DEFAULT_PAIRING_EXPIRES_IN_SECONDS: u64 = 600;

const PAIRING_URI_PREFIX: &str = "p2pvpn:";
const SIGNING_DOMAIN: &[u8] = b"p2p-vpn pairing offer v1\n";
const REQUEST_SIGNING_DOMAIN: &[u8] = b"p2p-vpn pairing request v1\n";
const RESPONSE_SIGNING_DOMAIN: &[u8] = b"p2p-vpn pairing response v1\n";
const RENDEZVOUS_TOKEN_LEN: usize = 16;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingOffer {
    pub payload: PairingOfferPayload,
    pub signature: String,
}

impl PairingOffer {
    pub fn verify_at(&self, now_unix_seconds: u64) -> Result<(), PairingError> {
        validate_payload(&self.payload, now_unix_seconds)?;
        let public_key = decode_public_key(&self.payload.inviter_public_key)?;
        let inviter_peer = self.payload.inviter_peer.parse::<Libp2pPeerId>()?;
        if public_key.to_peer_id() != inviter_peer {
            return Err(PairingError::PublicKeyPeerMismatch {
                expected: self.payload.inviter_peer.clone(),
                actual: public_key.to_peer_id().to_string(),
            });
        }
        let signature = STANDARD.decode(&self.signature)?;
        if !public_key.verify(&signing_message(&self.payload)?, &signature) {
            return Err(PairingError::InvalidSignature);
        }

        Ok(())
    }

    pub fn to_uri(&self) -> Result<String, PairingError> {
        let bytes = serde_json::to_vec(self)?;
        Ok(format!(
            "{PAIRING_URI_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(bytes)
        ))
    }

    pub fn from_uri(input: &str) -> Result<Self, PairingError> {
        let encoded = input
            .strip_prefix(PAIRING_URI_PREFIX)
            .ok_or(PairingError::InvalidUriScheme)?;
        let bytes = URL_SAFE_NO_PAD.decode(encoded)?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingOfferPayload {
    pub version: u8,
    pub network_name: String,
    pub inviter_peer: String,
    pub inviter_public_key: String,
    pub rendezvous_token: String,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    #[serde(default)]
    pub inviter_addresses: Vec<String>,
    #[serde(default)]
    pub bootstrap_peers: Vec<BootstrapPeerConfig>,
    pub discovery: DiscoveryConfig,
    pub protocols: PairingProtocols,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingProtocols {
    pub control: String,
    pub packet: String,
    pub service: String,
    pub wire_version: u8,
    pub packet_header_len: usize,
}

impl Default for PairingProtocols {
    fn default() -> Self {
        Self {
            control: CONTROL_PROTOCOL.to_owned(),
            packet: PACKET_PROTOCOL.to_owned(),
            service: SERVICE_PROTOCOL.to_owned(),
            wire_version: WIRE_VERSION,
            packet_header_len: HEADER_LEN,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingRequest {
    pub payload: PairingRequestPayload,
    pub signature: String,
}

impl PairingRequest {
    pub fn verify_for_offer_at(
        &self,
        offer: &PairingOffer,
        now_unix_seconds: u64,
    ) -> Result<(), PairingError> {
        offer.verify_at(now_unix_seconds)?;
        validate_request_payload(&self.payload, offer, now_unix_seconds)?;
        let public_key = decode_public_key(&self.payload.joiner_public_key)?;
        let joiner_peer = self.payload.joiner_peer.parse::<Libp2pPeerId>()?;
        if public_key.to_peer_id() != joiner_peer {
            return Err(PairingError::PublicKeyPeerMismatch {
                expected: self.payload.joiner_peer.clone(),
                actual: public_key.to_peer_id().to_string(),
            });
        }
        let signature = STANDARD.decode(&self.signature)?;
        if !public_key.verify(&request_signing_message(&self.payload)?, &signature) {
            return Err(PairingError::InvalidSignature);
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingRequestPayload {
    pub version: u8,
    pub network_name: String,
    pub inviter_peer: String,
    pub joiner_peer: String,
    pub joiner_public_key: String,
    pub rendezvous_token: String,
    pub issued_at_unix_seconds: u64,
    #[serde(default)]
    pub requested_vpn_ip: Option<String>,
    #[serde(default)]
    pub requested_routes: Vec<RouteConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingResponse {
    pub payload: PairingResponsePayload,
    pub signature: String,
}

impl PairingResponse {
    pub fn verify_for_offer_at(
        &self,
        offer: &PairingOffer,
        joiner_identity: &NodeIdentity,
        now_unix_seconds: u64,
    ) -> Result<(), PairingError> {
        offer.verify_at(now_unix_seconds)?;
        validate_response_payload(&self.payload, offer, joiner_identity, now_unix_seconds)?;
        let public_key = decode_public_key(&self.payload.inviter_public_key)?;
        let inviter_peer = self.payload.inviter_peer.parse::<Libp2pPeerId>()?;
        if public_key.to_peer_id() != inviter_peer {
            return Err(PairingError::PublicKeyPeerMismatch {
                expected: self.payload.inviter_peer.clone(),
                actual: public_key.to_peer_id().to_string(),
            });
        }
        let signature = STANDARD.decode(&self.signature)?;
        if !public_key.verify(&response_signing_message(&self.payload)?, &signature) {
            return Err(PairingError::InvalidSignature);
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingResponsePayload {
    pub version: u8,
    pub network_name: String,
    pub inviter_peer: String,
    pub inviter_public_key: String,
    pub joiner_peer: String,
    pub rendezvous_token: String,
    pub issued_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    #[serde(default)]
    pub assigned_vpn_ip: Option<String>,
    #[serde(default)]
    pub membership_key: Option<String>,
    #[serde(default)]
    pub member_records: Vec<SignedMembershipRecord>,
    #[serde(default)]
    pub inviter_addresses: Vec<String>,
    #[serde(default)]
    pub inviter_routes: Vec<RouteConfig>,
    #[serde(default)]
    pub bootstrap_peers: Vec<BootstrapPeerConfig>,
    #[serde(default)]
    pub relay_reservations: Vec<String>,
    pub discovery: DiscoveryConfig,
    pub protocols: PairingProtocols,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingOfferOptions {
    pub expires_in_seconds: u64,
    pub rendezvous_token: Option<String>,
}

impl Default for PairingOfferOptions {
    fn default() -> Self {
        Self {
            expires_in_seconds: DEFAULT_PAIRING_EXPIRES_IN_SECONDS,
            rendezvous_token: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingRequestOptions {
    pub identity: NodeIdentity,
    pub requested_vpn_ip: Option<String>,
    pub requested_routes: Vec<RouteConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingResponseOptions {
    pub joiner_peer: String,
    pub assigned_vpn_ip: Option<String>,
    pub membership_key: Option<String>,
    pub member_records: Vec<SignedMembershipRecord>,
    pub expires_in_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingConfigOptions {
    pub identity: NodeIdentity,
    pub interface_name: String,
    pub mtu: u16,
    pub local_routes: Vec<RouteConfig>,
    pub peer_name: Option<String>,
}

pub fn export_pairing_offer(
    config: &Config,
    options: PairingOfferOptions,
) -> Result<PairingOffer, PairingError> {
    export_pairing_offer_at(config, options, current_unix_seconds()?)
}

pub fn export_pairing_offer_at(
    config: &Config,
    options: PairingOfferOptions,
    issued_at_unix_seconds: u64,
) -> Result<PairingOffer, PairingError> {
    config.validate_runtime()?;
    if options.expires_in_seconds == 0 {
        return Err(PairingError::InvalidExpiry);
    }
    let identity = NodeIdentity::from_private_key(
        config
            .network
            .private_key
            .as_deref()
            .ok_or(PairingError::MissingPrivateKey)?,
    )?;
    let expires_at_unix_seconds = issued_at_unix_seconds
        .checked_add(options.expires_in_seconds)
        .ok_or(PairingError::ExpiryOverflow)?;
    let payload = PairingOfferPayload {
        version: PAIRING_OFFER_VERSION,
        network_name: config.network.name.clone(),
        inviter_peer: config.network.local_peer.clone(),
        inviter_public_key: STANDARD.encode(identity.public_key_protobuf()?),
        rendezvous_token: match options.rendezvous_token {
            Some(token) => validate_rendezvous_token(&token)?,
            None => generate_rendezvous_token(),
        },
        issued_at_unix_seconds,
        expires_at_unix_seconds,
        inviter_addresses: exported_inviter_addresses(config),
        bootstrap_peers: exported_bootstrap_peers(config),
        discovery: config.network.discovery.clone(),
        protocols: PairingProtocols::default(),
    };
    let signature = STANDARD.encode(identity.sign(&signing_message(&payload)?)?);
    let offer = PairingOffer { payload, signature };
    offer.verify_at(issued_at_unix_seconds)?;

    Ok(offer)
}

pub fn build_pairing_request_at(
    offer: &PairingOffer,
    options: PairingRequestOptions,
    issued_at_unix_seconds: u64,
) -> Result<PairingRequest, PairingError> {
    offer.verify_at(issued_at_unix_seconds)?;
    let payload = PairingRequestPayload {
        version: PAIRING_OFFER_VERSION,
        network_name: offer.payload.network_name.clone(),
        inviter_peer: offer.payload.inviter_peer.clone(),
        joiner_peer: options.identity.peer_id.clone(),
        joiner_public_key: STANDARD.encode(options.identity.public_key_protobuf()?),
        rendezvous_token: offer.payload.rendezvous_token.clone(),
        issued_at_unix_seconds,
        requested_vpn_ip: options.requested_vpn_ip,
        requested_routes: options.requested_routes,
    };
    let signature = STANDARD.encode(options.identity.sign(&request_signing_message(&payload)?)?);
    let request = PairingRequest { payload, signature };
    request.verify_for_offer_at(offer, issued_at_unix_seconds)?;

    Ok(request)
}

pub fn build_pairing_response_at(
    config: &Config,
    offer: &PairingOffer,
    options: PairingResponseOptions,
    issued_at_unix_seconds: u64,
) -> Result<PairingResponse, PairingError> {
    config.validate_runtime()?;
    offer.verify_at(issued_at_unix_seconds)?;
    if options.expires_in_seconds == 0 {
        return Err(PairingError::InvalidExpiry);
    }
    if config.network.name != offer.payload.network_name
        || config.network.local_peer != offer.payload.inviter_peer
    {
        return Err(PairingError::OfferConfigMismatch);
    }
    let identity = NodeIdentity::from_private_key(
        config
            .network
            .private_key
            .as_deref()
            .ok_or(PairingError::MissingPrivateKey)?,
    )?;
    let expires_at_unix_seconds = issued_at_unix_seconds
        .checked_add(options.expires_in_seconds)
        .ok_or(PairingError::ExpiryOverflow)?;
    let payload = PairingResponsePayload {
        version: PAIRING_OFFER_VERSION,
        network_name: config.network.name.clone(),
        inviter_peer: config.network.local_peer.clone(),
        inviter_public_key: STANDARD.encode(identity.public_key_protobuf()?),
        joiner_peer: options.joiner_peer,
        rendezvous_token: offer.payload.rendezvous_token.clone(),
        issued_at_unix_seconds,
        expires_at_unix_seconds,
        assigned_vpn_ip: options.assigned_vpn_ip,
        membership_key: options.membership_key,
        member_records: options.member_records,
        inviter_addresses: exported_inviter_addresses(config),
        inviter_routes: config.network.routes.clone(),
        bootstrap_peers: exported_bootstrap_peers(config),
        relay_reservations: config.network.relay.reservations.clone(),
        discovery: config.network.discovery.clone(),
        protocols: PairingProtocols::default(),
    };
    let signature = STANDARD.encode(identity.sign(&response_signing_message(&payload)?)?);
    let response = PairingResponse { payload, signature };
    validate_response_payload(
        &response.payload,
        offer,
        &NodeIdentity {
            peer_id: response.payload.joiner_peer.clone(),
            private_key: String::new(),
        },
        issued_at_unix_seconds,
    )?;

    Ok(response)
}

pub fn import_pairing_response_config_at(
    offer: &PairingOffer,
    response: &PairingResponse,
    options: PairingConfigOptions,
    now_unix_seconds: u64,
) -> Result<Config, PairingError> {
    response.verify_for_offer_at(offer, &options.identity, now_unix_seconds)?;
    let config = Config {
        network: crate::config::NetworkConfig {
            name: response.payload.network_name.clone(),
            local_peer: options.identity.peer_id.clone(),
            private_key: Some(options.identity.private_key),
            membership_key: response.payload.membership_key.clone(),
            previous_membership_tags: Vec::new(),
            member_records: response.payload.member_records.clone(),
            vpn_ip: response.payload.assigned_vpn_ip.clone(),
            routes: options.local_routes,
            listen_addresses: crate::config::default_listen_addresses(),
            external_addresses: Vec::new(),
            bootstrap_peers: response.payload.bootstrap_peers.clone(),
            discovery: response.payload.discovery.clone(),
            relay: RelayConfig {
                server: false,
                reservations: response.payload.relay_reservations.clone(),
                ..RelayConfig::default()
            },
            packet_plane: crate::config::PacketPlaneConfig::default(),
        },
        interface: InterfaceConfig {
            name: options.interface_name,
            mtu: options.mtu,
        },
        peers: vec![PeerConfig {
            id: response.payload.inviter_peer.clone(),
            name: options.peer_name,
            ip: None,
            vpn_ip: None,
            addresses: response.payload.inviter_addresses.clone(),
            routes: response.payload.inviter_routes.clone(),
        }],
        queue: QueueConfig::default(),
        resources: ResourceConfig::default(),
    };
    config.validate_runtime()?;

    Ok(config)
}

fn validate_payload(
    payload: &PairingOfferPayload,
    now_unix_seconds: u64,
) -> Result<(), PairingError> {
    if payload.version != PAIRING_OFFER_VERSION {
        return Err(PairingError::UnsupportedVersion(payload.version));
    }
    if payload.network_name.is_empty() {
        return Err(PairingError::EmptyNetworkName);
    }
    if payload.issued_at_unix_seconds >= payload.expires_at_unix_seconds {
        return Err(PairingError::InvalidExpiry);
    }
    if now_unix_seconds > payload.expires_at_unix_seconds {
        return Err(PairingError::Expired {
            expired_at: payload.expires_at_unix_seconds,
            now: now_unix_seconds,
        });
    }
    validate_rendezvous_token(&payload.rendezvous_token)?;
    validate_multiaddrs(&payload.inviter_addresses)?;
    validate_bootstrap_peers(&payload.bootstrap_peers)?;
    validate_discovery(&payload.discovery)?;
    validate_protocols(&payload.protocols)?;

    Ok(())
}

fn validate_request_payload(
    payload: &PairingRequestPayload,
    offer: &PairingOffer,
    now_unix_seconds: u64,
) -> Result<(), PairingError> {
    if payload.version != PAIRING_OFFER_VERSION {
        return Err(PairingError::UnsupportedVersion(payload.version));
    }
    if payload.network_name != offer.payload.network_name {
        return Err(PairingError::NetworkMismatch {
            expected: offer.payload.network_name.clone(),
            actual: payload.network_name.clone(),
        });
    }
    if payload.inviter_peer != offer.payload.inviter_peer {
        return Err(PairingError::InviterMismatch {
            expected: offer.payload.inviter_peer.clone(),
            actual: payload.inviter_peer.clone(),
        });
    }
    if payload.rendezvous_token != offer.payload.rendezvous_token {
        return Err(PairingError::RendezvousTokenMismatch);
    }
    if now_unix_seconds > offer.payload.expires_at_unix_seconds {
        return Err(PairingError::Expired {
            expired_at: offer.payload.expires_at_unix_seconds,
            now: now_unix_seconds,
        });
    }
    payload.joiner_peer.parse::<Libp2pPeerId>()?;
    decode_public_key(&payload.joiner_public_key)?;
    validate_rendezvous_token(&payload.rendezvous_token)?;

    Ok(())
}

fn validate_response_payload(
    payload: &PairingResponsePayload,
    offer: &PairingOffer,
    joiner_identity: &NodeIdentity,
    now_unix_seconds: u64,
) -> Result<(), PairingError> {
    if payload.version != PAIRING_OFFER_VERSION {
        return Err(PairingError::UnsupportedVersion(payload.version));
    }
    if payload.network_name != offer.payload.network_name {
        return Err(PairingError::NetworkMismatch {
            expected: offer.payload.network_name.clone(),
            actual: payload.network_name.clone(),
        });
    }
    if payload.inviter_peer != offer.payload.inviter_peer {
        return Err(PairingError::InviterMismatch {
            expected: offer.payload.inviter_peer.clone(),
            actual: payload.inviter_peer.clone(),
        });
    }
    if payload.joiner_peer != joiner_identity.peer_id {
        return Err(PairingError::JoinerMismatch {
            expected: joiner_identity.peer_id.clone(),
            actual: payload.joiner_peer.clone(),
        });
    }
    if payload.rendezvous_token != offer.payload.rendezvous_token {
        return Err(PairingError::RendezvousTokenMismatch);
    }
    if payload.issued_at_unix_seconds >= payload.expires_at_unix_seconds {
        return Err(PairingError::InvalidExpiry);
    }
    if now_unix_seconds > payload.expires_at_unix_seconds {
        return Err(PairingError::Expired {
            expired_at: payload.expires_at_unix_seconds,
            now: now_unix_seconds,
        });
    }
    validate_rendezvous_token(&payload.rendezvous_token)?;
    validate_multiaddrs(&payload.inviter_addresses)?;
    validate_bootstrap_peers(&payload.bootstrap_peers)?;
    validate_multiaddrs(&payload.relay_reservations)?;
    validate_discovery(&payload.discovery)?;
    validate_protocols(&payload.protocols)?;
    validate_membership_records_at(
        &payload.member_records,
        &payload.network_name,
        now_unix_seconds,
    )?;
    if payload.membership_key.is_none() && !has_joiner_membership_record(payload) {
        return Err(PairingError::MissingMembershipGrant);
    }

    Ok(())
}

fn has_joiner_membership_record(payload: &PairingResponsePayload) -> bool {
    payload.member_records.iter().any(|record| {
        record.payload.member_peer == payload.joiner_peer
            && !record.payload.revoked
            && record
                .payload
                .roles
                .contains(&MembershipRole::OverlayMember)
    })
}

fn validate_rendezvous_token(token: &str) -> Result<String, PairingError> {
    let bytes = URL_SAFE_NO_PAD.decode(token)?;
    if bytes.len() != RENDEZVOUS_TOKEN_LEN {
        return Err(PairingError::InvalidRendezvousTokenLength {
            actual: bytes.len(),
            expected: RENDEZVOUS_TOKEN_LEN,
        });
    }
    Ok(token.to_owned())
}

fn validate_multiaddrs(addresses: &[String]) -> Result<(), PairingError> {
    for address in addresses {
        address.parse::<Multiaddr>()?;
    }
    Ok(())
}

fn validate_bootstrap_peers(peers: &[BootstrapPeerConfig]) -> Result<(), PairingError> {
    for peer in peers {
        peer.peer_address()?;
    }
    Ok(())
}

fn validate_discovery(discovery: &DiscoveryConfig) -> Result<(), PairingError> {
    if !discovery.mdns && !discovery.kademlia {
        return Err(PairingError::NoDiscoveryPath);
    }
    Ok(())
}

fn validate_protocols(protocols: &PairingProtocols) -> Result<(), PairingError> {
    if protocols.control != CONTROL_PROTOCOL
        || protocols.packet != crate::runtime::packet::PACKET_PROTOCOL
        || protocols.service != SERVICE_PROTOCOL
        || protocols.wire_version != WIRE_VERSION
        || protocols.packet_header_len != HEADER_LEN
    {
        return Err(PairingError::IncompatibleProtocols);
    }
    Ok(())
}

fn exported_inviter_addresses(config: &Config) -> Vec<String> {
    let mut addresses = config.network.external_addresses.clone();
    for address in &config.network.listen_addresses {
        if !addresses.contains(address) {
            addresses.push(address.clone());
        }
    }
    addresses
}

fn exported_bootstrap_peers(config: &Config) -> Vec<BootstrapPeerConfig> {
    if config.uses_public_ipfs_bootstrap_defaults() {
        public_ipfs_bootstrap_peer_configs()
    } else {
        config.network.bootstrap_peers.clone()
    }
}

fn generate_rendezvous_token() -> String {
    let mut bytes = [0_u8; RENDEZVOUS_TOKEN_LEN];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn signing_message(payload: &PairingOfferPayload) -> Result<Vec<u8>, PairingError> {
    let mut message = SIGNING_DOMAIN.to_vec();
    message.extend(serde_json::to_vec(payload)?);
    Ok(message)
}

fn request_signing_message(payload: &PairingRequestPayload) -> Result<Vec<u8>, PairingError> {
    let mut message = REQUEST_SIGNING_DOMAIN.to_vec();
    message.extend(serde_json::to_vec(payload)?);
    Ok(message)
}

fn response_signing_message(payload: &PairingResponsePayload) -> Result<Vec<u8>, PairingError> {
    let mut message = RESPONSE_SIGNING_DOMAIN.to_vec();
    message.extend(serde_json::to_vec(payload)?);
    Ok(message)
}

fn decode_public_key(encoded: &str) -> Result<PublicKey, PairingError> {
    Ok(PublicKey::try_decode_protobuf(&STANDARD.decode(encoded)?)?)
}

fn current_unix_seconds() -> Result<u64, PairingError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PairingError::SystemTimeBeforeEpoch)?
        .as_secs())
}

#[derive(Debug)]
pub enum PairingError {
    Config(crate::config::ConfigError),
    Identity(crate::identity::IdentityError),
    Json(serde_json::Error),
    Base64(base64::DecodeError),
    Libp2pIdentity(libp2p::identity::DecodingError),
    Libp2pPeerId(libp2p::identity::ParseError),
    Multiaddr(libp2p::multiaddr::Error),
    RoutePrefix(crate::config::RoutePrefixError),
    MembershipRecord(crate::membership::MembershipRecordError),
    UnsupportedVersion(u8),
    EmptyNetworkName,
    MissingPrivateKey,
    OfferConfigMismatch,
    NetworkMismatch { expected: String, actual: String },
    InviterMismatch { expected: String, actual: String },
    JoinerMismatch { expected: String, actual: String },
    RendezvousTokenMismatch,
    MissingMembershipGrant,
    InvalidExpiry,
    ExpiryOverflow,
    InvalidRendezvousTokenLength { actual: usize, expected: usize },
    NoDiscoveryPath,
    IncompatibleProtocols,
    PublicKeyPeerMismatch { expected: String, actual: String },
    InvalidSignature,
    InvalidUriScheme,
    Expired { expired_at: u64, now: u64 },
    SystemTimeBeforeEpoch,
}

impl From<crate::config::ConfigError> for PairingError {
    fn from(error: crate::config::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<crate::identity::IdentityError> for PairingError {
    fn from(error: crate::identity::IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<serde_json::Error> for PairingError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<base64::DecodeError> for PairingError {
    fn from(error: base64::DecodeError) -> Self {
        Self::Base64(error)
    }
}

impl From<libp2p::identity::DecodingError> for PairingError {
    fn from(error: libp2p::identity::DecodingError) -> Self {
        Self::Libp2pIdentity(error)
    }
}

impl From<libp2p::identity::ParseError> for PairingError {
    fn from(error: libp2p::identity::ParseError) -> Self {
        Self::Libp2pPeerId(error)
    }
}

impl From<libp2p::multiaddr::Error> for PairingError {
    fn from(error: libp2p::multiaddr::Error) -> Self {
        Self::Multiaddr(error)
    }
}

impl From<crate::config::RoutePrefixError> for PairingError {
    fn from(error: crate::config::RoutePrefixError) -> Self {
        Self::RoutePrefix(error)
    }
}

impl From<crate::membership::MembershipRecordError> for PairingError {
    fn from(error: crate::membership::MembershipRecordError) -> Self {
        Self::MembershipRecord(error)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        config::{
            InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig, RelayConfig, ResourceConfig,
        },
        identity::NodeIdentity,
        membership::{
            MembershipRecordIssueOptions, MembershipRecordSubject,
            issue_membership_record_for_subject_at,
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

    #[test]
    fn pairing_offer_uri_round_trips_and_verifies() {
        let offer = export_pairing_offer_at(
            &config(),
            PairingOfferOptions {
                expires_in_seconds: 600,
                rendezvous_token: Some(URL_SAFE_NO_PAD.encode([7_u8; RENDEZVOUS_TOKEN_LEN])),
            },
            1_000,
        )
        .expect("offer");

        let parsed = PairingOffer::from_uri(&offer.to_uri().expect("uri")).expect("parsed");

        assert_eq!(parsed, offer);
        parsed.verify_at(1_001).expect("verified");
        assert_eq!(parsed.payload.network_name, "lab");
        assert_eq!(parsed.payload.expires_at_unix_seconds, 1_600);
        assert!(!parsed.payload.bootstrap_peers.is_empty());
    }

    #[test]
    fn pairing_offer_rejects_expired_uri() {
        let offer = export_pairing_offer_at(&config(), PairingOfferOptions::default(), 1_000)
            .expect("offer");

        assert!(matches!(
            offer.verify_at(1_601),
            Err(PairingError::Expired {
                expired_at: 1_600,
                now: 1_601
            })
        ));
    }

    #[test]
    fn pairing_offer_rejects_tampered_payload() {
        let mut offer = export_pairing_offer_at(&config(), PairingOfferOptions::default(), 1_000)
            .expect("offer");

        offer.payload.network_name = "other".to_owned();

        assert!(matches!(
            offer.verify_at(1_001),
            Err(PairingError::InvalidSignature)
        ));
    }

    #[test]
    fn pairing_offer_requires_valid_rendezvous_token() {
        assert!(matches!(
            export_pairing_offer_at(
                &config(),
                PairingOfferOptions {
                    expires_in_seconds: 600,
                    rendezvous_token: Some(URL_SAFE_NO_PAD.encode([1_u8; 8])),
                },
                1_000,
            ),
            Err(PairingError::InvalidRendezvousTokenLength {
                actual: 8,
                expected: RENDEZVOUS_TOKEN_LEN
            })
        ));
    }

    #[test]
    fn pairing_request_round_trips_against_offer() {
        let offer = export_pairing_offer_at(&config(), PairingOfferOptions::default(), 1_000)
            .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");

        let request = build_pairing_request_at(
            &offer,
            PairingRequestOptions {
                identity: joiner.clone(),
                requested_vpn_ip: Some("10.42.0.2".to_owned()),
                requested_routes: vec![RouteConfig {
                    prefix: "10.42.0.2/32".to_owned(),
                    metric: 100,
                }],
            },
            1_001,
        )
        .expect("request");

        request
            .verify_for_offer_at(&offer, 1_002)
            .expect("request verifies");
        assert_eq!(request.payload.joiner_peer, joiner.peer_id);
        assert_eq!(
            request.payload.requested_vpn_ip.as_deref(),
            Some("10.42.0.2")
        );
    }

    #[test]
    fn pairing_response_imports_minimal_config_with_shared_key() {
        let inviter_config = config();
        let offer = export_pairing_offer_at(&inviter_config, PairingOfferOptions::default(), 1_000)
            .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let membership_key = STANDARD.encode([9_u8; 32]);
        let response = build_pairing_response_at(
            &inviter_config,
            &offer,
            PairingResponseOptions {
                joiner_peer: joiner.peer_id.clone(),
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                membership_key: Some(membership_key.clone()),
                member_records: Vec::new(),
                expires_in_seconds: 300,
            },
            1_010,
        )
        .expect("response");

        let imported = import_pairing_response_config_at(
            &offer,
            &response,
            PairingConfigOptions {
                identity: joiner,
                interface_name: "pv-pair".to_owned(),
                mtu: 1280,
                local_routes: Vec::new(),
                peer_name: Some("inviter".to_owned()),
            },
            1_011,
        )
        .expect("config");

        assert_eq!(imported.network.name, "lab");
        assert_eq!(imported.network.vpn_ip.as_deref(), Some("10.42.0.2"));
        assert_eq!(
            imported.network.membership_key.as_deref(),
            Some(membership_key.as_str())
        );
        assert_eq!(imported.interface.name, "pv-pair");
        assert_eq!(imported.peers.len(), 1);
        assert_eq!(imported.peers[0].id, offer.payload.inviter_peer);
    }

    #[test]
    fn pairing_response_accepts_signed_membership_record_without_shared_key() {
        let inviter_config = config();
        let inviter_identity = NodeIdentity::from_private_key(
            inviter_config
                .network
                .private_key
                .as_deref()
                .expect("private key"),
        )
        .expect("inviter identity");
        let offer = export_pairing_offer_at(&inviter_config, PairingOfferOptions::default(), 1_000)
            .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let member_record = issue_membership_record_for_subject_at(
            &inviter_identity,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&joiner).expect("subject"),
                membership_epoch: 1,
                sequence: 1,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_005,
        )
        .expect("record");

        let response = build_pairing_response_at(
            &inviter_config,
            &offer,
            PairingResponseOptions {
                joiner_peer: joiner.peer_id.clone(),
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                membership_key: None,
                member_records: vec![member_record.clone()],
                expires_in_seconds: 300,
            },
            1_010,
        )
        .expect("response");

        response
            .verify_for_offer_at(&offer, &joiner, 1_011)
            .expect("response verifies");
        assert_eq!(response.payload.member_records, vec![member_record]);
        assert!(response.payload.membership_key.is_none());
    }

    #[test]
    fn pairing_response_rejects_missing_membership_grant() {
        let inviter_config = config();
        let offer = export_pairing_offer_at(&inviter_config, PairingOfferOptions::default(), 1_000)
            .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");

        assert!(matches!(
            build_pairing_response_at(
                &inviter_config,
                &offer,
                PairingResponseOptions {
                    joiner_peer: joiner.peer_id,
                    assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                    membership_key: None,
                    member_records: Vec::new(),
                    expires_in_seconds: 300,
                },
                1_010,
            ),
            Err(PairingError::MissingMembershipGrant)
        ));
    }
}
