//! Query routing: load balancing across replicas and scatter-gather across shards.

use std::collections::HashMap;

use crate::sharding::{NodeId, ShardId, ShardManager};

/// How a replica is chosen among the replicas of a shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BalancePolicy {
    /// Cycle through the replicas.
    RoundRobin,
    /// The replica with the fewest requests in flight; ties go to the earlier replica.
    LeastConnections,
    /// Pseudo-random choice from a seeded generator (deterministic for a given seed).
    Random {
        /// Seed for the generator.
        seed: u64,
    },
    /// Weighted round robin. Replicas missing from the list get weight 1.
    Weighted(Vec<(NodeId, u32)>),
}

/// Chooses replicas according to a [`BalancePolicy`] and tracks requests in flight.
#[derive(Debug, Clone)]
pub struct LoadBalancer {
    policy: BalancePolicy,
    cursor: u64,
    rng: u64,
    in_flight: HashMap<NodeId, usize>,
}

impl LoadBalancer {
    /// Creates a balancer.
    pub fn new(policy: BalancePolicy) -> Self {
        let rng = match policy {
            BalancePolicy::Random { seed } => seed | 1,
            _ => 1,
        };
        LoadBalancer {
            policy,
            cursor: 0,
            rng,
            in_flight: HashMap::new(),
        }
    }

    /// Picks one of `replicas` (in preference order) and counts it as in flight.
    /// Returns `None` if there are no replicas.
    pub fn pick(&mut self, replicas: &[NodeId]) -> Option<NodeId> {
        if replicas.is_empty() {
            return None;
        }
        let chosen = match self.policy.clone() {
            BalancePolicy::RoundRobin => {
                let i = (self.cursor % replicas.len() as u64) as usize;
                self.cursor += 1;
                replicas[i]
            }
            BalancePolicy::LeastConnections => *replicas
                .iter()
                .min_by_key(|n| self.in_flight.get(n).copied().unwrap_or(0))?,
            BalancePolicy::Random { .. } => {
                let r = self.next_random();
                replicas[(r % replicas.len() as u64) as usize]
            }
            BalancePolicy::Weighted(weights) => {
                let weight_of = |n: &NodeId| {
                    weights
                        .iter()
                        .find(|(id, _)| id == n)
                        .map_or(1u64, |(_, w)| u64::from(*w))
                };
                let total: u64 = replicas.iter().map(weight_of).sum();
                let mut pos = self.cursor % total.max(1);
                self.cursor += 1;
                let mut chosen = replicas[0];
                for n in replicas {
                    let w = weight_of(n);
                    if pos < w {
                        chosen = *n;
                        break;
                    }
                    pos -= w;
                }
                chosen
            }
        };
        *self.in_flight.entry(chosen).or_default() += 1;
        Some(chosen)
    }

    /// Marks a request to `node` as finished.
    pub fn release(&mut self, node: NodeId) {
        if let Some(count) = self.in_flight.get_mut(&node) {
            *count = count.saturating_sub(1);
        }
    }

    /// Requests currently in flight to `node`.
    pub fn in_flight(&self, node: NodeId) -> usize {
        self.in_flight.get(&node).copied().unwrap_or(0)
    }

    fn next_random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

/// One scored result from a shard.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Document identifier.
    pub doc_id: String,
    /// Relevance score; larger is better.
    pub score: f32,
}

/// The merged output of a scatter-gather query.
#[derive(Debug, Clone, PartialEq)]
pub struct Gathered {
    /// Top hits across the shards that answered, best first.
    pub hits: Vec<Hit>,
    /// Shards with no reachable replica. Their hits are missing from `hits`.
    pub failed_shards: Vec<ShardId>,
}

/// Routes documents to shards and runs queries across shards.
#[derive(Debug, Clone)]
pub struct QueryRouter {
    manager: ShardManager,
    balancer: LoadBalancer,
}

impl QueryRouter {
    /// Creates a router over a shard manager.
    pub fn new(manager: ShardManager, policy: BalancePolicy) -> Self {
        QueryRouter {
            manager,
            balancer: LoadBalancer::new(policy),
        }
    }

    /// The shard placement.
    pub fn manager(&self) -> &ShardManager {
        &self.manager
    }

    /// Mutable access to placement, for rebalancing.
    pub fn manager_mut(&mut self) -> &mut ShardManager {
        &mut self.manager
    }

    /// The balancer, for inspecting in-flight counts.
    pub fn balancer(&self) -> &LoadBalancer {
        &self.balancer
    }

