//! The sync engine: a replica's documents, its change log, and the offline queue.
//!
//! Every change a replica makes is a [`SyncEntry`] with a per-replica sequence number.
//! A [`VersionVector`] records, for each replica, the highest sequence number this
//! replica has applied. A peer sends whatever the other side's version vector lacks, so
//! each exchange transfers only the delta.
//!
//! The offline queue is the set of local entries the upstream (usually the cloud) has
//! not acknowledged. Edits made offline stay queued until a later upload succeeds. When
//! the engine is opened with a path, every entry and every acknowledgement is written to
//! a [`WriteAheadLog`] first, so the queue survives a restart.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tpt_search_core::{DocumentId, FieldValue, StorageError};
use tpt_search_storage::{WalEntry, WriteAheadLog};

use crate::bandwidth::{BandwidthManager, Batch};
use crate::crdt::{LwwDocument, ReplicaId, Stamp};
use crate::monitor::SyncReport;

/// Highest applied sequence number per replica.
pub type VersionVector = BTreeMap<ReplicaId, u64>;

/// What a change does to a document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Change {
    /// Sets a field, or clears it with `None`.
    Set {
        /// Field name.
        field: String,
        /// New value, or `None` to clear.
        value: Option<FieldValue>,
    },
    /// Deletes the document.
    Delete,
}

/// One change, as it is logged and exchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncEntry {
    /// Replica that made the change.
    pub replica: ReplicaId,
    /// That replica's sequence number for the change, starting at 1.
    pub seq: u64,
    /// Logical timestamp of the change.
    pub stamp: Stamp,
    /// Document changed.
    pub doc: DocumentId,
    /// What changed.
    pub change: Change,
}

/// Records written to the WAL. Each is a postcard-encoded `Record` in an opaque WAL entry.
#[derive(Serialize, Deserialize)]
enum Record {
    Entry(SyncEntry),
    Ack(VersionVector),
}

/// Errors from the sync engine.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The write-ahead log failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A stored record could not be decoded.
    #[error("corrupt sync record: {0}")]
    Corrupt(String),
}

/// One replica of the synced data.
pub struct SyncEngine {
    replica: ReplicaId,
    clock: u64,
    docs: BTreeMap<DocumentId, LwwDocument>,
    log: Vec<SyncEntry>,
    version: VersionVector,
    acked: VersionVector,
    wal: Option<WriteAheadLog>,
}

impl SyncEngine {
    /// Creates an in-memory replica. Nothing survives a restart.
    pub fn new(replica: ReplicaId) -> Self {
        SyncEngine {
            replica,
            clock: 0,
            docs: BTreeMap::new(),
            log: Vec::new(),
            version: VersionVector::new(),
            acked: VersionVector::new(),
            wal: None,
        }
    }

    /// Opens a durable replica at `path`, replaying its log and acknowledgements.
    pub fn open(replica: ReplicaId, path: &Path) -> Result<Self, SyncError> {
        let mut engine = SyncEngine::new(replica);
        let wal = WriteAheadLog::open(path)?;
        for (_, record) in wal.replay()? {
            let WalEntry::Opaque(bytes) = record else {
                continue;
            };
            let record: Record =
                postcard::from_bytes(&bytes).map_err(|e| SyncError::Corrupt(e.to_string()))?;
            match record {
                Record::Entry(entry) => {
                    engine.integrate(entry)?;
                }
                Record::Ack(upto) => merge_versions(&mut engine.acked, &upto),
            }
        }
        engine.wal = Some(wal);
        Ok(engine)
    }

    /// This replica's id.
    pub fn replica(&self) -> ReplicaId {
        self.replica
    }

    /// What this replica has applied.
    pub fn version(&self) -> &VersionVector {
        &self.version
    }

    /// The document with this id, live or deleted.
    pub fn document(&self, id: &DocumentId) -> Option<&LwwDocument> {
        self.docs.get(id)
    }

    /// The current value of a field, if the document is live and the field is set.
    pub fn get(&self, id: &DocumentId, field: &str) -> Option<&FieldValue> {
        self.docs.get(id)?.get(field)
    }

