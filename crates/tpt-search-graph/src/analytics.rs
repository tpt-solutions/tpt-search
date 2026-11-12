//! Graph analytics: components, PageRank, betweenness centrality, and communities.
//!
//! Results are keyed by node id. Every algorithm is deterministic: ties are broken
//! by insertion order, so the same graph always gives the same answer.

use std::collections::{HashMap, VecDeque};

use crate::error::GraphError;
use crate::graph::{Direction, PropertyGraph};

/// Weakly connected components (edge direction ignored), largest first. Each
/// component lists its node ids in insertion order.
pub fn connected_components(graph: &PropertyGraph) -> Vec<Vec<String>> {
    let n = graph.node_count();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for i in graph.node_indices() {
        for (j, _) in graph.adjacent(i, Direction::Outgoing) {
            let (a, b) = (find(&mut parent, i), find(&mut parent, j));
            if a != b {
                parent[a.max(b)] = a.min(b);
            }
        }
    }
    let mut groups: HashMap<usize, Vec<String>> = HashMap::new();
    for i in graph.node_indices() {
        let root = find(&mut parent, i);
        groups
            .entry(root)
            .or_default()
            .push(graph.node_at(i).id.clone());
    }
    let mut out: Vec<Vec<String>> = groups.into_values().collect();
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
    out
}

/// PageRank with damping factor `damping` (conventionally 0.85), following outgoing
/// edges. Dangling nodes spread their rank evenly. Iterates until the total change is
/// below `1e-9` or `max_iterations` is reached. The scores sum to 1.
pub fn pagerank(
    graph: &PropertyGraph,
    damping: f64,
    max_iterations: usize,
) -> Result<HashMap<String, f64>, GraphError> {
    if !(0.0..1.0).contains(&damping) {
        return Err(GraphError::Unsupported(format!(
            "damping {damping} must be in [0, 1)"
        )));
    }
    let n = graph.node_count();
    if n == 0 {
        return Ok(HashMap::new());
    }
    let nf = n as f64;
    let out_degree: Vec<usize> = graph
        .node_indices()
        .map(|i| graph.adjacent(i, Direction::Outgoing).len())
        .collect();
    let mut rank = vec![1.0 / nf; n];
    for _ in 0..max_iterations {
        let dangling: f64 = graph
            .node_indices()
            .filter(|&i| out_degree[i] == 0)
            .map(|i| rank[i])
            .sum();
        let base = (1.0 - damping) / nf + damping * dangling / nf;
        let mut next = vec![base; n];
        for i in graph.node_indices() {
            if out_degree[i] > 0 {
                let share = damping * rank[i] / out_degree[i] as f64;
                for (j, _) in graph.adjacent(i, Direction::Outgoing) {
                    next[j] += share;
                }
            }
        }
        let delta: f64 = rank.iter().zip(&next).map(|(a, b)| (a - b).abs()).sum();
        rank = next;
        if delta < 1e-9 {
            break;
        }
    }
    Ok(graph
        .node_indices()
        .map(|i| (graph.node_at(i).id.clone(), rank[i]))
        .collect())
}

/// Betweenness centrality on the directed, unweighted graph (Brandes' algorithm).
/// Values are unnormalized: each is the number of shortest paths that pass through
/// the node, summed over all source–target pairs.
pub fn betweenness_centrality(graph: &PropertyGraph) -> HashMap<String, f64> {
    let n = graph.node_count();
    let mut centrality = vec![0.0f64; n];
    for s in graph.node_indices() {
        let mut stack = Vec::with_capacity(n);
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut sigma = vec![0.0f64; n];
        let mut dist: Vec<i64> = vec![-1; n];
        sigma[s] = 1.0;
        dist[s] = 0;
        let mut queue = VecDeque::from([s]);
        while let Some(v) = queue.pop_front() {
            stack.push(v);
            for (w, _) in graph.adjacent(v, Direction::Outgoing) {
                if dist[w] < 0 {
                    dist[w] = dist[v] + 1;
                    queue.push_back(w);
                }
                if dist[w] == dist[v] + 1 {
                    sigma[w] += sigma[v];
                    preds[w].push(v);
                }
            }
        }
        let mut delta = vec![0.0f64; n];
        while let Some(w) = stack.pop() {
            for &v in &preds[w] {
                delta[v] += sigma[v] / sigma[w] * (1.0 + delta[w]);
            }
            if w != s {
                centrality[w] += delta[w];
            }
        }
    }
    graph
        .node_indices()
        .map(|i| (graph.node_at(i).id.clone(), centrality[i]))
        .collect()
}

