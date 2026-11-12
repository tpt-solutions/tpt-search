//! Edge-to-cloud sync for `tpt-search`: CRDT documents that converge without
//! coordination, a delta-based sync engine with an offline queue that survives restarts,
//! bandwidth-aware planning and compression, and sync monitoring.
//!
//! # Example
//!
//! ```
//! use tpt_search_core::DocumentId;
//! use tpt_search_sync::{BandwidthManager, SyncEngine};
//!
//! let bandwidth = BandwidthManager::default();
//! let mut edge = SyncEngine::new(1);
//! let mut cloud = SyncEngine::new(0);
//!
//! // Edited while offline: queued locally.
//! edge.set_field(&DocumentId::from("note-1"), "title", "draft".into())?;
//! assert_eq!(edge.pending().len(), 1);
//!
//! // Back online: upload the queue, then the queue is acknowledged.
//! let report = edge.upload(&mut cloud, &bandwidth, usize::MAX)?;
//! assert_eq!(report.applied, 1);
//! assert!(edge.pending().is_empty());
//! # Ok::<(), tpt_search_sync::SyncError>(())
//! ```

#![doc(html_root_url = "https://docs.rs/tpt-search-sync")]

pub mod bandwidth;
pub mod crdt;
pub mod monitor;
pub mod sync;

pub use bandwidth::{BandwidthManager, Batch, LinkState, SyncPriority};
pub use crdt::{CrdtDocument, GCounter, LwwDocument, LwwField, OrSet, PnCounter, ReplicaId, Stamp};
pub use monitor::{SyncMonitor, SyncReport};
pub use sync::{Change, SyncEngine, SyncEntry, SyncError, VersionVector};
