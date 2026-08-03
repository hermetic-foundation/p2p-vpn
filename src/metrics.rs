use std::sync::atomic::{AtomicU64, Ordering};

use crate::queue::QueueStats;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketDropReason {
    MalformedPacket,
    NoRoute,
    NoTransportPeer,
    PacketTooLarge,
    QueueFull,
    Replay,
    UnauthorizedPeer,
    UnauthorizedSource,
    UnauthorizedDestination,
    UnexpectedPayload,
}

#[derive(Debug, Default)]
pub struct RuntimeMetrics {
    tun_read_packets: AtomicU64,
    tun_read_bytes: AtomicU64,
    tun_write_packets: AtomicU64,
    tun_write_bytes: AtomicU64,
    outbound_sent_packets: AtomicU64,
    inbound_accepted_packets: AtomicU64,
    outbound_dropped_packets: AtomicU64,
    inbound_dropped_packets: AtomicU64,
    outbound_drop_malformed_packets: AtomicU64,
    outbound_drop_no_route_packets: AtomicU64,
    outbound_drop_no_transport_peer_packets: AtomicU64,
    outbound_drop_packet_too_large_packets: AtomicU64,
    outbound_drop_queue_full_packets: AtomicU64,
    outbound_drop_unauthorized_source_packets: AtomicU64,
    inbound_drop_malformed_packets: AtomicU64,
    inbound_drop_packet_too_large_packets: AtomicU64,
    inbound_drop_replay_packets: AtomicU64,
    inbound_drop_unauthorized_peer_packets: AtomicU64,
    inbound_drop_unauthorized_source_packets: AtomicU64,
    inbound_drop_unauthorized_destination_packets: AtomicU64,
    inbound_drop_unexpected_payload_packets: AtomicU64,
    outbound_failures: AtomicU64,
    inbound_failures: AtomicU64,
    direct_connections_established: AtomicU64,
    relayed_connections_established: AtomicU64,
    unauthorized_connections_dropped: AtomicU64,
    relay_reservations_accepted: AtomicU64,
    relay_outbound_circuits_established: AtomicU64,
    relay_inbound_circuits_established: AtomicU64,
    relay_server_reservations_accepted: AtomicU64,
    relay_server_circuits_accepted: AtomicU64,
    dcutr_successes: AtomicU64,
    dcutr_failures: AtomicU64,
    control_requests_sent: AtomicU64,
    control_requests_received: AtomicU64,
    control_responses_received: AtomicU64,
    control_failures: AtomicU64,
    redial_attempts: AtomicU64,
    redial_skipped_connected: AtomicU64,
    redial_failures: AtomicU64,
    outbound_queue_blocked_no_supported_path_events: AtomicU64,
}

