//! Shared helpers and policy for the Internal:All rule modules.
//!
//! unparsed-body policy (a blank parser-derived value is "never measured", not
//! "observed absent"), URL utilities, uncaptured-target probes, and the
//! column-presence test behind that same distinction for whole exports.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};
use spiderhead_formats::models::Page;
use spiderhead_parse::py_url::{parse_qsl, unquote, urlencode, urlsplit, urlunsplit};

use crate::context::{AuditContext, ContextError};
use crate::normalize::{find_column, norm_url, INTERNAL_FIELD_MAP};
use crate::pyvalue::{str_or_dumps, truthy, truthy_opt};

/// `NON_ASCII`: any character outside ASCII.
pub static NON_ASCII: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\x00-\x7F]").expect("non-ascii regex"));

// Tracking/ad/click-ID parameters that create session-specific URLs and pollute
// the index when they leak into indexable pages. Whole-param-name match.
/// `TRACKING_PARAM_RE`.
pub static TRACKING_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:utm_[a-z_]+|gcl(?:id|src)|fbclid|msclkid|yclid|dclid|w?braid|_hs(?:enc|mi)|mc_[ce]id|pk_(?:campaign|kwd|source|medium|content)|vero_(?:id|conv)|trksid?|cmpid|mbid)$",
    )
    .expect("tracking-param regex")
});

/// `SESSION_PARAM_RE`.
pub static SESSION_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:sid|session(?:_?id)?|phpsessid|jsessionid|asp\.net_sessionid|cfid|cftoken)$",
    )
    .expect("session-param regex")
});

// Deliberately literal editorial/template placeholders only. Broad words such
// as "coming soon" and "sample" describe legitimate content too often to be
// audit facts; these bracketed tokens are an observed unfinished declaration.
/// `PLACEHOLDER_MARKER_RE`.
pub static PLACEHOLDER_MARKER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\{\{\s*(?:todo|tbd|placeholder)\s*\}\}|\[\s*(?:todo|tbd|placeholder)\s*\])")
        .expect("placeholder regex")
});

/// `parse_qs` keys in first-seen order (blank values dropped, as the reference
/// default `keep_blank_values=False` does).
fn parse_qs_keys(query: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for (name, _value) in parse_qsl(query) {
        if !seen.contains(&name) {
            seen.push(name);
        }
    }
    seen
}

