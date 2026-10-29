//! Static Lighthouse re-implementations (issue #59) plus `check_tech_extra`:
//! charset, doctype, viewport and compression verdicts read from the columns
//! a native crawl projects (or Custom Extraction supplies).

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};
use spiderhead_parse::py_str::py_strip;

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::pyvalue::{truthy, truthy_opt};
use crate::rules::shared::{body_unavailable, has_column, rec};

use super::{ordered_operand, str_or_blank};

/// `check_tech_extra`.
pub fn check_tech_extra(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let has_http_version = has_column(ctx, "http_version");
    let has_amphtml = has_column(ctx, "amphtml");
    for page in ctx.html_pages() {
        let rec_obj = page.map(|p| rec(p).cloned().unwrap_or_default());
        // SF emits HTTP Version as "HTTP/1.1", "HTTP/2" or bare "1.1"/"2"; str() because
        // an all-numeric column parses as float.
        let hv = py_strip(
            &str_or_blank(rec_obj.get("http_version"))
                .to_uppercase()
                .replace("HTTP/", ""),
        )
        .to_owned();
        if has_http_version && hv.starts_with('1') {
            let mut details = Map::new();
            details.insert(
                "http_version".into(),
                rec_obj.get("http_version").cloned().unwrap_or(Value::Null),
            );
            ctx.add(
                "HTTP1_ONLY",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
        if has_amphtml && truthy_opt(rec_obj.get("amphtml")) {
            let mut details = Map::new();
            details.insert(
                "amphtml".into(),
                rec_obj.get("amphtml").cloned().unwrap_or(Value::Null),
            );
            ctx.add(
                "AMPHTML_PRESENT",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
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
    Ok(())
}

// --------------------------------------------------------------------------
// Static Lighthouse audits (issue #59) — see the reference suite for which audit id
// each check corresponds to and its documentation link. None of these run a
// browser or a performance trace, and none of them contributes to, or can be
// mistaken for, a Lighthouse Performance score: each is a from-the-description
// reimplementation of one narrow, documented rule against the response and
// markup a crawl already has.
//
// None of the four fields these checks read (Content-Encoding, Doctype,
// Viewport, Meta Charset) is a default crawler export column; a native
// seohead crawl (seohead.crawl.evidence) always populates them, an SF export
// only does with matching Custom Extraction configured. Each check follows
// check_og's honesty contract: if no page in the run carries the evidence at
// all, it skips by name rather than flag every page.
// --------------------------------------------------------------------------

/// `_CHARSET_IN_HEADER_RE`.
static CHARSET_IN_HEADER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)charset\s*=").expect("charset-in-header re"));

// The encoding a Content-Type header names, when it names one
// ("text/html; charset=iso-8859-1"). _CHARSET_IN_HEADER_RE only detects the
// parameter; judging its value needs the value itself.
/// `_HEADER_CHARSET_VALUE_RE`.
static HEADER_CHARSET_VALUE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)charset\s*=\s*"?([^\s";]+)"#).expect("header-charset re"));

// The one encoding the HTML standard tells browsers to assume and the Encoding
// Standard treats as canonical (#1576). Anything else is a legacy declaration:
// the value the page actually declared is kept in the finding's details.
/// `_UTF8_CHARSETS`.
const UTF8_CHARSETS: [&str; 2] = ["utf-8", "utf8"];

/// `check_charset`: Lighthouse `charset`, split by what is wrong (#1576).
///
/// MISSING_CHARSET keeps the audit's absent half (no encoding declared via
/// Content-Type or an early <meta> tag). INVALID_CHARSET carries the
/// recommendation the audit does not state: an encoding that *is* declared but
/// is not UTF-8. The HTML declaration wins over the header when both exist --
/// that is the value a browser uses for the document itself.
pub fn check_charset(ctx: &mut AuditContext) -> Result<(), ContextError> {
    // Content-Type is a base column every crawl already carries and is read for its own
    // sake below; Meta Charset is the one column gated here, present only via a native
    // crawl or Custom Extraction in SF. Gating on its presence rather than on any page
    // happening to carry a truthy value means an all-negative corpus (no header charset,
    // no meta charset anywhere) is a real finding, not missing evidence (#268) — while an
    // export that never carries the column at all still honestly skips.
    if !has_column(ctx, "meta_charset") {
        // Without this column, absence is unmeasurable rather than absent. A page
        // whose Content-Type carries no charset may still declare <meta charset>
        // in its HTML, which an export without the column does not show, so
        // firing on the header alone reports a defect nobody measured. Gating on
        // any page *happening* to carry a header charset -- which is what this
        // check did before #396 -- decided that question with evidence that
        // cannot answer it.
        //
        // The reason says which column is missing, not that Content-Type carries
        // no charset: on a run where some pages do carry one, that sentence was
        // simply untrue, and a skip reason a reader cannot trust is worth less
        // than no reason at all. INVALID_CHARSET skips under the same gate: its
        // verdict names the encoding the page declares, and without the column
        // only the header's half of that declaration is visible.
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
        return Ok(());
    }
    for page in ctx.html_pages() {
        let (rec_obj, content_type) =
            page.map(|p| (rec(p).cloned().unwrap_or_default(), p.content_type.clone()));
        if body_unavailable(&rec_obj) {
            continue; // unparsed body: no meta declaration to read either way
        }
        let meta_charset = py_strip(&str_or_blank(rec_obj.get("meta_charset"))).to_owned();
        let header_declares = content_type
            .as_deref()
            .is_some_and(|ct| !ct.is_empty() && CHARSET_IN_HEADER_RE.is_match(ct));
        let value_match = header_declares
            .then(|| {
                content_type
                    .as_deref()
                    .and_then(|ct| HEADER_CHARSET_VALUE_RE.captures(ct))
            })
            .flatten();
        let header_charset = value_match
            .and_then(|m| {
                m.get(1)
                    .map(|g| g.as_str().trim_matches(['"', '\'']).to_owned())
            })
            .unwrap_or_default();
        if meta_charset.is_empty() && !header_declares {
            let mut details = Map::new();
            details.insert(
                "content_type".into(),
                content_type.map(Value::String).unwrap_or(Value::Null),
            );
            ctx.add(
                "MISSING_CHARSET",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
            continue;
        }
        if meta_charset.is_empty() && header_charset.is_empty() {
            continue; // a bare "charset=" names no encoding; there is nothing to judge
        }
        let (declared, declared_by) = if !meta_charset.is_empty() {
            (meta_charset, "meta")
        } else {
            (header_charset, "header")
        };
        if !UTF8_CHARSETS.contains(&declared.to_lowercase().as_str()) {
            let mut details = Map::new();
            details.insert("charset".into(), Value::String(declared));
            details.insert("declared_by".into(), Value::String(declared_by.to_owned()));
            ctx.add(
                "INVALID_CHARSET",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    Ok(())
}

/// `_DOCTYPE_NAME_RE` — `re.match` anchors at the string start.
static DOCTYPE_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^<!DOCTYPE\s+([a-zA-Z0-9]+)").expect("doctype re"));

/// `check_doctype`: Lighthouse `doctype`, split by what is wrong (#1576).
///
/// MISSING_DOCTYPE keeps the audit's absent half. INVALID_DOCTYPE names the
/// present-but-not-HTML5 half that used to be reported under the missing name:
/// a legacy PUBLIC/SYSTEM identifier or another root name, either of which
/// still triggers quirks mode.
pub fn check_doctype(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if !has_column(ctx, "doctype") {
        let reason = "no Doctype column (needs a native seohead crawl or Custom Extraction in SF)";
        ctx.skip("MISSING_DOCTYPE", reason);
        ctx.skip("INVALID_DOCTYPE", reason);
        return Ok(());
    }
    for page in ctx.html_pages() {
        let rec_obj = page.map(|p| rec(p).cloned().unwrap_or_default());
        if body_unavailable(&rec_obj) {
            continue;
        }
        let raw = rec_obj.get("doctype").cloned().unwrap_or(Value::Null);
        if !truthy(&raw) {
            let mut details = Map::new();
            details.insert("reason".into(), Value::String("no doctype".to_owned()));
            ctx.add(
                "MISSING_DOCTYPE",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
            continue;
        }
        let Some(raw_str) = raw.as_str() else {
            // `raw.lower()` / `re.match(raw)` raise on a non-str in reference;
            // the check does not catch it.
            return Err(ContextError::Value(
                "expected string for doctype evidence".to_owned(),
            ));
        };
        let name = DOCTYPE_NAME_RE
            .captures(raw_str)
            .and_then(|m| m.get(1).map(|g| g.as_str().to_lowercase()))
            .unwrap_or_default();
        let lowered = raw_str.to_lowercase();
        let has_legacy_identifier = lowered.contains("public") || lowered.contains("system");
        if name != "html" || has_legacy_identifier {
            let mut details = Map::new();
            details.insert("doctype".into(), raw);
            ctx.add(
                "INVALID_DOCTYPE",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    Ok(())
}

/// `_INITIAL_SCALE_RE`.
static INITIAL_SCALE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)initial-scale\s*=\s*([0-9.]+)").expect("initial-scale re"));

/// `_VIEWPORT_WIDTH_RE`.
static VIEWPORT_WIDTH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bwidth\s*=").expect("viewport-width re"));

/// `check_mobile_alternate`: MOBILE_ALTERNATE_LINK — a media-qualified
/// rel=alternate that names no URL (#1016).
pub fn check_mobile_alternate(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if !has_column(ctx, "mobile_alternate_broken") {
        ctx.skip(
            "MOBILE_ALTERNATE_LINK",
            "no rel=alternate media evidence (native crawl only)",
        );
        return Ok(());
    }
    for page in ctx.html_pages() {
        let rec_obj = page.map(|p| rec(p).cloned().unwrap_or_default());
        if body_unavailable(&rec_obj) {
            continue;
        }
        let count = rec_obj
            .get("mobile_alternate_broken")
            .cloned()
            .unwrap_or(Value::Null);
        let count = if truthy(&count) {
            count
        } else {
            Value::from(0)
        };
        if ordered_operand(&count)? > 0.0 {
            let mut details = Map::new();
            details.insert("occurrences".into(), count);
            details.insert(
                "reason".into(),
                Value::String("rel=alternate with media has no href".to_owned()),
            );
            ctx.add(
                "MOBILE_ALTERNATE_LINK",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    Ok(())
}

/// `check_viewport`: Lighthouse `viewport` split (#1576): absence vs. a
/// declaration that cannot work.
///
/// MISSING_VIEWPORT keeps the audit's absent half. INVALID_VIEWPORT names a tag
/// that exists but is unusable as written: it pins initial-scale below 1, or it
/// names neither a width nor an initial-scale at all.
pub fn check_viewport(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if !has_column(ctx, "viewport") {
        let reason = "no Viewport column (needs a native seohead crawl or Custom Extraction in SF)";
        ctx.skip("VIEWPORT_MISSING", reason);
        ctx.skip("INVALID_VIEWPORT", reason);
        return Ok(());
    }
    for page in ctx.html_pages() {
        let rec_obj = page.map(|p| rec(p).cloned().unwrap_or_default());
        if body_unavailable(&rec_obj) {
            continue;
        }
        let content = rec_obj.get("viewport").cloned().unwrap_or(Value::Null);
        if truthy(&content) && !content.is_string() {
            // `re.search` on a non-str raises TypeError; the check does not
            // catch it.
            return Err(ContextError::Value(
                "expected string for viewport regex".to_owned(),
            ));
        }
        if !truthy(&content) {
            let mut details = Map::new();
            details.insert(
                "reason".into(),
                Value::String("no viewport meta tag".to_owned()),
            );
            ctx.add(
                "VIEWPORT_MISSING",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
            continue;
        }
        let content_str = content.as_str().unwrap_or("");
        let scale_match = INITIAL_SCALE_RE.captures(content_str);
        if let Some(m) = scale_match {
            // `float(group(1))` — a capture like "1.2.3" is a ValueError in
            // reference, and the check does not catch it.
            let scale = m
                .get(1)
                .and_then(|g| crate::pyvalue::py_float(g.as_str()))
                .ok_or_else(|| {
                    ContextError::Value("could not convert initial-scale to float".to_owned())
                })?;
            if scale < 1.0 {
                let mut details = Map::new();
                details.insert("viewport".into(), content);
                details.insert(
                    "reason".into(),
                    Value::String("initial-scale below 1".to_owned()),
                );
                ctx.add(
                    "INVALID_VIEWPORT",
                    AddOpts {
                        target_url: Some(page.url()),
                        details,
                        ..AddOpts::default()
                    },
                )?;
            }
        } else if !VIEWPORT_WIDTH_RE.is_match(content_str) {
            let mut details = Map::new();
            details.insert("viewport".into(), content);
            details.insert(
                "reason".into(),
                Value::String("no width or initial-scale".to_owned()),
            );
            ctx.add(
                "INVALID_VIEWPORT",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
    }
    Ok(())
}

// Lighthouse's own ignore threshold for `uses-text-compression`: below this
// many bytes, compressing the response saves too little to report.
/// `_COMPRESSION_IGNORE_BYTES`.
const COMPRESSION_IGNORE_BYTES: f64 = 1400.0;

/// `_COMPRESSED_ENCODINGS`.
const COMPRESSED_ENCODINGS: [&str; 4] = ["gzip", "br", "deflate", "zstd"];

/// `check_compression`: Lighthouse `uses-text-compression` (now
/// `document-latency-insight`): see the reference suite.
pub fn check_compression(ctx: &mut AuditContext) -> Result<(), ContextError> {
    if !has_column(ctx, "content_encoding") {
        ctx.skip(
            "NO_COMPRESSION",
            "no Content-Encoding column (needs a native seohead crawl or Custom Extraction in SF)",
        );
        return Ok(());
    }
    for page in ctx.html_pages() {
        let rec_obj = page.map(|p| rec(p).cloned().unwrap_or_default());
        // Content-Encoding is comma-separated when codings are stacked (e.g. "gzip, br"
        // from a CDN re-compressing an already-gzipped origin response) — comparing the
        // whole header string against a single-token set flagged every stacked value as
        // uncompressed even though each listed token is a real compression coding (#269).
        let tokens: Vec<String> = str_or_blank(rec_obj.get("content_encoding"))
            .split(',')
            .map(|t| py_strip(t).to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        if tokens
            .iter()
            .any(|t| COMPRESSED_ENCODINGS.contains(&t.as_str()))
        {
            continue;
        }
        // A missing Size (bytes) column is not "0 bytes, too small to matter" -- it is
        // unmeasured evidence, and applying the ignore threshold to it would let an
        // uncompressed page hide behind a size nobody actually observed (#445). Only
        // apply the threshold when size was genuinely measured.
        let size = rec_obj.get("size_bytes").cloned().unwrap_or(Value::Null);
        if !size.is_null() && ordered_operand(&size)? < COMPRESSION_IGNORE_BYTES {
            continue;
        }
        let mut details = Map::new();
        details.insert(
            "content_encoding".into(),
            rec_obj
                .get("content_encoding")
                .cloned()
                .unwrap_or(Value::Null),
        );
        details.insert("size_bytes".into(), size);
        ctx.add(
            "NO_COMPRESSION",
            AddOpts {
                target_url: Some(page.url()),
                details,
                ..AddOpts::default()
            },
        )?;
    }
    Ok(())
}
