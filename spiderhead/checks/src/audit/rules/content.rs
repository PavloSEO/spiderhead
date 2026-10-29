//! Content-quality and URL-shape checks (reference the reference suite).

use std::collections::BTreeMap;

use rusqlite::OptionalExtension;
use serde_json::Value;

use super::links::PLACEMENT_EVIDENCE_ITEMS;
use super::{details, num};
use crate::audit::AuditContext;

/// `check_content`: thin content and low text ratio (near-duplicate evidence is not scanned yet).
pub fn check_content(ctx: &mut AuditContext) {
    let t = ctx.thresholds.clone();
    let frames_known = ctx.has_column("content_frames_same_origin");
    if !frames_known {
        ctx.skip("CONTENT_IN_IFRAME", "no iframe inventory in this evidence");
    }
    let mut has_text_ratio = false;
    let mut low_ratio: Vec<(String, f64)> = Vec::new();
    for i in ctx.indexable_html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let wc = page.num("word_count");
        if let Some(wc) = wc.filter(|w| *w < t.thin_content_words) {
            let framed = if frames_known {
                page.num("content_frames_same_origin").unwrap_or(0.0)
            } else {
                0.0
            };
            if framed != 0.0 {
                ctx.add(
                    "CONTENT_IN_IFRAME",
                    Some(&page.url),
                    details([
                        ("word_count", num(wc)),
                        ("threshold", num(t.thin_content_words)),
                        ("same_origin_frames_in_content_area", num(framed)),
                        (
                            "frames_in_content_area",
                            page.rec
                                .get("content_frames")
                                .cloned()
                                .unwrap_or(Value::Null),
                        ),
                    ]),
                );
            } else {
                ctx.add(
                    "THIN_CONTENT",
                    Some(&page.url),
                    details([
                        ("word_count", num(wc)),
                        ("threshold", num(t.thin_content_words)),
                    ]),
                );
            }
        }
        if let Some(ratio) = page.num("text_ratio") {
            has_text_ratio = true;
            if ratio < t.low_text_ratio_pct && !wc.is_some_and(|w| w >= t.thin_content_words) {
                low_ratio.push((page.url.clone(), ratio));
            }
        }
    }
    if !low_ratio.is_empty() {
        let ratios: Vec<f64> = low_ratio.iter().map(|(_, r)| *r).collect();
        let threshold = num(t.low_text_ratio_pct);
        let d = if low_ratio.len() == 1 {
            details([
                ("text_ratio", Value::from(ratios[0])),
                ("threshold", threshold),
            ])
        } else {
            let min = ratios.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = ratios.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            details([
                ("text_ratio_min", Value::from(min)),
                ("text_ratio_max", Value::from(max)),
                ("threshold", threshold),
            ])
        };
        let urls: Vec<String> = low_ratio.into_iter().map(|(u, _)| u).collect();
        ctx.add_site_wide("LOW_TEXT_RATIO", &urls, d);
    }
    if !has_text_ratio {
        ctx.skip("LOW_TEXT_RATIO", "no Text Ratio column in Internal:All");
    }
}

