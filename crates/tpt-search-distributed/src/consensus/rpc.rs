//! Raft messages, the actor inbox, and the in-process router that delivers RPCs.
//!
//! Every node runs one actor task. Peers exchange RPCs through the [`Router`], which
//! hands each request to the target actor's inbox and returns the reply. A call to a
//! node that is stopped or was never started fails after a short delay, as a real
//! connection timeout would. The delay keeps retry loops from spinning without
//! yielding.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::{ClusterRequest, ClusterResponse};

/// Node identifier.
pub type NodeId = u64;
/// Raft term.
pub type Term = u64;
/// Log index. Index 0 is a sentinel; real entries start at 1.
pub type Index = u64;

/// How long a call to an unreachable node takes to fail.
const UNREACHABLE_DELAY: Duration = Duration::from_millis(10);

/// What a log entry carries.
#[derive(Clone, Debug, PartialEq)]
pub enum Payload {
    /// Leader's marker at the start of its term. Committing it commits earlier entries.
    Noop,
    /// A voting membership configuration.
    Config(Vec<NodeId>),
    /// A client command for the state machine.
    Normal(ClusterRequest),
}

/// One entry of the replicated log.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Term in which the leader created the entry.
    pub term: Term,
    /// Position in the log.
    pub index: Index,
    /// Content.
    pub payload: Payload,
}

/// A request from one node to another.
#[derive(Clone, Debug)]
pub enum Rpc {
    /// Ask for a vote.
    RequestVote {
        /// Candidate's term.
        term: Term,
        /// Candidate.
        candidate: NodeId,
        /// Index of the candidate's last log entry.
        last_index: Index,
        /// Term of the candidate's last log entry.
        last_term: Term,
    },
    /// Replicate entries, or act as a heartbeat when `entries` is empty.
    AppendEntries {
        /// Leader's term.
        term: Term,
        /// Leader.
        leader: NodeId,
        /// Index immediately before the new entries.
        prev_index: Index,
        /// Term of the entry at `prev_index`.
        prev_term: Term,
        /// Entries to store, in index order.
        entries: Vec<Entry>,
        /// The leader's commit index.
        leader_commit: Index,
    },
}

/// The reply to an [`Rpc`].
#[derive(Clone, Debug, PartialEq)]
pub enum RpcReply {
    /// Reply to `RequestVote`.
    Vote {
        /// Responder's term.
        term: Term,
        /// Whether the vote was granted.
        granted: bool,
    },
    /// Reply to `AppendEntries`.
    Append {
        /// Responder's term.
        term: Term,
        /// Whether the entries matched and were stored.
        success: bool,
        /// Last index the responder now holds that matches the leader (on success),
        /// or a hint for where to retry (on failure).
        index: Index,
    },
}

/// Errors from the consensus layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsensusError {
    /// The node is not the leader. The hint names the leader it knows, if any.
    NotLeader(Option<NodeId>),
    /// The node could not be reached.
    Unreachable(NodeId),
    /// The node has stopped.
    Stopped,
    /// A required condition did not hold in time.
    Timeout(&'static str),
    /// The request was not accepted for the stated reason.
    Rejected(String),
}

impl fmt::Display for ConsensusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConsensusError::NotLeader(Some(l)) => write!(f, "not the leader; leader is node {l}"),
            ConsensusError::NotLeader(None) => write!(f, "not the leader and no leader is known"),
            ConsensusError::Unreachable(n) => write!(f, "node {n} is unreachable"),
            ConsensusError::Stopped => write!(f, "node has stopped"),
            ConsensusError::Timeout(what) => write!(f, "timed out waiting for {what}"),
            ConsensusError::Rejected(why) => write!(f, "rejected: {why}"),
        }
    }
}

/// Everything a node actor can be asked to do, plus its own internal results.
pub enum Msg {
    /// An RPC from a peer.
    Rpc {
        /// The request.
        rpc: Rpc,
        /// Where the reply goes.
        reply: oneshot::Sender<RpcReply>,
    },
    /// A client command, to be committed through the log.
    Propose {
        /// The command.
        request: ClusterRequest,
        /// Receives the state machine's response once applied.
        reply: oneshot::Sender<Result<ClusterResponse, ConsensusError>>,
    },
    /// Start replicating to a node that is not yet a voter (a learner).
    Track(NodeId),
    /// Append a membership configuration. Replies with the index of the entry, which
    /// takes effect when appended and is committed later.
    Configure {
        /// The new voting members.
        voters: Vec<NodeId>,
        /// Receives the entry index, or the reason the change cannot start.
        reply: oneshot::Sender<Result<Index, ConsensusError>>,
    },
    /// A linearizable read point. Replies with the commit index to read at, once this
    /// node has confirmed it is still the leader.
    Read {
        /// Receives the read index, or `NotLeader`.
        reply: oneshot::Sender<Result<Index, ConsensusError>>,
    },
    /// Stop the node.
    Shutdown,
    /// Internal: a `RequestVote` reply from `peer`.
    VoteResult {
        /// Voter.
        peer: NodeId,
        /// The election term this vote belongs to.
        term: Term,
        /// The reply, or `None` if the call failed.
        reply: Option<RpcReply>,
    },
    /// Internal: an `AppendEntries` reply from `peer`.
    AppendResult {
        /// Follower.
        peer: NodeId,
        /// Sequence number of the request this answers.
        seq: u64,
        /// Last index the request carried.
        sent_last: Index,
        /// The reply, or `None` if the call failed.
        reply: Option<RpcReply>,
    },
}

/// Delivers RPCs between node actors. Cloning shares the same registry.
#[derive(Clone, Default)]
pub struct Router {
    nodes: Arc<RwLock<BTreeMap<NodeId, mpsc::UnboundedSender<Msg>>>>,
}

impl Router {
    /// Creates an empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a node's inbox.
    pub fn register(&self, id: NodeId, inbox: mpsc::UnboundedSender<Msg>) {
        if let Ok(mut nodes) = self.nodes.write() {
            nodes.insert(id, inbox);
        }
    }

    /// Makes a node unreachable. Returns its inbox if it was registered.
    pub fn unregister(&self, id: NodeId) -> Option<mpsc::UnboundedSender<Msg>> {
        self.nodes.write().ok()?.remove(&id)
    }

    /// The inbox of a reachable node.
    pub fn inbox(&self, id: NodeId) -> Option<mpsc::UnboundedSender<Msg>> {
        self.nodes.read().ok()?.get(&id).cloned()
    }

    /// Ids of reachable nodes, in order.
    pub fn online(&self) -> Vec<NodeId> {
        self.nodes
            .read()
            .map(|n| n.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Sends an RPC and waits for the reply. Fails if the target is unreachable.
    pub async fn call(&self, target: NodeId, rpc: Rpc) -> Result<RpcReply, ConsensusError> {
        let Some(inbox) = self.inbox(target) else {
            tokio::time::sleep(UNREACHABLE_DELAY).await;
            return Err(ConsensusError::Unreachable(target));
        };
        let (reply, rx) = oneshot::channel();
        if inbox.send(Msg::Rpc { rpc, reply }).is_err() {
            tokio::time::sleep(UNREACHABLE_DELAY).await;
            return Err(ConsensusError::Unreachable(target));
        }
        rx.await.map_err(|_| ConsensusError::Unreachable(target))
    }
}
