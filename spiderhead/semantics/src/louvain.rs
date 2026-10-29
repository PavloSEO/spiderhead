//! Louvain community detection by modularity optimization.
//! `PyGraph` keeps nodes and adjacency lists in insertion order, so every
//! traversal is deterministic.
//!
//! Evaluation order comes from a seeded shuffle of the node and community
//! lists (`pymt::randomize`, seed 42). Renumbering and modularity sums iterate
//! hash sets in a fixed order through `pyset::PySet<i64>`.

use indexmap::IndexMap;
use std::collections::HashMap;

use crate::pymt::{randomize, RandomState};
use crate::pyset::py_int_set;

const MIN_IMPROVEMENT: f64 = 0.0000001;

/// Undirected weighted graph with insertion-ordered nodes and adjacency.
#[derive(Debug, Default)]
pub struct PyGraph {
    nodes: Vec<String>,
    index: HashMap<String, usize>,
    adj: IndexMap<String, IndexMap<String, f64>>,
}

impl PyGraph {
    /// Empty graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node.
    pub fn add_node(&mut self, n: &str) {
        if !self.index.contains_key(n) {
            self.index.insert(n.to_string(), self.nodes.len());
            self.nodes.push(n.to_string());
        }
    }

    /// Add an edge; a repeated edge updates its weight.
    pub fn add_edge(&mut self, a: &str, b: &str, w: f64) {
        self.add_node(a);
        self.add_node(b);
        self.adj
            .entry(a.to_string())
            .or_default()
            .insert(b.to_string(), w);
        self.adj
            .entry(b.to_string())
            .or_default()
            .insert(a.to_string(), w);
    }

    /// Nodes in insertion order.
    pub fn nodes(&self) -> &[String] {
        &self.nodes
    }

    /// Neighbours of a node in adjacency insertion order.
    pub fn neighbors<'a>(&'a self, n: &'a str) -> impl Iterator<Item = (&'a String, &'a f64)> {
        self.adj.get(n).into_iter().flatten()
    }

    /// Edge weight, or `None` if the edge does not exist.
    pub fn edge_weight(&self, a: &str, b: &str) -> Option<f64> {
        self.adj.get(a).and_then(|m| m.get(b)).copied()
    }

    /// Weighted degree; a self-loop counts twice.
    pub fn degree(&self, n: &str) -> f64 {
        self.neighbors(n).map(|(_, w)| *w).sum::<f64>() + self.edge_weight(n, n).unwrap_or(0.0)
    }

    /// Total edge weight; a self-loop counts once.
    pub fn size(&self) -> f64 {
        self.nodes.iter().map(|n| self.degree(n)).sum::<f64>() / 2.0
    }

    /// Number of edges.
    pub fn n_edges(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| {
                self.neighbors(n)
                    .filter(|(nbr, _)| self.index[*nbr] >= self.index[n])
                    .count()
            })
            .sum()
    }

    /// Each undirected edge once, in first-seen order;
    /// the first endpoint is the earlier node in insertion order.
    pub fn edges(&self) -> Vec<(String, String, f64)> {
        let mut out = Vec::new();
        for n in &self.nodes {
            for (nbr, w) in self.neighbors(n) {
                if self.index[nbr] >= self.index[n] {
                    out.push((n.clone(), nbr.clone(), *w));
                }
            }
        }
        out
    }
}

/// Per-community bookkeeping: `node2com`, `degrees`, `internals`,
/// `gdegrees`, `loops`, `total_weight`.
#[derive(Debug, Default)]
struct Status {
    node2com: IndexMap<String, i64>,
    total_weight: f64,
    degrees: IndexMap<i64, f64>,
    internals: IndexMap<i64, f64>,
    gdegrees: HashMap<String, f64>,
    loops: HashMap<String, f64>,
}

impl Status {
    /// Start with every node in its own community.
    fn init(&mut self, graph: &PyGraph) {
        self.node2com.clear();
        self.total_weight = 0.0;
        self.degrees.clear();
        self.gdegrees.clear();
        self.internals.clear();
        self.loops.clear();
        self.total_weight = graph.size();
        for (count, node) in graph.nodes().iter().enumerate() {
            self.node2com.insert(node.clone(), count as i64);
            let deg = graph.degree(node);
            debug_assert!(deg >= 0.0, "Bad node degree ({deg})");
            self.degrees.insert(count as i64, deg);
            self.gdegrees.insert(node.clone(), deg);
            let self_loop = graph.edge_weight(node, node).unwrap_or(0.0);
            self.loops.insert(node.clone(), self_loop);
            self.internals.insert(count as i64, self_loop);
        }
    }
}

