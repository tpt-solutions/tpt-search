//! Parser for the SQL-like query language.
//!
//! Grammar (keywords are case-insensitive):
//!
//! ```text
//! expr      := and_expr ( OR and_expr )*
//! and_expr  := not_expr ( AND not_expr )*
//! not_expr  := NOT not_expr | primary
//! primary   := '(' expr ')'
//!            | MATCH '(' field ',' string ')'
//!            | PHRASE '(' field ',' string ')'
//!            | TERM '(' field ',' literal ')'
//!            | field ( '=' | '>' | '>=' | '<' | '<=' ) literal
//! literal   := string | integer | float | TRUE | FALSE
//! field     := identifier (letters, digits, '_' and '.')
//! string    := '"' ... '"'   (escapes: \" and \\)
//! ```
//!
//! Example: `MATCH(title, "rust programming") AND year >= 2020`

use std::ops::Bound;

use tpt_search_core::{FieldValue, QueryError};

use crate::ast::{FusionMethod, Query};

/// Maximum accepted query length in bytes.
pub const MAX_QUERY_BYTES: usize = 64 * 1024;
/// Maximum parenthesis / NOT nesting depth.
pub const MAX_DEPTH: usize = 64;
/// Maximum components in a query vector.
pub const MAX_VECTOR_DIM: usize = 65_536;

/// Parses query text into a [`Query`].
pub fn parse_query(input: &str) -> Result<Query, QueryError> {
    if input.len() > MAX_QUERY_BYTES {
        return Err(QueryError::ResourceExhausted(format!(
            "query is {} bytes; the limit is {MAX_QUERY_BYTES}",
            input.len()
        )));
    }
    let tokens = lex(input)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
    };
    let query = parser.parse_or()?;
    match parser.peek() {
        Tok::Eof => Ok(query),
        other => Err(parse_err(format!("unexpected {}", other.describe()))),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Op(CmpOp),
    Ident(String),
    Str(String),
    Int(i64),
    Float(f64),
    Keyword(Kw),
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmpOp {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kw {
    And,
    Or,
    Not,
    Match,
    Phrase,
    Term,
    True,
    False,
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::LParen => "'('".into(),
            Tok::RParen => "')'".into(),
            Tok::LBracket => "'['".into(),
            Tok::RBracket => "']'".into(),
            Tok::Colon => "':'".into(),
            Tok::Comma => "','".into(),
            Tok::Op(_) => "comparison operator".into(),
            Tok::Ident(s) => format!("identifier `{s}`"),
            Tok::Str(_) => "string literal".into(),
            Tok::Int(_) | Tok::Float(_) => "number".into(),
            Tok::Keyword(k) => format!("keyword {k:?}"),
            Tok::Eof => "end of query".into(),
        }
    }
}

fn parse_err(msg: impl Into<String>) -> QueryError {
    QueryError::ParseError(msg.into())
}

fn lex(input: &str) -> Result<Vec<Tok>, QueryError> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            ']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            ':' => {
                out.push(Tok::Colon);
                i += 1;
            }
            '=' => {
                out.push(Tok::Op(CmpOp::Eq));
                i += 1;
            }
            '>' | '<' => {
                let or_equal = chars.get(i + 1) == Some(&'=');
                out.push(Tok::Op(match (c, or_equal) {
                    ('>', false) => CmpOp::Gt,
                    ('>', true) => CmpOp::Ge,
                    ('<', false) => CmpOp::Lt,
                    _ => CmpOp::Le,
                }));
                i += if or_equal { 2 } else { 1 };
            }
            '"' => {
                let (s, next) = lex_string(&chars, i)?;
                out.push(Tok::Str(s));
                i = next;
            }
            '-' | '0'..='9' => {
                let start = i;
                i += 1;
                while i < chars.len()
                    && (chars[i].is_ascii_digit()
                        || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-'))
                {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                if text.contains(['.', 'e', 'E']) {
                    let f: f64 = text
                        .parse()
                        .map_err(|_| parse_err(format!("invalid number `{text}`")))?;
                    out.push(Tok::Float(f));
                } else {
                    let n: i64 = text
                        .parse()
                        .map_err(|_| parse_err(format!("invalid integer `{text}`")))?;
                    out.push(Tok::Int(n));
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || matches!(chars[i], '_' | '.'))
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                out.push(match keyword(&word) {
                    Some(kw) => Tok::Keyword(kw),
                    None => Tok::Ident(word),
                });
            }
            other => return Err(parse_err(format!("unexpected character `{other}` at {i}"))),
        }
    }
    out.push(Tok::Eof);
    Ok(out)
}

