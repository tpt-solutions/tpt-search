//! Segments: immutable-on-disk units of indexed documents, and the manager that
//! ingests, merges, searches, and persists them.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tpt_search_core::{Document, DocumentId, Schema, StorageError};
use tpt_search_index::{BooleanQuery, Collection, ScoredDocument, TermQuery};

use crate::mapped::MappedSegment;

/// Identifier of a segment. Larger ids are newer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SegmentId(pub u64);

/// A group of documents indexed together.
///
/// Within one [`SegmentManager`], an id is live in at most one segment. Writing a
/// newer version of a document deletes the older copy from the other segments.
#[derive(Debug, Serialize, Deserialize)]
pub struct Segment {
    /// Identifier of this segment.
    pub id: SegmentId,
    /// Indexed documents and their live/deleted state.
    pub collection: Collection,
}

impl Segment {
    /// Builds a segment by indexing `docs` in order.
    pub fn build(id: SegmentId, schema: Schema, docs: &[Document]) -> Result<Self, StorageError> {
        let mut collection = Collection::new(schema)?;
        for doc in docs {
            collection.upsert(doc)?;
        }
        Ok(Segment { id, collection })
    }

    /// Number of live documents.
    pub fn live_count(&self) -> usize {
        self.collection.len()
    }
}

/// Decides when and what to merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergePolicy {
    /// When at least `min_segments` exist, merge the smallest `max_segments` of them.
    /// Requires `2 <= min_segments <= max_segments`.
    SizeTiered {
        /// Segment count that triggers a merge.
        min_segments: usize,
        /// Maximum number of segments merged at once.
        max_segments: usize,
    },
    /// Merge runs of consecutive (by id) segments while their combined live
    /// document count stays within `target_size`.
    LogStructured {
        /// Maximum live documents in a merged segment.
        target_size: usize,
    },
}

impl MergePolicy {
    /// Picks the segments to merge next, given `(id, live_count)` pairs sorted
    /// by id. Returns `None` if no merge is warranted.
    pub fn select(&self, segments: &[(SegmentId, usize)]) -> Option<Vec<SegmentId>> {
        match *self {
            MergePolicy::SizeTiered {
                min_segments,
                max_segments,
            } => {
                if segments.len() < min_segments.max(2) {
                    return None;
                }
                let mut by_size = segments.to_vec();
                by_size.sort_by_key(|(_, count)| *count);
                let take = max_segments.max(2).min(by_size.len());
                Some(by_size[..take].iter().map(|(id, _)| *id).collect())
            }
            MergePolicy::LogStructured { target_size } => {
                let mut group: Vec<SegmentId> = Vec::new();
                let mut total = 0usize;
                for &(id, count) in segments {
                    if total + count > target_size && group.len() >= 2 {
                        return Some(group);
                    }
                    if total + count > target_size {
                        group.clear();
                        total = 0;
                    }
                    group.push(id);
                    total += count;
                }
                (group.len() >= 2).then_some(group)
            }
        }
    }
}

/// Owns a directory of segment files and the in-memory segments loaded from it.
#[derive(Debug)]
pub struct SegmentManager {
    dir: PathBuf,
    schema: Schema,
    policy: MergePolicy,
    segments: Vec<Segment>,
    dirty: BTreeSet<SegmentId>,
    removed: Vec<SegmentId>,
    next_id: u64,
}