/// Map of neighbouring community to edge weight, in adjacency order.
fn neighcom(node: &str, graph: &PyGraph, status: &Status) -> IndexMap<i64, f64> {
    let mut weights: IndexMap<i64, f64> = IndexMap::new();
    for (neighbor, w) in graph.neighbors(node).collect::<Vec<_>>() {
        if neighbor != node {
            let com = status.node2com[neighbor];
            *weights.entry(com).or_insert(0.0) += *w;
        }
    }
    weights
}

/// Modularity; communities are summed in hash-set order
/// (the order affects the f64 result).
fn modularity(status: &Status, resolution: f64) -> f64 {
    let links = status.total_weight;
    let communities = py_int_set(status.node2com.values().copied());
    let mut result = 0.0;
    for com in communities.iter() {
        let in_degree = status.internals.get(&com).copied().unwrap_or(0.0);
        let degree = status.degrees.get(&com).copied().unwrap_or(0.0);
        if links > 0.0 {
            result += in_degree * resolution / links - (degree / (2.0 * links)).powi(2);
        }
    }
    result
}

/// One level of the Louvain local-moving pass.
fn one_level(graph: &PyGraph, status: &mut Status, resolution: f64, rs: &mut RandomState) {
    let mut modified = true;
    let mut cur_mod = modularity(status, resolution);
    let mut new_mod = cur_mod;
    while modified {
        cur_mod = new_mod;
        modified = false;
        for node in randomize(graph.nodes(), rs) {
            let com_node = status.node2com[&node];
            let degc_totw = status.gdegrees[&node] / (status.total_weight * 2.0);
            let neigh = neighcom(&node, graph, status);
            let remove_cost = -neigh.get(&com_node).copied().unwrap_or(0.0)
                + resolution
                    * (status.degrees.get(&com_node).copied().unwrap_or(0.0)
                        - status.gdegrees[&node])
                    * degc_totw;
            let w = neigh.get(&com_node).copied().unwrap_or(0.0);
            *status.degrees.entry(com_node).or_insert(0.0) -= status.gdegrees[&node];
            *status.internals.entry(com_node).or_insert(0.0) -=
                w + status.loops.get(&node).copied().unwrap_or(0.0);
            status.node2com.insert(node.clone(), -1);
            let mut best_com = com_node;
            let mut best_increase = 0.0f64;
            let items: Vec<(i64, f64)> = neigh.iter().map(|(k, v)| (*k, *v)).collect();
            for (com, dnc) in randomize(&items, rs) {
                let incr = remove_cost + dnc
                    - resolution * status.degrees.get(&com).copied().unwrap_or(0.0) * degc_totw;
                if incr > best_increase {
                    best_increase = incr;
                    best_com = com;
                }
            }
            status.node2com.insert(node.clone(), best_com);
            *status.degrees.entry(best_com).or_insert(0.0) += status.gdegrees[&node];
            let w2 = neigh.get(&best_com).copied().unwrap_or(0.0);
            *status.internals.entry(best_com).or_insert(0.0) +=
                w2 + status.loops.get(&node).copied().unwrap_or(0.0);
            if best_com != com_node {
                modified = true;
            }
        }
        new_mod = modularity(status, resolution);
        if new_mod - cur_mod < MIN_IMPROVEMENT {
            break;
        }
    }
}

/// Compact community ids to `0..n`, using the set-order walk.
fn renumber(dictionary: &IndexMap<String, i64>) -> IndexMap<String, i64> {
    let values = py_int_set(dictionary.values().copied());
    let target = py_int_set(0..values.len() as i64);
    let same = values.len() == target.len() && values.iter().all(|v| target.contains(&v));
    if same {
        return dictionary.clone();
    }
    let mut renumbering: HashMap<i64, i64> = HashMap::new();
    for v in target.intersection(&values).iter() {
        renumbering.insert(v, v);
    }
    for (a, b) in values
        .difference(&target)
        .iter()
        .zip(target.difference(&values).iter())
    {
        renumbering.insert(a, b);
    }
    dictionary
        .iter()
        .map(|(k, v)| (k.clone(), renumbering[v]))
        .collect()
}

/// `induced_graph` — communities become nodes; edge weights accumulate in
/// the stored edge order.
fn induced_graph(partition: &IndexMap<String, i64>, graph: &PyGraph) -> PyGraph {
    let mut ret = PyGraph::new();
    for com in partition.values() {
        ret.add_node(&com.to_string());
    }
    for (n1, n2, w) in graph.edges() {
        let c1 = partition[&n1].to_string();
        let c2 = partition[&n2].to_string();
        let prev = ret.edge_weight(&c1, &c2).unwrap_or(0.0);
        ret.add_edge(&c1, &c2, prev + w);
    }
    ret
}

