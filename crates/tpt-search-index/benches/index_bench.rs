//! Indexing throughput and query latency for the scalar/full-text layer.

use std::hint::black_box;
use std::ops::Bound;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use tpt_search_core::{Analyzer, Document, FieldSchema, FieldType, FieldValue, Schema};
use tpt_search_index::{BooleanQuery, Collection, PhraseQuery, Predicate, TermQuery};

const WORDS: &[&str] = &[
    "rust",
    "search",
    "engine",
    "index",
    "vector",
    "graph",
    "query",
    "planner",
    "storage",
    "segment",
    "merge",
    "posting",
    "ranking",
    "bm25",
    "phrase",
    "boolean",
    "range",
    "column",
    "aggregate",
    "snapshot",
    "replica",
    "shard",
    "latency",
    "throughput",
    "memory",
    "disk",
];

fn schema() -> Schema {
    Schema::new("bench", "id")
        .with_field(FieldSchema::new("id", FieldType::Text))
        .unwrap()
        .with_field(
            FieldSchema::new("body", FieldType::Text)
                .indexed()
                .with_analyzer(Analyzer::english()),
        )
        .unwrap()
        .with_field(FieldSchema::new("year", FieldType::Integer).indexed())
        .unwrap()
}

/// Deterministic pseudo-random corpus (no RNG dependency needed).
fn corpus(n: usize) -> Vec<Document> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..n)
        .map(|i| {
            let len = 20 + (next() % 30) as usize;
            let body: Vec<&str> = (0..len)
                .map(|_| WORDS[(next() % WORDS.len() as u64) as usize])
                .collect();
            Document::new(format!("d{i}"))
                .with_field("body", body.join(" "))
                .with_field("year", FieldValue::Integer(2000 + (next() % 25) as i64))
        })
        .collect()
}

fn populated(n: usize) -> Collection {
    let mut collection = Collection::new(schema()).unwrap();
    for doc in corpus(n) {
        collection.upsert(&doc).unwrap();
    }
    collection
}

fn bench_indexing(c: &mut Criterion) {
    let docs = corpus(5_000);
    let mut group = c.benchmark_group("index_throughput");
    group.throughput(Throughput::Elements(docs.len() as u64));
    group.sample_size(10);
    group.bench_function("upsert_5k_docs", |b| {
        b.iter(|| {
            let mut collection = Collection::new(schema()).unwrap();
            for doc in &docs {
                collection.upsert(black_box(doc)).unwrap();
            }
            collection
        })
    });
    group.finish();
}

fn bench_queries(c: &mut Criterion) {
    let collection = populated(50_000);
    let term = TermQuery {
        field: "body".into(),
        text: "rust".into(),
    };
    let phrase = PhraseQuery {
        field: "body".into(),
        text: "vector graph query".into(),
    };
    let boolean = BooleanQuery {
        must: vec![term.clone()],
        should: vec![],
        must_not: vec![TermQuery {
            field: "body".into(),
            text: "disk".into(),
        }],
    };
    let range = Predicate::Range {
        field: "year".into(),
        start: Bound::Included(FieldValue::Integer(2010)),
        end: Bound::Excluded(FieldValue::Integer(2015)),
    };

    let mut group = c.benchmark_group("query_latency_50k");
    group.sample_size(20);
    group.bench_function("term", |b| {
        b.iter(|| collection.search(black_box(&term)).unwrap())
    });
    group.bench_function("phrase", |b| {
        b.iter(|| collection.phrase_search(black_box(&phrase)).unwrap())
    });
    group.bench_function("boolean", |b| {
        b.iter(|| collection.boolean_search(black_box(&boolean)).unwrap())
    });
    group.bench_function("range_indexed", |b| {
        b.iter(|| collection.scan(black_box(&range)).unwrap())
    });
    group.finish();
}

criterion_group!(benches, bench_indexing, bench_queries);
criterion_main!(benches);
