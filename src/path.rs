use crate::{PathKind, PeerId};

const PATH_FAILURE_PENALTY_STEP: u16 = 50;
const PATH_FAILURE_PENALTY_RECOVERY_STEP: u16 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathCandidate {
    pub peer: PeerId,
    pub kind: PathKind,
    pub observed_rtt_ms: Option<u16>,
    pub estimated_mtu: Option<u16>,
    pub failure_penalty: u16,
    pub relay: bool,
    pub healthy: bool,
    pub established_connections: u32,
}

impl PathCandidate {
    #[must_use]
    pub const fn new(peer: PeerId, kind: PathKind) -> Self {
        Self {
            peer,
            kind,
            observed_rtt_ms: None,
            estimated_mtu: None,
            failure_penalty: 0,
            relay: matches!(kind, PathKind::CircuitRelay),
            healthy: true,
            established_connections: 0,
        }
    }

    #[must_use]
    pub const fn with_estimated_mtu(mut self, mtu: u16) -> Self {
        self.estimated_mtu = Some(mtu);
        self
    }

    #[must_use]
    pub fn effective_mtu(self, fallback_mtu: u16) -> u16 {
        self.estimated_mtu
            .map_or(fallback_mtu, |mtu| mtu.min(fallback_mtu))
    }

    #[must_use]
    pub fn score(self) -> i32 {
        if !self.healthy {
            return i32::MIN;
        }

        let latency_penalty = self
            .observed_rtt_ms
            .map_or(0, |rtt| i32::from(rtt.saturating_div(10)));
        i32::from(self.kind.default_score()) - latency_penalty - i32::from(self.failure_penalty)
    }

    #[must_use]
    pub const fn is_relay(self) -> bool {
        self.relay
    }

    #[must_use]
    pub const fn is_direct(self) -> bool {
        !self.relay
    }

