//! Pagination checks over the rel="next"/rel="prev" declaration graph.
//!
//! live in [`graph`]; the ctx-emitting checks live in [`checks`].

mod checks;
mod graph;

pub use checks::{check_pagination_sequence, check_pagination_series};
pub use graph::{pagination_loops, pagination_page_number, series_from, PaginationLoop};

use std::collections::HashSet;

use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use spiderhead_parse::py_url::{urlsplit, urlunsplit};

use crate::context::{AddOpts, AuditContext, ContextError, PageHandle};
use crate::normalize::norm_url;
use crate::pyvalue::truthy_opt;
use crate::rules::shared::{con, probed_targets, rec, ProbedTarget};

/// `_rel_has(token)` over a declaration `rel` value that may be a string or a
/// list (reference accepts either and lowercases every token). Public so the
/// other rule domains can fold declarations the same way.
pub fn rel_has_value(value: Option<&Value>, token: &str) -> bool {
    let target = token.to_lowercase();
    match value {
        Some(Value::String(s)) => s
            .split([' ', '\t', '\r', '\n', '\x0C'])
            .any(|t| !t.is_empty() && t.to_lowercase() == target),
        Some(Value::Array(items)) => items
            .iter()
            .any(|t| t.as_str().is_some_and(|s| s.to_lowercase() == target)),
        _ => false,
    }
}

/// `_pagination_key`: native graph nodes use the collector's identities;
/// exports retain their tolerance.
pub(crate) fn pagination_key(ctx: &AuditContext, url: &str) -> Result<String, ContextError> {
    if ctx.scan_con.is_some() {
        let parts = urlsplit(url).map_err(|e| ContextError::Value(e.to_string()))?;
        let path = if parts.path.is_empty() {
            "/"
        } else {
            parts.path.as_str()
        };
        return Ok(urlunsplit(
            &parts.scheme,
            &parts.netloc,
            path,
            &parts.query,
            &parts.fragment,
        ));
    }
    Ok(norm_url(Some(url))?)
}

/// One row of `pagination_records`: the page plus the rec carrying
/// `rel_next`/`rel_prev` (normalized record in export mode, synthesized
/// declaration record for a retained native scan).
pub(crate) type PaginationRecord = (PageHandle, Map<String, Value>);

/// `pagination_records`: unique pagination declarations from selected native
/// HTML or SF rows.
pub(crate) fn pagination_records(
    ctx: &mut AuditContext,
) -> Result<Vec<PaginationRecord>, ContextError> {
    if ctx.scan_con.is_none() {
        return Ok(ctx
            .html_pages()
            .into_iter()
            .map(|page| {
                let rec = page.map(|p| rec(p).cloned().unwrap_or_default());
                (page, rec)
            })
            .collect());
    }
    let active =
        spiderhead_store::head_links::active(ctx.scan_con.as_ref().expect("scan_con checked"))
            .map_err(|e| ContextError::Value(e.to_string()))?;
    let mut out = Vec::new();
    for (source, declarations) in active {
        let Some(page) = ctx.page_by_url(&source) else {
            continue;
        };
        let eligible = page.map(|p| p.is_html() && p.is_2xx());
        let Some(declarations) = declarations else {
            continue;
        };
        if !eligible || declarations.len() >= 100 {
            continue;
        }
        out.push((page, pagination_record(&declarations)?));
    }
    Ok(out)
}

/// `_pagination_record`: fold a native declaration list into the
/// `rel_next`/`rel_prev` shape an export record already carries. A relation
/// contributes only when exactly one unique normalized target declares it.
pub(crate) fn pagination_record(
    declarations: &[Map<String, Value>],
) -> Result<Map<String, Value>, ContextError> {
    let mut rec = Map::new();
    for relation in ["next", "prev"] {
        let mut values: IndexMap<String, &Map<String, Value>> = IndexMap::new();
        for row in declarations {
            let target = row.get("target").and_then(Value::as_str).unwrap_or("");
            if !target.is_empty() && rel_has_value(row.get("rel"), relation) {
                let key = norm_url(Some(target))?;
                values.insert(key, row);
            }
        }
        if values.len() == 1 {
            let Some(&declaration) = values.values().next() else {
                continue;
            };
            rec.insert(
                format!("rel_{relation}"),
                declaration.get("target").cloned().unwrap_or(Value::Null),
            );
            rec.insert(
                format!("annotation_{relation}"),
                Value::Object(declaration.clone()),
            );
        }
    }
    Ok(rec)
}

/// A resolved pagination target: a crawled `Page` or a stored `ProbedTarget`.
pub(crate) enum Target {
    /// A page row from Internal:All.
    Page(PageHandle),
    /// A stored probe of a target the crawl never captured.
    Probe(ProbedTarget),
}

impl Target {
    fn url(&self) -> String {
        match self {
            Self::Page(h) => h.url(),
            Self::Probe(t) => t.url.clone(),
        }
    }

    fn status_code(&self) -> Option<i64> {
        match self {
            Self::Page(h) => h.map(|p| p.status_code),
            Self::Probe(t) => Some(t.status_code),
        }
    }

