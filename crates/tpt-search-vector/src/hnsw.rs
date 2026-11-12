//! Hierarchical Navigable Small World (HNSW) graph for approximate k-NN search.
//!
//! Each node is assigned a random top layer (geometric distribution, so upper
//! layers are sparse). Search descends greedily through the upper layers, then
//! runs a best-first beam search (`ef` candidates) on layer 0.
//!
//! Neighbour lists are chosen with the diversity heuristic from the HNSW paper
//! (Algorithm 4), which keeps links spread across clusters and noticeably improves
//! recall on clustered data.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

use serde::{Deserialize, Serialize};
use tpt_search_core::{DistanceMetric, IndexError};

use crate::distance::{distance, dot, score_from_distance};

/// Tuning parameters for an [`HnswIndex`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HnswConfig {
    /// Similarity metric.
    pub metric: DistanceMetric,
    /// Links per node on upper layers. Layer 0 allows `2 * m`.
    pub m: usize,
    /// Beam width while building. Larger is slower to build and gives better graphs.
    pub ef_construction: usize,
    /// Default beam width for callers that do not pick one themselves.
    pub ef_search: usize,
    /// Seed for the layer-assignment generator. Equal seeds give equal graphs.
    pub seed: u64,
}

impl Default for HnswConfig {
    fn default() -> Self {
        HnswConfig {
            metric: DistanceMetric::Cosine,
            m: 16,
            ef_construction: 200,
            ef_search: 50,
            seed: 0x5EED_1234_ABCD_0001,
        }
    }
}

impl HnswConfig {
    /// Checks that the parameters are usable.
    pub fn validate(&self) -> Result<(), IndexError> {
        if self.m < 2 {
            return Err(IndexError::InvalidValue("HNSW m must be at least 2".into()));
        }
        if self.ef_construction < self.m {
            return Err(IndexError::InvalidValue(
                "HNSW ef_construction must be at least m".into(),
            ));
        }
        if self.ef_search == 0 {
            return Err(IndexError::InvalidValue("HNSW ef_search must be at least 1".into()));
        }
        Ok(())
    }
}

/// A search result.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredVector {
    /// Key the vector was inserted with.
    pub key: u64,
    /// Similarity-style score; larger is closer (see [`crate::distance::score_from_distance`]).
    pub score: f32,
    /// Raw distance under the index metric; smaller is closer.
    pub distance: f32,
}

/// One vertex of the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HnswNode {
    /// External key.
    pub key: u64,
    /// The stored vector.
    pub vector: Vec<f32>,
    /// `neighbors[l]` lists this node's links on layer `l`. The node's top layer is
    /// `neighbors.len() - 1`.
    pub neighbors: Vec<Vec<u32>>,
}

/// Approximate nearest-neighbour index over `f32` vectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswIndex {
    config: HnswConfig,
    dim: Option<usize>,
    nodes: Vec<HnswNode>,
    entry: Option<u32>,
    max_level: usize,
    by_key: HashMap<u64, u32>,
    rng_state: u64,
}

/// Candidate ordered by distance, then node id, so ordering is total.
#[derive(Debug, Clone, Copy)]
struct Cand {
    dist: f32,
    id: u32,
}

impl PartialEq for Cand {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Cand {}

impl PartialOrd for Cand {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Cand {
    fn cmp(&self, other: &Self) -> Ordering {
        self.dist
            .total_cmp(&other.dist)
            .then(self.id.cmp(&other.id))
    }
}

/// Multiplicative hasher for dense `u32` node ids. SipHash is needlessly slow for
/// the visited set, which is probed thousands of times per search.
#[derive(Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    fn write_u32(&mut self, id: u32) {
        self.0 = u64::from(id).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type IdSet = HashSet<u32, BuildHasherDefault<IdHasher>>;

/// Upper bound on the layer assigned to any node.
const MAX_LEVEL: usize = 32;

impl HnswIndex {
    /// Creates an empty index. Fails if the configuration is invalid.
    pub fn new(config: HnswConfig) -> Result<Self, IndexError> {
        config.validate()?;
        let rng_state = config.seed | 1;
        Ok(HnswIndex {
            config,
            dim: None,
            nodes: Vec::new(),
            entry: None,
            max_level: 0,
            by_key: HashMap::new(),
            rng_state,
        })
    }

