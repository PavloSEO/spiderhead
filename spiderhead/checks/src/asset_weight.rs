//! CSS/JS weight and delivery analysis for one page.
//!
//! Fetches a page, discovers its linked stylesheets and scripts, fetches each
//! of them, and reports the checks answerable from bytes on the wire and
//! static markup — no rendering required: minification, render-blocking head
//! resources, oversized files, duplicate libraries (by content hash), cache
//! lifetime, compression, missing `font-display`, legacy transpiled JS,
//! fetchable source maps, debug code, `document.write`, and `@import` chains.
//! Two checks are deliberately reported under `skipped` rather than attempted:
//! unused CSS/JS (needs a rendered DOM) and per-site outliers (needs more than
//! one page; see [`flag_outlier_pages`]).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use spiderhead_parse::asset_evidence::{
    check_cache_lifetime, find_missing_font_display, find_render_blocking_resources,
    looks_legacy_transpiled, looks_minified,
};
use spiderhead_parse::dom::{find_all, parse_document, Soup};
use spiderhead_parse::models::ResourceKind;
use spiderhead_parse::parser::extract::extract_resource_declarations;
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};
use spiderhead_parse::py_str::py_strip;
use spiderhead_parse::urlutil::urljoin;

/// Default per-request timeout the real handler builds its client with.
pub const DEFAULT_TIMEOUT: f64 = 15.0;
/// Default per-file oversize threshold.
pub const DEFAULT_FILE_SIZE_THRESHOLD: i64 = 500_000;
/// Bound on one page's own fetch fan-out: a page linking hundreds of resources
/// (often third-party trackers, not the site's delivery problem) must not turn
/// one audit call into an unbounded crawl.
pub const MAX_RESOURCES: usize = 60;
/// A CSS file importing more than this many other stylesheets is almost always
/// a build mistake, not a deliberate chain worth reporting one round trip at a
/// time — bounds the follow-up fetches the same way [`MAX_RESOURCES`] bounds
/// the initial discovery.
pub const MAX_IMPORTS_PER_FILE: usize = 10;

/// `Content-Encoding` values that count as "served compressed".
const COMPRESSED_ENCODINGS: [&str; 4] = ["gzip", "br", "deflate", "zstd"];

/// Captures a `sourceMappingURL` target in both the JS (`//# …`) and CSS
/// (`/*# … */`) comment forms; stopping at whitespace or `*` keeps the CSS
/// block comment's closing `*/` out of the capture.
const SOURCE_MAP_PATTERN: &str = r"sourceMappingURL=([^\s\x1c-\x1f*]+)";
/// Debug markers meaningful only in otherwise-minified bundles.
const DEBUG_CODE_PATTERN: &str = r"\bconsole\.(?:log|debug)\s*\(|\bdebugger\b|\balert\s*\(";
/// The parser-blocking `document.write` call (not `document.writeln`).
const DOCUMENT_WRITE_PATTERN: &str = r"\bdocument\.write\s*\(";
/// Covers `@import "x.css"`, `@import url(x.css)` and `@import url("x.css")`,
/// with or without a trailing media query, which the capture group ignores.
const CSS_IMPORT_PATTERN: &str =
    r#"(?i)@import[\s\x1c-\x1f]+(?:url\([\s\x1c-\x1f]*)?['"]?([^'"()\[\s\x1c-\x1f;]+)['"]?\)?"#;
/// Runs of whitespace removed before hashing for duplicate detection.
const WHITESPACE_PATTERN: &str = r"[\s\x1c-\x1f]+";

static SOURCE_MAP: OnceLock<Option<Regex>> = OnceLock::new();
static DEBUG_CODE: OnceLock<Option<Regex>> = OnceLock::new();
static DOCUMENT_WRITE: OnceLock<Option<Regex>> = OnceLock::new();
static CSS_IMPORT: OnceLock<Option<Regex>> = OnceLock::new();
static WHITESPACE: OnceLock<Option<Regex>> = OnceLock::new();

