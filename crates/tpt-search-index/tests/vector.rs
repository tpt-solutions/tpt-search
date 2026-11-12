use tpt_search_core::{
    DistanceMetric, Document, DocumentId, FieldSchema, FieldType, IndexError, Schema, VectorConfig,
};
use tpt_search_index::Collection;

fn schema() -> Schema {
    Schema::new("items", "id")
        .with_field(FieldSchema::new("id", FieldType::Text))
        .unwrap()
        .with_field(FieldSchema::new("name", FieldType::Text))
        .unwrap()
        .with_field(FieldSchema {
            vector_config: Some(VectorConfig {
                dimension: 3,
                metric: DistanceMetric::Euclidean,
            }),
            ..FieldSchema::new("embedding", FieldType::Vector(3)).indexed()
        })
        .unwrap()
}

fn item(id: &str, v: [f32; 3]) -> Document {
    Document::new(id)
        .with_field("name", id)
        .with_field("embedding", v.to_vec())
}

fn collection() -> Collection {
    let mut c = Collection::new(schema()).unwrap();
    c.upsert(&item("a", [0.0, 0.0, 0.0])).unwrap();
    c.upsert(&item("b", [1.0, 0.0, 0.0])).unwrap();
    c.upsert(&item("c", [5.0, 5.0, 5.0])).unwrap();
    c.upsert(&item("d", [0.5, 0.0, 0.0])).unwrap();
    c
}

fn ids(hits: &[tpt_search_index::ScoredDocument]) -> Vec<String> {
    hits.iter().map(|h| h.doc_id.as_str().to_owned()).collect()
}

#[test]
fn knn_returns_nearest_documents_closest_first() {
    let c = collection();
    let hits = c.knn("embedding", &[0.9, 0.0, 0.0], 2, None).unwrap();
    assert_eq!(ids(&hits), ["b", "d"]);
    assert!(hits[0].score >= hits[1].score);
}

#[test]
fn deleted_and_superseded_documents_are_not_returned() {
    let mut c = collection();
    c.delete(&DocumentId::from("b"));
    c.upsert(&item("d", [9.0, 9.0, 9.0])).unwrap();
    // Query where the deleted "b" used to be, and where "d" now is.
    let near_b = c.knn("embedding", &[1.0, 0.0, 0.0], 3, Some(50)).unwrap();
    assert!(!ids(&near_b).contains(&"b".to_owned()), "deleted doc returned");
    let near_old_d = c.knn("embedding", &[0.5, 0.0, 0.0], 4, Some(50)).unwrap();
    let d_hits: Vec<_> = near_old_d.iter().filter(|h| h.doc_id.as_str() == "d").collect();
    assert!(d_hits.len() <= 1 && d_hits.iter().all(|h| h.row == c.row_of(&DocumentId::from("d")).unwrap()), "superseded row returned");
    let near_d = c.knn("embedding", &[9.0, 9.0, 9.0], 2, Some(50)).unwrap();
    assert_eq!(ids(&near_d), ["d", "c"]);
    assert_eq!(near_d[0].row, c.row_of(&DocumentId::from("d")).unwrap());
}

#[test]
fn invalid_vectors_leave_collection_unchanged() {
    let mut c = collection();
    let before = c.len();
    let nan = item("x", [f32::NAN, 0.0, 0.0]);
    assert!(matches!(c.upsert(&nan), Err(IndexError::InvalidValue(_))));
    assert_eq!(c.len(), before);
    assert!(c.get(&DocumentId::from("x")).is_none());
    // Wrong dimension is caught by schema validation before indexing.
    let short = Document::new("y").with_field("embedding", vec![1.0f32, 2.0]);
    assert!(c.upsert(&short).is_err());
    assert_eq!(c.len(), before);
}

#[test]
fn knn_rejects_non_vector_fields_and_bad_queries() {
    let c = collection();
    assert!(matches!(
        c.knn("name", &[0.0, 0.0, 0.0], 1, None),
        Err(IndexError::Unsupported { .. })
    ));
    assert!(matches!(
        c.knn("missing", &[0.0, 0.0, 0.0], 1, None),
        Err(IndexError::UnknownField(_))
    ));
    assert!(matches!(
        c.knn("embedding", &[0.0, 0.0], 1, None),
        Err(IndexError::DimensionMismatch { .. })
    ));
}

#[test]
fn documents_without_a_vector_are_never_returned() {
    let mut c = collection();
    c.upsert(&Document::new("no-vector").with_field("name", "plain")).unwrap();
    let hits = c.knn("embedding", &[0.0, 0.0, 0.0], 10, Some(50)).unwrap();
    assert!(!ids(&hits).contains(&"no-vector".to_owned()));
    assert_eq!(hits.len(), 4);
}
