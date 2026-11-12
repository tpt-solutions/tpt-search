//! A Cypher-style pattern language: parser and matcher.
//!
//! Supported form (keywords are case-insensitive):
//!
//! ```text
//! MATCH (a:Document {status: "published"})-[:CITES*1..3]->(b:Document)
//! WHERE a.year >= 2020 AND (b.topic = "search" OR b.year < 2000)
//! RETURN b
//! ORDER BY b.year DESC
//! LIMIT 10
//! ```
//!
//! * Nodes: `(var:Label {key: value, ...})`. Every part is optional.
//! * Edges: `-[:TYPE]->` (outgoing), `<-[:TYPE]-` (incoming), `-[:TYPE]-` (either way).
//!   A hop range `*min..max` follows the edge repeatedly (at most [`MAX_HOPS`] hops).
//! * `WHERE`: comparisons `var.property op literal` (`=`, `>`, `>=`, `<`, `<=`)
//!   joined with `AND`, `OR`, and parentheses. `AND` binds tighter than `OR`.
//!
//! Matching is over distinct node assignments per variable; variable-length hops
//! are expanded layer by layer, so each node reached at a depth is considered once
//! per layer. Results are capped (see [`execute`]).

use std::collections::{BTreeMap, BTreeSet};

use crate::error::GraphError;
use crate::graph::{Direction, PropertyGraph, PropertyValue};

/// Largest allowed hop count in a variable-length edge.
pub const MAX_HOPS: usize = 10;
/// Default cap on the number of matches returned by [`execute`].
pub const DEFAULT_MAX_MATCHES: usize = 100_000;

/// A node in a pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct NodePattern {
    /// Variable the matched node is bound to, if named.
    pub var: Option<String>,
    /// Required label, if any.
    pub label: Option<String>,
    /// Required property values.
    pub properties: Vec<(String, PropertyValue)>,
}

/// An edge in a pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgePattern {
    /// Required relationship type, if any.
    pub label: Option<String>,
    /// Direction the edge is followed in.
    pub direction: Direction,
    /// Fewest hops (at least 1).
    pub min_hops: usize,
    /// Most hops (at most [`MAX_HOPS`]).
    pub max_hops: usize,
}

/// Comparison operator in a `WHERE` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    /// `=`
    Eq,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `<`
    Lt,
    /// `<=`
    Le,
}

/// A `WHERE` expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// All sub-expressions hold.
    And(Vec<Expr>),
    /// At least one sub-expression holds.
    Or(Vec<Expr>),
    /// `var.property op value`.
    Compare {
        /// Bound variable.
        var: String,
        /// Property name.
        property: String,
        /// Operator.
        op: CmpOp,
        /// Literal to compare with.
        value: PropertyValue,
    },
}

/// `ORDER BY var.property [DESC]`.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderBy {
    /// Bound variable.
    pub var: String,
    /// Property to sort by. Nodes without it sort last.
    pub property: String,
    /// Descending order.
    pub descending: bool,
}

/// A parsed graph query.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphQuery {
    /// First node.
    pub start: NodePattern,
    /// Each edge followed by the node it leads to.
    pub hops: Vec<(EdgePattern, NodePattern)>,
    /// Filter applied to complete matches.
    pub filter: Option<Expr>,
    /// Variables to return, in order.
    pub returns: Vec<String>,
    /// Sort order of the results.
    pub order_by: Option<OrderBy>,
    /// Maximum number of results.
    pub limit: Option<usize>,
}

/// One match: each bound variable mapped to a node id.
pub type Binding = BTreeMap<String, String>;

/// Parses a graph query and checks that every variable it uses is bound.
pub fn parse_graph_query(input: &str) -> Result<GraphQuery, GraphError> {
    let tokens = lex(input)?;
    let mut p = Parser { tokens, pos: 0 };
    let query = p.query()?;
    if !p.at_end() {
        return Err(parse_err(format!("unexpected {}", p.peek().describe())));
    }
    query.validate()?;
    Ok(query)
}

