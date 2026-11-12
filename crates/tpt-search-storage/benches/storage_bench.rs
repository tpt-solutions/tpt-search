//! WAL append throughput and segment merge cost.

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use tpt_search_core::{Analyzer, Document, DocumentId, FieldSchema, FieldType, Schema};
use tpt_search_storage::{MergePolicy, SegmentManager, WalEntry, WriteAheadLog};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tpt-bench-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn schema() -> Schema {
    Schema::new("bench", "id")
        .with_field(FieldSchema::new("id", FieldType::Text))
        .unwrap()
        .with_field(
            FieldSchema::new("body", FieldType::Text)
                .indexed()
                .with_analyzer(Analyzer::standard()),
        )
        .unwrap()
}

fn docs(n: usize, offset: usize) -> Vec<Document> {
    (0..n)
        .map(|i| {
            Document::new(format!("d{}", offset + i)).with_field(
                "body",
                format!("document number {} about rust search engines", i),
            )
        })
        .collect()
}

fn bench_wal(c: &mut Criterion) {
    let dir = scratch("wal");
    let mut wal = WriteAheadLog::open(&dir.join("wal.log")).unwrap();
    let entry = WalEntry::Delete(DocumentId::from("some-document-id"));

    let mut group = c.benchmark_group("wal");
    group.throughput(Throughput::Elements(1));
    // Each append fsyncs, so this measures durable-write latency.
    group.sample_size(20);
    group.bench_function("append_fsync", |b| {
        b.iter(|| wal.append(black_box(entry.clone())).unwrap())
    });
    group.finish();
}

fn bench_merge(c: &mut Criterion) {
    let mut group = c.benchmark_group("segment_merge");
    group.sample_size(10);
    group.bench_function("merge_8_segments_x_1k_docs", |b| {
        b.iter_batched(
            || {
                let dir = scratch("merge");
                let policy = MergePolicy::LogStructured {
                    target_size: usize::MAX,
                };
                let mut mgr = SegmentManager::open(&dir, schema(), policy).unwrap();
                for s in 0..8 {
                    mgr.ingest(&docs(1_000, s * 1_000), 1_000).unwrap();
                }
                mgr
            },
            |mut mgr| {
                let ids: Vec<_> = mgr.segments().iter().map(|s| s.id).collect();
                black_box(mgr.merge_segments(&ids).unwrap())
            },
            criterion::BatchSize::PerIteration,
        )
    });
    group.finish();
}

criterion_group!(benches, bench_wal, bench_merge);
criterion_main!(benches);
