//! Text analysis: tokenizers and token filters that turn text into index terms.
//!
//! An [`Analyzer`] is a tokenizer followed by an ordered list of filters.
//! Every token keeps its position in the original stream (so phrase queries
//! still work after stop words are removed) and its byte offsets (so search
//! results can be highlighted).

use serde::{Deserialize, Serialize};

use crate::error::SchemaError;

/// Languages supported by stemmers and stop-word lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    /// English.
    English,
    /// French (stemming only).
    French,
    /// German (stemming only).
    German,
    /// Spanish (stemming only).
    Spanish,
}

impl Language {
    fn algorithm(self) -> rust_stemmers::Algorithm {
        use rust_stemmers::Algorithm;
        match self {
            Language::English => Algorithm::English,
            Language::French => Algorithm::French,
            Language::German => Algorithm::German,
            Language::Spanish => Algorithm::Spanish,
        }
    }
}

/// How text is split into raw tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tokenizer {
    /// Split on Unicode whitespace.
    Whitespace,
    /// Split on runs of non-alphanumeric characters.
    Standard,
    /// Character n-grams of length `min..=max` taken from standard tokens.
    Ngram {
        /// Smallest gram length in characters (at least 1).
        min: usize,
        /// Largest gram length in characters (at least `min`).
        max: usize,
    },
    /// Regex-based tokenization. Not supported yet; rejected by
    /// [`Analyzer::validate`]. When analysis is run without validation it falls
    /// back to [`Tokenizer::Standard`].
    Custom(Vec<String>),
}

/// Transformation applied to each token after tokenization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenFilter {
    /// Lowercase the term (Unicode-aware).
    Lowercase,
    /// Reduce the term to its stem.
    Stemmer(Language),
    /// Drop common words. Only English lists are bundled.
    StopWords(Language),
    /// Drop terms whose length in characters is outside `min..=max`.
    Length {
        /// Minimum length in characters, inclusive.
        min: usize,
        /// Maximum length in characters, inclusive.
        max: usize,
    },
}

/// A tokenizer followed by filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Analyzer {
    /// Raw token splitter.
    pub tokenizer: Tokenizer,
    /// Filters applied in order.
    pub filters: Vec<TokenFilter>,
}

/// A single analyzed term with its position and location in the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Normalized term text.
    pub term: String,
    /// Ordinal position in the token stream, counting dropped tokens too.
    pub position: u32,
    /// Start byte offset in the source text (inclusive).
    pub start: usize,
    /// End byte offset in the source text (exclusive).
    pub end: usize,
}

impl Analyzer {
    /// Creates an analyzer from parts. Call [`Analyzer::validate`] before use
    /// if the parts come from untrusted input.
    pub fn new(tokenizer: Tokenizer, filters: Vec<TokenFilter>) -> Self {
        Analyzer { tokenizer, filters }
    }

    /// Standard tokenizer with lowercasing.
    pub fn standard() -> Self {
        Analyzer::new(Tokenizer::Standard, vec![TokenFilter::Lowercase])
    }

    /// Standard tokenizer, lowercasing, English stop words, and English stemming.
    pub fn english() -> Self {
        Analyzer::new(
            Tokenizer::Standard,
            vec![
                TokenFilter::Lowercase,
                TokenFilter::StopWords(Language::English),
                TokenFilter::Stemmer(Language::English),
            ],
        )
    }

    /// Checks that the analyzer can be executed.
    pub fn validate(&self) -> Result<(), SchemaError> {
        match &self.tokenizer {
            Tokenizer::Custom(_) => {
                return Err(SchemaError::UnsupportedAnalyzer(
                    "custom regex tokenizers are not supported yet".into(),
                ))
            }
            Tokenizer::Ngram { min, max } => {
                if *min == 0 || min > max {
                    return Err(SchemaError::UnsupportedAnalyzer(format!(
                        "invalid ngram range {min}..={max}"
                    )));
                }
            }
            Tokenizer::Whitespace | Tokenizer::Standard => {}
        }
        for filter in &self.filters {
            match filter {
                TokenFilter::StopWords(lang) if *lang != Language::English => {
                    return Err(SchemaError::UnsupportedAnalyzer(format!(
                        "no stop-word list for {lang:?}"
                    )))
                }
                TokenFilter::Length { min, max } if min > max => {
                    return Err(SchemaError::UnsupportedAnalyzer(format!(
                        "invalid length range {min}..={max}"
                    )))
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Analyzes `text` into tokens.
    pub fn analyze(&self, text: &str) -> Vec<Token> {
        let raw = self.tokenize(text);
        let stages: Vec<Stage> = self.filters.iter().map(Stage::from_filter).collect();
        let mut out = Vec::with_capacity(raw.len());
        for (position, (term, start, end)) in raw.into_iter().enumerate() {
            let mut current = Some(term);
            for stage in &stages {
                current = match current {
                    Some(t) => stage.apply(t),
                    None => break,
                };
            }
            if let Some(term) = current {
                out.push(Token {
                    term,
                    position: position as u32,
                    start,
                    end,
                });
            }
        }
        out
    }

    fn tokenize(&self, text: &str) -> Vec<(String, usize, usize)> {
        let spans: Vec<(usize, usize)> = match &self.tokenizer {
            Tokenizer::Whitespace => word_spans(text, |c| !c.is_whitespace()),
            Tokenizer::Standard | Tokenizer::Custom(_) => word_spans(text, char::is_alphanumeric),
            Tokenizer::Ngram { min, max } => {
                let mut grams = Vec::new();
                for (s, e) in word_spans(text, char::is_alphanumeric) {
                    let token = &text[s..e];
                    let offsets: Vec<usize> = token
                        .char_indices()
                        .map(|(i, _)| s + i)
                        .chain(std::iter::once(e))
                        .collect();
                    let chars = offsets.len() - 1;
                    for start_char in 0..chars {
                        for n in *min..=*max {
                            if start_char + n <= chars {
                                grams.push((offsets[start_char], offsets[start_char + n]));
                            }
                        }
                    }
                }
                grams
            }
        };
        spans
            .into_iter()
            .map(|(s, e)| (text[s..e].to_owned(), s, e))
            .collect()
    }
}

/// Byte spans of maximal runs of characters for which `keep` is true.
fn word_spans(text: &str, keep: impl Fn(char) -> bool) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if keep(c) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            spans.push((s, i));
        }
    }
    if let Some(s) = start {
        spans.push((s, text.len()));
    }
    spans
}

/// A prepared filter: stemmers are built once per `analyze` call, not per token.
enum Stage {
    Lowercase,
    Stem(rust_stemmers::Stemmer),
    Stop(Language),
    Length { min: usize, max: usize },
}

impl Stage {
    fn from_filter(filter: &TokenFilter) -> Stage {
        match filter {
            TokenFilter::Lowercase => Stage::Lowercase,
            TokenFilter::Stemmer(lang) => {
                Stage::Stem(rust_stemmers::Stemmer::create(lang.algorithm()))
            }
            TokenFilter::StopWords(lang) => Stage::Stop(*lang),
            TokenFilter::Length { min, max } => Stage::Length {
                min: *min,
                max: *max,
            },
        }
    }