/// Runs a query against a graph. Returns at most `max_matches` bindings (and never more
/// than the query's `LIMIT`). Fails with [`GraphError::TooManyMatches`] if the cap is
/// exceeded before the limit applies.
pub fn execute(
    graph: &PropertyGraph,
    query: &GraphQuery,
    max_matches: usize,
) -> Result<Vec<Binding>, GraphError> {
    let mut matcher = Matcher {
        graph,
        query,
        max: max_matches,
        out: Vec::new(),
    };
    let mut env: BTreeMap<&str, usize> = BTreeMap::new();
    for start in graph.node_indices() {
        if !node_matches(graph, &query.start, start) {
            continue;
        }
        if let Some(var) = &query.start.var {
            env.insert(var, start);
        }
        matcher.extend(0, start, &mut env)?;
        if let Some(var) = &query.start.var {
            env.remove(var.as_str());
        }
    }
    let mut results = matcher.out;
    if let Some(order) = &query.order_by {
        results.sort_by(|a, b| compare_by(graph, a, b, order));
    }
    if let Some(limit) = query.limit {
        results.truncate(limit);
    }
    Ok(results)
}

fn compare_by(
    graph: &PropertyGraph,
    a: &Binding,
    b: &Binding,
    order: &OrderBy,
) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let key = |binding: &Binding| {
        binding
            .get(&order.var)
            .and_then(|id| graph.get_node(id))
            .and_then(|n| n.property(&order.property).cloned())
    };
    let (ka, kb) = (key(a), key(b));
    let ordering = match (&ka, &kb) {
        (Some(x), Some(y)) => x.partial_cmp_value(y).unwrap_or(Equal),
        (Some(_), None) => Less,
        (None, Some(_)) => Greater,
        (None, None) => Equal,
    };
    // Missing values stay last in both directions.
    match (&ka, &kb, order.descending) {
        (Some(_), Some(_), true) => ordering.reverse(),
        _ => ordering,
    }
}

struct Matcher<'q, 'g> {
    graph: &'g PropertyGraph,
    query: &'q GraphQuery,
    max: usize,
    out: Vec<Binding>,
}

