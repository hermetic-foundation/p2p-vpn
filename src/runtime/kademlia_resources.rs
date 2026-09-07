use libp2p::kad;

use super::p2p::Behaviour;

/// Numeric snapshots only: no addresses, identities, or per-connection history.
pub(super) struct KademliaResources {
    primary: DhtResources,
    pairing: Option<DhtResources>,
}

impl KademliaResources {
    pub(super) fn capture(behaviour: &Behaviour) -> Self {
        Self {
            primary: DhtResources::capture(&behaviour.kad),
            pairing: behaviour.pairing_kad.as_ref().map(DhtResources::capture),
        }
    }

    pub(super) fn extend_lines(&self, lines: &mut Vec<String>) {
        self.primary.extend_lines("kad_primary", lines);
        if let Some(pairing) = &self.pairing {
            pairing.extend_lines("kad_pairing", lines);
        } else {
            lines.push("kad_pairing_present 0".to_owned());
        }
    }
}

struct DhtResources {
    routing: kad::RoutingUsage,
    queries: kad::QueryPoolUsage,
    lifecycle: kad::QueryLifecycleUsage,
    caches: kad::QueryResourceSnapshot,
    metadata: kad::QueryMetadataUsage,
    rpc: kad::PendingRpcUsage,
    jobs: kad::BackgroundJobUsage,
    events: kad::BehaviourQueueUsage,
    dials: kad::DialQueueUsage,
    handlers: kad::HandlerResourceUsage,
}

impl DhtResources {
    fn capture(behaviour: &kad::Behaviour<kad::store::MemoryStore>) -> Self {
        Self {
            routing: behaviour.routing_resource_usage(),
            queries: behaviour.query_pool_usage(),
            lifecycle: behaviour.query_lifecycle_usage(),
            caches: behaviour.query_resource_snapshot(),
            metadata: behaviour.query_metadata_usage(),
            rpc: behaviour.pending_rpc_usage(),
            jobs: behaviour.background_job_usage(),
            events: behaviour.behaviour_queue_usage(),
            dials: behaviour.dial_queue_usage(),
            handlers: behaviour.handler_resource_usage(),
        }
    }