fn lex_string(chars: &[char], start: usize) -> Result<(String, usize), QueryError> {
    let mut s = String::new();
    let mut i = start + 1;
    while i < chars.len() {
        match chars[i] {
            '"' => return Ok((s, i + 1)),
            '\\' => {
                let next = chars.get(i + 1).copied();
                match next {
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    _ => return Err(parse_err(format!("invalid escape at {i}"))),
                }
                i += 2;
            }
            c => {
                s.push(c);
                i += 1;
            }
        }
    }
    Err(parse_err(format!(
        "unterminated string starting at {start}"
    )))
}

fn keyword(word: &str) -> Option<Kw> {
    Some(match word.to_ascii_uppercase().as_str() {
        "AND" => Kw::And,
        "OR" => Kw::Or,
        "NOT" => Kw::Not,
        "MATCH" => Kw::Match,
        "PHRASE" => Kw::Phrase,
        "TERM" => Kw::Term,
        "TRUE" => Kw::True,
        "FALSE" => Kw::False,
        _ => return None,
    })
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Tok {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, want: &Tok) -> Result<(), QueryError> {
        if self.peek() == want {
            self.advance();
            Ok(())
        } else {
            Err(parse_err(format!(
                "expected {} but found {}",
                want.describe(),
                self.peek().describe()
            )))
        }
    }

    fn parse_or(&mut self) -> Result<Query, QueryError> {
        let mut parts = vec![self.parse_and()?];
        while *self.peek() == Tok::Keyword(Kw::Or) {
            self.advance();
            parts.push(self.parse_and()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Query::Boolean {
                must: vec![],
                should: parts,
                must_not: vec![],
            }
        })
    }

    fn parse_and(&mut self) -> Result<Query, QueryError> {
        let mut parts = vec![self.parse_not()?];
        while *self.peek() == Tok::Keyword(Kw::And) {
            self.advance();
            parts.push(self.parse_not()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Query::Boolean {
                must: parts,
                should: vec![],
                must_not: vec![],
            }
        })
    }

    fn parse_not(&mut self) -> Result<Query, QueryError> {
        if *self.peek() == Tok::Keyword(Kw::Not) {
            self.advance();
            self.enter()?;
            let inner = self.parse_not()?;
            self.depth -= 1;
            return Ok(Query::Boolean {
                must: vec![],
                should: vec![],
                must_not: vec![inner],
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Query, QueryError> {
        match self.advance() {
            Tok::LParen => {
                self.enter()?;
                let inner = self.parse_or()?;
                self.expect(&Tok::RParen)?;
                self.depth -= 1;
                Ok(inner)
            }
            Tok::Keyword(kw @ (Kw::Match | Kw::Phrase)) => {
                self.expect(&Tok::LParen)?;
                let field = self.field_name()?;
                self.expect(&Tok::Comma)?;
                let text = match self.advance() {
                    Tok::Str(s) => s,
                    other => {
                        return Err(parse_err(format!(
                            "expected string literal but found {}",
                            other.describe()
                        )))
                    }
                };
                self.expect(&Tok::RParen)?;
                Ok(if kw == Kw::Match {
                    Query::Match { field, text }
                } else {
                    Query::Phrase { field, text }
                })
            }
            Tok::Keyword(Kw::Term) => {
                self.expect(&Tok::LParen)?;
                let field = self.field_name()?;
                self.expect(&Tok::Comma)?;
                let value = self.literal()?;
                self.expect(&Tok::RParen)?;
                Ok(Query::Term { field, value })
            }
            Tok::Ident(name) if name.eq_ignore_ascii_case("KNN") && *self.peek() == Tok::LParen => {
                self.parse_knn()
            }
            Tok::Ident(name) if name.eq_ignore_ascii_case("HYBRID") && *self.peek() == Tok::LParen => {
                self.parse_hybrid()
            }
            Tok::Ident(field) => {
                let op = match self.advance() {
                    Tok::Op(op) => op,
                    other => {
                        return Err(parse_err(format!(
                            "expected comparison after `{field}` but found {}",
                            other.describe()
                        )))
                    }
                };
                let value = self.literal()?;
                Ok(match op {
                    CmpOp::Eq => Query::Term { field, value },
                    CmpOp::Gt => Query::Range {
                        field,
                        start: Bound::Excluded(value),
                        end: Bound::Unbounded,
                    },
                    CmpOp::Ge => Query::Range {
                        field,
                        start: Bound::Included(value),
                        end: Bound::Unbounded,
                    },
                    CmpOp::Lt => Query::Range {
                        field,
                        start: Bound::Unbounded,
                        end: Bound::Excluded(value),
                    },
                    CmpOp::Le => Query::Range {
                        field,
                        start: Bound::Unbounded,
                        end: Bound::Included(value),
                    },
                })
            }
            other => Err(parse_err(format!("unexpected {}", other.describe()))),
        }
    }

    /// `KNN(field, [v, ...], k=N[, ef=M])`. The KNN keyword has been consumed.
    fn parse_knn(&mut self) -> Result<Query, QueryError> {
        self.expect(&Tok::LParen)?;
        let field = self.field_name()?;
        self.expect(&Tok::Comma)?;
        let vector = self.vector_literal()?;
        let mut k = None;
        let mut ef = None;
        while *self.peek() == Tok::Comma {
            self.advance();
            let name = self.field_name()?;
            self.expect(&Tok::Op(CmpOp::Eq))?;
            let value = self.positive_int()?;
            match name.to_ascii_lowercase().as_str() {
                "k" => k = Some(value),
                "ef" => ef = Some(value),
                other => return Err(parse_err(format!("unknown KNN argument `{other}`"))),
            }
        }
        self.expect(&Tok::RParen)?;
        let k = k.ok_or_else(|| parse_err("KNN requires k=<count>"))?;
        Ok(Query::Knn {
            field,
            vector,
            k,
            ef,
        })
    }

    /// `HYBRID(scalar: <expr>, vector: KNN(...), fusion: RRF(k=N))`. The keyword has been consumed.
    fn parse_hybrid(&mut self) -> Result<Query, QueryError> {
        self.expect(&Tok::LParen)?;
        let mut scalar = None;
        let mut vector = None;
        let mut fusion = FusionMethod::default();
        loop {
            let name = self.field_name()?;
            self.expect(&Tok::Colon)?;
            match name.to_ascii_lowercase().as_str() {
                "scalar" => scalar = Some(Box::new(self.parse_or()?)),
                "vector" => {
                    let q = self.parse_primary()?;
                    if !matches!(q, Query::Knn { .. }) {
                        return Err(parse_err("HYBRID vector input must be a KNN(...) query"));
                    }
                    vector = Some(Box::new(q));
                }
                "fusion" => fusion = self.fusion()?,
                other => return Err(parse_err(format!("unknown HYBRID argument `{other}`"))),
            }
            match self.advance() {
                Tok::Comma => continue,
                Tok::RParen => break,
                other => {
                    return Err(parse_err(format!(
                        "expected ',' or ')' but found {}",
                        other.describe()
                    )))
                }
            }
        }
        if scalar.is_none() && vector.is_none() {
            return Err(parse_err("HYBRID needs a scalar or vector input"));
        }
        Ok(Query::Hybrid {
            scalar,
            vector,
            fusion,
        })
    }

    /// `RRF(k=N)`, with `k` optional (default 60).
    fn fusion(&mut self) -> Result<FusionMethod, QueryError> {
        let name = self.field_name()?;
        if !name.eq_ignore_ascii_case("RRF") {
            return Err(parse_err(format!("unknown fusion method `{name}`")));
        }
        self.expect(&Tok::LParen)?;
        let mut k = 60;
        if *self.peek() != Tok::RParen {
            let arg = self.field_name()?;
            if !arg.eq_ignore_ascii_case("k") {
                return Err(parse_err(format!("unknown RRF argument `{arg}`")));
            }
            self.expect(&Tok::Op(CmpOp::Eq))?;
            k = self.positive_int()?;
        }
        self.expect(&Tok::RParen)?;
        Ok(FusionMethod::ReciprocalRankFusion { k })
    }

    fn positive_int(&mut self) -> Result<usize, QueryError> {
        match self.advance() {
            Tok::Int(n) if n > 0 => Ok(n as usize),
            other => Err(parse_err(format!(
                "expected a positive integer but found {}",
                other.describe()
            ))),
        }
    }

    /// `[v1, v2, ...]` of finite numbers.
    fn vector_literal(&mut self) -> Result<Vec<f32>, QueryError> {
        self.expect(&Tok::LBracket)?;
        let mut values = Vec::new();
        loop {
            let value = match self.advance() {
                Tok::Int(n) => n as f32,
                Tok::Float(f) => f as f32,
                other => {
                    return Err(parse_err(format!(
                        "expected a number in vector but found {}",
                        other.describe()
                    )))
                }
            };
            if !value.is_finite() {
                return Err(parse_err("vector values must be finite"));
            }
            values.push(value);
            if values.len() > MAX_VECTOR_DIM {
                return Err(QueryError::ResourceExhausted(format!(
                    "query vector exceeds {MAX_VECTOR_DIM} components"
                )));
            }
            match self.advance() {
                Tok::Comma => continue,
                Tok::RBracket => break,
                other => {
                    return Err(parse_err(format!(
                        "expected ',' or ']' in vector but found {}",
                        other.describe()
                    )))
                }
            }
        }
        Ok(values)
    }

    fn field_name(&mut self) -> Result<String, QueryError> {
        match self.advance() {
            Tok::Ident(name) => Ok(name),
            other => Err(parse_err(format!(
                "expected field name but found {}",
                other.describe()
            ))),
        }
    }

    fn literal(&mut self) -> Result<FieldValue, QueryError> {
        match self.advance() {
            Tok::Str(s) => Ok(FieldValue::Text(s)),
            Tok::Int(i) => Ok(FieldValue::Integer(i)),
            Tok::Float(f) => Ok(FieldValue::Float(f)),
            Tok::Keyword(Kw::True) => Ok(FieldValue::Boolean(true)),
            Tok::Keyword(Kw::False) => Ok(FieldValue::Boolean(false)),
            other => Err(parse_err(format!(
                "expected a value but found {}",
                other.describe()
            ))),
        }
    }

    fn enter(&mut self) -> Result<(), QueryError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(QueryError::ResourceExhausted(format!(
                "query nesting exceeds {MAX_DEPTH} levels"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_match_and_range_with_precedence() {
        let q = parse_query(r#"MATCH(title, "rust programming") AND year >= 2020"#).unwrap();
        assert_eq!(
            q,
            Query::Boolean {
                must: vec![
                    Query::Match {
                        field: "title".into(),
                        text: "rust programming".into()
                    },
                    Query::Range {
                        field: "year".into(),
                        start: Bound::Included(FieldValue::Integer(2020)),
                        end: Bound::Unbounded,
                    },
                ],
                should: vec![],
                must_not: vec![],
            }
        );
    }

    #[test]
    fn or_binds_looser_than_and() {
        let q = parse_query("a = 1 OR b = 2 AND c = 3").unwrap();
        match q {
            Query::Boolean { should, .. } => {
                assert_eq!(should.len(), 2);
                assert!(matches!(&should[1], Query::Boolean { must, .. } if must.len() == 2));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn not_and_parentheses() {
        let q = parse_query(r#"NOT (PHRASE(body, "a b") OR TERM(live, true))"#).unwrap();
        assert!(matches!(q, Query::Boolean { must_not, .. } if must_not.len() == 1));
    }

    #[test]
    fn keywords_are_case_insensitive_and_strings_unescape() {
        let q = parse_query(r#"match(t, "say \"hi\"")"#).unwrap();
        assert_eq!(
            q,
            Query::Match {
                field: "t".into(),
                text: r#"say "hi""#.into()
            }
        );
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in ["", "MATCH(title", "a >", "x = \"open", "a = 1 )", "@"] {
            assert!(
                matches!(parse_query(bad), Err(QueryError::ParseError(_))),
                "{bad:?} should fail to parse"
            );
        }
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let deep = format!("{}a = 1{}", "(".repeat(100), ")".repeat(100));
        assert!(matches!(
            parse_query(&deep),
            Err(QueryError::ResourceExhausted(_))
        ));
    }
}
