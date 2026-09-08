use std::{
    collections::BTreeMap,
    convert::Infallible,
    task::{Context, Poll},
};

use libp2p::{
    Multiaddr, PeerId,
    core::{Endpoint, transport::PortUse, upgrade::DeniedUpgrade},
    swarm::{
        ConnectionDenied, ConnectionHandler, ConnectionHandlerEvent, ConnectionId, FromSwarm,
        NetworkBehaviour, NotifyHandler, SubstreamProtocol, ToSwarm, handler::ConnectionEvent,
    },
};

#[derive(Default)]
pub struct Behaviour {
    connections: BTreeMap<ConnectionId, Retention>,
}

struct Retention {
    peer: PeerId,
    desired: bool,
    notified: bool,
}

impl Behaviour {
    /// Coalesce policy changes in the existing connection owner, never a queue.
    pub(crate) fn retain_connections(
        &mut self,
        mut retain: impl FnMut(PeerId, ConnectionId) -> bool,
    ) {
        for (id, state) in &mut self.connections {
            state.desired = retain(state.peer, *id);
        }
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = Infallible;

    fn handle_established_inbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: &Multiaddr,
    ) -> Result<Handler, ConnectionDenied> {
        Ok(Handler::default())
    }

    fn handle_established_outbound_connection(
        &mut self,
        _: ConnectionId,
        _: PeerId,
        _: &Multiaddr,
        _: Endpoint,
        _: PortUse,
    ) -> Result<Handler, ConnectionDenied> {
        Ok(Handler::default())
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        match event {
            FromSwarm::ConnectionEstablished(event) => {
                self.connections.insert(
                    event.connection_id,
                    Retention {
                        peer: event.peer_id,
                        desired: false,
                        notified: false,
                    },
                );
            }
            FromSwarm::ConnectionClosed(event) => {
                if self
                    .connections
                    .get(&event.connection_id)
                    .is_some_and(|state| state.peer == event.peer_id)
                {
                    self.connections.remove(&event.connection_id);
                }
            }
            _ => {}
        }
    }

    fn on_connection_handler_event(&mut self, _: PeerId, _: ConnectionId, event: Infallible) {
        match event {}
    }

    fn poll(&mut self, _: &mut Context<'_>) -> Poll<ToSwarm<Infallible, bool>> {
        for (id, state) in &mut self.connections {
            if state.desired != state.notified {
                state.notified = state.desired;
                return Poll::Ready(ToSwarm::NotifyHandler {
                    peer_id: state.peer,
                    handler: NotifyHandler::One(*id),
                    event: state.desired,
                });
            }
        }
        Poll::Pending
    }
}

#[derive(Default)]
pub struct Handler {
    retained: bool,
}

impl ConnectionHandler for Handler {
    type FromBehaviour = bool;
    type ToBehaviour = Infallible;
    type InboundProtocol = DeniedUpgrade;
    type OutboundProtocol = DeniedUpgrade;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<DeniedUpgrade> {
        SubstreamProtocol::new(DeniedUpgrade, ())
    }

    fn connection_keep_alive(&self) -> bool {
        self.retained
    }

    fn on_behaviour_event(&mut self, retained: bool) {
        self.retained = retained;
    }

    fn poll(
        &mut self,
        _: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<DeniedUpgrade, (), Infallible>> {
        Poll::Pending
    }

    fn on_connection_event(&mut self, _: ConnectionEvent<DeniedUpgrade, DeniedUpgrade, (), ()>) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;
    use std::time::Duration;

    fn swarm() -> libp2p::Swarm<Behaviour> {
        libp2p::SwarmBuilder::with_new_identity()
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )
            .unwrap()
            .with_quic()
            .with_behaviour(|_| Behaviour::default())
            .unwrap()
            .with_swarm_config(|config| {
                config.with_idle_connection_timeout(Duration::from_millis(200))
            })
            .build()
    }

