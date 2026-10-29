//! Heuristics beyond the exporter's own findings: HTML weight anomalies, DOM
//! size, bytes per word, stored-text duplication and templated titles.
//!
//! Derived metrics are written back onto each page even when no threshold
//! trips, so downstream analysis keeps the raw signal.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use spiderhead_parse::content_area::{extract_area_text, resolve_content_area};
use spiderhead_parse::dom::parse_document;
use spiderhead_parse::duplicate::{content_hash, find_duplicates, DuplicateItem, DuplicateOptions};
use spiderhead_parse::py_str::py_strip;
use spiderhead_parse::types::{ContentAreaConfig, ParseError};
use spiderhead_store::bodies::read_page_document;
use spiderhead_store::corpus_inputs::{
    MAX_CORPUS_DOCUMENTS, MAX_CORPUS_INPUT_BYTES, MAX_DOCUMENT_BYTES,
    MAX_DUPLICATE_CANDIDATE_COMPARISONS, MAX_DUPLICATE_SHINGLES,
};
use spiderhead_store::error::StoreError;

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::normalize::find_column;
use crate::pyvalue::truthy;

/// Title separators tried in order when looking for a shared template token.
pub const SEPARATORS: [&str; 6] = [" | ", " — ", " - ", " · ", " :: ", " // "];

/// A Tukey fence measures distance in units of spread. A templated site has
/// almost none, so below this fraction of the median the fence degenerates
/// into "heavier than the median" and is not used.
pub const MIN_IQR_FRACTION_OF_MEDIAN: f64 = 0.10;

/// An absolute HTML ceiling, separate from the site-relative outlier rule.
pub const HTML_OVER_2MB_BYTES: i64 = 2 * 1024 * 1024;

/// Linear-interpolated quantile of an ascending slice.
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

