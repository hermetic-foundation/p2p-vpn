use std::{
    fmt, fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::PathBuf,
    process::{Command, ExitStatus},
};

#[cfg(target_os = "linux")]
use std::io::{Read as _, Write as _};

use crate::{
    PeerId,
    config::{Config, RouteConfig, effective_packet_mtu, vpn_ip_host_route},
    route::{IpCidr, Route, builtin_ipv4, builtin_ipv6},
};

/// Blocking packet source used by the platform-neutral runtime.
///
/// Implementations should arrange for a blocked read to wake periodically so
/// the owning runtime can shut down without leaking a worker thread.
pub trait PacketRead: Send + 'static {
    fn read_packet(&mut self, buffer: &mut [u8]) -> io::Result<usize>;
}

/// Blocking packet sink used by the platform-neutral runtime.
pub trait PacketWrite: Send + 'static {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize>;
}

pub struct PacketReader {
    inner: Box<dyn PacketRead>,
}

pub struct PacketWriter {
    inner: Box<dyn PacketWrite>,
}

/// A split packet device supplied by the host platform.
pub struct PacketIo {
    reader: PacketReader,
    writer: PacketWriter,
}

impl PacketIo {
    #[must_use]
    pub fn new(reader: impl PacketRead, writer: impl PacketWrite) -> Self {
        Self {
            reader: PacketReader {
                inner: Box::new(reader),
            },
            writer: PacketWriter {
                inner: Box::new(writer),
            },
        }
    }

    #[must_use]
    pub fn split(self) -> (PacketReader, PacketWriter) {
        (self.reader, self.writer)
    }
}

impl PacketReader {
    pub fn read_packet(&mut self, buffer: &mut [u8]) -> Result<usize, TunRuntimeError> {
        let length = self
            .inner
            .read_packet(buffer)
            .map_err(TunRuntimeError::Io)?;
        if length > buffer.len() {
            return Err(TunRuntimeError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "packet reader returned more bytes than its buffer",
            )));
        }
        Ok(length)
    }
}

impl PacketWriter {
    pub fn write_packet(&mut self, packet: &[u8]) -> Result<usize, TunRuntimeError> {
        let length = self
            .inner
            .write_packet(packet)
            .map_err(TunRuntimeError::Io)?;
        if length != packet.len() {
            return Err(TunRuntimeError::Io(io::Error::new(
                io::ErrorKind::WriteZero,
                "packet writer did not consume the complete packet",
            )));
        }
        Ok(length)
    }
}

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
    pub additional_addresses: Vec<IpCidr>,
    pub routes: Vec<Route>,
}

/// Structured ownership of additions which may survive a failed pairing commit.
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
pub(crate) struct PairingTunCleanup {
    interface: String,
    addresses: Vec<RouteConfig>,
    routes: Vec<RouteConfig>,
}

impl PairingTunCleanup {
    pub(crate) fn capture(
        installed: &TunRuntimeConfig,
        attempted: &TunRuntimeConfig,
    ) -> Result<Self, TunRuntimeError> {
        attempted.pairing_reconciliation_from(installed)?;
        let prefix_config = |prefix: IpCidr| RouteConfig {
            prefix: prefix.to_string(),
            metric: 0,
        };
        Ok(Self {
            interface: installed.name.clone(),
            addresses: attempted
                .additional_addresses
                .iter()
                .filter(|address| !installed.has_local_address(**address))
                .map(|address| prefix_config(*address))
                .collect(),
            routes: attempted
                .routes
                .iter()
                .filter(|route| {
                    !installed
                        .routes
                        .iter()
                        .any(|old| old.prefix == route.prefix)
                })
                .map(|route| prefix_config(route.prefix))
                .collect(),
        })
    }