/// Compiled pattern, built once; a pattern that failed to compile matches nothing.
fn compiled(cell: &'static OnceLock<Option<Regex>>, pattern: &str) -> Option<&'static Regex> {
    cell.get_or_init(|| Regex::new(pattern).ok()).as_ref()
}

/// One fetched reply as the analyzer needs it: status, final URL, decoded
/// text, raw bytes and headers — the surface Python's injected fetcher
/// responses carry (`status_code`, `url`, `content`, `text`, `headers`).
#[derive(Debug, Clone)]
pub struct AssetReply {
    /// HTTP status code.
    pub status_code: u16,
    /// Final URL after redirects; `None` keeps the requested URL.
    pub url: Option<String>,
    /// Raw body bytes; `None` falls back to the text length for sizing.
    pub content: Option<Vec<u8>>,
    /// Decoded body text.
    pub text: String,
    /// Response headers as `(name, value)` pairs, looked up case-insensitively.
    pub headers: Vec<(String, String)>,
}

impl AssetReply {
    /// First value of a header, case-insensitive on the name (httpx's
    /// `CaseInsensitiveDict.get`).
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Decoded byte size: what the browser parses and executes, which is what
    /// a minification/bloat check cares about, not the compressed wire size.
    fn body_size(&self) -> i64 {
        match &self.content {
            Some(bytes) => bytes.len() as i64,
            None => self.text.len() as i64,
        }
    }
}

/// Fetch seam replacing the network client, so tests exercise the whole
/// pipeline without a socket. The default [`AssetFetcher::resolves`] probes
/// through `get`; the live adapter overrides it with a HEAD (falling back to
/// GET) because it only needs a status code, never the map itself.
pub trait AssetFetcher {
    /// GET `url`; `Err` is the transport-failure text.
    fn get(&self, url: &str) -> Result<AssetReply, String>;

    /// Whether `url` resolves at all (the source-map exposure probe).
    fn resolves(&self, url: &str) -> bool {
        self.get(url)
            .map(|reply| reply.status_code < 400)
            .unwrap_or(false)
    }
}

/// One fetched resource with its delivery verdicts, before serialization.
#[derive(Debug, Clone)]
pub struct FetchedResource {
    /// Resolved URL as discovered.
    pub url: String,
    /// `"css"` or `"js"`.
    pub kind: &'static str,
    /// Status was below 400.
    pub ok: bool,
    /// HTTP status code (0 when the fetch itself failed).
    pub status_code: u16,
    /// Decoded byte size.
    pub bytes: i64,
    /// Decoded body text.
    pub text: String,
    /// `Cache-Control` header value, when sent.
    pub cache_control: Option<String>,
    /// `Content-Encoding` header value, when sent.
    pub content_encoding: Option<String>,
    /// Transport-failure text; `Some` only when the fetch itself failed.
    pub error: Option<String>,
}

impl FetchedResource {
    /// The wire document: the `{url, kind, ok, error}` shape on a transport
    /// failure, the full delivery shape otherwise — including `ok: false`
    /// HTTP failures, which keep their status, text and headers.
    pub fn to_json(&self) -> Value {
        match &self.error {
            Some(error) => json!({
                "url": self.url,
                "kind": self.kind,
                "ok": false,
                "error": error,
            }),
            None => json!({
                "url": self.url,
                "kind": self.kind,
                "ok": self.ok,
                "status_code": self.status_code,
                "bytes": self.bytes,
                "text": self.text,
                "cache_control": self.cache_control,
                "content_encoding": self.content_encoding,
            }),
        }
    }
}

/// Tunables of [`analyze_page_asset_weight`] other than the fetch seam.
#[derive(Debug, Clone)]
pub struct AssetWeightOptions {
    /// Per-file oversize threshold in bytes.
    pub file_size_threshold: i64,
    /// Fetch fan-out cap; discovery beyond it is reported as truncated.
    pub max_resources: usize,
}

impl Default for AssetWeightOptions {
    fn default() -> Self {
        Self {
            file_size_threshold: DEFAULT_FILE_SIZE_THRESHOLD,
            max_resources: MAX_RESOURCES,
        }
    }
}

/// SHA-256 of `text` with all whitespace removed.
///
/// Whitespace-only reformatting (a different line-wrap width, a trailing
/// newline) must not hide that two files bundle the same library, and must
/// not manufacture a "duplicate" out of two files that only look alike.
pub fn content_hash(text: &str) -> String {
    let normalized = compiled(&WHITESPACE, WHITESPACE_PATTERN)
        .map(|regex| regex.replace_all(text, "").into_owned())
        .unwrap_or_else(|| text.to_owned());
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Group fetched resources of the same kind by content hash.
///
/// A group is reported only when it spans more than one distinct URL —
/// the same file linked twice is not a duplicate library, it is one file.
pub fn find_duplicate_libraries(resources: &[FetchedResource]) -> Vec<Value> {
    let mut by_key: BTreeMap<(&str, String), BTreeSet<String>> = BTreeMap::new();
    for resource in resources {
        if !resource.ok || resource.text.is_empty() {
            continue;
        }
        by_key
            .entry((resource.kind, content_hash(&resource.text)))
            .or_default()
            .insert(resource.url.clone());
    }
    by_key
        .into_iter()
        .filter(|(_, urls)| urls.len() > 1)
        .map(|((kind, digest), urls)| {
            json!({ "kind": kind, "hash": digest, "urls": urls.into_iter().collect::<Vec<_>>() })
        })
        .collect()
}

/// The `sourceMappingURL` target referenced by `text`, or `None`.
///
/// Only the comment is read here — whether the target is actually fetchable
/// is a network question the caller answers separately. A `data:` URI is
/// an inline map with nothing served over the network, so it is not
/// "exposed" and is excluded. When a file carries more than one such
/// comment (rebuilt without stripping the old one), the last one wins, since
/// that is the one a real browser would act on.
pub fn find_source_map_comment(text: &str) -> Option<String> {
    let regex = compiled(&SOURCE_MAP, SOURCE_MAP_PATTERN)?;
    let target = regex
        .captures_iter(text)
        .last()?
        .get(1)?
        .as_str()
        .to_owned();
    (!target.to_lowercase().starts_with("data:")).then_some(target)
}

/// Debug markers (`console.log`/`debug`, `debugger`, `alert(`) in `js_text`.
///
/// Meaningful only in a file that is otherwise minified: a `debugger`
/// statement halts execution for anyone with devtools open, and a
/// `console` call in a hot path costs real main-thread time — but the same
/// calls in hand-authored source are just normal development noise, so an
/// unminified file is never flagged here.
pub fn find_debug_code(js_text: &str) -> Vec<String> {
    if !looks_minified(js_text) {
        return Vec::new();
    }
    let Some(regex) = compiled(&DEBUG_CODE, DEBUG_CODE_PATTERN) else {
        return Vec::new();
    };
    regex
        .find_iter(js_text)
        .map(|found| found.as_str().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether `js_text` calls `document.write`.
///
/// `document.write` blocks the HTML parser while it runs, and Chrome
/// ignores it outright for scripts injected into a page loaded over a slow
/// connection — so the call either stalls rendering or silently does
/// nothing.
pub fn has_document_write(js_text: &str) -> bool {
    compiled(&DOCUMENT_WRITE, DOCUMENT_WRITE_PATTERN).is_some_and(|regex| regex.is_match(js_text))
}

/// Raw `@import` targets referenced by `css_text`, in source order, capped at
/// [`MAX_IMPORTS_PER_FILE`].
pub fn find_css_imports(css_text: &str) -> Vec<String> {
    let Some(regex) = compiled(&CSS_IMPORT, CSS_IMPORT_PATTERN) else {
        return Vec::new();
    };
    regex
        .captures_iter(css_text)
        .filter_map(|captures| captures.get(1).map(|target| target.as_str().to_owned()))
        .take(MAX_IMPORTS_PER_FILE)
        .collect()
}

/// Whether a resource was served compressed: `(ok, encoding)`, where a
/// missing header yields `(false, None)`.
pub fn check_compression(content_encoding: Option<&str>) -> (bool, Option<String>) {
    let value = py_strip(content_encoding.unwrap_or("")).to_lowercase();
    let ok = COMPRESSED_ENCODINGS.contains(&value.as_str());
    let encoding = (!value.is_empty()).then_some(value);
    (ok, encoding)
}

/// Pages whose total CSS+JS payload dwarfs the site's median.
///
/// `min_bytes` guards a templated site of near-identical pages from having
/// every page above the median called an outlier over a few stray bytes.
pub fn flag_outlier_pages(
    page_totals: &BTreeMap<String, i64>,
    multiple: f64,
    min_bytes: i64,
) -> Vec<String> {
    let mut sizes: Vec<f64> = page_totals.values().map(|&total| total as f64).collect();
    if sizes.len() < 2 {
        return Vec::new();
    }
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = sizes.len() / 2;
    let median = if sizes.len() % 2 == 1 {
        sizes[middle]
    } else {
        (sizes[middle - 1] + sizes[middle]) / 2.0
    };
    let baseline = if median == 0.0 { 1.0 } else { median };
    page_totals
        .iter()
        .filter(|(_, &total)| {
            total as f64 > baseline * multiple && total as f64 - baseline > min_bytes as f64
        })
        .map(|(url, _)| url.clone())
        .collect()
}

/// Direct `<link rel=stylesheet>` then `<script src>` URLs, deduplicated.
///
/// Stylesheets come first so the fetch fan-out cap keeps the earliest
/// stylesheets when a page declares more resources than it may fetch.
/// `<template>` descendants are already excluded by discovery: a stylesheet
/// or script held only in a DocumentFragment is never requested by a browser,
/// so counting it would fabricate bytes, minification, cache, and
/// duplicate-library findings for a resource nothing ever fetches.
fn discover_resources(soup: &Soup, base_url: &str) -> Result<Vec<(String, &'static str)>, String> {
    let (declarations, _omitted) = extract_resource_declarations(soup, base_url, None);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (kind, output) in [
        (ResourceKind::Stylesheet, "css"),
        (ResourceKind::Script, "js"),
    ] {
        for declaration in &declarations {
            if declaration.kind != kind || !seen.insert(declaration.url.clone()) {
                continue;
            }
            out.push((declaration.url.clone(), output));
        }
    }
    Ok(out)
}

/// Fetch one discovered target; a transport failure becomes an error-shaped
/// resource instead of aborting the page.
fn fetch_one(target: &(String, &'static str), fetcher: &dyn AssetFetcher) -> FetchedResource {
    let (url, kind) = (&target.0, target.1);
    let reply = match fetcher.get(url) {
        Ok(reply) => reply,
        Err(error) => {
            return FetchedResource {
                url: url.clone(),
                kind,
                ok: false,
                status_code: 0,
                bytes: 0,
                text: String::new(),
                cache_control: None,
                content_encoding: None,
                error: Some(error),
            };
        }
    };
    FetchedResource {
        url: url.clone(),
        kind,
        ok: reply.status_code < 400,
        status_code: reply.status_code,
        bytes: reply.body_size(),
        text: reply.text.clone(),
        cache_control: reply.header("cache-control").map(str::to_owned),
        content_encoding: reply.header("content-encoding").map(str::to_owned),
        error: None,
    }
}

/// Body text of `url`, or `None` on any failure.
///
/// Used for one-level `@import` follow-up, where (unlike a source-map
/// probe) the imported file's own content must be inspected.
fn fetch_text(url: &str, fetcher: &dyn AssetFetcher) -> Option<String> {
    let reply = fetcher.get(url).ok()?;
    (reply.status_code < 400).then_some(reply.text)
}

/// The two checks this static-fetch analysis deliberately does not attempt,
/// reported so silence never reads as "clean".
fn skipped_checks() -> Vec<Value> {
    vec![
        json!({
            "check": "unused_css_js",
            "reason": "needs a rendered DOM to tell loaded from used (tracked in #18)",
        }),
        json!({
            "check": "site_median_outlier",
            "reason":
                "needs more than one page; call flag_outlier_pages() with each page's total_bytes",
        }),
    ]
}

/// Fetch `url` and its linked CSS/JS, and run every static-analysis check.
///
/// A page-level failure short-circuits to `{"ok": false, …}`; every other
/// outcome is a full report document, including one where individual
/// resources failed to fetch.
pub fn analyze_page_asset_weight(
    url: &str,
    fetcher: &dyn AssetFetcher,
    options: AssetWeightOptions,
) -> Result<Value, String> {
    let page = match fetcher.get(url) {
        Ok(page) => page,
        Err(error) => return Ok(json!({ "ok": false, "url": url, "error": error })),
    };
    if page.status_code >= 400 {
        return Ok(json!({ "ok": false, "url": url, "status_code": page.status_code }));
    }

    let final_url = page
        .url
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(url)
        .to_owned();
    let soup = parse_document(&page.text);
    let base_url = document_base_url(DocumentSource::Tree(&soup), &final_url);

    let render_blocking =
        find_render_blocking_resources(&soup, &base_url).map_err(|error| error.to_string())?;
    let mut targets = discover_resources(&soup, &base_url)?;
    let truncated = targets.len() > options.max_resources;
    targets.truncate(options.max_resources);

    let resources: Vec<FetchedResource> = targets
        .iter()
        .map(|target| fetch_one(target, fetcher))
        .collect();

    // A source map is only a real exposure once its target is confirmed
    // fetchable — the comment alone proves nothing about production.
    let mut exposed_source_maps = Vec::new();
    for resource in &resources {
        if !resource.ok {
            continue;
        }
        let Some(comment) = find_source_map_comment(&resource.text) else {
            continue;
        };
        let map_url = urljoin(&resource.url, &comment).map_err(|error| error.to_string())?;
        if fetcher.resolves(&map_url) {
            exposed_source_maps.push(json!({ "source": resource.url, "map_url": map_url }));
        }
    }

    // One level of @import follow-up: fetch what the stylesheet imports,
    // and check only that file (not its own imports) for further chaining.
    let mut import_chains = Vec::new();
    for resource in &resources {
        if !resource.ok || resource.kind != "css" {
            continue;
        }
        for target in find_css_imports(&resource.text) {
            if target.to_lowercase().starts_with("data:") {
                continue;
            }
            let import_url = urljoin(&resource.url, &target).map_err(|error| error.to_string())?;
            let depth = match fetch_text(&import_url, fetcher) {
                Some(imported_text) if !find_css_imports(&imported_text).is_empty() => 2,
                _ => 1,
            };
            import_chains
                .push(json!({ "source": resource.url, "import_url": import_url, "depth": depth }));
        }
    }

    let oversized: Vec<Value> = resources
        .iter()
        .filter(|resource| resource.ok && resource.bytes > options.file_size_threshold)
        .map(|resource| {
            json!({
                "url": resource.url,
                "bytes": resource.bytes,
                "threshold": options.file_size_threshold,
            })
        })
        .collect();
    let duplicates = find_duplicate_libraries(&resources);

    let mut unminified = Vec::new();
    let mut missing_font_display = Vec::new();
    let mut legacy_js = Vec::new();
    let mut cache_findings = Vec::new();
    let mut compression_findings = Vec::new();
    let mut debug_code = Vec::new();
    let mut document_write = Vec::new();
    for resource in &resources {
        if !resource.ok {
            continue;
        }
        if !looks_minified(&resource.text) {
            unminified.push(resource.url.clone());
        }
        if resource.kind == "css" {
            for finding in find_missing_font_display(&resource.text) {
                missing_font_display
                    .push(json!({ "source": resource.url, "excerpt": finding.excerpt }));
            }
        } else {
            if looks_legacy_transpiled(&resource.text) {
                legacy_js.push(resource.url.clone());
            }
            let markers = find_debug_code(&resource.text);
            if !markers.is_empty() {
                debug_code.push(json!({ "url": resource.url, "markers": markers }));
            }
            if has_document_write(&resource.text) {
                document_write.push(resource.url.clone());
            }
        }
        let cache = check_cache_lifetime(resource.cache_control.as_deref());
        if !cache.ok {
            cache_findings.push(json!({
                "url": resource.url,
                "ok": cache.ok,
                "max_age": cache.max_age,
                "reason": cache.reason,
            }));
        }
        let (compressed, encoding) = check_compression(resource.content_encoding.as_deref());
        if !compressed {
            compression_findings
                .push(json!({ "url": resource.url, "ok": compressed, "encoding": encoding }));
        }
    }

    // Inline <style> blocks carry the same font-display risk as an external
    // stylesheet, at zero fetch cost.
    for style_tag in find_all(&soup, "style") {
        let text = style_tag.text().collect::<String>();
        for finding in find_missing_font_display(&text) {
            missing_font_display.push(json!({ "source": "inline", "excerpt": finding.excerpt }));
        }
    }

    let total_bytes: i64 = resources
        .iter()
        .filter(|resource| resource.ok)
        .map(|resource| resource.bytes)
        .sum();

    let mut findings: Vec<String> = Vec::new();
    if !render_blocking.is_empty() {
        findings.push(format!(
            "{} render-blocking resource(s) in <head>",
            render_blocking.len()
        ));
    }
    if !oversized.is_empty() {
        findings.push(format!(
            "{} file(s) over the {}-byte threshold",
            oversized.len(),
            options.file_size_threshold
        ));
    }
    if !duplicates.is_empty() {
        findings.push(format!(
            "{} library bundled more than once",
            duplicates.len()
        ));
    }
    if !unminified.is_empty() {
        findings.push(format!("{} file(s) do not look minified", unminified.len()));
    }
    if !missing_font_display.is_empty() {
        findings.push(format!(
            "{} @font-face block(s) without font-display",
            missing_font_display.len()
        ));
    }
    if !legacy_js.is_empty() {
        findings.push(format!(
            "{} script(s) look like unconditional legacy/polyfill code",
            legacy_js.len()
        ));
    }
    if !cache_findings.is_empty() {
        findings.push(format!(
            "{} resource(s) without a long-lived Cache-Control",
            cache_findings.len()
        ));
    }
    if !compression_findings.is_empty() {
        findings.push(format!(
            "{} resource(s) served uncompressed",
            compression_findings.len()
        ));
    }
    if !exposed_source_maps.is_empty() {
        findings.push(format!(
            "{} source map(s) fetchable in production, exposing original source, internal paths, \
             and sometimes API endpoints",
            exposed_source_maps.len()
        ));
    }
    if !debug_code.is_empty() {
        findings.push(format!(
            "{} minified file(s) still ship debug code (console/debugger/alert) that costs \
             main-thread time or halts execution for anyone with devtools open",
            debug_code.len()
        ));
    }
    if !document_write.is_empty() {
        findings.push(format!(
            "{} script(s) call document.write, which blocks the parser and is ignored outright \
             by Chrome on a slow connection",
            document_write.len()
        ));
    }
    if !import_chains.is_empty() {
        let chained = import_chains
            .iter()
            .filter(|chain| chain["depth"].as_i64().unwrap_or(1) > 1)
            .count();
        let detail = if chained > 0 {
            format!(", {chained} at least two levels deep")
        } else {
            String::new()
        };
        findings.push(format!(
            "{} @import chain(s) each serializing a round trip before styles apply{detail}",
            import_chains.len()
        ));
    }

    Ok(json!({
        "ok": true,
        "url": url,
        "final_url": final_url,
        "resources": resources.iter().map(FetchedResource::to_json).collect::<Vec<_>>(),
        "resources_truncated": truncated,
        "total_bytes": total_bytes,
        "render_blocking": render_blocking,
        "oversized": oversized,
        "duplicate_libraries": duplicates,
        "unminified": unminified,
        "missing_font_display": missing_font_display,
        "legacy_js": legacy_js,
        "cache_findings": cache_findings,
        "compression_findings": compression_findings,
        "exposed_source_maps": exposed_source_maps,
        "debug_code": debug_code,
        "document_write": document_write,
        "css_import_chains": import_chains,
        "findings": findings,
        "skipped": skipped_checks(),
    }))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    // ── fixtures ────────────────────────────────────────────────────────────

    /// Python `str.strip()` per line, then concatenation.
    fn minify(css: &str) -> String {
        css.lines().map(py_strip).collect()
    }

    fn repeat_readable_css(rule_count: usize) -> String {
        ".header {\n    display: flex;\n    align-items: center;\n    justify-content: \
         space-between;\n    padding: 20px 40px;\n    background-color: #ffffff;\n}\n\n"
            .repeat(rule_count)
    }

    fn repeat_readable_js(block_count: usize) -> String {
        "function calculateTotal(items) {\n    console.log('debugging total calculation');\n    \
         let total = 0;\n    for (const item of items) {\n        total += item.price;\n    }\n    \
         return total;\n}\n\n"
            .repeat(block_count)
    }

    /// A canned reply with `.text` semantics: content is the UTF-8 of the text.
    #[derive(Debug, Clone)]
    struct FakeResponse {
        text: String,
        status_code: u16,
        headers: Vec<(String, String)>,
        url: Option<String>,
    }

    impl FakeResponse {
        fn new(text: &str) -> Self {
            Self {
                text: text.to_owned(),
                status_code: 200,
                headers: Vec::new(),
                url: None,
            }
        }

        fn with_headers(text: &str, headers: &[(&str, &str)]) -> Self {
            Self {
                text: text.to_owned(),
                status_code: 200,
                headers: headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                url: None,
            }
        }

        fn into_reply(self) -> AssetReply {
            AssetReply {
                status_code: self.status_code,
                url: self.url,
                content: Some(self.text.clone().into_bytes()),
                text: self.text,
                headers: self.headers,
            }
        }
    }

    /// A plain URL-map fetcher; missing URLs answer 404, never a transport
    /// error — the same default the Python dict-lookup fetcher has.
    struct MapFetcher {
        responses: HashMap<String, FakeResponse>,
    }

    impl MapFetcher {
        fn new(pairs: Vec<(&str, FakeResponse)>) -> Self {
            Self {
                responses: pairs
                    .into_iter()
                    .map(|(url, response)| (url.to_owned(), response))
                    .collect(),
            }
        }
    }

    impl AssetFetcher for MapFetcher {
        fn get(&self, url: &str) -> Result<AssetReply, String> {
            let response = self.responses.get(url).cloned().unwrap_or(FakeResponse {
                text: String::new(),
                status_code: 404,
                headers: Vec::new(),
                url: None,
            });
            Ok(response.into_reply())
        }
    }

    fn page(text: &str) -> (&str, FakeResponse) {
        ("https://example.com/", FakeResponse::new(text))
    }

    fn head_with(markup: &str) -> String {
        format!("<html><head>{markup}</head><body></body></html>")
    }

    /// `find_duplicate_libraries` input from `(url, kind, text)` triples.
    fn resources_of(triples: &[(&str, &str, &str)]) -> Vec<FetchedResource> {
        triples
            .iter()
            .map(|(url, kind, text)| FetchedResource {
                url: (*url).to_owned(),
                kind: if *kind == "css" { "css" } else { "js" },
                ok: true,
                status_code: 200,
                bytes: text.len() as i64,
                text: (*text).to_owned(),
                cache_control: None,
                content_encoding: None,
                error: None,
            })
            .collect()
    }

    // ── pure checks ─────────────────────────────────────────────────────────

    /// from tests/test_asset_weight.py::test_hand_authored_css_is_not_minified
    #[test]
    fn hand_authored_css_is_not_minified() {
        assert!(!looks_minified(&repeat_readable_css(6)));
    }

    /// from tests/test_asset_weight.py::test_minified_css_is_recognized
    #[test]
    fn minified_css_is_recognized() {
        assert!(looks_minified(&minify(&repeat_readable_css(6))));
    }

    /// from tests/test_asset_weight.py::test_tiny_files_are_classified_by_shape_not_size_alone
    #[test]
    fn tiny_files_are_classified_by_shape_not_size_alone() {
        assert!(looks_minified(".a{color:red}"));
        assert!(!looks_minified("body { color: red; }"));
    }

    /// from tests/test_asset_weight.py::test_content_hash_ignores_whitespace_reformatting
    #[test]
    fn content_hash_ignores_whitespace_reformatting() {
        assert_eq!(
            content_hash(".x { color: red; }\n\n.y { color: blue; }"),
            content_hash(".x{color:red;}.y{color:blue;}")
        );
    }

    /// from tests/test_asset_weight.py::test_content_hash_differs_for_different_content
    #[test]
    fn content_hash_differs_for_different_content() {
        assert_ne!(
            content_hash(".x{color:red}"),
            content_hash(".x{color:blue}")
        );
    }

    /// from tests/test_asset_weight.py::test_duplicate_libraries_detected_by_hash_not_filename
    #[test]
    fn duplicate_libraries_detected_by_hash_not_filename() {
        let resources = resources_of(&[
            ("https://example.com/vendor/jquery.min.js", "js", "a  b"),
            ("https://example.com/chunks/2.a1b2c3.js", "js", "a b"),
        ]);
        let dupes = find_duplicate_libraries(&resources);
        assert_eq!(dupes.len(), 1);
        assert_eq!(
            dupes[0]["urls"],
            json!([
                "https://example.com/chunks/2.a1b2c3.js",
                "https://example.com/vendor/jquery.min.js"
            ])
        );
    }

    /// from tests/test_asset_weight.py::test_no_duplicate_when_content_actually_differs
    #[test]
    fn no_duplicate_when_content_actually_differs() {
        let resources = resources_of(&[
            ("https://example.com/a.js", "js", "console.log(1)"),
            ("https://example.com/b.js", "js", "console.log(2)"),
        ]);
        assert!(find_duplicate_libraries(&resources).is_empty());
    }

    /// from tests/test_asset_weight.py::test_same_url_fetched_twice_is_not_a_duplicate
    #[test]
    fn same_url_fetched_twice_is_not_a_duplicate() {
        let resources = resources_of(&[
            ("https://example.com/a.js", "js", "x"),
            ("https://example.com/a.js", "js", "x"),
        ]);
        assert!(find_duplicate_libraries(&resources).is_empty());
    }

    /// from tests/test_asset_weight.py::test_css_and_js_with_the_same_bytes_are_not_cross_matched
    #[test]
    fn css_and_js_with_the_same_bytes_are_not_cross_matched() {
        let resources = resources_of(&[
            ("https://example.com/a.css", "css", "shared"),
            ("https://example.com/b.js", "js", "shared"),
        ]);
        assert!(find_duplicate_libraries(&resources).is_empty());
    }

    /// from tests/test_asset_weight.py::test_plain_script_in_head_is_render_blocking
    #[test]
    fn plain_script_in_head_is_render_blocking() {
        use spiderhead_parse::asset_evidence::is_render_blocking;
        assert!(is_render_blocking("script", &[]));
    }

    /// from tests/test_asset_weight.py::test_deferred_and_async_scripts_are_not_render_blocking
    #[test]
    fn deferred_and_async_scripts_are_not_render_blocking() {
        use spiderhead_parse::asset_evidence::is_render_blocking;
        let defer = [("defer".to_owned(), String::new())];
        let r#async = [("async".to_owned(), String::new())];
        assert!(!is_render_blocking("script", &defer));
        assert!(!is_render_blocking("script", &r#async));
    }

    /// from tests/test_asset_weight.py::test_module_script_is_not_render_blocking
    #[test]
    fn module_script_is_not_render_blocking() {
        use spiderhead_parse::asset_evidence::is_render_blocking;
        let attrs = [("type".to_owned(), "module".to_owned())];
        assert!(!is_render_blocking("script", &attrs));
    }

    /// from tests/test_asset_weight.py::test_data_island_script_type_is_not_render_blocking
    #[test]
    fn data_island_script_type_is_not_render_blocking() {
        use spiderhead_parse::asset_evidence::is_render_blocking;
        let attrs = [("type".to_owned(), "application/ld+json".to_owned())];
        assert!(!is_render_blocking("script", &attrs));
    }

    /// from tests/test_asset_weight.py::test_plain_stylesheet_link_is_render_blocking
    #[test]
    fn plain_stylesheet_link_is_render_blocking() {
        use spiderhead_parse::asset_evidence::is_render_blocking;
        let screen = [("media".to_owned(), "screen".to_owned())];
        assert!(is_render_blocking("link", &[]));
        assert!(is_render_blocking("link", &screen));
    }

    /// from tests/test_asset_weight.py::test_print_media_stylesheet_is_not_render_blocking
    #[test]
    fn print_media_stylesheet_is_not_render_blocking() {
        use spiderhead_parse::asset_evidence::is_render_blocking;
        let print = [("media".to_owned(), "print".to_owned())];
        assert!(!is_render_blocking("link", &print));
    }

    /// from tests/test_asset_weight.py::test_find_render_blocking_resources_in_head
    #[test]
    fn find_render_blocking_resources_in_head() {
        let soup = parse_document(
            "<html><head><script src=\"/a.js\"></script>\
             <script src=\"/b.js\" defer></script>\
             <link rel=\"stylesheet\" href=\"/blocking.css\">\
             <link rel=\"stylesheet\" href=\"/print.css\" media=\"print\">\
             <link rel=\"icon\" href=\"/favicon.ico\">\
             </head><body></body></html>",
        );
        let found = spiderhead_parse::asset_evidence::find_render_blocking_resources(
            &soup,
            "https://example.com/",
        )
        .expect("resolve");
        let urls: BTreeSet<_> = found.iter().map(|found| found.url.as_str()).collect();
        assert_eq!(
            urls,
            BTreeSet::from([
                "https://example.com/a.js",
                "https://example.com/blocking.css"
            ])
        );
    }

    /// from tests/test_asset_weight.py::test_template_only_resources_are_not_render_blocking
    #[test]
    fn template_only_resources_are_not_render_blocking() {
        let soup = parse_document(
            "<html><head><template><script src=\"/never-loaded.js\"></script>\
             <link rel=\"stylesheet\" href=\"/never-loaded.css\"></template>\
             <script src=\"/real.js\"></script></head><body></body></html>",
        );
        let found = spiderhead_parse::asset_evidence::find_render_blocking_resources(
            &soup,
            "https://example.com/",
        )
        .expect("resolve");
        let urls: BTreeSet<_> = found.iter().map(|found| found.url.as_str()).collect();
        assert_eq!(urls, BTreeSet::from(["https://example.com/real.js"]));
    }

    /// from tests/test_asset_weight.py::test_font_display_swap_is_compliant
    #[test]
    fn font_display_swap_is_compliant() {
        assert!(find_missing_font_display(
            "@font-face { font-family: A; src: url(a.woff2); font-display: swap; }"
        )
        .is_empty());
    }

    /// from tests/test_asset_weight.py::test_font_display_missing_is_flagged
    #[test]
    fn font_display_missing_is_flagged() {
        let findings =
            find_missing_font_display("@font-face { font-family: A; src: url(a.woff2); }");
        assert_eq!(findings.len(), 1);
        assert!(findings[0].excerpt.contains("font-family: A"));
    }

    /// from tests/test_asset_weight.py::test_font_display_auto_is_still_flagged
    #[test]
    fn font_display_auto_is_still_flagged() {
        assert_eq!(
            find_missing_font_display("@font-face { font-family: A; font-display: auto; }").len(),
            1
        );
    }

    /// from tests/test_asset_weight.py::test_multiple_font_face_blocks_each_checked
    #[test]
    fn multiple_font_face_blocks_each_checked() {
        let findings = find_missing_font_display(
            "@font-face { font-family: A; font-display: swap; }@font-face { font-family: B; }",
        );
        assert_eq!(findings.len(), 1);
    }

    /// from tests/test_asset_weight.py::test_legacy_transpile_markers_detected
    #[test]
    fn legacy_transpile_markers_detected() {
        assert!(looks_legacy_transpiled(
            "require('core-js/modules/es.promise')"
        ));
        assert!(looks_legacy_transpiled("var regeneratorRuntime = {}"));
        assert!(!looks_legacy_transpiled("const x = () => 1;"));
    }

    /// from tests/test_asset_weight.py::test_source_map_comment_found_in_js
    #[test]
    fn source_map_comment_found_in_js() {
        assert_eq!(
            find_source_map_comment("var x=1;\n//# sourceMappingURL=app.js.map"),
            Some("app.js.map".to_owned())
        );
    }

    /// from tests/test_asset_weight.py::test_source_map_comment_found_in_css_block_comment
    #[test]
    fn source_map_comment_found_in_css_block_comment() {
        assert_eq!(
            find_source_map_comment("body{color:red}/*# sourceMappingURL=app.css.map */"),
            Some("app.css.map".to_owned())
        );
    }

    /// from tests/test_asset_weight.py::test_source_map_comment_absent_returns_none
    #[test]
    fn source_map_comment_absent_returns_none() {
        assert_eq!(find_source_map_comment("var x = 1;"), None);
    }

    /// from tests/test_asset_weight.py::test_source_map_inline_data_uri_is_not_reported
    #[test]
    fn source_map_inline_data_uri_is_not_reported() {
        let encoded = "//# sourceMappingURL=data:application/json;base64,eyJ2ZXJzaW9uIjozfQ==";
        assert_eq!(find_source_map_comment(encoded), None);
    }

    /// from tests/test_asset_weight.py::test_source_map_last_comment_wins_when_more_than_one
    #[test]
    fn source_map_last_comment_wins_when_more_than_one() {
        assert_eq!(
            find_source_map_comment(
                "//# sourceMappingURL=old.js.map\nvar x=1;\n//# sourceMappingURL=new.js.map"
            ),
            Some("new.js.map".to_owned())
        );
    }

    /// from tests/test_asset_weight.py::test_debug_code_flagged_in_minified_bundle
    #[test]
    fn debug_code_flagged_in_minified_bundle() {
        let minified = minify(&repeat_readable_js(6));
        assert!(find_debug_code(&minified).contains(&"console.log(".to_owned()));
    }

    /// from tests/test_asset_weight.py::test_debug_code_detects_debugger_and_alert
    #[test]
    fn debug_code_detects_debugger_and_alert() {
        let minified = &minify(&"function f(){debugger;alert('hi');return 1;}".repeat(10));
        let markers = find_debug_code(minified);
        assert!(markers.contains(&"debugger".to_owned()));
        assert!(markers.contains(&"alert(".to_owned()));
    }

    /// from tests/test_asset_weight.py::test_debug_code_in_unminified_source_produces_nothing
    #[test]
    fn debug_code_in_unminified_source_produces_nothing() {
        assert!(find_debug_code(&repeat_readable_js(6)).is_empty());
    }

    /// from tests/test_asset_weight.py::test_debug_code_absent_from_clean_minified_bundle
    #[test]
    fn debug_code_absent_from_clean_minified_bundle() {
        let minified = &minify(&"function f(){return 1;}".repeat(20));
        assert!(find_debug_code(minified).is_empty());
    }

    /// from tests/test_asset_weight.py::test_debug_code_absent_from_short_hand_authored_script
    #[test]
    fn debug_code_absent_from_short_hand_authored_script() {
        let js = "\nfunction greet(name) {\n    console.log(\"hello\", name);\n}\n";
        assert!(find_debug_code(js).is_empty());
    }

    /// from tests/test_asset_weight.py::test_debug_code_flagged_in_short_genuinely_minified_script
    #[test]
    fn debug_code_flagged_in_short_genuinely_minified_script() {
        assert_eq!(
            find_debug_code("function greet(n){console.log(\"hello\",n)}"),
            vec!["console.log(".to_owned()]
        );
    }

    /// from tests/test_asset_weight.py::test_document_write_detected
    #[test]
    fn document_write_detected() {
        assert!(has_document_write(
            "document.write('<script src=\"x.js\"></script>')"
        ));
    }

    /// from tests/test_asset_weight.py::test_document_write_absent
    #[test]
    fn document_write_absent() {
        assert!(!has_document_write("document.writeln('<p>x</p>')"));
        assert!(!has_document_write("console.log('no document.write here')"));
    }

    /// from tests/test_asset_weight.py::test_css_import_targets_extracted_in_all_syntaxes
    #[test]
    fn css_import_targets_extracted_in_all_syntaxes() {
        assert_eq!(
            find_css_imports("@import \"a.css\";@import url(b.css);@import url(\"c.css\") screen;"),
            vec!["a.css", "b.css", "c.css"]
        );
    }

    /// from tests/test_asset_weight.py::test_css_import_absent_returns_empty_list
    #[test]
    fn css_import_absent_returns_empty_list() {
        assert!(find_css_imports("body{color:red}").is_empty());
    }

    /// from tests/test_asset_weight.py::test_cache_control_missing
    #[test]
    fn cache_control_missing() {
        assert_eq!(
            serde_json::to_value(check_cache_lifetime(None)).unwrap(),
            json!({"ok": false, "max_age": null, "reason": "no Cache-Control max-age"})
        );
    }

    /// from tests/test_asset_weight.py::test_cache_control_short_max_age_is_flagged
    #[test]
    fn cache_control_short_max_age_is_flagged() {
        let result = check_cache_lifetime(Some("public, max-age=60"));
        assert!(!result.ok);
        assert_eq!(result.max_age, Some(60));
    }

    /// from tests/test_asset_weight.py::test_cache_control_long_max_age_is_ok
    #[test]
    fn cache_control_long_max_age_is_ok() {
        assert!(check_cache_lifetime(Some("public, max-age=31536000, immutable")).ok);
    }

    /// from tests/test_asset_weight.py::test_cache_control_no_store_is_flagged
    #[test]
    fn cache_control_no_store_is_flagged() {
        assert!(!check_cache_lifetime(Some("no-store")).ok);
    }

    /// from tests/test_asset_weight.py::test_compression_gzip_is_ok
    #[test]
    fn compression_gzip_is_ok() {
        assert!(check_compression(Some("gzip")).0);
    }

    /// from tests/test_asset_weight.py::test_compression_brotli_is_ok
    #[test]
    fn compression_brotli_is_ok() {
        assert!(check_compression(Some("br")).0);
    }

    /// from tests/test_asset_weight.py::test_compression_missing_is_not_ok
    #[test]
    fn compression_missing_is_not_ok() {
        assert!(!check_compression(None).0);
    }

    /// from tests/test_asset_weight.py::test_flag_outlier_pages_needs_more_than_one_page
    #[test]
    fn flag_outlier_pages_needs_more_than_one_page() {
        let totals = BTreeMap::from([("https://example.com/a".to_owned(), 500_000)]);
        assert!(flag_outlier_pages(&totals, 2.0, 100_000).is_empty());
    }

    /// from tests/test_asset_weight.py::test_flag_outlier_pages_ignores_near_identical_sites
    #[test]
    fn flag_outlier_pages_ignores_near_identical_sites() {
        let mut totals = BTreeMap::new();
        for page in 0..10 {
            totals.insert(format!("https://example.com/p{page}"), 200_000);
        }
        totals.insert("https://example.com/p10".to_owned(), 201_000);
        assert!(flag_outlier_pages(&totals, 2.0, 100_000).is_empty());
    }

    /// from tests/test_asset_weight.py::test_flag_outlier_pages_catches_a_genuine_outlier
    #[test]
    fn flag_outlier_pages_catches_a_genuine_outlier() {
        let mut totals = BTreeMap::new();
        for page in 0..10 {
            totals.insert(format!("https://example.com/p{page}"), 200_000);
        }
        totals.insert("https://example.com/heavy".to_owned(), 900_000);
        assert_eq!(
            flag_outlier_pages(&totals, 2.0, 100_000),
            vec!["https://example.com/heavy"]
        );
    }

    // ── orchestrator ────────────────────────────────────────────────────────

    /// from tests/test_asset_weight.py::test_page_with_every_delivery_problem_is_flagged_with_byte_accurate_size
    #[test]
    fn page_with_every_delivery_problem_is_flagged_with_byte_accurate_size() {
        let css_body = "@font-face { font-family: Body; src: url(b.woff2); }\n".to_owned()
            + &repeat_readable_css(50);
        assert!(css_body.len() > 5_000);
        let js_body = "function legacy(){require('core-js/modules/es.array.map')}";

        let fetcher = MapFetcher::new(vec![
            page(&head_with(
                "<script src=\"/blocking.js\"></script>\
                 <link rel=\"stylesheet\" href=\"/heavy.css\">",
            )),
            (
                "https://example.com/blocking.js",
                FakeResponse::new(js_body),
            ),
            (
                "https://example.com/heavy.css",
                FakeResponse::with_headers(&css_body, &[("cache-control", "no-store")]),
            ),
        ]);

        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions {
                file_size_threshold: 5_000,
                ..AssetWeightOptions::default()
            },
        )
        .expect("analyze");

        assert_eq!(result["ok"], json!(true));
        assert_eq!(
            result["total_bytes"],
            json!((css_body.len() + js_body.len()) as i64)
        );
        let blocking: BTreeSet<_> = result["render_blocking"]
            .as_array()
            .unwrap()
            .iter()
            .map(|found| found["url"].as_str().unwrap())
            .collect();
        assert_eq!(
            blocking,
            BTreeSet::from([
                "https://example.com/blocking.js",
                "https://example.com/heavy.css"
            ])
        );
        assert_eq!(
            result["oversized"],
            json!([{
                "url": "https://example.com/heavy.css",
                "bytes": css_body.len() as i64,
                "threshold": 5_000,
            }])
        );
        assert_eq!(
            result["unminified"],
            json!(["https://example.com/heavy.css"])
        );
        let sources: Vec<_> = result["missing_font_display"]
            .as_array()
            .unwrap()
            .iter()
            .map(|finding| finding["source"].as_str().unwrap())
            .collect();
        assert_eq!(sources, ["https://example.com/heavy.css"]);
        assert_eq!(
            result["legacy_js"],
            json!(["https://example.com/blocking.js"])
        );
        for finding in result["cache_findings"].as_array().unwrap() {
            assert_eq!(finding["ok"], json!(false));
        }
        let cache_urls: BTreeSet<_> = result["cache_findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|finding| finding["url"].as_str().unwrap())
            .collect();
        assert_eq!(
            cache_urls,
            BTreeSet::from([
                "https://example.com/blocking.js",
                "https://example.com/heavy.css"
            ])
        );
        let compression_urls: BTreeSet<_> = result["compression_findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|finding| finding["url"].as_str().unwrap())
            .collect();
        assert_eq!(
            compression_urls,
            BTreeSet::from([
                "https://example.com/blocking.js",
                "https://example.com/heavy.css"
            ])
        );
        assert!(result["findings"].as_array().unwrap().len() >= 6);
        let skipped: BTreeSet<_> = result["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["check"].as_str().unwrap())
            .collect();
        assert_eq!(
            skipped,
            BTreeSet::from(["unused_css_js", "site_median_outlier"])
        );
    }

    /// from tests/test_asset_weight.py::test_clean_page_produces_zero_findings
    #[test]
    fn clean_page_produces_zero_findings() {
        let minified_css = minify(
            &("@font-face { font-family: Body; src: url(b.woff2); font-display: swap; }"
                .to_owned()
                + &repeat_readable_css(10)),
        );
        let minified_js = minify("function f(){\n  return 1;\n}");
        let good_headers: [(&str, &str); 2] = [
            ("cache-control", "public, max-age=31536000, immutable"),
            ("content-encoding", "br"),
        ];

        let fetcher = MapFetcher::new(vec![
            page(&head_with(
                "<script src=\"/app.js\" defer></script>\
                 <link rel=\"stylesheet\" href=\"/app.css\" media=\"print\" \
                 onload=\"this.media='all'\">",
            )),
            (
                "https://example.com/app.js",
                FakeResponse::with_headers(&minified_js, &good_headers),
            ),
            (
                "https://example.com/app.css",
                FakeResponse::with_headers(&minified_css, &good_headers),
            ),
        ]);

        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");

        assert_eq!(result["ok"], json!(true));
        assert_eq!(result["render_blocking"], json!([]));
        assert_eq!(result["oversized"], json!([]));
        assert_eq!(result["duplicate_libraries"], json!([]));
        assert_eq!(result["unminified"], json!([]));
        assert_eq!(result["missing_font_display"], json!([]));
        assert_eq!(result["legacy_js"], json!([]));
        assert_eq!(result["cache_findings"], json!([]));
        assert_eq!(result["compression_findings"], json!([]));
        assert_eq!(result["findings"], json!([]));
    }

    /// from tests/test_asset_weight.py::test_duplicate_library_bundled_twice_is_caught_end_to_end
    #[test]
    fn duplicate_library_bundled_twice_is_caught_end_to_end() {
        let lib_a = "var lib = { version: 1 };\nlib.run = function () { return 1; };\n".repeat(20);
        let lib_b = minify(&lib_a);

        let fetcher = MapFetcher::new(vec![
            page(&head_with(
                "<script src=\"/vendor/lib.min.js\"></script>\
                 <script src=\"/chunks/9f.js\"></script>",
            )),
            (
                "https://example.com/vendor/lib.min.js",
                FakeResponse::new(&lib_b),
            ),
            (
                "https://example.com/chunks/9f.js",
                FakeResponse::new(&lib_a),
            ),
        ]);

        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");
        assert_eq!(result["duplicate_libraries"].as_array().unwrap().len(), 1);
        let urls: BTreeSet<_> = result["duplicate_libraries"][0]["urls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|url| url.as_str().unwrap())
            .collect();
        assert_eq!(
            urls,
            BTreeSet::from([
                "https://example.com/vendor/lib.min.js",
                "https://example.com/chunks/9f.js"
            ])
        );
    }

    /// from tests/test_asset_weight.py::test_source_map_reported_only_when_the_target_actually_resolves
    #[test]
    fn source_map_reported_only_when_the_target_actually_resolves() {
        let fetcher = MapFetcher::new(vec![
            page(&head_with(
                "<script src=\"/app.js\"></script><script src=\"/legacy.js\"></script>",
            )),
            (
                "https://example.com/app.js",
                FakeResponse::new("var x=1;\n//# sourceMappingURL=app.js.map"),
            ),
            (
                "https://example.com/app.js.map",
                FakeResponse::new("{\"version\":3}"),
            ),
            // legacy.js references a map that 404s: the comment exists but
            // nothing is actually exposed, so it must not be reported.
            (
                "https://example.com/legacy.js",
                FakeResponse::new("var y=2;\n//# sourceMappingURL=legacy.js.map"),
            ),
        ]);

        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");

        assert_eq!(
            result["exposed_source_maps"],
            json!([{
                "source": "https://example.com/app.js",
                "map_url": "https://example.com/app.js.map",
            }])
        );
        assert!(result["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding.as_str().unwrap().contains("source map")));
    }

    /// from tests/test_asset_weight.py::test_debug_code_flagged_end_to_end_only_when_minified
    #[test]
    fn debug_code_flagged_end_to_end_only_when_minified() {
        let minified_with_console = minify(&"function f(){console.log('x');return 1;}".repeat(15));
        let fetcher = MapFetcher::new(vec![
            page(&head_with(
                "<script src=\"/bad.js\"></script><script src=\"/dev.js\"></script>",
            )),
            (
                "https://example.com/bad.js",
                FakeResponse::new(&minified_with_console),
            ),
            // Hand-authored (unminified): the same console.log call is noise here.
            (
                "https://example.com/dev.js",
                FakeResponse::new(&repeat_readable_js(6)),
            ),
        ]);

        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");

        assert_eq!(
            result["debug_code"],
            json!([{
                "url": "https://example.com/bad.js",
                "markers": ["console.log("],
            }])
        );
        assert!(result["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding.as_str().unwrap().contains("debug code")));
    }

    /// from tests/test_asset_weight.py::test_document_write_flagged_end_to_end
    #[test]
    fn document_write_flagged_end_to_end() {
        let fetcher = MapFetcher::new(vec![
            page(&head_with("<script src=\"/inject.js\"></script>")),
            (
                "https://example.com/inject.js",
                FakeResponse::new("document.write('<script src=\"ads.js\"></script>')"),
            ),
        ]);

        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");

        assert_eq!(
            result["document_write"],
            json!(["https://example.com/inject.js"])
        );
        assert!(result["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding.as_str().unwrap().contains("document.write")));
    }

    /// from tests/test_asset_weight.py::test_css_import_chain_depth_follows_one_level
    #[test]
    fn css_import_chain_depth_follows_one_level() {
        let fetcher = MapFetcher::new(vec![
            page(&head_with("<link rel=\"stylesheet\" href=\"/a.css\">")),
            // a.css imports b.css (depth so far: 1)...
            (
                "https://example.com/a.css",
                FakeResponse::new("@import \"b.css\"; .a{color:red}"),
            ),
            // ...and b.css itself imports c.css, so the chain is confirmed 2 deep.
            (
                "https://example.com/b.css",
                FakeResponse::new("@import \"c.css\"; .b{color:blue}"),
            ),
            (
                "https://example.com/c.css",
                FakeResponse::new(".c{color:green}"),
            ),
        ]);

        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");

        assert_eq!(
            result["css_import_chains"],
            json!([{
                "source": "https://example.com/a.css",
                "import_url": "https://example.com/b.css",
                "depth": 2,
            }])
        );
        assert!(result["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding.as_str().unwrap().contains("@import chain")));
    }

    /// from tests/test_asset_weight.py::test_css_import_without_further_chaining_is_depth_one
    #[test]
    fn css_import_without_further_chaining_is_depth_one() {
        let fetcher = MapFetcher::new(vec![
            page(&head_with("<link rel=\"stylesheet\" href=\"/a.css\">")),
            (
                "https://example.com/a.css",
                FakeResponse::new("@import \"b.css\"; .a{color:red}"),
            ),
            (
                "https://example.com/b.css",
                FakeResponse::new(".b{color:blue}"),
            ),
        ]);

        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");

        assert_eq!(
            result["css_import_chains"],
            json!([{
                "source": "https://example.com/a.css",
                "import_url": "https://example.com/b.css",
                "depth": 1,
            }])
        );
    }

    /// from tests/test_asset_weight.py::test_a_broken_resource_is_reported_not_silently_dropped
    #[test]
    fn a_broken_resource_is_reported_not_silently_dropped() {
        let fetcher = MapFetcher::new(vec![page(&head_with(
            "<link rel=\"stylesheet\" href=\"/missing.css\">",
        ))]);
        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");
        assert_eq!(result["resources"][0]["ok"], json!(false));
        assert_eq!(result["resources"][0]["status_code"], json!(404));
        assert_eq!(result["total_bytes"], json!(0));
    }

    /// from tests/test_asset_weight.py::test_page_fetch_failure_is_reported_as_an_error_not_raised
    #[test]
    fn page_fetch_failure_is_reported_as_an_error_not_raised() {
        let fetcher = MapFetcher::new(Vec::new());
        let result = analyze_page_asset_weight(
            "https://example.com/gone",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");
        assert_eq!(
            result,
            json!({"ok": false, "url": "https://example.com/gone", "status_code": 404})
        );
    }

    /// from tests/test_asset_weight.py::test_template_only_resources_are_never_fetched
    #[test]
    fn template_only_resources_are_never_fetched() {
        // The fetcher has no entry for the template-only URLs at all: a fetch
        // attempt on them would 404 through the mapping's default and this
        // test would still fail, proving the exclusion happens before any
        // network call, not just after.
        let fetcher = MapFetcher::new(vec![
            page(&head_with(
                "<template><script src=\"/never-loaded.js\"></script>\
                 <link rel=\"stylesheet\" href=\"/never-loaded.css\"></template>\
                 <script src=\"/real.js\"></script>",
            )),
            (
                "https://example.com/real.js",
                FakeResponse::new("function f(){return 1}"),
            ),
        ]);
        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");
        assert_eq!(result["ok"], json!(true));
        let urls: BTreeSet<_> = result["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|resource| resource["url"].as_str().unwrap())
            .collect();
        assert_eq!(urls, BTreeSet::from(["https://example.com/real.js"]));
        assert_eq!(
            result["render_blocking"],
            json!([{"url": "https://example.com/real.js", "tag": "script"}])
        );
    }

    /// from tests/test_asset_weight.py::test_resource_fan_out_is_bounded
    #[test]
    fn resource_fan_out_is_bounded() {
        let many_links: String = (0..100)
            .map(|index| format!("<link rel=\"stylesheet\" href=\"/s{index}.css\">"))
            .collect();
        let mut pairs = vec![(
            "https://example.com/",
            FakeResponse::new(&head_with(&many_links)),
        )];
        for index in 0..100 {
            pairs.push((
                Box::leak(format!("https://example.com/s{index}.css").into_boxed_str()) as &str,
                FakeResponse::new(".a{color:red}"),
            ));
        }
        let fetcher = MapFetcher::new(pairs);

        let result = analyze_page_asset_weight(
            "https://example.com/",
            &fetcher,
            AssetWeightOptions {
                max_resources: 10,
                ..AssetWeightOptions::default()
            },
        )
        .expect("analyze");

        assert_eq!(result["resources_truncated"], json!(true));
        assert_eq!(result["resources"].as_array().unwrap().len(), 10);
    }

    // ── transport-failure branches (no Python-test counterpart: the Python
    //    dict fetcher never raises; the seam allows it, so pin the shapes) ──

    /// `test_page_fetch_failure_is_reported_as_an_error_not_raised`, transport variant.
    #[test]
    fn page_transport_failure_reports_error_shape() {
        struct DeadFetcher;
        impl AssetFetcher for DeadFetcher {
            fn get(&self, _url: &str) -> Result<AssetReply, String> {
                Err("connection refused".to_owned())
            }
        }
        let result = analyze_page_asset_weight(
            "https://example.com/",
            &DeadFetcher,
            AssetWeightOptions::default(),
        )
        .expect("analyze");
        assert_eq!(
            result,
            json!({"ok": false, "url": "https://example.com/", "error": "connection refused"})
        );
    }

    /// `test_a_broken_resource_is_reported_not_silently_dropped`, transport variant.
    #[test]
    fn resource_transport_failure_reports_error_shape() {
        struct DeadPage;
        impl AssetFetcher for DeadPage {
            fn get(&self, url: &str) -> Result<AssetReply, String> {
                if url == "https://example.com/" {
                    return Ok(AssetReply {
                        status_code: 200,
                        url: None,
                        content: Some(Vec::new()),
                        text: head_with("<script src=\"/dead.js\"></script>"),
                        headers: Vec::new(),
                    });
                }
                Err("name resolution failed".to_owned())
            }
        }
        let result = analyze_page_asset_weight(
            "https://example.com/",
            &DeadPage,
            AssetWeightOptions::default(),
        )
        .expect("analyze");
        assert_eq!(
            result["resources"][0],
            json!({
                "url": "https://example.com/dead.js",
                "kind": "js",
                "ok": false,
                "error": "name resolution failed",
            })
        );
        assert_eq!(result["total_bytes"], json!(0));
    }
}
