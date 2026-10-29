//! Split out of `providers`.

use super::*;

/// `_REGISTRY` in declaration order — `sources_doctor` iterates it.
pub const REGISTRY: &[(&str, RegistryEntry)] = &[
    (
        "arsenkin",
        entry(
            &["api_token"],
            "read_only_paid",
            &["keyword_frequency", "serp_clustering"],
            "paid limit credits",
            "aggregate",
        ),
    ),
    (
        "yandex_cloud",
        entry(
            &["api_key", "folder_id"],
            "read_only_paid",
            &["wordstat", "web_search"],
            "provider quota and recorded spend",
            "aggregate",
        ),
    ),
    (
        "gsc",
        entry(
            &["oauth_bearer", "service_account"],
            "read_only",
            &[
                "verify",
                "properties",
                "search_analytics",
                "inspection",
                "sitemaps",
            ],
            "Google Search Console row and request limits",
            "restricted",
        ),
    ),
    (
        "crux",
        entry(
            &["api_key"],
            "read_only",
            &["current", "history"],
            "Google Cloud API quota",
            "aggregate",
        ),
    ),
    (
        "pagespeed",
        entry(
            &["api_key"],
            "read_only",
            &["mobile_samples", "desktop_samples"],
            "Google API quota",
            "aggregate",
        ),
    ),
    (
        "ga4",
        entry(
            &["oauth_bearer", "service_account"],
            "read_only",
            &["landing_pages", "page_views"],
            "GA4 Data API quota",
            "restricted",
        ),
    ),
    (
        "metrika",
        entry(
            &["oauth_bearer"],
            "read_only",
            &["counters", "aggregate_report"],
            "Yandex Metrika API quota",
            "restricted",
        ),
    ),
    (
        "yandex_webmaster",
        entry(
            &["oauth_bearer"],
            "read_only",
            WEBMASTER_OPS,
            "Yandex Webmaster application quota",
            "restricted",
        ),
    ),
    (
        "miratext",
        entry(
            &["api_key"],
            "read_only_optional_paid",
            &["seo_analysis"],
            "free queue or explicitly confirmed paid provider task",
            "restricted",
        ),
    ),
    (
        "bing_webmaster",
        entry(
            &["api_key"],
            "read_only",
            &[
                "sites",
                "crawl",
                "links",
                "url_links",
                "keywords",
                "search_performance",
            ],
            "Bing Webmaster API quota",
            "restricted",
        ),
    ),
    (
        "dataforseo_backlinks",
        entry(
            &["login", "password"],
            "read_only_optional_paid",
            &[
                "backlinks_summary",
                "backlinks",
                "referring_domains",
                "backlinks_history",
                "domain_pages_summary",
            ],
            "paid per provider response",
            "restricted",
        ),
    ),
    (
        "moz",
        entry(
            &["api_token"],
            "read_only_paid",
            &["backlinks_summary"],
            "Moz API plan limits",
            "restricted",
        ),
    ),
    (
        "cloudflare",
        entry(
            &["api_token"],
            "read_only",
            &["token_verify", "traffic", "logpull", "history"],
            "Cloudflare account and zone plan limits",
            "restricted",
        ),
    ),
    (
        "indexnow",
        entry(
            &["submission_key"],
            "confirmed_write",
            &["submit"],
            "provider submission quota",
            "public_url_list",
        ),
    ),
    (
        "wayback",
        entry(
            &[],
            "read_only",
            &["history"],
            "public service pacing",
            "public",
        ),
    ),
    (
        "crtsh",
        entry(
            &[],
            "read_only",
            &["subdomains"],
            "public service availability",
            "public",
        ),
    ),
];

pub(super) const DEFAULT_DISABLED: &[&str] = &["dataforseo_backlinks", "moz", "indexnow"];
pub(super) const EXCLUDED_METRIKA: &[&str] = &["raw_logs"];
