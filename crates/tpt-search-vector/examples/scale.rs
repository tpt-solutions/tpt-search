//! Scale check for the Phase 2 milestone: k-NN with cosine similarity over many
//! vectors. Reports build time, query latency, and recall@k against exact search.
//!
//! Run in release mode:
//!     cargo run --release -p tpt-search-vector --example scale -- 1000000 128
//!
//! Arguments: number of vectors (default 100000), dimension (default 128).

use std::time::Instant;

use tpt_search_vector::{distance, DistanceMetric, HnswConfig, HnswIndex};

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100_000);
    let dim: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(128);
    let queries = 200usize;
    let k = 10usize;
    let ef: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);

    println!("kernel: {:?}", tpt_search_vector::active_kernel());
    println!("generating {n} vectors of dimension {dim}");
    let mut state = 0x1234_5678_9ABC_DEF1u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 2_000_000) as f32 / 1_000_000.0 - 1.0
    };
    let data: Vec<Vec<f32>> = (0..n)
        .map(|_| (0..dim).map(|_| next()).collect())
        .collect();
    let query_set: Vec<Vec<f32>> = (0..queries)
        .map(|_| (0..dim).map(|_| next()).collect())
        .collect();

    let config = HnswConfig {
        metric: DistanceMetric::Cosine,
        m: 16,
        ef_construction: 200,
        ef_search: 100,
        seed: 7,
    };
    let mut index = HnswIndex::new(config).expect("valid config");
    let started = Instant::now();
    for (i, v) in data.iter().enumerate() {
        index.insert(i as u64, v.clone()).expect("insert");
        if (i + 1) % (n / 10).max(1) == 0 {
            println!(
                "  inserted {:>9} / {n}  elapsed {:.1}s",
                i + 1,
                started.elapsed().as_secs_f64()
            );
        }
    }
    let build = started.elapsed().as_secs_f64();
    println!("build: {build:.1}s ({:.0} inserts/s)", n as f64 / build);


    let mut latencies = Vec::with_capacity(queries);
    let mut recall_hits = 0usize;
    for q in &query_set {
        let t = Instant::now();
        let found = index.search(q, k, ef).expect("search");
        latencies.push(t.elapsed().as_secs_f64() * 1000.0);

        let mut exact: Vec<(f32, u64)> = data
            .iter()
            .enumerate()
            .map(|(i, v)| (distance(DistanceMetric::Cosine, q, v), i as u64))
            .collect();
        exact.sort_by(|a, b| a.0.total_cmp(&b.0));
        let truth: Vec<u64> = exact.iter().take(k).map(|(_, id)| *id).collect();
        recall_hits += found.iter().filter(|h| truth.contains(&h.key)).count();
    }
    latencies.sort_by(|a, b| a.total_cmp(b));
    let p = |q: f64| latencies[((latencies.len() as f64 * q) as usize).min(latencies.len() - 1)];
    println!(
        "query (k={k}, ef={ef}, {queries} queries): p50 {:.2} ms, p95 {:.2} ms, p99 {:.2} ms",
        p(0.50),
        p(0.95),
        p(0.99)
    );
    println!(
        "recall@{k}: {:.4}",
        recall_hits as f64 / (queries * k) as f64
    );
}
