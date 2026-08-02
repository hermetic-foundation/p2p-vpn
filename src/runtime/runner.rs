use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use libp2p::{
    Swarm,
    request_response::{self, Message},
    swarm::SwarmEvent,
};
use tokio::sync::mpsc;

use crate::{
    config::{Config, QueueConfig},
    queue::PeerQueues,
    runtime::{
        forward::{ForwardError, Forwarder},
        p2p::{Behaviour, BehaviourEvent, HostConfig, P2pBuildError, P2pNode, build_node},
        tun::{TunDevice, TunRuntimeError},
    },
};

const TUN_READ_CHANNEL: usize = 1024;

pub async fn run_config(config: Config, device: TunDevice) -> Result<(), RunnerError> {
    let identity = config.identity()?;
    let node = build_node(HostConfig {
        identity,
        mtu: config.interface.mtu,
        listen_addresses: config.listen_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
    })?;
    let forwarder = Forwarder::from_config(&config)?;

    run_node(node, forwarder, device, config.interface.mtu, config.queue).await
}

pub async fn run_node(
    mut node: P2pNode,
    mut forwarder: Forwarder,
    device: TunDevice,
    mtu: u16,
    queue_config: QueueConfig,
) -> Result<(), RunnerError> {
    let device = Arc::new(Mutex::new(device));
    let mut tun_rx = spawn_tun_reader(Arc::clone(&device), mtu);
    let mut queues = PeerQueues::new(
        queue_config.max_packets_per_peer,
        queue_config.max_bytes_per_peer,
    );

    loop {
        tokio::select! {
            Some(packet) = tun_rx.recv() => {
                if let Err(error) = forwarder.enqueue_tun_packet(&mut queues, packet) {
                    eprintln!("dropping outbound packet: {error:?}");
                }
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues);
            }
            event = node.swarm.select_next_some() => {
                handle_swarm_event(&mut node.swarm, &forwarder, &device, event)?;
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues);
            }
        }
    }
}

fn spawn_tun_reader(device: Arc<Mutex<TunDevice>>, mtu: u16) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel(TUN_READ_CHANNEL);
    std::thread::spawn(move || {
        let mut buffer = vec![0; usize::from(mtu)];
        loop {
            let read = {
                let mut device = device.lock().expect("TUN mutex poisoned");
                device.read_packet(&mut buffer)
            };

            match read {
                Ok(length) => {
                    if tx.blocking_send(buffer[..length].to_vec()).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    eprintln!("failed to read TUN packet: {error:?}");
                    return;
                }
            }
        }
    });
    rx
}

fn drain_outbound_queue(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    queues: &mut PeerQueues,
) {
    while let Some(packet) = queues.dequeue() {
        if let Err(error) = forwarder.send_queued_packet(swarm, &packet) {
            eprintln!("dropping queued outbound packet: {error:?}");
        }
    }
}

fn handle_swarm_event(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    device: &Arc<Mutex<TunDevice>>,
    event: SwarmEvent<BehaviourEvent>,
) -> Result<(), RunnerError> {
    match event {
        SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        })) => {
            let packet = forwarder.accept_inbound_packet(peer, &request)?;
            {
                let mut device = device.lock().expect("TUN mutex poisoned");
                device.write_packet(packet)?;
            }
            Forwarder::send_packet_response(swarm, channel)
                .map_err(|_| RunnerError::PacketResponseDropped)?;
        }
        SwarmEvent::Behaviour(BehaviourEvent::Packet(
            request_response::Event::OutboundFailure { peer, error, .. },
        )) => {
            eprintln!("packet request to {peer} failed: {error}");
        }
        SwarmEvent::Behaviour(BehaviourEvent::Packet(
            request_response::Event::InboundFailure { peer, error, .. },
        )) => {
            eprintln!("packet request from {peer} failed: {error}");
        }
        _ => {}
    }

    Ok(())
}

#[derive(Debug)]
pub enum RunnerError {
    Config(crate::config::ConfigError),
    P2p(P2pBuildError),
    Forward(ForwardError),
    Tun(TunRuntimeError),
    PacketResponseDropped,
}

impl From<crate::config::ConfigError> for RunnerError {
    fn from(error: crate::config::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<P2pBuildError> for RunnerError {
    fn from(error: P2pBuildError) -> Self {
        Self::P2p(error)
    }
}

impl From<ForwardError> for RunnerError {
    fn from(error: ForwardError) -> Self {
        Self::Forward(error)
    }
}

impl From<TunRuntimeError> for RunnerError {
    fn from(error: TunRuntimeError) -> Self {
        Self::Tun(error)
    }
}
