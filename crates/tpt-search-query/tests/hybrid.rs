//! Vector (KNN) queries and hybrid scalar + vector fusion through the full
//! parse, plan, and execute path.

use tpt_search_core::{Analyzer, Document, FieldSchema, FieldType, QueryError, Schema};
use tpt_search_query::{parse_query, IndexRegistry, PlanNode, Query, QueryExecutor, QueryPlanner};
use tpt_search_query::ast::FusionMethod;

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

fn registry() -> IndexRegistry {
    let mut registry = IndexRegistry::new();
    registry.create("docs", schema()).unwrap();
    let c = registry.get_mut("docs").unwrap();
    c.upsert(&doc("d1", "rust search engine", 2022, [1.0, 0.0])).unwrap();
    c.upsert(&doc("d2", "vector search tutorial", 2019, [0.9, 0.1])).unwrap();
    c.upsert(&doc("d3", "cooking pasta", 2023, [0.0, 1.0])).unwrap();
    c.upsert(&doc("d4", "rust vector database", 2024, [0.7, 0.7])).unwrap();
    registry
}

/// Runs a query and returns document ids in result order.
fn run(registry: &IndexRegistry, text: &str) -> Result<Vec<String>, tpt_search_core::SearchError> {
    let query = parse_query(text)?;
    let plan = QueryPlanner::new().plan("docs", registry.get("docs").unwrap(), &query)?;
    let result = QueryExecutor::new(registry).execute(&plan)?;
    Ok(result.documents.iter().map(|d| d.id.as_str().to_owned()).collect())
}

#[test]
fn parses_knn_with_optional_ef() {
    assert_eq!(
        parse_query("KNN(embedding, [0.5, -1, 2e1], k=3, ef=40)").unwrap(),
        Query::Knn {
            field: "embedding".into(),
            vector: vec![0.5, -1.0, 20.0],
            k: 3,
            ef: Some(40),
        }
    );
    assert!(matches!(
        parse_query("KNN(embedding, [1.0])"),
        Err(QueryError::ParseError(_))
    ), "k is required");
}

#[test]
fn parses_hybrid_with_named_arguments() {
    let q = parse_query(
        r#"HYBRID(scalar: MATCH(title, "rust") AND year >= 2020, vector: KNN(embedding, [1, 0], k=5), fusion: RRF(k=10))"#,
    )
    .unwrap();
    match q {
        Query::Hybrid {
            scalar: Some(_),
            vector: Some(v),
            fusion,
        } => {
            assert!(matches!(*v, Query::Knn { k: 5, .. }));
            assert_eq!(fusion, FusionMethod::ReciprocalRankFusion { k: 10 });
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(matches!(
        parse_query("HYBRID(scalar: year >= 1)").unwrap(),
        Query::Hybrid { vector: None, .. }
    ));
    assert!(parse_query("HYBRID(vector: MATCH(title, \"x\"))").is_err(), "vector must be KNN");
    assert!(parse_query("HYBRID()").is_err());
}

#[test]
fn knn_returns_nearest_by_cosine() {
    let reg = registry();
    assert_eq!(run(&reg, "KNN(embedding, [1, 0], k=2)").unwrap(), ["d1", "d2"]);
    assert_eq!(run(&reg, "KNN(embedding, [0, 1], k=1)").unwrap(), ["d3"]);
}

#[test]
fn knn_plan_uses_vector_search_node() {
    let reg = registry();
    let query = parse_query("KNN(embedding, [1, 0], k=2)").unwrap();
    let plan = QueryPlanner::new().plan("docs", reg.get("docs").unwrap(), &query).unwrap();
    match &plan.root {
        PlanNode::Sort { input, .. } => {
            assert!(matches!(**input, PlanNode::VectorSearch { k: 2, .. }));
        }
        other => panic!("unexpected root {other:?}"),
    }
}

#[test]
fn hybrid_fusion_ranks_documents_that_score_well_in_both_inputs_first() {
    let reg = registry();
    // Scalar: "rust" matches d1 and d4. Vector: d1, then d2, then d4.
    // RRF (k=60): d1 ranks high in both lists; d4 is in both lists too, so it
    // beats d2, which only appears in the vector list.
    let ids = run(
        &reg,
        r#"HYBRID(scalar: MATCH(title, "rust"), vector: KNN(embedding, [1, 0], k=3))"#,
    )
    .unwrap();
    assert_eq!(ids, ["d1", "d4", "d2"]);
}

#[test]
fn hybrid_with_only_a_scalar_input_matches_the_scalar_query() {
    let reg = registry();
    let mut hybrid = run(&reg, "HYBRID(scalar: year >= 2022)").unwrap();
    let mut scalar = run(&reg, "year >= 2022").unwrap();
    hybrid.sort();
    scalar.sort();
    assert_eq!(hybrid, scalar);
}

#[test]
fn knn_errors_are_reported() {
    let reg = registry();
    let collection = reg.get("docs").unwrap();
    let plan_err = |text: &str| {
        let q = parse_query(text).unwrap();
        QueryPlanner::new().plan("docs", collection, &q)
    };
    assert!(matches!(
        plan_err("KNN(embedding, [1, 0, 0], k=1)"),
        Err(QueryError::TypeMismatch { .. })
    ));
    assert!(matches!(
        plan_err("KNN(title, [1, 0], k=1)"),
        Err(QueryError::InvalidField(_))
    ));
    assert!(matches!(
        plan_err("KNN(missing, [1, 0], k=1)"),
        Err(QueryError::InvalidField(_))
    ));
}
