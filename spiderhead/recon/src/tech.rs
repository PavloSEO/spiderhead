//! Detect website technologies from HTML, response headers, and cookies.
//!
//! `detect_tech` fetches a page through the
//! guarded client and hands it to [`analyze_tech`], the pure half a crawl can
//! call once per already-downloaded page. [`tag_coverage`] aggregates those
//! per-page results into a site-wide report.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Map, Value as Json};
use spiderhead_parse::dom::Soup;

use crate::net_client::{http_client, normalize_url, Cookie, ReconClient};
use crate::tech_db;

mod detect;
pub use detect::*;
mod findings;
pub use findings::*;

#[derive(serde::Deserialize)]
struct TechTables {
    signatures: Vec<(String, String, String, String)>,
    identifier_patterns: BTreeMap<String, String>,
    ssr_frameworks: Vec<String>,
    default_coverage_tags: Vec<String>,
    injected_by: BTreeMap<String, Vec<String>>,
    max_html_bytes: usize,
}

static TABLES: LazyLock<TechTables> = LazyLock::new(|| {
    serde_json::from_str(include_str!("data/tech_tables.json"))
        .unwrap_or_else(|e| panic!("embedded tech tables are invalid: {e}"))
});

/// `(category, name, kind, marker)` fingerprint registry.
/// `kind` is one of `header`/`value`/`cookie`/`html`/`script`.
pub static SIGNATURES: LazyLock<Vec<(String, String, String, String)>> =
    LazyLock::new(|| TABLES.signatures.clone());

/// Frameworks that render the page themselves; co-detection with a CMS
/// suggests headless.
pub static SSR_FRAMEWORKS: LazyLock<BTreeSet<String>> =
    LazyLock::new(|| TABLES.ssr_frameworks.iter().cloned().collect());

/// Technology name → identifier capture regex.
pub static IDENTIFIER_PATTERNS: LazyLock<BTreeMap<String, Regex>> = LazyLock::new(|| {
    TABLES
        .identifier_patterns
        .iter()
        .filter_map(|(k, v)| Regex::new(v).ok().map(|r| (k.clone(), r)))
        .collect()
});

/// Tags an audit is most often asked to prove on every page
pub static DEFAULT_COVERAGE_TAGS: LazyLock<Vec<String>> =
    LazyLock::new(|| TABLES.default_coverage_tags.clone());

/// Tag → managers expected to inject it client-side.
pub static INJECTED_BY: LazyLock<BTreeMap<String, Vec<String>>> =
    LazyLock::new(|| TABLES.injected_by.clone());
