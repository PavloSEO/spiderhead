//! Technical checks (reference the reference suite).

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{json, Map, Value};

use super::details;
use crate::audit::model::norm_url;
use crate::audit::AuditContext;

/// `initial-scale\s*=\s*([0-9.]+)` — the scale declaration a viewport tag may pin.
fn initial_scale(content: &str) -> Option<f64> {
    let lower = content.to_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("initial-scale") {
        let rest = lower[from + at + 13..].trim_start();
        if let Some(rest) = rest.strip_prefix('=') {
            let rest = rest.trim_start();
            let value: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if !value.is_empty() {
                return value.parse().ok();
            }
        }
        from += at + 13;
    }
    None
}

/// `\bwidth\s*=` case-insensitive — a width token the viewport tag declares.
fn declares_width(content: &str) -> bool {
    let lower = content.to_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("width") {
        let start = from + at;
        let boundary = start == 0
            || !lower[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if boundary {
            let rest = lower[start + 5..].trim_start();
            if rest.starts_with('=') {
                return true;
            }
        }
        from = start + 5;
    }
    false
}

/// `check_viewport` (#1576): the absent-tag row plus a declaration that cannot
/// work — initial-scale below 1, or neither a width nor an initial-scale.
pub fn check_viewport(ctx: &mut AuditContext) {
    if !ctx.has_column("viewport") {
        let reason = "no Viewport column (needs a native seohead crawl or Custom Extraction in SF)";
        ctx.skip("VIEWPORT_MISSING", reason);
        ctx.skip("INVALID_VIEWPORT", reason);
        return;
    }
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let content = page.text("viewport").to_owned();
        if content.is_empty() {
            ctx.add(
                "VIEWPORT_MISSING",
                Some(&page.url),
                details([("reason", "no viewport meta tag".into())]),
            );
            continue;
        }
        let scale = initial_scale(&content);
        if scale.is_some_and(|s| s < 1.0) {
            ctx.add(
                "INVALID_VIEWPORT",
                Some(&page.url),
                details([
                    ("viewport", content.into()),
                    ("reason", "initial-scale below 1".into()),
                ]),
            );
        } else if scale.is_none() && !declares_width(&content) {
            ctx.add(
                "INVALID_VIEWPORT",
                Some(&page.url),
                details([
                    ("viewport", content.into()),
                    ("reason", "no width or initial-scale".into()),
                ]),
            );
        }
    }
}

/// Declared charset of a `Content-Type` header (`charset\s*=\s*"?([^\s";]+)`), when it names one.
fn header_charset(content_type: &str) -> (bool, String) {
    let lower = content_type.to_lowercase();
    let mut found = false;
    let mut from = 0;
    while let Some(at) = lower[from..].find("charset") {
        let rest = lower[from + at + 7..].trim_start();
        if let Some(rest) = rest.strip_prefix('=') {
            found = true;
            let rest = rest.trim_start().trim_start_matches('"');
            let value: String = rest
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '"' && *c != ';')
                .collect();
            let start = content_type.len() - rest.len();
            let original = &content_type[start..start + value.len()];
            return (found, original.trim_matches(['"', '\'']).to_owned());
        }
        from += at + 7;
    }
    (found, String::new())
}

/// `check_charset`: pages whose declared encoding is missing or not UTF-8.
pub fn check_charset(ctx: &mut AuditContext) {
    // Without the Meta Charset column absence is unmeasurable: a page whose
    // Content-Type carries no charset may still declare <meta charset> in its
    // HTML, and firing on the header alone reports a defect nobody measured.
    if !ctx.has_column("meta_charset") {
        ctx.skip(
            "MISSING_CHARSET",
            "no Meta Charset column, so a page without a header charset cannot be \
             distinguished from one declaring <meta charset> (needs a native seohead \
             crawl or Custom Extraction in SF)",
        );
        ctx.skip(
            "INVALID_CHARSET",
            "no Meta Charset column, so the encoding a page declares in its HTML cannot \
             be read (needs a native seohead crawl or Custom Extraction in SF)",
        );
        return;
    }
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let meta = page.text("meta_charset").trim().to_owned();
        let (header_declares, header) = header_charset(&page.content_type);
        if meta.is_empty() && !header_declares {
            ctx.add(
                "MISSING_CHARSET",
                Some(&page.url),
                details([("content_type", page.content_type.clone().into())]),
            );
            continue;
        }
        if meta.is_empty() && header.is_empty() {
            continue;
        }
        let (declared, by) = if meta.is_empty() {
            (header, "header")
        } else {
            (meta, "meta")
        };
        if !matches!(declared.to_lowercase().as_str(), "utf-8" | "utf8") {
            ctx.add(
                "INVALID_CHARSET",
                Some(&page.url),
                details([("charset", declared.into()), ("declared_by", by.into())]),
            );
        }
    }
}

