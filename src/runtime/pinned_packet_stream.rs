use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt, io,
    task::{Context, Poll},
    time::Duration,
};

use futures::{FutureExt as _, future::BoxFuture};
use libp2p::{
    Multiaddr, PeerId, Stream, StreamProtocol,
    core::{
        Endpoint,
        transport::PortUse,
        upgrade::{InboundUpgrade, OutboundUpgrade, UpgradeInfo},
    },
    swarm::{
        ConnectionDenied, ConnectionHandler, ConnectionHandlerEvent, ConnectionId, FromSwarm,
        NetworkBehaviour, NotifyHandler, SubstreamProtocol, ToSwarm,
        handler::{
            ConnectionEvent, DialUpgradeError, FullyNegotiatedInbound, FullyNegotiatedOutbound,
            ListenUpgradeError,
        },
    },
};

use crate::{
    runtime::packet::{
        PACKET_PROTOCOL, PacketResponse, read_futures_frame, read_futures_response,
        write_futures_frame, write_futures_response,
    },
    wire::Frame,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(u64);

#[derive(Debug)]
pub enum Event {
    InboundRequest {
        peer: PeerId,
        connection_id: ConnectionId,
        request_id: RequestId,
        frame: Frame,
    },
    OutboundResponse {
        peer: PeerId,
        connection_id: ConnectionId,
        request_id: RequestId,
        response: PacketResponse,
    },
    OutboundFailure {
        peer: PeerId,
        connection_id: ConnectionId,
        request_id: RequestId,
        error: Failure,
    },
    InboundFailure {
        peer: PeerId,
        connection_id: ConnectionId,
        error: Failure,
    },
    ResponseSent {
        peer: PeerId,
        connection_id: ConnectionId,
        request_id: RequestId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Failure {
    StreamUpgrade(String),
    Io(String),
    MissingInboundRequest(RequestId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponseChannel {
    peer: PeerId,
    connection_id: ConnectionId,
    request_id: RequestId,
}

#[derive(Debug, Default)]
pub struct Behaviour {
    max_payload_len: usize,
    next_outbound_request_id: u64,
    connections: BTreeMap<ConnectionId, PeerId>,
    outbound_owners: BTreeMap<u64, (PeerId, ConnectionId)>,
    pending_events: VecDeque<ToSwarm<Event, HandlerCommand>>,
}

impl Behaviour {
    #[must_use]
    pub const fn new(max_payload_len: usize) -> Self {
        Self {
            max_payload_len,
            next_outbound_request_id: 1,
            connections: BTreeMap::new(),
            outbound_owners: BTreeMap::new(),
            pending_events: VecDeque::new(),
        }
    }

    pub fn send_request_on_connection(
        &mut self,
        peer: PeerId,
        connection_id: ConnectionId,
        frame: Frame,
    ) -> RequestId {
        let request_id = RequestId(self.next_outbound_request_id);
        self.next_outbound_request_id = self.next_outbound_request_id.wrapping_add(1).max(1);
        if self.connections.get(&connection_id) != Some(&peer) {
            self.pending_events
                .push_back(ToSwarm::GenerateEvent(Event::OutboundFailure {
                    peer,
                    connection_id,
                    request_id,
                    error: Failure::Io("connection is not established".to_owned()),
                }));
            return request_id;
        }
        self.outbound_owners
            .insert(request_id.0, (peer, connection_id));
        self.pending_events.push_back(ToSwarm::NotifyHandler {
            peer_id: peer,
            handler: NotifyHandler::One(connection_id),
            event: HandlerCommand::OutboundRequest { request_id, frame },
        });
        request_id
    }

    pub fn response_channel(
        peer: PeerId,
        connection_id: ConnectionId,
        request_id: RequestId,
    ) -> ResponseChannel {
        ResponseChannel {
            peer,
            connection_id,
            request_id,
        }
    }

    pub fn send_response(&mut self, channel: ResponseChannel, response: PacketResponse) {
        self.pending_events.push_back(ToSwarm::NotifyHandler {
            peer_id: channel.peer,
            handler: NotifyHandler::One(channel.connection_id),
            event: HandlerCommand::InboundResponse {
                request_id: channel.request_id,
                response,
            },
        });
    }
}

impl NetworkBehaviour for Behaviour {
    type ConnectionHandler = Handler;
    type ToSwarm = Event;

    fn handle_established_inbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        _local_addr: &Multiaddr,
        _remote_addr: &Multiaddr,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(Handler::new(peer, connection_id, self.max_payload_len))
    }

    fn handle_established_outbound_connection(
        &mut self,
        connection_id: ConnectionId,
        peer: PeerId,
        _addr: &Multiaddr,
        _role_override: Endpoint,
        _port_use: PortUse,
    ) -> Result<Self::ConnectionHandler, ConnectionDenied> {
        Ok(Handler::new(peer, connection_id, self.max_payload_len))
    }

    fn on_swarm_event(&mut self, event: FromSwarm) {
        if let FromSwarm::ConnectionEstablished(established) = event {
            self.connections
                .insert(established.connection_id, established.peer_id);
            return;
        }
        let FromSwarm::ConnectionClosed(closed) = event else {
            return;
        };
        if self.connections.get(&closed.connection_id) != Some(&closed.peer_id) {
            return;
        }
        self.connections.remove(&closed.connection_id);
        let owner = (closed.peer_id, closed.connection_id);
        self.pending_events.retain(|event| {
            !matches!(event, ToSwarm::NotifyHandler {
                peer_id, handler: NotifyHandler::One(connection_id), ..
            } if (*peer_id, *connection_id) == owner)
        });
        // Handlers disappear on closure, including requests already handed to them.
        // Retain only ownership metadata here, never another copy of the payload.
        self.outbound_owners.retain(|id, request_owner| {
            if *request_owner != owner {
                return true;
            }
            self.pending_events
                .push_back(ToSwarm::GenerateEvent(Event::OutboundFailure {
                    peer: closed.peer_id,
                    connection_id: closed.connection_id,
                    request_id: RequestId(*id),
                    error: Failure::Io("connection closed".to_owned()),
                }));
            false
        });
    }

    fn on_connection_handler_event(
        &mut self,
        peer: PeerId,
        connection_id: ConnectionId,
        event: HandlerEvent,
    ) {
        if let HandlerEvent::OutboundResponse { request_id, .. }
        | HandlerEvent::OutboundFailure { request_id, .. } = &event
        {
            if self.outbound_owners.get(&request_id.0) != Some(&(peer, connection_id)) {
                return;
            }
            self.outbound_owners.remove(&request_id.0);
        }
        match event {
            HandlerEvent::InboundRequest { request_id, frame } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(Event::InboundRequest {
                        peer,
                        connection_id,
                        request_id,
                        frame,
                    }));
            }
            HandlerEvent::OutboundResponse {
                request_id,
                response,
            } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(Event::OutboundResponse {
                        peer,
                        connection_id,
                        request_id,
                        response,
                    }));
            }
            HandlerEvent::OutboundFailure { request_id, error } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(Event::OutboundFailure {
                        peer,
                        connection_id,
                        request_id,
                        error,
                    }));
            }
            HandlerEvent::InboundFailure { error } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(Event::InboundFailure {
                        peer,
                        connection_id,
                        error,
                    }));
            }
            HandlerEvent::ResponseSent { request_id } => {
                self.pending_events
                    .push_back(ToSwarm::GenerateEvent(Event::ResponseSent {
                        peer,
                        connection_id,
                        request_id,
                    }));
            }
        }
    }

    fn poll(&mut self, _cx: &mut Context<'_>) -> Poll<ToSwarm<Self::ToSwarm, HandlerCommand>> {
        self.pending_events
            .pop_front()
            .map_or(Poll::Pending, Poll::Ready)
    }
}

