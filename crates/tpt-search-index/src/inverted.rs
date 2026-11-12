//! Inverted index: maps analyzed terms to the documents that contain them,
//! with BM25 scoring, phrase matching, and boolean combination.

use std::collections::HashMap;
use std::sync::Arc;

use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use tpt_search_core::{
    Analyzer, Document, DocumentId, FieldName, FieldSchema, FieldType, FieldValue, IndexError,
    Schema, Token,
};

/// BM25 term-frequency saturation parameter.
const K1: f64 = 1.2;
/// BM25 length-normalization parameter.
const B: f64 = 0.75;

/// An analyzed term within a specific field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Term {
    /// Field the term came from.
    pub field: FieldName,
    /// Normalized term text.
    pub text: String,
}

impl Term {
    /// Creates a term. The text is used as-is; it is not analyzed.
    pub fn new(field: impl Into<FieldName>, text: impl Into<String>) -> Self {
        Term {
            field: field.into(),
            text: text.into(),
        }
    }
}

/// Postings for one term: which rows contain it and where.
///
/// Rows are internal, dense, insertion-ordered document numbers assigned by
/// [`InvertedIndex::index_document`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PostingList {
    /// Rows containing the term.
    pub doc_ids: RoaringBitmap,
    /// Row -> sorted token positions of the term.
    pub positions: HashMap<u32, Vec<u32>>,
    /// Row -> number of occurrences.
    pub frequencies: HashMap<u32, u32>,
    /// Row -> byte offsets of each occurrence, parallel to `positions`.
    pub offsets: HashMap<u32, Vec<(usize, usize)>>,
}

/// A single exact-term query on one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermQuery {
    /// Field to search.
    pub field: FieldName,
    /// Term text, matched exactly (no analysis).
    pub text: String,
}

/// A phrase query: the text is analyzed and its tokens must appear consecutively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhraseQuery {
    /// Field to search.
    pub field: FieldName,
    /// Phrase text, analyzed with the field's analyzer.
    pub text: String,
}

/// A boolean combination of exact-term clauses.
///
/// Semantics: every `must` clause must match. If there are no `must` clauses,
/// at least one `should` clause must match. `should` clauses also add to the
/// score when `must` clauses are present, but do not filter. Any `must_not`
/// match excludes the document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BooleanQuery {
    /// Required clauses.
    pub must: Vec<TermQuery>,
    /// Optional clauses (required only when there are no `must` clauses).
    pub should: Vec<TermQuery>,
    /// Excluding clauses.
    pub must_not: Vec<TermQuery>,
}

/// A matched document with its score and highlight spans.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredDocument {
    /// External document identifier.
    pub doc_id: DocumentId,
    /// Internal row number, used by collections to filter deleted documents.
    pub row: u32,
    /// Relevance score; higher is better.
    pub score: f32,
    /// Matched spans, one entry per field that matched.
    pub highlights: Vec<Highlight>,
}

/// Byte ranges in a field's original text that matched the query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Highlight {
    /// Field the spans belong to.
    pub field: FieldName,
    /// `(start, end)` byte offsets, end exclusive.
    pub positions: Vec<(usize, usize)>,
}

type Entry = (f32, Vec<Highlight>);
/// Term text -> (token positions, byte offsets) while indexing one field.
type TokenGroups = HashMap<String, (Vec<u32>, Vec<(usize, usize)>)>;

/// Full-text inverted index over the indexed text fields of a schema.
#[derive(Debug, Serialize, Deserialize)]
pub struct InvertedIndex {
    schema: Arc<Schema>,
    analyzers: HashMap<FieldName, Analyzer>,
    terms: HashMap<Term, PostingList>,
    doc_ids: Vec<DocumentId>,
    field_lengths: HashMap<FieldName, Vec<u32>>,
    total_tokens: HashMap<FieldName, u64>,
}

impl InvertedIndex {
    /// Creates an empty index for `schema`. The schema must already be valid.
    pub fn new(schema: Arc<Schema>) -> Self {
        let analyzers = schema
            .fields
            .values()
            .filter(|f| is_text_index(f))
            .map(|f| {
                (
                    f.name.clone(),
                    f.analyzer.clone().unwrap_or_else(Analyzer::standard),
                )
            })
            .collect();
        InvertedIndex {
            schema,
            analyzers,
            terms: HashMap::new(),
            doc_ids: Vec::new(),
            field_lengths: HashMap::new(),
            total_tokens: HashMap::new(),
        }
    }

    /// Number of rows ever indexed, including rows later superseded by the
    /// collection layer.
    pub fn row_count(&self) -> u32 {
        self.doc_ids.len() as u32
    }