/// `check_compression`: HTML served without gzip/br/deflate/zstd above Lighthouse's 1400-byte floor.
pub fn check_compression(ctx: &mut AuditContext) {
    if !ctx.has_column("content_encoding") {
        ctx.skip(
            "NO_COMPRESSION",
            "no Content-Encoding column (needs a native seohead crawl or Custom Extraction in SF)",
        );
        return;
    }
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        let encoding = page.text("content_encoding").to_lowercase();
        let compressed = encoding
            .split(',')
            .map(str::trim)
            .any(|t| matches!(t, "gzip" | "br" | "deflate" | "zstd"));
        if compressed {
            continue;
        }
        if page.num("size_bytes").is_some_and(|s| s < 1400.0) {
            continue;
        }
        ctx.add(
            "NO_COMPRESSION",
            Some(&page.url),
            details([
                ("content_encoding", page.opt("content_encoding")),
                ("size_bytes", page.opt("size_bytes")),
            ]),
        );
    }
}

/// `str(rec.get(field) or "")`: record values stringify the reference way —
/// strings pass through, numbers keep their plain form.
fn py_str(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        _ => String::new(),
    }
}

/// `check_tech_extra`: HTTP/1-served pages and pages declaring an AMP target.
pub fn check_tech_extra(ctx: &mut AuditContext) {
    let has_http_version = ctx.has_column("http_version");
    let has_amphtml = ctx.has_column("amphtml");
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        let hv = py_str(page.rec.get("http_version"))
            .to_uppercase()
            .replace("HTTP/", "")
            .trim()
            .to_owned();
        if has_http_version && hv.starts_with('1') {
            ctx.add(
                "HTTP1_ONLY",
                Some(&page.url),
                details([(
                    "http_version",
                    page.rec.get("http_version").cloned().unwrap_or(Value::Null),
                )]),
            );
        }
        if has_amphtml
            && page
                .rec
                .get("amphtml")
                .and_then(Value::as_str)
                .is_some_and(|a| !a.is_empty())
        {
            ctx.add(
                "AMPHTML_PRESENT",
                Some(&page.url),
                details([(
                    "amphtml",
                    page.rec.get("amphtml").cloned().unwrap_or(Value::Null),
                )]),
            );
        }
    }
    if !has_http_version {
        ctx.skip("HTTP1_ONLY", "no HTTP Version column in Internal:All");
    }
    if !has_amphtml {
        ctx.skip(
            "AMPHTML_PRESENT",
            "no amphtml Link Element column in Internal:All",
        );
    }
}

/// `_AMP_CODES` (#1020): every code `check_amp_pairing` settles.
const AMP_CODES: [&str; 5] = [
    "AMP_NON_200",
    "AMP_MISSING_CANONICAL",
    "AMP_MISSING_RETURN_LINK",
    "AMP_NON_INDEXABLE_CANONICAL",
    "AMP_INDEXABLE",
];