/// A positive numeric metric as `f64`; anything else is "no value".
fn num(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

/// Round to `places` decimals, correctly rounded with ties to even.
fn round_to(x: f64, places: usize) -> f64 {
    format!("{x:.places$}").parse().unwrap_or(x)
}

/// A number as JSON, integral values as integers.
fn jnum(x: f64) -> Value {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 9.0e15 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

fn threshold(ctx: &AuditContext, key: &str) -> Result<f64, ContextError> {
    ctx.thresholds
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| ContextError::Value(format!("threshold {key} missing")))
}

/// Size distribution of the indexable HTML pages; empty when none has a size.
pub fn size_stats(ctx: &mut AuditContext) -> Map<String, Value> {
    let mut sizes: Vec<f64> = ctx
        .indexable_html_pages()
        .iter()
        .filter_map(|p| p.map(|p| num(p.metrics.get("size_bytes"))))
        .filter(|s| *s > 0.0)
        .collect();
    sizes.sort_by(f64::total_cmp);
    let mut out = Map::new();
    if sizes.is_empty() {
        return out;
    }
    out.insert("count".into(), json!(sizes.len()));
    out.insert("median".into(), jnum(median(&sizes)));
    out.insert("p75".into(), jnum(quantile(&sizes, 0.75)));
    out.insert("p90".into(), jnum(quantile(&sizes, 0.90)));
    out.insert("p95".into(), jnum(quantile(&sizes, 0.95)));
    out.insert("max".into(), jnum(sizes[sizes.len() - 1]));
    out.insert(
        "iqr".into(),
        jnum(quantile(&sizes, 0.75) - quantile(&sizes, 0.25)),
    );
    out
}

/// LARGE_HTML, HTML_OVER_2MB and HTML_BLOAT from the page size distribution.
pub fn check_html_weight(ctx: &mut AuditContext) -> Result<Map<String, Value>, ContextError> {
    let stats = size_stats(ctx);
    if stats.is_empty() {
        for id in ["LARGE_HTML", "HTML_BLOAT", "HTML_OVER_2MB"] {
            ctx.skip(id, "no Size (bytes) data for HTML pages");
        }
        return Ok(stats);
    }
    let s = |k: &str| stats[k].as_f64().unwrap_or(0.0);
    let med = if s("median") == 0.0 { 1.0 } else { s("median") };
    let abs_kb = ctx
        .thresholds
        .get("large_html_abs_kb")
        .cloned()
        .unwrap_or(Value::Null);
    let abs_bytes = threshold(ctx, "large_html_abs_kb")? * 1024.0;
    let k = threshold(ctx, "large_html_x_median")?;
    let bpw_x = threshold(ctx, "bytes_per_word_x_median")?;
    let has_spread = s("iqr") >= med * MIN_IQR_FRACTION_OF_MEDIAN;
    let tukey_upper = has_spread.then(|| s("p75") + 1.5 * s("iqr"));

    let pages = ctx.indexable_html_pages();
    // Rank by size, heaviest first; equal sizes keep page order.
    let mut ranked: Vec<(String, f64)> = pages
        .iter()
        .filter_map(|p| {
            p.map(|p| {
                let size = num(p.metrics.get("size_bytes")).filter(|s| *s != 0.0)?;
                Some((p.url.clone(), size))
            })
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let rank_of: HashMap<&str, usize> = ranked
        .iter()
        .enumerate()
        .map(|(i, (u, _))| (u.as_str(), i + 1))
        .collect();

    // Bytes-per-word distribution for the bloat rule.
    let mut bpw_vals: Vec<f64> = Vec::new();
    for page in &pages {
        let url = page.url();
        let (size, wc) = page.map(|p| {
            (
                num(p.metrics.get("size_bytes")).filter(|s| *s != 0.0),
                num(p.metrics.get("word_count")).filter(|w| *w > 0.0),
            )
        });
        if let (Some(size), Some(wc)) = (size, wc) {
            let bpw = size / wc;
            ctx.mutate_page(&url, |p| {
                p.metrics
                    .insert("bytes_per_word".into(), json!(round_to(bpw, 1)));
            });
            bpw_vals.push(bpw);
        }
    }
    bpw_vals.sort_by(f64::total_cmp);
    let bpw_median = (!bpw_vals.is_empty()).then(|| median(&bpw_vals));

    for page in &pages {
        let url = page.url();
        let Some(size) = page.map(|p| num(p.metrics.get("size_bytes")).filter(|s| *s != 0.0))
        else {
            continue;
        };
        let ratio = size / med;
        ctx.mutate_page(&url, |p| {
            p.metrics
                .insert("size_vs_median_ratio".into(), json!(round_to(ratio, 2)));
        });
        let is_outlier = size > med * k || tukey_upper.is_some_and(|u| size > u);
        if size > abs_bytes || is_outlier {
            let mut d = Map::new();
            d.insert("size_bytes".into(), json!(size as i64));
            d.insert("site_median".into(), json!(med as i64));
            d.insert("ratio".into(), json!(round_to(ratio, 2)));
            d.insert(
                "rank".into(),
                rank_of.get(url.as_str()).map_or(Value::Null, |r| json!(r)),
            );
            d.insert("abs_threshold_kb".into(), abs_kb.clone());
            d.insert("outlier".into(), json!(is_outlier));
            ctx.add(
                "LARGE_HTML",
                AddOpts {
                    target_url: Some(url.clone()),
                    details: d,
                    ..AddOpts::default()
                },
            )?;
        }
        if size > HTML_OVER_2MB_BYTES as f64 {
            let mut d = Map::new();
            d.insert("size_bytes".into(), json!(size as i64));
            d.insert("limit_bytes".into(), json!(HTML_OVER_2MB_BYTES));
            ctx.add(
                "HTML_OVER_2MB",
                AddOpts {
                    target_url: Some(url.clone()),
                    details: d,
                    ..AddOpts::default()
                },
            )?;
        }
        let (bpw, wc) = page.map(|p| {
            (
                num(p.metrics.get("bytes_per_word")),
                p.metrics.get("word_count").cloned(),
            )
        });
        let wc_positive = num(wc.as_ref()).is_some_and(|w| w > 0.0);
        if let (Some(bpw), Some(bm)) = (bpw.filter(|b| *b != 0.0), bpw_median.filter(|m| *m != 0.0))
        {
            if bpw > bm * bpw_x && wc_positive {
                let mut d = Map::new();
                d.insert("bytes_per_word".into(), json!(round_to(bpw, 1)));
                d.insert("site_median_bpw".into(), json!(round_to(bm, 1)));
                d.insert("word_count".into(), wc.unwrap_or(Value::Null));
                d.insert("size_bytes".into(), json!(size as i64));
                ctx.add(
                    "HTML_BLOAT",
                    AddOpts {
                        target_url: Some(url.clone()),
                        details: d,
                        ..AddOpts::default()
                    },
                )?;
            }
        }
    }
    Ok(stats)
}

// -- DOM size ---------------------------------------------------------------

/// Tags whose presence makes a body a block container; irrelevant to depth, so
/// only the single-element rule below matters.
fn looks_like_full_html(html: &str) -> bool {
    let t = html.trim_start();
    let head: String = t.chars().take(9).collect::<String>().to_lowercase();
    head.starts_with("<html") || head.starts_with("<!doctype")
}

fn has_xml_encoding_declaration(html: &str) -> bool {
    let re = regex::Regex::new(r#"^(<\?xml[^>]+)\s+encoding\s*=\s*["'][^"']*["'](\s*\?>|)"#)
        .expect("xml declaration regex");
    re.is_match(html)
}

/// Max element nesting depth and element count of the document the way the
/// forgiving HTML parser would hand it to a caller: a lone top-level element is
/// the root, loose content is wrapped in its body.
pub fn dom_metrics(html: &str) -> Result<(i64, i64), String> {
    if has_xml_encoding_declaration(html) {
        return Err("Unicode strings with encoding declaration are not supported. Please use bytes input or XML fragments without declaration.".into());
    }
    let soup = parse_document(html);
    let root = soup.tree.root();
    let doc = root
        .children()
        .find(|n| n.value().is_element())
        .ok_or_else(|| "Document is empty".to_string())?;
    let mut start = doc;
    if !looks_like_full_html(html) {
        let child_named = |name: &str| {
            doc.children()
                .filter(|c| c.value().as_element().is_some_and(|e| e.name() == name))
                .collect::<Vec<_>>()
        };
        let bodies = child_named("body");
        let heads = child_named("head");
        if heads.is_empty() {
            if let Some(body) = bodies.first() {
                let kids: Vec<_> = body.children().filter(|c| !c.value().is_text()).collect();
                let lead_blank = body
                    .children()
                    .take_while(|c| c.value().is_text())
                    .all(|c| c.value().as_text().is_none_or(|t| py_strip(t).is_empty()));
                let trail_blank = kids.last().is_none_or(|last| {
                    last.next_siblings()
                        .all(|c| c.value().as_text().is_none_or(|t| py_strip(t).is_empty()))
                });
                start = if kids.len() == 1 && lead_blank && trail_blank {
                    kids[0]
                } else {
                    *body
                };
            }
        }
    }
    let (mut nodes, mut max_depth) = (0i64, 0i64);
    let mut stack = vec![(start, 0i64)];
    while let Some((node, depth)) = stack.pop() {
        if node.value().is_element() {
            nodes += 1;
            max_depth = max_depth.max(depth);
        }
        for child in node.children() {
            stack.push((child, depth + 1));
        }
    }
    Ok((max_depth, nodes))
}

// -- stored HTML --------------------------------------------------------------

/// Stored-HTML files keyed by lower-cased relative path and by basename.
pub fn build_html_index(html_dir: &Path) -> HashMap<String, PathBuf> {
    let mut index: HashMap<String, PathBuf> = HashMap::new();
    let mut stack = vec![html_dir.to_path_buf()];
    let mut files: Vec<PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                files.push(p);
            }
        }
    }
    files.sort();
    for full in files {
        let name = full
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !(name.ends_with(".html") || name.ends_with(".htm")) {
            continue;
        }
        let rel = full
            .strip_prefix(html_dir)
            .unwrap_or(&full)
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/")
            .to_lowercase();
        index.entry(rel.clone()).or_insert_with(|| full.clone());
        index
            .entry(rel.trim_start_matches('/').to_string())
            .or_insert_with(|| full.clone());
        index.entry(name).or_insert(full);
    }
    index
}

/// The stored file for a URL: host+path, then path, then basename.
pub fn match_html_file<'a>(index: &'a HashMap<String, PathBuf>, url: &str) -> Option<&'a PathBuf> {
    let parsed = spiderhead_parse::py_url::urlsplit(url).ok()?;
    let host = parsed.netloc.to_lowercase();
    let path = parsed.path.clone();
    let base = {
        let b = path.rsplit('/').next().unwrap_or("");
        if b.is_empty() {
            "index.html".to_string()
        } else {
            b.to_lowercase()
        }
    };
    let mut candidates: Vec<String> = Vec::new();
    if !host.is_empty() && !path.is_empty() {
        let hp = format!("{host}{path}").to_lowercase();
        candidates.push(hp.clone());
        candidates.push(format!("{}/index.html", hp.trim_end_matches('/')));
    }
    if !path.is_empty() {
        candidates.push(path.trim_start_matches('/').to_lowercase());
    }
    candidates.push(base);
    candidates.iter().find_map(|k| index.get(k))
}