    /// The configuration this index was built with.
    pub fn config(&self) -> &HnswConfig {
        &self.config
    }

    /// Vector dimension, fixed by the first insertion.
    pub fn dim(&self) -> Option<usize> {
        self.dim
    }

    /// Number of live keys.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Returns `true` if no keys are present.
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// Returns whether `key` is present.
    pub fn contains_key(&self, key: u64) -> bool {
        self.by_key.contains_key(&key)
    }

    /// Inserts a vector under `key`. Inserting an existing key replaces it: the old
    /// node stays in the graph for navigation but is never returned.
    pub fn insert(&mut self, key: u64, vector: Vec<f32>) -> Result<(), IndexError> {
        self.check_vector(&vector)?;
        let vector = self.prepared(vector);
        if self.dim.is_none() {
            self.dim = Some(vector.len());
        }
        let level = self.random_level();

        // Phase 1: find, per layer, the neighbours the new node should link to.
        // The new node is not in the graph yet, so searches cannot return it.
        let mut plan: Vec<(usize, Vec<u32>)> = Vec::new();
        if let Some(entry) = self.entry {
            let mut entries = vec![entry];
            for layer in (level + 1..=self.max_level).rev() {
                let best = self.search_layer(&vector, &entries, 1, layer, &|_| true);
                entries = vec![best[0].id];
            }
            for layer in (0..=level.min(self.max_level)).rev() {
                let cands = self.search_layer(
                    &vector,
                    &entries,
                    self.config.ef_construction,
                    layer,
                    &|_| true,
                );
                let selected = self.select_neighbors(&cands, self.max_neighbors(layer));
                entries = cands.iter().map(|c| c.id).collect();
                plan.push((layer, selected));
            }
        }

        // Phase 2: add the node and its forward links, then add back-links and prune.
        let mut neighbors = vec![Vec::new(); level + 1];
        for (layer, selected) in &plan {
            neighbors[*layer] = selected.clone();
        }
        self.nodes.push(HnswNode {
            key,
            vector,
            neighbors,
        });
        let new_id = (self.nodes.len() - 1) as u32;

        for (layer, selected) in plan {
            let limit = self.max_neighbors(layer);
            for s in selected {
                self.nodes[s as usize].neighbors[layer].push(new_id);
                if self.nodes[s as usize].neighbors[layer].len() > limit {
                    let links = self.nodes[s as usize].neighbors[layer].clone();
                    let base = self.nodes[s as usize].vector.clone();
                    let cands = self.sorted_candidates(&base, &links);
                    // Nearest-only pruning: the diversity heuristic costs O(limit^2)
                    // distances per overflow and dominated build time.
                    let pruned: Vec<u32> = cands.iter().take(limit).map(|c| c.id).collect();
                    self.nodes[s as usize].neighbors[layer] = pruned;
                }
            }
        }

        if self.entry.is_none() || level > self.max_level {
            self.entry = Some(new_id);
            self.max_level = level;
        }
        self.by_key.insert(key, new_id);
        Ok(())
    }

    /// Inserts many vectors. Stops at the first error; earlier items stay inserted.
    pub fn batch_insert(
        &mut self,
        items: impl IntoIterator<Item = (u64, Vec<f32>)>,
    ) -> Result<(), IndexError> {
        for (key, vector) in items {
            self.insert(key, vector)?;
        }
        Ok(())
    }

    /// Returns up to `k` nearest live keys, closest first, searching with beam
    /// width `max(ef, k)`.
    pub fn search(&self, query: &[f32], k: usize, ef: usize) -> Result<Vec<ScoredVector>, IndexError> {
        self.search_inner(query, k, ef, &|_| true)
    }