/// `check_amp_pairing`: each declared AMP target's own record. Uncaptured
/// targets are unavailable evidence — named in a skip, never reported.
pub fn check_amp_pairing(ctx: &mut AuditContext) {
    if !ctx.has_column("amphtml") {
        for code in AMP_CODES {
            ctx.skip(code, "no amphtml Link Element column in Internal:All");
        }
        return;
    }
    let mut order = Vec::new();
    let mut targets: HashMap<String, (String, Vec<String>)> = HashMap::new();
    for i in ctx.html_pages() {
        let Some(amp) = ctx.pages[i]
            .rec
            .get("amphtml")
            .and_then(Value::as_str)
            .filter(|a| !a.is_empty())
        else {
            continue;
        };
        let key = norm_url(amp);
        let entry = targets.entry(key.clone()).or_insert_with(|| {
            order.push(key);
            (amp.to_owned(), Vec::new())
        });
        entry.1.push(ctx.pages[i].url.clone());
    }
    if targets.is_empty() {
        for code in AMP_CODES {
            ctx.skip(code, "no page declares an AMP target (rel=amphtml)");
        }
        return;
    }
    let (by_norm, _rep) = super::pagination::norm_index(ctx);
    let mut found: HashSet<&str> = HashSet::new();
    let mut uncaptured = 0i64;
    for key in &order {
        let (amp_url, desktop_urls) = &targets[key];
        let Some(rows) = by_norm.get(key) else {
            uncaptured += 1;
            continue;
        };
        let omitted = desktop_urls.len().saturating_sub(20) as i64;
        let base = details([
            ("amphtml", json!(amp_url)),
            (
                "desktop_urls",
                json!(&desktop_urls[..desktop_urls.len().min(20)]),
            ),
            ("desktop_urls_omitted", json!(omitted)),
        ]);

        let codes: Vec<Option<i64>> = rows.iter().map(|&r| ctx.pages[r].status_code).collect();
        if codes.iter().flatten().any(|c| *c != 200) {
            // `set(codes)` counts the unmeasured None as its own value; only a
            // single distinct row set lifts `status_code` to the top level.
            let distinct_all: BTreeSet<Option<i64>> = codes.iter().copied().collect();
            let status_code = (distinct_all.len() == 1).then_some(codes[0]).flatten();
            let measured: BTreeSet<i64> = codes.iter().flatten().copied().collect();
            let mut d = base.clone();
            d.insert("status_codes".into(), json!(measured));
            ctx.add_with("AMP_NON_200", Some(amp_url), d, status_code, 1, None);
            found.insert("AMP_NON_200");
        }

        for &r in rows {
            let row = &ctx.pages[r];
            if row.status_code != Some(200) || row.body_unavailable() {
                // An unparsed body carries no canonical evidence either way.
                continue;
            }
            let row_url = row.url.clone();
            let row_indexable = row.is_indexable();
            let canonical = row
                .rec
                .get("canonical")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let Some(canonical) = canonical.filter(|c| !c.is_empty()) else {
                ctx.add("AMP_MISSING_CANONICAL", Some(amp_url), base.clone());
                found.insert("AMP_MISSING_CANONICAL");
                if row_indexable {
                    ctx.add("AMP_INDEXABLE", Some(amp_url), base.clone());
                    found.insert("AMP_INDEXABLE");
                }
                continue;
            };
            if !desktop_urls
                .iter()
                .any(|d| norm_url(d) == norm_url(&canonical))
            {
                let mut d = base.clone();
                d.insert("canonical".into(), json!(canonical));
                ctx.add("AMP_MISSING_RETURN_LINK", Some(amp_url), d);
                found.insert("AMP_MISSING_RETURN_LINK");
            }
            if norm_url(&canonical) != norm_url(&row_url) {
                let empty = Vec::new();
                let canonical_targets = by_norm.get(&norm_url(&canonical)).unwrap_or(&empty);
                if !canonical_targets.is_empty()
                    && !canonical_targets
                        .iter()
                        .any(|&t| ctx.pages[t].is_indexable())
                {
                    let mut d = base.clone();
                    d.insert("canonical".into(), json!(canonical));
                    ctx.add("AMP_NON_INDEXABLE_CANONICAL", Some(amp_url), d);
                    found.insert("AMP_NON_INDEXABLE_CANONICAL");
                }
            }
            if row_indexable {
                ctx.add("AMP_INDEXABLE", Some(amp_url), base.clone());
                found.insert("AMP_INDEXABLE");
            }
        }
    }
    if uncaptured > 0 {
        for code in AMP_CODES {
            if !found.contains(code) {
                ctx.skip(
                    code,
                    &format!(
                        "{uncaptured} AMP target(s) were declared but not captured by the crawl"
                    ),
                );
            }
        }
    }
}

