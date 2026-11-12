//! Bandwidth-aware sync: when to sync, what to send first, what fits in a budget, and
//! how batches are compressed on the wire.
//!
//! A replica's entries must arrive in sequence order. The planner therefore never sends
//! a later entry of a replica without the earlier ones: when an entry does not fit, the
//! rest of that replica's entries wait too.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};

use crate::crdt::ReplicaId;
use crate::sync::{Change, SyncEntry};

/// How urgently an entry should cross a slow link.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SyncPriority {
    /// Deletes, and changes to the configured priority fields.
    High,
    /// Everything else.
    Normal,
}

/// A snapshot of the link to the upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkState {
    /// Whether the link is up at all.
    pub up: bool,
    /// Measured throughput in bytes per second.
    pub bytes_per_sec: u64,
}

/// The result of planning a budgeted send.
#[derive(Debug, Default, PartialEq)]
pub struct Batch {
    /// Entries to send now, in an order that keeps each replica's sequence intact.
    pub send: Vec<SyncEntry>,
    /// Entries that did not fit in the budget.
    pub deferred: Vec<SyncEntry>,
}

/// Decides when and how much to sync.
#[derive(Clone, Debug, Default)]
pub struct BandwidthManager {
    min_bytes_per_sec: u64,
    priority_fields: Vec<String>,
}

impl BandwidthManager {
    /// Creates a manager that syncs only over links at least `min_bytes_per_sec` fast.
    pub fn new(min_bytes_per_sec: u64) -> Self {
        BandwidthManager {
            min_bytes_per_sec,
            priority_fields: Vec::new(),
        }
    }

    /// Marks fields whose changes should go out first.
    pub fn with_priority_fields(
        mut self,
        fields: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.priority_fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Whether a sync should start now on this link.
    pub fn should_sync(&self, link: LinkState) -> bool {
        link.up && link.bytes_per_sec >= self.min_bytes_per_sec
    }

    /// Bytes that fit in a window of `seconds` on this link.
    pub fn budget_for(&self, link: LinkState, seconds: u64) -> usize {
        if !link.up {
            return 0;
        }
        link.bytes_per_sec.saturating_mul(seconds) as usize
    }

    /// Priority of one entry.
    pub fn priority(&self, entry: &SyncEntry) -> SyncPriority {
        match &entry.change {
            Change::Delete => SyncPriority::High,
            Change::Set { field, .. } if self.priority_fields.contains(field) => SyncPriority::High,
            Change::Set { .. } => SyncPriority::Normal,
        }
    }

    /// Splits `entries` into a batch that fits in `budget` bytes and the rest. Replicas
    /// with high-priority entries go first. Within a replica, sequence order is kept, so
    /// a replica's entries are cut at the first one that does not fit.
    pub fn plan(&self, entries: Vec<SyncEntry>, budget: usize) -> Batch {
        // Group by replica, preserving sequence order.
        let mut by_replica: BTreeMap<ReplicaId, Vec<SyncEntry>> = BTreeMap::new();
        for entry in entries {
            by_replica.entry(entry.replica).or_default().push(entry);
        }
        // Replicas with a high-priority entry go first.
        let mut order: Vec<(SyncPriority, ReplicaId)> = by_replica
            .iter()
            .map(|(r, es)| {
                let best = es
                    .iter()
                    .map(|e| self.priority(e))
                    .min()
                    .unwrap_or(SyncPriority::Normal);
                (best, *r)
            })
            .collect();
        order.sort();

        let mut batch = Batch::default();
        let mut used = 0usize;
        for (_, replica) in order {
            let mut cut = false;
            for entry in by_replica.remove(&replica).unwrap_or_default() {
                let size = postcard::to_stdvec(&entry)
                    .map(|v| v.len())
                    .unwrap_or(usize::MAX);
                if cut || used.saturating_add(size) > budget {
                    cut = true;
                    batch.deferred.push(entry);
                } else {
                    used += size;
                    batch.send.push(entry);
                }
            }
        }
        batch
    }

    /// Encodes and compresses a batch for the wire.
    pub fn compress(&self, entries: &[SyncEntry]) -> Result<Vec<u8>, String> {
        let raw = postcard::to_stdvec(entries).map_err(|e| e.to_string())?;
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).map_err(|e| e.to_string())?;
        encoder.finish().map_err(|e| e.to_string())
    }

    /// Reverses [`compress`](Self::compress).
    pub fn decompress(&self, bytes: &[u8]) -> Result<Vec<SyncEntry>, String> {
        let mut decoder = DeflateDecoder::new(bytes);
        let mut raw = Vec::new();
        decoder.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        postcard::from_bytes(&raw).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crdt::Stamp;
    use tpt_search_core::{DocumentId, FieldValue};

    fn entry(replica: ReplicaId, seq: u64, field: &str) -> SyncEntry {
        SyncEntry {
            replica,
            seq,
            stamp: Stamp {
                clock: seq,
                replica,
            },
            doc: DocumentId::from("d"),
            change: Change::Set {
                field: field.into(),
                value: Some(FieldValue::Text("x".repeat(40))),
            },
        }
    }

    #[test]
    fn link_gates_sync_on_speed_and_state() {
        let bw = BandwidthManager::new(1_000);
        assert!(bw.should_sync(LinkState {
            up: true,
            bytes_per_sec: 5_000
        }));
        assert!(!bw.should_sync(LinkState {
            up: true,
            bytes_per_sec: 10
        }));
        assert!(!bw.should_sync(LinkState {
            up: false,
            bytes_per_sec: 5_000
        }));
        assert_eq!(
            bw.budget_for(
                LinkState {
                    up: true,
                    bytes_per_sec: 100
                },
                3
            ),
            300
        );
    }

    #[test]
    fn priority_fields_and_deletes_go_first() {
        let bw = BandwidthManager::default().with_priority_fields(["title"]);
        assert_eq!(bw.priority(&entry(1, 1, "title")), SyncPriority::High);
        assert_eq!(bw.priority(&entry(1, 1, "body")), SyncPriority::Normal);
    }

    #[test]
    fn plan_respects_budget_and_sequence_order() {
        let bw = BandwidthManager::default().with_priority_fields(["title"]);
        let entries = vec![
            entry(1, 1, "body"),
            entry(1, 2, "body"),
            entry(1, 3, "body"),
            entry(2, 1, "title"),
        ];
        let one = postcard::to_stdvec(&entries[0]).unwrap().len();
        // Room for the high-priority entry and one more.
        let batch = bw.plan(entries, 2 * one + 10);
        assert_eq!(batch.send[0].replica, 2, "high priority replica goes first");
        let replica1: Vec<u64> = batch
            .send
            .iter()
            .filter(|e| e.replica == 1)
            .map(|e| e.seq)
            .collect();
        assert_eq!(
            replica1,
            [1],
            "later entries of replica 1 wait behind the cut"
        );
        assert_eq!(batch.deferred.len(), 2);
    }

    #[test]
    fn compression_round_trips_and_shrinks_repetitive_data() {
        let bw = BandwidthManager::default();
        let entries: Vec<SyncEntry> = (1..=50).map(|s| entry(1, s, "body")).collect();
        let packed = bw.compress(&entries).unwrap();
        let raw = postcard::to_stdvec(&entries).unwrap();
        assert!(packed.len() < raw.len());
        assert_eq!(bw.decompress(&packed).unwrap(), entries);
    }
}