    /// External identifier of a row.
    pub fn doc_id(&self, row: u32) -> Option<&DocumentId> {
        self.doc_ids.get(row as usize)
    }

    /// Postings for a term, if it occurs in any row.
    pub fn postings(&self, term: &Term) -> Option<&PostingList> {
        self.terms.get(term)
    }

    /// Analyzes text with a text field's analyzer.
    pub fn analyze_text(&self, field: &str, text: &str) -> Result<Vec<Token>, IndexError> {
        self.check_text_field(field)?;
        Ok(self.analyzers[field].analyze(text))
    }

    /// Analyzes and indexes the text fields of `doc`, returning its new row.
    ///
    /// The document is validated against the schema first, so an error leaves
    /// the index unchanged.
    pub fn index_document(&mut self, doc: &Document) -> Result<u32, IndexError> {
        self.schema.validate_document(doc)?;
        let row = self.row_count();

        let mut analyzed: Vec<(FieldName, Vec<Token>)> = Vec::with_capacity(self.analyzers.len());
        for (name, analyzer) in &self.analyzers {
            let text = doc.get(name).and_then(FieldValue::as_text).unwrap_or("");
            analyzed.push((name.clone(), analyzer.analyze(text)));
        }

        for (name, tokens) in analyzed {
            let len = tokens.len() as u32;
            *self.total_tokens.entry(name.clone()).or_insert(0) += u64::from(len);
            self.field_lengths
                .entry(name.clone())
                .or_default()
                .push(len);

            let mut grouped: TokenGroups = HashMap::new();
            for token in tokens {
                let entry = grouped.entry(token.term).or_default();
                entry.0.push(token.position);
                entry.1.push((token.start, token.end));
            }
            for (text, (positions, offsets)) in grouped {
                let frequency = positions.len() as u32;
                let posting = self.terms.entry(Term::new(name.clone(), text)).or_default();
                posting.doc_ids.insert(row);
                posting.frequencies.insert(row, frequency);
                posting.positions.insert(row, positions);
                posting.offsets.insert(row, offsets);
            }
        }

        self.doc_ids.push(doc.id.clone());
        Ok(row)
    }

    /// BM25 contribution of `term` to `row`; zero if the row does not contain it.
    pub fn bm25(&self, term: &Term, row: u32) -> f32 {
        let Some(posting) = self.terms.get(term) else {
            return 0.0;
        };
        let Some(&tf) = posting.frequencies.get(&row) else {
            return 0.0;
        };
        let n = self.doc_ids.len() as f64;
        let df = posting.doc_ids.len() as f64;
        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();

        let doc_len = self
            .field_lengths
            .get(&term.field)
            .and_then(|lens| lens.get(row as usize))
            .copied()
            .unwrap_or(0) as f64;
        let total = self.total_tokens.get(&term.field).copied().unwrap_or(0) as f64;
        let avg_len = if n > 0.0 { (total / n).max(1.0) } else { 1.0 };

        let tf = f64::from(tf);
        let norm = K1 * (1.0 - B + B * doc_len / avg_len);
        (idf * tf * (K1 + 1.0) / (tf + norm)) as f32
    }

    /// Searches for documents containing an exact term.
    pub fn search(&self, query: &TermQuery) -> Result<Vec<ScoredDocument>, IndexError> {
        let entries = self.term_entries(query)?;
        Ok(self.to_sorted(entries))
    }

    /// Searches for documents containing the analyzed tokens of `query.text`
    /// at consecutive positions.
    pub fn phrase_search(&self, query: &PhraseQuery) -> Result<Vec<ScoredDocument>, IndexError> {
        self.check_text_field(&query.field)?;
        let tokens = self.analyze_text(&query.field, &query.text)?;
        let Some(first) = tokens.first() else {
            return Ok(Vec::new());
        };
        let base = first.position;
        let terms: Vec<(Term, u32)> = tokens
            .iter()
            .map(|t| {
                (
                    Term::new(query.field.clone(), t.term.clone()),
                    t.position - base,
                )
            })
            .collect();

        let mut candidates: Option<RoaringBitmap> = None;
        for (term, _) in &terms {
            let Some(posting) = self.terms.get(term) else {
                return Ok(Vec::new());
            };
            candidates = Some(match candidates {
                None => posting.doc_ids.clone(),
                Some(c) => c & &posting.doc_ids,
            });
        }
        let Some(candidates) = candidates else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        for row in candidates.iter() {
            let Some(spans) = self.phrase_spans(&terms, row) else {
                continue;
            };
            let score: f32 = terms.iter().map(|(t, _)| self.bm25(t, row)).sum();
            out.push(ScoredDocument {
                doc_id: self.doc_ids[row as usize].clone(),
                row,
                score,
                highlights: vec![Highlight {
                    field: query.field.clone(),
                    positions: spans,
                }],
            });
        }
        sort_results(&mut out);
        Ok(out)
    }