impl<'q> Matcher<'q, '_> {
    fn extend(
        &mut self,
        pos: usize,
        current: usize,
        env: &mut BTreeMap<&'q str, usize>,
    ) -> Result<(), GraphError> {
        let query = self.query;
        if pos == query.hops.len() {
            if let Some(filter) = &query.filter {
                if !eval(self.graph, filter, env) {
                    return Ok(());
                }
            }
            if self.out.len() >= self.max {
                return Err(GraphError::TooManyMatches(self.max));
            }
            let binding = env
                .iter()
                .map(|(var, idx)| ((*var).to_owned(), self.graph.node_at(*idx).id.clone()))
                .collect();
            self.out.push(binding);
            return Ok(());
        }

        let (edge, next) = &query.hops[pos];
        for end in self.reachable(current, edge) {
            if !node_matches(self.graph, next, end) {
                continue;
            }
            let mut added = None;
            if let Some(var) = &next.var {
                match env.get(var.as_str()) {
                    Some(&bound) if bound != end => continue,
                    Some(_) => {}
                    None => {
                        env.insert(var, end);
                        added = Some(var.as_str());
                    }
                }
            }
            self.extend(pos + 1, end, env)?;
            if let Some(var) = added {
                env.remove(var);
            }
        }
        Ok(())
    }

    /// Distinct nodes reachable from `from` in `min..=max` hops along `edge`.
    fn reachable(&self, from: usize, edge: &EdgePattern) -> Vec<usize> {
        let mut found = BTreeSet::new();
        let mut layer = vec![from];
        for depth in 1..=edge.max_hops {
            let mut next = BTreeSet::new();
            for &node in &layer {
                for (other, e) in self.graph.adjacent(node, edge.direction) {
                    let label_ok = edge
                        .label
                        .as_ref()
                        .is_none_or(|l| self.graph.edge_at(e).label == *l);
                    if label_ok {
                        next.insert(other);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            if depth >= edge.min_hops {
                found.extend(next.iter().copied());
            }
            layer = next.into_iter().collect();
        }
        found.into_iter().collect()
    }
}

fn node_matches(graph: &PropertyGraph, pattern: &NodePattern, idx: usize) -> bool {
    let node = graph.node_at(idx);
    if let Some(label) = &pattern.label {
        if !node.has_label(label) {
            return false;
        }
    }
    pattern
        .properties
        .iter()
        .all(|(key, value)| node.property(key) == Some(value))
}

fn eval(graph: &PropertyGraph, expr: &Expr, env: &BTreeMap<&str, usize>) -> bool {
    match expr {
        Expr::And(parts) => parts.iter().all(|p| eval(graph, p, env)),
        Expr::Or(parts) => parts.iter().any(|p| eval(graph, p, env)),
        Expr::Compare {
            var,
            property,
            op,
            value,
        } => {
            let Some(&idx) = env.get(var.as_str()) else {
                return false;
            };
            let Some(actual) = graph.node_at(idx).property(property) else {
                return false;
            };
            match actual.partial_cmp_value(value) {
                None => false,
                Some(ord) => match op {
                    CmpOp::Eq => ord.is_eq(),
                    CmpOp::Gt => ord.is_gt(),
                    CmpOp::Ge => ord.is_ge(),
                    CmpOp::Lt => ord.is_lt(),
                    CmpOp::Le => ord.is_le(),
                },
            }
        }
    }
}

impl GraphQuery {
    /// Variables bound by the pattern, in order of first appearance.
    fn bound_vars(&self) -> Vec<&str> {
        let mut vars: Vec<&str> = Vec::new();
        let nodes = std::iter::once(&self.start).chain(self.hops.iter().map(|(_, n)| n));
        for node in nodes {
            if let Some(v) = &node.var {
                if !vars.contains(&v.as_str()) {
                    vars.push(v);
                }
            }
        }
        vars
    }

    fn validate(&self) -> Result<(), GraphError> {
        let bound = self.bound_vars();
        let check = |var: &str| {
            if bound.contains(&var) {
                Ok(())
            } else {
                Err(GraphError::UnknownVariable(var.to_owned()))
            }
        };
        for var in &self.returns {
            check(var)?;
        }
        if let Some(order) = &self.order_by {
            check(&order.var)?;
        }
        if let Some(filter) = &self.filter {
            check_expr(filter, &check)?;
        }
        if self.returns.is_empty() {
            return Err(parse_err("RETURN needs at least one variable"));
        }
        Ok(())
    }
}

fn check_expr(
    expr: &Expr,
    check: &dyn Fn(&str) -> Result<(), GraphError>,
) -> Result<(), GraphError> {
    match expr {
        Expr::And(parts) | Expr::Or(parts) => parts.iter().try_for_each(|p| check_expr(p, check)),
        Expr::Compare { var, .. } => check(var),
    }
}

fn parse_err(msg: impl Into<String>) -> GraphError {
    GraphError::Parse(msg.into())
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Str(String),
    Int(i64),
    Float(f64),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Dash,
    Star,
    DotDot,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Eof,
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::Word(w) => format!("`{w}`"),
            Tok::Str(_) => "string literal".into(),
            Tok::Int(_) | Tok::Float(_) => "number".into(),
            Tok::Eof => "end of query".into(),
            other => format!("{other:?}"),
        }
    }
}

