//! Live loopback contention, not production-timer settling or WAN acceptance.
//! The receiver serves Kad; a held TCP handshake keeps real discovery pending.

use super::*;
use libp2p::kad::store::RecordStore as _;
use tokio::net::{TcpListener, TcpStream};

type TestKad = kad::Behaviour<kad::store::MemoryStore>;

// Frozen before execution. Only record replication is accelerated, to inject
// background work; production query capacity, timeouts and job limits remain.
const CASE_DEADLINE: Duration = Duration::from_secs(20);
const CONNECT_DEADLINE: Duration = Duration::from_secs(5);
const PACKET_PHASE_DEADLINE: Duration = Duration::from_secs(3);
const BACKGROUND_DEADLINE: Duration = Duration::from_secs(5);
const CLEANUP_DEADLINE: Duration = Duration::from_secs(2);
const SATURATED_DWELL: Duration = Duration::from_millis(750);
const AUTONAT_DWELL: Duration = Duration::from_millis(250);
const BACKGROUND_DWELL: Duration = Duration::from_millis(500);
const REPLICATION_INTERVAL: Duration = Duration::from_millis(100);
const PACKETS_PER_DIRECTION: usize = 16;
const PACKET_INTERVAL: Duration = Duration::from_millis(25);
const QUERY_CAPACITY: usize = 32;

#[tokio::test]
async fn live_kademlia_contention_preserves_tcp_and_quic_packets_and_resumes_producers() {
    for protocol in [
        PUBLIC_IPFS_KADEMLIA_PROTOCOL,
        crate::config::PRIVATE_KADEMLIA_PROTOCOL,
    ] {
        for listen in ["/ip4/127.0.0.1/tcp/0", "/ip4/127.0.0.1/udp/0/quic-v1"] {
            tokio::time::timeout(CASE_DEADLINE, exercise_contention(protocol, listen))
                .await
                .unwrap_or_else(|_| panic!("contention case exceeded 20s: {protocol}, {listen}"));
        }
    }
}

fn test_node(
    identity: NodeIdentity,
    protocol: &'static str,
    listen: &str,
    sender: bool,
) -> P2pNode {
    let mut node = build_node(&HostConfig {
        identity,
        network_name: "kad-contention".to_owned(),
        membership_tag: None,
        mtu: 1280,
        max_concurrent_control_streams: 64,
        max_concurrent_packet_streams: 256,
        listen_addresses: vec![listen.parse().unwrap()],
        external_addresses: Vec::new(),
        bootstrap_peers: Vec::new(),
        known_peers: Vec::new(),
        relay_reservations: Vec::new(),
        relay_server: false,
        relay_resources: crate::config::RelayResourceConfig::default(),
        resources: ResourceConfig::default(),
        discovery: DiscoveryConfig {
            mdns: false,
            dcutr: false,
            autonat: true,
            kademlia_protocol: protocol.to_owned(),
            ..DiscoveryConfig::default()
        },
    })
    .unwrap();
    assert!(node.swarm.behaviour().autonat.is_enabled());
    assert!(node.bootstrap_peer_addresses.is_empty());
    assert_eq!(
        node.swarm.behaviour().pairing_kad.is_enabled(),
        protocol != PUBLIC_IPFS_KADEMLIA_PROTOCOL
    );
    // Replace before the first poll, removing built-in protected public seeds.
    // No production limit is overridden, including the 2-total/1-per-poll jobs.
    let local = node.local_peer_id;
    let make_kad = |protocol| {
        let mut config =
            crate::runtime::p2p::controlled_kademlia_config(libp2p::StreamProtocol::new(protocol));
        if sender {
            config.set_replication_interval(Some(REPLICATION_INTERVAL));
        }
        let mut kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        kad.set_mode(Some(if sender {
            kad::Mode::Client
        } else {
            kad::Mode::Server
        }));
        assert_eq!(kad.query_pool_usage().capacity, Some(32));
        assert_eq!(kad.query_pool_usage().retained, 0);
        kad
    };
    node.swarm.behaviour_mut().kad = make_kad(protocol);
    if let Some(pairing) = node.swarm.behaviour_mut().pairing_kad.as_mut() {
        *pairing = make_kad(PUBLIC_IPFS_KADEMLIA_PROTOCOL);
    }
    node
}

