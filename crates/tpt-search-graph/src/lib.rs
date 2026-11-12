//! Graph search for `tpt-search`: a directed property graph, traversal (BFS, DFS,
//! shortest paths), graph analytics (components, PageRank, betweenness, communities),
//! and a Cypher-style pattern language.
//!
//! Node ids match document ids in the search layer, so a graph pattern's results can
//! be joined back to documents.
//!
//! # Example
//!
//! ```
//! use tpt_search_graph::{parse_graph_query, execute, PropertyGraph, DEFAULT_MAX_MATCHES};
//!
//! let mut g = PropertyGraph::new();
//! g.add_labelled_node("a", &["Doc"])?;
//! g.add_labelled_node("b", &["Doc"])?;
//! g.add_labelled_node("c", &["Doc"])?;
//! g.add_edge("a", "b", "CITES", Default::default())?;
//! g.add_edge("b", "c", "CITES", Default::default())?;
//!
//! let q = parse_graph_query("MATCH (x:Doc)-[:CITES*1..2]->(y) RETURN y")?;
//! let rows = execute(&g, &q, DEFAULT_MAX_MATCHES)?;
//! assert_eq!(rows.len(), 3);
//! # Ok::<(), tpt_search_graph::GraphError>(())
//! ```

#![doc(html_root_url = "https://docs.rs/tpt-search-graph")]

pub mod analytics;
pub mod error;
pub mod graph;
pub mod pattern;
pub mod traversal;

pub use error::GraphError;
pub use graph::{Direction, Edge, Node, PropertyGraph, PropertyValue};
pub use pattern::{
    execute, parse_graph_query, Binding, CmpOp, EdgePattern, Expr, GraphQuery, NodePattern,
    OrderBy, DEFAULT_MAX_MATCHES, MAX_HOPS,
};
pub use traversal::{bfs, dfs, shortest_path, TraversalFilter, TraversalPattern};
