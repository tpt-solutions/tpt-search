//! Distance kernels and HNSW build/query performance.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use tpt_search_vector::{distance, DistanceMetric, HnswConfig, HnswIndex};

fn vector(dim: usize, seed: u64) -> Vec<f32> {
    let mut state = seed | 1;
    (0..dim)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % 2_000_000) as f32 / 1_000_000.0 - 1.0
        })
        .collect()
}

fn bench_kernels(c: &mut Criterion) {
    let mut group = c.benchmark_group("distance_kernels");
    for dim in [128usize, 768] {
        let a = vector(dim, 1);
        let b = vector(dim, 2);
        group.throughput(Throughput::Elements(dim as u64));
        group.bench_with_input(BenchmarkId::new("dot", dim), &dim, |bench, _| {
            bench.iter(|| tpt_search_vector::distance::dot(black_box(&a), black_box(&b)))
        });
        group.bench_with_input(BenchmarkId::new("squared_l2", dim), &dim, |bench, _| {
            bench.iter(|| tpt_search_vector::distance::squared_l2(black_box(&a), black_box(&b)))
        });
        group.bench_with_input(
            BenchmarkId::new("cosine_distance", dim),
            &dim,
            |bench, _| {
                bench.iter(|| distance(DistanceMetric::Cosine, black_box(&a), black_box(&b)))
            },
        );
    }
    group.finish();
}

fn bench_hnsw(c: &mut Criterion) {
    let dim = 128;
    let n = 20_000u64;
    let data: Vec<Vec<f32>> = (0..n).map(|i| vector(dim, i + 11)).collect();

    let mut group = c.benchmark_group("hnsw");
    group.sample_size(10);
    group.bench_function("build_20k_x128", |b| {
        b.iter(|| {
            let mut index = HnswIndex::new(HnswConfig::default()).unwrap();
            for (i, v) in data.iter().enumerate() {
                index.insert(i as u64, v.clone()).unwrap();
            }
            black_box(index.len())
        })
    });

    let mut index = HnswIndex::new(HnswConfig::default()).unwrap();
    for (i, v) in data.iter().enumerate() {
        index.insert(i as u64, v.clone()).unwrap();
    }
    let queries: Vec<Vec<f32>> = (0..100).map(|i| vector(dim, 90_000 + i)).collect();
    group.sample_size(50);
    group.bench_function("search_k10_ef100_20k", |b| {
        let mut i = 0usize;
        b.iter(|| {
            i = (i + 1) % queries.len();
            index.search(black_box(&queries[i]), 10, 100).unwrap()
        })
    });
    group.finish();
}

criterion_group!(benches, bench_kernels, bench_hnsw);
criterion_main!(benches);
