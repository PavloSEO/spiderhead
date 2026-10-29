//! Internal link score over a finished internal follow-link graph.
//!
//! One new edge changes every page's score, so this is a whole-graph
//! computation that can only run once the crawl has stopped discovering pages.

use std::collections::{BTreeMap, BTreeSet};

/// The classic PageRank damping factor: the probability a random surfer keeps
/// following links rather than jumping to an arbitrary page.
pub const DEFAULT_DAMPING: f64 = 0.85;
/// Iteration cap; the tolerance normally stops the walk first.
pub const DEFAULT_MAX_ITERATIONS: usize = 200;
/// Converged once no score moves more than this between rounds.
pub const DEFAULT_TOLERANCE: f64 = 1e-10;

/// Power-iteration scores for every node; scores over all nodes sum to 1.
///
/// `edges` must already be the internal, followed edge list. `urls` adds nodes
/// that have no edges at all: an isolated internal page still holds a share.
/// A dangling node (no outlink) spreads its share over every page so the total
/// stays conserved each round. Self-links carry no distinct evidence and are
/// skipped.
pub fn compute_link_scores(
    edges: &[(String, String)],
    urls: Option<&BTreeSet<String>>,
    damping: f64,
    max_iterations: usize,
    tolerance: f64,
) -> BTreeMap<String, f64> {
    let mut nodes: BTreeSet<&str> = urls
        .map(|u| u.iter().map(String::as_str).collect())
        .unwrap_or_default();
    for (a, b) in edges {
        nodes.insert(a);
        nodes.insert(b);
    }
    if nodes.is_empty() {
        return BTreeMap::new();
    }
    let n = nodes.len() as f64;

    let mut adjacency: BTreeMap<&str, Vec<&str>> = nodes.iter().map(|u| (*u, Vec::new())).collect();
    for (a, b) in edges {
        if a != b {
            adjacency
                .get_mut(a.as_str())
                .expect("edge node registered")
                .push(b);
        }
    }

    let mut score: BTreeMap<&str, f64> = nodes.iter().map(|u| (*u, 1.0 / n)).collect();
    let base = (1.0 - damping) / n;
    let dangling: Vec<&str> = nodes
        .iter()
        .copied()
        .filter(|u| adjacency[u].is_empty())
        .collect();

    for _ in 0..max_iterations {
        let dangling_mass: f64 = dangling.iter().map(|u| score[u]).sum();
        let mut next: BTreeMap<&str, f64> = nodes
            .iter()
            .map(|u| (*u, base + damping * dangling_mass / n))
            .collect();
        for u in &nodes {
            let out = &adjacency[u];
            if out.is_empty() {
                continue;
            }
            let share = damping * score[u] / out.len() as f64;
            for v in out {
                *next.get_mut(v).expect("edge node registered") += share;
            }
        }
        let delta = nodes
            .iter()
            .map(|u| (next[u] - score[u]).abs())
            .fold(0.0, f64::max);
        score = next;
        if delta < tolerance {
            break;
        }
    }
    score.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}