    /// Searches with a boolean combination of exact-term clauses.
    pub fn boolean_search(&self, query: &BooleanQuery) -> Result<Vec<ScoredDocument>, IndexError> {
        let mut result: Option<HashMap<u32, Entry>> = None;
        for clause in &query.must {
            let entries = self.term_entries(clause)?;
            result = Some(match result {
                None => entries,
                Some(mut acc) => {
                    acc.retain(|row, _| entries.contains_key(row));
                    merge_into(&mut acc, entries);
                    acc
                }
            });
        }

        let mut should_union: HashMap<u32, Entry> = HashMap::new();
        for clause in &query.should {
            merge_into(&mut should_union, self.term_entries(clause)?);
        }

        let mut result = match result {
            Some(mut acc) => {
                for (row, (score, highlights)) in should_union {
                    if let Some(entry) = acc.get_mut(&row) {
                        entry.0 += score;
                        entry.1.extend(highlights);
                    }
                }
                acc
            }
            None if !query.should.is_empty() => should_union,
            None => return Ok(Vec::new()),
        };

        for clause in &query.must_not {
            for row in self.term_entries(clause)?.into_keys() {
                result.remove(&row);
            }
        }
        Ok(self.to_sorted(result))
    }

    fn term_entries(&self, query: &TermQuery) -> Result<HashMap<u32, Entry>, IndexError> {
        self.check_text_field(&query.field)?;
        let term = Term::new(query.field.clone(), query.text.clone());
        let mut out = HashMap::new();
        if let Some(posting) = self.terms.get(&term) {
            for row in posting.doc_ids.iter() {
                out.insert(row, (self.bm25(&term, row), self.highlights(&term, row)));
            }
        }
        Ok(out)
    }

    fn highlights(&self, term: &Term, row: u32) -> Vec<Highlight> {
        self.terms
            .get(term)
            .and_then(|p| p.offsets.get(&row))
            .map(|spans| {
                vec![Highlight {
                    field: term.field.clone(),
                    positions: spans.clone(),
                }]
            })
            .unwrap_or_default()
    }

    /// Returns the matched spans of a phrase in `row`, or `None` if the phrase
    /// does not occur there.
    fn phrase_spans(&self, terms: &[(Term, u32)], row: u32) -> Option<Vec<(usize, usize)>> {
        let (first_term, _) = terms.first()?;
        let first_positions = self.terms.get(first_term)?.positions.get(&row)?;

        let mut spans = Vec::new();
        'starts: for &start in first_positions {
            let mut matched = Vec::with_capacity(terms.len());
            for (term, offset) in terms {
                let posting = self.terms.get(term)?;
                let positions = posting.positions.get(&row)?;
                match positions.binary_search(&(start + *offset)) {
                    Ok(idx) => matched.push(posting.offsets[&row][idx]),
                    Err(_) => continue 'starts,
                }
            }
            let (first, last) = (matched.first()?, matched.last()?);
            spans.push((first.0, last.1));
        }
        if spans.is_empty() {
            None
        } else {
            Some(spans)
        }
    }

    fn to_sorted(&self, entries: HashMap<u32, Entry>) -> Vec<ScoredDocument> {
        let mut out: Vec<ScoredDocument> = entries
            .into_iter()
            .map(|(row, (score, highlights))| ScoredDocument {
                doc_id: self.doc_ids[row as usize].clone(),
                row,
                score,
                highlights,
            })
            .collect();
        sort_results(&mut out);
        out
    }

    fn check_text_field(&self, field: &str) -> Result<(), IndexError> {
        let schema_field = self
            .schema
            .field(field)
            .ok_or_else(|| IndexError::UnknownField(field.to_owned()))?;
        if schema_field.field_type != FieldType::Text {
            return Err(IndexError::Unsupported {
                field: field.to_owned(),
                operation: "full-text search",
            });
        }
        if !schema_field.indexed {
            return Err(IndexError::NotIndexed(field.to_owned()));
        }
        Ok(())
    }
}

fn is_text_index(field: &FieldSchema) -> bool {
    field.indexed && field.field_type == FieldType::Text
}

/// Adds `other`'s scores and highlights into `acc`, inserting missing rows.
fn merge_into(acc: &mut HashMap<u32, Entry>, other: HashMap<u32, Entry>) {
    for (row, (score, highlights)) in other {
        let entry = acc.entry(row).or_insert_with(|| (0.0, Vec::new()));
        entry.0 += score;
        entry.1.extend(highlights);
    }
}

fn sort_results(results: &mut [ScoredDocument]) {
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.row.cmp(&b.row))
    });
}
