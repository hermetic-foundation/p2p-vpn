use std::{
    collections::{BTreeMap, VecDeque},
    fmt, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use p2p_vpn::{
    route::IpCidr,
    runtime::{
        runner::{RunnerError, TunRouteController},
        tun::{
            PacketIo, PacketRead, PacketWrite, TunRouteUpdate, TunRuntimeConfig, TunRuntimeError,
        },
    },
};

use crate::packet_translation::{PacketTranslator, PrimaryAddresses, validate_packet_isolation};

pub(crate) const MAX_NETWORKS: usize = 16;
pub(crate) const DEFAULT_QUEUE_MAX_PACKETS: usize = 256;
pub(crate) const DEFAULT_QUEUE_MAX_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_PREFIXES_PER_NETWORK: usize = 1024;
pub(crate) const MAX_TOTAL_PREFIXES: usize = 4096;

const MAX_NETWORK_ID_BYTES: usize = 128;
const ROUTE_UPDATE_REJECTED: &str = "multi-network route update is ambiguous";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct QueueLimits {
    pub max_packets: usize,
    pub max_bytes: usize,
}

impl QueueLimits {
    pub(crate) const DEFAULT: Self = Self {
        max_packets: DEFAULT_QUEUE_MAX_PACKETS,
        max_bytes: DEFAULT_QUEUE_MAX_BYTES,
    };

    fn validate(self) -> Result<Self, SupervisorError> {
        if self.max_packets == 0 || self.max_bytes == 0 {
            return Err(SupervisorError::InvalidQueueLimits);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NetworkSpec {
    pub id: String,
    pub tun: TunRuntimeConfig,
}

pub(crate) struct NetworkPort {
    pub id: String,
    pub packet_io: PacketIo,
    pub route_controller: SupervisorTunRoutes,
}

pub(crate) struct PacketSwitch {
    routes: DispatchRegistry,
    outbound: BTreeMap<String, OutboundPort>,
    inbound: Vec<InboundPort>,
    controls: BTreeMap<String, NetworkControl>,
    next_inbound: Mutex<usize>,
    wake: Arc<WriterWake>,
    metrics: Arc<SwitchMetrics>,
}

impl PacketSwitch {
    #[cfg(test)]
    pub(crate) fn new(
        networks: Vec<NetworkSpec>,
        limits: QueueLimits,
    ) -> Result<(Self, Vec<NetworkPort>), SupervisorError> {
        let presentation = networks
            .first()
            .map(|network| primary_addresses(&network.tun))
            .ok_or(SupervisorError::InvalidNetworkCount { actual: 0 })?;
        Self::new_with_presentation(networks, presentation, limits)
    }

    pub(crate) fn new_with_presentation(
        networks: Vec<NetworkSpec>,
        presentation: PrimaryAddresses,
        limits: QueueLimits,
    ) -> Result<(Self, Vec<NetworkPort>), SupervisorError> {
        let limits = limits.validate()?;
        if networks.is_empty() || networks.len() > MAX_NETWORKS {
            return Err(SupervisorError::InvalidNetworkCount {
                actual: networks.len(),
            });
        }

        validate_presentation(presentation)?;
        let translation_policy = TranslationPolicy::new(&networks, presentation);
        let routes = DispatchRegistry::new(&networks, presentation)?;
        let wake = Arc::new(WriterWake::default());
        let metrics = Arc::new(SwitchMetrics::default());
        let mut outbound = BTreeMap::new();
        let mut inbound = Vec::with_capacity(networks.len());
        let mut controls = BTreeMap::new();
        let mut ports = Vec::with_capacity(networks.len());

        for network in networks {
            let translator = PacketTranslator::new(presentation, primary_addresses(&network.tun));
            let network_metrics = Arc::new(NetworkMetrics::default());
            metrics.insert_network(&network.id, Arc::clone(&network_metrics));

            let (outbound_sender, outbound_receiver) = packet_queue(limits);
            let (inbound_sender, inbound_receiver) = packet_queue(limits);
            let inbound_active = Arc::new(Mutex::new(true));
            let control = NetworkControl {
                id: network.id.clone(),
                routes: routes.clone(),
                outbound: outbound_sender.clone(),
                inbound: inbound_receiver.clone(),
                active: Arc::clone(&inbound_active),
                metrics: Arc::clone(&network_metrics),
                wake: Arc::clone(&wake),
            };
            outbound.insert(
                network.id.clone(),
                OutboundPort {
                    sender: outbound_sender,
                    mtu: usize::from(network.tun.mtu),
                    metrics: Arc::clone(&network_metrics),
                    translator,
                    translation_policy,
                },
            );
            inbound.push(InboundPort {
                id: network.id.clone(),
                receiver: inbound_receiver,
                metrics: Arc::clone(&network_metrics),
                active: inbound_active,
                routes: routes.clone(),
                translator,
                translation_policy,
            });
            controls.insert(network.id.clone(), control.clone());
            ports.push(NetworkPort {
                id: network.id.clone(),
                packet_io: PacketIo::new(
                    PortReader {
                        receiver: outbound_receiver,
                    },
                    PortWriter {
                        sender: inbound_sender,
                        wake: Arc::clone(&wake),
                        metrics: network_metrics,
                        mtu: usize::from(network.tun.mtu),
                        network_id: network.id.clone(),
                        routes: routes.clone(),
                    },
                ),
                route_controller: SupervisorTunRoutes { control },
            });
        }

        Ok((
            Self {
                routes,
                outbound,
                inbound,
                controls,
                next_inbound: Mutex::new(0),
                wake,
                metrics,
            },
            ports,
        ))
    }

    pub(crate) fn dispatch_packet(&self, packet: &[u8]) -> DispatchOutcome {
        let (source, destination) = match packet_addresses(packet) {
            Ok(addresses) => addresses,
            Err(_) => {
                self.metrics
                    .malformed_outbound_packets
                    .fetch_add(1, Ordering::Relaxed);
                return DispatchOutcome::Malformed;
            }
        };
        let network_id = match self.routes.resolve(source, destination) {
            RouteResolution::Routed(network_id) => network_id,
            RouteResolution::SourceMismatch(network_id) => {
                self.metrics
                    .source_mismatch_outbound_packets
                    .fetch_add(1, Ordering::Relaxed);
                if let Some(port) = self.outbound.get(&network_id) {
                    port.metrics
                        .outbound_source_mismatch_drops
                        .fetch_add(1, Ordering::Relaxed);
                }
                return DispatchOutcome::SourceMismatch { network_id };
            }
            RouteResolution::NoRoute => {
                self.metrics
                    .unroutable_outbound_packets
                    .fetch_add(1, Ordering::Relaxed);
                return DispatchOutcome::NoRoute;
            }
        };
        let Some(port) = self.outbound.get(&network_id) else {
            self.metrics
                .unroutable_outbound_packets
                .fetch_add(1, Ordering::Relaxed);
            return DispatchOutcome::NoRoute;
        };
        if packet.len() > port.mtu {
            port.metrics
                .outbound_oversized_drops
                .fetch_add(1, Ordering::Relaxed);
            return DispatchOutcome::Oversized { network_id };
        }

        let queue_result = if port.translator.outbound_requires_translation(source) {
            match port.sender.reserve(packet.len()) {
                Ok(reservation) => {
                    let mut translated = packet.to_vec();
                    if port.translator.translate_outbound(&mut translated).is_err() {
                        port.metrics
                            .outbound_translation_drops
                            .fetch_add(1, Ordering::Relaxed);
                        return DispatchOutcome::TranslationFailed { network_id };
                    }
                    reservation.commit(translated)
                }
                Err(outcome) => outcome,
            }
        } else if port.translation_policy.applies(source) {
            if PacketTranslator::validate_supported(packet).is_err() {
                port.metrics
                    .outbound_translation_drops
                    .fetch_add(1, Ordering::Relaxed);
                return DispatchOutcome::TranslationFailed { network_id };
            }
            port.sender.push(packet)
        } else {
            port.sender.push(packet)
        };

        match queue_result {
            QueuePush::Queued => {
                port.metrics
                    .outbound_enqueued_packets
                    .fetch_add(1, Ordering::Relaxed);
                DispatchOutcome::Queued { network_id }
            }
            QueuePush::Full => {
                port.metrics
                    .outbound_queue_drops
                    .fetch_add(1, Ordering::Relaxed);
                DispatchOutcome::QueueFull { network_id }
            }
            QueuePush::Oversized => {
                port.metrics
                    .outbound_oversized_drops
                    .fetch_add(1, Ordering::Relaxed);
                DispatchOutcome::Oversized { network_id }
            }
            QueuePush::Closed => {
                port.metrics
                    .outbound_removed_drops
                    .fetch_add(1, Ordering::Relaxed);
                DispatchOutcome::Closed { network_id }
            }
            QueuePush::ReservationMismatch => {
                port.metrics
                    .outbound_translation_drops
                    .fetch_add(1, Ordering::Relaxed);
                DispatchOutcome::TranslationFailed { network_id }
            }
        }
    }

    pub(crate) fn write_next(&self, writer: &mut impl PacketWrite) -> io::Result<Option<String>> {
        if self.inbound.is_empty() {
            return Ok(None);
        }
        let mut next_inbound = self
            .next_inbound
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for offset in 0..self.inbound.len() {
            let index = (*next_inbound + offset) % self.inbound.len();
            let port = &self.inbound[index];
            let active = port
                .active
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !*active {
                continue;
            }
            let Some(mut packet) = port.receiver.try_pop() else {
                continue;
            };
            *next_inbound = (index + 1) % self.inbound.len();
            drop(next_inbound);
            let validation = port.routes.validate_inbound(&port.id, &packet);
            if validation != InboundValidation::Valid {
                record_inbound_validation_drop(&port.metrics, validation);
                return Ok(Some(port.id.clone()));
            }
            if port.translation_policy.applies_packet(&packet)
                && port.translator.translate_inbound(&mut packet).is_err()
            {
                port.metrics
                    .inbound_translation_drops
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(Some(port.id.clone()));
            }
            match writer.write_packet(&packet) {
                Ok(length) if length == packet.len() => {
                    port.metrics
                        .inbound_written_packets
                        .fetch_add(1, Ordering::Relaxed);
                    return Ok(Some(port.id.clone()));
                }
                Ok(_) => {
                    port.metrics
                        .inbound_write_failures
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "shared TUN writer did not consume the complete packet",
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    port.metrics
                        .inbound_write_backpressure_drops
                        .fetch_add(1, Ordering::Relaxed);
                    return Ok(Some(port.id.clone()));
                }
                Err(error) => {
                    port.metrics
                        .inbound_write_failures
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(error);
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn inbound_generation(&self) -> u64 {
        self.wake.generation()
    }

    pub(crate) fn wait_for_inbound_since(&self, generation: u64, timeout: Duration) -> bool {
        self.wake.wait_since(generation, timeout)
    }

    pub(crate) fn remove_network(&self, network_id: &str) {
        if let Some(control) = self.controls.get(network_id) {
            control.deactivate();
        }
    }

    pub(crate) fn close(&self) {
        for control in self.controls.values() {
            control.deactivate();
        }
    }

    pub(crate) fn network_lease(
        self: &Arc<Self>,
        network_id: &str,
    ) -> Result<NetworkLease, SupervisorError> {
        if !self.outbound.contains_key(network_id) {
            return Err(SupervisorError::UnknownNetwork(network_id.to_owned()));
        }
        Ok(NetworkLease {
            packet_switch: Arc::clone(self),
            network_id: Some(network_id.to_owned()),
        })
    }

    pub(crate) fn snapshot(&self) -> SwitchSnapshot {
        self.metrics.snapshot()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DispatchOutcome {
    Queued { network_id: String },
    QueueFull { network_id: String },
    Oversized { network_id: String },
    Closed { network_id: String },
    SourceMismatch { network_id: String },
    TranslationFailed { network_id: String },
    Malformed,
    NoRoute,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SwitchSnapshot {
    pub malformed_outbound_packets: u64,
    pub unroutable_outbound_packets: u64,
    pub source_mismatch_outbound_packets: u64,
    pub networks: Vec<NetworkSnapshot>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct NetworkSnapshot {
    pub id: String,
    pub outbound_enqueued_packets: u64,
    pub outbound_queue_drops: u64,
    pub outbound_oversized_drops: u64,
    pub outbound_source_mismatch_drops: u64,
    pub outbound_translation_drops: u64,
    pub outbound_removed_drops: u64,
    pub inbound_enqueued_packets: u64,
    pub inbound_queue_drops: u64,
    pub inbound_oversized_drops: u64,
    pub inbound_malformed_drops: u64,
    pub inbound_source_mismatch_drops: u64,
    pub inbound_destination_mismatch_drops: u64,
    pub inbound_translation_drops: u64,
    pub inbound_removed_drops: u64,
    pub inbound_written_packets: u64,
    pub inbound_write_backpressure_drops: u64,
    pub inbound_write_failures: u64,
    pub route_update_rejections: u64,
}

pub(crate) struct NetworkLease {
    packet_switch: Arc<PacketSwitch>,
    network_id: Option<String>,
}

impl Drop for NetworkLease {
    fn drop(&mut self) {
        if let Some(network_id) = self.network_id.take() {
            self.packet_switch.remove_network(&network_id);
        }
    }
}

#[derive(Clone)]
pub(crate) struct SupervisorTunRoutes {
    control: NetworkControl,
}

impl TunRouteController for SupervisorTunRoutes {
    fn reconcile(
        &mut self,
        installed: &TunRuntimeConfig,
        next: &TunRuntimeConfig,
        _update: &TunRouteUpdate,
    ) -> Result<(), RunnerError> {
        if self.control.replace_routes(installed, next).is_ok() {
            return Ok(());
        }

        self.control
            .metrics
            .route_update_rejections
            .fetch_add(1, Ordering::Relaxed);
        Err(RunnerError::Tun(TunRuntimeError::NonAdditiveUpdate(
            ROUTE_UPDATE_REJECTED,
        )))
    }
}

#[derive(Clone)]
struct DispatchRegistry {
    state: Arc<RwLock<DispatchState>>,
}

impl DispatchRegistry {
    fn new(
        networks: &[NetworkSpec],
        presentation: PrimaryAddresses,
    ) -> Result<Self, SupervisorError> {
        let mut configured = BTreeMap::new();
        for network in networks {
            validate_network_id(&network.id)?;
            if configured
                .insert(
                    network.id.clone(),
                    NetworkRoutes::try_from_tun(&network.id, &network.tun)?,
                )
                .is_some()
            {
                return Err(SupervisorError::DuplicateNetworkId(network.id.clone()));
            }
        }
        Ok(Self {
            state: Arc::new(RwLock::new(DispatchState::validated(
                configured,
                presentation,
            )?)),
        })
    }

    fn resolve(&self, source: IpAddr, destination: IpAddr) -> RouteResolution {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        let Some(route) = state
            .dispatch
            .iter()
            .find(|route| route.prefix.contains(destination))
        else {
            return RouteResolution::NoRoute;
        };
        let Some(network) = state.networks.get(&route.network_id) else {
            return RouteResolution::NoRoute;
        };
        if network.local.iter().any(|prefix| prefix.contains(source))
            || state.presentation.contains(source)
        {
            RouteResolution::Routed(route.network_id.clone())
        } else {
            RouteResolution::SourceMismatch(route.network_id.clone())
        }
    }

    fn validate_inbound(&self, network_id: &str, packet: &[u8]) -> InboundValidation {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        let Some(network) = state.networks.get(network_id) else {
            return InboundValidation::Removed;
        };
        let (source, destination) = match packet_addresses(packet) {
            Ok(addresses) => addresses,
            Err(_) => return InboundValidation::Malformed,
        };
        if !network.remote.iter().any(|prefix| prefix.contains(source)) {
            return InboundValidation::SourceMismatch;
        }
        if !network
            .local
            .iter()
            .any(|prefix| prefix.contains(destination))
        {
            return InboundValidation::DestinationMismatch;
        }
        InboundValidation::Valid
    }

    fn replace(
        &self,
        network_id: &str,
        installed: &TunRuntimeConfig,
        next: &TunRuntimeConfig,
    ) -> Result<(), SupervisorError> {
        if primary_addresses(installed) != primary_addresses(next)
            || installed.additional_addresses != next.additional_addresses
        {
            return Err(SupervisorError::NetworkAddressChanged(
                network_id.to_owned(),
            ));
        }
        let installed_routes = NetworkRoutes::try_from_tun(network_id, installed)?;
        let next_routes = NetworkRoutes::try_from_tun(network_id, next)?;
        let (generation, presentation, mut candidate) = {
            let current = self.state.read().unwrap_or_else(|error| error.into_inner());
            let Some(expected) = current.networks.get(network_id) else {
                return Err(SupervisorError::UnknownNetwork(network_id.to_owned()));
            };
            if expected != &installed_routes {
                return Err(SupervisorError::StaleRouteUpdate(network_id.to_owned()));
            }
            (
                current.generation,
                current.presentation,
                current.networks.clone(),
            )
        };
        candidate.insert(network_id.to_owned(), next_routes);
        let mut candidate = DispatchState::validated(candidate, presentation)?;

        let mut current = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if current.generation != generation
            || current.networks.get(network_id) != Some(&installed_routes)
        {
            return Err(SupervisorError::StaleRouteUpdate(network_id.to_owned()));
        }
        candidate.generation = generation.wrapping_add(1);
        *current = candidate;
        Ok(())
    }

    fn remove(&self, network_id: &str) {
        let mut current = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if current.networks.remove(network_id).is_some() {
            current.generation = current.generation.wrapping_add(1);
            current.rebuild_dispatch();
        }
    }
}

enum RouteResolution {
    Routed(String),
    SourceMismatch(String),
    NoRoute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InboundValidation {
    Valid,
    Malformed,
    SourceMismatch,
    DestinationMismatch,
    Removed,
}

fn record_inbound_validation_drop(metrics: &NetworkMetrics, validation: InboundValidation) {
    let counter = match validation {
        InboundValidation::Valid => return,
        InboundValidation::Malformed => &metrics.inbound_malformed_drops,
        InboundValidation::SourceMismatch => &metrics.inbound_source_mismatch_drops,
        InboundValidation::DestinationMismatch => &metrics.inbound_destination_mismatch_drops,
        InboundValidation::Removed => &metrics.inbound_removed_drops,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NetworkRoutes {
    local: Vec<IpCidr>,
    remote: Vec<IpCidr>,
}

impl NetworkRoutes {
    fn try_from_tun(network_id: &str, tun: &TunRuntimeConfig) -> Result<Self, SupervisorError> {
        let mut local = vec![
            IpCidr::new(IpAddr::V4(tun.addresses.ipv4), 32).expect("IPv4 host prefix is valid"),
            IpCidr::new(IpAddr::V6(tun.addresses.ipv6), 128).expect("IPv6 host prefix is valid"),
        ];
        for address in &tun.additional_addresses {
            push_unique_bounded(network_id, &mut local, &[], *address)?;
        }
        let mut remote = Vec::new();
        for route in &tun.routes {
            push_unique_bounded(network_id, &mut remote, &local, route.prefix)?;
        }
        Ok(Self { local, remote })
    }

    fn all(&self) -> impl Iterator<Item = IpCidr> + '_ {
        self.local.iter().chain(&self.remote).copied()
    }
}

struct DispatchState {
    generation: u64,
    presentation: PrimaryAddresses,
    networks: BTreeMap<String, NetworkRoutes>,
    dispatch: Vec<DispatchRoute>,
}

impl DispatchState {
    fn validated(
        networks: BTreeMap<String, NetworkRoutes>,
        presentation: PrimaryAddresses,
    ) -> Result<Self, SupervisorError> {
        let total_prefixes = networks
            .values()
            .map(|routes| routes.local.len().saturating_add(routes.remote.len()))
            .sum::<usize>();
        if total_prefixes > MAX_TOTAL_PREFIXES {
            return Err(SupervisorError::TooManyTotalPrefixes {
                actual: total_prefixes,
            });
        }
        let entries = networks.iter().collect::<Vec<_>>();
        for (index, (left_id, left)) in entries.iter().enumerate() {
            for (right_id, right) in entries.iter().skip(index + 1) {
                for left_prefix in left.all() {
                    if let Some(right_prefix) = right
                        .all()
                        .find(|right_prefix| left_prefix.overlaps(*right_prefix))
                    {
                        return Err(SupervisorError::OverlappingNetworks {
                            first_network: (*left_id).clone(),
                            first_prefix: left_prefix,
                            second_network: (*right_id).clone(),
                            second_prefix: right_prefix,
                        });
                    }
                }
            }
        }
        for (network_id, routes) in &networks {
            for local in &routes.local {
                if let Some(remote) = routes.remote.iter().find(|remote| local.overlaps(**remote)) {
                    return Err(SupervisorError::LocalRouteOverlap {
                        network_id: network_id.clone(),
                        local: *local,
                        remote: *remote,
                    });
                }
            }
            for address in [IpAddr::V4(presentation.ipv4), IpAddr::V6(presentation.ipv6)] {
                if let Some(remote) = routes.remote.iter().find(|remote| remote.contains(address)) {
                    return Err(SupervisorError::PresentationRouteOverlap {
                        network_id: network_id.clone(),
                        presentation: address,
                        remote: *remote,
                    });
                }
            }
        }

        let mut state = Self {
            generation: 0,
            presentation,
            networks,
            dispatch: Vec::new(),
        };
        state.rebuild_dispatch();
        Ok(state)
    }

    fn rebuild_dispatch(&mut self) {
        self.dispatch = self
            .networks
            .iter()
            .flat_map(|(network_id, routes)| {
                routes.remote.iter().copied().map(|prefix| DispatchRoute {
                    network_id: network_id.clone(),
                    prefix,
                })
            })
            .collect();
        self.dispatch.sort_by(|left, right| {
            right
                .prefix
                .prefix_len()
                .cmp(&left.prefix.prefix_len())
                .then(left.network_id.cmp(&right.network_id))
        });
    }
}

struct DispatchRoute {
    network_id: String,
    prefix: IpCidr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SupervisorError {
    InvalidNetworkCount {
        actual: usize,
    },
    InvalidNetworkId,
    DuplicateNetworkId(String),
    InvalidQueueLimits,
    InvalidPresentationAddresses,
    UnknownNetwork(String),
    NetworkAddressChanged(String),
    StaleRouteUpdate(String),
    TooManyNetworkPrefixes {
        network_id: String,
        actual: usize,
    },
    TooManyTotalPrefixes {
        actual: usize,
    },
    LocalRouteOverlap {
        network_id: String,
        local: IpCidr,
        remote: IpCidr,
    },
    PresentationRouteOverlap {
        network_id: String,
        presentation: IpAddr,
        remote: IpCidr,
    },
    OverlappingNetworks {
        first_network: String,
        first_prefix: IpCidr,
        second_network: String,
        second_prefix: IpCidr,
    },
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNetworkCount { actual } => {
                write!(
                    formatter,
                    "Android supervisor requires 1 to {MAX_NETWORKS} networks, got {actual}"
                )
            }
            Self::InvalidNetworkId => write!(formatter, "Android supervisor network ID is invalid"),
            Self::DuplicateNetworkId(id) => {
                write!(
                    formatter,
                    "Android supervisor network ID is duplicated: {id}"
                )
            }
            Self::InvalidQueueLimits => {
                write!(formatter, "Android supervisor queue limits are invalid")
            }
            Self::InvalidPresentationAddresses => {
                write!(
                    formatter,
                    "Android supervisor presentation addresses are invalid"
                )
            }
            Self::UnknownNetwork(id) => {
                write!(formatter, "Android supervisor network is unknown: {id}")
            }
            Self::NetworkAddressChanged(id) => write!(
                formatter,
                "Android supervisor local addresses changed while {id} was active"
            ),
            Self::StaleRouteUpdate(id) => {
                write!(
                    formatter,
                    "Android supervisor route update is stale for network {id}"
                )
            }
            Self::TooManyNetworkPrefixes { network_id, actual } => write!(
                formatter,
                "Android supervisor network {network_id} has {actual} prefixes; maximum is {MAX_PREFIXES_PER_NETWORK}"
            ),
            Self::TooManyTotalPrefixes { actual } => write!(
                formatter,
                "Android supervisor has {actual} prefixes; maximum is {MAX_TOTAL_PREFIXES}"
            ),
            Self::LocalRouteOverlap {
                network_id,
                local,
                remote,
            } => write!(
                formatter,
                "Android supervisor network {network_id} overlaps local address {local} with route {remote}"
            ),
            Self::PresentationRouteOverlap {
                network_id,
                presentation,
                remote,
            } => write!(
                formatter,
                "Android supervisor network {network_id} route {remote} overlaps presentation address {presentation}"
            ),
            Self::OverlappingNetworks {
                first_network,
                first_prefix,
                second_network,
                second_prefix,
            } => write!(
                formatter,
                "Android supervisor networks {first_network} ({first_prefix}) and {second_network} ({second_prefix}) overlap"
            ),
        }
    }
}

impl std::error::Error for SupervisorError {}

fn validate_network_id(network_id: &str) -> Result<(), SupervisorError> {
    if network_id.is_empty()
        || network_id.len() > MAX_NETWORK_ID_BYTES
        || network_id.chars().any(char::is_control)
    {
        return Err(SupervisorError::InvalidNetworkId);
    }
    Ok(())
}

fn primary_addresses(tun: &TunRuntimeConfig) -> PrimaryAddresses {
    PrimaryAddresses {
        ipv4: tun.addresses.ipv4,
        ipv6: tun.addresses.ipv6,
    }
}

#[derive(Clone, Copy)]
struct TranslationPolicy {
    ipv4: bool,
    ipv6: bool,
}

impl TranslationPolicy {
    fn new(networks: &[NetworkSpec], presentation: PrimaryAddresses) -> Self {
        Self {
            ipv4: networks
                .iter()
                .any(|network| network.tun.addresses.ipv4 != presentation.ipv4),
            ipv6: networks
                .iter()
                .any(|network| network.tun.addresses.ipv6 != presentation.ipv6),
        }
    }

    fn applies(self, address: IpAddr) -> bool {
        match address {
            IpAddr::V4(_) => self.ipv4,
            IpAddr::V6(_) => self.ipv6,
        }
    }

    fn applies_packet(self, packet: &[u8]) -> bool {
        match packet.first().map(|byte| byte >> 4) {
            Some(4) => self.ipv4,
            Some(6) => self.ipv6,
            _ => false,
        }
    }
}

fn validate_presentation(presentation: PrimaryAddresses) -> Result<(), SupervisorError> {
    let invalid_ipv4 = presentation.ipv4.is_unspecified()
        || presentation.ipv4.is_broadcast()
        || presentation.ipv4.is_loopback()
        || presentation.ipv4.is_multicast();
    let invalid_ipv6 = presentation.ipv6.is_unspecified()
        || presentation.ipv6.is_loopback()
        || presentation.ipv6.is_multicast();
    if invalid_ipv4 || invalid_ipv6 {
        return Err(SupervisorError::InvalidPresentationAddresses);
    }
    Ok(())
}

fn push_unique_bounded(
    network_id: &str,
    prefixes: &mut Vec<IpCidr>,
    other: &[IpCidr],
    prefix: IpCidr,
) -> Result<(), SupervisorError> {
    if prefixes.contains(&prefix) {
        return Ok(());
    }
    let actual = prefixes.len().saturating_add(other.len()).saturating_add(1);
    if actual > MAX_PREFIXES_PER_NETWORK {
        return Err(SupervisorError::TooManyNetworkPrefixes {
            network_id: network_id.to_owned(),
            actual,
        });
    }
    prefixes.push(prefix);
    Ok(())
}

fn packet_addresses(packet: &[u8]) -> Result<(IpAddr, IpAddr), PacketAddressError> {
    validate_packet_isolation(packet).map_err(|_| PacketAddressError::InvalidPacket)?;
    let Some(version) = packet.first().map(|byte| byte >> 4) else {
        return Err(PacketAddressError::TooShort);
    };
    match version {
        4 => {
            let header_length = usize::from(packet[0] & 0x0f) * 4;
            if header_length < 20 || packet.len() < header_length {
                return Err(PacketAddressError::TooShort);
            }
            let declared_length = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
            if declared_length < header_length || declared_length != packet.len() {
                return Err(PacketAddressError::InvalidLength);
            }
            Ok((
                IpAddr::V4(Ipv4Addr::new(
                    packet[12], packet[13], packet[14], packet[15],
                )),
                IpAddr::V4(Ipv4Addr::new(
                    packet[16], packet[17], packet[18], packet[19],
                )),
            ))
        }
        6 => {
            if packet.len() < 40 {
                return Err(PacketAddressError::TooShort);
            }
            let payload_length = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
            if payload_length.saturating_add(40) != packet.len() {
                return Err(PacketAddressError::InvalidLength);
            }
            let source: [u8; 16] = packet[8..24]
                .try_into()
                .expect("validated IPv6 source slice length");
            let destination: [u8; 16] = packet[24..40]
                .try_into()
                .expect("validated IPv6 destination slice length");
            Ok((
                IpAddr::V6(Ipv6Addr::from(source)),
                IpAddr::V6(Ipv6Addr::from(destination)),
            ))
        }
        _ => Err(PacketAddressError::UnsupportedVersion),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PacketAddressError {
    TooShort,
    InvalidLength,
    InvalidPacket,
    UnsupportedVersion,
}

struct PortReader {
    receiver: QueueReceiver,
}

impl PacketRead for PortReader {
    fn read_packet(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.receiver.pop_into(buffer)
    }
}

struct PortWriter {
    sender: QueueSender,
    wake: Arc<WriterWake>,
    metrics: Arc<NetworkMetrics>,
    mtu: usize,
    network_id: String,
    routes: DispatchRegistry,
}

impl PacketWrite for PortWriter {
    fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
        if packet.len() > self.mtu {
            self.metrics
                .inbound_oversized_drops
                .fetch_add(1, Ordering::Relaxed);
            return Ok(packet.len());
        }
        let validation = self.routes.validate_inbound(&self.network_id, packet);
        if validation != InboundValidation::Valid {
            record_inbound_validation_drop(&self.metrics, validation);
            if validation == InboundValidation::Removed {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "shared TUN packet switch network is removed",
                ));
            }
            return Ok(packet.len());
        }
        match self.sender.push(packet) {
            QueuePush::Queued => {
                self.metrics
                    .inbound_enqueued_packets
                    .fetch_add(1, Ordering::Relaxed);
                self.wake.notify();
            }
            QueuePush::Full => {
                self.metrics
                    .inbound_queue_drops
                    .fetch_add(1, Ordering::Relaxed);
            }
            QueuePush::Oversized => {
                self.metrics
                    .inbound_oversized_drops
                    .fetch_add(1, Ordering::Relaxed);
            }
            QueuePush::Closed => {
                self.metrics
                    .inbound_removed_drops
                    .fetch_add(1, Ordering::Relaxed);
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "shared TUN packet switch is closed",
                ));
            }
            QueuePush::ReservationMismatch => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "shared TUN queue reservation length changed",
                ));
            }
        }
        // Queue pressure is a packet drop, not a fatal failure for this network runtime.
        Ok(packet.len())
    }
}

struct OutboundPort {
    sender: QueueSender,
    mtu: usize,
    metrics: Arc<NetworkMetrics>,
    translator: PacketTranslator,
    translation_policy: TranslationPolicy,
}

struct InboundPort {
    id: String,
    receiver: QueueReceiver,
    metrics: Arc<NetworkMetrics>,
    active: Arc<Mutex<bool>>,
    routes: DispatchRegistry,
    translator: PacketTranslator,
    translation_policy: TranslationPolicy,
}

#[derive(Clone)]
struct NetworkControl {
    id: String,
    routes: DispatchRegistry,
    outbound: QueueSender,
    inbound: QueueReceiver,
    active: Arc<Mutex<bool>>,
    metrics: Arc<NetworkMetrics>,
    wake: Arc<WriterWake>,
}

impl NetworkControl {
    fn deactivate(&self) {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.deactivate_locked(active);
    }

    fn replace_routes(
        &self,
        installed: &TunRuntimeConfig,
        next: &TunRuntimeConfig,
    ) -> Result<(), SupervisorError> {
        let active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !*active {
            return Err(SupervisorError::UnknownNetwork(self.id.clone()));
        }
        match self.routes.replace(&self.id, installed, next) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.deactivate_locked(active);
                Err(error)
            }
        }
    }

    fn deactivate_locked(&self, mut active: std::sync::MutexGuard<'_, bool>) {
        *active = false;
        self.routes.remove(&self.id);
        let outbound_dropped = self.outbound.close_and_discard();
        self.metrics
            .outbound_removed_drops
            .fetch_add(outbound_dropped, Ordering::Relaxed);
        let inbound_dropped = self.inbound.close_and_discard();
        self.metrics
            .inbound_removed_drops
            .fetch_add(inbound_dropped, Ordering::Relaxed);
        drop(active);
        self.wake.notify();
    }
}

fn packet_queue(limits: QueueLimits) -> (QueueSender, QueueReceiver) {
    let queue = Arc::new(PacketQueue {
        limits,
        state: Mutex::new(PacketQueueState::default()),
        available: Condvar::new(),
    });
    (
        QueueSender {
            queue: Arc::clone(&queue),
        },
        QueueReceiver { queue },
    )
}

struct PacketQueue {
    limits: QueueLimits,
    state: Mutex<PacketQueueState>,
    available: Condvar,
}

#[derive(Default)]
struct PacketQueueState {
    packets: VecDeque<Vec<u8>>,
    bytes: usize,
    reserved_packets: usize,
    reserved_bytes: usize,
    closed: bool,
}

#[derive(Clone)]
struct QueueSender {
    queue: Arc<PacketQueue>,
}

impl QueueSender {
    fn reserve(&self, packet_len: usize) -> Result<QueueReservation, QueuePush> {
        if packet_len > self.queue.limits.max_bytes {
            return Err(QueuePush::Oversized);
        }
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return Err(QueuePush::Closed);
        }
        if state.packets.len().saturating_add(state.reserved_packets)
            >= self.queue.limits.max_packets
            || state
                .bytes
                .saturating_add(state.reserved_bytes)
                .saturating_add(packet_len)
                > self.queue.limits.max_bytes
        {
            return Err(QueuePush::Full);
        }
        state.reserved_packets += 1;
        state.reserved_bytes += packet_len;
        drop(state);
        Ok(QueueReservation {
            queue: Arc::clone(&self.queue),
            packet_len,
            active: true,
        })
    }

    fn push(&self, packet: &[u8]) -> QueuePush {
        if packet.len() > self.queue.limits.max_bytes {
            return QueuePush::Oversized;
        }
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.closed {
            return QueuePush::Closed;
        }
        if state.packets.len().saturating_add(state.reserved_packets)
            >= self.queue.limits.max_packets
            || state
                .bytes
                .saturating_add(state.reserved_bytes)
                .saturating_add(packet.len())
                > self.queue.limits.max_bytes
        {
            return QueuePush::Full;
        }
        state.bytes += packet.len();
        state.packets.push_back(packet.to_vec());
        drop(state);
        self.queue.available.notify_one();
        QueuePush::Queued
    }

    fn close_and_discard(&self) -> u64 {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.closed = true;
        let dropped = u64::try_from(state.packets.len()).unwrap_or(u64::MAX);
        state.packets.clear();
        state.bytes = 0;
        drop(state);
        self.queue.available.notify_all();
        dropped
    }
}

struct QueueReservation {
    queue: Arc<PacketQueue>,
    packet_len: usize,
    active: bool,
}

impl QueueReservation {
    fn commit(mut self, packet: Vec<u8>) -> QueuePush {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.reserved_packets = state.reserved_packets.saturating_sub(1);
        state.reserved_bytes = state.reserved_bytes.saturating_sub(self.packet_len);
        self.active = false;
        if packet.len() != self.packet_len {
            return QueuePush::ReservationMismatch;
        }
        if state.closed {
            return QueuePush::Closed;
        }
        if state.packets.len() >= self.queue.limits.max_packets
            || state.bytes.saturating_add(packet.len()) > self.queue.limits.max_bytes
        {
            return QueuePush::Full;
        }
        state.bytes += packet.len();
        state.packets.push_back(packet);
        drop(state);
        self.queue.available.notify_one();
        QueuePush::Queued
    }
}

impl Drop for QueueReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.reserved_packets = state.reserved_packets.saturating_sub(1);
        state.reserved_bytes = state.reserved_bytes.saturating_sub(self.packet_len);
    }
}

#[derive(Clone)]
struct QueueReceiver {
    queue: Arc<PacketQueue>,
}

impl QueueReceiver {
    fn try_pop(&self) -> Option<Vec<u8>> {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let packet = state.packets.pop_front()?;
        state.bytes = state.bytes.saturating_sub(packet.len());
        Some(packet)
    }

    fn pop_into(&self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut state = self
            .queue
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        loop {
            if let Some(packet) = state.packets.pop_front() {
                state.bytes = state.bytes.saturating_sub(packet.len());
                if packet.len() > buffer.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "dispatched packet exceeds the network runtime MTU",
                    ));
                }
                buffer[..packet.len()].copy_from_slice(&packet);
                return Ok(packet.len());
            }
            if state.closed {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "shared TUN packet switch closed",
                ));
            }
            state = self
                .queue
                .available
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    fn close_and_discard(&self) -> u64 {
        QueueSender {
            queue: Arc::clone(&self.queue),
        }
        .close_and_discard()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueuePush {
    Queued,
    Full,
    Oversized,
    Closed,
    ReservationMismatch,
}

#[derive(Default)]
struct WriterWake {
    generation: Mutex<u64>,
    available: Condvar,
}

impl WriterWake {
    fn notify(&self) {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *generation = generation.wrapping_add(1);
        drop(generation);
        self.available.notify_one();
    }

    fn generation(&self) -> u64 {
        *self
            .generation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn wait_since(&self, observed: u64, timeout: Duration) -> bool {
        let generation = self
            .generation
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let (generation, _) = self
            .available
            .wait_timeout_while(generation, timeout, |generation| *generation == observed)
            .unwrap_or_else(|error| error.into_inner());
        *generation != observed
    }
}

#[derive(Default)]
struct SwitchMetrics {
    malformed_outbound_packets: AtomicU64,
    unroutable_outbound_packets: AtomicU64,
    source_mismatch_outbound_packets: AtomicU64,
    networks: Mutex<BTreeMap<String, Arc<NetworkMetrics>>>,
}

impl SwitchMetrics {
    fn insert_network(&self, network_id: &str, metrics: Arc<NetworkMetrics>) {
        self.networks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(network_id.to_owned(), metrics);
    }

    fn snapshot(&self) -> SwitchSnapshot {
        let networks = self
            .networks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(id, metrics)| metrics.snapshot(id))
            .collect();
        SwitchSnapshot {
            malformed_outbound_packets: self.malformed_outbound_packets.load(Ordering::Relaxed),
            unroutable_outbound_packets: self.unroutable_outbound_packets.load(Ordering::Relaxed),
            source_mismatch_outbound_packets: self
                .source_mismatch_outbound_packets
                .load(Ordering::Relaxed),
            networks,
        }
    }
}

#[derive(Default)]
struct NetworkMetrics {
    outbound_enqueued_packets: AtomicU64,
    outbound_queue_drops: AtomicU64,
    outbound_oversized_drops: AtomicU64,
    outbound_source_mismatch_drops: AtomicU64,
    outbound_translation_drops: AtomicU64,
    outbound_removed_drops: AtomicU64,
    inbound_enqueued_packets: AtomicU64,
    inbound_queue_drops: AtomicU64,
    inbound_oversized_drops: AtomicU64,
    inbound_malformed_drops: AtomicU64,
    inbound_source_mismatch_drops: AtomicU64,
    inbound_destination_mismatch_drops: AtomicU64,
    inbound_translation_drops: AtomicU64,
    inbound_removed_drops: AtomicU64,
    inbound_written_packets: AtomicU64,
    inbound_write_backpressure_drops: AtomicU64,
    inbound_write_failures: AtomicU64,
    route_update_rejections: AtomicU64,
}

impl NetworkMetrics {
    fn snapshot(&self, id: &str) -> NetworkSnapshot {
        NetworkSnapshot {
            id: id.to_owned(),
            outbound_enqueued_packets: self.outbound_enqueued_packets.load(Ordering::Relaxed),
            outbound_queue_drops: self.outbound_queue_drops.load(Ordering::Relaxed),
            outbound_oversized_drops: self.outbound_oversized_drops.load(Ordering::Relaxed),
            outbound_source_mismatch_drops: self
                .outbound_source_mismatch_drops
                .load(Ordering::Relaxed),
            outbound_translation_drops: self.outbound_translation_drops.load(Ordering::Relaxed),
            outbound_removed_drops: self.outbound_removed_drops.load(Ordering::Relaxed),
            inbound_enqueued_packets: self.inbound_enqueued_packets.load(Ordering::Relaxed),
            inbound_queue_drops: self.inbound_queue_drops.load(Ordering::Relaxed),
            inbound_oversized_drops: self.inbound_oversized_drops.load(Ordering::Relaxed),
            inbound_malformed_drops: self.inbound_malformed_drops.load(Ordering::Relaxed),
            inbound_source_mismatch_drops: self
                .inbound_source_mismatch_drops
                .load(Ordering::Relaxed),
            inbound_destination_mismatch_drops: self
                .inbound_destination_mismatch_drops
                .load(Ordering::Relaxed),
            inbound_translation_drops: self.inbound_translation_drops.load(Ordering::Relaxed),
            inbound_removed_drops: self.inbound_removed_drops.load(Ordering::Relaxed),
            inbound_written_packets: self.inbound_written_packets.load(Ordering::Relaxed),
            inbound_write_backpressure_drops: self
                .inbound_write_backpressure_drops
                .load(Ordering::Relaxed),
            inbound_write_failures: self.inbound_write_failures.load(Ordering::Relaxed),
            route_update_rejections: self.route_update_rejections.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier, Mutex, mpsc},
        thread,
    };

    use p2p_vpn::{
        PeerId,
        route::Route,
        runtime::tun::{PacketWrite, TunAddresses},
    };

    use super::*;

    fn peer(seed: u8) -> PeerId {
        let mut bytes = [0_u8; 32];
        bytes[0] = seed;
        PeerId::from_bytes(bytes)
    }

    fn cidr(address: &str, prefix: u8) -> IpCidr {
        IpCidr::new(address.parse().expect("IP address"), prefix).expect("CIDR")
    }

    fn tun(local: u8, routes: &[(&str, u8)]) -> TunRuntimeConfig {
        TunRuntimeConfig {
            name: "pv0".to_owned(),
            mtu: 1280,
            addresses: TunAddresses::for_peer(peer(local)),
            additional_addresses: Vec::new(),
            routes: routes
                .iter()
                .enumerate()
                .map(|(index, (address, prefix))| Route {
                    owner: peer(u8::try_from(index + 100).expect("test peer")),
                    prefix: cidr(address, *prefix),
                    metric: 0,
                })
                .collect(),
        }
    }

    fn packet(source: [u8; 4], destination: [u8; 4]) -> Vec<u8> {
        packet_with_len(source, destination, 20)
    }

    fn packet_with_len(source: [u8; 4], destination: [u8; 4], length: usize) -> Vec<u8> {
        let mut packet = vec![0_u8; length];
        packet[0] = 0x45;
        packet[8] = 64;
        packet[9] = 59;
        packet[2..4].copy_from_slice(
            &u16::try_from(length)
                .expect("test packet length fits IPv4")
                .to_be_bytes(),
        );
        packet[12..16].copy_from_slice(&source);
        packet[16..20].copy_from_slice(&destination);
        write_test_ipv4_checksum(&mut packet);
        packet
    }

    fn write_test_ipv4_checksum(packet: &mut [u8]) {
        let header_len = usize::from(packet[0] & 0x0f) * 4;
        packet[10] = 0;
        packet[11] = 0;
        let mut sum = 0_u32;
        for word in packet[..header_len].chunks_exact(2) {
            sum += u32::from(u16::from_be_bytes([word[0], word[1]]));
        }
        while sum > u32::from(u16::MAX) {
            sum = (sum & u32::from(u16::MAX)) + (sum >> 16);
        }
        packet[10..12].copy_from_slice(&(!u16::try_from(sum).expect("checksum")).to_be_bytes());
    }

    fn ipv4_source_route_packet(
        source: [u8; 4],
        destination: [u8; 4],
        ultimate: [u8; 4],
        option: u8,
    ) -> Vec<u8> {
        let mut packet = vec![0_u8; 28];
        packet[0] = 0x47;
        packet[2..4].copy_from_slice(&28_u16.to_be_bytes());
        packet[8] = 64;
        packet[9] = 59;
        packet[12..16].copy_from_slice(&source);
        packet[16..20].copy_from_slice(&destination);
        packet[20] = option;
        packet[21] = 7;
        packet[22] = 4;
        packet[23..27].copy_from_slice(&ultimate);
        write_test_ipv4_checksum(&mut packet);
        packet
    }

    fn ipv6_packet(source: Ipv6Addr, destination: Ipv6Addr) -> Vec<u8> {
        let mut packet = vec![0_u8; 40];
        packet[0] = 0x60;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        packet
    }

    fn ipv6_routing_packet(source: Ipv6Addr, destination: Ipv6Addr, ultimate: Ipv6Addr) -> Vec<u8> {
        let mut packet = vec![0_u8; 64];
        packet[0] = 0x60;
        packet[4..6].copy_from_slice(&24_u16.to_be_bytes());
        packet[6] = 43;
        packet[7] = 64;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        packet[40] = 59;
        packet[41] = 2;
        packet[42] = 4;
        packet[43] = 1;
        packet[48..64].copy_from_slice(&ultimate.octets());
        packet
    }

    fn local(seed: u8) -> [u8; 4] {
        tun(seed, &[]).addresses.ipv4.octets()
    }

    fn presentation(ipv4: Ipv4Addr) -> PrimaryAddresses {
        PrimaryAddresses {
            ipv4,
            ipv6: "2001:db8::1".parse().expect("presentation IPv6"),
        }
    }

    #[test]
    fn dispatches_packets_to_the_network_owning_the_longest_prefix() {
        let (switch, ports) = PacketSwitch::new(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: tun(1, &[("10.10.0.0", 16), ("10.10.4.0", 24)]),
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: tun(2, &[("10.20.0.0", 16)]),
                },
            ],
            QueueLimits::DEFAULT,
        )
        .expect("switch");

        let routed = packet(local(1), [10, 10, 4, 9]);
        assert_eq!(
            switch.dispatch_packet(&routed),
            DispatchOutcome::Queued {
                network_id: "alpha".to_owned()
            }
        );

        let alpha = ports
            .into_iter()
            .find(|port| port.id == "alpha")
            .expect("alpha port");
        let (mut reader, _) = alpha.packet_io.split();
        let mut buffer = vec![0_u8; 1280];
        let length = reader.read_packet(&mut buffer).expect("queued packet");
        assert_eq!(&buffer[..length], routed);
    }

    #[test]
    fn presentation_addresses_translate_at_both_shared_tun_boundaries() {
        let presentation = presentation(Ipv4Addr::new(192, 0, 2, 1));
        let (switch, ports) = PacketSwitch::new_with_presentation(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: tun(1, &[("10.10.0.0", 16)]),
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: tun(2, &[("10.20.0.0", 16)]),
                },
            ],
            presentation,
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let beta = ports
            .into_iter()
            .find(|port| port.id == "beta")
            .expect("beta port");
        let (mut beta_reader, mut beta_writer) = beta.packet_io.split();

        let outbound = packet(presentation.ipv4.octets(), [10, 20, 1, 9]);
        assert_eq!(
            switch.dispatch_packet(&outbound),
            DispatchOutcome::Queued {
                network_id: "beta".to_owned()
            }
        );
        let mut buffer = vec![0_u8; 1280];
        let length = beta_reader
            .read_packet(&mut buffer)
            .expect("runtime packet");
        assert_eq!(&buffer[..length], packet(local(2), [10, 20, 1, 9]));

        let inbound = packet([10, 20, 1, 9], local(2));
        beta_writer
            .write_packet(&inbound)
            .expect("runtime inbound packet");
        let written = Arc::new(Mutex::new(Vec::new()));
        switch
            .write_next(&mut RecordingWriter(Arc::clone(&written)))
            .expect("physical TUN write");
        assert_eq!(
            *written.lock().expect("written packets"),
            vec![packet([10, 20, 1, 9], presentation.ipv4.octets())]
        );
    }

    #[test]
    fn translation_failures_drop_only_the_packet_and_are_counted() {
        let presentation = presentation(Ipv4Addr::new(192, 0, 2, 1));
        let (switch, mut ports) = PacketSwitch::new_with_presentation(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: tun(1, &[("10.10.0.0", 16)]),
            }],
            presentation,
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let mut unsupported = packet(presentation.ipv4.octets(), [10, 10, 1, 9]);
        unsupported[9] = 99;
        write_test_ipv4_checksum(&mut unsupported);

        assert_eq!(
            switch.dispatch_packet(&unsupported),
            DispatchOutcome::TranslationFailed {
                network_id: "alpha".to_owned()
            }
        );
        assert_eq!(switch.snapshot().networks[0].outbound_translation_drops, 1);
        assert_eq!(
            switch.dispatch_packet(&packet(presentation.ipv4.octets(), [10, 10, 1, 10])),
            DispatchOutcome::Queued {
                network_id: "alpha".to_owned()
            }
        );

        let (_, mut runtime_writer) = ports.pop().expect("alpha port").packet_io.split();
        let mut unsupported_inbound = packet([10, 10, 1, 9], local(1));
        unsupported_inbound[9] = 99;
        write_test_ipv4_checksum(&mut unsupported_inbound);
        runtime_writer
            .write_packet(&unsupported_inbound)
            .expect("unsupported inbound packet is queued");
        let written = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            switch
                .write_next(&mut RecordingWriter(Arc::clone(&written)))
                .expect("translation drop"),
            Some("alpha".to_owned())
        );
        assert!(written.lock().expect("written packets").is_empty());
        assert_eq!(switch.snapshot().networks[0].inbound_translation_drops, 1);

        runtime_writer
            .write_packet(&packet([10, 10, 1, 10], local(1)))
            .expect("valid inbound packet");
        switch
            .write_next(&mut RecordingWriter(Arc::clone(&written)))
            .expect("valid physical write");
        assert_eq!(written.lock().expect("written packets").len(), 1);
    }

    #[test]
    fn concurrent_translation_uses_one_protocol_policy_for_every_network() {
        let (switch, ports) = PacketSwitch::new(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: tun(1, &[("10.10.0.0", 16)]),
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: tun(2, &[("10.20.0.0", 16)]),
                },
            ],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let mut alpha = packet(local(1), [10, 10, 1, 9]);
        alpha[9] = 99;
        write_test_ipv4_checksum(&mut alpha);
        let mut beta = packet(local(1), [10, 20, 1, 9]);
        beta[9] = 99;
        write_test_ipv4_checksum(&mut beta);

        assert!(matches!(
            switch.dispatch_packet(&alpha),
            DispatchOutcome::TranslationFailed { network_id } if network_id == "alpha"
        ));
        assert!(matches!(
            switch.dispatch_packet(&beta),
            DispatchOutcome::TranslationFailed { network_id } if network_id == "beta"
        ));
        let mut writers = ports
            .into_iter()
            .map(|port| {
                let (_, writer) = port.packet_io.split();
                (port.id, writer)
            })
            .collect::<BTreeMap<_, _>>();
        for (id, source, destination) in [
            ("alpha", [10, 10, 1, 9], local(1)),
            ("beta", [10, 20, 1, 9], local(2)),
        ] {
            let mut inbound = packet(source, destination);
            inbound[9] = 99;
            write_test_ipv4_checksum(&mut inbound);
            writers
                .get_mut(id)
                .expect("network writer")
                .write_packet(&inbound)
                .expect("unsupported inbound packet is queued for policy validation");
        }
        let written = Arc::new(Mutex::new(Vec::new()));
        switch
            .write_next(&mut RecordingWriter(Arc::clone(&written)))
            .expect("first policy drop");
        switch
            .write_next(&mut RecordingWriter(Arc::clone(&written)))
            .expect("second policy drop");
        assert!(written.lock().expect("written packets").is_empty());
        let snapshot = switch.snapshot();
        assert!(
            snapshot
                .networks
                .iter()
                .all(|network| network.outbound_translation_drops == 1
                    && network.inbound_translation_drops == 1)
        );
    }

    #[test]
    fn single_identity_mapped_network_preserves_unsupported_protocol_compatibility() {
        let (switch, _) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: tun(1, &[("10.10.0.0", 16)]),
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let mut packet = packet(local(1), [10, 10, 1, 9]);
        packet[9] = 99;
        write_test_ipv4_checksum(&mut packet);

        assert_eq!(
            switch.dispatch_packet(&packet),
            DispatchOutcome::Queued {
                network_id: "alpha".to_owned()
            }
        );
    }

    #[test]
    fn source_routing_is_rejected_before_outbound_or_inbound_ownership() {
        let alpha = tun(1, &[("10.10.0.0", 16), ("fd10::", 64)]);
        let beta = tun(2, &[("10.20.0.0", 16), ("fd20::", 64)]);
        let alpha_v6 = alpha.addresses.ipv6;
        let (switch, ports) = PacketSwitch::new(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: alpha,
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: beta,
                },
            ],
            QueueLimits::DEFAULT,
        )
        .expect("switch");

        let loose = ipv4_source_route_packet(local(1), [10, 10, 1, 9], [10, 20, 1, 9], 131);
        assert_eq!(switch.dispatch_packet(&loose), DispatchOutcome::Malformed);
        let routing = ipv6_routing_packet(
            alpha_v6,
            "fd10::9".parse().expect("IPv6 destination"),
            "fd20::9".parse().expect("IPv6 ultimate destination"),
        );
        assert_eq!(switch.dispatch_packet(&routing), DispatchOutcome::Malformed);

        let alpha_port = ports
            .into_iter()
            .find(|port| port.id == "alpha")
            .expect("alpha port");
        let (_, mut writer) = alpha_port.packet_io.split();
        let strict = ipv4_source_route_packet([10, 10, 1, 9], local(1), local(2), 137);
        writer
            .write_packet(&strict)
            .expect("source-routed inbound packet is dropped");
        let alpha_metrics = switch
            .snapshot()
            .networks
            .into_iter()
            .find(|network| network.id == "alpha")
            .expect("alpha metrics");
        assert_eq!(alpha_metrics.inbound_malformed_drops, 1);
    }

    #[test]
    fn rejects_invalid_or_routed_presentation_addresses() {
        let network = NetworkSpec {
            id: "alpha".to_owned(),
            tun: tun(1, &[("10.10.0.0", 16)]),
        };
        assert!(matches!(
            PacketSwitch::new_with_presentation(
                vec![network.clone()],
                PrimaryAddresses {
                    ipv4: Ipv4Addr::UNSPECIFIED,
                    ipv6: "2001:db8::1".parse().expect("presentation IPv6"),
                },
                QueueLimits::DEFAULT,
            ),
            Err(SupervisorError::InvalidPresentationAddresses)
        ));
        assert!(matches!(
            PacketSwitch::new_with_presentation(
                vec![network],
                presentation(Ipv4Addr::new(10, 10, 1, 1)),
                QueueLimits::DEFAULT,
            ),
            Err(SupervisorError::PresentationRouteOverlap { .. })
        ));
    }

    #[test]
    fn rejects_overlapping_routes_before_activation() {
        let result = PacketSwitch::new(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: tun(1, &[("10.10.0.0", 16)]),
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: tun(2, &[("10.10.4.0", 24)]),
                },
            ],
            QueueLimits::DEFAULT,
        );

        assert!(matches!(
            result,
            Err(SupervisorError::OverlappingNetworks { .. })
        ));
    }

    #[test]
    fn rejects_a_network_exceeding_its_prefix_budget() {
        let mut oversized = tun(1, &[]);
        oversized.routes = (0..(MAX_PREFIXES_PER_NETWORK - 1))
            .map(|index| Route {
                owner: peer(99),
                prefix: cidr(
                    &format!(
                        "10.{}.{}.{}",
                        (index >> 16) & 0xff,
                        (index >> 8) & 0xff,
                        index & 0xff
                    ),
                    32,
                ),
                metric: 0,
            })
            .collect();

        assert!(matches!(
            PacketSwitch::new(
                vec![NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: oversized,
                }],
                QueueLimits::DEFAULT,
            ),
            Err(SupervisorError::TooManyNetworkPrefixes {
                actual,
                ..
            }) if actual == MAX_PREFIXES_PER_NETWORK + 1
        ));
    }

    #[test]
    fn rejects_a_route_overlapping_another_network_local_address() {
        let beta = tun(2, &[]);
        let beta_local = beta.addresses.ipv4.to_string();
        let result = PacketSwitch::new(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: tun(1, &[(beta_local.as_str(), 32)]),
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: beta,
                },
            ],
            QueueLimits::DEFAULT,
        );

        assert!(matches!(
            result,
            Err(SupervisorError::OverlappingNetworks { .. })
        ));
    }

    #[test]
    fn rejects_conflicting_live_route_update_and_fails_only_that_network_closed() {
        let alpha_tun = tun(1, &[("10.10.0.0", 16)]);
        let beta_tun = tun(2, &[("10.20.0.0", 16)]);
        let (switch, mut ports) = PacketSwitch::new(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: alpha_tun,
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: beta_tun.clone(),
                },
            ],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let mut beta_port = ports
            .drain(..)
            .find(|port| port.id == "beta")
            .expect("beta port");
        let (mut beta_reader, mut beta_writer) = beta_port.packet_io.split();
        let next = TunRuntimeConfig {
            routes: vec![Route {
                owner: peer(42),
                prefix: cidr("10.10.8.0", 24),
                metric: 0,
            }],
            ..beta_tun.clone()
        };
        let update = next
            .route_reconciliation_from(&beta_tun)
            .expect("route update");

        let result = beta_port
            .route_controller
            .reconcile(&beta_tun, &next, &update);

        assert!(matches!(
            result,
            Err(RunnerError::Tun(TunRuntimeError::NonAdditiveUpdate(
                ROUTE_UPDATE_REJECTED
            )))
        ));
        assert_eq!(
            switch.dispatch_packet(&packet(local(2), [10, 20, 1, 1])),
            DispatchOutcome::NoRoute
        );
        assert_eq!(
            switch.dispatch_packet(&packet(local(1), [10, 10, 8, 1])),
            DispatchOutcome::Queued {
                network_id: "alpha".to_owned()
            }
        );
        let mut buffer = [0_u8; 1280];
        assert!(matches!(
            beta_reader
                .read_packet(&mut buffer)
                .expect_err("failed-closed network reader"),
            TunRuntimeError::Io(error) if error.kind() == io::ErrorKind::UnexpectedEof
        ));
        assert!(matches!(
            beta_writer
                .write_packet(b"packet after failed route update")
                .expect_err("failed-closed network writer"),
            TunRuntimeError::Io(error) if error.kind() == io::ErrorKind::BrokenPipe
        ));
        let snapshot = switch.snapshot();
        let beta = snapshot
            .networks
            .iter()
            .find(|network| network.id == "beta")
            .expect("beta metrics");
        assert_eq!(beta.route_update_rejections, 1);
    }

    #[test]
    fn rejects_live_additional_address_changes_before_dispatch_state_changes() {
        let current = tun(1, &[("10.10.0.0", 16)]);
        let (switch, mut ports) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: current.clone(),
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let mut port = ports.pop().expect("alpha port");
        let mut next = current.clone();
        next.additional_addresses.push(cidr("192.0.2.9", 32));
        let unchanged_update = current
            .route_reconciliation_from(&current)
            .expect("empty route update");

        let result = port
            .route_controller
            .reconcile(&current, &next, &unchanged_update);

        assert!(matches!(
            result,
            Err(RunnerError::Tun(TunRuntimeError::NonAdditiveUpdate(
                ROUTE_UPDATE_REJECTED
            )))
        ));
        assert_eq!(
            switch.dispatch_packet(&packet(local(1), [10, 10, 1, 9])),
            DispatchOutcome::NoRoute
        );
        assert_eq!(switch.snapshot().networks[0].route_update_rejections, 1);
    }

    #[test]
    fn queue_pressure_is_bounded_and_isolated_per_network() {
        let limits = QueueLimits {
            max_packets: 1,
            max_bytes: 64,
        };
        let (switch, _) = PacketSwitch::new(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: tun(1, &[("10.10.0.0", 16)]),
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: tun(2, &[("10.20.0.0", 16)]),
                },
            ],
            limits,
        )
        .expect("switch");

        assert!(matches!(
            switch.dispatch_packet(&packet(local(1), [10, 10, 1, 1])),
            DispatchOutcome::Queued { .. }
        ));
        assert!(matches!(
            switch.dispatch_packet(&packet(local(1), [10, 10, 1, 2])),
            DispatchOutcome::QueueFull { .. }
        ));
        assert_eq!(
            switch.dispatch_packet(&packet(local(2), [10, 20, 1, 1])),
            DispatchOutcome::Queued {
                network_id: "beta".to_owned()
            }
        );

        let snapshot = switch.snapshot();
        let alpha = snapshot
            .networks
            .iter()
            .find(|network| network.id == "alpha")
            .expect("alpha metrics");
        assert_eq!(alpha.outbound_queue_drops, 1);
    }

    #[test]
    fn queue_reservations_exclude_concurrent_packet_and_byte_pushes() {
        fn assert_reserved_capacity_is_exclusive(limits: QueueLimits, reserved_len: usize) {
            let (sender, receiver) = packet_queue(limits);
            let reserved = Arc::new(Barrier::new(2));
            let release = Arc::new(Barrier::new(2));
            let worker_sender = sender.clone();
            let worker_reserved = Arc::clone(&reserved);
            let worker_release = Arc::clone(&release);
            let worker = thread::spawn(move || {
                let reservation = worker_sender.reserve(reserved_len).expect("reservation");
                worker_reserved.wait();
                worker_release.wait();
                reservation.commit(vec![0_u8; reserved_len])
            });

            reserved.wait();
            assert_eq!(sender.push(&[0]), QueuePush::Full);
            release.wait();
            assert_eq!(
                worker.join().expect("reservation worker"),
                QueuePush::Queued
            );
            assert_eq!(receiver.try_pop(), Some(vec![0_u8; reserved_len]));
        }

        assert_reserved_capacity_is_exclusive(
            QueueLimits {
                max_packets: 1,
                max_bytes: 64,
            },
            20,
        );
        assert_reserved_capacity_is_exclusive(
            QueueLimits {
                max_packets: 2,
                max_bytes: 20,
            },
            20,
        );

        let (sender, _) = packet_queue(QueueLimits {
            max_packets: 1,
            max_bytes: 20,
        });
        let reservation = sender.reserve(20).expect("reservation");
        assert_eq!(
            reservation.commit(vec![0_u8; 19]),
            QueuePush::ReservationMismatch
        );
        assert_eq!(sender.push(&[0]), QueuePush::Queued);
    }

    #[test]
    fn rejects_a_source_owned_by_another_network_without_poisoning_dispatch() {
        let (switch, _) = PacketSwitch::new_with_presentation(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: tun(1, &[("10.10.0.0", 16)]),
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: tun(2, &[("10.20.0.0", 16)]),
                },
            ],
            presentation(Ipv4Addr::new(192, 0, 2, 1)),
            QueueLimits::DEFAULT,
        )
        .expect("switch");

        assert_eq!(
            switch.dispatch_packet(&packet(local(1), [10, 20, 1, 1])),
            DispatchOutcome::SourceMismatch {
                network_id: "beta".to_owned()
            }
        );
        assert_eq!(
            switch.dispatch_packet(&packet(local(2), [10, 20, 1, 1])),
            DispatchOutcome::Queued {
                network_id: "beta".to_owned()
            }
        );
        let snapshot = switch.snapshot();
        assert_eq!(snapshot.source_mismatch_outbound_packets, 1);
        let beta = snapshot
            .networks
            .iter()
            .find(|network| network.id == "beta")
            .expect("beta metrics");
        assert_eq!(beta.outbound_source_mismatch_drops, 1);
    }

    #[test]
    fn oversized_packet_is_dropped_before_the_runtime_reader_and_next_packet_survives() {
        let mut alpha = tun(1, &[("10.10.0.0", 16)]);
        alpha.mtu = 64;
        let (switch, mut ports) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: alpha,
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let port = ports.pop().expect("network port");
        let (mut reader, mut writer) = port.packet_io.split();

        assert_eq!(
            switch.dispatch_packet(&packet_with_len(local(1), [10, 10, 1, 1], 65)),
            DispatchOutcome::Oversized {
                network_id: "alpha".to_owned()
            }
        );
        let valid = packet(local(1), [10, 10, 1, 1]);
        assert!(matches!(
            switch.dispatch_packet(&valid),
            DispatchOutcome::Queued { .. }
        ));
        let mut buffer = [0_u8; 64];
        let length = reader.read_packet(&mut buffer).expect("valid packet");
        assert_eq!(&buffer[..length], valid);
        writer
            .write_packet(&[0_u8; 65])
            .expect("oversized inbound packet is dropped");
        assert_eq!(
            switch
                .write_next(&mut RecordingWriter(Arc::new(Mutex::new(Vec::new()))))
                .expect("empty inbound queue"),
            None
        );
        let snapshot = switch.snapshot();
        assert_eq!(snapshot.networks[0].outbound_oversized_drops, 1);
        assert_eq!(snapshot.networks[0].inbound_oversized_drops, 1);
    }

    #[test]
    fn inbound_writes_are_fair_across_networks() {
        let (switch, ports) = PacketSwitch::new(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: tun(1, &[("10.10.0.0", 16)]),
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: tun(2, &[("10.20.0.0", 16)]),
                },
            ],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let mut writers = ports
            .into_iter()
            .map(|port| {
                let (_, writer) = port.packet_io.split();
                (port.id, writer)
            })
            .collect::<BTreeMap<_, _>>();
        let alpha_1 = packet([10, 10, 1, 1], local(1));
        let alpha_2 = packet([10, 10, 1, 2], local(1));
        let beta_1 = packet([10, 20, 1, 1], local(2));
        let beta_presented = packet([10, 20, 1, 1], local(1));
        writers
            .get_mut("alpha")
            .expect("alpha writer")
            .write_packet(&alpha_1)
            .expect("alpha packet");
        writers
            .get_mut("alpha")
            .expect("alpha writer")
            .write_packet(&alpha_2)
            .expect("alpha packet");
        writers
            .get_mut("beta")
            .expect("beta writer")
            .write_packet(&beta_1)
            .expect("beta packet");
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut writer = RecordingWriter(Arc::clone(&written));

        assert_eq!(
            switch.write_next(&mut writer).expect("first write"),
            Some("alpha".to_owned())
        );
        assert_eq!(
            switch.write_next(&mut writer).expect("second write"),
            Some("beta".to_owned())
        );
        assert_eq!(
            switch.write_next(&mut writer).expect("third write"),
            Some("alpha".to_owned())
        );
        assert_eq!(
            *written.lock().expect("recorded packets"),
            vec![alpha_1, beta_presented, alpha_2]
        );
    }

    #[test]
    fn inbound_packets_are_validated_against_the_network_boundary() {
        let (switch, mut ports) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: tun(1, &[("10.10.0.0", 16)]),
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let (_, mut writer) = ports.pop().expect("network port").packet_io.split();
        let valid = packet([10, 10, 1, 1], local(1));
        let mut malformed = valid.clone();
        malformed[2..4].copy_from_slice(&21_u16.to_be_bytes());
        let wrong_source = packet([192, 0, 2, 1], local(1));
        let wrong_destination = packet([10, 10, 1, 1], [192, 0, 2, 2]);

        writer.write_packet(&valid).expect("valid inbound packet");
        writer
            .write_packet(&malformed)
            .expect("malformed inbound packet is dropped");
        writer
            .write_packet(&wrong_source)
            .expect("foreign inbound source is dropped");
        writer
            .write_packet(&wrong_destination)
            .expect("foreign inbound destination is dropped");

        let written = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            switch
                .write_next(&mut RecordingWriter(Arc::clone(&written)))
                .expect("valid write"),
            Some("alpha".to_owned())
        );
        assert_eq!(*written.lock().expect("written packets"), vec![valid]);
        assert_eq!(
            switch
                .write_next(&mut RecordingWriter(Arc::clone(&written)))
                .expect("invalid packets were not queued"),
            None
        );
        let network = &switch.snapshot().networks[0];
        assert_eq!(network.inbound_malformed_drops, 1);
        assert_eq!(network.inbound_source_mismatch_drops, 1);
        assert_eq!(network.inbound_destination_mismatch_drops, 1);
    }

    #[test]
    fn physical_tun_backpressure_drops_one_packet_without_failing_the_switch() {
        let (switch, mut ports) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: tun(1, &[("10.10.0.0", 16)]),
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let (_, mut writer) = ports.pop().expect("network port").packet_io.split();
        writer
            .write_packet(&packet([10, 10, 1, 1], local(1)))
            .expect("inbound packet");

        assert_eq!(
            switch
                .write_next(&mut WouldBlockWriter)
                .expect("backpressure is a bounded drop"),
            Some("alpha".to_owned())
        );
        assert_eq!(
            switch.snapshot().networks[0].inbound_write_backpressure_drops,
            1
        );
    }

    #[test]
    fn route_commit_waits_for_an_in_flight_physical_tun_write() {
        let current = tun(1, &[("10.10.0.0", 16)]);
        let next = TunRuntimeConfig {
            routes: vec![Route {
                owner: peer(42),
                prefix: cidr("10.11.0.0", 16),
                metric: 0,
            }],
            ..current.clone()
        };
        let update = next
            .route_reconciliation_from(&current)
            .expect("route update");
        let (switch, mut ports) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: current.clone(),
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let port = ports.pop().expect("network port");
        let (_, mut runtime_writer) = port.packet_io.split();
        runtime_writer
            .write_packet(&packet([10, 10, 1, 1], local(1)))
            .expect("inbound packet");

        let switch = Arc::new(switch);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer_switch = Arc::clone(&switch);
        let writer_thread = thread::spawn(move || {
            writer_switch.write_next(&mut GateWriter {
                entered: entered_tx,
                release: release_rx,
            })
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("physical writer entered");

        let (update_started_tx, update_started_rx) = mpsc::channel();
        let (update_done_tx, update_done_rx) = mpsc::channel();
        let mut route_controller = port.route_controller;
        let update_thread = thread::spawn(move || {
            update_started_tx.send(()).expect("update started signal");
            let result = route_controller.reconcile(&current, &next, &update);
            update_done_tx.send(result).expect("update result signal");
        });
        update_started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("route update started");
        let premature = update_done_rx.recv_timeout(Duration::from_millis(100));
        let completed_early = premature.is_ok();

        release_tx.send(()).expect("release physical writer");
        assert!(writer_thread.join().expect("writer thread").is_ok());
        let update_result = match premature {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => update_done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("route update completed after write"),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("route update result channel disconnected")
            }
        };
        update_thread.join().expect("route update thread");

        assert!(
            !completed_early,
            "route update completed while a physical write was in flight"
        );
        update_result.expect("route update");
        assert_eq!(
            switch.dispatch_packet(&packet(local(1), [10, 10, 1, 1])),
            DispatchOutcome::NoRoute
        );
        assert!(matches!(
            switch.dispatch_packet(&packet(local(1), [10, 11, 1, 1])),
            DispatchOutcome::Queued { .. }
        ));
    }

    #[test]
    fn queued_inbound_packet_is_revalidated_after_a_route_change() {
        let current = tun(1, &[("10.10.0.0", 16)]);
        let next = TunRuntimeConfig {
            routes: vec![Route {
                owner: peer(42),
                prefix: cidr("10.11.0.0", 16),
                metric: 0,
            }],
            ..current.clone()
        };
        let update = next
            .route_reconciliation_from(&current)
            .expect("route update");
        let (switch, mut ports) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: current.clone(),
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let mut port = ports.pop().expect("network port");
        let (_, mut runtime_writer) = port.packet_io.split();
        runtime_writer
            .write_packet(&packet([10, 10, 1, 1], local(1)))
            .expect("packet valid under old routes");

        port.route_controller
            .reconcile(&current, &next, &update)
            .expect("route update");

        let written = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            switch
                .write_next(&mut RecordingWriter(Arc::clone(&written)))
                .expect("stale packet is consumed as a drop"),
            Some("alpha".to_owned())
        );
        assert!(written.lock().expect("written packets").is_empty());
        assert_eq!(
            switch.snapshot().networks[0].inbound_source_mismatch_drops,
            1
        );

        let current_packet = packet([10, 11, 1, 1], local(1));
        runtime_writer
            .write_packet(&current_packet)
            .expect("packet valid under new routes");
        assert_eq!(
            switch
                .write_next(&mut RecordingWriter(Arc::clone(&written)))
                .expect("current packet write"),
            Some("alpha".to_owned())
        );
        assert_eq!(
            *written.lock().expect("written packets"),
            vec![current_packet]
        );
    }

    #[test]
    fn malformed_and_unroutable_packets_are_dropped_without_queueing() {
        let (switch, _) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: tun(1, &[("10.10.0.0", 16)]),
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");

        assert_eq!(switch.dispatch_packet(&[]), DispatchOutcome::Malformed);
        let mut truncated_ipv4_header = packet(local(1), [10, 10, 1, 1]);
        truncated_ipv4_header[0] = 0x4f;
        assert_eq!(
            switch.dispatch_packet(&truncated_ipv4_header),
            DispatchOutcome::Malformed
        );
        let mut invalid_ipv4_length = packet(local(1), [10, 10, 1, 1]);
        invalid_ipv4_length[2..4].copy_from_slice(&21_u16.to_be_bytes());
        assert_eq!(
            switch.dispatch_packet(&invalid_ipv4_length),
            DispatchOutcome::Malformed
        );
        let mut invalid_ipv6_length = ipv6_packet(
            tun(1, &[]).addresses.ipv6,
            "fd00::2".parse().expect("IPv6 destination"),
        );
        invalid_ipv6_length[4..6].copy_from_slice(&1_u16.to_be_bytes());
        assert_eq!(
            switch.dispatch_packet(&invalid_ipv6_length),
            DispatchOutcome::Malformed
        );
        assert_eq!(
            switch.dispatch_packet(&packet(local(1), [192, 0, 2, 1])),
            DispatchOutcome::NoRoute
        );
        assert_eq!(
            switch.snapshot(),
            SwitchSnapshot {
                malformed_outbound_packets: 4,
                unroutable_outbound_packets: 1,
                source_mismatch_outbound_packets: 0,
                networks: vec![NetworkSnapshot {
                    id: "alpha".to_owned(),
                    ..NetworkSnapshot::default()
                }],
            }
        );
    }

    #[test]
    fn network_removal_closes_its_port_and_removes_its_routes() {
        let (switch, mut ports) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: tun(1, &[("10.10.0.0", 16)]),
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let port = ports.pop().expect("network port");
        let (mut reader, mut writer) = port.packet_io.split();

        assert!(matches!(
            switch.dispatch_packet(&packet(local(1), [10, 10, 1, 1])),
            DispatchOutcome::Queued { .. }
        ));
        writer
            .write_packet(&packet([10, 10, 1, 1], local(1)))
            .expect("inbound packet");

        switch.remove_network("alpha");

        assert_eq!(
            switch.dispatch_packet(&packet(local(1), [10, 10, 1, 1])),
            DispatchOutcome::NoRoute
        );
        let mut buffer = vec![0_u8; 1280];
        assert!(matches!(
            reader
                .read_packet(&mut buffer)
                .expect_err("closed network port"),
            TunRuntimeError::Io(error) if error.kind() == io::ErrorKind::UnexpectedEof
        ));
        let written = Arc::new(Mutex::new(Vec::new()));
        assert_eq!(
            switch
                .write_next(&mut RecordingWriter(Arc::clone(&written)))
                .expect("no stale write"),
            None
        );
        assert!(written.lock().expect("written packets").is_empty());
        assert!(matches!(
            writer
                .write_packet(b"inbound after removal")
                .expect_err("removed network rejects inbound packets"),
            TunRuntimeError::Io(error) if error.kind() == io::ErrorKind::BrokenPipe
        ));
        let snapshot = switch.snapshot();
        assert_eq!(snapshot.networks[0].outbound_removed_drops, 1);
        assert_eq!(snapshot.networks[0].inbound_removed_drops, 2);
        switch.close();
    }

    #[test]
    fn dropping_a_network_lease_removes_only_that_network() {
        let (switch, _) = PacketSwitch::new(
            vec![
                NetworkSpec {
                    id: "alpha".to_owned(),
                    tun: tun(1, &[("10.10.0.0", 16)]),
                },
                NetworkSpec {
                    id: "beta".to_owned(),
                    tun: tun(2, &[("10.20.0.0", 16)]),
                },
            ],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let switch = Arc::new(switch);
        let lease = switch.network_lease("alpha").expect("network lease");

        drop(lease);

        assert_eq!(
            switch.dispatch_packet(&packet(local(1), [10, 10, 1, 1])),
            DispatchOutcome::NoRoute
        );
        assert!(matches!(
            switch.dispatch_packet(&packet(local(2), [10, 20, 1, 1])),
            DispatchOutcome::Queued { .. }
        ));
    }

    #[test]
    fn writer_wake_generation_prevents_a_lost_notification() {
        let (switch, mut ports) = PacketSwitch::new(
            vec![NetworkSpec {
                id: "alpha".to_owned(),
                tun: tun(1, &[("10.10.0.0", 16)]),
            }],
            QueueLimits::DEFAULT,
        )
        .expect("switch");
        let (_, mut writer) = ports.pop().expect("network port").packet_io.split();
        let generation = switch.inbound_generation();

        writer
            .write_packet(&packet([10, 10, 1, 1], local(1)))
            .expect("inbound packet");

        assert!(switch.wait_for_inbound_since(generation, Duration::from_secs(1)));
    }

    struct RecordingWriter(Arc<Mutex<Vec<Vec<u8>>>>);

    impl PacketWrite for RecordingWriter {
        fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("recording writer")
                .push(packet.to_vec());
            Ok(packet.len())
        }
    }

    struct WouldBlockWriter;

    impl PacketWrite for WouldBlockWriter {
        fn write_packet(&mut self, _packet: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "test TUN is congested",
            ))
        }
    }

    struct GateWriter {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl PacketWrite for GateWriter {
        fn write_packet(&mut self, packet: &[u8]) -> io::Result<usize> {
            self.entered.send(()).map_err(io::Error::other)?;
            self.release
                .recv_timeout(Duration::from_secs(2))
                .map_err(io::Error::other)?;
            Ok(packet.len())
        }
    }
}