fn dht(node: &P2pNode, role: usize) -> &TestKad {
    if role == 0 {
        &node.swarm.behaviour().kad
    } else {
        node.swarm.behaviour().pairing_kad.as_ref().unwrap()
    }
}

fn dht_mut(node: &mut P2pNode, role: usize) -> &mut TestKad {
    if role == 0 {
        &mut node.swarm.behaviour_mut().kad
    } else {
        node.swarm.behaviour_mut().pairing_kad.as_mut().unwrap()
    }
}

fn forwarder(local: &NodeIdentity, remote: &NodeIdentity, side: usize) -> Forwarder {
    let addresses = ["10.88.0.1", "10.88.0.2"];
    let config: Config = serde_json::from_value(serde_json::json!({
        "network": {"name": "kad-contention", "private_key": local.private_key, "vpn_ip": addresses[side]},
        "peers": [{"id": remote.peer_id, "vpn_ip": addresses[1 - side]}]
    }))
    .unwrap();
    Forwarder::from_config(&config).unwrap()
}

struct Contention {
    nodes: [P2pNode; 2],
    forwarders: [Forwarder; 2],
    connections: [Option<ConnectionId>; 2],
    addresses: [Option<Multiaddr>; 2],
    seed: Libp2pPeerId,
    blackhole: TcpListener,
    held_sockets: Vec<TcpStream>,
    held_queries: Vec<HashSet<kad::QueryId>>,
    maintenance: KademliaMaintenance,
    auto_relay: AutoRelayState,
    metrics: RuntimeMetrics,
    record_keys: Vec<kad::RecordKey>,
    background_seen: HashSet<usize>,
    delivered: [usize; 2],
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Saturated,
    AutoNat,
    Background,
    Clean,
}

impl Contention {
    fn check_bounds(&mut self, phase: Phase) {
        for role in 0..self.held_queries.len() {
            let kad = dht(&self.nodes[0], role);
            let usage = kad.query_pool_usage();
            assert_eq!(usage.capacity, Some(32));
            assert!(usage.retained <= 32, "{phase:?}, role={role}: {usage:?}");
            for query in &self.held_queries[role] {
                assert!(
                    kad.query_is_retained(query),
                    "held query retired during {phase:?}, role={role}"
                );
            }
            match phase {
                Phase::Saturated | Phase::AutoNat => assert_eq!(usage.retained, 32),
                Phase::Background => assert!((1..=2).contains(&usage.retained)),
                Phase::Clean => assert_eq!(usage.retained, 0),
            }
            let queue = kad.behaviour_queue_usage();
            assert!(queue.events <= 512 && queue.bytes <= 4 * 1024 * 1024);
            assert_eq!(
                usage.rejected, 1,
                "only the explicit full-pool attempt may reject"
            );
            for query in kad.iter_queries() {
                if matches!(query.info(), kad::QueryInfo::PutRecord { record, context: kad::PutRecordContext::Replicate, .. } if record.key == self.record_keys[role])
                {
                    assert!(matches!(phase, Phase::Background));
                    self.background_seen.insert(role);
                }
            }
            assert!(dht(&self.nodes[1], role).query_pool_usage().retained <= 32);
        }
        if matches!(phase, Phase::AutoNat) {
            assert_eq!(self.maintenance.pending_queries(), 1);
            let query = self.maintenance.queries.iter().next().unwrap();
            assert!(self.nodes[0].swarm.behaviour().kad.query_is_retained(query));
        } else {
            assert_eq!(self.maintenance.pending_queries(), 0);
            assert!(self.maintenance.started_at.is_none());
        }
        for side in 0..2 {
            assert!(
                self.nodes[side]
                    .swarm
                    .is_connected(&self.nodes[1 - side].local_peer_id)
            );
            assert!(
                self.nodes[side]
                    .swarm
                    .behaviour()
                    .pinned_packet_stream
                    .pending_outbound_count()
                    <= 4
            );
        }
        assert!(self.held_sockets.len() <= 16);
    }