    #[tokio::test]
    async fn retained_tcp_and_quic_connections_survive_idle_then_close_on_release() {
        for address in ["/ip4/127.0.0.1/tcp/0", "/ip4/127.0.0.1/udp/0/quic-v1"] {
            tokio::time::timeout(Duration::from_secs(5), async {
                let mut a = swarm();
                let mut b = swarm();
                a.listen_on(address.parse().unwrap()).unwrap();
                let address = loop {
                    if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } = a.select_next_some().await { break address; }
                };
                b.dial(address).unwrap();
                let mut established = [false; 2];
                while !established.iter().all(|connected| *connected) {
                    tokio::select! {
                        event = a.select_next_some() => if matches!(event, libp2p::swarm::SwarmEvent::ConnectionEstablished { .. }) {
                            a.behaviour_mut().retain_connections(|_, _| true);
                            established[0] = true;
                        },
                        event = b.select_next_some() => if matches!(event, libp2p::swarm::SwarmEvent::ConnectionEstablished { .. }) {
                            b.behaviour_mut().retain_connections(|_, _| true);
                            established[1] = true;
                        },
                    }
                }
                let dwell = tokio::time::sleep(Duration::from_secs(1));
                tokio::pin!(dwell);
                loop {
                    tokio::select! {
                        () = &mut dwell => break,
                        event = a.select_next_some() => assert!(!matches!(event, libp2p::swarm::SwarmEvent::ConnectionClosed { .. }), "retained connection closed: {event:?}"),
                        event = b.select_next_some() => assert!(!matches!(event, libp2p::swarm::SwarmEvent::ConnectionClosed { .. }), "retained connection closed: {event:?}"),
                    }
                }
                a.behaviour_mut().retain_connections(|_, _| false);
                b.behaviour_mut().retain_connections(|_, _| false);
                let mut closed = [false; 2];
                while !closed.iter().all(|closed| *closed) {
                    tokio::select! {
                        event = a.select_next_some() => if matches!(event, libp2p::swarm::SwarmEvent::ConnectionClosed { .. }) { closed[0] = true; },
                        event = b.select_next_some() => if matches!(event, libp2p::swarm::SwarmEvent::ConnectionClosed { .. }) { closed[1] = true; },
                    }
                }
                assert!(a.behaviour().connections.is_empty());
                assert!(b.behaviour().connections.is_empty());
            }).await.unwrap_or_else(|_| panic!("idle retention case timed out: {address}"));
        }
    }

    #[test]
    fn retention_updates_coalesce_and_release_without_protocol_work() {
        let peer = PeerId::random();
        let id = ConnectionId::new_unchecked(1);
        let mut behaviour = Behaviour::default();
        behaviour.connections.insert(
            id,
            Retention {
                peer,
                desired: false,
                notified: false,
            },
        );
        let mut handler = Handler::default();
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        assert!(!handler.connection_keep_alive());
        for _ in 0..10_000 {
            behaviour.retain_connections(|_, _| true);
            behaviour.retain_connections(|_, _| false);
        }
        assert!(behaviour.poll(&mut cx).is_pending());
        assert_eq!(behaviour.connections.len(), 1);
        behaviour.retain_connections(|p, c| p == peer && c == id);
        let Poll::Ready(ToSwarm::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(connection),
            event,
        }) = behaviour.poll(&mut cx)
        else {
            panic!("missing retention notification")
        };
        assert_eq!((peer_id, connection, event), (peer, id, true));
        handler.on_behaviour_event(event);
        assert!(handler.connection_keep_alive());
        assert!(handler.poll(&mut cx).is_pending());
        assert!(behaviour.poll(&mut cx).is_pending());
        behaviour.retain_connections(|_, _| false);
        let Poll::Ready(ToSwarm::NotifyHandler { event, .. }) = behaviour.poll(&mut cx) else {
            panic!("missing release notification")
        };
        handler.on_behaviour_event(event);
        assert!(!handler.connection_keep_alive());
    }

    #[tokio::test]
    async fn unretained_tcp_and_quic_connections_expire_without_application_traffic() {
        for address in ["/ip4/127.0.0.1/tcp/0", "/ip4/127.0.0.1/udp/0/quic-v1"] {
            tokio::time::timeout(Duration::from_secs(5), async {
                let mut a = swarm();
                let mut b = swarm();
                a.listen_on(address.parse().unwrap()).unwrap();
                let address = loop {
                    if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
                        a.select_next_some().await
                    {
                        break address;
                    }
                };
                b.dial(address).unwrap();
                let mut established = [false; 2];
                let mut closed = [false; 2];
                while !closed.iter().all(|closed| *closed) {
                    let (index, event) = tokio::select! {
                        event = a.select_next_some() => (0, event),
                        event = b.select_next_some() => (1, event),
                    };
                    match event {
                        libp2p::swarm::SwarmEvent::ConnectionEstablished { .. } => {
                            established[index] = true
                        }
                        libp2p::swarm::SwarmEvent::ConnectionClosed { .. } => {
                            assert!(established[index]);
                            closed[index] = true;
                        }
                        _ => {}
                    }
                }
                assert!(a.behaviour().connections.is_empty());
                assert!(b.behaviour().connections.is_empty());
            })
            .await
            .unwrap_or_else(|_| panic!("unretained idle case timed out: {address}"));
        }
    }

    #[test]
    fn policy_updates_cannot_create_connection_owners() {
        let mut behaviour = Behaviour::default();
        behaviour.retain_connections(|_, _| panic!("no live connection"));
        assert!(behaviour.connections.is_empty());
    }

    #[test]
    fn closing_exact_owner_retires_unsent_retention() {
        let peer = PeerId::random();
        let id = ConnectionId::new_unchecked(1);
        let endpoint = libp2p::core::ConnectedPoint::Listener {
            local_addr: "/memory/1".parse().unwrap(),
            send_back_addr: "/memory/2".parse().unwrap(),
        };
        let mut behaviour = Behaviour::default();
        behaviour.on_swarm_event(FromSwarm::ConnectionEstablished(
            libp2p::swarm::behaviour::ConnectionEstablished {
                peer_id: peer,
                connection_id: id,
                endpoint: &endpoint,
                failed_addresses: &[],
                other_established: 0,
            },
        ));
        behaviour.retain_connections(|_, _| true);
        for (closing_peer, connection_id, remaining) in [
            (PeerId::random(), id, 1),
            (peer, ConnectionId::new_unchecked(2), 1),
            (peer, id, 0),
        ] {
            behaviour.on_swarm_event(FromSwarm::ConnectionClosed(
                libp2p::swarm::behaviour::ConnectionClosed {
                    peer_id: closing_peer,
                    connection_id,
                    endpoint: &endpoint,
                    cause: None,
                    remaining_established: 0,
                },
            ));
            assert_eq!(behaviour.connections.len(), remaining);
        }
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        assert!(behaviour.poll(&mut cx).is_pending());
    }
}
