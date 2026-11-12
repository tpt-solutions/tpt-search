//! One Raft node: the actor that owns its log, term, role, and state machine.
//!
//! The protocol follows the Raft paper:
//!
//! * **Elections.** A follower whose election timer expires becomes a candidate,
//!   increments its term, and asks for votes. A candidate that gets a majority becomes
//!   leader and appends a no-op entry for its term. Votes go only to candidates whose
//!   log is at least as up to date.
//! * **Replication.** The leader sends each follower the entries that follow its
//!   `next_index` and backs off on a mismatch. A follower truncates conflicting entries
//!   and appends the new ones.
//! * **Commit.** The leader advances the commit index to an entry of its own term that
//!   a majority of voters hold. Earlier entries commit with it.
//! * **Reads.** A leader answers a read only after a majority confirms it is still
//!   leader, and after the no-op of its term commits. Followers forward the read to the
//!   leader and wait until they have applied up to the returned index.
//! * **Membership.** One voter is added or removed per configuration entry. A config
//!   entry takes effect as soon as it is appended, and a new change waits until the
//!   previous one has committed.
//!
//! Nothing is persisted and the log is never compacted. Both are limits of this
//! version; see `todo.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tpt_search_core::Schema;
use tpt_search_index::Collection;

use super::rpc::{ConsensusError, Entry, Index, Msg, NodeId, Payload, Router, Rpc, RpcReply, Term};
use super::{ClusterRequest, ClusterResponse};

const HEARTBEAT: Duration = Duration::from_millis(50);
const ELECTION_MIN_MS: u64 = 150;
const ELECTION_MAX_MS: u64 = 300;
const TICK: Duration = Duration::from_millis(10);
/// Most entries sent in one AppendEntries.
const MAX_BATCH: u64 = 256;

/// A node's role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Role {
    /// Follows a leader and votes.
    #[default]
    Follower,
    /// Asking for votes in an election.
    Candidate,
    /// Replicates the log for the cluster.
    Leader,
    /// Stopped. Serves nothing.
    Stopped,
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Role::Follower => "Follower",
            Role::Candidate => "Candidate",
            Role::Leader => "Leader",
            Role::Stopped => "Stopped",
        };
        f.write_str(name)
    }
}

/// A node's externally visible state, refreshed after every step.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Published {
    /// Current role.
    pub role: Role,
    /// Current term.
    pub term: Term,
    /// The leader this node follows.
    pub leader: Option<NodeId>,
    /// Commit index.
    pub commit: Index,
    /// Index of the last log entry.
    pub last_index: Index,
    /// Voting members according to the latest configuration in the log.
    pub voters: Vec<NodeId>,
    /// On a leader: how far each peer's log is known to match.
    pub match_index: BTreeMap<NodeId, Index>,
}

/// State shared between a node's actor and the cluster handle.
pub struct Shared {
    /// The state machine. Written only by the actor.
    pub collection: RwLock<Collection>,
    /// Index of the last entry applied to `collection`.
    pub applied: AtomicU64,
    /// Latest published state.
    pub published: RwLock<Published>,
}

impl Shared {
    /// Creates shared state with an empty collection.
    pub fn new(schema: Schema) -> Result<Self, tpt_search_core::IndexError> {
        Ok(Shared {
            collection: RwLock::new(Collection::new(schema)?),
            applied: AtomicU64::new(0),
            published: RwLock::new(Published::default()),
        })
    }
}

struct Proposal {
    term: Term,
    reply: oneshot::Sender<Result<ClusterResponse, ConsensusError>>,
}

struct PendingRead {
    read_index: Index,
    /// Acks count only for requests with a sequence number at least this large.
    required_seq: u64,
    acks: HashSet<NodeId>,
    reply: oneshot::Sender<Result<Index, ConsensusError>>,
}

