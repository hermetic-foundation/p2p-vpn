use std::{
    fmt, io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use hickory_proto::{
    op::{Edns, Message, MessageType, OpCode, ResponseCode},
    rr::{
        DNSClass, Name, RData, Record, RecordType,
        rdata::{A, AAAA, PTR, SOA},
    },
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{Semaphore, watch},
    task::{JoinHandle, JoinSet},
};

use crate::{
    config::Config,
    dns::{DnsZone, DnsZoneError},
    membership::SignedMembershipRecord,
};

pub const MAX_DNS_REQUEST_BYTES: usize = 4_096;
pub const MAX_DNS_UDP_RESPONSE_BYTES: usize = 1_232;
pub const MAX_DNS_TCP_RESPONSE_BYTES: usize = 65_535;
pub const MAX_DNS_TCP_CONNECTIONS: usize = 64;
pub const MAX_DNS_TCP_QUERIES_PER_CONNECTION: usize = 32;
pub const DNS_TCP_IO_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_DNS_CONTROL_LIST_LIMIT: usize = 256;
const DNS_ZONE_REFRESH_RETRY_SECONDS: u64 = 5;
const DNS_CONTROL_ADDRESS_PREVIEW: usize = 8;
const DNS_CONTROL_PEER_PREVIEW: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnsLookupType {
    Auto,
    A,
    Aaaa,
    Ptr,
    Any,
}

impl DnsLookupType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "AUTO",
            Self::A => "A",
            Self::Aaaa => "AAAA",
            Self::Ptr => "PTR",
            Self::Any => "ANY",
        }
    }
}

#[derive(Debug)]
pub struct DnsRuntime {
    listener: SocketAddr,
    zone_tx: watch::Sender<Arc<DnsZone>>,
    shutdown_tx: watch::Sender<bool>,
    metrics: Arc<DnsRuntimeMetrics>,
    last_refresh_attempt_unix_seconds: AtomicU64,
    refresh_failed: AtomicBool,
    tasks: Vec<JoinHandle<()>>,
}

impl DnsRuntime {
    pub async fn bind_at(
        config: &Config,
        member_records: &[SignedMembershipRecord],
        now_unix_seconds: u64,
    ) -> Result<Option<Self>, DnsRuntimeError> {
        Self::bind_with_hostname_records_at(
            config,
            member_records,
            &std::collections::HashMap::new(),
            now_unix_seconds,
        )
        .await
    }

    pub async fn bind_with_hostname_records_at(
        config: &Config,
        member_records: &[SignedMembershipRecord],
        hostname_records: &std::collections::HashMap<crate::PeerId, String>,
        now_unix_seconds: u64,
    ) -> Result<Option<Self>, DnsRuntimeError> {
        if !config.network.dns.enabled {
            return Ok(None);
        }

        let zone = Arc::new(DnsZone::from_config_with_hostname_records_at(
            config,
            member_records,
            hostname_records,
            now_unix_seconds,
        )?);
        Self::bind_zone(config.network.dns.listen, zone)
            .await
            .map(Some)
    }

    pub async fn bind_reserved_suffix(listener: SocketAddr) -> Result<Self, DnsRuntimeError> {
        if !listener.ip().is_loopback() {
            return Err(DnsRuntimeError::NonLoopbackListener(listener));
        }
        Self::bind_zone(listener, Arc::new(DnsZone::reserved_suffix_guard())).await
    }

    async fn bind_zone(
        requested_listener: SocketAddr,
        zone: Arc<DnsZone>,
    ) -> Result<Self, DnsRuntimeError> {
        let udp = UdpSocket::bind(requested_listener).await?;
        let listener = udp.local_addr()?;
        let tcp = TcpListener::bind(listener).await?;
        let (zone_tx, zone_rx) = watch::channel(zone);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let metrics = Arc::new(DnsRuntimeMetrics::default());
        let tasks = vec![
            tokio::spawn(run_udp_server(
                udp,
                zone_rx.clone(),
                shutdown_rx.clone(),
                Arc::clone(&metrics),
            )),
            tokio::spawn(run_tcp_server(
                tcp,
                zone_rx,
                shutdown_rx,
                Arc::clone(&metrics),
            )),
        ];

        Ok(Self {
            listener,
            zone_tx,
            shutdown_tx,
            metrics,
            last_refresh_attempt_unix_seconds: AtomicU64::new(0),
            refresh_failed: AtomicBool::new(false),
            tasks,
        })
    }

    #[must_use]
    pub const fn listener(&self) -> SocketAddr {
        self.listener
    }

    #[must_use]
    pub fn zone(&self) -> Arc<DnsZone> {
        Arc::clone(&self.zone_tx.borrow())
    }

    #[must_use]
    pub fn refresh_due(&self, now_unix_seconds: u64) -> bool {
        let last_attempt = self
            .last_refresh_attempt_unix_seconds
            .load(Ordering::Relaxed);
        if self.refresh_failed.load(Ordering::Relaxed) {
            return now_unix_seconds >= last_attempt.saturating_add(DNS_ZONE_REFRESH_RETRY_SECONDS);
        }
        self.zone()
            .next_refresh_unix_seconds()
            .is_some_and(|expires_at| expires_at <= now_unix_seconds && last_attempt < expires_at)
    }

