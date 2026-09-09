use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Counter,
    Gauge,
}

#[derive(Debug, Serialize)]
pub struct Metric {
    pub name: String,
    pub kind: Kind,
    pub availability: &'static str,
    pub source: &'static str,
    pub scope: &'static str,
}

pub fn catalog() -> Vec<Metric> {
    let mut metrics = Vec::new();
    for name in [
        "direct_connections_established",
        "relayed_connections_established",
        "outgoing_connection_errors",
        "redial_attempts",
        "discovered_address_dial_attempts",
        "packet_plane_path_recovery_dial_attempts",
        "auto_relay_infrastructure_dial_attempts",
        "auto_relay_infrastructure_dial_failures",
        "auto_relay_discovery_queries",
        "auto_relay_reservation_attempts",
        "auto_relay_reservation_failures",
        "kademlia_provider_lookups",
        "kademlia_provider_dial_attempts",
        "kademlia_provider_dial_failures",
        "kademlia_provider_advertisements",
        "kademlia_membership_record_lookups",
        "kademlia_membership_record_publications",
        "kademlia_bootstrap_refreshes",
        "kademlia_bootstrap_failures",
        "outbound_direct_tcp_stream_fallback_packets",
        "outbound_direct_quic_stream_fallback_packets",
        "outbound_relay_stream_fallback_packets",
        "outbound_quic_datagram_packets",
    ] {
        metrics.push(Metric {
            name: name.to_owned(),
            kind: Kind::Counter,
            availability: "common",
            source: "src/metrics.rs (identical pinned subjects)",
            scope: if name == "outbound_quic_datagram_packets" {
                "legacy name: packet-plane datagram sends across backends; not proof of QUIC transport"
            } else if name.starts_with("outbound_") {
                "overlay packet transport events; inspect when attributing resource deltas"
            } else if name.starts_with("auto_relay_") {
                "relay infrastructure application events"
            } else {
                "application events; not all libp2p RPCs or dials"
            },
        });
    }
    for prefix in ["kad_primary", "kad_pairing"] {
        for (kind, names) in [
            (
                Kind::Counter,
                &[
                    "query_phases_admitted",
                    "query_phases_retired",
                    "query_phases_completed",
                    "query_phases_timed_out",
                    "query_phases_cancelled",
                    "query_requests",
                    "query_successes",
                    "query_failures",
                    "query_pool_rejected",
                    "dial_intents_attempted",
                    "dial_intents_admitted",
                    "dial_intents_dispatched",
                    "dial_intents_discarded",
                    "handler_rejected",
                    "pending_rpc_count_rejections",
                    "pending_rpc_byte_rejections",
                ][..],
            ),
            (
                Kind::Gauge,
                &[
                    "routing_entries",
                    "routing_address_bytes",
                    "query_pool_retained",
                    "query_candidates",
                    "query_address_bytes",
                    "query_payload_bytes",
                    "pending_rpc_requests",
                    "pending_rpc_bytes",
                    "handlers",
                    "handler_pending_requests",
                    "handler_pending_bytes",
                    "events",
                    "event_bytes",
                ][..],
            ),
        ] {
            for name in names {
                metrics.push(Metric {
                    name: format!("{prefix}_{name}"),
                    kind,
                    availability: "current_only; pairing fields absent when no separate DHT",
                    source: "src/runtime/kademlia_resources.rs",
                    scope: "DHT-internal; not classified by overlay/infrastructure peer",
                });
            }
        }
    }
    for name in [
        "maintenance_queries",
        "address_publication_queries",
        "recovery_queries",
        "recovery_query_peers_retained",
        "discovered_recovery_addresses_retained",
        "recovery_dial_targets_retained",
        "connection_attempts_pending",
        "connections_retiring",
        "packet_hellos_pending",
    ] {
        metrics.push(Metric {
            name: format!("app_{name}"),
            kind: Kind::Gauge,
            availability: "current_only",
            source: "src/runtime/runner/recovery_snapshot.rs",
            scope: "application recovery owners",
        });
    }
    metrics
}

#[test]
fn catalog_names_are_unique_and_event_counts_are_not_connection_gauges() {
    let metrics = catalog();
    let names: std::collections::BTreeSet<_> = metrics.iter().map(|m| &m.name).collect();
    assert_eq!(names.len(), metrics.len());
    assert_eq!(
        metrics
            .iter()
            .find(|m| m.name == "direct_connections_established")
            .unwrap()
            .kind,
        Kind::Counter
    );
    assert!(
        metrics
            .iter()
            .filter(|m| m.name.starts_with("app_"))
            .all(|m| m.kind == Kind::Gauge)
    );
}