#[derive(Debug)]
pub struct Handler {
    max_payload_len: usize,
    next_inbound_request_id: u64,
    pending_outbound: VecDeque<(RequestId, Frame)>,
    pending_inbound: HashMap<RequestId, Stream>,
    pending_responses: VecDeque<(RequestId, PacketResponse)>,
    response_writes: futures::stream::FuturesUnordered<BoxFuture<'static, HandlerEvent>>,
    pending_events: VecDeque<HandlerEvent>,
}

impl Handler {
    fn new(_peer: PeerId, _connection_id: ConnectionId, max_payload_len: usize) -> Self {
        Self {
            max_payload_len,
            next_inbound_request_id: 1,
            pending_outbound: VecDeque::new(),
            pending_inbound: HashMap::new(),
            pending_responses: VecDeque::new(),
            response_writes: futures::stream::FuturesUnordered::new(),
            pending_events: VecDeque::new(),
        }
    }

    fn next_inbound_request_id(&mut self) -> RequestId {
        let request_id = RequestId(self.next_inbound_request_id);
        self.next_inbound_request_id = self.next_inbound_request_id.wrapping_add(1).max(1);
        request_id
    }
}

impl ConnectionHandler for Handler {
    type FromBehaviour = HandlerCommand;
    type InboundOpenInfo = ();
    type InboundProtocol = PacketInboundUpgrade;
    type OutboundOpenInfo = RequestId;
    type OutboundProtocol = PacketOutboundUpgrade;
    type ToBehaviour = HandlerEvent;

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol> {
        SubstreamProtocol::new(PacketInboundUpgrade::new(self.max_payload_len), ())
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            HandlerCommand::OutboundRequest { request_id, frame } => {
                self.pending_outbound.push_back((request_id, frame));
            }
            HandlerCommand::InboundResponse {
                request_id,
                response,
            } => {
                self.pending_responses.push_back((request_id, response));
            }
        }
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, HandlerEvent>>
    {
        use futures::StreamExt as _;

        if let Poll::Ready(Some(event)) = self.response_writes.poll_next_unpin(cx) {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        if let Some(event) = self.pending_events.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        if let Some((request_id, response)) = self.pending_responses.pop_front() {
            if let Some(mut stream) = self.pending_inbound.remove(&request_id) {
                self.response_writes.push(
                    async move {
                        match write_futures_response(&mut stream, response).await {
                            Ok(()) => HandlerEvent::ResponseSent { request_id },
                            Err(error) => HandlerEvent::InboundFailure {
                                error: Failure::Io(error.to_string()),
                            },
                        }
                    }
                    .boxed(),
                );
            } else {
                return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                    HandlerEvent::InboundFailure {
                        error: Failure::MissingInboundRequest(request_id),
                    },
                ));
            }
        }