    pub fn refresh_at(
        &self,
        config: &Config,
        member_records: &[SignedMembershipRecord],
        now_unix_seconds: u64,
    ) -> Result<(), DnsRuntimeError> {
        self.refresh_with_hostname_records_at(
            config,
            member_records,
            &std::collections::HashMap::new(),
            now_unix_seconds,
        )
    }

    pub fn refresh_with_hostname_records_at(
        &self,
        config: &Config,
        member_records: &[SignedMembershipRecord],
        hostname_records: &std::collections::HashMap<crate::PeerId, String>,
        now_unix_seconds: u64,
    ) -> Result<(), DnsRuntimeError> {
        self.last_refresh_attempt_unix_seconds
            .store(now_unix_seconds, Ordering::Relaxed);
        let zone = match DnsZone::from_config_with_hostname_records_at(
            config,
            member_records,
            hostname_records,
            now_unix_seconds,
        ) {
            Ok(zone) => zone,
            Err(error) => {
                self.metrics
                    .zone_refresh_failures
                    .fetch_add(1, Ordering::Relaxed);
                let fail_closed = self.zone().fail_closed();
                self.zone_tx.send_replace(Arc::new(fail_closed));
                self.refresh_failed.store(true, Ordering::Relaxed);
                return Err(error.into());
            }
        };
        self.zone_tx.send_replace(Arc::new(zone));
        self.refresh_failed.store(false, Ordering::Relaxed);
        self.metrics.zone_refreshes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> DnsRuntimeSnapshot {
        let zone = self.zone();
        DnsRuntimeSnapshot {
            listener: self.listener,
            zone: zone.name().to_owned(),
            ttl_seconds: zone.ttl_seconds(),
            record_sets: zone.records().count(),
            reverse_records: zone.reverse_records().count(),
            conflicts: zone.conflicts().count(),
            udp_queries: self.metrics.udp_queries.load(Ordering::Relaxed),
            tcp_queries: self.metrics.tcp_queries.load(Ordering::Relaxed),
            responses: self.metrics.responses.load(Ordering::Relaxed),
            format_errors: self.metrics.format_errors.load(Ordering::Relaxed),
            refused: self.metrics.refused.load(Ordering::Relaxed),
            nxdomain: self.metrics.nxdomain.load(Ordering::Relaxed),
            truncated: self.metrics.truncated.load(Ordering::Relaxed),
            oversized_requests: self.metrics.oversized_requests.load(Ordering::Relaxed),
            tcp_connections_rejected: self
                .metrics
                .tcp_connections_rejected
                .load(Ordering::Relaxed),
            io_errors: self.metrics.io_errors.load(Ordering::Relaxed),
            zone_refreshes: self.metrics.zone_refreshes.load(Ordering::Relaxed),
            zone_refresh_failures: self.metrics.zone_refresh_failures.load(Ordering::Relaxed),
            degraded: self.refresh_failed.load(Ordering::Relaxed),
        }
    }

    #[must_use]
    pub fn status_lines(&self) -> Vec<String> {
        let snapshot = self.snapshot();
        vec![
            format!(
                "dns enabled=true listener={} zone={} ttl_seconds={}",
                snapshot.listener, snapshot.zone, snapshot.ttl_seconds
            ),
            format!(
                "dns_records record_sets={} reverse_records={} conflicts={}",
                snapshot.record_sets, snapshot.reverse_records, snapshot.conflicts
            ),
            format!(
                "dns_queries udp={} tcp={} responses={} format_errors={} refused={} nxdomain={} truncated={} oversized_requests={} tcp_connections_rejected={} io_errors={}",
                snapshot.udp_queries,
                snapshot.tcp_queries,
                snapshot.responses,
                snapshot.format_errors,
                snapshot.refused,
                snapshot.nxdomain,
                snapshot.truncated,
                snapshot.oversized_requests,
                snapshot.tcp_connections_rejected,
                snapshot.io_errors,
            ),
            format!(
                "dns_refresh successful={} failed={} degraded={}",
                snapshot.zone_refreshes, snapshot.zone_refresh_failures, snapshot.degraded
            ),
        ]
    }

    #[must_use]
    pub fn list_lines(&self, offset: usize, limit: usize) -> Vec<String> {
        let zone = self.zone();
        let mut entries = Vec::new();
        for record in zone.records() {
            let sources = record
                .sources
                .iter()
                .map(|source| source.as_str())
                .collect::<Vec<_>>()
                .join(",");
            entries.push(format!(
                "dns_record name={} peer={} transport_peer={} ipv4={} ipv4_total={} ipv6={} ipv6_total={} fallback={} sources={}",
                record.fqdn,
                record.peer,
                record.transport_peer,
                preview_values(&record.ipv4, DNS_CONTROL_ADDRESS_PREVIEW),
                record.ipv4.len(),
                preview_values(&record.ipv6, DNS_CONTROL_ADDRESS_PREVIEW),
                record.ipv6.len(),
                record.fallback,
                sources,
            ));
        }
        for (owner, target) in zone.reverse_records() {
            entries.push(format!("dns_ptr name={owner} target={target}"));
        }
        for conflict in zone.conflicts() {
            entries.push(format!(
                "dns_conflict name={} peers={} peers_total={}",
                conflict.fqdn,
                preview_values(&conflict.peers, DNS_CONTROL_PEER_PREVIEW),
                conflict.peers.len(),
            ));
        }

        let total = entries.len();
        let limit = limit.clamp(1, MAX_DNS_CONTROL_LIST_LIMIT);
        let page = entries
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>();
        let returned = page.len();
        let mut lines = vec![format!(
            "dns_list offset={offset} limit={limit} returned={returned} total={total} more={}",
            offset.saturating_add(returned) < total
        )];
        lines.extend(page);
        lines
    }

    #[must_use]
    pub fn resolve_lines(&self, input: &str, lookup_type: DnsLookupType) -> Vec<String> {
        let zone = self.zone();
        let (name, lookup_type) = match control_lookup(&zone, input, lookup_type) {
            Ok(lookup) => lookup,
            Err(reason) => {
                return vec![format!(
                    "dns_resolution query={} type={} status=invalid reason={reason}",
                    sanitize_control_value(input),
                    lookup_type.as_str(),
                )];
            }
        };

        if lookup_type == DnsLookupType::Ptr {
            return match zone.reverse_target_name(&name) {
                Some(target) => vec![format!(
                    "dns_resolution query={} name={} type=PTR status=ok values={target}",
                    sanitize_control_value(input),
                    name,
                )],
                None => vec![format!(
                    "dns_resolution query={} name={} type=PTR status=nxdomain values=-",
                    sanitize_control_value(input),
                    name,
                )],
            };
        }

        if !name_is_in_zone(&name, zone.name()) {
            return vec![format!(
                "dns_resolution query={} name={} type={} status=refused values=-",
                sanitize_control_value(input),
                name,
                lookup_type.as_str(),
            )];
        }
        if let Some(conflict) = zone.conflict(&name) {
            return vec![format!(
                "dns_resolution query={} name={} type={} status=conflict values=- peers={} peers_total={}",
                sanitize_control_value(input),
                name,
                lookup_type.as_str(),
                preview_values(&conflict.peers, DNS_CONTROL_PEER_PREVIEW),
                conflict.peers.len(),
            )];
        }
        let Some(record) = zone.record(&name) else {
            return vec![format!(
                "dns_resolution query={} name={} type={} status=nxdomain values=-",
                sanitize_control_value(input),
                name,
                lookup_type.as_str(),
            )];
        };

        let values = match lookup_type {
            DnsLookupType::A => record
                .ipv4
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            DnsLookupType::Aaaa => record
                .ipv6
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            DnsLookupType::Auto | DnsLookupType::Any => record
                .ipv4
                .iter()
                .map(|address| format!("A:{address}"))
                .chain(record.ipv6.iter().map(|address| format!("AAAA:{address}")))
                .collect::<Vec<_>>(),
            DnsLookupType::Ptr => unreachable!("PTR lookups returned above"),
        };
        let status = if values.is_empty() { "nodata" } else { "ok" };
        let values = if values.is_empty() {
            "-".to_owned()
        } else {
            values.join(",")
        };
        vec![format!(
            "dns_resolution query={} name={} type={} status={status} values={values}",
            sanitize_control_value(input),
            name,
            lookup_type.as_str(),
        )]
    }
}

impl Drop for DnsRuntime {
    fn drop(&mut self) {
        self.shutdown_tx.send_replace(true);
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DnsRuntimeSnapshot {
    pub listener: SocketAddr,
    pub zone: String,
    pub ttl_seconds: u32,
    pub record_sets: usize,
    pub reverse_records: usize,
    pub conflicts: usize,
    pub udp_queries: u64,
    pub tcp_queries: u64,
    pub responses: u64,
    pub format_errors: u64,
    pub refused: u64,
    pub nxdomain: u64,
    pub truncated: u64,
    pub oversized_requests: u64,
    pub tcp_connections_rejected: u64,
    pub io_errors: u64,
    pub zone_refreshes: u64,
    pub zone_refresh_failures: u64,
    pub degraded: bool,
}

#[derive(Debug)]
pub enum DnsRuntimeError {
    Io(io::Error),
    Zone(DnsZoneError),
    NonLoopbackListener(SocketAddr),
}

impl fmt::Display for DnsRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "DNS listener failed: {error}"),
            Self::Zone(error) => write!(formatter, "DNS zone generation failed: {error:?}"),
            Self::NonLoopbackListener(listener) => {
                write!(formatter, "DNS listener must be loopback-only: {listener}")
            }
        }
    }
}

