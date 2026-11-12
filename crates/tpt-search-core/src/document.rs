//! Documents and the values they hold.

use std::collections::HashMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Name of a field within a [`Schema`](crate::Schema).
pub type FieldName = String;

/// Stable, user-supplied identifier for a document.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DocumentId(pub String);

impl DocumentId {
    /// Returns the identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for DocumentId {
    fn from(s: &str) -> Self {
        DocumentId(s.to_owned())
    }
}

impl From<String> for DocumentId {
    fn from(s: String) -> Self {
        DocumentId(s)
    }
}

/// A typed value stored in a document field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldValue {
    /// UTF-8 text.
    Text(String),
    /// Signed 64-bit integer.
    Integer(i64),
    /// IEEE-754 double. `NaN` is rejected by range indexes.
    Float(f64),
    /// Boolean.
    Boolean(bool),
    /// UTC timestamp.
    DateTime(DateTime<Utc>),
    /// Dense vector of `f32` components.
    Vector(Vec<f32>),
    /// Homogeneous or heterogeneous list of values.
    Array(Vec<FieldValue>),
    /// Nested named fields.
    Object(HashMap<FieldName, FieldValue>),
    /// Absent value.
    Null,
}

impl FieldValue {
    /// Short name of the variant, used in error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            FieldValue::Text(_) => "text",
            FieldValue::Integer(_) => "integer",
            FieldValue::Float(_) => "float",
            FieldValue::Boolean(_) => "boolean",
            FieldValue::DateTime(_) => "datetime",
            FieldValue::Vector(_) => "vector",
            FieldValue::Array(_) => "array",
            FieldValue::Object(_) => "object",
            FieldValue::Null => "null",
        }
    }

    /// Returns the text if this is a [`FieldValue::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            FieldValue::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the integer if this is a [`FieldValue::Integer`].
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            FieldValue::Integer(i) => Some(*i),
            _ => None,
        }
    }
}

impl From<&str> for FieldValue {
    fn from(s: &str) -> Self {
        FieldValue::Text(s.to_owned())
    }
}

impl From<String> for FieldValue {
    fn from(s: String) -> Self {
        FieldValue::Text(s)
    }
}

impl From<i64> for FieldValue {
    fn from(i: i64) -> Self {
        FieldValue::Integer(i)
    }
}

impl From<f64> for FieldValue {
    fn from(f: f64) -> Self {
        FieldValue::Float(f)
    }
}

impl From<bool> for FieldValue {
    fn from(b: bool) -> Self {
        FieldValue::Boolean(b)
    }
}

impl From<DateTime<Utc>> for FieldValue {
    fn from(t: DateTime<Utc>) -> Self {
        FieldValue::DateTime(t)
    }
}

impl From<Vec<f32>> for FieldValue {
    fn from(v: Vec<f32>) -> Self {
        FieldValue::Vector(v)
    }
}

/// A document: an identifier, typed fields, and bookkeeping metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Caller-supplied identifier. Re-indexing the same id replaces the document.
    pub id: DocumentId,
    /// Field values keyed by field name.
    pub fields: HashMap<FieldName, FieldValue>,
    /// Ingestion time (UTC). Set to `Utc::now()` by [`Document::new`].
    pub timestamp: DateTime<Utc>,
    /// Monotonic version chosen by the writer; defaults to 1.
    pub version: u64,
}

impl Document {
    /// Creates an empty document with the current UTC time and version 1.
    pub fn new(id: impl Into<DocumentId>) -> Self {
        Document {
            id: id.into(),
            fields: HashMap::new(),
            timestamp: Utc::now(),
            version: 1,
        }
    }

    /// Builder-style field setter.
    pub fn with_field(mut self, name: impl Into<FieldName>, value: impl Into<FieldValue>) -> Self {
        self.fields.insert(name.into(), value.into());
        self
    }

    /// Returns the value of a field, if present.
    pub fn get(&self, name: &str) -> Option<&FieldValue> {
        self.fields.get(name)
    }
}