        if let Some((request_id, frame)) = self.pending_outbound.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(
                    PacketOutboundUpgrade { request_id, frame },
                    request_id,
                )
                .with_timeout(Duration::from_secs(10)),
            });
        }

        Poll::Pending
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: inbound,
                ..
            }) => {
                let request_id = self.next_inbound_request_id();
                self.pending_inbound.insert(request_id, inbound.stream);
                self.pending_events.push_back(HandlerEvent::InboundRequest {
                    request_id,
                    frame: inbound.frame,
                });
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: outbound,
                ..
            }) => {
                self.pending_events
                    .push_back(HandlerEvent::OutboundResponse {
                        request_id: outbound.request_id,
                        response: outbound.response,
                    });
            }
            ConnectionEvent::DialUpgradeError(DialUpgradeError { info, error }) => {
                self.pending_events
                    .push_back(HandlerEvent::OutboundFailure {
                        request_id: info,
                        error: Failure::StreamUpgrade(format!("{error:?}")),
                    });
            }
            ConnectionEvent::ListenUpgradeError(ListenUpgradeError { error, .. }) => {
                self.pending_events.push_back(HandlerEvent::InboundFailure {
                    error: Failure::StreamUpgrade(format!("{error:?}")),
                });
            }
            ConnectionEvent::AddressChange(_)
            | ConnectionEvent::LocalProtocolsChange(_)
            | ConnectionEvent::RemoteProtocolsChange(_) => {}
            _ => {}
        }
    }
}

#[derive(Debug)]
pub enum HandlerCommand {
    OutboundRequest {
        request_id: RequestId,
        frame: Frame,
    },
    InboundResponse {
        request_id: RequestId,
        response: PacketResponse,
    },
}

pub enum HandlerEvent {
    InboundRequest {
        request_id: RequestId,
        frame: Frame,
    },
    OutboundResponse {
        request_id: RequestId,
        response: PacketResponse,
    },
    OutboundFailure {
        request_id: RequestId,
        error: Failure,
    },
    InboundFailure {
        error: Failure,
    },
    ResponseSent {
        request_id: RequestId,
    },
}

