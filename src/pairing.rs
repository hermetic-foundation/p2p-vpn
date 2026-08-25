use std::{
    net::IpAddr,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use libp2p::{Multiaddr, PeerId as Libp2pPeerId, identity::PublicKey, multiaddr::Protocol};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::{
    PeerId,
    config::{
        BootstrapPeerConfig, Config, DiscoveryConfig, InterfaceConfig, PeerConfig, QueueConfig,
        RelayConfig, ResourceConfig, RouteConfig,
    },
    identity::NodeIdentity,
    membership::{
        MembershipRole, SignedMembershipRecord, overlay_membership_trust_path_at,
        validate_membership_records_at,
    },
    route::{builtin_ipv4, builtin_ipv6},
    runtime::{control::CONTROL_PROTOCOL, packet::PACKET_PROTOCOL, service::SERVICE_PROTOCOL},
    wire::{HEADER_LEN, WIRE_VERSION},
};

pub const PAIRING_OFFER_VERSION: u8 = 1;
pub const DEFAULT_PAIRING_EXPIRES_IN_SECONDS: u64 = 600;
pub const MAX_PAIRING_MEMBERSHIP_RECORDS: usize = 16;
pub const MAX_PAIRING_MESSAGE_LEN: usize = 32 * 1024;

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
        validate_encoded_pairing_message("offer", self)?;
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
        validate_encoded_pairing_message("offer", self)?;
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
        if bytes.len() > MAX_PAIRING_MESSAGE_LEN {
            return Err(PairingError::EncodedMessageTooLarge(
                "offer",
                bytes.len(),
                MAX_PAIRING_MESSAGE_LEN,
            ));
        }
        let offer = serde_json::from_slice(&bytes)?;
        validate_encoded_pairing_message("offer", &offer)?;
        Ok(offer)
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
    #[serde(default, skip_serializing_if = "PairingAcceptanceMode::is_file_bearer")]
    pub acceptance_mode: PairingAcceptanceMode,
    #[serde(default)]
    pub inviter_addresses: Vec<String>,
    #[serde(default)]
    pub bootstrap_peers: Vec<BootstrapPeerConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relay_reservations: Vec<String>,
    pub discovery: DiscoveryConfig,
    pub protocols: PairingProtocols,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingAcceptanceMode {
    #[default]
    FileBearer,
    CodeApproval,
}

impl PairingAcceptanceMode {
    #[allow(clippy::trivially_copy_pass_by_ref)]
    const fn is_file_bearer(&self) -> bool {
        matches!(self, Self::FileBearer)
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<PairingOffer>,
    pub payload: PairingRequestPayload,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_authentication: Option<PairingCodeAuthentication>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PairingCodeAuthentication {
    pub locator: String,
    pub confirmation: String,
}

impl PairingRequest {
    pub fn verify_for_offer_at(
        &self,
        offer: &PairingOffer,
        now_unix_seconds: u64,
    ) -> Result<(), PairingError> {
        offer.verify_at(now_unix_seconds)?;
        validate_encoded_pairing_message("request", self)?;
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
    #[serde(default)]
    pub offer_issued_at_unix_seconds: u64,
    #[serde(default)]
    pub offer_expires_at_unix_seconds: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub offer_signature: String,
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
        validate_encoded_pairing_message("response", self)?;
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
    export_pairing_offer_at_with_policy(
        config,
        options,
        issued_at_unix_seconds,
        true,
        PairingAcceptanceMode::FileBearer,
    )
}

pub fn export_code_pairing_offer_at(
    config: &Config,
    options: PairingOfferOptions,
    issued_at_unix_seconds: u64,
) -> Result<PairingOffer, PairingError> {
    export_pairing_offer_at_with_policy(
        config,
        options,
        issued_at_unix_seconds,
        true,
        PairingAcceptanceMode::CodeApproval,
    )
}

pub fn export_discovery_only_pairing_offer(
    config: &Config,
    options: PairingOfferOptions,
) -> Result<PairingOffer, PairingError> {
    export_discovery_only_pairing_offer_at(config, options, current_unix_seconds()?)
}

pub fn export_discovery_only_pairing_offer_at(
    config: &Config,
    options: PairingOfferOptions,
    issued_at_unix_seconds: u64,
) -> Result<PairingOffer, PairingError> {
    export_pairing_offer_at_with_policy(
        config,
        options,
        issued_at_unix_seconds,
        false,
        PairingAcceptanceMode::FileBearer,
    )
}

fn export_pairing_offer_at_with_policy(
    config: &Config,
    options: PairingOfferOptions,
    issued_at_unix_seconds: u64,
    include_inviter_addresses: bool,
    acceptance_mode: PairingAcceptanceMode,
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
    let inviter_peer = config.local_peer()?;
    let expires_at_unix_seconds = issued_at_unix_seconds
        .checked_add(options.expires_in_seconds)
        .ok_or(PairingError::ExpiryOverflow)?;
    let payload = PairingOfferPayload {
        version: PAIRING_OFFER_VERSION,
        network_name: config.network.name.clone(),
        inviter_peer,
        inviter_public_key: STANDARD.encode(identity.public_key_protobuf()?),
        rendezvous_token: match options.rendezvous_token {
            Some(token) => validate_rendezvous_token(&token)?,
            None => generate_rendezvous_token(),
        },
        issued_at_unix_seconds,
        expires_at_unix_seconds,
        acceptance_mode,
        inviter_addresses: if include_inviter_addresses {
            exported_inviter_addresses(config)
        } else {
            Vec::new()
        },
        bootstrap_peers: exported_bootstrap_peers(config),
        relay_reservations: config.network.relay.reservations.clone(),
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
        offer_issued_at_unix_seconds: offer.payload.issued_at_unix_seconds,
        offer_expires_at_unix_seconds: offer.payload.expires_at_unix_seconds,
        offer_signature: offer.signature.clone(),
        issued_at_unix_seconds,
        requested_vpn_ip: options.requested_vpn_ip,
        requested_routes: options.requested_routes,
    };
    let signature = STANDARD.encode(options.identity.sign(&request_signing_message(&payload)?)?);
    let request = PairingRequest {
        offer: offer
            .payload
            .inviter_addresses
            .is_empty()
            .then(|| offer.clone()),
        payload,
        signature,
        code_authentication: None,
    };
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
    let inviter_peer = config.local_peer()?;
    if config.network.name != offer.payload.network_name
        || inviter_peer != offer.payload.inviter_peer
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
        inviter_peer,
        inviter_public_key: STANDARD.encode(identity.public_key_protobuf()?),
        joiner_peer: options.joiner_peer,
        rendezvous_token: offer.payload.rendezvous_token.clone(),
        issued_at_unix_seconds,
        expires_at_unix_seconds,
        assigned_vpn_ip: options.assigned_vpn_ip,
        membership_key: options.membership_key,
        member_records: options.member_records,
        inviter_addresses: exported_inviter_addresses(config),
        inviter_routes: exported_inviter_routes(config)?,
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

pub fn apply_pairing_response_to_config_at(
    base: &Config,
    offer: &PairingOffer,
    response: &PairingResponse,
    local_identity: &NodeIdentity,
    now_unix_seconds: u64,
) -> Result<Config, PairingError> {
    let local_peer = base.local_peer()?;
    if local_peer != local_identity.peer_id {
        return Err(PairingError::LocalIdentityMismatch {
            expected: local_peer,
            actual: local_identity.peer_id.clone(),
        });
    }

    let joiner_identity = if local_identity.peer_id == response.payload.joiner_peer {
        local_identity.clone()
    } else if local_identity.peer_id == response.payload.inviter_peer {
        NodeIdentity {
            peer_id: response.payload.joiner_peer.clone(),
            private_key: String::new(),
        }
    } else {
        return Err(PairingError::LocalPeerNotParticipant {
            local: local_identity.peer_id.clone(),
        });
    };
    response.verify_for_offer_at(offer, &joiner_identity, now_unix_seconds)?;
    validate_pairing_membership_record_count(&response.payload.member_records)?;
    validate_response_trust_root_against_existing_config(base, offer, response, now_unix_seconds)?;
    let mut next = base.clone();
    adopt_optional_value(
        &mut next.network.membership_key,
        response.payload.membership_key.as_ref(),
        PairingError::ConflictingMembershipKey,
    )?;
    if local_identity.peer_id == response.payload.joiner_peer {
        adopt_optional_value(
            &mut next.network.vpn_ip,
            response.payload.assigned_vpn_ip.as_ref(),
            PairingError::ConflictingAssignedVpnIp,
        )?;
    }
    for record in &response.payload.member_records {
        upsert_pairing_membership_record(&mut next.network.member_records, record)?;
    }
    next.validate_runtime()?;

    Ok(next)
}

fn validate_code_pairing_membership_records(
    offer: &PairingOffer,
    payload: &PairingResponsePayload,
    now_unix_seconds: u64,
) -> Result<(), PairingError> {
    if offer.payload.acceptance_mode != PairingAcceptanceMode::CodeApproval {
        return Ok(());
    }
    let records = &payload.member_records;
    validate_pairing_membership_record_count(records)?;

    if !records
        .iter()
        .any(|record| record.payload.issuer_peer == record.payload.member_peer)
    {
        return Err(PairingError::MissingInviterTrustRoot);
    }
    let trust_path =
        overlay_membership_trust_path_at(records, &payload.inviter_peer, now_unix_seconds)?
            .ok_or(PairingError::MissingInviterTrustRoot)?;
    let trust_root = &trust_path[0].payload.member_peer;
    if let Some(unexpected) = records.iter().find(|record| {
        record.payload.issuer_peer == record.payload.member_peer
            && record.payload.member_peer != *trust_root
    }) {
        return Err(PairingError::UnexpectedInviterTrustRoot {
            expected: trust_root.clone(),
            actual: unexpected.payload.member_peer.clone(),
        });
    }

    let joiner_record =
        latest_membership_record_from(records, &payload.inviter_peer, &payload.joiner_peer).filter(
            |record| {
                !record.payload.revoked
                    && record
                        .payload
                        .roles
                        .contains(&MembershipRole::OverlayMember)
            },
        );
    let Some(joiner_record) = joiner_record else {
        if let Some(record) = latest_membership_record_for(records, &payload.joiner_peer)
            .filter(|record| !record.payload.revoked)
        {
            return Err(PairingError::MembershipIssuerMismatch {
                expected: payload.inviter_peer.clone(),
                actual: record.payload.issuer_peer.clone(),
            });
        }
        return Err(PairingError::MissingMembershipGrant);
    };

    if let Some(assigned_vpn_ip) = payload.assigned_vpn_ip.as_deref() {
        let assigned_vpn_ip = assigned_vpn_ip.parse::<IpAddr>()?;
        let joiner_peer = payload.joiner_peer.parse::<Libp2pPeerId>()?;
        let joiner_overlay = PeerId::from_libp2p(joiner_peer);
        let is_builtin = assigned_vpn_ip == IpAddr::V4(builtin_ipv4(joiner_overlay))
            || assigned_vpn_ip == IpAddr::V6(builtin_ipv6(joiner_overlay));
        let is_granted = joiner_record
            .payload
            .roles
            .contains(&MembershipRole::RouteAuthority)
            && joiner_record
                .payload
                .route_grants
                .iter()
                .try_fold(false, |authorized, route| {
                    Ok::<_, PairingError>(authorized || route.prefix()?.contains(assigned_vpn_ip))
                })?;
        if !is_builtin && !is_granted {
            return Err(PairingError::AssignedVpnIpNotAuthorized {
                assigned: assigned_vpn_ip.to_string(),
            });
        }
    }

    Ok(())
}

fn validate_response_trust_root_against_existing_config(
    base: &Config,
    offer: &PairingOffer,
    response: &PairingResponse,
    now_unix_seconds: u64,
) -> Result<(), PairingError> {
    if offer.payload.acceptance_mode != PairingAcceptanceMode::CodeApproval {
        return Ok(());
    }
    let has_existing_root_record = base
        .network
        .member_records
        .iter()
        .any(|record| record.payload.issuer_peer == record.payload.member_peer);
    if !has_existing_root_record {
        if base
            .network
            .member_records
            .iter()
            .any(|record| record.payload.issuer_peer != response.payload.inviter_peer)
        {
            return Err(PairingError::LegacyMembershipMigrationRequired {
                inviter: response.payload.inviter_peer.clone(),
            });
        }
        return Ok(());
    }
    let existing_roots =
        latest_active_pairing_trust_roots(&base.network.member_records, now_unix_seconds);
    if existing_roots.is_empty() {
        return Err(PairingError::MissingInviterTrustRoot);
    }
    let response_root = overlay_membership_trust_path_at(
        &response.payload.member_records,
        &response.payload.inviter_peer,
        now_unix_seconds,
    )?
    .and_then(|path| path.first().cloned())
    .ok_or(PairingError::MissingInviterTrustRoot)?;
    if existing_roots.contains(&response_root.payload.member_peer) {
        return Ok(());
    }
    Err(PairingError::UnexpectedInviterTrustRoot {
        expected: existing_roots.join(","),
        actual: response_root.payload.member_peer,
    })
}

fn latest_active_pairing_trust_roots(
    records: &[SignedMembershipRecord],
    now_unix_seconds: u64,
) -> Vec<String> {
    let mut roots = records
        .iter()
        .filter(|record| record.payload.issuer_peer == record.payload.member_peer)
        .filter(|record| {
            latest_membership_record_from(
                records,
                &record.payload.issuer_peer,
                &record.payload.member_peer,
            ) == Some(*record)
        })
        .filter(|record| {
            !record.is_expired_at(now_unix_seconds)
                && !record.payload.revoked
                && record
                    .payload
                    .roles
                    .contains(&MembershipRole::OverlayMember)
        })
        .map(|record| record.payload.member_peer.clone())
        .collect::<Vec<_>>();
    roots.sort();
    roots
}

fn validate_pairing_membership_record_count(
    records: &[SignedMembershipRecord],
) -> Result<(), PairingError> {
    if records.len() > MAX_PAIRING_MEMBERSHIP_RECORDS {
        return Err(PairingError::TooManyMembershipRecords {
            actual: records.len(),
            max: MAX_PAIRING_MEMBERSHIP_RECORDS,
        });
    }
    Ok(())
}

fn latest_membership_record_for<'a>(
    records: &'a [SignedMembershipRecord],
    member_peer: &str,
) -> Option<&'a SignedMembershipRecord> {
    records
        .iter()
        .filter(|record| record.payload.member_peer == member_peer)
        .max_by_key(|record| (record.payload.membership_epoch, record.payload.sequence))
}

fn latest_membership_record_from<'a>(
    records: &'a [SignedMembershipRecord],
    issuer_peer: &str,
    member_peer: &str,
) -> Option<&'a SignedMembershipRecord> {
    records
        .iter()
        .filter(|record| {
            record.payload.issuer_peer == issuer_peer && record.payload.member_peer == member_peer
        })
        .max_by_key(|record| (record.payload.membership_epoch, record.payload.sequence))
}

fn adopt_optional_value<T: Clone + Eq>(
    current: &mut Option<T>,
    incoming: Option<&T>,
    conflict: PairingError,
) -> Result<(), PairingError> {
    match (current.as_ref(), incoming) {
        (Some(existing), Some(incoming)) if existing != incoming => Err(conflict),
        (None, Some(incoming)) => {
            *current = Some(incoming.clone());
            Ok(())
        }
        _ => Ok(()),
    }
}

fn upsert_pairing_membership_record(
    records: &mut Vec<SignedMembershipRecord>,
    incoming: &SignedMembershipRecord,
) -> Result<(), PairingError> {
    let existing = records.iter().position(|record| {
        record.payload.issuer_peer == incoming.payload.issuer_peer
            && record.payload.member_peer == incoming.payload.member_peer
    });
    let Some(index) = existing else {
        records.push(incoming.clone());
        return Ok(());
    };

    let current = &records[index];
    let current_version = (current.payload.membership_epoch, current.payload.sequence);
    let incoming_version = (incoming.payload.membership_epoch, incoming.payload.sequence);
    if incoming_version > current_version {
        records[index] = incoming.clone();
    } else if incoming_version == current_version && current != incoming {
        return Err(PairingError::ConflictingMembershipRecord {
            issuer: incoming.payload.issuer_peer.clone(),
            member: incoming.payload.member_peer.clone(),
        });
    }
    Ok(())
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
    validate_relay_reservations(&payload.relay_reservations)?;
    validate_pairing_reachability(
        &payload.discovery,
        &payload.inviter_addresses,
        &payload.bootstrap_peers,
        &payload.relay_reservations,
    )?;
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
    if payload.offer_issued_at_unix_seconds != 0
        && payload.offer_issued_at_unix_seconds != offer.payload.issued_at_unix_seconds
    {
        return Err(PairingError::InvalidSignature);
    }
    if payload.offer_expires_at_unix_seconds != 0
        && payload.offer_expires_at_unix_seconds != offer.payload.expires_at_unix_seconds
    {
        return Err(PairingError::InvalidSignature);
    }
    if !payload.offer_signature.is_empty() && payload.offer_signature != offer.signature {
        return Err(PairingError::InvalidSignature);
    }
    if now_unix_seconds > offer.payload.expires_at_unix_seconds {
        return Err(PairingError::Expired {
            expired_at: offer.payload.expires_at_unix_seconds,
            now: now_unix_seconds,
        });
    }
    payload.joiner_peer.parse::<Libp2pPeerId>()?;
    decode_public_key(&payload.joiner_public_key)?;
    validate_optional_ip(payload.requested_vpn_ip.as_deref())?;
    validate_routes(&payload.requested_routes)?;
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
    validate_relay_reservations(&payload.relay_reservations)?;
    validate_optional_ip(payload.assigned_vpn_ip.as_deref())?;
    validate_pairing_reachability(
        &payload.discovery,
        &payload.inviter_addresses,
        &payload.bootstrap_peers,
        &payload.relay_reservations,
    )?;
    validate_protocols(&payload.protocols)?;
    validate_membership_records_at(
        &payload.member_records,
        &payload.network_name,
        now_unix_seconds,
    )?;
    validate_code_pairing_membership_records(offer, payload, now_unix_seconds)?;
    if payload.membership_key.is_none() && !has_joiner_membership_record(payload) {
        return Err(PairingError::MissingMembershipGrant);
    }

    Ok(())
}

fn validate_optional_ip(value: Option<&str>) -> Result<(), PairingError> {
    if let Some(value) = value {
        value.parse::<IpAddr>()?;
    }
    Ok(())
}

fn validate_routes(routes: &[RouteConfig]) -> Result<(), PairingError> {
    for route in routes {
        route.prefix()?;
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

fn validate_relay_reservations(addresses: &[String]) -> Result<(), PairingError> {
    for address in addresses {
        validate_relay_reservation(address)?;
    }
    Ok(())
}

fn validate_relay_reservation(address: &str) -> Result<(), PairingError> {
    let address = address.parse::<Multiaddr>()?;
    let mut relay_peer = None;
    let mut saw_circuit = false;
    for protocol in &address {
        match protocol {
            Protocol::P2p(_) if saw_circuit => {
                return Err(PairingError::UnexpectedRelayTarget);
            }
            Protocol::P2p(peer) => relay_peer = Some(peer),
            Protocol::P2pCircuit if relay_peer.is_some() => saw_circuit = true,
            Protocol::P2pCircuit => return Err(PairingError::MissingRelayPeer),
            _ => {}
        }
    }

    if saw_circuit {
        Ok(())
    } else {
        Err(PairingError::MissingRelayCircuit)
    }
}

fn validate_bootstrap_peers(peers: &[BootstrapPeerConfig]) -> Result<(), PairingError> {
    for peer in peers {
        peer.peer_address()?;
    }
    Ok(())
}

fn validate_pairing_reachability(
    discovery: &DiscoveryConfig,
    inviter_addresses: &[String],
    bootstrap_peers: &[BootstrapPeerConfig],
    relay_reservations: &[String],
) -> Result<(), PairingError> {
    if !discovery.mdns
        && !discovery.kademlia
        && inviter_addresses.is_empty()
        && bootstrap_peers.is_empty()
        && relay_reservations.is_empty()
    {
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
    if let Ok(local_peer_string) = config.local_peer()
        && let Ok(local_peer) = local_peer_string.parse::<Libp2pPeerId>()
        && let Ok(relay_reservations) = config.relay_reservation_multiaddrs()
    {
        for address in relay_reservations {
            let relayed_address = address.with(Protocol::P2p(local_peer)).to_string();
            if !addresses.contains(&relayed_address) {
                addresses.push(relayed_address);
            }
        }
    }
    for address in &config.network.listen_addresses {
        if !addresses.contains(address) {
            addresses.push(address.clone());
        }
    }
    addresses
}

fn exported_bootstrap_peers(config: &Config) -> Vec<BootstrapPeerConfig> {
    if config.uses_public_ipfs_bootstrap_defaults() {
        Vec::new()
    } else {
        config.network.bootstrap_peers.clone()
    }
}

fn exported_inviter_routes(config: &Config) -> Result<Vec<RouteConfig>, PairingError> {
    let mut routes = config.network.routes.clone();
    if let Some(vpn_ip) = config.network.vpn_ip.as_deref() {
        let address = vpn_ip.parse::<IpAddr>()?;
        let prefix_len = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let route = RouteConfig {
            prefix: format!("{address}/{prefix_len}"),
            metric: 0,
        };
        if !routes.contains(&route) {
            routes.push(route);
        }
    }
    Ok(routes)
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

fn validate_encoded_pairing_message<M: Serialize>(
    message: &'static str,
    value: &M,
) -> Result<(), PairingError> {
    let actual = serde_json::to_vec(value)?.len();
    if actual <= MAX_PAIRING_MESSAGE_LEN {
        return Ok(());
    }
    Err(PairingError::EncodedMessageTooLarge(
        message,
        actual,
        MAX_PAIRING_MESSAGE_LEN,
    ))
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
    IpAddr(std::net::AddrParseError),
    RoutePrefix(crate::config::RoutePrefixError),
    MembershipRecord(crate::membership::MembershipRecordError),
    UnsupportedVersion(u8),
    EmptyNetworkName,
    MissingPrivateKey,
    OfferConfigMismatch,
    NetworkMismatch { expected: String, actual: String },
    InviterMismatch { expected: String, actual: String },
    JoinerMismatch { expected: String, actual: String },
    TransportPeerMismatch { expected: String, actual: String },
    RendezvousTokenMismatch,
    MissingMembershipGrant,
    MissingInviterTrustRoot,
    UnexpectedInviterTrustRoot { expected: String, actual: String },
    LegacyMembershipMigrationRequired { inviter: String },
    TooManyMembershipRecords { actual: usize, max: usize },
    EncodedMessageTooLarge(&'static str, usize, usize),
    MembershipIssuerMismatch { expected: String, actual: String },
    ConflictingMembershipRecord { issuer: String, member: String },
    MembershipVersionOverflow { issuer: String, member: String },
    ConflictingMembershipKey,
    ConflictingAssignedVpnIp,
    AssignedVpnIpNotAuthorized { assigned: String },
    LocalPeerNotParticipant { local: String },
    LocalIdentityMismatch { expected: String, actual: String },
    InvalidExpiry,
    ExpiryOverflow,
    InvalidRendezvousTokenLength { actual: usize, expected: usize },
    NoDiscoveryPath,
    IncompatibleProtocols,
    ApprovalRequired,
    MissingRelayPeer,
    MissingRelayCircuit,
    UnexpectedRelayTarget,
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

impl From<std::net::AddrParseError> for PairingError {
    fn from(error: std::net::AddrParseError) -> Self {
        Self::IpAddr(error)
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
        assert_eq!(
            parsed.payload.acceptance_mode,
            PairingAcceptanceMode::FileBearer
        );
        assert!(
            serde_json::to_value(&offer).expect("JSON")["payload"]
                .get("acceptance_mode")
                .is_none()
        );
        assert!(parsed.payload.bootstrap_peers.is_empty());
        assert!(parsed.payload.discovery.kademlia);
    }

    #[test]
    fn pairing_request_rejects_an_encoding_larger_than_the_transport_frame() {
        let offer = export_pairing_offer_at(
            &config(),
            PairingOfferOptions {
                expires_in_seconds: 600,
                rendezvous_token: Some(URL_SAFE_NO_PAD.encode([7_u8; RENDEZVOUS_TOKEN_LEN])),
            },
            1_000,
        )
        .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let route = RouteConfig {
            prefix: "2001:db8::/32".to_owned(),
            metric: 10,
        };

        let error = build_pairing_request_at(
            &offer,
            PairingRequestOptions {
                identity: joiner,
                requested_vpn_ip: None,
                requested_routes: vec![route; 1_024],
            },
            1_001,
        )
        .expect_err("oversized request");

        assert!(matches!(
            error,
            PairingError::EncodedMessageTooLarge(
                "request",
                actual,
                MAX_PAIRING_MESSAGE_LEN,
            ) if actual > MAX_PAIRING_MESSAGE_LEN
        ));
    }

    #[test]
    fn pairing_offer_derives_inviter_peer_from_compact_private_key_config() {
        let mut config = config();
        let expected_peer = config.local_peer().expect("derived local peer");
        config.network.local_peer.clear();

        let offer = export_pairing_offer_at(
            &config,
            PairingOfferOptions {
                expires_in_seconds: 600,
                rendezvous_token: Some(URL_SAFE_NO_PAD.encode([14_u8; RENDEZVOUS_TOKEN_LEN])),
            },
            1_000,
        )
        .expect("offer");

        offer.verify_at(1_001).expect("verified");
        assert_eq!(offer.payload.inviter_peer, expected_peer);
    }

    #[test]
    fn pairing_offer_can_omit_inviter_addresses_for_discovery_only_accept() {
        let offer = export_discovery_only_pairing_offer_at(
            &config(),
            PairingOfferOptions {
                expires_in_seconds: 600,
                rendezvous_token: Some(URL_SAFE_NO_PAD.encode([8_u8; RENDEZVOUS_TOKEN_LEN])),
            },
            1_000,
        )
        .expect("offer");

        offer.verify_at(1_001).expect("verified");
        assert!(offer.payload.inviter_addresses.is_empty());
        assert!(offer.payload.bootstrap_peers.is_empty());
        assert!(offer.payload.discovery.kademlia);
    }

    #[test]
    fn discovery_only_pairing_offer_keeps_relay_reservation_hints() {
        let mut config = config();
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        config.network.relay.reservations = vec![format!(
            "/ip4/127.0.0.1/tcp/4001/p2p/{}/p2p-circuit",
            relay.peer_id
        )];
        let offer = export_discovery_only_pairing_offer_at(
            &config,
            PairingOfferOptions {
                expires_in_seconds: 600,
                rendezvous_token: Some(URL_SAFE_NO_PAD.encode([10_u8; RENDEZVOUS_TOKEN_LEN])),
            },
            1_000,
        )
        .expect("offer");

        let parsed = PairingOffer::from_uri(&offer.to_uri().expect("uri")).expect("parsed");

        parsed.verify_at(1_001).expect("verified");
        assert!(parsed.payload.inviter_addresses.is_empty());
        assert_eq!(
            parsed.payload.relay_reservations,
            config.network.relay.reservations
        );
    }

    #[test]
    fn discovery_only_pairing_request_carries_signed_offer() {
        let identity = NodeIdentity::generate_ed25519().expect("joiner identity");
        let offer = export_discovery_only_pairing_offer_at(
            &config(),
            PairingOfferOptions {
                expires_in_seconds: 600,
                rendezvous_token: Some(URL_SAFE_NO_PAD.encode([9_u8; RENDEZVOUS_TOKEN_LEN])),
            },
            1_000,
        )
        .expect("offer");

        let request = build_pairing_request_at(
            &offer,
            PairingRequestOptions {
                identity,
                requested_vpn_ip: Some("10.42.0.2".to_owned()),
                requested_routes: Vec::new(),
            },
            1_001,
        )
        .expect("request");

        assert_eq!(request.offer, Some(offer));
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
    fn pairing_request_rejects_invalid_requested_vpn_ip() {
        let offer = export_pairing_offer_at(&config(), PairingOfferOptions::default(), 1_000)
            .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");

        assert!(matches!(
            build_pairing_request_at(
                &offer,
                PairingRequestOptions {
                    identity: joiner,
                    requested_vpn_ip: Some("not-an-ip".to_owned()),
                    requested_routes: Vec::new(),
                },
                1_001,
            ),
            Err(PairingError::IpAddr(_))
        ));
    }

    #[test]
    fn pairing_request_rejects_invalid_requested_route() {
        let offer = export_pairing_offer_at(&config(), PairingOfferOptions::default(), 1_000)
            .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");

        assert!(matches!(
            build_pairing_request_at(
                &offer,
                PairingRequestOptions {
                    identity: joiner,
                    requested_vpn_ip: Some("10.42.0.2".to_owned()),
                    requested_routes: vec![RouteConfig {
                        prefix: "not-a-route".to_owned(),
                        metric: 100,
                    }],
                },
                1_001,
            ),
            Err(PairingError::Config(
                crate::config::ConfigError::RoutePrefix(_)
            ))
        ));
    }

    fn code_pairing_response_fixture() -> (Config, NodeIdentity, PairingOffer, PairingResponse) {
        let inviter_config = config();
        let inviter = NodeIdentity::from_private_key(
            inviter_config
                .network
                .private_key
                .as_deref()
                .expect("inviter private key"),
        )
        .expect("inviter identity");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let offer =
            export_code_pairing_offer_at(&inviter_config, PairingOfferOptions::default(), 1_000)
                .expect("code offer");
        let inviter_record = issue_membership_record_for_subject_at(
            &inviter,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&inviter).expect("inviter subject"),
                membership_epoch: 1,
                sequence: 1_010,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_010,
        )
        .expect("inviter record");
        let joiner_record = issue_membership_record_for_subject_at(
            &inviter,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&joiner).expect("joiner subject"),
                membership_epoch: 1,
                sequence: 1_010,
                revoked: false,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: vec![RouteConfig {
                    prefix: "10.42.0.2/32".to_owned(),
                    metric: 0,
                }],
                expires_at_unix_seconds: None,
            },
            1_010,
        )
        .expect("joiner record");
        let response = build_pairing_response_at(
            &inviter_config,
            &offer,
            PairingResponseOptions {
                joiner_peer: joiner.peer_id.clone(),
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                membership_key: Some(STANDARD.encode([9_u8; 32])),
                member_records: vec![inviter_record, joiner_record],
                expires_in_seconds: 300,
            },
            1_010,
        )
        .expect("response");
        (inviter_config, joiner, offer, response)
    }

    fn code_pairing_joiner_config(mut inviter_config: Config, joiner: &NodeIdentity) -> Config {
        inviter_config.network.local_peer = joiner.peer_id.clone();
        inviter_config.network.private_key = Some(joiner.private_key.clone());
        inviter_config.network.membership_key = None;
        inviter_config.network.member_records.clear();
        inviter_config.network.vpn_ip = None;
        inviter_config
    }

    fn resign_code_pairing_response(config: &Config, response: &mut PairingResponse) {
        let inviter = NodeIdentity::from_private_key(
            config
                .network
                .private_key
                .as_deref()
                .expect("inviter private key"),
        )
        .expect("inviter identity");
        response.signature = STANDARD.encode(
            inviter
                .sign(&response_signing_message(&response.payload).expect("message"))
                .expect("signature"),
        );
    }

    #[test]
    fn code_pairing_response_applies_additively_without_static_peer_authorization() {
        let (inviter_config, joiner, offer, response) = code_pairing_response_fixture();
        let joiner_config = code_pairing_joiner_config(inviter_config, &joiner);

        let applied =
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011)
                .expect("applied response");

        assert_eq!(applied.network.vpn_ip.as_deref(), Some("10.42.0.2"));
        assert_eq!(
            applied.network.membership_key.as_deref(),
            response.payload.membership_key.as_deref()
        );
        assert_eq!(
            applied.network.member_records,
            response.payload.member_records
        );
        assert!(applied.peers.is_empty());
        applied.validate_runtime().expect("runtime config");
    }

    #[test]
    fn code_pairing_response_accepts_builtin_ip_without_matching_route_grant() {
        let (inviter_config, joiner, offer, mut response) = code_pairing_response_fixture();
        let joiner_peer = joiner.peer_id.parse::<Libp2pPeerId>().expect("joiner peer");
        let builtin_ip = builtin_ipv4(PeerId::from_libp2p(joiner_peer)).to_string();
        response.payload.assigned_vpn_ip = Some(builtin_ip.clone());
        resign_code_pairing_response(&inviter_config, &mut response);
        let joiner_config = code_pairing_joiner_config(inviter_config, &joiner);

        let applied =
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011)
                .expect("built-in address is implicitly authorized");

        assert_eq!(applied.network.vpn_ip.as_deref(), Some(builtin_ip.as_str()));
    }

    #[test]
    fn code_pairing_response_rejects_membership_key_without_joiner_grant() {
        let (inviter_config, joiner, offer, mut response) = code_pairing_response_fixture();
        response
            .payload
            .member_records
            .retain(|record| record.payload.member_peer != joiner.peer_id);
        assert!(response.payload.membership_key.is_some());
        resign_code_pairing_response(&inviter_config, &mut response);
        let joiner_config = code_pairing_joiner_config(inviter_config, &joiner);

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::MissingMembershipGrant)
        ));
    }

    #[test]
    fn code_pairing_response_rejects_revoked_joiner_grant() {
        let (inviter_config, joiner, offer, mut response) = code_pairing_response_fixture();
        let inviter = NodeIdentity::from_private_key(
            inviter_config
                .network
                .private_key
                .as_deref()
                .expect("inviter private key"),
        )
        .expect("inviter identity");
        let revocation = issue_membership_record_for_subject_at(
            &inviter,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&joiner).expect("joiner subject"),
                membership_epoch: 1,
                sequence: 1_011,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_010,
        )
        .expect("joiner revocation");
        response.payload.member_records.push(revocation);
        resign_code_pairing_response(&inviter_config, &mut response);
        let joiner_config = code_pairing_joiner_config(inviter_config, &joiner);

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::MissingMembershipGrant)
        ));
    }

    #[test]
    fn code_pairing_response_rejects_joiner_record_from_another_issuer() {
        let (inviter_config, joiner, offer, mut response) = code_pairing_response_fixture();
        let other_issuer = NodeIdentity::generate_ed25519().expect("other issuer");
        let joiner_record = issue_membership_record_for_subject_at(
            &other_issuer,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&joiner).expect("joiner subject"),
                membership_epoch: 1,
                sequence: 1_011,
                revoked: false,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: vec![RouteConfig {
                    prefix: "10.42.0.2/32".to_owned(),
                    metric: 0,
                }],
                expires_at_unix_seconds: None,
            },
            1_010,
        )
        .expect("joiner record");
        response.payload.member_records[1] = joiner_record;
        resign_code_pairing_response(&inviter_config, &mut response);
        let joiner_config = code_pairing_joiner_config(inviter_config, &joiner);

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::MembershipIssuerMismatch { expected, actual })
                if expected == response.payload.inviter_peer
                    && actual == other_issuer.peer_id
        ));
    }

    #[test]
    fn code_pairing_response_rejects_assigned_ip_outside_joiner_route_grants() {
        let (inviter_config, joiner, offer, mut response) = code_pairing_response_fixture();
        response.payload.assigned_vpn_ip = Some("10.42.0.3".to_owned());
        resign_code_pairing_response(&inviter_config, &mut response);
        let joiner_config = code_pairing_joiner_config(inviter_config, &joiner);

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::AssignedVpnIpNotAuthorized { assigned })
                if assigned == "10.42.0.3"
        ));
    }

    #[test]
    fn code_pairing_response_requires_inviter_self_trust_root() {
        let (inviter_config, joiner, offer, mut response) = code_pairing_response_fixture();
        response.payload.member_records.remove(0);
        resign_code_pairing_response(&inviter_config, &mut response);
        let joiner_config = code_pairing_joiner_config(inviter_config, &joiner);

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::MissingInviterTrustRoot)
        ));
    }

    #[test]
    fn code_pairing_response_rejects_revoked_inviter_trust_root() {
        let (inviter_config, joiner, offer, mut response) = code_pairing_response_fixture();
        let inviter = NodeIdentity::from_private_key(
            inviter_config
                .network
                .private_key
                .as_deref()
                .expect("inviter private key"),
        )
        .expect("inviter identity");
        let revocation = issue_membership_record_for_subject_at(
            &inviter,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&inviter).expect("inviter subject"),
                membership_epoch: 1,
                sequence: 1_011,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_010,
        )
        .expect("inviter revocation");
        response.payload.member_records.push(revocation);
        resign_code_pairing_response(&inviter_config, &mut response);
        let joiner_config = code_pairing_joiner_config(inviter_config, &joiner);

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::MissingInviterTrustRoot)
        ));
    }

    #[test]
    fn code_pairing_response_rejects_an_unrelated_trust_root() {
        let (inviter_config, joiner, offer, mut response) = code_pairing_response_fixture();
        let unrelated_root = NodeIdentity::generate_ed25519().expect("unrelated root");
        response.payload.member_records.push(
            issue_membership_record_for_subject_at(
                &unrelated_root,
                MembershipRecordIssueOptions {
                    network_name: "lab".to_owned(),
                    member: MembershipRecordSubject::from_identity(&unrelated_root)
                        .expect("unrelated root subject"),
                    membership_epoch: 1,
                    sequence: 1_011,
                    revoked: false,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1_010,
            )
            .expect("unrelated root record"),
        );
        resign_code_pairing_response(&inviter_config, &mut response);
        let joiner_config = code_pairing_joiner_config(inviter_config, &joiner);

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::UnexpectedInviterTrustRoot { expected, actual })
                if expected == response.payload.inviter_peer
                    && actual == unrelated_root.peer_id
        ));
    }

    #[test]
    fn code_pairing_response_must_match_an_existing_network_trust_root() {
        let (inviter_config, joiner, offer, response) = code_pairing_response_fixture();
        let existing_root = NodeIdentity::generate_ed25519().expect("existing root");
        let mut joiner_config = code_pairing_joiner_config(inviter_config, &joiner);
        joiner_config.network.member_records.push(
            issue_membership_record_for_subject_at(
                &existing_root,
                MembershipRecordIssueOptions {
                    network_name: "lab".to_owned(),
                    member: MembershipRecordSubject::from_identity(&existing_root)
                        .expect("existing root subject"),
                    membership_epoch: 1,
                    sequence: 1_000,
                    revoked: false,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1_000,
            )
            .expect("existing root record"),
        );

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::UnexpectedInviterTrustRoot { expected, actual })
                if expected == existing_root.peer_id
                    && actual == response.payload.inviter_peer
        ));
    }

    #[test]
    fn code_pairing_response_rejects_receiver_side_legacy_multi_issuer_migration() {
        let (inviter_config, joiner, offer, response) = code_pairing_response_fixture();
        let legacy_issuer = NodeIdentity::generate_ed25519().expect("legacy issuer");
        let legacy_member = NodeIdentity::generate_ed25519().expect("legacy member");
        let mut joiner_config = code_pairing_joiner_config(inviter_config, &joiner);
        joiner_config.network.member_records.push(
            issue_membership_record_for_subject_at(
                &legacy_issuer,
                MembershipRecordIssueOptions {
                    network_name: "lab".to_owned(),
                    member: MembershipRecordSubject::from_identity(&legacy_member)
                        .expect("legacy member subject"),
                    membership_epoch: 1,
                    sequence: 1_000,
                    revoked: false,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1_000,
            )
            .expect("legacy member record"),
        );

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::LegacyMembershipMigrationRequired { inviter })
                if inviter == response.payload.inviter_peer
        ));
    }

    #[test]
    fn code_pairing_response_rejects_conflicting_membership_key() {
        let (inviter_config, joiner, offer, response) = code_pairing_response_fixture();
        let mut joiner_config = code_pairing_joiner_config(inviter_config, &joiner);
        joiner_config.network.membership_key = Some(STANDARD.encode([7_u8; 32]));

        assert!(matches!(
            apply_pairing_response_to_config_at(&joiner_config, &offer, &response, &joiner, 1_011,),
            Err(PairingError::ConflictingMembershipKey)
        ));
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
        assert!(imported.peers[0].routes.contains(&RouteConfig {
            prefix: "10.42.0.1/32".to_owned(),
            metric: 0,
        }));
        assert!(imported.network.bootstrap_peers.is_empty());
        assert!(imported.uses_public_ipfs_bootstrap_defaults());
        assert!(
            !imported
                .effective_bootstrap_multiaddrs()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn pairing_response_exports_relayed_inviter_address_from_reservation() {
        let mut inviter_config = config();
        let relay = NodeIdentity::generate_ed25519().expect("relay");
        inviter_config.network.relay.reservations = vec![format!(
            "/ip4/127.0.0.1/tcp/4001/p2p/{}/p2p-circuit",
            relay.peer_id
        )];
        let offer = export_discovery_only_pairing_offer_at(
            &inviter_config,
            PairingOfferOptions::default(),
            1_000,
        )
        .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let response = build_pairing_response_at(
            &inviter_config,
            &offer,
            PairingResponseOptions {
                joiner_peer: joiner.peer_id,
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                membership_key: Some(STANDARD.encode([9_u8; 32])),
                member_records: Vec::new(),
                expires_in_seconds: 300,
            },
            1_010,
        )
        .expect("response");

        assert!(response.payload.inviter_addresses.iter().any(|address| {
            address
                == &format!(
                    "/ip4/127.0.0.1/tcp/4001/p2p/{}/p2p-circuit/p2p/{}",
                    relay.peer_id, inviter_config.network.local_peer
                )
        }));
    }

    #[test]
    fn pairing_response_rejects_invalid_assigned_vpn_ip() {
        let inviter_config = config();
        let offer = export_pairing_offer_at(&inviter_config, PairingOfferOptions::default(), 1_000)
            .expect("offer");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner");
        let membership_key = STANDARD.encode([9_u8; 32]);

        assert!(matches!(
            build_pairing_response_at(
                &inviter_config,
                &offer,
                PairingResponseOptions {
                    joiner_peer: joiner.peer_id,
                    assigned_vpn_ip: Some("not-an-ip".to_owned()),
                    membership_key: Some(membership_key),
                    member_records: Vec::new(),
                    expires_in_seconds: 300,
                },
                1_010,
            ),
            Err(PairingError::IpAddr(_))
        ));
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
