//! Heuristics beyond the native findings: HTML weight anomalies, bytes per word, templated titles,
//! retained-HTML DOM metrics and stored-text duplicate passes (reference the reference suite).

use std::collections::BTreeMap;

use serde_json::{json, Value};
use spiderhead_parse::duplicate::{content_hash, find_duplicates, DuplicateItem, DuplicateOptions};
use spiderhead_store::bodies::read_page_document;
use spiderhead_store::corpus_inputs::{
    MAX_CORPUS_DOCUMENTS, MAX_CORPUS_INPUT_BYTES, MAX_DOCUMENT_BYTES,
    MAX_DUPLICATE_CANDIDATE_COMPARISONS, MAX_DUPLICATE_SHINGLES,
};
use spiderhead_store::error::StoreError;

use super::{details, round_to};
use crate::audit::AuditContext;

const SEPARATORS: [&str; 6] = [" | ", " — ", " - ", " · ", " :: ", " // "];
const MIN_IQR_FRACTION_OF_MEDIAN: f64 = 0.10;
const HTML_OVER_2MB_BYTES: f64 = 2.0 * 1024.0 * 1024.0;

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let pos = (sorted.len() - 1) as f64 * q;
    let lo = pos as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    let frac = pos - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

fn median(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// `check_html_weight`: LARGE_HTML, HTML_OVER_2MB and HTML_BLOAT over the indexable HTML population.
pub fn check_html_weight(ctx: &mut AuditContext) {
    let pages = ctx.indexable_html_pages();
    let size_of =
        |ctx: &AuditContext, i: usize| ctx.pages[i].num("size_bytes").filter(|s| *s != 0.0);
    let mut sizes: Vec<f64> = pages
        .iter()
        .filter_map(|&i| ctx.pages[i].num("size_bytes"))
        .filter(|s| *s > 0.0)
        .collect();
    sizes.sort_by(f64::total_cmp);
    if sizes.is_empty() {
        for id in ["LARGE_HTML", "HTML_BLOAT", "HTML_OVER_2MB"] {
            ctx.skip(id, "no Size (bytes) data for HTML pages");
        }
        return;
    }
    let t = ctx.thresholds.clone();
    let med = median(&sizes);
    let (p75, p25) = (quantile(&sizes, 0.75), quantile(&sizes, 0.25));
    let iqr = p75 - p25;
    let median_div = if med == 0.0 { 1.0 } else { med };
    let abs_bytes = t.large_html_abs_kb * 1024.0;
    let tukey_upper = (iqr >= median_div * MIN_IQR_FRACTION_OF_MEDIAN).then_some(p75 + 1.5 * iqr);

    let mut ranked: Vec<(String, f64)> = pages
        .iter()
        .filter_map(|&i| size_of(ctx, i).map(|s| (ctx.pages[i].url.clone(), s)))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let rank_of: BTreeMap<String, usize> = ranked
        .iter()
        .enumerate()
        .map(|(n, (u, _))| (u.clone(), n + 1))
        .fold(BTreeMap::new(), |mut m, (u, n)| {
            m.entry(u).or_insert(n);
            m
        });

    let mut bpw_vals: Vec<f64> = Vec::new();
    for &i in &pages {
        let (size, wc) = (
            ctx.pages[i].num("size_bytes").unwrap_or(0.0),
            ctx.pages[i].num("word_count").unwrap_or(0.0),
        );
        if size != 0.0 && wc > 0.0 {
            let bpw = size / wc;
            ctx.pages[i]
                .metrics
                .insert("bytes_per_word".into(), Value::from(round_to(bpw, 1)));
            bpw_vals.push(bpw);
        }
    }
    bpw_vals.sort_by(f64::total_cmp);
    let bpw_median = (!bpw_vals.is_empty()).then(|| median(&bpw_vals));

    for &i in &pages {
        let Some(size) = size_of(ctx, i) else {
            continue;
        };
        let page = ctx.pages[i].clone();
        let ratio = size / median_div;
        ctx.pages[i].metrics.insert(
            "size_vs_median_ratio".into(),
            Value::from(round_to(ratio, 2)),
        );
        let outlier = size > med * t.large_html_x_median || tukey_upper.is_some_and(|u| size > u);
        if size > abs_bytes || outlier {
            ctx.add(
                "LARGE_HTML",
                Some(&page.url),
                details([
                    ("size_bytes", json!(size as i64)),
                    ("site_median", json!(med as i64)),
                    ("ratio", Value::from(round_to(ratio, 2))),
                    (
                        "rank",
                        rank_of.get(&page.url).map_or(Value::Null, |r| json!(r)),
                    ),
                    ("abs_threshold_kb", json!(t.large_html_abs_kb as i64)),
                    ("outlier", json!(outlier)),
                ]),
            );
        }
        if size > HTML_OVER_2MB_BYTES {
            ctx.add(
                "HTML_OVER_2MB",
                Some(&page.url),
                details([
                    ("size_bytes", json!(size as i64)),
                    ("limit_bytes", json!(HTML_OVER_2MB_BYTES as i64)),
                ]),
            );
        }
        let wc = page.num("word_count").unwrap_or(0.0);
        let bpw = ctx.pages[i]
            .metrics
            .get("bytes_per_word")
            .and_then(Value::as_f64);
        if let (Some(bpw), Some(m)) = (bpw, bpw_median) {
            if bpw != 0.0 && m != 0.0 && bpw > m * t.bytes_per_word_x_median && wc > 0.0 {
                ctx.add(
                    "HTML_BLOAT",
                    Some(&page.url),
                    details([
                        ("bytes_per_word", Value::from(round_to(bpw, 1))),
                        ("site_median_bpw", Value::from(round_to(m, 1))),
                        ("word_count", page.opt("word_count")),
                        ("size_bytes", json!(size as i64)),
                    ]),
                );
            }
        }
    }
}

// --------------------------------------------------------------------------
// DOM depth / nodes — selected native document (reference `_dom_metrics`, `check_dom`)
// --------------------------------------------------------------------------

/// `str(exc)` of the errors the retained-body read raises, for omission counting.
fn scan_message(err: &StoreError) -> String {
    match err {
        StoreError::ScanError(e) => e.message.clone(),
        StoreError::Invalid(m) => m.clone(),
        other => other.to_string(),
    }
}

/// `_stored_html_reader`: read the page's selected retained document, bounded.
pub(crate) fn stored_html(ctx: &AuditContext, url: &str) -> Result<String, String> {
    match ctx.scan.as_ref() {
        Some(con) => read_page_document(con, url, MAX_DOCUMENT_BYTES).map_err(|e| scan_message(&e)),
        None => {
            Err("no stored HTML (input.html_store_dir not set; crawl was not stored)".to_owned())
        }
    }
}

/// `_dom_metrics`: `(max depth, element count)` over the lxml tree, single DFS where the
/// document's top-level elements sit at depth 0 — `lxml.html.fromstring` returns the root
/// element, so an `<html>` root counts 0.
fn dom_metrics(html: &str) -> Result<(i64, i64), String> {
    if html.trim().is_empty() {
        // lxml.html.fromstring raises `ParserError("Document is empty")` on empty
        // or whitespace-only input; the check counts that as an omission reason.
        return Err("Document is empty".to_owned());
    }
    let soup = spiderhead_parse::dom::parse_document_lxml(html);
    let mut nodes = 0i64;
    let mut max_depth = 0i64;
    let mut stack: Vec<(spiderhead_parse::dom::ElementRef<'_>, i64)> = soup
        .tree
        .root()
        .children()
        .filter_map(spiderhead_parse::dom::ElementRef::wrap)
        .map(|el| (el, 0))
        .collect();
    while let Some((el, depth)) = stack.pop() {
        nodes += 1;
        max_depth = max_depth.max(depth);
        for child in el
            .children()
            .filter_map(spiderhead_parse::dom::ElementRef::wrap)
        {
            stack.push((child, depth + 1));
        }
    }
    Ok((max_depth, nodes))
}

/// `_body_coverage`: the "matched only M of N" note when pages were omitted, else `None`.
fn body_coverage(matched: i64, total: i64, omitted: &BTreeMap<String, i64>) -> Option<String> {
    if omitted.is_empty() {
        return None;
    }
    let reasons = omitted
        .iter()
        .map(|(reason, count)| format!("{reason} ({count})"))
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "stored HTML matched only {matched} of {total} indexable pages; {reasons}"
    ))
}

