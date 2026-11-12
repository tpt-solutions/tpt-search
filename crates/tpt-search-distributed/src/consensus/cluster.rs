//! Cluster management: start nodes, write and read through the leader, add and
//! remove members, stop nodes, and report status.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::Instant;
use tpt_search_core::{DistributedError, Document, DocumentId, Schema};
use tpt_search_index::{Collection, ScoredDocument, TermQuery};

use super::node::{Node, Role, Shared};
use super::rpc::{ConsensusError, Index, Msg, NodeId, Router};
use super::ClusterRequest;
use super::ClusterResponse;

/// How long one attempt to reach a node may take.
const ATTEMPT: Duration = Duration::from_secs(3);
/// How long a follower may take to apply up to a read index.
const APPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a new member may take to catch up before it is added as a voter.
const CATCH_UP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a write or read keeps retrying while a leader is elected or a hint is followed.
const RETRY_BUDGET: Duration = Duration::from_secs(10);

struct Member {
    shared: Arc<Shared>,
    inbox: tokio::sync::mpsc::UnboundedSender<Msg>,
}

/// Status of one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatus {
    /// Node id.
    pub id: NodeId,
    /// Whether the node is reachable.
    pub online: bool,
    /// Raft role (`Leader`, `Follower`, `Candidate`), or `Offline`.
    pub state: String,
    /// The leader this node currently follows, if any.
    pub leader: Option<NodeId>,
    /// Index of the last log entry applied to the state machine.
    pub last_applied: u64,
    /// Live documents in this node's collection.
    pub documents: usize,
}

/// Status of the whole cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterStatus {
    /// Per-node status, by id.
    pub nodes: Vec<NodeStatus>,
    /// The leader, if one is reachable.
    pub leader: Option<NodeId>,
    /// Whether a majority of members are reachable.
    pub has_quorum: bool,
}

/// A running search cluster in one process.
pub struct Cluster {
    schema: Schema,
    router: Router,
    members: BTreeMap<NodeId, Member>,
}

fn distributed(e: ConsensusError) -> DistributedError {
    match e {
        ConsensusError::Unreachable(n) => DistributedError::NodeUnavailable(n.to_string()),
        other => DistributedError::Consensus(other.to_string()),
    }
}

impl Cluster {
    /// Starts the founding members. Each node begins with the same configuration, so
    /// the nodes form a cluster as soon as they can elect a leader.
    pub async fn start(
        schema: Schema,
        ids: impl IntoIterator<Item = NodeId>,
    ) -> Result<Self, DistributedError> {
        let ids: Vec<NodeId> = ids.into_iter().collect();
        let mut cluster = Cluster {
            schema,
            router: Router::new(),
            members: BTreeMap::new(),
        };
        for &id in &ids {
            cluster.spawn(id, ids.clone())?;
        }
        Ok(cluster)
    }

    fn spawn(&mut self, id: NodeId, initial_config: Vec<NodeId>) -> Result<(), DistributedError> {
        let shared = Arc::new(
            Shared::new(self.schema.clone())
                .map_err(|e| DistributedError::InvalidConfig(e.to_string()))?,
        );
        let node = Node::new(id, initial_config, Arc::clone(&shared), self.router.clone());
        let inbox = node.inbox();
        self.router.register(id, inbox.clone());
        tokio::spawn(node.run());
        self.members.insert(id, Member { shared, inbox });
        Ok(())
    }

    /// Confirms the founding members have formed a cluster with a leader.
    ///
    /// The configuration is in place from [`start`](Self::start), so this waits for a
    /// leader rather than sending any setup.
    pub async fn initialize(&self) -> Result<(), DistributedError> {
        if self.members.is_empty() {
            return Err(DistributedError::InvalidConfig(
                "cluster has no nodes".into(),
            ));
        }
        self.wait_for_leader(CATCH_UP_TIMEOUT).await.map(|_| ())
    }

    /// Ids of reachable nodes.
    pub fn online(&self) -> Vec<NodeId> {
        self.router.online()
    }

    /// The leader, if a reachable node is the leader. With several leaders (possible
    /// briefly across terms), the one with the highest term wins.
    pub fn leader(&self) -> Option<NodeId> {
        self.members
            .iter()
            .filter(|(id, _)| self.router.inbox(**id).is_some())
            .filter_map(|(id, m)| {
                let p = m.shared.published.read().ok()?;
                (p.role == Role::Leader).then_some((p.term, *id))
            })
            .max()
            .map(|(_, id)| id)
    }

