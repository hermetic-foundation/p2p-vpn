use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::StreamExt as _;
use libp2p::{
    Swarm,
    request_response::{self, Message},
    swarm::SwarmEvent,
};
use tokio::sync::mpsc;

use crate::{
    config::{Config, QueueConfig},
    metrics::RuntimeMetrics,
    queue::PeerQueues,
    runtime::{
        forward::{ForwardError, Forwarder},
        p2p::{Behaviour, BehaviourEvent, HostConfig, P2pBuildError, P2pNode, build_node},
        tun::{TunDevice, TunRuntimeError},
    },
};

const TUN_READ_CHANNEL: usize = 1024;

pub async fn run_config(
    config: Config,
    device: TunDevice,
    metrics_interval: Option<Duration>,
) -> Result<(), RunnerError> {
    let identity = config.identity()?;
    let node = build_node(HostConfig {
        identity,
        mtu: config.interface.mtu,
        listen_addresses: config.listen_multiaddrs()?,
        bootstrap_peers: config.bootstrap_multiaddrs()?,
        known_peers: config.peer_multiaddrs()?,
    })?;
    let forwarder = Forwarder::from_config(&config)?;

    run_node(
        node,
        forwarder,
        device,
        config.interface.mtu,
        config.queue,
        metrics_interval,
    )
    .await
}

pub async fn run_node(
    mut node: P2pNode,
    mut forwarder: Forwarder,
    device: TunDevice,
    mtu: u16,
    queue_config: QueueConfig,
    metrics_interval: Option<Duration>,
) -> Result<(), RunnerError> {
    let device = Arc::new(Mutex::new(device));
    let metrics = Arc::new(RuntimeMetrics::default());
    let mut tun_rx = spawn_tun_reader(Arc::clone(&device), Arc::clone(&metrics), mtu);
    let mut queues = PeerQueues::new(
        queue_config.max_packets_per_peer,
        queue_config.max_bytes_per_peer,
    );
    let mut metrics_tick = metrics_interval.map(tokio::time::interval);

    loop {
        tokio::select! {
            Some(packet) = tun_rx.recv() => {
                if let Err(error) = forwarder.enqueue_tun_packet(&mut queues, packet) {
                    metrics.record_outbound_drop();
                    eprintln!("dropping outbound packet: {error:?}");
                }
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &metrics);
            }
            event = node.swarm.select_next_some() => {
                handle_swarm_event(&mut node.swarm, &forwarder, &device, &metrics, event)?;
                drain_outbound_queue(&mut node.swarm, &forwarder, &mut queues, &metrics);
            }
            () = async {
                metrics_tick
                    .as_mut()
                    .expect("metrics interval is present")
                    .tick()
                    .await;
            }, if metrics_tick.is_some() => {
                print_metrics(&metrics, queues.total_stats());
            }
        }
    }
}

fn spawn_tun_reader(
    device: Arc<Mutex<TunDevice>>,
    metrics: Arc<RuntimeMetrics>,
    mtu: u16,
) -> mpsc::Receiver<Vec<u8>> {
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
                    metrics.record_tun_read(length);
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
    metrics: &RuntimeMetrics,
) {
    while let Some(packet) = queues.dequeue() {
        if let Err(error) = forwarder.send_queued_packet(swarm, &packet) {
            metrics.record_outbound_drop();
            eprintln!("dropping queued outbound packet: {error:?}");
        } else {
            metrics.record_outbound_sent();
        }
    }
}

fn handle_swarm_event(
    swarm: &mut Swarm<Behaviour>,
    forwarder: &Forwarder,
    device: &Arc<Mutex<TunDevice>>,
    metrics: &RuntimeMetrics,
    event: SwarmEvent<BehaviourEvent>,
) -> Result<(), RunnerError> {
    match event {
        SwarmEvent::Behaviour(BehaviourEvent::Packet(request_response::Event::Message {
            peer,
            message: Message::Request {
                request, channel, ..
            },
            ..
        })) => match forwarder.accept_inbound_packet(peer, &request) {
            Ok(packet) => {
                {
                    let mut device = device.lock().expect("TUN mutex poisoned");
                    device.write_packet(packet)?;
                }
                metrics.record_tun_write(packet.len());
                metrics.record_inbound_accepted();
                Forwarder::send_packet_response(swarm, channel)
                    .map_err(|_| RunnerError::PacketResponseDropped)?;
            }
            Err(error) => {
                metrics.record_inbound_drop();
                eprintln!("dropping inbound packet from {peer}: {error:?}");
            }
        },
        SwarmEvent::Behaviour(BehaviourEvent::Packet(
            request_response::Event::OutboundFailure { peer, error, .. },
        )) => {
            metrics.record_outbound_failure();
            eprintln!("packet request to {peer} failed: {error}");
        }
        SwarmEvent::Behaviour(BehaviourEvent::Packet(
            request_response::Event::InboundFailure { peer, error, .. },
        )) => {
            metrics.record_inbound_failure();
            eprintln!("packet request from {peer} failed: {error}");
        }
        _ => {}
    }

    Ok(())
}

fn print_metrics(metrics: &RuntimeMetrics, queue: crate::queue::QueueStats) {
    eprintln!("metrics:");
    for line in metrics.snapshot(queue).lines() {
        eprintln!("  {line}");
    }
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
