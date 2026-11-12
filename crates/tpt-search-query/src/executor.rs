//! Query execution over a registry of collections.
//!
//! Each plan node evaluates to a set of rows with per-row scores and highlights.
//! Set operations combine those sets. Scores are summed across the positive
//! branches they come from, and `Difference` drops the excluded rows.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use roaring::RoaringBitmap;
use tpt_search_core::{Document, DocumentId, QueryError};
use tpt_search_index::{Collection, Highlight, PhraseQuery, TermQuery};

use tpt_search_graph::{execute as execute_pattern, GraphError, GraphQuery, DEFAULT_MAX_MATCHES};

use crate::ast::FusionMethod;
use crate::plan::{PlanNode, QueryPlan, SortKey};
use crate::registry::IndexRegistry;

/// Default cap on rows any single plan node may produce.
pub const DEFAULT_MAX_CANDIDATES: usize = 10_000_000;

/// The outcome of a query.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryResult {
    /// Matching documents, in plan order and after any limit.
    pub documents: Vec<Document>,
    /// Number of matches before the limit was applied.
    pub total_hits: usize,
    /// Wall-clock execution time in milliseconds.
    pub took_ms: u64,
    /// Highlight spans per returned document.
    pub highlights: HashMap<DocumentId, Vec<Highlight>>,
}

/// Runs [`QueryPlan`]s against an [`IndexRegistry`].
#[derive(Debug, Clone, Copy)]
pub struct QueryExecutor<'a> {
    registry: &'a IndexRegistry,
    timeout: Option<Duration>,
    max_candidates: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Hits {
    rows: RoaringBitmap,
    scores: HashMap<u32, f32>,
    highlights: HashMap<u32, Vec<Highlight>>,
}

impl Hits {
    fn from_scored(scored: Vec<tpt_search_index::ScoredDocument>) -> Self {
        let mut hits = Hits::default();
        for s in scored {
            hits.rows.insert(s.row);
            hits.scores.insert(s.row, s.score);
            if !s.highlights.is_empty() {
                hits.highlights.insert(s.row, s.highlights);
            }
        }
        hits
    }

    fn from_rows(rows: RoaringBitmap) -> Self {
        Hits {
            rows,
            ..Hits::default()
        }
    }

    fn and(mut self, other: Hits) -> Hits {
        self.rows &= &other.rows;
        self.scores.retain(|r, _| self.rows.contains(*r));
        self.highlights.retain(|r, _| self.rows.contains(*r));
        merge_scores(&mut self, other, false);
        self
    }

    fn or(mut self, other: Hits) -> Hits {
        self.rows |= &other.rows;
        merge_scores(&mut self, other, true);
        self
    }

    fn minus(mut self, other: &Hits) -> Hits {
        self.rows -= &other.rows;
        self.scores.retain(|r, _| self.rows.contains(*r));
        self.highlights.retain(|r, _| self.rows.contains(*r));
        self
    }
}

/// Adds `other`'s scores and highlights into `into` for rows that survive in `into`.
/// When `keep_all` is true, rows only present in `other` are kept too.
fn merge_scores(into: &mut Hits, other: Hits, keep_all: bool) {
    for (row, score) in other.scores {
        if keep_all || into.rows.contains(row) {
            *into.scores.entry(row).or_insert(0.0) += score;
        }
    }
    for (row, spans) in other.highlights {
        if keep_all || into.rows.contains(row) {
            into.highlights.entry(row).or_default().extend(spans);
        }
    }
}

impl<'a> QueryExecutor<'a> {
    /// Creates an executor with no timeout and the default candidate limit.
    pub fn new(registry: &'a IndexRegistry) -> Self {
        QueryExecutor {
            registry,
            timeout: None,
            max_candidates: DEFAULT_MAX_CANDIDATES,
        }
    }

    /// Fails with [`QueryError::Timeout`] if execution takes longer than `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Fails with [`QueryError::ResourceExhausted`] if any node produces more rows.
    pub fn with_max_candidates(mut self, max: usize) -> Self {
        self.max_candidates = max;
        self
    }

    /// Executes a plan and returns the matching documents.
    pub fn execute(&self, plan: &QueryPlan) -> Result<QueryResult, QueryError> {
        let started = Instant::now();
        let deadline = self.timeout.map(|t| started + t);
        let collection = self.collection(&plan.index)?;

        let (sort, limit, body) = split_root(&plan.root)?;
        let hits = self.eval(body, &plan.index, collection, deadline)?;
        let total_hits = hits.rows.len() as usize;

        let mut rows: Vec<u32> = hits.rows.iter().collect();
        if let Some(keys) = sort {
            sort_rows(&mut rows, keys, &hits, collection);
        }
        if let Some(limit) = limit {
            rows.truncate(limit);
        }

        let mut documents = Vec::with_capacity(rows.len());
        let mut highlights = HashMap::new();
        for row in rows {
            if let Some(doc) = collection.document_at(row) {
                if let Some(spans) = hits.highlights.get(&row) {
                    highlights.insert(doc.id.clone(), spans.clone());
                }
                documents.push(doc);
            }
        }

        Ok(QueryResult {
            documents,
            total_hits,
            took_ms: started.elapsed().as_millis() as u64,
            highlights,
        })
    }

