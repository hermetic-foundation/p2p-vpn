use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt, io,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use futures::StreamExt as _;
use libp2p::{
    Multiaddr, PeerId, StreamProtocol, Swarm, SwarmBuilder, connection_limits, dns, identify, kad,
    mdns,
    multiaddr::Protocol,
    noise, ping, request_response,
    request_response::Message,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux,
};

use crate::{
    config::{
        BootstrapPeerConfig, PUBLIC_IPFS_KADEMLIA_PROTOCOL, ResourceConfig, RouteConfig,
        public_ipfs_bootstrap_peer_configs,
    },
    identity::NodeIdentity,
    pairing::{
        PairingOffer, PairingRequestOptions, PairingResponse, build_named_pairing_request_at,
    },
    pairing_code::{
        PairingCode, PendingPairingCodeHelloV2, authenticate_pairing_request,
        open_pairing_code_challenge_v2_at, start_pairing_code_hello_v2_at,
    },
    runtime::{
        p2p::{controlled_kademlia_config, decode_keypair, kademlia_pairing_code_v2_key},
        pairing_code::{
            self, PAIRING_CODE_PROTOCOL, PAIRING_CODE_V2_PROTOCOL, PairingCodeRejectionReason,
            PairingCodeV2Codec, PairingCodeV2Request, PairingCodeV2Response,
        },
    },
};

pub const DEFAULT_PAIRING_BOOTSTRAP_TIMEOUT: Duration = Duration::from_mins(10);
pub const MAX_PAIRING_BOOTSTRAP_TIMEOUT: Duration = Duration::from_hours(1);
pub const PAIRING_BOOTSTRAP_LAN_GRACE: Duration = Duration::from_secs(3);
pub const MAX_PAIRING_BOOTSTRAP_CANDIDATES: usize = 128;
pub const MAX_PAIRING_BOOTSTRAP_ADDRESSES_PER_PEER: usize = 8;
pub const MAX_PAIRING_BOOTSTRAP_PENDING_HELLOS: usize = 32;
pub const MAX_PAIRING_BOOTSTRAP_ATTEMPTS_PER_PEER: u8 = 8;
pub const MAX_PAIRING_BOOTSTRAP_TOTAL_ATTEMPTS: u16 = 512;
pub const MAX_PAIRING_BOOTSTRAP_EXISTING_NETWORKS: usize = 256;
pub const MAX_PAIRING_BOOTSTRAP_CANDIDATE_HINTS: usize = 8;

const BOOTSTRAP_IDENTIFY_PROTOCOL: &str = "/p2p-vpn/pairing-bootstrap/2";
const BOOTSTRAP_TICK: Duration = Duration::from_millis(250);
const PUBLIC_LOOKUP_INTERVAL: Duration = Duration::from_secs(10);
const REQUEST_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_PUBLIC_LOOKUPS: u16 = 360;

#[derive(Clone, Debug)]
pub struct PairingBootstrapOptions {
    pub timeout: Duration,
    pub lan_grace: Duration,
    pub existing_network_names: Vec<String>,
    pub requested_hostname: Option<String>,
    pub requested_vpn_ip: Option<String>,
    pub requested_routes: Vec<RouteConfig>,
    /// Untrusted dial hints for environments where multicast discovery cannot cross the local
    /// boundary. The pairing code still authenticates the inviter before any profile is accepted.
    pub candidate_hints: Vec<BootstrapPeerConfig>,
}

impl Default for PairingBootstrapOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_PAIRING_BOOTSTRAP_TIMEOUT,
            lan_grace: PAIRING_BOOTSTRAP_LAN_GRACE,
            existing_network_names: Vec::new(),
            requested_hostname: None,
            requested_vpn_ip: None,
            requested_routes: Vec::new(),
            candidate_hints: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PairingBootstrapEnrollment {
    pub offer: PairingOffer,
    pub response: PairingResponse,
}

#[derive(Debug)]
pub enum PairingBootstrapError {
    InvalidTimeout,
    Build(String),
    Pairing(crate::pairing_code::PairingCodeError),
    Rejected(PairingCodeRejectionReason),
    UpgradeRequired { peers: usize },
    AlreadyJoined { network_name: String },
    Unavailable,
    TimedOut,
}

impl fmt::Display for PairingBootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTimeout => write!(
                formatter,
                "pairing timeout must be between 1 second and {} seconds, and LAN grace must not exceed it",
                MAX_PAIRING_BOOTSTRAP_TIMEOUT.as_secs()
            ),
            Self::Build(error) => write!(formatter, "failed to start pairing discovery: {error}"),
            Self::Pairing(error) => write!(formatter, "pairing exchange failed: {error:?}"),
            Self::Rejected(reason) => write!(formatter, "pairing request was rejected: {reason:?}"),
            Self::UpgradeRequired { peers } => write!(
                formatter,
                "{peers} discovered inviter(s) do not support profile-free pairing protocol v2"
            ),
            Self::AlreadyJoined { network_name } => {
                write!(
                    formatter,
                    "a profile for network {network_name:?} already exists"
                )
            }
            Self::Unavailable => formatter.write_str("no pairing inviter was discovered"),
            Self::TimedOut => formatter.write_str("pairing discovery timed out"),
        }
    }
}

impl Error for PairingBootstrapError {}

impl From<crate::pairing_code::PairingCodeError> for PairingBootstrapError {
    fn from(error: crate::pairing_code::PairingCodeError) -> Self {
        Self::Pairing(error)
    }
}

#[derive(NetworkBehaviour)]
struct PairingBootstrapBehaviour {
    connection_limits: connection_limits::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
    mdns: mdns::tokio::Behaviour,
    pairing_code_v2: request_response::Behaviour<PairingCodeV2Codec>,
}

struct Candidate {
    public_provider: bool,
    lan_addresses: HashSet<Multiaddr>,
    attempts: u8,
    in_flight: bool,
    next_attempt_at: Instant,
}