/// `check_dom`: DOM_TOO_DEEP and DOM_TOO_MANY_NODES over retained page HTML.
pub fn check_dom(ctx: &mut AuditContext) {
    let pages = ctx.indexable_html_pages();
    let total = pages.len() as i64;
    let mut omitted: BTreeMap<String, i64> = BTreeMap::new();
    let mut matched = 0i64;
    let mut measured: Vec<Option<(i64, i64)>> = Vec::with_capacity(pages.len());
    for &i in &pages {
        match stored_html(ctx, &ctx.pages[i].url).and_then(|html| dom_metrics(&html)) {
            Ok((depth, nodes)) => {
                matched += 1;
                ctx.pages[i]
                    .metrics
                    .insert("dom_depth".into(), Value::from(depth));
                ctx.pages[i]
                    .metrics
                    .insert("dom_nodes".into(), Value::from(nodes));
                measured.push(Some((depth, nodes)));
            }
            Err(reason) => {
                *omitted.entry(reason).or_default() += 1;
                measured.push(None);
            }
        }
    }
    let coverage_note = body_coverage(matched, total, &omitted);
    let t = ctx.thresholds.clone();
    for (&i, metrics) in pages.iter().zip(measured.iter()) {
        let Some((depth, nodes)) = metrics else {
            continue;
        };
        if *depth as f64 > t.dom_depth_max {
            let mut d = details([
                ("dom_depth", Value::from(*depth)),
                ("max", Value::from(t.dom_depth_max as i64)),
            ]);
            if let Some(note) = &coverage_note {
                d.insert("html_coverage".into(), Value::from(note.clone()));
            }
            let url = ctx.pages[i].url.clone();
            ctx.add("DOM_TOO_DEEP", Some(&url), d);
        }
        if *nodes as f64 > t.dom_nodes_max {
            let mut d = details([
                ("dom_nodes", Value::from(*nodes)),
                ("max", Value::from(t.dom_nodes_max as i64)),
            ]);
            if let Some(note) = &coverage_note {
                d.insert("html_coverage".into(), Value::from(note.clone()));
            }
            let url = ctx.pages[i].url.clone();
            ctx.add("DOM_TOO_MANY_NODES", Some(&url), d);
        }
    }
    if let Some(note) = &coverage_note {
        let reason = format!("{note} — DOM size not assessed for the rest");
        ctx.skip("DOM_TOO_DEEP", &reason);
        ctx.skip("DOM_TOO_MANY_NODES", &reason);
    } else if total == 0 {
        ctx.skip("DOM_TOO_DEEP", "no indexable HTML pages");
        ctx.skip("DOM_TOO_MANY_NODES", "no indexable HTML pages");
    }
}