impl std::error::Error for DnsRuntimeError {}

impl From<io::Error> for DnsRuntimeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DnsZoneError> for DnsRuntimeError {
    fn from(error: DnsZoneError) -> Self {
        Self::Zone(error)
    }
}

#[derive(Debug, Default)]
struct DnsRuntimeMetrics {
    udp_queries: AtomicU64,
    tcp_queries: AtomicU64,
    responses: AtomicU64,
    format_errors: AtomicU64,
    refused: AtomicU64,
    nxdomain: AtomicU64,
    truncated: AtomicU64,
    oversized_requests: AtomicU64,
    tcp_connections_rejected: AtomicU64,
    io_errors: AtomicU64,
    zone_refreshes: AtomicU64,
    zone_refresh_failures: AtomicU64,
}

struct EncodedDnsResponse {
    bytes: Vec<u8>,
    udp_payload_limit: usize,
}

async fn run_udp_server(
    socket: UdpSocket,
    zone_rx: watch::Receiver<Arc<DnsZone>>,
    mut shutdown_rx: watch::Receiver<bool>,
    metrics: Arc<DnsRuntimeMetrics>,
) {
    let mut buffer = vec![0_u8; MAX_DNS_REQUEST_BYTES + 1];
    loop {
        let received = tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break;
                }
                continue;
            }
            received = socket.recv_from(&mut buffer) => received,
        };
        let (length, source) = match received {
            Ok(received) => received,
            Err(_) => {
                metrics.io_errors.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        metrics.udp_queries.fetch_add(1, Ordering::Relaxed);
        let response = if length > MAX_DNS_REQUEST_BYTES {
            metrics.oversized_requests.fetch_add(1, Ordering::Relaxed);
            encode_error_response(&buffer[..length], ResponseCode::FormErr)
        } else {
            encode_response(&buffer[..length], &zone_rx.borrow(), &metrics)
        };
        let Some(response) = response else {
            continue;
        };
        let mut response_bytes = response.bytes;
        if response_bytes.len() > response.udp_payload_limit {
            response_bytes = truncate_response(&response_bytes, &metrics);
        }
        match socket.send_to(&response_bytes, source).await {
            Ok(_) => {
                metrics.responses.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                metrics.io_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

async fn run_tcp_server(
    listener: TcpListener,
    zone_rx: watch::Receiver<Arc<DnsZone>>,
    mut shutdown_rx: watch::Receiver<bool>,
    metrics: Arc<DnsRuntimeMetrics>,
) {
    let permits = Arc::new(Semaphore::new(MAX_DNS_TCP_CONNECTIONS));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(accepted) => accepted,
                    Err(_) => {
                        metrics.io_errors.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                };
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    metrics.tcp_connections_rejected.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                connections.spawn(handle_tcp_connection(
                    stream,
                    zone_rx.clone(),
                    shutdown_rx.clone(),
                    Arc::clone(&metrics),
                    permit,
                ));
            }
        }
    }
    connections.abort_all();
}

async fn handle_tcp_connection(
    mut stream: TcpStream,
    zone_rx: watch::Receiver<Arc<DnsZone>>,
    mut shutdown_rx: watch::Receiver<bool>,
    metrics: Arc<DnsRuntimeMetrics>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    for _ in 0..MAX_DNS_TCP_QUERIES_PER_CONNECTION {
        let mut length = [0_u8; 2];
        let read_length = tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    return;
                }
                continue;
            }
            result = tokio::time::timeout(DNS_TCP_IO_TIMEOUT, stream.read_exact(&mut length)) => result,
        };
        match read_length {
            Ok(Ok(_)) => {}
            Ok(Err(error)) if error.kind() == io::ErrorKind::UnexpectedEof => return,
            Ok(Err(_)) | Err(_) => {
                metrics.io_errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        let length = usize::from(u16::from_be_bytes(length));
        if length == 0 || length > MAX_DNS_REQUEST_BYTES {
            metrics.oversized_requests.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut request = vec![0_u8; length];
        match tokio::time::timeout(DNS_TCP_IO_TIMEOUT, stream.read_exact(&mut request)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => {
                metrics.io_errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        metrics.tcp_queries.fetch_add(1, Ordering::Relaxed);
        let Some(response) = encode_response(&request, &zone_rx.borrow(), &metrics) else {
            continue;
        };
        let mut response = response.bytes;
        if response.len() > MAX_DNS_TCP_RESPONSE_BYTES {
            response = truncate_response(&response, &metrics);
        }
        let Ok(length) = u16::try_from(response.len()) else {
            metrics.io_errors.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let write = async {
            stream.write_all(&length.to_be_bytes()).await?;
            stream.write_all(&response).await
        };
        match tokio::time::timeout(DNS_TCP_IO_TIMEOUT, write).await {
            Ok(Ok(())) => {
                metrics.responses.fetch_add(1, Ordering::Relaxed);
            }
            Ok(Err(_)) | Err(_) => {
                metrics.io_errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
    }
}

fn encode_response(
    request_bytes: &[u8],
    zone: &DnsZone,
    metrics: &DnsRuntimeMetrics,
) -> Option<EncodedDnsResponse> {
    let request = match Message::from_vec(request_bytes) {
        Ok(request) => request,
        Err(_) => {
            metrics.format_errors.fetch_add(1, Ordering::Relaxed);
            return encode_error_response(request_bytes, ResponseCode::FormErr);
        }
    };
    let udp_payload_limit = usize::from(request.max_payload()).min(MAX_DNS_UDP_RESPONSE_BYTES);
    let response = answer_request(&request, zone, metrics);
    match response.to_vec() {
        Ok(bytes) => Some(EncodedDnsResponse {
            bytes,
            udp_payload_limit,
        }),
        Err(_) => {
            metrics.io_errors.fetch_add(1, Ordering::Relaxed);
            None
        }
    }
}

fn encode_error_response(request_bytes: &[u8], code: ResponseCode) -> Option<EncodedDnsResponse> {
    let id = request_bytes
        .get(..2)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u16::from_be_bytes)
        .unwrap_or_default();
    Message::error_msg(id, OpCode::Query, code)
        .to_vec()
        .ok()
        .map(|bytes| EncodedDnsResponse {
            bytes,
            udp_payload_limit: 512,
        })
}

fn answer_request(request: &Message, zone: &DnsZone, metrics: &DnsRuntimeMetrics) -> Message {
    if request.extensions().is_some() && request.version() != 0 {
        return response_for(request, ResponseCode::BADVERS, false);
    }
    if request.message_type() != MessageType::Query
        || request.op_code() != OpCode::Query
        || request.queries().len() != 1
    {
        metrics.format_errors.fetch_add(1, Ordering::Relaxed);
        return response_for(request, ResponseCode::FormErr, false);
    }
    let query = request.query().expect("one DNS query was checked");
    if !matches!(query.query_class(), DNSClass::IN | DNSClass::ANY) {
        metrics.refused.fetch_add(1, Ordering::Relaxed);
        return response_for(request, ResponseCode::Refused, false);
    }

    let owner = canonical_name(query.name());
    let forward_in_zone = name_is_in_zone(&owner, zone.name());
    let reverse_target = zone.reverse_target_name(&owner);
    if !forward_in_zone && reverse_target.is_none() {
        metrics.refused.fetch_add(1, Ordering::Relaxed);
        return response_for(request, ResponseCode::Refused, false);
    }

    let mut response = response_for(request, ResponseCode::NoError, true);
    if let Some(target) = reverse_target {
        if matches!(query.query_type(), RecordType::PTR | RecordType::ANY) {
            match Name::from_ascii(target) {
                Ok(target) => {
                    response.add_answer(Record::from_rdata(
                        query.name().clone(),
                        zone.ttl_seconds(),
                        RData::PTR(PTR(target)),
                    ));
                }
                Err(_) => {
                    response.set_response_code(ResponseCode::ServFail);
                }
            }
        }
        if response.answers().is_empty() {
            add_negative_authority(&mut response, zone);
        }
        return response;
    }

    let Some(record) = zone.record(&owner) else {
        if owner != canonical_text_name(zone.name()) {
            response.set_response_code(ResponseCode::NXDomain);
            metrics.nxdomain.fetch_add(1, Ordering::Relaxed);
        }
        add_negative_authority(&mut response, zone);
        return response;
    };
    if matches!(query.query_type(), RecordType::A | RecordType::ANY) {
        response.add_answers(record.ipv4.iter().map(|address| {
            Record::from_rdata(
                query.name().clone(),
                zone.ttl_seconds(),
                RData::A(A(*address)),
            )
        }));
    }
    if matches!(query.query_type(), RecordType::AAAA | RecordType::ANY) {
        response.add_answers(record.ipv6.iter().map(|address| {
            Record::from_rdata(
                query.name().clone(),
                zone.ttl_seconds(),
                RData::AAAA(AAAA(*address)),
            )
        }));
    }
    if response.answers().is_empty() {
        add_negative_authority(&mut response, zone);
    }
    response
}

fn add_negative_authority(response: &mut Message, zone: &DnsZone) {
    let Ok(zone_name) = Name::from_ascii(zone.name()) else {
        return;
    };
    let Ok(primary) = Name::from_ascii(format!("ns.{}", zone.name())) else {
        return;
    };
    let Ok(responsible) = Name::from_ascii(format!("hostmaster.{}", zone.name())) else {
        return;
    };
    let ttl = zone.ttl_seconds();
    response.add_name_server(Record::from_rdata(
        zone_name,
        ttl,
        RData::SOA(SOA::new(primary, responsible, 1, 60, 30, 300, ttl)),
    ));
}

fn response_for(request: &Message, code: ResponseCode, authoritative: bool) -> Message {
    let mut response = Message::new();
    response
        .set_id(request.id())
        .set_message_type(MessageType::Response)
        .set_op_code(request.op_code())
        .set_authoritative(authoritative)
        .set_recursion_desired(request.recursion_desired())
        .set_recursion_available(false)
        .set_response_code(code)
        .add_queries(request.queries().iter().cloned());
    if request.extensions().is_some() {
        let mut response_edns = Edns::new();
        let server_payload = u16::try_from(MAX_DNS_UDP_RESPONSE_BYTES)
            .expect("maximum DNS UDP response size fits in an EDNS payload field");
        response_edns.set_max_payload(request.max_payload().min(server_payload));
        response.set_edns(response_edns);
    }
    response
}

fn truncate_response(response: &[u8], metrics: &DnsRuntimeMetrics) -> Vec<u8> {
    metrics.truncated.fetch_add(1, Ordering::Relaxed);
    Message::from_vec(response)
        .map(|response| response.truncate())
        .and_then(|response| response.to_vec())
        .unwrap_or_default()
}

fn canonical_name(name: &Name) -> String {
    canonical_text_name(&name.to_ascii())
}

fn canonical_text_name(name: &str) -> String {
    let mut canonical = name.to_ascii_lowercase();
    if !canonical.ends_with('.') {
        canonical.push('.');
    }
    canonical
}

fn control_lookup(
    zone: &DnsZone,
    input: &str,
    lookup_type: DnsLookupType,
) -> Result<(String, DnsLookupType), &'static str> {
    if input.is_empty() || input.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err("invalid_input");
    }
    if let Ok(address) = input.parse() {
        if matches!(lookup_type, DnsLookupType::A | DnsLookupType::Aaaa) {
            return Err("address_requires_ptr_or_auto");
        }
        return Ok((crate::dns::reverse_name(address), DnsLookupType::Ptr));
    }

    let name = if input.contains('.') {
        input.to_owned()
    } else {
        zone.qualify(input).map_err(|_| "invalid_name")?
    };
    let name = Name::from_ascii(&name).map_err(|_| "invalid_name")?;
    let lookup_type = if lookup_type == DnsLookupType::Auto {
        DnsLookupType::Any
    } else {
        lookup_type
    };
    Ok((canonical_name(&name), lookup_type))
}

fn preview_values<T: ToString>(values: &[T], limit: usize) -> String {
    if values.is_empty() {
        return "-".to_owned();
    }
    let mut preview = values
        .iter()
        .take(limit)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if values.len() > limit {
        preview.push("...".to_owned());
    }
    preview.join(",")
}

fn sanitize_control_value(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':') {
                char::from(byte)
            } else {
                '_'
            }
        })
        .collect()
}