    fn extend_lines(&self, prefix: &str, lines: &mut Vec<String>) {
        let handler = self.handlers.usage;
        let gauges = [
            ("present", 1),
            ("routing_entries", self.routing.entries),
            ("routing_address_bytes", self.routing.address_bytes),
            ("query_pool_retained", self.queries.retained),
            (
                "query_pool_limited",
                usize::from(self.queries.capacity.is_some()),
            ),
            ("query_pool_capacity", self.queries.capacity.unwrap_or(0)),
            ("query_bounded_caches", self.caches.bounded_queries),
            ("query_candidates", self.caches.candidates),
            ("query_address_bytes", self.caches.address_bytes),
            ("query_max_candidates", self.caches.max_candidates),
            ("query_max_address_bytes", self.caches.max_address_bytes),
            (
                "query_retained_rejected_reports",
                self.caches.rejected_reports,
            ),
            ("query_payload_bytes", self.metadata.payload_bytes),
            ("query_result_peers", self.metadata.result_peers),
            ("query_provider_addresses", self.metadata.provider_addresses),
            (
                "query_bootstrap_target_slots",
                self.metadata.bootstrap_target_slots,
            ),
            ("query_fixed_peer_slots", self.metadata.fixed_peer_slots),
            ("pending_rpc_requests", self.rpc.requests),
            ("pending_rpc_bytes", self.rpc.bytes),
            ("background_bounded_jobs", self.jobs.bounded_jobs),
            ("background_pending_keys", self.jobs.pending_keys),
            ("background_pending_key_bytes", self.jobs.pending_key_bytes),
            ("background_cursor_bytes", self.jobs.cursor_bytes),
            ("background_skipped_keys", self.jobs.skipped_keys),
            ("background_skipped_key_bytes", self.jobs.skipped_key_bytes),
            ("events", self.events.events),
            ("event_bytes", self.events.bytes),
            (
                "event_count_limited",
                usize::from(self.events.event_limit.is_some()),
            ),
            ("event_limit", self.events.event_limit.unwrap_or(0)),
            (
                "event_bytes_limited",
                usize::from(self.events.byte_limit.is_some()),
            ),
            ("event_byte_limit", self.events.byte_limit.unwrap_or(0)),
            ("handlers", self.handlers.handlers),
            ("handler_pending_requests", handler.requests),
            ("handler_pending_bytes", handler.bytes),
            ("handler_pending_negotiations", handler.pending_negotiations),
            ("handler_inbound_streams", handler.active_inbound_streams),
            ("handler_outbound_streams", handler.active_outbound_streams),
            ("handler_queued_rejections", handler.queued_rejections),
            (
                "handler_peak_pending_requests",
                self.handlers.peak_pending_requests_per_handler,
            ),
            (
                "handler_peak_pending_bytes",
                self.handlers.peak_pending_bytes_per_handler,
            ),
            (
                "handler_peak_pending_negotiations",
                self.handlers.peak_pending_negotiations_per_handler,
            ),
            (
                "handler_peak_inbound_streams",
                self.handlers.peak_inbound_streams_per_handler,
            ),
            (
                "handler_peak_outbound_streams",
                self.handlers.peak_outbound_streams_per_handler,
            ),
            (
                "handler_peak_queued_rejections",
                self.handlers.peak_queued_rejections_per_handler,
            ),
        ];
        let counters = [
            ("routing_entry_rejections", self.routing.entry_rejections),
            (
                "routing_address_rejections",
                self.routing.address_rejections,
            ),
            ("query_pool_rejected", self.queries.rejected),
            ("query_phases_admitted", self.lifecycle.admitted_phases),
            ("query_phases_retired", self.lifecycle.retired_phases),
            ("query_phases_completed", self.lifecycle.completed_phases),
            ("query_phases_timed_out", self.lifecycle.timed_out_phases),
            ("query_phases_cancelled", self.lifecycle.cancelled_phases),
            ("query_requests", self.lifecycle.requests),
            ("query_successes", self.lifecycle.successes),
            ("query_failures", self.lifecycle.failures),
            ("query_rejected_inputs", self.metadata.rejected_inputs),
            ("pending_rpc_count_rejections", self.rpc.count_rejections),
            ("pending_rpc_byte_rejections", self.rpc.byte_rejections),
            ("background_rejected_inputs", self.jobs.rejected_inputs),
            ("background_rejected_skips", self.jobs.rejected_skips),
            ("events_rejected", self.events.rejected),
            ("dial_intents_attempted", self.dials.attempted),
            ("dial_intents_admitted", self.dials.admitted),
            ("dial_intents_dispatched", self.dials.dispatched),
            ("dial_intents_discarded", self.dials.discarded),
            ("handler_rejected", handler.rejected),
            (
                "handler_unreported_rejections",
                handler.unreported_rejections,
            ),
            ("handler_expired", handler.expired),
            ("handler_inbound_rejections", handler.inbound_rejections),
            ("handler_inbound_replacements", handler.inbound_replacements),
            ("handler_inbound_expired", handler.inbound_expired),
        ];
        lines.extend(
            gauges
                .into_iter()
                .map(|(name, value)| format!("{prefix}_{name} {value}")),
        );
        lines.extend(
            counters
                .into_iter()
                .map(|(name, value)| format!("{prefix}_{name} {value}")),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_distinguish_absent_dht_and_unlimited_capacity() {
        let peer = libp2p::PeerId::random();
        let limited = kad::Behaviour::with_config(
            peer,
            kad::store::MemoryStore::new(peer),
            super::super::p2p::controlled_kademlia_config(libp2p::StreamProtocol::new("/test/kad")),
        );
        let unlimited = kad::Behaviour::new(peer, kad::store::MemoryStore::new(peer));
        let mut snapshot = KademliaResources {
            primary: DhtResources::capture(&limited),
            pairing: None,
        };
        let mut lines = vec!["existing_field 42".to_owned()];
        snapshot.extend_lines(&mut lines);
        assert_eq!(lines[0], "existing_field 42");
        assert!(lines.contains(&"kad_primary_query_pool_capacity 32".to_owned()));
        assert!(lines.contains(&"kad_primary_query_pool_limited 1".to_owned()));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("kad_pairing_"))
                .count(),
            1
        );
        assert!(lines.contains(&"kad_pairing_present 0".to_owned()));
        snapshot.pairing = Some(DhtResources::capture(&unlimited));
        lines.clear();
        snapshot.extend_lines(&mut lines);
        assert!(lines.contains(&"kad_pairing_present 1".to_owned()));
        assert!(lines.contains(&"kad_pairing_query_pool_limited 0".to_owned()));
        assert!(lines.contains(&"kad_pairing_event_count_limited 0".to_owned()));
        assert!(lines.contains(&"kad_pairing_event_bytes_limited 0".to_owned()));
        let mut keys = std::collections::HashSet::new();
        for line in lines {
            let (key, value) = line.split_once(' ').unwrap();
            assert!(keys.insert(key.to_owned()), "duplicate field: {key}");
            assert!(value.parse::<u64>().is_ok(), "non-numeric resource: {line}");
        }
    }
}
