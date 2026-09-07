use std::{collections::BTreeSet, num::NonZeroUsize};

use crate::record;

/// Per-job key-batch limits. No record values or provider addresses are snapshotted.
#[derive(Clone, Copy, Debug)]
pub struct BackgroundJobLimits {
    pub(super) keys: usize,
    pub(super) bytes: usize,
    pub(super) input_bytes: usize,
}

impl BackgroundJobLimits {
    /// Each key must fit both the batch byte budget and the key/value input limit.
    /// The record job's skipped-key set has the same count/byte bounds as a batch.
    pub fn new(keys: NonZeroUsize, bytes: NonZeroUsize, input_bytes: NonZeroUsize) -> Self {
        Self {
            keys: keys.get(),
            bytes: bytes.get(),
            input_bytes: input_bytes.get(),
        }
    }

    pub(super) fn accepts_key(self, key: &record::Key) -> bool {
        key.as_ref().len() <= self.bytes.min(self.input_bytes)
    }
}

/// Aggregate bounded-job storage for one behaviour; excludes the record store,
/// admitted queries, allocator overhead, and unconfigured legacy job snapshots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackgroundJobUsage {
    pub bounded_jobs: usize,
    pub pending_keys: usize,
    pub pending_key_bytes: usize,
    /// Resume cursors and first-excluded page boundaries; at most two keys per job.
    pub cursor_bytes: usize,
    pub skipped_keys: usize,
    pub skipped_key_bytes: usize,
    pub rejected_inputs: u64,
    pub rejected_skips: u64,
}

pub(super) struct KeyBatch {
    limits: BackgroundJobLimits,
    keys: BTreeSet<Vec<u8>>,
    bytes: usize,
    cursor: Option<Vec<u8>>,
    boundary: Option<Vec<u8>>,
    pub(super) publish: bool,
}

impl KeyBatch {
    pub(super) fn new(limits: BackgroundJobLimits, publish: bool) -> Self {
        Self {
            limits,
            keys: BTreeSet::new(),
            bytes: 0,
            cursor: None,
            boundary: None,
            publish,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub(super) fn start_page(&mut self) {
        debug_assert!(self.keys.is_empty());
        self.boundary = None;
    }

    fn exclude(&mut self, key: Vec<u8>) {
        if self
            .boundary
            .as_ref()
            .is_none_or(|boundary| key < *boundary)
        {
            self.boundary = Some(key);
        }
    }

    /// Keep the smallest contiguous key prefix after the cursor. Evict only the
    /// largest buffered keys before insertion, so byte pressure cannot starve a
    /// large key by advancing the cursor past it to smaller, later records.
    pub(super) fn consider(&mut self, key: &record::Key) {
        if !self.limits.accepts_key(key) {
            return;
        }
        let key = key.as_ref();
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| key <= cursor.as_slice())
            || self
                .boundary
                .as_ref()
                .is_some_and(|boundary| key >= boundary.as_slice())
            || self.keys.contains(key)
        {
            return;
        }
        while self.keys.len() >= self.limits.keys
            || key.len() > self.limits.bytes.saturating_sub(self.bytes)
        {
            let Some(last) = self.keys.last() else { return };
            if key >= last.as_slice() {
                self.exclude(key.to_vec());
                return;
            }
            let excluded = self.keys.pop_last().expect("last exists");
            self.bytes -= excluded.len();
            self.exclude(excluded);
        }
        self.keys.insert(key.to_vec());
        self.bytes += key.len();
    }

    pub(super) fn pop(&mut self) -> Option<record::Key> {
        let key = self.keys.pop_first()?;
        self.bytes -= key.len();
        let result = record::Key::new(&key);
        self.cursor = Some(key);
        Some(result)
    }

    pub(super) fn remove(&mut self, key: &record::Key) {
        if let Some(key) = self.keys.take(key.as_ref()) {
            self.bytes -= key.len();
        }
    }

    pub(super) fn add_usage(&self, usage: &mut BackgroundJobUsage) {
        usage.pending_keys += self.keys.len();
        usage.pending_key_bytes += self.bytes;
        usage.cursor_bytes += self.cursor.as_ref().map_or(0, Vec::len);
        usage.cursor_bytes += self.boundary.as_ref().map_or(0, Vec::len);
    }
}
