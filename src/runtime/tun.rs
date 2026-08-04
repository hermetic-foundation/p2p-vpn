use std::{
    fmt,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    process::{Command, ExitStatus},
};

use crate::{
    PeerId,
    config::{Config, effective_packet_mtu},
    route::{IpCidr, Route, builtin_ipv4, builtin_ipv6},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TunAddresses {
    pub ipv4: Ipv4Addr,
    pub ipv6: Ipv6Addr,
}

impl TunAddresses {
    #[must_use]
    pub fn for_peer(peer: PeerId) -> Self {
        Self {
            ipv4: builtin_ipv4(peer),
            ipv6: builtin_ipv6(peer),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunRuntimeConfig {
    pub name: String,
    pub mtu: u16,
    pub addresses: TunAddresses,
    pub routes: Vec<Route>,
}

impl TunRuntimeConfig {
    pub fn from_config(config: &Config) -> Result<Self, TunRuntimeError> {
        let local_peer = config.local_peer_id()?;
        let routes = config
            .compile_routes()?
            .routes()
            .iter()
            .copied()
            .filter(|route| route.owner != local_peer)
            .collect();

        Ok(Self {
            name: config.interface.name.clone(),
            mtu: effective_packet_mtu(config.interface.mtu),
            addresses: TunAddresses::for_peer(local_peer),
            routes,
        })
    }

    #[must_use]
    pub fn route_commands(&self) -> Vec<IpCommand> {
        let mut commands = vec![IpCommand::addr_add_v6(
            self.name.clone(),
            IpCidr::new(IpAddr::V6(self.addresses.ipv6), 128)
                .expect("128 is a valid IPv6 prefix length"),
        )];

        commands.extend(
            self.routes
                .iter()
                .map(|route| IpCommand::route_replace(self.name.clone(), route.prefix, self.mtu)),
        );
        commands
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpCommand {
    args: Vec<String>,
}

impl IpCommand {
    #[must_use]
    pub fn addr_add_v6(interface: String, prefix: IpCidr) -> Self {
        Self {
            args: vec![
                "-6".to_owned(),
                "addr".to_owned(),
                "replace".to_owned(),
                prefix.to_string(),
                "dev".to_owned(),
                interface,
            ],
        }
    }

    #[must_use]
    pub fn route_replace(interface: String, prefix: IpCidr, mtu: u16) -> Self {
        let mut args = Vec::new();
        if prefix.address().is_ipv6() {
            args.push("-6".to_owned());
        }
        args.extend([
            "route".to_owned(),
            "replace".to_owned(),
            prefix.to_string(),
            "dev".to_owned(),
            interface,
            "metric".to_owned(),
            "3000".to_owned(),
            "mtu".to_owned(),
            mtu.to_string(),
        ]);
        if let Some(advmss) = route_advmss(prefix, mtu) {
            args.extend(["advmss".to_owned(), advmss.to_string()]);
        }
        Self { args }
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn execute(&self) -> Result<ExitStatus, io::Error> {
        Command::new("ip").args(&self.args).status()
    }
}

impl fmt::Display for IpCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ip {}", self.args.join(" "))
    }
}

#[must_use]
pub fn route_advmss(prefix: IpCidr, mtu: u16) -> Option<u16> {
    let header_bytes = match prefix.address() {
        IpAddr::V4(_) => 40,
        IpAddr::V6(_) => 60,
    };
    mtu.checked_sub(header_bytes)
}

#[must_use]
pub fn packet_too_big(original: &[u8], mtu: u16) -> Option<Vec<u8>> {
    match original.first().map(|byte| byte >> 4) {
        Some(4) => ipv4_packet_too_big(original, mtu),
        Some(6) => ipv6_packet_too_big(original, mtu),
        _ => None,
    }
}

fn ipv4_packet_too_big(original: &[u8], mtu: u16) -> Option<Vec<u8>> {
    if original.len() < 20 {
        return None;
    }
    let ihl = usize::from(original[0] & 0x0f) * 4;
    if ihl < 20 || original.len() < ihl {
        return None;
    }

    let quote_len = original.len().min(548);
    let total_len = 20 + 8 + quote_len;
    let total_len = u16::try_from(total_len).ok()?;
    let mut packet = vec![0; usize::from(total_len)];
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(&total_len.to_be_bytes());
    packet[8] = 64;
    packet[9] = 1;
    packet[12..16].copy_from_slice(&original[16..20]);
    packet[16..20].copy_from_slice(&original[12..16]);
    let header_checksum = internet_checksum(&packet[..20]);
    packet[10..12].copy_from_slice(&header_checksum.to_be_bytes());

    let icmp = 20;
    packet[icmp] = 3;
    packet[icmp + 1] = 4;
    packet[icmp + 6..icmp + 8].copy_from_slice(&mtu.to_be_bytes());
    packet[icmp + 8..icmp + 8 + quote_len].copy_from_slice(&original[..quote_len]);
    let icmp_checksum = internet_checksum(&packet[icmp..]);
    packet[icmp + 2..icmp + 4].copy_from_slice(&icmp_checksum.to_be_bytes());

    Some(packet)
}

fn ipv6_packet_too_big(original: &[u8], mtu: u16) -> Option<Vec<u8>> {
    if original.len() < 40 {
        return None;
    }

    let quote_len = original.len().min(1232);
    let payload_len = 8 + quote_len;
    let payload_len = u16::try_from(payload_len).ok()?;
    let mut packet = vec![0; 40 + usize::from(payload_len)];
    packet[0] = 0x60;
    packet[4..6].copy_from_slice(&payload_len.to_be_bytes());
    packet[6] = 58;
    packet[7] = 64;
    packet[8..24].copy_from_slice(&original[24..40]);
    packet[24..40].copy_from_slice(&original[8..24]);

    let icmp = 40;
    packet[icmp] = 2;
    packet[icmp + 4..icmp + 8].copy_from_slice(&u32::from(mtu).to_be_bytes());
    packet[icmp + 8..icmp + 8 + quote_len].copy_from_slice(&original[..quote_len]);
    let icmp_checksum = icmpv6_checksum(&packet[8..24], &packet[24..40], &packet[icmp..]);
    packet[icmp + 2..icmp + 4].copy_from_slice(&icmp_checksum.to_be_bytes());

    Some(packet)
}

fn icmpv6_checksum(source: &[u8], destination: &[u8], payload: &[u8]) -> u16 {
    let payload_len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let mut pseudo = Vec::with_capacity(40 + payload.len());
    pseudo.extend_from_slice(source);
    pseudo.extend_from_slice(destination);
    pseudo.extend_from_slice(&payload_len.to_be_bytes());
    pseudo.extend_from_slice(&[0, 0, 0, 58]);
    pseudo.extend_from_slice(payload);
    internet_checksum(&pseudo)
}

fn internet_checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0_u32;
    for chunk in bytes.chunks(2) {
        let word = if chunk.len() == 2 {
            u16::from_be_bytes([chunk[0], chunk[1]])
        } else {
            u16::from(chunk[0]) << 8
        };
        sum = sum.wrapping_add(u32::from(word));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !u16::try_from(sum).expect("checksum sum is folded to 16 bits")
}

#[cfg(target_os = "linux")]
pub struct TunDevice {
    device: tun::Device,
}

#[cfg(target_os = "linux")]
pub struct TunReader {
    reader: tun::Reader,
}

#[cfg(target_os = "linux")]
pub struct TunWriter {
    writer: tun::Writer,
}

#[cfg(target_os = "linux")]
impl TunDevice {
    pub fn open(config: &TunRuntimeConfig) -> Result<Self, TunRuntimeError> {
        let mut tun_config = tun::Configuration::default();
        tun_config
            .tun_name(&config.name)
            .address(config.addresses.ipv4)
            .netmask(Ipv4Addr::BROADCAST)
            .mtu(config.mtu)
            .up()
            .layer(tun::Layer::L3);

        let device = tun::create(&tun_config)?;
        Ok(Self { device })
    }

    pub fn name(&self) -> Result<String, TunRuntimeError> {
        Ok(tun::AbstractDevice::tun_name(&self.device)?)
    }

    #[must_use]
    pub fn split(self) -> (TunReader, TunWriter) {
        let (reader, writer) = self.device.split();
        (TunReader { reader }, TunWriter { writer })
    }
}

#[cfg(target_os = "linux")]
impl TunReader {
    pub fn read_packet(&mut self, buffer: &mut [u8]) -> Result<usize, TunRuntimeError> {
        Ok(self.reader.read(buffer)?)
    }
}

#[cfg(target_os = "linux")]
impl TunWriter {
    pub fn write_packet(&mut self, packet: &[u8]) -> Result<usize, TunRuntimeError> {
        self.writer.write_all(packet)?;
        Ok(packet.len())
    }
}

#[derive(Debug)]
pub enum TunRuntimeError {
    Config(crate::config::ConfigError),
    Tun(tun::Error),
    Io(io::Error),
}

impl From<crate::config::ConfigError> for TunRuntimeError {
    fn from(error: crate::config::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<tun::Error> for TunRuntimeError {
    fn from(error: tun::Error) -> Self {
        Self::Tun(error)
    }
}

impl From<io::Error> for TunRuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use crate::{
        config::{
            Config, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig, ResourceConfig,
            RouteConfig,
        },
        route::builtin_ipv4,
    };

    use super::*;

    fn peer_hex(seed: u8) -> String {
        format!("{seed:02x}").repeat(32)
    }

    fn ipv4_packet(source: Ipv4Addr, destination: Ipv4Addr) -> Vec<u8> {
        let mut packet = vec![0; 60];
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&60_u16.to_be_bytes());
        packet[8] = 64;
        packet[9] = 6;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        packet
    }

    fn ipv6_packet(source: Ipv6Addr, destination: Ipv6Addr) -> Vec<u8> {
        let mut packet = vec![0; 80];
        packet[0] = 0x60;
        packet[4..6].copy_from_slice(&40_u16.to_be_bytes());
        packet[6] = 17;
        packet[7] = 64;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        packet
    }

    #[test]
    fn packet_too_big_builds_ipv4_fragmentation_needed() {
        let source = Ipv4Addr::new(100, 64, 1, 10);
        let destination = Ipv4Addr::new(100, 64, 2, 20);
        let original = ipv4_packet(source, destination);

        let reply = packet_too_big(&original, 1180).expect("packet too big");

        assert_eq!(reply[0] >> 4, 4);
        assert_eq!(reply[9], 1);
        assert_eq!(&reply[12..16], &destination.octets());
        assert_eq!(&reply[16..20], &source.octets());
        assert_eq!(reply[20], 3);
        assert_eq!(reply[21], 4);
        assert_eq!(u16::from_be_bytes([reply[26], reply[27]]), 1180);
        assert_eq!(&reply[28..], original.as_slice());
        assert_eq!(internet_checksum(&reply[..20]), 0);
        assert_eq!(internet_checksum(&reply[20..]), 0);
    }

    #[test]
    fn packet_too_big_builds_ipv6_packet_too_big() {
        let source = Ipv6Addr::LOCALHOST;
        let destination = Ipv6Addr::UNSPECIFIED;
        let original = ipv6_packet(source, destination);

        let reply = packet_too_big(&original, 1200).expect("packet too big");

        assert_eq!(reply[0] >> 4, 6);
        assert_eq!(reply[6], 58);
        assert_eq!(&reply[8..24], &destination.octets());
        assert_eq!(&reply[24..40], &source.octets());
        assert_eq!(reply[40], 2);
        assert_eq!(reply[41], 0);
        assert_eq!(
            u32::from_be_bytes([reply[44], reply[45], reply[46], reply[47]]),
            1200
        );
        assert_eq!(&reply[48..], original.as_slice());
        assert_eq!(
            icmpv6_checksum(&reply[8..24], &reply[24..40], &reply[40..]),
            0
        );
    }

    #[test]
    fn runtime_config_derives_local_addresses() {
        let local = PeerId::from_bytes([7; 32]);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: local.to_string(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: Vec::new(),
            queue: QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };

        let runtime = TunRuntimeConfig::from_config(&config).expect("runtime config");

        assert_eq!(runtime.name, "hs0");
        assert_eq!(runtime.mtu, 1280);
        assert_eq!(runtime.addresses.ipv4, builtin_ipv4(local));
    }

    #[test]
    fn runtime_config_uses_effective_packet_mtu() {
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: peer_hex(1),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: u16::MAX,
            },
            peers: Vec::new(),
            queue: QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };

        let runtime = TunRuntimeConfig::from_config(&config).expect("runtime config");

        assert_eq!(runtime.mtu, config.effective_packet_mtu());
    }

    #[test]
    fn runtime_config_installs_only_remote_routes() {
        let remote = PeerId::from_bytes([
            2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0,
        ]);
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: peer_hex(1),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 100,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: Some("node-b".to_owned()),
                addresses: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 10,
                }],
            }],
            queue: QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };

        let runtime = TunRuntimeConfig::from_config(&config).expect("runtime config");
        let local = config.local_peer_id().expect("local peer");

        assert!(runtime.routes.iter().all(|route| route.owner != local));
        assert!(
            runtime
                .routes
                .iter()
                .any(|route| route.prefix.to_string() == "10.42.0.0/24")
        );
        assert!(
            !runtime
                .routes
                .iter()
                .any(|route| route.prefix.to_string() == "10.41.0.0/24")
        );
    }

    #[test]
    fn route_commands_install_ipv6_address_and_peer_routes() {
        let config = Config {
            network: NetworkConfig {
                name: "lab".to_owned(),
                local_peer: peer_hex(1),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: PeerId::from_bytes([
                    2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0,
                ])
                .to_string(),
                name: Some("node-b".to_owned()),
                addresses: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.42.0.99/24".to_owned(),
                    metric: 10,
                }],
            }],
            queue: QueueConfig {
                max_packets_per_peer: 8,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        };

        let runtime = TunRuntimeConfig::from_config(&config).expect("runtime config");
        let commands = runtime
            .route_commands()
            .into_iter()
            .map(|command| command.to_string())
            .collect::<Vec<_>>();

        assert!(
            commands.iter().any(
                |command| command.starts_with("ip -6 addr replace fd00:6879:7072:7370:6163:65")
            )
        );
        assert!(commands.iter().any(|command| command
            == "ip route replace 10.42.0.0/24 dev hs0 metric 3000 mtu 1280 advmss 1240"));
        assert!(runtime.routes.iter().any(|route| {
            route
                .prefix
                .contains(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 10)))
        }));
    }

    #[test]
    fn route_commands_add_ipv6_mtu_and_mss_hint() {
        let command = IpCommand::route_replace(
            "hs0".to_owned(),
            IpCidr::new(
                "fd00:6879:7072:7370:6163:6500:4200:0"
                    .parse()
                    .expect("IPv6 network"),
                112,
            )
            .expect("IPv6 CIDR"),
            1280,
        );

        assert_eq!(
            command.to_string(),
            "ip -6 route replace fd00:6879:7072:7370:6163:6500:4200:0/112 dev hs0 metric 3000 mtu 1280 advmss 1220"
        );
    }

    #[test]
    fn route_commands_omit_mss_hint_when_mtu_is_too_small() {
        let command = IpCommand::route_replace(
            "hs0".to_owned(),
            IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 0)), 24).expect("IPv4 CIDR"),
            39,
        );

        assert_eq!(
            command.to_string(),
            "ip route replace 10.42.0.0/24 dev hs0 metric 3000 mtu 39"
        );
    }
}
