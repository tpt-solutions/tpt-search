//! Property graph: labelled nodes and edges, each carrying typed properties.
//!
//! Nodes are addressed by a string id, which in the search layer equals the id of
//! the document with the same identity. Edges are directed and have a label; traversal
//! can follow them forwards, backwards, or both ways.

use std::cmp::Ordering;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::GraphError;

/// A property value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PropertyValue {
    /// Absent value.
    Null,
    /// Boolean.
    Bool(bool),
    /// Signed integer.
    Int(i64),
    /// Floating-point number.
    Float(f64),
    /// Text.
    Text(String),
}

impl PropertyValue {
    /// Orders two values of compatible types. Integers and floats compare
    /// numerically. Returns `None` for incompatible types and for `NaN`.
    pub fn partial_cmp_value(&self, other: &PropertyValue) -> Option<Ordering> {
        match (self, other) {
            (PropertyValue::Int(a), PropertyValue::Int(b)) => Some(a.cmp(b)),
            (PropertyValue::Int(a), PropertyValue::Float(b)) => (*a as f64).partial_cmp(b),
            (PropertyValue::Float(a), PropertyValue::Int(b)) => a.partial_cmp(&(*b as f64)),
            (PropertyValue::Float(a), PropertyValue::Float(b)) => a.partial_cmp(b),
            (PropertyValue::Text(a), PropertyValue::Text(b)) => Some(a.cmp(b)),
            (PropertyValue::Bool(a), PropertyValue::Bool(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }
}

impl From<&str> for PropertyValue {
    fn from(s: &str) -> Self {
        PropertyValue::Text(s.to_owned())
    }
}

impl From<String> for PropertyValue {
    fn from(s: String) -> Self {
        PropertyValue::Text(s)
    }
}

impl From<i64> for PropertyValue {
    fn from(i: i64) -> Self {
        PropertyValue::Int(i)
    }
}

impl From<f64> for PropertyValue {
    fn from(f: f64) -> Self {
        PropertyValue::Float(f)
    }
}

impl From<bool> for PropertyValue {
    fn from(b: bool) -> Self {
        PropertyValue::Bool(b)
    }
}

/// Which way edges are followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// From source to target.
    Outgoing,
    /// From target to source.
    Incoming,
    /// Either way.
    Both,
}

/// A graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// External identifier, unique within the graph.
    pub id: String,
    /// Labels, such as `Person` or `Document`.
    pub labels: Vec<String>,
    /// Properties by name.
    pub properties: HashMap<String, PropertyValue>,
}

impl Node {
    /// Whether the node carries `label`.
    pub fn has_label(&self, label: &str) -> bool {
        self.labels.iter().any(|l| l == label)
    }

    /// Looks up a property.
    pub fn property(&self, name: &str) -> Option<&PropertyValue> {
        self.properties.get(name)
    }
}

/// A directed, labelled edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    /// Sequential identifier, assigned on insertion.
    pub id: u64,
    /// Source node (index into the node table).
    pub from: usize,
    /// Target node (index into the node table).
    pub to: usize,
    /// Relationship type, such as `CITES`.
    pub label: String,
    /// Properties by name.
    pub properties: HashMap<String, PropertyValue>,
}

/// A directed property graph.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PropertyGraph {
    nodes: Vec<Node>,
    index: HashMap<String, usize>,
    edges: Vec<Edge>,
    out_edges: Vec<Vec<usize>>,
    in_edges: Vec<Vec<usize>>,
}

impl PropertyGraph {
    /// Creates an empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edges.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Adds a node. Fails if the id is already present.
    pub fn add_node(
        &mut self,
        id: impl Into<String>,
        labels: Vec<String>,
        properties: HashMap<String, PropertyValue>,
    ) -> Result<(), GraphError> {
        let id = id.into();
        if self.index.contains_key(&id) {
            return Err(GraphError::DuplicateNode(id));
        }
        self.index.insert(id.clone(), self.nodes.len());
        self.nodes.push(Node {
            id,
            labels,
            properties,
        });
        self.out_edges.push(Vec::new());
        self.in_edges.push(Vec::new());
        Ok(())
    }

    /// Adds a node with the given labels and no properties.
    pub fn add_labelled_node(
        &mut self,
        id: impl Into<String>,
        labels: &[&str],
    ) -> Result<(), GraphError> {
        self.add_node(
            id,
            labels.iter().map(|l| (*l).to_owned()).collect(),
            HashMap::new(),
        )
    }