/// `check_content_quality`: readability, sentence length and the SF-only
/// spelling/grammar counters.
pub fn check_content_quality(ctx: &mut AuditContext) {
    let t = ctx.thresholds.clone();
    let awps_max = ctx
        .thresholds
        .get("avg_words_per_sentence_max")
        .unwrap_or(25.0);
    let (mut has_read, mut has_awps, mut has_spell, mut has_grammar) = (false, false, false, false);
    for i in ctx.indexable_html_pages() {
        let page = ctx.pages[i].clone();
        let flesch = page.num("flesch");
        let readability = page.rec.get("readability").and_then(Value::as_str);
        if flesch.is_some() || readability.is_some() {
            has_read = true;
            let difficult = flesch.is_some_and(|f| f < t.readability_flesch_min)
                || readability.is_some_and(|r| r.to_lowercase().contains("difficult"));
            if difficult {
                ctx.add(
                    "READABILITY_DIFFICULT",
                    Some(&page.url),
                    details([
                        (
                            "flesch",
                            page.rec.get("flesch").cloned().unwrap_or(Value::Null),
                        ),
                        ("readability", readability.map_or(Value::Null, Value::from)),
                        ("min", num(t.readability_flesch_min)),
                    ]),
                );
            }
        }
        if let Some(awps) = page.num("avg_words_per_sentence") {
            has_awps = true;
            if awps > awps_max {
                ctx.add(
                    "LONG_SENTENCES",
                    Some(&page.url),
                    details([
                        ("avg_words_per_sentence", num(awps)),
                        ("max", num(awps_max)),
                    ]),
                );
            }
        }
        if let Some(sp) = page.num("spelling_errors") {
            has_spell = true;
            if sp > 0.0 {
                ctx.add(
                    "SPELLING_ERRORS",
                    Some(&page.url),
                    details([("count", num(sp))]),
                );
            }
        }
        if let Some(gr) = page.num("grammar_errors") {
            has_grammar = true;
            if gr > 0.0 {
                ctx.add(
                    "GRAMMAR_ERRORS",
                    Some(&page.url),
                    details([("count", num(gr))]),
                );
            }
        }
    }
    if !has_read {
        ctx.skip("READABILITY_DIFFICULT", "no Readability/Flesch column");
    }
    if !has_awps {
        ctx.skip("LONG_SENTENCES", "no Average Words Per Sentence column");
    }
    if !has_spell {
        ctx.skip(
            "SPELLING_ERRORS",
            "no Spelling Errors column (enable spell-check in SF)",
        );
    }
    if !has_grammar {
        ctx.skip(
            "GRAMMAR_ERRORS",
            "no Grammar Errors column (enable grammar-check in SF)",
        );
    }
}

/// `_path_of`: the URL's path part — query and fragment must not leak into
/// path-based checks.
fn path_of(url: &str) -> String {
    spiderhead_parse::py_url::urlsplit(url)
        .map(|s| s.path)
        .unwrap_or_default()
}

/// `_decoded_path`: the path as a reader sees it, percent-escapes resolved.
fn decoded_path(url: &str) -> String {
    spiderhead_parse::py_url::unquote(&path_of(url))
}

/// `SESSION_PARAM_RE`: whole-name match, case-insensitive.
fn is_session_param(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "sid"
            | "session"
            | "sessionid"
            | "session_id"
            | "phpsessid"
            | "jsessionid"
            | "asp.net_sessionid"
            | "cfid"
            | "cftoken"
    )
}

/// `TRACKING_PARAM_RE`: whole-name match, case-insensitive.
fn is_tracking_param(name: &str) -> bool {
    let n = name.to_lowercase();
    if let Some(rest) = n.strip_prefix("utm_") {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_lowercase() || c == '_');
    }
    matches!(
        n.as_str(),
        "gclid"
            | "gclsrc"
            | "fbclid"
            | "msclkid"
            | "yclid"
            | "dclid"
            | "gbraid"
            | "wbraid"
            | "_hsenc"
            | "_hsmi"
            | "mc_cid"
            | "mc_eid"
            | "pk_campaign"
            | "pk_kwd"
            | "pk_source"
            | "pk_medium"
            | "pk_content"
            | "vero_id"
            | "vero_conv"
            | "trksi"
            | "trksid"
            | "cmpid"
            | "mbid"
    )
}

/// reference `parse_qsl(query, keep_blank_values=True)`: bare and blank-valued
/// names are kept (the default drops them).
pub(super) fn parse_qsl_keep_blank(query: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name_value in query.split('&') {
        if name_value.is_empty() {
            continue;
        }
        let (name, value) = name_value.split_once('=').unwrap_or((name_value, ""));
        out.push((
            spiderhead_parse::py_url::unquote(&name.replace('+', " ")),
            spiderhead_parse::py_url::unquote(&value.replace('+', " ")),
        ));
    }
    out
}

