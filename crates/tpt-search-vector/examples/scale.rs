//! Scale check for the Phase 2 milestone: k-NN with cosine similarity over many
//! vectors. Reports build time, query latency, and recall@k against exact search.
//!
//! Run in release mode:
//!     cargo run --release -p tpt-search-vector --example scale -- 1000000 128
//!
//! Arguments: number of vectors (default 100000), dimension (default 128),
//! mode "uniform" (default) or "clustered".

use std::time::Instant;

use tpt_search_vector::{distance, DistanceMetric, HnswConfig, HnswIndex};

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100_000);
    let dim: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(128);
    let queries = 200usize;
    let mode = args.next().unwrap_or_else(|| "uniform".to_owned());
    let k = 10usize;

    println!("kernel: {:?}", tpt_search_vector::active_kernel());
    println!("generating {n} {mode} vectors of dimension {dim}");
    let mut state = 0x1234_5678_9ABC_DEF1u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % 2_000_000) as f32 / 1_000_000.0 - 1.0
    };
    // "uniform": i.i.d. components, the hard case (no low-dimensional structure).
    // "clustered": points near 1000 centres, closer to real embedding data.
    let centres: Vec<Vec<f32>> = (0..1000)
        .map(|_| (0..dim).map(|_| next()).collect())
        .collect();
    let sample = |next: &mut dyn FnMut() -> f32| -> Vec<f32> {
        if mode == "clustered" {
            let c = ((next() + 1.0) * 500.0) as usize % centres.len();
            (0..dim).map(|d| centres[c][d] + next() * 0.15).collect()
        } else {
            (0..dim).map(|_| next()).collect()
        }
    };
    let data: Vec<Vec<f32>> = (0..n).map(|_| sample(&mut next)).collect();
    let query_set: Vec<Vec<f32>> = (0..queries).map(|_| sample(&mut next)).collect();

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

    let efs = [100usize, 200, 400];
    let mut latencies: Vec<Vec<f64>> = vec![Vec::with_capacity(queries); efs.len()];
    let mut recall_hits = vec![0usize; efs.len()];
    for q in &query_set {
        // Exact ground truth, computed once per query and shared by all beam widths.
        let mut exact: Vec<(f32, u64)> = data
            .iter()
            .enumerate()
            .map(|(i, v)| (distance(DistanceMetric::Cosine, q, v), i as u64))
            .collect();
        exact.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Tie-aware: a result is correct if it is no farther than the k-th exact neighbour.
        let kth = exact[k - 1].0 + 1e-5;

        for (slot, &ef) in efs.iter().enumerate() {
            let t = Instant::now();
            let found = index.search(q, k, ef).expect("search");
            latencies[slot].push(t.elapsed().as_secs_f64() * 1000.0);
            recall_hits[slot] += found.iter().filter(|h| h.distance <= kth).count();
        }
    }
    for (slot, &ef) in efs.iter().enumerate() {
        let lat = &mut latencies[slot];
        lat.sort_by(|a, b| a.total_cmp(b));
        let p = |q: f64| lat[((lat.len() as f64 * q) as usize).min(lat.len() - 1)];
        println!(
            "ef={ef:>4}: recall@{k} {:.4}  latency p50 {:.2} ms  p95 {:.2} ms  p99 {:.2} ms",
            recall_hits[slot] as f64 / (queries * k) as f64,
            p(0.50),
            p(0.95),
            p(0.99)
        );
    }
}
