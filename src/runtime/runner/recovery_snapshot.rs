use super::{
    ConnectionEpochs, DiscoveredPeerAddresses, Duration, Instant, KademliaMaintenance,
    PacketPlaneNegotiator, PublicDiscoveryBackoff,
};

/// Fixed-size, read-only application owners, independent of the Kad query pool.
/// Ages and relative deadlines are floored milliseconds; absent/due is zero.
/// Cooldown counts include only future deadlines, never retained idle entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ApplicationRecoverySnapshot([(&'static str, u64); 33]);

impl ApplicationRecoverySnapshot {
    pub(super) fn capture(
        maintenance: &KademliaMaintenance,
        discovered: &DiscoveredPeerAddresses,
        backoff: &PublicDiscoveryBackoff,
        connections: &ConnectionEpochs,
        negotiator: &PacketPlaneNegotiator,
        now: Instant,
    ) -> Self {
        let queries = discovered.recovery_queries.snapshot(now);
        let (quarantined, address_retry) = cooldowns(
            discovered
                .addresses
                .iter()
                .filter_map(|entry| entry.quarantined_until),
            now,
        );
        let (dial_cooldowns, dial_retry) = cooldowns(
            discovered
                .recovery_dial_attempts
                .values()
                .map(|attempt| attempt.retry_after),
            now,
        );
        Self([
            ("maintenance_queries", maintenance.queries.len() as u64),
            (
                "maintenance_oldest_pending_age_millis",
                oldest_age(
                    maintenance
                        .started_at
                        .filter(|_| !maintenance.queries.is_empty()),
                    now,
                ),
            ),
            (
                "maintenance_next_due_in_millis",
                until(Some(maintenance.next_due), now),
            ),
            (
                "address_publication_pending",
                u64::from(maintenance.address_publication_pending),
            ),
            (
                "address_publication_queries",
                u64::from(maintenance.address_publication.is_some()),
            ),
            (
                "address_publication_oldest_pending_age_millis",
                oldest_age(
                    maintenance.address_publication.map(|(_, started)| started),
                    now,
                ),
            ),
            (
                "address_publication_next_in_millis",
                until(Some(maintenance.address_publication_next), now),
            ),
            (
                "address_publication_refresh_scheduled",
                u64::from(maintenance.address_publication_refresh.is_some()),
            ),
            (
                "address_publication_refresh_in_millis",
                until(maintenance.address_publication_refresh, now),
            ),
            (
                "recovery_query_peers_retained",
                queries.peers_retained as u64,
            ),
            ("recovery_queries", queries.queries as u64),
            (
                "recovery_query_oldest_pending_age_millis",
                queries.oldest_pending_age_millis,
            ),
            (
                "recovery_query_cooldown_peers",
                queries.cooldown_peers as u64,
            ),
            (
                "recovery_query_next_retry_in_millis",
                queries.next_retry_in_millis,
            ),
            // This is the recovery cache, not AddressRetention's private admission index.
            (
                "discovered_recovery_addresses_retained",
                discovered.addresses.len() as u64,
            ),
            (
                "discovered_recovery_addresses_oldest_seen_age_millis",
                oldest_age(
                    discovered.addresses.iter().map(|entry| entry.last_seen),
                    now,
                ),
            ),
            ("discovered_recovery_addresses_quarantined", quarantined),
            (
                "discovered_recovery_addresses_next_retry_in_millis",
                address_retry,
            ),
            (
                "recovery_dial_targets_retained",
                discovered.recovery_dial_attempts.len() as u64,
            ),
            ("recovery_dial_cooldown_targets", dial_cooldowns),
            ("recovery_dial_next_retry_in_millis", dial_retry),
            (
                "recovery_dial_oldest_attempt_age_millis",
                oldest_age(
                    discovered
                        .recovery_dial_attempts
                        .values()
                        .map(|attempt| attempt.last_attempt),
                    now,
                ),
            ),
            (
                "public_discovery_suppressed",
                u64::from(backoff.suppresses(now)),
            ),
            (
                "public_discovery_retry_in_millis",
                until(backoff.suppressed_until, now),
            ),
            (
                "public_discovery_no_route_failures",
                u64::from(backoff.consecutive_no_route_failures),
            ),
            (
                "connection_attempts_pending",
                connections
                    .connections
                    .keys()
                    .filter(|id| !connections.established.contains(*id))
                    .count() as u64,
            ),
            ("connections_retiring", connections.retiring.len() as u64),
            ("packet_hellos_pending", negotiator.pending.len() as u64),
            (
                "packet_hello_oldest_pending_age_millis",
                oldest_age(
                    negotiator
                        .pending
                        .values()
                        .map(|pending| pending.created_at),
                    now,
                ),
            ),
            (
                "packet_responders_pending",
                negotiator.pending_responders.len() as u64,
            ),
            (
                "packet_responder_oldest_pending_age_millis",
                oldest_age(
                    negotiator
                        .pending_responders
                        .values()
                        .map(|pending| pending.created_at),
                    now,
                ),
            ),
            (
                "packet_quic_connection_tasks",
                negotiator.quic_connection_tasks.len() as u64,
            ),
            (
                "packet_quic_connection_task_owners",
                negotiator.quic_connection_task_handles.len() as u64,
            ),
        ])
    }