/// Bounded reader over the retained scan document or `input.html_store_dir`.
struct StoredHtml {
    index: Option<HashMap<String, PathBuf>>,
}

/// Why a page body could not be read: a per-page reason, or a hard failure.
enum ReadFail {
    Page(String),
    Hard(ContextError),
}

impl StoredHtml {
    fn new(ctx: &AuditContext) -> Self {
        let dir = ctx
            .config
            .get("input")
            .and_then(|i| i.get("html_store_dir"))
            .filter(|d| truthy(d))
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|d| d.is_dir());
        Self {
            index: dir.map(|d| build_html_index(&d)),
        }
    }

    fn read(&self, ctx: &AuditContext, url: &str) -> Result<String, ReadFail> {
        if let Some(con) = &ctx.scan_con {
            return read_page_document(con, url, MAX_DOCUMENT_BYTES).map_err(|e| match e {
                StoreError::ScanError(s) => ReadFail::Page(s.message),
                other => ReadFail::Hard(ContextError::Value(other.to_string())),
            });
        }
        let index = self.index.as_ref().ok_or_else(|| {
            ReadFail::Page(
                "no stored HTML (input.html_store_dir not set; crawl was not stored)".into(),
            )
        })?;
        let path = match_html_file(index, url).ok_or_else(|| {
            ReadFail::Page("stored HTML present but no files mapped to crawled URLs".into())
        })?;
        let mut raw = Vec::new();
        fs::File::open(path)
            .and_then(|f| f.take(MAX_DOCUMENT_BYTES as u64 + 1).read_to_end(&mut raw))
            .map_err(|e| ReadFail::Page(e.to_string()))?;
        if raw.len() as i64 > MAX_DOCUMENT_BYTES {
            return Err(ReadFail::Page(
                "stored HTML exceeds decoded byte limit".into(),
            ));
        }
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }
}

