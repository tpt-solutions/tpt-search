use std::ops::Bound;

use tpt_search_core::{
    Analyzer, Document, DocumentId, FieldSchema, FieldType, FieldValue, IndexError, Schema,
};
use tpt_search_index::{
    Aggregation, AggregationResult, BooleanQuery, Collection, IndexKey, PhraseQuery, Predicate,
    TermQuery,
};

fn schema() -> Schema {
    Schema::new("docs", "id")
        .with_field(FieldSchema::new("id", FieldType::Text))
        .unwrap()
        .with_field(
            FieldSchema::new("body", FieldType::Text)
                .indexed()
                .with_analyzer(Analyzer::standard()),
        )
        .unwrap()
        .with_field(FieldSchema::new("year", FieldType::Integer).indexed())
        .unwrap()
        // Deliberately not indexed: scans must fall back to the column.
        .with_field(FieldSchema::new("score", FieldType::Float))
        .unwrap()
        .with_field(FieldSchema::new("live", FieldType::Boolean))
        .unwrap()
}

fn doc(id: &str, body: &str, year: i64, score: f64, live: bool) -> Document {
    Document::new(id)
        .with_field("body", body)
        .with_field("year", year)
        .with_field("score", score)
        .with_field("live", live)
}

fn populated() -> Collection {
    let mut c = Collection::new(schema()).unwrap();
    c.upsert(&doc("a", "the quick brown fox", 2018, 1.5, true))
        .unwrap();
    c.upsert(&doc("b", "the lazy dog sleeps", 2020, 4.0, false))
        .unwrap();
    c.upsert(&doc("c", "a quick dog runs", 2024, 2.5, true))
        .unwrap();
    c
}

fn ids(hits: &[tpt_search_index::ScoredDocument]) -> Vec<&str> {
    let mut v: Vec<&str> = hits.iter().map(|h| h.doc_id.as_str()).collect();
    v.sort_unstable();
    v
}

#[test]
fn invalid_schema_is_rejected_at_construction() {
    let bad = Schema::new("x", "missing");
    assert!(Collection::new(bad).is_err());
}

#[test]
fn unknown_fields_and_wrong_types_are_rejected_without_mutation() {
    let mut c = populated();
    let before = c.len();
    let bogus = Document::new("z").with_field("nope", 1i64);
    assert!(c.upsert(&bogus).is_err());
    let wrong = Document::new("z").with_field("year", "not a number");
    assert!(c.upsert(&wrong).is_err());
    assert_eq!(c.len(), before);
    assert!(c.get(&DocumentId::from("z")).is_none());
    // The rejected document must not have left rows behind that could be mistaken
    // for live ones.
    assert_eq!(c.live_rows().len() as usize, before);
}

#[test]
fn full_text_term_search_scores_and_highlights() {
    let c = populated();
    let hits = c
        .search(&TermQuery {
            field: "body".into(),
            text: "quick".into(),
        })
        .unwrap();
    assert_eq!(ids(&hits), ["a", "c"]);
    let a = hits.iter().find(|h| h.doc_id.as_str() == "a").unwrap();
    assert_eq!(a.highlights[0].positions, [(4, 9)]);
    assert!(a.score > 0.0);
}

#[test]
fn phrase_search_requires_adjacent_terms() {
    let c = populated();
    let hit = c
        .phrase_search(&PhraseQuery {
            field: "body".into(),
            text: "quick brown".into(),
        })
        .unwrap();
    assert_eq!(ids(&hit), ["a"]);
    let miss = c
        .phrase_search(&PhraseQuery {
            field: "body".into(),
            text: "brown quick".into(),
        })
        .unwrap();
    assert!(miss.is_empty());
}

