use std::{
    fs,
    io::Read as _,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::Path,
};

use p2p_vpn::{
    PeerId,
    config::{BootstrapPeerConfig, Config, DiscoveryConfig, PeerConfig, RouteConfig},
    dns::canonical_dns_label,
    identity::NodeIdentity,
    membership::SignedMembershipRecord,
    route::IpCidr,
    runtime::{control_socket::PairRpcCompletionArtifacts, tun::TunRuntimeConfig},
};
use serde::Serialize;

#[cfg(any(target_os = "android", test))]
mod supervisor;

const BUILTIN_IPV4_NETWORK: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 0);
const BUILTIN_IPV4_PREFIX: u8 = 16;
const BUILTIN_IPV6_NETWORK: Ipv6Addr =
    Ipv6Addr::new(0xfd00, 0x6879, 0x7072, 0x7370, 0x6163, 0x6500, 0, 0);
const BUILTIN_IPV6_PREFIX: u8 = 96;
const E2E_PACKET_QUIC_ENDPOINT_MAX_LENGTH: usize = 512;
const E2E_RELAY_RESERVATION_MAX_LENGTH: usize = 1_024;
#[cfg(any(target_os = "android", test))]
const CONTROL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AndroidRuntimePhase {
    Starting,
    Running,
    Stopped,
    Failed,
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
    create_profile_with_identity(network_name, identity, None, None, None)
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
    )
}

