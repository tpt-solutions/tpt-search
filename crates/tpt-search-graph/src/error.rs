//! Errors from graph construction, traversal, and pattern matching.

use thiserror::Error;

/// Errors from the graph layer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GraphError {
    /// A node with this id already exists.
    #[error("node `{0}` already exists")]
    DuplicateNode(String),
    /// No node has this id.
    #[error("unknown node `{0}`")]
    UnknownNode(String),
    /// A pattern names a variable that is not bound.
    #[error("unknown variable `{0}`")]
    UnknownVariable(String),
    /// Query text could not be parsed.
    #[error("parse error: {0}")]
    Parse(String),
    /// A pattern is well-formed text but not supported (for example an unbounded hop).
    #[error("unsupported pattern: {0}")]
    Unsupported(String),
    /// A match produced more rows than the caller allows.
    #[error("more than {0} matches")]
    TooManyMatches(usize),
}