enum PendingRequest {
    Hello {
        peer: PeerId,
        pending: PendingPairingCodeHelloV2,
    },
    Submit {
        peer: PeerId,
        offer: PairingOffer,
    },
    Poll {
        peer: PeerId,
        offer: PairingOffer,
    },
}

struct PendingApproval {
    peer: PeerId,
    offer: PairingOffer,
    ticket: String,
    expires_at_unix_seconds: u64,
    next_poll_at: Instant,
    poll_in_flight: bool,
}

struct BootstrapState {
    candidates: HashMap<PeerId, Candidate>,
    requests: HashMap<request_response::OutboundRequestId, PendingRequest>,
    public_lookup_ids: HashSet<kad::QueryId>,
    public_lookup_attempts: u16,
    next_public_lookup_at: Instant,
    total_attempts: u16,
    selected_peer: Option<PeerId>,
    pending_approval: Option<PendingApproval>,
    v2_challenge_opened: bool,
    upgrade_required_peers: HashSet<PeerId>,
}

impl BootstrapState {
    fn new(now: Instant, lan_grace: Duration) -> Self {
        Self {
            candidates: HashMap::new(),
            requests: HashMap::new(),
            public_lookup_ids: HashSet::new(),
            public_lookup_attempts: 0,
            next_public_lookup_at: now + lan_grace,
            total_attempts: 0,
            selected_peer: None,
            pending_approval: None,
            v2_challenge_opened: false,
            upgrade_required_peers: HashSet::new(),
        }
    }

    fn ensure_candidate(&mut self, peer: PeerId, public_provider: bool, now: Instant) -> bool {
        if let Some(candidate) = self.candidates.get_mut(&peer) {
            candidate.public_provider |= public_provider;
            return true;
        }
        if self.candidates.len() >= MAX_PAIRING_BOOTSTRAP_CANDIDATES {
            return false;
        }
        self.candidates.insert(
            peer,
            Candidate {
                public_provider,
                lan_addresses: HashSet::new(),
                attempts: 0,
                in_flight: false,
                next_attempt_at: now,
            },
        );
        true
    }

    fn record_public_candidate(&mut self, peer: PeerId, now: Instant) {
        self.ensure_candidate(peer, true, now);
    }

    fn record_lan_candidate(&mut self, peer: PeerId, address: Multiaddr, now: Instant) -> bool {
        if !self.ensure_candidate(peer, false, now) {
            return false;
        }
        let candidate = self
            .candidates
            .get_mut(&peer)
            .expect("candidate was inserted immediately above");
        if candidate.lan_addresses.contains(&address) {
            return false;
        }
        if candidate.lan_addresses.len() >= MAX_PAIRING_BOOTSTRAP_ADDRESSES_PER_PEER {
            return false;
        }
        candidate.lan_addresses.insert(address)
    }

    fn next_candidate(&self, local_peer: PeerId, now: Instant) -> Option<PeerId> {
        if self.selected_peer.is_some()
            || self.total_attempts >= MAX_PAIRING_BOOTSTRAP_TOTAL_ATTEMPTS
            || self.pending_hello_count() >= MAX_PAIRING_BOOTSTRAP_PENDING_HELLOS
        {
            return None;
        }
        self.candidates
            .iter()
            .filter(|(peer, candidate)| {
                **peer != local_peer
                    && !candidate.in_flight
                    && candidate.attempts < MAX_PAIRING_BOOTSTRAP_ATTEMPTS_PER_PEER
                    && now >= candidate.next_attempt_at
            })
            .min_by_key(|(peer, candidate)| (candidate.attempts, peer.to_bytes()))
            .map(|(peer, _)| *peer)
    }

    fn mark_hello_started(&mut self, peer: PeerId) {
        let Some(candidate) = self.candidates.get_mut(&peer) else {
            return;
        };
        candidate.attempts = candidate.attempts.saturating_add(1);
        candidate.in_flight = true;
        self.total_attempts = self.total_attempts.saturating_add(1);
    }

    fn release_candidate(&mut self, peer: PeerId, now: Instant) {
        if let Some(candidate) = self.candidates.get_mut(&peer) {
            candidate.in_flight = false;
            candidate.next_attempt_at = now + REQUEST_RETRY_DELAY;
        }
    }

    fn pending_hello_count(&self) -> usize {
        self.requests
            .values()
            .filter(|request| matches!(request, PendingRequest::Hello { .. }))
            .count()
    }

    fn record_unsupported(&mut self, peer: PeerId) {
        if self
            .candidates
            .get(&peer)
            .is_some_and(|candidate| candidate.public_provider)
        {
            self.upgrade_required_peers.insert(peer);
        }
    }

    fn may_accept_challenge_from(&self, peer: PeerId) -> bool {
        self.selected_peer.is_none_or(|selected| selected == peer)
    }

    fn terminal_timeout_error(&self) -> PairingBootstrapError {
        if !self.v2_challenge_opened && !self.upgrade_required_peers.is_empty() {
            PairingBootstrapError::UpgradeRequired {
                peers: self.upgrade_required_peers.len(),
            }
        } else if self.candidates.is_empty() {
            PairingBootstrapError::Unavailable
        } else {
            PairingBootstrapError::TimedOut
        }
    }

    fn approval_expired(&self, now_unix_seconds: u64) -> bool {
        self.pending_approval
            .as_ref()
            .is_some_and(|approval| now_unix_seconds > approval.expires_at_unix_seconds)
    }
}

