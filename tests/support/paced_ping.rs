use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    io,
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};
use tokio::{
    net::UdpSocket,
    time::{Instant, sleep_until},
};

#[derive(Clone, Copy)]
pub struct Settings {
    pub rate: u32,
    pub seconds: u32,
    pub payload_bytes: usize,
    pub preload: u32,
}

impl Settings {
    fn count(self) -> io::Result<u32> {
        let count = self
            .rate
            .checked_mul(self.seconds)
            .ok_or_else(|| io::Error::other("packet count overflow"))?;
        if count == 0
            || count > u32::from(u16::MAX)
            || self.preload == 0
            || self.preload > count
            || !(8..=1400).contains(&self.payload_bytes)
        {
            return Err(io::Error::other("invalid paced ping settings"));
        }
        Ok(count)
    }

    fn offset(self, sequence: u32) -> Duration {
        let slot = sequence.saturating_add(1).saturating_sub(self.preload);
        Duration::from_nanos(u64::from(slot) * 1_000_000_000 / u64::from(self.rate))
    }

    fn skip_overdue(self, mut next: u32, count: u32, elapsed: Duration) -> u32 {
        while next + self.preload < count && self.offset(next + self.preload) <= elapsed {
            next += 1;
        }
        next
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Report {
    pub sent: u64,
    pub received: u64,
    pub skipped_slots: u64,
    pub duplicate_replies: u64,
    pub invalid_replies: u64,
    pub elapsed_seconds: f64,
    pub maximum_lateness_seconds: f64,
}

pub async fn run(
    source: Ipv4Addr,
    destination: Ipv4Addr,
    settings: Settings,
    interface: Option<&str>,
) -> io::Result<Report> {
    let count = settings.count()?;
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4))?;
    if let Some(interface) = interface {
        socket.bind_device(Some(interface.as_bytes()))?;
    }
    socket.bind(&SocketAddrV4::new(source, 0).into())?;
    socket.connect(&SocketAddrV4::new(destination, 0).into())?;
    socket.set_nonblocking(true)?;
    // Linux ping sockets preserve datagram boundaries and supply the ICMP
    // checksum/identifier. Tokio's datagram readiness also works for this fd.
    let socket = UdpSocket::from_std(socket.into())?;
    let mut sent = vec![false; count as usize];
    let mut received = vec![false; count as usize];
    let mut frame = vec![0x5a; settings.payload_bytes + 8];
    frame[..8].fill(0);
    frame[0] = 8;
    let mut buffer = vec![0; frame.len() + 1];
    let mut report = Report {
        sent: 0,
        received: 0,
        skipped_slots: 0,
        duplicate_replies: 0,
        invalid_replies: 0,
        elapsed_seconds: 0.0,
        maximum_lateness_seconds: 0.0,
    };
    let start = Instant::now();
    let end = start + Duration::from_secs(u64::from(settings.seconds));
    let mut next = 0_u32;
    loop {
        let target = if next < count {
            start + settings.offset(next)
        } else {
            end
        };
        tokio::select! {
            biased;
            () = sleep_until(end) => break,
            () = sleep_until(target), if next < count => {
                let now = Instant::now();
                // Never catch up an arbitrarily long pause with a packet burst.
                // Keep at most the declared preload's worth of overdue slots.
                let resumed = settings.skip_overdue(next, count, now.duration_since(start));
                report.skipped_slots += u64::from(resumed - next);
                next = resumed;
                let sequence = u16::try_from(next).unwrap();
                frame[6..8].copy_from_slice(&sequence.to_be_bytes());
                frame[8..16].copy_from_slice(&u64::from(next).to_be_bytes());
                report.maximum_lateness_seconds = report.maximum_lateness_seconds.max(now.saturating_duration_since(start + settings.offset(next)).as_secs_f64());
                let length = match tokio::time::timeout_at(end, socket.send(&frame)).await {
                    Ok(result) => result?,
                    Err(_) => break,
                };
                if length != frame.len() { return Err(io::Error::other("short ICMP datagram write")); }
                sent[next as usize] = true;
                report.sent += 1;
                next += 1;
            }
            length = socket.recv(&mut buffer) => {
                let length = length?;
                if length != frame.len() || buffer[0] != 0 || buffer[1] != 0 {
                    report.invalid_replies += 1;
                    continue;
                }
                let sequence = usize::from(u16::from_be_bytes([buffer[6], buffer[7]]));
                if sequence >= sent.len() || !sent[sequence]
                    || buffer[8..16] != (sequence as u64).to_be_bytes()
                    || buffer[16..length].iter().any(|byte| *byte != 0x5a) {
                    report.invalid_replies += 1;
                } else if received[sequence] {
                    report.duplicate_replies += 1;
                } else {
                    received[sequence] = true;
                    report.received += 1;
                }
            }
        }
    }
    report.elapsed_seconds = start.elapsed().as_secs_f64();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadlines_are_absolute_and_packet_counts_are_bounded() {
        let settings = Settings {
            rate: 50,
            seconds: 180,
            payload_bytes: 512,
            preload: 3,
        };
        assert_eq!(settings.count().unwrap(), 9000);
        assert_eq!(settings.offset(0), Duration::ZERO);
        assert_eq!(settings.offset(2), Duration::ZERO);
        assert_eq!(settings.offset(3), Duration::from_millis(20));
        assert_eq!(settings.offset(8999), Duration::from_millis(179940));
        assert_eq!(
            settings.skip_overdue(0, 9000, Duration::from_millis(500)),
            25
        );
        assert!(settings.offset(28) > Duration::from_millis(500));
        assert!(
            Settings {
                rate: 0,
                ..settings
            }
            .count()
            .is_err()
        );
        assert!(
            Settings {
                seconds: u32::MAX,
                ..settings
            }
            .count()
            .is_err()
        );
        assert!(
            Settings {
                payload_bytes: 7,
                ..settings
            }
            .count()
            .is_err()
        );
    }
}
