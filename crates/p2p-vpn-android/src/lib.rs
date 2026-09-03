use std::{
    fs,
    io::Read as _,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
};

#[cfg(any(target_os = "android", test))]
use std::{collections::BTreeSet, path::PathBuf};

use p2p_vpn::{
    PeerId,
    config::{BootstrapPeerConfig, Config, DiscoveryConfig, PeerConfig, RouteConfig},
    dns::canonical_dns_label,
    identity::NodeIdentity,
    membership::SignedMembershipRecord,
    network_peer::NetworkPeerSnapshot,
    route::IpCidr,
    runtime::{control_socket::PairRpcCompletionArtifacts, tun::TunRuntimeConfig},
};
#[cfg(any(target_os = "android", test))]
use p2p_vpn::{
    pairing::{PairingConfigOptions, import_pairing_response_config_at},
    runtime::pairing_bootstrap::PairingBootstrapEnrollment,
};
#[cfg(any(target_os = "android", test))]
use serde::Deserialize;
use serde::Serialize;

#[cfg(any(target_os = "android", test))]
mod packet_translation;
#[cfg(any(target_os = "android", test))]
mod recovery;
#[cfg(any(target_os = "android", test))]
mod supervisor;

const BUILTIN_IPV4_NETWORK: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 0);
const BUILTIN_IPV4_PREFIX: u8 = 16;
const BUILTIN_IPV6_NETWORK: Ipv6Addr =
    Ipv6Addr::new(0xfd00, 0x6879, 0x7072, 0x7370, 0x6163, 0x6500, 0, 0);
const BUILTIN_IPV6_PREFIX: u8 = 96;
const E2E_PACKET_QUIC_ENDPOINT_MAX_LENGTH: usize = 512;
const E2E_RELAY_RESERVATION_MAX_LENGTH: usize = 1_024;
const E2E_ADDITIONAL_ROUTE_MAX_LENGTH: usize = 64;
#[cfg(any(target_os = "android", test))]
const CONTROL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(any(target_os = "android", test))]
const ANDROID_RUNTIME_START_SCHEMA_VERSION: u8 = 1;
#[cfg(any(target_os = "android", test))]
const ANDROID_RUNTIME_START_MAX_BYTES: usize = 8 * 1024 * 1024;
#[cfg(any(target_os = "android", test))]
const ANDROID_RUNTIME_CONFIG_MAX_BYTES: usize = 2 * 1024 * 1024;
#[cfg(any(target_os = "android", test))]
const ANDROID_RUNTIME_STATE_PATH_MAX_BYTES: usize = 4_096;
#[cfg(any(target_os = "android", test))]
const ANDROID_NETWORK_ID_LENGTH: usize = 36;

#[cfg(any(target_os = "android", test))]
fn block_on_control<T>(
    future: impl std::future::Future<Output = std::io::Result<T>>,
) -> Result<T, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to create control runtime: {error}"))?;
    runtime
        .block_on(async move { tokio::time::timeout(CONTROL_TIMEOUT, future).await })
        .map_err(|_| "runtime control request timed out".to_owned())?
        .map_err(|error| format!("runtime control request failed: {error}"))
}

#[derive(Serialize)]
pub struct AndroidProfile {
    pub config_json: String,
    pub network_name: String,
    pub hostname: String,
    pub peer_id: String,
    pub interface_name: String,
    pub mtu: u16,
    pub addresses: Vec<AndroidCidr>,
    pub routes: Vec<AndroidCidr>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AndroidCidr {
    pub address: String,
    pub prefix_length: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AndroidRuntimeStatus {
    pub phase: AndroidRuntimePhase,
    pub detail: Option<String>,
    pub lines: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<AndroidNetworkRuntimeStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AndroidNetworkRuntimeStatus {
    pub id: String,
    pub phase: AndroidRuntimePhase,
    pub detail: Option<String>,
    pub lines: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_snapshot: Option<NetworkPeerSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AndroidRuntimePhase {
    Starting,
    Running,
    Stopped,
    Failed,
}

#[cfg(any(target_os = "android", test))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AndroidRuntimeStartRequest {
    schema_version: u8,
    presentation_addresses: AndroidRuntimePresentationAddresses,
    networks: Vec<AndroidRuntimeNetworkRequest>,
}

#[cfg(any(target_os = "android", test))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AndroidRuntimePresentationAddresses {
    ipv4: String,
    ipv6: String,
}

#[cfg(any(target_os = "android", test))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AndroidRuntimeNetworkRequest {
    id: String,
    config_json: String,
    pairing_state_path: String,
    membership_state_path: String,
}

#[cfg(any(target_os = "android", test))]
struct PreparedAndroidRuntimeStart {
    presentation: packet_translation::PrimaryAddresses,
    networks: Vec<PreparedAndroidRuntimeNetwork>,
    tun_mtu: u16,
}

#[cfg(any(target_os = "android", test))]
struct PreparedAndroidRuntimeNetwork {
    id: String,
    config: Config,
    tun: TunRuntimeConfig,
    pairing_state_path: PathBuf,
    membership_state_path: PathBuf,
}

#[cfg(any(target_os = "android", test))]
#[derive(Debug, Eq, PartialEq, Serialize)]
struct AndroidRuntimeValidation {
    networks: usize,
    mtu: u16,
}

#[cfg(any(target_os = "android", test))]
fn prepare_android_runtime_start(
    request_json: &str,
) -> Result<PreparedAndroidRuntimeStart, String> {
    if request_json.is_empty() || request_json.len() > ANDROID_RUNTIME_START_MAX_BYTES {
        return Err("Android runtime start request has an invalid size".to_owned());
    }
    let request: AndroidRuntimeStartRequest = serde_json::from_str(request_json)
        .map_err(|error| format!("invalid Android runtime start request: {error}"))?;
    if request.schema_version != ANDROID_RUNTIME_START_SCHEMA_VERSION {
        return Err(format!(
            "unsupported Android runtime start schema version: {}",
            request.schema_version
        ));
    }
    if request.networks.is_empty() || request.networks.len() > supervisor::MAX_NETWORKS {
        return Err(format!(
            "Android runtime start request requires 1 to {} networks",
            supervisor::MAX_NETWORKS
        ));
    }

    let presentation = packet_translation::PrimaryAddresses {
        ipv4: request
            .presentation_addresses
            .ipv4
            .parse()
            .map_err(|_| "Android runtime presentation IPv4 address is invalid".to_owned())?,
        ipv6: request
            .presentation_addresses
            .ipv6
            .parse()
            .map_err(|_| "Android runtime presentation IPv6 address is invalid".to_owned())?,
    };
    let mut ids = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut network_names = BTreeSet::new();
    let mut dns_zones = BTreeSet::new();
    let mut state_directories = BTreeSet::new();
    let mut networks = Vec::with_capacity(request.networks.len());
    let mut tun_mtu = u16::MAX;

    for network in request.networks {
        if !is_canonical_android_network_id(&network.id) {
            return Err("Android runtime start request contains an invalid network ID".to_owned());
        }
        if !ids.insert(network.id.clone()) {
            return Err("Android runtime start request duplicates a network ID".to_owned());
        }
        if network.config_json.is_empty()
            || network.config_json.len() > ANDROID_RUNTIME_CONFIG_MAX_BYTES
        {
            return Err(format!(
                "Android runtime network {} has an invalid configuration size",
                network.id
            ));
        }
        let config: Config = serde_json::from_str(&network.config_json)
            .map_err(|error| format!("invalid profile JSON for network {}: {error}", network.id))?;
        config
            .validate_runtime()
            .map_err(|error| format!("invalid profile for network {}: {error:?}", network.id))?;
        let identity = config.identity().map_err(|error| {
            format!(
                "invalid profile identity for network {}: {error:?}",
                network.id
            )
        })?;
        if !identities.insert(identity.peer_id) {
            return Err("Android runtime start request reuses a network identity".to_owned());
        }
        if config.network.dns.enabled {
            let zone = canonical_dns_label(&config.network.name).map_err(|_| {
                format!(
                    "Android runtime network {} has an invalid DNS zone",
                    network.id
                )
            })?;
            if !dns_zones.insert(zone) {
                return Err("Android runtime start request duplicates a DNS zone".to_owned());
            }
        }
        if !network_names.insert(config.network.name.to_lowercase()) {
            return Err("Android runtime start request duplicates a network name".to_owned());
        }
        let (pairing_state_path, membership_state_path, state_directory) =
            validate_android_runtime_state_paths(
                &network.id,
                &network.pairing_state_path,
                &network.membership_state_path,
            )?;
        if !state_directories.insert(state_directory) {
            return Err("Android runtime start request reuses a state directory".to_owned());
        }
        let tun = TunRuntimeConfig::from_config(&config).map_err(|error| {
            format!(
                "invalid profile routes for network {}: {error:?}",
                network.id
            )
        })?;
        tun_mtu = tun_mtu.min(tun.mtu);
        networks.push(PreparedAndroidRuntimeNetwork {
            id: network.id,
            config,
            tun,
            pairing_state_path,
            membership_state_path,
        });
    }

    Ok(PreparedAndroidRuntimeStart {
        presentation,
        networks,
        tun_mtu,
    })
}

#[cfg(any(target_os = "android", test))]
fn validate_android_runtime_start(request_json: &str) -> Result<AndroidRuntimeValidation, String> {
    let prepared = prepare_android_runtime_start(request_json)?;
    let network_count = prepared.networks.len();
    let network_specs = prepared
        .networks
        .into_iter()
        .map(|network| supervisor::NetworkSpec {
            id: network.id,
            tun: network.tun,
        })
        .collect();
    let (packet_switch, _ports) = supervisor::PacketSwitch::new_with_presentation(
        network_specs,
        prepared.presentation,
        supervisor::QueueLimits::DEFAULT,
    )
    .map_err(|error| error.to_string())?;
    packet_switch.close();
    Ok(AndroidRuntimeValidation {
        networks: network_count,
        mtu: prepared.tun_mtu,
    })
}

#[cfg(any(target_os = "android", test))]
fn is_canonical_android_network_id(value: &str) -> bool {
    value.len() == ANDROID_NETWORK_ID_LENGTH
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

#[cfg(any(target_os = "android", test))]
fn validate_android_runtime_state_paths(
    network_id: &str,
    pairing_state_path: &str,
    membership_state_path: &str,
) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let pairing =
        validate_android_runtime_state_path(network_id, pairing_state_path, "pairing-state.json")?;
    let membership = validate_android_runtime_state_path(
        network_id,
        membership_state_path,
        "membership-state.json",
    )?;
    let state_directory = pairing
        .parent()
        .ok_or_else(|| format!("Android runtime network {network_id} has no state directory"))?
        .to_path_buf();
    if state_directory.file_name().and_then(|name| name.to_str()) != Some(network_id) {
        return Err(format!(
            "Android runtime network {network_id} state directory does not match its network ID"
        ));
    }
    if membership.parent() != Some(state_directory.as_path()) {
        return Err(format!(
            "Android runtime network {network_id} splits its state directory"
        ));
    }
    Ok((pairing, membership, state_directory))
}

#[cfg(any(target_os = "android", test))]
fn validate_android_runtime_state_path(
    network_id: &str,
    value: &str,
    expected_file_name: &str,
) -> Result<PathBuf, String> {
    if value.is_empty()
        || value.len() > ANDROID_RUNTIME_STATE_PATH_MAX_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(format!(
            "Android runtime network {network_id} has an invalid state path"
        ));
    }
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || path.file_name().and_then(|name| name.to_str()) != Some(expected_file_name)
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(format!(
            "Android runtime network {network_id} has an invalid state path"
        ));
    }
    Ok(path)
}

#[cfg(target_os = "android")]
#[derive(Serialize)]
struct BridgeResponse<T> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub fn create_profile(network_name: &str) -> Result<AndroidProfile, String> {
    let network_name = validate_network_name(network_name)?;
    let identity = NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate identity: {error:?}"))?;
    create_profile_with_identity(network_name, identity, None, None, None, None)
}

pub fn create_profile_with_hostname(
    network_name: &str,
    hostname: &str,
) -> Result<AndroidProfile, String> {
    let profile = create_profile(network_name)?;
    let mut config: Config = serde_json::from_str(&profile.config_json)
        .map_err(|error| format!("failed to decode generated profile: {error}"))?;
    let hostname = canonical_dns_label(hostname)
        .map_err(|_| "Android profile contains an invalid hostname".to_owned())?;
    config.network.dns.hostname = Some(hostname);
    let config_json = serde_json::to_string(&config)
        .map_err(|error| format!("failed to encode generated profile: {error}"))?;
    inspect_profile(&config_json)
}

pub fn rename_profile(config_json: &str, hostname: &str) -> Result<AndroidProfile, String> {
    let before = inspect_profile(config_json)?;
    let mut config: Config = serde_json::from_str(config_json)
        .map_err(|error| format!("invalid profile JSON: {error}"))?;
    let hostname = canonical_dns_label(hostname)
        .map_err(|_| "Android profile contains an invalid hostname".to_owned())?;
    config.network.dns.hostname = Some(hostname);
    let encoded = serde_json::to_string(&config)
        .map_err(|error| format!("failed to encode renamed profile: {error}"))?;
    let after = inspect_profile(&encoded)?;
    if after.peer_id != before.peer_id || after.addresses != before.addresses {
        return Err("renaming a profile changed its network identity".to_owned());
    }
    Ok(after)
}

pub fn create_profile_with_bootstrap(
    network_name: &str,
    bootstrap_peer_id: &str,
    bootstrap_address: &str,
    kademlia_protocol: &str,
) -> Result<AndroidProfile, String> {
    create_profile_with_bootstrap_and_e2e_paths(
        network_name,
        bootstrap_peer_id,
        bootstrap_address,
        kademlia_protocol,
        None,
        None,
        None,
    )
}