    #[must_use]
    pub const fn is_selectable(self) -> bool {
        self.healthy && (!self.relay || self.established_connections > 0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PathTransportSupport {
    pub udp_datagrams: bool,
    pub quic_datagrams: bool,
}

impl PathTransportSupport {
    #[must_use]
    pub const fn stream_fallback() -> Self {
        Self {
            udp_datagrams: false,
            quic_datagrams: false,
        }
    }

    #[must_use]
    pub const fn supports(self, kind: PathKind) -> bool {
        match kind {
            PathKind::DirectUdpDatagram => self.udp_datagrams,
            PathKind::DirectQuicDatagram => self.quic_datagrams,
            PathKind::DirectQuicStream | PathKind::DirectTcpStream | PathKind::CircuitRelay => true,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PathSet {
    candidates: Vec<PathCandidate>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathSelectionChange {
    pub peer: PeerId,
    pub previous: Option<PathCandidate>,
    pub current: Option<PathCandidate>,
}

impl PathSelectionChange {
    #[must_use]
    pub const fn promoted_to_direct(self) -> bool {
        matches!(
            (self.previous, self.current),
            (Some(previous), Some(current)) if previous.is_relay() && current.is_direct()
        )
    }

    #[must_use]
    pub const fn fell_back_to_relay(self) -> bool {
        matches!(
            (self.previous, self.current),
            (Some(previous), Some(current)) if previous.is_direct() && current.is_relay()
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PathRuntimeStats {
    pub healthy_direct_udp_datagram_paths: u64,
    pub healthy_direct_quic_datagram_paths: u64,
    pub healthy_direct_quic_stream_paths: u64,
    pub healthy_direct_tcp_stream_paths: u64,
    pub healthy_relay_paths: u64,
    pub peers_with_supported_path: u64,
    pub peers_without_supported_path: u64,
}

impl PathSet {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            candidates: Vec::new(),
        }
    }

    pub fn upsert(&mut self, candidate: PathCandidate) {
        if let Some(existing) = self
            .candidates
            .iter_mut()
            .find(|existing| existing.peer == candidate.peer && existing.kind == candidate.kind)
        {
            *existing = candidate;
        } else {
            self.candidates.push(candidate);
        }
    }

    #[must_use]
    pub fn best_for(&self, peer: PeerId) -> Option<PathCandidate> {
        self.best_supported_for(
            peer,
            PathTransportSupport {
                udp_datagrams: true,
                quic_datagrams: true,
            },
        )
    }

    #[must_use]
    pub fn best_supported_for(
        &self,
        peer: PeerId,
        support: PathTransportSupport,
    ) -> Option<PathCandidate> {
        self.candidates
            .iter()
            .copied()
            .filter(|candidate| {
                candidate.peer == peer
                    && candidate.is_selectable()
                    && support.supports(candidate.kind)
            })
            .max_by_key(|candidate| candidate.score())
    }

    pub fn candidates_for(&self, peer: PeerId) -> impl Iterator<Item = PathCandidate> + '_ {
        self.candidates
            .iter()
            .copied()
            .filter(move |candidate| candidate.peer == peer)
    }

    pub fn mark_unhealthy(&mut self, peer: PeerId, kind: PathKind) -> Option<PathSelectionChange> {
        let previous = self.best_for(peer);
        if let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|candidate| candidate.peer == peer && candidate.kind == kind)
        {
            candidate.healthy = false;
            candidate.established_connections = 0;
            candidate.failure_penalty = candidate
                .failure_penalty
                .saturating_add(PATH_FAILURE_PENALTY_STEP);
        }
        self.selection_change(peer, previous)
    }

    pub fn record_established(
        &mut self,
        peer: PeerId,
        kind: PathKind,
    ) -> Option<PathSelectionChange> {
        self.record_established_with_mtu(peer, kind, None)
    }

    pub fn record_established_with_mtu(
        &mut self,
        peer: PeerId,
        kind: PathKind,
        estimated_mtu: Option<u16>,
    ) -> Option<PathSelectionChange> {
        let previous = self.best_for(peer);
        if let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|candidate| candidate.peer == peer && candidate.kind == kind)
        {
            candidate.healthy = true;
            if let Some(estimated_mtu) = estimated_mtu {
                candidate.estimated_mtu = Some(estimated_mtu);
            }
            candidate.established_connections = candidate.established_connections.saturating_add(1);
        } else {
            let mut candidate = PathCandidate::new(peer, kind);
            candidate.estimated_mtu = estimated_mtu;
            candidate.established_connections = 1;
            self.candidates.push(candidate);
        }
        self.selection_change(peer, previous)
    }

    pub fn record_closed(&mut self, peer: PeerId, kind: PathKind) -> Option<PathSelectionChange> {
        let previous = self.best_for(peer);
        if let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|candidate| candidate.peer == peer && candidate.kind == kind)
        {
            candidate.established_connections = candidate.established_connections.saturating_sub(1);
            if candidate.established_connections == 0 && !candidate.is_relay() {
                candidate.healthy = false;
            }
        }
        self.selection_change(peer, previous)
    }

    pub fn lower_path_mtu(&mut self, peer: PeerId, kind: PathKind, mtu: u16) -> bool {
        let Some(candidate) = self.candidates.iter_mut().find(|candidate| {
            candidate.peer == peer && candidate.kind == kind && candidate.healthy
        }) else {
            return false;
        };

        if candidate.estimated_mtu.is_none_or(|current| mtu < current) {
            candidate.estimated_mtu = Some(mtu);
            return true;
        }

        false
    }

    pub fn raise_path_mtu(&mut self, peer: PeerId, kind: PathKind, mtu: u16, ceiling: u16) -> bool {
        let Some(candidate) = self.candidates.iter_mut().find(|candidate| {
            candidate.peer == peer && candidate.kind == kind && candidate.healthy
        }) else {
            return false;
        };
        let mtu = mtu.min(ceiling);

        if candidate.estimated_mtu.is_some_and(|current| mtu > current) {
            candidate.estimated_mtu = Some(mtu);
            return true;
        }

        false
    }

    pub fn record_rtt(
        &mut self,
        peer: PeerId,
        kind: PathKind,
        rtt_ms: u16,
    ) -> Option<PathSelectionChange> {
        let previous = self.best_for(peer);
        let candidate = self.candidates.iter_mut().find(|candidate| {
            candidate.peer == peer && candidate.kind == kind && candidate.healthy
        })?;
        candidate.observed_rtt_ms = Some(rtt_ms);
        candidate.failure_penalty = candidate
            .failure_penalty
            .saturating_sub(PATH_FAILURE_PENALTY_RECOVERY_STEP);
        self.selection_change(peer, previous)
    }

    #[must_use]
    pub fn path_mtu(&self, peer: PeerId, kind: PathKind) -> Option<u16> {
        self.candidates
            .iter()
            .find(|candidate| candidate.peer == peer && candidate.kind == kind)
            .and_then(|candidate| candidate.estimated_mtu)
    }

    #[must_use]
    pub fn path_rtt(&self, peer: PeerId, kind: PathKind) -> Option<u16> {
        self.candidates
            .iter()
            .find(|candidate| candidate.peer == peer && candidate.kind == kind)
            .and_then(|candidate| candidate.observed_rtt_ms)
    }

    fn selection_change(
        &self,
        peer: PeerId,
        previous: Option<PathCandidate>,
    ) -> Option<PathSelectionChange> {
        let current = self.best_for(peer);
        if previous.map(|path| path.kind) == current.map(|path| path.kind) {
            None
        } else {
            Some(PathSelectionChange {
                peer,
                previous,
                current,
            })
        }
    }

    #[must_use]
    pub fn has_healthy_path(&self, peer: PeerId) -> bool {
        self.best_for(peer).is_some()
    }

    #[must_use]
    pub fn has_supported_path(&self, peer: PeerId, support: PathTransportSupport) -> bool {
        self.best_supported_for(peer, support).is_some()
    }

    #[must_use]
    pub fn runtime_stats_for_peers(
        &self,
        peers: impl IntoIterator<Item = PeerId>,
        mut support_for_peer: impl FnMut(PeerId) -> PathTransportSupport,
    ) -> PathRuntimeStats {
        let mut stats = PathRuntimeStats::default();
        for candidate in self.candidates.iter().filter(|candidate| candidate.healthy) {
            match candidate.kind {
                PathKind::DirectUdpDatagram => stats.healthy_direct_udp_datagram_paths += 1,
                PathKind::DirectQuicDatagram => stats.healthy_direct_quic_datagram_paths += 1,
                PathKind::DirectQuicStream => stats.healthy_direct_quic_stream_paths += 1,
                PathKind::DirectTcpStream => stats.healthy_direct_tcp_stream_paths += 1,
                PathKind::CircuitRelay => stats.healthy_relay_paths += 1,
            }
        }

        for peer in peers {
            if self.has_supported_path(peer, support_for_peer(peer)) {
                stats.peers_with_supported_path += 1;
            } else {
                stats.peers_without_supported_path += 1;
            }
        }

        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(seed: u8) -> PeerId {
        PeerId::from_bytes([seed; 32])
    }

    #[test]
    fn prefers_direct_datagrams_over_relay() {
        let mut paths = PathSet::new();
        paths.upsert(PathCandidate::new(peer(1), PathKind::CircuitRelay));
        paths.upsert(PathCandidate::new(peer(1), PathKind::DirectQuicDatagram));

        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.kind),
            Some(PathKind::DirectQuicDatagram)
        );
    }

    #[test]
    fn falls_back_when_best_path_is_unhealthy() {
        let mut paths = PathSet::new();
        paths.record_established(peer(1), PathKind::CircuitRelay);
        paths.upsert(PathCandidate::new(peer(1), PathKind::DirectQuicDatagram));

        let change = paths.mark_unhealthy(peer(1), PathKind::DirectQuicDatagram);

        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.kind),
            Some(PathKind::CircuitRelay)
        );
        assert_eq!(
            change.map(|change| {
                (
                    change.previous.map(|candidate| candidate.kind),
                    change.current.map(|candidate| candidate.kind),
                )
            }),
            Some((
                Some(PathKind::DirectQuicDatagram),
                Some(PathKind::CircuitRelay)
            ))
        );
    }

    #[test]
    fn falls_back_to_supported_path_when_datagrams_are_unavailable() {
        let mut paths = PathSet::new();
        paths.upsert(PathCandidate::new(peer(1), PathKind::DirectQuicDatagram));
        paths.upsert(PathCandidate::new(peer(1), PathKind::DirectQuicStream));

        assert_eq!(
            paths
                .best_supported_for(peer(1), PathTransportSupport::stream_fallback())
                .map(|path| path.kind),
            Some(PathKind::DirectQuicStream)
        );
    }

    #[test]
    fn ignores_datagram_only_path_when_datagrams_are_unavailable() {
        let mut paths = PathSet::new();
        paths.upsert(PathCandidate::new(peer(1), PathKind::DirectQuicDatagram));

        assert_eq!(
            paths.best_supported_for(peer(1), PathTransportSupport::stream_fallback()),
            None
        );
        assert!(!paths.has_supported_path(peer(1), PathTransportSupport::stream_fallback()));
    }

    #[test]
    fn prefers_datagrams_when_transport_support_allows_them() {
        let mut paths = PathSet::new();
        paths.upsert(PathCandidate::new(peer(1), PathKind::DirectQuicStream));
        paths.upsert(PathCandidate::new(peer(1), PathKind::DirectQuicDatagram));

        assert_eq!(
            paths
                .best_supported_for(
                    peer(1),
                    PathTransportSupport {
                        udp_datagrams: false,
                        quic_datagrams: true
                    }
                )
                .map(|path| path.kind),
            Some(PathKind::DirectQuicDatagram)
        );
    }

    #[test]
    fn reports_when_healthy_path_exists() {
        let mut paths = PathSet::new();
        paths.upsert(PathCandidate::new(peer(1), PathKind::DirectTcpStream));

        assert!(paths.has_healthy_path(peer(1)));

        paths.mark_unhealthy(peer(1), PathKind::DirectTcpStream);

        assert!(!paths.has_healthy_path(peer(1)));
    }

    #[test]
    fn tracks_established_connection_counts_per_path() {
        let mut paths = PathSet::new();

        paths.record_established(peer(1), PathKind::DirectTcpStream);
        paths.record_established(peer(1), PathKind::DirectTcpStream);

        assert_eq!(
            paths
                .best_for(peer(1))
                .map(|path| path.established_connections),
            Some(2)
        );

        paths.record_closed(peer(1), PathKind::DirectTcpStream);
        assert!(paths.has_healthy_path(peer(1)));

        paths.record_closed(peer(1), PathKind::DirectTcpStream);
        assert!(!paths.has_healthy_path(peer(1)));
    }

    #[test]
    fn demotion_clears_stale_connection_count() {
        let mut paths = PathSet::new();

        paths.record_established(peer(1), PathKind::DirectTcpStream);
        paths.record_established(peer(1), PathKind::DirectTcpStream);
        paths.mark_unhealthy(peer(1), PathKind::DirectTcpStream);

        let candidate = paths
            .candidates_for(peer(1))
            .find(|candidate| candidate.kind == PathKind::DirectTcpStream)
            .expect("direct tcp candidate");
        assert!(!candidate.healthy);
        assert_eq!(candidate.established_connections, 0);
        assert_eq!(paths.best_for(peer(1)), None);
    }

    #[test]
    fn relay_paths_remain_healthy_across_transient_stream_closes() {
        let mut paths = PathSet::new();

        paths.record_established(peer(1), PathKind::CircuitRelay);
        paths.record_closed(peer(1), PathKind::CircuitRelay);

        let relay = paths
            .candidates_for(peer(1))
            .find(|candidate| candidate.kind == PathKind::CircuitRelay)
            .expect("relay remains available for redial");
        assert_eq!(relay.kind, PathKind::CircuitRelay);
        assert_eq!(relay.established_connections, 0);
        assert!(relay.healthy);
        assert_eq!(paths.best_for(peer(1)), None);
    }

    #[test]
    fn reports_selection_changes_on_promotion_and_fallback() {
        let mut paths = PathSet::new();

        assert_eq!(
            paths
                .record_established(peer(1), PathKind::CircuitRelay)
                .map(|change| (
                    change.previous.map(|path| path.kind),
                    change.current.map(|path| path.kind)
                )),
            Some((None, Some(PathKind::CircuitRelay)))
        );
        let promotion = paths
            .record_established(peer(1), PathKind::DirectQuicStream)
            .expect("selected path changed");
        assert!(promotion.promoted_to_direct());
        assert_eq!(
            promotion.previous.map(|path| path.kind),
            Some(PathKind::CircuitRelay)
        );
        assert_eq!(
            promotion.current.map(|path| path.kind),
            Some(PathKind::DirectQuicStream)
        );

        let fallback = paths
            .record_closed(peer(1), PathKind::DirectQuicStream)
            .expect("selected path changed");
        assert!(fallback.fell_back_to_relay());
        assert_eq!(
            fallback.previous.map(|path| path.kind),
            Some(PathKind::DirectQuicStream)
        );
        assert_eq!(
            fallback.current.map(|path| path.kind),
            Some(PathKind::CircuitRelay)
        );
    }

    #[test]
    fn suppresses_selection_changes_when_best_path_stays_the_same() {
        let mut paths = PathSet::new();
        paths.record_established(peer(1), PathKind::DirectQuicStream);

        assert_eq!(
            paths.record_established(peer(1), PathKind::DirectTcpStream),
            None
        );
        assert_eq!(
            paths.record_closed(peer(1), PathKind::DirectTcpStream),
            None
        );
    }

    #[test]
    fn records_and_updates_path_mtu_estimates() {
        let mut paths = PathSet::new();

        paths.record_established_with_mtu(peer(1), PathKind::DirectTcpStream, Some(1200));
        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.estimated_mtu),
            Some(Some(1200))
        );
        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.effective_mtu(1280)),
            Some(1200)
        );

        paths.record_established_with_mtu(peer(1), PathKind::DirectTcpStream, Some(1180));

        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.estimated_mtu),
            Some(Some(1180))
        );
    }

    #[test]
    fn path_mtu_learning_only_lowers_healthy_paths() {
        let mut paths = PathSet::new();
        paths.record_established_with_mtu(peer(1), PathKind::DirectTcpStream, Some(1200));

        assert!(paths.lower_path_mtu(peer(1), PathKind::DirectTcpStream, 1180));
        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.estimated_mtu),
            Some(Some(1180))
        );
        assert!(!paths.lower_path_mtu(peer(1), PathKind::DirectTcpStream, 1190));
        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.estimated_mtu),
            Some(Some(1180))
        );

        paths.record_closed(peer(1), PathKind::DirectTcpStream);
        assert!(!paths.lower_path_mtu(peer(1), PathKind::DirectTcpStream, 1100));
    }

    #[test]
    fn path_mtu_probe_learning_only_raises_healthy_paths_to_ceiling() {
        let mut paths = PathSet::new();
        paths.record_established_with_mtu(peer(1), PathKind::DirectQuicDatagram, Some(1000));

        assert!(paths.raise_path_mtu(peer(1), PathKind::DirectQuicDatagram, 1100, 1200));
        assert_eq!(
            paths.path_mtu(peer(1), PathKind::DirectQuicDatagram),
            Some(1100)
        );
        assert!(!paths.raise_path_mtu(peer(1), PathKind::DirectQuicDatagram, 1050, 1200));
        assert_eq!(
            paths.path_mtu(peer(1), PathKind::DirectQuicDatagram),
            Some(1100)
        );
        assert!(paths.raise_path_mtu(peer(1), PathKind::DirectQuicDatagram, 1300, 1200));
        assert_eq!(
            paths.path_mtu(peer(1), PathKind::DirectQuicDatagram),
            Some(1200)
        );

        paths.record_closed(peer(1), PathKind::DirectQuicDatagram);
        assert!(!paths.raise_path_mtu(peer(1), PathKind::DirectQuicDatagram, 1300, 1400));
    }

    #[test]
    fn rtt_updates_can_change_selected_path() {
        let mut paths = PathSet::new();
        paths.record_established(peer(1), PathKind::DirectQuicDatagram);
        paths.record_established(peer(1), PathKind::DirectQuicStream);

        let change = paths
            .record_rtt(peer(1), PathKind::DirectQuicDatagram, 900)
            .expect("high datagram rtt should change selected path");

        assert_eq!(
            paths.path_rtt(peer(1), PathKind::DirectQuicDatagram),
            Some(900)
        );
        assert_eq!(
            change.previous.map(|path| path.kind),
            Some(PathKind::DirectQuicDatagram)
        );
        assert_eq!(
            change.current.map(|path| path.kind),
            Some(PathKind::DirectQuicStream)
        );
    }

    #[test]
    fn failure_penalty_survives_reconnect_and_decays_on_success() {
        let mut paths = PathSet::new();
        paths.record_established(peer(1), PathKind::CircuitRelay);
        paths.record_rtt(peer(1), PathKind::CircuitRelay, 50);
        paths.record_established(peer(1), PathKind::DirectTcpStream);
        paths.record_rtt(peer(1), PathKind::DirectTcpStream, 70);

        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.kind),
            Some(PathKind::DirectTcpStream)
        );

        paths.mark_unhealthy(peer(1), PathKind::DirectTcpStream);
        paths.record_established(peer(1), PathKind::DirectTcpStream);

        let direct = paths
            .candidates_for(peer(1))
            .find(|path| path.kind == PathKind::DirectTcpStream)
            .expect("direct path");
        assert_eq!(direct.failure_penalty, PATH_FAILURE_PENALTY_STEP);
        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.kind),
            Some(PathKind::CircuitRelay)
        );

        for _ in 0..5 {
            paths.record_rtt(peer(1), PathKind::DirectTcpStream, 70);
        }

        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.kind),
            Some(PathKind::DirectTcpStream)
        );
    }

    #[test]
    fn runtime_stats_report_healthy_paths_and_supported_peers() {
        let mut paths = PathSet::new();
        paths.record_established(peer(1), PathKind::DirectQuicStream);
        paths.record_established(peer(2), PathKind::CircuitRelay);
        paths.record_established(peer(3), PathKind::DirectQuicDatagram);
        paths.record_closed(peer(2), PathKind::CircuitRelay);

        let stats =
            paths.runtime_stats_for_peers([peer(1), peer(2), peer(3), peer(4)], |candidate_peer| {
                PathTransportSupport {
                    udp_datagrams: false,
                    quic_datagrams: candidate_peer == peer(3),
                }
            });

        assert_eq!(
            stats,
            PathRuntimeStats {
                healthy_direct_udp_datagram_paths: 0,
                healthy_direct_quic_datagram_paths: 1,
                healthy_direct_quic_stream_paths: 1,
                healthy_direct_tcp_stream_paths: 0,
                healthy_relay_paths: 1,
                peers_with_supported_path: 2,
                peers_without_supported_path: 2,
            }
        );
    }

    #[test]
    fn penalizes_high_latency_paths() {
        let mut slow_datagram = PathCandidate::new(peer(1), PathKind::DirectQuicDatagram);
        slow_datagram.observed_rtt_ms = Some(900);

        let mut fast_stream = PathCandidate::new(peer(1), PathKind::DirectQuicStream);
        fast_stream.observed_rtt_ms = Some(10);

        let mut paths = PathSet::new();
        paths.upsert(slow_datagram);
        paths.upsert(fast_stream);

        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.kind),
            Some(PathKind::DirectQuicStream)
        );
    }
}
