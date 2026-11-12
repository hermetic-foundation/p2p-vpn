//! Physical packet-plane retirement after authenticated quiet commitment.
use super::*;

pub(super) struct DatagramRetirementContext<'a> {
    pub forwarder: &'a Forwarder,
    pub queues: &'a PeerQueues,
    pub in_flight: &'a PacketInFlight,
    pub udp: &'a mut PacketPlaneRuntime,
    pub quic: Option<&'a mut PacketPlaneQuicRuntime>,
    pub negotiator: &'a mut PacketPlaneNegotiator,
    pub paths: &'a mut PathSet,
    pub probes: &'a mut PathProbeTracker,
}

pub(super) fn retire_datagrams(mut context: DatagramRetirementContext<'_>) -> usize {
    let mut retired = 0;
    for peer in context.forwarder.configured_overlay_peers() {
        if !context.forwarder.data_path_is_dormant(peer)
            || context.queues.peer_stats(peer).queued_packets > 0
            || context.in_flight.has_application_work(peer)
        {
            continue;
        }
        let pending = context.negotiator.pending.contains_key(&peer)
            || context.negotiator.pending_responders.contains_key(&peer)
            || context
                .negotiator
                .quic_connection_task_handles
                .keys()
                .any(|(owner, _)| *owner == peer);
        context.negotiator.remove_peer(peer);
        let udp = context.udp.forget_peer(peer);
        let quic = context
            .quic
            .as_deref_mut()
            .is_some_and(|quic| quic.forget_peer(peer));
        context.paths.retire_datagram_paths(peer);
        context.probes.clear_peer(peer);
        if udp || quic || pending {
            retired += 1;
            log_runtime_event(
                LogLevel::Info,
                "dormant_datagrams_retired",
                &[
                    ("peer", &peer.to_string()),
                    ("udp", &udp.to_string()),
                    ("quic", &quic.to_string()),
                    ("pending_negotiation", &pending.to_string()),
                ],
            );
        }
    }
    retired
}

const INITIAL_CONTROL_GRACE: Duration = Duration::from_secs(10);

#[cfg(test)]
#[path = "dormant_stream_tests.rs"]
mod stream_tests;

pub(super) struct StreamRetirementContext<'a> {
    pub node: &'a mut P2pNode,
    pub forwarder: &'a Forwarder,
    pub queues: &'a PeerQueues,
    pub in_flight: &'a PacketInFlight,
    pub epochs: &'a mut ConnectionEpochs,
    pub connections: &'a HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    pub infrastructure: &'a InfrastructurePeers,
    pub relay_readiness: &'a RelayReadiness,
    pub auto_relay: &'a AutoRelayState,
    pub now: Instant,
}

fn can_retire_stream(
    context: &StreamRetirementContext<'_>,
    peer: Libp2pPeerId,
    id: ConnectionId,
) -> bool {
    let overlay = PeerId::from_libp2p(peer);
    context.forwarder.is_configured_transport_peer(peer)
        && context.forwarder.data_path_is_dormant(overlay)
        && context.queues.peer_stats(overlay).queued_packets == 0
        && !context.in_flight.has_application_work(overlay)
        && !context.node.swarm.behaviour().has_pending_peer_work(peer)
        && context.epochs.is_usable(id)
        && context.epochs.established.get(&id).is_some_and(|opened| {
            context.now.saturating_duration_since(*opened) >= INITIAL_CONTROL_GRACE
        })
        && context.connections.contains_key(&(peer, id))
        && !is_infrastructure_dependency(context, peer)
}