fn create_profile_with_bootstrap_and_e2e_paths(
    network_name: &str,
    bootstrap_peer_id: &str,
    bootstrap_address: &str,
    kademlia_protocol: &str,
    packet_quic_listen: Option<&str>,
    packet_quic_external_endpoint: Option<&str>,
    relay_reservation: Option<&str>,
) -> Result<AndroidProfile, String> {
    create_profile_with_bootstrap_and_e2e_paths_and_route(
        network_name,
        bootstrap_peer_id,
        bootstrap_address,
        kademlia_protocol,
        packet_quic_listen,
        packet_quic_external_endpoint,
        relay_reservation,
        None,
    )
}

fn create_profile_with_bootstrap_and_e2e_paths_and_route(
    network_name: &str,
    bootstrap_peer_id: &str,
    bootstrap_address: &str,
    kademlia_protocol: &str,
    packet_quic_listen: Option<&str>,
    packet_quic_external_endpoint: Option<&str>,
    relay_reservation: Option<&str>,
    additional_route: Option<&str>,
) -> Result<AndroidProfile, String> {
    let network_name = validate_network_name(network_name)?;
    let bootstrap_peer_id = validate_bootstrap_value("bootstrap peer ID", bootstrap_peer_id, 256)?;
    let bootstrap_address =
        validate_bootstrap_value("bootstrap address", bootstrap_address, 1_024)?;
    let kademlia_protocol = validate_bootstrap_value("Kademlia protocol", kademlia_protocol, 128)?;
    let packet_quic = validate_e2e_packet_quic(packet_quic_listen, packet_quic_external_endpoint)?;
    let relay_reservation = relay_reservation
        .map(|value| {
            validate_bootstrap_value("relay reservation", value, E2E_RELAY_RESERVATION_MAX_LENGTH)
                .map(str::to_owned)
        })
        .transpose()?;
    let additional_route = additional_route
        .map(|value| {
            validate_bootstrap_value("additional route", value, E2E_ADDITIONAL_ROUTE_MAX_LENGTH)
                .map(str::to_owned)
        })
        .transpose()?;
    if packet_quic.is_some() && relay_reservation.is_some() {
        return Err("owned QUIC and relay-only paths cannot be combined".to_owned());
    }
    let identity = NodeIdentity::generate_ed25519()
        .map_err(|error| format!("failed to generate identity: {error:?}"))?;
    let discovery = DiscoveryConfig {
        mdns: false,
        kademlia: true,
        kademlia_provider_advertisement: true,
        kademlia_protocol: kademlia_protocol.to_owned(),
        dcutr: true,
        autonat: true,
    };
    create_profile_with_identity(
        network_name,
        identity,
        Some((
            BootstrapPeerConfig {
                id: bootstrap_peer_id.to_owned(),
                address: bootstrap_address.to_owned(),
            },
            discovery,
        )),
        packet_quic,
        relay_reservation,
        additional_route,
    )
}

fn create_profile_with_identity(
    network_name: &str,
    identity: NodeIdentity,
    bootstrap: Option<(BootstrapPeerConfig, DiscoveryConfig)>,
    packet_quic: Option<(String, String)>,
    relay_reservation: Option<String>,
    additional_route: Option<String>,
) -> Result<AndroidProfile, String> {
    let mut config: Config = serde_json::from_value(serde_json::json!({
        "network": {
            "name": network_name,
            "private_key": identity.private_key,
            "listen_addresses": [
                "/ip4/0.0.0.0/tcp/0",
                "/ip4/0.0.0.0/udp/0/quic-v1"
            ]
        }
    }))
    .map_err(|error| format!("failed to create profile: {error}"))?;
    if let Some((bootstrap_peer, discovery)) = bootstrap {
        config.network.bootstrap_peers = vec![bootstrap_peer];
        config.network.discovery = discovery;
    }
    if let Some((listen, external_endpoint)) = packet_quic {
        config.network.packet_plane.listen.clear();
        config.network.packet_plane.external_endpoints.clear();
        config.network.packet_plane.quic_listen = vec![listen];
        config.network.packet_plane.quic_external_endpoints = vec![external_endpoint];
    }
    if let Some(relay_reservation) = relay_reservation {
        config.network.listen_addresses.clear();
        config.network.relay.reservations = vec![relay_reservation];
        config.network.discovery.dcutr = false;
        config.network.discovery.autonat = false;
    }
    if let Some(prefix) = additional_route {
        config
            .network
            .routes
            .push(RouteConfig { prefix, metric: 0 });
    }
    config
        .validate_runtime()
        .map_err(|_| "invalid isolated E2E path configuration".to_owned())?;
    let config_json = serde_json::to_string(&config)
        .map_err(|error| format!("failed to encode profile: {error}"))?;

    inspect_profile(&config_json)
}

pub fn inspect_profile(config_json: &str) -> Result<AndroidProfile, String> {
    let mut config: Config = serde_json::from_str(config_json)
        .map_err(|error| format!("invalid profile JSON: {error}"))?;
    config
        .validate_runtime()
        .map_err(|error| format!("invalid profile: {error:?}"))?;
    let identity = config
        .identity()
        .map_err(|error| format!("invalid profile identity: {error:?}"))?;
    let hostname = match config.network.dns.hostname.as_deref() {
        Some(hostname) => canonical_dns_label(hostname)
            .map_err(|_| "Android profile contains an invalid hostname".to_owned())?,
        None => default_android_hostname(&identity.peer_id)?,
    };
    config.network.dns.hostname = Some(hostname.clone());
    let tun = TunRuntimeConfig::from_config(&config)
        .map_err(|error| format!("invalid profile routes: {error:?}"))?;

    let mut addresses = vec![
        AndroidCidr::new(IpAddr::V4(tun.addresses.ipv4), 32),
        AndroidCidr::new(IpAddr::V6(tun.addresses.ipv6), 128),
    ];
    addresses.extend(
        tun.additional_addresses
            .iter()
            .copied()
            .map(AndroidCidr::from),
    );

    let mut routes = vec![
        AndroidCidr::new(IpAddr::V4(BUILTIN_IPV4_NETWORK), BUILTIN_IPV4_PREFIX),
        AndroidCidr::new(IpAddr::V6(BUILTIN_IPV6_NETWORK), BUILTIN_IPV6_PREFIX),
    ];
    for route in tun.routes {
        push_unique(&mut routes, AndroidCidr::from(route.prefix));
    }

    Ok(AndroidProfile {
        config_json: serde_json::to_string(&config)
            .map_err(|error| format!("failed to normalize profile: {error}"))?,
        network_name: config.network.name,
        hostname,
        peer_id: identity.peer_id,
        interface_name: tun.name,
        mtu: tun.mtu,
        addresses,
        routes,
    })
}

fn default_android_hostname(peer_id: &str) -> Result<String, String> {
    let overlay_peer = peer_id
        .parse::<PeerId>()
        .map_err(|_| "Android profile contains an invalid peer ID".to_owned())?;
    let digest = overlay_peer.to_string();
    Ok(format!("android-{}", &digest[..16]))
}

pub fn apply_pairing_artifacts(
    config_json: &str,
    artifacts_json: &str,
) -> Result<AndroidProfile, String> {
    apply_pairing_artifacts_inner(config_json, artifacts_json, None)
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn apply_pairing_artifacts_with_managed_secrets(
    config_json: &str,
    artifacts_json: &str,
    runtime_state_directory: &Path,
) -> Result<AndroidProfile, String> {
    apply_pairing_artifacts_inner(config_json, artifacts_json, Some(runtime_state_directory))
}

fn apply_pairing_artifacts_inner(
    config_json: &str,
    artifacts_json: &str,
    runtime_state_directory: Option<&Path>,
) -> Result<AndroidProfile, String> {
    let mut config: Config = serde_json::from_str(config_json)
        .map_err(|error| format!("invalid profile JSON: {error}"))?;
    let artifacts: PairRpcCompletionArtifacts = serde_json::from_str(artifacts_json)
        .map_err(|error| format!("invalid pairing artifacts: {error}"))?;
    let identity = config
        .identity()
        .map_err(|error| format!("invalid profile identity: {error:?}"))?;
    let plan = artifacts.nix;

    if plan.network_name != config.network.name
        || artifacts.receipt.network_name != config.network.name
    {
        return Err("pairing artifacts belong to a different network".to_owned());
    }
    if plan.local_peer != identity.peer_id || artifacts.receipt.local_peer != identity.peer_id {
        return Err("pairing artifacts belong to a different local identity".to_owned());
    }
    if plan.peer.id != artifacts.receipt.remote_peer {
        return Err("pairing artifacts contain a mismatched remote identity".to_owned());
    }

    if let Some(vpn_ip) = plan.assigned_vpn_ip {
        config.network.vpn_ip = Some(vpn_ip);
    }
    for route in plan.additional_local_routes {
        push_route(
            &mut config.network.routes,
            RouteConfig {
                prefix: route.prefix,
                metric: route.metric,
            },
        );
    }

    let peer = PeerConfig {
        id: plan.peer.id,
        name: plan.peer.name,
        ip: None,
        vpn_ip: plan.peer.vpn_ip,
        addresses: Vec::new(),
        routes: plan
            .peer
            .routes
            .into_iter()
            .map(|route| RouteConfig {
                prefix: route.prefix,
                metric: route.metric,
            })
            .collect(),
    };
    upsert_peer(&mut config.peers, peer);

    let member_records: Vec<SignedMembershipRecord> = serde_json::from_value(
        serde_json::to_value(plan.member_records)
            .map_err(|error| format!("failed to encode membership records: {error}"))?,
    )
    .map_err(|error| format!("invalid membership records: {error}"))?;
    config.network.member_records = member_records;

    if let Some(path) = plan.membership_key_file {
        let membership_key = if let Some(runtime_state_directory) = runtime_state_directory {
            consume_managed_membership_key(&path, runtime_state_directory)?
        } else {
            fs::read_to_string(path)
                .map_err(|error| format!("failed to read paired membership key: {error}"))?
        };
        let membership_key = membership_key.trim();
        if membership_key.is_empty() {
            return Err("paired membership key is empty".to_owned());
        }
        config.network.membership_key = Some(membership_key.to_owned());
    }

    let updated = serde_json::to_string(&config)
        .map_err(|error| format!("failed to encode paired profile: {error}"))?;
    inspect_profile(&updated)
}

#[cfg(any(target_os = "android", test))]
fn profile_from_bootstrap_enrollment(
    identity: NodeIdentity,
    enrollment: &PairingBootstrapEnrollment,
    now_unix_seconds: u64,
) -> Result<AndroidProfile, String> {
    let defaults: Config = serde_json::from_value(serde_json::json!({
        "network": {
            "name": enrollment.response.payload.network_name,
            "private_key": identity.private_key.clone(),
        }
    }))
    .map_err(|error| format!("failed to prepare paired profile defaults: {error}"))?;
    let mut config = import_pairing_response_config_at(
        &enrollment.offer,
        &enrollment.response,
        PairingConfigOptions {
            identity: identity.clone(),
            interface_name: defaults.interface.name,
            mtu: defaults.interface.mtu,
            local_routes: Vec::new(),
            peer_name: None,
        },
        now_unix_seconds,
    )
    .map_err(|error| format!("invalid signed pairing enrollment: {error:?}"))?;
    config.network.dns.hostname = Some(
        signed_joiner_hostname(&enrollment.response, &identity.peer_id)?
            .ok_or_else(|| "pairing response did not assign a signed hostname".to_owned())?,
    );
    let encoded = serde_json::to_string(&config)
        .map_err(|error| format!("failed to encode paired profile: {error}"))?;
    inspect_profile(&encoded)
}

#[cfg(any(target_os = "android", test))]
fn signed_joiner_hostname(
    response: &p2p_vpn::pairing::PairingResponse,
    local_peer: &str,
) -> Result<Option<String>, String> {
    let record = response
        .payload
        .member_records
        .iter()
        .filter(|record| {
            record.payload.issuer_peer == response.payload.inviter_peer
                && record.payload.member_peer == local_peer
                && !record.payload.revoked
        })
        .max_by_key(|record| (record.payload.membership_epoch, record.payload.sequence));
    record
        .and_then(|record| record.payload.hostname.as_deref())
        .map(canonical_dns_label)
        .transpose()
        .map_err(|_| "pairing response assigned an invalid hostname".to_owned())
}

fn consume_managed_membership_key(
    path: &str,
    runtime_state_directory: &Path,
) -> Result<String, String> {
    let runtime_state_directory = fs::canonicalize(runtime_state_directory)
        .map_err(|error| format!("failed to resolve Android runtime state directory: {error}"))?;
    let path = Path::new(path);
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect paired membership key: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("paired membership key is not a regular managed file".to_owned());
    }
    let canonical_path = fs::canonicalize(path)
        .map_err(|error| format!("failed to resolve paired membership key: {error}"))?;
    if canonical_path.parent() != Some(runtime_state_directory.as_path())
        || canonical_path.file_name().and_then(|name| name.to_str()) != Some("membership.key")
    {
        return Err("paired membership key is outside Android runtime state storage".to_owned());
    }

    let mut key_file = fs::File::open(&canonical_path)
        .map_err(|error| format!("failed to open paired membership key: {error}"))?;
    fs::remove_file(&canonical_path)
        .map_err(|error| format!("failed to remove transient paired membership key: {error}"))?;
    let mut membership_key = String::new();
    key_file
        .read_to_string(&mut membership_key)
        .map_err(|error| format!("failed to read paired membership key: {error}"))?;
    Ok(membership_key)
}

impl AndroidCidr {
    fn new(address: IpAddr, prefix_length: u8) -> Self {
        Self {
            address: address.to_string(),
            prefix_length,
        }
    }
}

impl From<IpCidr> for AndroidCidr {
    fn from(prefix: IpCidr) -> Self {
        Self::new(prefix.address(), prefix.prefix_len())
    }
}