impl fmt::Debug for HandlerEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InboundRequest { request_id, frame } => formatter
                .debug_struct("InboundRequest")
                .field("request_id", request_id)
                .field("frame_header", &frame.header)
                .finish(),
            Self::OutboundResponse {
                request_id,
                response,
            } => formatter
                .debug_struct("OutboundResponse")
                .field("request_id", request_id)
                .field("response", response)
                .finish(),
            Self::OutboundFailure { request_id, error } => formatter
                .debug_struct("OutboundFailure")
                .field("request_id", request_id)
                .field("error", error)
                .finish(),
            Self::InboundFailure { error } => formatter
                .debug_struct("InboundFailure")
                .field("error", error)
                .finish(),
            Self::ResponseSent { request_id } => formatter
                .debug_struct("ResponseSent")
                .field("request_id", request_id)
                .finish(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PacketInboundUpgrade {
    max_payload_len: usize,
}

impl PacketInboundUpgrade {
    const fn new(max_payload_len: usize) -> Self {
        Self { max_payload_len }
    }
}

impl UpgradeInfo for PacketInboundUpgrade {
    type Info = StreamProtocol;
    type InfoIter = std::iter::Once<StreamProtocol>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(StreamProtocol::new(PACKET_PROTOCOL))
    }
}

impl InboundUpgrade<Stream> for PacketInboundUpgrade {
    type Error = io::Error;
    type Future = BoxFuture<'static, io::Result<InboundPacketStream>>;
    type Output = InboundPacketStream;

    fn upgrade_inbound(self, mut stream: Stream, _info: Self::Info) -> Self::Future {
        async move {
            let frame = read_futures_frame(&mut stream, self.max_payload_len).await?;
            Ok(InboundPacketStream { frame, stream })
        }
        .boxed()
    }
}

pub struct InboundPacketStream {
    frame: Frame,
    stream: Stream,
}

impl fmt::Debug for InboundPacketStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InboundPacketStream")
            .field("frame_header", &self.frame.header)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct PacketOutboundUpgrade {
    request_id: RequestId,
    frame: Frame,
}

impl UpgradeInfo for PacketOutboundUpgrade {
    type Info = StreamProtocol;
    type InfoIter = std::iter::Once<StreamProtocol>;

    fn protocol_info(&self) -> Self::InfoIter {
        std::iter::once(StreamProtocol::new(PACKET_PROTOCOL))
    }
}

impl OutboundUpgrade<Stream> for PacketOutboundUpgrade {
    type Error = io::Error;
    type Future = BoxFuture<'static, io::Result<OutboundPacketStream>>;
    type Output = OutboundPacketStream;

