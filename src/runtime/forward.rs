use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use libp2p::{PeerId as Libp2pPeerId, Swarm, identity::Keypair as Libp2pKeypair, request_response};

use crate::{
    PeerId, Sequence, SessionId,
    config::{Config, ConfigError, RouteConfig},
    hostname::{
        HostnameRecordError, HostnameRecordMergeStats, MAX_HOSTNAME_RECORD_INTEGER,
        MAX_HOSTNAME_RECORDS, SignedHostnameRecord, effective_hostname_records,
        issue_hostname_record_at, merge_hostname_records,
    },
    identity::NodeIdentity,
    membership::{
        MAX_MEMBERSHIP_RECORDS, MembershipRecordError, MembershipRecordMergeStats,
        SignedMembershipRecord, effective_membership_at, membership_trust_anchors,
        merge_membership_records_at,
    },
    queue::{EnqueueError, Packet, PeerQueues},
    route::{IpCidr, RouteError, RouteTable},
    runtime::{
        control::ControlRoute,
        p2p::Behaviour,
        packet::{AuthorizedPeers, PacketResponse},
    },
    wire::{Frame, FrameError, PayloadType},
};

#[derive(Debug)]
pub struct Forwarder {
    local_peer: PeerId,
    config: Config,
    member_records: Vec<SignedMembershipRecord>,
    hostname_records: Vec<SignedHostnameRecord>,
    routes: RouteTable,
    peers: HashMap<PeerId, Libp2pPeerId>,
    authorized_peers: AuthorizedPeers,
    membership_revision: u64,
    membership_effective_refresh_pending: bool,
    replay_windows: HashMap<(PeerId, SessionId), ReplayWindow>,
    replay_session_ttl: Duration,
    max_replay_windows: usize,
    session_id: SessionId,
    next_sequence: Sequence,
    mtu: usize,
}

#[derive(Debug)]
pub struct ForwarderUpdate {
    config: Config,
    member_records: Vec<SignedMembershipRecord>,
    routes: RouteTable,
    peers: HashMap<PeerId, Libp2pPeerId>,
    authorized_peers: AuthorizedPeers,
    mtu: usize,
}