use super::py_truthy;

/// `check_doctype` (#1576): MISSING_DOCTYPE for the absent tag, INVALID_DOCTYPE
/// for a present-but-not-HTML5 declaration — a legacy PUBLIC/SYSTEM identifier
/// or another root name, either of which still triggers quirks mode.
pub fn check_doctype(ctx: &mut AuditContext) {
    if !ctx.has_column("doctype") {
        let reason = "no Doctype column (needs a native seohead crawl or Custom Extraction in SF)";
        ctx.skip("MISSING_DOCTYPE", reason);
        ctx.skip("INVALID_DOCTYPE", reason);
        return;
    }
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let raw = page.text("doctype").to_owned();
        if raw.is_empty() {
            ctx.add(
                "MISSING_DOCTYPE",
                Some(&page.url),
                details([("reason", "no doctype".into())]),
            );
            continue;
        }
        // `<!DOCTYPE\s+([a-zA-Z0-9]+)` anchored at the string start.
        let name =
            raw.get(..9)
                .filter(|head| head.eq_ignore_ascii_case("<!doctype"))
                .and_then(|_| {
                    let rest = raw[9..].trim_start();
                    (!rest.is_empty() && raw[9..].chars().next().is_some_and(char::is_whitespace))
                        .then(|| {
                            rest.chars()
                                .take_while(|c| c.is_ascii_alphanumeric())
                                .collect::<String>()
                                .to_lowercase()
                        })
                })
                .unwrap_or_default();
        let lower = raw.to_lowercase();
        if name != "html" || lower.contains("public") || lower.contains("system") {
            ctx.add(
                "INVALID_DOCTYPE",
                Some(&page.url),
                details([("doctype", raw.into())]),
            );
        }
    }
}

/// `check_mobile_alternate` (#1016): a media-qualified rel=alternate that
/// names no URL. Native-only evidence.
pub fn check_mobile_alternate(ctx: &mut AuditContext) {
    if !ctx.has_column("mobile_alternate_broken") {
        ctx.skip(
            "MOBILE_ALTERNATE_LINK",
            "no rel=alternate media evidence (native crawl only)",
        );
        return;
    }
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let count = page.rec.get("mobile_alternate_broken").cloned();
        if py_truthy(count.as_ref()) {
            ctx.add(
                "MOBILE_ALTERNATE_LINK",
                Some(&page.url),
                details([
                    ("occurrences", count.clone().unwrap_or(Value::Null)),
                    ("reason", "rel=alternate with media has no href".into()),
                ]),
            );
        }
    }
}

/// `_ELEMENT_POSITION_CHECKS` — outside-<head> checks keyed by evidence field,
/// in the oracle's declaration order.
const ELEMENT_POSITION_CHECKS: [(&str, &str); 5] = [
    ("title_outside_head", "TITLE_OUTSIDE_HEAD"),
    ("meta_description_outside_head", "DESC_OUTSIDE_HEAD"),
    ("canonical_outside_head", "CANONICAL_OUTSIDE_HEAD"),
    ("directives_outside_head", "DIRECTIVES_OUTSIDE_HEAD"),
    ("hreflang_outside_head", "HREFLANG_OUTSIDE_HEAD"),
];

/// `check_element_position`: title/description ask about indexable content;
/// canonical/directives/hreflang matter on any HTML page.
pub fn check_element_position(ctx: &mut AuditContext) {
    const NO_POSITION_EVIDENCE: &str = "no element-position evidence (needs a native \
        seohead crawl; crawler exports do not record where an element sits)";
    for (field, check_id) in ELEMENT_POSITION_CHECKS {
        if !ctx.has_column(field) {
            ctx.skip(check_id, NO_POSITION_EVIDENCE);
            continue;
        }
        let pages = if matches!(
            field,
            "title_outside_head" | "meta_description_outside_head"
        ) {
            ctx.indexable_html_pages()
        } else {
            ctx.html_pages()
        };
        for i in pages {
            if py_truthy(ctx.pages[i].rec.get(field)) {
                let url = ctx.pages[i].url.clone();
                ctx.add(check_id, Some(&url), Map::new());
            }
        }
    }
}