/// Discovers an inviter and completes pairing v2 without requiring an overlay profile or network
/// name. The returned signed artifacts are intentionally not applied to persistent configuration;
/// the platform enrollment layer owns that atomic step.
pub async fn join_by_code_v2(
    identity: NodeIdentity,
    code: PairingCode,
    options: PairingBootstrapOptions,
) -> Result<PairingBootstrapEnrollment, PairingBootstrapError> {
    validate_options(&options)?;
    let mut swarm = build_bootstrap_swarm(&identity)?;
    let local_peer = *swarm.local_peer_id();
    let now = Instant::now();
    let mut state = BootstrapState::new(now, options.lan_grace);
    seed_candidate_hints(&mut swarm, &mut state, &options.candidate_hints, now)?;
    let mut tick = tokio::time::interval(BOOTSTRAP_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let deadline = tokio::time::sleep(options.timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            () = &mut deadline => return Err(state.terminal_timeout_error()),
            _ = tick.tick() => {
                if state.approval_expired(current_unix_seconds()) {
                    return Err(PairingBootstrapError::Rejected(
                        PairingCodeRejectionReason::Expired,
                    ));
                }
                drive_public_lookup(&mut swarm, &mut state, &code, Instant::now());
                drive_pending_poll(&mut swarm, &mut state, Instant::now());
                drive_hellos(&mut swarm, &mut state, &identity, &code, local_peer);
            }
            event = swarm.select_next_some() => {
                if let Some(result) = handle_swarm_event(
                    &mut swarm,
                    &mut state,
                    &identity,
                    &code,
                    &options,
                    event,
                )? {
                    return Ok(result);
                }
            }
        }
    }
}

fn validate_options(options: &PairingBootstrapOptions) -> Result<(), PairingBootstrapError> {
    if options.timeout < Duration::from_secs(1)
        || options.timeout > MAX_PAIRING_BOOTSTRAP_TIMEOUT
        || options.lan_grace > options.timeout
    {
        return Err(PairingBootstrapError::InvalidTimeout);
    }
    if options.existing_network_names.len() > MAX_PAIRING_BOOTSTRAP_EXISTING_NETWORKS
        || options.existing_network_names.iter().any(|network_name| {
            network_name.is_empty()
                || network_name.len() > 128
                || network_name.chars().any(char::is_control)
        })
    {
        return Err(PairingBootstrapError::Build(
            "existing network names are invalid".to_owned(),
        ));
    }
    if options.candidate_hints.len() > MAX_PAIRING_BOOTSTRAP_CANDIDATE_HINTS
        || options
            .candidate_hints
            .iter()
            .any(|candidate| candidate.peer_address().is_err())
    {
        return Err(PairingBootstrapError::Build(
            "pairing discovery candidate hints are invalid".to_owned(),
        ));
    }
    Ok(())
}

fn seed_candidate_hints(
    swarm: &mut Swarm<PairingBootstrapBehaviour>,
    state: &mut BootstrapState,
    candidates: &[BootstrapPeerConfig],
    now: Instant,
) -> Result<(), PairingBootstrapError> {
    for configured in candidates {
        let (peer, mut address) = configured.peer_address().map_err(|error| {
            PairingBootstrapError::Build(format!("pairing discovery candidate: {error:?}"))
        })?;
        strip_trailing_peer(&mut address, peer);
        if state.record_lan_candidate(peer, address.clone(), now) {
            swarm
                .behaviour_mut()
                .kad
                .add_protected_address(&peer, address);
        }
    }
    Ok(())
}

fn build_bootstrap_swarm(
    identity: &NodeIdentity,
) -> Result<Swarm<PairingBootstrapBehaviour>, PairingBootstrapError> {
    let keypair = decode_keypair(&identity.private_key)
        .map_err(|error| PairingBootstrapError::Build(format!("identity: {error:?}")))?;
    let built = (|| -> Result<Swarm<PairingBootstrapBehaviour>, Box<dyn Error + Send + Sync>> {
        let mut swarm = SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )?
            .with_quic()
            .with_dns_config(dns::ResolverConfig::default(), dns::ResolverOpts::default())
            .with_behaviour(
                |keypair| -> Result<PairingBootstrapBehaviour, Box<dyn Error + Send + Sync>> {
                    let local_peer = keypair.public().to_peer_id();
                    let store = kad::store::MemoryStore::new(local_peer);
                    let config = controlled_kademlia_config(StreamProtocol::new(
                        PUBLIC_IPFS_KADEMLIA_PROTOCOL,
                    ));
                    let mut kad = kad::Behaviour::with_config(local_peer, store, config);
                    kad.set_mode(Some(kad::Mode::Client));
                    Ok(PairingBootstrapBehaviour {
                        connection_limits: connection_limits::Behaviour::new(
                            ResourceConfig::default().to_connection_limits(),
                        ),
                        identify: identify::Behaviour::new(identify::Config::new(
                            BOOTSTRAP_IDENTIFY_PROTOCOL.to_owned(),
                            keypair.public(),
                        )),
                        ping: ping::Behaviour::new(ping::Config::new()),
                        kad,
                        mdns: mdns::tokio::Behaviour::new(mdns::Config::default(), local_peer)?,
                        pairing_code_v2: pairing_code::behaviour_v2(
                            MAX_PAIRING_BOOTSTRAP_PENDING_HELLOS,
                        ),
                    })
                },
            )?
            .build();

        for configured in public_ipfs_bootstrap_peer_configs() {
            let (peer, mut address) = configured
                .peer_address()
                .map_err(|error| io::Error::other(format!("{error:?}")))?;
            strip_trailing_peer(&mut address, peer);
            swarm
                .behaviour_mut()
                .kad
                .add_protected_address(&peer, address);
        }
        let _ = swarm
            .behaviour_mut()
            .kad
            .try_start_query(kad::Behaviour::bootstrap);
        swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse::<Multiaddr>()?)?;
        swarm.listen_on("/ip4/0.0.0.0/udp/0/quic-v1".parse::<Multiaddr>()?)?;
        Ok(swarm)
    })();
    built.map_err(|error| PairingBootstrapError::Build(error.to_string()))
}