const REPLAY_WINDOW_BITS: u64 = 64;
const DEFAULT_REPLAY_SESSION_TTL: Duration = Duration::from_mins(15);
const MAX_REPLAY_WINDOWS: usize = 4096;
pub const MAX_RETAINED_MEMBERSHIP_RECORDS: usize = MAX_MEMBERSHIP_RECORDS;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReplayWindow {
    highest: Option<Sequence>,
    seen: u64,
    updated_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReplayAcceptError {
    Duplicate,
    TooOld,
}

impl ReplayWindow {
    fn new(now: Instant) -> Self {
        Self {
            highest: None,
            seen: 0,
            updated_at: now,
        }
    }

    fn accept(&mut self, sequence: Sequence, now: Instant) -> Result<(), ReplayAcceptError> {
        let Some(highest) = self.highest else {
            self.highest = Some(sequence);
            self.seen = 1;
            self.updated_at = now;
            return Ok(());
        };

        if sequence > highest {
            let shift = sequence - highest;
            self.seen = if shift >= REPLAY_WINDOW_BITS {
                1
            } else {
                (self.seen << shift) | 1
            };
            self.highest = Some(sequence);
            self.updated_at = now;
            return Ok(());
        }

        let offset = highest - sequence;
        if offset >= REPLAY_WINDOW_BITS {
            return Err(ReplayAcceptError::TooOld);
        }
        let bit = 1_u64 << offset;
        if self.seen & bit != 0 {
            return Err(ReplayAcceptError::Duplicate);
        }

        self.seen |= bit;
        self.updated_at = now;
        Ok(())
    }

    fn is_expired(self, now: Instant, ttl: Duration) -> bool {
        now.saturating_duration_since(self.updated_at) > ttl
    }
}

impl Forwarder {
    pub fn from_config(config: &Config) -> Result<Self, ForwardError> {
        let member_records = config.network.member_records.clone();
        let now_unix_seconds = current_unix_seconds_lossy();

        let local_peer = config.local_peer_id()?;

        Ok(Self {
            local_peer,
            config: config.clone(),
            member_records: member_records.clone(),
            hostname_records: Vec::new(),
            routes: config
                .compile_routes_with_member_records_at(&member_records, now_unix_seconds)?,
            peers: transport_peers_from_config_and_records(
                config,
                &member_records,
                now_unix_seconds,
            )?,
            authorized_peers: authorized_peers_from_config_and_records(
                config,
                &member_records,
                now_unix_seconds,
            )?,
            membership_revision: 0,
            membership_effective_refresh_pending: false,
            replay_windows: HashMap::new(),
            replay_session_ttl: DEFAULT_REPLAY_SESSION_TTL,
            max_replay_windows: MAX_REPLAY_WINDOWS,
            session_id: fresh_session_id_for_peer(local_peer),
            next_sequence: 0,
            mtu: usize::from(config.effective_packet_mtu()),
        })
    }

    #[must_use]
    pub const fn mtu(&self) -> usize {
        self.mtu
    }

    pub fn send_tun_packet(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        packet: Vec<u8>,
    ) -> Result<request_response::OutboundRequestId, ForwardError> {
        let packet = self.prepare_tun_packet(packet)?;
        self.send_queued_packet(swarm, &packet)
    }

    pub fn enqueue_tun_packet(
        &mut self,
        queues: &mut PeerQueues,
        packet: Vec<u8>,
    ) -> Result<(), ForwardError> {
        let packet = self.prepare_tun_packet(packet)?;
        Ok(queues.enqueue(packet)?)
    }

    pub fn send_queued_packet(
        &self,
        swarm: &mut Swarm<Behaviour>,
        packet: &Packet,
    ) -> Result<request_response::OutboundRequestId, ForwardError> {
        self.send_queued_packet_with_mtu(swarm, packet, self.mtu_u16())
    }

    pub fn send_queued_packet_with_mtu(
        &self,
        swarm: &mut Swarm<Behaviour>,
        packet: &Packet,
        peer_mtu: u16,
    ) -> Result<request_response::OutboundRequestId, ForwardError> {
        let peer = self
            .peers
            .get(&packet.peer())
            .ok_or(ForwardError::NoTransportPeer(packet.peer()))?;
        let frame = self.queued_packet_frame_with_mtu(packet, peer_mtu)?;

        Ok(swarm.behaviour_mut().packet.send_request(peer, frame))
    }

    pub fn queued_packet_frame_with_mtu(
        &self,
        packet: &Packet,
        peer_mtu: u16,
    ) -> Result<Frame, ForwardError> {
        let max = self.mtu.min(usize::from(peer_mtu));
        if packet.payload().len() > max {
            return Err(ForwardError::PacketTooLarge {
                actual: packet.payload().len(),
                max,
            });
        }

        self.packet_frame(packet)
    }

    #[must_use]
    pub fn is_configured_transport_peer(&self, peer: Libp2pPeerId) -> bool {
        self.peers.values().any(|configured| *configured == peer)
    }

    pub fn configured_overlay_peers(&self) -> impl Iterator<Item = PeerId> + '_ {
        self.peers.keys().copied()
    }

    pub fn configured_transport_peers(&self) -> impl Iterator<Item = Libp2pPeerId> + '_ {
        self.peers.values().copied()
    }

    pub fn merge_membership_records(
        &mut self,
        records: &[SignedMembershipRecord],
        now_unix_seconds: u64,
    ) -> Result<MembershipRecordMergeStats, ForwardError> {
        let mut trusted_issuers =
            membership_trust_anchors(&self.member_records, &self.config.network.name)?;
        let has_explicit_root_record = self
            .member_records
            .iter()
            .any(|record| record.payload.issuer_peer == record.payload.member_peer);
        if trusted_issuers.is_empty() && !has_explicit_root_record {
            trusted_issuers.insert(self.config.local_peer()?);
        }
        self.merge_membership_records_with_trusted_issuers(
            records,
            now_unix_seconds,
            &trusted_issuers,
        )
    }

    pub(crate) fn restore_persisted_membership_records(
        &mut self,
        records: &[SignedMembershipRecord],
        now_unix_seconds: u64,
    ) -> Result<MembershipRecordMergeStats, ForwardError> {
        let mut anchor_records = self.member_records.clone();
        anchor_records.extend(
            records
                .iter()
                .filter(|record| record.payload.issuer_peer == record.payload.member_peer)
                .cloned(),
        );
        let mut trusted_issuers =
            membership_trust_anchors(&anchor_records, &self.config.network.name)?;
        if trusted_issuers.is_empty() {
            trusted_issuers.insert(self.config.local_peer()?);
        }
        self.merge_membership_records_with_trusted_issuers(
            records,
            now_unix_seconds,
            &trusted_issuers,
        )
    }

    fn merge_membership_records_with_trusted_issuers(
        &mut self,
        records: &[SignedMembershipRecord],
        now_unix_seconds: u64,
        trusted_issuers: &crate::membership::TrustedMembershipIssuers,
    ) -> Result<MembershipRecordMergeStats, ForwardError> {
        let mut member_records = self.member_records.clone();
        let stats = merge_membership_records_at(
            &mut member_records,
            records,
            &self.config.network.name,
            now_unix_seconds,
            trusted_issuers,
            MAX_RETAINED_MEMBERSHIP_RECORDS,
        )?;
        let routes = self
            .config
            .compile_routes_with_member_records_at(&member_records, now_unix_seconds)?;
        let authorized_peers = authorized_peers_from_config_and_records(
            &self.config,
            &member_records,
            now_unix_seconds,
        )?;
        let peers = transport_peers_from_config_and_records(
            &self.config,
            &member_records,
            now_unix_seconds,
        )?;
        let records_changed = member_records != self.member_records;
        let effective_changed = routes != self.routes
            || peers != self.peers
            || authorized_peers != self.authorized_peers;
        if records_changed || effective_changed {
            self.membership_revision = self.membership_revision.wrapping_add(1);
        }
        self.membership_effective_refresh_pending |= effective_changed;
        self.peers = peers;
        self.routes = routes;
        self.authorized_peers = authorized_peers;
        self.member_records = member_records;
        Ok(stats)
    }

    pub fn merge_hostname_records(
        &mut self,
        records: &[SignedHostnameRecord],
    ) -> Result<HostnameRecordMergeStats, ForwardError> {
        let stats = merge_hostname_records(
            &mut self.hostname_records,
            records,
            &self.config.network.name,
            MAX_HOSTNAME_RECORDS,
        )?;
        if stats.accepted > 0 {
            self.membership_revision = self.membership_revision.wrapping_add(1);
        }
        Ok(stats)
    }

    pub fn reconcile_local_hostname_record(
        &mut self,
        identity: &NodeIdentity,
        now_unix_seconds: u64,
    ) -> Result<bool, ForwardError> {
        let local_peer = PeerId::from_libp2p(identity.peer_id.parse()?);
        if local_peer != self.local_peer {
            return Err(ForwardError::LocalPeerChanged {
                expected: self.local_peer,
                actual: local_peer,
            });
        }
        let Some(hostname) = self.config.network.dns.hostname.as_deref() else {
            return Ok(false);
        };
        let current = self
            .hostname_records
            .iter()
            .find(|record| record.payload.peer == identity.peer_id);
        if current.is_some_and(|record| record.payload.hostname == hostname) {
            return Ok(false);
        }
        let sequence = current
            .map_or(0, |record| record.payload.sequence)
            .checked_add(1)
            .filter(|sequence| *sequence <= MAX_HOSTNAME_RECORD_INTEGER)
            .ok_or(ForwardError::HostnameSequenceExhausted)?;
        let record = issue_hostname_record_at(
            identity,
            &self.config.network.name,
            hostname,
            sequence,
            now_unix_seconds,
        )?;
        Ok(self.merge_hostname_records(&[record])?.accepted == 1)
    }

    pub fn prepare_reconfigure(
        &self,
        config: Config,
        now_unix_seconds: u64,
    ) -> Result<ForwarderUpdate, ForwardError> {
        let local_peer = config.local_peer_id()?;
        if local_peer != self.local_peer {
            return Err(ForwardError::LocalPeerChanged {
                expected: self.local_peer,
                actual: local_peer,
            });
        }

        let member_records = config.network.member_records.clone();
        let routes =
            config.compile_routes_with_member_records_at(&member_records, now_unix_seconds)?;
        let peers =
            transport_peers_from_config_and_records(&config, &member_records, now_unix_seconds)?;
        let authorized_peers =
            authorized_peers_from_config_and_records(&config, &member_records, now_unix_seconds)?;
        let mtu = usize::from(config.effective_packet_mtu());

        Ok(ForwarderUpdate {
            config,
            member_records,
            routes,
            peers,
            authorized_peers,
            mtu,
        })
    }

    pub fn commit_reconfigure(&mut self, update: ForwarderUpdate) {
        if self.member_records != update.member_records {
            self.membership_revision = self.membership_revision.wrapping_add(1);
        }
        self.config = update.config;
        self.member_records = update.member_records;
        self.routes = update.routes;
        self.peers = update.peers;
        self.authorized_peers = update.authorized_peers;
        self.membership_effective_refresh_pending = false;
        self.mtu = update.mtu;
    }

    pub fn prune_membership_records(
        &mut self,
        now_unix_seconds: u64,
    ) -> Result<MembershipRecordMergeStats, ForwardError> {
        self.merge_membership_records(&[], now_unix_seconds)
    }

    pub(crate) fn refresh_membership_records(
        &mut self,
        now_unix_seconds: u64,
    ) -> Result<(MembershipRecordMergeStats, bool), ForwardError> {
        let stats = self.prune_membership_records(now_unix_seconds)?;
        let effective_changed = self.take_membership_effective_refresh_pending();
        Ok((stats, effective_changed))
    }

    pub(crate) fn take_membership_effective_refresh_pending(&mut self) -> bool {
        std::mem::take(&mut self.membership_effective_refresh_pending)
    }

    #[must_use]
    pub fn member_record_count(&self) -> usize {
        self.member_records.len()
    }

    #[must_use]
    pub const fn membership_revision(&self) -> u64 {
        self.membership_revision
    }

    #[must_use]
    pub fn member_records(&self) -> &[SignedMembershipRecord] {
        &self.member_records
    }

    #[must_use]
    pub fn hostname_records(&self) -> &[SignedHostnameRecord] {
        &self.hostname_records
    }

    pub fn effective_hostname_records(&self) -> Result<HashMap<PeerId, String>, ForwardError> {
        Ok(effective_hostname_records(
            &self.hostname_records,
            &self.config.network.name,
        )?)
    }

    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.config
    }

    #[must_use]
    pub fn replay_window_count(&self) -> usize {
        self.replay_windows.len()
    }

    pub fn expire_replay_sessions(&mut self) -> usize {
        self.expire_replay_windows_at(Instant::now())
    }

    #[must_use]
    pub fn transport_peer_for_overlay(&self, peer: PeerId) -> Option<Libp2pPeerId> {
        self.peers.get(&peer).copied()
    }

    #[must_use]
    pub fn local_advertised_routes(&self) -> Vec<ControlRoute> {
        self.routes
            .routes_for(self.local_peer)
            .map(|route| ControlRoute::new(route.prefix.to_string(), route.metric))
            .collect()
    }

    pub fn local_advertised_route_prefixes(&self) -> impl Iterator<Item = IpCidr> + '_ {
        self.routes
            .routes_for(self.local_peer)
            .map(|route| route.prefix)
    }

    #[must_use]
    pub fn authorizes_advertised_routes(
        &self,
        peer: Libp2pPeerId,
        routes: &[ControlRoute],
    ) -> bool {
        let owner = PeerId::from_libp2p(peer);
        routes.iter().all(|route| {
            let Ok(prefix) = (RouteConfig {
                prefix: route.prefix.clone(),
                metric: route.metric,
            })
            .prefix() else {
                return false;
            };

            self.routes.authorizes_route(owner, prefix)
        })
    }

    fn packet_frame(&self, packet: &Packet) -> Result<Frame, ForwardError> {
        Ok(Frame::packet(
            self.session_id,
            packet.sequence(),
            packet.payload().to_vec(),
        )?)
    }

    pub fn send_path_probe_with_mtu(
        &mut self,
        swarm: &mut Swarm<Behaviour>,
        peer: PeerId,
        peer_mtu: u16,
        payload: &[u8],
    ) -> Result<request_response::OutboundRequestId, ForwardError> {
        let transport_peer = self
            .peers
            .get(&peer)
            .copied()
            .ok_or(ForwardError::NoTransportPeer(peer))?;
        let frame = self.path_probe_frame_with_mtu(peer_mtu, payload)?;

        Ok(swarm
            .behaviour_mut()
            .packet
            .send_request(&transport_peer, frame))
    }

    pub fn path_probe_frame_with_mtu(
        &mut self,
        peer_mtu: u16,
        payload: &[u8],
    ) -> Result<Frame, ForwardError> {
        let max = self.mtu.min(usize::from(peer_mtu));
        if payload.len() > max {
            return Err(ForwardError::PacketTooLarge {
                actual: payload.len(),
                max,
            });
        }

        let frame = Frame::path_probe(self.session_id, self.next_sequence, payload.to_vec())?;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        Ok(frame)
    }

    fn mtu_u16(&self) -> u16 {
        u16::try_from(self.mtu).unwrap_or(u16::MAX)
    }

    fn prepare_tun_packet(&mut self, packet: Vec<u8>) -> Result<Packet, ForwardError> {
        if packet.len() > self.mtu {
            return Err(ForwardError::PacketTooLarge {
                actual: packet.len(),
                max: self.mtu,
            });
        }

        let source = packet_source(&packet)?;
        self.authorize_local_source(source)?;

        let destination = packet_destination(&packet)?;
        let route = self
            .routes
            .resolve(destination)
            .ok_or(ForwardError::NoRoute(destination))?;
        if !self.peers.contains_key(&route.owner) {
            return Err(ForwardError::NoTransportPeer(route.owner));
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);

        Ok(Packet::new(route.owner, sequence, packet))
    }

    fn authorize_local_source(&self, source: IpAddr) -> Result<(), ForwardError> {
        self.routes
            .authorize_source(self.local_peer, source)
            .map_err(|_| ForwardError::UnauthorizedLocalSource { source })
    }

    pub fn accept_inbound_packet<'a>(
        &mut self,
        peer: Libp2pPeerId,
        frame: &'a Frame,
    ) -> Result<&'a [u8], ForwardError> {
        self.validate_inbound_frame_metadata(peer, frame, PayloadType::IpPacket)?;

        let overlay_peer = PeerId::from_libp2p(peer);
        let source = packet_source(&frame.payload)?;
        self.routes.authorize_source(overlay_peer, source)?;
        let destination = packet_destination(&frame.payload)?;
        self.authorize_local_destination(destination)?;
        self.accept_sequence(overlay_peer, frame.header.session_id, frame.header.sequence)?;

        Ok(&frame.payload)
    }

    pub fn accept_inbound_stream_packet<'a>(
        &self,
        peer: Libp2pPeerId,
        frame: &'a Frame,
    ) -> Result<&'a [u8], ForwardError> {
        self.validate_inbound_frame_metadata(peer, frame, PayloadType::IpPacket)?;
        let overlay_peer = PeerId::from_libp2p(peer);
        let source = packet_source(&frame.payload)?;
        self.routes.authorize_source(overlay_peer, source)?;
        let destination = packet_destination(&frame.payload)?;
        self.authorize_local_destination(destination)?;

        Ok(&frame.payload)
    }

    pub fn accept_inbound_control_frame(
        &mut self,
        peer: Libp2pPeerId,
        frame: &Frame,
        expected_payload_type: PayloadType,
    ) -> Result<(), ForwardError> {
        self.validate_inbound_frame_metadata(peer, frame, expected_payload_type)?;
        self.accept_sequence(
            PeerId::from_libp2p(peer),
            frame.header.session_id,
            frame.header.sequence,
        )
    }

    pub fn validate_inbound_packet_plane_control_frame(
        &self,
        peer: Libp2pPeerId,
        frame: &Frame,
        expected_payload_type: PayloadType,
    ) -> Result<(), ForwardError> {
        self.validate_inbound_frame_metadata(peer, frame, expected_payload_type)
    }

    fn validate_inbound_frame_metadata(
        &self,
        peer: Libp2pPeerId,
        frame: &Frame,
        expected_payload_type: PayloadType,
    ) -> Result<(), ForwardError> {
        if !self.authorized_peers.allows(&peer) {
            return Err(ForwardError::UnauthorizedPeer(peer));
        }
        if frame.header.payload_type != expected_payload_type {
            return Err(ForwardError::UnexpectedPayload(frame.header.payload_type));
        }
        if usize::from(frame.header.payload_len) != frame.payload.len() {
            return Err(ForwardError::PayloadLengthMismatch {
                header: frame.header.payload_len,
                actual: frame.payload.len(),
            });
        }
        if frame.payload.len() > self.mtu {
            return Err(ForwardError::PacketTooLarge {
                actual: frame.payload.len(),
                max: self.mtu,
            });
        }

        Ok(())
    }

    fn accept_sequence(
        &mut self,
        peer: PeerId,
        session_id: SessionId,
        sequence: Sequence,
    ) -> Result<(), ForwardError> {
        self.accept_sequence_at(peer, session_id, sequence, Instant::now())
    }

    fn accept_sequence_at(
        &mut self,
        peer: PeerId,
        session_id: SessionId,
        sequence: Sequence,
        now: Instant,
    ) -> Result<(), ForwardError> {
        self.expire_replay_windows_at(now);
        let key = (peer, session_id);
        if self.replay_windows.len() >= self.max_replay_windows.max(1)
            && !self.replay_windows.contains_key(&key)
        {
            self.prune_oldest_replay_window();
        }

        let window = self
            .replay_windows
            .entry(key)
            .or_insert_with(|| ReplayWindow::new(now));
        window.accept(sequence, now).map_err(|error| match error {
            ReplayAcceptError::Duplicate => ForwardError::ReplayedPacket {
                peer,
                session_id,
                sequence,
            },
            ReplayAcceptError::TooOld => ForwardError::PacketOutsideReplayWindow {
                peer,
                session_id,
                sequence,
            },
        })
    }

    fn expire_replay_windows_at(&mut self, now: Instant) -> usize {
        let before = self.replay_windows.len();
        let ttl = self.replay_session_ttl;
        self.replay_windows
            .retain(|_, window| !window.is_expired(now, ttl));
        before - self.replay_windows.len()
    }

    fn prune_oldest_replay_window(&mut self) {
        let Some(oldest) = self
            .replay_windows
            .iter()
            .min_by_key(|(_, window)| window.updated_at)
            .map(|(key, _)| *key)
        else {
            return;
        };
        self.replay_windows.remove(&oldest);
    }

    fn authorize_local_destination(&self, destination: IpAddr) -> Result<(), ForwardError> {
        self.routes
            .authorize_source(self.local_peer, destination)
            .map_err(|_| ForwardError::UnauthorizedLocalDestination { destination })
    }

    pub fn send_packet_response(
        swarm: &mut Swarm<Behaviour>,
        channel: request_response::ResponseChannel<PacketResponse>,
        response: PacketResponse,
    ) -> Result<(), PacketResponse> {
        swarm
            .behaviour_mut()
            .packet
            .send_response(channel, response)
    }
}

