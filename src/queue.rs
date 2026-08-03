use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use crate::{PeerId, Sequence};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Packet {
    peer: PeerId,
    sequence: Sequence,
    bytes: Vec<u8>,
    enqueued_at: Instant,
}

impl Packet {
    #[must_use]
    pub fn new(peer: PeerId, sequence: Sequence, bytes: Vec<u8>) -> Self {
        Self {
            peer,
            sequence,
            bytes,
            enqueued_at: Instant::now(),
        }
    }

    #[must_use]
    pub fn new_at(peer: PeerId, sequence: Sequence, bytes: Vec<u8>, enqueued_at: Instant) -> Self {
        Self {
            peer,
            sequence,
            bytes,
            enqueued_at,
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

    #[must_use]
    pub const fn enqueued_at(&self) -> Instant {
        self.enqueued_at
    }

    #[must_use]
    pub fn is_expired(&self, now: Instant, max_age: Duration) -> bool {
        now.saturating_duration_since(self.enqueued_at) > max_age
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueStats {
    pub queued_packets: usize,
    pub queued_bytes: usize,
    pub oldest_packet_age_millis: u64,
    pub dropped_packets: u64,
    pub dropped_bytes: u64,
    pub expired_packets: u64,
    pub expired_bytes: u64,
}

impl QueueStats {
    #[must_use]
    pub const fn add(self, other: Self) -> Self {
        Self {
            queued_packets: self.queued_packets + other.queued_packets,
            queued_bytes: self.queued_bytes + other.queued_bytes,
            oldest_packet_age_millis: if self.oldest_packet_age_millis
                > other.oldest_packet_age_millis
            {
                self.oldest_packet_age_millis
            } else {
                other.oldest_packet_age_millis
            },
            dropped_packets: self.dropped_packets + other.dropped_packets,
            dropped_bytes: self.dropped_bytes + other.dropped_bytes,
            expired_packets: self.expired_packets + other.expired_packets,
            expired_bytes: self.expired_bytes + other.expired_bytes,
        }
    }
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
    max_packet_age: Duration,
    stats: QueueStats,
    packets: VecDeque<Packet>,
}

impl PeerQueue {
    #[must_use]
    pub fn new(max_packets: usize, max_bytes: usize) -> Self {
        Self::with_packet_ttl(max_packets, max_bytes, Duration::from_secs(1))
    }

    #[must_use]
    pub fn with_packet_ttl(max_packets: usize, max_bytes: usize, max_packet_age: Duration) -> Self {
        Self {
            max_packets,
            max_bytes,
            max_packet_age,
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

    pub fn drop_expired(&mut self, now: Instant) {
        let Some(first_fresh) = self
            .packets
            .iter()
            .position(|packet| !packet.is_expired(now, self.max_packet_age))
        else {
            while let Some(packet) = self.dequeue() {
                self.record_expire(packet.len());
            }
            return;
        };

        for _ in 0..first_fresh {
            if let Some(packet) = self.dequeue() {
                self.record_expire(packet.len());
            }
        }
    }

    #[must_use]
    pub fn stats(&self) -> QueueStats {
        self.stats_at(Instant::now())
    }

    #[must_use]
    pub fn stats_at(&self, now: Instant) -> QueueStats {
        QueueStats {
            oldest_packet_age_millis: self
                .packets
                .front()
                .map_or(0, |packet| duration_millis(now, packet.enqueued_at())),
            ..self.stats
        }
    }

    fn record_drop(&mut self, packet_bytes: usize) {
        self.stats.dropped_packets += 1;
        self.stats.dropped_bytes += u64::try_from(packet_bytes).unwrap_or(u64::MAX);
    }

    fn record_expire(&mut self, packet_bytes: usize) {
        self.record_drop(packet_bytes);
        self.stats.expired_packets += 1;
        self.stats.expired_bytes += u64::try_from(packet_bytes).unwrap_or(u64::MAX);
    }
}

#[derive(Debug)]
pub struct PeerQueues {
    max_packets_per_peer: usize,
    max_bytes_per_peer: usize,
    max_packet_age: Duration,
    queues: HashMap<PeerId, PeerQueue>,
    ready: VecDeque<PeerId>,
}

impl PeerQueues {
    #[must_use]
    pub fn new(max_packets_per_peer: usize, max_bytes_per_peer: usize) -> Self {
        Self::with_packet_ttl(
            max_packets_per_peer,
            max_bytes_per_peer,
            Duration::from_secs(1),
        )
    }

    #[must_use]
    pub fn with_packet_ttl(
        max_packets_per_peer: usize,
        max_bytes_per_peer: usize,
        max_packet_age: Duration,
    ) -> Self {
        Self {
            max_packets_per_peer,
            max_bytes_per_peer,
            max_packet_age,
            queues: HashMap::new(),
            ready: VecDeque::new(),
        }
    }

    pub fn enqueue(&mut self, packet: Packet) -> Result<(), EnqueueError> {
        let peer = packet.peer();
        let was_empty = self
            .queues
            .get(&peer)
            .is_none_or(|queue| queue.stats().queued_packets == 0);
        let queue = self.queue_mut(peer);
        queue.enqueue(packet)?;
        if was_empty {
            self.ready.push_back(peer);
        }

        Ok(())
    }

    pub fn dequeue(&mut self) -> Option<Packet> {
        let peer = self.ready.pop_front()?;
        let queue = self.queues.get_mut(&peer)?;
        let packet = queue.dequeue()?;
        if queue.stats().queued_packets > 0 {
            self.ready.push_back(peer);
        }

        Some(packet)
    }

    pub fn dequeue_ready(&mut self, mut is_ready: impl FnMut(PeerId) -> bool) -> Option<Packet> {
        let ready_len = self.ready.len();
        for _ in 0..ready_len {
            let peer = self.ready.pop_front()?;
            if !is_ready(peer) {
                self.ready.push_back(peer);
                continue;
            }

            let Some(queue) = self.queues.get_mut(&peer) else {
                continue;
            };
            let Some(packet) = queue.dequeue() else {
                continue;
            };
            if queue.stats().queued_packets > 0 {
                self.ready.push_back(peer);
            }

            return Some(packet);
        }

        None
    }

    pub fn drop_expired(&mut self, now: Instant) {
        let ready_len = self.ready.len();
        for _ in 0..ready_len {
            let Some(peer) = self.ready.pop_front() else {
                return;
            };
            let Some(queue) = self.queues.get_mut(&peer) else {
                continue;
            };
            queue.drop_expired(now);
            if queue.stats().queued_packets > 0 {
                self.ready.push_back(peer);
            }
        }
    }

    #[must_use]
    pub fn peer_stats(&self, peer: PeerId) -> QueueStats {
        self.peer_stats_at(peer, Instant::now())
    }

    #[must_use]
    pub fn peer_stats_at(&self, peer: PeerId, now: Instant) -> QueueStats {
        self.queues
            .get(&peer)
            .map_or_else(QueueStats::default, |queue| queue.stats_at(now))
    }

    #[must_use]
    pub fn total_stats(&self) -> QueueStats {
        self.total_stats_at(Instant::now())
    }

    #[must_use]
    pub fn total_stats_at(&self, now: Instant) -> QueueStats {
        self.queues
            .values()
            .fold(QueueStats::default(), |total, queue| {
                total.add(queue.stats_at(now))
            })
    }

    fn queue_mut(&mut self, peer: PeerId) -> &mut PeerQueue {
        self.queues.entry(peer).or_insert_with(|| {
            PeerQueue::with_packet_ttl(
                self.max_packets_per_peer,
                self.max_bytes_per_peer,
                self.max_packet_age,
            )
        })
    }
}

fn duration_millis(now: Instant, then: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(then).as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(seed: u8) -> PeerId {
        PeerId::from_bytes([seed; 32])
    }

    #[test]
    fn queue_drops_expired_packets() {
        let now = Instant::now();
        let expired_at = now
            .checked_sub(Duration::from_millis(101))
            .expect("test instant should allow subtraction");
        let mut queue = PeerQueue::with_packet_ttl(4, 4096, Duration::from_millis(100));
        queue
            .enqueue(Packet::new_at(peer(1), 1, vec![1, 2, 3], expired_at))
            .expect("expired packet should initially fit");
        queue
            .enqueue(Packet::new_at(peer(1), 2, vec![4, 5], now))
            .expect("fresh packet should fit");

        queue.drop_expired(now);

        assert_eq!(queue.stats().queued_packets, 1);
        assert_eq!(queue.stats().queued_bytes, 2);
        assert_eq!(queue.stats().dropped_packets, 1);
        assert_eq!(queue.stats().dropped_bytes, 3);
        assert_eq!(queue.stats().expired_packets, 1);
        assert_eq!(queue.stats().expired_bytes, 3);
        assert_eq!(queue.dequeue().expect("fresh packet remains").sequence(), 2);
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
                oldest_packet_age_millis: 0,
                dropped_packets: 0,
                dropped_bytes: 0,
                expired_packets: 0,
                expired_bytes: 0
            }
        );

        let packet = queue.dequeue().expect("packet should be queued");
        assert_eq!(packet.sequence(), 7);
        assert_eq!(packet.payload(), &[1, 2, 3]);
        assert_eq!(queue.stats(), QueueStats::default());
    }

    #[test]
    fn queue_stats_report_oldest_packet_age() {
        let now = Instant::now();
        let older = now
            .checked_sub(Duration::from_millis(250))
            .expect("test instant should allow subtraction");
        let newer = now
            .checked_sub(Duration::from_millis(75))
            .expect("test instant should allow subtraction");
        let mut queue = PeerQueue::new(4, 4096);

        queue
            .enqueue(Packet::new_at(peer(1), 1, vec![1], older))
            .expect("older packet");
        queue
            .enqueue(Packet::new_at(peer(1), 2, vec![2], newer))
            .expect("newer packet");

        assert_eq!(queue.stats_at(now).oldest_packet_age_millis, 250);
        queue.dequeue().expect("older packet dequeued");
        assert_eq!(queue.stats_at(now).oldest_packet_age_millis, 75);
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
        assert_eq!(queue.stats().expired_packets, 0);
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
        assert_eq!(queue.stats().expired_bytes, 0);
        assert!(queue.dequeue().is_none());
    }

    #[test]
    fn peer_queues_dequeue_only_ready_peers_without_dropping_blocked_packets() {
        let mut queues = PeerQueues::new(4, 4096);
        queues
            .enqueue(Packet::new(peer(1), 1, vec![1]))
            .expect("peer 1 packet");
        queues
            .enqueue(Packet::new(peer(2), 2, vec![2]))
            .expect("peer 2 packet");

        let packet = queues
            .dequeue_ready(|candidate| candidate == peer(2))
            .expect("ready packet");

        assert_eq!(packet.peer(), peer(2));
        assert_eq!(packet.sequence(), 2);
        assert_eq!(queues.peer_stats(peer(1)).queued_packets, 1);
        assert_eq!(queues.peer_stats(peer(2)).queued_packets, 0);
        assert_eq!(queues.dequeue_ready(|candidate| candidate == peer(2)), None);
        assert_eq!(
            queues
                .dequeue_ready(|candidate| candidate == peer(1))
                .expect("peer 1 remains")
                .sequence(),
            1
        );
    }

    #[test]
    fn peer_queues_drain_fairly_across_peers() {
        let mut queues = PeerQueues::new(8, 1024);

        queues
            .enqueue(Packet::new(peer(1), 1, vec![1]))
            .expect("enqueue");
        queues
            .enqueue(Packet::new(peer(1), 2, vec![2]))
            .expect("enqueue");
        queues
            .enqueue(Packet::new(peer(2), 3, vec![3]))
            .expect("enqueue");

        assert_eq!(queues.dequeue().expect("packet").sequence(), 1);
        assert_eq!(queues.dequeue().expect("packet").sequence(), 3);
        assert_eq!(queues.dequeue().expect("packet").sequence(), 2);
        assert!(queues.dequeue().is_none());
    }

    #[test]
    fn peer_queues_keep_drop_stats_per_peer_and_total() {
        let mut queues = PeerQueues::new(1, 2);

        queues
            .enqueue(Packet::new(peer(1), 1, vec![1]))
            .expect("enqueue");
        assert_eq!(
            queues.enqueue(Packet::new(peer(1), 2, vec![2])),
            Err(EnqueueError::QueueFull { packet_bytes: 1 })
        );
        assert_eq!(
            queues.enqueue(Packet::new(peer(2), 3, vec![0; 3])),
            Err(EnqueueError::PacketTooLarge {
                packet_bytes: 3,
                byte_limit: 2
            })
        );

        assert_eq!(queues.peer_stats(peer(1)).dropped_packets, 1);
        assert_eq!(queues.peer_stats(peer(2)).dropped_bytes, 3);
        assert_eq!(queues.total_stats().expired_packets, 0);
        assert_eq!(queues.total_stats().queued_packets, 1);
        assert_eq!(queues.total_stats().dropped_packets, 2);
    }

    #[test]
    fn peer_queues_total_stats_report_oldest_age_across_peers() {
        let now = Instant::now();
        let older = now
            .checked_sub(Duration::from_millis(500))
            .expect("test instant should allow subtraction");
        let newer = now
            .checked_sub(Duration::from_millis(125))
            .expect("test instant should allow subtraction");
        let mut queues = PeerQueues::new(4, 4096);

        queues
            .enqueue(Packet::new_at(peer(1), 1, vec![1], older))
            .expect("older packet");
        queues
            .enqueue(Packet::new_at(peer(2), 2, vec![2], newer))
            .expect("newer packet");

        assert_eq!(
            queues.peer_stats_at(peer(2), now).oldest_packet_age_millis,
            125
        );
        assert_eq!(queues.total_stats_at(now).oldest_packet_age_millis, 500);
    }

    #[test]
    fn peer_queues_drop_expired_packets_and_keep_fresh_ready() {
        let now = Instant::now();
        let expired_at = now
            .checked_sub(Duration::from_millis(101))
            .expect("test instant should allow subtraction");
        let mut queues = PeerQueues::with_packet_ttl(4, 4096, Duration::from_millis(100));
        queues
            .enqueue(Packet::new_at(peer(1), 1, vec![1], expired_at))
            .expect("expired packet");
        queues
            .enqueue(Packet::new_at(peer(2), 2, vec![2], now))
            .expect("fresh packet");

        queues.drop_expired(now);

        assert_eq!(queues.peer_stats(peer(1)).queued_packets, 0);
        assert_eq!(queues.peer_stats(peer(1)).dropped_packets, 1);
        assert_eq!(queues.peer_stats(peer(1)).expired_packets, 1);
        assert_eq!(queues.peer_stats(peer(1)).expired_bytes, 1);
        assert_eq!(queues.total_stats().queued_packets, 1);
        assert_eq!(queues.total_stats().expired_packets, 1);
        assert_eq!(
            queues
                .dequeue_ready(|candidate| candidate == peer(2))
                .expect("fresh peer remains")
                .sequence(),
            2
        );
    }
}
