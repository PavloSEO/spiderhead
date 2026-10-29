//! Sitemap and robots.txt audit.
//!
//! merged: (1) the vendor's native `Sitemaps:*` exports when present, and (2)
//! a direct parse of robots.txt / sitemap.xml (with sitemap-index recursion
//! and gzip) over the network. Network access is opt-in and injected — the
//! real fetch lands with the recon crate.

mod isodate;
mod parse;

pub use isodate::{civil_from_epoch, epoch_date_iso, parse_date, IsoDate};
pub use parse::{
    host, normalize_url, normalized_index, null_fetch, origin, parse_sitemap_bytes,
    resolve_sitemap_loc, safe_gunzip, Fetch, NormalizeUrlError, ParseState, SitemapParseError,
    MAX_DECOMPRESSED_BYTES, MAX_DOWNLOAD_BYTES, MAX_SITEMAP_BYTES_PER_FILE, MAX_SITEMAP_DEPTH,
    MAX_SITEMAP_URLS, MAX_SITEMAP_URLS_PER_FILE,
};

use serde_json::{Map, Value};
use std::collections::HashSet;

use crate::context::{AddOpts, AuditContext};
use crate::normalize::{find_column, normalize_value, to_float};
use crate::pyvalue;

/// `SITEMAP_DIRECTIVE` — robots.txt `Sitemap:` lines.
fn sitemap_directives(robots_text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"(?im)^\s*sitemap:\s*(\S+)").expect("static regex");
    re.captures_iter(robots_text)
        .map(|c| c[1].to_owned())
        .collect()
}

/// `disallow:` path rules in robots.txt (oracle's inline regex).
fn robots_disallows(robots_text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"(?im)^\s*disallow:\s*(\S+)").expect("static regex");
    re.captures_iter(robots_text)
        .map(|c| c[1].to_owned())
        .collect()
}

fn resource_pattern() -> regex::Regex {
    regex::Regex::new(r"\.(?:js|css)\b|/_next/|/static/|/assets/|/js/|/css/").expect("static regex")
}

/// `_base_url`: the most common scheme+host across the crawl's pages.
fn base_url(ctx: &mut AuditContext) -> Option<String> {
    let mut counts: indexmap::IndexMap<String, usize> = indexmap::IndexMap::new();
    for handle in ctx.pages_iter() {
        let url = handle.url();
        let parts: Vec<&str> = url.splitn(4, '/').collect();
        if parts.len() >= 3 {
            *counts
                .entry(format!("{}//{}", parts[0], parts[2]))
                .or_insert(0) += 1;
        }
    }
    // Counter.most_common(1): the first max in insertion order.
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(k, _)| k)
}

/// `_site_target`: a site-wide finding's URL — the crawled home page when it
/// exists, else the bare origin.
fn site_target(ctx: &mut AuditContext, base: Option<&str>) -> Option<String> {
    let base = base?;
    for candidate in [base.to_owned(), format!("{base}/")] {
        for handle in ctx.pages_iter() {
            let url = handle.url();
            if url == candidate {
                return Some(url);
            }
        }
    }
    Some(base.to_owned())
}

// --------------------------------------------------------------------------
// export-driven checks
// --------------------------------------------------------------------------

/// `_urls_from_export`: normalized non-empty values of the Address/URL column.
fn urls_from_export(ctx: &mut AuditContext, key: &str) -> Vec<Value> {
    let Some(df) = ctx.exports.get(key) else {
        return Vec::new();
    };
    if df.is_empty() {
        return Vec::new();
    }
    let Some(col) = find_column(df.columns(), &["Address", "URL"]).map(str::to_owned) else {
        return Vec::new();
    };
    let Some(values) = df.column(&col) else {
        return Vec::new();
    };
    values
        .into_iter()
        .map(normalize_value)
        .filter(pyvalue::truthy)
        .collect()
}

/// `_emit_from_export`: one finding per exported URL.
fn emit_from_export(ctx: &mut AuditContext, key: &str, check_id: &str) -> usize {
    let urls = urls_from_export(ctx, key);
    let export_file = ctx
        .exports
        .files
        .get(key)
        .cloned()
        .map(Value::String)
        .unwrap_or(Value::Null);
    for url in &urls {
        let mut details = Map::new();
        details.insert("in_sitemap".to_owned(), Value::Bool(true));
        let mut evidence = Map::new();
        evidence.insert("export".to_owned(), export_file.clone());
        let _ = ctx.add(
            check_id,
            AddOpts {
                target_url: Some(pyvalue::str_or_dumps(url)),
                details,
                evidence,
                ..AddOpts::default()
            },
        );
    }
    urls.len()
}