    /// Like [`search`](Self::search), but only keys for which `accept` returns
    /// `true` can be returned. Traversal still uses rejected nodes, so filtering
    /// costs recall only when most nodes are rejected.
    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        accept: impl Fn(u64) -> bool,
    ) -> Result<Vec<ScoredVector>, IndexError> {
        self.search_inner(query, k, ef, &|id| accept(self.nodes[id as usize].key))
    }

    fn search_inner(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        accept_node: &dyn Fn(u32) -> bool,
    ) -> Result<Vec<ScoredVector>, IndexError> {
        let query_owned;
        let query: &[f32] = if self.config.metric == DistanceMetric::Cosine {
            query_owned = unit(query);
            &query_owned
        } else {
            query
        };
        if let Some(dim) = self.dim {
            if query.len() != dim {
                return Err(IndexError::DimensionMismatch {
                    expected: dim,
                    got: query.len(),
                });
            }
        }
        check_finite(query)?;
        let Some(entry) = self.entry else {
            return Ok(Vec::new());
        };
        if k == 0 {
            return Ok(Vec::new());
        }

        let mut entries = vec![entry];
        for layer in (1..=self.max_level).rev() {
            let best = self.search_layer(query, &entries, 1, layer, &|_| true);
            entries = vec![best[0].id];
        }
        // Only current versions of a key may be returned.
        let current = |id: u32| {
            let key = self.nodes[id as usize].key;
            self.by_key.get(&key) == Some(&id) && accept_node(id)
        };
        let beam = ef.max(k);
        let found = self.search_layer(query, &entries, beam, 0, &current);

        Ok(found
            .into_iter()
            .take(k)
            .map(|c| ScoredVector {
                key: self.nodes[c.id as usize].key,
                score: score_from_distance(self.config.metric, c.dist),
                distance: c.dist,
            })
            .collect())
    }

    /// Best-first beam search on one layer.
    ///
    /// The result holds at most `ef` candidates that pass `accept`, sorted by
    /// distance. Only accepted nodes are returned, but rejected nodes are still
    /// expanded so the search can route through them.
    fn search_layer(
        &self,
        query: &[f32],
        entries: &[u32],
        ef: usize,
        layer: usize,
        accept: &dyn Fn(u32) -> bool,
    ) -> Vec<Cand> {
        let ef = ef.max(1);
        let mut visited: IdSet = IdSet::with_capacity_and_hasher(ef * 4, Default::default());
        let mut candidates: BinaryHeap<Reverse<Cand>> = BinaryHeap::new();
        let mut results: BinaryHeap<Cand> = BinaryHeap::new();

        for &e in entries {
            if !visited.insert(e) {
                continue;
            }
            let cand = Cand {
                dist: self.dist(query, e),
                id: e,
            };
            candidates.push(Reverse(cand));
            if accept(e) {
                results.push(cand);
                if results.len() > ef {
                    results.pop();
                }
            }
        }

        while let Some(Reverse(current)) = candidates.pop() {
            if results.len() >= ef && results.peek().is_some_and(|worst| current.dist > worst.dist) {
                break;
            }
            let Some(links) = self.nodes[current.id as usize].neighbors.get(layer) else {
                continue;
            };
            for &n in links {
                if !visited.insert(n) {
                    continue;
                }
                let cand = Cand {
                    dist: self.dist(query, n),
                    id: n,
                };
                let worst = results.peek().map_or(f32::INFINITY, |w| w.dist);
                if results.len() < ef || cand.dist < worst {
                    candidates.push(Reverse(cand));
                    if accept(n) {
                        results.push(cand);
                        if results.len() > ef {
                            results.pop();
                        }
                    }
                }
            }
        }
        results.into_sorted_vec()
    }