fn transport_peers_from_config_and_records(
    config: &Config,
    member_records: &[SignedMembershipRecord],
    now_unix_seconds: u64,
) -> Result<HashMap<PeerId, Libp2pPeerId>, ConfigError> {
    let local_peer = config.local_peer_id()?;
    let effective =
        effective_membership_at(member_records, &config.network.name, now_unix_seconds)?;
    let mut peers = HashMap::new();
    if !effective.authorizes_configured_peer(local_peer) {
        return Ok(peers);
    }
    for peer in &config.peers {
        let transport_peer = peer
            .id
            .parse::<Libp2pPeerId>()
            .map_err(ConfigError::Libp2pPeerId)?;
        let overlay_peer = PeerId::from_libp2p(transport_peer);
        if effective.authorizes_configured_peer(overlay_peer) {
            peers.insert(overlay_peer, transport_peer);
        }
    }
    for member in effective.overlay_members() {
        if member.peer != local_peer {
            peers.insert(member.peer, member.transport_peer);
        }
    }
    Ok(peers)
}

fn authorized_peers_from_config_and_records(
    config: &Config,
    member_records: &[SignedMembershipRecord],
    now_unix_seconds: u64,
) -> Result<AuthorizedPeers, ConfigError> {
    let local_peer = config.local_peer_id()?;
    let effective =
        effective_membership_at(member_records, &config.network.name, now_unix_seconds)?;
    let mut authorized = AuthorizedPeers::default();
    if !effective.authorizes_configured_peer(local_peer) {
        return Ok(authorized);
    }
    for peer in &config.peers {
        let transport_peer = peer
            .id
            .parse::<Libp2pPeerId>()
            .map_err(ConfigError::Libp2pPeerId)?;
        if effective.authorizes_configured_peer(PeerId::from_libp2p(transport_peer)) {
            authorized.insert(transport_peer);
        }
    }
    for member in effective.overlay_members() {
        if member.peer != local_peer {
            authorized.insert(member.transport_peer);
        }
    }
    Ok(authorized)
}

