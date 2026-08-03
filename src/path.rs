use crate::{PathKind, PeerId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathCandidate {
    pub peer: PeerId,
    pub kind: PathKind,
    pub observed_rtt_ms: Option<u16>,
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
            relay: matches!(kind, PathKind::CircuitRelay),
            healthy: true,
            established_connections: 0,
        }
    }

    #[must_use]
    pub fn score(self) -> i32 {
        if !self.healthy {
            return i32::MIN;
        }

        let latency_penalty = self
            .observed_rtt_ms
            .map_or(0, |rtt| i32::from(rtt.saturating_div(10)));
        i32::from(self.kind.default_score()) - latency_penalty
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PathTransportSupport {
    pub quic_datagrams: bool,
}

impl PathTransportSupport {
    #[must_use]
    pub const fn stream_fallback() -> Self {
        Self {
            quic_datagrams: false,
        }
    }

    #[must_use]
    pub const fn supports(self, kind: PathKind) -> bool {
        !kind.requires_quic_datagrams() || self.quic_datagrams
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PathSet {
    candidates: Vec<PathCandidate>,
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
                candidate.peer == peer && candidate.healthy && support.supports(candidate.kind)
            })
            .max_by_key(|candidate| candidate.score())
    }

    pub fn mark_unhealthy(&mut self, peer: PeerId, kind: PathKind) {
        if let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|candidate| candidate.peer == peer && candidate.kind == kind)
        {
            candidate.healthy = false;
        }
    }

    pub fn record_established(&mut self, peer: PeerId, kind: PathKind) {
        if let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|candidate| candidate.peer == peer && candidate.kind == kind)
        {
            candidate.healthy = true;
            candidate.established_connections = candidate.established_connections.saturating_add(1);
        } else {
            let mut candidate = PathCandidate::new(peer, kind);
            candidate.established_connections = 1;
            self.candidates.push(candidate);
        }
    }

    pub fn record_closed(&mut self, peer: PeerId, kind: PathKind) {
        if let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|candidate| candidate.peer == peer && candidate.kind == kind)
        {
            candidate.established_connections = candidate.established_connections.saturating_sub(1);
            if candidate.established_connections == 0 {
                candidate.healthy = false;
            }
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
        paths.upsert(PathCandidate::new(peer(1), PathKind::CircuitRelay));
        paths.upsert(PathCandidate::new(peer(1), PathKind::DirectQuicDatagram));

        paths.mark_unhealthy(peer(1), PathKind::DirectQuicDatagram);

        assert_eq!(
            paths.best_for(peer(1)).map(|path| path.kind),
            Some(PathKind::CircuitRelay)
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
