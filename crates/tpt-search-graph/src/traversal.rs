//! Traversal: breadth-first and depth-first walks and shortest paths.
//!
//! Walks expand through every node, but only nodes passing the [`TraversalFilter`]
//! are reported. A filter therefore selects results without cutting paths.

use std::collections::{HashSet, VecDeque};

use crate::error::GraphError;
use crate::graph::{Direction, Node, PropertyGraph, PropertyValue};

/// How far and which way a walk goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalPattern {
    /// Edge direction to follow.
    pub direction: Direction,
    /// Only follow edges with this label, if set.
    pub edge_label: Option<String>,
    /// Deepest hop count to report (at least 1).
    pub max_depth: usize,
}

impl TraversalPattern {
    /// Outgoing edges, unlimited depth (bounded by graph size), any label.
    pub fn outgoing() -> Self {
        TraversalPattern {
            direction: Direction::Outgoing,
            edge_label: None,
            max_depth: usize::MAX,
        }
    }
}

/// Which reached nodes are reported.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TraversalFilter {
    /// The node must carry this label.
    pub node_label: Option<String>,
    /// The node must have this property with this value.
    pub property: Option<(String, PropertyValue)>,
}

impl TraversalFilter {
    /// Whether a node passes this filter.
    pub fn accepts(&self, node: &Node) -> bool {
        if let Some(label) = &self.node_label {
            if !node.has_label(label) {
                return false;
            }
        }
        if let Some((name, value)) = &self.property {
            if node.property(name) != Some(value) {
                return false;
            }
        }
        true
    }
}

/// A reached node and its hop count from the start node.
pub type Reached<'a> = (&'a Node, usize);

/// Breadth-first walk from `start`. Reports reached nodes in order of hop count.
/// The start node itself is not reported.
pub fn bfs<'a>(
    graph: &'a PropertyGraph,
    start: &str,
    pattern: &TraversalPattern,
    filter: &TraversalFilter,
) -> Result<Vec<Reached<'a>>, GraphError> {
    let start_idx = graph.node_index(start)?;
    let mut seen = HashSet::from([start_idx]);
    let mut queue = VecDeque::from([(start_idx, 0usize)]);
    let mut out = Vec::new();
    while let Some((node, depth)) = queue.pop_front() {
        if depth == pattern.max_depth {
            continue;
        }
        for (next, _) in step(graph, node, pattern) {
            if seen.insert(next) {
                let n = graph.node_at(next);
                if filter.accepts(n) {
                    out.push((n, depth + 1));
                }
                queue.push_back((next, depth + 1));
            }
        }
    }
    Ok(out)
}

/// Depth-first walk from `start`, pre-order. Reports reached nodes with their depth.
pub fn dfs<'a>(
    graph: &'a PropertyGraph,
    start: &str,
    pattern: &TraversalPattern,
    filter: &TraversalFilter,
) -> Result<Vec<Reached<'a>>, GraphError> {
    let start_idx = graph.node_index(start)?;
    let mut seen = HashSet::from([start_idx]);
    let mut stack = vec![(start_idx, 0usize)];
    let mut out = Vec::new();
    while let Some((node, depth)) = stack.pop() {
        // Report on visit (pre-order), so the output follows the walk order.
        if node != start_idx {
            let node_ref = graph.node_at(node);
            if filter.accepts(node_ref) {
                out.push((node_ref, depth));
            }
        }
        if depth == pattern.max_depth {
            continue;
        }
        // Push in reverse so that the first neighbour is visited first.
        let mut next: Vec<usize> = step(graph, node, pattern)
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        next.reverse();
        for n in next {
            if seen.insert(n) {
                stack.push((n, depth + 1));
            }
        }
    }
    Ok(out)
}

/// Shortest path in hops from `from` to `to`, following edges as `direction` and
/// `edge_label` allow. Returns the node ids along the path, including both ends.
/// Returns `None` if no path exists.
pub fn shortest_path(
    graph: &PropertyGraph,
    from: &str,
    to: &str,
    direction: Direction,
    edge_label: Option<&str>,
) -> Result<Option<Vec<String>>, GraphError> {
    let src = graph.node_index(from)?;
    let dst = graph.node_index(to)?;
    let pattern = TraversalPattern {
        direction,
        edge_label: edge_label.map(str::to_owned),
        max_depth: usize::MAX,
    };
    let mut parent: Vec<Option<usize>> = vec![None; graph.node_count()];
    let mut seen = vec![false; graph.node_count()];
    seen[src] = true;
    let mut queue = VecDeque::from([src]);
    while let Some(node) = queue.pop_front() {
        if node == dst {
            let mut path = vec![dst];
            let mut cur = dst;
            while let Some(p) = parent[cur] {
                path.push(p);
                cur = p;
            }
            path.reverse();
            return Ok(Some(
                path.into_iter()
                    .map(|i| graph.node_at(i).id.clone())
                    .collect(),
            ));
        }
        for (next, _) in step(graph, node, &pattern) {
            if !seen[next] {
                seen[next] = true;
                parent[next] = Some(node);
                queue.push_back(next);
            }
        }
    }
    Ok(None)
}

