//! A collection couples the inverted index, columnar store, and range indexes
//! behind one write path, and tracks which rows are live.

use std::collections::HashMap;
use std::ops::Bound;
use std::sync::Arc;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use tpt_search_core::{Document, DocumentId, FieldName, FieldType, FieldValue, IndexError, Schema};

use crate::columnar::{Aggregation, AggregationResult, ColumnarStore, IndexKey, ScalarPredicate};
use crate::inverted::{BooleanQuery, InvertedIndex, PhraseQuery, ScoredDocument, TermQuery};
use crate::range::RangeIndex;
use tpt_search_vector::{HnswConfig, HnswIndex};

/// A condition over scalar fields and combinations of conditions.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    /// Field equals the value.
    Equals {
        /// Scalar field.
        field: FieldName,
        /// Value to match.
        value: FieldValue,
    },
    /// Field equals one of the values.
    In {
        /// Scalar field.
        field: FieldName,
        /// Values to match.
        values: Vec<FieldValue>,
    },
    /// Field lies between the bounds.
    Range {
        /// Scalar field.
        field: FieldName,
        /// Lower bound.
        start: Bound<FieldValue>,
        /// Upper bound.
        end: Bound<FieldValue>,
    },
    /// All sub-predicates hold.
    And(Vec<Predicate>),
    /// At least one sub-predicate holds.
    Or(Vec<Predicate>),
    /// The sub-predicate does not hold (over live rows).
    Not(Box<Predicate>),
}

/// An indexed set of documents conforming to one schema.
///
/// Re-inserting a document with an existing id supersedes the old row: the old
/// row is marked deleted and the new one becomes live.
#[derive(Debug, Serialize, Deserialize)]
pub struct Collection {
    schema: Arc<Schema>,
    inverted: InvertedIndex,
    columns: ColumnarStore,
    ranges: HashMap<FieldName, RangeIndex>,
    vectors: HashMap<FieldName, HnswIndex>,
    sources: Vec<Document>,
    deleted: RoaringBitmap,
    rows_by_id: HashMap<DocumentId, u32>,
}

impl Collection {
    /// Creates an empty collection. Fails if the schema is invalid.
    pub fn new(schema: Schema) -> Result<Self, IndexError> {
        schema.validate()?;
        let schema = Arc::new(schema);
        let ranges = schema
            .fields
            .values()
            .filter(|f| f.indexed && is_scalar(&f.field_type))
            .map(|f| (f.name.clone(), RangeIndex::new()))
            .collect();
        let vectors = schema
            .fields
            .values()
            .filter(|f| f.indexed && matches!(f.field_type, FieldType::Vector(_)))
            .map(|f| {
                let metric = f.vector_config.map(|c| c.metric).unwrap_or_default();
                let config = HnswConfig {
                    metric,
                    ..HnswConfig::default()
                };
                Ok((f.name.clone(), HnswIndex::new(config)?))
            })
            .collect::<Result<HashMap<_, _>, IndexError>>()?;
        Ok(Collection {
            vectors,
            inverted: InvertedIndex::new(Arc::clone(&schema)),
            columns: ColumnarStore::for_schema(&schema),
            ranges,
            sources: Vec::new(),
            deleted: RoaringBitmap::new(),
            rows_by_id: HashMap::new(),
            schema,
        })
    }

    /// The collection's schema.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Number of live documents.
    pub fn len(&self) -> usize {
        self.rows_by_id.len()
    }

    /// Returns `true` if no live documents exist.
    pub fn is_empty(&self) -> bool {
        self.rows_by_id.is_empty()
    }

    /// Inserts or replaces a document. Returns the row it was stored at.
    pub fn upsert(&mut self, doc: &Document) -> Result<u32, IndexError> {
        self.schema.validate_document(doc)?;
        self.check_vectors(doc)?;
        let row = self.inverted.row_count();

        // Columns and ranges are updated before the inverted index. Validation has
        // already run, so none of these steps can fail partway through.
        self.columns.push_row(row, &doc.fields)?;
        for (name, range) in &mut self.ranges {
            if let Some(key) = doc.get(name).and_then(IndexKey::from_value) {
                range.insert(key, row);
            }
        }
        for (name, index) in &mut self.vectors {
            if let Some(FieldValue::Vector(v)) = doc.get(name) {
                index.insert(u64::from(row), v.clone())?;
            }
        }
        let inverted_row = self.inverted.index_document(doc)?;
        debug_assert_eq!(inverted_row, row);

        if let Some(old) = self.rows_by_id.insert(doc.id.clone(), row) {
            self.deleted.insert(old);
        }
        // The full source document is kept so that merges can re-index it; fields
        // that are not `stored` are removed only when a document is retrieved.
        self.sources.push(doc.clone());
        Ok(row)
    }