impl SegmentManager {
    /// Opens (or creates) the segment directory and loads every segment file in it.
    pub fn open(dir: &Path, schema: Schema, policy: MergePolicy) -> Result<Self, StorageError> {
        fs::create_dir_all(dir)?;
        let mut segments = Vec::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "tseg") {
                segments.push(MappedSegment::open(&path)?.load()?);
            }
        }
        segments.sort_by_key(|s| s.id);
        let next_id = segments.last().map_or(1, |s| s.id.0 + 1);
        Ok(SegmentManager {
            dir: dir.to_path_buf(),
            schema,
            policy,
            segments,
            dirty: BTreeSet::new(),
            removed: Vec::new(),
            next_id,
        })
    }

    /// Loaded segments, oldest first.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Adds an existing segment. It is written to disk on the next [`flush`](Self::flush).
    pub fn add_segment(&mut self, segment: Segment) {
        self.next_id = self.next_id.max(segment.id.0 + 1);
        self.dirty.insert(segment.id);
        self.segments.push(segment);
        self.segments.sort_by_key(|s| s.id);
    }

    /// Indexes `docs` into new segments of at most `max_segment_docs` documents.
    ///
    /// Older copies of each document are deleted from existing segments first.
    /// Returns the ids of the new segments.
    pub fn ingest(
        &mut self,
        docs: &[Document],
        max_segment_docs: usize,
    ) -> Result<Vec<SegmentId>, StorageError> {
        let max = max_segment_docs.max(1);
        let mut created = Vec::new();
        for chunk in docs.chunks(max) {
            for doc in chunk {
                self.delete_everywhere(&doc.id);
            }
            let id = self.allocate_id();
            let segment = Segment::build(id, self.schema.clone(), chunk)?;
            self.add_segment(segment);
            created.push(id);
        }
        Ok(created)
    }

    /// Deletes a document from every segment that holds a live copy.
    pub fn delete(&mut self, id: &DocumentId) -> bool {
        self.delete_everywhere(id)
    }

    /// Searches every segment for an exact term and merges the results by score.
    pub fn search(&self, query: &TermQuery) -> Result<Vec<ScoredDocument>, StorageError> {
        let mut out = Vec::new();
        for segment in &self.segments {
            out.extend(segment.collection.search(query)?);
        }
        Ok(sort_hits(out))
    }

    /// Searches every segment with a boolean query and merges the results by score.
    pub fn boolean_search(
        &self,
        query: &BooleanQuery,
    ) -> Result<Vec<ScoredDocument>, StorageError> {
        let mut out = Vec::new();
        for segment in &self.segments {
            out.extend(segment.collection.boolean_search(query)?);
        }
        Ok(sort_hits(out))
    }

    /// Merges the given segments into one new segment and returns its id.
    ///
    /// Only live documents are copied, so deleted rows are reclaimed. The
    /// merged segment is re-indexed from source documents, so scores can change
    /// after a merge because the collection-wide statistics change.
    pub fn merge_segments(&mut self, ids: &[SegmentId]) -> Result<SegmentId, StorageError> {
        if ids.len() < 2 {
            return Err(StorageError::Corrupt(
                "a merge needs at least two segments".into(),
            ));
        }
        let wanted: BTreeSet<SegmentId> = ids.iter().copied().collect();
        let mut docs: Vec<Document> = Vec::new();
        for segment in self.segments.iter().filter(|s| wanted.contains(&s.id)) {
            docs.extend(segment.collection.live_sources().into_iter().cloned());
        }

        let id = self.allocate_id();
        let merged = Segment::build(id, self.schema.clone(), &docs)?;

        self.segments.retain(|s| !wanted.contains(&s.id));
        for old in wanted {
            self.dirty.remove(&old);
            self.removed.push(old);
        }
        self.add_segment(merged);
        Ok(id)
    }

    /// Runs the merge policy once. Returns the id of the merged segment, if any.
    pub fn maybe_merge(&mut self) -> Result<Option<SegmentId>, StorageError> {
        let sizes: Vec<(SegmentId, usize)> = self
            .segments
            .iter()
            .map(|s| (s.id, s.live_count()))
            .collect();
        match self.policy.select(&sizes) {
            Some(ids) => self.merge_segments(&ids).map(Some),
            None => Ok(None),
        }
    }

    /// Writes changed segments to disk and deletes files of merged-away segments.
    pub fn flush(&mut self) -> Result<(), StorageError> {
        fs::create_dir_all(&self.dir)?;
        for id in std::mem::take(&mut self.removed) {
            let path = self.segment_path(id);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        let dirty: Vec<SegmentId> = self.dirty.iter().copied().collect();
        for id in dirty {
            if let Some(segment) = self.segments.iter().find(|s| s.id == id) {
                segment.write_to(&self.segment_path(id))?;
            }
        }
        self.dirty.clear();
        Ok(())
    }

    /// File path a segment is stored at.
    pub fn segment_path(&self, id: SegmentId) -> PathBuf {
        self.dir.join(format!("seg-{:016x}.tseg", id.0))
    }

    fn allocate_id(&mut self) -> SegmentId {
        let id = SegmentId(self.next_id);
        self.next_id += 1;
        id
    }

    fn delete_everywhere(&mut self, id: &DocumentId) -> bool {
        let mut any = false;
        for segment in &mut self.segments {
            if segment.collection.delete(id) {
                self.dirty.insert(segment.id);
                any = true;
            }
        }
        any
    }
}