    fn upgrade_outbound(self, mut stream: Stream, _info: Self::Info) -> Self::Future {
        async move {
            write_futures_frame(&mut stream, &self.frame).await?;
            futures::AsyncWriteExt::flush(&mut stream).await?;
            let response = read_futures_response(&mut stream).await?;
            Ok(OutboundPacketStream {
                request_id: self.request_id,
                response,
            })
        }
        .boxed()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutboundPacketStream {
    request_id: RequestId,
    response: PacketResponse,
}

#[cfg(test)]
mod tests {
    use std::task::Poll;

    use futures::future::poll_fn;
    use libp2p::{
        identity::Keypair,
        swarm::{NetworkBehaviour as _, NotifyHandler, ToSwarm},
    };

    use super::*;

    fn establish_connection(behaviour: &mut Behaviour, peer: PeerId, connection_id: ConnectionId) {
        let endpoint = libp2p::core::ConnectedPoint::Listener {
            local_addr: "/memory/1".parse().unwrap(),
            send_back_addr: "/memory/2".parse().unwrap(),
        };
        behaviour.on_swarm_event(FromSwarm::ConnectionEstablished(
            libp2p::swarm::behaviour::ConnectionEstablished {
                peer_id: peer,
                connection_id,
                endpoint: &endpoint,
                failed_addresses: &[],
                other_established: 0,
            },
        ));
    }

    fn close_connection(behaviour: &mut Behaviour, peer: PeerId, connection_id: ConnectionId) {
        let endpoint = libp2p::core::ConnectedPoint::Listener {
            local_addr: "/memory/1".parse().unwrap(),
            send_back_addr: "/memory/2".parse().unwrap(),
        };
        behaviour.on_swarm_event(FromSwarm::ConnectionClosed(
            libp2p::swarm::behaviour::ConnectionClosed {
                peer_id: peer,
                connection_id,
                endpoint: &endpoint,
                cause: None,
                remaining_established: 1,
            },
        ));
    }

    #[test]
    fn unavailable_targets_fail_without_retaining_ownership() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let stranger = Keypair::generate_ed25519().public().to_peer_id();
        let connection = ConnectionId::new_unchecked(1);
        let mut behaviour = Behaviour::new(1280);
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        for target in [peer, stranger, peer] {
            let request = behaviour.send_request_on_connection(
                target,
                connection,
                Frame::packet(1, 7, vec![0x45, 0, 0, 20]).unwrap(),
            );
            assert!(
                matches!(behaviour.poll(&mut cx), Poll::Ready(ToSwarm::GenerateEvent(Event::OutboundFailure { request_id, .. })) if request_id == request)
            );
            assert!(behaviour.outbound_owners.is_empty());
            assert!(behaviour.poll(&mut cx).is_pending());
            if target == stranger {
                close_connection(&mut behaviour, peer, connection);
            } else {
                establish_connection(&mut behaviour, peer, connection);
            }
        }
    }

    #[test]
    fn closure_completes_queued_and_delivered_requests_once() {
        for delivered in [false, true] {
            let peer = Keypair::generate_ed25519().public().to_peer_id();
            let closed = ConnectionId::new_unchecked(1);
            let healthy = ConnectionId::new_unchecked(2);
            let mut behaviour = Behaviour::new(1280);
            let mut cx = Context::from_waker(futures::task::noop_waker_ref());
            let frame = Frame::packet(1, 7, vec![0x45, 0, 0, 20]).unwrap();
            establish_connection(&mut behaviour, peer, closed);
            establish_connection(&mut behaviour, peer, healthy);
            let request = behaviour.send_request_on_connection(peer, closed, frame.clone());
            if delivered {
                let Poll::Ready(ToSwarm::NotifyHandler { event, .. }) = behaviour.poll(&mut cx)
                else {
                    panic!("expected queued request");
                };
                let mut handler = Handler::new(peer, closed, 1280);
                handler.on_behaviour_event(event);
                assert!(matches!(
                    handler.poll(&mut cx),
                    Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest { .. })
                ));
                drop(handler);
            }
            let other = behaviour.send_request_on_connection(peer, healthy, frame);
            close_connection(&mut behaviour, peer, closed);
            let mut failures = Vec::new();
            let mut notifications = Vec::new();
            while let Poll::Ready(event) = behaviour.poll(&mut cx) {
                match event {
                    ToSwarm::GenerateEvent(Event::OutboundFailure {
                        peer: owner,
                        connection_id,
                        request_id,
                        ..
                    }) => {
                        assert_eq!((owner, connection_id), (peer, closed));
                        failures.push(request_id);
                    }
                    ToSwarm::NotifyHandler {
                        handler: NotifyHandler::One(id),
                        event: HandlerCommand::OutboundRequest { request_id, .. },
                        ..
                    } => {
                        assert_eq!(id, healthy);
                        notifications.push(request_id);
                    }
                    event => panic!("unexpected event: {event:?}"),
                }
            }
            assert_eq!(failures, [request]);
            assert_eq!(notifications, [other]);
            close_connection(&mut behaviour, peer, closed);
            behaviour.on_connection_handler_event(
                peer,
                closed,
                HandlerEvent::OutboundResponse {
                    request_id: request,
                    response: PacketResponse::Accepted,
                },
            );
            behaviour.on_connection_handler_event(
                peer,
                closed,
                HandlerEvent::OutboundFailure {
                    request_id: request,
                    error: Failure::Io("late".into()),
                },
            );
            assert!(behaviour.poll(&mut cx).is_pending());
            behaviour.on_connection_handler_event(
                peer,
                healthy,
                HandlerEvent::OutboundResponse {
                    request_id: other,
                    response: PacketResponse::Accepted,
                },
            );
            assert!(
                matches!(behaviour.poll(&mut cx), Poll::Ready(ToSwarm::GenerateEvent(Event::OutboundResponse { request_id, .. })) if request_id == other)
            );
        }
    }

