//! Replicated search state on Raft.
//!
//! Every node runs a Raft actor whose state machine is a [`tpt_search_index::Collection`].
//! Writes are committed through the Raft log, so every replica applies the same
//! commands in the same order. Linearizable reads use Raft's ReadIndex: a read is
//! answered only after the leader confirms it is still leader, and the reading node has
//! applied up to the confirmed index.
//!
//! This is an in-house implementation. It was written because the two Rust Raft
//! libraries checked against `deny.toml` either pull in Apache-2.0-only crates (openraft)
//! or did not behave correctly in testing (async-raft 0.6.1 dropped a committed follower
//! entry). See `todo.md` for the limits: nothing is persisted, the log is never compacted,
//! and the transport is in-process.

mod cluster;
mod node;
mod rpc;

use std::fmt;

use serde::{Deserialize, Serialize};
use tpt_search_core::{Document, DocumentId};

pub use cluster::{Cluster, ClusterStatus, NodeStatus};
pub use rpc::ConsensusError;

/// A command replicated through the Raft log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClusterRequest {
    /// Insert or replace a document.
    Upsert(Document),
    /// Delete a document by id.
    Delete(DocumentId),
}

impl fmt::Display for ClusterRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClusterRequest::Upsert(doc) => write!(f, "Upsert({})", doc.id),
            ClusterRequest::Delete(id) => write!(f, "Delete({id})"),
        }
    }
}

/// The result of applying a [`ClusterRequest`] to the state machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterResponse {
    /// For a delete: whether a live document was removed. Always `false` for upserts.
    pub existed: bool,
    /// The schema rejected the command. The rejection is deterministic, so every replica
    /// records the same error and stays consistent.
    pub error: Option<String>,
}