    fn apply(&self, term: String) -> Option<String> {
        match self {
            Stage::Lowercase => Some(term.to_lowercase()),
            Stage::Stem(stemmer) => Some(stemmer.stem(&term).into_owned()),
            Stage::Stop(lang) => {
                if is_stop_word(*lang, &term) {
                    None
                } else {
                    Some(term)
                }
            }
            Stage::Length { min, max } => {
                let len = term.chars().count();
                if len >= *min && len <= *max {
                    Some(term)
                } else {
                    None
                }
            }
        }
    }
}

const ENGLISH_STOP_WORDS: &[&str] = &[
    "a", "about", "above", "after", "again", "against", "all", "am", "an", "and", "any", "are",
    "as", "at", "be", "because", "been", "before", "being", "below", "between", "both", "but",
    "by", "can", "could", "did", "do", "does", "doing", "down", "during", "each", "few", "for",
    "from", "further", "had", "has", "have", "having", "he", "her", "here", "hers", "him", "his",
    "how", "i", "if", "in", "into", "is", "it", "its", "just", "me", "more", "most", "my", "no",
    "nor", "not", "now", "of", "off", "on", "once", "only", "or", "other", "our", "out", "over",
    "own", "same", "she", "should", "so", "some", "such", "than", "that", "the", "their", "them",
    "then", "there", "these", "they", "this", "those", "through", "to", "too", "under", "until",
    "up", "very", "was", "we", "were", "what", "when", "where", "which", "while", "who", "whom",
    "why", "will", "with", "would", "you", "your",
];

fn is_stop_word(lang: Language, term: &str) -> bool {
    match lang {
        Language::English => ENGLISH_STOP_WORDS.binary_search(&term).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_word_list_is_sorted_for_binary_search() {
        let mut sorted = ENGLISH_STOP_WORDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, ENGLISH_STOP_WORDS);
    }

    #[test]
    fn standard_splits_and_lowercases() {
        let tokens = Analyzer::standard().analyze("Hello, World-2020!");
        let terms: Vec<&str> = tokens.iter().map(|t| t.term.as_str()).collect();
        assert_eq!(terms, ["hello", "world", "2020"]);
    }

    #[test]
    fn offsets_point_back_into_source() {
        let text = "Naïve café";
        for tok in Analyzer::standard().analyze(text) {
            assert_eq!(tok.term, text[tok.start..tok.end].to_lowercase());
        }
    }

    #[test]
    fn stop_words_keep_positions_for_phrases() {
        let tokens = Analyzer::english().analyze("the rust of programming");
        let terms: Vec<(&str, u32)> = tokens
            .iter()
            .map(|t| (t.term.as_str(), t.position))
            .collect();
        assert_eq!(terms, [("rust", 1), ("program", 3)]);
    }

    #[test]
    fn english_stemmer_reduces_inflections() {
        let tokens = Analyzer::english().analyze("Running runs");
        assert!(tokens.iter().all(|t| t.term == "run"));
    }

    #[test]
    fn ngrams_cover_all_lengths() {
        let analyzer = Analyzer::new(Tokenizer::Ngram { min: 2, max: 3 }, vec![]);
        let terms: Vec<String> = analyzer
            .analyze("abc")
            .into_iter()
            .map(|t| t.term)
            .collect();
        assert_eq!(terms, ["ab", "abc", "bc"]);
    }

    #[test]
    fn validate_rejects_unsupported_parts() {
        assert!(Analyzer::new(Tokenizer::Custom(vec![]), vec![])
            .validate()
            .is_err());
        assert!(Analyzer::new(Tokenizer::Ngram { min: 0, max: 2 }, vec![])
            .validate()
            .is_err());
        assert!(Analyzer::new(
            Tokenizer::Standard,
            vec![TokenFilter::StopWords(Language::French)]
        )
        .validate()
        .is_err());
        assert!(Analyzer::english().validate().is_ok());
    }
}
