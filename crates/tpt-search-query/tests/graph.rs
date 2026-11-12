//! Graph patterns and graph inputs to hybrid queries, through the full
//! parse, plan, and execute path.

use tpt_search_core::{
    Analyzer, Document, FieldSchema, FieldType, QueryError, Schema, SearchError,
};
use tpt_search_query::{parse_query, IndexRegistry, QueryExecutor, QueryPlanner};

fn schema() -> Schema {
    Schema::new("docs", "id")
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
        .with_field(FieldSchema::new("embedding", FieldType::Vector(2)).indexed())
        .unwrap()
}

fn doc(id: &str, title: &str, year: i64, embedding: [f32; 2]) -> Document {
    Document::new(id)
        .with_field("title", title)
        .with_field("year", year)
        .with_field("embedding", embedding.to_vec())
}

/// Four documents and a citation graph: d2 cites d1, d3 cites d2, d4 cites d1 and d2.
fn registry() -> IndexRegistry {
    let mut registry = IndexRegistry::new();
    registry.create("docs", schema()).unwrap();
    let c = registry.get_mut("docs").unwrap();
    c.upsert(&doc("d1", "rust search engine", 2022, [1.0, 0.0]))
        .unwrap();
    c.upsert(&doc("d2", "vector search tutorial", 2019, [0.9, 0.1]))
        .unwrap();
    c.upsert(&doc("d3", "cooking pasta", 2023, [0.0, 1.0]))
        .unwrap();
    c.upsert(&doc("d4", "rust vector database", 2024, [0.7, 0.7]))
        .unwrap();

    let g = registry.graph_mut("docs").unwrap();
    for id in ["d1", "d2", "d3", "d4"] {
        g.add_labelled_node(id, &["Doc"]).unwrap();
    }
    use std::collections::HashMap;
    g.add_edge("d2", "d1", "CITES", HashMap::new()).unwrap();
    g.add_edge("d3", "d2", "CITES", HashMap::new()).unwrap();
    g.add_edge("d4", "d1", "CITES", HashMap::new()).unwrap();
    g.add_edge("d4", "d2", "CITES", HashMap::new()).unwrap();
    registry
}

/// Runs a query and returns document ids in result order.
fn run(registry: &IndexRegistry, text: &str) -> Result<Vec<String>, SearchError> {
    let query = parse_query(text)?;
    let plan = QueryPlanner::new().plan("docs", registry.get("docs").unwrap(), &query)?;
    let result = QueryExecutor::new(registry).execute(&plan)?;
    Ok(result
        .documents
        .iter()
        .map(|d| d.id.as_str().to_owned())
        .collect())
}

const CITED: &str = r#"GRAPH("MATCH (a)-[:CITES]->(b) RETURN b")"#;

#[test]
fn graph_query_joins_matched_nodes_to_documents_by_score() {
    let reg = registry();
    // d1 is cited twice (by d2 and d4), d2 is cited twice (by d3 and d4).
    // Ties keep row order, so d1 comes before d2.
    assert_eq!(run(&reg, CITED).unwrap(), ["d1", "d2"]);
}

#[test]
fn variable_length_graph_patterns() {
    let reg = registry();
    // Exactly two hops: d3 -> d2 -> d1 and d4 -> d2 -> d1 both end at d1.
    let ids = run(&reg, r#"GRAPH("MATCH (a:Doc)-[:CITES*2]->(b) RETURN b")"#).unwrap();
    assert_eq!(ids, ["d1"]);
    // One or two hops: d1 is reached from three starts, d2 from two.
    let ids = run(
        &reg,
        r#"GRAPH("MATCH (a:Doc)-[:CITES*1..2]->(b) RETURN b")"#,
    )
    .unwrap();
    assert_eq!(ids, ["d1", "d2"]);
}

#[test]
fn graph_match_composes_with_scalar_filters_in_hybrid_queries() {
    let reg = registry();
    // Scalar: "rust" matches d1 and d4. Graph: d1 and d2 are cited. Both d1 and d2
    // count, and d1 is in both inputs, so it ranks first.
    let text =
        format!(r#"HYBRID(scalar: MATCH(title, "rust"), graph: {CITED}, fusion: LINEAR(1, 1))"#);
    let ids = run(&reg, &text).unwrap();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(sorted, ["d1", "d2", "d4"]);
    assert_eq!(ids[0], "d1");
}

#[test]
fn interleaving_alternates_between_inputs() {
    let reg = registry();
    // Vector top-2 for [0, 1]: d3, then d4. Graph ranking: d1, then d2.
    let text = format!(
        r#"HYBRID(vector: KNN(embedding, [0, 1], k=2), graph: {CITED}, fusion: INTERLEAVE)"#
    );
    assert_eq!(run(&reg, &text).unwrap(), ["d3", "d1", "d4", "d2"]);
}

#[test]
fn linear_combination_weights_the_inputs() {
    let reg = registry();
    // Weight the vector input heavily: d3 (closest to [0, 1]) should win.
    let text = format!(
        r#"HYBRID(vector: KNN(embedding, [0, 1], k=2), graph: {CITED}, fusion: LINEAR(10, 1))"#
    );
    let ids = run(&reg, &text).unwrap();
    assert_eq!(ids[0], "d3");
}

#[test]
fn linear_weights_must_match_inputs() {
    let reg = registry();
    let text = format!(r#"HYBRID(scalar: year >= 2000, graph: {CITED}, fusion: LINEAR(1, 2, 3))"#);
    let query = parse_query(&text).unwrap();
    let err = QueryPlanner::new().plan("docs", reg.get("docs").unwrap(), &query);
    assert!(matches!(err, Err(QueryError::ParseError(_))));
}

#[test]
fn graph_syntax_errors_are_parse_errors() {
    assert!(matches!(
        parse_query(r#"GRAPH("MATCH (a RETURN a")"#),
        Err(QueryError::ParseError(_))
    ));
    assert!(matches!(
        parse_query(r#"GRAPH("MATCH (a) RETURN zz")"#),
        Err(QueryError::ParseError(_))
    ));
    assert!(
        parse_query("HYBRID(graph: year >= 1)").is_err(),
        "graph input must be GRAPH(...)"
    );
}

#[test]
fn graph_without_matches_returns_nothing() {
    let reg = registry();
    let ids = run(&reg, r#"GRAPH("MATCH (a)-[:WROTE]->(b) RETURN b")"#).unwrap();
    assert!(ids.is_empty());
}

#[test]
fn three_way_hybrid_fuses_scalar_vector_and_graph_inputs() {
    let reg = registry();
    // Scalar: d1, d4 ("rust"). Vector top-2 for [0, 1]: d3, d4. Graph: d1, d2 (cited).
    // Every document is in at least one input, so all four appear.
    let text = format!(
        r#"HYBRID(scalar: MATCH(title, "rust"), vector: KNN(embedding, [0, 1], k=2), graph: {CITED}, fusion: RRF(k=60))"#
    );
    let ids = run(&reg, &text).unwrap();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(sorted, ["d1", "d2", "d3", "d4"]);
    // d4 is in the scalar and vector inputs, and d1 is in the scalar and graph inputs,
    // so both outrank d2 and d3, which each appear in one input only.
    assert!(
        ids.iter().position(|d| d == "d1").unwrap() < ids.iter().position(|d| d == "d2").unwrap()
    );
}