/// Community detection by label propagation over undirected neighbourhoods.
///
/// Each node starts with its own label. In each round, nodes are visited in insertion
/// order and adopt the most frequent label among their neighbours, breaking ties by
/// the smaller label. The process stops when nothing changes or after
/// `max_iterations`. Returns a community number per node id, numbered from 0 in order
/// of first appearance.
pub fn label_propagation(graph: &PropertyGraph, max_iterations: usize) -> HashMap<String, usize> {
    let n = graph.node_count();
    let mut label: Vec<usize> = (0..n).collect();
    for _ in 0..max_iterations {
        let mut changed = false;
        for i in graph.node_indices() {
            let mut counts: HashMap<usize, usize> = HashMap::new();
            for (j, _) in graph.adjacent(i, Direction::Both) {
                *counts.entry(label[j]).or_default() += 1;
            }
            let best = counts
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
                .map(|(l, _)| l);
            if let Some(best) = best {
                if best != label[i] {
                    label[i] = best;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut renumber: HashMap<usize, usize> = HashMap::new();
    graph
        .node_indices()
        .map(|i| {
            let next = renumber.len();
            let community = *renumber.entry(label[i]).or_insert(next);
            (graph.node_at(i).id.clone(), community)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;

    fn graph(edges: &[(&str, &str)]) -> PropertyGraph {
        let mut g = PropertyGraph::new();
        let mut names: Vec<&str> = edges.iter().flat_map(|(a, b)| [*a, *b]).collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            g.add_labelled_node(name, &["N"]).unwrap();
        }
        for (a, b) in edges {
            g.add_edge(a, b, "E", Map::new()).unwrap();
        }
        g
    }

    #[test]
    fn components_ignore_direction() {
        let g = graph(&[("a", "b"), ("c", "b"), ("d", "e")]);
        let comps = connected_components(&g);
        assert_eq!(comps.len(), 2);
        assert_eq!(comps[0].len(), 3);
        assert_eq!(comps[1].len(), 2);
    }

    #[test]
    fn pagerank_sums_to_one_and_favours_popular_nodes() {
        let g = graph(&[("a", "hub"), ("b", "hub"), ("c", "hub"), ("hub", "a")]);
        let ranks = pagerank(&g, 0.85, 200).unwrap();
        let total: f64 = ranks.values().sum();
        assert!((total - 1.0).abs() < 1e-6, "total {total}");
        assert!(ranks["hub"] > ranks["b"]);
    }

    #[test]
    fn pagerank_rejects_bad_damping() {
        let g = graph(&[("a", "b")]);
        assert!(pagerank(&g, 1.0, 10).is_err());
    }

    #[test]
    fn betweenness_finds_the_bridge() {
        // Two triangles joined through "bridge" in each direction.
        let g = graph(&[
            ("a", "bridge"),
            ("bridge", "c"),
            ("d", "bridge"),
            ("bridge", "e"),
        ]);
        let bc = betweenness_centrality(&g);
        assert!(bc["bridge"] > 0.0);
        assert_eq!(bc["a"], 0.0);
        assert_eq!(bc["c"], 0.0);
    }

    #[test]
    fn label_propagation_separates_dense_groups() {
        let g = graph(&[
            ("a1", "a2"),
            ("a2", "a3"),
            ("a3", "a1"),
            ("b1", "b2"),
            ("b2", "b3"),
            ("b3", "b1"),
        ]);
        let c = label_propagation(&g, 50);
        assert_eq!(c["a1"], c["a2"]);
        assert_eq!(c["a2"], c["a3"]);
        assert_eq!(c["b1"], c["b3"]);
        assert_ne!(c["a1"], c["b1"]);
    }
}