    /// Diversity heuristic: take candidates closest-first, but skip any candidate
    /// that is closer to an already selected neighbour than to the base point.
    /// Pads with the nearest skipped candidates if fewer than `m` were kept.
    fn select_neighbors(&self, cands: &[Cand], m: usize) -> Vec<u32> {
        let mut selected: Vec<Cand> = Vec::with_capacity(m);
        let mut skipped: Vec<Cand> = Vec::new();
        for &c in cands {
            if selected.len() >= m {
                break;
            }
            let diverse = selected.iter().all(|s| {
                self.dist_between(c.id, s.id) > c.dist
            });
            if diverse {
                selected.push(c);
            } else {
                skipped.push(c);
            }
        }
        for c in skipped {
            if selected.len() >= m {
                break;
            }
            selected.push(c);
        }
        selected.into_iter().map(|c| c.id).collect()
    }

    /// Candidates for `links` measured against `base`, closest first.
    fn sorted_candidates(&self, base: &[f32], links: &[u32]) -> Vec<Cand> {
        let mut cands: Vec<Cand> = links
            .iter()
            .map(|&id| Cand {
                dist: self.dist(base, id),
                id,
            })
            .collect();
        cands.sort();
        cands
    }

    /// Distance under the index metric. Cosine indexes store unit vectors, so their
    /// distance is `1 - dot`, which costs a single dot product.
    fn kernel_distance(&self, a: &[f32], b: &[f32]) -> f32 {
        match self.config.metric {
            DistanceMetric::Cosine => 1.0 - dot(a, b),
            metric => distance(metric, a, b),
        }
    }

    fn dist(&self, query: &[f32], id: u32) -> f32 {
        self.kernel_distance(query, &self.nodes[id as usize].vector)
    }

    fn dist_between(&self, a: u32, b: u32) -> f32 {
        self.kernel_distance(&self.nodes[a as usize].vector, &self.nodes[b as usize].vector)
    }

    fn max_neighbors(&self, layer: usize) -> usize {
        if layer == 0 {
            2 * self.config.m
        } else {
            self.config.m
        }
    }

    /// Stored form of a vector: unit length for cosine, unchanged otherwise.
    fn prepared(&self, vector: Vec<f32>) -> Vec<f32> {
        if self.config.metric == DistanceMetric::Cosine {
            unit(&vector)
        } else {
            vector
        }
    }

    fn check_vector(&self, vector: &[f32]) -> Result<(), IndexError> {
        if let Some(dim) = self.dim {
            if vector.len() != dim {
                return Err(IndexError::DimensionMismatch {
                    expected: dim,
                    got: vector.len(),
                });
            }
        }
        check_finite(vector)
    }

    /// Draws a layer from the geometric distribution `floor(-ln(U) * mL)`, with
    /// `mL = 1 / ln(m)`.
    fn random_level(&mut self) -> usize {
        let u = (self.next_u64() as f64 + 1.0) / (u64::MAX as f64 + 2.0);
        let ml = 1.0 / (self.config.m as f64).ln();
        let level = (-u.ln() * ml).floor();
        (level as usize).min(MAX_LEVEL)
    }

