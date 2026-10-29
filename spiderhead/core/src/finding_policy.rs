//! Check-id policy sets and the implausibility threshold for audit findings.
//!
//! Check-id policy sets and the implausibility threshold for audit findings. The sets name checks whose findings assert a negative over the whole site or a crawl-wide property.
//! Those findings are withheld on a partial crawl rather than reported as fact.

/// Share of crawled pages above which a finding is named as implausible (strict majority).
pub const IMPLAUSIBLE_SHARE: f64 = 0.5;

/// Findings asserting "nothing links here" over the whole site. Provable only on a complete
/// crawl, so they are withheld on a partial one: a partial crawl cannot support specific conclusions.
pub const UNLINKED_FINDING_CHECKS: &[&str] = &[
    "ORPHAN_PAGE",
    "SITEMAP_ORPHAN",
    "UNLINKED_CANONICAL",
    "HREFLANG_UNLINKED_TARGET",
    "UNLINKED_PAGINATION_SERIES",
];

/// Findings judging the whole link graph (link score, nofollow-only or non-indexable inlinks,
/// discovery depth). Withheld on a partial crawl for the same reason as the unlinked set.
pub const GRAPH_WIDE_FINDING_CHECKS: &[&str] = &[
    "LOW_LINK_SCORE",
    "ONLY_NOFOLLOW_INLINKS",
    "ONLY_NONINDEXABLE_SOURCE_INLINKS",
    "DEEP_DISCOVERY_PATH",
];

/// Click-depth findings. A depth verdict cannot be proven when the crawl did not reach every
/// URL, because the part never fetched may hold a shorter route.
pub const DEPTH_FINDING_CHECKS: &[&str] = &["DEEP_CLICK_DEPTH"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implausible_share_is_a_strict_majority() {
        assert!((IMPLAUSIBLE_SHARE - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn policy_sets_hold_the_reference_check_ids() {
        assert_eq!(UNLINKED_FINDING_CHECKS.len(), 5);
        assert!(UNLINKED_FINDING_CHECKS.contains(&"ORPHAN_PAGE"));
        assert!(UNLINKED_FINDING_CHECKS.contains(&"UNLINKED_PAGINATION_SERIES"));
        assert_eq!(GRAPH_WIDE_FINDING_CHECKS.len(), 4);
        assert!(GRAPH_WIDE_FINDING_CHECKS.contains(&"DEEP_DISCOVERY_PATH"));
        assert_eq!(DEPTH_FINDING_CHECKS, &["DEEP_CLICK_DEPTH"]);
    }

    #[test]
    fn policy_sets_do_not_overlap() {
        for check in UNLINKED_FINDING_CHECKS {
            assert!(!GRAPH_WIDE_FINDING_CHECKS.contains(check));
            assert!(!DEPTH_FINDING_CHECKS.contains(check));
        }
        for check in GRAPH_WIDE_FINDING_CHECKS {
            assert!(!DEPTH_FINDING_CHECKS.contains(check));
        }
    }
}