fn sort_hits(mut hits: Vec<ScoredDocument>) -> Vec<ScoredDocument> {
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.doc_id.cmp(&b.doc_id))
    });
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_search_core::{Analyzer, FieldSchema, FieldType};

    fn schema() -> Schema {
        Schema::new("s", "id")
            .with_field(FieldSchema::new("id", FieldType::Text))
            .unwrap()
            .with_field(
                FieldSchema::new("body", FieldType::Text)
                    .indexed()
                    .with_analyzer(Analyzer::standard()),
            )
            .unwrap()
    }

    fn doc(id: &str, body: &str) -> Document {
        Document::new(id).with_field("body", body)
    }

    #[test]
    fn size_tiered_merges_smallest_segments() {
        let policy = MergePolicy::SizeTiered {
            min_segments: 3,
            max_segments: 2,
        };
        let sizes = [(SegmentId(1), 10), (SegmentId(2), 1), (SegmentId(3), 5)];
        assert_eq!(
            policy.select(&sizes),
            Some(vec![SegmentId(2), SegmentId(3)])
        );
        assert_eq!(policy.select(&sizes[..2]), None);
    }

    #[test]
    fn log_structured_groups_within_target() {
        let policy = MergePolicy::LogStructured { target_size: 10 };
        let sizes = [(SegmentId(1), 4), (SegmentId(2), 4), (SegmentId(3), 9)];
        assert_eq!(
            policy.select(&sizes),
            Some(vec![SegmentId(1), SegmentId(2)])
        );
    }

    #[test]
    fn newer_version_supersedes_across_segments() {
        let dir = std::env::temp_dir().join(format!("tpt-seg-supersede-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut mgr = SegmentManager::open(
            &dir,
            schema(),
            MergePolicy::SizeTiered {
                min_segments: 8,
                max_segments: 8,
            },
        )
        .unwrap();
        mgr.ingest(&[doc("a", "old words")], 10).unwrap();
        mgr.ingest(&[doc("a", "new words")], 10).unwrap();

        let q = |t: &str| TermQuery {
            field: "body".into(),
            text: t.into(),
        };
        assert!(mgr.search(&q("old")).unwrap().is_empty());
        assert_eq!(mgr.search(&q("new")).unwrap().len(), 1);
        let live: usize = mgr.segments().iter().map(Segment::live_count).sum();
        assert_eq!(live, 1);
    }

    #[test]
    fn merge_reclaims_deleted_rows_and_flush_round_trips() {
        let dir = std::env::temp_dir().join(format!("tpt-seg-merge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let policy = MergePolicy::LogStructured { target_size: 100 };
        {
            let mut mgr = SegmentManager::open(&dir, schema(), policy).unwrap();
            mgr.ingest(&[doc("a", "alpha"), doc("b", "beta")], 10)
                .unwrap();
            mgr.ingest(&[doc("c", "gamma")], 10).unwrap();
            mgr.delete(&DocumentId::from("b"));
            let merged = mgr.maybe_merge().unwrap();
            assert!(merged.is_some());
            assert_eq!(mgr.segments().len(), 1);
            assert_eq!(mgr.segments()[0].live_count(), 2);
            mgr.flush().unwrap();
        }
        let mgr = SegmentManager::open(&dir, schema(), policy).unwrap();
        assert_eq!(mgr.segments().len(), 1);
        let q = TermQuery {
            field: "body".into(),
            text: "gamma".into(),
        };
        assert_eq!(mgr.search(&q).unwrap().len(), 1);
        let q = TermQuery {
            field: "body".into(),
            text: "beta".into(),
        };
        assert!(mgr.search(&q).unwrap().is_empty());
    }
}