    pub(super) fn extend_lines(&self, lines: &mut Vec<String>) {
        lines.extend(
            self.0
                .iter()
                .map(|(name, value)| format!("app_{name} {value}")),
        );
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn oldest_age(starts: impl IntoIterator<Item = Instant>, now: Instant) -> u64 {
    starts
        .into_iter()
        .min()
        .map_or(0, |start| millis(now.saturating_duration_since(start)))
}

fn until(deadline: Option<Instant>, now: Instant) -> u64 {
    deadline.map_or(0, |deadline| {
        millis(deadline.saturating_duration_since(now))
    })
}

fn cooldowns(deadlines: impl IntoIterator<Item = Instant>, now: Instant) -> (u64, u64) {
    let mut count = 0;
    let mut next = None;
    for deadline in deadlines.into_iter().filter(|deadline| *deadline > now) {
        count += 1;
        next = Some(next.map_or(deadline, |previous: Instant| previous.min(deadline)));
    }
    (count, until(next, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::{Multiaddr, PeerId, kad, swarm::ConnectionId};

    fn value(snapshot: &ApplicationRecoverySnapshot, name: &str) -> u64 {
        snapshot.0.iter().find(|(key, _)| *key == name).unwrap().1
    }

    #[test]
    fn independent_query_owners_survive_empty_kad_pool_and_finish_independently() {
        let start = Instant::now();
        let peer = PeerId::random();
        let mut kad = kad::Behaviour::new(peer, kad::store::MemoryStore::new(peer));
        let maintenance_query = kad.get_record(kad::RecordKey::new(&b"maintenance"));
        let publication_query = kad.get_record(kad::RecordKey::new(&b"publication"));
        let recovery_query = kad.get_record(kad::RecordKey::new(&b"recovery"));
        let mut maintenance = KademliaMaintenance::new(start);
        maintenance.record_started(maintenance_query, start);
        maintenance.address_publication = Some((publication_query, start + Duration::from_secs(1)));
        maintenance.address_publication_pending = true;
        let mut discovered = DiscoveredPeerAddresses::default();
        assert!(discovered.should_query_recovery_discovery_at(peer, start));
        discovered.record_recovery_discovery_queries(
            peer,
            [recovery_query],
            start + Duration::from_secs(2),
        );
        let now = start + Duration::from_secs(10);
        let capture = |maintenance: &KademliaMaintenance, discovered: &DiscoveredPeerAddresses| {
            ApplicationRecoverySnapshot::capture(
                maintenance,
                discovered,
                &PublicDiscoveryBackoff::default(),
                &ConnectionEpochs::default(),
                &PacketPlaneNegotiator::default(),
                now,
            )
        };
        let filled = capture(&maintenance, &discovered);
        assert_eq!(value(&filled, "maintenance_queries"), 1);
        assert_eq!(
            value(&filled, "maintenance_oldest_pending_age_millis"),
            10_000
        );
        assert_eq!(value(&filled, "address_publication_queries"), 1);
        assert_eq!(value(&filled, "address_publication_pending"), 1);
        assert_eq!(
            value(&filled, "address_publication_oldest_pending_age_millis"),
            9_000
        );
        assert_eq!(value(&filled, "recovery_queries"), 1);
        assert_eq!(
            value(&filled, "recovery_query_oldest_pending_age_millis"),
            8_000
        );
        for query in [maintenance_query, publication_query, recovery_query] {
            kad.cancel_query(&query);
        }
        assert_eq!(kad.query_pool_usage().retained, 0);
        assert_eq!(capture(&maintenance, &discovered), filled);

        assert!(maintenance.finish_query(maintenance_query));
        let snapshot = capture(&maintenance, &discovered);
        assert_eq!(value(&snapshot, "maintenance_queries"), 0);
        assert_eq!(value(&snapshot, "maintenance_oldest_pending_age_millis"), 0);
        assert_eq!(value(&snapshot, "address_publication_queries"), 1);
        assert_eq!(value(&snapshot, "recovery_queries"), 1);
        assert!(maintenance.finish_query(publication_query));
        let snapshot = capture(&maintenance, &discovered);
        assert_eq!(value(&snapshot, "address_publication_queries"), 0);
        assert_eq!(
            value(&snapshot, "address_publication_oldest_pending_age_millis"),
            0
        );
        assert_eq!(value(&snapshot, "address_publication_pending"), 1);
        assert_eq!(value(&snapshot, "recovery_queries"), 1);
        discovered.finish_recovery_discovery_query(recovery_query, now);
        let snapshot = capture(&maintenance, &discovered);
        assert_eq!(value(&snapshot, "recovery_queries"), 0);
        assert_eq!(
            value(&snapshot, "recovery_query_oldest_pending_age_millis"),
            0
        );
        assert_eq!(value(&snapshot, "recovery_query_peers_retained"), 1);
        assert_eq!(value(&snapshot, "recovery_query_cooldown_peers"), 1);

        let mut lines = vec!["existing_field 42".to_owned()];
        filled.extend_lines(&mut lines);
        assert_eq!(lines[0], "existing_field 42");
        assert_eq!(lines.len(), 34);
        let mut keys = std::collections::HashSet::new();
        for line in &lines[1..] {
            let (key, value) = line.split_once(' ').unwrap();
            assert!(key.starts_with("app_"));
            assert!(keys.insert(key));
            assert!(value.parse::<u64>().is_ok());
        }
    }

    #[test]
    fn retained_addresses_and_healthy_dial_cooldowns_are_not_pending_work() {
        let start = Instant::now();
        let peer = PeerId::random();
        let address: Multiaddr = "/ip4/192.168.1.2/tcp/4001".parse().unwrap();
        let mut discovered = DiscoveredPeerAddresses::default();
        discovered.insert_at(peer, address.clone(), start);
        discovered.record_failure_at(peer, &address, start);
        assert!(discovered.should_attempt_recovery_dial_at(peer, &address, start));
        discovered.record_recovery_connection_at(peer, start);
        let mut maintenance = KademliaMaintenance::new(start);
        maintenance.address_publication_refresh = Some(start + Duration::from_secs(60));
        maintenance.address_publication_next = start + Duration::from_secs(5);
        let mut backoff = PublicDiscoveryBackoff {
            suppressed_until: Some(start + Duration::from_secs(30)),
            ..PublicDiscoveryBackoff::default()
        };
        let mut connections = ConnectionEpochs::default();
        let pending = ConnectionId::new_unchecked(1);
        let established = ConnectionId::new_unchecked(2);
        connections.record_started(pending);
        connections.record_established(established);
        connections.mark_retiring(established);
        let capture = |discovered: &DiscoveredPeerAddresses,
                       backoff: &PublicDiscoveryBackoff,
                       connections: &ConnectionEpochs,
                       now| {
            ApplicationRecoverySnapshot::capture(
                &maintenance,
                discovered,
                backoff,
                connections,
                &PacketPlaneNegotiator::default(),
                now,
            )
        };
        let snapshot = capture(&discovered, &backoff, &connections, start);
        assert_eq!(
            value(&snapshot, "discovered_recovery_addresses_retained"),
            1
        );
        assert_eq!(
            value(&snapshot, "discovered_recovery_addresses_quarantined"),
            1
        );
        assert_eq!(
            value(
                &snapshot,
                "discovered_recovery_addresses_next_retry_in_millis"
            ),
            10_000
        );
        assert_eq!(value(&snapshot, "recovery_dial_targets_retained"), 1);
        assert_eq!(value(&snapshot, "recovery_dial_cooldown_targets"), 1);
        assert_eq!(
            value(&snapshot, "recovery_dial_next_retry_in_millis"),
            10_000
        );
        assert_eq!(value(&snapshot, "public_discovery_suppressed"), 1);
        assert_eq!(value(&snapshot, "public_discovery_retry_in_millis"), 30_000);
        assert_eq!(
            value(&snapshot, "address_publication_next_in_millis"),
            5_000
        );
        assert_eq!(value(&snapshot, "address_publication_refresh_scheduled"), 1);
        assert_eq!(
            value(&snapshot, "address_publication_refresh_in_millis"),
            60_000
        );
        assert_eq!(value(&snapshot, "connection_attempts_pending"), 1);
        assert_eq!(value(&snapshot, "connections_retiring"), 1);
        assert_eq!(
            capture(&discovered, &backoff, &connections, start),
            snapshot
        );

        let snapshot = capture(
            &discovered,
            &backoff,
            &connections,
            start + Duration::from_secs(30),
        );
        assert_eq!(
            value(&snapshot, "discovered_recovery_addresses_retained"),
            1
        );
        assert_eq!(
            value(&snapshot, "discovered_recovery_addresses_quarantined"),
            0
        );
        assert_eq!(
            value(
                &snapshot,
                "discovered_recovery_addresses_next_retry_in_millis"
            ),
            0
        );
        assert_eq!(value(&snapshot, "recovery_dial_targets_retained"), 1);
        assert_eq!(value(&snapshot, "recovery_dial_cooldown_targets"), 0);
        assert_eq!(value(&snapshot, "recovery_dial_next_retry_in_millis"), 0);
        assert_eq!(value(&snapshot, "public_discovery_suppressed"), 0);
        assert_eq!(value(&snapshot, "public_discovery_retry_in_millis"), 0);
        assert_eq!(value(&snapshot, "recovery_queries"), 0);
        assert_eq!(
            value(
                &snapshot,
                "discovered_recovery_addresses_oldest_seen_age_millis"
            ),
            30_000
        );
        assert_eq!(
            value(&snapshot, "recovery_dial_oldest_attempt_age_millis"),
            30_000
        );

        assert!(discovered.remove(peer, &address));
        connections.remove(pending);
        connections.remove(established);
        backoff.reset();
        let snapshot = capture(&discovered, &backoff, &connections, start);
        assert_eq!(
            value(&snapshot, "discovered_recovery_addresses_retained"),
            0
        );
        assert_eq!(
            value(
                &snapshot,
                "discovered_recovery_addresses_oldest_seen_age_millis"
            ),
            0
        );
        assert_eq!(value(&snapshot, "connection_attempts_pending"), 0);
        assert_eq!(value(&snapshot, "connections_retiring"), 0);
        assert_eq!(value(&snapshot, "public_discovery_retry_in_millis"), 0);
    }

    #[test]
    fn relative_times_saturate_and_cooldowns_ignore_due_entries() {
        let now = Instant::now();
        let future = now + Duration::from_secs(1);
        assert_eq!(oldest_age([future], now), 0);
        assert_eq!(until(Some(now), future), 0);
        assert_eq!(millis(Duration::MAX), u64::MAX);
        assert_eq!(
            cooldowns([now, future, future + Duration::from_secs(1)], now),
            (2, 1_000)
        );
    }
}
