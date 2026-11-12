//! Query planning: turns a [`Query`] into a tree of [`PlanNode`]s.
//!
//! Phase 1 planning is rule-based. It validates fields against the schema,
//! chooses between index lookups and column scans, and estimates cost from
//! posting-list sizes. A cost-based rewrite (`optimize`) is Phase 6.

use std::ops::Bound;

use tpt_search_core::{FieldType, FieldValue, IndexError, QueryError};
use tpt_search_index::{Collection, Predicate, Term};

use crate::ast::{FusionMethod, Query};
use tpt_search_graph::GraphQuery;

/// Sort criterion for the final result ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    /// Scalar field to sort by; `None` sorts by relevance score.
    pub field: Option<String>,
    /// Whether to sort in descending order.
    pub descending: bool,
}

/// A node in a physical plan.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanNode {
    /// Rows of a collection satisfying a scalar predicate.
    Scan {
        /// Collection name.
        index: String,
        /// Predicate over scalar fields.
        predicate: Predicate,
    },
    /// Documents containing an exact term, with BM25 scores.
    IndexLookup {
        /// Collection name.
        index: String,
        /// Exact term.
        term: Term,
    },
    /// Documents containing an analyzed phrase.
    PhraseLookup {
        /// Collection name.
        index: String,
        /// Text field.
        field: String,
        /// Phrase text, analyzed at execution time.
        text: String,
    },
    /// Every live document.
    All {
        /// Collection name.
        index: String,
    },
    /// Documents present in every child.
    Intersect(Vec<PlanNode>),
    /// Documents present in any child.
    Union(Vec<PlanNode>),
    /// Documents in `input` that are not in `exclude`.
    Difference {
        /// Documents to keep.
        input: Box<PlanNode>,
        /// Documents to remove.
        exclude: Box<PlanNode>,
    },
    /// Orders the rows produced by `input`. Only valid at the root.
    Sort {
        /// Child node.
        input: Box<PlanNode>,
        /// Sort keys, most significant first.
        keys: Vec<SortKey>,
    },
    /// Graph pattern match; matched nodes join to documents by id, scored by match count.
    GraphMatch {
        /// Collection (and graph) name.
        index: String,
        /// Parsed pattern.
        query: GraphQuery,
        /// Variable whose bindings are joined to documents.
        return_var: String,
    },
    /// Top-k nearest neighbours from a vector index, scored by similarity.
    VectorSearch {
        /// Collection name.
        index: String,
        /// Indexed vector field.
        field: String,
        /// Query vector.
        vector: Vec<f32>,
        /// Number of neighbours.
        k: usize,
        /// Beam width; `None` uses the index default.
        ef: Option<usize>,
    },
    /// Ranks each input and fuses the rankings into one score per document.
    Fusion {
        /// Ranked inputs.
        inputs: Vec<PlanNode>,
        /// Fusion method.
        method: FusionMethod,
    },
    /// Keeps the first `limit` rows of `input`. Only valid at the root.
    Limit {
        /// Child node.
        input: Box<PlanNode>,
        /// Maximum rows to return.
        limit: usize,
    },
}

/// A plan ready for execution.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPlan {
    /// Collection the plan runs against.
    pub index: String,
    /// Root of the plan tree.
    pub root: PlanNode,
    /// Relative work estimate (posting entries and rows touched).
    pub estimated_cost: f64,
    /// Estimated number of matching rows before any limit.
    pub estimated_cardinality: usize,
}

impl QueryPlan {
    /// Caps the number of rows the plan returns.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.root = PlanNode::Limit {
            input: Box::new(self.root),
            limit,
        };
        self
    }
}

/// Builds plans for queries against a single collection.
#[derive(Debug, Default, Clone, Copy)]
pub struct QueryPlanner;

impl QueryPlanner {
    /// Creates a planner.
    pub fn new() -> Self {
        QueryPlanner
    }

    /// Plans `query` against `collection`. Results are ordered by score, descending.
    ///
    /// Boolean semantics follow [`Query::Boolean`]. When `must` clauses are present,
    /// `should` clauses are ignored for matching (they only add to scores in the
    /// index-level `boolean_search`).
    pub fn plan(
        &self,
        index: &str,
        collection: &Collection,
        query: &Query,
    ) -> Result<QueryPlan, QueryError> {
        let ctx = Ctx { index, collection };
        let root = ctx.node(query)?;
        let estimated_cardinality = ctx.cardinality(&root);
        let estimated_cost = ctx.cost(&root);
        let root = PlanNode::Sort {
            input: Box::new(root),
            keys: vec![SortKey {
                field: None,
                descending: true,
            }],
        };
        Ok(QueryPlan {
            index: index.to_owned(),
            root,
            estimated_cost,
            estimated_cardinality,
        })
    }
}

pub(crate) struct Ctx<'a> {
    pub(crate) index: &'a str,
    pub(crate) collection: &'a Collection,
}

