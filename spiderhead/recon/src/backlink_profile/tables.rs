//! Column aliases and metric names for backlink imports.

use super::*;

/// Normalized field to candidate CSV headers; the first match wins.
pub(super) static ALIASES: LazyLock<IndexMap<&'static str, Vec<&'static str>>> =
    LazyLock::new(|| {
        IndexMap::from([
            (
                "source_url",
                vec![
                    "source_url",
                    "url_from",
                    "referring page url",
                    "sourceurl",
                    "source url",
                    "source",
                    "linking page",
                    "linking page url",
                    "referring page",
                    "url",
                ],
            ),
            (
                "target_url",
                vec![
                    "target_url",
                    "url_to",
                    "destination_url",
                    "target url",
                    "targeturl",
                    "target page",
                    "target page url",
                    "target",
                    "destination",
                ],
            ),
            (
                "anchor",
                vec!["anchor", "anchor text", "anchortext", "link text"],
            ),
            ("rel", vec!["rel", "link rel", "rel tokens"]),
            (
                "nofollow",
                vec![
                    "nofollow",
                    "no follow",
                    "is_nofollow",
                    "flagnoffollow",
                    "flagnofollow",
                ],
            ),
            (
                "sponsored",
                vec!["sponsored", "is_sponsored", "flagsponsored"],
            ),
            ("ugc", vec!["ugc", "is_ugc", "flagugc"]),
            ("follow", vec!["follow", "dofollow", "is_dofollow"]),
            (
                "first_seen",
                vec![
                    "first_seen_link",
                    "first_seen",
                    "first seen",
                    "firstseen",
                    "first indexed date",
                    "firstindexeddate",
                    "discovery_date",
                    "date",
                ],
            ),
            (
                "last_seen",
                vec![
                    "last_visited",
                    "last_seen",
                    "last seen",
                    "lastseen",
                    "last check",
                    "lastcheckdate",
                    "source_last_access_date",
                    "lastseendate",
                ],
            ),
            (
                "lost_at",
                vec![
                    "lost_at",
                    "lost",
                    "lost date",
                    "date lost",
                    "lost_date",
                    "datelost",
                ],
            ),
            (
                "state",
                vec![
                    "state",
                    "link status",
                    "status",
                    "is_lost",
                    "deleted",
                    "deletedate",
                    "flagdeleted",
                ],
            ),
            (
                "source_domain",
                vec![
                    "source_domain",
                    "source domain",
                    "referring domain",
                    "linking site",
                    "domain",
                    "sourcehost",
                    "site",
                ],
            ),
            (
                "target_domain",
                vec!["target_domain", "target domain", "targethost"],
            ),
        ])
    });

/// Provider metric names.
pub(super) static METRICS: LazyLock<IndexMap<&'static str, Vec<&'static str>>> =
    LazyLock::new(|| {
        IndexMap::from([
            (
                "provider_spam_score",
                vec!["spam_score", "backlink_spam_score", "spam score"],
            ),
            (
                "domain_rating",
                vec![
                    "domain_rating",
                    "domain_rating_source",
                    "domain rating",
                    "dr",
                ],
            ),
            (
                "domain_authority",
                vec!["domain_authority", "domain authority", "da"],
            ),
            (
                "trust_flow",
                vec!["trust flow", "trustflow", "trust_flow", "sourcetrustflow"],
            ),
            (
                "citation_flow",
                vec![
                    "citation flow",
                    "citationflow",
                    "citation_flow",
                    "sourcecitationflow",
                ],
            ),
            (
                "external_links",
                vec![
                    "external_links",
                    "external links",
                    "external links count",
                    "external_links_count",
                    "outboundlinks",
                    "outbound links",
                    "links_external",
                    "sourceoutlinksexternal",
                ],
            ),
        ])
    });