fn current_unix_seconds_lossy() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[must_use]
pub fn session_id_for_peer(peer: PeerId) -> SessionId {
    let bytes = peer.as_bytes();
    let session_id = SessionId::from_be_bytes(bytes[..4].try_into().expect("fixed slice length"));
    session_id.max(1)
}

fn fresh_session_id_for_peer(peer: PeerId) -> SessionId {
    let entropy = Libp2pKeypair::generate_ed25519()
        .public()
        .to_peer_id()
        .to_bytes();
    let mut bytes = [0; 4];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = peer.as_bytes()[index] ^ entropy[index % entropy.len()];
    }
    SessionId::from_be_bytes(bytes).max(1)
}

pub fn packet_source(packet: &[u8]) -> Result<IpAddr, ForwardError> {
    match ip_version(packet)? {
        4 => ipv4_endpoint(packet, 12),
        6 => ipv6_endpoint(packet, 8),
        version => Err(ForwardError::UnsupportedIpVersion(version)),
    }
}

pub fn packet_destination(packet: &[u8]) -> Result<IpAddr, ForwardError> {
    match ip_version(packet)? {
        4 => ipv4_endpoint(packet, 16),
        6 => ipv6_endpoint(packet, 24),
        version => Err(ForwardError::UnsupportedIpVersion(version)),
    }
}

fn ip_version(packet: &[u8]) -> Result<u8, ForwardError> {
    packet
        .first()
        .map(|byte| byte >> 4)
        .ok_or(ForwardError::TruncatedIpPacket {
            actual: 0,
            expected: 1,
        })
}

fn ipv4_endpoint(packet: &[u8], offset: usize) -> Result<IpAddr, ForwardError> {
    let expected = offset + 4;
    if packet.len() < expected {
        return Err(ForwardError::TruncatedIpPacket {
            actual: packet.len(),
            expected,
        });
    }

    Ok(IpAddr::V4(Ipv4Addr::from(
        <[u8; 4]>::try_from(&packet[offset..expected]).expect("fixed slice length"),
    )))
}

fn ipv6_endpoint(packet: &[u8], offset: usize) -> Result<IpAddr, ForwardError> {
    let expected = offset + 16;
    if packet.len() < expected {
        return Err(ForwardError::TruncatedIpPacket {
            actual: packet.len(),
            expected,
        });
    }

    Ok(IpAddr::V6(Ipv6Addr::from(
        <[u8; 16]>::try_from(&packet[offset..expected]).expect("fixed slice length"),
    )))
}

#[derive(Debug)]
pub enum ForwardError {
    Config(ConfigError),
    HostnameRecord(HostnameRecordError),
    MembershipRecord(MembershipRecordError),
    Route(RouteError),
    Frame(FrameError),
    Enqueue(EnqueueError),
    NoRoute(IpAddr),
    NoTransportPeer(PeerId),
    LocalPeerChanged {
        expected: PeerId,
        actual: PeerId,
    },
    HostnameSequenceExhausted,
    PacketTooLarge {
        actual: usize,
        max: usize,
    },
    UnauthorizedLocalSource {
        source: IpAddr,
    },
    UnauthorizedLocalDestination {
        destination: IpAddr,
    },
    TruncatedIpPacket {
        actual: usize,
        expected: usize,
    },
    UnsupportedIpVersion(u8),
    UnauthorizedPeer(Libp2pPeerId),
    UnexpectedPayload(PayloadType),
    PayloadLengthMismatch {
        header: u16,
        actual: usize,
    },
    ReplayedPacket {
        peer: PeerId,
        session_id: SessionId,
        sequence: Sequence,
    },
    PacketOutsideReplayWindow {
        peer: PeerId,
        session_id: SessionId,
        sequence: Sequence,
    },
}

impl From<ConfigError> for ForwardError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<HostnameRecordError> for ForwardError {
    fn from(error: HostnameRecordError) -> Self {
        Self::HostnameRecord(error)
    }
}

impl From<libp2p::identity::ParseError> for ForwardError {
    fn from(error: libp2p::identity::ParseError) -> Self {
        Self::HostnameRecord(HostnameRecordError::PeerId(error))
    }
}

impl From<MembershipRecordError> for ForwardError {
    fn from(error: MembershipRecordError) -> Self {
        Self::MembershipRecord(error)
    }
}

impl From<RouteError> for ForwardError {
    fn from(error: RouteError) -> Self {
        Self::Route(error)
    }
}

