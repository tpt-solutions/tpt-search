//! Columnar storage for scalar fields, with predicate scans and aggregations.
//!
//! Each scalar field (integer, float, boolean, datetime) gets one column. Row
//! `r` of every column describes row `r` of the collection; missing values are
//! recorded in a per-column null bitmap.

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use tpt_search_core::{FieldName, FieldType, FieldValue, IndexError, Schema};

/// Totally ordered wrapper over `f64`, usable as a map key.
///
/// Ordering follows `f64::total_cmp`, with `-0.0` normalized to `0.0`
/// when constructed through [`IndexKey::from_value`]. `NaN` is never stored.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct OrdF64(pub f64);

impl PartialEq for OrdF64 {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0).is_eq()
    }
}

impl Eq for OrdF64 {}

impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl std::hash::Hash for OrdF64 {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_bits().hash(state);
    }
}

/// A scalar value in index-key form, ordered within its own variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum IndexKey {
    /// Boolean key.
    Boolean(bool),
    /// Integer key.
    Integer(i64),
    /// Float key.
    Float(OrdF64),
    /// Timestamp as microseconds since the Unix epoch.
    DateTime(i64),
    /// Exact text key (not analyzed).
    Text(String),
}

impl IndexKey {
    /// Converts a value into an index key. Returns `None` for `Null`, `NaN`,
    /// and composite values (vectors, arrays, objects).
    pub fn from_value(value: &FieldValue) -> Option<IndexKey> {
        Some(match value {
            FieldValue::Boolean(b) => IndexKey::Boolean(*b),
            FieldValue::Integer(i) => IndexKey::Integer(*i),
            FieldValue::Float(f) if f.is_nan() => return None,
            FieldValue::Float(f) => IndexKey::Float(OrdF64(if *f == 0.0 { 0.0 } else { *f })),
            FieldValue::DateTime(t) => IndexKey::DateTime(t.timestamp_micros()),
            FieldValue::Text(s) => IndexKey::Text(s.clone()),
            _ => return None,
        })
    }
}

/// A condition over index keys.
#[derive(Debug, Clone, PartialEq)]
pub enum ScalarPredicate {
    /// Key equals the given key.
    Equals(IndexKey),
    /// Key is one of the given keys.
    In(Vec<IndexKey>),
    /// Key lies between the bounds.
    Range {
        /// Lower bound.
        start: Bound<IndexKey>,
        /// Upper bound.
        end: Bound<IndexKey>,
    },
}

impl ScalarPredicate {
    /// Returns whether `key` satisfies the predicate.
    pub fn matches(&self, key: &IndexKey) -> bool {
        match self {
            ScalarPredicate::Equals(k) => key == k,
            ScalarPredicate::In(ks) => ks.contains(key),
            ScalarPredicate::Range { start, end } => {
                let lower = match start {
                    Bound::Included(s) => key >= s,
                    Bound::Excluded(s) => key > s,
                    Bound::Unbounded => true,
                };
                let upper = match end {
                    Bound::Included(e) => key <= e,
                    Bound::Excluded(e) => key < e,
                    Bound::Unbounded => true,
                };
                lower && upper
            }
        }
    }
}

/// Aggregation to compute over a column.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Aggregation {
    /// Number of non-null values.
    Count,
    /// Sum of values (numeric columns only).
    Sum,
    /// Mean of values (numeric columns only).
    Avg,
    /// Minimum value (numeric columns only).
    Min,
    /// Maximum value (numeric columns only).
    Max,
    /// Nearest-rank percentile, with the fraction in `0.0..=1.0`
    /// (numeric columns only).
    Percentile(f64),
}

/// Result of an [`Aggregation`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AggregationResult {
    /// Result of [`Aggregation::Count`].
    Count(u64),
    /// Result of any other aggregation. `None` when no values were aggregated.
    Value(Option<f64>),
}

/// A column of one scalar type plus the set of null rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Column {
    /// 64-bit integers.
    Integer {
        /// Values by row; null rows hold `0`.
        data: Vec<i64>,
        /// Rows whose value is null.
        nulls: RoaringBitmap,
    },
    /// 64-bit floats.
    Float {
        /// Values by row; null rows hold `0.0`.
        data: Vec<f64>,
        /// Rows whose value is null.
        nulls: RoaringBitmap,
    },
    /// Booleans.
    Boolean {
        /// Values by row; null rows hold `false`.
        data: Vec<bool>,
        /// Rows whose value is null.
        nulls: RoaringBitmap,
    },
    /// Timestamps as microseconds since the Unix epoch.
    DateTime {
        /// Values by row; null rows hold `0`.
        data: Vec<i64>,
        /// Rows whose value is null.
        nulls: RoaringBitmap,
    },
}

/// A value already checked against a column's type.
#[derive(Debug, Clone, Copy)]
enum Encoded {
    Null,
    Integer(i64),
    Float(f64),
    Boolean(bool),
    DateTime(i64),
}