#[test]
fn boolean_search_applies_must_should_and_must_not() {
    let c = populated();
    let q = |text: &str| TermQuery {
        field: "body".into(),
        text: text.into(),
    };

    let must_and_not = c
        .boolean_search(&BooleanQuery {
            must: vec![q("dog")],
            should: vec![],
            must_not: vec![q("lazy")],
        })
        .unwrap();
    assert_eq!(ids(&must_and_not), ["c"]);

    let should_only = c
        .boolean_search(&BooleanQuery {
            must: vec![],
            should: vec![q("fox"), q("runs")],
            must_not: vec![],
        })
        .unwrap();
    assert_eq!(ids(&should_only), ["a", "c"]);

    let empty = c.boolean_search(&BooleanQuery::default()).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn indexed_and_unindexed_scans_agree_with_expected_rows() {
    let c = populated();
    // `year` is indexed (range index); `score` is not (column scan).
    let recent = c
        .scan(&Predicate::Range {
            field: "year".into(),
            start: Bound::Included(FieldValue::Integer(2020)),
            end: Bound::Unbounded,
        })
        .unwrap();
    assert_eq!(recent.iter().collect::<Vec<_>>(), [1, 2]);

    let high_score = c
        .scan(&Predicate::Range {
            field: "score".into(),
            start: Bound::Excluded(FieldValue::Float(2.0)),
            end: Bound::Unbounded,
        })
        .unwrap();
    assert_eq!(high_score.iter().collect::<Vec<_>>(), [1, 2]);
}

#[test]
fn boolean_combinators_over_scalar_predicates() {
    let c = populated();
    let pred = Predicate::And(vec![
        Predicate::Equals {
            field: "live".into(),
            value: FieldValue::Boolean(true),
        },
        Predicate::Not(Box::new(Predicate::Equals {
            field: "year".into(),
            value: FieldValue::Integer(2018),
        })),
    ]);
    assert_eq!(c.scan(&pred).unwrap().iter().collect::<Vec<_>>(), [2]);

    let either = Predicate::Or(vec![Predicate::In {
        field: "year".into(),
        values: vec![FieldValue::Integer(2018), FieldValue::Integer(2024)],
    }]);
    assert_eq!(c.scan(&either).unwrap().iter().collect::<Vec<_>>(), [0, 2]);
}

#[test]
fn reindexing_a_document_supersedes_the_old_row() {
    let mut c = populated();
    c.upsert(&doc("a", "completely different text", 2001, 9.0, true))
        .unwrap();

    assert_eq!(c.len(), 3);
    let old_hits = c
        .search(&TermQuery {
            field: "body".into(),
            text: "fox".into(),
        })
        .unwrap();
    assert!(old_hits.is_empty(), "old body must not match after update");

    let by_year = c
        .scan(&Predicate::Equals {
            field: "year".into(),
            value: FieldValue::Integer(2018),
        })
        .unwrap();
    assert!(by_year.is_empty(), "old scalar row must not be live");

    let new_row = c.row_of(&DocumentId::from("a")).unwrap();
    assert!(c
        .scan(&Predicate::Equals {
            field: "year".into(),
            value: FieldValue::Integer(2001)
        })
        .unwrap()
        .contains(new_row));
}

#[test]
fn delete_removes_document_from_every_path() {
    let mut c = populated();
    assert!(c.delete(&DocumentId::from("c")));
    assert!(!c.delete(&DocumentId::from("c")));
    assert_eq!(c.len(), 2);

    let hits = c
        .search(&TermQuery {
            field: "body".into(),
            text: "runs".into(),
        })
        .unwrap();
    assert!(hits.is_empty());
    let all = c.scan(&Predicate::Not(Box::new(Predicate::In {
        field: "year".into(),
        values: vec![],
    })));
    assert_eq!(all.unwrap().len(), 2);
}

#[test]
fn aggregations_respect_live_rows() {
    let mut c = populated();
    assert_eq!(
        c.aggregate("score", Aggregation::Count).unwrap(),
        AggregationResult::Count(3)
    );
    assert_eq!(
        c.aggregate("score", Aggregation::Sum).unwrap(),
        AggregationResult::Value(Some(8.0))
    );
    assert_eq!(
        c.aggregate("score", Aggregation::Max).unwrap(),
        AggregationResult::Value(Some(4.0))
    );

    c.delete(&DocumentId::from("b"));
    assert_eq!(
        c.aggregate("score", Aggregation::Avg).unwrap(),
        AggregationResult::Value(Some(2.0))
    );
    assert_eq!(
        c.aggregate("score", Aggregation::Percentile(1.0)).unwrap(),
        AggregationResult::Value(Some(2.5))
    );
}

#[test]
fn group_by_counts_per_key() {
    let c = populated();
    let groups = c.group_by("live", "score", Aggregation::Count).unwrap();
    assert_eq!(
        groups[&IndexKey::Boolean(true)],
        AggregationResult::Count(2)
    );
    assert_eq!(
        groups[&IndexKey::Boolean(false)],
        AggregationResult::Count(1)
    );
}

#[test]
fn numeric_aggregation_on_boolean_is_an_error() {
    let c = populated();
    assert!(matches!(
        c.aggregate("live", Aggregation::Sum),
        Err(IndexError::Unsupported { .. })
    ));
}

#[test]
fn stored_copy_drops_non_stored_fields() {
    let mut s = schema();
    s.fields.get_mut("score").unwrap().stored = false;
    let mut c = Collection::new(s).unwrap();
    c.upsert(&doc("a", "hello", 2000, 1.0, true)).unwrap();
    let stored = c.get(&DocumentId::from("a")).unwrap();
    assert!(stored.get("score").is_none());
    assert!(stored.get("body").is_some());
}