impl Ctx<'_> {
    fn node(&self, query: &Query) -> Result<PlanNode, QueryError> {
        let index = self.index.to_owned();
        Ok(match query {
            Query::Term { field, value } => match self.field_type(field)? {
                FieldType::Text => {
                    let text = value
                        .as_text()
                        .ok_or_else(|| type_mismatch(FieldType::Text, value))?;
                    self.require_indexed_text(field)?;
                    PlanNode::IndexLookup {
                        index,
                        term: Term::new(field.clone(), text),
                    }
                }
                ty if is_scalar(&ty) => PlanNode::Scan {
                    index,
                    predicate: Predicate::Equals {
                        field: field.clone(),
                        value: value.clone(),
                    },
                },
                _ => return Err(QueryError::InvalidField(field.clone())),
            },
            Query::Range { field, start, end } => {
                let ty = self.field_type(field)?;
                if !is_scalar(&ty) {
                    return Err(QueryError::InvalidField(field.clone()));
                }
                check_bound(&ty, start)?;
                check_bound(&ty, end)?;
                PlanNode::Scan {
                    index,
                    predicate: Predicate::Range {
                        field: field.clone(),
                        start: start.clone(),
                        end: end.clone(),
                    },
                }
            }
            Query::Match { field, text } => {
                self.require_indexed_text(field)?;
                let tokens = self
                    .collection
                    .inverted()
                    .analyze_text(field, text)
                    .map_err(index_err)?;
                let mut seen = std::collections::BTreeSet::new();
                let children: Vec<PlanNode> = tokens
                    .into_iter()
                    .filter(|t| seen.insert(t.term.clone()))
                    .map(|t| PlanNode::IndexLookup {
                        index: index.clone(),
                        term: Term::new(field.clone(), t.term),
                    })
                    .collect();
                // No tokens (for example, only stop words) means nothing can match.
                PlanNode::Union(children)
            }
            Query::Phrase { field, text } => {
                self.require_indexed_text(field)?;
                PlanNode::PhraseLookup {
                    index,
                    field: field.clone(),
                    text: text.clone(),
                }
            }
            Query::Graph(graph) => PlanNode::GraphMatch {
                index,
                query: graph.clone(),
                return_var: graph.returns[0].clone(),
            },
            Query::Knn {
                field,
                vector,
                k,
                ef,
            } => {
                let FieldType::Vector(dim) = self.field_type(field)? else {
                    return Err(QueryError::InvalidField(field.clone()));
                };
                if !self
                    .collection
                    .schema()
                    .field(field)
                    .is_some_and(|f| f.indexed)
                {
                    return Err(QueryError::InvalidField(field.clone()));
                }
                if vector.len() != dim as usize {
                    return Err(QueryError::TypeMismatch {
                        expected: FieldType::Vector(dim),
                        got: FieldType::Vector(vector.len() as u32),
                    });
                }
                PlanNode::VectorSearch {
                    index,
                    field: field.clone(),
                    vector: vector.clone(),
                    k: *k,
                    ef: *ef,
                }
            }
            Query::Hybrid {
                scalar,
                vector,
                graph,
                fusion,
            } => {
                let mut inputs = Vec::new();
                if let Some(s) = scalar {
                    inputs.push(self.node(s)?);
                }
                if let Some(v) = vector {
                    inputs.push(self.node(v)?);
                }
                if let Some(g) = graph {
                    inputs.push(self.node(g)?);
                }
                if inputs.is_empty() {
                    return Err(QueryError::ParseError("HYBRID needs an input".into()));
                }
                if let FusionMethod::LinearCombination { weights } = fusion {
                    if !weights.is_empty() && weights.len() != inputs.len() {
                        return Err(QueryError::ParseError(format!(
                            "LINEAR has {} weights for {} inputs",
                            weights.len(),
                            inputs.len()
                        )));
                    }
                }
                PlanNode::Fusion {
                    inputs,
                    method: fusion.clone(),
                }
            }
            Query::Boolean {
                must,
                should,
                must_not,
            } => {
                let base = if !must.is_empty() {
                    PlanNode::Intersect(self.nodes(must)?)
                } else if !should.is_empty() {
                    PlanNode::Union(self.nodes(should)?)
                } else {
                    PlanNode::All { index }
                };
                if must_not.is_empty() {
                    base
                } else {
                    PlanNode::Difference {
                        input: Box::new(base),
                        exclude: Box::new(PlanNode::Union(self.nodes(must_not)?)),
                    }
                }
            }
        })
    }

    fn nodes(&self, queries: &[Query]) -> Result<Vec<PlanNode>, QueryError> {
        queries.iter().map(|q| self.node(q)).collect()
    }

    fn field_type(&self, field: &str) -> Result<FieldType, QueryError> {
        self.collection
            .schema()
            .field(field)
            .map(|f| f.field_type.clone())
            .ok_or_else(|| QueryError::InvalidField(field.to_owned()))
    }

    fn require_indexed_text(&self, field: &str) -> Result<(), QueryError> {
        let schema_field = self
            .collection
            .schema()
            .field(field)
            .ok_or_else(|| QueryError::InvalidField(field.to_owned()))?;
        if schema_field.field_type != FieldType::Text || !schema_field.indexed {
            return Err(QueryError::InvalidField(field.to_owned()));
        }
        Ok(())
    }

    /// Estimated number of rows a node produces.
    pub(crate) fn cardinality(&self, node: &PlanNode) -> usize {
        let total = self.collection.len();
        match node {
            PlanNode::Scan { predicate, .. } => match scan_field(predicate) {
                Some(f) if self.collection.schema().field(f).is_some_and(|s| s.indexed) => {
                    total / 10
                }
                _ => total,
            },
            PlanNode::IndexLookup { term, .. } => self
                .collection
                .inverted()
                .postings(term)
                .map_or(0, |p| p.doc_ids.len() as usize),
            PlanNode::PhraseLookup { .. } => total / 10,
            PlanNode::All { .. } => total,
            PlanNode::Intersect(children) => children
                .iter()
                .map(|c| self.cardinality(c))
                .min()
                .unwrap_or(0),
            PlanNode::Union(children) => children
                .iter()
                .map(|c| self.cardinality(c))
                .sum::<usize>()
                .min(total),
            PlanNode::Difference { input, .. } => self.cardinality(input),
            PlanNode::Sort { input, .. } => self.cardinality(input),
            PlanNode::GraphMatch { .. } => total / 10,
            PlanNode::VectorSearch { k, .. } => (*k).min(total),
            PlanNode::Fusion { inputs, .. } => inputs
                .iter()
                .map(|i| self.cardinality(i))
                .sum::<usize>()
                .min(total),
            PlanNode::Limit { input, limit } => self.cardinality(input).min(*limit),
        }
    }

    /// Relative work: the sum of the rows each leaf reads.
    pub(crate) fn cost(&self, node: &PlanNode) -> f64 {
        match node {
            PlanNode::Scan { .. } | PlanNode::All { .. } | PlanNode::PhraseLookup { .. } => {
                self.cardinality(node).max(1) as f64 + self.collection.len() as f64 * 0.01
            }
            PlanNode::IndexLookup { .. } => self.cardinality(node).max(1) as f64,
            PlanNode::Intersect(children) | PlanNode::Union(children) => {
                children.iter().map(|c| self.cost(c)).sum::<f64>() + 1.0
            }
            PlanNode::Difference { input, exclude } => self.cost(input) + self.cost(exclude) + 1.0,
            PlanNode::GraphMatch { .. } => self.collection.len() as f64 + 1.0,
            PlanNode::VectorSearch { k, .. } => {
                let n = self.collection.len().max(2) as f64;
                (*k as f64).max(1.0) * n.log2() * 4.0
            }
            PlanNode::Fusion { inputs, .. } => {
                inputs.iter().map(|i| self.cost(i)).sum::<f64>() + 1.0
            }
            PlanNode::Sort { input, .. } | PlanNode::Limit { input, .. } => self.cost(input) + 1.0,
        }
    }
}