/// Dendrogram levels; the last level is the best
/// partition.
fn generate_dendrogram(
    graph: &PyGraph,
    resolution: f64,
    rs: &mut RandomState,
) -> Vec<IndexMap<String, i64>> {
    if graph.n_edges() == 0 {
        let mut part = IndexMap::new();
        for (i, node) in graph.nodes().iter().enumerate() {
            part.insert(node.clone(), i as i64);
        }
        return vec![part];
    }
    let mut current_graph = PyGraph::new();
    for n in graph.nodes() {
        current_graph.add_node(n);
    }
    for (a, b, w) in graph.edges() {
        current_graph.add_edge(&a, &b, w);
    }
    let mut status = Status::default();
    status.init(&current_graph);
    let mut levels = Vec::new();
    one_level(&current_graph, &mut status, resolution, rs);
    let mut new_mod = modularity(&status, resolution);
    let mut partition = renumber(&status.node2com);
    levels.push(partition.clone());
    let mut mod_prev = new_mod;
    current_graph = induced_graph(&partition, &current_graph);
    status.init(&current_graph);
    loop {
        one_level(&current_graph, &mut status, resolution, rs);
        new_mod = modularity(&status, resolution);
        if new_mod - mod_prev < MIN_IMPROVEMENT {
            break;
        }
        partition = renumber(&status.node2com);
        levels.push(partition.clone());
        mod_prev = new_mod;
        current_graph = induced_graph(&partition, &current_graph);
        status.init(&current_graph);
    }
    levels
}

/// Best partition with `weight="weight"`, the given resolution and seed 42.
/// Returns `{node: community}`.
pub fn best_partition(graph: &PyGraph, resolution: f64, seed: u32) -> IndexMap<String, i64> {
    let mut rs = RandomState::new(seed);
    let dendo = generate_dendrogram(graph, resolution, &mut rs);
    // Take the last (best) dendrogram level.
    let mut partition = dendo[0].clone();
    for level in &dendo[1..] {
        for com in partition.values_mut() {
            *com = level[&com.to_string()];
        }
    }
    partition
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(nodes: &[&str], edges: &[(&str, &str, f64)]) -> PyGraph {
        let mut g = PyGraph::new();
        for n in nodes {
            g.add_node(n);
        }
        for (a, b, w) in edges {
            g.add_edge(a, b, *w);
        }
        g
    }

    // Fixed expected partitions: weight="weight", resolution=1.2, seed 42.
    #[test]
    fn best_partition_two_triangles_and_chain() {
        let g = graph(
            &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k"],
            &[
                ("a", "b", 5.0),
                ("b", "c", 5.0),
                ("a", "c", 5.0),
                ("d", "e", 4.0),
                ("e", "f", 4.0),
                ("d", "f", 4.0),
                ("c", "d", 2.0),
                ("g", "h", 3.0),
                ("h", "i", 3.0),
                ("g", "i", 2.0),
                ("i", "j", 2.0),
                ("j", "k", 2.0),
            ],
        );
        let part = best_partition(&g, 1.2, 42);
        let expected: Vec<(&str, i64)> = vec![
            ("a", 0),
            ("b", 0),
            ("c", 0),
            ("d", 1),
            ("e", 1),
            ("f", 1),
            ("g", 2),
            ("h", 2),
            ("i", 2),
            ("j", 2),
            ("k", 2),
        ];
        assert_eq!(
            part.iter()
                .map(|(k, v)| (k.as_str(), *v))
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn best_partition_sixty_node_ring_of_cliques() {
        let nodes: Vec<String> = (0..60).map(|i| format!("n{i}")).collect();
        let node_refs: Vec<&str> = nodes.iter().map(|s| s.as_str()).collect();
        let mut edge_list: Vec<(String, String, f64)> = Vec::new();
        for c in 0..6usize {
            for i in 0..10 {
                for j in (i + 1)..10 {
                    edge_list.push((nodes[c * 10 + i].clone(), nodes[c * 10 + j].clone(), 3.0));
                }
            }
            if c > 0 {
                edge_list.push((nodes[c * 10].clone(), nodes[(c - 1) * 10].clone(), 2.0));
            }
        }
        let edge_refs: Vec<(&str, &str, f64)> = edge_list
            .iter()
            .map(|(a, b, w)| (a.as_str(), b.as_str(), *w))
            .collect();
        let g = graph(&node_refs, &edge_refs);
        let part = best_partition(&g, 1.2, 42);
        let expected_com = |i: usize| -> i64 {
            match i / 10 {
                0 => 2,
                1 => 3,
                2 => 5,
                3 => 0,
                4 => 1,
                5 => 4,
                _ => unreachable!(),
            }
        };
        for (i, n) in nodes.iter().enumerate() {
            assert_eq!(part[n], expected_com(i), "node {n}");
        }
        // The first level keeps the node insertion order too.
        assert_eq!(part.keys().cloned().collect::<Vec<_>>(), nodes);
    }
}
