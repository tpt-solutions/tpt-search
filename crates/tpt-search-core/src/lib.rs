//! Core types for `tpt-search`: documents, schemas, text analysis, the error
//! hierarchy, and async/telemetry helpers shared by every other crate.
//!
//! # Example
//!
//! ```
//! use tpt_search_core::{Analyzer, Document, FieldSchema, FieldType, Schema};
//!
//! let schema = Schema::new("articles", "id")
//!     .with_field(FieldSchema::new("id", FieldType::Text))
//!     .unwrap()
//!     .with_field(
//!         FieldSchema::new("title", FieldType::Text)
//!             .indexed()
//!             .with_analyzer(Analyzer::english()),
//!     )
//!     .unwrap();
//! schema.validate().unwrap();
//!
//! let doc = Document::new("a-1").with_field("title", "Rust programming");
//! schema.validate_document(&doc).unwrap();
//! ```

#![doc(html_root_url = "https://docs.rs/tpt-search-core")]

pub mod analysis;
pub mod document;
pub mod error;
pub mod runtime;
pub mod schema;
pub mod telemetry;

pub use analysis::{Analyzer, Language, Token, TokenFilter, Tokenizer};
pub use document::{Document, DocumentId, FieldName, FieldValue};
pub use error::{
    DistributedError, IndexError, QueryError, Result, SchemaError, SearchError, StorageError,
    SyncError,
};
pub use schema::{DistanceMetric, FieldSchema, FieldType, Schema, VectorConfig};
