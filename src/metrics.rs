use std::sync::atomic::{AtomicU64, Ordering};

use crate::{path::PathRuntimeStats, queue::QueueStats, runtime::control::ControlRejectionReason};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoNatReachability {
    Unknown,
    Public,
    Private,
}

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
    RateLimited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketPlaneDropReason {
    NoListener,
    NoSession,
    NoSessions,
    UnknownEndpoint,
    UnexpectedEndpoint,
    IoError,
    Encrypt,
    Decrypt,
    InvalidMagic,
    Truncated,
    UnsupportedVersion,
    CiphertextTooLarge,
    PayloadTooLarge,
    FrameDecode,
    FrameLengthMismatch,
    HeaderMismatch,
    ReplayedDatagram,
    DatagramOutsideReplayWindow,
    TrailingBytes,
}

#[derive(Debug, Default)]
pub struct RuntimeMetrics {
    tun_read_packets: AtomicU64,
    tun_read_bytes: AtomicU64,
    tun_write_packets: AtomicU64,
    tun_write_bytes: AtomicU64,
    outbound_sent_packets: AtomicU64,
    outbound_stream_fallback_packets: AtomicU64,
    outbound_quic_datagram_packets: AtomicU64,
    outbound_quic_datagram_unavailable_packets: AtomicU64,
    outbound_path_probes_sent: AtomicU64,
    outbound_path_probe_acks_sent: AtomicU64,
    outbound_path_probe_failures: AtomicU64,
    packet_plane_sessions_expired: AtomicU64,
    inbound_accepted_packets: AtomicU64,
    inbound_keepalives_accepted: AtomicU64,
    inbound_path_probes_accepted: AtomicU64,
    outbound_dropped_packets: AtomicU64,
    inbound_dropped_packets: AtomicU64,
    outbound_drop_malformed_packets: AtomicU64,
    outbound_drop_no_route_packets: AtomicU64,
    outbound_drop_no_transport_peer_packets: AtomicU64,
    outbound_drop_packet_too_large_packets: AtomicU64,
    outbound_drop_queue_full_packets: AtomicU64,
    outbound_drop_queue_expired_packets: AtomicU64,
    outbound_drop_unauthorized_source_packets: AtomicU64,
    outbound_packet_too_big_notifications: AtomicU64,
    outbound_packet_too_big_no_writer: AtomicU64,
    outbound_packet_too_big_unparseable: AtomicU64,
    outbound_packet_too_big_write_failures: AtomicU64,
    inbound_drop_malformed_packets: AtomicU64,
    inbound_drop_packet_too_large_packets: AtomicU64,
    inbound_drop_replay_packets: AtomicU64,
    inbound_drop_unauthorized_peer_packets: AtomicU64,
    inbound_drop_unauthorized_source_packets: AtomicU64,
    inbound_drop_unauthorized_destination_packets: AtomicU64,
    inbound_drop_unexpected_payload_packets: AtomicU64,
    inbound_drop_rate_limited_packets: AtomicU64,
    packet_plane_inbound_dropped_datagrams: AtomicU64,
    packet_plane_inbound_drop_no_listener_datagrams: AtomicU64,
    packet_plane_inbound_drop_no_session_datagrams: AtomicU64,
    packet_plane_inbound_drop_no_sessions_datagrams: AtomicU64,
    packet_plane_inbound_drop_unknown_endpoint_datagrams: AtomicU64,
    packet_plane_inbound_drop_unexpected_endpoint_datagrams: AtomicU64,
    packet_plane_inbound_drop_io_error_datagrams: AtomicU64,
    packet_plane_inbound_drop_encrypt_datagrams: AtomicU64,
    packet_plane_inbound_drop_decrypt_datagrams: AtomicU64,
    packet_plane_inbound_drop_invalid_magic_datagrams: AtomicU64,
    packet_plane_inbound_drop_truncated_datagrams: AtomicU64,
    packet_plane_inbound_drop_unsupported_version_datagrams: AtomicU64,
    packet_plane_inbound_drop_ciphertext_too_large_datagrams: AtomicU64,
    packet_plane_inbound_drop_payload_too_large_datagrams: AtomicU64,
    packet_plane_inbound_drop_frame_decode_datagrams: AtomicU64,
    packet_plane_inbound_drop_frame_length_mismatch_datagrams: AtomicU64,
    packet_plane_inbound_drop_header_mismatch_datagrams: AtomicU64,
    packet_plane_inbound_drop_replayed_datagrams: AtomicU64,
    packet_plane_inbound_drop_outside_replay_window_datagrams: AtomicU64,
    packet_plane_inbound_drop_trailing_bytes_datagrams: AtomicU64,
    outbound_failures: AtomicU64,
    inbound_failures: AtomicU64,
    direct_connections_established: AtomicU64,
    relayed_connections_established: AtomicU64,
    path_promotions_to_direct: AtomicU64,
    path_fallbacks_to_relay: AtomicU64,
    packet_plane_path_demotions: AtomicU64,
    stream_fallback_path_demotions: AtomicU64,
    packet_plane_path_recovery_dial_attempts: AtomicU64,
    packet_plane_path_recovery_dial_failures: AtomicU64,
    outbound_path_mtu_updates: AtomicU64,
    outbound_path_mtu_probe_confirmations: AtomicU64,
    unauthorized_connections_dropped: AtomicU64,
    relay_reservations_accepted: AtomicU64,
    relay_reservations_lost: AtomicU64,
    auto_relay_infrastructure_candidates: AtomicU64,
    auto_relay_infrastructure_dial_attempts: AtomicU64,
    auto_relay_infrastructure_dial_failures: AtomicU64,
    auto_relay_discovery_queries: AtomicU64,
    auto_relay_candidates: AtomicU64,
    auto_relay_reservation_attempts: AtomicU64,
    auto_relay_reservation_failures: AtomicU64,
    relay_outbound_circuits_established: AtomicU64,
    relay_inbound_circuits_established: AtomicU64,
    relay_server_reservations_accepted: AtomicU64,
    relay_server_reservations_denied: AtomicU64,
    relay_server_reservations_closed: AtomicU64,
    relay_server_reservations_timed_out: AtomicU64,
    relay_server_circuits_accepted: AtomicU64,
    relay_server_circuits_denied: AtomicU64,
    relay_server_circuits_closed: AtomicU64,
    dcutr_successes: AtomicU64,
    dcutr_failures: AtomicU64,
    external_address_candidates: AtomicU64,
    external_addresses_confirmed: AtomicU64,
    external_addresses_expired: AtomicU64,
    observed_packet_plane_external_addresses: AtomicU64,
    observed_packet_plane_external_addresses_rejected: AtomicU64,
    observed_packet_plane_udp_endpoint_candidates: AtomicU64,
    observed_packet_plane_quic_endpoint_candidates: AtomicU64,
    autonat_probes_scheduled: AtomicU64,
    autonat_status_unknown: AtomicU64,
    autonat_status_public: AtomicU64,
    autonat_status_private: AtomicU64,
    autonat_status_changes_to_unknown: AtomicU64,
    autonat_status_changes_to_public: AtomicU64,
    autonat_status_changes_to_private: AtomicU64,
    kademlia_provider_lookups: AtomicU64,
    kademlia_providers_found: AtomicU64,
    kademlia_providers_ignored: AtomicU64,
    kademlia_provider_dial_attempts: AtomicU64,
    kademlia_provider_dial_failures: AtomicU64,
    kademlia_provider_advertisements: AtomicU64,
    kademlia_provider_advertisement_failures: AtomicU64,
    kademlia_membership_record_lookups: AtomicU64,
    kademlia_membership_records_found: AtomicU64,
    kademlia_membership_records_accepted: AtomicU64,
    kademlia_membership_record_invalid: AtomicU64,
    kademlia_membership_record_publications: AtomicU64,
    kademlia_membership_record_publication_failures: AtomicU64,
    kademlia_bootstrap_refreshes: AtomicU64,
    kademlia_bootstrap_failures: AtomicU64,
    control_requests_sent: AtomicU64,
    control_requests_received: AtomicU64,
    control_responses_received: AtomicU64,
    control_capability_accepts: AtomicU64,
    control_capability_rejections: AtomicU64,
    control_reject_unauthorized_peer: AtomicU64,
    control_reject_wrong_network: AtomicU64,
    control_reject_membership_mismatch: AtomicU64,
    control_reject_unsupported_wire_version: AtomicU64,
    control_reject_unsupported_packet_protocol: AtomicU64,
    control_reject_unsupported_packet_header_length: AtomicU64,
    control_reject_invalid_effective_mtu: AtomicU64,
    control_reject_unsupported_preferred_path: AtomicU64,
    control_reject_unauthorized_route_advertisement: AtomicU64,
    control_reject_invalid_owned_quic_certificate: AtomicU64,
    control_reject_invalid_membership_record: AtomicU64,
    control_failures: AtomicU64,
    service_requests_sent: AtomicU64,
    service_requests_received: AtomicU64,
    service_responses_received: AtomicU64,
    service_status_accepts: AtomicU64,
    service_status_rejections: AtomicU64,
    service_reject_unauthorized_peer: AtomicU64,
    service_reject_wrong_network: AtomicU64,
    service_reject_membership_mismatch: AtomicU64,
    service_failures: AtomicU64,
    redial_attempts: AtomicU64,
    redial_skipped_connected: AtomicU64,
    redial_failures: AtomicU64,
    outgoing_connection_errors: AtomicU64,
    discovered_addresses_accepted: AtomicU64,
    discovered_address_dial_attempts: AtomicU64,
    discovered_address_dial_failures: AtomicU64,
    discovered_addresses_rejected: AtomicU64,
    discovered_addresses_expired: AtomicU64,
    outbound_queue_blocked_no_supported_path_events: AtomicU64,
    outbound_queue_blocked_packet_window_events: AtomicU64,
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