    pub(crate) fn validate(&self) -> Result<(), TunRuntimeError> {
        if self.interface.is_empty() || self.interface.contains('\0') {
            return Err(TunRuntimeError::NonAdditiveUpdate(
                "invalid pairing cleanup interface",
            ));
        }
        for (entries, addresses) in [(&self.addresses, true), (&self.routes, false)] {
            let mut seen = std::collections::BTreeSet::new();
            for entry in entries {
                let prefix = entry.prefix().map_err(TunRuntimeError::Config)?;
                if entry.metric != 0
                    || !seen.insert(prefix.to_string())
                    || (addresses
                        && prefix.prefix_len() != if prefix.address().is_ipv4() { 32 } else { 128 })
                {
                    return Err(TunRuntimeError::NonAdditiveUpdate(
                        "invalid pairing cleanup prefix",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn merge(&mut self, next: &Self) -> Result<(), TunRuntimeError> {
        self.validate()?;
        next.validate()?;
        if self.interface != next.interface {
            return Err(TunRuntimeError::NonAdditiveUpdate(
                "pairing cleanup interface changed",
            ));
        }
        for (existing, additions) in [
            (&mut self.addresses, &next.addresses),
            (&mut self.routes, &next.routes),
        ] {
            for addition in additions {
                if !existing.contains(addition) {
                    existing.push(addition.clone());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn update(
        &self,
        surviving: &TunRuntimeConfig,
    ) -> Result<TunRouteUpdate, TunRuntimeError> {
        self.validate()?;
        if self.interface != surviving.name {
            return Err(TunRuntimeError::NonAdditiveUpdate(
                "pairing cleanup interface changed",
            ));
        }
        let mut commands = surviving.pairing_abort_commands_from(surviving)?;
        for route in &self.routes {
            let prefix = route.prefix().map_err(TunRuntimeError::Config)?;
            if !surviving.routes.iter().any(|route| route.prefix == prefix) {
                commands.push(IpCommand::route_delete(self.interface.clone(), prefix));
            }
        }
        for address in &self.addresses {
            let prefix = address.prefix().map_err(TunRuntimeError::Config)?;
            if !surviving.has_local_address(prefix) {
                commands.push(IpCommand::addr_delete(self.interface.clone(), prefix));
            }
        }
        Ok(TunRouteUpdate::abort_cleanup(commands))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunRouteUpdate {
    apply: Vec<IpCommand>,
    rollback: Vec<IpCommand>,
    abort_cleanup: bool,
}

impl TunRouteUpdate {
    pub(crate) fn abort_cleanup(apply: Vec<IpCommand>) -> Self {
        Self {
            apply,
            rollback: Vec::new(),
            abort_cleanup: true,
        }
    }

    pub(crate) const fn is_abort_cleanup(&self) -> bool {
        self.abort_cleanup
    }

    #[must_use]
    pub fn apply_commands(&self) -> &[IpCommand] {
        &self.apply
    }

    #[must_use]
    pub fn rollback_commands(&self) -> &[IpCommand] {
        &self.rollback
    }
}

impl TunRuntimeConfig {
    fn has_local_address(&self, prefix: IpCidr) -> bool {
        self.additional_addresses.contains(&prefix)
            || match (prefix.address(), prefix.prefix_len()) {
                (IpAddr::V4(address), 32) => address == self.addresses.ipv4,
                (IpAddr::V6(address), 128) => address == self.addresses.ipv6,
                _ => false,
            }
    }

    pub(crate) fn from_config_with_routes(
        config: &Config,
        routes: &[Route],
    ) -> Result<Self, TunRuntimeError> {
        let local_peer = config.local_peer_id()?;
        let additional_addresses = local_tun_addresses(config)?;
        Ok(Self {
            name: config.interface.name.clone(),
            mtu: effective_packet_mtu(config.interface.mtu),
            addresses: TunAddresses::for_peer(local_peer),
            additional_addresses,
            routes: routes
                .iter()
                .copied()
                .filter(|route| route.owner != local_peer)
                .collect(),
        })
    }

    pub fn from_config(config: &Config) -> Result<Self, TunRuntimeError> {
        Self::from_config_with_member_records(config, &config.network.member_records)
    }

    pub fn from_config_with_member_records(
        config: &Config,
        member_records: &[crate::membership::SignedMembershipRecord],
    ) -> Result<Self, TunRuntimeError> {
        let local_peer = config.local_peer_id()?;
        let additional_addresses = local_tun_addresses(config)?;
        let routes = config
            .compile_routes_with_member_records(member_records)?
            .routes()
            .iter()
            .copied()
            .filter(|route| route.owner != local_peer)
            .collect();

        Ok(Self {
            name: config.interface.name.clone(),
            mtu: effective_packet_mtu(config.interface.mtu),
            addresses: TunAddresses::for_peer(local_peer),
            additional_addresses,
            routes,
        })
    }

    pub fn from_config_with_member_records_at(
        config: &Config,
        member_records: &[crate::membership::SignedMembershipRecord],
        now_unix_seconds: u64,
    ) -> Result<Self, TunRuntimeError> {
        let local_peer = config.local_peer_id()?;
        let additional_addresses = local_tun_addresses(config)?;
        let routes = config
            .compile_routes_with_member_records_at(member_records, now_unix_seconds)?
            .routes()
            .iter()
            .copied()
            .filter(|route| route.owner != local_peer)
            .collect();

        Ok(Self {
            name: config.interface.name.clone(),
            mtu: effective_packet_mtu(config.interface.mtu),
            addresses: TunAddresses::for_peer(local_peer),
            additional_addresses,
            routes,
        })
    }

    #[must_use]
    pub fn sysctl_commands(&self) -> Vec<SysctlCommand> {
        vec![
            SysctlCommand::set_interface(self.name.clone(), "rp_filter", "0"),
            SysctlCommand::set_interface(self.name.clone(), "accept_local", "1"),
        ]
    }

    #[must_use]
    pub fn route_commands(&self) -> Vec<IpCommand> {
        let mut commands = vec![IpCommand::addr_add_v6(
            self.name.clone(),
            IpCidr::new(IpAddr::V6(self.addresses.ipv6), 128)
                .expect("128 is a valid IPv6 prefix length"),
        )];

        commands.extend(
            self.additional_addresses
                .iter()
                .copied()
                .map(|prefix| IpCommand::addr_replace(self.name.clone(), prefix)),
        );
        commands.extend(self.routes.iter().map(|route| {
            IpCommand::route_replace(
                self.name.clone(),
                route.prefix,
                self.route_source(route.prefix),
                self.mtu,
            )
        }));
        commands
    }

    pub fn additive_update_from(&self, current: &Self) -> Result<TunRouteUpdate, TunRuntimeError> {
        if self.name != current.name
            || self.mtu != current.mtu
            || self.addresses != current.addresses
        {
            return Err(TunRuntimeError::NonAdditiveUpdate(
                "pairing cannot change the running TUN identity or MTU",
            ));
        }
        if current
            .additional_addresses
            .iter()
            .any(|address| !self.has_local_address(*address))
            || current
                .routes
                .iter()
                .any(|route| !self.routes.contains(route))
        {
            return Err(TunRuntimeError::NonAdditiveUpdate(
                "pairing cannot remove or replace a running TUN address or route",
            ));
        }

        let new_addresses = self
            .additional_addresses
            .iter()
            .copied()
            .filter(|address| !current.has_local_address(*address))
            .collect::<Vec<_>>();
        let new_routes = self
            .routes
            .iter()
            .copied()
            .filter(|route| !current.routes.contains(route))
            .collect::<Vec<_>>();

        let mut apply = new_addresses
            .iter()
            .copied()
            .map(|prefix| IpCommand::addr_replace(self.name.clone(), prefix))
            .collect::<Vec<_>>();
        apply.extend(new_routes.iter().map(|route| {
            IpCommand::route_replace(
                self.name.clone(),
                route.prefix,
                self.route_source(route.prefix),
                self.mtu,
            )
        }));

        let mut rollback = new_addresses
            .iter()
            .copied()
            .map(|prefix| IpCommand::addr_delete(self.name.clone(), prefix))
            .collect::<Vec<_>>();
        rollback.extend(
            new_routes
                .iter()
                .map(|route| IpCommand::route_delete(self.name.clone(), route.prefix)),
        );

        Ok(TunRouteUpdate {
            apply,
            rollback,
            abort_cleanup: false,
        })
    }

    pub fn route_reconciliation_from(
        &self,
        current: &Self,
    ) -> Result<TunRouteUpdate, TunRuntimeError> {
        if self.name != current.name
            || self.mtu != current.mtu
            || self.addresses != current.addresses
            || self.additional_addresses != current.additional_addresses
        {
            return Err(TunRuntimeError::NonAdditiveUpdate(
                "live membership reconciliation cannot change the running TUN identity, MTU, or local addresses",
            ));
        }

        Ok(self.route_update_from(current))
    }

    pub(crate) fn pairing_reconciliation_from(
        &self,
        current: &Self,
    ) -> Result<TunRouteUpdate, TunRuntimeError> {
        if self.name != current.name
            || self.mtu != current.mtu
            || self.addresses != current.addresses
            || current
                .additional_addresses
                .iter()
                .any(|address| !self.has_local_address(*address))
        {
            return Err(TunRuntimeError::NonAdditiveUpdate(
                "pairing cannot change the running TUN identity, MTU, or remove local addresses",
            ));
        }
        let mut update = TunRouteUpdate {
            apply: Vec::new(),
            rollback: Vec::new(),
            abort_cleanup: false,
        };
        for address in &self.additional_addresses {
            if !current.has_local_address(*address) {
                update
                    .apply
                    .push(IpCommand::addr_replace(self.name.clone(), *address));
                update
                    .rollback
                    .push(IpCommand::addr_delete(self.name.clone(), *address));
            }
        }
        let routes = self.route_update_from(current);
        update.apply.extend(routes.apply);
        update.rollback.extend(routes.rollback);
        Ok(update)
    }

    pub(crate) fn pairing_abort_commands_from(
        &self,
        attempted: &Self,
    ) -> Result<Vec<IpCommand>, TunRuntimeError> {
        if self.name != attempted.name
            || self.mtu != attempted.mtu
            || self.addresses != attempted.addresses
        {
            return Err(TunRuntimeError::NonAdditiveUpdate(
                "pairing abort cannot change the running TUN identity or MTU",
            ));
        }
        // Restore surviving sources before routes, then remove pairing-only
        // addresses. Abort cleanup is retried, not rolled back into enrollment.
        let mut commands = self
            .additional_addresses
            .iter()
            .copied()
            .map(|address| IpCommand::addr_replace(self.name.clone(), address))
            .collect::<Vec<_>>();
        commands.extend(self.routes.iter().map(|route| {
            IpCommand::route_replace(
                self.name.clone(),
                route.prefix,
                self.route_source(route.prefix),
                self.mtu,
            )
        }));
        commands.extend(
            attempted
                .routes
                .iter()
                .filter(|route| {
                    !self
                        .routes
                        .iter()
                        .any(|survivor| survivor.prefix == route.prefix)
                })
                .map(|route| IpCommand::route_delete(self.name.clone(), route.prefix)),
        );
        commands.extend(
            attempted
                .additional_addresses
                .iter()
                .copied()
                .filter(|address| !self.has_local_address(*address))
                .map(|address| IpCommand::addr_delete(self.name.clone(), address)),
        );
        Ok(commands)
    }

    fn route_update_from(&self, current: &Self) -> TunRouteUpdate {
        let mut apply = Vec::new();
        let mut rollback = Vec::new();
        for route in &self.routes {
            let previous = current
                .routes
                .iter()
                .find(|candidate| candidate.prefix == route.prefix);
            if previous == Some(route)
                && self.route_source(route.prefix) == current.route_source(route.prefix)
            {
                continue;
            }
            apply.push(IpCommand::route_replace(
                self.name.clone(),
                route.prefix,
                self.route_source(route.prefix),
                self.mtu,
            ));
            rollback.push(previous.map_or_else(
                || IpCommand::route_delete(self.name.clone(), route.prefix),
                |previous| {
                    IpCommand::route_replace(
                        current.name.clone(),
                        previous.prefix,
                        current.route_source(previous.prefix),
                        current.mtu,
                    )
                },
            ));
        }
        for route in &current.routes {
            if self
                .routes
                .iter()
                .any(|candidate| candidate.prefix == route.prefix)
            {
                continue;
            }
            apply.push(IpCommand::route_delete(current.name.clone(), route.prefix));
            rollback.push(IpCommand::route_replace(
                current.name.clone(),
                route.prefix,
                current.route_source(route.prefix),
                current.mtu,
            ));
        }

        TunRouteUpdate {
            apply,
            rollback,
            abort_cleanup: false,
        }
    }

    fn route_source(&self, prefix: IpCidr) -> IpAddr {
        self.additional_addresses
            .iter()
            .map(|prefix| prefix.address())
            .find(|address| address.is_ipv4() == prefix.address().is_ipv4())
            .unwrap_or(match prefix.address() {
                IpAddr::V4(_) => IpAddr::V4(self.addresses.ipv4),
                IpAddr::V6(_) => IpAddr::V6(self.addresses.ipv6),
            })
    }
}

fn local_tun_addresses(config: &Config) -> Result<Vec<IpCidr>, crate::config::ConfigError> {
    let mut addresses = Vec::new();
    if let Some(vpn_ip) = &config.network.vpn_ip {
        push_unique(&mut addresses, vpn_ip_host_route(vpn_ip)?);
    }
    for route in &config.network.routes {
        if let Some(address) = route.host_address()? {
            push_unique(&mut addresses, address);
        }
    }
    Ok(addresses)
}

fn push_unique(addresses: &mut Vec<IpCidr>, address: IpCidr) {
    if !addresses.contains(&address) {
        addresses.push(address);
    }
}

trait RouteConfigExt {
    fn host_address(&self) -> Result<Option<IpCidr>, crate::config::ConfigError>;
}

impl RouteConfigExt for crate::config::RouteConfig {
    fn host_address(&self) -> Result<Option<IpCidr>, crate::config::ConfigError> {
        let prefix = self.prefix()?;
        let is_host_route = match prefix.address() {
            IpAddr::V4(_) => prefix.prefix_len() == 32,
            IpAddr::V6(_) => prefix.prefix_len() == 128,
        };

        Ok(is_host_route.then_some(prefix))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpCommand {
    args: Vec<String>,
    deletion: Option<IpDeletion>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct IpDeletion {
    interface: String,
    prefix: IpCidr,
    route: bool,
}

#[derive(serde::Deserialize)]
struct InterfaceAddressListing {
    ifname: String,
    addr_info: Vec<ListedAddress>,
}

#[derive(serde::Deserialize)]
struct ListedAddress {
    local: IpAddr,
    prefixlen: u8,
}

impl IpCommand {
    #[must_use]
    pub fn addr_add_v6(interface: String, prefix: IpCidr) -> Self {
        Self::addr_replace(interface, prefix)
    }

    #[must_use]
    pub fn addr_replace(interface: String, prefix: IpCidr) -> Self {
        let mut args = Vec::new();
        let is_ipv6 = prefix.address().is_ipv6();
        if is_ipv6 {
            args.push("-6".to_owned());
        }
        args.extend([
            "addr".to_owned(),
            "replace".to_owned(),
            prefix.to_string(),
            "dev".to_owned(),
            interface,
        ]);
        if is_ipv6 {
            args.push("nodad".to_owned());
        }
        Self {
            args,
            deletion: None,
        }
    }

    #[must_use]
    pub fn addr_delete(interface: String, prefix: IpCidr) -> Self {
        let deletion = Some(IpDeletion {
            interface: interface.clone(),
            prefix,
            route: false,
        });
        let mut args = Vec::new();
        if prefix.address().is_ipv6() {
            args.push("-6".to_owned());
        }
        args.extend([
            "addr".to_owned(),
            "del".to_owned(),
            prefix.to_string(),
            "dev".to_owned(),
            interface,
        ]);
        Self { args, deletion }
    }

    #[must_use]
    pub fn route_replace(interface: String, prefix: IpCidr, source: IpAddr, mtu: u16) -> Self {
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
            "src".to_owned(),
            source.to_string(),
            "metric".to_owned(),
            "3000".to_owned(),
            "mtu".to_owned(),
            mtu.to_string(),
        ]);
        if let Some(advmss) = route_advmss(prefix, mtu) {
            args.extend(["advmss".to_owned(), advmss.to_string()]);
        }
        Self {
            args,
            deletion: None,
        }
    }

    #[must_use]
    pub fn route_delete(interface: String, prefix: IpCidr) -> Self {
        let deletion = Some(IpDeletion {
            interface: interface.clone(),
            prefix,
            route: true,
        });
        let mut args = Vec::new();
        if prefix.address().is_ipv6() {
            args.push("-6".to_owned());
        }
        args.extend([
            "route".to_owned(),
            "del".to_owned(),
            prefix.to_string(),
            "dev".to_owned(),
            interface,
            "metric".to_owned(),
            "3000".to_owned(),
        ]);
        Self { args, deletion }
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn execute(&self) -> Result<ExitStatus, io::Error> {
        Command::new("ip").args(&self.args).status()
    }

    pub(crate) fn deletion_is_absent(&self) -> io::Result<bool> {
        self.deletion_is_absent_with(|args| {
            let output = Command::new("ip").args(args).output()?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "cannot verify pairing cleanup: {}",
                    String::from_utf8_lossy(&output.stderr),
                )));
            }
            Ok(output.stdout)
        })
    }

    fn deletion_is_absent_with(
        &self,
        mut query: impl FnMut(&[String]) -> io::Result<Vec<u8>>,
    ) -> io::Result<bool> {
        let Some(target) = &self.deletion else {
            return Ok(false);
        };
        let args = ["-j", "address", "show", "dev", &target.interface].map(str::to_owned);
        let interfaces: Vec<InterfaceAddressListing> = serde_json::from_slice(&query(&args)?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if interfaces.len() != 1 || interfaces[0].ifname != target.interface {
            return Err(io::Error::other(
                "pairing cleanup interface is missing or ambiguous",
            ));
        }
        if !target.route {
            return Ok(!interfaces[0].addr_info.iter().any(|address| {
                address.local == target.prefix.address()
                    && address.prefixlen == target.prefix.prefix_len()
            }));
        }
        let mut args = vec![];
        if target.prefix.address().is_ipv6() {
            args.push("-6".to_owned());
        }
        args.extend(["-j", "route", "show", "exact"].map(str::to_owned));
        args.extend([
            target.prefix.to_string(),
            "dev".to_owned(),
            target.interface.clone(),
            "metric".to_owned(),
            "3000".to_owned(),
        ]);
        let routes: Vec<serde_json::Map<String, serde_json::Value>> =
            serde_json::from_slice(&query(&args)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(routes.is_empty())
    }
}

impl fmt::Display for IpCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ip {}", self.args.join(" "))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysctlCommand {
    key: String,
    value: String,
    proc_path: PathBuf,
}

impl SysctlCommand {
    #[must_use]
    pub fn set(key: String, value: impl Into<String>) -> Self {
        let proc_path = PathBuf::from("/proc/sys").join(key.replace('.', "/"));
        Self {
            key,
            value: value.into(),
            proc_path,
        }
    }

    #[must_use]
    pub fn set_interface(
        interface: String,
        field: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let field = field.into();
        Self {
            key: format!("net.ipv4.conf.{interface}.{field}"),
            value: value.into(),
            proc_path: PathBuf::from("/proc/sys/net/ipv4/conf")
                .join(interface)
                .join(field),
        }
    }

    pub fn execute(&self) -> Result<(), io::Error> {
        fs::write(&self.proc_path, format!("{}\n", self.value))
    }
}

impl fmt::Display for SysctlCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "sysctl -w {}={}", self.key, self.value)
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

    #[must_use]
    pub fn into_packet_io(self) -> PacketIo {
        let (reader, writer) = self.split();
        PacketIo::new(reader, writer)
    }
}

#[cfg(target_os = "linux")]
impl TunReader {
    pub fn read_packet(&mut self, buffer: &mut [u8]) -> Result<usize, TunRuntimeError> {
        Ok(self.reader.read(buffer)?)
    }
}

#[cfg(target_os = "linux")]
impl PacketRead for TunReader {
    fn read_packet(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buffer)
    }
}

#[cfg(target_os = "linux")]
impl TunWriter {
    pub fn write_packet(&mut self, packet: &[u8]) -> Result<usize, TunRuntimeError> {
        self.writer.write_all(packet)?;
        Ok(packet.len())
    }
}

#[cfg(target_os = "linux")]
impl PacketWrite for TunWriter {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
        self.writer.write_all(packet)?;
        Ok(packet.len())
    }
}

#[derive(Debug)]
pub enum TunRuntimeError {
    Config(crate::config::ConfigError),
    #[cfg(target_os = "linux")]
    Tun(tun::Error),
    Io(io::Error),
    NonAdditiveUpdate(&'static str),
}

impl From<crate::config::ConfigError> for TunRuntimeError {
    fn from(error: crate::config::ConfigError) -> Self {
        Self::Config(error)
    }
}

#[cfg(target_os = "linux")]
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
    use std::{
        net::Ipv4Addr,
        sync::{Arc, Mutex},
    };

    use crate::{
        config::{
            Config, InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig, ResourceConfig,
            RouteConfig,
        },
        identity::NodeIdentity,
        membership::{
            MembershipRecordIssueOptions, MembershipRecordSubject, MembershipRole,
            issue_membership_record_for_subject_at,
        },
        route::builtin_ipv4,
    };

    use super::*;

    struct TestPacketReader(Vec<u8>);

    #[test]
    fn pairing_cannot_take_cleanup_ownership_of_builtin_addresses() {
        let installed = TunRuntimeConfig {
            name: "pv0".to_owned(),
            mtu: 1280,
            addresses: TunAddresses::for_peer(PeerId::from_bytes([1; 32])),
            additional_addresses: Vec::new(),
            routes: Vec::new(),
        };
        let mut attempted = installed.clone();
        attempted.additional_addresses = vec![
            IpCidr::new(installed.addresses.ipv4.into(), 32).unwrap(),
            IpCidr::new(installed.addresses.ipv6.into(), 128).unwrap(),
        ];
        let cleanup = PairingTunCleanup::capture(&installed, &attempted).unwrap();
        assert!(
            cleanup.addresses.is_empty(),
            "built-in addresses are already owned by the TUN"
        );
        assert!(
            attempted
                .additive_update_from(&installed)
                .unwrap()
                .apply_commands()
                .is_empty()
        );
        assert!(
            attempted
                .pairing_reconciliation_from(&installed)
                .unwrap()
                .apply_commands()
                .is_empty()
        );
        assert!(
            installed
                .pairing_abort_commands_from(&attempted)
                .unwrap()
                .is_empty()
        );
        let recorded = PairingTunCleanup {
            interface: installed.name.clone(),
            addresses: attempted
                .additional_addresses
                .iter()
                .map(|prefix| RouteConfig {
                    prefix: prefix.to_string(),
                    metric: 0,
                })
                .collect(),
            routes: Vec::new(),
        };
        assert!(
            recorded
                .update(&installed)
                .unwrap()
                .apply_commands()
                .is_empty(),
            "even a retained ownership record must preserve built-in addresses"
        );
    }

    #[test]
    fn cleanup_ownership_retains_additions_across_retries_and_preserves_survivors() {
        let prefix = |ip: &str| IpCidr::new(ip.parse().unwrap(), 32).unwrap();
        let route = |ip: &str| Route {
            owner: PeerId::from_bytes([2; 32]),
            prefix: prefix(ip),
            metric: 0,
        };
        let installed = TunRuntimeConfig {
            name: "pv0".to_owned(),
            mtu: 1280,
            addresses: TunAddresses::for_peer(PeerId::from_bytes([1; 32])),
            additional_addresses: vec![prefix("10.42.0.1")],
            routes: vec![route("10.42.0.2")],
        };
        let mut attempted = installed.clone();
        attempted.additional_addresses.push(prefix("10.43.0.1"));
        attempted.routes.push(route("10.43.0.2"));
        let mut cleanup = PairingTunCleanup::capture(&installed, &attempted).unwrap();
        assert_eq!(cleanup.addresses.len(), 1);
        assert_eq!(cleanup.routes.len(), 1);
        let original = cleanup.clone();
        cleanup.merge(&original).unwrap();
        assert_eq!(cleanup, original);
        attempted.routes.push(route("10.44.0.2"));
        cleanup
            .merge(&PairingTunCleanup::capture(&installed, &attempted).unwrap())
            .unwrap();
        assert_eq!(cleanup.routes.len(), 2);
        let decoded: PairingTunCleanup =
            serde_json::from_slice(&serde_json::to_vec(&cleanup).unwrap()).unwrap();
        assert_eq!(decoded, cleanup);
        let update = decoded.update(&installed).unwrap();
        let commands = update
            .apply_commands()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            &commands[commands.len() - 3..],
            [
                "ip route del 10.43.0.2/32 dev pv0 metric 3000",
                "ip route del 10.44.0.2/32 dev pv0 metric 3000",
                "ip addr del 10.43.0.1/32 dev pv0",
            ]
        );
        assert!(update.is_abort_cleanup());
        assert!(update.rollback_commands().is_empty());
        let surviving = attempted.clone();
        assert!(
            decoded
                .update(&surviving)
                .unwrap()
                .apply_commands()
                .iter()
                .all(|command| !command.args().iter().any(|arg| arg == "del"))
        );
        let mut wrong_interface = installed.clone();
        wrong_interface.name = "pv1".to_owned();
        assert!(decoded.update(&wrong_interface).is_err());
        for invalid in [
            serde_json::json!({"interface":"", "addresses":[], "routes":[]}),
            serde_json::json!({"interface":"pv0", "addresses":[{"prefix":"10.0.0.0/24"}], "routes":[]}),
            serde_json::json!({"interface":"pv0", "addresses":[], "routes":[{"prefix":"10.0.0.0/999"}]}),
            serde_json::json!({"interface":"pv0", "addresses":[], "routes":[{"prefix":"10.0.0.0/24", "metric":7}]}),
        ] {
            let invalid: PairingTunCleanup = serde_json::from_value(invalid).unwrap();
            assert!(invalid.validate().is_err());
            assert!(invalid.update(&installed).is_err());
            let before = cleanup.clone();
            assert!(cleanup.merge(&invalid).is_err());
            assert_eq!(cleanup, before);
        }
    }

    #[test]
    fn cleanup_absence_checks_are_exact_and_fail_closed() {
        let prefix = IpCidr::new("10.42.0.1".parse().unwrap(), 32).unwrap();
        let delete = IpCommand::addr_delete("pv0".to_owned(), prefix);
        let listing = |addresses: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!([
                {"ifname": "pv0", "addr_info": addresses}
            ]))
            .unwrap()
        };
        for (addresses, absent) in [
            (serde_json::json!([]), true),
            (
                serde_json::json!([{"local":"10.42.0.1", "prefixlen":32}]),
                false,
            ),
            (
                serde_json::json!([{"local":"10.42.0.2", "prefixlen":32}]),
                true,
            ),
            (
                serde_json::json!([{"local":"10.42.0.1", "prefixlen":24}]),
                true,
            ),
        ] {
            assert_eq!(
                delete
                    .deletion_is_absent_with(|_| Ok(listing(addresses.clone())))
                    .unwrap(),
                absent
            );
        }
        for invalid in [
            b"[]".as_slice(),
            b"null",
            b"not-json",
            br#"[{"ifname":"pv1","addr_info":[]}]"#,
            br#"[{"ifname":"pv0"}]"#,
        ] {
            assert!(
                delete
                    .deletion_is_absent_with(|_| Ok(invalid.to_vec()))
                    .is_err()
            );
        }
        assert!(
            delete
                .deletion_is_absent_with(|_| Err(io::Error::from(io::ErrorKind::PermissionDenied)))
                .is_err()
        );
        assert!(
            !IpCommand::addr_replace("pv0".to_owned(), prefix)
                .deletion_is_absent_with(|_| panic!("replace must not query deletion state"))
                .unwrap()
        );
        for (routes, absent) in [
            (b"[]".as_slice(), true),
            (br#"[{"dst":"10.42.0.1"}]"#, false),
        ] {
            let mut queries = Vec::new();
            assert_eq!(
                IpCommand::route_delete("pv0".to_owned(), prefix)
                    .deletion_is_absent_with(|args| {
                        queries.push(args.to_vec());
                        Ok(if queries.len() == 1 {
                            listing(serde_json::json!([]))
                        } else {
                            routes.to_vec()
                        })
                    })
                    .unwrap(),
                absent
            );
            assert_eq!(
                queries[1],
                [
                    "-j",
                    "route",
                    "show",
                    "exact",
                    "10.42.0.1/32",
                    "dev",
                    "pv0",
                    "metric",
                    "3000"
                ]
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires an isolated Linux user/network namespace and iproute2"]
    fn cleanup_absence_checks_match_kernel_state() {
        const PARENT_NAMESPACE: &str = "P2P_VPN_CLEANUP_PARENT_NETNS";
        let namespace = fs::read_link("/proc/self/ns/net").unwrap();
        let Some(parent_namespace) = std::env::var_os(PARENT_NAMESPACE) else {
            let status = Command::new("unshare")
                .args(["--user", "--map-root-user", "--net"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "runtime::tun::tests::cleanup_absence_checks_match_kernel_state",
                    "--nocapture",
                ])
                .env(PARENT_NAMESPACE, namespace)
                .status()
                .unwrap();
            assert!(status.success(), "isolated cleanup test failed");
            return;
        };
        assert_ne!(
            namespace,
            PathBuf::from(parent_namespace),
            "cleanup test must not mutate its parent's network namespace"
        );
        for args in [
            vec!["link", "add", "pv0", "type", "dummy"],
            vec!["link", "set", "pv0", "up"],
        ] {
            assert!(Command::new("ip").args(args).status().unwrap().success());
        }
        for (address, destination, bits) in
            [("10.42.0.1", "10.42.0.2", 32), ("fd42::1", "fd42::2", 128)]
        {
            let address = IpCidr::new(address.parse().unwrap(), bits).unwrap();
            let destination = IpCidr::new(destination.parse().unwrap(), bits).unwrap();
            let delete_address = IpCommand::addr_delete("pv0".to_owned(), address);
            let delete_route = IpCommand::route_delete("pv0".to_owned(), destination);
            assert!(delete_address.deletion_is_absent().unwrap());
            assert!(delete_route.deletion_is_absent().unwrap());
            assert!(
                IpCommand::addr_replace("pv0".to_owned(), address)
                    .execute()
                    .unwrap()
                    .success()
            );
            assert!(!delete_address.deletion_is_absent().unwrap());
            if bits == 128 {
                assert!(
                    Command::new("ip")
                        .args([
                            "-6",
                            "addr",
                            "replace",
                            &address.to_string(),
                            "dev",
                            "pv0",
                            "nodad"
                        ])
                        .status()
                        .unwrap()
                        .success()
                );
            }
            assert!(
                IpCommand::route_replace("pv0".to_owned(), destination, address.address(), 1280)
                    .execute()
                    .unwrap()
                    .success()
            );
            assert!(!delete_route.deletion_is_absent().unwrap());
            assert!(delete_route.execute().unwrap().success());
            assert!(delete_route.deletion_is_absent().unwrap());
            assert!(!delete_route.execute().unwrap().success());
            assert!(delete_route.deletion_is_absent().unwrap());
            assert!(delete_address.execute().unwrap().success());
            assert!(delete_address.deletion_is_absent().unwrap());
            assert!(!delete_address.execute().unwrap().success());
            assert!(delete_address.deletion_is_absent().unwrap());
        }
        assert!(
            Command::new("ip")
                .args(["link", "del", "pv0"])
                .status()
                .unwrap()
                .success()
        );
        let prefix = IpCidr::new("10.42.0.1".parse().unwrap(), 32).unwrap();
        assert!(
            IpCommand::addr_delete("pv0".to_owned(), prefix)
                .deletion_is_absent()
                .is_err()
        );
        assert!(
            IpCommand::route_delete("pv0".to_owned(), prefix)
                .deletion_is_absent()
                .is_err()
        );
    }

    impl PacketRead for TestPacketReader {
        fn read_packet(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let length = self.0.len();
            buffer[..length].copy_from_slice(&self.0);
            Ok(length)
        }
    }

    struct TestPacketWriter(Arc<Mutex<Vec<u8>>>);

    impl PacketWrite for TestPacketWriter {
        fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("packet writer lock").extend(packet);
            Ok(packet.len())
        }
    }

    struct InvalidLengthReader;

    impl PacketRead for InvalidLengthReader {
        fn read_packet(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            Ok(buffer.len() + 1)
        }
    }

    struct ShortPacketWriter;

    impl PacketWrite for ShortPacketWriter {
        fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
            Ok(packet.len().saturating_sub(1))
        }
    }

    #[test]
    fn packet_io_adapts_platform_readers_and_writers() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let packet_io = PacketIo::new(
            TestPacketReader(vec![1, 2, 3]),
            TestPacketWriter(Arc::clone(&written)),
        );
        let (mut reader, mut writer) = packet_io.split();
        let mut buffer = [0_u8; 8];

        assert_eq!(reader.read_packet(&mut buffer).expect("read packet"), 3);
        assert_eq!(&buffer[..3], &[1, 2, 3]);
        assert_eq!(writer.write_packet(&[4, 5]).expect("write packet"), 2);
        assert_eq!(*written.lock().expect("written packet lock"), [4, 5]);
    }

    #[test]
    fn packet_io_rejects_invalid_adapter_lengths() {
        let packet_io = PacketIo::new(InvalidLengthReader, ShortPacketWriter);
        let (mut reader, mut writer) = packet_io.split();

        let read_error = reader
            .read_packet(&mut [0_u8; 1])
            .expect_err("oversized read must fail");
        assert!(
            matches!(read_error, TunRuntimeError::Io(error) if error.kind() == io::ErrorKind::InvalidData)
        );

        let write_error = writer
            .write_packet(&[1])
            .expect_err("short packet write must fail");
        assert!(
            matches!(write_error, TunRuntimeError::Io(error) if error.kind() == io::ErrorKind::WriteZero)
        );
    }

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
    fn additive_route_update_has_inverse_commands_in_safe_order() {
        let local = PeerId::from_bytes([1; 32]);
        let remote = PeerId::from_bytes([2; 32]);
        let current = TunRuntimeConfig {
            name: "pv0".to_owned(),
            mtu: 1280,
            addresses: TunAddresses::for_peer(local),
            additional_addresses: Vec::new(),
            routes: Vec::new(),
        };
        let address =
            IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 1)), 32).expect("host address");
        let route = Route {
            owner: remote,
            prefix: IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 2)), 32).expect("host route"),
            metric: 0,
        };
        let next = TunRuntimeConfig {
            additional_addresses: vec![address],
            routes: vec![route],
            ..current.clone()
        };

