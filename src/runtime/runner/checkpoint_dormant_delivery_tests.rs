//! Root-runner checkpoint delivery over a retained, initially disconnected TCP edge.

use super::*;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

use crate::{
    membership::checkpoint::{CheckpointMember, CooperativeMembershipState, SnapshotPolicy},
    runtime::{
        membership_store::checkpoint::CheckpointCredentials,
        tun::{PacketRead, PacketWrite},
    },
};

const NETWORK: &str = "checkpoint-dormant-delivery";

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove checkpoint delivery fixtures");
    }
}

fn private_directory(path: &std::path::Path) {
    #[cfg(unix)]
    {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    #[cfg(windows)]
    crate::private_fs::ensure_private_directory(path).unwrap();
}

struct EmptyPacketDevice;

impl PacketRead for EmptyPacketDevice {
    fn read_packet(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}

impl PacketWrite for EmptyPacketDevice {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
        Ok(packet.len())
    }
}

#[tokio::test]
async fn root_checkpoint_departure_reconnects_retained_tcp_peer_without_packet_authority() {
    let directory = TestDirectory(std::env::temp_dir().join(format!(
        "p2p-vpn-root-checkpoint-delivery-{}",
        std::process::id(),
    )));
    private_directory(&directory.0);
    let mut allocated = HashSet::new();
    let identities = (0..3)
        .map(|_| {
            (0..1_024)
                .find_map(|_| {
                    let identity = NodeIdentity::generate_ed25519().unwrap();
                    let peer = PeerId::from_libp2p(identity.peer_id.parse().unwrap());
                    allocated
                        .insert(crate::route::builtin_ipv4(peer))
                        .then_some(identity)
                })
                .expect("three distinct derived VPN addresses")
        })
        .collect::<Vec<_>>();
    let credentials = CheckpointCredentials::generate().unwrap();
    let anchor = credentials.anchor().clone();
    let initial = CooperativeMembershipState::bootstrap_at(
        credentials.capability().unwrap(),
        identities[0].peer_id.clone(),
        identities
            .iter()
            .map(|identity| CheckpointMember::new(identity).unwrap())
            .collect(),
        SnapshotPolicy::default(),
        current_unix_seconds_lossy(),
    )
    .unwrap();
    let listeners = (0..3)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
        .collect::<Vec<_>>();
    let addresses = listeners
        .iter()
        .map(|listener| {
            format!(
                "/ip4/127.0.0.1/tcp/{}",
                listener.local_addr().unwrap().port()
            )
            .parse::<Multiaddr>()
            .unwrap()
        })
        .collect::<Vec<_>>();
    drop(listeners);

    let mut setups = Vec::new();
    let mut paths = Vec::new();
    for (index, identity) in identities.iter().enumerate() {
        let node_directory = directory.0.join(format!("node-{index}"));
        private_directory(&node_directory);
        let path = node_directory.join("membership-state.json");
        MembershipStateStore::new(&path)
            .save_checkpoint(
                NETWORK,
                &identity.peer_id,
                &credentials,
                &initial.retained(),
            )
            .unwrap();
        let peers = identities
            .iter()
            .enumerate()
            .filter(|(peer_index, _)| *peer_index != index)
            .map(|(peer_index, peer)| {
                let addresses = if peer_index == index + 1 {
                    vec![addresses[peer_index].to_string()]
                } else {
                    Vec::new()
                };
                serde_json::json!({"id": peer.peer_id, "addresses": addresses})
            })
            .collect::<Vec<_>>();
        let mut config: Config = serde_json::from_value(serde_json::json!({
            "network": {
                "name": NETWORK,
                "local_peer": identity.peer_id,
                "private_key": identity.private_key,
                "listen_addresses": [addresses[index].to_string()],
            },
            "peers": peers,
        }))
        .unwrap();
        config.network.discovery = DiscoveryConfig {
            mdns: false,
            kademlia: false,
            autonat: false,
            dcutr: false,
            kademlia_provider_advertisement: false,
            ..DiscoveryConfig::default()
        };
        config.network.relay.auto.max_reservations = 0;
        config.network.packet_plane.listen.clear();
        config.network.packet_plane.quic_listen.clear();
        let mut epochs = ConnectionEpochs::default();
        let mut node = super::super::p2p::build_node_with_dial_observer(
            &HostConfig {
                identity: identity.clone(),
                network_name: NETWORK.to_owned(),
                membership_tag: config.membership_tag().unwrap(),
                mtu: config.effective_packet_mtu(),
                max_concurrent_control_streams: config.resources.control_stream_limit(),
                max_concurrent_packet_streams: config.resources.packet_stream_limit(),
                listen_addresses: config.listen_multiaddrs().unwrap(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                known_peers: config.peer_multiaddrs().unwrap(),
                relay_reservations: Vec::new(),
                relay_server: false,
                relay_resources: config.network.relay.resources,
                resources: config.resources,
                discovery: config.network.discovery.clone(),
            },
            |connection| epochs.record_started(connection),
        )
        .unwrap();
        if index == 0 {
            let third = identities[2].peer_id.parse().unwrap();
            assert!(
                node.configured_peer_addresses
                    .iter()
                    .all(|(peer, _)| *peer != third)
            );
            // Shared native reachability only: no startup dial or runner recovery target for A-C.
            node.swarm.add_peer_address(third, addresses[2].clone());
            assert!(!node.swarm.is_connected(&third));
        }
        let forwarder = Forwarder::from_config(&config).unwrap();
        let membership = OverlayMembership::from_config(&config).unwrap();
        setups.push((node, epochs, forwarder, membership, config, path.clone()));
        paths.push(path);
    }

    let mut controls = Vec::new();
    let mut daemons = Vec::new();
    let mut shutdowns = Vec::new();
    for (node, epochs, forwarder, membership, config, path) in setups {
        let (control, receiver) = runtime_control_channel();
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        controls.push(control);
        shutdowns.push(shutdown);
        daemons.push(tokio::spawn(run_node_until_with_membership_state(
            node,
            epochs,
            forwarder,
            membership,
            Vec::new(),
            PacketIo::new(EmptyPacketDevice, EmptyPacketDevice),
            Box::new(PreconfiguredTunRoutes),
            None,
            config.effective_packet_mtu(),
            config.queue,
            config.resources,
            None,
            None,
            Some(receiver),
            None,
            Some(path),
            PacketPlaneRuntime::disabled(),
            None,
            Vec::new(),
            config.network.packet_plane.session_ttl(),
            config.network.packet_plane.replay_window_limit(),
            config.network.relay.auto,
            false,
            None,
            async {
                let _ = stopped.await;
                ShutdownReason::ControlSocket
            },
        )));
    }

    let mut scenario = tokio::spawn(async move {
        let started = Instant::now();
        loop {
            let mut ready = true;
            let mut diagnostics = Vec::new();
            for (control, count) in controls.iter().zip([1, 2, 1]) {
                let state = control.state().await.unwrap();
                ready &= state.contains(&"checkpoint_sync_state participating".to_owned());
                ready &= state.contains(&format!("connected_overlay_peers {count}"));
                diagnostics.push(state);
            }
            if ready {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "root checkpoint chain did not become ready: {diagnostics:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let departure = controls[0].revoke_member(None).await.unwrap();
        assert!(departure.resigned);
        assert_eq!(departure.membership_epoch, 1);
        assert!(controls[0].network_peers().await.unwrap().peers.is_empty());
        wait_for_handoff(&controls[0], 2).await;
        let excluded = controls[0].state().await.unwrap();
        assert!(excluded.contains(&"connected_overlay_peers 0".to_owned()));
        assert!(excluded.contains(&"checkpoint_sync_state excluded".to_owned()));
        assert!(controls[0].network_peers().await.unwrap().peers.is_empty());
        for index in [1, 2] {
            let peers = controls[index].network_peers().await.unwrap().peers;
            assert_eq!(peers.len(), 2);
            assert!(
                peers
                    .iter()
                    .all(|peer| peer.peer_id != identities[0].peer_id)
            );
            let Some(PersistedAuthority::Checkpoint(loaded)) =
                MembershipStateStore::new(&paths[index])
                    .load_authority(NETWORK, &identities[index].peer_id, Some(&anchor), None)
                    .unwrap()
            else {
                panic!("survivor lost checkpoint authority");
            };
            assert!(
                loaded
                    .retained
                    .snapshot
                    .payload
                    .member(&identities[0].peer_id)
                    .is_none()
            );
            assert_eq!(loaded.retained.snapshot.payload.authority_revision, 1);
        }

        // Complete any live catch-up started by the first mutation's advertisements.
        let ready = Instant::now();
        loop {
            if controls[1]
                .state()
                .await
                .unwrap()
                .contains(&"checkpoint_sync_state participating".to_owned())
            {
                break;
            }
            assert!(ready.elapsed() < Duration::from_secs(20));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let removal = controls[1]
            .revoke_member(Some(identities[2].peer_id.clone()))
            .await
            .unwrap();
        assert!(!removal.resigned);
        assert_eq!(removal.membership_epoch, 2);
        wait_for_handoff(&controls[1], 1).await;
        let peers = controls[1].network_peers().await.unwrap().peers;
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].peer_id, identities[1].peer_id);
        assert!(controls[2].network_peers().await.unwrap().peers.is_empty());
        let Some(PersistedAuthority::Checkpoint(loaded)) = MembershipStateStore::new(&paths[1])
            .load_authority(NETWORK, &identities[1].peer_id, Some(&anchor), None)
            .unwrap()
        else {
            panic!("surviving governor lost checkpoint authority");
        };
        assert_eq!(loaded.retained.snapshot.payload.members.len(), 1);
        assert_eq!(loaded.retained.snapshot.payload.authority_revision, 2);
    });

    let result = match tokio::time::timeout(Duration::from_secs(75), &mut scenario).await {
        Ok(result) => Ok(result),
        Err(error) => {
            scenario.abort();
            let _ = scenario.await;
            Err(error)
        }
    };
    for shutdown in shutdowns {
        let _ = shutdown.send(());
    }
    let mut outcomes = Vec::new();
    for mut daemon in daemons {
        match tokio::time::timeout(Duration::from_secs(5), &mut daemon).await {
            Ok(outcome) => outcomes.push(Some(outcome)),
            Err(_) => {
                daemon.abort();
                let _ = daemon.await;
                outcomes.push(None);
            }
        }
    }
    for outcome in outcomes {
        outcome
            .expect("checkpoint runner did not shut down")
            .unwrap()
            .unwrap();
    }
    result.expect("root checkpoint delivery timed out").unwrap();
}

async fn wait_for_handoff(
    control: &crate::runtime::control_socket::RuntimeControlHandle,
    count: usize,
) {
    tokio::time::timeout(Duration::from_secs(16), async {
        loop {
            let state = control.state().await.unwrap();
            if state.contains(&"checkpoint_handoff_active 0".to_owned())
                && state.contains(&"checkpoint_handoff_pending_requests 0".to_owned())
            {
                assert!(
                    state.contains(&format!("checkpoint_handoff_recipients {count}")),
                    "{state:?}"
                );
                assert!(
                    state.contains(&format!("checkpoint_handoff_acknowledged {count}")),
                    "{state:?}"
                );
                assert!(
                    state.contains(&"checkpoint_handoff_failed 0".to_owned()),
                    "{state:?}"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("bounded root mutation handoff did not acknowledge every roster recipient");
}