fn strip_trailing_peer(address: &mut Multiaddr, peer: PeerId) {
    if matches!(address.iter().last(), Some(Protocol::P2p(address_peer)) if address_peer == peer) {
        address.pop();
    }
}

fn drive_public_lookup(
    swarm: &mut Swarm<PairingBootstrapBehaviour>,
    state: &mut BootstrapState,
    code: &PairingCode,
    now: Instant,
) {
    if now < state.next_public_lookup_at
        || state.public_lookup_attempts >= MAX_PUBLIC_LOOKUPS
        || !state.public_lookup_ids.is_empty()
        || state.selected_peer.is_some()
    {
        return;
    }
    let query = swarm
        .behaviour_mut()
        .kad
        .try_get_providers(kademlia_pairing_code_v2_key(&code.global_locator()));
    state.next_public_lookup_at = now + PUBLIC_LOOKUP_INTERVAL;
    let Ok(query_id) = query else {
        return;
    };
    state.public_lookup_ids.insert(query_id);
    state.public_lookup_attempts = state.public_lookup_attempts.saturating_add(1);
}

fn drive_hellos(
    swarm: &mut Swarm<PairingBootstrapBehaviour>,
    state: &mut BootstrapState,
    identity: &NodeIdentity,
    code: &PairingCode,
    local_peer: PeerId,
) {
    let now = Instant::now();
    while let Some(peer) = state.next_candidate(local_peer, now) {
        let Ok((hello, pending)) =
            start_pairing_code_hello_v2_at(code, identity, peer, current_unix_seconds())
        else {
            state.release_candidate(peer, now);
            return;
        };
        state.mark_hello_started(peer);
        let request_id = swarm.behaviour_mut().pairing_code_v2.send_request(
            &peer,
            PairingCodeV2Request::Hello {
                hello: Box::new(hello),
            },
        );
        state
            .requests
            .insert(request_id, PendingRequest::Hello { peer, pending });
    }
}

fn drive_pending_poll(
    swarm: &mut Swarm<PairingBootstrapBehaviour>,
    state: &mut BootstrapState,
    now: Instant,
) {
    let Some(approval) = state.pending_approval.as_mut() else {
        return;
    };
    if approval.poll_in_flight || now < approval.next_poll_at {
        return;
    }
    if current_unix_seconds() > approval.expires_at_unix_seconds {
        return;
    }
    let request_id = swarm.behaviour_mut().pairing_code_v2.send_request(
        &approval.peer,
        PairingCodeV2Request::Poll {
            ticket: approval.ticket.clone(),
        },
    );
    approval.poll_in_flight = true;
    state.requests.insert(
        request_id,
        PendingRequest::Poll {
            peer: approval.peer,
            offer: approval.offer.clone(),
        },
    );
}

fn handle_swarm_event(
    swarm: &mut Swarm<PairingBootstrapBehaviour>,
    state: &mut BootstrapState,
    identity: &NodeIdentity,
    code: &PairingCode,
    options: &PairingBootstrapOptions,
    event: SwarmEvent<PairingBootstrapBehaviourEvent>,
) -> Result<Option<PairingBootstrapEnrollment>, PairingBootstrapError> {
    match event {
        SwarmEvent::Behaviour(PairingBootstrapBehaviourEvent::Mdns(mdns::Event::Discovered(
            peers,
        ))) => {
            let now = Instant::now();
            for (peer, address) in peers {
                if state.record_lan_candidate(peer, address.clone(), now) {
                    swarm.behaviour_mut().kad.add_address(&peer, address);
                }
            }
        }
        SwarmEvent::Behaviour(PairingBootstrapBehaviourEvent::Kad(
            kad::Event::OutboundQueryProgressed {
                id, result, step, ..
            },
        )) => {
            if let kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                key,
                providers,
                ..
            })) = &result
                && state.public_lookup_ids.contains(&id)
                && *key == kademlia_pairing_code_v2_key(&code.global_locator())
            {
                let now = Instant::now();
                for peer in providers {
                    state.record_public_candidate(*peer, now);
                }
            }
            if step.last {
                state.public_lookup_ids.remove(&id);
            }
        }
        SwarmEvent::Behaviour(PairingBootstrapBehaviourEvent::Identify(
            identify::Event::Received { peer_id, info, .. },
        )) => {
            let supports_v1 = info
                .protocols
                .iter()
                .any(|protocol| protocol.as_ref() == PAIRING_CODE_PROTOCOL);
            let supports_v2 = info
                .protocols
                .iter()
                .any(|protocol| protocol.as_ref() == PAIRING_CODE_V2_PROTOCOL);
            if supports_v1 && !supports_v2 {
                state.record_unsupported(peer_id);
            }
        }
        SwarmEvent::Behaviour(PairingBootstrapBehaviourEvent::PairingCodeV2(event)) => {
            return handle_pairing_event(swarm, state, identity, options, event);
        }
        _ => {}
    }
    drive_public_lookup(swarm, state, code, Instant::now());
    Ok(None)
}