fn scan_field(predicate: &Predicate) -> Option<&str> {
    match predicate {
        Predicate::Equals { field, .. }
        | Predicate::In { field, .. }
        | Predicate::Range { field, .. } => Some(field),
        _ => None,
    }
}

fn is_scalar(ty: &FieldType) -> bool {
    matches!(
        ty,
        FieldType::Integer | FieldType::Float | FieldType::Boolean | FieldType::DateTime
    )
}

fn check_bound(ty: &FieldType, bound: &Bound<FieldValue>) -> Result<(), QueryError> {
    match bound {
        Bound::Included(v) | Bound::Excluded(v) => {
            if ty.matches(v).unwrap_or(false) && !matches!(v, FieldValue::Null) {
                Ok(())
            } else {
                Err(type_mismatch(ty.clone(), v))
            }
        }
        Bound::Unbounded => Ok(()),
    }
}

fn type_mismatch(expected: FieldType, value: &FieldValue) -> QueryError {
    QueryError::TypeMismatch {
        expected,
        got: match value {
            FieldValue::Text(_) => FieldType::Text,
            FieldValue::Integer(_) => FieldType::Integer,
            FieldValue::Float(_) => FieldType::Float,
            FieldValue::Boolean(_) => FieldType::Boolean,
            FieldValue::DateTime(_) => FieldType::DateTime,
            FieldValue::Vector(v) => FieldType::Vector(v.len() as u32),
            _ => FieldType::Text,
        },
    }
}

fn index_err(e: IndexError) -> QueryError {
    QueryError::Index(e)
}