fn create_profile_with_identity(
    network_name: &str,
    identity: NodeIdentity,
    bootstrap: Option<(BootstrapPeerConfig, DiscoveryConfig)>,
    packet_quic: Option<(String, String)>,
    relay_reservation: Option<String>,
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
        fs::File,
        io,
        io::{Read as _, Write as _},
        os::fd::{AsRawFd as _, FromRawFd as _},
        panic::{AssertUnwindSafe, catch_unwind, set_hook},
        ptr,
        sync::{
            Arc, Mutex, Once,
            atomic::{AtomicBool, AtomicU8, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use jni::{
        JNIEnv,
        objects::{JClass, JString},
        sys::{jint, jstring},
    };
    use p2p_vpn::runtime::{
        control_socket::{
            PairRpcRequest, PairRpcResponseEnvelope, RuntimeControlHandle, RuntimeNetworkChange,
            runtime_control_channel,
        },
        runner::{RuntimePlatform, ShutdownReason, run_config_until_with_runtime_platform},
        tun::{PacketRead, PacketWrite, TunRuntimeConfig},
    };
    use tokio::sync::oneshot;

    use super::{
        supervisor::{NetworkSpec, PacketSwitch, QueueLimits},
        *,
    };

    static LOG_INIT: Once = Once::new();
    static RUNTIME: Mutex<Option<RuntimeInstance>> = Mutex::new(None);
    const CONTROL_FAILURE_LIMIT: u8 = 3;
    const TUN_WRITE_POLL_TIMEOUT: Duration = Duration::from_millis(250);

    struct RuntimeInstance {
        control: RuntimeControlHandle,
        shutdown: Option<oneshot::Sender<ShutdownReason>>,
        tun_stop: Arc<AtomicBool>,
        tun_error: Arc<Mutex<Option<String>>>,
        packet_switch: Arc<PacketSwitch>,
        control_failures: Arc<AtomicU8>,
        status: Arc<Mutex<AndroidRuntimeStatus>>,
        thread: Option<thread::JoinHandle<()>>,
        tun_threads: Vec<thread::JoinHandle<()>>,
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
    ) -> jstring {
        jni_response(&mut env, |env| {
            let network_name = read_string(env, &network_name)?;
            create_profile(&network_name)
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
            create_profile_with_bootstrap_and_e2e_paths(
                &network_name,
                &bootstrap_peer_id,
                &bootstrap_address,
                &kademlia_protocol,
                packet_quic_listen.as_deref(),
                packet_quic_external_endpoint.as_deref(),
                relay_reservation.as_deref(),
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
            start_runtime(
                network_id,
                &config_json,
                tun_file,
                pairing_state_path,
                membership_state_path,
            )
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
    pub extern "system" fn Java_org_hermeticfoundation_p2pvpn_NativeBridge_nativePairRpc(
        mut env: JNIEnv,
        _class: JClass,
        request_json: JString,
    ) -> jstring {
        jni_response(&mut env, |env| {
            let request_json = read_string(env, &request_json)?;
            let request: PairRpcRequest = serde_json::from_str(&request_json)
                .map_err(|error| format!("invalid pairing request: {error}"))?;
            pair_rpc(request)
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

    fn start_runtime(
        network_id: String,
        config_json: &str,
        writer_file: File,
        pairing_state_path: String,
        membership_state_path: String,
    ) -> Result<AndroidRuntimeStatus, String> {
        init_logging();
        let _ = stop_runtime();
        let config: Config = serde_json::from_str(config_json)
            .map_err(|error| format!("invalid profile JSON: {error}"))?;
        config
            .validate_runtime()
            .map_err(|error| format!("invalid profile: {error:?}"))?;
        let tun = TunRuntimeConfig::from_config(&config)
            .map_err(|error| format!("invalid profile routes: {error:?}"))?;
        let tun_mtu = tun.mtu;
        let (packet_switch, mut ports) = PacketSwitch::new(
            vec![NetworkSpec {
                id: network_id.clone(),
                tun,
            }],
            QueueLimits::DEFAULT,
        )
        .map_err(|error| error.to_string())?;
        let packet_switch = Arc::new(packet_switch);
        let network_port = ports
            .pop()
            .ok_or_else(|| "Android packet supervisor did not create a network port".to_owned())?;
        if network_port.id != network_id {
            return Err("Android packet supervisor returned the wrong network port".to_owned());
        }
        let network_lease = packet_switch
            .network_lease(&network_id)
            .map_err(|error| error.to_string())?;

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

        let reader_switch = Arc::clone(&packet_switch);
        let reader_stop = Arc::clone(&tun_stop);
        let reader_error = Arc::clone(&tun_error);
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
                            return;
                        }
                    }
                }
            })
            .map_err(|error| format!("failed to start shared TUN reader: {error}"))?;

        let writer_switch = Arc::clone(&packet_switch);
        let writer_stop = Arc::clone(&tun_stop);
        let writer_error = Arc::clone(&tun_error);
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

        let (control, receiver) = runtime_control_channel();
        let (shutdown, shutdown_rx) = oneshot::channel();
        let status = Arc::new(Mutex::new(AndroidRuntimeStatus {
            phase: AndroidRuntimePhase::Starting,
            detail: None,
            lines: Vec::new(),
        }));
        let control_failures = Arc::new(AtomicU8::new(0));
        let thread_status = Arc::clone(&status);
        let thread = match thread::Builder::new()
            .name("p2p-vpn-runtime".to_owned())
            .spawn(move || {
                let _network_lease = network_lease;
                let result = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .map_err(|error| format!("failed to create async runtime: {error}"))
                    .and_then(|runtime| {
                        runtime.block_on(run_config_until_with_runtime_platform(
                            config,
                            RuntimePlatform::new(
                                network_port.packet_io,
                                network_port.route_controller,
                            )
                                .with_control(receiver),
                            None,
                            None,
                            Some(pairing_state_path.into()),
                            Some(membership_state_path.into()),
                            async move {
                                shutdown_rx.await.unwrap_or(ShutdownReason::Terminate)
                            },
                        ))
                        .map_err(|error| format!("runtime failed: {error:?}"))
                    });
                let mut status = thread_status
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                match result {
                    Ok(()) => {
                        status.phase = AndroidRuntimePhase::Stopped;
                        status.detail = None;
                    }
                    Err(error) => {
                        status.phase = AndroidRuntimePhase::Failed;
                        status.detail = Some(error);
                    }
                }
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

        let instance = RuntimeInstance {
            control,
            shutdown: Some(shutdown),
            tun_stop,
            tun_error,
            packet_switch,
            control_failures,
            status: Arc::clone(&status),
            thread: Some(thread),
            tun_threads: vec![reader_thread, writer_thread],
        };
        *RUNTIME.lock().unwrap_or_else(|error| error.into_inner()) = Some(instance);
        Ok(status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone())
    }

    fn record_tun_error(target: &Mutex<Option<String>>, operation: &str, error: &io::Error) {
        let mut target = target.lock().unwrap_or_else(|error| error.into_inner());
        if target.is_none() {
            *target = Some(format!("shared Android TUN {operation} failed: {error}"));
        }
    }

    fn stop_runtime() -> Result<AndroidRuntimeStatus, String> {
        let Some(mut instance) = RUNTIME
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        else {
            return Ok(AndroidRuntimeStatus {
                phase: AndroidRuntimePhase::Stopped,
                detail: None,
                lines: Vec::new(),
            });
        };

        let _ = block_on_control(instance.control.shutdown());
        if let Some(shutdown) = instance.shutdown.take() {
            let _ = shutdown.send(ShutdownReason::Terminate);
        }
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
        Ok(instance
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone())
    }

    fn runtime_status() -> Result<AndroidRuntimeStatus, String> {
        let (control, control_failures, status, tun_error, packet_switch) = {
            let runtime = RUNTIME.lock().unwrap_or_else(|error| error.into_inner());
            let Some(instance) = runtime.as_ref() else {
                return Ok(AndroidRuntimeStatus {
                    phase: AndroidRuntimePhase::Stopped,
                    detail: None,
                    lines: Vec::new(),
                });
            };
            (
                instance.control.clone(),
                Arc::clone(&instance.control_failures),
                Arc::clone(&instance.status),
                Arc::clone(&instance.tun_error),
                Arc::clone(&instance.packet_switch),
            )
        };

        let mut snapshot = status
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let tun_failure = tun_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if let Some(error) = tun_failure {
            snapshot.phase = AndroidRuntimePhase::Failed;
            snapshot.detail = Some(error);
        }
        if matches!(
            snapshot.phase,
            AndroidRuntimePhase::Starting | AndroidRuntimePhase::Running
        ) {
            match block_on_control(control.status()) {
                Ok(lines) => {
                    control_failures.store(0, Ordering::Release);
                    snapshot.phase = AndroidRuntimePhase::Running;
                    snapshot.detail = None;
                    snapshot.lines = lines;
                }
                Err(error) => {
                    let previous = control_failures
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                            Some(value.saturating_add(1))
                        })
                        .unwrap_or_else(|value| value);
                    let failures = previous.saturating_add(1);
                    snapshot.detail = Some(format!(
                        "runtime health check {failures} of {CONTROL_FAILURE_LIMIT} failed: {error}"
                    ));
                    if failures >= CONTROL_FAILURE_LIMIT {
                        snapshot.phase = AndroidRuntimePhase::Failed;
                    }
                }
            }

            let mut current = status.lock().unwrap_or_else(|error| error.into_inner());
            if matches!(
                current.phase,
                AndroidRuntimePhase::Stopped | AndroidRuntimePhase::Failed
            ) {
                snapshot = current.clone();
            } else {
                *current = snapshot.clone();
            }
        }
        append_switch_status(&mut snapshot, &packet_switch);
        Ok(snapshot)
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
                "android_supervisor_network index={index} outbound_enqueued={} outbound_queue_drops={} outbound_oversized_drops={} outbound_source_mismatch_drops={} outbound_removed_drops={} inbound_enqueued={} inbound_queue_drops={} inbound_oversized_drops={} inbound_malformed_drops={} inbound_source_mismatch_drops={} inbound_destination_mismatch_drops={} inbound_removed_drops={} inbound_written={} inbound_write_backpressure_drops={} inbound_write_failures={} route_update_rejections={}",
                network.outbound_enqueued_packets,
                network.outbound_queue_drops,
                network.outbound_oversized_drops,
                network.outbound_source_mismatch_drops,
                network.outbound_removed_drops,
                network.inbound_enqueued_packets,
                network.inbound_queue_drops,
                network.inbound_oversized_drops,
                network.inbound_malformed_drops,
                network.inbound_source_mismatch_drops,
                network.inbound_destination_mismatch_drops,
                network.inbound_removed_drops,
                network.inbound_written_packets,
                network.inbound_write_backpressure_drops,
                network.inbound_write_failures,
                network.route_update_rejections,
            ));
        }
    }

    fn pair_rpc(request: PairRpcRequest) -> Result<PairRpcResponseEnvelope, String> {
        let control = RUNTIME
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|instance| instance.control.clone())
            .ok_or_else(|| "p2p-vpn is not connected".to_owned())?;
        block_on_control(control.pair_rpc(request))
    }

    fn network_changed() -> Result<RuntimeNetworkChange, String> {
        let control = RUNTIME
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|instance| instance.control.clone())
            .ok_or_else(|| "p2p-vpn is not connected".to_owned())?;
        block_on_control(control.network_changed())
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

    use p2p_vpn::runtime::control_socket::{
        PairRpcCompletionArtifacts, PairRpcNixPlan, PairRpcPeer, PairRpcReceipt, PairRpcRole,
    };

    use super::*;

    #[test]
    fn control_timeout_is_created_inside_the_runtime() {
        let response =
            block_on_control(async { Ok::<_, std::io::Error>("ready") }).expect("control response");

        assert_eq!(response, "ready");
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