impl RuntimeMetrics {
    pub fn record_tun_read(&self, bytes: usize) {
        self.tun_read_packets.fetch_add(1, Ordering::Relaxed);
        self.tun_read_bytes
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn record_tun_write(&self, bytes: usize) {
        self.tun_write_packets.fetch_add(1, Ordering::Relaxed);
        self.tun_write_bytes
            .fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub fn record_outbound_sent(&self) {
        self.outbound_sent_packets.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inbound_accepted(&self) {
        self.inbound_accepted_packets
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_drop(&self, reason: PacketDropReason) {
        self.outbound_dropped_packets
            .fetch_add(1, Ordering::Relaxed);
        match reason {
            PacketDropReason::MalformedPacket
            | PacketDropReason::Replay
            | PacketDropReason::UnauthorizedPeer
            | PacketDropReason::UnauthorizedDestination
            | PacketDropReason::UnexpectedPayload => self
                .outbound_drop_malformed_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::NoRoute => self
                .outbound_drop_no_route_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::NoTransportPeer => self
                .outbound_drop_no_transport_peer_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::PacketTooLarge => self
                .outbound_drop_packet_too_large_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::QueueFull => self
                .outbound_drop_queue_full_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::UnauthorizedSource => self
                .outbound_drop_unauthorized_source_packets
                .fetch_add(1, Ordering::Relaxed),
        };
    }

    pub fn record_inbound_drop(&self, reason: PacketDropReason) {
        self.inbound_dropped_packets.fetch_add(1, Ordering::Relaxed);
        match reason {
            PacketDropReason::MalformedPacket | PacketDropReason::NoRoute => self
                .inbound_drop_malformed_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::PacketTooLarge | PacketDropReason::QueueFull => self
                .inbound_drop_packet_too_large_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::Replay => self
                .inbound_drop_replay_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::UnauthorizedPeer | PacketDropReason::NoTransportPeer => self
                .inbound_drop_unauthorized_peer_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::UnauthorizedSource => self
                .inbound_drop_unauthorized_source_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::UnauthorizedDestination => self
                .inbound_drop_unauthorized_destination_packets
                .fetch_add(1, Ordering::Relaxed),
            PacketDropReason::UnexpectedPayload => self
                .inbound_drop_unexpected_payload_packets
                .fetch_add(1, Ordering::Relaxed),
        };
    }

    pub fn record_outbound_failure(&self) {
        self.outbound_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inbound_failure(&self) {
        self.inbound_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_connection_established(&self, relayed: bool) {
        if relayed {
            self.relayed_connections_established
                .fetch_add(1, Ordering::Relaxed);
        } else {
            self.direct_connections_established
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_unauthorized_connection_dropped(&self) {
        self.unauthorized_connections_dropped
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_reservation_accepted(&self) {
        self.relay_reservations_accepted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_outbound_circuit_established(&self) {
        self.relay_outbound_circuits_established
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_inbound_circuit_established(&self) {
        self.relay_inbound_circuits_established
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_server_reservation_accepted(&self) {
        self.relay_server_reservations_accepted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_server_circuit_accepted(&self) {
        self.relay_server_circuits_accepted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_dcutr_result(&self, success: bool) {
        if success {
            self.dcutr_successes.fetch_add(1, Ordering::Relaxed);
        } else {
            self.dcutr_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_control_request_sent(&self) {
        self.control_requests_sent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_control_request_received(&self) {
        self.control_requests_received
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_control_response_received(&self) {
        self.control_responses_received
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_control_failure(&self) {
        self.control_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_redial_attempt(&self) {
        self.redial_attempts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_redial_skipped_connected(&self) {
        self.redial_skipped_connected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_redial_failure(&self) {
        self.redial_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_queue_blocked_no_supported_path(&self) {
        self.outbound_queue_blocked_no_supported_path_events
            .fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self, queue: QueueStats) -> RuntimeSnapshot {
        RuntimeSnapshot {
            tun_read_packets: self.tun_read_packets.load(Ordering::Relaxed),
            tun_read_bytes: self.tun_read_bytes.load(Ordering::Relaxed),
            tun_write_packets: self.tun_write_packets.load(Ordering::Relaxed),
            tun_write_bytes: self.tun_write_bytes.load(Ordering::Relaxed),
            outbound_sent_packets: self.outbound_sent_packets.load(Ordering::Relaxed),
            inbound_accepted_packets: self.inbound_accepted_packets.load(Ordering::Relaxed),
            outbound_dropped_packets: self.outbound_dropped_packets.load(Ordering::Relaxed),
            inbound_dropped_packets: self.inbound_dropped_packets.load(Ordering::Relaxed),
            outbound_drop_malformed_packets: self
                .outbound_drop_malformed_packets
                .load(Ordering::Relaxed),
            outbound_drop_no_route_packets: self
                .outbound_drop_no_route_packets
                .load(Ordering::Relaxed),
            outbound_drop_no_transport_peer_packets: self
                .outbound_drop_no_transport_peer_packets
                .load(Ordering::Relaxed),
            outbound_drop_packet_too_large_packets: self
                .outbound_drop_packet_too_large_packets
                .load(Ordering::Relaxed),
            outbound_drop_queue_full_packets: self
                .outbound_drop_queue_full_packets
                .load(Ordering::Relaxed),
            outbound_drop_unauthorized_source_packets: self
                .outbound_drop_unauthorized_source_packets
                .load(Ordering::Relaxed),
            inbound_drop_malformed_packets: self
                .inbound_drop_malformed_packets
                .load(Ordering::Relaxed),
            inbound_drop_packet_too_large_packets: self
                .inbound_drop_packet_too_large_packets
                .load(Ordering::Relaxed),
            inbound_drop_replay_packets: self.inbound_drop_replay_packets.load(Ordering::Relaxed),
            inbound_drop_unauthorized_peer_packets: self
                .inbound_drop_unauthorized_peer_packets
                .load(Ordering::Relaxed),
            inbound_drop_unauthorized_source_packets: self
                .inbound_drop_unauthorized_source_packets
                .load(Ordering::Relaxed),
            inbound_drop_unauthorized_destination_packets: self
                .inbound_drop_unauthorized_destination_packets
                .load(Ordering::Relaxed),
            inbound_drop_unexpected_payload_packets: self
                .inbound_drop_unexpected_payload_packets
                .load(Ordering::Relaxed),
            outbound_failures: self.outbound_failures.load(Ordering::Relaxed),
            inbound_failures: self.inbound_failures.load(Ordering::Relaxed),
            direct_connections_established: self
                .direct_connections_established
                .load(Ordering::Relaxed),
            relayed_connections_established: self
                .relayed_connections_established
                .load(Ordering::Relaxed),
            unauthorized_connections_dropped: self
                .unauthorized_connections_dropped
                .load(Ordering::Relaxed),
            relay_reservations_accepted: self.relay_reservations_accepted.load(Ordering::Relaxed),
            relay_outbound_circuits_established: self
                .relay_outbound_circuits_established
                .load(Ordering::Relaxed),
            relay_inbound_circuits_established: self
                .relay_inbound_circuits_established
                .load(Ordering::Relaxed),
            relay_server_reservations_accepted: self
                .relay_server_reservations_accepted
                .load(Ordering::Relaxed),
            relay_server_circuits_accepted: self
                .relay_server_circuits_accepted
                .load(Ordering::Relaxed),
            dcutr_successes: self.dcutr_successes.load(Ordering::Relaxed),
            dcutr_failures: self.dcutr_failures.load(Ordering::Relaxed),
            control_requests_sent: self.control_requests_sent.load(Ordering::Relaxed),
            control_requests_received: self.control_requests_received.load(Ordering::Relaxed),
            control_responses_received: self.control_responses_received.load(Ordering::Relaxed),
            control_failures: self.control_failures.load(Ordering::Relaxed),
            redial_attempts: self.redial_attempts.load(Ordering::Relaxed),
            redial_skipped_connected: self.redial_skipped_connected.load(Ordering::Relaxed),
            redial_failures: self.redial_failures.load(Ordering::Relaxed),
            outbound_queue_blocked_no_supported_path_events: self
                .outbound_queue_blocked_no_supported_path_events
                .load(Ordering::Relaxed),
            queue,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub tun_read_packets: u64,
    pub tun_read_bytes: u64,
    pub tun_write_packets: u64,
    pub tun_write_bytes: u64,
    pub outbound_sent_packets: u64,
    pub inbound_accepted_packets: u64,
    pub outbound_dropped_packets: u64,
    pub inbound_dropped_packets: u64,
    pub outbound_drop_malformed_packets: u64,
    pub outbound_drop_no_route_packets: u64,
    pub outbound_drop_no_transport_peer_packets: u64,
    pub outbound_drop_packet_too_large_packets: u64,
    pub outbound_drop_queue_full_packets: u64,
    pub outbound_drop_unauthorized_source_packets: u64,
    pub inbound_drop_malformed_packets: u64,
    pub inbound_drop_packet_too_large_packets: u64,
    pub inbound_drop_replay_packets: u64,
    pub inbound_drop_unauthorized_peer_packets: u64,
    pub inbound_drop_unauthorized_source_packets: u64,
    pub inbound_drop_unauthorized_destination_packets: u64,
    pub inbound_drop_unexpected_payload_packets: u64,
    pub outbound_failures: u64,
    pub inbound_failures: u64,
    pub direct_connections_established: u64,
    pub relayed_connections_established: u64,
    pub unauthorized_connections_dropped: u64,
    pub relay_reservations_accepted: u64,
    pub relay_outbound_circuits_established: u64,
    pub relay_inbound_circuits_established: u64,
    pub relay_server_reservations_accepted: u64,
    pub relay_server_circuits_accepted: u64,
    pub dcutr_successes: u64,
    pub dcutr_failures: u64,
    pub control_requests_sent: u64,
    pub control_requests_received: u64,
    pub control_responses_received: u64,
    pub control_failures: u64,
    pub redial_attempts: u64,
    pub redial_skipped_connected: u64,
    pub redial_failures: u64,
    pub outbound_queue_blocked_no_supported_path_events: u64,
    pub queue: QueueStats,
}

impl RuntimeSnapshot {
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("tun_read_packets {}", self.tun_read_packets),
            format!("tun_read_bytes {}", self.tun_read_bytes),
            format!("tun_write_packets {}", self.tun_write_packets),
            format!("tun_write_bytes {}", self.tun_write_bytes),
            format!("outbound_sent_packets {}", self.outbound_sent_packets),
            format!("inbound_accepted_packets {}", self.inbound_accepted_packets),
        ];
        self.extend_drop_lines(&mut lines);
        lines.extend([
            format!("outbound_failures {}", self.outbound_failures),
            format!("inbound_failures {}", self.inbound_failures),
            format!(
                "direct_connections_established {}",
                self.direct_connections_established
            ),
            format!(
                "relayed_connections_established {}",
                self.relayed_connections_established
            ),
            format!(
                "unauthorized_connections_dropped {}",
                self.unauthorized_connections_dropped
            ),
            format!(
                "relay_reservations_accepted {}",
                self.relay_reservations_accepted
            ),
            format!(
                "relay_outbound_circuits_established {}",
                self.relay_outbound_circuits_established
            ),
            format!(
                "relay_inbound_circuits_established {}",
                self.relay_inbound_circuits_established
            ),
            format!(
                "relay_server_reservations_accepted {}",
                self.relay_server_reservations_accepted
            ),
            format!(
                "relay_server_circuits_accepted {}",
                self.relay_server_circuits_accepted
            ),
            format!("dcutr_successes {}", self.dcutr_successes),
            format!("dcutr_failures {}", self.dcutr_failures),
            format!("control_requests_sent {}", self.control_requests_sent),
            format!(
                "control_requests_received {}",
                self.control_requests_received
            ),
            format!(
                "control_responses_received {}",
                self.control_responses_received
            ),
            format!("control_failures {}", self.control_failures),
            format!("redial_attempts {}", self.redial_attempts),
            format!("redial_skipped_connected {}", self.redial_skipped_connected),
            format!("redial_failures {}", self.redial_failures),
            format!(
                "outbound_queue_blocked_no_supported_path_events {}",
                self.outbound_queue_blocked_no_supported_path_events
            ),
            format!("queue_queued_packets {}", self.queue.queued_packets),
            format!("queue_queued_bytes {}", self.queue.queued_bytes),
            format!("queue_dropped_packets {}", self.queue.dropped_packets),
            format!("queue_dropped_bytes {}", self.queue.dropped_bytes),
            format!("queue_expired_packets {}", self.queue.expired_packets),
            format!("queue_expired_bytes {}", self.queue.expired_bytes),
        ]);
        lines
    }

    fn extend_drop_lines(&self, lines: &mut Vec<String>) {
        lines.extend([
            format!("outbound_dropped_packets {}", self.outbound_dropped_packets),
            format!("inbound_dropped_packets {}", self.inbound_dropped_packets),
            format!(
                "outbound_drop_malformed_packets {}",
                self.outbound_drop_malformed_packets
            ),
            format!(
                "outbound_drop_no_route_packets {}",
                self.outbound_drop_no_route_packets
            ),
            format!(
                "outbound_drop_no_transport_peer_packets {}",
                self.outbound_drop_no_transport_peer_packets
            ),
            format!(
                "outbound_drop_packet_too_large_packets {}",
                self.outbound_drop_packet_too_large_packets
            ),
            format!(
                "outbound_drop_queue_full_packets {}",
                self.outbound_drop_queue_full_packets
            ),
            format!(
                "outbound_drop_unauthorized_source_packets {}",
                self.outbound_drop_unauthorized_source_packets
            ),
            format!(
                "inbound_drop_malformed_packets {}",
                self.inbound_drop_malformed_packets
            ),
            format!(
                "inbound_drop_packet_too_large_packets {}",
                self.inbound_drop_packet_too_large_packets
            ),
            format!(
                "inbound_drop_replay_packets {}",
                self.inbound_drop_replay_packets
            ),
            format!(
                "inbound_drop_unauthorized_peer_packets {}",
                self.inbound_drop_unauthorized_peer_packets
            ),
            format!(
                "inbound_drop_unauthorized_source_packets {}",
                self.inbound_drop_unauthorized_source_packets
            ),
            format!(
                "inbound_drop_unauthorized_destination_packets {}",
                self.inbound_drop_unauthorized_destination_packets
            ),
            format!(
                "inbound_drop_unexpected_payload_packets {}",
                self.inbound_drop_unexpected_payload_packets
            ),
        ]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated_snapshot() -> RuntimeSnapshot {
        let metrics = RuntimeMetrics::default();
        metrics.record_tun_read(20);
        metrics.record_tun_write(40);
        metrics.record_outbound_sent();
        metrics.record_inbound_accepted();
        metrics.record_outbound_drop(PacketDropReason::NoRoute);
        metrics.record_outbound_drop(PacketDropReason::PacketTooLarge);
        metrics.record_outbound_drop(PacketDropReason::QueueFull);
        metrics.record_outbound_drop(PacketDropReason::UnauthorizedSource);
        metrics.record_inbound_drop(PacketDropReason::MalformedPacket);
        metrics.record_inbound_drop(PacketDropReason::PacketTooLarge);
        metrics.record_inbound_drop(PacketDropReason::Replay);
        metrics.record_inbound_drop(PacketDropReason::UnauthorizedPeer);
        metrics.record_inbound_drop(PacketDropReason::UnauthorizedSource);
        metrics.record_inbound_drop(PacketDropReason::UnauthorizedDestination);
        metrics.record_inbound_drop(PacketDropReason::UnexpectedPayload);
        metrics.record_outbound_failure();
        metrics.record_inbound_failure();
        metrics.record_connection_established(false);
        metrics.record_connection_established(true);
        metrics.record_unauthorized_connection_dropped();
        metrics.record_relay_reservation_accepted();
        metrics.record_relay_outbound_circuit_established();
        metrics.record_relay_inbound_circuit_established();
        metrics.record_relay_server_reservation_accepted();
        metrics.record_relay_server_circuit_accepted();
        metrics.record_dcutr_result(true);
        metrics.record_dcutr_result(false);
        metrics.record_control_request_sent();
        metrics.record_control_request_received();
        metrics.record_control_response_received();
        metrics.record_control_failure();
        metrics.record_redial_attempt();
        metrics.record_redial_skipped_connected();
        metrics.record_redial_failure();
        metrics.record_outbound_queue_blocked_no_supported_path();

        metrics.snapshot(QueueStats {
            queued_packets: 2,
            queued_bytes: 80,
            dropped_packets: 3,
            dropped_bytes: 120,
            expired_packets: 2,
            expired_bytes: 60,
        })
    }

    fn assert_metric_line(snapshot: &RuntimeSnapshot, line: &str) {
        assert!(snapshot.lines().contains(&line.to_owned()));
    }

    #[test]
    fn metrics_snapshot_reports_runtime_and_queue_counters() {
        let snapshot = populated_snapshot();

        assert_eq!(snapshot.tun_read_packets, 1);
        assert_eq!(snapshot.tun_read_bytes, 20);
        assert_eq!(snapshot.tun_write_packets, 1);
        assert_eq!(snapshot.tun_write_bytes, 40);
        assert_eq!(snapshot.outbound_sent_packets, 1);
        assert_eq!(snapshot.inbound_accepted_packets, 1);
        assert_eq!(snapshot.outbound_dropped_packets, 4);
        assert_eq!(snapshot.inbound_dropped_packets, 7);
        assert_eq!(snapshot.outbound_drop_no_route_packets, 1);
        assert_eq!(snapshot.outbound_drop_packet_too_large_packets, 1);
        assert_eq!(snapshot.outbound_drop_queue_full_packets, 1);
        assert_eq!(snapshot.outbound_drop_unauthorized_source_packets, 1);
        assert_eq!(snapshot.inbound_drop_malformed_packets, 1);
        assert_eq!(snapshot.inbound_drop_packet_too_large_packets, 1);
        assert_eq!(snapshot.inbound_drop_replay_packets, 1);
        assert_eq!(snapshot.inbound_drop_unauthorized_peer_packets, 1);
        assert_eq!(snapshot.inbound_drop_unauthorized_source_packets, 1);
        assert_eq!(snapshot.inbound_drop_unauthorized_destination_packets, 1);
        assert_eq!(snapshot.inbound_drop_unexpected_payload_packets, 1);
        assert_eq!(snapshot.outbound_failures, 1);
        assert_eq!(snapshot.inbound_failures, 1);
        assert_eq!(snapshot.direct_connections_established, 1);
        assert_eq!(snapshot.relayed_connections_established, 1);
        assert_eq!(snapshot.unauthorized_connections_dropped, 1);
        assert_eq!(snapshot.relay_reservations_accepted, 1);
        assert_eq!(snapshot.relay_outbound_circuits_established, 1);
        assert_eq!(snapshot.relay_inbound_circuits_established, 1);
        assert_eq!(snapshot.relay_server_reservations_accepted, 1);
        assert_eq!(snapshot.relay_server_circuits_accepted, 1);
        assert_eq!(snapshot.dcutr_successes, 1);
        assert_eq!(snapshot.dcutr_failures, 1);
        assert_eq!(snapshot.control_requests_sent, 1);
        assert_eq!(snapshot.control_requests_received, 1);
        assert_eq!(snapshot.control_responses_received, 1);
        assert_eq!(snapshot.control_failures, 1);
        assert_eq!(snapshot.redial_attempts, 1);
        assert_eq!(snapshot.redial_skipped_connected, 1);
        assert_eq!(snapshot.redial_failures, 1);
        assert_eq!(snapshot.outbound_queue_blocked_no_supported_path_events, 1);
    }

    #[test]
    fn metrics_snapshot_reports_runtime_and_queue_lines() {
        let snapshot = populated_snapshot();

        assert_metric_line(&snapshot, "queue_queued_packets 2");
        assert_metric_line(&snapshot, "outbound_drop_no_route_packets 1");
        assert_metric_line(&snapshot, "outbound_drop_packet_too_large_packets 1");
        assert_metric_line(&snapshot, "outbound_drop_queue_full_packets 1");
        assert_metric_line(&snapshot, "outbound_drop_unauthorized_source_packets 1");
        assert_metric_line(&snapshot, "inbound_drop_malformed_packets 1");
        assert_metric_line(&snapshot, "inbound_drop_packet_too_large_packets 1");
        assert_metric_line(&snapshot, "inbound_drop_replay_packets 1");
        assert_metric_line(&snapshot, "inbound_drop_unauthorized_peer_packets 1");
        assert_metric_line(&snapshot, "inbound_drop_unauthorized_source_packets 1");
        assert_metric_line(&snapshot, "inbound_drop_unauthorized_destination_packets 1");
        assert_metric_line(&snapshot, "inbound_drop_unexpected_payload_packets 1");
        assert_metric_line(&snapshot, "relayed_connections_established 1");
        assert_metric_line(&snapshot, "unauthorized_connections_dropped 1");
        assert_metric_line(&snapshot, "dcutr_successes 1");
        assert_metric_line(&snapshot, "control_requests_sent 1");
        assert_metric_line(&snapshot, "control_requests_received 1");
        assert_metric_line(&snapshot, "control_responses_received 1");
        assert_metric_line(&snapshot, "control_failures 1");
        assert_metric_line(&snapshot, "redial_attempts 1");
        assert_metric_line(&snapshot, "redial_skipped_connected 1");
        assert_metric_line(&snapshot, "redial_failures 1");
        assert_metric_line(
            &snapshot,
            "outbound_queue_blocked_no_supported_path_events 1",
        );
        assert_metric_line(&snapshot, "queue_expired_packets 2");
        assert_metric_line(&snapshot, "queue_expired_bytes 60");
    }
}