fn validate_network_name(network_name: &str) -> Result<&str, String> {
    let network_name = network_name.trim();
    if network_name.is_empty() || network_name.len() > 128 {
        return Err("network name must contain between 1 and 128 bytes".to_owned());
    }
    if network_name.chars().any(char::is_control) {
        return Err("network name cannot contain control characters".to_owned());
    }
    Ok(network_name)
}

fn validate_bootstrap_value<'a>(
    label: &str,
    value: &'a str,
    maximum_length: usize,
) -> Result<&'a str, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > maximum_length || value.chars().any(char::is_control) {
        return Err(format!("{label} is invalid"));
    }
    Ok(value)
}

fn validate_e2e_packet_quic(
    listen: Option<&str>,
    external_endpoint: Option<&str>,
) -> Result<Option<(String, String)>, String> {
    match (listen, external_endpoint) {
        (None, None) => Ok(None),
        (Some(listen), Some(external_endpoint)) => {
            let listen = validate_bootstrap_value(
                "owned QUIC listen endpoint",
                listen,
                E2E_PACKET_QUIC_ENDPOINT_MAX_LENGTH,
            )?;
            let external_endpoint = validate_bootstrap_value(
                "owned QUIC external endpoint",
                external_endpoint,
                E2E_PACKET_QUIC_ENDPOINT_MAX_LENGTH,
            )?;
            Ok(Some((listen.to_owned(), external_endpoint.to_owned())))
        }
        _ => Err("owned QUIC profile requires both listen and external endpoints".to_owned()),
    }
}

fn push_unique(prefixes: &mut Vec<AndroidCidr>, prefix: AndroidCidr) {
    if !prefixes.contains(&prefix) {
        prefixes.push(prefix);
    }
}

fn push_route(routes: &mut Vec<RouteConfig>, route: RouteConfig) {
    if !routes
        .iter()
        .any(|existing| existing.prefix == route.prefix && existing.metric == route.metric)
    {
        routes.push(route);
    }
}

fn upsert_peer(peers: &mut Vec<PeerConfig>, peer: PeerConfig) {
    if let Some(existing) = peers.iter_mut().find(|existing| existing.id == peer.id) {
        *existing = peer;
    } else {
        peers.push(peer);
    }
}

#[cfg(target_os = "android")]
mod android {
    use std::{
        collections::BTreeMap,
        fs::File,
        io,
        io::{Read as _, Write as _},
        os::fd::{AsRawFd as _, FromRawFd as _},
        panic::{AssertUnwindSafe, catch_unwind, set_hook},
        ptr,
        sync::{
            Arc, Mutex, Once,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use jni::{
        JNIEnv,
        objects::{JClass, JString},
        sys::{jint, jstring},
    };
    use p2p_vpn::pairing_code::PairingCode;
    use p2p_vpn::runtime::{
        control_socket::{
            MembershipMutationResult, PairRpcRequest, PairRpcResponseEnvelope,
            RuntimeControlHandle, RuntimeNetworkChange, runtime_control_channel,
        },
        pairing_bootstrap::{PairingBootstrapOptions, join_by_code_v2},
        runner::{RuntimePlatform, ShutdownReason, run_config_until_with_runtime_platform},
        tun::{PacketRead, PacketWrite, TunRuntimeConfig},
    };
    use tokio::sync::{oneshot, watch};

    use super::{
        recovery::{HEALTHY_RESET_AFTER, RecoveryBackoff, wait_or_shutdown},
        supervisor::{NetworkLease, NetworkPort, NetworkSpec, PacketSwitch, QueueLimits},
        *,
    };

    static LOG_INIT: Once = Once::new();
    static RUNTIME: Mutex<Option<RuntimeInstance>> = Mutex::new(None);
    static PROFILE_JOIN: Mutex<Option<ProfileJoinOperation>> = Mutex::new(None);
    static PENDING_PROFILE_JOIN_CANCELLATION: Mutex<Option<String>> = Mutex::new(None);
    const CONTROL_FAILURE_LIMIT: u8 = 3;
    const TUN_WRITE_POLL_TIMEOUT: Duration = Duration::from_millis(250);
    const RUNTIME_HEALTH_INITIAL_DELAY: Duration = Duration::from_millis(500);
    const RUNTIME_HEALTH_INTERVAL: Duration = Duration::from_secs(5);
    const PEER_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(1);
    const RUNTIME_SHUTDOWN_GRACE: Duration = Duration::from_secs(4);
    const RUNTIME_ABORT_GRACE: Duration = Duration::from_secs(1);

    struct ProfileJoinOperation {
        id: String,
        cancelled: Arc<AtomicBool>,
    }

    struct ProfileJoinLease {
        id: String,
        cancelled: Arc<AtomicBool>,
    }

    impl ProfileJoinLease {
        fn acquire(id: String) -> Result<Self, String> {
            validate_profile_join_operation_id(&id)?;
            let mut operation = PROFILE_JOIN
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if operation.is_some() {
                return Err("another profile-free pairing operation is active".to_owned());
            }
            let cancellation_requested = PENDING_PROFILE_JOIN_CANCELLATION
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .is_some_and(|pending| pending == id);
            let cancelled = Arc::new(AtomicBool::new(cancellation_requested));
            *operation = Some(ProfileJoinOperation {
                id: id.clone(),
                cancelled: Arc::clone(&cancelled),
            });
            Ok(Self { id, cancelled })
        }
    }

    impl Drop for ProfileJoinLease {
        fn drop(&mut self) {
            let mut operation = PROFILE_JOIN
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if operation
                .as_ref()
                .is_some_and(|operation| operation.id == self.id)
            {
                *operation = None;
            }
        }
    }

    fn validate_profile_join_operation_id(id: &str) -> Result<(), String> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err("pairing operation ID is invalid".to_owned());
        }
        Ok(())
    }

    struct RuntimeInstance {
        networks: BTreeMap<String, Arc<RuntimeNetworkSlot>>,
        tun_stop: Arc<AtomicBool>,
        tun_error: Arc<Mutex<Option<String>>>,
        packet_switch: Arc<PacketSwitch>,
        supervisor_shutdown: watch::Sender<bool>,
        thread: Option<thread::JoinHandle<()>>,
        tun_threads: Vec<thread::JoinHandle<()>>,
    }

    struct RuntimeNetworkSlot {
        state: Mutex<RuntimeNetworkSlotState>,
    }

    struct RuntimeNetworkSlotState {
        generation: u64,
        consecutive_failures: u32,
        restarts: u64,
        control: Option<RuntimeControlHandle>,
        status: AndroidNetworkRuntimeStatus,
    }

    struct RuntimeLaunch {
        id: String,
        config: Config,
        initial_activation: Option<(NetworkPort, NetworkLease)>,
        pairing_state_path: PathBuf,
        membership_state_path: PathBuf,
        slot: Arc<RuntimeNetworkSlot>,
    }

    impl RuntimeNetworkSlot {
        fn new(id: String) -> Self {
            Self {
                state: Mutex::new(RuntimeNetworkSlotState {
                    generation: 0,
                    consecutive_failures: 0,
                    restarts: 0,
                    control: None,
                    status: AndroidNetworkRuntimeStatus {
                        id,
                        phase: AndroidRuntimePhase::Starting,
                        detail: None,
                        lines: Vec::new(),
                        peer_snapshot: None,
                    },
                }),
            }
        }

        fn publish_attempt(
            &self,
            generation: u64,
            consecutive_failures: u32,
            control: RuntimeControlHandle,
        ) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.generation != 0 && state.generation != generation {
                state.restarts = state.restarts.saturating_add(1);
            }
            state.generation = generation;
            state.consecutive_failures = consecutive_failures;
            state.control = Some(control);
            state.status.phase = AndroidRuntimePhase::Starting;
            state.status.detail = Some(format!("Starting runtime generation {generation}"));
            state.status.lines.clear();
            state.status.peer_snapshot = None;
        }

