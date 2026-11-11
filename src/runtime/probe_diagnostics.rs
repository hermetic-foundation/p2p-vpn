use std::time::{Duration, Instant};

use crate::PeerId;

/// A single bounded, ephemeral lease; never a persistent routing policy.
#[derive(Debug, Default)]
pub(super) struct ProbePause {
    peer: Option<PeerId>,
    until: Option<Instant>,
}

impl ProbePause {
    pub(super) fn set(&mut self, peer: Option<PeerId>, seconds: u16, now: Instant) -> bool {
        if seconds > 1800 {
            return false;
        }
        self.peer = peer;
        self.until = (seconds != 0).then(|| now + Duration::from_secs(u64::from(seconds)));
        true
    }

    pub(super) fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    pub(super) fn blocks(&self, peer: PeerId, now: Instant) -> bool {
        self.active(now) && self.peer.is_none_or(|target| target == peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targeted_pause_preserves_other_peers_and_expires() {
        let a = PeerId::from_bytes([1; 32]);
        let b = PeerId::from_bytes([2; 32]);
        let now = Instant::now();
        let mut pause = ProbePause::default();
        assert!(!pause.blocks(a, now));
        assert!(pause.set(Some(a), 60, now));
        assert!(pause.blocks(a, now));
        assert!(!pause.blocks(b, now));
        assert!(!pause.blocks(a, now + Duration::from_mins(1)));
    }

    #[test]
    fn all_peer_pause_resumes_and_rejects_invalid_duration_without_mutation() {
        let a = PeerId::from_bytes([1; 32]);
        let b = PeerId::from_bytes([2; 32]);
        let now = Instant::now();
        let mut pause = ProbePause::default();
        assert!(pause.set(None, 1800, now));
        assert!(pause.blocks(a, now) && pause.blocks(b, now));
        assert!(!pause.set(Some(a), 1801, now));
        assert!(pause.blocks(b, now));
        assert!(pause.set(None, 0, now));
        assert!(!pause.active(now));
    }
}