    /// xorshift64*: a small, fast, deterministic generator.
    fn next_u64(&mut self) -> u64 {
        let mut x = self.rng_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng_state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Scales a vector to unit length. Zero vectors are returned unchanged.
fn unit(v: &[f32]) -> Vec<f32> {
    let norm = dot(v, v).sqrt();
    if norm > 0.0 {
        v.iter().map(|x| x / norm).collect()
    } else {
        v.to_vec()
    }
}

fn check_finite(values: &[f32]) -> Result<(), IndexError> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(IndexError::InvalidValue(
            "vector contains NaN or infinity".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(metric: DistanceMetric) -> HnswConfig {
        HnswConfig {
            metric,
            ..HnswConfig::default()
        }
    }

    #[test]
    fn invalid_config_is_rejected() {
        let bad = HnswConfig {
            m: 1,
            ..HnswConfig::default()
        };
        assert!(HnswIndex::new(bad).is_err());
        let bad = HnswConfig {
            ef_construction: 4,
            m: 8,
            ..HnswConfig::default()
        };
        assert!(HnswIndex::new(bad).is_err());
    }

    #[test]
    fn finds_exact_neighbours_on_a_small_set() {
        let mut idx = HnswIndex::new(cfg(DistanceMetric::Euclidean)).unwrap();
        for i in 0..20u64 {
            idx.insert(i, vec![i as f32, 0.0]).unwrap();
        }
        let hits = idx.search(&[7.2, 0.0], 3, 20).unwrap();
        let keys: Vec<u64> = hits.iter().map(|h| h.key).collect();
        assert_eq!(keys, [7, 8, 6]);
        assert!(hits[0].score >= hits[1].score);
    }

    #[test]
    fn cosine_ignores_magnitude() {
        let mut idx = HnswIndex::new(cfg(DistanceMetric::Cosine)).unwrap();
        idx.insert(1, vec![1.0, 0.0]).unwrap();
        idx.insert(2, vec![0.0, 1.0]).unwrap();
        idx.insert(3, vec![100.0, 1.0]).unwrap();
        let hits = idx.search(&[5.0, 0.0], 1, 10).unwrap();
        assert_eq!(hits[0].key, 1);
        assert!((hits[0].score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn replacing_a_key_hides_the_old_vector() {
        let mut idx = HnswIndex::new(cfg(DistanceMetric::Euclidean)).unwrap();
        idx.insert(1, vec![0.0, 0.0]).unwrap();
        idx.insert(2, vec![10.0, 10.0]).unwrap();
        idx.insert(1, vec![10.0, 9.0]).unwrap();
        assert_eq!(idx.len(), 2);
        idx.search(&[0.0, 0.0], 2, 10).unwrap();
        let nearest = &idx.search(&[10.0, 10.0], 1, 10).unwrap()[0];
        assert_eq!(nearest.key, 2);
    }

    #[test]
    fn filtered_search_respects_predicate() {
        let mut idx = HnswIndex::new(cfg(DistanceMetric::Euclidean)).unwrap();
        for i in 0..50u64 {
            idx.insert(i, vec![i as f32]).unwrap();
        }
        let hits = idx.search_filtered(&[10.0], 5, 50, |k| k % 2 == 1).unwrap();
        assert_eq!(hits.len(), 5);
        assert!(hits.iter().all(|h| h.key % 2 == 1));
    }

    #[test]
    fn rejects_wrong_dimension_and_non_finite_values() {
        let mut idx = HnswIndex::new(HnswConfig::default()).unwrap();
        idx.insert(1, vec![1.0, 2.0, 3.0]).unwrap();
        assert!(matches!(
            idx.insert(2, vec![1.0]),
            Err(IndexError::DimensionMismatch { expected: 3, got: 1 })
        ));
        assert!(matches!(
            idx.search(&[1.0, 2.0], 1, 1),
            Err(IndexError::DimensionMismatch { .. })
        ));
        assert!(idx.insert(3, vec![f32::NAN, 0.0, 0.0]).is_err());
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn empty_index_and_zero_k_return_nothing() {
        let idx = HnswIndex::new(HnswConfig::default()).unwrap();
        assert!(idx.search(&[1.0], 5, 5).unwrap().is_empty());
        let mut idx = HnswIndex::new(HnswConfig::default()).unwrap();
        idx.insert(1, vec![1.0]).unwrap();
        assert!(idx.search(&[1.0], 0, 5).unwrap().is_empty());
    }

    #[test]
    fn same_seed_gives_same_graph() {
        let build = || {
            let mut idx = HnswIndex::new(cfg(DistanceMetric::Euclidean)).unwrap();
            for i in 0..200u64 {
                idx.insert(i, vec![(i * 37 % 101) as f32, (i * 13 % 59) as f32]).unwrap();
            }
            idx
        };
        let a = build();
        let b = build();
        assert_eq!(a.nodes, b.nodes);
    }
}