/// `_SKELETON_FIELDS`: every column the document-skeleton checks need before
/// they can say anything.
const SKELETON_FIELDS: [&str; 4] = [
    "head_count",
    "body_count",
    "head_not_first",
    "invalid_head_elements",
];

/// `check_document_skeleton`: <head>/<body> presence, count and order. One
/// finding per page, never one per stray element.
pub fn check_document_skeleton(ctx: &mut AuditContext) {
    const SKELETON_CHECKS: [&str; 6] = [
        "HEAD_MISSING",
        "HEAD_MULTIPLE",
        "BODY_MISSING",
        "BODY_MULTIPLE",
        "INVALID_HEAD_ELEMENT",
        "HEAD_NOT_FIRST",
    ];
    let has_evidence = SKELETON_FIELDS.iter().all(|f| ctx.has_column(f));
    if !has_evidence {
        const NO_POSITION_EVIDENCE: &str = "no element-position evidence (needs a native \
            seohead crawl; crawler exports do not record where an element sits)";
        for check_id in SKELETON_CHECKS {
            ctx.skip(check_id, NO_POSITION_EVIDENCE);
        }
        return;
    }
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let head_count = page.num("head_count").unwrap_or(0.0);
        let body_count = page.num("body_count").unwrap_or(0.0);
        if head_count == 0.0 {
            ctx.add("HEAD_MISSING", Some(&page.url), Map::new());
        } else if head_count > 1.0 {
            ctx.add(
                "HEAD_MULTIPLE",
                Some(&page.url),
                details([(
                    "head_count",
                    page.rec.get("head_count").cloned().unwrap_or(Value::Null),
                )]),
            );
        }
        if body_count == 0.0 {
            ctx.add("BODY_MISSING", Some(&page.url), Map::new());
        } else if body_count > 1.0 {
            ctx.add(
                "BODY_MULTIPLE",
                Some(&page.url),
                details([(
                    "body_count",
                    page.rec.get("body_count").cloned().unwrap_or(Value::Null),
                )]),
            );
        }
        let invalid = py_str(page.rec.get("invalid_head_elements"));
        if !invalid.is_empty() {
            let elements: Vec<String> = invalid
                .split(',')
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_owned)
                .collect();
            ctx.add(
                "INVALID_HEAD_ELEMENT",
                Some(&page.url),
                details([("elements", json!(elements))]),
            );
        }
        if py_truthy(page.rec.get("head_not_first")) {
            ctx.add("HEAD_NOT_FIRST", Some(&page.url), Map::new());
        }
    }
}

/// `mimetypes.guess_type` for the URL-path extensions a crawl meets, exported
/// from the oracle's table (reference 3.14). Unlisted extensions are unmeasured.
fn guess_type(path: &str) -> Option<&'static str> {
    const MIME_BY_EXT: [(&str, &str); 44] = [
        ("atom", "application/atom+xml"),
        ("avif", "image/avif"),
        ("bmp", "image/bmp"),
        ("css", "text/css"),
        ("csv", "text/csv"),
        ("eot", "application/vnd.ms-fontobject"),
        ("gif", "image/gif"),
        ("heic", "image/heic"),
        ("heif", "image/heif"),
        ("htm", "text/html"),
        ("html", "text/html"),
        ("ico", "image/x-icon"),
        ("jpeg", "image/jpeg"),
        ("jpg", "image/jpeg"),
        ("js", "text/javascript"),
        ("json", "application/json"),
        ("md", "text/markdown"),
        ("mjs", "text/javascript"),
        ("mov", "video/quicktime"),
        ("mp3", "audio/mpeg"),
        ("mp4", "video/mp4"),
        ("ogg", "audio/ogg"),
        ("otf", "font/otf"),
        ("pdf", "application/pdf"),
        ("png", "image/png"),
        ("rss", "application/rss+xml"),
        ("svg", "image/svg+xml"),
        ("tar", "application/x-tar"),
        ("text", "text/plain"),
        ("tif", "image/tiff"),
        ("tiff", "image/tiff"),
        ("ttf", "font/ttf"),
        ("txt", "text/plain"),
        ("wav", "audio/x-wav"),
        ("webm", "video/webm"),
        ("webmanifest", "application/manifest+json"),
        ("webp", "image/webp"),
        ("woff", "font/woff"),
        ("woff2", "font/woff2"),
        ("xml", "application/xml"),
        ("xsl", "application/xml"),
        ("yaml", "application/yaml"),
        ("yml", "application/yaml"),
        ("zip", "application/zip"),
    ];
    let base = path.rsplit('/').next().unwrap_or(path);
    let dot = base.rfind('.')?;
    if dot == 0 {
        return None;
    }
    let ext = base[dot + 1..].to_ascii_lowercase();
    if ext.is_empty() {
        return None;
    }
    MIME_BY_EXT
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, mime)| *mime)
}

