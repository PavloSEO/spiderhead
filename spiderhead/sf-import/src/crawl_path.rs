//! Shortest discovery routes from a seed over a finished internal link graph.
//!
//! The crawler's own depth column gives distances but not the route, and a
//! route can only be rebuilt once every edge is known.

use std::collections::{BTreeMap, VecDeque};

fn adjacency(edges: &[(String, String)]) -> BTreeMap<&str, Vec<&str>> {
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (a, b) in edges {
        adj.entry(a).or_default().push(b);
    }
    adj
}

/// `{url: [seed, ..., url]}` for every node reachable from `seed`, by hop count.
/// The seed maps to a zero-hop path; unreachable nodes have no entry.
pub fn shortest_paths_from_seed(
    edges: &[(String, String)],
    seed: &str,
) -> BTreeMap<String, Vec<String>> {
    let adj = adjacency(edges);
    let mut paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    paths.insert(seed.to_string(), vec![seed.to_string()]);
    let mut queue: VecDeque<String> = VecDeque::from([seed.to_string()]);
    while let Some(current) = queue.pop_front() {
        for next in adj.get(current.as_str()).into_iter().flatten() {
            if paths.contains_key(*next) {
                continue;
            }
            let mut route = paths[&current].clone();
            route.push((*next).to_string());
            paths.insert((*next).to_string(), route);
            queue.push_back((*next).to_string());
        }
    }
    paths
}

/// Breadth-first tree from `seed`: `(depths, parents)`. Keeping only the tree
/// costs one entry per node, where holding every route is quadratic on a
/// chain-shaped archive.
pub fn bfs_tree_from_seed(
    edges: &[(String, String)],
    seed: &str,
) -> (BTreeMap<String, usize>, BTreeMap<String, Option<String>>) {
    let adj = adjacency(edges);
    let mut depths = BTreeMap::from([(seed.to_string(), 0usize)]);
    let mut parents: BTreeMap<String, Option<String>> = BTreeMap::from([(seed.to_string(), None)]);
    let mut queue: VecDeque<String> = VecDeque::from([seed.to_string()]);
    while let Some(current) = queue.pop_front() {
        let depth = depths[&current] + 1;
        for next in adj.get(current.as_str()).into_iter().flatten() {
            if depths.contains_key(*next) {
                continue;
            }
            depths.insert((*next).to_string(), depth);
            parents.insert((*next).to_string(), Some(current.clone()));
            queue.push_back((*next).to_string());
        }
    }
    (depths, parents)
}

/// Just the distances of [`bfs_tree_from_seed`].
pub fn shortest_depths_from_seed(
    edges: &[(String, String)],
    seed: &str,
) -> BTreeMap<String, usize> {
    bfs_tree_from_seed(edges, seed).0
}

/// The route to `target`, walked back up the parent tree, or `None` if unreached.
pub fn route_from_parents(
    parents: &BTreeMap<String, Option<String>>,
    target: &str,
) -> Option<Vec<String>> {
    if !parents.contains_key(target) {
        return None;
    }
    let mut route = Vec::new();
    let mut current = Some(target.to_string());
    while let Some(node) = current {
        current = parents[&node].clone();
        route.push(node);
    }
    route.reverse();
    Some(route)
}
