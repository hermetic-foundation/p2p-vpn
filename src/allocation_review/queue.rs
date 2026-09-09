use super::{delta, snapshot};
use crate::{
    PeerId,
    queue::{Packet, PeerQueues},
};
use std::time::{Duration, Instant};

#[test]
#[ignore = "allocation review: run alone in a fresh process with --test-threads=1"]
fn measure_packet_queue_allocations() {
    const PAYLOAD_BYTES: usize = 1028;
    const PEERS: u8 = 4;
    let mode =
        std::env::var("P2P_VPN_REVIEW_QUEUE_LIMIT").expect("explicit packets or bytes limit");
    let (packet_limit, byte_limit, admitted) = match mode.as_str() {
        "packets" => (4, 8192, 4),
        "bytes" => (16, 4096, 3),
        _ => panic!("queue limit must be packets or bytes"),
    };
    let mut rows = Vec::with_capacity(10);
    let initial = snapshot();
    let mut queues = PeerQueues::with_packet_ttl(packet_limit, byte_limit, Duration::from_secs(1));
    for cycle in 1..=10 {
        let now = Instant::now();
        let before = snapshot();
        for seed in 1..=PEERS {
            let peer = PeerId::from_bytes([seed; 32]);
            for sequence in 0..admitted {
                queues
                    .enqueue(Packet::new_at(
                        peer,
                        sequence as u64,
                        vec![0; PAYLOAD_BYTES],
                        now,
                    ))
                    .unwrap();
            }
            assert!(
                queues
                    .enqueue(Packet::new_at(peer, 100, vec![0; PAYLOAD_BYTES], now))
                    .is_err()
            );
        }
        assert_eq!(
            queues.total_stats().queued_packets,
            usize::from(PEERS) * admitted
        );
        assert_eq!(
            queues.total_stats().queued_bytes,
            usize::from(PEERS) * admitted * PAYLOAD_BYTES
        );
        let full = snapshot();
        while let Some(packet) = queues.dequeue() {
            std::hint::black_box(&packet);
            drop(packet);
        }
        let drained = snapshot();
        assert_eq!(queues.total_stats().queued_bytes, 0);
        for seed in 1..=PEERS {
            queues
                .enqueue(Packet::new_at(
                    PeerId::from_bytes([seed; 32]),
                    200,
                    vec![0; PAYLOAD_BYTES],
                    now,
                ))
                .unwrap();
        }
        let expiry_full = snapshot();
        queues.drop_expired(now + Duration::from_secs(2));
        let expired = snapshot();
        assert_eq!(queues.total_stats().queued_packets, 0);
        assert_eq!(queues.total_stats().queued_bytes, 0);
        assert_eq!(queues.retain_peers(|_| false), 0);
        let retired = snapshot();
        rows.push((cycle, before, full, drained, expiry_full, expired, retired));
    }
    drop(queues);
    let dropped = snapshot();
    // Only serialize after owner teardown; report allocations are outside every interval.
    let cycles: Vec<_> = rows.into_iter().map(|(cycle, before, full, drained, expiry_full, expired, retired)| {
        serde_json::json!({
            "cycle": cycle,
            "fill_and_reject": delta(before, full),
            "drain": delta(full, drained),
            "refill_for_expiry": delta(drained, expiry_full),
            "expire": delta(expiry_full, expired),
            "retire_peers": delta(expired, retired),
            "live_after_retirement_from_initial": super::live_bytes(retired) - super::live_bytes(initial),
        })
    }).collect();
    eprintln!(
        "queue_allocation_sample {}",
        serde_json::json!({
            "schema_version": 1, "mode": mode, "peers": PEERS,
            "payload_bytes": PAYLOAD_BYTES, "packet_limit": packet_limit,
            "byte_limit": byte_limit, "admitted_per_peer": admitted,
            "cycles": cycles, "whole_owner": delta(initial, dropped),
        })
    );
}