/// reference `parse_qs(query).keys()`: first-seen parameter names in order,
/// bare/blank-valued names dropped.
fn parse_qs_names(query: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for (name, _) in spiderhead_parse::py_url::parse_qsl(query) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// `_redact_query_values`: each `name=value` becomes `name=[redacted]`,
/// urlencoded exactly as Creference `urlencode` renders it.
fn redact_query_values(url: &str) -> String {
    let Ok(parts) = spiderhead_parse::py_url::urlsplit(url) else {
        return url.to_owned();
    };
    let query = parse_qsl_keep_blank(&parts.query)
        .iter()
        .map(|(name, _)| {
            format!(
                "{}={}",
                spiderhead_parse::py_url::quote_plus(name),
                spiderhead_parse::py_url::quote_plus("[redacted]")
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    spiderhead_parse::py_url::urlunsplit(
        &parts.scheme,
        &parts.netloc,
        &parts.path,
        &query,
        &parts.fragment,
    )
}

/// `_slash_key`'s tuple: (scheme, netloc, path minus trailing slash, query).
type SlashKey = (String, String, String, String);
/// One observed variant in a slash group:
/// (url, status_code, is_indexable, canonical, redirect_url).
type SlashVariant = (String, Option<i64>, bool, String, Option<String>);

/// `_slash_key`: None for the site root, else (scheme, netloc, path minus any
/// trailing slash, query) — the identity of a trailing-slash pair.
fn slash_key(url: &str) -> Option<SlashKey> {
    let p = spiderhead_parse::py_url::urlsplit(url).ok()?;
    if p.path.is_empty() || p.path == "/" {
        return None;
    }
    Some((
        p.scheme.to_lowercase(),
        p.netloc.to_lowercase(),
        p.path.trim_end_matches('/').to_owned(),
        p.query,
    ))
}

/// `check_url_and_perf`: URL shape, depth, orphan and response-time checks
/// (reference the reference suite).
pub fn check_url_and_perf(ctx: &mut AuditContext) {
    let t = ctx.thresholds.clone();
    let url_max = t.get("url_max_chars").unwrap_or(115.0);
    let depth_max = t.get("crawl_depth_max").unwrap_or(4.0);
    let orphan_min = t.get("orphan_inlinks_min").unwrap_or(1.0);
    let rt_max = t.get("response_time_max_s").unwrap_or(1.5);
    let search_segments = ["search", "site-search", "recherche", "suche", "poisk"];
    let search_params = ["s", "search", "q", "query"];
    let (mut has_depth, mut has_inlinks, mut has_rt) = (false, false, false);
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        let url = page.url.clone();
        let path = decoded_path(&url);
        if url.chars().count() as f64 > url_max {
            ctx.add(
                "URL_TOO_LONG",
                Some(&url),
                details([
                    ("length", num(url.chars().count() as f64)),
                    ("max_chars", num(url_max)),
                ]),
            );
        }
        if url.contains('?')
            && page.is_indexable()
            && page.text("canonical").is_empty()
            && !page.body_unavailable()
        {
            ctx.add("URL_HAS_PARAMS", Some(&url), Default::default());
        }
        let search_parts = spiderhead_parse::py_url::urlsplit(&url).ok();
        let raw_path_segments: Vec<String> = search_parts
            .as_ref()
            .map(|s| {
                s.path
                    .to_lowercase()
                    .split('/')
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if raw_path_segments
            .iter()
            .any(|seg| search_segments.contains(&seg.as_str()))
        {
            ctx.add(
                "URL_INTERNAL_SEARCH",
                Some(&url),
                details([
                    ("path", Value::from(path.clone())),
                    (
                        "reason",
                        "recognized path-based internal search route".into(),
                    ),
                ]),
            );
        } else {
            let params: Vec<String> = search_parts
                .as_ref()
                .map(|s| parse_qs_names(&s.query))
                .unwrap_or_default();
            if params.iter().any(|p| search_params.contains(&p.as_str())) {
                ctx.add(
                    "URL_INTERNAL_SEARCH",
                    Some(&url),
                    details([(
                        "reason",
                        "recognized internal search query parameter".into(),
                    )]),
                );
            }
        }
        if search_parts.as_ref().is_some_and(|s| !s.query.is_empty()) {
            ctx.add("URL_HAS_PARAMETERS", Some(&url), Default::default());
        }
        if !path.is_ascii() {
            ctx.add("URL_NON_ASCII", Some(&url), Default::default());
        }
        if path != path.to_lowercase() {
            ctx.add("URL_UPPERCASE", Some(&url), Default::default());
        }
        if url.starts_with("http://") {
            ctx.add("HTTP_URL", Some(&url), Default::default());
        }
        let depth = page.num("crawl_depth");
        if let Some(d) = depth {
            has_depth = true;
            if d > depth_max {
                ctx.add(
                    "DEEP_CRAWL_DEPTH",
                    Some(&url),
                    details([("crawl_depth", num(d)), ("max", num(depth_max))]),
                );
            }
        }
        let inlinks = page.num("inlinks");
        // depth != 0 excludes the homepage; missing depth (None) still counts.
        if let Some(n) = inlinks {
            has_inlinks = true;
            if n < orphan_min && page.is_indexable() && depth != Some(0.0) {
                ctx.add("ORPHAN_PAGE", Some(&url), details([("inlinks", num(n))]));
            }
        }
        if let Some(rt) = page.num("response_time") {
            has_rt = true;
            if rt > rt_max {
                ctx.add(
                    "SLOW_RESPONSE",
                    Some(&url),
                    details([("response_time", num(rt)), ("max_s", num(rt_max))]),
                );
            }
        }
    }
    if !has_depth {
        ctx.skip("DEEP_CRAWL_DEPTH", "no Crawl Depth column in Internal:All");
    }
    if !has_inlinks {
        ctx.skip("ORPHAN_PAGE", "no Inlinks column in Internal:All");
    }
    if !has_rt {
        ctx.skip("SLOW_RESPONSE", "no Response Time column in Internal:All");
    }
}

/// `check_url_extra`: underscores, doubled slashes, spaces, repetitive path
/// segments and tracking parameters (reference `check_url_extra`).
pub fn check_url_extra(ctx: &mut AuditContext) {
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        let url = page.url.clone();
        let path = path_of(&url);
        if decoded_path(&url).contains('_') {
            ctx.add("URL_UNDERSCORES", Some(&url), Default::default());
        }
        if path.contains("//") {
            ctx.add("URL_MULTIPLE_SLASHES", Some(&url), Default::default());
        }
        if url.contains(' ') || url.contains("%20") {
            ctx.add("URL_CONTAINS_SPACE", Some(&url), Default::default());
        }
        // A repeated *word* is a duplicated prefix or a crawl trap; a repeated
        // number is a coordinate, so numeric segments are not compared.
        let segs: Vec<&str> = path
            .split('/')
            .filter(|s| !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()))
            .collect();
        if segs.len() >= 2
            && segs.iter().collect::<std::collections::HashSet<_>>().len() != segs.len()
        {
            ctx.add(
                "URL_REPETITIVE_PATH",
                Some(&url),
                details([("path", Value::from(path.clone()))]),
            );
        }
        if page.is_indexable() {
            let tp: Vec<String> = spiderhead_parse::py_url::urlsplit(&url)
                .map(|s| {
                    parse_qs_names(&s.query)
                        .into_iter()
                        .filter(|k| is_tracking_param(k))
                        .collect()
                })
                .unwrap_or_default();
            if !tp.is_empty() {
                ctx.add(
                    "URL_TRACKING_PARAMS",
                    Some(&url),
                    details([("params", Value::from(tp))]),
                );
            }
        }
    }
}

/// `check_url_hygiene`: known session parameters and observed slash pairs,
/// without URL-policy guesses (reference `check_url_hygiene`, #831).
pub fn check_url_hygiene(ctx: &mut AuditContext) {
    let mut session_observed = 0usize;
    let mut indexable_pages = 0usize;
    for i in ctx.indexable_html_pages() {
        indexable_pages += 1;
        let page = ctx.pages[i].clone();
        let query = spiderhead_parse::py_url::urlsplit(&page.url)
            .map(|s| s.query)
            .unwrap_or_default();
        let mut names: Vec<String> = spiderhead_parse::py_url::parse_qsl(&query)
            .into_iter()
            .map(|(name, _)| name)
            .filter(|n| is_session_param(n))
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup_by(|a, b| a == b);
        if names.is_empty() {
            continue;
        }
        session_observed += 1;
        ctx.add(
            "URL_SESSION_ID",
            Some(&redact_query_values(&page.url)),
            details([
                ("parameter_names", Value::from(names)),
                ("url_values_redacted", Value::from(true)),
            ]),
        );
    }
    if session_observed == 0 && indexable_pages == 0 {
        ctx.skip(
            "URL_SESSION_ID",
            "no indexable HTML pages to inspect for session parameters",
        );
    }
    // Slash pairs: group every page by its trailing-slash identity, then report
    // a live indexable pair that nothing converged.
    let mut pairs: std::collections::HashMap<SlashKey, Vec<SlashVariant>> = Default::default();
    for page in &ctx.pages {
        let Some(key) = slash_key(&page.url) else {
            continue;
        };
        let redirect = page
            .rec
            .get("redirect_url")
            .and_then(Value::as_str)
            .map(str::to_owned);
        pairs.entry(key).or_default().push((
            page.url.clone(),
            page.status_code,
            page.is_indexable(),
            page.text("canonical").to_owned(),
            redirect,
        ));
    }
    for group in pairs.values() {
        let slash: Vec<_> = group
            .iter()
            .filter(|(u, ..)| {
                spiderhead_parse::py_url::urlsplit(u)
                    .map(|s| s.path.ends_with('/'))
                    .unwrap_or(false)
            })
            .collect();
        let plain: Vec<_> = group
            .iter()
            .filter(|(u, ..)| {
                spiderhead_parse::py_url::urlsplit(u)
                    .map(|s| !s.path.ends_with('/'))
                    .unwrap_or(false)
            })
            .collect();
        if slash.is_empty() || plain.is_empty() {
            continue;
        }
        let observed: Vec<_> = slash.iter().chain(plain.iter()).collect();
        let urls: std::collections::BTreeSet<String> =
            observed.iter().map(|(u, ..)| u.clone()).collect();
        let converged = observed.iter().any(|(_, status, _, _, redirect)| {
            status.is_some_and(|c| (300..=399).contains(&c))
                && redirect.as_ref().is_some_and(|r| urls.contains(r))
        }) || observed.iter().any(|(u, _, _, canonical, _)| {
            !canonical.is_empty() && urls.contains(canonical) && canonical != u
        });
        if converged
            || !observed
                .iter()
                .all(|(_, status, indexable, _, _)| *indexable && *status == Some(200))
        {
            continue;
        }
        let mut url_list: Vec<String> = urls.iter().cloned().collect();
        url_list.sort();
        ctx.add_with(
            "URL_TRAILING_SLASH_INCONSISTENT",
            Some(&url_list[0]),
            details([
                ("observed_variants", Value::from(url_list.clone())),
                (
                    "status_codes",
                    Value::Object(
                        observed
                            .iter()
                            .map(|(u, status, ..)| {
                                (u.clone(), status.map_or(Value::Null, Value::from))
                            })
                            .collect(),
                    ),
                ),
                (
                    "canonical",
                    Value::Object(
                        observed
                            .iter()
                            .map(|(u, _, _, canonical, _)| {
                                (
                                    u.clone(),
                                    if canonical.is_empty() {
                                        Value::Null
                                    } else {
                                        Value::from(canonical.clone())
                                    },
                                )
                            })
                            .collect(),
                    ),
                ),
            ]),
            None,
            url_list.len() as i64,
            None,
        );
    }
}

/// Insertion-ordered grouping of URLs by a value (reference `defaultdict(list)` iteration order).
fn push_group(groups: &mut Vec<(String, Vec<String>)>, value: &str, url: &str) {
    match groups.iter_mut().find(|(v, _)| v == value) {
        Some((_, urls)) => urls.push(url.to_owned()),
        None => groups.push((value.to_owned(), vec![url.to_owned()])),
    }
}

/// `check_duplicates`: pages sharing a title, description or heading value.
pub fn check_duplicates(ctx: &mut AuditContext) {
    let mut by: [Vec<(String, Vec<String>)>; 5] = Default::default();
    for i in ctx.indexable_html_pages() {
        let page = &ctx.pages[i];
        for (slot, key) in ["title", "meta_description", "h1", "h2", "hash"]
            .into_iter()
            .enumerate()
        {
            let value = page.text(key);
            if !value.is_empty() {
                push_group(&mut by[slot], value.trim(), &page.url);
            }
        }
    }
    let checks = [
        "TITLE_DUPLICATE",
        "DESC_DUPLICATE",
        "H1_DUPLICATE",
        "H2_DUPLICATE",
        "DUPLICATE_BY_HASH",
    ];
    for (groups, check) in by.into_iter().zip(checks) {
        for (value, urls) in groups {
            if urls.len() < 2 {
                continue;
            }
            let mut sorted = urls.clone();
            sorted.sort();
            let group = ctx.add_group(check, Some(&value), sorted);
            for url in &urls {
                ctx.add_with(
                    check,
                    Some(url),
                    details([
                        // The hash itself never appears in a finding's details.
                        (
                            "value",
                            if check == "DUPLICATE_BY_HASH" {
                                Value::Null
                            } else {
                                Value::from(value.clone())
                            },
                        ),
                        ("duplicate_count", urls.len().into()),
                    ]),
                    None,
                    1,
                    Some(group.clone()),
                );
            }
        }
    }
}

/// `check_native_structured_and_mixed`: STRUCTURED_DATA_MISSING from the stored block count.
pub fn check_structured_data_missing(ctx: &mut AuditContext) {
    let mut measured = 0;
    for i in ctx.indexable_html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let Some(found) = page.num("jsonld_blocks_found") else {
            continue;
        };
        measured += 1;
        if found == 0.0 {
            ctx.add(
                "STRUCTURED_DATA_MISSING",
                Some(&page.url),
                Default::default(),
            );
        }
    }
    if measured == 0 {
        ctx.skip(
            "STRUCTURED_DATA_MISSING",
            "no indexable HTML page carries a block count",
        );
    }
}

/// `check_native_structured_and_mixed`: MIXED_CONTENT — an https page whose
/// resource graph names an http subresource. The check is native-resolved
/// only while the scan carries a non-empty `resource_graph_occurrences`
/// table (`native_check_ids`), so a missing table or connection exits early.
pub fn check_mixed_content(ctx: &mut AuditContext) {
    let Some(con) = ctx.scan.as_ref() else {
        return;
    };
    let has_table = con
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='resource_graph_occurrences'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .is_some();
    let has_row = has_table
        && con
            .query_row(
                "SELECT 1 FROM resource_graph_occurrences LIMIT 1",
                [],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
    if !has_row {
        return;
    }
    let insecure = {
        let Ok(mut stmt) = con.prepare(
            "SELECT p.url, o.resolved_url FROM resource_graph_occurrences o \
             JOIN urls p ON p.url_id=o.page_url_id \
             WHERE p.url LIKE 'https://%' AND o.resolved_url LIKE 'http://%' \
             ORDER BY p.url, o.resolved_url",
        ) else {
            return;
        };
        let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        else {
            return;
        };
        let mut insecure: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in rows.flatten() {
            let (page_url, resource) = row;
            let list = insecure.entry(page_url).or_default();
            if !list.contains(&resource) {
                list.push(resource);
            }
        }
        insecure
    };
    for (page_url, resources) in insecure {
        ctx.add(
            "MIXED_CONTENT",
            Some(&page_url),
            details([
                ("count", Value::from(resources.len() as i64)),
                (
                    "insecure_resources",
                    Value::Array(
                        resources
                            .iter()
                            .take(PLACEMENT_EVIDENCE_ITEMS)
                            .map(|r| Value::from(r.clone()))
                            .collect(),
                    ),
                ),
            ]),
        );
    }
}

/// Literal non-Lorem template markers (`{{todo}}`, `[tbd]`, ...) in page declarations.
fn placeholder_marker_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)(?:\{\{\s*(?:todo|tbd|placeholder)\s*\}\}|\[\s*(?:todo|tbd|placeholder)\s*\])",
        )
        .expect("static pattern")
    })
}

/// `check_placeholder_markers`: template markers left in title, description or headings.
pub fn check_placeholder_markers(ctx: &mut AuditContext) {
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let mut observed = Vec::new();
        for field in ["title", "meta_description", "h1", "h1_2", "h2"] {
            let Some(value) = page.rec.get(field).and_then(Value::as_str) else {
                continue;
            };
            let mut markers: Vec<String> = placeholder_marker_re()
                .find_iter(value)
                .map(|m| m.as_str().to_owned())
                .collect();
            markers.sort_by_key(|m| (m.to_lowercase(), m.clone()));
            markers.dedup();
            if !markers.is_empty() {
                observed.push(Value::Object(details([
                    ("field", Value::from(field)),
                    ("markers", Value::from(markers)),
                    (
                        "value",
                        Value::from(value.chars().take(240).collect::<String>()),
                    ),
                ])));
            }
        }
        if !observed.is_empty() {
            ctx.add(
                "PLACEHOLDER_MARKER",
                Some(&page.url),
                details([("observed", Value::Array(observed))]),
            );
        }
    }
}
