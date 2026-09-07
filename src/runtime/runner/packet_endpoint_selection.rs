use std::borrow::Cow;

use super::*;

/// Prefer a mutually advertised endpoint pair on an authenticated, healthy LAN
/// connection. Public address classification alone does not prove reachability.
pub(super) fn packet_capabilities_for_lan_connection<'a>(
    local: &'a ControlCapabilities,
    remote: &'a ControlCapabilities,
    peer: Libp2pPeerId,
    paths: &PathSet,
    connections: &HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
    networks: &[LocalInterfaceNetwork],
) -> (Cow<'a, ControlCapabilities>, Cow<'a, ControlCapabilities>) {
    let lan_pair = paths
        .candidates_for(PeerId::from_libp2p(peer))
        .find_map(|path| {
            if !path.healthy || !path.is_direct() || path.kind.requires_quic_datagrams() {
                return None;
            }
            let endpoint = connections.get(&(peer, path.latest_connection_id?))?;
            if endpoint.is_relayed() {
                return None;
            }
            let remote_ip = direct_endpoint_remote_ip(endpoint)?;
            networks
                .iter()
                .find(|network| network.contains(remote_ip))
                .map(|network| (network.ip, remote_ip))
        });
    let mut local = Cow::Borrowed(local);
    let mut remote = Cow::Borrowed(remote);
    let Some((local_ip, remote_ip)) = lan_pair else {
        return (local, remote);
    };

    for quic in [false, true] {
        let candidates = |capabilities: &ControlCapabilities, ip| {
            let candidates = if quic {
                &capabilities.owned_quic_packet_endpoint_candidates
            } else {
                &capabilities.packet_endpoint_candidates
            };
            candidates
                .iter()
                .filter(|candidate| {
                    candidate
                        .parse::<SocketAddr>()
                        .is_ok_and(|endpoint| endpoint.ip() == ip)
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let local_candidates = candidates(&local, local_ip);
        let remote_candidates = candidates(&remote, remote_ip);
        // Never synthesize an endpoint or change one side without reciprocal evidence.
        if local_candidates.is_empty() || remote_candidates.is_empty() {
            continue;
        }
        if quic {
            local.to_mut().owned_quic_packet_endpoint_candidates = local_candidates;
            remote.to_mut().owned_quic_packet_endpoint_candidates = remote_candidates;
        } else {
            local.to_mut().packet_endpoint_candidates = local_candidates;
            remote.to_mut().packet_endpoint_candidates = remote_candidates;
        }
    }
    (local, remote)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capabilities(host: u8) -> ControlCapabilities {
        let mut capabilities = ControlCapabilities::local("lan-selection", None, 1280)
            .with_packet_endpoint_candidates(vec![
                format!("11.251.0.{host}:4002"),
                format!("10.253.0.{host}:4002"),
            ]);
        capabilities.owned_quic_packet_endpoint_candidates = vec![
            format!("11.251.0.{host}:4003"),
            format!("10.253.0.{host}:4003"),
        ];
        capabilities
    }

    fn fixture() -> (
        Libp2pPeerId,
        PathSet,
        HashMap<(Libp2pPeerId, ConnectionId), ConnectedPoint>,
        Vec<LocalInterfaceNetwork>,
    ) {
        let peer = Libp2pPeerId::random();
        let connection = ConnectionId::new_unchecked(7);
        let mut paths = PathSet::new();
        paths.record_established_with_details(
            PeerId::from_libp2p(peer),
            PathKind::DirectTcpStream,
            None,
            Some(1280),
            PathOrigin::Mdns,
            PathConnectionRole::Dialer,
            false,
            Some(connection),
            Some(1),
        );
        let endpoint = ConnectedPoint::Dialer {
            address: "/ip4/10.253.0.2/tcp/4001".parse().unwrap(),
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        (
            peer,
            paths,
            HashMap::from([((peer, connection), endpoint)]),
            vec![LocalInterfaceNetwork {
                ip: "10.253.0.1".parse().unwrap(),
                netmask: "255.255.255.0".parse().unwrap(),
            }],
        )
    }

    #[test]
    fn authenticated_lan_pair_wins_without_changing_advertisements_or_signatures() {
        let (peer, paths, connections, networks) = fixture();
        let local = capabilities(1);
        let remote = capabilities(2);
        assert_eq!(
            first_packet_plane_endpoint(&local).unwrap().ip(),
            "11.251.0.1".parse::<IpAddr>().unwrap()
        );
        let (preferred_local, preferred_remote) = packet_capabilities_for_lan_connection(
            &local,
            &remote,
            peer,
            &paths,
            &connections,
            &networks,
        );
        for backend in [
            PacketDatagramBackend::OwnedUdp,
            PacketDatagramBackend::OwnedQuic,
        ] {
            let selected =
                first_packet_plane_endpoint_for_backend(&preferred_local, backend).unwrap();
            assert_eq!(selected.ip(), "10.253.0.1".parse::<IpAddr>().unwrap());
            assert!(endpoint_is_advertised_for_backend(
                &local, selected, backend
            ));
            let selected =
                first_packet_plane_endpoint_for_backend(&preferred_remote, backend).unwrap();
            assert_eq!(selected.ip(), "10.253.0.2".parse::<IpAddr>().unwrap());
            assert!(endpoint_is_advertised_for_backend(
                &remote, selected, backend
            ));
        }
        assert_eq!(local.packet_endpoint_candidates.len(), 2);
        assert_eq!(remote.packet_endpoint_candidates.len(), 2);
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let (_, encoded, verified) = signed_packet_plane_handshake(
            PacketPlaneHandshakeKind::Hello,
            &identity,
            &preferred_local,
            PacketDatagramBackend::OwnedUdp,
        )
        .unwrap();
        assert_eq!(verified.endpoint, "10.253.0.1:4002".parse().unwrap());
        assert!(
            encoded
                .verify(
                    "lan-selection",
                    Some(PeerId::from_libp2p(identity.peer_id.parse().unwrap()))
                )
                .is_ok()
        );
    }

    #[test]
    fn unrelated_stale_relay_or_off_lan_connection_cannot_choose_endpoints() {
        let (peer, mut paths, mut connections, networks) = fixture();
        let local = capabilities(1);
        let remote = capabilities(2);
        let unchanged = |peer,
                         paths: &PathSet,
                         connections: &HashMap<_, _>,
                         networks: &[LocalInterfaceNetwork]| {
            let (a, b) = packet_capabilities_for_lan_connection(
                &local,
                &remote,
                peer,
                paths,
                connections,
                networks,
            );
            assert!(matches!(a, Cow::Borrowed(_)) && matches!(b, Cow::Borrowed(_)));
        };
        unchanged(Libp2pPeerId::random(), &paths, &connections, &networks);
        unchanged(peer, &paths, &HashMap::new(), &networks);
        unchanged(peer, &paths, &connections, &[]);
        paths.mark_unhealthy(PeerId::from_libp2p(peer), PathKind::DirectTcpStream);
        unchanged(peer, &paths, &connections, &networks);
        let (peer, paths, _, networks) = fixture();
        connections.clear();
        connections.insert(
            (peer, ConnectionId::new_unchecked(7)),
            ConnectedPoint::Dialer {
                address: "/ip4/10.253.0.2/tcp/4001/p2p-circuit".parse().unwrap(),
                role_override: libp2p::core::Endpoint::Dialer,
                port_use: libp2p::core::transport::PortUse::Reuse,
            },
        );
        unchanged(peer, &paths, &connections, &networks);
    }

    #[test]
    fn one_sided_advertisement_preserves_existing_endpoint_selection() {
        let (peer, paths, connections, networks) = fixture();
        let local = capabilities(1);
        let mut remote = capabilities(2);
        remote.packet_endpoint_candidates.truncate(1);
        remote.owned_quic_packet_endpoint_candidates.truncate(1);
        let (a, b) = packet_capabilities_for_lan_connection(
            &local,
            &remote,
            peer,
            &paths,
            &connections,
            &networks,
        );
        assert!(matches!(a, Cow::Borrowed(_)) && matches!(b, Cow::Borrowed(_)));
        assert_eq!(
            first_packet_plane_endpoint(&a),
            first_packet_plane_endpoint(&local)
        );
    }
}