    // Both real swarms stay polled even while the seed's TCP handshake stalls.
    async fn step(&mut self) -> Option<(usize, SwarmEvent<BehaviourEvent>)> {
        let [left, right] = &mut self.nodes;
        tokio::select! {
            event = left.swarm.select_next_some() => Some((0, event)),
            event = right.swarm.select_next_some() => Some((1, event)),
            accepted = self.blackhole.accept() => {
                let (socket, remote) = accepted.unwrap();
                assert!(remote.ip().is_loopback());
                assert!(self.held_sockets.len() < 16, "unexpected seed dial storm");
                self.held_sockets.push(socket);
                None
            }
            () = tokio::time::sleep(Duration::from_millis(10)) => None,
        }
    }

    fn observe_connection(&mut self, side: usize, event: &SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                assert!(
                    address
                        .iter()
                        .any(|part| matches!(part, Protocol::Ip4(ip) if ip.is_loopback()))
                );
                self.addresses[side] = Some(address.clone());
            }
            SwarmEvent::ConnectionEstablished {
                peer_id,
                connection_id,
                num_established,
                ..
            } => {
                assert_eq!(*peer_id, self.nodes[1 - side].local_peer_id);
                assert_eq!(num_established.get(), 1, "VPN connection replaced");
                assert!(self.connections[side].replace(*connection_id).is_none());
            }
            SwarmEvent::ConnectionClosed { peer_id, .. } => {
                assert_ne!(
                    *peer_id,
                    self.nodes[1 - side].local_peer_id,
                    "Kad contention closed VPN connection"
                );
            }
            SwarmEvent::OutgoingConnectionError { peer_id, .. } => {
                assert_eq!(
                    *peer_id,
                    Some(self.seed),
                    "unexpected loopback dial failure"
                );
            }
            SwarmEvent::Dialing {
                peer_id: Some(peer),
                ..
            } => {
                assert!(
                    *peer == self.seed || *peer == self.nodes[1 - side].local_peer_id,
                    "unexpected infrastructure dial"
                );
            }
            _ => {}
        }
    }

    fn trigger_autonat(&mut self) {
        handle_autonat_event_monotonic_at(
            &mut self.nodes[0].swarm,
            &mut self.auto_relay,
            &mut self.maintenance,
            &DiscoveredPeerAddresses::default(),
            &self.metrics,
            false,
            autonat::Event::StatusChanged {
                old: autonat::NatStatus::Unknown,
                new: autonat::NatStatus::Private,
            },
            Instant::now(),
        );
    }

    fn replicated(&mut self) -> bool {
        (0..self.record_keys.len()).all(|role| {
            dht_mut(&mut self.nodes[1], role)
                .store_mut()
                .get(&self.record_keys[role])
                .is_some_and(|record| record.value.as_slice() == b"contention-background-record")
        })
    }

    async fn packets(&mut self, phase: Phase, dwell: Duration, deadline: Duration, wave: u64) {
        tokio::time::timeout(deadline, self.packets_until_done(phase, dwell, wave))
            .await
            .unwrap_or_else(|_| panic!("{phase:?} packet/producer deadline exceeded"));
    }

    async fn packets_until_done(&mut self, phase: Phase, dwell: Duration, wave: u64) {
        let start = Instant::now();
        let mut next_packet = start;
        let mut sent = [0; 2];
        let mut received = [HashSet::new(), HashSet::new()];
        let mut pending: [HashMap<pinned_packet_stream::RequestId, Frame>; 2] =
            [HashMap::new(), HashMap::new()];
        loop {
            self.check_bounds(phase);
            if Instant::now() >= next_packet {
                for side in 0..2 {
                    if sent[side] == PACKETS_PER_DIRECTION || pending[side].len() >= 4 {
                        continue;
                    }
                    let sequence = wave * 16 + u64::try_from(sent[side]).unwrap();
                    let remote = self.nodes[1 - side].local_peer_id;
                    let packet = crate::queue::Packet::new(
                        PeerId::from_libp2p(remote),
                        sequence,
                        ip_packet(side, sequence),
                    );
                    let frame = self.forwarders[side]
                        .queued_packet_frame_with_mtu(&packet, 1280)
                        .unwrap();
                    let request = self.nodes[side]
                        .swarm
                        .behaviour_mut()
                        .pinned_packet_stream
                        .send_request_on_connection(
                            remote,
                            self.connections[side].unwrap(),
                            frame.clone(),
                        );
                    assert!(pending[side].insert(request, frame).is_none());
                    sent[side] += 1;
                }
                next_packet = Instant::now() + PACKET_INTERVAL;
            }
            if let Some((side, event)) = self.step().await {
                self.observe_connection(side, &event);
                match event {
                    SwarmEvent::Behaviour(BehaviourEvent::Packet(
                        request_response::Event::Message {
                            peer,
                            message:
                                Message::Request {
                                    request, channel, ..
                                },
                            ..
                        },
                    )) => {
                        assert_eq!(peer, self.nodes[1 - side].local_peer_id);
                        let payload = self.forwarders[side]
                            .accept_inbound_stream_packet(peer, &request)
                            .unwrap();
                        assert_eq!(payload, ip_packet(1 - side, request.header.sequence));
                        assert!(pending[1 - side].values().any(|frame| frame == &request));
                        assert!(received[side].insert(request.header.sequence));
                        self.nodes[side]
                            .swarm
                            .behaviour_mut()
                            .packet
                            .send_response(channel, PacketResponse::Accepted)
                            .unwrap();
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
                        pinned_packet_stream::Event::OutboundResponse {
                            peer,
                            connection_id,
                            request_id,
                            response,
                        },
                    )) => {
                        assert_eq!(peer, self.nodes[1 - side].local_peer_id);
                        assert_eq!(Some(connection_id), self.connections[side]);
                        assert_eq!(response, PacketResponse::Accepted);
                        assert!(pending[side].remove(&request_id).is_some());
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
                        pinned_packet_stream::Event::OutboundFailure { error, .. }
                        | pinned_packet_stream::Event::InboundFailure { error, .. },
                    )) => panic!("VPN stream failed during {phase:?}: {error:?}"),
                    SwarmEvent::Behaviour(BehaviourEvent::PinnedPacketStream(
                        pinned_packet_stream::Event::InboundRequest { .. },
                    )) => {
                        panic!("default request-response packet owner was bypassed");
                    }
                    SwarmEvent::Behaviour(BehaviourEvent::Packet(
                        request_response::Event::InboundFailure { error, .. },
                    )) => {
                        panic!("VPN packet owner failed during {phase:?}: {error:?}");
                    }
                    _ => {}
                }
            }
            self.check_bounds(phase);
            if sent == [16, 16]
                && pending.iter().all(HashMap::is_empty)
                && start.elapsed() >= dwell
                && (!matches!(phase, Phase::Background) || self.replicated())
            {
                assert_eq!(received[0].len(), 16);
                assert_eq!(received[1].len(), 16);
                for side in 0..2 {
                    self.delivered[side] += received[side].len();
                }
                break;
            }
        }
    }
}

