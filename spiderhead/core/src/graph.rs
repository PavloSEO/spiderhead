//! Neutral stored-graph contracts shared by the storage and analysis layers.
//!
//! Records are plain structs and the protocols are object-safe traits. Iterators stay lazy and borrow the implementation and the caller's predicates for their lifetime.
//! Mixed-value maps are `serde_json` maps, since the stored rows are JSON.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

/// One anchor text that links one source page to one destination.
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorGroup {
    /// The page the links sit on.
    pub source_url: String,
    /// How many links share this anchor and destination.
    pub occurrences_count: u64,
    /// Links whose anchor text is generic (for example "click here").
    pub generic_links: Vec<Map<String, Value>>,
    /// Where each link sits on the page, capped by the caller.
    pub locations: Vec<Map<String, Value>>,
}

/// Input of the link-score computation (PageRank-style).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkScoreConstants {
    /// Damping factor.
    pub damping: f64,
    /// Iteration cap.
    pub max_iterations: u64,
    /// Convergence tolerance.
    pub tolerance: f64,
}

/// Read access to computed link scores.
pub trait ScoreView {
    /// Number of scored URLs.
    fn count(&self) -> u64;
    /// Median score over all URLs.
    fn median(&self) -> f64;
    /// Score of one normalised URL, when it was scored.
    fn score_for(&self, normalized_url: &str) -> Option<f64>;
}

/// Inlink composition of one destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlinkCompositionRow {
    /// The destination key.
    pub destination_key: String,
    /// Inlinks to the destination.
    pub occurrences_count: u64,
    /// Whether every inlink is nofollow.
    pub all_nofollow: bool,
    /// Whether every inlink's source is known.
    pub has_known_source: bool,
    /// Whether at least one inlink's source is indexable.
    pub has_indexable_source: bool,
    /// A few example sources, capped by the caller.
    pub source_examples: Vec<String>,
}

/// One source page repeating the same (destination, anchor) more than once.
///
/// `surplus_total` counts the repeats beyond the first occurrence of each pair: how much of this
/// page's link graph is a copy of itself. `repeats` lists the pairs, capped by the caller.
#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateLinkGroup {
    /// The page the links sit on.
    pub source_url: String,
    /// Repeats beyond the first occurrence of each pair.
    pub surplus_total: u64,
    /// The repeated pairs.
    pub repeats: Vec<Map<String, Value>>,
}

/// A shortest-path session from one seed URL.
pub trait PathSession {
    /// Click depth of a target, when reachable.
    fn depth_for(&self, target: &str) -> Option<u32>;
    /// The path from the seed to a target, when reachable.
    fn path_to(&self, target: &str) -> Option<Vec<String>>;
    /// Every reached URL with its depth.
    fn iter_depths<'a>(&'a self) -> Box<dyn Iterator<Item = (String, u32)> + 'a>;
}

