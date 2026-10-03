//! Retain current catch-up reachability without retaining departed-device discovery history.

use super::*;
use libp2p::kad::store::RecordStore as _;

#[derive(Default)]
pub(super) struct CheckpointDiscoveryRetention {
    eligible: HashSet<Libp2pPeerId>,
    last_membership_revision: Option<u64>,
    next_reconcile: Option<Instant>,
}

impl CheckpointDiscoveryRetention {
    pub(super) fn reconcile(
        &mut self,
        runtime: Option<&CheckpointRuntime>,
        forwarder: &Forwarder,
        membership: &OverlayMembership,
        node: &mut P2pNode,
        discovered: &mut DiscoveredPeerAddresses,
        now: Instant,
    ) -> Result<(), RunnerError> {
        let Some(runtime) = runtime else {
            return Ok(());
        };
        if self.last_membership_revision == Some(forwarder.membership_revision())
            && self.next_reconcile.is_some_and(|deadline| now < deadline)
        {
            return Ok(());
        }
        // Returning roster members and bounded reply/handoff owners need control-only
        // connectivity even when forwarding is gated. This never grants packet access.
        let mut eligible = runtime
            .state()
            .snapshot()
            .payload
            .members
            .iter()
            .map(|member| {
                member
                    .subject
                    .peer_id
                    .parse()
                    .map_err(ConfigError::Libp2pPeerId)
            })
            .collect::<Result<HashSet<_>, _>>()?;
        eligible.extend(runtime.sync_candidates(now));
        eligible.extend(membership.configured_infrastructure_peers.iter().copied());
        self.replace_eligible(eligible, node, discovered);
        self.last_membership_revision = Some(forwarder.membership_revision());
        self.next_reconcile = Some(now + Duration::from_secs(1));
        Ok(())
    }

    fn replace_eligible(
        &mut self,
        eligible: HashSet<Libp2pPeerId>,
        node: &mut P2pNode,
        discovered: &mut DiscoveredPeerAddresses,
    ) {
        let mut previous = std::mem::replace(&mut self.eligible, eligible);
        // Re-scan bounded overlay references after restoration and relearning. The
        // gated forwarder no longer exposes these stale static/protected owners.
        previous.extend(node.configured_peer_addresses.iter().map(|(peer, _)| *peer));
        previous.extend(discovered.peer_ids());
        previous.extend(discovered.retention.overlay_peers());
        let departed = previous
            .difference(&self.eligible)
            .copied()
            .collect::<HashSet<_>>();
        let mut removed_records = prune_owned_records(
            &mut node.swarm.behaviour_mut().kad,
            &node.network_name,
            node.membership_tag.as_deref(),
            &self.eligible,
        );
        if let Some(pairing_kad) = node.swarm.behaviour_mut().pairing_kad.as_mut() {
            removed_records += prune_owned_records(
                pairing_kad,
                &node.network_name,
                node.membership_tag.as_deref(),
                &self.eligible,
            );
        }
        if departed.is_empty() && removed_records == 0 {
            return;
        }
        let keep = |peer| !departed.contains(&peer);
        let configured_before = node.configured_peer_addresses.len();
        node.configured_peer_addresses
            .retain(|(peer, _)| keep(*peer));
        let removed_addresses = discovered.retention.retain_peers(keep);
        discovered.addresses.retain(|entry| keep(entry.peer));
        discovered
            .recovery_dial_attempts
            .retain(|(peer, _), _| keep(*peer));
        discovered
            .lan_first_recovery_until
            .retain(|peer, _| keep(*peer));
        let cancelled = discovered.recovery_queries.retain_peers(keep);
        let cancelled_count =
            finish_targeted_recovery_queries(&mut node.swarm.behaviour_mut().kad, cancelled);
        for peer in &departed {
            // Drop the whole routing entry, including addresses learned outside our
            // cache. Configured public infrastructure is included in `eligible` above.
            node.swarm.behaviour_mut().kad.remove_peer(peer);
            if let Some(pairing_kad) = node.swarm.behaviour_mut().pairing_kad.as_mut() {
                pairing_kad.remove_peer(peer);
            }
        }
        log_runtime_event(
            LogLevel::Info,
            "checkpoint_discovery_state_removed",
            &[
                ("peers", &departed.len().to_string()),
                ("addresses", &removed_addresses.len().to_string()),
                (
                    "configured_addresses",
                    &(configured_before - node.configured_peer_addresses.len()).to_string(),
                ),
                ("cancelled_queries", &cancelled_count.to_string()),
                ("records", &removed_records.to_string()),
            ],
        );
    }
}

