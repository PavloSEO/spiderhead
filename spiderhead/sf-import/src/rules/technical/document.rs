//! Element position & document skeleton (issue #123) and `check_html_structure`.
//!
//! A browser closes `<head>` at the first element that does not belong there,
//! and everything after that point is read from `<body>` instead. Crawler
//! exports do not carry element position at all: the signal exists only where
//! the native parse resolved the tree.

use serde_json::{Map, Value};
use spiderhead_parse::py_str::py_strip;

use crate::context::{AddOpts, AuditContext, ContextError};
use crate::normalize::{find_column, INTERNAL_FIELD_MAP};
use crate::pyvalue::{py_eq, truthy, truthy_opt};
use crate::rules::shared::{body_unavailable, has_column, rec};

use super::{ordered_operand, str_or_blank};

// --------------------------------------------------------------------------
// Element position & document skeleton (issue #123)
//
// A browser closes <head> at the first element that does not belong there, and
// everything after that point is read from <body> instead — a canonical or a
// robots directive placed there silently stops applying, while the source text
// still looks fine. Crawler exports do not carry this at all: the signal
// exists only where seohead.checks.parser.parse_html resolved the tree (see its
// module docstring for what was verified against lxml directly), so — like the
// static Lighthouse audits just above — these need a native seohead crawl.
// --------------------------------------------------------------------------

/// `_ELEMENT_POSITION_CHECKS`.
const ELEMENT_POSITION_CHECKS: [(&str, &str); 5] = [
    ("title_outside_head", "TITLE_OUTSIDE_HEAD"),
    ("meta_description_outside_head", "DESC_OUTSIDE_HEAD"),
    ("canonical_outside_head", "CANONICAL_OUTSIDE_HEAD"),
    ("directives_outside_head", "DIRECTIVES_OUTSIDE_HEAD"),
    ("hreflang_outside_head", "HREFLANG_OUTSIDE_HEAD"),
];

// Title/description ask about the page's own indexable content, matching
// check_titles/check_descriptions; canonical/directives/hreflang matter on any
// HTML page, matching check_canonical_directives.
/// `_ELEMENT_POSITION_ON_INDEXABLE_ONLY`.
const ELEMENT_POSITION_ON_INDEXABLE_ONLY: [&str; 2] =
    ["title_outside_head", "meta_description_outside_head"];

/// `_SKELETON_CHECKS`.
const SKELETON_CHECKS: [&str; 6] = [
    "HEAD_MISSING",
    "HEAD_MULTIPLE",
    "BODY_MISSING",
    "BODY_MULTIPLE",
    "INVALID_HEAD_ELEMENT",
    "HEAD_NOT_FIRST",
];

/// `_SKELETON_FIELDS`.
const SKELETON_FIELDS: [&str; 4] = [
    "head_count",
    "body_count",
    "head_not_first",
    "invalid_head_elements",
];

/// `_NO_POSITION_EVIDENCE`.
const NO_POSITION_EVIDENCE: &str =
    "no element-position evidence (needs a native seohead crawl; crawler exports do not \
     record where an element sits)";

/// `check_element_position`: outside-<head> checks for title, description,
/// canonical, directives, and hreflang.
pub fn check_element_position(ctx: &mut AuditContext) -> Result<(), ContextError> {
    for (field, check_id) in ELEMENT_POSITION_CHECKS {
        let candidates = INTERNAL_FIELD_MAP
            .iter()
            .find(|(name, _)| *name == field)
            .map(|(_, c)| *c)
            .unwrap_or(&[]);
        if ctx.internal_df.is_none()
            || find_column(
                ctx.internal_df.as_ref().map(|d| d.columns()).unwrap_or(&[]),
                candidates,
            )
            .is_none()
        {
            ctx.skip(check_id, NO_POSITION_EVIDENCE);
            continue;
        }
        let pages = if ELEMENT_POSITION_ON_INDEXABLE_ONLY.contains(&field) {
            ctx.indexable_html_pages()
        } else {
            ctx.html_pages()
        };
        for page in pages {
            if page.map(|p| truthy_opt(rec(p).and_then(|r| r.get(field)))) {
                ctx.add(
                    check_id,
                    AddOpts {
                        target_url: Some(page.url()),
                        ..AddOpts::default()
                    },
                )?;
            }
        }
    }
    Ok(())
}

