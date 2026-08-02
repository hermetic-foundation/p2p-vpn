use std::collections::VecDeque;

use crate::{PeerId, Sequence};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Packet {
    peer: PeerId,
    sequence: Sequence,
    bytes: Vec<u8>,
}

impl Packet {
    #[must_use]
    pub fn new(peer: PeerId, sequence: Sequence, bytes: Vec<u8>) -> Self {
        Self {
            peer,
            sequence,
            bytes,
        }
    }

    #[must_use]
    pub const fn peer(&self) -> PeerId {
        self.peer
    }

    #[must_use]
    pub const fn sequence(&self) -> Sequence {
        self.sequence
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueStats {
    pub queued_packets: usize,
    pub queued_bytes: usize,
    pub dropped_packets: u64,
    pub dropped_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnqueueError {
    PacketTooLarge {
        packet_bytes: usize,
        byte_limit: usize,
    },
    QueueFull {
        packet_bytes: usize,
    },
}

#[derive(Debug)]
pub struct PeerQueue {
    max_packets: usize,
    max_bytes: usize,
    stats: QueueStats,
    packets: VecDeque<Packet>,
}

impl PeerQueue {
    #[must_use]
    pub fn new(max_packets: usize, max_bytes: usize) -> Self {
        Self {
            max_packets,
            max_bytes,
            stats: QueueStats::default(),
            packets: VecDeque::new(),
        }
    }

    pub fn enqueue(&mut self, packet: Packet) -> Result<(), EnqueueError> {
        let packet_bytes = packet.len();

        if packet_bytes > self.max_bytes {
            self.record_drop(packet_bytes);
            return Err(EnqueueError::PacketTooLarge {
                packet_bytes,
                byte_limit: self.max_bytes,
            });
        }

        if self.packets.len() >= self.max_packets
            || self.stats.queued_bytes.saturating_add(packet_bytes) > self.max_bytes
        {
            self.record_drop(packet_bytes);
            return Err(EnqueueError::QueueFull { packet_bytes });
        }

        self.stats.queued_packets += 1;
        self.stats.queued_bytes += packet_bytes;
        self.packets.push_back(packet);
        Ok(())
    }

    pub fn dequeue(&mut self) -> Option<Packet> {
        let packet = self.packets.pop_front()?;
        self.stats.queued_packets -= 1;
        self.stats.queued_bytes -= packet.len();
        Some(packet)
    }

    #[must_use]
    pub const fn stats(&self) -> QueueStats {
        self.stats
    }

    fn record_drop(&mut self, packet_bytes: usize) {
        self.stats.dropped_packets += 1;
        self.stats.dropped_bytes += u64::try_from(packet_bytes).unwrap_or(u64::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(seed: u8) -> PeerId {
        PeerId::from_bytes([seed; 32])
    }

    #[test]
    fn queue_tracks_queued_bytes_and_packets() {
        let mut queue = PeerQueue::new(2, 10);

        queue
            .enqueue(Packet::new(peer(1), 7, vec![1, 2, 3]))
            .expect("packet should fit");

        assert_eq!(
            queue.stats(),
            QueueStats {
                queued_packets: 1,
                queued_bytes: 3,
                dropped_packets: 0,
                dropped_bytes: 0
            }
        );

        let packet = queue.dequeue().expect("packet should be queued");
        assert_eq!(packet.sequence(), 7);
        assert_eq!(packet.payload(), &[1, 2, 3]);
        assert_eq!(queue.stats(), QueueStats::default());
    }

    #[test]
    fn queue_drops_when_packet_limit_is_reached() {
        let mut queue = PeerQueue::new(1, 10);

        queue
            .enqueue(Packet::new(peer(1), 1, vec![1]))
            .expect("first packet should fit");

        let error = queue
            .enqueue(Packet::new(peer(1), 2, vec![2]))
            .expect_err("second packet should be dropped");

        assert_eq!(error, EnqueueError::QueueFull { packet_bytes: 1 });
        assert_eq!(queue.stats().dropped_packets, 1);
        assert_eq!(queue.dequeue().expect("first packet remains").sequence(), 1);
    }

    #[test]
    fn queue_drops_oversized_packet() {
        let mut queue = PeerQueue::new(10, 3);

        let error = queue
            .enqueue(Packet::new(peer(1), 1, vec![0; 4]))
            .expect_err("oversized packet should be rejected");

        assert_eq!(
            error,
            EnqueueError::PacketTooLarge {
                packet_bytes: 4,
                byte_limit: 3
            }
        );
        assert_eq!(queue.stats().dropped_bytes, 4);
        assert!(queue.dequeue().is_none());
    }
}