    fn indexability(&self) -> Option<String> {
        match self {
            Self::Page(h) => h.map(|p| p.indexability.clone()),
            Self::Probe(t) => t.indexability.clone(),
        }
    }

    fn indexability_status(&self) -> Option<String> {
        match self {
            Self::Page(h) => h.map(|p| p.indexability_status.clone()),
            Self::Probe(t) => t.indexability_status.clone(),
        }
    }

    fn is_2xx(&self) -> bool {
        match self {
            Self::Page(h) => h.map(|p| p.is_2xx()),
            Self::Probe(t) => t.is_2xx(),
        }
    }

    fn is_indexable(&self) -> bool {
        match self {
            Self::Page(h) => h.map(|p| p.is_indexable()),
            Self::Probe(t) => t.is_indexable(),
        }
    }
}

/// `check_pagination`: PAGINATION_NONINDEXABLE — a pagination page, or its
/// declared neighbor, cannot be indexed.
///
/// Two independent sources feed the same finding: a page that itself declares
/// rel="next"/rel="prev" while being non-indexable, and a rel="next"/rel="prev"
/// *target* that the crawl reached and found non-2xx — a 404/3xx/5xx page 2 is
/// exactly as unreachable as a noindex one. Only a target the crawl actually
/// captured is judged; an uncrawled URL named in the relation is left alone,
/// since nothing here knows its status.
pub fn check_pagination(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let mut reported: HashSet<String> = HashSet::new();
    let mut unmeasured = 0i64;
    let mut complete_sources = 0i64;
    for (page, rec) in pagination_records(ctx)? {
        complete_sources += 1;
        let declares = truthy_opt(rec.get("rel_next")) || truthy_opt(rec.get("rel_prev"));
        if declares && !page.map(|p| p.is_indexable()) {
            reported.insert(page.url());
            ctx.add(
                "PAGINATION_NONINDEXABLE",
                AddOpts {
                    target_url: Some(page.url()),
                    details: {
                        let mut d = Map::new();
                        d.insert(
                            "indexability_status".into(),
                            page.map(|p| p.indexability_status.clone()).into(),
                        );
                        d
                    },
                    ..AddOpts::default()
                },
            )?;
        }
    }
    for (page, rec) in pagination_records(ctx)? {
        for relation in ["rel_next", "rel_prev"] {
            let raw = rec.get(relation);
            if !truthy_opt(raw) {
                continue;
            }
            let Some(value) = raw.and_then(Value::as_str) else {
                // `norm_url(value)` calls `.strip()` — AttributeError on a
                // non-str, which the check does not catch.
                return Err(ContextError::Value(
                    "expected string for pagination declaration".to_owned(),
                ));
            };
            let norm = norm_url(Some(value))?;
            let probed: Vec<ProbedTarget> = if ctx.pages_by_norm(&norm).is_empty() {
                probed_targets(ctx, &norm)
            } else {
                Vec::new()
            };
            let target = match ctx.page_by_url(value) {
                Some(p) => Some(Target::Page(p)),
                None => match ctx.page_by_norm(&norm) {
                    Some(p) => Some(Target::Page(p)),
                    None => probed.into_iter().next().map(Target::Probe),
                },
            };
            let Some(target) = target else {
                unmeasured += 1;
                continue;
            };
            if reported.contains(&target.url()) {
                continue;
            }
            let Some(status) = target.status_code() else {
                // The row exists but its Status Code cell was blank or
                // unparseable. "Nobody measured it" is not "it is broken": the
                // same rule that forbids missing evidence reading as clean
                // forbids it reading as a defect.
                unmeasured += 1;
                continue;
            };
            if target.is_2xx() && target.indexability().is_none() {
                unmeasured += 1;
                continue;
            }
            if target.is_2xx() && target.is_indexable() {
                continue;
            }
            reported.insert(target.url());
            let target_url = match &target {
                Target::Probe(_) => page.url(),
                Target::Page(_) => target.url(),
            };
            let mut details = Map::new();
            details.insert(
                "indexability_status".into(),
                target.indexability_status().into(),
            );
            details.insert("status_code".into(), json!(status));
            details.insert(
                "relation".into(),
                json!(format!("{}\"", relation.replace("rel_", "rel=\""))),
            );
            details.insert("source_url".into(), json!(page.url()));
            details.insert("pagination_target_url".into(), json!(value));
            details.insert(
                "reason".into(),
                json!("observed target status or indexability prevents pagination"),
            );
            ctx.add(
                "PAGINATION_NONINDEXABLE",
                AddOpts {
                    target_url: Some(target_url),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
    }

    if ctx.scan_con.is_some() {
        let eligible = spiderhead_store::head_links::eligible_count(con(ctx))
            .map_err(|e| ContextError::Value(e.to_string()))?;
        if complete_sources < eligible {
            ctx.skip(
                "PAGINATION_NONINDEXABLE",
                "not measured completely by this native crawl: pagination source annotations were unavailable or reached their limit",
            );
        }
    }
    if unmeasured > 0 {
        ctx.skip(
            "PAGINATION_NONINDEXABLE",
            &format!(
                "{unmeasured} pagination target(s) have unmeasured status or indexability; \
                 native probes may lack parsed HTML"
            ),
        );
    }
    Ok(())
}