impl From<FrameError> for ForwardError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<EnqueueError> for ForwardError {
    fn from(error: EnqueueError) -> Self {
        Self::Enqueue(error)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use libp2p::identity::Keypair;

    use crate::{
        config::{
            InterfaceConfig, NetworkConfig, PeerConfig, QueueConfig, ResourceConfig, RouteConfig,
        },
        identity::NodeIdentity,
        membership::{
            MembershipRecordIssueOptions, MembershipRecordOptions, MembershipRecordSubject,
            MembershipRole, issue_membership_record_at, issue_membership_record_for_subject_at,
        },
        route::{builtin_ipv4, builtin_ipv6},
        runtime::p2p::{HostConfig, build_node},
        wire::Header,
    };

    use super::*;

    fn config_for(remote: Libp2pPeerId) -> Config {
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_peer = loop {
            let candidate = Keypair::generate_ed25519().public().to_peer_id();
            let candidate_overlay = PeerId::from_libp2p(candidate);
            if builtin_ipv4(candidate_overlay) != builtin_ipv4(remote_overlay)
                && builtin_ipv6(candidate_overlay) != builtin_ipv6(remote_overlay)
            {
                break candidate.to_string();
            }
        };

        Config {
            network: NetworkConfig {
                dns: crate::dns::DnsConfig::default(),
                name: "lab".to_owned(),
                local_peer,
                private_key: None,
                membership_key: None,
                previous_membership_tags: Vec::new(),
                member_records: Vec::new(),
                vpn_ip: None,
                routes: Vec::new(),
                listen_addresses: Vec::new(),
                external_addresses: Vec::new(),
                bootstrap_peers: Vec::new(),
                discovery: crate::config::DiscoveryConfig::default(),
                relay: crate::config::RelayConfig::default(),
                packet_plane: crate::config::PacketPlaneConfig::default(),
            },
            interface: InterfaceConfig {
                name: "hs0".to_owned(),
                mtu: 1280,
            },
            peers: vec![PeerConfig {
                id: remote.to_string(),
                name: Some("remote".to_owned()),
                ip: None,
                vpn_ip: None,
                addresses: Vec::new(),
                routes: Vec::new(),
            }],
            queue: QueueConfig {
                max_packets_per_peer: 4,
                max_bytes_per_peer: 4096,
                max_packet_age_millis: 1_000,
            },
            resources: ResourceConfig::default(),
        }
    }

    fn record_only_config_for(member: NodeIdentity, roles: Vec<MembershipRole>) -> Config {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let transport_peer = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let mut config = config_for(transport_peer);
        config.peers.clear();
        config.network.member_records = vec![
            issue_membership_record_at(
                &issuer,
                MembershipRecordOptions {
                    network_name: "lab".to_owned(),
                    member,
                    membership_epoch: 1,
                    sequence: 1,
                    roles,
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1_000,
            )
            .expect("member record"),
        ];
        config
    }

    fn local_ipv4(config: &Config) -> Ipv4Addr {
        builtin_ipv4(config.local_peer_id().expect("local peer id"))
    }

    fn local_ipv6(config: &Config) -> Ipv6Addr {
        builtin_ipv6(config.local_peer_id().expect("local peer id"))
    }

    fn ipv4_packet(source: Ipv4Addr, destination: Ipv4Addr) -> Vec<u8> {
        let mut packet = vec![0; 20];
        packet[0] = 0x45;
        packet[12..16].copy_from_slice(&source.octets());
        packet[16..20].copy_from_slice(&destination.octets());
        packet
    }

    fn ipv6_packet(source: Ipv6Addr, destination: Ipv6Addr) -> Vec<u8> {
        let mut packet = vec![0; 40];
        packet[0] = 0x60;
        packet[8..24].copy_from_slice(&source.octets());
        packet[24..40].copy_from_slice(&destination.octets());
        packet
    }

    #[test]
    fn session_id_is_derived_from_local_peer_and_never_zero() {
        assert_eq!(session_id_for_peer(PeerId::from_bytes([0; 32])), 1);
        assert_eq!(
            session_id_for_peer(PeerId::from_bytes([
                0x12, 0x34, 0x56, 0x78, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0
            ])),
            0x1234_5678
        );
    }

    #[test]
    fn packet_endpoints_parse_ipv4_and_ipv6() {
        let source4 = Ipv4Addr::new(100, 64, 1, 2);
        let destination4 = Ipv4Addr::new(100, 64, 3, 4);
        let source6 = Ipv6Addr::LOCALHOST;
        let destination6 = Ipv6Addr::UNSPECIFIED;

        assert_eq!(
            packet_source(&ipv4_packet(source4, destination4)).expect("source"),
            IpAddr::V4(source4)
        );
        assert_eq!(
            packet_destination(&ipv4_packet(source4, destination4)).expect("destination"),
            IpAddr::V4(destination4)
        );
        assert_eq!(
            packet_source(&ipv6_packet(source6, destination6)).expect("source"),
            IpAddr::V6(source6)
        );
        assert_eq!(
            packet_destination(&ipv6_packet(source6, destination6)).expect("destination"),
            IpAddr::V6(destination6)
        );
    }

    #[test]
    fn inbound_packet_must_match_peer_route_ownership() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert_eq!(
            forwarder
                .accept_inbound_packet(remote, &frame)
                .expect("packet accepted"),
            frame.payload.as_slice()
        );
    }

    #[test]
    fn local_advertised_routes_include_builtin_host_routes() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let config = config_for(remote);
        let local = config.local_peer_id().expect("local peer");
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        let advertised = forwarder.local_advertised_routes();
        assert!(advertised.contains(&ControlRoute::new(format!("{}/32", builtin_ipv4(local)), 0)));
        assert!(advertised.contains(&ControlRoute::new(
            format!("{}/128", builtin_ipv6(local)),
            0
        )));
    }

    #[test]
    fn local_hostname_reconciliation_preserves_identity_and_advances_sequence() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut config = config_for(remote);
        let identity = NodeIdentity::generate_ed25519().expect("identity");
        config.network.local_peer.clone_from(&identity.peer_id);
        config.network.private_key = Some(identity.private_key.clone());
        config.network.dns.hostname = Some("old-host".to_owned());
        let original_peer = identity.peer_id.clone();
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert!(
            forwarder
                .reconcile_local_hostname_record(&identity, 1_000)
                .expect("initial claim")
        );
        assert_eq!(forwarder.hostname_records()[0].payload.sequence, 1);
        assert!(
            !forwarder
                .reconcile_local_hostname_record(&identity, 1_001)
                .expect("stable claim")
        );