    /// Marks a document deleted. Returns `false` if no live document has the id.
    pub fn delete(&mut self, id: &DocumentId) -> bool {
        match self.rows_by_id.remove(id) {
            Some(row) => {
                self.deleted.insert(row);
                true
            }
            None => false,
        }
    }

    /// Returns the live document with this id, with non-stored fields removed.
    pub fn get(&self, id: &DocumentId) -> Option<Document> {
        self.rows_by_id
            .get(id)
            .and_then(|row| self.document_at(*row))
    }

    /// Returns the document at `row` (live or not) with non-stored fields removed.
    pub fn document_at(&self, row: u32) -> Option<Document> {
        self.sources
            .get(row as usize)
            .map(|doc| self.retrieval_copy(doc))
    }

    /// Full source documents of all live rows, in row order. Non-stored fields
    /// are included; this is what segment merges re-index from.
    pub fn live_sources(&self) -> Vec<&Document> {
        self.live_rows()
            .iter()
            .filter_map(|row| self.sources.get(row as usize))
            .collect()
    }

    /// The live row for an id.
    pub fn row_of(&self, id: &DocumentId) -> Option<u32> {
        self.rows_by_id.get(id).copied()
    }

    /// Rows that have been appended and are not superseded or deleted.
    pub fn live_rows(&self) -> RoaringBitmap {
        let mut all = RoaringBitmap::new();
        all.insert_range(0..self.inverted.row_count());
        all - &self.deleted
    }

    /// Whether `row` is live.
    pub fn is_live(&self, row: u32) -> bool {
        !self.deleted.contains(row) && (row as usize) < self.sources.len()
    }

    /// Inverted index, for text-level access.
    pub fn inverted(&self) -> &InvertedIndex {
        &self.inverted
    }

    /// Columnar store, for scalar-level access.
    pub fn columns(&self) -> &ColumnarStore {
        &self.columns
    }

    /// Full-text exact-term search over live documents.
    pub fn search(&self, query: &TermQuery) -> Result<Vec<ScoredDocument>, IndexError> {
        Ok(self.live_only(self.inverted.search(query)?))
    }

    /// Full-text phrase search over live documents.
    pub fn phrase_search(&self, query: &PhraseQuery) -> Result<Vec<ScoredDocument>, IndexError> {
        Ok(self.live_only(self.inverted.phrase_search(query)?))
    }

    /// Full-text boolean search over live documents.
    pub fn boolean_search(&self, query: &BooleanQuery) -> Result<Vec<ScoredDocument>, IndexError> {
        Ok(self.live_only(self.inverted.boolean_search(query)?))
    }

    /// Live rows that satisfy a scalar predicate.
    pub fn scan(&self, predicate: &Predicate) -> Result<RoaringBitmap, IndexError> {
        Ok(self.eval(predicate)? & self.live_rows())
    }

    /// Aggregates a scalar field over live rows.
    pub fn aggregate(
        &self,
        field: &str,
        agg: Aggregation,
    ) -> Result<AggregationResult, IndexError> {
        self.columns.aggregate(field, agg, &self.live_rows())
    }

    /// Groups live rows by one scalar field and aggregates another within each group.
    pub fn group_by(
        &self,
        group_field: &str,
        agg_field: &str,
        agg: Aggregation,
    ) -> Result<std::collections::BTreeMap<IndexKey, AggregationResult>, IndexError> {
        self.columns
            .group_by(group_field, agg_field, agg, &self.live_rows())
    }

    /// Approximate nearest neighbours of `query` in the indexed vector field `field`.
    /// Only live documents are returned. `ef` defaults to the index's `ef_search`.
    pub fn knn(
        &self,
        field: &str,
        query: &[f32],
        k: usize,
        ef: Option<usize>,
    ) -> Result<Vec<ScoredDocument>, IndexError> {
        let index = self.vectors.get(field).ok_or_else(|| match self.schema.field(field) {
            Some(f) if matches!(f.field_type, FieldType::Vector(_)) => {
                IndexError::NotIndexed(field.to_owned())
            }
            Some(_) => IndexError::Unsupported {
                field: field.to_owned(),
                operation: "vector search",
            },
            None => IndexError::UnknownField(field.to_owned()),
        })?;
        let ef = ef.unwrap_or(index.config().ef_search);
        let hits = index.search_filtered(query, k, ef, |key| self.is_live(key as u32))?;
        Ok(hits
            .into_iter()
            .map(|hit| {
                let row = hit.key as u32;
                ScoredDocument {
                    doc_id: self.sources[row as usize].id.clone(),
                    row,
                    score: hit.score,
                    highlights: Vec::new(),
                }
            })
            .collect())
    }

