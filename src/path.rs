use crate::{PathKind, PeerId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathCandidate {
    pub peer: PeerId,
    pub kind: PathKind,
    pub observed_rtt_ms: Option<u16>,
    pub relay: bool,
    pub healthy: bool,
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
        self.candidates
            .iter()
            .copied()
            .filter(|candidate| candidate.peer == peer && candidate.healthy)
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