/// One Raft node. Driven by [`Node::run`].
pub struct Node {
    id: NodeId,
    shared: Arc<Shared>,
    router: Router,
    tx: mpsc::UnboundedSender<Msg>,
    rx: mpsc::UnboundedReceiver<Msg>,

    role: Role,
    term: Term,
    voted_for: Option<NodeId>,
    leader: Option<NodeId>,
    /// `log[i - 1]` holds the entry at index `i`.
    log: Vec<Entry>,
    commit: Index,
    applied: Index,
    /// Voters according to the last configuration entry in the log.
    config: Vec<NodeId>,
    /// Non-voting peers the leader replicates to.
    learners: BTreeSet<NodeId>,
    /// Index of the configuration entry not yet committed, if any.
    pending_config: Option<Index>,

    deadline: Instant,
    rng: u64,
    votes: HashSet<NodeId>,

    next_index: BTreeMap<NodeId, Index>,
    match_index: BTreeMap<NodeId, Index>,
    /// Peers with an AppendEntries in flight, and that request's sequence number.
    in_flight: HashMap<NodeId, u64>,
    seq: u64,
    next_heartbeat: Instant,
    /// Index of this term's no-op. The leader serves reads only once it commits.
    term_start: Index,
    proposals: BTreeMap<Index, Proposal>,
    reads: Vec<PendingRead>,
}

impl Node {
    /// Creates a node. A node with a non-empty `initial_config` starts with that
    /// configuration committed at index 1. Every founding member does the same, so
    /// their logs agree from the start. A node with an empty config joins later and
    /// learns the log from its leader.
    pub fn new(
        id: NodeId,
        initial_config: Vec<NodeId>,
        shared: Arc<Shared>,
        router: Router,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut log = Vec::new();
        let (commit, config) = if initial_config.is_empty() {
            (0, Vec::new())
        } else {
            log.push(Entry {
                term: 0,
                index: 1,
                payload: Payload::Config(initial_config.clone()),
            });
            (1, initial_config)
        };
        let mut node = Node {
            id,
            shared,
            router,
            tx,
            rx,
            role: Role::Follower,
            term: 0,
            voted_for: None,
            leader: None,
            log,
            commit,
            applied: 0,
            config,
            learners: BTreeSet::new(),
            pending_config: None,
            deadline: Instant::now(),
            rng: id.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
            votes: HashSet::new(),
            next_index: BTreeMap::new(),
            match_index: BTreeMap::new(),
            in_flight: HashMap::new(),
            seq: 0,
            next_heartbeat: Instant::now(),
            term_start: 0,
            proposals: BTreeMap::new(),
            reads: Vec::new(),
        };
        node.reset_deadline();
        node.publish();
        node
    }

    /// The sender other tasks use to reach this node.
    pub fn inbox(&self) -> mpsc::UnboundedSender<Msg> {
        self.tx.clone()
    }