    /// Runs `run(shard, node)` on every shard and merges the top `k` hits.
    ///
    /// For each shard the balancer picks a replica first, and the other replicas
    /// follow in preference order. If `run` fails on one, the next replica is tried.
    /// A shard that no replica can answer is listed in `failed_shards` and does not
    /// stop the query.
    pub fn scatter_gather<E>(
        &mut self,
        k: usize,
        mut run: impl FnMut(ShardId, NodeId) -> Result<Vec<Hit>, E>,
    ) -> Gathered {
        let mut hits = Vec::new();
        let mut failed_shards = Vec::new();
        for shard in self.manager.route_query() {
            let replicas = self
                .manager
                .shard(shard)
                .map(|s| s.replicas.clone())
                .unwrap_or_default();
            let Some(first) = self.balancer.pick(&replicas) else {
                failed_shards.push(shard);
                continue;
            };
            self.balancer.release(first);
            let order =
                std::iter::once(first).chain(replicas.iter().copied().filter(|n| *n != first));

            let mut answered = false;
            for node in order {
                if let Ok(mut shard_hits) = run(shard, node) {
                    hits.append(&mut shard_hits);
                    answered = true;
                    break;
                }
            }
            if !answered {
                failed_shards.push(shard);
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.doc_id.cmp(&b.doc_id))
        });
        hits.truncate(k);
        Gathered {
            hits,
            failed_shards,
        }
    }

    /// Shard that holds a document key.
    pub fn route_document(&self, key: &str) -> ShardId {
        self.manager.route_document(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sharding::ShardingStrategy;

    #[test]
    fn round_robin_cycles_replicas() {
        let mut lb = LoadBalancer::new(BalancePolicy::RoundRobin);
        let picks: Vec<NodeId> = (0..4).map(|_| lb.pick(&[1, 2]).unwrap()).collect();
        assert_eq!(picks, [1, 2, 1, 2]);
    }

    #[test]
    fn least_connections_prefers_idle_replica() {
        let mut lb = LoadBalancer::new(BalancePolicy::LeastConnections);
        assert_eq!(lb.pick(&[1, 2]), Some(1));
        assert_eq!(lb.pick(&[1, 2]), Some(2));
        lb.release(1);
        assert_eq!(lb.pick(&[1, 2]), Some(1));
        assert_eq!(lb.in_flight(2), 1);
    }

    #[test]
    fn weighted_matches_weights_over_a_cycle() {
        let mut lb = LoadBalancer::new(BalancePolicy::Weighted(vec![(1, 3), (2, 1)]));
        let picks: Vec<NodeId> = (0..8).map(|_| lb.pick(&[1, 2]).unwrap()).collect();
        assert_eq!(picks.iter().filter(|&&n| n == 1).count(), 6);
    }

    #[test]
    fn random_is_deterministic_for_a_seed() {
        let run = || {
            let mut lb = LoadBalancer::new(BalancePolicy::Random { seed: 9 });
            (0..10)
                .map(|_| lb.pick(&[1, 2, 3]).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    fn router() -> QueryRouter {
        let m = ShardManager::new(ShardingStrategy::Hash { shards: 3 }, 2, [1, 2, 3]).unwrap();
        QueryRouter::new(m, BalancePolicy::RoundRobin)
    }

    #[test]
    fn scatter_gather_merges_and_truncates() {
        let mut r = router();
        let gathered = r.scatter_gather(3, |shard, _node| {
            Ok::<_, ()>(vec![
                Hit {
                    doc_id: format!("s{shard}-a"),
                    score: shard as f32,
                },
                Hit {
                    doc_id: format!("s{shard}-b"),
                    score: 0.5,
                },
            ])
        });
        assert!(gathered.failed_shards.is_empty());
        assert_eq!(gathered.hits.len(), 3);
        assert_eq!(gathered.hits[0].doc_id, "s2-a");
        assert_eq!(gathered.hits[1].doc_id, "s1-a");
    }

    #[test]
    fn failover_uses_another_replica() {
        let mut r = router();
        // Node 1 is down; every shard that has node 1 as a replica must fail over.
        let gathered = r.scatter_gather(10, |shard, node| {
            if node == 1 {
                Err("down")
            } else {
                Ok(vec![Hit {
                    doc_id: format!("s{shard}"),
                    score: 1.0,
                }])
            }
        });
        assert!(gathered.failed_shards.is_empty());
        assert_eq!(gathered.hits.len(), 3);
    }

    #[test]
    fn shards_without_any_live_replica_are_reported() {
        let mut r = router();
        let gathered = r.scatter_gather(10, |shard, _node| {
            if shard == 0 {
                Err("gone")
            } else {
                Ok(vec![Hit {
                    doc_id: format!("s{shard}"),
                    score: 1.0,
                }])
            }
        });
        assert_eq!(gathered.failed_shards, [0]);
        assert_eq!(gathered.hits.len(), 2);
    }
}
