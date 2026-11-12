//! Sharded, replicated search: one Raft group per shard, placed by [`ShardManager`].
//!
//! Writes and point reads go to the shard that owns the document key. A search goes to
//! every shard, reads each shard linearizably, and merges the hits by score. Each shard
//! is an independent Raft group, so a failure in one shard leaves the others serving.

use tpt_search_core::{DistributedError, Document, DocumentId, Schema};
use tpt_search_index::{ScoredDocument, TermQuery};

use crate::consensus::Cluster;
use crate::sharding::{NodeId, ShardId, ShardManager, ShardingStrategy};

/// A set of shard groups and the placement that maps keys onto them.
pub struct ShardedCluster {
    manager: ShardManager,
    shards: Vec<Cluster>,
}

impl ShardedCluster {
    /// Starts one Raft group per shard, on the replicas the placement assigns.
    pub async fn start(
        schema: Schema,
        strategy: ShardingStrategy,
        replication_factor: usize,
        nodes: impl IntoIterator<Item = NodeId>,
    ) -> Result<Self, DistributedError> {
        let manager = ShardManager::new(strategy, replication_factor, nodes)?;
        let mut shards = Vec::with_capacity(manager.shards().len());
        for info in manager.shards() {
            let cluster = Cluster::start(schema.clone(), info.replicas.clone()).await?;
            cluster.initialize().await?;
            shards.push(cluster);
        }
        Ok(ShardedCluster { manager, shards })
    }

    /// The placement.
    pub fn manager(&self) -> &ShardManager {
        &self.manager
    }

    /// The Raft group for a shard.
    pub fn shard(&self, id: ShardId) -> Option<&Cluster> {
        self.shards.get(id as usize)
    }

    fn owner(&self, key: &str) -> &Cluster {
        // Placement and shard groups are built together, so the index is always valid.
        &self.shards[self.manager.route_document(key) as usize]
    }

    /// Writes a document to the shard that owns its key.
    pub async fn upsert(&self, doc: Document) -> Result<(), DistributedError> {
        self.owner(doc.id.as_str()).upsert(doc).await
    }

    /// Deletes a document from the shard that owns its key.
    pub async fn delete(&self, id: DocumentId) -> Result<bool, DistributedError> {
        self.owner(id.as_str()).delete(id).await
    }

    /// Reads a document from its shard, after a linearizable read on that shard.
    pub async fn get(&self, id: &DocumentId) -> Result<Option<Document>, DistributedError> {
        let shard = self.owner(id.as_str());
        let node = shard
            .leader()
            .or_else(|| shard.online().first().copied())
            .ok_or_else(|| DistributedError::NodeUnavailable(id.to_string()))?;
        shard.get(node, id).await
    }

    /// Searches every shard and merges the hits by score, best first.
    pub async fn search(&self, query: &TermQuery) -> Result<Vec<ScoredDocument>, DistributedError> {
        let mut hits = Vec::new();
        for shard in &self.shards {
            let node = shard
                .leader()
                .or_else(|| shard.online().first().copied())
                .ok_or_else(|| DistributedError::NodeUnavailable("shard".into()))?;
            hits.extend(shard.search(node, query).await?);
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        Ok(hits)
    }
}