    /// Ids of live documents, in order.
    pub fn live_documents(&self) -> Vec<DocumentId> {
        self.docs
            .iter()
            .filter(|(_, d)| d.is_live())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Sets a field on a document, making a local change. Returns the logged entry.
    pub fn set_field(
        &mut self,
        doc: &DocumentId,
        field: &str,
        value: FieldValue,
    ) -> Result<SyncEntry, SyncError> {
        self.local(
            doc,
            Change::Set {
                field: field.to_owned(),
                value: Some(value),
            },
        )
    }

    /// Clears a field on a document.
    pub fn clear_field(&mut self, doc: &DocumentId, field: &str) -> Result<SyncEntry, SyncError> {
        self.local(
            doc,
            Change::Set {
                field: field.to_owned(),
                value: None,
            },
        )
    }

    /// Deletes a document.
    pub fn delete(&mut self, doc: &DocumentId) -> Result<SyncEntry, SyncError> {
        self.local(doc, Change::Delete)
    }

    fn local(&mut self, doc: &DocumentId, change: Change) -> Result<SyncEntry, SyncError> {
        self.clock += 1;
        let entry = SyncEntry {
            replica: self.replica,
            seq: self.version.get(&self.replica).copied().unwrap_or(0) + 1,
            stamp: Stamp {
                clock: self.clock,
                replica: self.replica,
            },
            doc: doc.clone(),
            change,
        };
        self.integrate(entry.clone())?;
        Ok(entry)
    }

    /// Applies one entry in its replica's sequence. Returns `true` if it was new.
    fn integrate(&mut self, entry: SyncEntry) -> Result<bool, SyncError> {
        let have = self.version.get(&entry.replica).copied().unwrap_or(0);
        if entry.seq <= have {
            return Ok(false);
        }
        self.clock = self.clock.max(entry.stamp.clock);
        let doc = self.docs.entry(entry.doc.clone()).or_default();
        match &entry.change {
            Change::Set { field, value } => {
                doc.set_field(field, value.clone(), entry.stamp);
            }
            Change::Delete => {
                doc.delete(entry.stamp);
            }
        }
        self.version.insert(entry.replica, entry.seq);
        if let Some(wal) = self.wal.as_mut() {
            let bytes = postcard::to_stdvec(&Record::Entry(entry.clone()))
                .map_err(|e| SyncError::Corrupt(e.to_string()))?;
            wal.append(WalEntry::Opaque(bytes))?;
        }
        self.log.push(entry);
        Ok(true)
    }

    /// Entries the peer, described by its version vector, does not have.
    pub fn delta_for(&self, peer: &VersionVector) -> Vec<SyncEntry> {
        self.log
            .iter()
            .filter(|e| e.seq > peer.get(&e.replica).copied().unwrap_or(0))
            .cloned()
            .collect()
    }

    /// Local entries the upstream has not acknowledged yet: the offline queue.
    pub fn pending(&self) -> Vec<SyncEntry> {
        self.log
            .iter()
            .filter(|e| e.replica == self.replica)
            .filter(|e| e.seq > self.acked.get(&self.replica).copied().unwrap_or(0))
            .cloned()
            .collect()
    }

    /// Applies entries received from a peer. Entries are applied in sequence order per
    /// replica. Entries that cannot be applied yet (a gap) are reported as errors.
    pub fn apply_delta(&mut self, entries: Vec<SyncEntry>) -> SyncReport {
        let mut report = SyncReport::default();
        let mut remaining = entries;
        loop {
            let before = remaining.len();
            let mut next = Vec::new();
            for entry in remaining {
                let have = self.version.get(&entry.replica).copied().unwrap_or(0);
                if entry.seq <= have {
                    report.duplicates += 1;
                    continue;
                }
                if entry.seq != have + 1 {
                    next.push(entry);
                    continue;
                }
                let conflict = self.conflicts_with(&entry);
                match self.integrate(entry) {
                    Ok(_) => {
                        report.applied += 1;
                        if conflict {
                            report.conflicts += 1;
                        }
                    }
                    Err(e) => report.errors.push(e.to_string()),
                }
            }
            if next.is_empty() {
                break;
            }
            if next.len() == before {
                for entry in &next {
                    report.errors.push(format!(
                        "gap: replica {} seq {} after {}",
                        entry.replica,
                        entry.seq,
                        self.version.get(&entry.replica).copied().unwrap_or(0)
                    ));
                }
                break;
            }
            remaining = next;
        }
        report
    }

    /// Whether `entry` disagrees with a value this replica holds that another replica wrote.
    /// Every replica that applies one side of a conflicting pair observes it, so a pair is
    /// observed once per side that learns of it. Last-writer-wins still decides the value.
    fn conflicts_with(&self, entry: &SyncEntry) -> bool {
        let Change::Set { field, value } = &entry.change else {
            return false;
        };
        let Some(existing) = self.docs.get(&entry.doc).and_then(|d| d.fields.get(field)) else {
            return false;
        };
        existing.stamp.replica != entry.replica && existing.value != *value
    }

    /// Records that the upstream has everything up to `upto`. Persisted when durable.
    pub fn acknowledge(&mut self, upto: &VersionVector) -> Result<(), SyncError> {
        merge_versions(&mut self.acked, upto);
        if let Some(wal) = self.wal.as_mut() {
            let bytes = postcard::to_stdvec(&Record::Ack(self.acked.clone()))
                .map_err(|e| SyncError::Corrupt(e.to_string()))?;
            wal.append(WalEntry::Opaque(bytes))?;
        }
        Ok(())
    }

    /// Sends the offline queue to `upstream` as one compressed batch, within `budget`
    /// bytes, then acknowledges what was accepted. Entries that do not fit wait for the next
    /// upload. Returns what was sent.
    pub fn upload(
        &mut self,
        upstream: &mut SyncEngine,
        bandwidth: &BandwidthManager,
        budget: usize,
    ) -> Result<SyncReport, SyncError> {
        let pending = self.pending_for(upstream.version());
        let Batch { send, deferred } = bandwidth.plan(pending, budget);
        let bytes = bandwidth.compress(&send).map_err(SyncError::Corrupt)?;
        let received = bandwidth.decompress(&bytes).map_err(SyncError::Corrupt)?;
        let mut report = upstream.apply_delta(received);
        report.bytes_sent = bytes.len();
        report.deferred = deferred.len();
        // Acknowledge only what the upstream now holds.
        let upto = upstream.version().clone();
        self.acknowledge(&upto)?;
        Ok(report)
    }

    /// Pulls what `upstream` has that this replica lacks. Returns what arrived.
    pub fn download(
        &mut self,
        upstream: &SyncEngine,
        bandwidth: &BandwidthManager,
    ) -> Result<SyncReport, SyncError> {
        let delta = upstream.delta_for(&self.version);
        let bytes = bandwidth.compress(&delta).map_err(SyncError::Corrupt)?;
        let received = bandwidth.decompress(&bytes).map_err(SyncError::Corrupt)?;
        let mut report = self.apply_delta(received);
        report.bytes_sent = bytes.len();
        Ok(report)
    }

    /// Two-way exchange between peers, without acknowledgements. Used for replica-to-
    /// replica sync where both sides keep everything.
    pub fn exchange(
        a: &mut SyncEngine,
        b: &mut SyncEngine,
        bandwidth: &BandwidthManager,
    ) -> Result<SyncReport, SyncError> {
        let pulled = a.download(b, bandwidth)?;
        let pushed = b.download(a, bandwidth)?;
        Ok(SyncReport {
            applied: pulled.applied + pushed.applied,
            conflicts: pulled.conflicts + pushed.conflicts,
            duplicates: pulled.duplicates + pushed.duplicates,
            bytes_sent: pulled.bytes_sent + pushed.bytes_sent,
            errors: [pulled.errors, pushed.errors].concat(),
            ..SyncReport::default()
        })
    }

    /// Own entries that `peer` lacks, in sequence order.
    fn pending_for(&self, peer: &VersionVector) -> Vec<SyncEntry> {
        self.log
            .iter()
            .filter(|e| e.replica == self.replica)
            .filter(|e| e.seq > peer.get(&self.replica).copied().unwrap_or(0))
            .cloned()
            .collect()
    }
}

fn merge_versions(into: &mut VersionVector, other: &VersionVector) {
    for (&replica, &seq) in other {
        let slot = into.entry(replica).or_default();
        *slot = (*slot).max(seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> DocumentId {
        DocumentId::from(s)
    }

    #[test]
    fn local_edits_are_sequenced_and_queued() {
        let mut e = SyncEngine::new(1);
        e.set_field(&id("d"), "title", "a".into()).unwrap();
        e.set_field(&id("d"), "title", "b".into()).unwrap();
        let seqs: Vec<u64> = e.pending().iter().map(|x| x.seq).collect();
        assert_eq!(seqs, [1, 2]);
        assert_eq!(
            e.get(&id("d"), "title"),
            Some(&FieldValue::Text("b".into()))
        );
    }

    #[test]
    fn delta_contains_only_what_the_peer_lacks() {
        let mut a = SyncEngine::new(1);
        a.set_field(&id("d"), "x", 1i64.into()).unwrap();
        a.set_field(&id("d"), "y", 2i64.into()).unwrap();
        let mut peer = VersionVector::new();
        peer.insert(1, 1);
        let delta = a.delta_for(&peer);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].seq, 2);
    }

    #[test]
    fn duplicate_and_gapped_entries_are_handled() {
        let mut a = SyncEngine::new(1);
        let e1 = a.set_field(&id("d"), "x", 1i64.into()).unwrap();
        let e2 = a.set_field(&id("d"), "x", 2i64.into()).unwrap();
        let mut b = SyncEngine::new(2);
        // Out of order: seq 2 first, then 1. Both should apply.
        let report = b.apply_delta(vec![e2.clone(), e1.clone(), e1.clone()]);
        assert_eq!(report.applied, 2);
        assert_eq!(report.duplicates, 1);
        assert_eq!(b.get(&id("d"), "x"), Some(&FieldValue::Integer(2)));
        // A gap that never fills is reported, not applied.
        let mut c = SyncEngine::new(3);
        let report = c.apply_delta(vec![e2]);
        assert_eq!(report.applied, 0);
        assert_eq!(report.errors.len(), 1);
    }

    #[test]
    fn concurrent_writes_converge_and_count_one_conflict() {
        let bw = BandwidthManager::default();
        let mut a = SyncEngine::new(1);
        let mut b = SyncEngine::new(2);
        a.set_field(&id("d"), "title", "from a".into()).unwrap();
        b.set_field(&id("d"), "title", "from b".into()).unwrap();
        let report = SyncEngine::exchange(&mut a, &mut b, &bw).unwrap();
        assert_eq!(a.get(&id("d"), "title"), b.get(&id("d"), "title"));
        // Each side applies the other's write and observes the conflict once.
        assert_eq!(report.conflicts, 2);
    }
}