/// `check_html_structure` (#828): DUPLICATE_ID from parsed-DOM evidence and
/// DECLARED_MIME_MISMATCH from unambiguous filename extensions against the
/// received Content-Type family.
pub fn check_html_structure(ctx: &mut AuditContext) {
    let has_duplicate_ids = ctx.has_column("duplicate_ids");
    if !has_duplicate_ids {
        ctx.skip(
            "DUPLICATE_ID",
            "no duplicate-id evidence (native crawl only)",
        );
    }
    let mut unmeasured_ids = 0i64;
    if has_duplicate_ids {
        for i in ctx.html_pages() {
            let page = &ctx.pages[i];
            let values = page.rec.get("duplicate_ids");
            if page.body_unavailable() || !matches!(values, Some(Value::Array(_))) {
                unmeasured_ids += 1;
                continue;
            }
            let items = values
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let url = page.url.clone();
            for item in items {
                let Some(object) = item.as_object() else {
                    continue;
                };
                let Some(count) = object.get("count").and_then(Value::as_i64) else {
                    continue;
                };
                if let Some(identifier) = object.get("id").and_then(Value::as_str) {
                    if count > 1 {
                        ctx.add_with(
                            "DUPLICATE_ID",
                            Some(&url),
                            details([("id", identifier.into()), ("count", count.into())]),
                            None,
                            count,
                            None,
                        );
                    }
                }
            }
        }
    }
    if unmeasured_ids > 0 {
        ctx.skip(
            "DUPLICATE_ID",
            &format!("{unmeasured_ids} page(s) have no parsed duplicate-id evidence"),
        );
    }

    let mut unmeasured_mime = 0i64;
    let mut observed_mime = 0i64;
    for i in 0..ctx.pages.len() {
        let path = spiderhead_parse::py_url::urlsplit(&ctx.pages[i].url)
            .map(|split| split.path)
            .unwrap_or_default();
        let Some(expected) = guess_type(&path) else {
            continue;
        };
        let page = &ctx.pages[i];
        let content_type = page.rec.get("content_type").and_then(Value::as_str);
        let Some(content_type) = content_type.filter(|ct| !ct.trim().is_empty()) else {
            unmeasured_mime += 1;
            continue;
        };
        let declared = content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if !declared.contains('/') {
            unmeasured_mime += 1;
            continue;
        }
        observed_mime += 1;
        if declared != expected {
            let url = page.url.clone();
            ctx.add(
                "DECLARED_MIME_MISMATCH",
                Some(&url),
                details([
                    ("declared_content_type", declared.into()),
                    ("extension_content_type", expected.into()),
                ]),
            );
        }
    }
    if observed_mime == 0 {
        ctx.skip(
            "DECLARED_MIME_MISMATCH",
            "no URL with a recognized extension and Content-Type",
        );
    } else if unmeasured_mime > 0 {
        ctx.skip(
            "DECLARED_MIME_MISMATCH",
            &format!("{unmeasured_mime} recognized-extension URL(s) have no usable Content-Type"),
        );
    }
}
