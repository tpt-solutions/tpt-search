//! Property test: HNSW recall against exact brute-force search.

use proptest::prelude::*;
use tpt_search_vector::{distance, DistanceMetric, HnswConfig, HnswIndex};

fn brute_force(data: &[Vec<f32>], query: &[f32], k: usize, metric: DistanceMetric) -> Vec<u64> {
    let mut order: Vec<(f32, u64)> = data
        .iter()
        .enumerate()
        .map(|(i, v)| (distance(metric, query, v), i as u64))
        .collect();
    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    order.into_iter().take(k).map(|(_, id)| id).collect()
}

fn recall(metric: DistanceMetric, data: &[Vec<f32>], queries: &[Vec<f32>], k: usize) -> f64 {
    let config = HnswConfig {
        metric,
        m: 16,
        ef_construction: 200,
        ef_search: 64,
        seed: 42,
    };
    let mut index = HnswIndex::new(config).unwrap();
    for (i, v) in data.iter().enumerate() {
        index.insert(i as u64, v.clone()).unwrap();
    }
    let mut hits = 0usize;
    for q in queries {
        let truth = brute_force(data, q, k, metric);
        let found: Vec<u64> = index
            .search(q, k, 64)
            .unwrap()
            .into_iter()
            .map(|s| s.key)
            .collect();
        hits += found.iter().filter(|id| truth.contains(id)).count();
    }
    hits as f64 / (queries.len() * k) as f64
}

fn vectors(n: usize, dim: usize) -> impl Strategy<Value = Vec<Vec<f32>>> {
    proptest::collection::vec(proptest::collection::vec(-1.0f32..1.0, dim), n)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    #[test]
    fn recall_is_high_for_random_data_euclidean(
        data in vectors(400, 12),
        queries in vectors(10, 12),
    ) {
        let r = recall(DistanceMetric::Euclidean, &data, &queries, 10);
        prop_assert!(r >= 0.85, "recall {r}");
    }

    #[test]
    fn recall_is_high_for_random_data_cosine(
        data in vectors(400, 12),
        queries in vectors(10, 12),
    ) {
        // Skip near-zero vectors: cosine is undefined there and both sides
        // would tie arbitrarily.
        prop_assume!(data.iter().all(|v| v.iter().map(|x| x * x).sum::<f32>() > 1e-3));
        let r = recall(DistanceMetric::Cosine, &data, &queries, 10);
        prop_assert!(r >= 0.85, "recall {r}");
    }
}

#[test]
fn recall_on_clustered_data() {
    // Ten tight clusters: the diversity heuristic is what keeps these connected.
    let mut data = Vec::new();
    let mut state = 17u64;
    let mut rnd = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 10_000) as f32 / 10_000.0 - 0.5
    };
    for c in 0..10 {
        let center: Vec<f32> = (0..16).map(|d| ((c * 7 + d) % 5) as f32 * 3.0).collect();
        for _ in 0..200 {
            data.push(center.iter().map(|x| x + rnd() * 0.2).collect::<Vec<f32>>());
        }
    }
    let queries: Vec<Vec<f32>> = data.iter().step_by(137).cloned().collect();
    let r = recall(DistanceMetric::Euclidean, &data, &queries, 10);
    assert!(r >= 0.9, "clustered recall {r}");
}