    /// Runs the node until it is shut down or its inbox closes.
    pub async fn run(mut self) {
        let mut tick = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                msg = self.rx.recv() => match msg {
                    Some(Msg::Shutdown) | None => break,
                    Some(msg) => self.handle(msg),
                },
                _ = tick.tick() => self.on_tick(),
            }
            self.apply_committed();
            self.publish();
        }
        self.stop();
    }

    fn stop(&mut self) {
        self.role = Role::Stopped;
        for (_, p) in std::mem::take(&mut self.proposals) {
            let _ = p.reply.send(Err(ConsensusError::Stopped));
        }
        for r in std::mem::take(&mut self.reads) {
            let _ = r.reply.send(Err(ConsensusError::Stopped));
        }
        self.publish();
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Rpc { rpc, reply } => self.handle_rpc(rpc, reply),
            Msg::Propose { request, reply } => self.propose(request, reply),
            Msg::Track(peer) => {
                if !self.config.contains(&peer) {
                    self.learners.insert(peer);
                    if self.role == Role::Leader {
                        let next = self.last_index() + 1;
                        self.next_index.entry(peer).or_insert(next);
                        self.match_index.entry(peer).or_insert(0);
                        self.send_append(peer, false);
                    }
                }
            }
            Msg::Configure { voters, reply } => {
                let _ = reply.send(self.configure(voters));
            }
            Msg::Read { reply } => self.start_read(reply),
            Msg::VoteResult { peer, term, reply } => self.on_vote(peer, term, reply),
            Msg::AppendResult {
                peer,
                seq,
                sent_last,
                reply,
            } => self.on_append_result(peer, seq, sent_last, reply),
            Msg::Shutdown => {}
        }
    }

    // ---- RPC handling ----------------------------------------------------------------

    fn handle_rpc(&mut self, rpc: Rpc, reply: oneshot::Sender<RpcReply>) {
        match rpc {
            Rpc::RequestVote {
                term,
                candidate,
                last_index,
                last_term,
            } => {
                if term > self.term {
                    self.become_follower(term, None);
                }
                let up_to_date = last_term > self.last_term()
                    || (last_term == self.last_term() && last_index >= self.last_index());
                let granted = term == self.term
                    && (self.voted_for.is_none() || self.voted_for == Some(candidate))
                    && up_to_date;
                if granted {
                    self.voted_for = Some(candidate);
                    self.reset_deadline();
                }
                let _ = reply.send(RpcReply::Vote {
                    term: self.term,
                    granted,
                });
            }
            Rpc::AppendEntries {
                term,
                leader,
                prev_index,
                prev_term,
                entries,
                leader_commit,
            } => {
                if term < self.term {
                    let _ = reply.send(RpcReply::Append {
                        term: self.term,
                        success: false,
                        index: self.last_index(),
                    });
                    return;
                }
                if term > self.term || self.role != Role::Follower {
                    self.become_follower(term, Some(leader));
                }
                self.leader = Some(leader);
                self.reset_deadline();

                if prev_index > self.last_index() {
                    let _ = reply.send(RpcReply::Append {
                        term: self.term,
                        success: false,
                        index: self.last_index(),
                    });
                    return;
                }
                if prev_index > 0 && self.term_at(prev_index) != prev_term {
                    // The entry at prev_index conflicts. Drop it and everything after.
                    self.truncate_from(prev_index);
                    let _ = reply.send(RpcReply::Append {
                        term: self.term,
                        success: false,
                        index: prev_index - 1,
                    });
                    return;
                }

                let last_new = prev_index + entries.len() as u64;
                for entry in entries {
                    if entry.index <= self.last_index() {
                        if self.term_at(entry.index) == entry.term {
                            continue;
                        }
                        self.truncate_from(entry.index);
                    }
                    self.log.push(entry);
                }
                self.refresh_config();
                if leader_commit > self.commit {
                    self.commit = leader_commit.min(last_new);
                }
                let _ = reply.send(RpcReply::Append {
                    term: self.term,
                    success: true,
                    index: last_new,
                });
            }
        }
    }

    // ---- elections -------------------------------------------------------------------

    fn on_tick(&mut self) {
        match self.role {
            Role::Stopped => {}
            Role::Leader => {
                if Instant::now() >= self.next_heartbeat {
                    self.next_heartbeat = Instant::now() + HEARTBEAT;
                    let peers = self.peers();
                    for peer in peers {
                        self.send_append(peer, false);
                    }
                }
            }
            Role::Follower | Role::Candidate => {
                if Instant::now() >= self.deadline && self.config.contains(&self.id) {
                    self.start_election();
                }
            }
        }
    }

    fn start_election(&mut self) {
        self.role = Role::Candidate;
        self.term += 1;
        self.voted_for = Some(self.id);
        self.leader = None;
        self.votes = HashSet::from([self.id]);
        self.reset_deadline();
        if self.has_majority(&self.votes) {
            self.become_leader();
            return;
        }
        let term = self.term;
        let (last_index, last_term) = (self.last_index(), self.last_term());
        let candidate = self.id;
        for peer in self.config.iter().copied().filter(|p| *p != self.id) {
            let router = self.router.clone();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let rpc = Rpc::RequestVote {
                    term,
                    candidate,
                    last_index,
                    last_term,
                };
                let reply = router.call(peer, rpc).await.ok();
                let _ = tx.send(Msg::VoteResult { peer, term, reply });
            });
        }
    }

    fn on_vote(&mut self, peer: NodeId, term: Term, reply: Option<RpcReply>) {
        if self.role != Role::Candidate || term != self.term {
            return;
        }
        if let Some(RpcReply::Vote {
            term: their_term,
            granted,
        }) = reply
        {
            if their_term > self.term {
                self.become_follower(their_term, None);
                return;
            }
            if granted {
                self.votes.insert(peer);
                if self.has_majority(&self.votes) {
                    self.become_leader();
                }
            }
        }
    }

    fn become_follower(&mut self, term: Term, leader: Option<NodeId>) {
        if term > self.term {
            self.term = term;
            self.voted_for = None;
        }
        let was_leader = self.role == Role::Leader;
        self.role = Role::Follower;
        self.leader = leader;
        self.in_flight.clear();
        self.reset_deadline();
        if was_leader || !self.proposals.is_empty() || !self.reads.is_empty() {
            for (_, p) in std::mem::take(&mut self.proposals) {
                let _ = p.reply.send(Err(ConsensusError::NotLeader(leader)));
            }
            for r in std::mem::take(&mut self.reads) {
                let _ = r.reply.send(Err(ConsensusError::NotLeader(leader)));
            }
        }
    }

    fn become_leader(&mut self) {
        self.role = Role::Leader;
        self.leader = Some(self.id);
        self.in_flight.clear();
        let next = self.last_index() + 1;
        self.next_index = self.peers().into_iter().map(|p| (p, next)).collect();
        self.match_index = self.peers().into_iter().map(|p| (p, 0)).collect();
        // The no-op lets this term commit entries from earlier terms.
        self.append_local(Payload::Noop);
        self.term_start = self.last_index();
        self.next_heartbeat = Instant::now();
        self.advance_commit();
        let peers = self.peers();
        for peer in peers {
            self.send_append(peer, true);
        }
    }

    // ---- replication -----------------------------------------------------------------

    /// Every peer the leader replicates to: voters and learners, except itself.
    fn peers(&self) -> Vec<NodeId> {
        self.config
            .iter()
            .copied()
            .chain(self.learners.iter().copied())
            .filter(|p| *p != self.id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn send_append(&mut self, peer: NodeId, force: bool) {
        if !force && self.in_flight.contains_key(&peer) {
            return;
        }
        let fallback = self.last_index() + 1;
        let next = *self.next_index.entry(peer).or_insert(fallback);
        let prev_index = next - 1;
        let prev_term = self.term_at(prev_index);
        let entries: Vec<Entry> = self
            .log
            .iter()
            .skip(prev_index as usize)
            .take(MAX_BATCH as usize)
            .cloned()
            .collect();
        let sent_last = prev_index + entries.len() as u64;
        self.seq += 1;
        let seq = self.seq;
        self.in_flight.insert(peer, seq);
        let rpc = Rpc::AppendEntries {
            term: self.term,
            leader: self.id,
            prev_index,
            prev_term,
            entries,
            leader_commit: self.commit,
        };
        let router = self.router.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let reply = router.call(peer, rpc).await.ok();
            let _ = tx.send(Msg::AppendResult {
                peer,
                seq,
                sent_last,
                reply,
            });
        });
    }

    fn on_append_result(
        &mut self,
        peer: NodeId,
        seq: u64,
        sent_last: Index,
        reply: Option<RpcReply>,
    ) {
        if self.in_flight.get(&peer) == Some(&seq) {
            self.in_flight.remove(&peer);
        }
        let Some(RpcReply::Append {
            term,
            success,
            index,
        }) = reply
        else {
            return;
        };
        if term > self.term {
            self.become_follower(term, None);
            return;
        }
        if self.role != Role::Leader || term != self.term {
            return;
        }
        // Any reply from a follower of this term counts toward read quorums.
        for read in &mut self.reads {
            if seq >= read.required_seq {
                read.acks.insert(peer);
            }
        }
        if success {
            let matched = self.match_index.entry(peer).or_insert(0);
            *matched = (*matched).max(sent_last);
            let matched = *matched;
            self.next_index.insert(peer, matched + 1);
            self.advance_commit();
            self.check_reads();
            if matched < self.last_index() {
                self.send_append(peer, false);
            }
        } else {
            let next = self.next_index.get(&peer).copied().unwrap_or(1);
            let retry = next.saturating_sub(1).min(index + 1).max(1);
            self.next_index.insert(peer, retry);
            self.send_append(peer, false);
        }
    }

    fn advance_commit(&mut self) {
        if self.role != Role::Leader || self.config.is_empty() {
            return;
        }
        let mut matches: Vec<Index> = self
            .config
            .iter()
            .map(|v| {
                if *v == self.id {
                    self.last_index()
                } else {
                    self.match_index.get(v).copied().unwrap_or(0)
                }
            })
            .collect();
        matches.sort_unstable_by(|a, b| b.cmp(a));
        let majority = self.config.len() / 2 + 1;
        let candidate = matches[majority - 1];
        if candidate > self.commit && self.term_at(candidate) == self.term {
            self.commit = candidate;
            if self.pending_config.is_some_and(|i| i <= self.commit) {
                self.pending_config = None;
            }
        }
        self.check_reads();
    }

    // ---- log -------------------------------------------------------------------------

    fn append_local(&mut self, payload: Payload) -> Index {
        let index = self.last_index() + 1;
        let is_config = matches!(payload, Payload::Config(_));
        self.log.push(Entry {
            term: self.term,
            index,
            payload,
        });
        if is_config {
            self.refresh_config();
            self.pending_config = Some(index);
        }
        index
    }

    fn truncate_from(&mut self, index: Index) {
        self.log.truncate(index.saturating_sub(1) as usize);
        self.refresh_config();
    }

    fn refresh_config(&mut self) {
        self.config = self
            .log
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                Payload::Config(voters) => Some(voters.clone()),
                _ => None,
            })
            .unwrap_or_default();
    }

    fn last_index(&self) -> Index {
        self.log.len() as Index
    }

    fn last_term(&self) -> Term {
        self.log.last().map_or(0, |e| e.term)
    }

    fn term_at(&self, index: Index) -> Term {
        if index == 0 {
            return 0;
        }
        self.log.get(index as usize - 1).map_or(0, |e| e.term)
    }

    fn has_majority(&self, set: &HashSet<NodeId>) -> bool {
        let count = self.config.iter().filter(|v| set.contains(v)).count();
        !self.config.is_empty() && count > self.config.len() / 2
    }

    /// Applies committed entries to the state machine and answers their proposals.
    fn apply_committed(&mut self) {
        while self.applied < self.commit {
            self.applied += 1;
            let entry = self.log[self.applied as usize - 1].clone();
            let response = match &entry.payload {
                Payload::Normal(request) => {
                    let mut collection = match self.shared.collection.write() {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    apply_request(&mut collection, request)
                }
                _ => ClusterResponse::default(),
            };
            self.shared.applied.store(self.applied, Ordering::SeqCst);
            if let Some(p) = self.proposals.remove(&entry.index) {
                let _ = p.reply.send(if p.term == entry.term {
                    Ok(response)
                } else {
                    Err(ConsensusError::Rejected(
                        "superseded by another leader".into(),
                    ))
                });
            }
        }
    }

    // ---- client requests -------------------------------------------------------------

    fn propose(
        &mut self,
        request: ClusterRequest,
        reply: oneshot::Sender<Result<ClusterResponse, ConsensusError>>,
    ) {
        if self.role != Role::Leader {
            let _ = reply.send(Err(ConsensusError::NotLeader(self.leader)));
            return;
        }
        let index = self.append_local(Payload::Normal(request));
        self.proposals.insert(
            index,
            Proposal {
                term: self.term,
                reply,
            },
        );
        let peers = self.peers();
        for peer in peers {
            self.send_append(peer, false);
        }
        self.advance_commit();
    }

    fn configure(&mut self, voters: Vec<NodeId>) -> Result<Index, ConsensusError> {
        if self.role != Role::Leader {
            return Err(ConsensusError::NotLeader(self.leader));
        }
        if voters.is_empty() {
            return Err(ConsensusError::Rejected(
                "a configuration needs at least one voter".into(),
            ));
        }
        if self.pending_config.is_some() {
            return Err(ConsensusError::Rejected(
                "another membership change is in progress".into(),
            ));
        }
        let old: BTreeSet<NodeId> = self.config.iter().copied().collect();
        let new: BTreeSet<NodeId> = voters.iter().copied().collect();
        let changed = old.symmetric_difference(&new).count();
        if changed > 1 {
            return Err(ConsensusError::Rejected(
                "change one member at a time".into(),
            ));
        }
        let index = self.append_local(Payload::Config(new.into_iter().collect()));
        self.learners.retain(|l| !self.config.contains(l));
        let peers = self.peers();
        for peer in peers {
            self.next_index.entry(peer).or_insert(index);
            self.match_index.entry(peer).or_insert(0);
            self.send_append(peer, false);
        }
        self.advance_commit();
        Ok(index)
    }

    fn start_read(&mut self, reply: oneshot::Sender<Result<Index, ConsensusError>>) {
        if self.role != Role::Leader {
            let _ = reply.send(Err(ConsensusError::NotLeader(self.leader)));
            return;
        }
        let required_seq = self.seq + 1;
        self.reads.push(PendingRead {
            read_index: self.commit,
            required_seq,
            acks: HashSet::from([self.id]),
            reply,
        });
        // Force a fresh round so the acks come from requests sent after this read started.
        let peers = self.peers();
        for peer in peers {
            self.send_append(peer, true);
        }
        self.check_reads();
    }

    fn check_reads(&mut self) {
        if self.role != Role::Leader || self.commit < self.term_start {
            return;
        }
        let mut still_waiting = Vec::new();
        for read in std::mem::take(&mut self.reads) {
            if self.has_majority(&read.acks) {
                let _ = read.reply.send(Ok(read.read_index));
            } else {
                still_waiting.push(read);
            }
        }
        self.reads = still_waiting;
    }

    // ---- publishing ------------------------------------------------------------------

    fn reset_deadline(&mut self) {
        // xorshift64: deterministic per node, so elections are reproducible.
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        let span = ELECTION_MAX_MS - ELECTION_MIN_MS;
        let ms = ELECTION_MIN_MS + x % span;
        self.deadline = Instant::now() + Duration::from_millis(ms);
    }

    fn publish(&self) {
        if let Ok(mut published) = self.shared.published.write() {
            *published = Published {
                role: self.role,
                term: self.term,
                leader: self.leader,
                commit: self.commit,
                last_index: self.last_index(),
                voters: self.config.clone(),
                match_index: if self.role == Role::Leader {
                    self.match_index.clone()
                } else {
                    BTreeMap::new()
                },
            };
        }
    }
}

fn apply_request(collection: &mut Collection, request: &ClusterRequest) -> ClusterResponse {
    match request {
        ClusterRequest::Upsert(doc) => match collection.upsert(doc) {
            Ok(_) => ClusterResponse::default(),
            Err(e) => ClusterResponse {
                existed: false,
                error: Some(e.to_string()),
            },
        },
        ClusterRequest::Delete(id) => ClusterResponse {
            existed: collection.delete(id),
            error: None,
        },
    }
}