        fn record_running(
            &self,
            generation: u64,
            lines: Vec<String>,
            peer_snapshot: Option<NetworkPeerSnapshot>,
        ) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.generation != generation || state.control.is_none() {
                return;
            }
            state.status.phase = AndroidRuntimePhase::Running;
            state.status.detail = None;
            state.status.lines = lines;
            state.status.peer_snapshot = peer_snapshot;
        }

        fn record_health_failure(&self, generation: u64, failures: u8, error: &str) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.generation != generation || state.control.is_none() {
                return;
            }
            state.status.detail = Some(format!(
                "Runtime health check {failures} of {CONTROL_FAILURE_LIMIT} failed: {error}"
            ));
        }

        fn record_recovery(
            &self,
            generation: u64,
            consecutive_failures: u32,
            delay: Duration,
            error: String,
        ) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.generation != generation {
                return;
            }
            state.consecutive_failures = consecutive_failures;
            state.control = None;
            state.status.phase = AndroidRuntimePhase::Starting;
            state.status.detail = Some(format!(
                "Recovering after {error}; attempt {consecutive_failures} in {} ms",
                delay.as_millis()
            ));
            state.status.lines.clear();
            state.status.peer_snapshot = None;
        }

        fn record_activation_failure(
            &self,
            consecutive_failures: u32,
            delay: Duration,
            error: String,
        ) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.consecutive_failures = consecutive_failures;
            state.control = None;
            state.status.phase = AndroidRuntimePhase::Starting;
            state.status.detail = Some(format!(
                "Recovering after {error}; attempt {consecutive_failures} in {} ms",
                delay.as_millis()
            ));
            state.status.lines.clear();
            state.status.peer_snapshot = None;
        }

        fn record_stopped(&self, generation: Option<u64>) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if generation.is_some_and(|generation| state.generation != generation) {
                return;
            }
            state.control = None;
            state.status.phase = AndroidRuntimePhase::Stopped;
            state.status.detail = None;
            state.status.lines.clear();
            state.status.peer_snapshot = None;
        }

        fn record_supervisor_failure(&self, detail: String) {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.control = None;
            state.status.phase = AndroidRuntimePhase::Failed;
            state.status.detail = Some(detail);
            state.status.lines.clear();
            state.status.peer_snapshot = None;
        }

        fn snapshot(&self) -> AndroidNetworkRuntimeStatus {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let mut status = state.status.clone();
            status
                .lines
                .retain(|line| !line.starts_with("android_runtime_supervisor "));
            status.lines.push(format!(
                "android_runtime_supervisor id={} generation={} consecutive_failures={} restarts={} control_available={}",
                status.id,
                state.generation,
                state.consecutive_failures,
                state.restarts,
                state.control.is_some(),
            ));
            status
        }

        fn control(&self) -> Option<RuntimeControlHandle> {
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .control
                .clone()
        }
    }

    #[derive(Serialize)]
    #[serde(untagged)]
    enum AndroidRuntimeNetworkChange {
        Single(RuntimeNetworkChange),
        Multiple {
            networks: Vec<AndroidRuntimeNetworkChangeResult>,
        },
    }

    #[derive(Serialize)]
    struct AndroidRuntimeNetworkChangeResult {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        change: Option<RuntimeNetworkChange>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }

    struct AndroidTunReader {
        file: File,
        stop: Arc<AtomicBool>,
    }

    struct AndroidTunWriter {
        file: File,
        stop: Arc<AtomicBool>,
    }

    impl PacketRead for AndroidTunReader {
        fn read_packet(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            loop {
                if self.stop.load(Ordering::Acquire) {
                    return Ok(0);
                }
                let mut descriptor = libc::pollfd {
                    fd: self.file.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // `descriptor` is valid for one element for the duration of this call.
                let result = unsafe { libc::poll(&mut descriptor, 1, 250) };
                if result < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if result == 0 {
                    continue;
                }
                if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    return Err(io::Error::other(
                        "Android TUN descriptor became unavailable",
                    ));
                }
                let length = match self.file.read(buffer) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                    result => result?,
                };
                if length == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Android TUN descriptor closed",
                    ));
                }
                return Ok(length);
            }
        }
    }

    impl PacketWrite for AndroidTunWriter {
        fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
            let started = Instant::now();
            loop {
                if self.stop.load(Ordering::Acquire) {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "shared Android TUN writer stopped",
                    ));
                }
                let Some(remaining) = TUN_WRITE_POLL_TIMEOUT.checked_sub(started.elapsed()) else {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "shared Android TUN writer remained blocked",
                    ));
                };
                if remaining.is_zero() {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "shared Android TUN writer remained blocked",
                    ));
                }
                let timeout_millis =
                    i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
                let mut descriptor = libc::pollfd {
                    fd: self.file.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // `descriptor` is valid for one element for the duration of this call.
                let result = unsafe { libc::poll(&mut descriptor, 1, timeout_millis) };
                if result < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if result == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "shared Android TUN writer remained blocked",
                    ));
                }
                if descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    return Err(io::Error::other(
                        "Android TUN descriptor became unavailable",
                    ));
                }
                match self.file.write(packet) {
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                        ) => {}
                    result => return result,
                }
            }
        }
    }

    fn set_nonblocking(file: &File) -> Result<(), String> {
        // The descriptor is owned and valid for both `fcntl` calls.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            return Err(format!(
                "failed to read Android TUN descriptor flags: {}",
                io::Error::last_os_error()
            ));
        }
        // `O_NONBLOCK` is shared by the duplicated descriptor's open file description.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(format!(
                "failed to make Android TUN descriptor nonblocking: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeCreateProfile(
        mut env: JNIEnv,
        _class: JClass,
        network_name: JString,
        hostname: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let network_name = read_string(env, &network_name)?;
            let hostname = read_string(env, &hostname)?;
            create_profile_with_hostname(&network_name, &hostname)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeCreateE2eProfile(
        mut env: JNIEnv,
        _class: JClass,
        network_name: JString,
        bootstrap_peer_id: JString,
        bootstrap_address: JString,
        kademlia_protocol: JString,
        packet_quic_listen: JString,
        packet_quic_external_endpoint: JString,
        relay_reservation: JString,
        additional_route: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let network_name = read_string(env, &network_name)?;
            let bootstrap_peer_id = read_string(env, &bootstrap_peer_id)?;
            let bootstrap_address = read_string(env, &bootstrap_address)?;
            let kademlia_protocol = read_string(env, &kademlia_protocol)?;
            let packet_quic_listen = read_optional_string(env, &packet_quic_listen)?;
            let packet_quic_external_endpoint =
                read_optional_string(env, &packet_quic_external_endpoint)?;
            let relay_reservation = read_optional_string(env, &relay_reservation)?;
            let additional_route = read_optional_string(env, &additional_route)?;
            create_profile_with_bootstrap_and_e2e_paths_and_route(
                &network_name,
                &bootstrap_peer_id,
                &bootstrap_address,
                &kademlia_protocol,
                packet_quic_listen.as_deref(),
                packet_quic_external_endpoint.as_deref(),
                relay_reservation.as_deref(),
                additional_route.as_deref(),
            )
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeInspectProfile(
        mut env: JNIEnv,
        _class: JClass,
        config_json: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let config_json = read_string(env, &config_json)?;
            inspect_profile(&config_json)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeRenameProfile(
        mut env: JNIEnv,
        _class: JClass,
        config_json: JString,
        hostname: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let config_json = read_string(env, &config_json)?;
            let hostname = read_string(env, &hostname)?;
            rename_profile(&config_json, &hostname)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeStart(
        mut env: JNIEnv,
        _class: JClass,
        network_id: JString,
        config_json: JString,
        tun_fd: jint,
        pairing_state_path: JString,
        membership_state_path: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            if tun_fd < 0 {
                return Err("Android supplied an invalid TUN descriptor".to_owned());
            }
            // `VpnService` detached the descriptor, so JNI must adopt it before any fallible work.
            let tun_file = unsafe { File::from_raw_fd(tun_fd) };
            let network_id = read_string(env, &network_id)?;
            let config_json = read_string(env, &config_json)?;
            let pairing_state_path = read_string(env, &pairing_state_path)?;
            let membership_state_path = read_string(env, &membership_state_path)?;
            let request_json = single_runtime_start_request(
                &network_id,
                &config_json,
                &pairing_state_path,
                &membership_state_path,
            )?;
            start_runtime(&request_json, tun_file)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeStartNetworks(
        mut env: JNIEnv,
        _class: JClass,
        request_json: JString,
        tun_fd: jint,
    ) -> jstring {
        jni_response(&mut env, |env| {
            if tun_fd < 0 {
                return Err("Android supplied an invalid TUN descriptor".to_owned());
            }
            // `VpnService` detached the descriptor, so JNI must adopt it before any fallible work.
            let tun_file = unsafe { File::from_raw_fd(tun_fd) };
            let request_json = read_string(env, &request_json)?;
            start_runtime(&request_json, tun_file)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeValidateStartNetworks(
        mut env: JNIEnv,
        _class: JClass,
        request_json: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let request_json = read_string(env, &request_json)?;
            validate_android_runtime_start(&request_json)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeStop(
        mut env: JNIEnv,
        _class: JClass,
    ) -> jstring {
        jni_response(&mut env, |_| stop_runtime())
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeStatus(
        mut env: JNIEnv,
        _class: JClass,
    ) -> jstring {
        jni_response(&mut env, |_| runtime_status())
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeNetworkChanged(
        mut env: JNIEnv,
        _class: JClass,
    ) -> jstring {
        jni_response(&mut env, |_| network_changed())
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeRevokeMember(
        mut env: JNIEnv,
        _class: JClass,
        network_id: JString,
        member_peer: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let network_id = read_string(env, &network_id)?;
            let member_peer = read_string(env, &member_peer)?;
            membership_mutation(&network_id, Some(member_peer))
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeResignMembership(
        mut env: JNIEnv,
        _class: JClass,
        network_id: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let network_id = read_string(env, &network_id)?;
            membership_mutation(&network_id, None)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativePairRpc(
        mut env: JNIEnv,
        _class: JClass,
        request_json: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let request_json = read_string(env, &request_json)?;
            let request: PairRpcRequest = serde_json::from_str(&request_json)
                .map_err(|error| format!("invalid pairing request: {error}"))?;
            pair_rpc(None, request)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativePairRpcForNetwork(
        mut env: JNIEnv,
        _class: JClass,
        network_id: JString,
        request_json: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let network_id = read_string(env, &network_id)?;
            let request_json = read_string(env, &request_json)?;
            let request: PairRpcRequest = serde_json::from_str(&request_json)
                .map_err(|error| format!("invalid pairing request: {error}"))?;
            pair_rpc(Some(&network_id), request)
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeApplyPairingArtifacts(
        mut env: JNIEnv,
        _class: JClass,
        config_json: JString,
        artifacts_json: JString,
        runtime_state_directory: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let config_json = read_string(env, &config_json)?;
            let artifacts_json = read_string(env, &artifacts_json)?;
            let runtime_state_directory = read_string(env, &runtime_state_directory)?;
            apply_pairing_artifacts_with_managed_secrets(
                &config_json,
                &artifacts_json,
                Path::new(&runtime_state_directory),
            )
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeJoinProfileByCode(
        mut env: JNIEnv,
        _class: JClass,
        operation_id: JString,
        pairing_code: JString,
        hostname: JString,
        existing_network_names_json: JString,
        candidate_hints_json: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let operation_id = read_string(env, &operation_id)?;
            let pairing_code = read_string(env, &pairing_code)?;
            let hostname = read_string(env, &hostname)?;
            let existing_network_names_json = read_string(env, &existing_network_names_json)?;
            let candidate_hints_json = read_string(env, &candidate_hints_json)?;
            join_profile_by_code(
                &operation_id,
                &pairing_code,
                &hostname,
                &existing_network_names_json,
                &candidate_hints_json,
            )
        })
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativeCancelProfileJoin(
        mut env: JNIEnv,
        _class: JClass,
        operation_id: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let operation_id = read_string(env, &operation_id)?;
            validate_profile_join_operation_id(&operation_id)?;
            let operation = PROFILE_JOIN
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let cancelled = if let Some(operation) = operation
                .as_ref()
                .filter(|operation| operation.id == operation_id)
            {
                operation.cancelled.store(true, Ordering::Release);
                true
            } else if operation.is_none() {
                *PENDING_PROFILE_JOIN_CANCELLATION
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(operation_id);
                true
            } else {
                false
            };
            Ok(serde_json::json!({ "cancelled": cancelled }))
        })
    }

    fn join_profile_by_code(
        operation_id: &str,
        pairing_code: &str,
        hostname: &str,
        existing_network_names_json: &str,
        candidate_hints_json: &str,
    ) -> Result<AndroidProfile, String> {
        let lease = ProfileJoinLease::acquire(operation_id.to_owned())?;
        if lease.cancelled.load(Ordering::Acquire) {
            return Err("pairing discovery was cancelled".to_owned());
        }
        let code = pairing_code
            .parse::<PairingCode>()
            .map_err(|_| "pairing code is invalid".to_owned())?;
        let hostname = canonical_dns_label(hostname)
            .map_err(|_| "pairing hostname is not a valid DNS label".to_owned())?;
        let existing_network_names: Vec<String> = serde_json::from_str(existing_network_names_json)
            .map_err(|_| "existing network list is invalid".to_owned())?;
        if existing_network_names.len() > 16 {
            return Err("existing network list is invalid".to_owned());
        }
        let candidate_hints = serde_json::from_str(candidate_hints_json)
            .map_err(|_| "pairing discovery candidate hints are invalid".to_owned())?;
        let identity = NodeIdentity::generate_ed25519()
            .map_err(|error| format!("failed to generate pairing identity: {error:?}"))?;
        let options = PairingBootstrapOptions {
            existing_network_names,
            requested_hostname: Some(hostname),
            candidate_hints,
            ..PairingBootstrapOptions::default()
        };
        let runtime = control_runtime()?;
        let cancellation = Arc::clone(&lease.cancelled);
        let enrollment = runtime.block_on(async move {
            tokio::select! {
                result = join_by_code_v2(identity.clone(), code, options) => {
                    result.map(|enrollment| (identity, enrollment))
                        .map_err(|error| error.to_string())
                }
                () = wait_for_profile_join_cancellation(cancellation) => {
                    Err("pairing discovery was cancelled".to_owned())
                }
            }
        })?;
        let now_unix_seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "system clock is before the Unix epoch".to_owned())?
            .as_secs();
        profile_from_bootstrap_enrollment(enrollment.0, &enrollment.1, now_unix_seconds)
    }

    async fn wait_for_profile_join_cancellation(cancelled: Arc<AtomicBool>) {
        while !cancelled.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn single_runtime_start_request(
        network_id: &str,
        config_json: &str,
        pairing_state_path: &str,
        membership_state_path: &str,
    ) -> Result<String, String> {
        let config: Config = serde_json::from_str(config_json)
            .map_err(|error| format!("invalid profile JSON: {error}"))?;
        config
            .validate_runtime()
            .map_err(|error| format!("invalid profile: {error:?}"))?;
        let tun = TunRuntimeConfig::from_config(&config)
            .map_err(|error| format!("invalid profile routes: {error:?}"))?;
        serde_json::to_string(&serde_json::json!({
            "schema_version": ANDROID_RUNTIME_START_SCHEMA_VERSION,
            "presentation_addresses": {
                "ipv4": tun.addresses.ipv4,
                "ipv6": tun.addresses.ipv6,
            },
            "networks": [{
                "id": network_id,
                "config_json": config_json,
                "pairing_state_path": pairing_state_path,
                "membership_state_path": membership_state_path,
            }],
        }))
        .map_err(|error| format!("failed to encode Android runtime start request: {error}"))
    }

    fn start_runtime(
        request_json: &str,
        writer_file: File,
    ) -> Result<AndroidRuntimeStatus, String> {
        init_logging();
        let prepared = prepare_android_runtime_start(request_json)?;
        let network_specs = prepared
            .networks
            .iter()
            .map(|network| NetworkSpec {
                id: network.id.clone(),
                tun: network.tun.clone(),
            })
            .collect();
        let (packet_switch, ports) = PacketSwitch::new_with_presentation(
            network_specs,
            prepared.presentation,
            QueueLimits::DEFAULT,
        )
        .map_err(|error| error.to_string())?;
        let packet_switch = Arc::new(packet_switch);
        let mut ports = ports
            .into_iter()
            .map(|port| (port.id.clone(), port))
            .collect::<BTreeMap<_, _>>();
        let tun_mtu = prepared.tun_mtu;
        let mut networks = BTreeMap::new();
        let mut launches = Vec::with_capacity(prepared.networks.len());
        for network in prepared.networks {
            let port = ports.remove(&network.id).ok_or_else(|| {
                format!(
                    "Android packet supervisor omitted network port {}",
                    network.id
                )
            })?;
            let lease = packet_switch
                .network_lease(&network.id)
                .map_err(|error| error.to_string())?;
            let slot = Arc::new(RuntimeNetworkSlot::new(network.id.clone()));
            networks.insert(network.id.clone(), Arc::clone(&slot));
            launches.push(RuntimeLaunch {
                id: network.id,
                config: network.config,
                initial_activation: Some((port, lease)),
                pairing_state_path: network.pairing_state_path,
                membership_state_path: network.membership_state_path,
                slot,
            });
        }
        if !ports.is_empty() {
            return Err("Android packet supervisor returned unknown network ports".to_owned());
        }
        let _ = stop_runtime();

        // The reader and writer need independent owners for the same TUN endpoint.
        let reader_fd = unsafe { libc::dup(writer_file.as_raw_fd()) };
        if reader_fd < 0 {
            return Err(format!(
                "failed to duplicate Android TUN descriptor: {}",
                io::Error::last_os_error()
            ));
        }
        let reader_file = unsafe { File::from_raw_fd(reader_fd) };
        set_nonblocking(&writer_file)?;
        let tun_stop = Arc::new(AtomicBool::new(false));
        let tun_error = Arc::new(Mutex::new(None));
        let (supervisor_shutdown, supervisor_shutdown_receiver) = watch::channel(false);

        let reader_switch = Arc::clone(&packet_switch);
        let reader_stop = Arc::clone(&tun_stop);
        let reader_error = Arc::clone(&tun_error);
        let reader_shutdown = supervisor_shutdown.clone();
        let reader_thread = thread::Builder::new()
            .name("p2p-vpn-tun-reader".to_owned())
            .spawn(move || {
                let mut reader = AndroidTunReader {
                    file: reader_file,
                    stop: Arc::clone(&reader_stop),
                };
                let mut buffer = vec![0_u8; usize::from(tun_mtu)];
                loop {
                    match reader.read_packet(&mut buffer) {
                        Ok(0) if reader_stop.load(Ordering::Acquire) => return,
                        Ok(0) => continue,
                        Ok(length) => {
                            let _ = reader_switch.dispatch_packet(&buffer[..length]);
                        }
                        Err(error) => {
                            if !reader_stop.load(Ordering::Acquire) {
                                record_tun_error(&reader_error, "read", &error);
                            }
                            reader_stop.store(true, Ordering::Release);
                            reader_switch.close();
                            let _ = reader_shutdown.send(true);
                            return;
                        }
                    }
                }
            })
            .map_err(|error| format!("failed to start shared TUN reader: {error}"))?;

        let writer_switch = Arc::clone(&packet_switch);
        let writer_stop = Arc::clone(&tun_stop);
        let writer_error = Arc::clone(&tun_error);
        let writer_shutdown = supervisor_shutdown.clone();
        let writer_thread = match thread::Builder::new()
            .name("p2p-vpn-tun-writer".to_owned())
            .spawn(move || {
                let mut writer = AndroidTunWriter {
                    file: writer_file,
                    stop: Arc::clone(&writer_stop),
                };
                while !writer_stop.load(Ordering::Acquire) {
                    let generation = writer_switch.inbound_generation();
                    match writer_switch.write_next(&mut writer) {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            let _ = writer_switch
                                .wait_for_inbound_since(generation, Duration::from_millis(250));
                        }
                        Err(error) => {
                            if !writer_stop.load(Ordering::Acquire) {
                                record_tun_error(&writer_error, "write", &error);
                            }
                            writer_stop.store(true, Ordering::Release);
                            writer_switch.close();
                            let _ = writer_shutdown.send(true);
                            return;
                        }
                    }
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                tun_stop.store(true, Ordering::Release);
                packet_switch.close();
                let _ = reader_thread.join();
                return Err(format!("failed to start shared TUN writer: {error}"));
            }
        };

        let runtime_worker_count = launches.len().clamp(2, 4);
        let runtime_stop = Arc::clone(&tun_stop);
        let runtime_switch = Arc::clone(&packet_switch);
        let thread = match thread::Builder::new()
            .name("p2p-vpn-runtime-supervisor".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(runtime_worker_count)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let error = format!("failed to create shared async runtime: {error}");
                        for launch in launches {
                            launch.slot.record_supervisor_failure(error.clone());
                        }
                        runtime_stop.store(true, Ordering::Release);
                        runtime_switch.close();
                        return;
                    }
                };
                let supervisor_switch = Arc::clone(&runtime_switch);
                runtime.block_on(async move {
                    let mut tasks = tokio::task::JoinSet::new();
                    let mut task_slots = BTreeMap::new();
                    for launch in launches {
                        let slot = Arc::clone(&launch.slot);
                        let task = tasks.spawn(supervise_network(
                            launch,
                            Arc::clone(&supervisor_switch),
                            supervisor_shutdown_receiver.clone(),
                        ));
                        task_slots.insert(task.id(), slot);
                    }
                    while !tasks.is_empty() {
                        if let Some(result) = tasks.join_next_with_id().await {
                            record_runtime_task_result(result, &task_slots);
                        }
                    }
                });
                runtime_stop.store(true, Ordering::Release);
                runtime_switch.close();
                runtime.shutdown_timeout(RUNTIME_ABORT_GRACE);
            }) {
            Ok(thread) => thread,
            Err(error) => {
                tun_stop.store(true, Ordering::Release);
                packet_switch.close();
                let _ = reader_thread.join();
                let _ = writer_thread.join();
                return Err(format!("failed to start runtime thread: {error}"));
            }
        };

        let initial_status = aggregate_runtime_status(&networks, None, &packet_switch);
        let instance = RuntimeInstance {
            networks,
            tun_stop,
            tun_error,
            packet_switch,
            supervisor_shutdown,
            thread: Some(thread),
            tun_threads: vec![reader_thread, writer_thread],
        };
        *RUNTIME.lock().unwrap_or_else(|error| error.into_inner()) = Some(instance);
        Ok(initial_status)
    }

    async fn supervise_network(
        mut launch: RuntimeLaunch,
        packet_switch: Arc<PacketSwitch>,
        mut supervisor_shutdown: watch::Receiver<bool>,
    ) {
        let mut backoff = RecoveryBackoff::default();
        loop {
            if *supervisor_shutdown.borrow() {
                launch.slot.record_stopped(None);
                return;
            }
            let activation = match launch.initial_activation.take() {
                Some(activation) => Ok(activation),
                None => packet_switch.activate_network(&launch.id),
            };
            let (port, lease) = match activation {
                Ok(activation) => activation,
                Err(error) => {
                    let delay = backoff.record_failure(&launch.id, Duration::ZERO);
                    log::warn!(
                        "event=android_network_activation_failed network_id={} attempt={} retry_ms={} error={}",
                        launch.id,
                        backoff.consecutive_failures(),
                        delay.as_millis(),
                        error,
                    );
                    launch.slot.record_activation_failure(
                        backoff.consecutive_failures(),
                        delay,
                        format!("packet supervisor activation failed: {error}"),
                    );
                    if !wait_or_shutdown(delay, &mut supervisor_shutdown).await {
                        launch.slot.record_stopped(None);
                        return;
                    }
                    continue;
                }
            };
            let generation = port.generation;
            let (control, control_receiver) = runtime_control_channel();
            launch.slot.publish_attempt(
                generation,
                backoff.consecutive_failures(),
                control.clone(),
            );
            log::info!(
                "event=android_network_runtime_started network_id={} generation={} prior_failures={}",
                launch.id,
                generation,
                backoff.consecutive_failures(),
            );
            let (shutdown, shutdown_receiver) = oneshot::channel();
            let config = launch.config.clone();
            let pairing_state_path = launch.pairing_state_path.clone();
            let membership_state_path = launch.membership_state_path.clone();
            let mut runtime = tokio::spawn(async move {
                let _network_lease = lease;
                run_config_until_with_runtime_platform(
                    config,
                    RuntimePlatform::new(port.packet_io, port.route_controller)
                        .with_control(control_receiver),
                    None,
                    None,
                    Some(pairing_state_path),
                    Some(membership_state_path),
                    async move { shutdown_receiver.await.unwrap_or(ShutdownReason::Terminate) },
                )
                .await
                .map_err(|error| format!("runtime failed: {error:?}"))
            });
            let mut shutdown = Some(shutdown);
            let mut probe = Box::pin(runtime_health_probe(
                control.clone(),
                RUNTIME_HEALTH_INITIAL_DELAY,
            ));
            let mut health_failures = 0_u8;
            let mut running_since = None;
            let mut reset_backoff = false;

            let exit = loop {
                tokio::select! {
                    result = &mut runtime => break RuntimeAttemptExit::Runtime(result),
                    result = &mut probe => {
                        match result {
                            Ok(probe) => {
                                health_failures = 0;
                                running_since.get_or_insert_with(Instant::now);
                                reset_backoff |= running_since
                                    .is_some_and(|since| since.elapsed() >= HEALTHY_RESET_AFTER);
                                launch.slot.record_running(
                                    generation,
                                    probe.lines,
                                    probe.peer_snapshot,
                                );
                            }
                            Err(error) => {
                                health_failures = health_failures.saturating_add(1);
                                reset_backoff |= running_since
                                    .is_some_and(|since| since.elapsed() >= HEALTHY_RESET_AFTER);
                                running_since = None;
                                log::warn!(
                                    "event=android_network_health_failed network_id={} generation={} failures={} error={}",
                                    launch.id,
                                    generation,
                                    health_failures,
                                    error,
                                );
                                launch.slot.record_health_failure(
                                    generation,
                                    health_failures,
                                    &error,
                                );
                                if health_failures >= CONTROL_FAILURE_LIMIT {
                                    break RuntimeAttemptExit::Unhealthy(error);
                                }
                            }
                        }
                        probe = Box::pin(runtime_health_probe(
                            control.clone(),
                            RUNTIME_HEALTH_INTERVAL,
                        ));
                    }
                    changed = supervisor_shutdown.changed() => {
                        if changed.is_err() || *supervisor_shutdown.borrow() {
                            break RuntimeAttemptExit::Shutdown;
                        }
                    }
                }
            };

            let healthy_for = if reset_backoff {
                HEALTHY_RESET_AFTER
            } else {
                running_since.map_or(Duration::ZERO, |since| since.elapsed())
            };
            let failure = match exit {
                RuntimeAttemptExit::Shutdown => {
                    stop_runtime_attempt(&mut runtime, shutdown.take()).await;
                    launch.slot.record_stopped(Some(generation));
                    log::info!(
                        "event=android_network_runtime_stopped network_id={} generation={}",
                        launch.id,
                        generation,
                    );
                    return;
                }
                RuntimeAttemptExit::Runtime(result) => runtime_exit_error(result),
                RuntimeAttemptExit::Unhealthy(error) => {
                    stop_runtime_attempt(&mut runtime, shutdown.take()).await;
                    format!(
                        "runtime became unhealthy after {CONTROL_FAILURE_LIMIT} control failures: {error}"
                    )
                }
            };
            let delay = backoff.record_failure(&launch.id, healthy_for);
            log::warn!(
                "event=android_network_recovery_scheduled network_id={} generation={} attempt={} retry_ms={} error={}",
                launch.id,
                generation,
                backoff.consecutive_failures(),
                delay.as_millis(),
                failure,
            );
            launch
                .slot
                .record_recovery(generation, backoff.consecutive_failures(), delay, failure);
            if !wait_or_shutdown(delay, &mut supervisor_shutdown).await {
                launch.slot.record_stopped(Some(generation));
                return;
            }
        }
    }

    enum RuntimeAttemptExit {
        Shutdown,
        Runtime(Result<Result<(), String>, tokio::task::JoinError>),
        Unhealthy(String),
    }

    struct RuntimeHealthProbe {
        lines: Vec<String>,
        peer_snapshot: Option<NetworkPeerSnapshot>,
    }

    async fn runtime_health_probe(
        control: RuntimeControlHandle,
        delay: Duration,
    ) -> Result<RuntimeHealthProbe, String> {
        tokio::time::sleep(delay).await;
        let lines = tokio::time::timeout(CONTROL_TIMEOUT, control.status())
            .await
            .map_err(|_| "runtime control request timed out".to_owned())?
            .map_err(|error| format!("runtime control request failed: {error}"))?;
        let peer_snapshot = tokio::time::timeout(PEER_SNAPSHOT_TIMEOUT, control.peer_snapshot())
            .await
            .ok()
            .and_then(Result::ok);
        Ok(RuntimeHealthProbe {
            lines,
            peer_snapshot,
        })
    }

    async fn stop_runtime_attempt(
        runtime: &mut tokio::task::JoinHandle<Result<(), String>>,
        shutdown: Option<oneshot::Sender<ShutdownReason>>,
    ) {
        if let Some(shutdown) = shutdown {
            let _ = shutdown.send(ShutdownReason::Terminate);
        }
        if tokio::time::timeout(RUNTIME_SHUTDOWN_GRACE, &mut *runtime)
            .await
            .is_ok()
        {
            return;
        }
        runtime.abort();
        let _ = tokio::time::timeout(RUNTIME_ABORT_GRACE, &mut *runtime).await;
    }

    fn runtime_exit_error(result: Result<Result<(), String>, tokio::task::JoinError>) -> String {
        match result {
            Ok(Ok(())) => "runtime stopped unexpectedly".to_owned(),
            Ok(Err(error)) => error,
            Err(error) if error.is_panic() => "network runtime task panicked".to_owned(),
            Err(_) => "network runtime task was cancelled".to_owned(),
        }
    }

    fn record_tun_error(target: &Mutex<Option<String>>, operation: &str, error: &io::Error) {
        let mut target = target.lock().unwrap_or_else(|error| error.into_inner());
        if target.is_none() {
            *target = Some(format!("shared Android TUN {operation} failed: {error}"));
        }
    }

    fn record_runtime_task_result(
        result: Result<(tokio::task::Id, ()), tokio::task::JoinError>,
        slots: &BTreeMap<tokio::task::Id, Arc<RuntimeNetworkSlot>>,
    ) {
        let Err(error) = result else {
            return;
        };
        let Some(slot) = slots.get(&error.id()) else {
            return;
        };
        slot.record_supervisor_failure(if error.is_panic() {
            "network runtime task panicked".to_owned()
        } else {
            "network runtime task was cancelled".to_owned()
        });
    }

    fn stop_runtime() -> Result<AndroidRuntimeStatus, String> {
        let Some(mut instance) = RUNTIME
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        else {
            return Ok(stopped_runtime_status());
        };

        let _ = instance.supervisor_shutdown.send(true);
        instance.tun_stop.store(true, Ordering::Release);
        instance.packet_switch.close();
        let mut join_error = None;
        if let Some(thread) = instance.thread.take() {
            if thread.join().is_err() {
                join_error = Some("runtime thread panicked while stopping".to_owned());
            }
        }
        for thread in instance.tun_threads {
            if thread.join().is_err() && join_error.is_none() {
                join_error = Some("shared TUN thread panicked while stopping".to_owned());
            }
        }
        if let Some(error) = join_error {
            return Err(error);
        }
        let tun_failure = instance
            .tun_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        Ok(aggregate_runtime_status(
            &instance.networks,
            tun_failure,
            &instance.packet_switch,
        ))
    }

    fn runtime_status() -> Result<AndroidRuntimeStatus, String> {
        let (snapshots, tun_error, packet_switch) = {
            let runtime = RUNTIME.lock().unwrap_or_else(|error| error.into_inner());
            let Some(instance) = runtime.as_ref() else {
                return Ok(stopped_runtime_status());
            };
            (
                instance
                    .networks
                    .values()
                    .map(|network| network.snapshot())
                    .collect(),
                Arc::clone(&instance.tun_error),
                Arc::clone(&instance.packet_switch),
            )
        };
        let tun_failure = tun_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        Ok(aggregate_runtime_snapshots(
            snapshots,
            tun_failure,
            &packet_switch,
        ))
    }

    fn stopped_runtime_status() -> AndroidRuntimeStatus {
        AndroidRuntimeStatus {
            phase: AndroidRuntimePhase::Stopped,
            detail: None,
            lines: Vec::new(),
            networks: Vec::new(),
        }
    }

    fn aggregate_runtime_status(
        networks: &BTreeMap<String, Arc<RuntimeNetworkSlot>>,
        tun_failure: Option<String>,
        packet_switch: &PacketSwitch,
    ) -> AndroidRuntimeStatus {
        let snapshots = networks
            .values()
            .map(|network| network.snapshot())
            .collect();
        aggregate_runtime_snapshots(snapshots, tun_failure, packet_switch)
    }

    fn aggregate_runtime_snapshots(
        networks: Vec<AndroidNetworkRuntimeStatus>,
        tun_failure: Option<String>,
        packet_switch: &PacketSwitch,
    ) -> AndroidRuntimeStatus {
        let running = networks
            .iter()
            .filter(|network| network.phase == AndroidRuntimePhase::Running)
            .count();
        let starting = networks
            .iter()
            .filter(|network| network.phase == AndroidRuntimePhase::Starting)
            .count();
        let failed = networks
            .iter()
            .filter(|network| network.phase == AndroidRuntimePhase::Failed)
            .count();
        let phase = if tun_failure.is_some() {
            AndroidRuntimePhase::Failed
        } else if running > 0 {
            AndroidRuntimePhase::Running
        } else if starting > 0 {
            AndroidRuntimePhase::Starting
        } else if failed > 0 {
            AndroidRuntimePhase::Failed
        } else {
            AndroidRuntimePhase::Stopped
        };
        let detail = tun_failure.or_else(|| match networks.as_slice() {
            [network] => network.detail.clone(),
            [_, _, ..] => Some({
                format!(
                    "{running} running, {starting} starting, {failed} failed of {} networks",
                    networks.len()
                )
            }),
            [] => None,
        });
        let lines = if networks.len() == 1 {
            networks[0].lines.clone()
        } else {
            networks
                .iter()
                .flat_map(|network| network.lines.iter())
                .filter(|line| line.starts_with("android_runtime_supervisor "))
                .cloned()
                .collect()
        };
        let mut status = AndroidRuntimeStatus {
            phase,
            detail,
            lines,
            networks,
        };
        append_switch_status(&mut status, packet_switch);
        status
    }

    fn append_switch_status(status: &mut AndroidRuntimeStatus, packet_switch: &PacketSwitch) {
        status
            .lines
            .retain(|line| !line.starts_with("android_supervisor_"));
        let snapshot = packet_switch.snapshot();
        status.lines.push(format!(
            "android_supervisor_networks {}",
            snapshot.networks.len()
        ));
        status.lines.push(format!(
            "android_supervisor_outbound malformed={} no_route={} source_mismatch={}",
            snapshot.malformed_outbound_packets,
            snapshot.unroutable_outbound_packets,
            snapshot.source_mismatch_outbound_packets,
        ));
        for (index, network) in snapshot.networks.iter().enumerate() {
            status.lines.push(format!(
                "android_supervisor_network index={index} id={} outbound_enqueued={} outbound_queue_drops={} outbound_oversized_drops={} outbound_source_mismatch_drops={} outbound_translation_drops={} outbound_removed_drops={} inbound_enqueued={} inbound_queue_drops={} inbound_oversized_drops={} inbound_malformed_drops={} inbound_source_mismatch_drops={} inbound_destination_mismatch_drops={} inbound_translation_drops={} inbound_removed_drops={} inbound_written={} inbound_write_backpressure_drops={} inbound_write_failures={} route_update_rejections={}",
                network.id,
                network.outbound_enqueued_packets,
                network.outbound_queue_drops,
                network.outbound_oversized_drops,
                network.outbound_source_mismatch_drops,
                network.outbound_translation_drops,
                network.outbound_removed_drops,
                network.inbound_enqueued_packets,
                network.inbound_queue_drops,
                network.inbound_oversized_drops,
                network.inbound_malformed_drops,
                network.inbound_source_mismatch_drops,
                network.inbound_destination_mismatch_drops,
                network.inbound_translation_drops,
                network.inbound_removed_drops,
                network.inbound_written_packets,
                network.inbound_write_backpressure_drops,
                network.inbound_write_failures,
                network.route_update_rejections,
            ));
        }
    }

    fn pair_rpc(
        network_id: Option<&str>,
        request: PairRpcRequest,
    ) -> Result<PairRpcResponseEnvelope, String> {
        let runtime = RUNTIME.lock().unwrap_or_else(|error| error.into_inner());
        let instance = runtime
            .as_ref()
            .ok_or_else(|| "p2p-vpn is not connected".to_owned())?;
        let network = match network_id {
            Some(network_id) => instance
                .networks
                .get(network_id)
                .ok_or_else(|| "requested p2p-vpn network is not active".to_owned())?,
            None if instance.networks.len() == 1 => instance
                .networks
                .values()
                .next()
                .ok_or_else(|| "p2p-vpn runtime has no active network".to_owned())?,
            None => {
                return Err(
                    "pairing RPC must select a network while multiple networks are active"
                        .to_owned(),
                );
            }
        };
        let control = network.control().ok_or_else(|| {
            "requested p2p-vpn network is temporarily unavailable while recovering".to_owned()
        })?;
        drop(runtime);
        block_on_control(control.pair_rpc(request))
    }

    fn membership_mutation(
        network_id: &str,
        member_peer: Option<String>,
    ) -> Result<MembershipMutationResult, String> {
        let runtime = RUNTIME.lock().unwrap_or_else(|error| error.into_inner());
        let instance = runtime
            .as_ref()
            .ok_or_else(|| "p2p-vpn is not connected".to_owned())?;
        let network = instance
            .networks
            .get(network_id)
            .ok_or_else(|| "requested p2p-vpn network is not active".to_owned())?;
        let control = network.control().ok_or_else(|| {
            "requested p2p-vpn network is temporarily unavailable while recovering".to_owned()
        })?;
        drop(runtime);
        block_on_control(control.revoke_member(member_peer))
    }

    fn network_changed() -> Result<AndroidRuntimeNetworkChange, String> {
        let networks = RUNTIME
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|instance| {
                instance
                    .networks
                    .iter()
                    .map(|(id, network)| (id.clone(), network.control()))
                    .collect::<Vec<_>>()
            })
            .ok_or_else(|| "p2p-vpn is not connected".to_owned())?;
        let controls = networks
            .iter()
            .filter_map(|(id, control)| control.clone().map(|control| (id.clone(), control)))
            .collect::<Vec<_>>();
        let active_controls = controls.len();
        let mut results = request_network_changes(controls)?;
        for result in &results {
            if let Some(error) = &result.error {
                log::warn!(
                    "event=android_network_change_signal_failed network_id={} error={}",
                    result.id,
                    error,
                );
            }
        }
        if active_controls > 0 && results.iter().all(|result| result.change.is_none()) {
            return Err("network recovery signal failed for every running network".to_owned());
        }
        results.extend(networks.into_iter().filter_map(|(id, control)| {
            control
                .is_none()
                .then_some(AndroidRuntimeNetworkChangeResult {
                    id,
                    change: None,
                    error: Some("network runtime is recovering".to_owned()),
                })
        }));
        results.sort_by(|left, right| left.id.cmp(&right.id));
        if let [result] = results.as_slice()
            && let Some(change) = result.change
        {
            return Ok(AndroidRuntimeNetworkChange::Single(change));
        }
        Ok(AndroidRuntimeNetworkChange::Multiple { networks: results })
    }

    fn request_network_changes(
        controls: Vec<(String, RuntimeControlHandle)>,
    ) -> Result<Vec<AndroidRuntimeNetworkChangeResult>, String> {
        let runtime = control_runtime()?;
        runtime.block_on(async move {
            let mut tasks = tokio::task::JoinSet::new();
            let mut task_ids = BTreeMap::new();
            for (id, control) in controls {
                let task_id = id.clone();
                let task = tasks.spawn(async move {
                    let result = tokio::time::timeout(CONTROL_TIMEOUT, control.network_changed())
                        .await
                        .map_err(|_| "runtime control request timed out".to_owned())
                        .and_then(|result| {
                            result
                                .map_err(|error| format!("runtime control request failed: {error}"))
                        });
                    match result {
                        Ok(change) => AndroidRuntimeNetworkChangeResult {
                            id,
                            change: Some(change),
                            error: None,
                        },
                        Err(error) => AndroidRuntimeNetworkChangeResult {
                            id,
                            change: None,
                            error: Some(error),
                        },
                    }
                });
                task_ids.insert(task.id(), task_id);
            }
            let mut results = Vec::new();
            while let Some(result) = tasks.join_next_with_id().await {
                match result {
                    Ok((_, result)) => results.push(result),
                    Err(error) => results.push(AndroidRuntimeNetworkChangeResult {
                        id: task_ids
                            .remove(&error.id())
                            .unwrap_or_else(|| "unknown".to_owned()),
                        change: None,
                        error: Some("runtime network-change task failed".to_owned()),
                    }),
                }
            }
            results.sort_by(|left, right| left.id.cmp(&right.id));
            Ok(results)
        })
    }

    fn control_runtime() -> Result<tokio::runtime::Runtime, String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("failed to create control runtime: {error}"))
    }

    fn read_string(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Result<String, String> {
        env.get_string(value)
            .map(String::from)
            .map_err(|error| format!("failed to read Java string: {error}"))
    }

    fn read_optional_string(
        env: &mut JNIEnv<'_>,
        value: &JString<'_>,
    ) -> Result<Option<String>, String> {
        if value.as_raw().is_null() {
            return Ok(None);
        }
        read_string(env, value).map(Some)
    }

    fn jni_response<T: Serialize>(
        env: &mut JNIEnv<'_>,
        operation: impl FnOnce(&mut JNIEnv<'_>) -> Result<T, String>,
    ) -> jstring {
        init_logging();
        let result = catch_unwind(AssertUnwindSafe(|| operation(env)))
            .map_err(|_| "native operation panicked".to_owned())
            .and_then(|result| result);
        let response = match result {
            Ok(value) => BridgeResponse {
                ok: true,
                value: Some(value),
                error: None,
            },
            Err(error) => BridgeResponse {
                ok: false,
                value: None,
                error: Some(error),
            },
        };
        let encoded = serde_json::to_string(&response).unwrap_or_else(|_| {
            r#"{"ok":false,"error":"failed to encode native response"}"#.to_owned()
        });
        env.new_string(encoded)
            .map_or(ptr::null_mut(), |value| value.into_raw())
    }

    fn init_logging() {
        LOG_INIT.call_once(|| {
            android_logger::init_once(
                android_logger::Config::default()
                    .with_tag("p2p-vpn")
                    .with_max_level(log::LevelFilter::Info),
            );
            set_hook(Box::new(|panic| {
                if let Some(location) = panic.location() {
                    log::error!(
                        "event=native_panic file={} line={} column={}",
                        location.file(),
                        location.line(),
                        location.column()
                    );
                } else {
                    log::error!("event=native_panic location=unknown");
                }
            }));
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use p2p_vpn::{
        membership::{
            MembershipRecordIssueOptions, MembershipRecordSubject, MembershipRole,
            issue_named_membership_record_for_subject_at,
        },
        network_peer::{
            NETWORK_PEER_SNAPSHOT_SCHEMA_VERSION, NetworkPeerConnectionState, NetworkPeerInviter,
            NetworkPeerMembership, NetworkPeerMembershipSource, NetworkPeerMembershipState,
            NetworkPeerPathKind, NetworkPeerPathOrigin, NetworkPeerSnapshotPeer,
        },
        pairing::{
            PairingOfferOptions, PairingResponseOptions, build_pairing_response_at,
            export_code_pairing_offer_at,
        },
        runtime::{
            control_socket::{
                PairRpcCompletionArtifacts, PairRpcNixPlan, PairRpcPeer, PairRpcReceipt,
                PairRpcRole,
            },
            pairing_bootstrap::PairingBootstrapEnrollment,
        },
    };

    use super::*;

    const ALPHA_NETWORK_ID: &str = "00000000-0000-4000-8000-000000000001";
    const BETA_NETWORK_ID: &str = "00000000-0000-4000-8000-000000000002";

    #[test]
    fn control_timeout_is_created_inside_the_runtime() {
        let response =
            block_on_control(async { Ok::<_, std::io::Error>("ready") }).expect("control response");

        assert_eq!(response, "ready");
    }

    #[test]
    fn android_runtime_status_serializes_the_bounded_peer_snapshot() {
        let status = AndroidNetworkRuntimeStatus {
            id: ALPHA_NETWORK_ID.to_owned(),
            phase: AndroidRuntimePhase::Running,
            detail: None,
            lines: Vec::new(),
            peer_snapshot: Some(NetworkPeerSnapshot {
                schema_version: NETWORK_PEER_SNAPSHOT_SCHEMA_VERSION,
                observed_at_unix_seconds: 1_788_291_000,
                total_peers: 1,
                returned_peers: 1,
                truncated: false,
                peers: vec![NetworkPeerSnapshotPeer {
                    peer_id: "12D3KooWAndroidPeer".to_owned(),
                    hostnames: vec!["android-phone".to_owned()],
                    ipv4: vec![Ipv4Addr::new(10, 42, 0, 2)],
                    ipv6: Vec::new(),
                    local: false,
                    membership: Some(NetworkPeerMembership {
                        state: NetworkPeerMembershipState::Active,
                        effective_inviter: Some(NetworkPeerInviter {
                            peer_id: "12D3KooWInviter".to_owned(),
                            hostname: Some("inviter".to_owned()),
                        }),
                        original_inviter: Some(NetworkPeerInviter {
                            peer_id: "12D3KooWInviter".to_owned(),
                            hostname: Some("inviter".to_owned()),
                        }),
                        admitted_at_unix_seconds: Some(1_788_290_000),
                        original_admitted_at_unix_seconds: Some(1_788_290_000),
                        state_changed_at_unix_seconds: Some(1_788_290_000),
                    }),
                    membership_sources: vec![NetworkPeerMembershipSource::SignedMembership],
                    connection_state: NetworkPeerConnectionState::Connected,
                    selected_path: Some(NetworkPeerPathKind::DirectQuicStream),
                    path_origin: Some(NetworkPeerPathOrigin::Mdns),
                }],
            }),
        };

        let encoded = serde_json::to_value(status).expect("serialized runtime status");

        assert_eq!(encoded["peer_snapshot"]["schema_version"], 1);
        assert_eq!(
            encoded["peer_snapshot"]["peers"][0]["connection_state"],
            "connected"
        );
        assert_eq!(
            encoded["peer_snapshot"]["peers"][0]["selected_path"],
            "direct_quic_stream"
        );
        assert_eq!(
            encoded["peer_snapshot"]["peers"][0]["membership"]["effective_inviter"]["hostname"],
            "inviter"
        );
        assert!(
            encoded["peer_snapshot"]["peers"][0]
                .get("multiaddr")
                .is_none()
        );
    }

    #[test]
    fn profile_free_enrollment_uses_signed_assigned_hostname() {
        let inviter_profile = create_profile("personal").expect("inviter profile");
        let inviter_config: Config =
            serde_json::from_str(&inviter_profile.config_json).expect("inviter config");
        let inviter = inviter_config.identity().expect("inviter identity");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner identity");
        let offer =
            export_code_pairing_offer_at(&inviter_config, PairingOfferOptions::default(), 1_000)
                .expect("pairing offer");
        let response = build_pairing_response_at(
            &inviter_config,
            &offer,
            PairingResponseOptions {
                joiner_peer: joiner.peer_id.clone(),
                assigned_vpn_ip: None,
                membership_key: None,
                member_records: vec![
                    named_member_record(&inviter, &inviter, "inviter", 1_010),
                    named_member_record(&inviter, &joiner, "assigned-phone", 1_010),
                ],
                expires_in_seconds: 300,
            },
            1_010,
        )
        .expect("pairing response");
        let profile = profile_from_bootstrap_enrollment(
            joiner.clone(),
            &PairingBootstrapEnrollment { offer, response },
            1_011,
        )
        .expect("paired profile");
        let config: Config = serde_json::from_str(&profile.config_json).expect("paired config");

        assert_eq!(profile.network_name, "personal");
        assert_eq!(profile.hostname, "assigned-phone");
        assert_eq!(profile.peer_id, joiner.peer_id);
        assert_eq!(
            config.network.dns.hostname.as_deref(),
            Some("assigned-phone")
        );
        assert_eq!(config.network.member_records.len(), 2);
        config.validate_runtime().expect("valid paired profile");
    }

    fn named_member_record(
        issuer: &NodeIdentity,
        member: &NodeIdentity,
        hostname: &str,
        issued_at_unix_seconds: u64,
    ) -> SignedMembershipRecord {
        issue_named_membership_record_for_subject_at(
            issuer,
            MembershipRecordIssueOptions {
                network_name: "personal".to_owned(),
                member: MembershipRecordSubject::from_identity(member).expect("member subject"),
                membership_epoch: 1,
                sequence: issued_at_unix_seconds,
                revoked: false,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            Some(hostname),
            issued_at_unix_seconds,
        )
        .expect("named membership record")
    }

    #[test]
    fn runtime_start_request_prepares_two_isolated_networks() {
        let alpha = create_profile("alpha").expect("alpha profile");
        let beta = create_profile("beta").expect("beta profile");
        let mut beta_config: Config =
            serde_json::from_str(&beta.config_json).expect("beta configuration");
        beta_config.interface.mtu = 1_200;
        let beta_config = serde_json::to_string(&beta_config).expect("beta JSON");
        let request = runtime_start_request(vec![
            runtime_network(ALPHA_NETWORK_ID, &alpha.config_json, ALPHA_NETWORK_ID),
            runtime_network(BETA_NETWORK_ID, &beta_config, BETA_NETWORK_ID),
        ]);

        let prepared = prepare_android_runtime_start(&request.to_string()).expect("start request");
        let validation =
            validate_android_runtime_start(&request.to_string()).expect("runtime preflight");

        assert_eq!(prepared.networks.len(), 2);
        assert_eq!(
            validation,
            AndroidRuntimeValidation {
                networks: 2,
                mtu: 1_200,
            }
        );
        assert_eq!(prepared.networks[0].id, ALPHA_NETWORK_ID);
        assert_eq!(prepared.networks[0].config.network.name, "alpha");
        assert_eq!(prepared.networks[1].id, BETA_NETWORK_ID);
        assert_eq!(prepared.networks[1].config.network.name, "beta");
        assert_ne!(
            prepared.networks[0].pairing_state_path.parent(),
            prepared.networks[1].pairing_state_path.parent()
        );
        assert_ne!(
            prepared.networks[0].membership_state_path.parent(),
            prepared.networks[1].membership_state_path.parent()
        );
        assert_eq!(prepared.networks[1].tun.mtu, 1_200);
        assert_eq!(prepared.tun_mtu, 1_200);
        assert_eq!(
            prepared.presentation.ipv4,
            Ipv4Addr::new(100, 127, 255, 254)
        );
        assert_eq!(
            prepared.presentation.ipv6,
            "fd00:6879:7072:7370:ffff:ffff:ffff:fffe"
                .parse::<Ipv6Addr>()
                .expect("presentation IPv6")
        );
    }

    #[test]
    fn runtime_preflight_rejects_overlapping_network_routes() {
        let alpha = create_profile("alpha").expect("alpha profile");
        let beta = create_profile("beta").expect("beta profile");
        let mut alpha_config: Config =
            serde_json::from_str(&alpha.config_json).expect("alpha configuration");
        let mut beta_config: Config =
            serde_json::from_str(&beta.config_json).expect("beta configuration");
        alpha_config.peers.push(route_peer("192.0.2.0/24"));
        beta_config.peers.push(route_peer("192.0.2.0/24"));
        let request = runtime_start_request(vec![
            runtime_network(
                ALPHA_NETWORK_ID,
                &serde_json::to_string(&alpha_config).expect("alpha JSON"),
                ALPHA_NETWORK_ID,
            ),
            runtime_network(
                BETA_NETWORK_ID,
                &serde_json::to_string(&beta_config).expect("beta JSON"),
                BETA_NETWORK_ID,
            ),
        ]);

        let error = validate_android_runtime_start(&request.to_string())
            .expect_err("overlapping routes must fail preflight");

        assert!(error.contains("overlap"), "unexpected error: {error}");
    }

    #[test]
    fn runtime_start_request_rejects_unknown_schema_and_fields() {
        let profile = create_profile("alpha").expect("profile");
        let mut future = runtime_start_request(vec![runtime_network(
            ALPHA_NETWORK_ID,
            &profile.config_json,
            ALPHA_NETWORK_ID,
        )]);
        future["schema_version"] = serde_json::json!(2);
        assert!(runtime_start_error(&future).contains("unsupported Android runtime start schema"));

        let mut unknown = runtime_start_request(vec![runtime_network(
            ALPHA_NETWORK_ID,
            &profile.config_json,
            ALPHA_NETWORK_ID,
        )]);
        unknown["networks"][0]["unexpected"] = serde_json::json!(true);
        assert!(runtime_start_error(&unknown).contains("unknown field"));
    }

    #[test]
    fn runtime_start_request_rejects_duplicate_ids_and_identities() {
        let alpha = create_profile("alpha").expect("alpha profile");
        let beta = create_profile("beta").expect("beta profile");
        let duplicate_id = runtime_start_request(vec![
            runtime_network(ALPHA_NETWORK_ID, &alpha.config_json, ALPHA_NETWORK_ID),
            runtime_network(ALPHA_NETWORK_ID, &beta.config_json, ALPHA_NETWORK_ID),
        ]);
        assert!(runtime_start_error(&duplicate_id).contains("duplicates a network ID"));

        let mut duplicate_identity: Config =
            serde_json::from_str(&alpha.config_json).expect("alpha configuration");
        duplicate_identity.network.name = "beta".to_owned();
        let duplicate_identity =
            serde_json::to_string(&duplicate_identity).expect("duplicate identity JSON");
        let request = runtime_start_request(vec![
            runtime_network(ALPHA_NETWORK_ID, &alpha.config_json, ALPHA_NETWORK_ID),
            runtime_network(BETA_NETWORK_ID, &duplicate_identity, BETA_NETWORK_ID),
        ]);
        assert!(runtime_start_error(&request).contains("reuses a network identity"));
    }

    #[test]
    fn runtime_start_request_rejects_duplicate_names_and_dns_zones() {
        let alpha = create_profile("alpha").expect("alpha profile");
        let beta = create_profile("beta").expect("beta profile");
        let mut beta_config: Config =
            serde_json::from_str(&beta.config_json).expect("beta configuration");
        beta_config.network.name = "ALPHA".to_owned();
        let duplicate_name = serde_json::to_string(&beta_config).expect("duplicate name JSON");
        let request = runtime_start_request(vec![
            runtime_network(ALPHA_NETWORK_ID, &alpha.config_json, ALPHA_NETWORK_ID),
            runtime_network(BETA_NETWORK_ID, &duplicate_name, BETA_NETWORK_ID),
        ]);
        assert!(runtime_start_error(&request).contains("duplicates a network name"));

        let mut alpha_config: Config =
            serde_json::from_str(&alpha.config_json).expect("alpha configuration");
        alpha_config.network.dns.enabled = true;
        beta_config.network.dns.enabled = true;
        let alpha_dns = serde_json::to_string(&alpha_config).expect("alpha DNS JSON");
        let beta_dns = serde_json::to_string(&beta_config).expect("beta DNS JSON");
        let request = runtime_start_request(vec![
            runtime_network(ALPHA_NETWORK_ID, &alpha_dns, ALPHA_NETWORK_ID),
            runtime_network(BETA_NETWORK_ID, &beta_dns, BETA_NETWORK_ID),
        ]);
        assert!(runtime_start_error(&request).contains("duplicates a DNS zone"));
    }

    #[test]
    fn runtime_start_request_rejects_noncanonical_ids_and_shared_state() {
        let alpha = create_profile("alpha").expect("alpha profile");
        let beta = create_profile("beta").expect("beta profile");
        let malformed = runtime_start_request(vec![runtime_network(
            "00000000-0000-4000-8000-00000000000A",
            &alpha.config_json,
            "00000000-0000-4000-8000-00000000000A",
        )]);
        assert!(runtime_start_error(&malformed).contains("invalid network ID"));

        let shared_state = runtime_start_request(vec![
            runtime_network(ALPHA_NETWORK_ID, &alpha.config_json, ALPHA_NETWORK_ID),
            runtime_network(BETA_NETWORK_ID, &beta.config_json, ALPHA_NETWORK_ID),
        ]);
        assert!(runtime_start_error(&shared_state).contains("does not match its network ID"));
    }

    #[test]
    fn runtime_state_path_errors_do_not_echo_path_contents() {
        let profile = create_profile("alpha").expect("profile");
        let mut request = runtime_start_request(vec![runtime_network(
            ALPHA_NETWORK_ID,
            &profile.config_json,
            ALPHA_NETWORK_ID,
        )]);
        request["networks"][0]["pairing_state_path"] =
            serde_json::json!("/tmp/private-marker/not-pairing-state.json");

        let error = runtime_start_error(&request);

        assert!(error.contains("invalid state path"));
        assert!(!error.contains("private-marker"));
    }

    #[test]
    fn generated_profile_is_minimal_valid_and_has_overlay_routes() {
        let profile = create_profile("personal").expect("profile");
        let encoded: serde_json::Value =
            serde_json::from_str(&profile.config_json).expect("profile JSON");

        assert_eq!(encoded["network"]["name"], "personal");
        assert!(encoded["network"]["private_key"].is_string());
        assert_eq!(encoded["network"]["dns"]["enabled"], false);
        assert_eq!(encoded["network"]["dns"]["hostname"], profile.hostname);
        assert!(profile.hostname.starts_with("android-"));
        assert_eq!(profile.hostname.len(), 24);
        assert!(
            profile.hostname[8..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
        assert_eq!(profile.addresses.len(), 2);
        assert!(profile.routes.contains(&AndroidCidr {
            address: BUILTIN_IPV4_NETWORK.to_string(),
            prefix_length: BUILTIN_IPV4_PREFIX,
        }));
        assert!(profile.routes.contains(&AndroidCidr {
            address: BUILTIN_IPV6_NETWORK.to_string(),
            prefix_length: BUILTIN_IPV6_PREFIX,
        }));
    }

    #[test]
    fn generated_profile_accepts_an_explicit_android_device_hostname() {
        let profile = create_profile_with_hostname("personal", "midi-pixel")
            .expect("profile with device hostname");
        let encoded: serde_json::Value =
            serde_json::from_str(&profile.config_json).expect("profile JSON");

        assert_eq!(profile.hostname, "midi-pixel");
        assert_eq!(encoded["network"]["dns"]["hostname"], "midi-pixel");
        assert!(create_profile_with_hostname("personal", "not valid!").is_err());
    }

    #[test]
    fn profile_hostname_can_change_without_changing_network_identity() {
        let profile =
            create_profile_with_hostname("personal", "old-phone").expect("profile with hostname");

        let renamed = rename_profile(&profile.config_json, "Pixel-8-Pro").expect("renamed profile");

        assert_eq!(renamed.hostname, "pixel-8-pro");
        assert_eq!(renamed.peer_id, profile.peer_id);
        assert_eq!(renamed.addresses, profile.addresses);
        assert_eq!(renamed.routes, profile.routes);
        assert!(rename_profile(&profile.config_json, "not valid!").is_err());
    }

    #[test]
    fn profile_inspection_migrates_a_missing_android_hostname_stably() {
        let profile = create_profile("personal").expect("profile");
        let mut config: Config =
            serde_json::from_str(&profile.config_json).expect("profile config");
        config.network.dns.hostname = None;
        let legacy = serde_json::to_string(&config).expect("legacy profile");

        let migrated = inspect_profile(&legacy).expect("migrated profile");

        assert_eq!(migrated.peer_id, profile.peer_id);
        assert_eq!(migrated.hostname, profile.hostname);
        assert_eq!(
            inspect_profile(&migrated.config_json)
                .expect("reinspected profile")
                .hostname,
            profile.hostname
        );
    }

    #[test]
    fn fixture_profile_configures_only_its_bootstrap_router() {
        let bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
        let address = format!("/ip4/10.0.2.2/tcp/42300/p2p/{}", bootstrap.peer_id);
        let profile = create_profile_with_bootstrap(
            "android-e2e",
            &bootstrap.peer_id,
            &address,
            "/p2p-vpn/kad/1",
        )
        .expect("fixture profile");
        let config: Config = serde_json::from_str(&profile.config_json).expect("profile config");

        assert!(config.peers.is_empty());
        assert_eq!(config.network.bootstrap_peers.len(), 1);
        assert_eq!(config.network.bootstrap_peers[0].id, bootstrap.peer_id);
        assert_eq!(config.network.bootstrap_peers[0].address, address);
        assert!(!config.network.discovery.mdns);
        assert_eq!(config.network.discovery.kademlia_protocol, "/p2p-vpn/kad/1");
        assert!(!config.uses_public_ipfs_bootstrap_defaults());
        let encoded: serde_json::Value =
            serde_json::from_str(&profile.config_json).expect("profile JSON");
        assert!(encoded["network"].get("packet_plane").is_none());
    }

    #[test]
    fn fixture_profile_adds_only_a_valid_requested_route() {
        let bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
        let address = format!("/ip4/10.0.2.2/tcp/42300/p2p/{}", bootstrap.peer_id);
        let profile = create_profile_with_bootstrap_and_e2e_paths_and_route(
            "android-e2e",
            &bootstrap.peer_id,
            &address,
            "/p2p-vpn/kad/1",
            None,
            None,
            None,
            Some("198.51.100.0/24"),
        )
        .expect("fixture profile with route");
        let config: Config = serde_json::from_str(&profile.config_json).expect("profile config");

        assert_eq!(
            config.network.routes,
            [RouteConfig {
                prefix: "198.51.100.0/24".to_owned(),
                metric: 0,
            }]
        );

        let invalid_route = "sensitive invalid route";
        let error = match create_profile_with_bootstrap_and_e2e_paths_and_route(
            "android-e2e",
            &bootstrap.peer_id,
            &address,
            "/p2p-vpn/kad/1",
            None,
            None,
            None,
            Some(invalid_route),
        ) {
            Ok(_) => panic!("invalid route was accepted"),
            Err(error) => error,
        };
        assert!(error.contains("invalid isolated E2E path configuration"));
        assert!(!error.contains(invalid_route));
    }

    #[test]
    fn fixture_profile_configures_only_owned_quic_packet_endpoints() {
        let bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
        let address = format!("/ip4/10.0.2.2/tcp/42300/p2p/{}", bootstrap.peer_id);
        let profile = create_profile_with_bootstrap_and_e2e_paths(
            "android-e2e",
            &bootstrap.peer_id,
            &address,
            "/p2p-vpn/kad/1",
            Some("0.0.0.0:51821"),
            Some("127.0.0.1:51821"),
            None,
        )
        .expect("owned QUIC fixture profile");
        let config: Config = serde_json::from_str(&profile.config_json).expect("profile config");

        assert!(config.network.packet_plane.listen.is_empty());
        assert!(config.network.packet_plane.external_endpoints.is_empty());
        assert_eq!(
            config.network.packet_plane.quic_listen,
            vec!["0.0.0.0:51821"]
        );
        assert_eq!(
            config.network.packet_plane.quic_external_endpoints,
            vec!["127.0.0.1:51821"]
        );
        config.validate_runtime().expect("valid owned QUIC profile");
    }

    #[test]
    fn fixture_profile_rejects_incomplete_or_invalid_owned_quic_settings() {
        let bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
        let address = format!("/ip4/10.0.2.2/tcp/42300/p2p/{}", bootstrap.peer_id);

        assert!(
            create_profile_with_bootstrap_and_e2e_paths(
                "android-e2e",
                &bootstrap.peer_id,
                &address,
                "/p2p-vpn/kad/1",
                Some("0.0.0.0:51821"),
                None,
                None,
            )
            .is_err()
        );
        assert!(
            create_profile_with_bootstrap_and_e2e_paths(
                "android-e2e",
                &bootstrap.peer_id,
                &address,
                "/p2p-vpn/kad/1",
                None,
                Some("127.0.0.1:51821"),
                None,
            )
            .is_err()
        );

        let invalid_endpoint = "sensitive invalid endpoint";
        let error = match create_profile_with_bootstrap_and_e2e_paths(
            "android-e2e",
            &bootstrap.peer_id,
            &address,
            "/p2p-vpn/kad/1",
            Some("0.0.0.0:51821"),
            Some(invalid_endpoint),
            None,
        ) {
            Ok(_) => panic!("invalid external endpoint was accepted"),
            Err(error) => error,
        };
        assert!(!error.contains(invalid_endpoint));

        let oversized_endpoint = "a".repeat(E2E_PACKET_QUIC_ENDPOINT_MAX_LENGTH + 1);
        assert!(
            create_profile_with_bootstrap_and_e2e_paths(
                "android-e2e",
                &bootstrap.peer_id,
                &address,
                "/p2p-vpn/kad/1",
                Some(&oversized_endpoint),
                Some("127.0.0.1:51821"),
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn fixture_profile_configures_relay_without_direct_listeners() {
        let bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
        let address = format!("/ip4/10.0.2.2/tcp/42300/p2p/{}", bootstrap.peer_id);
        let reservation = format!("{address}/p2p-circuit");
        let profile = create_profile_with_bootstrap_and_e2e_paths(
            "android-e2e",
            &bootstrap.peer_id,
            &address,
            "/p2p-vpn/kad/1",
            None,
            None,
            Some(&reservation),
        )
        .expect("relay fixture profile");
        let config: Config = serde_json::from_str(&profile.config_json).expect("profile config");

        assert!(config.network.listen_addresses.is_empty());
        assert_eq!(config.network.relay.reservations, [reservation]);
        assert!(!config.network.discovery.dcutr);
        assert!(!config.network.discovery.autonat);
        config.validate_runtime().expect("valid relay profile");
    }

    #[test]
    fn fixture_profile_rejects_combined_owned_quic_and_relay_paths() {
        let bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
        let address = format!("/ip4/10.0.2.2/tcp/42300/p2p/{}", bootstrap.peer_id);
        let reservation = format!("{address}/p2p-circuit");

        assert!(
            create_profile_with_bootstrap_and_e2e_paths(
                "android-e2e",
                &bootstrap.peer_id,
                &address,
                "/p2p-vpn/kad/1",
                Some("0.0.0.0:51821"),
                Some("127.0.0.1:51821"),
                Some(&reservation),
            )
            .is_err()
        );
    }

    #[test]
    fn fixture_profile_rejects_invalid_bootstrap_settings() {
        let bootstrap = NodeIdentity::generate_ed25519().expect("bootstrap identity");
        let address = format!("/ip4/10.0.2.2/tcp/42300/p2p/{}", bootstrap.peer_id);

        assert!(
            create_profile_with_bootstrap(
                "android-e2e",
                "not-a-peer-id",
                &address,
                "/p2p-vpn/kad/1"
            )
            .is_err()
        );
        assert!(
            create_profile_with_bootstrap(
                "android-e2e",
                &bootstrap.peer_id,
                "/ip4/10.0.2.2/tcp/not-a-port",
                "/p2p-vpn/kad/1"
            )
            .is_err()
        );
        assert!(
            create_profile_with_bootstrap(
                "android-e2e",
                &bootstrap.peer_id,
                &address,
                "p2p-vpn/kad/1"
            )
            .is_err()
        );
    }

    #[test]
    fn pairing_artifacts_update_the_encrypted_profile_payload() {
        let profile = create_profile("runners").expect("profile");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let artifacts = PairRpcCompletionArtifacts {
            receipt: PairRpcReceipt {
                network_name: "runners".to_owned(),
                local_peer: profile.peer_id.clone(),
                remote_peer: remote.peer_id.clone(),
                role: PairRpcRole::Joiner,
                transcript_sha256: "ab".repeat(32),
                completed_at_unix_seconds: 1_700_000_000,
            },
            nix: PairRpcNixPlan {
                instance_name: "runners".to_owned(),
                network_name: "runners".to_owned(),
                local_peer: profile.peer_id,
                assigned_vpn_ip: Some("10.42.0.2".to_owned()),
                additional_local_routes: Vec::new(),
                peer: PairRpcPeer {
                    id: remote.peer_id,
                    name: Some("runner-host".to_owned()),
                    vpn_ip: Some("10.42.0.1".to_owned()),
                    routes: Vec::new(),
                },
                member_records: Vec::new(),
                membership_key_file: None,
            },
        };
        let updated = apply_pairing_artifacts(
            &profile.config_json,
            &serde_json::to_string(&artifacts).expect("artifacts JSON"),
        )
        .expect("updated profile");
        let config: Config = serde_json::from_str(&updated.config_json).expect("updated config");

        assert_eq!(config.network.vpn_ip.as_deref(), Some("10.42.0.2"));
        assert_eq!(config.peers.len(), 1);
        assert_eq!(config.peers[0].name.as_deref(), Some("runner-host"));
        assert!(updated.addresses.contains(&AndroidCidr {
            address: "10.42.0.2".to_owned(),
            prefix_length: 32,
        }));
        assert!(updated.routes.contains(&AndroidCidr {
            address: "10.42.0.1".to_owned(),
            prefix_length: 32,
        }));
    }

    #[test]
    fn profile_rejects_blank_or_mismatched_networks() {
        assert!(create_profile("  ").is_err());
        let profile = create_profile("one").expect("profile");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let artifacts = PairRpcCompletionArtifacts {
            receipt: PairRpcReceipt {
                network_name: "two".to_owned(),
                local_peer: profile.peer_id.clone(),
                remote_peer: remote.peer_id.clone(),
                role: PairRpcRole::Inviter,
                transcript_sha256: "cd".repeat(32),
                completed_at_unix_seconds: 1_700_000_000,
            },
            nix: PairRpcNixPlan {
                instance_name: "two".to_owned(),
                network_name: "two".to_owned(),
                local_peer: profile.peer_id,
                assigned_vpn_ip: None,
                additional_local_routes: Vec::new(),
                peer: PairRpcPeer {
                    id: remote.peer_id,
                    name: None,
                    vpn_ip: None,
                    routes: Vec::new(),
                },
                member_records: Vec::new(),
                membership_key_file: None,
            },
        };

        assert!(
            apply_pairing_artifacts(
                &profile.config_json,
                &serde_json::to_string(&artifacts).expect("artifacts JSON"),
            )
            .is_err()
        );
    }

    #[test]
    fn managed_membership_key_is_unlinked_before_profile_update() {
        let profile = create_profile("private").expect("profile");
        let remote = NodeIdentity::generate_ed25519().expect("remote identity");
        let state_directory = test_state_directory("consume-membership-key");
        fs::create_dir_all(&state_directory).expect("state directory");
        let membership_key_file = state_directory.join("membership.key");
        fs::write(
            &membership_key_file,
            "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=\n",
        )
        .expect("membership key");
        let artifacts = PairRpcCompletionArtifacts {
            receipt: PairRpcReceipt {
                network_name: "private".to_owned(),
                local_peer: profile.peer_id.clone(),
                remote_peer: remote.peer_id.clone(),
                role: PairRpcRole::Joiner,
                transcript_sha256: "ef".repeat(32),
                completed_at_unix_seconds: 1_700_000_000,
            },
            nix: PairRpcNixPlan {
                instance_name: "private".to_owned(),
                network_name: "private".to_owned(),
                local_peer: profile.peer_id.clone(),
                assigned_vpn_ip: None,
                additional_local_routes: Vec::new(),
                peer: PairRpcPeer {
                    id: remote.peer_id,
                    name: None,
                    vpn_ip: None,
                    routes: Vec::new(),
                },
                member_records: Vec::new(),
                membership_key_file: Some(
                    membership_key_file
                        .to_str()
                        .expect("UTF-8 state path")
                        .to_owned(),
                ),
            },
        };

        let updated = apply_pairing_artifacts_with_managed_secrets(
            &profile.config_json,
            &serde_json::to_string(&artifacts).expect("artifacts JSON"),
            &state_directory,
        )
        .expect("updated profile");
        let config: Config = serde_json::from_str(&updated.config_json).expect("updated config");

        assert_eq!(
            config.network.membership_key.as_deref(),
            Some("CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=")
        );
        assert!(!membership_key_file.exists());
        fs::remove_dir_all(state_directory).expect("remove state directory");
    }

    fn route_peer(prefix: &str) -> PeerConfig {
        PeerConfig {
            id: NodeIdentity::generate_ed25519()
                .expect("route peer identity")
                .peer_id,
            name: None,
            ip: None,
            vpn_ip: None,
            addresses: Vec::new(),
            routes: vec![RouteConfig {
                prefix: prefix.to_owned(),
                metric: 0,
            }],
        }
    }

    fn runtime_start_request(networks: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "schema_version": ANDROID_RUNTIME_START_SCHEMA_VERSION,
            "presentation_addresses": {
                "ipv4": "100.127.255.254",
                "ipv6": "fd00:6879:7072:7370:ffff:ffff:ffff:fffe",
            },
            "networks": networks,
        })
    }

    fn runtime_start_error(request: &serde_json::Value) -> String {
        match prepare_android_runtime_start(&request.to_string()) {
            Ok(_) => panic!("invalid Android runtime start request was accepted"),
            Err(error) => error,
        }
    }

    fn runtime_network(id: &str, config_json: &str, state_directory: &str) -> serde_json::Value {
        let state_directory =
            format!("/data/user/0/org.hermeticfoundation.p2pvpn/files/runtime/{state_directory}");
        serde_json::json!({
            "id": id,
            "config_json": config_json,
            "pairing_state_path": format!("{state_directory}/pairing-state.json"),
            "membership_state_path": format!("{state_directory}/membership-state.json"),
        })
    }

    fn test_state_directory(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "p2p-vpn-android-{label}-{}-{unique}",
            std::process::id()
        ))
    }
}
