//! Shard management: how documents map to shards, where shard replicas live, and
//! how placement changes when nodes join or leave.
//!
//! Hash routing uses FNV-1a, not the standard library hasher, because routing must
//! agree across processes and releases.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::DistributedError;

/// Identifier of a cluster node.
pub type NodeId = u64;
/// Identifier of a shard.
pub type ShardId = u32;

/// How document keys map to shards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShardingStrategy {
    /// `hash(key) mod shards`. Spreads keys evenly; range queries touch every shard.
    Hash {
        /// Number of shards (at least 1).
        shards: u32,
    },
    /// Sorted exclusive upper bounds. Shard `i` holds keys in `[boundaries[i-1], boundaries[i])`,
    /// and the last shard is unbounded above. Keeps key ranges together.
    Range {
        /// Sorted, distinct upper bounds. `n` bounds make `n + 1` shards.
        boundaries: Vec<String>,
    },
}

impl ShardingStrategy {
    /// Number of shards this strategy produces.
    pub fn shard_count(&self) -> usize {
        match self {
            ShardingStrategy::Hash { shards } => *shards as usize,
            ShardingStrategy::Range { boundaries } => boundaries.len() + 1,
        }
    }

    /// The shard for a document key.
    pub fn route(&self, key: &str) -> ShardId {
        match self {
            ShardingStrategy::Hash { shards } => {
                (fnv1a(key.as_bytes()) % u64::from(*shards)) as ShardId
            }
            ShardingStrategy::Range { boundaries } => {
                boundaries.partition_point(|b| b.as_str() <= key) as ShardId
            }
        }
    }

    fn validate(&self) -> Result<(), DistributedError> {
        match self {
            ShardingStrategy::Hash { shards } if *shards == 0 => Err(
                DistributedError::InvalidConfig("shard count must be at least 1".into()),
            ),
            ShardingStrategy::Range { boundaries } => {
                if boundaries.windows(2).all(|w| w[0] < w[1]) {
                    Ok(())
                } else {
                    Err(DistributedError::InvalidConfig(
                        "range boundaries must be sorted and distinct".into(),
                    ))
                }
            }
            _ => Ok(()),
        }
    }
}

/// FNV-1a, 64-bit.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// A shard and the nodes that hold its replicas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardInfo {
    /// Shard identifier.
    pub id: ShardId,
    /// Nodes holding a replica, in preference order (the first is the preferred leader).
    pub replicas: Vec<NodeId>,
}

/// A replica that must be copied to a new node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardMigration {
    /// Shard being moved.
    pub shard: ShardId,
    /// A node that already holds the shard, to copy from. `None` if the shard had no replica.
    pub from: Option<NodeId>,
    /// Node receiving the replica.
    pub to: NodeId,
}

/// Tracks shard placement across the cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShardManager {
    strategy: ShardingStrategy,
    replication_factor: usize,
    nodes: BTreeSet<NodeId>,
    shards: Vec<ShardInfo>,
}

impl ShardManager {
    /// Creates a manager and places replicas on `nodes`.
    ///
    /// Replicas are spread round-robin: shard `i`'s replicas start at node `i mod n`.
    /// The replication factor cannot exceed the number of nodes.
    pub fn new(
        strategy: ShardingStrategy,
        replication_factor: usize,
        nodes: impl IntoIterator<Item = NodeId>,
    ) -> Result<Self, DistributedError> {
        strategy.validate()?;
        let nodes: BTreeSet<NodeId> = nodes.into_iter().collect();
        if replication_factor == 0 || replication_factor > nodes.len() {
            return Err(DistributedError::InvalidConfig(format!(
                "replication factor {replication_factor} needs between 1 and {} nodes",
                nodes.len()
            )));
        }
        let mut manager = ShardManager {
            shards: Vec::new(),
            strategy,
            replication_factor,
            nodes,
        };
        manager.shards = manager.placement();
        Ok(manager)
    }

    /// The sharding strategy.
    pub fn strategy(&self) -> &ShardingStrategy {
        &self.strategy
    }