fn name_is_in_zone(name: &str, zone: &str) -> bool {
    let zone = canonical_text_name(zone);
    name == zone
        || name
            .strip_suffix(&zone)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{Config, RouteConfig},
        identity::NodeIdentity,
    };
    use hickory_proto::rr::rdata::opt::EdnsOption;
    use tokio::net::UdpSocket;

    fn config(hostname: &str) -> Config {
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        serde_json::from_value(serde_json::json!({
            "network": {
                "name": "runner-mesh",
                "private_key": identity.private_key,
                "dns": {
                    "enabled": true,
                    "hostname": hostname,
                    "listen": "127.0.0.1:0"
                }
            }
        }))
        .expect("DNS test config")
    }

    fn query(name: &str, record_type: RecordType) -> Vec<u8> {
        let mut query = Message::new();
        query
            .set_id(42)
            .set_recursion_desired(true)
            .add_query(hickory_proto::op::Query::query(
                Name::from_ascii(name).expect("query name"),
                record_type,
            ));
        query.to_vec().expect("query encoding")
    }

    fn query_with_edns(name: &str, record_type: RecordType, max_payload: u16) -> Vec<u8> {
        let mut query = Message::from_vec(&query(name, record_type)).expect("base query");
        let mut edns = Edns::new();
        edns.set_max_payload(max_payload);
        edns.options_mut()
            .insert(EdnsOption::Unknown(65_001, vec![1, 2, 3]));
        query.set_edns(edns);
        query.to_vec().expect("EDNS query encoding")
    }

    async fn udp_query(listener: SocketAddr, request: &[u8]) -> Message {
        let socket = UdpSocket::bind("127.0.0.1:0").await.expect("UDP client");
        socket.send_to(request, listener).await.expect("send query");
        let mut response = [0_u8; MAX_DNS_UDP_RESPONSE_BYTES];
        let (length, _) =
            tokio::time::timeout(Duration::from_secs(1), socket.recv_from(&mut response))
                .await
                .expect("DNS response timeout")
                .expect("DNS response");
        Message::from_vec(&response[..length]).expect("response encoding")
    }

    #[test]
    fn authoritative_response_answers_a_aaaa_ptr_and_refuses_other_zones() {
        let config = config("worker-1");
        let zone = DnsZone::from_config_at(&config, &[], 1_000).expect("zone");
        let metrics = DnsRuntimeMetrics::default();
        let fqdn = "worker-1.runner-mesh.p2p-vpn.internal.";

        let a = answer_request(
            &Message::from_vec(&query(fqdn, RecordType::A)).expect("A query"),
            &zone,
            &metrics,
        );
        assert_eq!(a.response_code(), ResponseCode::NoError);
        assert!(a.authoritative());
        assert!(
            a.answers()
                .iter()
                .all(|answer| answer.record_type() == RecordType::A)
        );
        assert!(!a.answers().is_empty());

        let aaaa = answer_request(
            &Message::from_vec(&query(fqdn, RecordType::AAAA)).expect("AAAA query"),
            &zone,
            &metrics,
        );
        assert!(
            aaaa.answers()
                .iter()
                .all(|answer| answer.record_type() == RecordType::AAAA)
        );
        assert!(!aaaa.answers().is_empty());

        let reverse = zone.reverse_records().next().expect("reverse record");
        let ptr = answer_request(
            &Message::from_vec(&query(reverse.0, RecordType::PTR)).expect("PTR query"),
            &zone,
            &metrics,
        );
        assert_eq!(ptr.answers().len(), 1);
        assert_eq!(ptr.answers()[0].record_type(), RecordType::PTR);

        let refused = answer_request(
            &Message::from_vec(&query("example.com.", RecordType::A)).expect("external query"),
            &zone,
            &metrics,
        );
        assert_eq!(refused.response_code(), ResponseCode::Refused);
        assert!(!refused.authoritative());
        assert!(!refused.recursion_available());
    }

    #[test]
    fn unknown_overlay_name_is_nxdomain_and_known_other_type_is_nodata() {
        let config = config("worker-1");
        let zone = DnsZone::from_config_at(&config, &[], 1_000).expect("zone");
        let metrics = DnsRuntimeMetrics::default();
        let missing = answer_request(
            &Message::from_vec(&query(
                "missing.runner-mesh.p2p-vpn.internal.",
                RecordType::A,
            ))
            .expect("missing query"),
            &zone,
            &metrics,
        );
        assert_eq!(missing.response_code(), ResponseCode::NXDomain);
        assert_eq!(missing.name_servers().len(), 1);
        assert_eq!(missing.name_servers()[0].record_type(), RecordType::SOA);

        let nodata = answer_request(
            &Message::from_vec(&query(
                "worker-1.runner-mesh.p2p-vpn.internal.",
                RecordType::TXT,
            ))
            .expect("TXT query"),
            &zone,
            &metrics,
        );
        assert_eq!(nodata.response_code(), ResponseCode::NoError);
        assert!(nodata.answers().is_empty());
        assert_eq!(nodata.name_servers().len(), 1);
        assert_eq!(nodata.name_servers()[0].record_type(), RecordType::SOA);
    }

    #[test]
    fn reserved_suffix_guard_blocks_private_names_and_refuses_unrelated_queries() {
        let zone = DnsZone::reserved_suffix_guard();
        let metrics = DnsRuntimeMetrics::default();
        let private = answer_request(
            &Message::from_vec(&query(
                "worker.runner-mesh.p2p-vpn.internal.",
                RecordType::A,
            ))
            .expect("private query"),
            &zone,
            &metrics,
        );
        assert_eq!(private.response_code(), ResponseCode::NXDomain);
        assert!(private.authoritative());
        assert_eq!(private.name_servers().len(), 1);
        assert_eq!(private.name_servers()[0].record_type(), RecordType::SOA);

        let unrelated = answer_request(
            &Message::from_vec(&query("example.com.", RecordType::A)).expect("external query"),
            &zone,
            &metrics,
        );
        assert_eq!(unrelated.response_code(), ResponseCode::Refused);
        assert!(!unrelated.authoritative());
        assert_eq!(metrics.nxdomain.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.refused.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn authoritative_response_negotiates_edns_without_echoing_client_options() {
        let zone = DnsZone::reserved_suffix_guard();
        let metrics = DnsRuntimeMetrics::default();
        let request = Message::from_vec(&query_with_edns(
            "unknown.p2p-vpn.internal.",
            RecordType::A,
            4_096,
        ))
        .expect("EDNS query");
        let response = answer_request(&request, &zone, &metrics);
        let edns = response.extensions().as_ref().expect("EDNS response");

        assert_eq!(edns.version(), 0);
        assert_eq!(edns.max_payload(), MAX_DNS_UDP_RESPONSE_BYTES as u16);
        assert!(edns.options().as_ref().is_empty());
    }

    #[test]
    fn unsupported_edns_version_returns_badvers() {
        let zone = DnsZone::reserved_suffix_guard();
        let metrics = DnsRuntimeMetrics::default();
        let mut request = Message::from_vec(&query_with_edns(
            "unknown.p2p-vpn.internal.",
            RecordType::A,
            1_232,
        ))
        .expect("EDNS query");
        request
            .extensions_mut()
            .as_mut()
            .expect("EDNS request")
            .set_version(1);
        let encoded = answer_request(&request, &zone, &metrics)
            .to_vec()
            .expect("BADVERS response");
        let response = Message::from_vec(&encoded).expect("decoded BADVERS response");

        // Hickory decodes wire code 16 as BADSIG because BADVERS shares that value.
        assert_eq!(u16::from(response.response_code()), 16);
        assert_eq!(
            response
                .extensions()
                .as_ref()
                .expect("BADVERS EDNS response")
                .rcode_high(),
            1
        );
        assert_eq!(response.version(), 0);
        assert!(!response.authoritative());
    }

    #[test]
    fn encoded_response_honors_udp_payload_negotiation() {
        let zone = DnsZone::reserved_suffix_guard();
        let metrics = DnsRuntimeMetrics::default();
        let plain = encode_response(
            &query("unknown.p2p-vpn.internal.", RecordType::A),
            &zone,
            &metrics,
        )
        .expect("plain response");
        let extended = encode_response(
            &query_with_edns("unknown.p2p-vpn.internal.", RecordType::A, 4_096),
            &zone,
            &metrics,
        )
        .expect("EDNS response");

        assert_eq!(plain.udp_payload_limit, 512);
        assert_eq!(extended.udp_payload_limit, MAX_DNS_UDP_RESPONSE_BYTES);
    }

    #[tokio::test]
    async fn reserved_suffix_guard_binds_only_to_loopback() {
        let guard = DnsRuntime::bind_reserved_suffix("127.0.0.1:0".parse().expect("listener"))
            .await
            .expect("guard bind");
        let private = udp_query(
            guard.listener(),
            &query("unknown.p2p-vpn.internal.", RecordType::A),
        )
        .await;
        assert_eq!(private.response_code(), ResponseCode::NXDomain);

        let error = DnsRuntime::bind_reserved_suffix("0.0.0.0:0".parse().expect("listener"))
            .await
            .expect_err("non-loopback guard");
        assert!(matches!(error, DnsRuntimeError::NonLoopbackListener(_)));
    }

    #[tokio::test]
    async fn runtime_serves_udp_and_tcp_on_the_same_ephemeral_port() {
        let config = config("worker-1");
        let runtime = DnsRuntime::bind_at(&config, &[], 1_000)
            .await
            .expect("DNS bind")
            .expect("enabled DNS");
        let request = query("worker-1.runner-mesh.p2p-vpn.internal.", RecordType::A);
        let udp = udp_query(runtime.listener(), &request).await;
        assert_eq!(udp.response_code(), ResponseCode::NoError);
        assert!(!udp.answers().is_empty());

        let mut tcp = TcpStream::connect(runtime.listener())
            .await
            .expect("TCP client");
        tcp.write_all(
            &u16::try_from(request.len())
                .expect("request length")
                .to_be_bytes(),
        )
        .await
        .expect("TCP length");
        tcp.write_all(&request).await.expect("TCP query");
        let length = tcp.read_u16().await.expect("TCP response length");
        let mut response = vec![0_u8; usize::from(length)];
        tcp.read_exact(&mut response).await.expect("TCP response");
        let response = Message::from_vec(&response).expect("TCP DNS response");
        assert_eq!(response.response_code(), ResponseCode::NoError);
        assert!(!response.answers().is_empty());

        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.udp_queries, 1);
        assert_eq!(snapshot.tcp_queries, 1);
        assert_eq!(snapshot.responses, 2);
    }

    #[tokio::test]
    async fn runtime_refresh_replaces_the_zone_atomically() {
        let mut config = config("worker-1");
        let runtime = DnsRuntime::bind_at(&config, &[], 1_000)
            .await
            .expect("DNS bind")
            .expect("enabled DNS");
        config.network.dns.hostname = Some("worker-2".to_owned());
        runtime.refresh_at(&config, &[], 1_001).expect("refresh");

        let present = udp_query(
            runtime.listener(),
            &query("worker-2.runner-mesh.p2p-vpn.internal.", RecordType::A),
        )
        .await;
        assert_eq!(present.response_code(), ResponseCode::NoError);
        let absent = udp_query(
            runtime.listener(),
            &query("worker-1.runner-mesh.p2p-vpn.internal.", RecordType::A),
        )
        .await;
        assert_eq!(absent.response_code(), ResponseCode::NXDomain);
        assert_eq!(runtime.snapshot().zone_refreshes, 1);
    }

    #[tokio::test]
    async fn udp_replies_formerr_to_malformed_input_and_truncates_large_answers() {
        let mut config = config("worker-1");
        config.network.routes = (0_u8..100)
            .map(|suffix| RouteConfig {
                prefix: format!("10.2.0.{suffix}/32"),
                metric: 100,
            })
            .collect();
        let runtime = DnsRuntime::bind_at(&config, &[], 1_000)
            .await
            .expect("DNS bind")
            .expect("enabled DNS");

        let malformed = udp_query(runtime.listener(), &[0x12, 0x34, 0xff]).await;
        assert_eq!(malformed.id(), 0x1234);
        assert_eq!(malformed.response_code(), ResponseCode::FormErr);

        let large = udp_query(
            runtime.listener(),
            &query("worker-1.runner-mesh.p2p-vpn.internal.", RecordType::A),
        )
        .await;
        assert!(large.truncated());
        assert!(large.answers().is_empty());
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.format_errors, 1);
        assert_eq!(snapshot.truncated, 1);
    }

    #[tokio::test]
    async fn failed_refresh_fails_closed_and_retries() {
        let mut config = config("worker-1");
        let runtime = DnsRuntime::bind_at(&config, &[], 1_000)
            .await
            .expect("DNS bind")
            .expect("enabled DNS");
        config.network.dns.hostname = Some("-invalid".to_owned());

        assert!(runtime.refresh_at(&config, &[], 1_001).is_err());
        assert!(
            runtime
                .zone()
                .record("worker-1.runner-mesh.p2p-vpn.internal.")
                .is_none()
        );
        let snapshot = runtime.snapshot();
        assert_eq!(snapshot.zone_refreshes, 0);
        assert_eq!(snapshot.zone_refresh_failures, 1);
        assert!(snapshot.degraded);
        assert!(!runtime.refresh_due(1_005));
        assert!(runtime.refresh_due(1_006));

        config.network.dns.hostname = Some("worker-2".to_owned());
        runtime
            .refresh_at(&config, &[], 1_006)
            .expect("valid retry");
        assert!(
            runtime
                .zone()
                .record("worker-2.runner-mesh.p2p-vpn.internal.")
                .is_some()
        );
        assert!(!runtime.snapshot().degraded);
    }

    #[tokio::test]
    async fn control_views_report_status_pages_and_qualified_resolution() {
        let config = config("worker-1");
        let runtime = DnsRuntime::bind_at(&config, &[], 1_000)
            .await
            .expect("DNS bind")
            .expect("enabled DNS");

        let status = runtime.status_lines();
        assert!(status[0].contains("enabled=true"));
        assert!(status[0].contains("zone=runner-mesh.p2p-vpn.internal."));

        let list = runtime.list_lines(0, 1);
        assert!(list[0].starts_with("dns_list offset=0 limit=1 returned=1"));
        assert!(list[0].contains("more=true"));

        let resolved = runtime.resolve_lines("worker-1", DnsLookupType::Auto);
        assert!(resolved[0].contains("name=worker-1.runner-mesh.p2p-vpn.internal."));
        assert!(resolved[0].contains("status=ok"));
        assert!(resolved[0].contains("A:"));
        assert!(resolved[0].contains("AAAA:"));

        let address = runtime
            .zone()
            .record("worker-1.runner-mesh.p2p-vpn.internal.")
            .expect("local record")
            .ipv4[0];
        let reverse = runtime.resolve_lines(&address.to_string(), DnsLookupType::Auto);
        assert!(reverse[0].contains("type=PTR status=ok"));
        assert!(reverse[0].contains("values=worker-1.runner-mesh.p2p-vpn.internal."));

        let refused = runtime.resolve_lines("example.com", DnsLookupType::A);
        assert!(refused[0].contains("status=refused"));
    }
}
