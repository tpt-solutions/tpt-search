//! Error hierarchy. Every crate in the workspace reports failures through these
//! types; [`SearchError`] is the umbrella that the API layer will expose.

use thiserror::Error;

use crate::schema::FieldType;

/// Convenience alias for results carrying [`SearchError`].
pub type Result<T, E = SearchError> = std::result::Result<T, E>;

/// Umbrella error across all subsystems.
#[derive(Debug, Error)]
pub enum SearchError {
    /// Schema definition or document validation failed.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// Indexing or index lookup failed.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// A query could not be parsed, planned, or executed.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// Persistence (WAL, segments, snapshots) failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// Cluster operations failed.
    #[error(transparent)]
    Distributed(#[from] DistributedError),
    /// Edge-to-cloud synchronization failed.
    #[error(transparent)]
    Sync(#[from] SyncError),
    /// Underlying I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors from schema definition and document validation.
#[derive(Debug, Error, PartialEq)]
pub enum SchemaError {
    /// A field with this name is already declared.
    #[error("field `{0}` is already declared")]
    DuplicateField(String),
    /// The primary key names a field that is not declared.
    #[error("primary key `{0}` is not a declared field")]
    MissingPrimaryKey(String),
    /// A document contains a field the schema does not declare.
    #[error("unknown field `{0}`")]
    UnknownField(String),
    /// A value does not match its field's declared type.
    #[error("field `{field}`: expected {expected:?}, got {got}")]
    TypeMismatch {
        /// Offending field.
        field: String,
        /// Declared type.
        expected: FieldType,
        /// Type name of the supplied value.
        got: String,
    },
    /// A field definition is internally inconsistent.
    #[error("field `{field}`: {reason}")]
    InvalidField {
        /// Offending field.
        field: String,
        /// What is wrong with it.
        reason: String,
    },
    /// An analyzer uses a tokenizer or filter that is not available.
    #[error("unsupported analyzer: {0}")]
    UnsupportedAnalyzer(String),
}

/// Errors from indexing and index lookup.
#[derive(Debug, Error, PartialEq)]
pub enum IndexError {
    /// The field is not declared in the schema.
    #[error("unknown field `{0}`")]
    UnknownField(String),
    /// The operation needs an index on the field, but it was not declared `indexed`.
    #[error("field `{0}` is not indexed")]
    NotIndexed(String),
    /// The field type does not support the requested operation.
    #[error("field `{field}` does not support {operation}")]
    Unsupported {
        /// Field the operation was attempted on.
        field: String,
        /// Operation name.
        operation: &'static str,
    },
    /// The supplied value cannot be compared with the field's type.
    #[error("value of type {got} cannot be used with field `{field}`")]
    ValueTypeMismatch {
        /// Field the value was compared against.
        field: String,
        /// Type name of the supplied value.
        got: &'static str,
    },
    /// A vector has the wrong number of components.
    #[error("vector dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// Dimension the index was built for.
        expected: usize,
        /// Dimension of the supplied vector.
        got: usize,
    },
    /// A value is not usable (for example NaN or infinity in a vector).
    #[error("invalid value: {0}")]
    InvalidValue(String),
    /// Schema validation failed while indexing.
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

/// Errors from query parsing, planning, and execution.
#[derive(Debug, Error, PartialEq)]
pub enum QueryError {
    /// The query text is malformed.
    #[error("parse error: {0}")]
    ParseError(String),
    /// The query references a field that does not exist or cannot be used that way.
    #[error("invalid field `{0}`")]
    InvalidField(String),
    /// A literal does not match the field's type.
    #[error("type mismatch: expected {expected:?}, got {got:?}")]
    TypeMismatch {
        /// Expected field type.
        expected: FieldType,
        /// Type of the literal supplied.
        got: FieldType,
    },
    /// Execution exceeded its time budget.
    #[error("query timed out")]
    Timeout,
    /// Execution exceeded a resource bound (for example candidate-set size or query length).
    #[error("resource limit exceeded: {0}")]
    ResourceExhausted(String),
    /// The query names an index that is not registered.
    #[error("unknown index `{0}`")]
    UnknownIndex(String),
    /// A plan has a shape the executor cannot run.
    #[error("invalid plan: {0}")]
    InvalidPlan(String),
    /// Error bubbled up from the index layer.
    #[error(transparent)]
    Index(#[from] IndexError),
}

/// Errors from persistence.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Filesystem error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// On-disk data failed a checksum or structural check.
    #[error("corrupt data: {0}")]
    Corrupt(String),
    /// Encoding or decoding failed.
    #[error("serialization error: {0}")]
    Serialization(String),
    /// Error bubbled up from the index layer.
    #[error(transparent)]
    Index(#[from] IndexError),
}

/// Errors from cluster operations. Reserved for the distributed layer (Phase 4).
#[derive(Debug, Error, PartialEq)]
pub enum DistributedError {
    /// A node could not be reached.
    #[error("node `{0}` is unavailable")]
    NodeUnavailable(String),
    /// Consensus could not be reached or was lost.
    #[error("consensus error: {0}")]
    Consensus(String),
    /// The cluster configuration is invalid.
    #[error("invalid cluster configuration: {0}")]
    InvalidConfig(String),
    /// A shard has no reachable replica.
    #[error("shard {0} has no reachable replica")]
    ShardUnavailable(u32),
}

/// Errors from edge-to-cloud synchronization. Reserved for Phase 5.
#[derive(Debug, Error, PartialEq)]
pub enum SyncError {
    /// Replica state could not be reconciled.
    #[error("sync conflict: {0}")]
    Conflict(String),
    /// The peer is offline.
    #[error("offline")]
    Offline,
}