fn is_infrastructure_dependency(context: &StreamRetirementContext<'_>, peer: Libp2pPeerId) -> bool {
    context.node.startup.relay_server_enabled
        || context
            .node
            .bootstrap_peer_addresses
            .iter()
            .any(|(owner, _)| *owner == peer)
        || context
            .node
            .relay_peer_addresses
            .iter()
            .any(|(owner, _)| *owner == peer)
        || context.infrastructure.contains(peer)
        || context
            .relay_readiness
            .accepted_reservations
            .contains(&peer)
        || context
            .relay_readiness
            .relayed_listen_addresses
            .contains_key(&peer)
        || context.auto_relay.pending_reservations.contains_key(&peer)
        || context
            .auto_relay
            .accepted_reservation_peers
            .contains(&peer)
        || context
            .connections
            .values()
            .any(|endpoint| relay_peer_for_endpoint(endpoint) == Some(PeerId::from_libp2p(peer)))
}

pub(super) fn retire_streams(context: &mut StreamRetirementContext<'_>) -> usize {
    let mut candidates = context
        .connections
        .keys()
        .copied()
        .filter(|(peer, id)| can_retire_stream(context, *peer, *id))
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|(_, id)| *id);
    let mut retired = 0;
    for (peer, id) in candidates {
        if !can_retire_stream(context, peer, id) || !context.epochs.mark_retiring(id) {
            continue;
        }
        let close_requested = context.node.swarm.close_connection(id);
        retired += usize::from(close_requested);
        log_runtime_event(
            LogLevel::Info,
            "dormant_stream_retired",
            &[
                ("peer", &peer.to_string()),
                ("connection_id", &id.to_string()),
                ("close_requested", &close_requested.to_string()),
            ],
        );
    }
    retired
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{path::PathCandidate, queue::Packet};

    struct Fixture {
        forwarder: Forwarder,
        peer: PeerId,
        queues: PeerQueues,
        in_flight: PacketInFlight,
        udp: PacketPlaneRuntime,
        negotiator: PacketPlaneNegotiator,
        paths: PathSet,
        probes: PathProbeTracker,
    }

    impl Fixture {
        fn new() -> Self {
            Self::for_peer(Libp2pPeerId::random())
        }

        fn for_peer(remote: Libp2pPeerId) -> Self {
            let identity = NodeIdentity::generate_ed25519().unwrap();
            let config: Config = serde_json::from_value(serde_json::json!({
                "network":{"name":"dormant-path-retirement","private_key":identity.private_key},
                "peers":[{"id":remote.to_string()}],
            }))
            .unwrap();
            let peer = PeerId::from_libp2p(remote);
            let mut paths = PathSet::new();
            paths.record_established(peer, PathKind::DirectUdpDatagram);
            paths.record_established(peer, PathKind::DirectQuicDatagram);
            paths.record_established(peer, PathKind::DirectTcpStream);
            let mut negotiator = PacketPlaneNegotiator::default();
            negotiator.next_quic_attempt.insert(peer, Instant::now());
            Self {
                forwarder: Forwarder::from_config(&config).unwrap(),
                peer,
                queues: PeerQueues::new(4, 4096),
                in_flight: PacketInFlight::new(4),
                udp: PacketPlaneRuntime::disabled(),
                negotiator,
                paths,
                probes: PathProbeTracker::default(),
            }
        }

        fn quiet(&self) {
            let version = self
                .forwarder
                .application_demand(self.peer)
                .unwrap()
                .version
                .unwrap();
            assert!(self.forwarder.grant_data_path_quiet(self.peer, version));
        }

        fn retire(&mut self) -> usize {
            retire_datagrams(DatagramRetirementContext {
                forwarder: &self.forwarder,
                queues: &self.queues,
                in_flight: &self.in_flight,
                udp: &mut self.udp,
                quic: None,
                negotiator: &mut self.negotiator,
                paths: &mut self.paths,
                probes: &mut self.probes,
            })
        }

        fn path(&self, kind: PathKind) -> PathCandidate {
            self.paths
                .candidates_for(self.peer)
                .find(|path| path.kind == kind)
                .unwrap()
        }
    }

    #[test]
    fn only_current_quiet_permission_can_retire_paths() {
        let mut fixture = Fixture::new();
        assert_eq!(fixture.retire(), 0);
        assert!(fixture.path(PathKind::DirectQuicDatagram).healthy);
        assert!(
            fixture
                .negotiator
                .next_quic_attempt
                .contains_key(&fixture.peer)
        );
        fixture.quiet();
        assert!(
            fixture
                .forwarder
                .record_application_demand(fixture.peer, Instant::now())
        );
        fixture.retire();
        assert!(fixture.path(PathKind::DirectQuicDatagram).healthy);
    }

    #[test]
    fn retirement_preserves_streams_membership_and_path_quality() {
        let mut fixture = Fixture::new();
        fixture.quiet();
        let before = fixture.path(PathKind::DirectQuicDatagram);
        fixture.retire();
        let after = fixture.path(PathKind::DirectQuicDatagram);
        assert!(!after.healthy);
        assert_eq!(after.established_connections, 0);
        assert_eq!(before.failure_penalty, after.failure_penalty);
        assert_eq!(before.estimated_mtu, after.estimated_mtu);
        assert!(fixture.path(PathKind::DirectTcpStream).healthy);
        assert!(
            fixture
                .forwarder
                .transport_peer_for_overlay(fixture.peer)
                .is_some()
        );
        assert!(
            !fixture
                .negotiator
                .next_quic_attempt
                .contains_key(&fixture.peer)
        );
        assert_eq!(fixture.retire(), 0);
        fixture
            .paths
            .record_established(fixture.peer, PathKind::DirectQuicDatagram);
        assert_eq!(
            fixture.path(PathKind::DirectQuicDatagram).failure_penalty,
            before.failure_penalty
        );
    }

    #[test]
    fn queued_payload_and_in_flight_work_fence_retirement() {
        let mut fixture = Fixture::new();
        fixture.quiet();
        let packet = Packet::new(fixture.peer, 1, vec![1; 20]);
        fixture.queues.enqueue(packet.clone()).unwrap();
        fixture.retire();
        assert!(fixture.path(PathKind::DirectQuicDatagram).healthy);
        fixture.queues.dequeue().unwrap();
        let mut wire = request_response::Behaviour::<crate::runtime::packet::PacketCodec>::new(
            [(
                libp2p::StreamProtocol::new(crate::runtime::packet::PACKET_PROTOCOL),
                request_response::ProtocolSupport::Full,
            )],
            request_response::Config::default(),
        );
        let remote = fixture
            .forwarder
            .transport_peer_for_overlay(fixture.peer)
            .unwrap();
        let id = PacketInFlightId::RequestResponse(
            wire.send_request(&remote, Frame::packet(1, 1, vec![1; 20]).unwrap()),
        );
        let probe = PacketInFlightId::RequestResponse(
            wire.send_request(&remote, Frame::packet(1, 2, vec![1; 20]).unwrap()),
        );
        fixture.in_flight.record_path_probe(
            fixture.peer,
            probe,
            fixture.path(PathKind::DirectTcpStream),
        );
        fixture
            .in_flight
            .record(&packet, id, fixture.path(PathKind::DirectTcpStream));
        fixture.retire();
        assert!(fixture.path(PathKind::DirectQuicDatagram).healthy);
        fixture.in_flight.complete(id);
        assert!(!fixture.in_flight.has_application_work(fixture.peer));
        assert_eq!(fixture.in_flight.in_flight_for(fixture.peer), 1);
        fixture.retire();
        assert!(!fixture.path(PathKind::DirectQuicDatagram).healthy);
        fixture.in_flight.complete(probe);
        assert_eq!(fixture.in_flight.in_flight_for(fixture.peer), 0);
    }

    #[tokio::test]
    async fn quiet_rejects_inbound_and_completed_hellos_until_actual_demand() {
        let mut fixture = Fixture::new();
        fixture.quiet();
        let peer = fixture
            .forwarder
            .transport_peer_for_overlay(fixture.peer)
            .unwrap();
        let identity = fixture.forwarder.config().identity().unwrap();
        let capabilities = PeerCapabilities::default();
        let local = ControlCapabilities::local("dormant-path-retirement", None, 1280);
        let metrics = RuntimeMetrics::default();
        for quiet in [true, false] {
            if !quiet {
                fixture
                    .forwarder
                    .record_application_demand(fixture.peer, Instant::now());
            }
            let incoming = accept_packet_plane_hello(
                &mut PacketPlaneAcceptContext {
                    active_connections: &HashMap::new(),
                    forwarder: &fixture.forwarder,
                    peer_capabilities: &capabilities,
                    paths: &mut fixture.paths,
                    metrics: &metrics,
                    packet_plane: &mut fixture.udp,
                    packet_plane_quic: None,
                    negotiator: &mut fixture.negotiator,
                    identity: &identity,
                    local_capabilities: &local,
                    path_probe_tracker: &mut fixture.probes,
                },
                peer,
                &[],
            )
            .await;
            let completed = complete_packet_plane_hello(
                &mut PacketPlaneCompleteContext {
                    forwarder: &fixture.forwarder,
                    peer_capabilities: &capabilities,
                    packet_plane: &mut fixture.udp,
                    packet_plane_quic: None,
                    negotiator: &mut fixture.negotiator,
                    paths: &mut fixture.paths,
                    metrics: &metrics,
                    network_name: &local.network_name,
                    path_probe_tracker: &mut fixture.probes,
                },
                peer,
                &[],
            );
            for result in [incoming.map(|_| ()), completed] {
                if quiet {
                    assert!(matches!(
                        result,
                        Err(PacketPlaneNegotiationError::DormantPeer)
                    ));
                } else {
                    assert!(matches!(
                        result,
                        Err(PacketPlaneNegotiationError::MissingRemoteCapabilities
                            | PacketPlaneNegotiationError::NoPendingHello)
                    ));
                }
            }
        }
        assert!(!fixture.udp.has_session(fixture.peer));
        assert!(!fixture.negotiator.has_pending(fixture.peer));
    }

    #[tokio::test]
    async fn retirement_cancels_only_owned_tasks_and_rejects_late_results() {
        let mut fixture = Fixture::new();
        fixture.quiet();
        let other = PeerId::from_bytes([19; 32]);
        for (peer, role) in [
            (fixture.peer, PacketPlaneQuicNegotiationRole::Initiator),
            (fixture.peer, PacketPlaneQuicNegotiationRole::Responder),
            (other, PacketPlaneQuicNegotiationRole::Initiator),
        ] {
            let abort_handle = fixture
                .negotiator
                .quic_connection_tasks
                .spawn(std::future::pending());
            fixture.negotiator.quic_connection_task_handles.insert(
                (peer, role),
                PacketPlaneQuicConnectionTask {
                    generation: 7,
                    abort_handle,
                },
            );
        }
        fixture.negotiator.next_quic_connection_task_generation = 7;
        assert_eq!(fixture.retire(), 1);
        assert_eq!(fixture.negotiator.next_quic_connection_task_generation, 7);
        for role in [
            PacketPlaneQuicNegotiationRole::Initiator,
            PacketPlaneQuicNegotiationRole::Responder,
        ] {
            assert!(
                !fixture
                    .negotiator
                    .finish_quic_connection_task(fixture.peer, role, 7)
            );
        }
        assert!(
            fixture
                .negotiator
                .quic_connection_task_handles
                .contains_key(&(other, PacketPlaneQuicNegotiationRole::Initiator))
        );
        for _ in 0..2 {
            assert!(
                tokio::time::timeout(
                    Duration::from_secs(2),
                    fixture.negotiator.quic_connection_tasks.join_next()
                )
                .await
                .unwrap()
                .unwrap()
                .is_err_and(|error| error.is_cancelled())
            );
        }
        fixture.negotiator.clear();
    }
}
