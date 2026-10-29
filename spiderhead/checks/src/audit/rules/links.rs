//! Link-count checks (reference the reference suite).

use serde_json::{json, Map, Value};

use super::content_meta::{body_unavailable_reason, skip_for_body_unavailable};
use super::details;
use crate::audit::AuditContext;

/// Evidence cap for link-placement and invalid-target lists
/// (`_PLACEMENT_EVIDENCE_ITEMS` in the reference suite).
pub(crate) const PLACEMENT_EVIDENCE_ITEMS: usize = 5;

/// `check_links_extra`: empty and over-threshold outlink counts.
pub fn check_links_extra(ctx: &mut AuditContext) {
    let high_outlinks = ctx.thresholds.get("high_outlinks").unwrap_or(300.0);
    let high_external = ctx
        .thresholds
        .get("high_external_outlinks")
        .unwrap_or(100.0);
    for i in ctx.indexable_html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let outlinks = page.rec.get("outlinks").cloned().unwrap_or(Value::Null);
        let external = page
            .rec
            .get("external_outlinks")
            .cloned()
            .unwrap_or(Value::Null);
        if !outlinks.is_null() {
            let n = outlinks.as_f64().unwrap_or(0.0);
            if n <= 0.0 {
                ctx.add(
                    "NO_INTERNAL_OUTLINKS",
                    Some(&page.url),
                    details([
                        ("outlinks", outlinks.clone()),
                        ("external_outlinks", external.clone()),
                    ]),
                );
            }
            if n > high_outlinks {
                ctx.add(
                    "HIGH_OUTLINKS",
                    Some(&page.url),
                    details([("outlinks", outlinks), ("max", json!(high_outlinks))]),
                );
            }
        }
        if !external.is_null() && external.as_f64().is_some_and(|e| e > high_external) {
            ctx.add(
                "HIGH_EXTERNAL_OUTLINKS",
                Some(&page.url),
                details([
                    ("external_outlinks", external),
                    ("max", json!(high_external)),
                ]),
            );
        }
    }
}

/// `check_invalid_link_targets` (the reference suite): hrefs the link parser
/// rejected while `discovery.record_invalid_targets` was on.
pub fn check_invalid_link_targets(ctx: &mut AuditContext) {
    if !ctx.has_column("invalid_link_targets") {
        ctx.skip(
            "INVALID_LINK_TARGET",
            "no invalid link target evidence (native crawl only)",
        );
        return;
    }
    // Measured over every page, findings only for indexable ones.
    let measured = ctx.pages.iter().any(|page| {
        page.rec
            .get("invalid_link_targets")
            .is_some_and(|v| !v.is_null())
    });
    if !measured {
        let pages = ctx.indexable_html_pages();
        let reason = body_unavailable_reason(ctx, &pages).unwrap_or_else(|| {
            "invalid link target recording was off for this crawl \
             (discovery.record_invalid_targets)"
                .to_owned()
        });
        ctx.skip("INVALID_LINK_TARGET", &reason);
        return;
    }
    for i in ctx.indexable_html_pages() {
        let page = ctx.pages[i].clone();
        let Some(targets) = page
            .rec
            .get("invalid_link_targets")
            .and_then(Value::as_array)
            .filter(|list| !list.is_empty())
        else {
            continue;
        };
        let first_targets: Vec<Value> = targets
            .iter()
            .take(PLACEMENT_EVIDENCE_ITEMS)
            .map(|item| {
                let mut row = Map::new();
                row.insert(
                    "raw_href".into(),
                    item.get("raw_href").cloned().unwrap_or_default(),
                );
                row.insert(
                    "reason".into(),
                    item.get("reason").cloned().unwrap_or_default(),
                );
                Value::Object(row)
            })
            .collect();
        ctx.add(
            "INVALID_LINK_TARGET",
            Some(&page.url),
            details([
                ("count", json!(targets.len())),
                ("first_targets", json!(first_targets)),
            ]),
        );
    }
}

/// the reference `int(x or fallback)` for JSON numbers: falsy values collapse to
/// `fallback`, a truthy float truncates toward zero.
fn int_or(value: Option<&Value>, fallback: usize) -> i64 {
    match value {
        Some(v) if !v.is_null() => {
            let truthy = match v {
                Value::Bool(b) => *b,
                Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
                Value::String(s) => !s.is_empty(),
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
                Value::Null => false,
            };
            if truthy {
                v.as_f64().map(|f| f as i64).unwrap_or(fallback as i64)
            } else {
                fallback as i64
            }
        }
        _ => fallback as i64,
    }
}

/// `check_link_placement` (the reference suite): anchors sitting inside the
/// page's own headings, and image links carrying no readable anchor.
pub fn check_link_placement(ctx: &mut AuditContext) {
    const IDS: [&str; 2] = ["LINK_INSIDE_HEADING", "IMAGE_LINK_WITHOUT_TEXT"];
    if !ctx.has_column("link_placement") {
        for check_id in IDS {
            ctx.skip(check_id, "no link-placement evidence (native crawl only)");
        }
        return;
    }
    let pages = ctx.indexable_html_pages();
    for check_id in IDS {
        skip_for_body_unavailable(ctx, check_id, &pages);
    }
    let mut unmeasured = 0_i64;
    for &i in &pages {
        let page = ctx.pages[i].clone();
        let Some(placement) = page.rec.get("link_placement").and_then(Value::as_object) else {
            unmeasured += 1;
            continue;
        };
        let in_heading = placement
            .get("in_heading")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !in_heading.is_empty() {
            let count = int_or(placement.get("in_heading_total"), in_heading.len());
            let mut levels: Vec<String> = in_heading
                .iter()
                .map(|item| format!("h{}", item.get("level").cloned().unwrap_or_default()))
                .collect();
            levels.sort();
            levels.dedup();
            let first_links: Vec<Value> = in_heading
                .iter()
                .take(PLACEMENT_EVIDENCE_ITEMS)
                .map(|item| {
                    let mut row = Map::new();
                    for key in ["level", "anchor", "destination"] {
                        row.insert(key.into(), item.get(key).cloned().unwrap_or_default());
                    }
                    Value::Object(row)
                })
                .collect();
            ctx.add_with(
                "LINK_INSIDE_HEADING",
                Some(&page.url),
                details([
                    ("count", json!(count)),
                    ("levels", json!(levels)),
                    ("first_links", json!(first_links)),
                ]),
                None,
                count,
                None,
            );
        }
        let image_links = placement
            .get("image_no_text")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !image_links.is_empty() {
            let count = int_or(placement.get("image_no_text_total"), image_links.len());
            let first_destinations: Vec<Value> = image_links
                .iter()
                .take(PLACEMENT_EVIDENCE_ITEMS)
                .map(|item| item.get("destination").cloned().unwrap_or_default())
                .collect();
            ctx.add_with(
                "IMAGE_LINK_WITHOUT_TEXT",
                Some(&page.url),
                details([
                    ("count", json!(count)),
                    ("first_destinations", json!(first_destinations)),
                ]),
                None,
                count,
                None,
            );
        }
    }
    if unmeasured > 0 {
        for check_id in IDS {
            ctx.skip(
                check_id,
                &format!(
                    "{unmeasured} page(s) carry no link-placement evidence -- their anchors \
                     were never inspected for this"
                ),
            );
        }
    }
}