/// `check_document_skeleton`: document-skeleton validity — <head>/<body>
/// presence, count, and order.
///
/// One finding per page, never one per stray element — a page with two
/// <body> tags is a single BODY_MULTIPLE, not one per tag.
pub fn check_document_skeleton(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let has_evidence = ctx.internal_df.is_some()
        && SKELETON_FIELDS.iter().all(|field| {
            let candidates = INTERNAL_FIELD_MAP
                .iter()
                .find(|(name, _)| name == field)
                .map(|(_, c)| *c)
                .unwrap_or(&[]);
            find_column(
                ctx.internal_df.as_ref().map(|d| d.columns()).unwrap_or(&[]),
                candidates,
            )
            .is_some()
        });
    if !has_evidence {
        for check_id in SKELETON_CHECKS {
            ctx.skip(check_id, NO_POSITION_EVIDENCE);
        }
        return Ok(());
    }
    for page in ctx.html_pages() {
        let rec_obj = page.map(|p| rec(p).cloned().unwrap_or_default());
        if body_unavailable(&rec_obj) {
            continue;
        }
        let head_count = rec_obj.get("head_count").cloned().unwrap_or(Value::Null);
        let head_count = if truthy(&head_count) {
            head_count
        } else {
            Value::from(0)
        };
        let body_count = rec_obj.get("body_count").cloned().unwrap_or(Value::Null);
        let body_count = if truthy(&body_count) {
            body_count
        } else {
            Value::from(0)
        };
        if py_eq(&head_count, &Value::from(0)) {
            ctx.add(
                "HEAD_MISSING",
                AddOpts {
                    target_url: Some(page.url()),
                    ..AddOpts::default()
                },
            )?;
        } else if ordered_operand(&head_count)? > 1.0 {
            let mut details = Map::new();
            details.insert("head_count".into(), head_count);
            ctx.add(
                "HEAD_MULTIPLE",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
        if py_eq(&body_count, &Value::from(0)) {
            ctx.add(
                "BODY_MISSING",
                AddOpts {
                    target_url: Some(page.url()),
                    ..AddOpts::default()
                },
            )?;
        } else if ordered_operand(&body_count)? > 1.0 {
            let mut details = Map::new();
            details.insert("body_count".into(), body_count);
            ctx.add(
                "BODY_MULTIPLE",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
        let invalid = str_or_blank(rec_obj.get("invalid_head_elements"));
        if !invalid.is_empty() {
            let elements: Vec<Value> = invalid
                .split(',')
                .map(|e| py_strip(e).to_owned())
                .filter(|e| !e.is_empty())
                .map(Value::String)
                .collect();
            let mut details = Map::new();
            details.insert("elements".into(), Value::Array(elements));
            ctx.add(
                "INVALID_HEAD_ELEMENT",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
        }
        if truthy_opt(rec_obj.get("head_not_first")) {
            ctx.add(
                "HEAD_NOT_FIRST",
                AddOpts {
                    target_url: Some(page.url()),
                    ..AddOpts::default()
                },
            )?;
        }
    }
    Ok(())
}

/// `check_html_structure`: DUPLICATE_ID and DECLARED_MIME_MISMATCH from
/// bounded observed facts (#828).
///
/// Repeated ids need the parsed DOM, so an export without the native column
/// is explicitly skipped. MIME is deliberately narrower: it only compares
/// unambiguous filename extensions with a received Content-Type family and
/// names absent/malformed headers as unmeasured instead of guessing.
pub fn check_html_structure(ctx: &mut AuditContext) -> Result<(), ContextError> {
    let has_duplicate_ids = has_column(ctx, "duplicate_ids");
    if !has_duplicate_ids {
        ctx.skip(
            "DUPLICATE_ID",
            "no duplicate-id evidence (native crawl only)",
        );
    }
    let mut unmeasured_ids = 0i64;
    if has_duplicate_ids {
        for page in ctx.html_pages() {
            let rec_obj = page.map(|p| rec(p).cloned().unwrap_or_default());
            let values = rec_obj.get("duplicate_ids");
            if body_unavailable(&rec_obj) || !matches!(values, Some(Value::Array(_))) {
                unmeasured_ids += 1;
                continue;
            }
            for item in values.and_then(Value::as_array).into_iter().flatten() {
                let Some(item) = item.as_object() else {
                    continue;
                };
                let Some(count) = item.get("count").and_then(Value::as_i64) else {
                    continue;
                };
                let Some(identifier) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if count > 1 {
                    let mut details = Map::new();
                    details.insert("id".into(), Value::String(identifier.to_owned()));
                    details.insert("count".into(), Value::from(count));
                    ctx.add(
                        "DUPLICATE_ID",
                        AddOpts {
                            target_url: Some(page.url()),
                            occurrences_count: Some(count),
                            details,
                            ..AddOpts::default()
                        },
                    )?;
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
    for page in ctx.pages_iter() {
        let path = crate::rules::shared::path_of(&page.url())?;
        let Some(expected) = crate::mime_guess::guess_type(&path) else {
            continue;
        };
        if expected.is_empty() {
            continue;
        }
        let rec_obj = page.map(|p| rec(p).cloned().unwrap_or_default());
        let content_type = rec_obj.get("content_type").and_then(Value::as_str);
        let Some(content_type) = content_type else {
            unmeasured_mime += 1;
            continue;
        };
        if py_strip(content_type).is_empty() {
            unmeasured_mime += 1;
            continue;
        }
        let declared = py_strip(content_type.split(';').next().unwrap_or("")).to_lowercase();
        if !declared.contains('/') {
            unmeasured_mime += 1;
            continue;
        }
        observed_mime += 1;
        if declared != expected.to_lowercase() {
            let mut details = Map::new();
            details.insert("declared_content_type".into(), Value::String(declared));
            details.insert("extension_content_type".into(), Value::String(expected));
            ctx.add(
                "DECLARED_MIME_MISMATCH",
                AddOpts {
                    target_url: Some(page.url()),
                    details,
                    ..AddOpts::default()
                },
            )?;
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
    Ok(())
}