// --------------------------------------------------------------------------
// Near-duplicate content — retained native document (`check_content_duplication`)
// --------------------------------------------------------------------------

/// `check_content_duplication`: NEAR_DUPLICATE and DUPLICATE_BY_HASH over content-area text.
pub fn check_content_duplication(ctx: &mut AuditContext) {
    let candidate_pages = ctx.indexable_html_pages();
    let total = candidate_pages.len() as i64;
    let mut omitted: BTreeMap<String, i64> = BTreeMap::new();
    let mut items: Vec<DuplicateItem> = Vec::new();
    let mut input_bytes = 0usize;
    let mut visited = 0i64;
    for &i in &candidate_pages {
        if items.len() as i64 >= MAX_CORPUS_DOCUMENTS {
            *omitted
                .entry("duplicate document limit exceeded".to_owned())
                .or_default() += total - visited;
            break;
        }
        visited += 1;
        let html = match stored_html(ctx, &ctx.pages[i].url) {
            Ok(html) => html,
            Err(reason) => {
                *omitted.entry(reason).or_default() += 1;
                continue;
            }
        };
        let text = (|| -> Result<String, String> {
            let soup = spiderhead_parse::dom::parse_document(&html);
            let area = spiderhead_parse::content_area::resolve_content_area(&soup, None)
                .map_err(|e| e.to_string())?;
            Ok(spiderhead_parse::content_area::extract_area_text(&area))
        })();
        let text = match text {
            Ok(text) => text,
            Err(reason) => {
                *omitted.entry(reason).or_default() += 1;
                continue;
            }
        };
        if text.trim().is_empty() {
            *omitted
                .entry("no comparable content-area text".to_owned())
                .or_default() += 1;
            continue;
        }
        let size = text.len();
        if input_bytes + size > MAX_CORPUS_INPUT_BYTES as usize {
            *omitted
                .entry("duplicate input-byte budget exceeded".to_owned())
                .or_default() += total - visited + 1;
            break;
        }
        input_bytes += size;
        items.push(DuplicateItem {
            id: ctx.pages[i].url.clone(),
            text,
            indexable: true,
        });
    }

    let coverage_note = body_coverage(items.len() as i64, total, &omitted);
    if items.len() < 2 {
        let reason = coverage_note
            .unwrap_or_else(|| "fewer than two comparable indexable page bodies".to_owned());
        ctx.skip("NEAR_DUPLICATE", &reason);
        ctx.skip("DUPLICATE_BY_HASH", &reason);
        return;
    }

    let options = DuplicateOptions {
        threshold: ctx.thresholds.near_duplicate_similarity,
        max_shingles: Some(MAX_DUPLICATE_SHINGLES as usize),
        max_candidate_comparisons: Some(MAX_DUPLICATE_CANDIDATE_COMPARISONS as usize),
        ..DuplicateOptions::default()
    };
    let report = match find_duplicates(&items, &options) {
        Ok(report) => report,
        Err(spiderhead_parse::types::ParseError::BudgetExceeded(reason)) => {
            ctx.skip("NEAR_DUPLICATE", &reason);
            // Exact hashes need no candidate/shingle budget; keep their measurement.
            let mut exact: Vec<(String, Vec<String>)> = Vec::new();
            for item in &items {
                let hash = content_hash(&item.text);
                match exact.iter_mut().find(|(h, _)| *h == hash) {
                    Some((_, members)) => members.push(item.id.clone()),
                    None => exact.push((hash, vec![item.id.clone()])),
                }
            }
            spiderhead_parse::duplicate::DuplicateReport {
                ok: true,
                count: items.len() as u64,
                threshold: Some(options.threshold),
                clusters: Vec::new(),
                exact_duplicates: exact
                    .into_iter()
                    .filter(|(_, members)| members.len() > 1)
                    .map(
                        |(hash, members)| spiderhead_parse::duplicate::ExactDuplicateGroup {
                            hash,
                            members,
                        },
                    )
                    .collect(),
                candidate_pairs_checked: 0,
                excluded_non_indexable: 0,
                excluded_no_id: 0,
                excluded_no_text: 0,
                excluded_duplicate_id: 0,
                measured: false,
                reason: None,
                fingerprints: None,
            }
        }
        Err(e) => {
            ctx.skip("NEAR_DUPLICATE", &e.to_string());
            ctx.skip("DUPLICATE_BY_HASH", &e.to_string());
            return;
        }
    };

    for cluster in &report.clusters {
        let mut members = cluster.members.clone();
        members.sort();
        let group = ctx.add_group("NEAR_DUPLICATE", None, members.clone());
        for url in &members {
            let mut d = details([
                (
                    "cluster_min_similarity",
                    Value::from(cluster.min_similarity),
                ),
                ("cluster_size", Value::from(members.len() as i64)),
                (
                    "comparison_url",
                    members
                        .iter()
                        .find(|m| *m != url)
                        .map_or(Value::Null, |m| Value::from(m.clone())),
                ),
                (
                    "status_code",
                    ctx.page_index(url)
                        .and_then(|p| ctx.pages[p].status_code)
                        .map_or(Value::Null, Value::from),
                ),
                ("method", Value::from("content_area_simhash_lsh")),
            ]);
            if let Some(note) = &coverage_note {
                d.insert("html_coverage".into(), Value::from(note.clone()));
            }
            ctx.add_with(
                "NEAR_DUPLICATE",
                Some(url.as_str()),
                d,
                None,
                1,
                Some(group.clone()),
            );
        }
    }
    if coverage_note.is_some() {
        ctx.skip(
            "NEAR_DUPLICATE",
            &format!(
                "{} — duplicates not assessed for the rest",
                coverage_note.as_deref().unwrap_or("")
            ),
        );
    }
    for exact in &report.exact_duplicates {
        let mut members = exact.members.clone();
        members.sort();
        let group = ctx.add_group("DUPLICATE_BY_HASH", None, members.clone());
        for url in &members {
            let mut d = details([
                ("duplicate_count", Value::from(members.len() as i64)),
                (
                    "comparison_url",
                    members
                        .iter()
                        .find(|m| *m != url)
                        .map_or(Value::Null, |m| Value::from(m.clone())),
                ),
                (
                    "status_code",
                    ctx.page_index(url)
                        .and_then(|p| ctx.pages[p].status_code)
                        .map_or(Value::Null, Value::from),
                ),
                ("content_hash", Value::from(exact.hash.clone())),
                ("method", Value::from("content_area_text_hash")),
            ]);
            if let Some(note) = &coverage_note {
                d.insert("html_coverage".into(), Value::from(note.clone()));
            }
            ctx.add_with(
                "DUPLICATE_BY_HASH",
                Some(url.as_str()),
                d,
                None,
                1,
                Some(group.clone()),
            );
        }
    }
    if coverage_note.is_some() {
        ctx.skip(
            "DUPLICATE_BY_HASH",
            &format!(
                "{} — duplicates not assessed for the rest",
                coverage_note.as_deref().unwrap_or("")
            ),
        );
    }
}