impl Column {
    /// Creates an empty column for a scalar field type; `None` for other types.
    pub fn for_type(field_type: &FieldType) -> Option<Column> {
        Some(match field_type {
            FieldType::Integer => Column::Integer {
                data: Vec::new(),
                nulls: RoaringBitmap::new(),
            },
            FieldType::Float => Column::Float {
                data: Vec::new(),
                nulls: RoaringBitmap::new(),
            },
            FieldType::Boolean => Column::Boolean {
                data: Vec::new(),
                nulls: RoaringBitmap::new(),
            },
            FieldType::DateTime => Column::DateTime {
                data: Vec::new(),
                nulls: RoaringBitmap::new(),
            },
            _ => return None,
        })
    }

    /// Number of rows stored so far.
    pub fn len(&self) -> usize {
        match self {
            Column::Integer { data, .. } | Column::DateTime { data, .. } => data.len(),
            Column::Float { data, .. } => data.len(),
            Column::Boolean { data, .. } => data.len(),
        }
    }

    /// Returns `true` if no rows are stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn encode(&self, field: &str, value: &FieldValue) -> Result<Encoded, IndexError> {
        Ok(match (self, value) {
            (_, FieldValue::Null) => Encoded::Null,
            (Column::Integer { .. }, FieldValue::Integer(i)) => Encoded::Integer(*i),
            (Column::Float { .. }, FieldValue::Float(f)) => Encoded::Float(*f),
            (Column::Boolean { .. }, FieldValue::Boolean(b)) => Encoded::Boolean(*b),
            (Column::DateTime { .. }, FieldValue::DateTime(t)) => {
                Encoded::DateTime(t.timestamp_micros())
            }
            _ => {
                return Err(IndexError::ValueTypeMismatch {
                    field: field.to_owned(),
                    got: value.type_name(),
                })
            }
        })
    }

    fn push(&mut self, row: u32, value: Encoded) {
        debug_assert_eq!(
            self.len(),
            row as usize,
            "columns must be appended in row order"
        );
        match (self, value) {
            (Column::Integer { data, .. }, Encoded::Integer(i)) => data.push(i),
            (Column::Float { data, .. }, Encoded::Float(f)) => data.push(f),
            (Column::Boolean { data, .. }, Encoded::Boolean(b)) => data.push(b),
            (Column::DateTime { data, .. }, Encoded::DateTime(t)) => data.push(t),
            (Column::Integer { data, nulls }, Encoded::Null) => {
                data.push(0);
                nulls.insert(row);
            }
            (Column::Float { data, nulls }, Encoded::Null) => {
                data.push(0.0);
                nulls.insert(row);
            }
            (Column::Boolean { data, nulls }, Encoded::Null) => {
                data.push(false);
                nulls.insert(row);
            }
            (Column::DateTime { data, nulls }, Encoded::Null) => {
                data.push(0);
                nulls.insert(row);
            }
            _ => unreachable!("values are encoded against the column type before pushing"),
        }
    }

    /// Index key of the value at `row`; `None` if null or out of range.
    pub fn key_at(&self, row: u32) -> Option<IndexKey> {
        if self.is_null(row) {
            return None;
        }
        let i = row as usize;
        match self {
            Column::Integer { data, .. } => data.get(i).map(|v| IndexKey::Integer(*v)),
            Column::Float { data, .. } => data.get(i).map(|v| IndexKey::Float(OrdF64(*v))),
            Column::Boolean { data, .. } => data.get(i).map(|v| IndexKey::Boolean(*v)),
            Column::DateTime { data, .. } => data.get(i).map(|v| IndexKey::DateTime(*v)),
        }
    }

    fn is_null(&self, row: u32) -> bool {
        match self {
            Column::Integer { nulls, .. }
            | Column::Float { nulls, .. }
            | Column::Boolean { nulls, .. }
            | Column::DateTime { nulls, .. } => nulls.contains(row),
        }
    }

    fn numeric_at(&self, row: u32) -> Option<f64> {
        if self.is_null(row) {
            return None;
        }
        match self {
            Column::Integer { data, .. } => data.get(row as usize).map(|v| *v as f64),
            Column::Float { data, .. } => data.get(row as usize).copied(),
            _ => None,
        }
    }
}

/// Columns for every scalar field of a schema.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColumnarStore {
    columns: HashMap<FieldName, Column>,
    row_count: u64,
}

impl ColumnarStore {
    /// Creates empty columns for each scalar field in `schema`.
    pub fn for_schema(schema: &Schema) -> Self {
        let columns = schema
            .fields
            .values()
            .filter_map(|f| Column::for_type(&f.field_type).map(|c| (f.name.clone(), c)))
            .collect();
        ColumnarStore {
            columns,
            row_count: 0,
        }
    }

