//! Query layer for `tpt-search`: a SQL-like query language, a rule-based
//! planner, and an executor over named collections.
//!
//! Phase 1 covers scalar and full-text queries. Vector, graph, and hybrid
//! fusion arrive in later phases.
//!
//! # Example
//!
//! ```
//! use tpt_search_core::{Analyzer, Document, FieldSchema, FieldType, Schema};
//! use tpt_search_query::{parse_query, IndexRegistry, QueryExecutor, QueryPlanner};
//!
//! let schema = Schema::new("articles", "id")
//!     .with_field(FieldSchema::new("id", FieldType::Text))?
//!     .with_field(FieldSchema::new("title", FieldType::Text).indexed().with_analyzer(Analyzer::english()))?
//!     .with_field(FieldSchema::new("year", FieldType::Integer).indexed())?;
//!
//! let mut registry = IndexRegistry::new();
//! registry.create("articles", schema)?;
//! let collection = registry.get_mut("articles").unwrap();
//! collection.upsert(&Document::new("1").with_field("title", "Rust programming").with_field("year", 2021i64))?;
//! collection.upsert(&Document::new("2").with_field("title", "Rust in the kitchen").with_field("year", 2015i64))?;
//!
//! let query = parse_query(r#"MATCH(title, "rust programming") AND year >= 2020"#)?;
//! let plan = QueryPlanner::new().plan("articles", registry.get("articles").unwrap(), &query)?;
//! let result = QueryExecutor::new(&registry).execute(&plan)?;
//! assert_eq!(result.documents.len(), 1);
//! assert_eq!(result.documents[0].id.as_str(), "1");
//! # Ok::<(), tpt_search_core::SearchError>(())
//! ```

#![doc(html_root_url = "https://docs.rs/tpt-search-query")]

pub mod ast;
pub mod executor;
pub mod parser;
pub mod plan;
pub mod registry;

pub use ast::Query;
pub use executor::{QueryExecutor, QueryResult, DEFAULT_MAX_CANDIDATES};
pub use parser::{parse_query, MAX_DEPTH, MAX_QUERY_BYTES};
pub use plan::{PlanNode, QueryPlan, QueryPlanner, SortKey};
pub use registry::IndexRegistry;