    /// Waits until a leader is elected.
    pub async fn wait_for_leader(&self, timeout: Duration) -> Result<NodeId, DistributedError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(leader) = self.leader() {
                return Ok(leader);
            }
            if Instant::now() >= deadline {
                return Err(DistributedError::Consensus(
                    "no leader elected in time".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Writes a document through the leader. Returns once it is committed and applied.
    pub async fn upsert(&self, doc: Document) -> Result<(), DistributedError> {
        let response = self.write(ClusterRequest::Upsert(doc)).await?;
        match response.error {
            Some(e) => Err(DistributedError::Consensus(format!("rejected: {e}"))),
            None => Ok(()),
        }
    }

    /// Deletes a document through the leader. Returns whether it existed.
    pub async fn delete(&self, id: DocumentId) -> Result<bool, DistributedError> {
        Ok(self.write(ClusterRequest::Delete(id)).await?.existed)
    }

    async fn write(&self, request: ClusterRequest) -> Result<ClusterResponse, DistributedError> {
        let mut target = self.leader();
        let deadline = Instant::now() + RETRY_BUDGET;
        while Instant::now() < deadline {
            let Some(node) = target.or_else(|| self.online().first().copied()) else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                target = self.leader();
                continue;
            };
            let Some(inbox) = self.router.inbox(node) else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                target = self.leader();
                continue;
            };
            let (reply, rx) = oneshot::channel();
            if inbox
                .send(Msg::Propose {
                    request: request.clone(),
                    reply,
                })
                .is_err()
            {
                target = self.leader();
                continue;
            }
            match tokio::time::timeout(ATTEMPT, rx).await {
                Ok(Ok(Ok(response))) => return Ok(response),
                Ok(Ok(Err(ConsensusError::NotLeader(Some(l))))) => target = Some(l),
                Ok(Ok(Err(ConsensusError::NotLeader(None)))) | Ok(Err(_)) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    target = self.leader();
                }
                Ok(Ok(Err(e))) => return Err(distributed(e)),
                Err(_) => {
                    return Err(DistributedError::Consensus(
                        "write did not commit in time".into(),
                    ))
                }
            }
        }
        Err(DistributedError::Consensus(
            "no leader accepted the write".into(),
        ))
    }

