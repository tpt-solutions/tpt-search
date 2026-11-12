//! Scalar search layer for `tpt-search`: inverted index with BM25 scoring and
//! phrase/boolean queries, columnar storage with aggregations, range indexes,
//! and the [`Collection`] that keeps them consistent.
//!
//! # Example
//!
//! ```
//! use std::ops::Bound;
//! use tpt_search_core::{Analyzer, Document, FieldSchema, FieldType, FieldValue, Schema};
//! use tpt_search_index::{Collection, Predicate, TermQuery};
//!
//! let schema = Schema::new("articles", "id")
//!     .with_field(FieldSchema::new("id", FieldType::Text))?
//!     .with_field(
//!         FieldSchema::new("title", FieldType::Text)
//!             .indexed()
//!             .with_analyzer(Analyzer::english()),
//!     )?
//!     .with_field(FieldSchema::new("year", FieldType::Integer).indexed())?;
//! let mut collection = Collection::new(schema)?;
//! collection.upsert(&Document::new("a").with_field("title", "Rust in production").with_field("year", 2021i64))?;
//! collection.upsert(&Document::new("b").with_field("title", "Cooking with rust pans").with_field("year", 2019i64))?;
//!
//! let hits = collection.search(&TermQuery { field: "title".into(), text: "rust".into() })?;
//! assert_eq!(hits.len(), 2);
//!
//! let recent = collection.scan(&Predicate::Range {
//!     field: "year".into(),
//!     start: Bound::Included(FieldValue::Integer(2020)),
//!     end: Bound::Unbounded,
//! })?;
//! assert_eq!(recent.len(), 1);
//! # Ok::<(), tpt_search_core::SearchError>(())
//! ```

#![doc(html_root_url = "https://docs.rs/tpt-search-index")]

pub mod collection;
pub mod columnar;
pub mod inverted;
pub mod range;

pub use collection::{Collection, Predicate};
pub use columnar::{
    Aggregation, AggregationResult, Column, ColumnarStore, IndexKey, OrdF64, ScalarPredicate,
};
pub use inverted::{
    BooleanQuery, Highlight, InvertedIndex, PhraseQuery, PostingList, ScoredDocument, Term,
    TermQuery,
};
pub use range::RangeIndex;
