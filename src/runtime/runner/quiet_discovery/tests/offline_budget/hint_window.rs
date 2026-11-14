use super::*;

impl OfflineFixture {
    fn signed_member(&mut self) -> NodeIdentity {
        assert_eq!(self.members.len(), 1);
        let (identity, forwarder) =
            std::iter::repeat_with(|| NodeIdentity::generate_ed25519().unwrap())
                .take(1024)
                .find_map(|identity| {
                    let peer = identity.peer_id.parse().unwrap();
                    Forwarder::from_config(&config_with_peer(&self.f.node.identity, peer))
                        .ok()
                        .map(|forwarder| (identity, forwarder))
                })
                .expect("noncolliding signed member");
        self.f.peer = identity.peer_id.parse().unwrap();
        self.members = vec![self.f.peer];
        self.f.forwarder = forwarder;
        self.f
            .discovered
            .recovery_queries
            .retain_peers(|peer| peer == self.f.peer);
        self.reconcile();
        identity
    }
}

fn signed_hints(identity: &NodeIdentity, addresses: Vec<Multiaddr>) -> kad::QueryResult {
    let peer = identity.peer_id.parse().unwrap();
    let value = encode_kademlia_peer_address_record(
        "lab",
        None,
        identity,
        addresses,
        current_unix_seconds_lossy(),
    )
    .unwrap();
    kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(kad::PeerRecord {
        peer: None,
        record: kad::Record::new(
            crate::runtime::p2p::kademlia_peer_addresses_key("lab", None, peer),
            value,
        ),
    })))
}

fn pending(t: &OfflineFixture) -> u32 {
    t.f.node
        .swarm
        .network_info()
        .connection_counters()
        .num_pending_outgoing()
}

#[tokio::test]
async fn signed_wan_hint_survives_lan_grace_and_is_used_without_another_lookup() {
    let mut t = OfflineFixture::new_at(
        1,
        Instant::now() - PEER_RECOVERY_LAN_FIRST_GRACE - Duration::from_secs(1),
    )
    .await;
    let identity = t.signed_member();
    let peer = t.f.peer;
    let query = t.spend(peer);
    assert!(t.f.discovered.start_lan_first_recovery(peer, t.f.now));
    let address: Multiaddr = "/ip4/11.252.0.2/tcp/4001".parse().unwrap();
    t.deliver(query, signed_hints(&identity, vec![address.clone()]));
    assert!(t.f.discovered.as_vec().contains(&(peer, address)));
    assert_eq!(
        pending(&t),
        0,
        "LAN grace defers WAN dial effects, not hint retention"
    );
    assert!(t.f.discovered.recovery_dial_attempts.is_empty());
    t.f.now += PEER_RECOVERY_LAN_FIRST_GRACE;
    t.redial(false);
    assert_eq!(pending(&t), 1, "cached hint is admitted after grace");
    assert!(!t.f.discovered.recovery_queries.has_pending());
    assert_eq!(
        t.f.discovered
            .recovery_queries
            .state(&peer)
            .unwrap()
            .attempt_count,
        1
    );
    // Do not poll the admitted WAN transport; this observes admission only.
}

#[tokio::test]
async fn expired_window_blocks_cached_redial_against_the_real_clock() {
    let mut t = OfflineFixture::new_at(1, Instant::now() - Duration::from_secs(601)).await;
    let identity = t.signed_member();
    let peer = t.f.peer;
    let query = t.spend(peer);
    t.expire_window();
    assert!(t.dial_suppressed(peer));
    assert!(!t.suppressed(peer), "native query still owns its drain");
    let address: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().unwrap();
    t.deliver(query, signed_hints(&identity, vec![address.clone()]));
    assert!(t.f.discovered.as_vec().contains(&(peer, address)));
    assert!(t.suppressed(peer));
    assert_eq!(pending(&t), 0, "late owned result only caches");
    t.redial(false);
    assert_eq!(
        pending(&t),
        0,
        "real-clock redial cannot reopen the expired window"
    );
    assert!(t.f.discovered.recovery_dial_attempts.is_empty());
    assert!(!t.f.discovered.recovery_queries.has_pending());
    assert_eq!(
        t.f.discovered
            .recovery_queries
            .state(&peer)
            .unwrap()
            .attempt_count,
        1
    );
}

#[tokio::test]
async fn stale_pending_dial_does_not_strand_a_later_signed_hint() {
    let mut t = OfflineFixture::new_at(1, Instant::now() - Duration::from_secs(31)).await;
    let identity = t.signed_member();
    let peer = t.f.peer;
    let query = t.spend(peer);
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stale: Multiaddr = format!("/ip4/127.0.0.1/tcp/{}", closed.local_addr().unwrap().port())
        .parse()
        .unwrap();
    drop(closed);
    let replacement = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address: Multiaddr = format!(
        "/ip4/127.0.0.1/tcp/{}",
        replacement.local_addr().unwrap().port()
    )
    .parse()
    .unwrap();
    let options = known_peer_dial_options(peer, [stale], PeerCondition::NotDialing);
    let stale_id = options.connection_id();
    t.epochs.dial(&mut t.f.node.swarm, options).unwrap();
    t.deliver(query, signed_hints(&identity, vec![address.clone()]));
    assert_eq!(
        pending(&t),
        1,
        "replacement is refused while stale dial is pending"
    );
    assert!(t.f.discovered.as_vec().contains(&(peer, address)));
    assert!(!t.f.discovered.recovery_queries.has_pending());
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            tokio::select! {
                _ = t._server.swarm.select_next_some() => {},
                event = t.f.node.swarm.select_next_some() => {
                    if let SwarmEvent::OutgoingConnectionError { connection_id, .. } = event
                        && connection_id == stale_id {
                        t.epochs.remove(stale_id);
                        break;
                    }
                },
            }
        }
    })
    .await
    .expect("loopback stale dial fails");
    assert_eq!(pending(&t), 0);
    t.redial(false);
    assert_eq!(
        pending(&t),
        1,
        "existing grouped redial admits the retained replacement"
    );
    assert!(!t.f.discovered.recovery_queries.has_pending());
    assert_eq!(
        t.f.discovered
            .recovery_queries
            .state(&peer)
            .unwrap()
            .attempt_count,
        1
    );
    assert!(!t.dial_suppressed(peer));
}
