//! Phase 1 milestone: index and search documents with full-text, range, and
//! boolean queries, including a persistence round trip through the storage layer.

use std::time::Duration;

use tpt_search_core::{
    Analyzer, Document, FieldSchema, FieldType, QueryError, Schema, SearchError,
};
use tpt_search_query::{parse_query, IndexRegistry, QueryExecutor, QueryPlan, QueryPlanner};
use tpt_search_storage::{MergePolicy, SegmentManager};

fn schema() -> Schema {
    Schema::new("papers", "id")
        .with_field(FieldSchema::new("id", FieldType::Text))
        .unwrap()
        .with_field(
            FieldSchema::new("title", FieldType::Text)
                .indexed()
                .with_analyzer(Analyzer::english()),
        )
        .unwrap()
        .with_field(FieldSchema::new("year", FieldType::Integer).indexed())
        .unwrap()
        .with_field(FieldSchema::new("citations", FieldType::Integer))
        .unwrap()
        .with_field(FieldSchema::new("open_access", FieldType::Boolean).indexed())
        .unwrap()
}

fn corpus() -> Vec<Document> {
    vec![
        Document::new("p1")
            .with_field("title", "Rust programming for systems engineering")
            .with_field("year", 2021i64)
            .with_field("citations", 40i64)
            .with_field("open_access", true),
        Document::new("p2")
            .with_field("title", "Programming language theory")
            .with_field("year", 2019i64)
            .with_field("citations", 12i64)
            .with_field("open_access", false),
        Document::new("p3")
            .with_field("title", "Rust in embedded programming")
            .with_field("year", 2023i64)
            .with_field("citations", 3i64)
            .with_field("open_access", true),
        Document::new("p4")
            .with_field("title", "Cooking with cast iron")
            .with_field("year", 2024i64)
            .with_field("citations", 0i64)
            .with_field("open_access", true),
    ]
}

fn run(registry: &IndexRegistry, text: &str) -> Result<Vec<String>, SearchError> {
    let query = parse_query(text)?;
    let collection = registry.get("papers").unwrap();
    let plan = QueryPlanner::new().plan("papers", collection, &query)?;
    let result = QueryExecutor::new(registry).execute(&plan)?;
    let mut ids: Vec<String> = result
        .documents
        .iter()
        .map(|d| d.id.as_str().to_owned())
        .collect();
    ids.sort();
    Ok(ids)
}

fn registry_from(docs: &[Document]) -> IndexRegistry {
    let mut registry = IndexRegistry::new();
    registry.create("papers", schema()).unwrap();
    let collection = registry.get_mut("papers").unwrap();
    for doc in docs {
        collection.upsert(doc).unwrap();
    }
    registry
}

#[test]
fn full_text_range_and_boolean_queries() {
    let registry = registry_from(&corpus());

    // Stemming makes "programming" match "programming" across all three titles.
    assert_eq!(
        run(&registry, r#"MATCH(title, "programming")"#).unwrap(),
        ["p1", "p2", "p3"]
    );
    // Phrases respect word order.
    assert_eq!(
        run(&registry, r#"PHRASE(title, "rust programming")"#).unwrap(),
        ["p1"]
    );
    assert!(run(&registry, r#"PHRASE(title, "programming rust")"#)
        .unwrap()
        .is_empty());

    // Range predicates.
    assert_eq!(run(&registry, "year >= 2021").unwrap(), ["p1", "p3", "p4"]);
    assert_eq!(run(&registry, "citations < 10").unwrap(), ["p3", "p4"]);

    // Boolean combinations.
    assert_eq!(
        run(&registry, r#"MATCH(title, "rust") AND year >= 2020"#).unwrap(),
        ["p1", "p3"]
    );
    assert_eq!(
        run(
            &registry,
            r#"MATCH(title, "programming") AND NOT MATCH(title, "embedded")"#
        )
        .unwrap(),
        ["p1", "p2"]
    );
    assert_eq!(
        run(
            &registry,
            "open_access = true AND (year < 2020 OR citations = 3)"
        )
        .unwrap(),
        ["p3"]
    );
    assert_eq!(run(&registry, "NOT open_access = true").unwrap(), ["p2"]);
}

#[test]
fn results_are_scored_and_highlighted() {
    let registry = registry_from(&corpus());
    let query = parse_query(r#"MATCH(title, "rust")"#).unwrap();
    let plan = QueryPlanner::new()
        .plan("papers", registry.get("papers").unwrap(), &query)
        .unwrap();
    let result = QueryExecutor::new(&registry).execute(&plan).unwrap();
    assert_eq!(result.total_hits, 2);
    let spans = &result.highlights[&result.documents[0].id];
    assert_eq!(spans[0].field, "title");
    assert!(!spans[0].positions.is_empty());
}

#[test]
fn limits_and_errors_are_reported() {
    let registry = registry_from(&corpus());
    let collection = registry.get("papers").unwrap();

    let query = parse_query("year >= 2000").unwrap();
    let plan = QueryPlanner::new()
        .plan("papers", collection, &query)
        .unwrap()
        .with_limit(2);
    let result = QueryExecutor::new(&registry).execute(&plan).unwrap();
    assert_eq!(result.total_hits, 4);
    assert_eq!(result.documents.len(), 2);

    let bad = parse_query("nope = 1").unwrap();
    let err = QueryPlanner::new().plan("papers", collection, &bad);
    assert!(matches!(err, Err(QueryError::InvalidField(f)) if f == "nope"));

    let plan = QueryPlanner::new()
        .plan("papers", collection, &query)
        .unwrap();
    let unknown = QueryExecutor::new(&registry).execute(&QueryPlan {
        index: "other".into(),
        ..plan.clone()
    });
    assert!(matches!(unknown, Err(QueryError::UnknownIndex(_))));

    let limited = QueryExecutor::new(&registry)
        .with_max_candidates(1)
        .execute(&plan);
    assert!(matches!(limited, Err(QueryError::ResourceExhausted(_))));

    let timed = QueryExecutor::new(&registry)
        .with_timeout(Duration::ZERO)
        .execute(&plan);
    assert!(matches!(timed, Err(QueryError::Timeout)));
}

#[test]
fn survives_persistence_round_trip() {
    let dir = std::env::temp_dir().join(format!("tpt-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let policy = MergePolicy::LogStructured { target_size: 1000 };

    {
        let mut mgr = SegmentManager::open(&dir, schema(), policy).unwrap();
        mgr.ingest(&corpus()[..2], 10).unwrap();
        mgr.ingest(&corpus()[2..], 10).unwrap();
        // Update p2 in a later ingest: the old copy must not return after reload.
        mgr.ingest(
            &[Document::new("p2")
                .with_field("title", "Programming language theory, revised")
                .with_field("year", 2025i64)
                .with_field("citations", 99i64)
                .with_field("open_access", true)],
            10,
        )
        .unwrap();
        mgr.maybe_merge().unwrap();
        mgr.flush().unwrap();
    }

    let reopened = SegmentManager::open(&dir, schema(), policy).unwrap();
    let mut docs: Vec<Document> = Vec::new();
    for segment in reopened.segments() {
        docs.extend(segment.collection.live_sources().into_iter().cloned());
    }
    assert_eq!(docs.len(), 4);

    let registry = registry_from(&docs);
    assert_eq!(run(&registry, "year >= 2025").unwrap(), ["p2"]);
    assert_eq!(
        run(&registry, r#"MATCH(title, "revised")"#).unwrap(),
        ["p2"]
    );
    assert!(run(&registry, "citations = 12").unwrap().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}