const NATIVE_SITEMAP_STATUS: [&str; 3] = [
    "SITEMAP_URL_4XX_5XX",
    "SITEMAP_URL_3XX",
    "SITEMAP_URL_NON_INDEXABLE",
];

/// `_native_sitemap_status`: sitemap-listed URLs judged by the crawl's own
/// fetch of each URL. Unrequested URLs are counted and named, never read as
/// clean.
fn native_sitemap_status(ctx: &mut AuditContext, entries: &[Map<String, Value>]) {
    let listed = normalized_index(
        entries
            .iter()
            .filter_map(|e| e.get("loc").and_then(Value::as_str)),
    );
    if listed.is_empty() {
        for check_id in NATIVE_SITEMAP_STATUS {
            ctx.skip(check_id, "no sitemap entries were fetched to compare");
        }
        return;
    }
    type CrawledRow = (String, String, Option<i64>, Option<String>, bool);
    let crawled: Vec<CrawledRow> = ctx
        .pages_iter()
        .iter()
        .map(|h| {
            h.map(|p| {
                (
                    normalize_url(&p.url).unwrap_or_default(),
                    p.url.clone(),
                    p.status_code,
                    p.indexability.clone(),
                    p.is_indexable(),
                )
            })
        })
        .collect();
    let crawled: std::collections::HashMap<String, (Option<i64>, Option<String>, bool)> = crawled
        .into_iter()
        .map(|(key, _url, code, indexability, is_indexable)| {
            (key, (code, indexability, is_indexable))
        })
        .collect();
    let mut unmeasured = 0i64;
    for (key, loc) in &listed {
        let page = crawled.get(key);
        let code = page.and_then(|p| p.0);
        let Some(code) = code else {
            unmeasured += 1;
            continue;
        };
        let mut details = Map::new();
        details.insert("in_sitemap".to_owned(), Value::Bool(true));
        details.insert("status_code".to_owned(), Value::from(code));
        if code >= 400 {
            let _ = ctx.add(
                "SITEMAP_URL_4XX_5XX",
                AddOpts {
                    target_url: Some(loc.clone()),
                    details,
                    ..AddOpts::default()
                },
            );
        } else if (300..400).contains(&code) {
            let _ = ctx.add(
                "SITEMAP_URL_3XX",
                AddOpts {
                    target_url: Some(loc.clone()),
                    details,
                    ..AddOpts::default()
                },
            );
        } else {
            let (indexability, is_indexable) =
                page.map(|p| (p.1.clone(), p.2)).unwrap_or((None, false));
            // `page.indexability and not page.is_indexable` — an empty
            // indexability string is falsy in reference, so it is no finding.
            if indexability.as_deref().is_some_and(|s| !s.is_empty()) && !is_indexable {
                details.insert(
                    "indexability".to_owned(),
                    indexability.map(Value::String).unwrap_or(Value::Null),
                );
                let _ = ctx.add(
                    "SITEMAP_URL_NON_INDEXABLE",
                    AddOpts {
                        target_url: Some(loc.clone()),
                        details,
                        ..AddOpts::default()
                    },
                );
            }
        }
    }
    if unmeasured > 0 {
        let reason = format!(
            "{unmeasured} sitemap URL(s) were not requested by this crawl, \
             so their status is unmeasured"
        );
        for check_id in NATIVE_SITEMAP_STATUS {
            ctx.skip(check_id, &reason);
        }
    }
}

/// `settle_native_sitemap_status`: leave no native sitemap-status check silent
/// when its sitemap was never compared.
pub fn settle_native_sitemap_status(ctx: &mut AuditContext, reason: &str) {
    for check_id in NATIVE_SITEMAP_STATUS {
        if !ctx.settled(check_id) {
            ctx.skip(check_id, reason);
        }
    }
}