    #[test]
    fn terminal_events_require_exact_owner_and_survive_later_closure() {
        for success in [false, true] {
            let peer = Keypair::generate_ed25519().public().to_peer_id();
            let stranger = Keypair::generate_ed25519().public().to_peer_id();
            let connection = ConnectionId::new_unchecked(1);
            let other_connection = ConnectionId::new_unchecked(2);
            let mut behaviour = Behaviour::new(1280);
            establish_connection(&mut behaviour, peer, connection);
            let mut cx = Context::from_waker(futures::task::noop_waker_ref());
            let request = behaviour.send_request_on_connection(
                peer,
                connection,
                Frame::packet(1, 7, vec![0x45, 0, 0, 20]).unwrap(),
            );
            assert!(matches!(
                behaviour.poll(&mut cx),
                Poll::Ready(ToSwarm::NotifyHandler { .. })
            ));
            let terminal = || {
                if success {
                    HandlerEvent::OutboundResponse {
                        request_id: request,
                        response: PacketResponse::Accepted,
                    }
                } else {
                    HandlerEvent::OutboundFailure {
                        request_id: request,
                        error: Failure::Io("failed".into()),
                    }
                }
            };
            for (wrong_peer, wrong_connection) in [(stranger, connection), (peer, other_connection)]
            {
                behaviour.on_connection_handler_event(wrong_peer, wrong_connection, terminal());
                close_connection(&mut behaviour, wrong_peer, wrong_connection);
                assert!(behaviour.poll(&mut cx).is_pending());
                assert_eq!(behaviour.outbound_owners.len(), 1);
            }
            behaviour.on_connection_handler_event(peer, connection, terminal());
            behaviour.on_connection_handler_event(peer, connection, terminal());
            close_connection(&mut behaviour, peer, connection);
            match behaviour.poll(&mut cx) {
                Poll::Ready(ToSwarm::GenerateEvent(Event::OutboundResponse {
                    request_id, ..
                })) if success => assert_eq!(request_id, request),
                Poll::Ready(ToSwarm::GenerateEvent(Event::OutboundFailure {
                    request_id, ..
                })) if !success => assert_eq!(request_id, request),
                event => panic!("unexpected terminal event: {event:?}"),
            }
            assert!(behaviour.poll(&mut cx).is_pending());
            assert!(behaviour.outbound_owners.is_empty());
        }
    }

    #[tokio::test]
    async fn outbound_request_targets_selected_connection() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let connection_id = ConnectionId::new_unchecked(42);
        let frame = Frame::packet(1, 7, vec![0x45, 0, 0, 20]).expect("frame");
        let mut behaviour = Behaviour::new(1280);

        establish_connection(&mut behaviour, peer, connection_id);
        let request_id = behaviour.send_request_on_connection(peer, connection_id, frame.clone());

        let event = poll_fn(|cx| match behaviour.poll(cx) {
            Poll::Ready(event) => Poll::Ready(event),
            Poll::Pending => Poll::Pending,
        })
        .await;

        let ToSwarm::NotifyHandler {
            peer_id,
            handler,
            event,
        } = event
        else {
            panic!("expected notify handler event");
        };
        assert_eq!(peer_id, peer);
        let NotifyHandler::One(observed_connection_id) = handler else {
            panic!("expected single connection target");
        };
        assert_eq!(observed_connection_id, connection_id);
        let HandlerCommand::OutboundRequest {
            request_id: observed_request_id,
            frame: observed_frame,
        } = event
        else {
            panic!("expected outbound request command");
        };
        assert_eq!(observed_request_id, request_id);
        assert_eq!(observed_frame, frame);
    }

    #[tokio::test]
    async fn response_targets_inbound_request_connection() {
        let peer = Keypair::generate_ed25519().public().to_peer_id();
        let connection_id = ConnectionId::new_unchecked(7);
        let request_id = RequestId(9);
        let mut behaviour = Behaviour::new(1280);
        let channel = Behaviour::response_channel(peer, connection_id, request_id);

        behaviour.send_response(channel, PacketResponse::Accepted);

        let event = poll_fn(|cx| match behaviour.poll(cx) {
            Poll::Ready(event) => Poll::Ready(event),
            Poll::Pending => Poll::Pending,
        })
        .await;

        let ToSwarm::NotifyHandler {
            peer_id,
            handler,
            event,
        } = event
        else {
            panic!("expected notify handler event");
        };
        assert_eq!(peer_id, peer);
        let NotifyHandler::One(observed_connection_id) = handler else {
            panic!("expected single connection target");
        };
        assert_eq!(observed_connection_id, connection_id);
        let HandlerCommand::InboundResponse {
            request_id: observed_request_id,
            response,
        } = event
        else {
            panic!("expected inbound response command");
        };
        assert_eq!(observed_request_id, request_id);
        assert_eq!(response, PacketResponse::Accepted);
    }
}