    /// Rejects vector values that cannot be indexed, before anything is modified.
    fn check_vectors(&self, doc: &Document) -> Result<(), IndexError> {
        for name in self.vectors.keys() {
            if let Some(FieldValue::Vector(v)) = doc.get(name) {
                if v.iter().any(|x| !x.is_finite()) {
                    return Err(IndexError::InvalidValue(format!(
                        "field `{name}` contains NaN or infinity"
                    )));
                }
            }
        }
        Ok(())
    }

    fn eval(&self, predicate: &Predicate) -> Result<RoaringBitmap, IndexError> {
        match predicate {
            Predicate::Equals { field, value } => {
                let key = self.key_for(field, value)?;
                self.scalar_rows(field, &ScalarPredicate::Equals(key))
            }
            Predicate::In { field, values } => {
                let keys = values
                    .iter()
                    .map(|v| self.key_for(field, v))
                    .collect::<Result<Vec<_>, _>>()?;
                self.scalar_rows(field, &ScalarPredicate::In(keys))
            }
            Predicate::Range { field, start, end } => {
                let start = self.map_bound(field, start)?;
                let end = self.map_bound(field, end)?;
                self.scalar_rows(field, &ScalarPredicate::Range { start, end })
            }
            Predicate::And(parts) => {
                let mut acc = self.live_rows();
                for part in parts {
                    acc &= self.eval(part)?;
                }
                Ok(acc)
            }
            Predicate::Or(parts) => {
                let mut acc = RoaringBitmap::new();
                for part in parts {
                    acc |= self.eval(part)?;
                }
                Ok(acc)
            }
            Predicate::Not(inner) => Ok(self.live_rows() - self.eval(inner)?),
        }
    }

    /// Uses the range index when the field is indexed, otherwise scans its column.
    fn scalar_rows(
        &self,
        field: &str,
        pred: &ScalarPredicate,
    ) -> Result<RoaringBitmap, IndexError> {
        let schema_field = self
            .schema
            .field(field)
            .ok_or_else(|| IndexError::UnknownField(field.to_owned()))?;
        if schema_field.indexed {
            let range = &self.ranges[field];
            Ok(match pred {
                ScalarPredicate::Equals(k) => range.equality_query(k),
                ScalarPredicate::In(ks) => range.in_query(ks),
                ScalarPredicate::Range { start, end } => {
                    range.range_query(start.clone(), end.clone())
                }
            })
        } else {
            self.columns.scan(field, pred)
        }
    }

    fn key_for(&self, field: &str, value: &FieldValue) -> Result<IndexKey, IndexError> {
        let schema_field = self.scalar_field(field)?;
        if matches!(value, FieldValue::Null)
            || !schema_field.field_type.matches(value).unwrap_or(false)
        {
            return Err(IndexError::ValueTypeMismatch {
                field: field.to_owned(),
                got: value.type_name(),
            });
        }
        IndexKey::from_value(value).ok_or_else(|| IndexError::ValueTypeMismatch {
            field: field.to_owned(),
            got: value.type_name(),
        })
    }

    fn map_bound(
        &self,
        field: &str,
        bound: &Bound<FieldValue>,
    ) -> Result<Bound<IndexKey>, IndexError> {
        Ok(match bound {
            Bound::Included(v) => Bound::Included(self.key_for(field, v)?),
            Bound::Excluded(v) => Bound::Excluded(self.key_for(field, v)?),
            Bound::Unbounded => Bound::Unbounded,
        })
    }

    fn scalar_field(&self, field: &str) -> Result<&tpt_search_core::FieldSchema, IndexError> {
        let schema_field = self
            .schema
            .field(field)
            .ok_or_else(|| IndexError::UnknownField(field.to_owned()))?;
        if !is_scalar(&schema_field.field_type) {
            return Err(IndexError::Unsupported {
                field: field.to_owned(),
                operation: "scalar predicate",
            });
        }
        Ok(schema_field)
    }

    fn live_only(&self, mut results: Vec<ScoredDocument>) -> Vec<ScoredDocument> {
        results.retain(|s| self.is_live(s.row));
        results
    }

    fn retrieval_copy(&self, doc: &Document) -> Document {
        let mut copy = doc.clone();
        copy.fields
            .retain(|name, _| self.schema.field(name).is_some_and(|f| f.stored));
        copy
    }
}

fn is_scalar(field_type: &FieldType) -> bool {
    matches!(
        field_type,
        FieldType::Integer | FieldType::Float | FieldType::Boolean | FieldType::DateTime
    )
}