/// `_check_protocol_limits`: the two hard protocol limits plus URLs declared
/// in two files. Reported against the individual document.
fn check_protocol_limits(
    ctx: &mut AuditContext,
    documents: &[Map<String, Value>],
    entries: &[Map<String, Value>],
    summary: &mut Map<String, Value>,
) {
    for doc in documents {
        let url = doc
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .map(str::to_owned)
            .or_else(|| base_url(ctx));
        let declared = doc.get("declared").and_then(Value::as_i64).unwrap_or(0);
        if declared > MAX_SITEMAP_URLS_PER_FILE {
            let mut details = Map::new();
            details.insert("declared".to_owned(), Value::from(declared));
            details.insert("limit".to_owned(), Value::from(MAX_SITEMAP_URLS_PER_FILE));
            let _ = ctx.add(
                "SITEMAP_TOO_MANY_URLS",
                AddOpts {
                    target_url: url.clone(),
                    occurrences_count: Some(declared),
                    details,
                    ..AddOpts::default()
                },
            );
        }
        let bytes = doc.get("bytes").and_then(Value::as_i64).unwrap_or(0);
        if bytes > MAX_SITEMAP_BYTES_PER_FILE {
            let mut details = Map::new();
            details.insert("bytes".to_owned(), Value::from(bytes));
            details.insert("limit".to_owned(), Value::from(MAX_SITEMAP_BYTES_PER_FILE));
            let _ = ctx.add(
                "SITEMAP_TOO_LARGE",
                AddOpts {
                    target_url: url,
                    details,
                    ..AddOpts::default()
                },
            );
        }
    }

    // Grouped on the normalised key: "/a" in one sitemap and "/a/" in another
    // is one duplicated page.
    let mut sources: indexmap::IndexMap<String, Vec<String>> = indexmap::IndexMap::new();
    let mut display: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
    for entry in entries {
        let Some(loc) = entry.get("loc").and_then(Value::as_str) else {
            continue;
        };
        let source = entry.get("source").and_then(Value::as_str).unwrap_or("");
        let key = normalize_url(loc).unwrap_or_else(|_| loc.to_owned());
        display.entry(key.clone()).or_insert_with(|| loc.to_owned());
        let seen = sources.entry(key).or_default();
        if !source.is_empty() && !seen.iter().any(|s| s == source) {
            seen.push(source.to_owned());
        }
    }
    let mut duplicated: Vec<(String, Vec<String>)> = sources
        .into_iter()
        .filter(|(_, srcs)| srcs.len() > 1)
        .collect();
    duplicated.sort_by(|a, b| a.0.cmp(&b.0));
    if !duplicated.is_empty() {
        summary.insert(
            "urls_in_multiple_sitemaps".to_owned(),
            Value::from(duplicated.len() as i64),
        );
    }
    for (key, srcs) in &duplicated {
        let mut details = Map::new();
        details.insert(
            "sitemaps".to_owned(),
            Value::Array(srcs.iter().cloned().map(Value::String).collect()),
        );
        let _ = ctx.add(
            "SITEMAP_URL_DUPLICATED",
            AddOpts {
                target_url: display.get(key).cloned(),
                details,
                ..AddOpts::default()
            },
        );
    }
}

/// `run_sitemap` options — the oracle's keyword parameters.
#[derive(Debug, Default)]
pub struct SitemapOptions {
    /// `--sitemap` single-target mode.
    pub sitemap_url: Option<String>,
    /// Compare the sitemap against the crawl (set false when the caller
    /// reconciles on its own).
    pub compare_with_crawl: bool,
    /// Every discovered sitemap root to audit (wins over `sitemap_url`).
    pub sitemap_urls: Vec<String>,
    /// The crawl did not reach the whole frontier — withhold thresholded
    /// site-wide verdicts.
    pub crawl_partial: bool,
    /// Injected "now" for lastmod analysis (epoch seconds). `None` = the
    /// system clock.
    pub now: Option<f64>,
}

impl SitemapOptions {
    /// Default comparison-enabled options.
    pub fn new() -> Self {
        Self {
            compare_with_crawl: true,
            ..Self::default()
        }
    }
}

fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn str_list(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// `run_sitemap`: the merged sitemap/robots audit; returns the summary block.
pub fn run_sitemap(
    ctx: &mut AuditContext,
    options: &SitemapOptions,
    fetch: &Fetch<'_>,
) -> Map<String, Value> {
    let mut summary = Map::new();
    let cfg_live = ctx
        .config
        .get("live_recheck")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_default();
    // `ua`/`timeout` belong to the real fetch the recon crate will supply;
    // the injected closure carries its own.

    // --- 1. native Sitemaps:* exports --------------------------------------
    let sf_in = urls_from_export(ctx, "sitemap_in");
    emit_from_export(ctx, "sitemap_non_200", "SITEMAP_URL_4XX_5XX");
    emit_from_export(ctx, "sitemap_redirects", "SITEMAP_URL_3XX");
    emit_from_export(ctx, "sitemap_non_indexable", "SITEMAP_URL_NON_INDEXABLE");
    // Kept as its own list, not just a count: the summary needs the declared
    // orphan URLs themselves.
    let sf_orphans = urls_from_export(ctx, "sitemap_orphan");
    let orphan_file = ctx
        .exports
        .files
        .get("sitemap_orphan")
        .cloned()
        .map(Value::String)
        .unwrap_or(Value::Null);
    for orphan_url in &sf_orphans {
        let mut details = Map::new();
        details.insert("in_sitemap".to_owned(), Value::Bool(true));
        let mut evidence = Map::new();
        evidence.insert("export".to_owned(), orphan_file.clone());
        let _ = ctx.add(
            "SITEMAP_ORPHAN",
            AddOpts {
                target_url: Some(pyvalue::str_or_dumps(orphan_url)),
                details,
                evidence,
                ..AddOpts::default()
            },
        );
    }
    for url in &urls_from_export(ctx, "sitemap_not_in") {
        let _ = ctx.add(
            "URL_NOT_IN_SITEMAP",
            AddOpts {
                target_url: Some(pyvalue::str_or_dumps(url)),
                ..AddOpts::default()
            },
        );
    }

    // --- 2. direct robots.txt + sitemap parse (opt-in) ----------------------
    let mut sitemap_entries: Vec<Map<String, Value>> = Vec::new();
    let mut declared_in_robots: Option<bool> = None;
    let mut sitemaps_declared: Vec<String> = Vec::new();
    let want_network = options.sitemap_url.is_some()
        || !options.sitemap_urls.is_empty()
        || cfg_live.get("enabled").is_some_and(pyvalue::truthy);
    let mut base = base_url(ctx);
    if base.is_none() {
        // No crawled pages to derive an origin from — but an explicit target
        // names its own host.
        let explicit_first = if !options.sitemap_urls.is_empty() {
            options.sitemap_urls.first()
        } else {
            options.sitemap_url.as_ref()
        };
        if let Some(first) = explicit_first {
            base = origin(first);
        }
    }
    let mut fetch_failures: Vec<String> = Vec::new();
    let mut depth_truncated: Vec<String> = Vec::new();
    // Whether a sitemap was actually pursued over the network, as opposed to
    // merely requested-but-unable (no base host).
    let network_attempted = want_network && base.is_some();

    if network_attempted {
        let base_s = base.clone().unwrap_or_default();
        if let Some(robots) = fetch(&format!("{base_s}/robots.txt")) {
            let robots_text = String::from_utf8_lossy(&robots).into_owned();
            sitemaps_declared = sitemap_directives(&robots_text);
            declared_in_robots = Some(!sitemaps_declared.is_empty());
            if sitemaps_declared.is_empty() {
                let target = site_target(ctx, Some(&base_s));
                let _ = ctx.add(
                    "SITEMAP_NOT_IN_ROBOTS",
                    AddOpts {
                        target_url: target,
                        ..AddOpts::default()
                    },
                );
            }
            // robots blocking render-critical resources breaks rendering.
            let disallows = robots_disallows(&robots_text);
            let res_re = resource_pattern();
            let blocked_res: Vec<String> = disallows
                .into_iter()
                .filter(|d| res_re.is_match(d))
                .collect();
            if !blocked_res.is_empty() {
                let mut details = Map::new();
                details.insert(
                    "rules".to_owned(),
                    Value::Array(
                        blocked_res
                            .iter()
                            .take(10)
                            .cloned()
                            .map(Value::String)
                            .collect(),
                    ),
                );
                let target = site_target(ctx, Some(&base_s));
                let _ = ctx.add(
                    "ROBOTS_BLOCKS_RESOURCES",
                    AddOpts {
                        target_url: target,
                        details,
                        ..AddOpts::default()
                    },
                );
            }
        }
        let targets: Vec<String> = if !options.sitemap_urls.is_empty() {
            // Every discovered root, de-duplicated but order-preserved.
            let mut seen_targets = HashSet::new();
            options
                .sitemap_urls
                .iter()
                .filter(|t| !t.is_empty() && seen_targets.insert((*t).clone()))
                .cloned()
                .collect()
        } else if let Some(u) = &options.sitemap_url {
            vec![u.clone()]
        } else if !sitemaps_declared.is_empty() {
            sitemaps_declared.clone()
        } else {
            vec![format!("{base_s}/sitemap.xml")]
        };
        // SSRF allow-list: the base host plus the hosts of given sitemaps.
        let mut allowed_hosts: HashSet<String> = targets.iter().map(|t| host(t)).collect();
        allowed_hosts.insert(host(&base_s));
        allowed_hosts.remove("");
        let mut state = ParseState {
            seen: targets.iter().cloned().collect(),
            allowed_hosts,
            track_failures: true,
            track_documents: true,
            track_truncated: true,
            ..ParseState::default()
        };
        for sm_url in &targets {
            match fetch(sm_url) {
                Some(data) => {
                    sitemap_entries.extend(parse_sitemap_bytes(&data, &mut state, sm_url, 0, fetch))
                }
                // Root and child failures share one list, in call order.
                None => state.failures.push(sm_url.clone()),
            }
        }
        fetch_failures = state.failures.clone();
        depth_truncated = state.truncated.clone();
        let sitemap_documents = state.documents.clone();
        if !fetch_failures.is_empty() || !depth_truncated.is_empty() {
            if !fetch_failures.is_empty() {
                summary.insert(
                    "sitemap_fetch_failures".to_owned(),
                    Value::Array(
                        fetch_failures
                            .iter()
                            .take(20)
                            .cloned()
                            .map(Value::String)
                            .collect(),
                    ),
                );
            }
            if !depth_truncated.is_empty() {
                summary.insert(
                    "sitemap_depth_truncated".to_owned(),
                    Value::Array(
                        depth_truncated
                            .iter()
                            .take(20)
                            .cloned()
                            .map(Value::String)
                            .collect(),
                    ),
                );
            }
            let mut details = Map::new();
            details.insert(
                "failed_count".to_owned(),
                Value::from(fetch_failures.len() as i64),
            );
            details.insert(
                "examples".to_owned(),
                Value::Array(
                    fetch_failures
                        .iter()
                        .take(10)
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
            details.insert(
                "depth_truncated_count".to_owned(),
                Value::from(depth_truncated.len() as i64),
            );
            details.insert(
                "depth_truncated_examples".to_owned(),
                Value::Array(
                    depth_truncated
                        .iter()
                        .take(10)
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
            details.insert("max_depth".to_owned(), Value::from(state.max_depth as i64));
            let _ = ctx.add(
                "SITEMAP_FETCH_INCOMPLETE",
                AddOpts {
                    target_url: Some(base_s.clone()),
                    details,
                    ..AddOpts::default()
                },
            );
        }
        check_protocol_limits(ctx, &sitemap_documents, &sitemap_entries, &mut summary);
        if sitemap_documents.is_empty() {
            let reason = "no sitemap document was fetched to measure";
            ctx.skip("SITEMAP_TOO_MANY_URLS", reason);
            ctx.skip("SITEMAP_TOO_LARGE", reason);
        }
        if sitemap_entries.is_empty() {
            let reason = "no sitemap entries were fetched to compare";
            ctx.skip("SITEMAP_URL_DUPLICATED", reason);
            ctx.skip("SITEMAP_STALE_LASTMOD", reason);
        }
        if ctx.scan_con.is_some() {
            native_sitemap_status(ctx, &sitemap_entries);
        }
        run_sitemap_tail(
            ctx,
            options,
            &mut summary,
            sitemap_entries,
            declared_in_robots,
            sitemaps_declared,
            sf_in,
            sf_orphans,
            network_attempted,
            fetch_failures,
            depth_truncated,
            base,
        )
    } else {
        // Nothing to fetch: name the reason for every check that needed it.
        let reason = if !want_network {
            "no sitemap URL to check (no export, no --sitemap, and live_recheck disabled)"
        } else {
            "could not determine the crawled site's base URL"
        };
        ctx.skip("SITEMAP_NOT_IN_ROBOTS", reason);
        ctx.skip("ROBOTS_BLOCKS_RESOURCES", reason);
        ctx.skip("SITEMAP_FETCH_INCOMPLETE", reason);
        check_protocol_limits(ctx, &[], &sitemap_entries, &mut summary);
        {
            let reason = "no sitemap document was fetched to measure";
            ctx.skip("SITEMAP_TOO_MANY_URLS", reason);
            ctx.skip("SITEMAP_TOO_LARGE", reason);
        }
        {
            let reason = "no sitemap entries were fetched to compare";
            ctx.skip("SITEMAP_URL_DUPLICATED", reason);
            ctx.skip("SITEMAP_STALE_LASTMOD", reason);
        }
        if ctx.scan_con.is_some() {
            native_sitemap_status(ctx, &sitemap_entries);
        }
        run_sitemap_tail(
            ctx,
            options,
            &mut summary,
            sitemap_entries,
            declared_in_robots,
            sitemaps_declared,
            sf_in,
            sf_orphans,
            network_attempted,
            fetch_failures,
            depth_truncated,
            base,
        )
    }
}

/// The desync/lastmod/summary half of `run_sitemap`, shared between the
/// network-attempted and skipped paths (mirroring the oracle's single flow).
#[allow(clippy::too_many_arguments)]
fn run_sitemap_tail(
    ctx: &mut AuditContext,
    options: &SitemapOptions,
    summary: &mut Map<String, Value>,
    sitemap_entries: Vec<Map<String, Value>>,
    declared_in_robots: Option<bool>,
    sitemaps_declared: Vec<String>,
    sf_in: Vec<Value>,
    sf_orphans: Vec<Value>,
    network_attempted: bool,
    fetch_failures: Vec<String>,
    depth_truncated: Vec<String>,
    base: Option<String>,
) -> Map<String, Value> {
    // Prefer the richer source for the URL set / lastmod analysis.
    let sitemap_locs: Vec<String> = if sitemap_entries.is_empty() {
        str_list(&sf_in)
    } else {
        sitemap_entries
            .iter()
            .filter_map(|e| e.get("loc").and_then(Value::as_str).map(str::to_owned))
            .collect()
    };
    // True only when the URL set came from the export (`sitemap_in`) rather
    // than a live fetch: Internal:All presence is not proof of an internal
    // link for the export path.
    let using_export_sitemap = !sf_in.is_empty() && sitemap_entries.is_empty();

    // --- 3. lastmod staleness ----------------------------------------------
    let now = options.now.unwrap_or_else(now_seconds);
    let lastmod_summary = analyze_lastmod(ctx, &sitemap_entries, base.as_deref(), now);
    if let Some(ls) = lastmod_summary {
        summary.insert("lastmod".to_owned(), Value::Object(ls));
    }

    // --- 4. desync (both directions) ----------------------------------------
    let sitemap_index = normalized_index(sitemap_locs.iter().map(String::as_str));
    let page_urls: Vec<String> = ctx.pages_iter().iter().map(|h| h.url()).collect();
    let pages_index = normalized_index(page_urls.iter().map(String::as_str));
    let indexable_urls: Vec<String> = ctx.indexable_html_pages().iter().map(|h| h.url()).collect();
    let indexable_index = normalized_index(indexable_urls.iter().map(String::as_str));
    let sitemap_keys: HashSet<&String> = sitemap_index.keys().collect();
    let pages_keys: HashSet<&String> = pages_index.keys().collect();
    // Declared, named from the sitemap's own text.
    let mut in_sitemap_not_crawl: Vec<String> = sitemap_keys
        .difference(&pages_keys)
        .map(|k| sitemap_index[*k].clone())
        .collect();
    in_sitemap_not_crawl.sort();
    // Named from the crawl's own text.
    let indexable_keys: HashSet<&String> = indexable_index.keys().collect();
    let mut in_crawl_not_sitemap: Vec<String> = if sitemap_keys.is_empty() {
        Vec::new()
    } else {
        indexable_keys
            .difference(&sitemap_keys)
            .map(|k| indexable_index[*k].clone())
            .collect()
    };
    in_crawl_not_sitemap.sort();

    // Mark pages with sitemap membership, same normalised key as the diff.
    for handle in ctx.pages_iter() {
        let mut handle = handle;
        if !sitemap_keys.is_empty() {
            let url = handle.url();
            let member = normalize_url(&url)
                .map(|k| sitemap_keys.contains(&k))
                .unwrap_or(false);
            handle.mutate(|p| {
                p.metrics
                    .insert("is_in_sitemap".to_owned(), Value::Bool(member));
            });
        }
    }

    if !sitemap_keys.is_empty() && !options.compare_with_crawl {
        // The caller reconciles the sitemap against its own crawl and owns
        // these findings.
        ctx.skip(
            "SITEMAP_DESYNC",
            "the caller reconciles the sitemap against its own crawl",
        );
    } else if !sitemap_keys.is_empty() && options.crawl_partial {
        ctx.skip(
            "SITEMAP_DESYNC",
            "crawl is partial: a sitemap-versus-link-graph verdict cannot be proven \
             when the crawl did not reach every URL",
        );
    } else if !sitemap_keys.is_empty() {
        let threshold = ctx
            .thresholds
            .get("sitemap_desync_pct_warn")
            .and_then(to_float)
            .unwrap_or(0.0);
        let crawl_only_pct = round1(
            100.0 * in_crawl_not_sitemap.len() as f64 / (indexable_index.len().max(1)) as f64,
        );
        let sitemap_only_pct =
            round1(100.0 * in_sitemap_not_crawl.len() as f64 / (sitemap_keys.len().max(1)) as f64);
        if crawl_only_pct >= threshold || sitemap_only_pct >= threshold {
            let mut details = Map::new();
            details.insert(
                "in_crawl_not_in_sitemap".to_owned(),
                Value::from(in_crawl_not_sitemap.len() as i64),
            );
            details.insert(
                "in_sitemap_not_in_crawl".to_owned(),
                Value::from(in_sitemap_not_crawl.len() as i64),
            );
            details.insert(
                "crawl_not_in_sitemap_pct".to_owned(),
                Value::from(crawl_only_pct),
            );
            details.insert(
                "sitemap_not_in_crawl_pct".to_owned(),
                Value::from(sitemap_only_pct),
            );
            details.insert(
                "examples_missing_from_sitemap".to_owned(),
                Value::Array(
                    in_crawl_not_sitemap
                        .iter()
                        .take(20)
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
            details.insert(
                "examples_in_sitemap_not_crawled".to_owned(),
                Value::Array(
                    in_sitemap_not_crawl
                        .iter()
                        .take(20)
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
            let target = site_target(ctx, base.as_deref());
            let _ = ctx.add(
                "SITEMAP_DESYNC",
                AddOpts {
                    target_url: target,
                    details,
                    ..AddOpts::default()
                },
            );
        }
    } else if !network_attempted {
        ctx.skip(
            "SITEMAP_DESYNC",
            "no sitemap URL set (no export and network disabled)",
        );
    } else if !depth_truncated.is_empty() {
        // Distinct from a fetch/parse failure: every attempted document came
        // back and parsed, but a nested index hit the depth cap.
        ctx.skip(
            "SITEMAP_DESYNC",
            &format!(
                "sitemap traversal hit the depth cap ({} levels) before {} descendant \
                 document(s) were followed: {}",
                parse::MAX_SITEMAP_DEPTH,
                depth_truncated.len(),
                py_list(&depth_truncated[..depth_truncated.len().min(5)])
            ),
        );
    } else if !fetch_failures.is_empty() {
        ctx.skip(
            "SITEMAP_DESYNC",
            &format!(
                "sitemap fetch/parse failed for all {} attempted document(s): {}",
                fetch_failures.len(),
                py_list(&fetch_failures[..fetch_failures.len().min(5)])
            ),
        );
    } else {
        ctx.skip("SITEMAP_DESYNC", "sitemap fetched but declared zero URLs");
    }

    summary.insert(
        "declared_in_robots".to_owned(),
        declared_in_robots.map(Value::Bool).unwrap_or(Value::Null),
    );
    let sitemaps_summary: Vec<String> = if !sitemaps_declared.is_empty() {
        sitemaps_declared
    } else if !options.sitemap_urls.is_empty() {
        options.sitemap_urls.clone()
    } else {
        options.sitemap_url.clone().into_iter().collect()
    };
    summary.insert(
        "sitemaps".to_owned(),
        Value::Array(sitemaps_summary.into_iter().map(Value::String).collect()),
    );
    summary.insert(
        "urls_in_sitemap".to_owned(),
        Value::from(sitemap_keys.len() as i64),
    );
    summary.insert(
        "urls_in_crawl_indexable".to_owned(),
        Value::from(indexable_index.len() as i64),
    );
    summary.insert(
        "in_sitemap_not_in_crawl".to_owned(),
        Value::from(in_sitemap_not_crawl.len() as i64),
    );
    summary.insert(
        "in_crawl_not_in_sitemap".to_owned(),
        Value::from(in_crawl_not_sitemap.len() as i64),
    );
    summary.insert(
        "non_200_in_sitemap".to_owned(),
        Value::from(urls_from_export(ctx, "sitemap_non_200").len() as i64),
    );
    summary.insert(
        "non_indexable_in_sitemap".to_owned(),
        Value::from(urls_from_export(ctx, "sitemap_non_indexable").len() as i64),
    );
    // Extension coverage: counted separately from the page entries and
    // surfaced only when the sitemap declared any.
    for (name, total) in [
        (
            "image_entries_in_sitemap",
            entry_sum(&sitemap_entries, "image_entries"),
        ),
        (
            "news_entries_in_sitemap",
            entry_sum(&sitemap_entries, "news_entries"),
        ),
        (
            "video_entries_in_sitemap",
            entry_sum(&sitemap_entries, "video_entries"),
        ),
    ] {
        if total > 0 {
            summary.insert(name.to_owned(), Value::from(total));
        }
    }
    if !sitemap_keys.is_empty() {
        if using_export_sitemap && ctx.exports.has("sitemap_orphan") {
            // The dedicated "no internal link reaches this" evidence is the
            // Orphan URLs export, not membership in Internal:All.
            let orphan_strs = str_list(&sf_orphans);
            let orphan_index = normalized_index(orphan_strs.iter().map(String::as_str));
            let orphan_keys: HashSet<&String> = orphan_index.keys().collect();
            let mut not_linked_keys: HashSet<String> = sitemap_keys
                .iter()
                .filter(|k| orphan_keys.contains(*k))
                .map(|k| (*k).clone())
                .collect();
            for k in sitemap_keys.difference(&pages_keys) {
                not_linked_keys.insert((*k).clone());
            }
            let mut not_linked: Vec<String> = not_linked_keys
                .iter()
                .map(|k| {
                    orphan_index
                        .get(k)
                        .or_else(|| sitemap_index.get(k))
                        .cloned()
                        .unwrap_or_else(|| k.clone())
                })
                .collect();
            not_linked.sort();
            summary.insert(
                "in_sitemap_not_linked".to_owned(),
                Value::Array(not_linked.into_iter().map(Value::String).collect()),
            );
            let mut linked: Vec<String> = sitemap_keys
                .intersection(&pages_keys)
                .filter(|k| !not_linked_keys.contains((*k).as_str()))
                .map(|k| pages_index[*k].clone())
                .collect();
            linked.sort();
            summary.insert(
                "in_sitemap_and_linked".to_owned(),
                Value::Array(linked.into_iter().map(Value::String).collect()),
            );
        } else if using_export_sitemap {
            // No Orphan URLs export: Internal:All membership alone is not proof
            // an internal link reaches the page.
            summary.insert(
                "in_sitemap_linked_unavailable".to_owned(),
                Value::String(
                    "no Sitemaps: Orphan URLs export; Internal: All presence is not proof \
                     of an internal link"
                        .to_owned(),
                ),
            );
        } else {
            let mut linked: Vec<String> = sitemap_keys
                .intersection(&pages_keys)
                .map(|k| pages_index[*k].clone())
                .collect();
            linked.sort();
            summary.insert(
                "in_sitemap_and_linked".to_owned(),
                Value::Array(linked.into_iter().map(Value::String).collect()),
            );
            summary.insert(
                "in_sitemap_not_linked".to_owned(),
                Value::Array(
                    in_sitemap_not_crawl
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        summary.insert(
            "linked_not_in_sitemap".to_owned(),
            Value::Array(
                in_crawl_not_sitemap
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    std::mem::take(summary)
}

fn entry_sum(entries: &[Map<String, Value>], key: &str) -> i64 {
    entries
        .iter()
        .map(|e| e.get(key).and_then(Value::as_i64).unwrap_or(0))
        .sum()
}

/// reference `round(x, 1)` — banker's rounding on the decimal representation.
fn round1(x: f64) -> f64 {
    (x * 10.0).round_ties_even() / 10.0
}

/// reference `repr(list[str])` for skip-reason text: `['a', 'b']`.
fn py_list(items: &[String]) -> String {
    let inner = items
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

/// `_analyze_lastmod`: staleness statistics over the parsed entries.
fn analyze_lastmod(
    ctx: &mut AuditContext,
    entries: &[Map<String, Value>],
    base: Option<&str>,
    now: f64,
) -> Option<Map<String, Value>> {
    let mut dates: Vec<IsoDate> = Vec::new();
    let mut invalid = 0i64;
    let mut future = 0i64;
    for e in entries {
        let Some(lastmod) = e.get("lastmod").and_then(Value::as_str) else {
            continue;
        };
        let Some(dt) = parse_date(Some(lastmod)) else {
            invalid += 1;
            continue;
        };
        if dt.epoch > now {
            future += 1;
            continue; // generation errors — keep them out of stats
        }
        dates.push(dt);
    }
    if dates.is_empty() {
        return None;
    }
    dates.sort_by(|a, b| {
        a.epoch
            .partial_cmp(&b.epoch)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let stale_days_value = ctx
        .thresholds
        .get("sitemap_lastmod_stale_days")
        .cloned()
        .unwrap_or(Value::Null);
    let stale_days = to_float(&stale_days_value).unwrap_or(0.0);
    let cutoff_secs = stale_days * 86400.0;
    let cutoff_share =
        dates.iter().filter(|d| now - d.epoch > cutoff_secs).count() as f64 / dates.len() as f64;
    let all_same = dates
        .iter()
        .map(|d| (d.year, d.month, d.day))
        .collect::<HashSet<_>>()
        .len()
        == 1;
    // statistics.median of the epoch timestamps.
    let mut stamps: Vec<f64> = dates.iter().map(|d| d.epoch).collect();
    stamps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = stamps.len();
    let median_ts = if n % 2 == 1 {
        stamps[n / 2]
    } else {
        (stamps[n / 2 - 1] + stamps[n / 2]) / 2.0
    };
    let mut summary = Map::new();
    summary.insert("oldest".to_owned(), Value::String(dates[0].date_iso()));
    summary.insert(
        "median".to_owned(),
        Value::String(epoch_date_iso(median_ts)),
    );
    summary.insert("newest".to_owned(), Value::String(dates[n - 1].date_iso()));
    summary.insert(
        "share_older_than_threshold".to_owned(),
        Value::from(round2(cutoff_share)),
    );
    summary.insert("threshold_days".to_owned(), stale_days_value);
    summary.insert("all_identical".to_owned(), Value::Bool(all_same));
    summary.insert("invalid_count".to_owned(), Value::from(invalid));
    summary.insert("future_count".to_owned(), Value::from(future));
    if cutoff_share >= 0.5 || all_same || future > 0 || invalid > 0 {
        let target = site_target(ctx, base);
        let _ = ctx.add(
            "SITEMAP_STALE_LASTMOD",
            AddOpts {
                target_url: target,
                details: summary.clone(),
                ..AddOpts::default()
            },
        );
    }
    Some(summary)
}

/// reference `round(x, 2)` — banker's rounding at two decimals.
fn round2(x: f64) -> f64 {
    (x * 100.0).round_ties_even() / 100.0
}
