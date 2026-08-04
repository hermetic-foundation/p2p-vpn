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
