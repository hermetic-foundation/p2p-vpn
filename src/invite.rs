use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use libp2p::{Multiaddr, PeerId as Libp2pPeerId, identity::PublicKey, multiaddr::Protocol};
use serde::{Deserialize, Serialize};

use crate::{
    config::{
        AutoRelayConfig, BootstrapPeerConfig, Config, DiscoveryConfig, InterfaceConfig, PeerConfig,
        QueueConfig, RelayConfig, ResourceConfig, RouteConfig, membership_tag,
    },
    identity::NodeIdentity,
    runtime::{control::CONTROL_PROTOCOL, packet::PACKET_PROTOCOL, service::SERVICE_PROTOCOL},
    wire::{HEADER_LEN, WIRE_VERSION},
};

pub const INVITE_VERSION: u8 = 1;

const SIGNING_DOMAIN: &[u8] = b"p2p-vpn signed invite v1\n";
const EXPECTED_PACKET_HEADER_LEN: usize = HEADER_LEN;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SignedInvite {
    pub payload: InvitePayload,
    pub signature: String,
}

impl SignedInvite {
    pub fn verify_at(&self, now_unix_seconds: u64) -> Result<(), InviteError> {
        validate_payload(&self.payload, now_unix_seconds)?;
        let public_key = decode_public_key(&self.payload.inviter_public_key)?;
        let inviter_peer = self.payload.inviter_peer.parse::<Libp2pPeerId>()?;
        if public_key.to_peer_id() != inviter_peer {
            return Err(InviteError::PublicKeyPeerMismatch {
                expected: self.payload.inviter_peer.clone(),
                actual: public_key.to_peer_id().to_string(),
            });
        }
        let signature = STANDARD.decode(&self.signature)?;
        if !public_key.verify(&signing_message(&self.payload)?, &signature) {
            return Err(InviteError::InvalidSignature);
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InvitePayload {
    pub version: u8,
    pub network_name: String,
    pub inviter_peer: String,
    pub inviter_public_key: String,
    #[serde(default)]
    pub membership_key: Option<String>,
    #[serde(default)]
    pub membership_tag: Option<String>,
    #[serde(default = "default_membership_epoch")]
    pub membership_epoch: u64,
    #[serde(default)]
    pub previous_membership_tags: Vec<String>,
    pub issued_at_unix_seconds: u64,
    #[serde(default)]
    pub expires_at_unix_seconds: Option<u64>,
    #[serde(default)]
    pub inviter_addresses: Vec<String>,
    #[serde(default)]
    pub inviter_routes: Vec<RouteConfig>,
    #[serde(default)]
    pub bootstrap_peers: Vec<BootstrapPeerConfig>,
    #[serde(default)]
    pub relay_reservations: Vec<String>,
    pub discovery: DiscoveryConfig,
    pub protocols: InviteProtocols,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InviteProtocols {
    pub control: String,
    pub packet: String,
    pub service: String,
    pub wire_version: u8,
    pub packet_header_len: usize,
}

impl Default for InviteProtocols {
    fn default() -> Self {
        Self {
            control: CONTROL_PROTOCOL.to_owned(),
            packet: PACKET_PROTOCOL.to_owned(),
            service: SERVICE_PROTOCOL.to_owned(),
            wire_version: WIRE_VERSION,
            packet_header_len: EXPECTED_PACKET_HEADER_LEN,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InviteExportOptions {
    pub expires_at_unix_seconds: Option<u64>,
    pub membership_epoch: u64,
    pub previous_membership_tags: Vec<String>,
}

impl Default for InviteExportOptions {
    fn default() -> Self {
        Self {
            expires_at_unix_seconds: None,
            membership_epoch: default_membership_epoch(),
            previous_membership_tags: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InviteImportOptions {
    pub identity: NodeIdentity,
    pub interface_name: String,
    pub mtu: u16,
    pub local_routes: Vec<RouteConfig>,
    pub peer_name: Option<String>,
}

pub fn export_signed_invite_at(
    config: &Config,
    options: InviteExportOptions,
    issued_at_unix_seconds: u64,
) -> Result<SignedInvite, InviteError> {
    config.validate_runtime()?;
    if let Some(expires_at) = options.expires_at_unix_seconds
        && expires_at <= issued_at_unix_seconds
    {
        return Err(InviteError::ExpiredBeforeIssued);
    }
    validate_membership_tags(&options.previous_membership_tags)?;
    let identity = config.identity()?;
    let payload = InvitePayload {
        version: INVITE_VERSION,
        network_name: config.network.name.clone(),
        inviter_peer: config.network.local_peer.clone(),
        inviter_public_key: STANDARD.encode(identity.public_key_protobuf()?),
        membership_key: config.network.membership_key.clone(),
        membership_tag: config.membership_tag()?,
        membership_epoch: options.membership_epoch.max(1),
        previous_membership_tags: options.previous_membership_tags,
        issued_at_unix_seconds,
        expires_at_unix_seconds: options.expires_at_unix_seconds,
        inviter_addresses: exported_inviter_addresses(config),
        inviter_routes: config.network.routes.clone(),
        bootstrap_peers: config.network.bootstrap_peers.clone(),
        relay_reservations: config.network.relay.reservations.clone(),
        discovery: config.network.discovery.clone(),
        protocols: InviteProtocols::default(),
    };
    validate_payload(&payload, issued_at_unix_seconds)?;
    let signature = STANDARD.encode(identity.sign(&signing_message(&payload)?)?);

    Ok(SignedInvite { payload, signature })
}

pub fn export_signed_invite(
    config: &Config,
    options: InviteExportOptions,
) -> Result<SignedInvite, InviteError> {
    export_signed_invite_at(config, options, current_unix_seconds()?)
}

pub fn import_invite_config_at(
    invite: &SignedInvite,
    options: InviteImportOptions,
    now_unix_seconds: u64,
) -> Result<Config, InviteError> {
    invite.verify_at(now_unix_seconds)?;
    if options.identity.peer_id == invite.payload.inviter_peer {
        return Err(InviteError::CannotImportSelf);
    }

    let mut bootstrap_peers = invite.payload.bootstrap_peers.clone();
    for address in &invite.payload.inviter_addresses {
        upsert_bootstrap_peer(
            &mut bootstrap_peers,
            &invite.payload.inviter_peer,
            address.clone(),
        );
    }

    let config = Config {
        network: crate::config::NetworkConfig {
            name: invite.payload.network_name.clone(),
            local_peer: options.identity.peer_id,
            private_key: Some(options.identity.private_key),
            membership_key: invite.payload.membership_key.clone(),
            previous_membership_tags: invite.payload.previous_membership_tags.clone(),
            member_records: Vec::new(),
            routes: options.local_routes,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers,
            discovery: invite.payload.discovery.clone(),
            relay: RelayConfig {
                server: false,
                reservations: invite.payload.relay_reservations.clone(),
                auto: AutoRelayConfig::default(),
                resources: crate::config::RelayResourceConfig::default(),
            },
            packet_plane: crate::config::PacketPlaneConfig::default(),
        },
        interface: InterfaceConfig {
            name: options.interface_name,
            mtu: options.mtu,
        },
        peers: vec![PeerConfig {
            id: invite.payload.inviter_peer.clone(),
            name: options.peer_name,
            ip: None,
            addresses: invite.payload.inviter_addresses.clone(),
            routes: invite.payload.inviter_routes.clone(),
        }],
        queue: QueueConfig::default(),
        resources: ResourceConfig::default(),
    };
    config.validate_runtime()?;

    Ok(config)
}

pub fn import_invite_config(
    invite: &SignedInvite,
    options: InviteImportOptions,
) -> Result<Config, InviteError> {
    import_invite_config_at(invite, options, current_unix_seconds()?)
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

fn validate_payload(payload: &InvitePayload, now_unix_seconds: u64) -> Result<(), InviteError> {
    if payload.version != INVITE_VERSION {
        return Err(InviteError::UnsupportedVersion(payload.version));
    }
    if payload.network_name.is_empty() {
        return Err(InviteError::EmptyNetworkName);
    }
    if payload.membership_epoch == 0 {
        return Err(InviteError::InvalidMembershipEpoch);
    }
    if let Some(expires_at) = payload.expires_at_unix_seconds
        && now_unix_seconds > expires_at
    {
        return Err(InviteError::Expired {
            expired_at: expires_at,
            now: now_unix_seconds,
        });
    }
    validate_membership(payload)?;
    validate_membership_tags(&payload.previous_membership_tags)?;
    validate_discovery(&payload.discovery)?;
    validate_protocols(&payload.protocols)?;
    validate_inviter_addresses(payload)?;
    validate_routes(&payload.inviter_routes)?;
    validate_bootstrap_peers(&payload.bootstrap_peers)?;
    validate_multiaddrs(&payload.relay_reservations)?;

    Ok(())
}

fn validate_membership(payload: &InvitePayload) -> Result<(), InviteError> {
    match (&payload.membership_key, &payload.membership_tag) {
        (Some(key), Some(tag)) => {
            let key = decode_membership_key(key)?;
            let expected = membership_tag(&payload.network_name, &key);
            if expected != *tag {
                return Err(InviteError::MembershipTagMismatch);
            }
        }
        (None, None) => {}
        _ => return Err(InviteError::MembershipFieldsMismatch),
    }

    Ok(())
}

fn validate_membership_tags(tags: &[String]) -> Result<(), InviteError> {
    for tag in tags {
        let decoded = STANDARD.decode(tag)?;
        if decoded.len() != 32 {
            return Err(InviteError::InvalidPreviousMembershipTag);
        }
    }

    Ok(())
}

fn validate_discovery(discovery: &DiscoveryConfig) -> Result<(), InviteError> {
    if discovery.kademlia_provider_advertisement && !discovery.kademlia {
        return Err(InviteError::IncompatibleDiscovery);
    }
    if !discovery.kademlia_protocol.starts_with('/') {
        return Err(InviteError::InvalidKademliaProtocol);
    }

    Ok(())
}

fn validate_protocols(protocols: &InviteProtocols) -> Result<(), InviteError> {
    if protocols.control != CONTROL_PROTOCOL
        || protocols.packet != PACKET_PROTOCOL
        || protocols.service != SERVICE_PROTOCOL
        || protocols.wire_version != WIRE_VERSION
        || protocols.packet_header_len != EXPECTED_PACKET_HEADER_LEN
    {
        return Err(InviteError::IncompatibleProtocols);
    }

    Ok(())
}

fn validate_inviter_addresses(payload: &InvitePayload) -> Result<(), InviteError> {
    let inviter = payload.inviter_peer.parse::<Libp2pPeerId>()?;
    for address in &payload.inviter_addresses {
        validate_peer_address(&inviter, address)?;
    }

    Ok(())
}

fn validate_peer_address(peer: &Libp2pPeerId, address: &str) -> Result<(), InviteError> {
    let address = address.parse::<Multiaddr>()?;
    let Some(target) = peer_address_target(&address) else {
        return Ok(());
    };
    if target != *peer {
        return Err(InviteError::AddressPeerMismatch {
            expected: peer.to_string(),
            actual: target.to_string(),
        });
    }

    Ok(())
}

fn peer_address_target(address: &Multiaddr) -> Option<Libp2pPeerId> {
    let mut direct_target = None;
    let mut relayed_target = None;
    let mut after_circuit = false;

    for protocol in address {
        match protocol {
            Protocol::P2p(peer) if after_circuit => relayed_target = Some(peer),
            Protocol::P2p(peer) => direct_target = Some(peer),
            Protocol::P2pCircuit => after_circuit = true,
            _ => {}
        }
    }

    if after_circuit {
        relayed_target
    } else {
        direct_target
    }
}

fn validate_routes(routes: &[RouteConfig]) -> Result<(), InviteError> {
    for route in routes {
        route.prefix()?;
    }

    Ok(())
}

fn validate_bootstrap_peers(peers: &[BootstrapPeerConfig]) -> Result<(), InviteError> {
    for peer in peers {
        let peer_id = peer.id.parse::<Libp2pPeerId>()?;
        validate_peer_address(&peer_id, &peer.address)?;
    }

    Ok(())
}

fn validate_multiaddrs(addresses: &[String]) -> Result<(), InviteError> {
    for address in addresses {
        validate_relay_reservation(address)?;
    }

    Ok(())
}

fn validate_relay_reservation(address: &str) -> Result<(), InviteError> {
    let address = address.parse::<Multiaddr>()?;
    let mut relay_peer = None;
    let mut saw_circuit = false;
    for protocol in &address {
        match protocol {
            Protocol::P2p(_) if saw_circuit => {
                return Err(InviteError::UnexpectedRelayTarget);
            }
            Protocol::P2p(peer) => relay_peer = Some(peer),
            Protocol::P2pCircuit if relay_peer.is_some() => saw_circuit = true,
            Protocol::P2pCircuit => return Err(InviteError::MissingRelayPeer),
            _ => {}
        }
    }

    if saw_circuit {
        Ok(())
    } else {
        Err(InviteError::MissingRelayCircuit)
    }
}

fn upsert_bootstrap_peer(peers: &mut Vec<BootstrapPeerConfig>, id: &str, address: String) {
    if peers
        .iter()
        .any(|peer| peer.id == id && peer.address == address)
    {
        return;
    }
    peers.push(BootstrapPeerConfig {
        id: id.to_owned(),
        address,
    });
}

fn signing_message(payload: &InvitePayload) -> Result<Vec<u8>, InviteError> {
    let payload = serde_json::to_vec(payload)?;
    let mut message = Vec::with_capacity(SIGNING_DOMAIN.len() + payload.len());
    message.extend_from_slice(SIGNING_DOMAIN);
    message.extend_from_slice(&payload);
    Ok(message)
}

fn decode_public_key(encoded: &str) -> Result<PublicKey, InviteError> {
    let bytes = STANDARD.decode(encoded)?;
    Ok(PublicKey::try_decode_protobuf(&bytes)?)
}

fn decode_membership_key(input: &str) -> Result<Vec<u8>, InviteError> {
    let key = STANDARD.decode(input)?;
    if key.len() < 32 {
        return Err(InviteError::MembershipKeyTooShort {
            actual: key.len(),
            minimum: 32,
        });
    }

    Ok(key)
}

fn current_unix_seconds() -> Result<u64, InviteError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| InviteError::SystemTimeBeforeEpoch)?
        .as_secs())
}

const fn default_membership_epoch() -> u64 {
    1
}

#[derive(Debug)]
pub enum InviteError {
    Config(crate::config::ConfigError),
    Identity(crate::identity::IdentityError),
    Json(serde_json::Error),
    Base64(base64::DecodeError),
    Libp2pIdentity(libp2p::identity::DecodingError),
    Libp2pPeerId(libp2p::identity::ParseError),
    Multiaddr(libp2p::multiaddr::Error),
    RoutePrefix(crate::config::RoutePrefixError),
    UnsupportedVersion(u8),
    EmptyNetworkName,
    InvalidMembershipEpoch,
    MembershipKeyTooShort { actual: usize, minimum: usize },
    MembershipFieldsMismatch,
    MembershipTagMismatch,
    InvalidPreviousMembershipTag,
    IncompatibleDiscovery,
    InvalidKademliaProtocol,
    IncompatibleProtocols,
    PublicKeyPeerMismatch { expected: String, actual: String },
    AddressPeerMismatch { expected: String, actual: String },
    MissingRelayCircuit,
    MissingRelayPeer,
    UnexpectedRelayTarget,
    InvalidSignature,
    Expired { expired_at: u64, now: u64 },
    ExpiredBeforeIssued,
    CannotImportSelf,
    SystemTimeBeforeEpoch,
}

impl From<crate::config::ConfigError> for InviteError {
    fn from(error: crate::config::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<crate::identity::IdentityError> for InviteError {
    fn from(error: crate::identity::IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<serde_json::Error> for InviteError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<base64::DecodeError> for InviteError {
    fn from(error: base64::DecodeError) -> Self {
        Self::Base64(error)
    }
}

impl From<libp2p::identity::DecodingError> for InviteError {
    fn from(error: libp2p::identity::DecodingError) -> Self {
        Self::Libp2pIdentity(error)
    }
}

impl From<libp2p::identity::ParseError> for InviteError {
    fn from(error: libp2p::identity::ParseError) -> Self {
        Self::Libp2pPeerId(error)
    }
}

impl From<libp2p::multiaddr::Error> for InviteError {
    fn from(error: libp2p::multiaddr::Error) -> Self {
        Self::Multiaddr(error)
    }
}

impl From<crate::config::RoutePrefixError> for InviteError {
    fn from(error: crate::config::RoutePrefixError) -> Self {
        Self::RoutePrefix(error)
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use crate::{
        config::{BootstrapPeerConfig, NetworkConfig, RelayConfig},
        identity::NodeIdentity,
    };

    use super::*;

    fn config_with_membership() -> Config {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: identity.peer_id,
                private_key: Some(identity.private_key),
                membership_key: Some(STANDARD.encode([7_u8; 32])),
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 100,
                }],
                listen_addresses: vec!["/ip4/0.0.0.0/tcp/4001".to_owned()],
                external_addresses: vec!["/dns4/node-a.example.net/udp/4001/quic-v1".to_owned()],
                bootstrap_peers: Vec::new(),
                discovery: DiscoveryConfig::default(),
                relay: RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig::default(),
            resources: ResourceConfig::default(),
        }
    }

    #[test]
    fn signed_invite_round_trips_into_runtime_config() {
        let source = config_with_membership();
        let invited = NodeIdentity::generate_ed25519().expect("invited identity");
        let invite = export_signed_invite_at(
            &source,
            InviteExportOptions {
                expires_at_unix_seconds: Some(2_000),
                membership_epoch: 3,
                previous_membership_tags: vec![STANDARD.encode([9_u8; 32])],
            },
            1_000,
        )
        .expect("invite");

        invite.verify_at(1_500).expect("verified invite");
        let imported = import_invite_config_at(
            &invite,
            InviteImportOptions {
                identity: invited.clone(),
                interface_name: "hs1".to_owned(),
                mtu: 1400,
                local_routes: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 100,
                }],
                peer_name: Some("node-a".to_owned()),
            },
            1_500,
        )
        .expect("imported config");

        assert_eq!(imported.network.name, "lab");
        assert_eq!(imported.network.local_peer, invited.peer_id);
        assert_eq!(
            imported.network.membership_key,
            source.network.membership_key
        );
        assert_eq!(
            imported.network.previous_membership_tags,
            invite.payload.previous_membership_tags
        );
        assert_eq!(imported.network.bootstrap_peers.len(), 2);
        assert_eq!(imported.interface.name, "hs1");
        assert_eq!(imported.interface.mtu, 1400);
        assert_eq!(imported.peers.len(), 1);
        assert_eq!(imported.peers[0].id, source.network.local_peer);
        assert_eq!(imported.peers[0].routes, source.network.routes);
        imported.validate_runtime().expect("runtime config");
    }

    #[test]
    fn signed_invite_round_trips_relay_assisted_reachability() {
        let mut source = config_with_membership();
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let relayed_inviter_address = format!(
            "/dns4/relay.example.net/tcp/4001/p2p/{}/p2p-circuit/p2p/{}",
            relay.peer_id, source.network.local_peer
        );
        source.network.external_addresses = vec![relayed_inviter_address.clone()];
        source.network.listen_addresses = Vec::new();
        source.network.bootstrap_peers = vec![BootstrapPeerConfig {
            id: relay.peer_id.clone(),
            address: format!("/dns4/relay.example.net/tcp/4001/p2p/{}", relay.peer_id),
        }];
        source.network.relay.reservations = vec![format!(
            "/dns4/relay.example.net/tcp/4001/p2p/{}/p2p-circuit",
            relay.peer_id
        )];
        let invited = NodeIdentity::generate_ed25519().expect("invited identity");

        let invite = export_signed_invite_at(&source, InviteExportOptions::default(), 1_000)
            .expect("invite");
        invite.verify_at(1_000).expect("verified invite");
        let imported = import_invite_config_at(
            &invite,
            InviteImportOptions {
                identity: invited,
                interface_name: "hs1".to_owned(),
                mtu: 1280,
                local_routes: Vec::new(),
                peer_name: Some("node-a".to_owned()),
            },
            1_000,
        )
        .expect("imported config");

        assert_eq!(
            invite.payload.inviter_addresses,
            vec![relayed_inviter_address]
        );
        assert_eq!(
            invite.payload.bootstrap_peers,
            source.network.bootstrap_peers
        );
        assert_eq!(
            invite.payload.relay_reservations,
            source.network.relay.reservations
        );
        assert_eq!(imported.network.bootstrap_peers.len(), 2);
        assert_eq!(
            imported.network.relay.reservations,
            source.network.relay.reservations
        );
        assert_eq!(
            imported.peers[0].addresses,
            invite.payload.inviter_addresses
        );
        imported.validate_runtime().expect("runtime config");
    }

    #[test]
    fn invite_rejects_tampered_payload() {
        let source = config_with_membership();
        let mut invite = export_signed_invite_at(&source, InviteExportOptions::default(), 1_000)
            .expect("invite");
        invite.payload.membership_epoch += 1;

        assert!(matches!(
            invite.verify_at(1_000),
            Err(InviteError::InvalidSignature)
        ));
    }

    #[test]
    fn invite_rejects_wrong_public_key_binding() {
        let source = config_with_membership();
        let other = NodeIdentity::generate_ed25519().expect("other identity");
        let mut invite = export_signed_invite_at(&source, InviteExportOptions::default(), 1_000)
            .expect("invite");
        invite.payload.inviter_public_key =
            STANDARD.encode(other.public_key_protobuf().expect("other public key"));
        invite.signature = STANDARD.encode(
            other
                .sign(&signing_message(&invite.payload).expect("message"))
                .expect("signature"),
        );

        assert!(matches!(
            invite.verify_at(1_000),
            Err(InviteError::PublicKeyPeerMismatch { .. })
        ));
    }

    #[test]
    fn invite_rejects_expired_payload() {
        let source = config_with_membership();
        let invite = export_signed_invite_at(
            &source,
            InviteExportOptions {
                expires_at_unix_seconds: Some(1_100),
                ..InviteExportOptions::default()
            },
            1_000,
        )
        .expect("invite");

        assert!(matches!(
            invite.verify_at(1_101),
            Err(InviteError::Expired {
                expired_at: 1_100,
                now: 1_101
            })
        ));
    }

    #[test]
    fn invite_rejects_incompatible_protocols() {
        let source = config_with_membership();
        let mut invite = export_signed_invite_at(&source, InviteExportOptions::default(), 1_000)
            .expect("invite");
        invite.payload.protocols.wire_version = WIRE_VERSION.saturating_add(1);
        let identity = source.identity().expect("identity");
        invite.signature = STANDARD.encode(
            identity
                .sign(&signing_message(&invite.payload).expect("message"))
                .expect("signature"),
        );

        assert!(matches!(
            invite.verify_at(1_000),
            Err(InviteError::IncompatibleProtocols)
        ));
    }

    #[test]
    fn invite_rejects_malformed_relay_reservation_payloads() {
        let source = config_with_membership();
        let relay = NodeIdentity::generate_ed25519().expect("relay identity");
        let mut invite = export_signed_invite_at(&source, InviteExportOptions::default(), 1_000)
            .expect("invite");

        invite.payload.relay_reservations = vec![format!(
            "/dns4/relay.example.net/tcp/4001/p2p/{}",
            relay.peer_id
        )];
        assert!(matches!(
            invite.verify_at(1_000),
            Err(InviteError::MissingRelayCircuit)
        ));

        invite.payload.relay_reservations = vec![format!(
            "/dns4/relay.example.net/tcp/4001/p2p/{}/p2p-circuit/p2p/{}",
            relay.peer_id, source.network.local_peer
        )];
        assert!(matches!(
            invite.verify_at(1_000),
            Err(InviteError::UnexpectedRelayTarget)
        ));
    }
}