fn handle_pairing_event(
    swarm: &mut Swarm<PairingBootstrapBehaviour>,
    state: &mut BootstrapState,
    identity: &NodeIdentity,
    options: &PairingBootstrapOptions,
    event: request_response::Event<PairingCodeV2Request, PairingCodeV2Response>,
) -> Result<Option<PairingBootstrapEnrollment>, PairingBootstrapError> {
    match event {
        request_response::Event::Message {
            peer,
            message:
                Message::Response {
                    request_id,
                    response,
                },
            ..
        } => handle_pairing_response(swarm, state, identity, options, peer, request_id, response),
        request_response::Event::OutboundFailure {
            peer,
            request_id,
            error,
            ..
        } => {
            let pending = state.requests.remove(&request_id);
            if matches!(
                error,
                request_response::OutboundFailure::UnsupportedProtocols
            ) {
                state.record_unsupported(peer);
            }
            match pending {
                Some(PendingRequest::Hello { peer, .. }) => {
                    state.release_candidate(peer, Instant::now());
                }
                Some(PendingRequest::Submit { peer, .. }) => {
                    state.selected_peer = None;
                    state.release_candidate(peer, Instant::now());
                }
                Some(PendingRequest::Poll { .. }) => {
                    if let Some(approval) = state.pending_approval.as_mut() {
                        approval.poll_in_flight = false;
                        approval.next_poll_at = Instant::now() + REQUEST_RETRY_DELAY;
                    }
                }
                None => {}
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn handle_pairing_response(
    swarm: &mut Swarm<PairingBootstrapBehaviour>,
    state: &mut BootstrapState,
    identity: &NodeIdentity,
    options: &PairingBootstrapOptions,
    peer: PeerId,
    request_id: request_response::OutboundRequestId,
    response: PairingCodeV2Response,
) -> Result<Option<PairingBootstrapEnrollment>, PairingBootstrapError> {
    let Some(pending) = state.requests.remove(&request_id) else {
        return Ok(None);
    };
    let expected_peer = match &pending {
        PendingRequest::Hello { peer, .. }
        | PendingRequest::Submit { peer, .. }
        | PendingRequest::Poll { peer, .. } => *peer,
    };
    if peer != expected_peer {
        state.release_candidate(expected_peer, Instant::now());
        return Ok(None);
    }

    match (pending, response) {
        (PendingRequest::Hello { pending, .. }, PairingCodeV2Response::Challenge { challenge }) => {
            handle_challenge_response(swarm, state, identity, options, peer, pending, &challenge)
        }
        (
            PendingRequest::Submit { offer, .. } | PendingRequest::Poll { offer, .. },
            PairingCodeV2Response::Pending {
                ticket,
                expires_at_unix_seconds,
            },
        ) => {
            state.pending_approval = Some(PendingApproval {
                peer,
                offer,
                ticket,
                expires_at_unix_seconds,
                next_poll_at: Instant::now() + REQUEST_RETRY_DELAY,
                poll_in_flight: false,
            });
            Ok(None)
        }
        (
            PendingRequest::Submit { offer, .. } | PendingRequest::Poll { offer, .. },
            PairingCodeV2Response::Accepted { response },
        ) => {
            if offer.payload.inviter_peer != peer.to_string() {
                return Err(PairingBootstrapError::TimedOut);
            }
            response
                .verify_for_offer_at(&offer, identity, current_unix_seconds())
                .map_err(crate::pairing_code::PairingCodeError::from)?;
            Ok(Some(PairingBootstrapEnrollment {
                offer,
                response: *response,
            }))
        }
        (PendingRequest::Hello { peer, .. }, PairingCodeV2Response::Rejected { .. }) => {
            // A discovery candidate has not authenticated as the inviter yet.
            state.release_candidate(peer, Instant::now());
            Ok(None)
        }
        (
            PendingRequest::Submit { peer, .. } | PendingRequest::Poll { peer, .. },
            PairingCodeV2Response::Rejected {
                reason: PairingCodeRejectionReason::Busy | PairingCodeRejectionReason::RateLimited,
            },
        ) => {
            state.selected_peer = None;
            state.pending_approval = None;
            state.release_candidate(peer, Instant::now());
            Ok(None)
        }
        (_, PairingCodeV2Response::Rejected { reason }) => {
            Err(PairingBootstrapError::Rejected(reason))
        }
        (PendingRequest::Hello { peer, .. }, _) => {
            state.release_candidate(peer, Instant::now());
            Ok(None)
        }
        _ => Err(PairingBootstrapError::TimedOut),
    }
}

fn handle_challenge_response(
    swarm: &mut Swarm<PairingBootstrapBehaviour>,
    state: &mut BootstrapState,
    identity: &NodeIdentity,
    options: &PairingBootstrapOptions,
    peer: PeerId,
    pending: PendingPairingCodeHelloV2,
    challenge: &crate::pairing_code::PairingCodeChallengeV2,
) -> Result<Option<PairingBootstrapEnrollment>, PairingBootstrapError> {
    if !state.may_accept_challenge_from(peer) {
        state.release_candidate(peer, Instant::now());
        return Ok(None);
    }
    let Ok((offer, session)) =
        open_pairing_code_challenge_v2_at(pending, challenge, peer, current_unix_seconds())
    else {
        state.release_candidate(peer, Instant::now());
        return Ok(None);
    };
    if options.existing_network_names.iter().any(|network_name| {
        network_name.to_lowercase() == offer.payload.network_name.to_lowercase()
    }) {
        return Err(PairingBootstrapError::AlreadyJoined {
            network_name: offer.payload.network_name,
        });
    }
    state.v2_challenge_opened = true;
    state.selected_peer = Some(peer);
    let mut request = build_named_pairing_request_at(
        &offer,
        PairingRequestOptions {
            identity: identity.clone(),
            requested_vpn_ip: options.requested_vpn_ip.clone(),
            requested_routes: options.requested_routes.clone(),
        },
        options.requested_hostname.as_deref(),
        current_unix_seconds(),
    )
    .map_err(crate::pairing_code::PairingCodeError::from)?;
    authenticate_pairing_request(&mut request, &session)?;
    let request_id = swarm.behaviour_mut().pairing_code_v2.send_request(
        &peer,
        PairingCodeV2Request::Submit {
            request: Box::new(request),
        },
    );
    state
        .requests
        .insert(request_id, PendingRequest::Submit { peer, offer });
    Ok(None)
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn standalone_public_lookup_retries_after_query_capacity_is_released() {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let mut swarm = build_bootstrap_swarm(&identity).unwrap();
        let local = *swarm.local_peer_id();
        let mut config =
            controlled_kademlia_config(StreamProtocol::new(PUBLIC_IPFS_KADEMLIA_PROTOCOL));
        config.set_query_pool_capacity(std::num::NonZeroUsize::MIN);
        swarm.behaviour_mut().kad =
            kad::Behaviour::with_config(local, kad::store::MemoryStore::new(local), config);
        let held = swarm
            .behaviour_mut()
            .kad
            .get_closest_peers(PeerId::random());
        let now = Instant::now();
        let mut state = BootstrapState::new(now, Duration::ZERO);
        let code = PairingCode::generate();

        drive_public_lookup(&mut swarm, &mut state, &code, now);
        assert!(state.public_lookup_ids.is_empty());
        assert_eq!(state.public_lookup_attempts, 0);
        assert_eq!(state.next_public_lookup_at, now + PUBLIC_LOOKUP_INTERVAL);
        assert!(swarm.behaviour_mut().kad.cancel_query(&held));
        drive_public_lookup(&mut swarm, &mut state, &code, now);
        assert!(state.public_lookup_ids.is_empty());

        drive_public_lookup(&mut swarm, &mut state, &code, now + PUBLIC_LOOKUP_INTERVAL);
        assert_eq!(state.public_lookup_attempts, 1);
        assert_eq!(state.public_lookup_ids.len(), 1);
        let query = *state.public_lookup_ids.iter().next().unwrap();
        assert!(swarm.behaviour().kad.query_is_retained(&query));
        assert_eq!(swarm.behaviour().kad.query_pool_usage().retained, 1);
    }

    #[tokio::test]
    async fn standalone_bootstrap_bounds_routing_addresses_and_preserves_seed() {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let mut swarm = build_bootstrap_swarm(&identity).unwrap();
        // Do not poll the swarm or contact public seeds. Exercise the actual
        // constructor's routing owner, including its seed protection.
        let (peer, seed) = public_ipfs_bootstrap_peer_configs()[0]
            .peer_address()
            .unwrap();
        let kad = &mut swarm.behaviour_mut().kad;
        assert_eq!(kad.query_pool_usage().capacity, Some(32));
        assert_eq!(kad.background_job_usage().bounded_jobs, 2);
        assert_eq!(kad.behaviour_queue_usage().event_limit, Some(512));
        assert_eq!(
            kad.behaviour_queue_usage().byte_limit,
            Some(4 * 1024 * 1024)
        );
        assert!(matches!(
            kad.try_get_closest_peers(vec![0; 256 * 1024 + 1]),
            Err(kad::QueryStartError::InputTooLarge(_))
        ));
        for index in 1..=100 {
            kad.add_address(&peer, format!("/memory/{index}").parse().unwrap());
        }
        let oversized = Multiaddr::empty().with(Protocol::Dns("a".repeat(2048).into()));
        assert_eq!(
            kad.add_address(&peer, oversized),
            kad::RoutingUpdate::Failed
        );
        let addresses = kad.remove_peer(&peer).unwrap().node.value;
        assert_eq!(addresses.len(), 64);
        assert!(
            addresses
                .iter()
                .any(|a| a == &seed.clone().with_p2p(peer).unwrap())
        );
        assert!(addresses.iter().all(|a| a.len() <= 2048));
    }

    #[test]
    fn options_reject_unbounded_timeouts() {
        let mut options = PairingBootstrapOptions {
            timeout: Duration::ZERO,
            ..PairingBootstrapOptions::default()
        };
        assert!(matches!(
            validate_options(&options),
            Err(PairingBootstrapError::InvalidTimeout)
        ));

        options.timeout = MAX_PAIRING_BOOTSTRAP_TIMEOUT + Duration::from_secs(1);
        assert!(matches!(
            validate_options(&options),
            Err(PairingBootstrapError::InvalidTimeout)
        ));
    }

    #[test]
    fn options_reject_unbounded_existing_networks() {
        let options = PairingBootstrapOptions {
            existing_network_names: vec![
                "network".to_owned();
                MAX_PAIRING_BOOTSTRAP_EXISTING_NETWORKS + 1
            ],
            ..PairingBootstrapOptions::default()
        };

        assert!(matches!(
            validate_options(&options),
            Err(PairingBootstrapError::Build(_))
        ));
    }

    #[test]
    fn options_reject_unbounded_or_invalid_candidate_hints() {
        let mut options = PairingBootstrapOptions {
            candidate_hints: vec![
                BootstrapPeerConfig {
                    id: PeerId::random().to_string(),
                    address: "/ip4/127.0.0.1/tcp/1".to_owned(),
                };
                MAX_PAIRING_BOOTSTRAP_CANDIDATE_HINTS + 1
            ],
            ..PairingBootstrapOptions::default()
        };
        assert!(matches!(
            validate_options(&options),
            Err(PairingBootstrapError::Build(_))
        ));

        options.candidate_hints = vec![BootstrapPeerConfig {
            id: "not-a-peer-id".to_owned(),
            address: "/ip4/127.0.0.1/tcp/1".to_owned(),
        }];
        assert!(matches!(
            validate_options(&options),
            Err(PairingBootstrapError::Build(_))
        ));
    }

    #[test]
    fn candidate_set_is_bounded() {
        let now = Instant::now();
        let mut state = BootstrapState::new(now, PAIRING_BOOTSTRAP_LAN_GRACE);
        for _ in 0..(MAX_PAIRING_BOOTSTRAP_CANDIDATES + 10) {
            state.record_public_candidate(PeerId::random(), now);
        }
        assert_eq!(state.candidates.len(), MAX_PAIRING_BOOTSTRAP_CANDIDATES);
    }

    #[test]
    fn lan_addresses_are_bounded_per_candidate() {
        let now = Instant::now();
        let mut state = BootstrapState::new(now, PAIRING_BOOTSTRAP_LAN_GRACE);
        let peer = PeerId::random();
        for port in
            1..=u16::try_from(MAX_PAIRING_BOOTSTRAP_ADDRESSES_PER_PEER + 4).expect("port count")
        {
            state.record_lan_candidate(
                peer,
                format!("/ip4/127.0.0.1/tcp/{port}")
                    .parse()
                    .expect("address"),
                now,
            );
        }

        assert_eq!(
            state
                .candidates
                .get(&peer)
                .expect("candidate")
                .lan_addresses
                .len(),
            MAX_PAIRING_BOOTSTRAP_ADDRESSES_PER_PEER
        );
    }

    #[test]
    fn only_public_provider_evidence_requests_upgrade() {
        let now = Instant::now();
        let mut state = BootstrapState::new(now, PAIRING_BOOTSTRAP_LAN_GRACE);
        let lan_peer = PeerId::random();
        let public_peer = PeerId::random();
        state.record_lan_candidate(
            lan_peer,
            "/ip4/127.0.0.1/tcp/1".parse().expect("address"),
            now,
        );
        state.record_public_candidate(public_peer, now);
        state.record_unsupported(lan_peer);
        state.record_unsupported(public_peer);

        assert!(matches!(
            state.terminal_timeout_error(),
            PairingBootstrapError::UpgradeRequired { peers: 1 }
        ));
    }

    #[test]
    fn authenticated_inviter_selection_is_sticky() {
        let now = Instant::now();
        let mut state = BootstrapState::new(now, PAIRING_BOOTSTRAP_LAN_GRACE);
        let selected = PeerId::random();
        let other = PeerId::random();

        assert!(state.may_accept_challenge_from(selected));
        state.selected_peer = Some(selected);
        assert!(state.may_accept_challenge_from(selected));
        assert!(!state.may_accept_challenge_from(other));
    }

    #[tokio::test]
    async fn hello_rejection_cannot_terminate_another_selected_inviter() {
        for reason in [
            PairingCodeRejectionReason::InvalidRequest,
            PairingCodeRejectionReason::UserRejected,
            PairingCodeRejectionReason::Expired,
            PairingCodeRejectionReason::Unavailable,
            PairingCodeRejectionReason::Busy,
            PairingCodeRejectionReason::RateLimited,
        ] {
            let identity = NodeIdentity::generate_ed25519().unwrap();
            let mut swarm = build_bootstrap_swarm(&identity).unwrap();
            let local = *swarm.local_peer_id();
            let now = Instant::now();
            let mut state = BootstrapState::new(now, PAIRING_BOOTSTRAP_LAN_GRACE);
            let selected = PeerId::random();
            let other = PeerId::random();
            state.record_public_candidate(other, now);
            drive_hellos(
                &mut swarm,
                &mut state,
                &identity,
                &PairingCode::generate(),
                local,
            );
            let id = *state.requests.keys().next().unwrap();
            assert!(state.candidates[&other].in_flight);
            // Model selection by an earlier authenticated challenge while this Hello is pending.
            state.selected_peer = Some(selected);
            let result = handle_pairing_event(
                &mut swarm,
                &mut state,
                &identity,
                &PairingBootstrapOptions::default(),
                request_response::Event::Message {
                    peer: other,
                    connection_id: libp2p::swarm::ConnectionId::new_unchecked(1),
                    message: Message::Response {
                        request_id: id,
                        response: PairingCodeV2Response::Rejected { reason },
                    },
                },
            );
            assert!(
                result.is_ok(),
                "unselected Hello rejection terminated the join: {result:?}"
            );
            assert!(result.unwrap().is_none());
            assert_eq!(state.selected_peer, Some(selected));
            assert!(state.requests.is_empty());
            assert!(!state.candidates[&other].in_flight);
            let retry = state.candidates[&other].next_attempt_at;
            assert!(retry > now);
            assert!(state.next_candidate(local, retry).is_none());
            // Losing selection later must still permit bounded discovery, not strand the candidate.
            state.selected_peer = None;
            assert!(state.next_candidate(local, now).is_none());
            assert_eq!(state.next_candidate(local, retry), Some(other));
        }
    }

    #[tokio::test]
    async fn selected_submit_poll_rejection_remains_terminal() {
        let identity = NodeIdentity::generate_ed25519().unwrap();
        let inviter = NodeIdentity::generate_ed25519().unwrap();
        let peer = inviter.peer_id.parse().unwrap();
        let config = serde_json::from_value(serde_json::json!({
            "network": { "name": "lab", "local_peer": inviter.peer_id, "private_key": inviter.private_key },
            "peers": []
        })).unwrap();
        let offer = crate::pairing::export_code_pairing_offer_at(
            &config,
            crate::pairing::PairingOfferOptions {
                expires_in_seconds: 600,
                rendezvous_token: None,
            },
            current_unix_seconds(),
        )
        .unwrap();
        for poll in [false, true] {
            let mut swarm = build_bootstrap_swarm(&identity).unwrap();
            let mut state = BootstrapState::new(Instant::now(), PAIRING_BOOTSTRAP_LAN_GRACE);
            state.selected_peer = Some(peer);
            let id = swarm.behaviour_mut().pairing_code_v2.send_request(
                &peer,
                PairingCodeV2Request::Poll {
                    ticket: "fixture".to_owned(),
                },
            );
            state.requests.insert(
                id,
                if poll {
                    PendingRequest::Poll {
                        peer,
                        offer: offer.clone(),
                    }
                } else {
                    PendingRequest::Submit {
                        peer,
                        offer: offer.clone(),
                    }
                },
            );
            let result = handle_pairing_response(
                &mut swarm,
                &mut state,
                &identity,
                &PairingBootstrapOptions::default(),
                peer,
                id,
                PairingCodeV2Response::Rejected {
                    reason: PairingCodeRejectionReason::UserRejected,
                },
            );
            assert!(matches!(
                result,
                Err(PairingBootstrapError::Rejected(
                    PairingCodeRejectionReason::UserRejected
                ))
            ));
            assert!(!state.requests.contains_key(&id));
        }
    }

    #[tokio::test]
    async fn rejected_hello_retries_over_loopback_and_completes_authenticated_pairing() {
        use crate::membership::{
            MembershipRecordIssueOptions, MembershipRecordSubject, MembershipRole,
            issue_membership_record_for_subject_at,
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            let identity = NodeIdentity::generate_ed25519().unwrap();
            let inviter = NodeIdentity::generate_ed25519().unwrap();
            let peer = inviter.peer_id.parse().unwrap();
            let local = identity.peer_id.parse().unwrap();
            let config = serde_json::from_value(serde_json::json!({
                "network": { "name": "lab", "local_peer": inviter.peer_id, "private_key": inviter.private_key },
                "peers": []
            })).unwrap();
            let code = PairingCode::generate();
            let mut client = build_bootstrap_swarm(&identity).unwrap();
            let mut server = build_bootstrap_swarm(&inviter).unwrap();
            // Remove seeded bootstrap queries before polling either swarm.
            for swarm in [&mut client, &mut server] {
                swarm.behaviour_mut().kad = kad::Behaviour::with_config(
                    *swarm.local_peer_id(), kad::store::MemoryStore::new(*swarm.local_peer_id()),
                    controlled_kademlia_config(StreamProtocol::new(PUBLIC_IPFS_KADEMLIA_PROTOCOL)),
                );
            }
            let address = loop {
                if let SwarmEvent::NewListenAddr { address, .. } = server.select_next_some().await {
                    if let Some(libp2p::multiaddr::Protocol::Tcp(port)) = address.iter().find(|p| matches!(p, libp2p::multiaddr::Protocol::Tcp(_))) {
                        break format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap();
                    }
                }
            };
            client.dial(libp2p::swarm::dial_opts::DialOpts::peer_id(peer).addresses(vec![address]).build()).unwrap();
            let mut state = BootstrapState::new(Instant::now(), PAIRING_BOOTSTRAP_LAN_GRACE);
            state.record_public_candidate(peer, Instant::now());
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            let mut hellos = 0;
            let mut session = None;
            let mut server_offer = None;
            let mut rejection_at = None;
            let enrollment = loop {
                tokio::select! {
                    _ = tick.tick() => drive_hellos(&mut client, &mut state, &identity, &code, local),
                    event = client.select_next_some() => {
                        if let SwarmEvent::Behaviour(PairingBootstrapBehaviourEvent::PairingCodeV2(event)) = event {
                            if let Some(enrollment) = handle_pairing_event(&mut client, &mut state, &identity,
                                &PairingBootstrapOptions::default(), event).unwrap() { break enrollment; }
                        }
                    }
                    event = server.select_next_some() => {
                        if let SwarmEvent::Behaviour(PairingBootstrapBehaviourEvent::PairingCodeV2(request_response::Event::Message {
                            peer: sender, message: Message::Request { request, channel, .. }, ..
                        })) = event {
                            assert_eq!(sender, local);
                            let reply = match request {
                                PairingCodeV2Request::Hello { hello } => {
                                    hellos += 1;
                                    if hellos == 1 {
                                        rejection_at = Some(Instant::now());
                                        PairingCodeV2Response::Rejected { reason: PairingCodeRejectionReason::InvalidRequest }
                                    } else {
                                        assert_eq!(hellos, 2);
                                        assert!(rejection_at.unwrap().elapsed() >= REQUEST_RETRY_DELAY);
                                        let (challenge, authenticated) = crate::pairing_code::answer_pairing_code_hello_v2_at(
                                            &config, &code, &hello, sender,
                                            crate::pairing::PairingOfferOptions { expires_in_seconds: 600, rendezvous_token: None },
                                            current_unix_seconds(),
                                        ).unwrap();
                                        server_offer = Some(crate::pairing::export_code_pairing_offer_at(
                                            &config, crate::pairing::PairingOfferOptions {
                                                expires_in_seconds: 600,
                                                rendezvous_token: Some(authenticated.rendezvous_token().to_owned()),
                                            }, challenge.payload.issued_at_unix_seconds,
                                        ).unwrap());
                                        session = Some(authenticated);
                                        PairingCodeV2Response::Challenge { challenge: Box::new(challenge) }
                                    }
                                }
                                PairingCodeV2Request::Submit { request } => {
                                    crate::pairing_code::verify_pairing_request_code_authentication(&request, session.as_ref().unwrap()).unwrap();
                                    let offer = server_offer.as_ref().unwrap();
                                    let records = [&inviter, &identity].into_iter().map(|member| {
                                        issue_membership_record_for_subject_at(&inviter, MembershipRecordIssueOptions {
                                            network_name: "lab".to_owned(), member: MembershipRecordSubject::from_identity(member).unwrap(),
                                            membership_epoch: 1, sequence: 1, revoked: false,
                                            roles: vec![MembershipRole::OverlayMember], route_grants: vec![], expires_at_unix_seconds: None,
                                        }, current_unix_seconds()).unwrap()
                                    }).collect();
                                    let response = crate::pairing::build_pairing_response_at(&config, offer,
                                        crate::pairing::PairingResponseOptions {
                                            joiner_peer: sender.to_string(), assigned_vpn_ip: None,
                                            membership_key: None, member_records: records, expires_in_seconds: 600,
                                        }, current_unix_seconds(),
                                    ).unwrap();
                                    PairingCodeV2Response::Accepted { response: Box::new(response) }
                                }
                                PairingCodeV2Request::Poll { .. } => panic!("immediate acceptance needs no poll"),
                            };
                            server.behaviour_mut().pairing_code_v2.send_response(channel, reply).unwrap();
                        }
                    }
                }
            };
            assert_eq!(hellos, 2);
            assert!(state.requests.is_empty());
            assert_eq!(enrollment.offer.payload.inviter_peer, peer.to_string());
            enrollment.response.verify_for_offer_at(&enrollment.offer, &identity, current_unix_seconds()).unwrap();
        }).await.expect("loopback V2 pairing deadline");
    }
}