    fn collection(&self, name: &str) -> Result<&'a Collection, QueryError> {
        self.registry
            .get(name)
            .ok_or_else(|| QueryError::UnknownIndex(name.to_owned()))
    }

    /// Runs a graph pattern on the collection's graph and joins the return variable's
    /// nodes to live documents with the same id. Each match adds one to the score.
    fn graph_hits(
        &self,
        index: &str,
        collection: &Collection,
        query: &GraphQuery,
        return_var: &str,
    ) -> Result<Hits, QueryError> {
        let Some(graph) = self.registry.graph(index) else {
            return Ok(Hits::default());
        };
        let bindings = execute_pattern(graph, query, DEFAULT_MAX_MATCHES).map_err(graph_error)?;
        let mut counts: HashMap<u32, usize> = HashMap::new();
        for binding in &bindings {
            if let Some(id) = binding.get(return_var) {
                if let Some(row) = collection.row_of(&DocumentId::from(id.as_str())) {
                    *counts.entry(row).or_default() += 1;
                }
            }
        }
        let mut hits = Hits::default();
        for (row, count) in counts {
            hits.rows.insert(row);
            hits.scores.insert(row, count as f32);
        }
        Ok(hits)
    }

    fn eval(
        &self,
        node: &PlanNode,
        index: &str,
        collection: &Collection,
        deadline: Option<Instant>,
    ) -> Result<Hits, QueryError> {
        if deadline.is_some_and(|d| Instant::now() > d) {
            return Err(QueryError::Timeout);
        }
        let recurse = |n: &PlanNode| self.eval(n, index, collection, deadline);
        let hits = match node {
            PlanNode::Scan { predicate, .. } => {
                Hits::from_rows(collection.scan(predicate).map_err(QueryError::Index)?)
            }
            PlanNode::IndexLookup { term, .. } => Hits::from_scored(
                collection
                    .search(&TermQuery {
                        field: term.field.clone(),
                        text: term.text.clone(),
                    })
                    .map_err(QueryError::Index)?,
            ),
            PlanNode::PhraseLookup { field, text, .. } => Hits::from_scored(
                collection
                    .phrase_search(&PhraseQuery {
                        field: field.clone(),
                        text: text.clone(),
                    })
                    .map_err(QueryError::Index)?,
            ),
            PlanNode::All { .. } => Hits::from_rows(collection.live_rows()),
            PlanNode::Intersect(children) => {
                let mut acc: Option<Hits> = None;
                for child in children {
                    let h = recurse(child)?;
                    acc = Some(match acc {
                        None => h,
                        Some(a) => a.and(h),
                    });
                }
                acc.unwrap_or_default()
            }
            PlanNode::Union(children) => {
                let mut acc = Hits::default();
                for child in children {
                    acc = acc.or(recurse(child)?);
                }
                acc
            }
            PlanNode::Difference { input, exclude } => {
                let keep = recurse(input)?;
                let drop = recurse(exclude)?;
                keep.minus(&drop)
            }
            PlanNode::GraphMatch {
                query, return_var, ..
            } => self.graph_hits(index, collection, query, return_var)?,
            PlanNode::VectorSearch {
                field,
                vector,
                k,
                ef,
                ..
            } => Hits::from_scored(
                collection
                    .knn(field, vector, *k, *ef)
                    .map_err(QueryError::Index)?,
            ),
            PlanNode::Fusion { inputs, method } => {
                let mut lists = Vec::with_capacity(inputs.len());
                for input in inputs {
                    lists.push(recurse(input)?);
                }
                fuse(lists, method)
            }
            PlanNode::Sort { .. } | PlanNode::Limit { .. } => {
                return Err(QueryError::InvalidPlan(
                    "Sort and Limit may only appear at the root".into(),
                ))
            }
        };
        if hits.rows.len() as usize > self.max_candidates {
            return Err(QueryError::ResourceExhausted(format!(
                "{} candidate rows exceeds the limit of {}",
                hits.rows.len(),
                self.max_candidates
            )));
        }
        Ok(hits)
    }
}

/// Sort keys, optional limit, and plan body, as split from the root node.
type SplitRoot<'a> = (Option<&'a [SortKey]>, Option<usize>, &'a PlanNode);

/// Fuses ranked inputs according to `method`. Every method returns a score where
/// larger is better, so the root sort orders the fused result.
fn fuse(lists: Vec<Hits>, method: &FusionMethod) -> Hits {
    match method {
        FusionMethod::ReciprocalRankFusion { k } => rrf(lists, *k),
        FusionMethod::LinearCombination { weights } => linear(lists, weights),
        FusionMethod::Interleaving => interleave(lists),
    }
}