    pub fn record_outbound_stream_fallback(&self) {
        self.outbound_stream_fallback_packets
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_quic_datagram(&self) {
        self.outbound_quic_datagram_packets
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_quic_datagram_unavailable(&self) {
        self.outbound_quic_datagram_unavailable_packets
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_path_probe_sent(&self) {
        self.outbound_path_probes_sent
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_path_probe_ack_sent(&self) {
        self.outbound_path_probe_acks_sent
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_path_probe_failure(&self) {
        self.outbound_path_probe_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_packet_plane_session_expired(&self) {
        self.packet_plane_sessions_expired
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inbound_accepted(&self) {
        self.inbound_accepted_packets
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inbound_keepalive_accepted(&self) {
        self.inbound_keepalives_accepted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_inbound_path_probe_accepted(&self) {
        self.inbound_path_probes_accepted
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
            | PacketDropReason::UnexpectedPayload
            | PacketDropReason::RateLimited => self
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

    pub fn record_outbound_queue_expired(&self, packets: u64) {
        self.outbound_dropped_packets
            .fetch_add(packets, Ordering::Relaxed);
        self.outbound_drop_queue_expired_packets
            .fetch_add(packets, Ordering::Relaxed);
    }

    pub fn record_outbound_packet_too_big_notification(&self) {
        self.outbound_packet_too_big_notifications
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_packet_too_big_no_writer(&self) {
        self.outbound_packet_too_big_no_writer
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_packet_too_big_unparseable(&self) {
        self.outbound_packet_too_big_unparseable
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_packet_too_big_write_failure(&self) {
        self.outbound_packet_too_big_write_failures
            .fetch_add(1, Ordering::Relaxed);
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
            PacketDropReason::RateLimited => self
                .inbound_drop_rate_limited_packets
                .fetch_add(1, Ordering::Relaxed),
        };
    }

    pub fn record_packet_plane_inbound_drop(&self, reason: PacketPlaneDropReason) {
        self.packet_plane_inbound_dropped_datagrams
            .fetch_add(1, Ordering::Relaxed);
        match reason {
            PacketPlaneDropReason::NoListener => self
                .packet_plane_inbound_drop_no_listener_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::NoSession => self
                .packet_plane_inbound_drop_no_session_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::NoSessions => self
                .packet_plane_inbound_drop_no_sessions_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::UnknownEndpoint => self
                .packet_plane_inbound_drop_unknown_endpoint_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::UnexpectedEndpoint => self
                .packet_plane_inbound_drop_unexpected_endpoint_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::IoError => self
                .packet_plane_inbound_drop_io_error_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::Encrypt => self
                .packet_plane_inbound_drop_encrypt_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::Decrypt => self
                .packet_plane_inbound_drop_decrypt_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::InvalidMagic => self
                .packet_plane_inbound_drop_invalid_magic_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::Truncated => self
                .packet_plane_inbound_drop_truncated_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::UnsupportedVersion => self
                .packet_plane_inbound_drop_unsupported_version_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::CiphertextTooLarge => self
                .packet_plane_inbound_drop_ciphertext_too_large_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::PayloadTooLarge => self
                .packet_plane_inbound_drop_payload_too_large_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::FrameDecode => self
                .packet_plane_inbound_drop_frame_decode_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::FrameLengthMismatch => self
                .packet_plane_inbound_drop_frame_length_mismatch_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::HeaderMismatch => self
                .packet_plane_inbound_drop_header_mismatch_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::ReplayedDatagram => self
                .packet_plane_inbound_drop_replayed_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::DatagramOutsideReplayWindow => self
                .packet_plane_inbound_drop_outside_replay_window_datagrams
                .fetch_add(1, Ordering::Relaxed),
            PacketPlaneDropReason::TrailingBytes => self
                .packet_plane_inbound_drop_trailing_bytes_datagrams
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

    pub fn record_path_promotion_to_direct(&self) {
        self.path_promotions_to_direct
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_path_fallback_to_relay(&self) {
        self.path_fallbacks_to_relay.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_packet_plane_path_demotion(&self) {
        self.packet_plane_path_demotions
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_stream_fallback_path_demotion(&self) {
        self.stream_fallback_path_demotions
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_packet_plane_path_recovery_dial_attempt(&self) {
        self.packet_plane_path_recovery_dial_attempts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_packet_plane_path_recovery_dial_failure(&self) {
        self.packet_plane_path_recovery_dial_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_path_mtu_update(&self) {
        self.outbound_path_mtu_updates
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_path_mtu_probe_confirmation(&self) {
        self.outbound_path_mtu_probe_confirmations
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_unauthorized_connection_dropped(&self) {
        self.unauthorized_connections_dropped
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_reservation_accepted(&self) {
        self.relay_reservations_accepted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_reservation_lost(&self) {
        self.relay_reservations_lost.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auto_relay_infrastructure_candidate(&self) {
        self.auto_relay_infrastructure_candidates
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auto_relay_infrastructure_dial_attempt(&self) {
        self.auto_relay_infrastructure_dial_attempts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auto_relay_infrastructure_dial_failure(&self) {
        self.auto_relay_infrastructure_dial_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auto_relay_discovery_query(&self) {
        self.auto_relay_discovery_queries
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auto_relay_candidate(&self) {
        self.auto_relay_candidates.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auto_relay_reservation_attempt(&self) {
        self.auto_relay_reservation_attempts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_auto_relay_reservation_failure(&self) {
        self.auto_relay_reservation_failures
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

    pub fn record_relay_server_reservation_denied(&self) {
        self.relay_server_reservations_denied
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_server_reservation_closed(&self) {
        self.relay_server_reservations_closed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_server_reservation_timed_out(&self) {
        self.relay_server_reservations_timed_out
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_server_circuit_accepted(&self) {
        self.relay_server_circuits_accepted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_server_circuit_denied(&self) {
        self.relay_server_circuits_denied
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_relay_server_circuit_closed(&self) {
        self.relay_server_circuits_closed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_dcutr_result(&self, success: bool) {
        if success {
            self.dcutr_successes.fetch_add(1, Ordering::Relaxed);
        } else {
            self.dcutr_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn record_external_address_candidate(&self) {
        self.external_address_candidates
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_external_address_confirmed(&self) {
        self.external_addresses_confirmed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_external_address_expired(&self) {
        self.external_addresses_expired
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_observed_packet_plane_external_address(&self) {
        self.observed_packet_plane_external_addresses
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_observed_packet_plane_external_address_rejected(&self) {
        self.observed_packet_plane_external_addresses_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_observed_packet_plane_udp_endpoint_candidate(&self) {
        self.observed_packet_plane_udp_endpoint_candidates
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_observed_packet_plane_quic_endpoint_candidate(&self) {
        self.observed_packet_plane_quic_endpoint_candidates
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_autonat_probe_scheduled(&self) {
        self.autonat_probes_scheduled
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_autonat_status(&self, reachability: AutoNatReachability) {
        self.autonat_status_unknown.store(0, Ordering::Relaxed);
        self.autonat_status_public.store(0, Ordering::Relaxed);
        self.autonat_status_private.store(0, Ordering::Relaxed);
        match reachability {
            AutoNatReachability::Unknown => {
                self.autonat_status_unknown.store(1, Ordering::Relaxed);
                self.autonat_status_changes_to_unknown
                    .fetch_add(1, Ordering::Relaxed);
            }
            AutoNatReachability::Public => {
                self.autonat_status_public.store(1, Ordering::Relaxed);
                self.autonat_status_changes_to_public
                    .fetch_add(1, Ordering::Relaxed);
            }
            AutoNatReachability::Private => {
                self.autonat_status_private.store(1, Ordering::Relaxed);
                self.autonat_status_changes_to_private
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn record_kademlia_provider_lookup(&self) {
        self.kademlia_provider_lookups
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_providers_found(&self, providers: usize) {
        self.kademlia_providers_found.fetch_add(
            u64::try_from(providers).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    pub fn record_kademlia_provider_ignored(&self) {
        self.kademlia_providers_ignored
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_provider_dial_attempt(&self) {
        self.kademlia_provider_dial_attempts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_provider_dial_failure(&self) {
        self.kademlia_provider_dial_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_provider_advertisement(&self) {
        self.kademlia_provider_advertisements
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_provider_advertisement_failure(&self) {
        self.kademlia_provider_advertisement_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_membership_record_lookup(&self) {
        self.kademlia_membership_record_lookups
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_membership_records_found(&self) {
        self.kademlia_membership_records_found
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_membership_records_accepted(&self, records: usize) {
        self.kademlia_membership_records_accepted.fetch_add(
            u64::try_from(records).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    pub fn record_kademlia_membership_record_invalid(&self) {
        self.kademlia_membership_record_invalid
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_membership_record_publication(&self) {
        self.kademlia_membership_record_publications
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_membership_record_publication_failure(&self) {
        self.kademlia_membership_record_publication_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_bootstrap_refresh(&self) {
        self.kademlia_bootstrap_refreshes
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_kademlia_bootstrap_failure(&self) {
        self.kademlia_bootstrap_failures
            .fetch_add(1, Ordering::Relaxed);
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

    pub fn record_control_capability_accept(&self) {
        self.control_capability_accepts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_control_capability_rejection(&self, reason: ControlRejectionReason) {
        self.control_capability_rejections
            .fetch_add(1, Ordering::Relaxed);
        match reason {
            ControlRejectionReason::UnauthorizedPeer => self
                .control_reject_unauthorized_peer
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::WrongNetwork => self
                .control_reject_wrong_network
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::MembershipMismatch => self
                .control_reject_membership_mismatch
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::UnsupportedWireVersion => self
                .control_reject_unsupported_wire_version
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::UnsupportedPacketProtocol => self
                .control_reject_unsupported_packet_protocol
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::UnsupportedPacketHeaderLength => self
                .control_reject_unsupported_packet_header_length
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::InvalidEffectiveMtu => self
                .control_reject_invalid_effective_mtu
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::UnsupportedPreferredPath => self
                .control_reject_unsupported_preferred_path
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::UnauthorizedRouteAdvertisement => self
                .control_reject_unauthorized_route_advertisement
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::InvalidOwnedQuicCertificate => self
                .control_reject_invalid_owned_quic_certificate
                .fetch_add(1, Ordering::Relaxed),
            ControlRejectionReason::InvalidMembershipRecord => self
                .control_reject_invalid_membership_record
                .fetch_add(1, Ordering::Relaxed),
        };
    }

    pub fn record_control_failure(&self) {
        self.control_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_service_request_sent(&self) {
        self.service_requests_sent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_service_request_received(&self) {
        self.service_requests_received
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_service_response_received(&self) {
        self.service_responses_received
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_service_status_accept(&self) {
        self.service_status_accepts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_service_status_rejection(
        &self,
        reason: crate::runtime::service::ServiceRejectionReason,
    ) {
        self.service_status_rejections
            .fetch_add(1, Ordering::Relaxed);
        match reason {
            crate::runtime::service::ServiceRejectionReason::UnauthorizedPeer => self
                .service_reject_unauthorized_peer
                .fetch_add(1, Ordering::Relaxed),
            crate::runtime::service::ServiceRejectionReason::WrongNetwork => self
                .service_reject_wrong_network
                .fetch_add(1, Ordering::Relaxed),
            crate::runtime::service::ServiceRejectionReason::MembershipMismatch => self
                .service_reject_membership_mismatch
                .fetch_add(1, Ordering::Relaxed),
        };
    }

    pub fn record_service_failure(&self) {
        self.service_failures.fetch_add(1, Ordering::Relaxed);
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

    pub fn record_outgoing_connection_error(&self) {
        self.outgoing_connection_errors
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_discovered_address_accepted(&self) {
        self.discovered_addresses_accepted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_discovered_address_dial_attempt(&self) {
        self.discovered_address_dial_attempts
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_discovered_address_dial_failure(&self) {
        self.discovered_address_dial_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_discovered_address_rejected(&self) {
        self.discovered_addresses_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_discovered_address_expired(&self, addresses: u64) {
        self.discovered_addresses_expired
            .fetch_add(addresses, Ordering::Relaxed);
    }

    pub fn record_outbound_queue_blocked_no_supported_path(&self) {
        self.outbound_queue_blocked_no_supported_path_events
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_outbound_queue_blocked_packet_window(&self) {
        self.outbound_queue_blocked_packet_window_events
            .fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self, queue: QueueStats) -> RuntimeSnapshot {
        self.snapshot_with_paths(queue, PathRuntimeStats::default())
    }

    #[must_use]
    pub fn snapshot_with_paths(
        &self,
        queue: QueueStats,
        path: PathRuntimeStats,
    ) -> RuntimeSnapshot {
        let mut snapshot = RuntimeSnapshot {
            queue,
            path,
            ..RuntimeSnapshot::default()
        };
        self.fill_packet_snapshot(&mut snapshot);
        self.fill_drop_snapshot(&mut snapshot);
        self.fill_transport_snapshot(&mut snapshot);
        self.fill_discovery_snapshot(&mut snapshot);
        self.fill_control_snapshot(&mut snapshot);
        snapshot
    }

    fn fill_packet_snapshot(&self, snapshot: &mut RuntimeSnapshot) {
        snapshot.tun_read_packets = self.tun_read_packets.load(Ordering::Relaxed);
        snapshot.tun_read_bytes = self.tun_read_bytes.load(Ordering::Relaxed);
        snapshot.tun_write_packets = self.tun_write_packets.load(Ordering::Relaxed);
        snapshot.tun_write_bytes = self.tun_write_bytes.load(Ordering::Relaxed);
        snapshot.outbound_sent_packets = self.outbound_sent_packets.load(Ordering::Relaxed);
        snapshot.outbound_stream_fallback_packets = self
            .outbound_stream_fallback_packets
            .load(Ordering::Relaxed);
        snapshot.outbound_quic_datagram_packets =
            self.outbound_quic_datagram_packets.load(Ordering::Relaxed);
        snapshot.outbound_quic_datagram_unavailable_packets = self
            .outbound_quic_datagram_unavailable_packets
            .load(Ordering::Relaxed);
        snapshot.outbound_path_probes_sent = self.outbound_path_probes_sent.load(Ordering::Relaxed);
        snapshot.outbound_path_probe_acks_sent =
            self.outbound_path_probe_acks_sent.load(Ordering::Relaxed);
        snapshot.outbound_path_probe_failures =
            self.outbound_path_probe_failures.load(Ordering::Relaxed);
        snapshot.packet_plane_sessions_expired =
            self.packet_plane_sessions_expired.load(Ordering::Relaxed);
        snapshot.inbound_accepted_packets = self.inbound_accepted_packets.load(Ordering::Relaxed);
        snapshot.inbound_keepalives_accepted =
            self.inbound_keepalives_accepted.load(Ordering::Relaxed);
        snapshot.inbound_path_probes_accepted =
            self.inbound_path_probes_accepted.load(Ordering::Relaxed);
        snapshot.outbound_dropped_packets = self.outbound_dropped_packets.load(Ordering::Relaxed);
        snapshot.inbound_dropped_packets = self.inbound_dropped_packets.load(Ordering::Relaxed);
        snapshot.outbound_failures = self.outbound_failures.load(Ordering::Relaxed);
        snapshot.inbound_failures = self.inbound_failures.load(Ordering::Relaxed);
    }

    fn fill_drop_snapshot(&self, snapshot: &mut RuntimeSnapshot) {
        snapshot.outbound_drop_malformed_packets =
            self.outbound_drop_malformed_packets.load(Ordering::Relaxed);
        snapshot.outbound_drop_no_route_packets =
            self.outbound_drop_no_route_packets.load(Ordering::Relaxed);
        snapshot.outbound_drop_no_transport_peer_packets = self
            .outbound_drop_no_transport_peer_packets
            .load(Ordering::Relaxed);
        snapshot.outbound_drop_packet_too_large_packets = self
            .outbound_drop_packet_too_large_packets
            .load(Ordering::Relaxed);
        snapshot.outbound_drop_queue_full_packets = self
            .outbound_drop_queue_full_packets
            .load(Ordering::Relaxed);
        snapshot.outbound_drop_queue_expired_packets = self
            .outbound_drop_queue_expired_packets
            .load(Ordering::Relaxed);
        snapshot.outbound_drop_unauthorized_source_packets = self
            .outbound_drop_unauthorized_source_packets
            .load(Ordering::Relaxed);
        snapshot.outbound_packet_too_big_notifications = self
            .outbound_packet_too_big_notifications
            .load(Ordering::Relaxed);
        snapshot.outbound_packet_too_big_no_writer = self
            .outbound_packet_too_big_no_writer
            .load(Ordering::Relaxed);
        snapshot.outbound_packet_too_big_unparseable = self
            .outbound_packet_too_big_unparseable
            .load(Ordering::Relaxed);
        snapshot.outbound_packet_too_big_write_failures = self
            .outbound_packet_too_big_write_failures
            .load(Ordering::Relaxed);
        snapshot.inbound_drop_malformed_packets =
            self.inbound_drop_malformed_packets.load(Ordering::Relaxed);
        snapshot.inbound_drop_packet_too_large_packets = self
            .inbound_drop_packet_too_large_packets
            .load(Ordering::Relaxed);
        snapshot.inbound_drop_replay_packets =
            self.inbound_drop_replay_packets.load(Ordering::Relaxed);
        snapshot.inbound_drop_unauthorized_peer_packets = self
            .inbound_drop_unauthorized_peer_packets
            .load(Ordering::Relaxed);
        snapshot.inbound_drop_unauthorized_source_packets = self
            .inbound_drop_unauthorized_source_packets
            .load(Ordering::Relaxed);
        snapshot.inbound_drop_unauthorized_destination_packets = self
            .inbound_drop_unauthorized_destination_packets
            .load(Ordering::Relaxed);
        snapshot.inbound_drop_unexpected_payload_packets = self
            .inbound_drop_unexpected_payload_packets
            .load(Ordering::Relaxed);
        snapshot.inbound_drop_rate_limited_packets = self
            .inbound_drop_rate_limited_packets
            .load(Ordering::Relaxed);
        self.fill_packet_plane_drop_snapshot(snapshot);
    }

    fn fill_packet_plane_drop_snapshot(&self, snapshot: &mut RuntimeSnapshot) {
        snapshot.packet_plane_inbound_dropped_datagrams = self
            .packet_plane_inbound_dropped_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_no_listener_datagrams = self
            .packet_plane_inbound_drop_no_listener_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_no_session_datagrams = self
            .packet_plane_inbound_drop_no_session_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_no_sessions_datagrams = self
            .packet_plane_inbound_drop_no_sessions_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_unknown_endpoint_datagrams = self
            .packet_plane_inbound_drop_unknown_endpoint_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_unexpected_endpoint_datagrams = self
            .packet_plane_inbound_drop_unexpected_endpoint_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_io_error_datagrams = self
            .packet_plane_inbound_drop_io_error_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_encrypt_datagrams = self
            .packet_plane_inbound_drop_encrypt_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_decrypt_datagrams = self
            .packet_plane_inbound_drop_decrypt_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_invalid_magic_datagrams = self
            .packet_plane_inbound_drop_invalid_magic_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_truncated_datagrams = self
            .packet_plane_inbound_drop_truncated_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_unsupported_version_datagrams = self
            .packet_plane_inbound_drop_unsupported_version_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_ciphertext_too_large_datagrams = self
            .packet_plane_inbound_drop_ciphertext_too_large_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_payload_too_large_datagrams = self
            .packet_plane_inbound_drop_payload_too_large_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_frame_decode_datagrams = self
            .packet_plane_inbound_drop_frame_decode_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_frame_length_mismatch_datagrams = self
            .packet_plane_inbound_drop_frame_length_mismatch_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_header_mismatch_datagrams = self
            .packet_plane_inbound_drop_header_mismatch_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_replayed_datagrams = self
            .packet_plane_inbound_drop_replayed_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_outside_replay_window_datagrams = self
            .packet_plane_inbound_drop_outside_replay_window_datagrams
            .load(Ordering::Relaxed);
        snapshot.packet_plane_inbound_drop_trailing_bytes_datagrams = self
            .packet_plane_inbound_drop_trailing_bytes_datagrams
            .load(Ordering::Relaxed);
    }

    fn fill_transport_snapshot(&self, snapshot: &mut RuntimeSnapshot) {
        snapshot.direct_connections_established =
            self.direct_connections_established.load(Ordering::Relaxed);
        snapshot.relayed_connections_established =
            self.relayed_connections_established.load(Ordering::Relaxed);
        snapshot.path_promotions_to_direct = self.path_promotions_to_direct.load(Ordering::Relaxed);
        snapshot.path_fallbacks_to_relay = self.path_fallbacks_to_relay.load(Ordering::Relaxed);
        snapshot.packet_plane_path_demotions =
            self.packet_plane_path_demotions.load(Ordering::Relaxed);
        snapshot.stream_fallback_path_demotions =
            self.stream_fallback_path_demotions.load(Ordering::Relaxed);
        snapshot.packet_plane_path_recovery_dial_attempts = self
            .packet_plane_path_recovery_dial_attempts
            .load(Ordering::Relaxed);
        snapshot.packet_plane_path_recovery_dial_failures = self
            .packet_plane_path_recovery_dial_failures
            .load(Ordering::Relaxed);
        snapshot.outbound_path_mtu_updates = self.outbound_path_mtu_updates.load(Ordering::Relaxed);
        snapshot.outbound_path_mtu_probe_confirmations = self
            .outbound_path_mtu_probe_confirmations
            .load(Ordering::Relaxed);
        snapshot.unauthorized_connections_dropped = self
            .unauthorized_connections_dropped
            .load(Ordering::Relaxed);
        snapshot.relay_reservations_accepted =
            self.relay_reservations_accepted.load(Ordering::Relaxed);
        snapshot.relay_reservations_lost = self.relay_reservations_lost.load(Ordering::Relaxed);
        snapshot.auto_relay_infrastructure_candidates = self
            .auto_relay_infrastructure_candidates
            .load(Ordering::Relaxed);
        snapshot.auto_relay_infrastructure_dial_attempts = self
            .auto_relay_infrastructure_dial_attempts
            .load(Ordering::Relaxed);
        snapshot.auto_relay_infrastructure_dial_failures = self
            .auto_relay_infrastructure_dial_failures
            .load(Ordering::Relaxed);
        snapshot.auto_relay_discovery_queries =
            self.auto_relay_discovery_queries.load(Ordering::Relaxed);
        snapshot.auto_relay_candidates = self.auto_relay_candidates.load(Ordering::Relaxed);
        snapshot.auto_relay_reservation_attempts =
            self.auto_relay_reservation_attempts.load(Ordering::Relaxed);
        snapshot.auto_relay_reservation_failures =
            self.auto_relay_reservation_failures.load(Ordering::Relaxed);
        snapshot.relay_outbound_circuits_established = self
            .relay_outbound_circuits_established
            .load(Ordering::Relaxed);
        snapshot.relay_inbound_circuits_established = self
            .relay_inbound_circuits_established
            .load(Ordering::Relaxed);
        snapshot.relay_server_reservations_accepted = self
            .relay_server_reservations_accepted
            .load(Ordering::Relaxed);
        snapshot.relay_server_reservations_denied = self
            .relay_server_reservations_denied
            .load(Ordering::Relaxed);
        snapshot.relay_server_reservations_closed = self
            .relay_server_reservations_closed
            .load(Ordering::Relaxed);
        snapshot.relay_server_reservations_timed_out = self
            .relay_server_reservations_timed_out
            .load(Ordering::Relaxed);
        snapshot.relay_server_circuits_accepted =
            self.relay_server_circuits_accepted.load(Ordering::Relaxed);
        snapshot.relay_server_circuits_denied =
            self.relay_server_circuits_denied.load(Ordering::Relaxed);
        snapshot.relay_server_circuits_closed =
            self.relay_server_circuits_closed.load(Ordering::Relaxed);
        snapshot.dcutr_successes = self.dcutr_successes.load(Ordering::Relaxed);
        snapshot.dcutr_failures = self.dcutr_failures.load(Ordering::Relaxed);
    }

    fn fill_discovery_snapshot(&self, snapshot: &mut RuntimeSnapshot) {
        snapshot.external_address_candidates =
            self.external_address_candidates.load(Ordering::Relaxed);
        snapshot.external_addresses_confirmed =
            self.external_addresses_confirmed.load(Ordering::Relaxed);
        snapshot.external_addresses_expired =
            self.external_addresses_expired.load(Ordering::Relaxed);
        snapshot.observed_packet_plane_external_addresses = self
            .observed_packet_plane_external_addresses
            .load(Ordering::Relaxed);
        snapshot.observed_packet_plane_external_addresses_rejected = self
            .observed_packet_plane_external_addresses_rejected
            .load(Ordering::Relaxed);
        snapshot.observed_packet_plane_udp_endpoint_candidates = self
            .observed_packet_plane_udp_endpoint_candidates
            .load(Ordering::Relaxed);
        snapshot.observed_packet_plane_quic_endpoint_candidates = self
            .observed_packet_plane_quic_endpoint_candidates
            .load(Ordering::Relaxed);
        snapshot.autonat_probes_scheduled = self.autonat_probes_scheduled.load(Ordering::Relaxed);
        snapshot.autonat_status_unknown = self.autonat_status_unknown.load(Ordering::Relaxed);
        snapshot.autonat_status_public = self.autonat_status_public.load(Ordering::Relaxed);
        snapshot.autonat_status_private = self.autonat_status_private.load(Ordering::Relaxed);
        if snapshot.autonat_status_unknown
            + snapshot.autonat_status_public
            + snapshot.autonat_status_private
            == 0
        {
            snapshot.autonat_status_unknown = 1;
        }
        snapshot.autonat_status_changes_to_unknown = self
            .autonat_status_changes_to_unknown
            .load(Ordering::Relaxed);
        snapshot.autonat_status_changes_to_public = self
            .autonat_status_changes_to_public
            .load(Ordering::Relaxed);
        snapshot.autonat_status_changes_to_private = self
            .autonat_status_changes_to_private
            .load(Ordering::Relaxed);
        snapshot.kademlia_provider_lookups = self.kademlia_provider_lookups.load(Ordering::Relaxed);
        snapshot.kademlia_providers_found = self.kademlia_providers_found.load(Ordering::Relaxed);
        snapshot.kademlia_providers_ignored =
            self.kademlia_providers_ignored.load(Ordering::Relaxed);
        snapshot.kademlia_provider_dial_attempts =
            self.kademlia_provider_dial_attempts.load(Ordering::Relaxed);
        snapshot.kademlia_provider_dial_failures =
            self.kademlia_provider_dial_failures.load(Ordering::Relaxed);
        snapshot.kademlia_provider_advertisements = self
            .kademlia_provider_advertisements
            .load(Ordering::Relaxed);
        snapshot.kademlia_provider_advertisement_failures = self
            .kademlia_provider_advertisement_failures
            .load(Ordering::Relaxed);
        snapshot.kademlia_membership_record_lookups = self
            .kademlia_membership_record_lookups
            .load(Ordering::Relaxed);
        snapshot.kademlia_membership_records_found = self
            .kademlia_membership_records_found
            .load(Ordering::Relaxed);
        snapshot.kademlia_membership_records_accepted = self
            .kademlia_membership_records_accepted
            .load(Ordering::Relaxed);
        snapshot.kademlia_membership_record_invalid = self
            .kademlia_membership_record_invalid
            .load(Ordering::Relaxed);
        snapshot.kademlia_membership_record_publications = self
            .kademlia_membership_record_publications
            .load(Ordering::Relaxed);
        snapshot.kademlia_membership_record_publication_failures = self
            .kademlia_membership_record_publication_failures
            .load(Ordering::Relaxed);
        snapshot.kademlia_bootstrap_refreshes =
            self.kademlia_bootstrap_refreshes.load(Ordering::Relaxed);
        snapshot.kademlia_bootstrap_failures =
            self.kademlia_bootstrap_failures.load(Ordering::Relaxed);
    }

    fn fill_control_snapshot(&self, snapshot: &mut RuntimeSnapshot) {
        snapshot.control_requests_sent = self.control_requests_sent.load(Ordering::Relaxed);
        snapshot.control_requests_received = self.control_requests_received.load(Ordering::Relaxed);
        snapshot.control_responses_received =
            self.control_responses_received.load(Ordering::Relaxed);
        snapshot.control_capability_accepts =
            self.control_capability_accepts.load(Ordering::Relaxed);
        snapshot.control_capability_rejections =
            self.control_capability_rejections.load(Ordering::Relaxed);
        snapshot.control_reject_unauthorized_peer = self
            .control_reject_unauthorized_peer
            .load(Ordering::Relaxed);
        snapshot.control_reject_wrong_network =
            self.control_reject_wrong_network.load(Ordering::Relaxed);
        snapshot.control_reject_membership_mismatch = self
            .control_reject_membership_mismatch
            .load(Ordering::Relaxed);
        snapshot.control_reject_unsupported_wire_version = self
            .control_reject_unsupported_wire_version
            .load(Ordering::Relaxed);
        snapshot.control_reject_unsupported_packet_protocol = self
            .control_reject_unsupported_packet_protocol
            .load(Ordering::Relaxed);
        snapshot.control_reject_unsupported_packet_header_length = self
            .control_reject_unsupported_packet_header_length
            .load(Ordering::Relaxed);
        snapshot.control_reject_invalid_effective_mtu = self
            .control_reject_invalid_effective_mtu
            .load(Ordering::Relaxed);
        snapshot.control_reject_unsupported_preferred_path = self
            .control_reject_unsupported_preferred_path
            .load(Ordering::Relaxed);
        snapshot.control_reject_unauthorized_route_advertisement = self
            .control_reject_unauthorized_route_advertisement
            .load(Ordering::Relaxed);
        snapshot.control_reject_invalid_owned_quic_certificate = self
            .control_reject_invalid_owned_quic_certificate
            .load(Ordering::Relaxed);
        snapshot.control_reject_invalid_membership_record = self
            .control_reject_invalid_membership_record
            .load(Ordering::Relaxed);
        snapshot.control_failures = self.control_failures.load(Ordering::Relaxed);
        snapshot.service_requests_sent = self.service_requests_sent.load(Ordering::Relaxed);
        snapshot.service_requests_received = self.service_requests_received.load(Ordering::Relaxed);
        snapshot.service_responses_received =
            self.service_responses_received.load(Ordering::Relaxed);
        snapshot.service_status_accepts = self.service_status_accepts.load(Ordering::Relaxed);
        snapshot.service_status_rejections = self.service_status_rejections.load(Ordering::Relaxed);
        snapshot.service_reject_unauthorized_peer = self
            .service_reject_unauthorized_peer
            .load(Ordering::Relaxed);
        snapshot.service_reject_wrong_network =
            self.service_reject_wrong_network.load(Ordering::Relaxed);
        snapshot.service_reject_membership_mismatch = self
            .service_reject_membership_mismatch
            .load(Ordering::Relaxed);
        snapshot.service_failures = self.service_failures.load(Ordering::Relaxed);
        snapshot.redial_attempts = self.redial_attempts.load(Ordering::Relaxed);
        snapshot.redial_skipped_connected = self.redial_skipped_connected.load(Ordering::Relaxed);
        snapshot.redial_failures = self.redial_failures.load(Ordering::Relaxed);
        snapshot.outgoing_connection_errors =
            self.outgoing_connection_errors.load(Ordering::Relaxed);
        snapshot.discovered_addresses_accepted =
            self.discovered_addresses_accepted.load(Ordering::Relaxed);
        snapshot.discovered_address_dial_attempts = self
            .discovered_address_dial_attempts
            .load(Ordering::Relaxed);
        snapshot.discovered_address_dial_failures = self
            .discovered_address_dial_failures
            .load(Ordering::Relaxed);
        snapshot.discovered_addresses_rejected =
            self.discovered_addresses_rejected.load(Ordering::Relaxed);
        snapshot.discovered_addresses_expired =
            self.discovered_addresses_expired.load(Ordering::Relaxed);
        snapshot.outbound_queue_blocked_no_supported_path_events = self
            .outbound_queue_blocked_no_supported_path_events
            .load(Ordering::Relaxed);
        snapshot.outbound_queue_blocked_packet_window_events = self
            .outbound_queue_blocked_packet_window_events
            .load(Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub tun_read_packets: u64,
    pub tun_read_bytes: u64,
    pub tun_write_packets: u64,
    pub tun_write_bytes: u64,
    pub outbound_sent_packets: u64,
    pub outbound_stream_fallback_packets: u64,
    pub outbound_quic_datagram_packets: u64,
    pub outbound_quic_datagram_unavailable_packets: u64,
    pub outbound_path_probes_sent: u64,
    pub outbound_path_probe_acks_sent: u64,
    pub outbound_path_probe_failures: u64,
    pub packet_plane_sessions_expired: u64,
    pub inbound_accepted_packets: u64,
    pub inbound_keepalives_accepted: u64,
    pub inbound_path_probes_accepted: u64,
    pub outbound_dropped_packets: u64,
    pub inbound_dropped_packets: u64,
    pub outbound_drop_malformed_packets: u64,
    pub outbound_drop_no_route_packets: u64,
    pub outbound_drop_no_transport_peer_packets: u64,
    pub outbound_drop_packet_too_large_packets: u64,
    pub outbound_drop_queue_full_packets: u64,
    pub outbound_drop_queue_expired_packets: u64,
    pub outbound_drop_unauthorized_source_packets: u64,
    pub outbound_packet_too_big_notifications: u64,
    pub outbound_packet_too_big_no_writer: u64,
    pub outbound_packet_too_big_unparseable: u64,
    pub outbound_packet_too_big_write_failures: u64,
    pub inbound_drop_malformed_packets: u64,
    pub inbound_drop_packet_too_large_packets: u64,
    pub inbound_drop_replay_packets: u64,
    pub inbound_drop_unauthorized_peer_packets: u64,
    pub inbound_drop_unauthorized_source_packets: u64,
    pub inbound_drop_unauthorized_destination_packets: u64,
    pub inbound_drop_unexpected_payload_packets: u64,
    pub inbound_drop_rate_limited_packets: u64,
    pub packet_plane_inbound_dropped_datagrams: u64,
    pub packet_plane_inbound_drop_no_listener_datagrams: u64,
    pub packet_plane_inbound_drop_no_session_datagrams: u64,
    pub packet_plane_inbound_drop_no_sessions_datagrams: u64,
    pub packet_plane_inbound_drop_unknown_endpoint_datagrams: u64,
    pub packet_plane_inbound_drop_unexpected_endpoint_datagrams: u64,
    pub packet_plane_inbound_drop_io_error_datagrams: u64,
    pub packet_plane_inbound_drop_encrypt_datagrams: u64,
    pub packet_plane_inbound_drop_decrypt_datagrams: u64,
    pub packet_plane_inbound_drop_invalid_magic_datagrams: u64,
    pub packet_plane_inbound_drop_truncated_datagrams: u64,
    pub packet_plane_inbound_drop_unsupported_version_datagrams: u64,
    pub packet_plane_inbound_drop_ciphertext_too_large_datagrams: u64,
    pub packet_plane_inbound_drop_payload_too_large_datagrams: u64,
    pub packet_plane_inbound_drop_frame_decode_datagrams: u64,
    pub packet_plane_inbound_drop_frame_length_mismatch_datagrams: u64,
    pub packet_plane_inbound_drop_header_mismatch_datagrams: u64,
    pub packet_plane_inbound_drop_replayed_datagrams: u64,
    pub packet_plane_inbound_drop_outside_replay_window_datagrams: u64,
    pub packet_plane_inbound_drop_trailing_bytes_datagrams: u64,
    pub outbound_failures: u64,
    pub inbound_failures: u64,
    pub direct_connections_established: u64,
    pub relayed_connections_established: u64,
    pub path_promotions_to_direct: u64,
    pub path_fallbacks_to_relay: u64,
    pub packet_plane_path_demotions: u64,
    pub stream_fallback_path_demotions: u64,
    pub packet_plane_path_recovery_dial_attempts: u64,
    pub packet_plane_path_recovery_dial_failures: u64,
    pub outbound_path_mtu_updates: u64,
    pub outbound_path_mtu_probe_confirmations: u64,
    pub unauthorized_connections_dropped: u64,
    pub relay_reservations_accepted: u64,
    pub relay_reservations_lost: u64,
    pub auto_relay_infrastructure_candidates: u64,
    pub auto_relay_infrastructure_dial_attempts: u64,
    pub auto_relay_infrastructure_dial_failures: u64,
    pub auto_relay_discovery_queries: u64,
    pub auto_relay_candidates: u64,
    pub auto_relay_reservation_attempts: u64,
    pub auto_relay_reservation_failures: u64,
    pub relay_outbound_circuits_established: u64,
    pub relay_inbound_circuits_established: u64,
    pub relay_server_reservations_accepted: u64,
    pub relay_server_reservations_denied: u64,
    pub relay_server_reservations_closed: u64,
    pub relay_server_reservations_timed_out: u64,
    pub relay_server_circuits_accepted: u64,
    pub relay_server_circuits_denied: u64,
    pub relay_server_circuits_closed: u64,
    pub dcutr_successes: u64,
    pub dcutr_failures: u64,
    pub external_address_candidates: u64,
    pub external_addresses_confirmed: u64,
    pub external_addresses_expired: u64,
    pub observed_packet_plane_external_addresses: u64,
    pub observed_packet_plane_external_addresses_rejected: u64,
    pub observed_packet_plane_udp_endpoint_candidates: u64,
    pub observed_packet_plane_quic_endpoint_candidates: u64,
    pub autonat_probes_scheduled: u64,
    pub autonat_status_unknown: u64,
    pub autonat_status_public: u64,
    pub autonat_status_private: u64,
    pub autonat_status_changes_to_unknown: u64,
    pub autonat_status_changes_to_public: u64,
    pub autonat_status_changes_to_private: u64,
    pub kademlia_provider_lookups: u64,
    pub kademlia_providers_found: u64,
    pub kademlia_providers_ignored: u64,
    pub kademlia_provider_dial_attempts: u64,
    pub kademlia_provider_dial_failures: u64,
    pub kademlia_provider_advertisements: u64,
    pub kademlia_provider_advertisement_failures: u64,
    pub kademlia_membership_record_lookups: u64,
    pub kademlia_membership_records_found: u64,
    pub kademlia_membership_records_accepted: u64,
    pub kademlia_membership_record_invalid: u64,
    pub kademlia_membership_record_publications: u64,
    pub kademlia_membership_record_publication_failures: u64,
    pub kademlia_bootstrap_refreshes: u64,
    pub kademlia_bootstrap_failures: u64,
    pub control_requests_sent: u64,
    pub control_requests_received: u64,
    pub control_responses_received: u64,
    pub control_capability_accepts: u64,
    pub control_capability_rejections: u64,
    pub control_reject_unauthorized_peer: u64,
    pub control_reject_wrong_network: u64,
    pub control_reject_membership_mismatch: u64,
    pub control_reject_unsupported_wire_version: u64,
    pub control_reject_unsupported_packet_protocol: u64,
    pub control_reject_unsupported_packet_header_length: u64,
    pub control_reject_invalid_effective_mtu: u64,
    pub control_reject_unsupported_preferred_path: u64,
    pub control_reject_unauthorized_route_advertisement: u64,
    pub control_reject_invalid_owned_quic_certificate: u64,
    pub control_reject_invalid_membership_record: u64,
    pub control_failures: u64,
    pub service_requests_sent: u64,
    pub service_requests_received: u64,
    pub service_responses_received: u64,
    pub service_status_accepts: u64,
    pub service_status_rejections: u64,
    pub service_reject_unauthorized_peer: u64,
    pub service_reject_wrong_network: u64,
    pub service_reject_membership_mismatch: u64,
    pub service_failures: u64,
    pub redial_attempts: u64,
    pub redial_skipped_connected: u64,
    pub redial_failures: u64,
    pub outgoing_connection_errors: u64,
    pub discovered_addresses_accepted: u64,
    pub discovered_address_dial_attempts: u64,
    pub discovered_address_dial_failures: u64,
    pub discovered_addresses_rejected: u64,
    pub discovered_addresses_expired: u64,
    pub outbound_queue_blocked_no_supported_path_events: u64,
    pub outbound_queue_blocked_packet_window_events: u64,
    pub queue: QueueStats,
    pub path: PathRuntimeStats,
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
            format!(
                "outbound_stream_fallback_packets {}",
                self.outbound_stream_fallback_packets
            ),
            format!(
                "outbound_quic_datagram_packets {}",
                self.outbound_quic_datagram_packets
            ),
            format!(
                "outbound_quic_datagram_unavailable_packets {}",
                self.outbound_quic_datagram_unavailable_packets
            ),
            format!(
                "outbound_path_probes_sent {}",
                self.outbound_path_probes_sent
            ),
            format!(
                "outbound_path_probe_acks_sent {}",
                self.outbound_path_probe_acks_sent
            ),
            format!(
                "outbound_path_probe_failures {}",
                self.outbound_path_probe_failures
            ),
            format!(
                "packet_plane_sessions_expired {}",
                self.packet_plane_sessions_expired
            ),
            format!(
                "outbound_packet_too_big_notifications {}",
                self.outbound_packet_too_big_notifications
            ),
            format!(
                "outbound_packet_too_big_no_writer {}",
                self.outbound_packet_too_big_no_writer
            ),
            format!(
                "outbound_packet_too_big_unparseable {}",
                self.outbound_packet_too_big_unparseable
            ),
            format!(
                "outbound_packet_too_big_write_failures {}",
                self.outbound_packet_too_big_write_failures
            ),
            format!("inbound_accepted_packets {}", self.inbound_accepted_packets),
            format!(
                "inbound_keepalives_accepted {}",
                self.inbound_keepalives_accepted
            ),
            format!(
                "inbound_path_probes_accepted {}",
                self.inbound_path_probes_accepted
            ),
        ];
        self.extend_drop_lines(&mut lines);
        self.extend_transport_lines(&mut lines);
        self.extend_discovery_lines(&mut lines);
        self.extend_control_and_queue_lines(&mut lines);
        lines
    }

    #[must_use]
    pub fn prometheus_lines(&self) -> Vec<String> {
        prometheus_lines_from_metric_lines(&self.lines())
    }

    fn extend_transport_lines(&self, lines: &mut Vec<String>) {
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
                "path_promotions_to_direct {}",
                self.path_promotions_to_direct
            ),
            format!("path_fallbacks_to_relay {}", self.path_fallbacks_to_relay),
            format!(
                "packet_plane_path_demotions {}",
                self.packet_plane_path_demotions
            ),
            format!(
                "stream_fallback_path_demotions {}",
                self.stream_fallback_path_demotions
            ),
            format!(
                "packet_plane_path_recovery_dial_attempts {}",
                self.packet_plane_path_recovery_dial_attempts
            ),
            format!(
                "packet_plane_path_recovery_dial_failures {}",
                self.packet_plane_path_recovery_dial_failures
            ),
            format!(
                "outbound_path_mtu_updates {}",
                self.outbound_path_mtu_updates
            ),
            format!(
                "outbound_path_mtu_probe_confirmations {}",
                self.outbound_path_mtu_probe_confirmations
            ),
            format!(
                "unauthorized_connections_dropped {}",
                self.unauthorized_connections_dropped
            ),
            format!(
                "relay_reservations_accepted {}",
                self.relay_reservations_accepted
            ),
            format!("relay_reservations_lost {}", self.relay_reservations_lost),
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
                "relay_server_reservations_denied {}",
                self.relay_server_reservations_denied
            ),
            format!(
                "relay_server_reservations_closed {}",
                self.relay_server_reservations_closed
            ),
            format!(
                "relay_server_reservations_timed_out {}",
                self.relay_server_reservations_timed_out
            ),
            format!(
                "relay_server_circuits_accepted {}",
                self.relay_server_circuits_accepted
            ),
            format!(
                "relay_server_circuits_denied {}",
                self.relay_server_circuits_denied
            ),
            format!(
                "relay_server_circuits_closed {}",
                self.relay_server_circuits_closed
            ),
        ]);
        self.extend_auto_relay_lines(lines);
    }

    fn extend_auto_relay_lines(&self, lines: &mut Vec<String>) {
        lines.extend([
            format!(
                "auto_relay_infrastructure_candidates {}",
                self.auto_relay_infrastructure_candidates
            ),
            format!(
                "auto_relay_infrastructure_dial_attempts {}",
                self.auto_relay_infrastructure_dial_attempts
            ),
            format!(
                "auto_relay_infrastructure_dial_failures {}",
                self.auto_relay_infrastructure_dial_failures
            ),
            format!(
                "auto_relay_discovery_queries {}",
                self.auto_relay_discovery_queries
            ),
            format!("auto_relay_candidates {}", self.auto_relay_candidates),
            format!(
                "auto_relay_reservation_attempts {}",
                self.auto_relay_reservation_attempts
            ),
            format!(
                "auto_relay_reservation_failures {}",
                self.auto_relay_reservation_failures
            ),
        ]);
    }

    fn extend_discovery_lines(&self, lines: &mut Vec<String>) {
        lines.extend([
            format!("dcutr_successes {}", self.dcutr_successes),
            format!("dcutr_failures {}", self.dcutr_failures),
            format!(
                "external_address_candidates {}",
                self.external_address_candidates
            ),
            format!(
                "external_addresses_confirmed {}",
                self.external_addresses_confirmed
            ),
            format!(
                "external_addresses_expired {}",
                self.external_addresses_expired
            ),
            format!("autonat_probes_scheduled {}", self.autonat_probes_scheduled),
            format!("autonat_status_unknown {}", self.autonat_status_unknown),
            format!("autonat_status_public {}", self.autonat_status_public),
            format!("autonat_status_private {}", self.autonat_status_private),
            format!(
                "autonat_status_changes_to_unknown {}",
                self.autonat_status_changes_to_unknown
            ),
            format!(
                "autonat_status_changes_to_public {}",
                self.autonat_status_changes_to_public
            ),
            format!(
                "autonat_status_changes_to_private {}",
                self.autonat_status_changes_to_private
            ),
            format!(
                "kademlia_provider_lookups {}",
                self.kademlia_provider_lookups
            ),
            format!("kademlia_providers_found {}", self.kademlia_providers_found),
            format!(
                "kademlia_providers_ignored {}",
                self.kademlia_providers_ignored
            ),
            format!(
                "kademlia_provider_dial_attempts {}",
                self.kademlia_provider_dial_attempts
            ),
            format!(
                "kademlia_provider_dial_failures {}",
                self.kademlia_provider_dial_failures
            ),
            format!(
                "kademlia_provider_advertisements {}",
                self.kademlia_provider_advertisements
            ),
            format!(
                "kademlia_provider_advertisement_failures {}",
                self.kademlia_provider_advertisement_failures
            ),
            format!(
                "kademlia_membership_record_lookups {}",
                self.kademlia_membership_record_lookups
            ),
            format!(
                "kademlia_membership_records_found {}",
                self.kademlia_membership_records_found
            ),
            format!(
                "kademlia_membership_records_accepted {}",
                self.kademlia_membership_records_accepted
            ),
            format!(
                "kademlia_membership_record_invalid {}",
                self.kademlia_membership_record_invalid
            ),
            format!(
                "kademlia_membership_record_publications {}",
                self.kademlia_membership_record_publications
            ),
            format!(
                "kademlia_membership_record_publication_failures {}",
                self.kademlia_membership_record_publication_failures
            ),
            format!(
                "kademlia_bootstrap_refreshes {}",
                self.kademlia_bootstrap_refreshes
            ),
            format!(
                "kademlia_bootstrap_failures {}",
                self.kademlia_bootstrap_failures
            ),
        ]);
        self.extend_observed_packet_plane_lines(lines);
    }

    fn extend_observed_packet_plane_lines(&self, lines: &mut Vec<String>) {
        lines.extend([
            format!(
                "observed_packet_plane_external_addresses {}",
                self.observed_packet_plane_external_addresses
            ),
            format!(
                "observed_packet_plane_external_addresses_rejected {}",
                self.observed_packet_plane_external_addresses_rejected
            ),
            format!(
                "observed_packet_plane_udp_endpoint_candidates {}",
                self.observed_packet_plane_udp_endpoint_candidates
            ),
            format!(
                "observed_packet_plane_quic_endpoint_candidates {}",
                self.observed_packet_plane_quic_endpoint_candidates
            ),
        ]);
    }

    fn extend_control_and_queue_lines(&self, lines: &mut Vec<String>) {
        self.extend_control_lines(lines);
        self.extend_runtime_state_lines(lines);
        self.extend_path_and_queue_lines(lines);
    }

    fn extend_control_lines(&self, lines: &mut Vec<String>) {
        lines.extend([
            format!("control_requests_sent {}", self.control_requests_sent),
            format!(
                "control_requests_received {}",
                self.control_requests_received
            ),
            format!(
                "control_responses_received {}",
                self.control_responses_received
            ),
            format!(
                "control_capability_accepts {}",
                self.control_capability_accepts
            ),
            format!(
                "control_capability_rejections {}",
                self.control_capability_rejections
            ),
            format!(
                "control_reject_unauthorized_peer {}",
                self.control_reject_unauthorized_peer
            ),
            format!(
                "control_reject_wrong_network {}",
                self.control_reject_wrong_network
            ),
            format!(
                "control_reject_membership_mismatch {}",
                self.control_reject_membership_mismatch
            ),
            format!(
                "control_reject_unsupported_wire_version {}",
                self.control_reject_unsupported_wire_version
            ),
            format!(
                "control_reject_unsupported_packet_protocol {}",
                self.control_reject_unsupported_packet_protocol
            ),
            format!(
                "control_reject_unsupported_packet_header_length {}",
                self.control_reject_unsupported_packet_header_length
            ),
            format!(
                "control_reject_invalid_effective_mtu {}",
                self.control_reject_invalid_effective_mtu
            ),
            format!(
                "control_reject_unsupported_preferred_path {}",
                self.control_reject_unsupported_preferred_path
            ),
            format!(
                "control_reject_unauthorized_route_advertisement {}",
                self.control_reject_unauthorized_route_advertisement
            ),
            format!(
                "control_reject_invalid_owned_quic_certificate {}",
                self.control_reject_invalid_owned_quic_certificate
            ),
            format!(
                "control_reject_invalid_membership_record {}",
                self.control_reject_invalid_membership_record
            ),
            format!("control_failures {}", self.control_failures),
            format!("service_requests_sent {}", self.service_requests_sent),
            format!(
                "service_requests_received {}",
                self.service_requests_received
            ),
            format!(
                "service_responses_received {}",
                self.service_responses_received
            ),
            format!("service_status_accepts {}", self.service_status_accepts),
            format!(
                "service_status_rejections {}",
                self.service_status_rejections
            ),
            format!(
                "service_reject_unauthorized_peer {}",
                self.service_reject_unauthorized_peer
            ),
            format!(
                "service_reject_wrong_network {}",
                self.service_reject_wrong_network
            ),
            format!(
                "service_reject_membership_mismatch {}",
                self.service_reject_membership_mismatch
            ),
            format!("service_failures {}", self.service_failures),
        ]);
    }

    fn extend_runtime_state_lines(&self, lines: &mut Vec<String>) {
        lines.extend([
            format!("redial_attempts {}", self.redial_attempts),
            format!("redial_skipped_connected {}", self.redial_skipped_connected),
            format!("redial_failures {}", self.redial_failures),
            format!(
                "outgoing_connection_errors {}",
                self.outgoing_connection_errors
            ),
            format!(
                "discovered_addresses_accepted {}",
                self.discovered_addresses_accepted
            ),
            format!(
                "discovered_address_dial_attempts {}",
                self.discovered_address_dial_attempts
            ),
            format!(
                "discovered_address_dial_failures {}",
                self.discovered_address_dial_failures
            ),
            format!(
                "discovered_addresses_rejected {}",
                self.discovered_addresses_rejected
            ),
            format!(
                "discovered_addresses_expired {}",
                self.discovered_addresses_expired
            ),
            format!(
                "outbound_queue_blocked_no_supported_path_events {}",
                self.outbound_queue_blocked_no_supported_path_events
            ),
            format!(
                "outbound_queue_blocked_packet_window_events {}",
                self.outbound_queue_blocked_packet_window_events
            ),
        ]);
    }

    fn extend_path_and_queue_lines(&self, lines: &mut Vec<String>) {
        lines.extend([
            format!(
                "path_healthy_direct_udp_datagram_paths {}",
                self.path.healthy_direct_udp_datagram_paths
            ),
            format!(
                "path_healthy_direct_quic_datagram_paths {}",
                self.path.healthy_direct_quic_datagram_paths
            ),
            format!(
                "path_healthy_direct_quic_stream_paths {}",
                self.path.healthy_direct_quic_stream_paths
            ),
            format!(
                "path_healthy_direct_tcp_stream_paths {}",
                self.path.healthy_direct_tcp_stream_paths
            ),
            format!("path_healthy_relay_paths {}", self.path.healthy_relay_paths),
            format!(
                "path_peers_with_supported_path {}",
                self.path.peers_with_supported_path
            ),
            format!(
                "path_peers_without_supported_path {}",
                self.path.peers_without_supported_path
            ),
            format!("queue_queued_packets {}", self.queue.queued_packets),
            format!("queue_queued_bytes {}", self.queue.queued_bytes),
            format!(
                "queue_oldest_packet_age_millis {}",
                self.queue.oldest_packet_age_millis
            ),
            format!("queue_dropped_packets {}", self.queue.dropped_packets),
            format!("queue_dropped_bytes {}", self.queue.dropped_bytes),
            format!("queue_expired_packets {}", self.queue.expired_packets),
            format!("queue_expired_bytes {}", self.queue.expired_bytes),
        ]);
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
                "outbound_drop_queue_expired_packets {}",
                self.outbound_drop_queue_expired_packets
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
            format!(
                "inbound_drop_rate_limited_packets {}",
                self.inbound_drop_rate_limited_packets
            ),
        ]);
        self.extend_packet_plane_drop_lines(lines);
    }

    fn extend_packet_plane_drop_lines(&self, lines: &mut Vec<String>) {
        lines.extend([
            format!(
                "packet_plane_inbound_dropped_datagrams {}",
                self.packet_plane_inbound_dropped_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_no_listener_datagrams {}",
                self.packet_plane_inbound_drop_no_listener_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_no_session_datagrams {}",
                self.packet_plane_inbound_drop_no_session_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_no_sessions_datagrams {}",
                self.packet_plane_inbound_drop_no_sessions_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_unknown_endpoint_datagrams {}",
                self.packet_plane_inbound_drop_unknown_endpoint_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_unexpected_endpoint_datagrams {}",
                self.packet_plane_inbound_drop_unexpected_endpoint_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_io_error_datagrams {}",
                self.packet_plane_inbound_drop_io_error_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_encrypt_datagrams {}",
                self.packet_plane_inbound_drop_encrypt_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_decrypt_datagrams {}",
                self.packet_plane_inbound_drop_decrypt_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_invalid_magic_datagrams {}",
                self.packet_plane_inbound_drop_invalid_magic_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_truncated_datagrams {}",
                self.packet_plane_inbound_drop_truncated_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_unsupported_version_datagrams {}",
                self.packet_plane_inbound_drop_unsupported_version_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_ciphertext_too_large_datagrams {}",
                self.packet_plane_inbound_drop_ciphertext_too_large_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_payload_too_large_datagrams {}",
                self.packet_plane_inbound_drop_payload_too_large_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_frame_decode_datagrams {}",
                self.packet_plane_inbound_drop_frame_decode_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_frame_length_mismatch_datagrams {}",
                self.packet_plane_inbound_drop_frame_length_mismatch_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_header_mismatch_datagrams {}",
                self.packet_plane_inbound_drop_header_mismatch_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_replayed_datagrams {}",
                self.packet_plane_inbound_drop_replayed_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_outside_replay_window_datagrams {}",
                self.packet_plane_inbound_drop_outside_replay_window_datagrams
            ),
            format!(
                "packet_plane_inbound_drop_trailing_bytes_datagrams {}",
                self.packet_plane_inbound_drop_trailing_bytes_datagrams
            ),
        ]);
    }
}

#[must_use]
pub fn prometheus_lines_from_metric_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| prometheus_line_from_metric_line(line))
        .collect()
}

fn prometheus_line_from_metric_line(line: &str) -> Option<String> {
    let (name, value) = line.split_once(' ')?;
    if value.contains(' ') || !value.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    if !is_prometheus_metric_suffix(name) {
        return None;
    }

    Some(format!("p2p_vpn_{name} {value}"))
}

fn is_prometheus_metric_suffix(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }

    characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn populated_snapshot() -> RuntimeSnapshot {
        let metrics = RuntimeMetrics::default();
        populate_packet_metrics(&metrics);
        populate_transport_and_discovery_metrics(&metrics);
        populate_control_and_service_metrics(&metrics);
        populate_runtime_state_metrics(&metrics);

        metrics.snapshot_with_paths(populated_queue_stats(), populated_path_stats())
    }

    fn populate_packet_metrics(metrics: &RuntimeMetrics) {
        metrics.record_tun_read(20);
        metrics.record_tun_write(40);
        metrics.record_outbound_sent();
        metrics.record_outbound_stream_fallback();
        metrics.record_outbound_quic_datagram();
        metrics.record_outbound_quic_datagram_unavailable();
        metrics.record_outbound_path_probe_sent();
        metrics.record_outbound_path_probe_ack_sent();
        metrics.record_outbound_path_probe_failure();
        metrics.record_packet_plane_session_expired();
        metrics.record_inbound_accepted();
        metrics.record_inbound_keepalive_accepted();
        metrics.record_inbound_path_probe_accepted();
        metrics.record_outbound_drop(PacketDropReason::NoRoute);
        metrics.record_outbound_drop(PacketDropReason::PacketTooLarge);
        metrics.record_outbound_drop(PacketDropReason::QueueFull);
        metrics.record_outbound_queue_expired(2);
        metrics.record_outbound_drop(PacketDropReason::UnauthorizedSource);
        metrics.record_outbound_packet_too_big_notification();
        metrics.record_outbound_packet_too_big_no_writer();
        metrics.record_outbound_packet_too_big_unparseable();
        metrics.record_outbound_packet_too_big_write_failure();
        metrics.record_inbound_drop(PacketDropReason::MalformedPacket);
        metrics.record_inbound_drop(PacketDropReason::PacketTooLarge);
        metrics.record_inbound_drop(PacketDropReason::Replay);
        metrics.record_inbound_drop(PacketDropReason::UnauthorizedPeer);
        metrics.record_inbound_drop(PacketDropReason::UnauthorizedSource);
        metrics.record_inbound_drop(PacketDropReason::UnauthorizedDestination);
        metrics.record_inbound_drop(PacketDropReason::UnexpectedPayload);
        metrics.record_inbound_drop(PacketDropReason::RateLimited);
        for reason in [
            PacketPlaneDropReason::NoListener,
            PacketPlaneDropReason::NoSession,
            PacketPlaneDropReason::NoSessions,
            PacketPlaneDropReason::UnknownEndpoint,
            PacketPlaneDropReason::UnexpectedEndpoint,
            PacketPlaneDropReason::IoError,
            PacketPlaneDropReason::Encrypt,
            PacketPlaneDropReason::Decrypt,
            PacketPlaneDropReason::InvalidMagic,
            PacketPlaneDropReason::Truncated,
            PacketPlaneDropReason::UnsupportedVersion,
            PacketPlaneDropReason::CiphertextTooLarge,
            PacketPlaneDropReason::PayloadTooLarge,
            PacketPlaneDropReason::FrameDecode,
            PacketPlaneDropReason::FrameLengthMismatch,
            PacketPlaneDropReason::HeaderMismatch,
            PacketPlaneDropReason::ReplayedDatagram,
            PacketPlaneDropReason::DatagramOutsideReplayWindow,
            PacketPlaneDropReason::TrailingBytes,
        ] {
            metrics.record_packet_plane_inbound_drop(reason);
        }
        metrics.record_outbound_failure();
        metrics.record_inbound_failure();
    }

    fn populate_transport_and_discovery_metrics(metrics: &RuntimeMetrics) {
        metrics.record_connection_established(false);
        metrics.record_connection_established(true);
        metrics.record_path_promotion_to_direct();
        metrics.record_path_fallback_to_relay();
        metrics.record_packet_plane_path_demotion();
        metrics.record_stream_fallback_path_demotion();
        metrics.record_packet_plane_path_recovery_dial_attempt();
        metrics.record_packet_plane_path_recovery_dial_failure();
        metrics.record_outbound_path_mtu_update();
        metrics.record_outbound_path_mtu_probe_confirmation();
        metrics.record_unauthorized_connection_dropped();
        metrics.record_relay_reservation_accepted();
        metrics.record_relay_reservation_lost();
        metrics.record_auto_relay_infrastructure_candidate();
        metrics.record_auto_relay_infrastructure_dial_attempt();
        metrics.record_auto_relay_infrastructure_dial_failure();
        metrics.record_auto_relay_discovery_query();
        metrics.record_auto_relay_candidate();
        metrics.record_auto_relay_reservation_attempt();
        metrics.record_auto_relay_reservation_failure();
        metrics.record_relay_outbound_circuit_established();
        metrics.record_relay_inbound_circuit_established();
        metrics.record_relay_server_reservation_accepted();
        metrics.record_relay_server_reservation_denied();
        metrics.record_relay_server_reservation_closed();
        metrics.record_relay_server_reservation_timed_out();
        metrics.record_relay_server_circuit_accepted();
        metrics.record_relay_server_circuit_denied();
        metrics.record_relay_server_circuit_closed();
        metrics.record_dcutr_result(true);
        metrics.record_dcutr_result(false);
        metrics.record_external_address_candidate();
        metrics.record_external_address_confirmed();
        metrics.record_external_address_expired();
        metrics.record_observed_packet_plane_external_address();
        metrics.record_observed_packet_plane_external_address_rejected();
        metrics.record_observed_packet_plane_udp_endpoint_candidate();
        metrics.record_observed_packet_plane_quic_endpoint_candidate();
        metrics.record_autonat_probe_scheduled();
        metrics.record_autonat_status(AutoNatReachability::Public);
        metrics.record_autonat_status(AutoNatReachability::Private);
        metrics.record_kademlia_provider_lookup();
        metrics.record_kademlia_providers_found(2);
        metrics.record_kademlia_provider_ignored();
        metrics.record_kademlia_provider_dial_attempt();
        metrics.record_kademlia_provider_dial_failure();
        metrics.record_kademlia_provider_advertisement();
        metrics.record_kademlia_provider_advertisement_failure();
        metrics.record_kademlia_membership_record_lookup();
        metrics.record_kademlia_membership_records_found();
        metrics.record_kademlia_membership_records_accepted(2);
        metrics.record_kademlia_membership_record_invalid();
        metrics.record_kademlia_membership_record_publication();
        metrics.record_kademlia_membership_record_publication_failure();
        metrics.record_kademlia_bootstrap_refresh();
        metrics.record_kademlia_bootstrap_failure();
    }

    fn populate_control_and_service_metrics(metrics: &RuntimeMetrics) {
        metrics.record_control_request_sent();
        metrics.record_control_request_received();
        metrics.record_control_response_received();
        metrics.record_control_capability_accept();
        for reason in [
            ControlRejectionReason::UnauthorizedPeer,
            ControlRejectionReason::WrongNetwork,
            ControlRejectionReason::MembershipMismatch,
            ControlRejectionReason::UnsupportedWireVersion,
            ControlRejectionReason::UnsupportedPacketProtocol,
            ControlRejectionReason::UnsupportedPacketHeaderLength,
            ControlRejectionReason::InvalidEffectiveMtu,
            ControlRejectionReason::UnsupportedPreferredPath,
            ControlRejectionReason::UnauthorizedRouteAdvertisement,
            ControlRejectionReason::InvalidOwnedQuicCertificate,
            ControlRejectionReason::InvalidMembershipRecord,
        ] {
            metrics.record_control_capability_rejection(reason);
        }
        metrics.record_control_failure();
        metrics.record_service_request_sent();
        metrics.record_service_request_received();
        metrics.record_service_response_received();
        metrics.record_service_status_accept();
        for reason in [
            crate::runtime::service::ServiceRejectionReason::UnauthorizedPeer,
            crate::runtime::service::ServiceRejectionReason::WrongNetwork,
            crate::runtime::service::ServiceRejectionReason::MembershipMismatch,
        ] {
            metrics.record_service_status_rejection(reason);
        }
        metrics.record_service_failure();
    }

    fn populate_runtime_state_metrics(metrics: &RuntimeMetrics) {
        metrics.record_redial_attempt();
        metrics.record_redial_skipped_connected();
        metrics.record_redial_failure();
        metrics.record_outgoing_connection_error();
        metrics.record_discovered_address_accepted();
        metrics.record_discovered_address_dial_attempt();
        metrics.record_discovered_address_dial_failure();
        metrics.record_discovered_address_rejected();
        metrics.record_discovered_address_expired(2);
        metrics.record_outbound_queue_blocked_no_supported_path();
        metrics.record_outbound_queue_blocked_packet_window();
    }

    fn populated_queue_stats() -> QueueStats {
        QueueStats {
            queued_packets: 2,
            queued_bytes: 80,
            oldest_packet_age_millis: 45,
            dropped_packets: 3,
            dropped_bytes: 120,
            expired_packets: 2,
            expired_bytes: 60,
        }
    }

    fn populated_path_stats() -> PathRuntimeStats {
        PathRuntimeStats {
            healthy_direct_udp_datagram_paths: 1,
            healthy_direct_quic_datagram_paths: 2,
            healthy_direct_quic_stream_paths: 3,
            healthy_direct_tcp_stream_paths: 4,
            healthy_relay_paths: 5,
            peers_with_supported_path: 6,
            peers_without_supported_path: 7,
        }
    }

    fn assert_metric_line(snapshot: &RuntimeSnapshot, line: &str) {
        assert!(snapshot.lines().contains(&line.to_owned()));
    }

    fn assert_observed_packet_plane_lines(snapshot: &RuntimeSnapshot) {
        assert_metric_line(snapshot, "observed_packet_plane_external_addresses 1");
        assert_metric_line(
            snapshot,
            "observed_packet_plane_external_addresses_rejected 1",
        );
        assert_metric_line(snapshot, "observed_packet_plane_udp_endpoint_candidates 1");
        assert_metric_line(snapshot, "observed_packet_plane_quic_endpoint_candidates 1");
    }

    fn assert_kademlia_discovery_counters(snapshot: &RuntimeSnapshot) {
        assert_eq!(snapshot.kademlia_provider_lookups, 1);
        assert_eq!(snapshot.kademlia_providers_found, 2);
        assert_eq!(snapshot.kademlia_providers_ignored, 1);
        assert_eq!(snapshot.kademlia_provider_dial_attempts, 1);
        assert_eq!(snapshot.kademlia_provider_dial_failures, 1);
        assert_eq!(snapshot.kademlia_provider_advertisements, 1);
        assert_eq!(snapshot.kademlia_provider_advertisement_failures, 1);
        assert_eq!(snapshot.kademlia_membership_record_lookups, 1);
        assert_eq!(snapshot.kademlia_membership_records_found, 1);
        assert_eq!(snapshot.kademlia_membership_records_accepted, 2);
        assert_eq!(snapshot.kademlia_membership_record_invalid, 1);
        assert_eq!(snapshot.kademlia_membership_record_publications, 1);
        assert_eq!(snapshot.kademlia_membership_record_publication_failures, 1);
        assert_eq!(snapshot.kademlia_bootstrap_refreshes, 1);
        assert_eq!(snapshot.kademlia_bootstrap_failures, 1);
    }

    fn assert_kademlia_discovery_lines(snapshot: &RuntimeSnapshot) {
        assert_metric_line(snapshot, "kademlia_provider_lookups 1");
        assert_metric_line(snapshot, "kademlia_providers_found 2");
        assert_metric_line(snapshot, "kademlia_provider_dial_attempts 1");
        assert_metric_line(snapshot, "kademlia_provider_dial_failures 1");
        assert_metric_line(snapshot, "kademlia_provider_advertisements 1");
        assert_metric_line(snapshot, "kademlia_provider_advertisement_failures 1");
        assert_metric_line(snapshot, "kademlia_membership_record_lookups 1");
        assert_metric_line(snapshot, "kademlia_membership_records_found 1");
        assert_metric_line(snapshot, "kademlia_membership_records_accepted 2");
        assert_metric_line(snapshot, "kademlia_membership_record_invalid 1");
        assert_metric_line(snapshot, "kademlia_membership_record_publications 1");
        assert_metric_line(
            snapshot,
            "kademlia_membership_record_publication_failures 1",
        );
        assert_metric_line(snapshot, "kademlia_bootstrap_refreshes 1");
        assert_metric_line(snapshot, "kademlia_bootstrap_failures 1");
    }

    #[test]
    fn snapshot_prometheus_lines_prefix_numeric_metrics() {
        let snapshot = populated_snapshot();
        let lines = snapshot.prometheus_lines();

        assert!(lines.contains(&"p2p_vpn_tun_read_packets 1".to_owned()));
        assert!(lines.contains(&"p2p_vpn_tun_read_bytes 20".to_owned()));
        assert!(lines.contains(&"p2p_vpn_queue_queued_packets 2".to_owned()));
        assert!(lines.contains(&"p2p_vpn_path_peers_with_supported_path 6".to_owned()));
    }

    #[test]
    fn prometheus_lines_filter_human_status_lines() {
        let lines = prometheus_lines_from_metric_lines(&[
            "network: lab".to_owned(),
            "runtime metrics:".to_owned(),
            "tun_read_packets 7".to_owned(),
            "packet_plane_session_ttl_seconds 600".to_owned(),
            "peer state: abc".to_owned(),
            "invalid-metric 1".to_owned(),
            "queue_queued_packets not-a-number".to_owned(),
        ]);

        assert_eq!(
            lines,
            vec![
                "p2p_vpn_tun_read_packets 7".to_owned(),
                "p2p_vpn_packet_plane_session_ttl_seconds 600".to_owned(),
            ]
        );
    }

    fn assert_packet_drop_counters(snapshot: &RuntimeSnapshot) {
        assert_eq!(snapshot.outbound_dropped_packets, 6);
        assert_eq!(snapshot.inbound_dropped_packets, 8);
        assert_eq!(snapshot.outbound_drop_no_route_packets, 1);
        assert_eq!(snapshot.outbound_drop_packet_too_large_packets, 1);
        assert_eq!(snapshot.outbound_drop_queue_full_packets, 1);
        assert_eq!(snapshot.outbound_drop_queue_expired_packets, 2);
        assert_eq!(snapshot.outbound_drop_unauthorized_source_packets, 1);
        assert_eq!(snapshot.outbound_packet_too_big_notifications, 1);
        assert_eq!(snapshot.outbound_packet_too_big_no_writer, 1);
        assert_eq!(snapshot.outbound_packet_too_big_unparseable, 1);
        assert_eq!(snapshot.outbound_packet_too_big_write_failures, 1);
        assert_eq!(snapshot.inbound_drop_malformed_packets, 1);
        assert_eq!(snapshot.inbound_drop_packet_too_large_packets, 1);
        assert_eq!(snapshot.inbound_drop_replay_packets, 1);
        assert_eq!(snapshot.inbound_drop_unauthorized_peer_packets, 1);
        assert_eq!(snapshot.inbound_drop_unauthorized_source_packets, 1);
        assert_eq!(snapshot.inbound_drop_unauthorized_destination_packets, 1);
        assert_eq!(snapshot.inbound_drop_unexpected_payload_packets, 1);
        assert_eq!(snapshot.inbound_drop_rate_limited_packets, 1);
        assert_eq!(snapshot.packet_plane_inbound_dropped_datagrams, 19);
        assert_eq!(snapshot.packet_plane_inbound_drop_no_listener_datagrams, 1);
        assert_eq!(snapshot.packet_plane_inbound_drop_no_session_datagrams, 1);
        assert_eq!(snapshot.packet_plane_inbound_drop_no_sessions_datagrams, 1);
        assert_eq!(
            snapshot.packet_plane_inbound_drop_unknown_endpoint_datagrams,
            1
        );
        assert_eq!(
            snapshot.packet_plane_inbound_drop_unexpected_endpoint_datagrams,
            1
        );
        assert_eq!(snapshot.packet_plane_inbound_drop_io_error_datagrams, 1);
        assert_eq!(snapshot.packet_plane_inbound_drop_encrypt_datagrams, 1);
        assert_eq!(snapshot.packet_plane_inbound_drop_decrypt_datagrams, 1);
        assert_eq!(
            snapshot.packet_plane_inbound_drop_invalid_magic_datagrams,
            1
        );
        assert_eq!(snapshot.packet_plane_inbound_drop_truncated_datagrams, 1);
        assert_eq!(
            snapshot.packet_plane_inbound_drop_unsupported_version_datagrams,
            1
        );
        assert_eq!(
            snapshot.packet_plane_inbound_drop_ciphertext_too_large_datagrams,
            1
        );
        assert_eq!(
            snapshot.packet_plane_inbound_drop_payload_too_large_datagrams,
            1
        );
        assert_eq!(snapshot.packet_plane_inbound_drop_frame_decode_datagrams, 1);
        assert_eq!(
            snapshot.packet_plane_inbound_drop_frame_length_mismatch_datagrams,
            1
        );
        assert_eq!(
            snapshot.packet_plane_inbound_drop_header_mismatch_datagrams,
            1
        );
        assert_eq!(snapshot.packet_plane_inbound_drop_replayed_datagrams, 1);
        assert_eq!(
            snapshot.packet_plane_inbound_drop_outside_replay_window_datagrams,
            1
        );
        assert_eq!(
            snapshot.packet_plane_inbound_drop_trailing_bytes_datagrams,
            1
        );
    }

    fn assert_packet_drop_lines(snapshot: &RuntimeSnapshot) {
        assert_metric_line(snapshot, "outbound_drop_no_route_packets 1");
        assert_metric_line(snapshot, "outbound_drop_packet_too_large_packets 1");
        assert_metric_line(snapshot, "outbound_drop_queue_full_packets 1");
        assert_metric_line(snapshot, "outbound_drop_queue_expired_packets 2");
        assert_metric_line(snapshot, "outbound_drop_unauthorized_source_packets 1");
        assert_metric_line(snapshot, "outbound_packet_too_big_notifications 1");
        assert_metric_line(snapshot, "outbound_packet_too_big_no_writer 1");
        assert_metric_line(snapshot, "outbound_packet_too_big_unparseable 1");
        assert_metric_line(snapshot, "outbound_packet_too_big_write_failures 1");
        assert_metric_line(snapshot, "inbound_drop_malformed_packets 1");
        assert_metric_line(snapshot, "inbound_drop_packet_too_large_packets 1");
        assert_metric_line(snapshot, "inbound_drop_replay_packets 1");
        assert_metric_line(snapshot, "inbound_drop_unauthorized_peer_packets 1");
        assert_metric_line(snapshot, "inbound_drop_unauthorized_source_packets 1");
        assert_metric_line(snapshot, "inbound_drop_unauthorized_destination_packets 1");
        assert_metric_line(snapshot, "inbound_drop_unexpected_payload_packets 1");
        assert_metric_line(snapshot, "inbound_drop_rate_limited_packets 1");
        assert_metric_line(snapshot, "packet_plane_inbound_dropped_datagrams 19");
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_no_listener_datagrams 1",
        );
        assert_metric_line(snapshot, "packet_plane_inbound_drop_no_session_datagrams 1");
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_no_sessions_datagrams 1",
        );
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_unknown_endpoint_datagrams 1",
        );
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_unexpected_endpoint_datagrams 1",
        );
        assert_metric_line(snapshot, "packet_plane_inbound_drop_io_error_datagrams 1");
        assert_metric_line(snapshot, "packet_plane_inbound_drop_encrypt_datagrams 1");
        assert_metric_line(snapshot, "packet_plane_inbound_drop_decrypt_datagrams 1");
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_invalid_magic_datagrams 1",
        );
        assert_metric_line(snapshot, "packet_plane_inbound_drop_truncated_datagrams 1");
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_unsupported_version_datagrams 1",
        );
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_ciphertext_too_large_datagrams 1",
        );
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_payload_too_large_datagrams 1",
        );
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_frame_decode_datagrams 1",
        );
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_frame_length_mismatch_datagrams 1",
        );
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_header_mismatch_datagrams 1",
        );
        assert_metric_line(snapshot, "packet_plane_inbound_drop_replayed_datagrams 1");
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_outside_replay_window_datagrams 1",
        );
        assert_metric_line(
            snapshot,
            "packet_plane_inbound_drop_trailing_bytes_datagrams 1",
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn metrics_snapshot_reports_runtime_and_queue_counters() {
        let snapshot = populated_snapshot();

        assert_eq!(snapshot.tun_read_packets, 1);
        assert_eq!(snapshot.tun_read_bytes, 20);
        assert_eq!(snapshot.tun_write_packets, 1);
        assert_eq!(snapshot.tun_write_bytes, 40);
        assert_eq!(snapshot.outbound_sent_packets, 1);
        assert_eq!(snapshot.outbound_path_probes_sent, 1);
        assert_eq!(snapshot.outbound_path_probe_acks_sent, 1);
        assert_eq!(snapshot.outbound_path_probe_failures, 1);
        assert_eq!(snapshot.packet_plane_sessions_expired, 1);
        assert_eq!(snapshot.inbound_accepted_packets, 1);
        assert_eq!(snapshot.inbound_keepalives_accepted, 1);
        assert_eq!(snapshot.inbound_path_probes_accepted, 1);
        assert_packet_drop_counters(&snapshot);
        assert_eq!(snapshot.outbound_failures, 1);
        assert_eq!(snapshot.inbound_failures, 1);
        assert_eq!(snapshot.direct_connections_established, 1);
        assert_eq!(snapshot.relayed_connections_established, 1);
        assert_eq!(snapshot.unauthorized_connections_dropped, 1);
        assert_eq!(snapshot.relay_reservations_accepted, 1);
        assert_eq!(snapshot.relay_reservations_lost, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_candidates, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_dial_attempts, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_dial_failures, 1);
        assert_eq!(snapshot.auto_relay_discovery_queries, 1);
        assert_eq!(snapshot.auto_relay_candidates, 1);
        assert_eq!(snapshot.auto_relay_reservation_attempts, 1);
        assert_eq!(snapshot.auto_relay_reservation_failures, 1);
        assert_eq!(snapshot.relay_outbound_circuits_established, 1);
        assert_eq!(snapshot.relay_inbound_circuits_established, 1);
        assert_eq!(snapshot.relay_server_reservations_accepted, 1);
        assert_eq!(snapshot.relay_server_reservations_denied, 1);
        assert_eq!(snapshot.relay_server_reservations_closed, 1);
        assert_eq!(snapshot.relay_server_reservations_timed_out, 1);
        assert_eq!(snapshot.relay_server_circuits_accepted, 1);
        assert_eq!(snapshot.relay_server_circuits_denied, 1);
        assert_eq!(snapshot.relay_server_circuits_closed, 1);
        assert_eq!(snapshot.dcutr_successes, 1);
        assert_eq!(snapshot.dcutr_failures, 1);
        assert_eq!(snapshot.external_address_candidates, 1);
        assert_eq!(snapshot.external_addresses_confirmed, 1);
        assert_eq!(snapshot.external_addresses_expired, 1);
        assert_eq!(snapshot.observed_packet_plane_external_addresses, 1);
        assert_eq!(
            snapshot.observed_packet_plane_external_addresses_rejected,
            1
        );
        assert_eq!(snapshot.observed_packet_plane_udp_endpoint_candidates, 1);
        assert_eq!(snapshot.observed_packet_plane_quic_endpoint_candidates, 1);
        assert_eq!(snapshot.autonat_probes_scheduled, 1);
        assert_eq!(snapshot.autonat_status_unknown, 0);
        assert_eq!(snapshot.autonat_status_public, 0);
        assert_eq!(snapshot.autonat_status_private, 1);
        assert_eq!(snapshot.autonat_status_changes_to_unknown, 0);
        assert_eq!(snapshot.autonat_status_changes_to_public, 1);
        assert_eq!(snapshot.autonat_status_changes_to_private, 1);
        assert_kademlia_discovery_counters(&snapshot);
        assert_eq!(snapshot.control_requests_sent, 1);
        assert_eq!(snapshot.control_requests_received, 1);
        assert_eq!(snapshot.control_responses_received, 1);
        assert_eq!(snapshot.control_capability_accepts, 1);
        assert_eq!(snapshot.control_capability_rejections, 11);
        assert_eq!(snapshot.control_reject_unauthorized_peer, 1);
        assert_eq!(snapshot.control_reject_wrong_network, 1);
        assert_eq!(snapshot.control_reject_membership_mismatch, 1);
        assert_eq!(snapshot.control_reject_unsupported_wire_version, 1);
        assert_eq!(snapshot.control_reject_unsupported_packet_protocol, 1);
        assert_eq!(snapshot.control_reject_unsupported_packet_header_length, 1);
        assert_eq!(snapshot.control_reject_invalid_effective_mtu, 1);
        assert_eq!(snapshot.control_reject_unsupported_preferred_path, 1);
        assert_eq!(snapshot.control_reject_unauthorized_route_advertisement, 1);
        assert_eq!(snapshot.control_reject_invalid_owned_quic_certificate, 1);
        assert_eq!(snapshot.control_reject_invalid_membership_record, 1);
        assert_eq!(snapshot.control_failures, 1);
        assert_eq!(snapshot.service_requests_sent, 1);
        assert_eq!(snapshot.service_requests_received, 1);
        assert_eq!(snapshot.service_responses_received, 1);
        assert_eq!(snapshot.service_status_accepts, 1);
        assert_eq!(snapshot.service_status_rejections, 3);
        assert_eq!(snapshot.service_reject_unauthorized_peer, 1);
        assert_eq!(snapshot.service_reject_wrong_network, 1);
        assert_eq!(snapshot.service_reject_membership_mismatch, 1);
        assert_eq!(snapshot.service_failures, 1);
        assert_eq!(snapshot.redial_attempts, 1);
        assert_eq!(snapshot.redial_skipped_connected, 1);
        assert_eq!(snapshot.redial_failures, 1);
        assert_eq!(snapshot.outgoing_connection_errors, 1);
        assert_eq!(snapshot.discovered_addresses_accepted, 1);
        assert_eq!(snapshot.discovered_address_dial_attempts, 1);
        assert_eq!(snapshot.discovered_address_dial_failures, 1);
        assert_eq!(snapshot.discovered_addresses_rejected, 1);
        assert_eq!(snapshot.discovered_addresses_expired, 2);
        assert_eq!(snapshot.outbound_queue_blocked_no_supported_path_events, 1);
        assert_eq!(snapshot.outbound_queue_blocked_packet_window_events, 1);
        assert_eq!(snapshot.path.healthy_direct_udp_datagram_paths, 1);
        assert_eq!(snapshot.path.healthy_direct_quic_datagram_paths, 2);
        assert_eq!(snapshot.path.healthy_direct_quic_stream_paths, 3);
        assert_eq!(snapshot.path.healthy_direct_tcp_stream_paths, 4);
        assert_eq!(snapshot.path.healthy_relay_paths, 5);
        assert_eq!(snapshot.path.peers_with_supported_path, 6);
        assert_eq!(snapshot.path.peers_without_supported_path, 7);
    }

    #[test]
    fn metrics_snapshot_reports_packet_transport_counters() {
        let snapshot = populated_snapshot();

        assert_eq!(snapshot.outbound_stream_fallback_packets, 1);
        assert_eq!(snapshot.outbound_quic_datagram_packets, 1);
        assert_eq!(snapshot.outbound_quic_datagram_unavailable_packets, 1);
    }

    #[test]
    fn metrics_snapshot_reports_path_selection_counters() {
        let snapshot = populated_snapshot();

        assert_eq!(snapshot.path_promotions_to_direct, 1);
        assert_eq!(snapshot.path_fallbacks_to_relay, 1);
        assert_eq!(snapshot.packet_plane_path_demotions, 1);
        assert_eq!(snapshot.stream_fallback_path_demotions, 1);
        assert_eq!(snapshot.packet_plane_path_recovery_dial_attempts, 1);
        assert_eq!(snapshot.packet_plane_path_recovery_dial_failures, 1);
        assert_eq!(snapshot.outbound_path_mtu_updates, 1);
        assert_eq!(snapshot.outbound_path_mtu_probe_confirmations, 1);
    }

    #[test]
    fn metrics_snapshot_reports_auto_relay_counters() {
        let snapshot = populated_snapshot();

        assert_eq!(snapshot.auto_relay_candidates, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_candidates, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_dial_attempts, 1);
        assert_eq!(snapshot.auto_relay_infrastructure_dial_failures, 1);
        assert_eq!(snapshot.auto_relay_discovery_queries, 1);
        assert_eq!(snapshot.auto_relay_reservation_attempts, 1);
        assert_eq!(snapshot.auto_relay_reservation_failures, 1);
        assert_metric_line(&snapshot, "auto_relay_infrastructure_candidates 1");
        assert_metric_line(&snapshot, "auto_relay_infrastructure_dial_attempts 1");
        assert_metric_line(&snapshot, "auto_relay_infrastructure_dial_failures 1");
        assert_metric_line(&snapshot, "auto_relay_discovery_queries 1");
        assert_metric_line(&snapshot, "auto_relay_candidates 1");
        assert_metric_line(&snapshot, "auto_relay_reservation_attempts 1");
        assert_metric_line(&snapshot, "auto_relay_reservation_failures 1");
    }

    #[test]
    fn metrics_snapshot_reports_runtime_and_queue_lines() {
        let snapshot = populated_snapshot();

        assert_metric_line(&snapshot, "queue_queued_packets 2");
        assert_metric_line(&snapshot, "outbound_stream_fallback_packets 1");
        assert_metric_line(&snapshot, "outbound_quic_datagram_packets 1");
        assert_metric_line(&snapshot, "outbound_quic_datagram_unavailable_packets 1");
        assert_metric_line(&snapshot, "outbound_path_probes_sent 1");
        assert_metric_line(&snapshot, "outbound_path_probe_acks_sent 1");
        assert_metric_line(&snapshot, "outbound_path_probe_failures 1");
        assert_metric_line(&snapshot, "packet_plane_sessions_expired 1");
        assert_metric_line(&snapshot, "inbound_keepalives_accepted 1");
        assert_metric_line(&snapshot, "inbound_path_probes_accepted 1");
        assert_metric_line(&snapshot, "queue_oldest_packet_age_millis 45");
        assert_packet_drop_lines(&snapshot);
        assert_metric_line(&snapshot, "relayed_connections_established 1");
        assert_metric_line(&snapshot, "path_promotions_to_direct 1");
        assert_metric_line(&snapshot, "path_fallbacks_to_relay 1");
        assert_metric_line(&snapshot, "packet_plane_path_demotions 1");
        assert_metric_line(&snapshot, "stream_fallback_path_demotions 1");
        assert_metric_line(&snapshot, "packet_plane_path_recovery_dial_attempts 1");
        assert_metric_line(&snapshot, "packet_plane_path_recovery_dial_failures 1");
        assert_metric_line(&snapshot, "outbound_path_mtu_updates 1");
        assert_metric_line(&snapshot, "outbound_path_mtu_probe_confirmations 1");
        assert_metric_line(&snapshot, "unauthorized_connections_dropped 1");
        assert_metric_line(&snapshot, "relay_reservations_lost 1");
        assert_metric_line(&snapshot, "relay_server_reservations_denied 1");
        assert_metric_line(&snapshot, "relay_server_reservations_closed 1");
        assert_metric_line(&snapshot, "relay_server_reservations_timed_out 1");
        assert_metric_line(&snapshot, "relay_server_circuits_denied 1");
        assert_metric_line(&snapshot, "relay_server_circuits_closed 1");
        assert_metric_line(&snapshot, "dcutr_successes 1");
        assert_metric_line(&snapshot, "external_address_candidates 1");
        assert_metric_line(&snapshot, "external_addresses_confirmed 1");
        assert_metric_line(&snapshot, "external_addresses_expired 1");
        assert_observed_packet_plane_lines(&snapshot);
        assert_metric_line(&snapshot, "autonat_probes_scheduled 1");
        assert_metric_line(&snapshot, "autonat_status_unknown 0");
        assert_metric_line(&snapshot, "autonat_status_public 0");
        assert_metric_line(&snapshot, "autonat_status_private 1");
        assert_metric_line(&snapshot, "autonat_status_changes_to_public 1");
        assert_metric_line(&snapshot, "autonat_status_changes_to_private 1");
        assert_kademlia_discovery_lines(&snapshot);
        assert_metric_line(&snapshot, "control_requests_sent 1");
        assert_metric_line(&snapshot, "control_requests_received 1");
        assert_metric_line(&snapshot, "control_responses_received 1");
        assert_metric_line(&snapshot, "control_capability_accepts 1");
        assert_metric_line(&snapshot, "control_capability_rejections 11");
        assert_metric_line(&snapshot, "control_reject_unauthorized_peer 1");
        assert_metric_line(&snapshot, "control_reject_wrong_network 1");
        assert_metric_line(&snapshot, "control_reject_membership_mismatch 1");
        assert_metric_line(&snapshot, "control_reject_unsupported_wire_version 1");
        assert_metric_line(&snapshot, "control_reject_unsupported_packet_protocol 1");
        assert_metric_line(
            &snapshot,
            "control_reject_unsupported_packet_header_length 1",
        );
        assert_metric_line(&snapshot, "control_reject_invalid_effective_mtu 1");
        assert_metric_line(&snapshot, "control_reject_unsupported_preferred_path 1");
        assert_metric_line(
            &snapshot,
            "control_reject_unauthorized_route_advertisement 1",
        );
        assert_metric_line(&snapshot, "control_reject_invalid_owned_quic_certificate 1");
        assert_metric_line(&snapshot, "control_reject_invalid_membership_record 1");
        assert_metric_line(&snapshot, "control_failures 1");
        assert_metric_line(&snapshot, "service_requests_sent 1");
        assert_metric_line(&snapshot, "service_requests_received 1");
        assert_metric_line(&snapshot, "service_responses_received 1");
        assert_metric_line(&snapshot, "service_status_accepts 1");
        assert_metric_line(&snapshot, "service_status_rejections 3");
        assert_metric_line(&snapshot, "service_reject_unauthorized_peer 1");
        assert_metric_line(&snapshot, "service_reject_wrong_network 1");
        assert_metric_line(&snapshot, "service_reject_membership_mismatch 1");
        assert_metric_line(&snapshot, "service_failures 1");
        assert_metric_line(&snapshot, "redial_attempts 1");
        assert_metric_line(&snapshot, "redial_skipped_connected 1");
        assert_metric_line(&snapshot, "redial_failures 1");
        assert_metric_line(&snapshot, "outgoing_connection_errors 1");
        assert_metric_line(&snapshot, "discovered_addresses_accepted 1");
        assert_metric_line(&snapshot, "discovered_address_dial_attempts 1");
        assert_metric_line(&snapshot, "discovered_address_dial_failures 1");
        assert_metric_line(&snapshot, "discovered_addresses_rejected 1");
        assert_metric_line(&snapshot, "discovered_addresses_expired 2");
        assert_metric_line(
            &snapshot,
            "outbound_queue_blocked_no_supported_path_events 1",
        );
        assert_metric_line(&snapshot, "outbound_queue_blocked_packet_window_events 1");
        assert_metric_line(&snapshot, "path_healthy_direct_udp_datagram_paths 1");
        assert_metric_line(&snapshot, "path_healthy_direct_quic_datagram_paths 2");
        assert_metric_line(&snapshot, "path_healthy_direct_quic_stream_paths 3");
        assert_metric_line(&snapshot, "path_healthy_direct_tcp_stream_paths 4");
        assert_metric_line(&snapshot, "path_healthy_relay_paths 5");
        assert_metric_line(&snapshot, "path_peers_with_supported_path 6");
        assert_metric_line(&snapshot, "path_peers_without_supported_path 7");
        assert_metric_line(&snapshot, "queue_expired_packets 2");
        assert_metric_line(&snapshot, "queue_expired_bytes 60");
    }

    #[test]
    fn metrics_snapshot_reports_default_autonat_unknown_status() {
        let snapshot = RuntimeMetrics::default().snapshot(QueueStats::default());

        assert_eq!(snapshot.autonat_status_unknown, 1);
        assert_eq!(snapshot.autonat_status_public, 0);
        assert_eq!(snapshot.autonat_status_private, 0);
        assert_eq!(snapshot.autonat_status_changes_to_unknown, 0);
    }
}