fn body_coverage(matched: usize, total: usize, omitted: &BTreeMap<String, i64>) -> Option<String> {
    if omitted.is_empty() {
        return None;
    }
    let reasons: Vec<String> = omitted.iter().map(|(r, c)| format!("{r} ({c})")).collect();
    Some(format!(
        "stored HTML matched only {matched} of {total} indexable pages; {}",
        reasons.join("; ")
    ))
}

/// DOM_TOO_DEEP and DOM_TOO_MANY_NODES from the stored page documents.
pub fn check_dom(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let on = |ctx: &AuditContext, id: &str| truthy(&ctx.enabled(id));
    if !(on(ctx, "DOM_TOO_DEEP") || on(ctx, "DOM_TOO_MANY_NODES")) {
        return Ok(());
    }
    let reader = StoredHtml::new(ctx);
    let max_depth = ctx
        .thresholds
        .get("dom_depth_max")
        .cloned()
        .unwrap_or(Value::Null);
    let max_nodes = ctx
        .thresholds
        .get("dom_nodes_max")
        .cloned()
        .unwrap_or(Value::Null);
    let (depth_cap, nodes_cap) = (
        threshold(ctx, "dom_depth_max")?,
        threshold(ctx, "dom_nodes_max")?,
    );
    let pages = ctx.indexable_html_pages();
    let total = pages.len();
    let mut omitted: BTreeMap<String, i64> = BTreeMap::new();
    let mut matched = 0usize;
    for page in &pages {
        let url = page.url();
        let html = match reader.read(ctx, &url) {
            Ok(h) => h,
            Err(ReadFail::Page(m)) => {
                *omitted.entry(m).or_insert(0) += 1;
                continue;
            }
            Err(ReadFail::Hard(e)) => return Err(e),
        };
        match dom_metrics(&html) {
            Ok((depth, nodes)) => {
                matched += 1;
                ctx.mutate_page(&url, |p| {
                    p.metrics.insert("dom_depth".into(), json!(depth));
                    p.metrics.insert("dom_nodes".into(), json!(nodes));
                });
            }
            Err(m) => *omitted.entry(m).or_insert(0) += 1,
        }
    }
    let note = body_coverage(matched, total, &omitted);
    for page in &pages {
        let url = page.url();
        let (depth, nodes) = ctx
            .page_by_url(&url)
            .map(|h| {
                h.map(|p| {
                    (
                        num(p.metrics.get("dom_depth")),
                        num(p.metrics.get("dom_nodes")),
                    )
                })
            })
            .unwrap_or((None, None));
        let (Some(depth), Some(nodes)) = (depth, nodes) else {
            continue;
        };
        if depth > depth_cap {
            let mut d = Map::new();
            d.insert("dom_depth".into(), json!(depth as i64));
            d.insert("max".into(), max_depth.clone());
            if let Some(n) = &note {
                d.insert("html_coverage".into(), json!(n));
            }
            ctx.add(
                "DOM_TOO_DEEP",
                AddOpts {
                    target_url: Some(url.clone()),
                    details: d,
                    ..AddOpts::default()
                },
            )?;
        }
        if nodes > nodes_cap {
            let mut d = Map::new();
            d.insert("dom_nodes".into(), json!(nodes as i64));
            d.insert("max".into(), max_nodes.clone());
            if let Some(n) = &note {
                d.insert("html_coverage".into(), json!(n));
            }
            ctx.add(
                "DOM_TOO_MANY_NODES",
                AddOpts {
                    target_url: Some(url.clone()),
                    details: d,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    if let Some(n) = note {
        let reason = format!("{n} — DOM size not assessed for the rest");
        ctx.skip("DOM_TOO_DEEP", &reason);
        ctx.skip("DOM_TOO_MANY_NODES", &reason);
    } else if total == 0 {
        ctx.skip("DOM_TOO_DEEP", "no indexable HTML pages");
        ctx.skip("DOM_TOO_MANY_NODES", "no indexable HTML pages");
    }
    Ok(())
}

// -- near-duplicate content -------------------------------------------------

fn content_area_config(ctx: &AuditContext) -> Option<ContentAreaConfig> {
    let v = ctx.config.get("content_area")?;
    if v.as_object().is_none_or(|o| o.is_empty()) {
        return None;
    }
    serde_json::from_value(v.clone()).ok()
}

/// NEAR_DUPLICATE and DUPLICATE_BY_HASH from the content-area text of stored
/// documents, filling only the half the export did not already answer.
pub fn check_content_duplication(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let on = |ctx: &AuditContext, id: &str| truthy(&ctx.enabled(id));
    if !(on(ctx, "NEAR_DUPLICATE") || on(ctx, "DUPLICATE_BY_HASH")) {
        return Ok(());
    }
    let has_col = |ctx: &AuditContext, names: &[&str]| {
        ctx.internal_df
            .as_ref()
            .is_some_and(|df| find_column(df.columns(), names).is_some_and(|c| !c.is_empty()))
    };
    let has_near = has_col(ctx, &["No. Near Duplicates"]);
    let has_hash = has_col(ctx, &["Hash", "Page Hash"]);
    if has_near {
        ctx.skip(
            "NEAR_DUPLICATE",
            "SF native Near Duplicates column already covers this",
        );
    }
    if has_hash {
        ctx.skip(
            "DUPLICATE_BY_HASH",
            "SF native Hash column already covers this",
        );
    }
    if has_near && has_hash {
        return Ok(());
    }

    let reader = StoredHtml::new(ctx);
    let content_cfg = content_area_config(ctx);
    let candidates = ctx.indexable_html_pages();
    let total = candidates.len();
    let mut omitted: BTreeMap<String, i64> = BTreeMap::new();
    let mut items: Vec<DuplicateItem> = Vec::new();
    let (mut input_bytes, mut visited) = (0i64, 0usize);
    for page in &candidates {
        if items.len() as i64 >= MAX_CORPUS_DOCUMENTS {
            *omitted
                .entry("duplicate document limit exceeded".into())
                .or_insert(0) += (total - visited) as i64;
            break;
        }
        visited += 1;
        let url = page.url();
        let html = match reader.read(ctx, &url) {
            Ok(h) => h,
            Err(ReadFail::Page(m)) => {
                *omitted.entry(m).or_insert(0) += 1;
                continue;
            }
            Err(ReadFail::Hard(e)) => return Err(e),
        };
        let soup = parse_document(&html);
        let area = resolve_content_area(&soup, content_cfg.as_ref())
            .map_err(|e| ContextError::Value(e.to_string()))?;
        let text = extract_area_text(&area);
        if py_strip(&text).is_empty() {
            *omitted
                .entry("no comparable content-area text".into())
                .or_insert(0) += 1;
            continue;
        }
        let size = text.len() as i64;
        if input_bytes + size > MAX_CORPUS_INPUT_BYTES {
            *omitted
                .entry("duplicate input-byte budget exceeded".into())
                .or_insert(0) += (total - visited + 1) as i64;
            break;
        }
        input_bytes += size;
        items.push(DuplicateItem {
            id: url,
            text,
            indexable: true,
        });
    }

    let note = body_coverage(items.len(), total, &omitted);
    if items.len() < 2 {
        let reason =
            note.unwrap_or_else(|| "fewer than two comparable indexable page bodies".into());
        if !has_near {
            ctx.skip("NEAR_DUPLICATE", &reason);
        }
        if !has_hash {
            ctx.skip("DUPLICATE_BY_HASH", &reason);
        }
        return Ok(());
    }

    let near_threshold = ctx
        .thresholds
        .get("near_duplicate_similarity")
        .and_then(Value::as_f64)
        .unwrap_or(0.92);
    let options = DuplicateOptions {
        threshold: near_threshold,
        max_shingles: Some(MAX_DUPLICATE_SHINGLES as usize),
        max_candidate_comparisons: Some(MAX_DUPLICATE_CANDIDATE_COMPARISONS as usize),
        ..DuplicateOptions::default()
    };
    let (clusters, exact) = match find_duplicates(&items, &options) {
        Ok(report) => (
            report
                .clusters
                .into_iter()
                .map(|c| (c.members, c.min_similarity))
                .collect::<Vec<_>>(),
            report
                .exact_duplicates
                .into_iter()
                .map(|g| (g.hash, g.members))
                .collect::<Vec<_>>(),
        ),
        Err(ParseError::BudgetExceeded(message)) => {
            if !has_near {
                ctx.skip("NEAR_DUPLICATE", &message);
            }
            // Exact hashes need no shingle budget, so their measurement survives.
            let mut groups: Vec<(String, Vec<String>)> = Vec::new();
            for item in &items {
                let h = content_hash(&item.text);
                match groups.iter_mut().find(|(k, _)| *k == h) {
                    Some((_, m)) => m.push(item.id.clone()),
                    None => groups.push((h, vec![item.id.clone()])),
                }
            }
            (
                Vec::new(),
                groups.into_iter().filter(|(_, m)| m.len() > 1).collect(),
            )
        }
        Err(e) => return Err(ContextError::Value(e.to_string())),
    };

    let status_of = |ctx: &mut AuditContext, url: &str| -> Value {
        ctx.page_by_url(url)
            .and_then(|h| h.map(|p| p.status_code))
            .map_or(Value::Null, |c| json!(c))
    };
    if !has_near {
        for (members, min_similarity) in clusters {
            let mut members = members;
            members.sort();
            let Some(group) = ctx.add_group("NEAR_DUPLICATE", None, members.clone())? else {
                continue;
            };
            for url in &members {
                let other = members
                    .iter()
                    .find(|m| *m != url)
                    .cloned()
                    .unwrap_or_default();
                let mut d = Map::new();
                d.insert("cluster_min_similarity".into(), json!(min_similarity));
                d.insert("cluster_size".into(), json!(members.len()));
                d.insert("comparison_url".into(), json!(other));
                d.insert("status_code".into(), status_of(ctx, url));
                d.insert("method".into(), json!("content_area_simhash_lsh"));
                if let Some(n) = &note {
                    d.insert("html_coverage".into(), json!(n));
                }
                ctx.add(
                    "NEAR_DUPLICATE",
                    AddOpts {
                        target_url: Some(url.clone()),
                        group_id: Some(group.group_id.clone()),
                        details: d,
                        ..AddOpts::default()
                    },
                )?;
            }
        }
        if let Some(n) = &note {
            ctx.skip(
                "NEAR_DUPLICATE",
                &format!("{n} — duplicates not assessed for the rest"),
            );
        }
    }
    if !has_hash {
        for (hash, members) in exact {
            let mut members = members;
            members.sort();
            let Some(group) = ctx.add_group("DUPLICATE_BY_HASH", None, members.clone())? else {
                continue;
            };
            for url in &members {
                let other = members
                    .iter()
                    .find(|m| *m != url)
                    .cloned()
                    .unwrap_or_default();
                let mut d = Map::new();
                d.insert("duplicate_count".into(), json!(members.len()));
                d.insert("comparison_url".into(), json!(other));
                d.insert("status_code".into(), status_of(ctx, url));
                d.insert("content_hash".into(), json!(hash));
                d.insert("method".into(), json!("content_area_text_hash"));
                if let Some(n) = &note {
                    d.insert("html_coverage".into(), json!(n));
                }
                ctx.add(
                    "DUPLICATE_BY_HASH",
                    AddOpts {
                        target_url: Some(url.clone()),
                        group_id: Some(group.group_id.clone()),
                        details: d,
                        ..AddOpts::default()
                    },
                )?;
            }
        }
        if let Some(n) = &note {
            ctx.skip(
                "DUPLICATE_BY_HASH",
                &format!("{n} — duplicates not assessed for the rest"),
            );
        }
    }
    Ok(())
}

// -- templated titles ---------------------------------------------------------

/// Counts in first-seen order; `most_common` returns the first of the maxima.
#[derive(Default)]
struct Tally(Vec<(String, i64)>);

impl Tally {
    fn bump(&mut self, key: String) {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some((_, c)) => *c += 1,
            None => self.0.push((key, 1)),
        }
    }
    fn top(&self) -> Option<(&str, i64)> {
        let mut best: Option<(&str, i64)> = None;
        for (k, c) in &self.0 {
            if best.is_none_or(|(_, bc)| *c > bc) {
                best = Some((k, *c));
            }
        }
        best
    }
}

/// TITLE_TEMPLATED: one prefix or suffix shared by most page titles.
pub fn check_templated_titles(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let titles: Vec<String> = ctx
        .indexable_html_pages()
        .iter()
        .filter_map(|p| p.map(|p| p.metrics.get("title").cloned()))
        .filter(truthy)
        .map(|t| py_strip(&crate::pyvalue::str_or_dumps(&t)).to_owned())
        .collect();
    if titles.len() < 5 {
        ctx.skip("TITLE_TEMPLATED", "too few titles to assess templating");
        return Ok(());
    }
    let (mut prefixes, mut suffixes) = (Tally::default(), Tally::default());
    for title in &titles {
        for sep in SEPARATORS {
            if title.contains(sep) {
                let head = title.split_once(sep).map_or("", |(h, _)| h);
                let tail = title.rsplit_once(sep).map_or("", |(_, t)| t);
                prefixes.bump(py_strip(head).to_owned());
                suffixes.bump(py_strip(tail).to_owned());
                break;
            }
        }
    }
    let mut best: Option<(&str, String, i64)> = None;
    for (direction, tally) in [("prefix", &prefixes), ("suffix", &suffixes)] {
        let Some((token, count)) = tally.top() else {
            continue;
        };
        if best.as_ref().is_none_or(|b| count > b.2) {
            best = Some((direction, token.to_owned(), count));
        }
    }
    let Some((direction, token, count)) = best else {
        return Ok(());
    };
    let share = count as f64 / titles.len() as f64;
    if share >= threshold(ctx, "templated_title_share")? {
        let mut d = Map::new();
        d.insert("direction".into(), json!(direction));
        d.insert("token".into(), json!(token));
        d.insert("share".into(), json!(round_to(share, 2)));
        d.insert("pages".into(), json!(count));
        d.insert("total".into(), json!(titles.len()));
        ctx.add(
            "TITLE_TEMPLATED",
            AddOpts {
                details: d,
                ..AddOpts::default()
            },
        )?;
    }
    Ok(())
}