    /// The commit index a linearizable read must reflect. A follower asks the leader.
    async fn read_index(&self, node: NodeId) -> Result<Index, DistributedError> {
        let mut target = node;
        let deadline = Instant::now() + RETRY_BUDGET;
        while Instant::now() < deadline {
            // A hint can name a node that has just stopped. Retry from the node that was asked,
            // or from the current leader, rather than failing.
            let sent = match self.router.inbox(target) {
                Some(inbox) => {
                    let (reply, rx) = oneshot::channel();
                    inbox.send(Msg::Read { reply }).ok().map(|_| rx)
                }
                None => None,
            };
            let Some(rx) = sent else {
                tokio::time::sleep(Duration::from_millis(20)).await;
                target = self.leader().unwrap_or(node);
                continue;
            };
            match tokio::time::timeout(ATTEMPT, rx).await {
                Ok(Ok(Ok(index))) => return Ok(index),
                Ok(Ok(Err(ConsensusError::NotLeader(Some(l))))) => target = l,
                Ok(Ok(Err(ConsensusError::NotLeader(None)))) | Ok(Err(_)) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    target = self.leader().unwrap_or(node);
                }
                Ok(Ok(Err(e))) => return Err(distributed(e)),
                Err(_) => {
                    return Err(DistributedError::Consensus(
                        "read index not confirmed in time".into(),
                    ))
                }
            }
        }
        Err(DistributedError::Consensus(
            "no leader to confirm a read".into(),
        ))
    }

    /// Runs `f` against a node's collection after a linearizable read: the node first
    /// obtains a confirmed read index, then waits until it has applied up to it.
    pub async fn read<T>(
        &self,
        node: NodeId,
        f: impl FnOnce(&Collection) -> T,
    ) -> Result<T, DistributedError> {
        let member = self
            .members
            .get(&node)
            .filter(|_| self.router.inbox(node).is_some())
            .ok_or_else(|| DistributedError::NodeUnavailable(node.to_string()))?;
        let index = self.read_index(node).await?;
        let deadline = Instant::now() + APPLY_TIMEOUT;
        while member
            .shared
            .applied
            .load(std::sync::atomic::Ordering::SeqCst)
            < index
        {
            if Instant::now() >= deadline {
                return Err(DistributedError::Consensus(format!(
                    "node {node} did not apply read index {index} in time"
                )));
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let collection = member
            .shared
            .collection
            .read()
            .map_err(|_| DistributedError::Consensus("state machine lock poisoned".into()))?;
        Ok(f(&collection))
    }

    /// Fetches a document from a node after a linearizable read.
    pub async fn get(
        &self,
        node: NodeId,
        id: &DocumentId,
    ) -> Result<Option<Document>, DistributedError> {
        self.read(node, |c| c.get(id)).await
    }

    /// Runs an exact-term search on a node after a linearizable read.
    pub async fn search(
        &self,
        node: NodeId,
        query: &TermQuery,
    ) -> Result<Vec<ScoredDocument>, DistributedError> {
        let query = query.clone();
        self.read(node, move |c| c.search(&query).unwrap_or_default())
            .await
    }

    /// Stops a node: it becomes unreachable to its peers and its task ends.
    pub async fn stop(&self, node: NodeId) -> Result<(), DistributedError> {
        let member = self
            .members
            .get(&node)
            .ok_or_else(|| DistributedError::NodeUnavailable(node.to_string()))?;
        self.router.unregister(node);
        let _ = member.inbox.send(Msg::Shutdown);
        Ok(())
    }

    /// Adds a node to a running cluster. It starts empty, replicates the log as a
    /// learner, and becomes a voter once it has caught up.
    pub async fn add_node(&mut self, id: NodeId) -> Result<(), DistributedError> {
        let leader = self
            .leader()
            .ok_or_else(|| DistributedError::Consensus("no leader".into()))?;
        self.spawn(id, Vec::new())?;
        self.send_to(leader, Msg::Track(id))?;

        // Wait for the new node to hold everything the leader has.
        let target_index = self.last_index_of(leader)?;
        let deadline = Instant::now() + CATCH_UP_TIMEOUT;
        loop {
            let matched = self.match_of(leader, id);
            if matched >= target_index {
                break;
            }
            if Instant::now() >= deadline {
                return Err(DistributedError::Consensus(format!(
                    "node {id} did not catch up in time"
                )));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let mut voters = self.voters_of(leader);
        voters.push(id);
        self.configure(leader, voters).await
    }

    /// Removes a node from the voting membership and stops it.
    pub async fn remove_node(&mut self, id: NodeId) -> Result<(), DistributedError> {
        let leader = self
            .leader()
            .ok_or_else(|| DistributedError::Consensus("no leader".into()))?;
        let voters: Vec<NodeId> = self
            .voters_of(leader)
            .into_iter()
            .filter(|v| *v != id)
            .collect();
        self.configure(leader, voters).await?;
        self.stop(id).await?;
        self.members.remove(&id);
        Ok(())
    }

    /// Sends a membership change to the leader and waits until it commits.
    async fn configure(&self, leader: NodeId, voters: Vec<NodeId>) -> Result<(), DistributedError> {
        let (reply, rx) = oneshot::channel();
        self.send_to(leader, Msg::Configure { voters, reply })?;
        let index = match tokio::time::timeout(ATTEMPT, rx).await {
            Ok(Ok(result)) => result.map_err(distributed)?,
            _ => {
                return Err(DistributedError::Consensus(
                    "membership change not accepted".into(),
                ))
            }
        };
        let deadline = Instant::now() + CATCH_UP_TIMEOUT;
        while self.commit_of(leader) < index {
            if Instant::now() >= deadline {
                return Err(DistributedError::Consensus(
                    "membership change did not commit in time".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }

    fn send_to(&self, node: NodeId, msg: Msg) -> Result<(), DistributedError> {
        self.router
            .inbox(node)
            .ok_or_else(|| DistributedError::NodeUnavailable(node.to_string()))?
            .send(msg)
            .map_err(|_| DistributedError::NodeUnavailable(node.to_string()))
    }

    fn published(&self, node: NodeId) -> Option<super::node::Published> {
        self.members
            .get(&node)?
            .shared
            .published
            .read()
            .ok()
            .map(|p| p.clone())
    }

    fn last_index_of(&self, node: NodeId) -> Result<Index, DistributedError> {
        Ok(self.published(node).map_or(0, |p| p.last_index))
    }

    fn commit_of(&self, node: NodeId) -> Index {
        self.published(node).map_or(0, |p| p.commit)
    }

    fn match_of(&self, leader: NodeId, peer: NodeId) -> Index {
        self.published(leader)
            .and_then(|p| p.match_index.get(&peer).copied())
            .unwrap_or(0)
    }

    fn voters_of(&self, node: NodeId) -> Vec<NodeId> {
        self.published(node).map_or_else(Vec::new, |p| p.voters)
    }

    /// Per-node and cluster-level status. Unreachable nodes are reported as offline.
    pub fn status(&self) -> ClusterStatus {
        let online: Vec<NodeId> = self.online();
        let nodes: Vec<NodeStatus> = self
            .members
            .iter()
            .map(|(&id, member)| {
                let is_online = online.contains(&id);
                let published = member
                    .shared
                    .published
                    .read()
                    .map(|p| p.clone())
                    .unwrap_or_default();
                let (state, leader) = if is_online {
                    (published.role.to_string(), published.leader)
                } else {
                    ("Offline".into(), None)
                };
                let documents = member
                    .shared
                    .collection
                    .read()
                    .map(|c| c.len())
                    .unwrap_or(0);
                NodeStatus {
                    id,
                    online: is_online,
                    state,
                    leader,
                    last_applied: member
                        .shared
                        .applied
                        .load(std::sync::atomic::Ordering::SeqCst),
                    documents,
                }
            })
            .collect();
        let voters = online
            .first()
            .and_then(|n| self.published(*n))
            .map_or(self.members.len(), |p| p.voters.len().max(1));
        let reachable_voters = self
            .published(online.first().copied().unwrap_or(0))
            .map_or(0, |p| {
                p.voters.iter().filter(|v| online.contains(v)).count()
            });
        ClusterStatus {
            leader: self.leader(),
            has_quorum: reachable_voters * 2 > voters,
            nodes,
        }
    }
}