fn prune_owned_records(
    kad: &mut kad::Behaviour<kad::store::MemoryStore>,
    network_name: &str,
    membership_tag: Option<&str>,
    eligible: &HashSet<Libp2pPeerId>,
) -> usize {
    let namespace = format!("/p2p-vpn/{network_name}/");
    let keys = kad
        .store_mut()
        .records()
        .filter(|record| record.key.as_ref().starts_with(namespace.as_bytes()))
        .filter_map(|record| {
            // The key and typed payload must agree before deleting application-owned
            // state. Arbitrary public DHT records are not membership history.
            if record.value.len() <= MAX_KADEMLIA_PEER_ADDRESS_RECORD_BYTES
                && let Ok(address) =
                    serde_json::from_slice::<KademliaPeerAddressRecord>(&record.value)
                && address.payload.version == 1
                && address.payload.network_name == network_name
                && let Ok(peer) = address.payload.peer_id.parse::<Libp2pPeerId>()
                && !eligible.contains(&peer)
                && (address.payload.membership_tag.as_deref() == membership_tag
                    || address.payload.membership_tag.is_none())
                && record.key
                    == crate::runtime::p2p::kademlia_peer_addresses_key(
                        network_name,
                        address.payload.membership_tag.as_deref(),
                        peer,
                    )
            {
                return Some(record.key.clone());
            }
            if record.value.len() <= MAX_KADEMLIA_MEMBERSHIP_RECORD_BYTES
                && let Ok(bundle) =
                    serde_json::from_slice::<KademliaMembershipRecordBundle>(&record.value)
                && bundle.version == 1
                && bundle.network_name == network_name
                && (bundle.membership_tag.as_deref() == membership_tag
                    || bundle.membership_tag.is_none())
                && record.key
                    == crate::runtime::p2p::kademlia_membership_records_key(
                        network_name,
                        bundle.membership_tag.as_deref(),
                    )
            {
                return Some(record.key.clone());
            }
            None
        })
        .collect::<Vec<_>>();
    for key in &keys {
        // Removing routing entries alone leaves locally published values available
        // for Kademlia's automatic republishing. Remove those values as well.
        kad.remove_record(key);
        kad.store_mut().remove(key);
        let provider_key = crate::runtime::p2p::kademlia_provider_wire_key(kad, key);
        kad.stop_providing(&provider_key);
    }
    let mut removed = keys.len();
    for tag in [membership_tag, None] {
        for key in [
            crate::runtime::p2p::kademlia_rendezvous_key(network_name, tag),
            crate::runtime::p2p::kademlia_membership_records_key(network_name, tag),
        ] {
            let wire_key = crate::runtime::p2p::kademlia_provider_wire_key(kad, &key);
            for provider in kad.store_mut().providers(&wire_key) {
                if !eligible.contains(&provider.provider) {
                    kad.store_mut()
                        .remove_provider(&wire_key, &provider.provider);
                    removed += 1;
                }
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::membership::checkpoint::{
        CheckpointMember, CooperativeMembershipState, NetworkAnchor, SnapshotPolicy,
    };
    use crate::runtime::membership_store::checkpoint::{
        CheckpointCredentials, LoadedCheckpointAuthority,
    };

    fn node(separate_pairing_dht: bool) -> P2pNode {
        let mut discovery = DiscoveryConfig::default();
        if separate_pairing_dht {
            discovery.kademlia_protocol = crate::config::PRIVATE_KADEMLIA_PROTOCOL.to_owned();
        }
        build_node(&HostConfig {
            identity: NodeIdentity::generate_ed25519().unwrap(),
            network_name: "cleanup".into(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: vec![],
            external_addresses: vec![],
            bootstrap_peers: vec![],
            known_peers: vec![],
            relay_reservations: vec![],
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: ResourceConfig::default(),
            discovery,
        })
        .unwrap()
    }

    fn address(port: u16) -> Multiaddr {
        format!("/ip4/11.252.0.2/tcp/{port}").parse().unwrap()
    }

    fn remember(
        node: &mut P2pNode,
        discovered: &mut DiscoveredPeerAddresses,
        peer: Libp2pPeerId,
        port: u16,
    ) {
        let address = address(port);
        node.configured_peer_addresses.push((peer, address.clone()));
        discovered.retention.protect(peer, address.clone());
        discovered
            .retention
            .admit(peer, address.clone(), true, Instant::now());
        discovered.insert(peer, address.clone());
        discovered.record_recovery_dial_failure_at(peer, &address, Instant::now());
        discovered.start_lan_first_recovery(peer, Instant::now());
        node.swarm
            .behaviour_mut()
            .kad
            .add_address(&peer, address.clone());
        public_pairing_kad_mut(node.swarm.behaviour_mut()).add_address(&peer, address);
    }

    fn kad_contains(kad: &mut kad::Behaviour<kad::store::MemoryStore>, peer: Libp2pPeerId) -> bool {
        kad.kbuckets().any(|bucket| {
            bucket
                .iter()
                .any(|entry| *entry.node.key.preimage() == peer)
        })
    }

    fn address_record(
        network_name: &str,
        membership_tag: Option<&str>,
        identity: &NodeIdentity,
        publisher: Libp2pPeerId,
    ) -> kad::Record {
        kad::Record {
            key: crate::runtime::p2p::kademlia_peer_addresses_key(
                network_name,
                membership_tag,
                identity.peer_id.parse().unwrap(),
            ),
            value: encode_kademlia_peer_address_record(
                network_name,
                membership_tag,
                identity,
                vec![address(4001)],
                1_000,
            )
            .unwrap(),
            publisher: Some(publisher),
            expires: None,
        }
    }

    #[tokio::test]
    async fn cleanup_erases_owned_dht_values_and_preserves_unrelated_public_records() {
        for separate in [false, true] {
            let mut node = node(separate);
            node.membership_tag = Some("current".into());
            let survivor = NodeIdentity::generate_ed25519().unwrap();
            let survivor_peer = survivor.peer_id.parse().unwrap();
            let removed = NodeIdentity::generate_ed25519().unwrap();
            let removed_peer = removed.peer_id.parse().unwrap();
            let cached = NodeIdentity::generate_ed25519().unwrap();
            let mut records = vec![
                (
                    address_record("cleanup", Some("current"), &removed, node.local_peer_id),
                    false,
                ),
                (
                    address_record("cleanup", None, &cached, removed_peer),
                    false,
                ),
                (
                    address_record("cleanup", Some("current"), &survivor, survivor_peer),
                    true,
                ),
                (
                    address_record("foreign", None, &removed, removed_peer),
                    true,
                ),
                (
                    address_record("cleanup", Some("other-network"), &removed, removed_peer),
                    true,
                ),
            ];
            let name = crate::hostname::issue_hostname_record_at(
                &removed,
                "cleanup",
                "removed-device",
                1,
                1_000,
            )
            .unwrap();
            for tag in [None, Some("current")] {
                records.push((
                    kad::Record {
                        key: crate::runtime::p2p::kademlia_membership_records_key("cleanup", tag),
                        value: encode_kademlia_membership_records(
                            "cleanup",
                            tag,
                            Vec::new(),
                            vec![name.clone()],
                        )
                        .unwrap(),
                        publisher: Some(node.local_peer_id),
                        expires: None,
                    },
                    false,
                ));
            }
            let mut wrong_key = address_record("cleanup", Some("current"), &removed, removed_peer);
            wrong_key.key = kad::RecordKey::new(&b"/p2p-vpn/cleanup/unrelated");
            records.push((wrong_key, true));
            records.push((
                kad::Record {
                    key: kad::RecordKey::new(&b"unrelated-public-data"),
                    value: b"public content".to_vec(),
                    publisher: Some(removed_peer),
                    expires: None,
                },
                true,
            ));
            let populate = |kad: &mut kad::Behaviour<kad::store::MemoryStore>| {
                for (record, _) in &records {
                    kad.store_mut().put(record.clone()).unwrap();
                }
            };
            populate(&mut node.swarm.behaviour_mut().kad);
            if let Some(pairing_kad) = node.swarm.behaviour_mut().pairing_kad.as_mut() {
                populate(pairing_kad);
            }
            let mut retention = CheckpointDiscoveryRetention::default();
            retention.eligible.insert(removed_peer);
            let mut discovered = DiscoveredPeerAddresses::default();
            // No address cache, old eligible set, or configured peer is needed to
            // retire a stale application-owned record after restoration.
            retention.replace_eligible(HashSet::from([survivor_peer]), &mut node, &mut discovered);
            for (record, retained) in &records {
                assert_eq!(
                    node.swarm
                        .behaviour_mut()
                        .kad
                        .store_mut()
                        .get(&record.key)
                        .is_some(),
                    *retained
                );
                if let Some(pairing_kad) = node.swarm.behaviour_mut().pairing_kad.as_mut() {
                    assert_eq!(
                        pairing_kad.store_mut().get(&record.key).is_some(),
                        *retained
                    );
                }
            }
            // Re-delivered obsolete records are removed without a new membership
            // revision or a permanent retired-identity archive.
            for (record, retained) in &records {
                if !retained {
                    node.swarm
                        .behaviour_mut()
                        .kad
                        .store_mut()
                        .put(record.clone())
                        .unwrap();
                }
            }
            retention.last_membership_revision = Some(1);
            retention.replace_eligible(HashSet::from([survivor_peer]), &mut node, &mut discovered);
            assert_eq!(
                node.swarm.behaviour_mut().kad.store_mut().records().count(),
                5
            );
            assert_eq!(retention.eligible, HashSet::from([survivor_peer]));
        }
    }

    #[tokio::test]
    async fn runtime_resync_gate_preserves_roster_and_expires_control_only_candidates() {
        let mut node = node(true);
        let returning = NodeIdentity::generate_ed25519().unwrap();
        let returning_peer = returning.peer_id.parse().unwrap();
        let removed = Libp2pPeerId::random();
        let candidate = Libp2pPeerId::random();
        let credentials =
            CheckpointCredentials::new(NetworkAnchor::new([7; 32]).unwrap(), vec![9; 32]).unwrap();
        let state = CooperativeMembershipState::bootstrap_at(
            credentials.capability().unwrap(),
            node.identity.peer_id.clone(),
            vec![
                CheckpointMember::new(&node.identity).unwrap(),
                CheckpointMember::new(&returning).unwrap(),
            ],
            SnapshotPolicy::default(),
            1_000,
        )
        .unwrap();
        let mut runtime = CheckpointRuntime::restore(
            "cleanup".into(),
            &node.identity.peer_id,
            LoadedCheckpointAuthority {
                credentials,
                retained: state.retained(),
                enrollment_floor: None,
            },
        )
        .unwrap();
        let config: Config = serde_json::from_value(serde_json::json!({
            "network": {"name": "cleanup", "private_key": node.identity.private_key},
            "peers": [{"id": returning.peer_id}, {"id": removed.to_string()}],
        }))
        .unwrap();
        let forwarder =
            Forwarder::from_checkpoint_config(&config, runtime.state(), runtime.anchor(), 1_000)
                .unwrap();
        assert!(!forwarder.is_configured_transport_peer(returning_peer));
        let mut membership = OverlayMembership::default();
        membership.replace_from_forwarder(&forwarder).unwrap();
        membership.replace_checkpoint_sync_peers(&runtime).unwrap();
        let now = Instant::now();
        let mut capabilities = ControlCapabilities::local("cleanup", None, 1280);
        runtime.decorate_capabilities(&mut capabilities).unwrap();
        assert!(runtime.observe_capabilities(
            candidate,
            &capabilities,
            capabilities.membership_tag.as_deref(),
            &[],
            now
        ));
        let mut discovered = DiscoveredPeerAddresses::default();
        remember(&mut node, &mut discovered, returning_peer, 4001);
        remember(&mut node, &mut discovered, removed, 4002);
        remember(&mut node, &mut discovered, candidate, 4003);
        let mut retention = CheckpointDiscoveryRetention::default();
        retention
            .reconcile(
                Some(&runtime),
                &forwarder,
                &membership,
                &mut node,
                &mut discovered,
                now,
            )
            .unwrap();
        assert!(
            discovered
                .retention
                .is_protected(returning_peer, &address(4001))
        );
        assert!(discovered.retention.is_protected(candidate, &address(4003)));
        assert!(!discovered.retention.is_protected(removed, &address(4002)));
        assert!(!kad_contains(&mut node.swarm.behaviour_mut().kad, removed));
        let expiry = now + super::super::super::checkpoint_runtime::RESYNC_WINDOW;
        // No packet or membership revision changed; bounded owners still expire.
        retention
            .reconcile(
                Some(&runtime),
                &forwarder,
                &membership,
                &mut node,
                &mut discovered,
                expiry,
            )
            .unwrap();
        assert!(
            discovered
                .retention
                .is_protected(returning_peer, &address(4001))
        );
        assert!(!discovered.retention.is_protected(candidate, &address(4003)));
        assert!(!kad_contains(
            &mut node.swarm.behaviour_mut().kad,
            candidate
        ));
        assert_eq!(
            retention.eligible,
            HashSet::from([node.local_peer_id, returning_peer])
        );
    }

    #[tokio::test]
    async fn cleanup_erases_static_discovered_and_query_owners_in_both_dht_modes() {
        for separate in [false, true] {
            let mut node = node(separate);
            let removed = Libp2pPeerId::random();
            let survivor = Libp2pPeerId::random();
            let infrastructure = Libp2pPeerId::random();
            let mut discovered = DiscoveredPeerAddresses::default();
            remember(&mut node, &mut discovered, removed, 4001);
            remember(&mut node, &mut discovered, survivor, 4002);
            let public_address = address(4003);
            discovered
                .retention
                .protect(infrastructure, public_address.clone());
            learn_public_pairing_address(
                &mut node.swarm,
                &mut discovered,
                &RuntimeMetrics::default(),
                infrastructure,
                public_address.clone(),
            );
            assert!(!discovered.should_query_recovery_discovery_at(removed, Instant::now()));
            let query_now = Instant::now() + PEER_RECOVERY_LAN_FIRST_GRACE;
            assert!(discovered.should_query_recovery_discovery_at(removed, query_now));
            let removed_query = node
                .swarm
                .behaviour_mut()
                .kad
                .get_record(kad::RecordKey::new(&b"removed"));
            discovered.record_recovery_discovery_queries(removed, [removed_query], query_now);
            let public_query = node
                .swarm
                .behaviour_mut()
                .kad
                .get_record(kad::RecordKey::new(&b"public"));
            assert!(kad_contains(&mut node.swarm.behaviour_mut().kad, removed));
            assert!(kad_contains(
                public_pairing_kad_mut(node.swarm.behaviour_mut()),
                removed
            ));

            let mut retention = CheckpointDiscoveryRetention::default();
            retention.replace_eligible(HashSet::from([survivor]), &mut node, &mut discovered);
            assert_eq!(
                node.configured_peer_addresses,
                vec![(survivor, address(4002))]
            );
            assert_eq!(discovered.peer_ids().collect::<Vec<_>>(), vec![survivor]);
            assert!(!discovered.retention.is_protected(removed, &address(4001)));
            assert!(
                discovered
                    .retention
                    .is_protected(infrastructure, &public_address)
            );
            assert!(
                discovered
                    .retention
                    .retain_peers(|peer| peer != removed)
                    .is_empty()
            );
            assert!(
                discovered
                    .recovery_dial_attempts
                    .keys()
                    .all(|(peer, _)| *peer == survivor)
            );
            assert!(!discovered.lan_first_recovery_until.contains_key(&removed));
            assert!(discovered.recovery_queries.state(&removed).is_none());
            assert!(node.swarm.behaviour().kad.query(&removed_query).is_none());
            assert!(node.swarm.behaviour().kad.query(&public_query).is_some());
            assert!(!kad_contains(&mut node.swarm.behaviour_mut().kad, removed));
            assert!(!kad_contains(
                public_pairing_kad_mut(node.swarm.behaviour_mut()),
                removed
            ));
            assert!(kad_contains(
                public_pairing_kad_mut(node.swarm.behaviour_mut()),
                infrastructure
            ));
            assert!(kad_contains(&mut node.swarm.behaviour_mut().kad, survivor));
            assert_eq!(retention.eligible, HashSet::from([survivor]));
            remember(&mut node, &mut discovered, removed, 4001);
            retention.last_membership_revision = Some(1);
            retention.replace_eligible(HashSet::from([survivor]), &mut node, &mut discovered);
            assert!(!discovered.retention.is_protected(removed, &address(4001)));
            assert!(!kad_contains(&mut node.swarm.behaviour_mut().kad, removed));
            assert!(!kad_contains(
                public_pairing_kad_mut(node.swarm.behaviour_mut()),
                removed
            ));
            assert_eq!(retention.eligible, HashSet::from([survivor]));
        }
    }

    #[tokio::test]
    async fn cleanup_retires_scoped_provider_records_without_purging_public_provider_cache() {
        for separate in [false, true] {
            let mut node = node(separate);
            let tag = "c".repeat(64);
            let survivor = Libp2pPeerId::random();
            let removed = Libp2pPeerId::random();
            let public_key = kad::RecordKey::new(&b"public-content-key");
            let populate_and_verify = |kad: &mut kad::Behaviour<kad::store::MemoryStore>| {
                let mut keys = Vec::new();
                for scope in [Some(tag.as_str()), None] {
                    for key in [
                        crate::runtime::p2p::kademlia_rendezvous_key("cleanup", scope),
                        crate::runtime::p2p::kademlia_membership_records_key("cleanup", scope),
                    ] {
                        keys.push(crate::runtime::p2p::kademlia_provider_wire_key(kad, &key));
                    }
                }
                for key in keys.iter().chain([&public_key]) {
                    for provider in [survivor, removed] {
                        kad.store_mut()
                            .add_provider(kad::ProviderRecord {
                                key: key.clone(),
                                provider,
                                expires: None,
                                addresses: vec![address(4001)],
                            })
                            .unwrap();
                    }
                }
                assert_eq!(
                    prune_owned_records(kad, "cleanup", Some(&tag), &HashSet::from([survivor]),),
                    4
                );
                for key in &keys {
                    let providers = kad.store_mut().providers(key);
                    assert_eq!(providers.len(), 1);
                    assert_eq!(providers[0].provider, survivor);
                }
                assert_eq!(kad.store_mut().providers(&public_key).len(), 2);
            };
            populate_and_verify(&mut node.swarm.behaviour_mut().kad);
            if let Some(pairing_kad) = node.swarm.behaviour_mut().pairing_kad.as_mut() {
                populate_and_verify(pairing_kad);
            }
        }
    }

    #[tokio::test]
    async fn cleanup_preserves_catch_up_and_bounded_reply_peers_until_their_scope_ends() {
        let mut node = node(true);
        let returning = Libp2pPeerId::random();
        let reply = Libp2pPeerId::random();
        let mut discovered = DiscoveredPeerAddresses::default();
        remember(&mut node, &mut discovered, returning, 4001);
        remember(&mut node, &mut discovered, reply, 4002);
        let mut retention = CheckpointDiscoveryRetention::default();
        retention.replace_eligible(
            HashSet::from([returning, reply]),
            &mut node,
            &mut discovered,
        );
        retention.last_membership_revision = Some(1);
        assert_eq!(node.configured_peer_addresses.len(), 2);
        assert_eq!(discovered.peer_ids().count(), 2);
        retention.replace_eligible(HashSet::from([returning]), &mut node, &mut discovered);
        assert_eq!(
            node.configured_peer_addresses,
            vec![(returning, address(4001))]
        );
        assert!(!discovered.retention.is_protected(reply, &address(4002)));
        assert!(!kad_contains(&mut node.swarm.behaviour_mut().kad, reply));
        assert!(kad_contains(&mut node.swarm.behaviour_mut().kad, returning));
        assert_eq!(retention.eligible, HashSet::from([returning]));
    }

    #[tokio::test]
    async fn repeated_checkpoint_cleanup_keeps_only_the_current_peer_and_addresses() {
        let mut node = node(true);
        let survivor = Libp2pPeerId::random();
        let mut discovered = DiscoveredPeerAddresses::default();
        remember(&mut node, &mut discovered, survivor, 4001);
        let mut retention = CheckpointDiscoveryRetention::default();
        retention.replace_eligible(HashSet::from([survivor]), &mut node, &mut discovered);
        retention.last_membership_revision = Some(1);
        for _ in 0..512 {
            let identity = NodeIdentity::generate_ed25519().unwrap();
            let peer = identity.peer_id.parse().unwrap();
            remember(&mut node, &mut discovered, peer, 4002);
            let record = address_record("cleanup", None, &identity, peer);
            node.swarm
                .behaviour_mut()
                .kad
                .store_mut()
                .put(record.clone())
                .unwrap();
            public_pairing_kad_mut(node.swarm.behaviour_mut())
                .store_mut()
                .put(record)
                .unwrap();
            retention.replace_eligible(HashSet::from([survivor, peer]), &mut node, &mut discovered);
            retention.replace_eligible(HashSet::from([survivor]), &mut node, &mut discovered);
            assert_eq!(retention.eligible.len(), 1);
            assert_eq!(node.configured_peer_addresses.len(), 1);
            assert_eq!(discovered.peer_ids().count(), 1);
            assert_eq!(discovered.recovery_dial_attempts.len(), 1);
            assert_eq!(discovered.lan_first_recovery_until.len(), 1);
            assert_eq!(discovered.retention.overlay_peers().count(), 1);
            assert!(!discovered.retention.is_protected(peer, &address(4002)));
            assert!(!kad_contains(&mut node.swarm.behaviour_mut().kad, peer));
            assert!(!kad_contains(
                public_pairing_kad_mut(node.swarm.behaviour_mut()),
                peer
            ));
            assert_eq!(
                node.swarm.behaviour_mut().kad.store_mut().records().count(),
                0
            );
            assert_eq!(
                public_pairing_kad_mut(node.swarm.behaviour_mut())
                    .store_mut()
                    .records()
                    .count(),
                0
            );
        }
    }
}
