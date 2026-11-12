//! Query abstract syntax tree (scalar, full-text, vector, and hybrid).

use std::ops::Bound;

use tpt_search_core::FieldValue;
use tpt_search_graph::GraphQuery;

/// A query over one or more collections.
///
/// Graph queries arrive in Phase 3.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    /// Exact match. On a text field the value is an exact term (not analyzed);
    /// on a scalar field it is an equality predicate.
    Term {
        /// Field to match.
        field: String,
        /// Value to match.
        value: FieldValue,
    },
    /// Scalar range predicate.
    Range {
        /// Scalar field.
        field: String,
        /// Lower bound.
        start: Bound<FieldValue>,
        /// Upper bound.
        end: Bound<FieldValue>,
    },
    /// Full-text match: the text is analyzed and any resulting term may match.
    Match {
        /// Indexed text field.
        field: String,
        /// Free text to analyze.
        text: String,
    },
    /// Full-text phrase: the analyzed tokens must appear consecutively.
    Phrase {
        /// Indexed text field.
        field: String,
        /// Phrase text to analyze.
        text: String,
    },
    /// Boolean combination. With `must` clauses, all must match. Without them,
    /// at least one `should` clause must match. With neither, every live document
    /// matches except those excluded by `must_not`.
    Boolean {
        /// Required sub-queries.
        must: Vec<Query>,
        /// Optional sub-queries (see above).
        should: Vec<Query>,
        /// Excluded sub-queries.
        must_not: Vec<Query>,
    },
    /// Approximate k-nearest-neighbour search over an indexed vector field.
    Knn {
        /// Indexed vector field.
        field: String,
        /// Query vector.
        vector: Vec<f32>,
        /// Number of neighbours to return.
        k: usize,
        /// Search beam width; `None` uses the index default.
        ef: Option<usize>,
    },
    /// Graph pattern match, with each matched node joined to the document of the same id.
    Graph(GraphQuery),
    /// Fusion of scalar, vector, and graph inputs into one ranking.
    Hybrid {
        /// Scalar (full-text, term, range, or boolean) input, if any.
        scalar: Option<Box<Query>>,
        /// `Knn` input, if any.
        vector: Option<Box<Query>>,
        /// `Graph` input, if any.
        graph: Option<Box<Query>>,
        /// How the input rankings are combined.
        fusion: FusionMethod,
    },
}

/// How the rankings of a hybrid query are combined.
#[derive(Debug, Clone, PartialEq)]
pub enum FusionMethod {
    /// Reciprocal rank fusion: each input contributes `1 / (k + rank)` for the
    /// rank of a document within that input. Rank starts at 1.
    ReciprocalRankFusion {
        /// Smoothing constant. The conventional value is 60.
        k: usize,
    },
    /// Weighted sum of min-max normalized scores, one weight per input in input
    /// order. Inputs without a weight count as 1.0.
    LinearCombination {
        /// Weights, in input order.
        weights: Vec<f32>,
    },
    /// Round-robin across the ranked inputs, skipping documents already taken.
    Interleaving,
}

impl Default for FusionMethod {
    fn default() -> Self {
        FusionMethod::ReciprocalRankFusion { k: 60 }
    }
}