/// `parse_qsl(query, keep_blank_values=True)`: same decoding as `parse_qsl`
/// but a `name=` pair keeps its empty value and a bare `name` yields `""`.
pub(crate) fn parse_qsl_keep_blanks(query: &str) -> Vec<(String, String)> {
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

/// `_tracking_params`: param names on `url` that look like tracking IDs
/// (empty == clean).
pub fn tracking_params(url: &str) -> Result<Vec<String>, ContextError> {
    let parts = urlsplit(url).map_err(|e| ContextError::Value(e.to_string()))?;
    if parts.query.is_empty() {
        return Ok(Vec::new());
    }
    Ok(parse_qs_keys(&parts.query)
        .into_iter()
        .filter(|k| TRACKING_PARAM_RE.is_match(k))
        .collect())
}

/// `_session_params`: known session parameter names only; values must never
/// enter audit output.
pub fn session_params(url: &str) -> Result<Vec<String>, ContextError> {
    let parts = urlsplit(url).map_err(|e| ContextError::Value(e.to_string()))?;
    Ok(parse_qsl(&parts.query)
        .into_iter()
        .filter(|(name, _value)| SESSION_PARAM_RE.is_match(name))
        .map(|(name, _value)| name)
        .collect())
}

/// `_redact_query_values`: every query value replaced by `[redacted]`.
pub fn redact_query_values(url: &str) -> Result<String, ContextError> {
    let parts = urlsplit(url).map_err(|e| ContextError::Value(e.to_string()))?;
    let query = urlencode(
        &parse_qsl_keep_blanks(&parts.query)
            .into_iter()
            .map(|(name, _value)| (name, "[redacted]".to_owned()))
            .collect::<Vec<_>>(),
    );
    Ok(urlunsplit(
        &parts.scheme,
        &parts.netloc,
        &parts.path,
        &query,
        &parts.fragment,
    ))
}

/// `_slash_key`: `(scheme, netloc, path without trailing slashes, query)` —
/// `None` for an empty or root-only path.
pub fn slash_key(url: &str) -> Result<Option<(String, String, String, String)>, ContextError> {
    let parts = urlsplit(url).map_err(|e| ContextError::Value(e.to_string()))?;
    if parts.path.is_empty() || parts.path == "/" {
        return Ok(None);
    }
    Ok(Some((
        parts.scheme.to_lowercase(),
        parts.netloc.to_lowercase(),
        parts.path.trim_end_matches('/').to_owned(),
        parts.query.clone(),
    )))
}

/// `ProbedTarget`: a canonical target no crawled page answered, as its stored
/// target probe reports it.
#[derive(Debug, Clone)]
pub struct ProbedTarget {
    /// The probed URL.
    pub url: String,
    /// HTTP status code the probe observed.
    pub status_code: i64,
    /// Where the probe was redirected, or "".
    pub redirect_url: String,
    /// `Indexable`/`Non-Indexable` when the probe measured it.
    pub indexability: Option<String>,
    /// Reason behind the indexability verdict.
    pub indexability_status: Option<String>,
}

impl ProbedTarget {
    /// `is_2xx`.
    pub fn is_2xx(&self) -> bool {
        (200..300).contains(&self.status_code)
    }

    /// `is_indexable`.
    pub fn is_indexable(&self) -> bool {
        self.is_2xx() && self.indexability.as_deref() == Some("Indexable")
    }
}

/// `_probed_targets`: stored probe of an uncaptured target (#987); empty when
/// none answered with a status.
pub fn probed_targets(ctx: &AuditContext, key: &str) -> Vec<ProbedTarget> {
    let Some(payload) = ctx.target_probes.get(key).and_then(Value::as_object) else {
        return Vec::new();
    };
    // `type(payload.get("status_code")) is not int` — a bool or float does not count.
    if !matches!(payload.get("status_code"), Some(Value::Number(n)) if n.is_i64() || n.is_u64()) {
        return Vec::new();
    }
    let observed = spiderhead_store::target_probes::indexability(payload);
    let indexability = match observed.get("state").map(String::as_str) {
        Some("indexable") => Some("Indexable".to_owned()),
        Some("non_indexable") => Some("Non-Indexable".to_owned()),
        _ => None,
    };
    vec![ProbedTarget {
        url: payload
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        status_code: payload
            .get("status_code")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        redirect_url: payload
            .get("redirect_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        indexability,
        indexability_status: observed.get("reason").cloned(),
    }]
}

/// The native scan connection when the context carries one. Callers gate on
/// `ctx.scan_con.is_some()`; the `&Connection` borrow must live inside the
/// call expression because holding it across `&mut ctx` calls is a borrow
/// error — `if let Some(con) = &ctx.scan_con` cannot span a loop that adds
/// issues.
pub(crate) fn con(ctx: &AuditContext) -> &rusqlite::Connection {
    ctx.scan_con.as_ref().expect("scan_con present")
}

/// `_rec`: the raw normalized record kept under `metrics["_record"]`.
pub fn rec(page: &Page) -> Option<&Map<String, Value>> {
    page.metrics.get("_record").and_then(Value::as_object)
}

/// The empty record, for callers that read a missing `_record` like the reference
/// `metrics.get("_record", {})`.
pub fn rec_or_empty(page: &Page) -> Map<String, Value> {
    rec(page).cloned().unwrap_or_default()
}

/// `_body_unavailable`: whether this row's HTML body was not parsed or
/// retained.
///
/// A blank title/description/h1/canonical on such a row means "never
/// measured", not "observed absent" — the same distinction `has_column` draws
/// for a column missing from the whole export, but here it is one row at a
/// time. The marker covers both the existing size limit and an explicit crawl
/// media-type filter.
pub fn body_unavailable(rec: &Map<String, Value>) -> bool {
    truthy_opt(rec.get("body_unavailable"))
}

/// `_body_unavailable_reason`: one sentence naming why the given pages were
/// never parsed, or `None` when they all were.
pub fn body_unavailable_reason<'a>(pages: impl Iterator<Item = &'a Page>) -> Option<String> {
    let mut reasons: HashMap<String, i64> = HashMap::new();
    for page in pages {
        // reference `str(rec.get("body_unavailable") or "")`: a falsy marker reads
        // back as "" and is skipped; a truthy one is `str()`-ed.
        let reason = rec(page)
            .and_then(|r| r.get("body_unavailable"))
            .filter(|v| truthy(v))
            .map(str_or_dumps);
        if let Some(reason) = reason {
            *reasons.entry(reason).or_insert(0) += 1;
        }
    }
    let count: i64 = reasons.values().sum();
    if reasons.len() == 1 && reasons.contains_key("oversized") {
        Some(format!(
            "{count} page(s) with an oversized, unparsed HTML body \
             -- this metadata was never measured"
        ))
    } else if count > 0 {
        let labels: HashMap<&str, &str> = HashMap::from([
            ("excluded_by_media_type", "excluded by media type"),
            ("not_included_by_media_type", "not included by media type"),
            (
                "media_type_unavailable",
                "missing or malformed Content-Type",
            ),
            ("oversized", "oversized HTML body"),
        ]);
        let mut items: Vec<(String, i64)> = reasons.into_iter().collect();
        items.sort();
        let summary = items
            .iter()
            .map(|(reason, amount)| {
                format!(
                    "{}: {}",
                    labels.get(reason.as_str()).unwrap_or(&reason.as_str()),
                    amount
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!(
            "{count} page(s) were not parsed ({summary}); this metadata was never measured"
        ))
    } else {
        None
    }
}

/// `_skip_for_body_unavailable`: name why unparsed HTML cannot contribute a
/// verdict to `check_id`.
pub fn skip_for_body_unavailable(ctx: &mut AuditContext, check_id: &str, pages: &[Page]) {
    let reason = body_unavailable_reason(pages.iter());
    if let Some(reason) = reason {
        ctx.skip(check_id, &reason);
    }
}

// A blank parser-derived column is not a negative result when HTML was not
// parsed. Keep this inventory next to the shared unavailable-body policy so
// new on-page rules can be checked against it during review. Checks that also
// have evidence elsewhere retain their normal verdict: AuditContext.add
// retracts a check's skip when that check actually fires.
/// `_BODY_DERIVED_INDEXABLE_CHECKS`.
pub const BODY_DERIVED_INDEXABLE_CHECKS: &[&str] = &[
    "NON_INDEXABLE_LINKED",
    "TITLE_MISSING",
    "TITLE_TOO_LONG",
    "TITLE_TOO_SHORT",
    "TITLE_EQUALS_H1",
    "TITLE_DUPLICATE",
    "DESC_MULTIPLE",
    "DESC_MISSING",
    "DESC_TOO_LONG",
    "DESC_TOO_SHORT",
    "DESC_DUPLICATE",
    "H1_MISSING",
    "H1_MULTIPLE",
    "H1_TOO_LONG",
    "H1_DUPLICATE",
    "H1_ALT_TEXT_ONLY",
    "H2_MISSING",
    "H2_TOO_LONG",
    "H2_DUPLICATE",
    "TITLE_OUTSIDE_HEAD",
    "DESC_OUTSIDE_HEAD",
    "CANONICAL_MISSING",
    "HEADING_BEFORE_H1",
    "HEADING_IN_PAGE_CHROME",
    "HEADING_SKIP",
    "LINK_INSIDE_HEADING",
    "IMAGE_LINK_WITHOUT_TEXT",
    "NO_INTERNAL_OUTLINKS",
    "HIGH_OUTLINKS",
    "HIGH_EXTERNAL_OUTLINKS",
    "URL_HAS_PARAMS",
    "CONTENT_IN_IFRAME",
    "THIN_CONTENT",
    "LOW_TEXT_RATIO",
    "HTML_BLOAT",
    "TITLE_TEMPLATED",
    "NEAR_DUPLICATE",
    "OG_MISSING",
];

/// `_BODY_DERIVED_HTML_CHECKS`.
pub const BODY_DERIVED_HTML_CHECKS: &[&str] = &[
    "MIXED_LINK_REL",
    "OUTBOUND_SPONSORED_WITHOUT_DISCLOSURE",
    "CANONICALISED",
    "CANONICAL_NON_INDEXABLE",
    "CANONICAL_RELATIVE",
    "CANONICAL_FRAGMENT",
    "CANONICAL_CHAIN",
    "CANONICAL_TO_REDIRECT",
    "UNLINKED_CANONICAL",
    "DIRECTIVES_OUTSIDE_HEAD",
    "NOINDEX",
    "NOFOLLOW_PAGE",
    "NOARCHIVE",
    "NOSNIPPET",
    "NOIMAGEINDEX",
    "NOTRANSLATE",
    "NOODP",
    "NOYDIR",
    "UNAVAILABLE_AFTER",
    "HREFLANG_OUTSIDE_HEAD",
    "META_REFRESH_REDIRECT",
    "CANONICAL_OUTSIDE_HEAD",
    "STRUCTURED_DATA_PARSE_ERROR",
    "LOREM_IPSUM_PLACEHOLDER",
    "UNSUPPORTED_PLUGIN",
    "IMG_MISSING_ALT_ATTRIBUTE",
    "IMG_ALT_TOO_LONG",
    "AJAX_CRAWLING_SCHEME_URL",
    "AJAX_CRAWLING_SCHEME_META_FRAGMENT",
    "HREFLANG_BROKEN_TARGET",
    "HREFLANG_UNLINKED_TARGET",
    "HREFLANG_INVALID_CODE",
    "HREFLANG_MULTIPLE_ENTRIES",
    "HREFLANG_MISSING_SELF_REFERENCE",
    "HREFLANG_MISSING_XDEFAULT",
    "HREFLANG_NOT_CANONICAL",
    "HREFLANG_MISSING_RETURN_LINK",
    "BROKEN_INTERNAL_LINK",
    "LINK_TO_5XX",
    "INTERNAL_LINK_TO_REDIRECT",
    "BROKEN_EXTERNAL_LINK",
    "EXTERNAL_LINK_TO_REDIRECT",
    "GENERIC_ANCHOR_TEXT",
    "LOW_LINK_SCORE",
    "ONLY_NOFOLLOW_INLINKS",
    "ONLY_NONINDEXABLE_SOURCE_INLINKS",
    "DEEP_DISCOVERY_PATH",
    "DEEP_CLICK_DEPTH",
    "DUPLICATE_INTERNAL_LINK",
    "INSECURE_SUBRESOURCE",
    "PAGINATION_MULTIPLE",
    "PAGINATION_URL_NOT_IN_ANCHOR",
    "PAGINATION_SEQUENCE_ERROR",
    "PAGINATION_LOOP",
    "UNLINKED_PAGINATION_SERIES",
    "PAGINATION_NONINDEXABLE",
    "MISSING_DOCTYPE",
    "INVALID_DOCTYPE",
    "VIEWPORT_MISSING",
    "INVALID_VIEWPORT",
    "RENDER_BLOCKING",
    "HEAD_MISSING",
    "HEAD_MULTIPLE",
    "BODY_MISSING",
    "BODY_MULTIPLE",
    "INVALID_HEAD_ELEMENT",
    "HEAD_NOT_FIRST",
];

/// `_canonical_policy_target_is_fetched`: whether this exact policy URL has a
/// response row in Internal:All.
///
/// The general URL index folds trailing slashes for link matching. A declared
/// policy target is stricter: a slashless redirect does not prove that its
/// distinct slash target was fetched. Rows without a status are placeholders,
/// not target-access evidence.
pub fn canonical_policy_target_is_fetched(
    ctx: &mut AuditContext,
    url: &str,
) -> Result<bool, ContextError> {
    let Ok(target) = urlsplit(url) else {
        return Ok(false);
    };
    let target_path = if target.path.is_empty() {
        "/".to_owned()
    } else {
        target.path.clone()
    };
    let target_identity = (
        target.scheme.to_lowercase(),
        target.netloc.to_lowercase(),
        target_path,
        target.query.clone(),
        target.fragment.clone(),
    );
    let norm = norm_url(Some(url))?;
    for handle in ctx.pages_by_norm(&norm) {
        let found = handle.map(|page| {
            if page.status_code.is_none() {
                return false;
            }
            let Ok(candidate) = urlsplit(&page.url) else {
                return false;
            };
            let candidate_path = if candidate.path.is_empty() {
                "/".to_owned()
            } else {
                candidate.path.clone()
            };
            (
                candidate.scheme.to_lowercase(),
                candidate.netloc.to_lowercase(),
                candidate_path,
                candidate.query.clone(),
                candidate.fragment.clone(),
            ) == target_identity
        });
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `_has_column`: whether `Internal:All` carries the source column for `field`
/// at all.
///
/// Same distinction as #205 (see check_titles): an absent column and a column
/// full of blanks both read back as `None`/falsy per page, but they are
/// different facts — one means the run never measured this, the other that
/// every page genuinely fails. Only the column's presence, never a passing
/// value showing up somewhere in the corpus, may decide whether a check ran.
pub fn has_column(ctx: &AuditContext, field: &str) -> bool {
    let Some(df) = &ctx.internal_df else {
        return false;
    };
    let Some(candidates) = INTERNAL_FIELD_MAP
        .iter()
        .find(|(name, _)| *name == field)
        .map(|(_, c)| *c)
    else {
        return false;
    };
    find_column(df.columns(), candidates).is_some()
}

/// `_path_of`: the path only — query/fragment must not leak into path-based
/// checks.
pub fn path_of(url: &str) -> Result<String, ContextError> {
    urlsplit(url)
        .map(|p| p.path)
        .map_err(|e| ContextError::Value(e.to_string()))
}

/// `_decoded_path`: the path as a reader sees it, with percent-escapes
/// resolved.
///
/// Exports carry the URL as crawled, so a path written in a non-Latin script
/// arrives percent-encoded, one three-character escape per byte. RFC 3986
/// prefers uppercase hex digits in those escapes, which makes the encoded form
/// of every such URL look uppercase and none of them look non-ASCII — the
/// exact opposite of the truth. Both questions are about the characters the
/// path denotes, so both are asked of the decoded form.
pub fn decoded_path(url: &str) -> Result<String, ContextError> {
    Ok(unquote(&path_of(url)?))
}