    /// Number of rows appended so far.
    pub fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Appends the scalar values of one row. `row` must equal the current row count.
    ///
    /// All values are checked before any column is modified.
    pub fn push_row(
        &mut self,
        row: u32,
        fields: &HashMap<FieldName, FieldValue>,
    ) -> Result<(), IndexError> {
        assert_eq!(
            u64::from(row),
            self.row_count,
            "rows must be appended in order"
        );
        static NULL: FieldValue = FieldValue::Null;
        let mut encoded = Vec::with_capacity(self.columns.len());
        for (name, column) in &self.columns {
            let value = fields.get(name).unwrap_or(&NULL);
            encoded.push((name.clone(), column.encode(name, value)?));
        }
        for (name, value) in encoded {
            if let Some(column) = self.columns.get_mut(&name) {
                column.push(row, value);
            }
        }
        self.row_count += 1;
        Ok(())
    }

    /// Rows of `field` whose key satisfies `predicate`. Scans linearly.
    pub fn scan(
        &self,
        field: &str,
        predicate: &ScalarPredicate,
    ) -> Result<RoaringBitmap, IndexError> {
        let column = self.column(field)?;
        let mut out = RoaringBitmap::new();
        for row in 0..self.row_count as u32 {
            if column
                .key_at(row)
                .is_some_and(|key| predicate.matches(&key))
            {
                out.insert(row);
            }
        }
        Ok(out)
    }

    /// Aggregates `field` over the rows in `rows`.
    pub fn aggregate(
        &self,
        field: &str,
        agg: Aggregation,
        rows: &RoaringBitmap,
    ) -> Result<AggregationResult, IndexError> {
        let column = self.column(field)?;
        if agg == Aggregation::Count {
            let count = rows.iter().filter(|r| !column.is_null(*r)).count() as u64;
            return Ok(AggregationResult::Count(count));
        }
        if let Aggregation::Percentile(p) = agg {
            if !(0.0..=1.0).contains(&p) {
                return Err(IndexError::Unsupported {
                    field: field.to_owned(),
                    operation: "percentile outside 0.0..=1.0",
                });
            }
        }
        let mut values = Vec::new();
        for row in rows.iter() {
            match column.numeric_at(row) {
                Some(v) => values.push(v),
                None if column.is_null(row) => {}
                None => {
                    return Err(IndexError::Unsupported {
                        field: field.to_owned(),
                        operation: "numeric aggregation",
                    })
                }
            }
        }
        Ok(AggregationResult::Value(compute(&mut values, agg)))
    }

    /// Groups the rows in `rows` by the value of `group_field` and aggregates
    /// `agg_field` within each group. Rows with a null group key are skipped.
    pub fn group_by(
        &self,
        group_field: &str,
        agg_field: &str,
        agg: Aggregation,
        rows: &RoaringBitmap,
    ) -> Result<BTreeMap<IndexKey, AggregationResult>, IndexError> {
        let group_column = self.column(group_field)?;
        let mut groups: BTreeMap<IndexKey, RoaringBitmap> = BTreeMap::new();
        for row in rows.iter() {
            if let Some(key) = group_column.key_at(row) {
                groups.entry(key).or_default().insert(row);
            }
        }
        groups
            .into_iter()
            .map(|(key, members)| {
                let result = self.aggregate(agg_field, agg, &members)?;
                Ok((key, result))
            })
            .collect()
    }

    /// Key of `field` at `row`, if the field is scalar and the value is non-null.
    pub fn key_at(&self, field: &str, row: u32) -> Option<IndexKey> {
        self.columns.get(field)?.key_at(row)
    }

    /// Returns the column for a field, if it exists.
    pub fn column(&self, field: &str) -> Result<&Column, IndexError> {
        self.columns
            .get(field)
            .ok_or_else(|| IndexError::Unsupported {
                field: field.to_owned(),
                operation: "columnar access (field is not a scalar)",
            })
    }
}

fn compute(values: &mut [f64], agg: Aggregation) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(match agg {
        Aggregation::Count => values.len() as f64,
        Aggregation::Sum => values.iter().sum(),
        Aggregation::Avg => values.iter().sum::<f64>() / values.len() as f64,
        Aggregation::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
        Aggregation::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        Aggregation::Percentile(p) => {
            values.sort_by(|a, b| a.total_cmp(b));
            let rank = (p * values.len() as f64).ceil() as usize;
            values[rank.clamp(1, values.len()) - 1]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ord_f64_normalizes_negative_zero() {
        let a = IndexKey::from_value(&FieldValue::Float(-0.0)).unwrap();
        let b = IndexKey::from_value(&FieldValue::Float(0.0)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn nan_and_composites_have_no_key() {
        assert!(IndexKey::from_value(&FieldValue::Float(f64::NAN)).is_none());
        assert!(IndexKey::from_value(&FieldValue::Null).is_none());
        assert!(IndexKey::from_value(&FieldValue::Vector(vec![1.0])).is_none());
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let mut v = vec![4.0, 1.0, 3.0, 2.0];
        assert_eq!(compute(&mut v, Aggregation::Percentile(0.5)), Some(2.0));
        assert_eq!(compute(&mut v, Aggregation::Percentile(1.0)), Some(4.0));
        assert_eq!(compute(&mut v, Aggregation::Percentile(0.0)), Some(1.0));
    }
}