/// Row numbers ordered by score, best first, ties by row number.
fn ranked(list: &Hits) -> Vec<u32> {
    let mut rows: Vec<u32> = list.rows.iter().collect();
    rows.sort_by(|a, b| {
        let sa = list.scores.get(a).copied().unwrap_or(0.0);
        let sb = list.scores.get(b).copied().unwrap_or(0.0);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(b))
    });
    rows
}

/// Moves a list's highlight spans into the output.
fn absorb_highlights(out: &mut Hits, list: Hits) {
    for (row, spans) in list.highlights {
        out.highlights.entry(row).or_default().extend(spans);
    }
}

/// Reciprocal rank fusion: each input contributes `1 / (k + rank)`, rank starting at 1.
fn rrf(lists: Vec<Hits>, k: usize) -> Hits {
    let mut out = Hits::default();
    for list in lists {
        for (rank, row) in ranked(&list).iter().enumerate() {
            out.rows.insert(*row);
            *out.scores.entry(*row).or_insert(0.0) += 1.0 / (k as f32 + rank as f32 + 1.0);
        }
        absorb_highlights(&mut out, list);
    }
    out
}

/// Weighted sum of min-max normalized scores, one weight per input (default 1.0).
/// An input whose scores are all equal contributes its full weight to each row.
fn linear(lists: Vec<Hits>, weights: &[f32]) -> Hits {
    let mut out = Hits::default();
    for (i, list) in lists.into_iter().enumerate() {
        let weight = weights.get(i).copied().unwrap_or(1.0);
        let values: Vec<f32> = list
            .rows
            .iter()
            .map(|r| list.scores.get(&r).copied().unwrap_or(0.0))
            .collect();
        let lo = values.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        for row in list.rows.iter() {
            let score = list.scores.get(&row).copied().unwrap_or(0.0);
            let norm = if hi > lo {
                (score - lo) / (hi - lo)
            } else {
                1.0
            };
            out.rows.insert(row);
            *out.scores.entry(row).or_insert(0.0) += weight * norm;
        }
        absorb_highlights(&mut out, list);
    }
    out
}

/// Round-robin across the ranked inputs, skipping documents already taken. Earlier
/// positions score higher.
fn interleave(lists: Vec<Hits>) -> Hits {
    let rankings: Vec<Vec<u32>> = lists.iter().map(ranked).collect();
    let longest = rankings.iter().map(Vec::len).max().unwrap_or(0);
    let mut order = Vec::new();
    let mut taken = HashSet::new();
    for position in 0..longest {
        for ranking in &rankings {
            if let Some(&row) = ranking.get(position) {
                if taken.insert(row) {
                    order.push(row);
                }
            }
        }
    }
    let mut out = Hits::default();
    for (i, row) in order.iter().enumerate() {
        out.rows.insert(*row);
        out.scores.insert(*row, (order.len() - i) as f32);
    }
    for list in lists {
        absorb_highlights(&mut out, list);
    }
    out
}

/// Maps graph-layer errors onto query errors.
fn graph_error(e: GraphError) -> QueryError {
    match e {
        GraphError::TooManyMatches(n) => {
            QueryError::ResourceExhausted(format!("graph pattern matched more than {n} rows"))
        }
        other => QueryError::ParseError(other.to_string()),
    }
}

/// Splits the optional root `Limit` and `Sort` from the body of a plan.
fn split_root(root: &PlanNode) -> Result<SplitRoot<'_>, QueryError> {
    let (limit, rest) = match root {
        PlanNode::Limit { input, limit } => (Some(*limit), input.as_ref()),
        other => (None, other),
    };
    match rest {
        PlanNode::Sort { input, keys } => Ok((Some(keys.as_slice()), limit, input.as_ref())),
        body => Ok((None, limit, body)),
    }
}

fn sort_rows(rows: &mut [u32], keys: &[SortKey], hits: &Hits, collection: &Collection) {
    rows.sort_by(|&a, &b| {
        for key in keys {
            let ordering = match &key.field {
                None => {
                    let sa = hits.scores.get(&a).copied().unwrap_or(0.0);
                    let sb = hits.scores.get(&b).copied().unwrap_or(0.0);
                    sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
                }
                Some(field) => {
                    // Nulls sort last regardless of direction.
                    match (
                        collection.columns().key_at(field, a),
                        collection.columns().key_at(field, b),
                    ) {
                        (Some(x), Some(y)) => x.cmp(&y),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    }
                }
            };
            let ordering = if key.descending {
                ordering.reverse()
            } else {
                ordering
            };
            if ordering != std::cmp::Ordering::Equal {
                return ordering;
            }
        }
        a.cmp(&b)
    });
}