/// Neighbours of `node` that the pattern allows, as `(node index, edge index)`.
fn step(graph: &PropertyGraph, node: usize, pattern: &TraversalPattern) -> Vec<(usize, usize)> {
    graph
        .adjacent(node, pattern.direction)
        .into_iter()
        .filter(|(_, e)| {
            pattern
                .edge_label
                .as_deref()
                .is_none_or(|label| graph.edge_at(*e).label == label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// a -> b -> c -> d, a -> e, and x -> a (incoming to a).
    fn chain() -> PropertyGraph {
        let mut g = PropertyGraph::new();
        for id in ["a", "b", "c", "d", "e", "x"] {
            g.add_labelled_node(id, &["N"]).unwrap();
        }
        g.add_edge("a", "b", "NEXT", HashMap::new()).unwrap();
        g.add_edge("b", "c", "NEXT", HashMap::new()).unwrap();
        g.add_edge("c", "d", "NEXT", HashMap::new()).unwrap();
        g.add_edge("a", "e", "OTHER", HashMap::new()).unwrap();
        g.add_edge("x", "a", "NEXT", HashMap::new()).unwrap();
        g
    }

    fn ids<'a>(v: &[Reached<'a>]) -> Vec<(&'a str, usize)> {
        v.iter().map(|(n, d)| (n.id.as_str(), *d)).collect()
    }

    #[test]
    fn bfs_reports_by_depth() {
        let g = chain();
        let out = bfs(
            &g,
            "a",
            &TraversalPattern::outgoing(),
            &TraversalFilter::default(),
        )
        .unwrap();
        assert_eq!(ids(&out), [("b", 1), ("e", 1), ("c", 2), ("d", 3)]);
    }

    #[test]
    fn label_and_depth_limits_apply() {
        let g = chain();
        let pattern = TraversalPattern {
            direction: Direction::Outgoing,
            edge_label: Some("NEXT".into()),
            max_depth: 2,
        };
        let out = bfs(&g, "a", &pattern, &TraversalFilter::default()).unwrap();
        assert_eq!(ids(&out), [("b", 1), ("c", 2)]);
    }

    #[test]
    fn dfs_visits_in_preorder() {
        let g = chain();
        let out = dfs(
            &g,
            "a",
            &TraversalPattern::outgoing(),
            &TraversalFilter::default(),
        )
        .unwrap();
        assert_eq!(ids(&out), [("b", 1), ("c", 2), ("d", 3), ("e", 1)]);
    }

    #[test]
    fn filter_selects_without_cutting_paths() {
        let mut g = PropertyGraph::new();
        g.add_labelled_node("a", &["N"]).unwrap();
        g.add_labelled_node("hidden", &["Skip"]).unwrap();
        g.add_labelled_node("far", &["Target"]).unwrap();
        g.add_edge("a", "hidden", "R", HashMap::new()).unwrap();
        g.add_edge("hidden", "far", "R", HashMap::new()).unwrap();
        let filter = TraversalFilter {
            node_label: Some("Target".into()),
            property: None,
        };
        let out = bfs(&g, "a", &TraversalPattern::outgoing(), &filter).unwrap();
        assert_eq!(ids(&out), [("far", 2)]);
    }

    #[test]
    fn shortest_path_respects_direction_and_label() {
        let g = chain();
        let path = shortest_path(&g, "a", "d", Direction::Outgoing, None).unwrap();
        assert_eq!(path, Some(["a", "b", "c", "d"].map(String::from).to_vec()));
        assert_eq!(
            shortest_path(&g, "a", "x", Direction::Outgoing, None).unwrap(),
            None
        );
        let undirected = shortest_path(&g, "x", "d", Direction::Both, Some("NEXT")).unwrap();
        assert_eq!(undirected.map(|p| p.len()), Some(5));
        assert_eq!(
            shortest_path(&g, "a", "e", Direction::Outgoing, Some("NEXT")).unwrap(),
            None
        );
    }

    #[test]
    fn unknown_start_is_an_error() {
        let g = chain();
        assert!(matches!(
            bfs(
                &g,
                "nope",
                &TraversalPattern::outgoing(),
                &TraversalFilter::default()
            ),
            Err(GraphError::UnknownNode(_))
        ));
    }
}
