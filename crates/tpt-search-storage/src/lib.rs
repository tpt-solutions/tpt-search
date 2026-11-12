//! Persistence for `tpt-search`: a checksummed, fsynced write-ahead log;
//! segment files with memory-mapped reads; a segment manager with merge
//! policies; and snapshot/restore with checksum manifests.
//!
//! Only this crate uses `unsafe` (for `memmap2`). The crate-level allow below
//! is the carve-out described in `todo.md`, deviation #2.

#![allow(unsafe_code)]
#![doc(html_root_url = "https://docs.rs/tpt-search-storage")]

pub mod mapped;
pub mod segment;
pub mod snapshot;
pub mod wal;

pub use mapped::{MappedSegment, SegmentMetadata};
pub use segment::{MergePolicy, Segment, SegmentId, SegmentManager};
pub use snapshot::{create_snapshot, restore_snapshot, verify_snapshot, Manifest};
pub use wal::{WalEntry, WriteAheadLog};
