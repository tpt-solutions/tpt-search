//! Schemas describe the fields a collection accepts and how each is indexed.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::analysis::Analyzer;
use crate::document::{Document, FieldName, FieldValue};
use crate::error::SchemaError;

/// A collection's schema: a named set of typed fields plus a primary key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    /// Collection name.
    pub name: String,
    /// Field definitions keyed by name.
    pub fields: HashMap<FieldName, FieldSchema>,
    /// Name of the field that identifies a document.
    pub primary_key: FieldName,
}

impl Schema {
    /// Creates an empty schema. Add fields with [`Schema::with_field`].
    pub fn new(name: impl Into<String>, primary_key: impl Into<FieldName>) -> Self {
        Schema {
            name: name.into(),
            fields: HashMap::new(),
            primary_key: primary_key.into(),
        }
    }

    /// Adds a field, failing if a field with the same name already exists.
    pub fn with_field(mut self, field: FieldSchema) -> Result<Self, SchemaError> {
        if self.fields.contains_key(&field.name) {
            return Err(SchemaError::DuplicateField(field.name));
        }
        self.fields.insert(field.name.clone(), field);
        Ok(self)
    }

    /// Looks up a field definition.
    pub fn field(&self, name: &str) -> Option<&FieldSchema> {
        self.fields.get(name)
    }

    /// Checks the schema itself: the primary key must be declared and every
    /// analyzer must be supported.
    pub fn validate(&self) -> Result<(), SchemaError> {
        if !self.fields.contains_key(&self.primary_key) {
            return Err(SchemaError::MissingPrimaryKey(self.primary_key.clone()));
        }
        for field in self.fields.values() {
            field.validate()?;
        }
        Ok(())
    }

    /// Checks that every field of `doc` is declared and has a matching type.
    pub fn validate_document(&self, doc: &Document) -> Result<(), SchemaError> {
        self.validate_fields(&doc.fields)
    }

    /// Checks a field map against this schema (also used for nested objects).
    pub fn validate_fields(
        &self,
        fields: &HashMap<FieldName, FieldValue>,
    ) -> Result<(), SchemaError> {
        for (name, value) in fields {
            let schema = self
                .fields
                .get(name)
                .ok_or_else(|| SchemaError::UnknownField(name.clone()))?;
            if !schema.field_type.matches(value)? {
                return Err(SchemaError::TypeMismatch {
                    field: name.clone(),
                    expected: schema.field_type.clone(),
                    got: value.type_name().to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// Definition of a single field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldSchema {
    /// Field name.
    pub name: FieldName,
    /// Declared type of the field's values.
    pub field_type: FieldType,
    /// Whether the field is searchable (inverted or range index).
    pub indexed: bool,
    /// Whether the original value is kept for retrieval.
    pub stored: bool,
    /// Text analysis applied when indexing. `None` means the default
    /// standard analyzer for text fields.
    pub analyzer: Option<Analyzer>,
    /// Vector options; only meaningful for [`FieldType::Vector`].
    pub vector_config: Option<VectorConfig>,
}

impl FieldSchema {
    /// Creates a stored, unindexed field of the given type.
    pub fn new(name: impl Into<FieldName>, field_type: FieldType) -> Self {
        FieldSchema {
            name: name.into(),
            field_type,
            indexed: false,
            stored: true,
            analyzer: None,
            vector_config: None,
        }
    }

    /// Marks the field as indexed.
    pub fn indexed(mut self) -> Self {
        self.indexed = true;
        self
    }

    /// Marks the field as not stored; retrieval will not return its value.
    pub fn not_stored(mut self) -> Self {
        self.stored = false;
        self
    }

    /// Sets the analyzer used for text indexing.
    pub fn with_analyzer(mut self, analyzer: Analyzer) -> Self {
        self.analyzer = Some(analyzer);
        self
    }

    /// Checks this field's analyzer (if any) and vector configuration.
    pub fn validate(&self) -> Result<(), SchemaError> {
        if let Some(analyzer) = &self.analyzer {
            analyzer.validate()?;
        }
        if let FieldType::Vector(dim) = self.field_type {
            if dim == 0 {
                return Err(SchemaError::InvalidField {
                    field: self.name.clone(),
                    reason: "vector dimension must be greater than zero".into(),
                });
            }
        }
        Ok(())
    }
}

/// Declared type of a field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FieldType {
    /// UTF-8 text.
    Text,
    /// 64-bit integer.
    Integer,
    /// 64-bit float.
    Float,
    /// Boolean.
    Boolean,
    /// UTC timestamp.
    DateTime,
    /// Vector of the given dimension.
    Vector(u32),
    /// Array whose elements all have the inner type.
    Array(Box<FieldType>),
    /// Nested object described by its own schema.
    Object(Box<Schema>),
}

impl FieldType {
    /// Returns whether `value` conforms to this type. `Null` conforms to every
    /// type. Errors only if a nested object schema is itself invalid.
    pub fn matches(&self, value: &FieldValue) -> Result<bool, SchemaError> {
        Ok(match (self, value) {
            (_, FieldValue::Null) => true,
            (FieldType::Text, FieldValue::Text(_)) => true,
            (FieldType::Integer, FieldValue::Integer(_)) => true,
            (FieldType::Float, FieldValue::Float(_)) => true,
            (FieldType::Boolean, FieldValue::Boolean(_)) => true,
            (FieldType::DateTime, FieldValue::DateTime(_)) => true,
            (FieldType::Vector(dim), FieldValue::Vector(v)) => v.len() == *dim as usize,
            (FieldType::Array(inner), FieldValue::Array(items)) => {
                for item in items {
                    if !inner.matches(item)? {
                        return Ok(false);
                    }
                }
                true
            }
            (FieldType::Object(schema), FieldValue::Object(map)) => {
                schema.validate_fields(map)?;
                true
            }
            _ => false,
        })
    }
}

/// Options for vector fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorConfig {
    /// Expected dimension; must match the [`FieldType::Vector`] dimension.
    pub dimension: u32,
    /// Similarity metric for nearest-neighbour search.
    pub metric: DistanceMetric,
}

/// How two vectors are compared. Smaller distances mean closer vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DistanceMetric {
    /// Cosine similarity (distance is `1 - cos`). The default.
    #[default]
    Cosine,
    /// Euclidean (L2). Distances are compared in squared form, which orders identically.
    Euclidean,
    /// Dot product (distance is `-dot`), for vectors already normalized or
    /// where magnitude matters.
    DotProduct,
    /// Manhattan (L1).
    Manhattan,
}
