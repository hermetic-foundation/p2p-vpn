use super::{PacketIo, PacketRead, PacketWrite, TunRuntimeConfig, TunRuntimeError};
use std::{io, sync::Arc};

pub struct TunDevice {
    device: Arc<tun_rs::SyncDevice>,
    name: String,
}

impl TunDevice {
    pub fn open(config: &TunRuntimeConfig) -> Result<Self, TunRuntimeError> {
        // Never search the current working directory for an elevated DLL load.
        let dll = std::env::current_exe()?.with_file_name("wintun.dll");
        if !dll.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "put the official architecture-matched wintun.dll beside the executable: {}",
                    dll.display()
                ),
            )
            .into());
        }
        let device = tun_rs::DeviceBuilder::new()
            .name(&config.name)
            .mtu(config.mtu)
            .wintun_file(dll.to_string_lossy().into_owned())
            .build_sync()?;
        Ok(Self {
            device: Arc::new(device),
            name: config.name.clone(),
        })
    }

    pub fn name(&self) -> Result<String, TunRuntimeError> {
        Ok(self.name.clone())
    }

    #[must_use]
    pub fn into_packet_io(self) -> PacketIo {
        PacketIo::new(Reader(Arc::clone(&self.device)), Writer(self.device))
    }
}

struct Reader(Arc<tun_rs::SyncDevice>);
struct Writer(Arc<tun_rs::SyncDevice>);

impl PacketRead for Reader {
    fn read_packet(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.recv(buffer)
    }
    fn cancellation(&self) -> Option<Box<dyn FnOnce() + Send>> {
        let device = Arc::clone(&self.0);
        Some(Box::new(move || {
            let _ = device.shutdown();
        }))
    }
}

impl PacketWrite for Writer {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
        self.0.send(packet)
    }
}

impl super::IpCommand {
    fn windows_parts(&self) -> (&str, &str, &str, &str) {
        let args = self
            .args
            .strip_prefix(&["-6".to_owned()])
            .unwrap_or(&self.args);
        (&args[0], &args[1], &args[2], &args[4])
    }

    pub(super) fn execute_windows(&self) -> io::Result<std::process::ExitStatus> {
        let (kind, operation, prefix, interface) = self.windows_parts();
        let script = if kind == "addr" {
            r"
                $ip,$bits=$env:P2PVPN_PREFIX.Split('/');
                $existing=@(Get-NetIPAddress -InterfaceIndex $index | Where-Object IPAddress -eq $ip);
                if($env:P2PVPN_OPERATION -eq 'del') {
                    $existing | Where-Object PrefixLength -eq ([int]$bits) | Remove-NetIPAddress -Confirm:$false;
                } elseif($existing.Count -eq 0) {
                    New-NetIPAddress -InterfaceIndex $index -IPAddress $ip -PrefixLength ([int]$bits) -PolicyStore ActiveStore | Out-Null;
                } elseif(@($existing | Where-Object PrefixLength -ne ([int]$bits)).Count -ne 0) {
                    throw 'existing adapter address has a conflicting prefix length';
                }
                if($env:P2PVPN_OPERATION -ne 'del') {
                    $deadline=[DateTime]::UtcNow.AddSeconds(10);
                    do {
                        $ready=@(Get-NetIPAddress -InterfaceIndex $index | Where-Object IPAddress -eq $ip);
                        if(@($ready | Where-Object AddressState -eq 'Duplicate').Count) { throw 'VPN address failed duplicate address detection' }
                        if(@($ready | Where-Object AddressState -eq 'Preferred').Count) { break }
                        Start-Sleep -Milliseconds 100;
                    } while([DateTime]::UtcNow -lt $deadline);
                    if(!@($ready | Where-Object AddressState -eq 'Preferred').Count) { throw 'VPN address did not become usable before timeout' }
                }
            "
        } else {
            r"
                $existing=@(Get-NetRoute -InterfaceIndex $index -PolicyStore ActiveStore | Where-Object DestinationPrefix -eq $env:P2PVPN_PREFIX);
                if($env:P2PVPN_OPERATION -eq 'del') {
                    $existing | Where-Object RouteMetric -eq 3000 | Remove-NetRoute -Confirm:$false;
                } elseif($existing.Count -eq 0) {
                    $hop=if($env:P2PVPN_PREFIX.Contains(':')) { '::' } else { '0.0.0.0' };
                    New-NetRoute -InterfaceIndex $index -DestinationPrefix $env:P2PVPN_PREFIX -NextHop $hop -RouteMetric 3000 -PolicyStore ActiveStore | Out-Null;
                } elseif(@($existing | Where-Object RouteMetric -ne 3000).Count -ne 0) {
                    throw 'existing adapter route is not owned by p2p-vpn';
                }
            "
        };
        let output = crate::private_fs::windows::powershell(
            &format!("{FIND_INTERFACE}\n{script}"),
            &[
                ("P2PVPN_INTERFACE", std::ffi::OsStr::new(interface)),
                ("P2PVPN_PREFIX", std::ffi::OsStr::new(prefix)),
                ("P2PVPN_OPERATION", std::ffi::OsStr::new(operation)),
            ],
        )?;
        Ok(output.status)
    }

