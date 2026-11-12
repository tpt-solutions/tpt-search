//! Distributed layer for `tpt-search`: shard placement and document routing, load
//! balancing and scatter-gather query routing, and replicated state machines built
//! on `openraft` (see the `consensus` module).
//!
//! # Example
//!
//! ```
//! use tpt_search_distributed::{BalancePolicy, Hit, QueryRouter, ShardManager, ShardingStrategy};
//!
//! let manager = ShardManager::new(ShardingStrategy::Hash { shards: 4 }, 2, [1, 2, 3])?;
//! let mut router = QueryRouter::new(manager, BalancePolicy::RoundRobin);
//! let gathered = router.scatter_gather(10, |shard, _node| {
//!     Ok::<_, ()>(vec![Hit { doc_id: format!("doc-in-shard-{shard}"), score: 1.0 }])
//! });
//! assert_eq!(gathered.hits.len(), 4);
//! # Ok::<(), tpt_search_core::DistributedError>(())
//! ```

#![doc(html_root_url = "https://docs.rs/tpt-search-distributed")]

pub mod consensus;
pub mod error;
pub mod router;
pub mod sharded;
pub mod sharding;

pub use consensus::{Cluster, ClusterRequest, ClusterResponse, ClusterStatus, NodeStatus};
pub use error::DistributedError;
pub use router::{BalancePolicy, Gathered, Hit, LoadBalancer, QueryRouter};
pub use sharded::ShardedCluster;
pub use sharding::{NodeId, ShardId, ShardInfo, ShardManager, ShardMigration, ShardingStrategy};