    /// Current cluster membership.
    pub fn nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.iter().copied()
    }

    /// All shards with their replicas.
    pub fn shards(&self) -> &[ShardInfo] {
        &self.shards
    }

    /// Looks up a shard.
    pub fn shard(&self, id: ShardId) -> Option<&ShardInfo> {
        self.shards.get(id as usize)
    }

    /// The shard that holds a document key.
    pub fn route_document(&self, key: &str) -> ShardId {
        self.strategy.route(key)
    }

    /// Shards a query must visit. Without key-based pruning this is every shard.
    pub fn route_query(&self) -> Vec<ShardId> {
        self.shards.iter().map(|s| s.id).collect()
    }

    /// Changes cluster membership and recomputes placement. Returns the replicas that
    /// must be copied to new nodes. Replicas on removed nodes are dropped, not
    /// migrated, because the source is gone.
    pub fn rebalance(
        &mut self,
        nodes: impl IntoIterator<Item = NodeId>,
    ) -> Result<Vec<ShardMigration>, DistributedError> {
        let nodes: BTreeSet<NodeId> = nodes.into_iter().collect();
        if self.replication_factor > nodes.len() {
            return Err(DistributedError::InvalidConfig(format!(
                "replication factor {} needs at least that many nodes",
                self.replication_factor
            )));
        }
        self.nodes = nodes;
        let placed = self.placement();

        let mut migrations = Vec::new();
        for (old, new) in self.shards.iter().zip(&placed) {
            for &to in &new.replicas {
                if !old.replicas.contains(&to) {
                    let from = old
                        .replicas
                        .iter()
                        .copied()
                        .find(|n| self.nodes.contains(n));
                    migrations.push(ShardMigration {
                        shard: new.id,
                        from,
                        to,
                    });
                }
            }
        }
        self.shards = placed;
        Ok(migrations)
    }

    /// Replica placement for the current nodes, ignoring the previous placement.
    fn placement(&self) -> Vec<ShardInfo> {
        let nodes: Vec<NodeId> = self.nodes.iter().copied().collect();
        (0..self.strategy.shard_count())
            .map(|i| ShardInfo {
                id: i as ShardId,
                replicas: (0..self.replication_factor)
                    .map(|j| nodes[(i + j) % nodes.len()])
                    .collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_routing_is_stable_and_spread() {
        let s = ShardingStrategy::Hash { shards: 4 };
        assert_eq!(s.route("doc-1"), s.route("doc-1"));
        let mut counts = [0usize; 4];
        for i in 0..4000 {
            counts[s.route(&format!("doc-{i}")) as usize] += 1;
        }
        assert!(counts.iter().all(|&c| c > 700), "uneven: {counts:?}");
    }

    #[test]
    fn fnv_matches_known_value() {
        // Reference value for FNV-1a 64 of "a".
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn range_routing_uses_bounds() {
        let s = ShardingStrategy::Range {
            boundaries: vec!["g".into(), "p".into()],
        };
        assert_eq!(s.route("apple"), 0);
        assert_eq!(s.route("g"), 1);
        assert_eq!(s.route("kiwi"), 1);
        assert_eq!(s.route("zebra"), 2);
        assert_eq!(s.shard_count(), 3);
    }

    #[test]
    fn invalid_configurations_are_rejected() {
        assert!(ShardManager::new(ShardingStrategy::Hash { shards: 0 }, 1, [1]).is_err());
        assert!(ShardManager::new(ShardingStrategy::Hash { shards: 2 }, 3, [1, 2]).is_err());
        assert!(ShardManager::new(
            ShardingStrategy::Range {
                boundaries: vec!["b".into(), "a".into()]
            },
            1,
            [1]
        )
        .is_err());
    }

    #[test]
    fn replicas_are_spread_and_distinct() {
        let m = ShardManager::new(ShardingStrategy::Hash { shards: 3 }, 2, [10, 20, 30]).unwrap();
        assert_eq!(m.shard(0).unwrap().replicas, [10, 20]);
        assert_eq!(m.shard(1).unwrap().replicas, [20, 30]);
        assert_eq!(m.shard(2).unwrap().replicas, [30, 10]);
        for s in m.shards() {
            let unique: BTreeSet<_> = s.replicas.iter().collect();
            assert_eq!(unique.len(), s.replicas.len());
        }
    }

    #[test]
    fn adding_a_node_reports_migrations_to_it() {
        let mut m = ShardManager::new(ShardingStrategy::Hash { shards: 3 }, 2, [1, 2, 3]).unwrap();
        let before: Vec<Vec<NodeId>> = m.shards().iter().map(|s| s.replicas.clone()).collect();
        let migrations = m.rebalance([1, 2, 3, 4]).unwrap();
        assert!(!migrations.is_empty());
        for mig in &migrations {
            assert!(!before[mig.shard as usize].contains(&mig.to));
            assert!(mig.from.is_some());
        }
    }

    #[test]
    fn removing_a_node_drops_its_replicas() {
        let mut m = ShardManager::new(ShardingStrategy::Hash { shards: 2 }, 2, [1, 2, 3]).unwrap();
        m.rebalance([1, 2]).unwrap();
        for s in m.shards() {
            assert!(s.replicas.iter().all(|n| *n == 1 || *n == 2));
        }
        assert!(
            m.rebalance([1]).is_err(),
            "replication factor 2 needs two nodes"
        );
    }
}