/// `check_templated_titles`: one shared title prefix or suffix across most pages.
pub fn check_templated_titles(ctx: &mut AuditContext) {
    let titles: Vec<String> = ctx
        .indexable_html_pages()
        .into_iter()
        .map(|i| ctx.pages[i].text("title").trim().to_owned())
        .filter(|t| !ctx.pages.is_empty() && !t.is_empty())
        .collect();
    if titles.len() < 5 {
        ctx.skip("TITLE_TEMPLATED", "too few titles to assess templating");
        return;
    }
    let mut prefixes: Vec<(String, usize)> = Vec::new();
    let mut suffixes: Vec<(String, usize)> = Vec::new();
    let bump = |v: &mut Vec<(String, usize)>, k: String| match v.iter_mut().find(|(t, _)| *t == k) {
        Some(e) => e.1 += 1,
        None => v.push((k, 1)),
    };
    for title in &titles {
        for sep in SEPARATORS {
            if title.contains(sep) {
                bump(
                    &mut prefixes,
                    title.split_once(sep).map_or("", |x| x.0).trim().to_owned(),
                );
                bump(
                    &mut suffixes,
                    title.rsplit_once(sep).map_or("", |x| x.1).trim().to_owned(),
                );
                break;
            }
        }
    }
    let mut best: Option<(&str, String, usize)> = None;
    for (direction, counts) in [("prefix", &prefixes), ("suffix", &suffixes)] {
        let Some(top) = counts.iter().fold(None::<&(String, usize)>, |acc, e| {
            if acc.is_none_or(|a| e.1 > a.1) {
                Some(e)
            } else {
                acc
            }
        }) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| top.1 > b.2) {
            best = Some((direction, top.0.clone(), top.1));
        }
    }
    let Some((direction, token, count)) = best else {
        return;
    };
    let share = count as f64 / titles.len() as f64;
    if share >= ctx.thresholds.templated_title_share {
        ctx.add(
            "TITLE_TEMPLATED",
            None,
            details([
                ("direction", direction.into()),
                ("token", token.into()),
                ("share", Value::from(round_to(share, 2))),
                ("pages", count.into()),
                ("total", titles.len().into()),
            ]),
        );
    }
}