        let update = next
            .additive_update_from(&current)
            .expect("additive update");
        assert_eq!(
            update
                .apply_commands()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "ip addr replace 10.42.0.1/32 dev pv0",
                "ip route replace 10.42.0.2/32 dev pv0 src 10.42.0.1 metric 3000 mtu 1280 advmss 1240",
            ]
        );
        assert_eq!(
            update
                .rollback_commands()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "ip addr del 10.42.0.1/32 dev pv0",
                "ip route del 10.42.0.2/32 dev pv0 metric 3000",
            ]
        );
    }

    #[test]
    fn additive_route_update_rejects_removal_or_runtime_identity_change() {
        let local = PeerId::from_bytes([1; 32]);
        let address =
            IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 1)), 32).expect("host address");
        let current = TunRuntimeConfig {
            name: "pv0".to_owned(),
            mtu: 1280,
            addresses: TunAddresses::for_peer(local),
            additional_addresses: vec![address],
            routes: Vec::new(),
        };
        let removed = TunRuntimeConfig {
            additional_addresses: Vec::new(),
            ..current.clone()
        };
        let renamed = TunRuntimeConfig {
            name: "pv1".to_owned(),
            ..current.clone()
        };

        assert!(matches!(
            removed.additive_update_from(&current),
            Err(TunRuntimeError::NonAdditiveUpdate(_))
        ));
        assert!(matches!(
            renamed.additive_update_from(&current),
            Err(TunRuntimeError::NonAdditiveUpdate(_))
        ));
        assert!(removed.pairing_reconciliation_from(&current).is_err());
        assert!(renamed.pairing_reconciliation_from(&current).is_err());
    }

    #[test]
    fn pairing_reconciliation_updates_sources_and_withdraws_routes_with_inverses() {
        let local = PeerId::from_bytes([1; 32]);
        let remote = PeerId::from_bytes([2; 32]);
        let retained = Route {
            owner: remote,
            prefix: IpCidr::new("10.42.0.2".parse().unwrap(), 32).unwrap(),
            metric: 0,
        };
        let expired = Route {
            prefix: IpCidr::new("10.88.0.0".parse().unwrap(), 24).unwrap(),
            ..retained
        };
        let current = TunRuntimeConfig {
            name: "pv0".to_owned(),
            mtu: 1280,
            addresses: TunAddresses::for_peer(local),
            additional_addresses: Vec::new(),
            routes: vec![retained, expired],
        };
        let address = IpCidr::new("10.42.0.1".parse().unwrap(), 32).unwrap();
        let next = TunRuntimeConfig {
            additional_addresses: vec![address],
            routes: vec![retained],
            ..current.clone()
        };
        let update = next.pairing_reconciliation_from(&current).unwrap();
        assert_eq!(
            update.apply_commands(),
            &[
                IpCommand::addr_replace("pv0".to_owned(), address),
                IpCommand::route_replace(
                    "pv0".to_owned(),
                    retained.prefix,
                    address.address(),
                    1280
                ),
                IpCommand::route_delete("pv0".to_owned(), expired.prefix),
            ]
        );
        assert_eq!(
            update.rollback_commands(),
            &[
                IpCommand::addr_delete("pv0".to_owned(), address),
                IpCommand::route_replace(
                    "pv0".to_owned(),
                    retained.prefix,
                    builtin_ipv4(local).into(),
                    1280
                ),
                IpCommand::route_replace(
                    "pv0".to_owned(),
                    expired.prefix,
                    builtin_ipv4(local).into(),
                    1280
                ),
            ]
        );
        assert!(
            next.pairing_reconciliation_from(&next)
                .unwrap()
                .apply_commands()
                .is_empty()
        );
        for incompatible in [
            TunRuntimeConfig {
                mtu: 1400,
                ..next.clone()
            },
            TunRuntimeConfig {
                addresses: TunAddresses {
                    ipv4: Ipv4Addr::new(100, 64, 1, 1),
                    ..current.addresses
                },
                ..next.clone()
            },
        ] {
            assert!(incompatible.pairing_reconciliation_from(&current).is_err());
        }
    }

    #[test]
    fn pairing_abort_restores_route_sources_before_removing_addresses() {
        for (alias, retained_prefix, added_prefix, host_len, subnet_len) in [
            ("10.42.0.1", "10.42.0.2", "10.43.0.0", 32, 24),
            ("fd42::1", "fd42::2", "fd43::", 128, 64),
        ] {
            let local = PeerId::from_bytes([1; 32]);
            let retained = Route {
                owner: PeerId::from_bytes([2; 32]),
                prefix: IpCidr::new(retained_prefix.parse().unwrap(), host_len).unwrap(),
                metric: 0,
            };
            let added = Route {
                prefix: IpCidr::new(added_prefix.parse().unwrap(), subnet_len).unwrap(),
                ..retained
            };
            let target = TunRuntimeConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
                addresses: TunAddresses::for_peer(local),
                additional_addresses: Vec::new(),
                routes: vec![retained],
            };
            let alias = IpCidr::new(alias.parse().unwrap(), host_len).unwrap();
            let attempted = TunRuntimeConfig {
                additional_addresses: vec![alias],
                routes: vec![retained, added],
                ..target.clone()
            };
            assert_eq!(
                target.pairing_abort_commands_from(&attempted).unwrap(),
                vec![
                    IpCommand::route_replace(
                        "pv0".to_owned(),
                        retained.prefix,
                        target.route_source(retained.prefix),
                        1280
                    ),
                    IpCommand::route_delete("pv0".to_owned(), added.prefix),
                    IpCommand::addr_delete("pv0".to_owned(), alias),
                ]
            );
            assert_eq!(
                target.pairing_abort_commands_from(&target).unwrap(),
                vec![IpCommand::route_replace(
                    "pv0".to_owned(),
                    retained.prefix,
                    target.route_source(retained.prefix),
                    1280
                ),]
            );

            let survivor = TunRuntimeConfig {
                additional_addresses: vec![alias],
                ..target.clone()
            };
            assert_eq!(
                survivor.pairing_abort_commands_from(&attempted).unwrap(),
                vec![
                    IpCommand::addr_replace("pv0".to_owned(), alias),
                    IpCommand::route_replace(
                        "pv0".to_owned(),
                        retained.prefix,
                        alias.address(),
                        1280
                    ),
                    IpCommand::route_delete("pv0".to_owned(), added.prefix),
                ]
            );
            for incompatible in [
                TunRuntimeConfig {
                    name: "pv1".to_owned(),
                    ..attempted.clone()
                },
                TunRuntimeConfig {
                    mtu: 1400,
                    ..attempted.clone()
                },
                TunRuntimeConfig {
                    addresses: TunAddresses {
                        ipv4: Ipv4Addr::new(100, 64, 1, 1),
                        ..attempted.addresses
                    },
                    ..attempted.clone()
                },
            ] {
                assert!(target.pairing_abort_commands_from(&incompatible).is_err());
            }
        }
    }

    #[test]
    fn route_reconciliation_adds_and_removes_routes_with_matching_rollback() {
        let local = PeerId::from_bytes([1; 32]);
        let first_remote = PeerId::from_bytes([2; 32]);
        let second_remote = PeerId::from_bytes([3; 32]);
        let first_route = Route {
            owner: first_remote,
            prefix: IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 2)), 32).expect("first route"),
            metric: 0,
        };
        let second_route = Route {
            owner: second_remote,
            prefix: IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 3)), 32).expect("second route"),
            metric: 0,
        };
        let current = TunRuntimeConfig {
            name: "pv0".to_owned(),
            mtu: 1280,
            addresses: TunAddresses::for_peer(local),
            additional_addresses: Vec::new(),
            routes: vec![first_route],
        };
        let next = TunRuntimeConfig {
            routes: vec![second_route],
            ..current.clone()
        };
        let source = builtin_ipv4(local);

        let update = next
            .route_reconciliation_from(&current)
            .expect("route reconciliation");

        assert_eq!(
            update
                .apply_commands()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                format!(
                    "ip route replace 10.42.0.3/32 dev pv0 src {source} metric 3000 mtu 1280 advmss 1240"
                ),
                "ip route del 10.42.0.2/32 dev pv0 metric 3000".to_owned(),
            ]
        );
        assert_eq!(
            update
                .rollback_commands()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "ip route del 10.42.0.3/32 dev pv0 metric 3000".to_owned(),
                format!(
                    "ip route replace 10.42.0.2/32 dev pv0 src {source} metric 3000 mtu 1280 advmss 1240"
                ),
            ]
        );
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
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: local.to_string(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
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
        assert!(runtime.additional_addresses.is_empty());
    }

    #[test]
    fn runtime_config_uses_effective_packet_mtu() {
        let config = Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: peer_hex(1),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
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
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: peer_hex(1),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: vec![RouteConfig {
                    prefix: "10.41.0.0/24".to_owned(),
                    metric: 100,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: Some("node-b".to_owned()),
                ip: None,
                vpn_ip: None,
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
    fn runtime_config_assigns_local_host_routes_as_addresses() {
        let config = Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: peer_hex(1),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: vec![
                    RouteConfig {
                        prefix: "10.44.0.1/32".to_owned(),
                        metric: 0,
                    },
                    RouteConfig {
                        prefix: "fd00::44/128".to_owned(),
                        metric: 0,
                    },
                    RouteConfig {
                        prefix: "10.45.0.0/24".to_owned(),
                        metric: 0,
                    },
                ],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
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
        let commands = runtime
            .route_commands()
            .into_iter()
            .map(|command| command.to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            runtime.additional_addresses,
            vec![
                IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 44, 0, 1)), 32).expect("IPv4 host"),
                IpCidr::new("fd00::44".parse().expect("IPv6 host"), 128).expect("IPv6 host"),
            ]
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "ip addr replace 10.44.0.1/32 dev hs0")
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "ip -6 addr replace fd00::44/128 dev hs0 nodad")
        );
        assert!(
            !commands
                .iter()
                .any(|command| command == "ip addr replace 10.45.0.0/24 dev hs0")
        );
    }

    #[test]
    fn runtime_config_assigns_local_vpn_ip_as_address_and_route_source() {
        let config = Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: peer_hex(1),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: Some("10.44.0.1".to_owned()),
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: PeerId::from_bytes([
                    2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0,
                ])
                .to_string(),
                name: Some("node-b".to_owned()),
                ip: None,
                vpn_ip: Some("10.44.0.2".to_owned()),
                addresses: Vec::new(),
                routes: Vec::new(),
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

        assert_eq!(
            runtime.additional_addresses,
            vec![IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 44, 0, 1)), 32).expect("IPv4 host")]
        );
        assert!(
            commands
                .iter()
                .any(|command| command == "ip addr replace 10.44.0.1/32 dev pv0")
        );
        assert!(commands.iter().any(|command| command
            == "ip route replace 10.44.0.2/32 dev pv0 src 10.44.0.1 metric 3000 mtu 1280 advmss 1240"));
    }

    #[test]
    fn runtime_config_installs_live_member_record_routes() {
        let inviter = NodeIdentity::generate_ed25519().expect("inviter identity");
        let joiner = NodeIdentity::generate_ed25519().expect("joiner identity");
        let config = Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: inviter.peer_id.clone(),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "pv0".to_owned(),
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
        let member_record = issue_membership_record_for_subject_at(
            &inviter,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&joiner).expect("joiner subject"),
                membership_epoch: 1,
                sequence: 100,
                revoked: false,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: vec![RouteConfig {
                    prefix: "10.77.0.0/24".to_owned(),
                    metric: 50,
                }],
                expires_at_unix_seconds: None,
            },
            100,
        )
        .expect("member record");

        let runtime = TunRuntimeConfig::from_config_with_member_records(&config, &[member_record])
            .expect("runtime config");
        let commands = runtime
            .route_commands()
            .into_iter()
            .map(|command| command.to_string())
            .collect::<Vec<_>>();
        let joiner_builtin = builtin_ipv4(joiner.peer_id.parse().expect("joiner peer"));

        assert!(commands.iter().any(|command| command
            == &format!(
                "ip route replace {joiner_builtin}/32 dev pv0 src {} metric 3000 mtu 1280 advmss 1240",
                runtime.addresses.ipv4
            )));
        assert!(commands.iter().any(|command| command
            == &format!(
                "ip route replace 10.77.0.0/24 dev pv0 src {} metric 3000 mtu 1280 advmss 1240",
                runtime.addresses.ipv4
            )));
    }

    #[test]
    fn sysctl_commands_prepare_tun_for_overlay_host_routes() {
        let runtime = TunRuntimeConfig {
            name: "pv0".to_owned(),
            mtu: 1280,
            addresses: TunAddresses {
                ipv4: Ipv4Addr::new(100, 64, 0, 1),
                ipv6: "fd00::1".parse().expect("IPv6 address"),
            },
            additional_addresses: Vec::new(),
            routes: Vec::new(),
        };

        let commands = runtime
            .sysctl_commands()
            .into_iter()
            .map(|command| command.to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            commands,
            vec![
                "sysctl -w net.ipv4.conf.pv0.rp_filter=0",
                "sysctl -w net.ipv4.conf.pv0.accept_local=1",
            ]
        );
    }

    #[test]
    fn route_commands_install_ipv6_address_and_peer_routes() {
        let config = Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: peer_hex(1),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
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
                ip: None,
                vpn_ip: None,
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
        assert!(commands.iter().any(|command| *command
            == format!(
                "ip route replace 10.42.0.0/24 dev hs0 src {} metric 3000 mtu 1280 advmss 1240",
                runtime.addresses.ipv4
            )));
        assert!(runtime.routes.iter().any(|route| {
            route
                .prefix
                .contains(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 10)))
        }));
    }

    #[test]
    fn route_commands_prefer_configured_local_host_route_as_source() {
        let config = Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer: peer_hex(1),
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: vec![RouteConfig {
                    prefix: "10.42.0.1/32".to_owned(),
                    metric: 100,
                }],
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "pv0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: PeerId::from_bytes([
                    2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0,
                ])
                .to_string(),
                name: Some("node-b".to_owned()),
                ip: None,
                vpn_ip: None,
                addresses: Vec::new(),
                routes: vec![RouteConfig {
                    prefix: "10.42.0.2/32".to_owned(),
                    metric: 100,
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

        assert!(commands.iter().any(|command| command
            == "ip route replace 10.42.0.2/32 dev pv0 src 10.42.0.1 metric 3000 mtu 1280 advmss 1240"));
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
            "fd00::1".parse().expect("IPv6 source"),
            1280,
        );

        assert_eq!(
            command.to_string(),
            "ip -6 route replace fd00:6879:7072:7370:6163:6500:4200:0/112 dev hs0 src fd00::1 metric 3000 mtu 1280 advmss 1220"
        );
    }

    #[test]
    fn route_commands_omit_mss_hint_when_mtu_is_too_small() {
        let command = IpCommand::route_replace(
            "hs0".to_owned(),
            IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 42, 0, 0)), 24).expect("IPv4 CIDR"),
            "10.42.0.1".parse().expect("IPv4 source"),
            39,
        );

        assert_eq!(
            command.to_string(),
            "ip route replace 10.42.0.0/24 dev hs0 src 10.42.0.1 metric 3000 mtu 39"
        );
    }
}