    pub(super) fn deletion_is_absent_windows(&self) -> io::Result<bool> {
        let Some(target) = &self.deletion else {
            return Ok(false);
        };
        let script = if target.route {
            "$items=@(Get-NetRoute -InterfaceIndex $index -PolicyStore ActiveStore | Where-Object { $_.DestinationPrefix -eq $env:P2PVPN_PREFIX -and $_.RouteMetric -eq 3000 });"
        } else {
            "$ip,$bits=$env:P2PVPN_PREFIX.Split('/'); $items=@(Get-NetIPAddress -InterfaceIndex $index | Where-Object { $_.IPAddress -eq $ip -and $_.PrefixLength -eq ([int]$bits) });"
        };
        let prefix = target.prefix.to_string();
        let output = crate::private_fs::windows::powershell(
            &format!("{FIND_INTERFACE}\n{script}\n$items.Count"),
            &[
                ("P2PVPN_INTERFACE", std::ffi::OsStr::new(&target.interface)),
                ("P2PVPN_PREFIX", std::ffi::OsStr::new(&prefix)),
            ],
        )?;
        Ok(String::from_utf8_lossy(&output.stdout).trim() == "0")
    }

    pub(super) fn display_windows(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, operation, prefix, interface) = self.windows_parts();
        write!(f, "Windows {kind} {operation} {prefix} on {interface:?}")
    }
}

const FIND_INTERFACE: &str = r"
    $nic=@(Get-NetAdapter -IncludeHidden | Where-Object Name -eq $env:P2PVPN_INTERFACE);
    if($nic.Count -ne 1) { throw 'VPN adapter is missing or ambiguous' }
    $index=$nic[0].ifIndex;
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        PeerId,
        route::IpCidr,
        runtime::tun::{IpCommand, TunAddresses},
    };
    use std::{
        net::{IpAddr, Ipv4Addr, UdpSocket},
        sync::mpsc,
        time::Duration,
    };

    #[test]
    #[ignore = "requires elevation and official wintun.dll beside the test executable"]
    fn wintun_udp_roundtrip_and_blocked_read_cancellation() {
        let config = TunRuntimeConfig {
            name: format!("p2p-test-{}", std::process::id()),
            mtu: 1280,
            addresses: TunAddresses::for_peer(PeerId::from_bytes([193; 32])),
            additional_addresses: vec![],
            routes: vec![],
        };
        let device = TunDevice::open(&config).unwrap();
        for command in config.route_commands() {
            assert!(command.execute().unwrap().success());
        }
        let destination = Ipv4Addr::new(198, 18, 255, 254);
        let route = IpCidr::new(IpAddr::V4(destination), 32).unwrap();
        assert!(
            IpCommand::route_replace(
                config.name.clone(),
                route,
                IpAddr::V4(config.addresses.ipv4),
                config.mtu
            )
            .execute()
            .unwrap()
            .success()
        );
        let (mut reader, mut writer) = device.into_packet_io().split();
        let cancel = reader.cancellation().unwrap();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut buffer = [0_u8; 2048];
            while let Ok(length) = reader.read_packet(&mut buffer) {
                if length == 0 {
                    break;
                }
                if length >= 28
                    && buffer[0] >> 4 == 4
                    && buffer[9] == 17
                    && buffer[16..20] == destination.octets()
                {
                    let _ = tx.send(buffer[..length].to_vec());
                }
            }
        });
        let socket = UdpSocket::bind((config.addresses.ipv4, 0)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .send_to(b"native Windows Wintun", (destination, 43210))
            .unwrap();
        let received = rx.recv_timeout(Duration::from_secs(10));
        if let Ok(mut packet) = received {
            let header = usize::from(packet[0] & 15) * 4;
            for i in 0..4 {
                packet.swap(12 + i, 16 + i);
            }
            packet.swap(header, header + 2);
            packet.swap(header + 1, header + 3);
            packet[header + 6..header + 8].fill(0); // IPv4 UDP checksum is optional.
            packet[10..12].fill(0);
            let checksum = crate::runtime::tun::internet_checksum(&packet[..header]);
            packet[10..12].copy_from_slice(&checksum.to_be_bytes());
            writer.write_packet(&packet).unwrap();
            let mut payload = [0_u8; 128];
            let echoed = socket.recv_from(&mut payload);
            cancel();
            worker.join().unwrap();
            let (length, remote) = echoed.unwrap();
            assert_eq!(&payload[..length], b"native Windows Wintun");
            assert_eq!(remote.ip(), IpAddr::V4(destination));
        } else {
            cancel();
            worker.join().unwrap();
            panic!("Windows did not deliver the outbound UDP packet to Wintun");
        }
        assert!(
            IpCommand::route_delete(config.name, route)
                .execute()
                .unwrap()
                .success()
        );
    }
}