fn ip_packet(side: usize, sequence: u64) -> Vec<u8> {
    let mut packet = vec![0_u8; 64];
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(&64_u16.to_be_bytes());
    packet[8] = 64;
    packet[9] = 253;
    let hosts = [[10, 88, 0, 1], [10, 88, 0, 2]];
    packet[12..16].copy_from_slice(&hosts[side]);
    packet[16..20].copy_from_slice(&hosts[1 - side]);
    packet[20..28].copy_from_slice(&sequence.to_be_bytes());
    let sum: u32 = packet[..20]
        .chunks_exact(2)
        .map(|pair| u32::from(u16::from_be_bytes([pair[0], pair[1]])))
        .sum();
    let folded = (sum & 0xffff) + (sum >> 16);
    let checksum = !u16::try_from((folded & 0xffff) + (folded >> 16)).unwrap();
    packet[10..12].copy_from_slice(&checksum.to_be_bytes());
    packet
}

async fn exercise_contention(protocol: &'static str, listen: &str) {
    let identities = [
        NodeIdentity::generate_ed25519().unwrap(),
        NodeIdentity::generate_ed25519().unwrap(),
    ];
    let mut test = Contention {
        nodes: [
            test_node(identities[0].clone(), protocol, listen, true),
            test_node(identities[1].clone(), protocol, listen, false),
        ],
        forwarders: [
            forwarder(&identities[0], &identities[1], 0),
            forwarder(&identities[1], &identities[0], 1),
        ],
        connections: [None, None],
        addresses: [None, None],
        seed: Libp2pPeerId::random(),
        blackhole: TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap(),
        held_sockets: Vec::new(),
        held_queries: Vec::new(),
        maintenance: KademliaMaintenance::new(Instant::now()),
        auto_relay: AutoRelayState::default(),
        metrics: RuntimeMetrics::default(),
        record_keys: Vec::new(),
        background_seen: HashSet::new(),
        delivered: [0, 0],
    };
    tokio::time::timeout(CONNECT_DEADLINE, async {
        while test.addresses.iter().any(Option::is_none) {
            if let Some((side, event)) = test.step().await {
                test.observe_connection(side, &event);
            }
        }
        let address = test.addresses[1]
            .clone()
            .unwrap()
            .with(Protocol::P2p(test.nodes[1].local_peer_id));
        test.nodes[0].swarm.dial(address).unwrap();
        while test.connections.iter().any(Option::is_none) {
            if let Some((side, event)) = test.step().await {
                test.observe_connection(side, &event);
            }
        }
    })
    .await
    .expect("authenticated loopback connection deadline");

    let roles = if protocol == PUBLIC_IPFS_KADEMLIA_PROTOCOL {
        1
    } else {
        2
    };
    let seed_address: Multiaddr = format!(
        "/ip4/127.0.0.1/tcp/{}",
        test.blackhole.local_addr().unwrap().port()
    )
    .parse()
    .unwrap();
    for role in 0..roles {
        let kad = dht_mut(&mut test.nodes[0], role);
        assert_eq!(kad.query_pool_usage().retained, 0);
        kad.add_address(&test.seed, seed_address.clone());
        let queries = (0..QUERY_CAPACITY)
            .map(|_| kad.try_get_closest_peers(Libp2pPeerId::random()).unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(queries.len(), 32);
        assert!(matches!(
            kad.try_get_closest_peers(Libp2pPeerId::random()),
            Err(kad::QueryStartError::Capacity(_))
        ));
        let key = kad::RecordKey::new(&[u8::try_from(role).unwrap(), 0xc1]);
        kad.store_mut()
            .put(kad::Record::new(
                key.clone(),
                b"contention-background-record".to_vec(),
            ))
            .unwrap();
        test.record_keys.push(key);
        test.held_queries.push(queries);
    }
    let maintenance_due = test.maintenance.next_due;
    test.trigger_autonat();
    assert_eq!(test.maintenance.next_due, maintenance_due);
    assert_eq!(test.maintenance.pending_queries(), 0);
    assert_eq!(
        test.metrics
            .snapshot(crate::queue::QueueStats::default())
            .auto_relay_discovery_queries,
        0
    );
    test.packets(Phase::Saturated, SATURATED_DWELL, PACKET_PHASE_DEADLINE, 0)
        .await;
    assert!(
        !test.held_sockets.is_empty(),
        "queries were never driven into the live seed transport"
    );
    assert!(
        test.background_seen.is_empty(),
        "background jobs ignored aggregate limit"
    );
    for role in 0..roles {
        assert!(dht(&test.nodes[0], role).dial_queue_usage().dispatched > 0);
    }

    let released = *test.held_queries[0].iter().next().unwrap();
    assert!(
        test.nodes[0]
            .swarm
            .behaviour_mut()
            .kad
            .cancel_query(&released)
    );
    test.held_queries[0].remove(&released);
    assert_eq!(
        test.nodes[0]
            .swarm
            .behaviour()
            .kad
            .query_pool_usage()
            .retained,
        31
    );
    test.trigger_autonat();
    assert_eq!(test.maintenance.pending_queries(), 1);
    assert_eq!(
        test.metrics
            .snapshot(crate::queue::QueueStats::default())
            .auto_relay_discovery_queries,
        1
    );
    assert_eq!(
        test.maintenance.next_due,
        test.maintenance.started_at.unwrap() + Duration::from_secs(120)
    );
    test.packets(Phase::AutoNat, AUTONAT_DWELL, PACKET_PHASE_DEADLINE, 1)
        .await;
    assert_eq!(test.held_queries[0].len(), 31);
    assert!(test.background_seen.is_empty());

    assert_eq!(
        test.maintenance
            .cancel_queries(&mut test.nodes[0].swarm.behaviour_mut().kad),
        1
    );
    for role in 0..roles {
        let keep = *test.held_queries[role].iter().next().unwrap();
        let release = test.held_queries[role]
            .iter()
            .copied()
            .filter(|id| *id != keep)
            .collect::<Vec<_>>();
        let remote = test.nodes[1].local_peer_id;
        let remote_address = test.addresses[1].clone().unwrap();
        let kad = dht_mut(&mut test.nodes[0], role);
        for id in release {
            assert!(kad.cancel_query(&id));
            test.held_queries[role].remove(&id);
        }
        assert_eq!(kad.query_pool_usage().retained, 1);
        assert!(kad.query_is_retained(&keep));
        // Only new work uses the useful routing peer. The old real query still
        // owns its stalled transport and must survive while replication runs.
        kad.remove_peer(&test.seed);
        kad.add_address(&remote, remote_address);
    }
    test.packets(Phase::Background, BACKGROUND_DWELL, BACKGROUND_DEADLINE, 2)
        .await;
    assert_eq!(test.background_seen.len(), roles);
    assert!(
        test.replicated(),
        "released capacity did not produce remote records"
    );

    for role in 0..roles {
        let kad = dht_mut(&mut test.nodes[0], role);
        kad.store_mut().remove(&test.record_keys[role]);
        let queries = kad
            .iter_queries()
            .map(|query| query.id())
            .collect::<Vec<_>>();
        for query in queries {
            assert!(kad.cancel_query(&query));
        }
        test.held_queries[role].clear();
        assert_eq!(kad.query_pool_usage().retained, 0);
    }
    test.held_sockets.clear();
    test.packets(Phase::Clean, Duration::ZERO, CLEANUP_DEADLINE, 3)
        .await;
    assert_eq!(test.delivered, [64, 64]);
    for node in &test.nodes {
        assert_eq!(
            node.swarm
                .behaviour()
                .pinned_packet_stream
                .pending_outbound_count(),
            0
        );
        for role in 0..roles {
            assert_eq!(dht(node, role).query_pool_usage().retained, 0);
            assert_eq!(dht(node, role).pending_rpc_usage().requests, 0);
        }
    }
    eprintln!(
        "kad_live_contention protocol={protocol} transport={listen} packets_each_way=64 roles={roles} retained=0 maintenance=0 accelerated_replication_ms=100"
    );
}