fn lex(input: &str) -> Result<Vec<Tok>, GraphError> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let tok = match c {
            c if c.is_whitespace() => {
                i += 1;
                continue;
            }
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            ':' => Tok::Colon,
            ',' => Tok::Comma,
            '-' => Tok::Dash,
            '*' => Tok::Star,
            '=' => Tok::Eq,
            '<' if next == Some('=') => {
                i += 2;
                out.push(Tok::Le);
                continue;
            }
            '<' => Tok::Lt,
            '>' if next == Some('=') => {
                i += 2;
                out.push(Tok::Ge);
                continue;
            }
            '>' => Tok::Gt,
            '.' if next == Some('.') => {
                i += 2;
                out.push(Tok::DotDot);
                continue;
            }
            '"' | '\'' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err(parse_err("unterminated string")),
                        Some(&ch) if ch == quote => break,
                        Some('\\') => {
                            match chars.get(i + 1) {
                                Some(&esc) => s.push(esc),
                                None => return Err(parse_err("unterminated string")),
                            }
                            i += 2;
                        }
                        Some(&ch) => {
                            s.push(ch);
                            i += 1;
                        }
                    }
                }
                i += 1;
                out.push(Tok::Str(s));
                continue;
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let is_float = chars.get(i) == Some(&'.')
                    && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit());
                if is_float {
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    let text: String = chars[start..i].iter().collect();
                    out.push(Tok::Float(
                        text.parse().map_err(|_| parse_err("bad number"))?,
                    ));
                } else {
                    let text: String = chars[start..i].iter().collect();
                    out.push(Tok::Int(text.parse().map_err(|_| parse_err("bad number"))?));
                }
                continue;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() {
                    let ch = chars[i];
                    let dot_then_name = ch == '.'
                        && chars
                            .get(i + 1)
                            .is_some_and(|d| d.is_alphanumeric() || *d == '_');
                    if ch.is_alphanumeric() || ch == '_' || dot_then_name {
                        i += 1;
                    } else {
                        break;
                    }
                }
                out.push(Tok::Word(chars[start..i].iter().collect()));
                continue;
            }
            other => return Err(parse_err(format!("unexpected character `{other}` at {i}"))),
        };
        out.push(tok);
        i += 1;
    }
    out.push(Tok::Eof);
    Ok(out)
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos]
    }

    fn peek_at(&self, offset: usize) -> &Tok {
        self.tokens.get(self.pos + offset).unwrap_or(&Tok::Eof)
    }

    fn advance(&mut self) -> Tok {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn at_end(&self) -> bool {
        *self.peek() == Tok::Eof
    }

    fn expect(&mut self, want: &Tok) -> Result<(), GraphError> {
        if self.peek() == want {
            self.advance();
            Ok(())
        } else {
            Err(parse_err(format!(
                "expected {} but found {}",
                Tok::describe(want),
                self.peek().describe()
            )))
        }
    }

    fn keyword(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Tok::Word(w) if w.eq_ignore_ascii_case(word)) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_keyword(&mut self, word: &str) -> Result<(), GraphError> {
        if self.keyword(word) {
            Ok(())
        } else {
            Err(parse_err(format!(
                "expected {word} but found {}",
                self.peek().describe()
            )))
        }
    }

    fn query(&mut self) -> Result<GraphQuery, GraphError> {
        self.expect_keyword("MATCH")?;
        let start = self.node()?;
        let mut hops = Vec::new();
        while matches!(self.peek(), Tok::Dash | Tok::Lt) {
            let edge = self.edge()?;
            let node = self.node()?;
            hops.push((edge, node));
        }

        let filter = if self.keyword("WHERE") {
            Some(self.or_expr()?)
        } else {
            None
        };

        self.expect_keyword("RETURN")?;
        let mut returns = vec![self.variable()?];
        while *self.peek() == Tok::Comma {
            self.advance();
            returns.push(self.variable()?);
        }

        let order_by = if self.keyword("ORDER") {
            self.expect_keyword("BY")?;
            let (var, property) = self.var_property()?;
            let descending = if self.keyword("DESC") {
                true
            } else {
                self.keyword("ASC");
                false
            };
            Some(OrderBy {
                var,
                property,
                descending,
            })
        } else {
            None
        };

        let limit = if self.keyword("LIMIT") {
            match self.advance() {
                Tok::Int(n) if n > 0 => Some(n as usize),
                other => {
                    return Err(parse_err(format!(
                        "LIMIT needs a positive integer, found {}",
                        other.describe()
                    )))
                }
            }
        } else {
            None
        };

        Ok(GraphQuery {
            start,
            hops,
            filter,
            returns,
            order_by,
            limit,
        })
    }

    /// `(var:Label {key: value, ...})`; every part is optional.
    fn node(&mut self) -> Result<NodePattern, GraphError> {
        self.expect(&Tok::LParen)?;
        let var = match self.peek() {
            Tok::Word(w) => {
                let v = w.clone();
                self.advance();
                Some(v)
            }
            _ => None,
        };
        let label = if *self.peek() == Tok::Colon {
            self.advance();
            Some(self.name()?)
        } else {
            None
        };
        let mut properties = Vec::new();
        if *self.peek() == Tok::LBrace {
            self.advance();
            loop {
                let key = self.name()?;
                self.expect(&Tok::Colon)?;
                let value = self.literal()?;
                properties.push((key, value));
                match self.advance() {
                    Tok::Comma => continue,
                    Tok::RBrace => break,
                    other => {
                        return Err(parse_err(format!(
                            "expected `,` or `}}` but found {}",
                            other.describe()
                        )))
                    }
                }
            }
        }
        self.expect(&Tok::RParen)?;
        Ok(NodePattern {
            var,
            label,
            properties,
        })
    }

    /// `-[...]->`, `<-[...]-`, or `-[...]-`, with an optional `:TYPE` and `*min..max`.
    fn edge(&mut self) -> Result<EdgePattern, GraphError> {
        let incoming = if *self.peek() == Tok::Lt {
            self.advance();
            true
        } else {
            false
        };
        self.expect(&Tok::Dash)?;
        self.expect(&Tok::LBracket)?;
        if matches!(self.peek(), Tok::Word(_)) && *self.peek_at(1) != Tok::Colon {
            // An edge variable: accepted for readability, not bound in results.
            self.advance();
        }
        let label = if *self.peek() == Tok::Colon {
            self.advance();
            Some(self.name()?)
        } else {
            None
        };
        let (min_hops, max_hops) = if *self.peek() == Tok::Star {
            self.advance();
            self.hop_range()?
        } else {
            (1, 1)
        };
        self.expect(&Tok::RBracket)?;
        self.expect(&Tok::Dash)?;
        let outgoing = !incoming && *self.peek() == Tok::Gt;
        if outgoing {
            self.advance();
        }
        let direction = match (incoming, outgoing) {
            (true, _) => Direction::Incoming,
            (false, true) => Direction::Outgoing,
            (false, false) => Direction::Both,
        };
        if min_hops == 0 || min_hops > max_hops {
            return Err(parse_err(format!(
                "invalid hop range {min_hops}..{max_hops}"
            )));
        }
        if max_hops > MAX_HOPS {
            return Err(GraphError::Unsupported(format!(
                "hop range may not exceed {MAX_HOPS}"
            )));
        }
        Ok(EdgePattern {
            label,
            direction,
            min_hops,
            max_hops,
        })
    }

    /// The part after `*`: nothing (1..1), `n` (n..n), `n..m`, or `..m` (1..m).
    fn hop_range(&mut self) -> Result<(usize, usize), GraphError> {
        let min = match self.peek() {
            Tok::Int(n) => {
                let n = *n as usize;
                self.advance();
                Some(n)
            }
            _ => None,
        };
        if *self.peek() == Tok::DotDot {
            self.advance();
            let max = match self.advance() {
                Tok::Int(n) => n as usize,
                other => {
                    return Err(parse_err(format!(
                        "expected an upper hop bound but found {}",
                        other.describe()
                    )))
                }
            };
            return Ok((min.unwrap_or(1), max));
        }
        let n = min.unwrap_or(1);
        Ok((n, n))
    }

    fn or_expr(&mut self) -> Result<Expr, GraphError> {
        let mut parts = vec![self.and_expr()?];
        while self.keyword("OR") {
            parts.push(self.and_expr()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Expr::Or(parts)
        })
    }

    fn and_expr(&mut self) -> Result<Expr, GraphError> {
        let mut parts = vec![self.atom()?];
        while self.keyword("AND") {
            parts.push(self.atom()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Expr::And(parts)
        })
    }

    fn atom(&mut self) -> Result<Expr, GraphError> {
        if *self.peek() == Tok::LParen {
            self.advance();
            let inner = self.or_expr()?;
            self.expect(&Tok::RParen)?;
            return Ok(inner);
        }
        let (var, property) = self.var_property()?;
        let op = match self.advance() {
            Tok::Eq => CmpOp::Eq,
            Tok::Gt => CmpOp::Gt,
            Tok::Ge => CmpOp::Ge,
            Tok::Lt => CmpOp::Lt,
            Tok::Le => CmpOp::Le,
            other => {
                return Err(parse_err(format!(
                    "expected a comparison but found {}",
                    other.describe()
                )))
            }
        };
        let value = self.literal()?;
        Ok(Expr::Compare {
            var,
            property,
            op,
            value,
        })
    }

    /// A bare variable name.
    fn variable(&mut self) -> Result<String, GraphError> {
        match self.advance() {
            Tok::Word(w) if !w.contains('.') => Ok(w),
            other => Err(parse_err(format!(
                "expected a variable but found {}",
                other.describe()
            ))),
        }
    }

    /// `var.property`, lexed as one word.
    fn var_property(&mut self) -> Result<(String, String), GraphError> {
        match self.advance() {
            Tok::Word(w) => match w.split_once('.') {
                Some((var, prop)) if !var.is_empty() && !prop.is_empty() => {
                    Ok((var.to_owned(), prop.to_owned()))
                }
                _ => Err(parse_err(format!(
                    "expected `variable.property` but found `{w}`"
                ))),
            },
            other => Err(parse_err(format!(
                "expected `variable.property` but found {}",
                other.describe()
            ))),
        }
    }

    /// A label, type, or property name.
    fn name(&mut self) -> Result<String, GraphError> {
        match self.advance() {
            Tok::Word(w) => Ok(w),
            other => Err(parse_err(format!(
                "expected a name but found {}",
                other.describe()
            ))),
        }
    }

    fn literal(&mut self) -> Result<PropertyValue, GraphError> {
        match self.advance() {
            Tok::Str(s) => Ok(PropertyValue::Text(s)),
            Tok::Int(n) => Ok(PropertyValue::Int(n)),
            Tok::Float(f) => Ok(PropertyValue::Float(f)),
            Tok::Dash => match self.advance() {
                Tok::Int(n) => Ok(PropertyValue::Int(-n)),
                Tok::Float(f) => Ok(PropertyValue::Float(-f)),
                other => Err(parse_err(format!(
                    "expected a number after `-` but found {}",
                    other.describe()
                ))),
            },
            Tok::Word(w) if w.eq_ignore_ascii_case("TRUE") => Ok(PropertyValue::Bool(true)),
            Tok::Word(w) if w.eq_ignore_ascii_case("FALSE") => Ok(PropertyValue::Bool(false)),
            other => Err(parse_err(format!(
                "expected a value but found {}",
                other.describe()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn props(pairs: &[(&str, PropertyValue)]) -> HashMap<String, PropertyValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    /// Papers citing each other, with years and topics.
    fn library() -> PropertyGraph {
        let mut g = PropertyGraph::new();
        let papers = [
            ("p1", 2021i64, "search"),
            ("p2", 2019, "search"),
            ("p3", 2023, "vectors"),
            ("p4", 1998, "search"),
        ];
        for (id, year, topic) in papers {
            g.add_node(
                id,
                vec!["Paper".into()],
                props(&[
                    ("year", PropertyValue::Int(year)),
                    ("topic", PropertyValue::from(topic)),
                ]),
            )
            .unwrap();
        }
        g.add_labelled_node("alice", &["Author"]).unwrap();
        g.add_edge("p1", "p2", "CITES", HashMap::new()).unwrap();
        g.add_edge("p2", "p4", "CITES", HashMap::new()).unwrap();
        g.add_edge("p3", "p1", "CITES", HashMap::new()).unwrap();
        g.add_edge("alice", "p1", "WROTE", HashMap::new()).unwrap();
        g
    }

    fn ids(bindings: &[Binding], var: &str) -> Vec<String> {
        let mut v: Vec<String> = bindings.iter().map(|b| b[var].clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn single_hop_with_labels_and_direction() {
        let g = library();
        let q = parse_graph_query("MATCH (a:Paper)-[:CITES]->(b:Paper) RETURN b").unwrap();
        let out = execute(&g, &q, 100).unwrap();
        assert_eq!(ids(&out, "b"), ["p1", "p2", "p4"]);
    }

    #[test]
    fn incoming_and_undirected_edges() {
        let g = library();
        let q = parse_graph_query("MATCH (a:Paper {topic: \"vectors\"})<-[:CITES]-(b) RETURN b")
            .unwrap();
        assert_eq!(
            ids(&execute(&g, &q, 100).unwrap(), "b"),
            Vec::<String>::new()
        );
        let q = parse_graph_query("MATCH (a:Paper {topic: \"vectors\"})-[:CITES]-(b) RETURN b")
            .unwrap();
        assert_eq!(ids(&execute(&g, &q, 100).unwrap(), "b"), ["p1"]);
        let q =
            parse_graph_query("MATCH (a:Paper {topic: \"search\"})<-[:CITES]-(b:Paper) RETURN b")
                .unwrap();
        assert_eq!(ids(&execute(&g, &q, 100).unwrap(), "b"), ["p1", "p2", "p3"]);
    }

    #[test]
    fn variable_length_hops() {
        let g = library();
        let q = parse_graph_query("MATCH (a {topic: \"vectors\"})-[:CITES*1..3]->(b) RETURN b")
            .unwrap();
        assert_eq!(ids(&execute(&g, &q, 100).unwrap(), "b"), ["p1", "p2", "p4"]);
        let q =
            parse_graph_query("MATCH (a {topic: \"vectors\"})-[:CITES*2]->(b) RETURN b").unwrap();
        assert_eq!(ids(&execute(&g, &q, 100).unwrap(), "b"), ["p2"]);
    }

    #[test]
    fn where_clause_with_and_or() {
        let g = library();
        let q = parse_graph_query(
            "MATCH (p:Paper) WHERE p.year >= 2019 AND (p.topic = \"search\" OR p.topic = \"vectors\") RETURN p",
        )
        .unwrap();
        assert_eq!(ids(&execute(&g, &q, 100).unwrap(), "p"), ["p1", "p2", "p3"]);
    }

    #[test]
    fn order_by_and_limit() {
        let g = library();
        let q = parse_graph_query("MATCH (p:Paper) RETURN p ORDER BY p.year DESC LIMIT 2").unwrap();
        let out = execute(&g, &q, 100).unwrap();
        let years: Vec<&str> = out.iter().map(|b| b["p"].as_str()).collect();
        assert_eq!(years, ["p3", "p1"]);
    }

    #[test]
    fn two_hop_pattern_with_shared_variable() {
        let g = library();
        let q = parse_graph_query(
            "MATCH (author:Author)-[:WROTE]->(p:Paper)-[:CITES]->(cited) RETURN cited",
        )
        .unwrap();
        assert_eq!(ids(&execute(&g, &q, 100).unwrap(), "cited"), ["p2"]);
    }

    #[test]
    fn parse_errors_and_unbound_variables() {
        assert!(matches!(
            parse_graph_query("MATCH (a RETURN a"),
            Err(GraphError::Parse(_))
        ));
        assert!(matches!(
            parse_graph_query("MATCH (a) RETURN b"),
            Err(GraphError::UnknownVariable(v)) if v == "b"
        ));
        assert!(matches!(
            parse_graph_query("MATCH (a)-[*1..50]->(b) RETURN b"),
            Err(GraphError::Unsupported(_))
        ));
        assert!(parse_graph_query("MATCH (a)-[*0..2]->(b) RETURN b").is_err());
        assert!(parse_graph_query("MATCH (a) WHERE a = 1 RETURN a").is_err());
    }

    #[test]
    fn match_cap_is_enforced() {
        let g = library();
        let q = parse_graph_query("MATCH (a)-[*1..3]-(b) RETURN b").unwrap();
        assert!(matches!(
            execute(&g, &q, 2),
            Err(GraphError::TooManyMatches(2))
        ));
    }
}
