//! Ordered range index over scalar keys.

use std::collections::BTreeMap;
use std::ops::Bound;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};

use crate::columnar::IndexKey;

/// Maps each scalar key to the rows that carry it, kept in key order so that
/// range queries touch only the matching span.
///
/// Equality and membership lookups use the same map, which is O(log n) in the
/// number of distinct keys. A separate low-cardinality bitmap index is not built
/// yet; see `todo.md`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RangeIndex {
    entries: BTreeMap<IndexKey, RoaringBitmap>,
}

impl RangeIndex {
    /// Creates an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `row` holds `key`.
    pub fn insert(&mut self, key: IndexKey, row: u32) {
        self.entries.entry(key).or_default().insert(row);
    }

    /// Rows whose key lies between `start` and `end`.
    pub fn range_query(&self, start: Bound<IndexKey>, end: Bound<IndexKey>) -> RoaringBitmap {
        // BTreeMap::range panics when start > end, so detect empty ranges first.
        if let (Some(s), Some(e)) = (bound_value(&start), bound_value(&end)) {
            match s.cmp(e) {
                std::cmp::Ordering::Greater => return RoaringBitmap::new(),
                std::cmp::Ordering::Equal
                    if !matches!((&start, &end), (Bound::Included(_), Bound::Included(_))) =>
                {
                    return RoaringBitmap::new()
                }
                _ => {}
            }
        }
        let mut out = RoaringBitmap::new();
        for (_, rows) in self.entries.range((start, end)) {
            out |= rows;
        }
        out
    }

    /// Rows whose key equals `key`.
    pub fn equality_query(&self, key: &IndexKey) -> RoaringBitmap {
        self.entries.get(key).cloned().unwrap_or_default()
    }

    /// Rows whose key is any of `keys`.
    pub fn in_query(&self, keys: &[IndexKey]) -> RoaringBitmap {
        let mut out = RoaringBitmap::new();
        for key in keys {
            if let Some(rows) = self.entries.get(key) {
                out |= rows;
            }
        }
        out
    }
}

fn bound_value(bound: &Bound<IndexKey>) -> Option<&IndexKey> {
    match bound {
        Bound::Included(k) | Bound::Excluded(k) => Some(k),
        Bound::Unbounded => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> RangeIndex {
        let mut idx = RangeIndex::new();
        for (row, year) in [(0u32, 2018i64), (1, 2020), (2, 2020), (3, 2024)] {
            idx.insert(IndexKey::Integer(year), row);
        }
        idx
    }

    #[test]
    fn range_bounds_are_respected() {
        let idx = index();
        let rows = idx.range_query(Bound::Included(IndexKey::Integer(2020)), Bound::Unbounded);
        assert_eq!(rows.iter().collect::<Vec<_>>(), [1, 2, 3]);

        let rows = idx.range_query(
            Bound::Excluded(IndexKey::Integer(2020)),
            Bound::Excluded(IndexKey::Integer(2024)),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn inverted_and_empty_ranges_return_nothing() {
        let idx = index();
        let rows = idx.range_query(
            Bound::Included(IndexKey::Integer(2024)),
            Bound::Included(IndexKey::Integer(2018)),
        );
        assert!(rows.is_empty());
        let rows = idx.range_query(
            Bound::Included(IndexKey::Integer(2020)),
            Bound::Excluded(IndexKey::Integer(2020)),
        );
        assert!(rows.is_empty());
    }

    #[test]
    fn equality_and_in_queries() {
        let idx = index();
        assert_eq!(idx.equality_query(&IndexKey::Integer(2020)).len(), 2);
        assert!(idx.equality_query(&IndexKey::Integer(1999)).is_empty());
        let rows = idx.in_query(&[IndexKey::Integer(2018), IndexKey::Integer(2024)]);
        assert_eq!(rows.iter().collect::<Vec<_>>(), [0, 3]);
    }
}