/// Check-independent graph facts. The analysis layer keeps every threshold and emission.
pub trait GraphAccess {
    /// Whether resource-type rows are stored.
    fn has_resource_type(&self) -> bool;
    /// Whether internal hyperlinks are stored.
    fn has_internal_hyperlinks(&self) -> bool;
    /// Anchor groups, lazily, with the generic-anchor predicate applied by the caller.
    fn iter_anchor_groups<'a>(
        &'a self,
        is_generic_anchor: &'a dyn Fn(&str) -> bool,
        max_locations: usize,
    ) -> Box<dyn Iterator<Item = AnchorGroup> + 'a>;
    /// Link scores, when computed for these constants.
    fn link_score(&self, constants: LinkScoreConstants) -> Option<Box<dyn ScoreView>>;
    /// Inlink composition per destination, lazily.
    fn iter_inlink_composition<'a>(
        &'a self,
        is_indexable_source: &'a dyn Fn(&str) -> Option<bool>,
        max_source_examples: usize,
    ) -> Box<dyn Iterator<Item = InlinkCompositionRow> + 'a>;
    /// A path session from `seed`, when the graph can answer path questions.
    fn begin_paths(&self, seed: &str) -> Option<Box<dyn PathSession>>;
    /// Link position totals by position name.
    fn position_totals(&self) -> BTreeMap<String, u64>;
    /// Duplicate-link groups, lazily, each listing at most `max_repeats` pairs.
    fn iter_duplicate_links<'a>(
        &'a self,
        max_repeats: usize,
    ) -> Box<dyn Iterator<Item = DuplicateLinkGroup> + 'a>;
    /// Resource rows as (source, destination, resource type), lazily.
    fn iter_resources<'a>(&'a self) -> Box<dyn Iterator<Item = (String, String, String)> + 'a>;
    /// Evidence links in crawl order: one record per stored link with
    /// `source_url`, `destination_url`, `anchor` and `link_position` (the
    /// latter two `null` when unknown). The fact-only attribution checks read
    /// this projection instead of the per-check SQL each other reader owns.
    fn iter_evidence_links<'a>(&'a self) -> Box<dyn Iterator<Item = Map<String, Value>> + 'a>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A graph with one page and one destination, enough to exercise object safety.
    struct OnePage;

    impl GraphAccess for OnePage {
        fn has_resource_type(&self) -> bool {
            false
        }

        fn has_internal_hyperlinks(&self) -> bool {
            true
        }

        fn iter_anchor_groups<'a>(
            &'a self,
            is_generic_anchor: &'a dyn Fn(&str) -> bool,
            _max_locations: usize,
        ) -> Box<dyn Iterator<Item = AnchorGroup> + 'a> {
            let generic = is_generic_anchor("click here");
            Box::new(std::iter::once(AnchorGroup {
                source_url: "https://example.test/source".into(),
                occurrences_count: 1,
                generic_links: if generic {
                    vec![json!({"anchor": "click here"})
                        .as_object()
                        .cloned()
                        .unwrap_or_default()]
                } else {
                    vec![]
                },
                locations: vec![],
            }))
        }

        fn link_score(&self, _constants: LinkScoreConstants) -> Option<Box<dyn ScoreView>> {
            None
        }

        fn iter_inlink_composition<'a>(
            &'a self,
            _is_indexable_source: &'a dyn Fn(&str) -> Option<bool>,
            _max_source_examples: usize,
        ) -> Box<dyn Iterator<Item = InlinkCompositionRow> + 'a> {
            Box::new(std::iter::empty())
        }

        fn begin_paths(&self, _seed: &str) -> Option<Box<dyn PathSession>> {
            None
        }

        fn position_totals(&self) -> BTreeMap<String, u64> {
            BTreeMap::new()
        }

        fn iter_duplicate_links<'a>(
            &'a self,
            _max_repeats: usize,
        ) -> Box<dyn Iterator<Item = DuplicateLinkGroup> + 'a> {
            Box::new(std::iter::empty())
        }

        fn iter_resources<'a>(&'a self) -> Box<dyn Iterator<Item = (String, String, String)> + 'a> {
            Box::new(std::iter::empty())
        }

        fn iter_evidence_links<'a>(&'a self) -> Box<dyn Iterator<Item = Map<String, Value>> + 'a> {
            Box::new(std::iter::empty())
        }
    }

    // Behaviour of the neutral contracts.
    #[test]
    fn graph_access_is_object_safe_and_iterators_stay_lazy() {
        let graph: Box<dyn GraphAccess> = Box::new(OnePage);
        let predicate = |text: &str| text == "click here";
        let groups: Vec<AnchorGroup> = graph.iter_anchor_groups(&predicate, 5).collect();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].generic_links.len(), 1);
        assert!(graph.begin_paths("https://example.test/").is_none());
        assert!(graph
            .link_score(LinkScoreConstants {
                damping: 0.85,
                max_iterations: 100,
                tolerance: 1e-6,
            })
            .is_none());
        assert!(graph.position_totals().is_empty());
    }

    #[test]
    fn duplicate_link_group_keeps_surplus_and_repeats_apart() {
        let group = DuplicateLinkGroup {
            source_url: "https://example.test/".into(),
            surplus_total: 2,
            repeats: vec![],
        };
        assert_eq!(group.surplus_total, 2);
        assert!(group.repeats.is_empty());
    }
}
