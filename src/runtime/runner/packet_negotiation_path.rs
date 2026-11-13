//! Control-path reachability is separate from bound QUIC data-path admission.
use super::*;

pub(super) fn permitted(
    forwarder: &Forwarder,
    paths: &PathSet,
    peer: PeerId,
    backend: PacketDatagramBackend,
    local: &ControlCapabilities,
    remote: &ControlCapabilities,
    now: Instant,
) -> bool {
    if forwarder.transport_peer_for_overlay(peer).is_none() {
        return false;
    }
    if has_direct_packet_plane_negotiation_path(paths, peer) {
        return true;
    }
    backend == PacketDatagramBackend::OwnedQuic
        && local.demand_lifecycle_epoch().is_some()
        && remote.demand_lifecycle_epoch().is_some()
        && local.network_name == remote.network_name
        && local.membership_tag == remote.membership_tag
        && [local, remote].into_iter().all(|capabilities| {
            capabilities.supports_owned_quic_packet_plane
                && capabilities.supports_bound_owned_quic_packet_plane
                && !capabilities
                    .owned_quic_packet_endpoint_candidates
                    .is_empty()
                && capabilities
                    .owned_quic_packet_plane_certificate_der
                    .as_ref()
                    .is_some_and(|certificate| !certificate.is_empty())
        })
        && !forwarder.data_path_maintenance_is_paused(peer)
        && forwarder.application_demand(peer).is_some_and(|demand| {
            demand.version.is_some()
                && demand.last_demand.is_some_and(|last| {
                    now.saturating_duration_since(last) < super::super::lifecycle::IDLE_AFTER
                })
        })
        && paths.candidates_for(peer).any(|path| {
            path.kind == PathKind::CircuitRelay && path.healthy && path.established_connections > 0
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::lifecycle::{Epoch, IDLE_AFTER};

    fn fixture() -> (Forwarder, PeerId, PathSet, ControlCapabilities, Instant) {
        let local = NodeIdentity::generate_ed25519().unwrap();
        let local_peer = PeerId::from_libp2p(local.peer_id.parse().unwrap());
        let remote = loop {
            let candidate = Libp2pPeerId::random();
            if local_peer.as_bytes() <= PeerId::from_libp2p(candidate).as_bytes()
                && crate::route::builtin_ipv4(PeerId::from_libp2p(candidate))
                    != crate::route::builtin_ipv4(local_peer)
            {
                break candidate;
            }
        };
        let config: Config = serde_json::from_value(serde_json::json!({
            "network":{"name":"relay-quic-admission","private_key":local.private_key},
            "peers":[{"id":remote.to_string()}],
        }))
        .unwrap();
        let forwarder = Forwarder::from_config(&config).unwrap();
        let peer = PeerId::from_libp2p(remote);
        let mut paths = PathSet::new();
        paths.record_established(peer, PathKind::CircuitRelay);
        let capabilities = ControlCapabilities::local("relay-quic-admission", None, 1280)
            .with_demand_lifecycle_epoch(Epoch::new([1; 16]).unwrap())
            .with_owned_quic_packet_plane_certificate(vec![1])
            .with_owned_quic_packet_endpoint_candidates(vec!["127.0.0.1:12345".to_owned()]);
        (forwarder, peer, paths, capabilities, Instant::now())
    }

    #[test]
    fn relayed_bound_quic_requires_fresh_actual_demand_without_renewing_it() {
        let (forwarder, peer, paths, capabilities, now) = fixture();
        let allowed = |time| {
            permitted(
                &forwarder,
                &paths,
                peer,
                PacketDatagramBackend::OwnedQuic,
                &capabilities,
                &capabilities,
                time,
            )
        };
        assert!(
            !allowed(now),
            "mere capabilities and an incoming path are not application demand"
        );
        forwarder.record_application_demand(peer, now);
        let observed = forwarder.application_demand(peer);
        assert!(allowed(now));
        assert!(allowed(now + IDLE_AFTER - Duration::from_nanos(1)));
        assert!(!allowed(now + IDLE_AFTER));
        assert_eq!(forwarder.application_demand(peer), observed);
    }

    #[test]
    fn relayed_admission_preserves_backend_scope_binding_and_authority_gates() {
        let (forwarder, peer, mut paths, capabilities, now) = fixture();
        forwarder.record_application_demand(peer, now);
        for variant in 0..8 {
            let mut changed = capabilities.clone();
            match variant {
                0 => changed.supports_demand_lifecycle = false,
                1 => changed.supports_bound_owned_quic_packet_plane = false,
                2 => changed.supports_owned_quic_packet_plane = false,
                3 => changed.owned_quic_packet_plane_certificate_der = Some(Vec::new()),
                4 => changed.owned_quic_packet_endpoint_candidates.clear(),
                5 => changed.network_name = "another-network".to_owned(),
                6 => changed.membership_tag = Some("another-scope".to_owned()),
                7 => changed.demand_lifecycle_epoch = None,
                _ => unreachable!(),
            }
            for (local, remote) in [(&changed, &capabilities), (&capabilities, &changed)] {
                assert!(
                    !permitted(
                        &forwarder,
                        &paths,
                        peer,
                        PacketDatagramBackend::OwnedQuic,
                        local,
                        remote,
                        now
                    ),
                    "gate {variant} must hold at both endpoints"
                );
            }
        }
        assert!(!permitted(
            &forwarder,
            &paths,
            peer,
            PacketDatagramBackend::OwnedUdp,
            &capabilities,
            &capabilities,
            now
        ));
        assert!(!permitted(
            &forwarder,
            &paths,
            PeerId::from_libp2p(Libp2pPeerId::random()),
            PacketDatagramBackend::OwnedQuic,
            &capabilities,
            &capabilities,
            now
        ));
        paths.mark_unhealthy(peer, PathKind::CircuitRelay);
        assert!(!permitted(
            &forwarder,
            &paths,
            peer,
            PacketDatagramBackend::OwnedQuic,
            &capabilities,
            &capabilities,
            now
        ));
    }

    #[test]
    fn maintenance_pause_and_committed_quiet_prevent_relay_negotiation() {
        let (forwarder, peer, paths, capabilities, now) = fixture();
        forwarder.record_application_demand(peer, now);
        let version = forwarder.application_demand(peer).unwrap().version.unwrap();
        assert!(forwarder.pause_data_path_maintenance(peer, version));
        assert!(!permitted(
            &forwarder,
            &paths,
            peer,
            PacketDatagramBackend::OwnedQuic,
            &capabilities,
            &capabilities,
            now
        ));
        forwarder.record_application_demand(peer, now + Duration::from_secs(1));
        let quiet_version = forwarder.application_demand(peer).unwrap().version.unwrap();
        assert!(forwarder.grant_data_path_quiet(peer, quiet_version));
        assert!(!permitted(
            &forwarder,
            &paths,
            peer,
            PacketDatagramBackend::OwnedQuic,
            &capabilities,
            &capabilities,
            now
        ));
        forwarder.record_application_demand(peer, now + Duration::from_secs(2));
        assert!(permitted(
            &forwarder,
            &paths,
            peer,
            PacketDatagramBackend::OwnedQuic,
            &capabilities,
            &capabilities,
            now + Duration::from_secs(2)
        ));
    }

    #[test]
    fn existing_direct_legacy_negotiation_is_unchanged() {
        let (forwarder, peer, mut paths, _, now) = fixture();
        paths.record_established(peer, PathKind::DirectTcpStream);
        let legacy = ControlCapabilities::local("relay-quic-admission", None, 1280);
        for backend in [
            PacketDatagramBackend::OwnedUdp,
            PacketDatagramBackend::OwnedQuic,
        ] {
            assert!(permitted(
                &forwarder, &paths, peer, backend, &legacy, &legacy, now
            ));
        }
    }

    #[tokio::test]
    async fn disconnected_cached_relay_metadata_cannot_create_a_negotiation_dial() {
        let (forwarder, peer, paths, capabilities, now) = fixture();
        forwarder.record_application_demand(peer, now);
        let config = forwarder.config();
        let mut node = crate::runtime::p2p::build_node(&HostConfig {
            identity: config.identity().unwrap(),
            network_name: config.network.name.clone(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 8,
            max_concurrent_packet_streams: 8,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: DiscoveryConfig {
                mdns: false,
                kademlia: false,
                autonat: false,
                dcutr: false,
                ..DiscoveryConfig::default()
            },
        })
        .unwrap();
        let udp = PacketPlaneRuntime::disabled();
        let quic = PacketPlaneQuicRuntime::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let local = capabilities
            .clone()
            .with_owned_quic_packet_plane_certificate(quic.snapshot().certificate_der.unwrap());
        assert!(permitted(
            &forwarder,
            &paths,
            peer,
            PacketDatagramBackend::OwnedQuic,
            &local,
            &capabilities,
            now
        ));
        assert_eq!(
            packet_plane_negotiation_backend(
                &paths,
                &local,
                &capabilities,
                &udp,
                Some(&quic),
                peer
            ),
            Some(PacketDatagramBackend::OwnedQuic)
        );
        let mut negotiator = PacketPlaneNegotiator::default();
        let before = forwarder.application_demand(peer);
        let remote = forwarder.transport_peer_for_overlay(peer).unwrap();
        assert!(!node.swarm.is_connected(&remote));
        maybe_send_packet_plane_hello(
            &mut node.swarm,
            &forwarder,
            &paths,
            &udp,
            Some(&quic),
            &mut negotiator,
            &config.identity().unwrap(),
            &local,
            remote,
            &capabilities,
            &RuntimeMetrics::default(),
            &HashMap::new(),
        );
        assert!(!negotiator.has_pending(peer));
        assert!(!negotiator.has_quic_connection_tasks());
        assert!(!node.swarm.behaviour().control.has_any_pending_requests());
        assert_eq!(forwarder.application_demand(peer), before);
    }

    #[tokio::test]
    async fn idle_relay_hello_is_rejected_before_signature_work_or_task_creation() {
        let (forwarder, peer, mut paths, capabilities, _) = fixture();
        let mut peers = PeerCapabilities::default();
        peers.record(peer, capabilities.clone());
        let mut udp = PacketPlaneRuntime::disabled();
        let mut negotiator = PacketPlaneNegotiator::default();
        let before = forwarder.application_demand(peer);
        let result = accept_packet_plane_hello(
            &mut PacketPlaneAcceptContext {
                active_connections: &HashMap::new(),
                forwarder: &forwarder,
                peer_capabilities: &peers,
                paths: &mut paths,
                metrics: &RuntimeMetrics::default(),
                packet_plane: &mut udp,
                packet_plane_quic: None,
                negotiator: &mut negotiator,
                identity: &forwarder.config().identity().unwrap(),
                local_capabilities: &capabilities,
                path_probe_tracker: &mut PathProbeTracker::default(),
            },
            forwarder.transport_peer_for_overlay(peer).unwrap(),
            &[0],
        )
        .await;
        assert!(matches!(
            result,
            Err(PacketPlaneNegotiationError::NoDirectNegotiationPath)
        ));
        assert!(!negotiator.has_pending(peer));
        assert!(!negotiator.has_quic_connection_tasks());
        assert_eq!(forwarder.application_demand(peer), before);
    }
}