    /// Adds an edge between two existing nodes and returns its id.
    pub fn add_edge(
        &mut self,
        from: &str,
        to: &str,
        label: &str,
        properties: HashMap<String, PropertyValue>,
    ) -> Result<u64, GraphError> {
        let from_idx = self.node_index(from)?;
        let to_idx = self.node_index(to)?;
        let id = self.edges.len() as u64;
        self.edges.push(Edge {
            id,
            from: from_idx,
            to: to_idx,
            label: label.to_owned(),
            properties,
        });
        self.out_edges[from_idx].push(self.edges.len() - 1);
        self.in_edges[to_idx].push(self.edges.len() - 1);
        Ok(id)
    }

    /// Looks up a node by id.
    pub fn get_node(&self, id: &str) -> Option<&Node> {
        self.index.get(id).map(|&i| &self.nodes[i])
    }

    /// Looks up an edge by id.
    pub fn get_edge(&self, id: u64) -> Option<&Edge> {
        self.edges.get(id as usize)
    }

    /// Nodes adjacent to `id` in the given direction, in insertion order of the edges.
    pub fn neighbors(&self, id: &str, direction: Direction) -> Result<Vec<&Node>, GraphError> {
        let idx = self.node_index(id)?;
        Ok(self
            .adjacent(idx, direction)
            .into_iter()
            .map(|(n, _)| &self.nodes[n])
            .collect())
    }

    /// Iterates over all nodes.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter()
    }

    /// Iterates over all edges.
    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.edges.iter()
    }

    /// Node index for an id.
    pub(crate) fn node_index(&self, id: &str) -> Result<usize, GraphError> {
        self.index
            .get(id)
            .copied()
            .ok_or_else(|| GraphError::UnknownNode(id.to_owned()))
    }

    /// Node at an index.
    pub(crate) fn node_at(&self, idx: usize) -> &Node {
        &self.nodes[idx]
    }

    /// `(neighbour index, edge index)` pairs for one node.
    pub(crate) fn adjacent(&self, node: usize, direction: Direction) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        if matches!(direction, Direction::Outgoing | Direction::Both) {
            for &e in &self.out_edges[node] {
                out.push((self.edges[e].to, e));
            }
        }
        if matches!(direction, Direction::Incoming | Direction::Both) {
            for &e in &self.in_edges[node] {
                out.push((self.edges[e].from, e));
            }
        }
        out
    }

    /// Node indices in insertion order.
    pub(crate) fn node_indices(&self) -> std::ops::Range<usize> {
        0..self.nodes.len()
    }

    /// Edge at an index.
    pub(crate) fn edge_at(&self, idx: usize) -> &Edge {
        &self.edges[idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PropertyGraph {
        let mut g = PropertyGraph::new();
        g.add_labelled_node("a", &["Doc"]).unwrap();
        g.add_labelled_node("b", &["Doc"]).unwrap();
        g.add_labelled_node("c", &["Person"]).unwrap();
        g.add_edge("a", "b", "CITES", HashMap::new()).unwrap();
        g.add_edge("c", "a", "WROTE", HashMap::new()).unwrap();
        g
    }

    #[test]
    fn duplicate_and_unknown_nodes_are_errors() {
        let mut g = sample();
        assert!(matches!(
            g.add_labelled_node("a", &[]),
            Err(GraphError::DuplicateNode(_))
        ));
        assert!(matches!(
            g.add_edge("a", "missing", "X", HashMap::new()),
            Err(GraphError::UnknownNode(_))
        ));
    }

    #[test]
    fn neighbours_follow_edge_direction() {
        let g = sample();
        let out: Vec<&str> = g
            .neighbors("a", Direction::Outgoing)
            .unwrap()
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(out, ["b"]);
        let inc: Vec<&str> = g
            .neighbors("a", Direction::Incoming)
            .unwrap()
            .iter()
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(inc, ["c"]);
        assert_eq!(g.neighbors("a", Direction::Both).unwrap().len(), 2);
    }

    #[test]
    fn property_values_compare_across_numeric_types() {
        assert_eq!(
            PropertyValue::Int(2).partial_cmp_value(&PropertyValue::Float(2.5)),
            Some(Ordering::Less)
        );
        assert_eq!(
            PropertyValue::Text("a".into()).partial_cmp_value(&PropertyValue::Int(1)),
            None
        );
    }
}