        forwarder.config.network.dns.hostname = Some("new-host".to_owned());
        assert!(
            forwarder
                .reconcile_local_hostname_record(&identity, 2_000)
                .expect("renamed claim")
        );
        assert_eq!(forwarder.hostname_records().len(), 1);
        assert_eq!(forwarder.hostname_records()[0].payload.hostname, "new-host");
        assert_eq!(forwarder.hostname_records()[0].payload.sequence, 2);
        assert_eq!(forwarder.hostname_records()[0].payload.peer, original_peer);
    }

    #[test]
    fn local_advertised_routes_include_configured_local_prefixes() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut config = config_for(remote);
        config.network.routes.push(RouteConfig {
            prefix: "10.41.0.0/24".to_owned(),
            metric: 75,
        });
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert!(
            forwarder
                .local_advertised_routes()
                .contains(&ControlRoute::new("10.41.0.0/24", 75))
        );
    }

    #[test]
    fn outbound_packet_allows_configured_local_route_source() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut config = config_for(remote);
        config.network.routes.push(RouteConfig {
            prefix: "10.41.0.0/24".to_owned(),
            metric: 75,
        });
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(Ipv4Addr::new(10, 41, 0, 9), builtin_ipv4(remote_overlay));

        let mut queues = PeerQueues::with_packet_ttl(4, 4096, Duration::from_secs(1));
        forwarder
            .enqueue_tun_packet(&mut queues, packet)
            .expect("configured local source accepted");

        assert_eq!(queues.total_stats().queued_packets, 1);
    }

    #[test]
    fn advertised_routes_must_match_configured_peer_ownership() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut config = config_for(remote);
        config.peers[0].routes.push(RouteConfig {
            prefix: "10.42.0.0/24".to_owned(),
            metric: 100,
        });
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert!(forwarder.authorizes_advertised_routes(
            remote,
            &[
                ControlRoute::new(format!("{}/32", builtin_ipv4(remote_overlay)), 0),
                ControlRoute::new("10.42.0.0/24", 1),
            ],
        ));
        assert!(
            !forwarder
                .authorizes_advertised_routes(remote, &[ControlRoute::new("10.42.9.0/24", 0)])
        );
        assert!(
            !forwarder
                .authorizes_advertised_routes(remote, &[ControlRoute::new("not-a-prefix", 0)])
        );
    }

    #[test]
    fn advertised_routes_can_match_member_record_route_grants() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let remote = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let mut config = config_for(remote);
        config.peers.clear();
        config.network.member_records = vec![
            issue_membership_record_at(
                &issuer,
                MembershipRecordOptions {
                    network_name: "lab".to_owned(),
                    member,
                    membership_epoch: 1,
                    sequence: 1,
                    roles: vec![
                        MembershipRole::OverlayMember,
                        MembershipRole::RouteAuthority,
                    ],
                    route_grants: vec![RouteConfig {
                        prefix: "10.42.0.0/24".to_owned(),
                        metric: 100,
                    }],
                    expires_at_unix_seconds: None,
                },
                1_000,
            )
            .expect("member record"),
        ];
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert!(
            forwarder.authorizes_advertised_routes(remote, &[ControlRoute::new("10.42.0.0/24", 1)])
        );
        assert!(
            !forwarder
                .authorizes_advertised_routes(remote, &[ControlRoute::new("10.42.1.0/24", 1)])
        );
    }

    #[test]
    fn merge_membership_records_trusts_local_issuer_for_live_pairing_records() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let mut config = config_for(member_peer);
        config.peers.clear();
        config.network.local_peer = issuer.peer_id.clone();
        config.network.private_key = Some(issuer.private_key.clone());
        let incoming = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: vec![RouteConfig {
                    prefix: "10.42.0.0/24".to_owned(),
                    metric: 100,
                }],
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("live pairing record");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        assert_eq!(forwarder.membership_revision(), 0);

        let stats = forwarder
            .merge_membership_records(std::slice::from_ref(&incoming), 1_001)
            .expect("merge locally issued record");

        assert_eq!(stats.accepted, 1);
        assert_eq!(forwarder.membership_revision(), 1);
        let stats = forwarder
            .merge_membership_records(&[incoming], 1_001)
            .expect("merge duplicate record");
        assert_eq!(stats.accepted, 0);
        assert_eq!(forwarder.membership_revision(), 1);
        assert!(forwarder.is_configured_transport_peer(member_peer));
        assert!(
            forwarder
                .authorizes_advertised_routes(member_peer, &[ControlRoute::new("10.42.0.0/24", 1)])
        );
    }

    #[test]
    fn membership_refresh_activates_retained_future_record_and_advances_revision() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let mut config = config_for(member_peer);
        config.peers.clear();
        config.network.local_peer = issuer.peer_id.clone();
        config.network.private_key = Some(issuer.private_key.clone());
        let root = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: issuer.clone(),
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            900,
        )
        .expect("root record");
        let future_grant = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_001,
        )
        .expect("future grant");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");

        forwarder
            .merge_membership_records(&[root], 1_000)
            .expect("root merge");
        forwarder
            .merge_membership_records(&[future_grant], 1_000)
            .expect("future grant merge");
        let retained_revision = forwarder.membership_revision();
        assert!(!forwarder.is_configured_transport_peer(member_peer));

        let (_, effective_changed) = forwarder
            .refresh_membership_records(1_001)
            .expect("membership refresh");

        assert!(effective_changed);
        assert!(forwarder.is_configured_transport_peer(member_peer));
        assert_eq!(
            forwarder.membership_revision(),
            retained_revision.wrapping_add(1)
        );
    }

    #[test]
    fn merge_membership_records_does_not_promote_a_local_delegate_to_root() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let delegate_peer = delegate
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("delegate peer");
        let mut config = config_for(delegate_peer);
        config.peers.clear();
        config.network.local_peer = delegate.peer_id.clone();
        config.network.private_key = Some(delegate.private_key.clone());
        config.network.member_records = vec![
            issue_membership_record_at(
                &root,
                MembershipRecordOptions {
                    network_name: "lab".to_owned(),
                    member: root.clone(),
                    membership_epoch: 1,
                    sequence: 1,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1_000,
            )
            .expect("root record"),
            issue_membership_record_at(
                &root,
                MembershipRecordOptions {
                    network_name: "lab".to_owned(),
                    member: delegate.clone(),
                    membership_epoch: 1,
                    sequence: 2,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1_000,
            )
            .expect("delegate record"),
        ];
        let delegate_self_root = issue_membership_record_at(
            &delegate,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: delegate.clone(),
                membership_epoch: 1,
                sequence: 3,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_001,
        )
        .expect("delegate self root");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert!(matches!(
            forwarder.merge_membership_records(&[delegate_self_root], 1_001),
            Err(ForwardError::MembershipRecord(
                MembershipRecordError::UntrustedIssuer { issuer }
            )) if issuer == delegate.peer_id
        ));
    }

    #[test]
    fn creator_resignation_retains_history_and_delegate_authority() {
        let root = NodeIdentity::generate_ed25519().expect("root");
        let delegate = NodeIdentity::generate_ed25519().expect("delegate");
        let candidate = NodeIdentity::generate_ed25519().expect("candidate");
        let delegate_peer = delegate
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("delegate peer");
        let mut config = config_for(delegate_peer);
        config.peers.clear();
        config.network.local_peer = delegate.peer_id.clone();
        config.network.private_key = Some(delegate.private_key.clone());
        let root_record = issue_membership_record_at(
            &root,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: root.clone(),
                membership_epoch: 1,
                sequence: 1,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("root record");
        let delegate_record = issue_membership_record_at(
            &root,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: delegate.clone(),
                membership_epoch: 1,
                sequence: 2,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("delegate record");
        let root_revocation = issue_membership_record_for_subject_at(
            &root,
            MembershipRecordIssueOptions {
                network_name: "lab".to_owned(),
                member: MembershipRecordSubject::from_identity(&root).expect("root subject"),
                membership_epoch: 1,
                sequence: 3,
                revoked: true,
                roles: Vec::new(),
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_100,
        )
        .expect("root revocation");
        config.network.member_records = vec![root_record, delegate_record, root_revocation];
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");

        let stats = forwarder
            .prune_membership_records(1_100)
            .expect("prune revoked trust graph");

        assert_eq!(stats.removed_untrusted, 0);
        assert_eq!(forwarder.member_records().len(), 3);
        assert!(
            forwarder
                .member_records()
                .iter()
                .any(|record| record.payload.revoked)
        );
        let delegated_grant = issue_membership_record_at(
            &delegate,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member: candidate,
                membership_epoch: 1,
                sequence: 4,
                roles: vec![MembershipRole::OverlayMember],
                route_grants: Vec::new(),
                expires_at_unix_seconds: None,
            },
            1_101,
        )
        .expect("delegated grant");
        forwarder
            .merge_membership_records(&[delegated_grant], 1_101)
            .expect("delegate remains authorized");
        assert_eq!(forwarder.member_records().len(), 4);
    }

    #[test]
    fn local_membership_record_is_not_a_transport_peer() {
        let local = NodeIdentity::generate_ed25519().expect("local");
        let local_transport = local
            .peer_id
            .parse::<Libp2pPeerId>()
            .expect("local transport peer");
        let mut config = config_for(Keypair::generate_ed25519().public().to_peer_id());
        config.peers.clear();
        config.network.local_peer = local.peer_id.clone();
        config.network.private_key = Some(local.private_key.clone());
        config.network.member_records = vec![
            issue_membership_record_at(
                &local,
                MembershipRecordOptions {
                    network_name: "lab".to_owned(),
                    member: local.clone(),
                    membership_epoch: 1,
                    sequence: 1,
                    roles: vec![MembershipRole::OverlayMember],
                    route_grants: Vec::new(),
                    expires_at_unix_seconds: None,
                },
                1_000,
            )
            .expect("local member record"),
        ];

        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert!(!forwarder.is_configured_transport_peer(local_transport));
        assert_eq!(forwarder.configured_transport_peers().count(), 0);
        assert_eq!(forwarder.local_advertised_routes().len(), 2);
    }

    #[test]
    fn merge_membership_records_routes_packets_to_requested_vpn_ip() {
        let issuer = NodeIdentity::generate_ed25519().expect("issuer");
        let member = NodeIdentity::generate_ed25519().expect("member");
        let member_peer = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let mut config = config_for(member_peer);
        config.peers.clear();
        config.network.local_peer.clear();
        config.network.private_key = Some(issuer.private_key.clone());
        config.network.vpn_ip = Some("10.47.0.1".to_owned());
        let incoming = issue_membership_record_at(
            &issuer,
            MembershipRecordOptions {
                network_name: "lab".to_owned(),
                member,
                membership_epoch: 1,
                sequence: 1,
                roles: vec![
                    MembershipRole::OverlayMember,
                    MembershipRole::RouteAuthority,
                ],
                route_grants: vec![RouteConfig {
                    prefix: "10.47.0.2/32".to_owned(),
                    metric: 0,
                }],
                expires_at_unix_seconds: None,
            },
            1_000,
        )
        .expect("live pairing record");
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");

        let stats = forwarder
            .merge_membership_records(&[incoming], 1_001)
            .expect("merge locally issued record");
        let packet = ipv4_packet(Ipv4Addr::new(10, 47, 0, 1), Ipv4Addr::new(10, 47, 0, 2));
        let prepared = forwarder
            .prepare_tun_packet(packet)
            .expect("packet routes to member requested VPN IP");

        assert_eq!(stats.accepted, 1);
        assert_eq!(prepared.peer(), PeerId::from_libp2p(member_peer));
    }

    #[test]
    fn staged_reconfigure_preserves_packet_and_replay_state_until_commit() {
        let original_remote = Keypair::generate_ed25519().public().to_peer_id();
        let added_remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut forwarder =
            Forwarder::from_config(&config_for(original_remote)).expect("forwarder");
        let original_session = forwarder.session_id;
        forwarder.next_sequence = 41;
        forwarder.replay_windows.insert(
            (PeerId::from_libp2p(original_remote), 7),
            ReplayWindow::new(Instant::now()),
        );

        let mut next = forwarder.config().clone();
        next.peers.push(PeerConfig {
            id: added_remote.to_string(),
            name: Some("added".to_owned()),
            ip: None,
            vpn_ip: None,
            addresses: Vec::new(),
            routes: Vec::new(),
        });
        let update = forwarder
            .prepare_reconfigure(next, 1_000)
            .expect("prepare update");

        assert!(!forwarder.is_configured_transport_peer(added_remote));
        assert_eq!(forwarder.session_id, original_session);
        assert_eq!(forwarder.next_sequence, 41);
        assert_eq!(forwarder.replay_window_count(), 1);

        forwarder.commit_reconfigure(update);

        assert!(forwarder.is_configured_transport_peer(added_remote));
        assert_eq!(forwarder.session_id, original_session);
        assert_eq!(forwarder.next_sequence, 41);
        assert_eq!(forwarder.replay_window_count(), 1);
    }

    #[test]
    fn staged_reconfigure_rejects_local_identity_changes() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let expected = forwarder.local_peer;
        let mut next = forwarder.config().clone();
        next.network.local_peer = Keypair::generate_ed25519()
            .public()
            .to_peer_id()
            .to_string();

        assert!(matches!(
            forwarder.prepare_reconfigure(next, 1_000),
            Err(ForwardError::LocalPeerChanged { expected: actual_expected, .. })
                if actual_expected == expected
        ));
    }

    #[test]
    fn inbound_packet_accepts_member_record_peer_with_builtin_source() {
        let member = NodeIdentity::generate_ed25519().expect("member");
        let remote = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = record_only_config_for(member, vec![MembershipRole::OverlayMember]);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert_eq!(
            forwarder
                .accept_inbound_packet(remote, &frame)
                .expect("packet accepted"),
            frame.payload.as_slice()
        );
    }

    #[test]
    fn inbound_packet_rejects_member_record_without_overlay_member_role() {
        let member = NodeIdentity::generate_ed25519().expect("member");
        let remote = member.peer_id.parse::<Libp2pPeerId>().expect("member peer");
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = record_only_config_for(member, vec![MembershipRole::RouteAuthority]);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::UnauthorizedPeer(peer)) if peer == remote
        ));
    }

    #[test]
    fn inbound_packet_rejects_source_spoofing() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let packet = ipv4_packet(Ipv4Addr::new(198, 51, 100, 1), Ipv4Addr::new(100, 64, 9, 9));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::Route(RouteError::UnauthorizedSource { .. }))
        ));
    }

    #[test]
    fn inbound_packet_must_target_local_overlay_address() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), Ipv4Addr::new(100, 64, 9, 9));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::UnauthorizedLocalDestination {
                destination: IpAddr::V4(destination)
            }) if destination == Ipv4Addr::new(100, 64, 9, 9)
        ));
    }

    #[test]
    fn inbound_packet_can_target_configured_local_route() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut config = config_for(remote);
        config.network.routes.push(RouteConfig {
            prefix: "10.41.0.0/24".to_owned(),
            metric: 75,
        });
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), Ipv4Addr::new(10, 41, 0, 9));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert_eq!(
            forwarder
                .accept_inbound_packet(remote, &frame)
                .expect("packet accepted"),
            frame.payload.as_slice()
        );
    }

    #[test]
    fn inbound_ipv6_packet_can_target_local_overlay_address() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv6_packet(builtin_ipv6(remote_overlay), local_ipv6(&config));
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert_eq!(
            forwarder
                .accept_inbound_packet(remote, &frame)
                .expect("packet accepted"),
            frame.payload.as_slice()
        );
    }

    #[test]
    fn inbound_packet_rejects_duplicate_sequence_in_session() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let frame = Frame::packet(7, 42, packet).expect("frame");

        forwarder
            .accept_inbound_packet(remote, &frame)
            .expect("first packet accepted");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::ReplayedPacket {
                peer,
                session_id: 7,
                sequence: 42
            }) if peer == remote_overlay
        ));
    }

    #[test]
    fn inbound_packet_accepts_out_of_order_sequence_inside_replay_window() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let later = Frame::packet(7, 42, packet.clone()).expect("later frame");
        let earlier = Frame::packet(7, 41, packet).expect("earlier frame");

        forwarder
            .accept_inbound_packet(remote, &later)
            .expect("later packet accepted");
        forwarder
            .accept_inbound_packet(remote, &earlier)
            .expect("earlier packet accepted");
    }

    #[test]
    fn inbound_packet_rejects_sequence_outside_replay_window() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let current = Frame::packet(7, 100, packet.clone()).expect("current frame");
        let too_old = Frame::packet(7, 36, packet).expect("old frame");

        forwarder
            .accept_inbound_packet(remote, &current)
            .expect("current packet accepted");

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &too_old),
            Err(ForwardError::PacketOutsideReplayWindow {
                peer,
                session_id: 7,
                sequence: 36
            }) if peer == remote_overlay
        ));
    }

    #[test]
    fn inbound_stream_or_packet_plane_packet_does_not_use_overlay_replay_window() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let datagram = Frame::packet(7, 100, packet.clone()).expect("datagram frame");
        let stream = Frame::packet(7, 0, packet).expect("stream frame");

        forwarder
            .accept_inbound_packet(remote, &datagram)
            .expect("datagram packet accepted");

        assert_eq!(
            forwarder
                .accept_inbound_stream_packet(remote, &stream)
                .expect("stream fallback and packet-plane datagrams bypass inner replay"),
            stream.payload.as_slice()
        );
    }

    #[test]
    fn inbound_packet_tracks_replay_windows_per_session() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(builtin_ipv4(remote_overlay), local_ipv4(&config));
        let first_session = Frame::packet(7, 42, packet.clone()).expect("first session frame");
        let second_session = Frame::packet(8, 42, packet).expect("second session frame");

        forwarder
            .accept_inbound_packet(remote, &first_session)
            .expect("first session accepted");
        forwarder
            .accept_inbound_packet(remote, &second_session)
            .expect("second session accepted");
    }

    #[test]
    fn replay_window_expires_after_session_ttl() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        forwarder.replay_session_ttl = Duration::from_secs(1);
        let start = Instant::now();

        forwarder
            .accept_sequence_at(remote_overlay, 7, 42, start)
            .expect("sequence accepted");
        assert_eq!(forwarder.replay_window_count(), 1);
        assert!(matches!(
            forwarder.accept_sequence_at(
                remote_overlay,
                7,
                42,
                start + Duration::from_millis(500)
            ),
            Err(ForwardError::ReplayedPacket {
                peer,
                session_id: 7,
                sequence: 42
            }) if peer == remote_overlay
        ));

        assert_eq!(
            forwarder.expire_replay_windows_at(start + Duration::from_secs(2)),
            1
        );
        assert_eq!(forwarder.replay_window_count(), 0);
        forwarder
            .accept_sequence_at(remote_overlay, 7, 42, start + Duration::from_secs(2))
            .expect("expired session starts a fresh replay window");
    }

    #[test]
    fn replay_windows_are_bounded() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        forwarder.max_replay_windows = 1;
        let start = Instant::now();

        forwarder
            .accept_sequence_at(remote_overlay, 7, 42, start)
            .expect("first session accepted");
        forwarder
            .accept_sequence_at(remote_overlay, 8, 42, start + Duration::from_millis(1))
            .expect("second session accepted");

        assert_eq!(forwarder.replay_window_count(), 1);
        forwarder
            .accept_sequence_at(remote_overlay, 7, 42, start + Duration::from_millis(2))
            .expect("oldest evicted session starts fresh");
        assert_eq!(forwarder.replay_window_count(), 1);
    }

    #[test]
    fn replay_windows_share_sequence_space_across_payload_types() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let start = Instant::now();

        forwarder
            .accept_sequence_at(remote_overlay, 7, 16, start)
            .expect("packet sequence accepted");
        forwarder
            .accept_sequence_at(remote_overlay, 7, 128, start + Duration::from_millis(1))
            .expect("probe accepted");

        assert!(matches!(
            forwarder.accept_sequence_at(remote_overlay, 7, 16, start + Duration::from_millis(2)),
            Err(ForwardError::PacketOutsideReplayWindow {
                peer,
                session_id: 7,
                sequence: 16
            }) if peer == remote_overlay
        ));
    }

    #[test]
    fn inbound_keepalive_is_authorized_and_replay_checked() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let frame = Frame::keepalive(7, 42).expect("frame");

        forwarder
            .accept_inbound_control_frame(remote, &frame, PayloadType::Keepalive)
            .expect("keepalive accepted");

        assert!(matches!(
            forwarder.accept_inbound_control_frame(remote, &frame, PayloadType::Keepalive),
            Err(ForwardError::ReplayedPacket {
                peer,
                session_id: 7,
                sequence: 42
            }) if peer == remote_overlay
        ));
    }

    #[test]
    fn inbound_path_probe_is_authorized_and_bounded_by_mtu() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut config = config_for(remote);
        config.interface.mtu = 4;
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let accepted = Frame::path_probe(7, 42, vec![1, 2, 3, 4]).expect("probe");
        let rejected = Frame::path_probe(7, 43, vec![1, 2, 3, 4, 5]).expect("probe");

        forwarder
            .accept_inbound_control_frame(remote, &accepted, PayloadType::PathProbe)
            .expect("path probe accepted");

        assert!(matches!(
            forwarder.accept_inbound_control_frame(remote, &rejected, PayloadType::PathProbe),
            Err(ForwardError::PacketTooLarge { actual: 5, max: 4 })
        ));
    }

    #[test]
    fn packet_plane_path_probe_validation_does_not_share_overlay_replay_window() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let start = Instant::now();
        forwarder
            .accept_sequence_at(remote_overlay, 7, 128, start)
            .expect("later packet accepted");
        let probe = Frame::path_probe(7, 1, vec![1, 2, 3, 4]).expect("probe");

        forwarder
            .validate_inbound_packet_plane_control_frame(remote, &probe, PayloadType::PathProbe)
            .expect("outer packet-plane replay protects packet-plane probes");
        assert!(matches!(
            forwarder.accept_inbound_control_frame(remote, &probe, PayloadType::PathProbe),
            Err(ForwardError::PacketOutsideReplayWindow {
                peer,
                session_id: 7,
                sequence: 1,
            }) if peer == remote_overlay
        ));
    }

    #[tokio::test]
    async fn outbound_path_probe_respects_peer_mtu() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut node = build_node(&HostConfig {
            identity: crate::identity::NodeIdentity::generate_ed25519().expect("identity"),
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: crate::config::DiscoveryConfig::default(),
        })
        .expect("node");

        assert!(matches!(
            forwarder.send_path_probe_with_mtu(
                &mut node.swarm,
                PeerId::from_libp2p(remote),
                4,
                b"probe",
            ),
            Err(ForwardError::PacketTooLarge { actual: 5, max: 4 })
        ));
    }

    #[test]
    fn inbound_control_frame_rejects_unexpected_payload_type() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let frame = Frame::keepalive(7, 42).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_control_frame(remote, &frame, PayloadType::PathProbe),
            Err(ForwardError::UnexpectedPayload(PayloadType::Keepalive))
        ));
    }

    #[tokio::test]
    async fn outbound_packet_resolves_destination_to_libp2p_peer() {
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut config = config_for(remote);
        config.network.local_peer = local_identity.peer_id.clone();
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: crate::config::DiscoveryConfig::default(),
        })
        .expect("node");
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));

        let request_id = forwarder
            .send_tun_packet(&mut node.swarm, packet)
            .expect("request id");

        assert_ne!(format!("{request_id:?}"), "");
    }

    #[test]
    fn outbound_packet_can_be_enqueued_before_send() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(1, 1280);
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));

        forwarder
            .enqueue_tun_packet(&mut queues, packet)
            .expect("queued");

        let queued_packet = queues.dequeue().expect("queued packet");
        assert_eq!(queued_packet.peer(), remote_overlay);
        assert_eq!(queued_packet.sequence(), 0);
    }

    #[test]
    fn outbound_frame_carries_current_session_and_packet_sequence() {
        let local = PeerId::from_bytes([
            0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0,
        ]);
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut config = config_for(remote);
        config.network.local_peer = local.to_string();
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));
        let mut queues = PeerQueues::new(1, 1280);

        forwarder
            .enqueue_tun_packet(&mut queues, packet)
            .expect("queued");
        let queued_packet = queues.dequeue().expect("queued packet");
        let frame = forwarder.packet_frame(&queued_packet).expect("frame");

        assert_ne!(frame.header.session_id, 0);
        assert_eq!(frame.header.session_id, forwarder.session_id);
        assert_eq!(frame.header.sequence, 0);
        assert_eq!(frame.header.payload_len, 20);
    }

    #[test]
    fn outbound_packet_reports_queue_backpressure() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let config = config_for(remote);
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(1, 1280);
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));

        forwarder
            .enqueue_tun_packet(&mut queues, packet.clone())
            .expect("first packet queued");

        assert!(matches!(
            forwarder.enqueue_tun_packet(&mut queues, packet),
            Err(ForwardError::Enqueue(EnqueueError::QueueFull { .. }))
        ));
    }

    #[tokio::test]
    async fn queued_packet_respects_peer_advertised_mtu() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let local_identity = crate::identity::NodeIdentity::generate_ed25519().expect("identity");
        let mut config = config_for(remote);
        config.network.local_peer = local_identity.peer_id.clone();
        let mut forwarder = Forwarder::from_config(&config).expect("forwarder");
        let mut queues = PeerQueues::new(1, 1280);
        let mut node = build_node(&HostConfig {
            identity: local_identity,
            network_name: "lab".to_owned(),
            membership_tag: None,
            mtu: 1280,
            max_concurrent_control_streams: 64,
            max_concurrent_packet_streams: 256,
            listen_addresses: Vec::new(),
            external_addresses: Vec::new(),
            bootstrap_peers: Vec::new(),
            known_peers: Vec::new(),
            relay_reservations: Vec::new(),
            relay_server: false,
            relay_resources: crate::config::RelayResourceConfig::default(),
            resources: crate::config::ResourceConfig::default(),
            discovery: crate::config::DiscoveryConfig::default(),
        })
        .expect("node");
        let packet = ipv4_packet(local_ipv4(&config), builtin_ipv4(remote_overlay));
        forwarder
            .enqueue_tun_packet(&mut queues, packet)
            .expect("queued");
        let queued_packet = queues.dequeue().expect("queued packet");

        assert!(matches!(
            forwarder.send_queued_packet_with_mtu(&mut node.swarm, &queued_packet, 19),
            Err(ForwardError::PacketTooLarge {
                actual: 20,
                max: 19
            })
        ));
    }

    #[test]
    fn outbound_packet_rejects_local_source_spoofing() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let mut queues = PeerQueues::new(1, 1280);
        let packet = ipv4_packet(Ipv4Addr::new(198, 51, 100, 1), builtin_ipv4(remote_overlay));

        assert!(matches!(
            forwarder.enqueue_tun_packet(&mut queues, packet),
            Err(ForwardError::UnauthorizedLocalSource {
                source: IpAddr::V4(source)
            }) if source == Ipv4Addr::new(198, 51, 100, 1)
        ));
    }

    #[test]
    fn forwarder_uses_effective_packet_mtu() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut config = config_for(remote);
        config.interface.mtu = u16::MAX;
        let forwarder = Forwarder::from_config(&config).expect("forwarder");

        assert_eq!(forwarder.mtu(), usize::from(config.effective_packet_mtu()));
    }

    #[test]
    fn inbound_packet_rejects_payload_length_mismatch() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let mut frame = Frame::packet(0, 1, vec![0x45; 20]).expect("frame");
        frame.header = Header::new(PayloadType::IpPacket, 0, 1, 19);

        assert!(matches!(
            forwarder.accept_inbound_packet(remote, &frame),
            Err(ForwardError::PayloadLengthMismatch { .. })
        ));
    }

    #[test]
    fn inbound_packet_rejects_unknown_peer() {
        let remote = Keypair::generate_ed25519().public().to_peer_id();
        let other = Keypair::generate_ed25519().public().to_peer_id();
        let remote_overlay = PeerId::from_libp2p(remote);
        let mut forwarder = Forwarder::from_config(&config_for(remote)).expect("forwarder");
        let packet = ipv6_packet(builtin_ipv6(remote_overlay), Ipv6Addr::LOCALHOST);
        let frame = Frame::packet(0, 1, packet).expect("frame");

        assert!(matches!(
            forwarder.accept_inbound_packet(other, &frame),
            Err(ForwardError::UnauthorizedPeer(peer)) if peer == other
        ));
    }
}
