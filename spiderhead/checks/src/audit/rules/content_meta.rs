//! Title and meta-description checks (reference the reference suite, native evidence path).

use serde_json::Value;

use super::{details, num, py_truthy};
use crate::audit::AuditContext;

/// Reason text of `_body_unavailable_reason`, when any page was not parsed.
pub(crate) fn body_unavailable_reason(ctx: &AuditContext, pages: &[usize]) -> Option<String> {
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    for &i in pages {
        let reason = ctx.pages[i].text("body_unavailable");
        if !reason.is_empty() {
            *reasons.entry(reason.to_owned()).or_default() += 1;
        }
    }
    let count: usize = reasons.values().sum();
    if count == 0 {
        return None;
    }
    if reasons.len() == 1 && reasons.contains_key("oversized") {
        return Some(format!("{count} page(s) with an oversized, unparsed HTML body -- this metadata was never measured"));
    }
    let label = |r: &str| match r {
        "excluded_by_media_type" => "excluded by media type".to_owned(),
        "not_included_by_media_type" => "not included by media type".to_owned(),
        "media_type_unavailable" => "missing or malformed Content-Type".to_owned(),
        "oversized" => "oversized HTML body".to_owned(),
        other => other.to_owned(),
    };
    let summary: Vec<String> = reasons
        .iter()
        .map(|(r, n)| format!("{}: {n}", label(r)))
        .collect();
    Some(format!(
        "{count} page(s) were not parsed ({}); this metadata was never measured",
        summary.join(", ")
    ))
}

pub(crate) fn skip_for_body_unavailable(ctx: &mut AuditContext, check: &str, pages: &[usize]) {
    if let Some(reason) = body_unavailable_reason(ctx, pages) {
        ctx.skip(check, &reason);
    }
}

/// `_check_snippet_pixels`: the SF pixel-width columns compared against the
/// configured bounds; a native crawl never measures pixels, so the checks
/// declare their real reason instead of a false clean (#1578).
fn check_snippet_pixels(
    ctx: &mut AuditContext,
    pages: &[usize],
    field: &str,
    max_key: &str,
    min_key: &str,
    over_code: &str,
    below_code: &str,
) {
    const PIXEL_SOURCE: &str = "SF-derived pixel-width column; native crawls leave it unmeasured";
    if !ctx.has_column(field) {
        let reason = format!("no {field} column in Internal:All ({PIXEL_SOURCE})");
        ctx.skip(over_code, &reason);
        ctx.skip(below_code, &reason);
        return;
    }
    let max_px = ctx.thresholds.get(max_key).unwrap_or(0.0);
    let min_px = ctx.thresholds.get(min_key).unwrap_or(0.0);
    let mut measured = 0usize;
    for &i in pages {
        let page = ctx.pages[i].clone();
        let Some(px) = page.num(field).filter(|v| *v != 0.0) else {
            continue;
        };
        measured += 1;
        if px > max_px {
            ctx.add(
                over_code,
                Some(&page.url),
                details([("pixel_width", num(px)), ("max_px", num(max_px))]),
            );
        } else if px < min_px {
            ctx.add(
                below_code,
                Some(&page.url),
                details([("pixel_width", num(px)), ("min_px", num(min_px))]),
            );
        }
    }
    if measured == 0 {
        let reason = format!("the {field} column carried no measured values ({PIXEL_SOURCE})");
        ctx.skip(over_code, &reason);
        ctx.skip(below_code, &reason);
    }
}

/// `check_titles`: pixel rows first, then the text rows; an absent Title
/// column skips TITLE_MISSING rather than fabricating findings (#205).
pub fn check_titles(ctx: &mut AuditContext) {
    let pages = ctx.indexable_html_pages();
    check_snippet_pixels(
        ctx,
        &pages,
        "title_px",
        "title_max_px",
        "title_min_px",
        "TITLE_PIXELS_OVER",
        "TITLE_PIXELS_BELOW",
    );
    if !ctx.has_column("title") {
        ctx.skip("TITLE_MISSING", "no Title column in Internal:All");
        return;
    }
    skip_for_body_unavailable(ctx, "TITLE_MISSING", &pages);
    let t = ctx.thresholds.clone();
    for i in pages {
        let page = ctx.pages[i].clone();
        let title = page.text("title");
        if title.is_empty() {
            if !page.body_unavailable() {
                ctx.add("TITLE_MISSING", Some(&page.url), Default::default());
            }
            continue;
        }
        let length = page
            .num("title_length")
            .unwrap_or(title.chars().count() as f64);
        if length > t.title_max_chars {
            ctx.add(
                "TITLE_TOO_LONG",
                Some(&page.url),
                details([
                    ("title", title.into()),
                    ("length", num(length)),
                    ("max_chars", num(t.title_max_chars)),
                ]),
            );
        } else if length < t.title_min_chars {
            ctx.add(
                "TITLE_TOO_SHORT",
                Some(&page.url),
                details([
                    ("title", title.into()),
                    ("length", num(length)),
                    ("min_chars", num(t.title_min_chars)),
                ]),
            );
        }
        let h1 = page.text("h1");
        if !h1.is_empty() && h1.trim() == title.trim() {
            ctx.add(
                "TITLE_EQUALS_H1",
                Some(&page.url),
                details([("value", Value::from(title))]),
            );
        }
    }
}

/// `check_descriptions`: the live-tag count first (independent evidence,
/// #385), then the text rows, then the pixel rows (#1578).
pub fn check_descriptions(ctx: &mut AuditContext) {
    let pages = ctx.indexable_html_pages();
    if ctx.has_column("meta_description_count") {
        for &i in &pages {
            let page = &ctx.pages[i];
            if let Some(count) = page.num("meta_description_count").filter(|c| *c > 1.0) {
                let url = page.url.clone();
                ctx.add(
                    "DESC_MULTIPLE",
                    Some(&url),
                    details([("count", num(count))]),
                );
            }
        }
    } else {
        ctx.skip(
            "DESC_MULTIPLE",
            "no meta description count evidence (native crawl only)",
        );
    }
    if ctx.has_column("meta_description") {
        skip_for_body_unavailable(ctx, "DESC_MISSING", &pages);
        let t = ctx.thresholds.clone();
        for i in pages.iter().copied() {
            let page = ctx.pages[i].clone();
            let desc = page.text("meta_description");
            if desc.is_empty() {
                if !page.body_unavailable() {
                    ctx.add("DESC_MISSING", Some(&page.url), Default::default());
                }
                continue;
            }
            let length = page
                .num("desc_length")
                .unwrap_or(desc.chars().count() as f64);
            if length > t.desc_max_chars {
                ctx.add(
                    "DESC_TOO_LONG",
                    Some(&page.url),
                    details([
                        ("length", num(length)),
                        ("max_chars", num(t.desc_max_chars)),
                    ]),
                );
            } else if length < t.desc_min_chars {
                ctx.add(
                    "DESC_TOO_SHORT",
                    Some(&page.url),
                    details([
                        ("length", num(length)),
                        ("min_chars", num(t.desc_min_chars)),
                    ]),
                );
            }
        }
    } else {
        ctx.skip("DESC_MISSING", "no Meta Description column in Internal:All");
    }
    check_snippet_pixels(
        ctx,
        &pages,
        "desc_px",
        "desc_max_px",
        "desc_min_px",
        "DESC_PIXELS_OVER",
        "DESC_PIXELS_BELOW",
    );
}

/// `check_headings`: H1 presence/multiplicity/length, H1 alt-text-only and the
/// H2 checks driven by the retained heading outline (or the `h2_2` export
/// column when an export supplies it).
pub fn check_headings(ctx: &mut AuditContext) {
    const HEADING_EVIDENCE_TEXTS: usize = 5;
    let require_h2 = ctx
        .requirements
        .get("require_h2")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !require_h2 {
        ctx.skip(
            "H2_MISSING",
            "requirements.require_h2 is false; the check was not evaluated",
        );
    }
    let has_h1_column = ctx.has_column("h1");
    let pages = ctx.indexable_html_pages();
    if !has_h1_column {
        ctx.skip("H1_MISSING", "no H1-1 column in Internal:All");
    } else {
        skip_for_body_unavailable(ctx, "H1_MISSING", &pages);
    }
    for i in pages {
        let page = ctx.pages[i].clone();
        let (h1, h1_2) = (page.text("h1"), page.text("h1_2"));
        if has_h1_column && h1.is_empty() && !page.body_unavailable() {
            ctx.add("H1_MISSING", Some(&page.url), Default::default());
        }
        if !h1_2.is_empty() {
            let texts: Vec<Value> = [h1, h1_2]
                .into_iter()
                .filter(|v| !v.is_empty())
                .map(Value::from)
                .collect();
            let count = page
                .metrics
                .get("h1_count")
                .cloned()
                .unwrap_or(Value::from(0));
            ctx.add(
                "H1_MULTIPLE",
                Some(&page.url),
                details([("h1_count", count), ("h1_texts", Value::Array(texts))]),
            );
        }
        let mut h1_len = page.num("h1_length");
        if h1_len.is_none() && !h1.is_empty() {
            h1_len = Some(h1.chars().count() as f64);
        }
        let h1_max = ctx.thresholds.get("h1_max_chars").unwrap_or(70.0);
        if !h1.is_empty() && h1_len.is_some_and(|l| l > h1_max) {
            ctx.add(
                "H1_TOO_LONG",
                Some(&page.url),
                details([
                    ("length", num(h1_len.unwrap_or(0.0))),
                    ("max_chars", num(h1_max)),
                ]),
            );
        }
        let alt_text = page.text("h1_alt_text");
        if !alt_text.is_empty() {
            ctx.add(
                "H1_ALT_TEXT_ONLY",
                Some(&page.url),
                details([("alt_text", Value::from(alt_text))]),
            );
        }
        let h2 = page.text("h2");
        if require_h2 && !h1.is_empty() && h2.is_empty() {
            ctx.add(
                "H2_MISSING",
                Some(&page.url),
                details([("h1", Value::from(h1))]),
            );
        }
        // H2_MULTIPLE (#1579): the native outline counts H2s exactly; an export's
        // second-H2 column only proves "more than one" — both name their source.
        let outline = page
            .rec
            .get("heading_outline")
            .cloned()
            .or_else(|| {
                page.rec
                    .get("heading_outline_json")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
            })
            .unwrap_or(Value::Null);
        let mut h2_multiple = false;
        if let Value::Array(items) = &outline {
            if !items.is_empty() {
                let h2s: Vec<&Value> = items
                    .iter()
                    .filter(|it| it.get("level").and_then(Value::as_i64) == Some(2))
                    .collect();
                if h2s.len() > 1 {
                    h2_multiple = true;
                    let texts: Vec<Value> = h2s
                        .iter()
                        .take(HEADING_EVIDENCE_TEXTS)
                        .map(|it| Value::from(it.get("text").and_then(Value::as_str).unwrap_or("")))
                        .collect();
                    ctx.add(
                        "H2_MULTIPLE",
                        Some(&page.url),
                        details([
                            ("h2_count", Value::from(h2s.len() as i64)),
                            ("source", Value::from("native-heading-outline")),
                            ("h2_texts", Value::Array(texts)),
                        ]),
                    );
                }
            }
        }
        let h2_2 = page.text("h2_2");
        if !h2_multiple && !h2_2.is_empty() {
            let texts: Vec<Value> = [h2, h2_2]
                .into_iter()
                .filter(|v| !v.is_empty())
                .map(Value::from)
                .collect();
            ctx.add(
                "H2_MULTIPLE",
                Some(&page.url),
                details([
                    ("h2_texts", Value::Array(texts)),
                    ("source", Value::from("sf-h2-2-column")),
                ]),
            );
        }
        // H2_TOO_LONG (#385): no dedicated length column exists, so the length is
        // measured off the value itself.
        let h2_max = ctx.thresholds.get("h2_max_chars").unwrap_or(70.0);
        if !h2.is_empty() && h2.chars().count() as f64 > h2_max {
            ctx.add(
                "H2_TOO_LONG",
                Some(&page.url),
                details([
                    ("length", Value::from(h2.chars().count() as i64)),
                    ("max_chars", num(h2_max)),
                ]),
            );
        }
    }
    if !ctx.has_column("h1_alt_text") {
        ctx.skip(
            "H1_ALT_TEXT_ONLY",
            "no H1 alt-text evidence (native crawl only)",
        );
    }
    if !ctx.has_column("h2_2") && !ctx.has_column("heading_outline") {
        ctx.skip(
            "H2_MULTIPLE",
            "no H2-2 column and no heading outline evidence; the H2 count cannot be measured",
        );
    }
}

/// Chrome headings shared by pages: the pattern, the pages and the quoted evidence.
type ChromeGroup = (Vec<(String, i64, String)>, Vec<String>, Value);

/// How many heading texts a finding quotes back.
const HEADING_EVIDENCE_TEXTS: usize = 5;

/// Page-chrome regions whose headings are template furniture (`CHROME_POSITIONS`).
const CHROME_POSITIONS: [&str; 5] = ["nav", "header", "sidebar", "footer", "breadcrumbs"];

/// The stored heading outline of a page, whether held as a value or as JSON text.
fn heading_outline_of(page: &crate::audit::Page) -> Option<Vec<Value>> {
    let value = page.rec.get("heading_outline").cloned().or_else(|| {
        page.rec
            .get("heading_outline_json")
            .and_then(Value::as_str)
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
    })?;
    match value {
        Value::Array(items) if !items.is_empty() => Some(items),
        _ => None,
    }
}

fn level_of(heading: &Value) -> Option<i64> {
    heading.get("level").and_then(Value::as_i64)
}

fn text_of(heading: &Value) -> Value {
    heading.get("text").cloned().unwrap_or(Value::Null)
}

/// `check_heading_outline`: the outline as a sequence and as a map of the page --
/// content before the H1, headings inside page chrome, and skipped levels.
pub fn check_heading_outline(ctx: &mut AuditContext) {
    const CODES: [&str; 3] = [
        "HEADING_BEFORE_H1",
        "HEADING_IN_PAGE_CHROME",
        "HEADING_SKIP",
    ];
    if !ctx.has_column("heading_outline") {
        for code in CODES {
            ctx.skip(code, "no heading outline evidence (native crawl only)");
        }
        return;
    }
    let pages = ctx.indexable_html_pages();
    for code in CODES {
        skip_for_body_unavailable(ctx, code, &pages);
    }
    let mut unplaced = 0;
    // Pages sharing the same chrome headings become one finding with a page count.
    let mut chrome: Vec<ChromeGroup> = Vec::new();
    for &i in &pages {
        let page = ctx.pages[i].clone();
        let Some(outline) = heading_outline_of(&page) else {
            continue;
        };
        let first_h1 = outline.iter().position(|h| level_of(h) == Some(1));
        if let Some(first) = first_h1.filter(|&n| n > 0) {
            let preceding = &outline[..first];
            let mut levels: Vec<String> = preceding
                .iter()
                .map(|h| format!("h{}", level_of(h).unwrap_or(0)))
                .collect();
            levels.sort();
            levels.dedup();
            ctx.add(
                "HEADING_BEFORE_H1",
                Some(&page.url),
                details([
                    ("count", Value::from(preceding.len())),
                    ("levels", Value::from(levels)),
                    (
                        "first_texts",
                        Value::Array(
                            preceding
                                .iter()
                                .take(HEADING_EVIDENCE_TEXTS)
                                .map(text_of)
                                .collect(),
                        ),
                    ),
                ]),
            );
        }
        let in_chrome: Vec<&Value> = outline
            .iter()
            .filter(|h| {
                h.get("region")
                    .and_then(Value::as_str)
                    .is_some_and(|r| CHROME_POSITIONS.contains(&r))
            })
            .collect();
        if !in_chrome.is_empty() {
            let pattern: Vec<(String, i64, String)> = in_chrome
                .iter()
                .map(|h| {
                    (
                        h["region"].as_str().unwrap_or("").to_owned(),
                        level_of(h).unwrap_or(0),
                        text_of(h).as_str().unwrap_or("").to_owned(),
                    )
                })
                .collect();
            if let Some(entry) = chrome.iter_mut().find(|(p, _, _)| *p == pattern) {
                entry.1.push(page.url.clone());
            } else {
                let mut regions: Vec<String> = pattern.iter().map(|(r, _, _)| r.clone()).collect();
                regions.sort();
                regions.dedup();
                let detail = Value::Object(details([
                    ("count", Value::from(in_chrome.len())),
                    ("regions", Value::from(regions)),
                    (
                        "first_headings",
                        Value::Array(
                            in_chrome
                                .iter()
                                .take(HEADING_EVIDENCE_TEXTS)
                                .map(|h| {
                                    Value::Object(details([
                                        ("region", h["region"].clone()),
                                        ("level", h["level"].clone()),
                                        ("text", text_of(h)),
                                    ]))
                                })
                                .collect(),
                        ),
                    ),
                ]));
                chrome.push((pattern, vec![page.url.clone()], detail));
            }
        }
        // A jump is meaningful only in the named content region; template
        // furniture and unplaced headings are excluded.
        let content: Vec<&Value> = outline
            .iter()
            .filter(|h| h.get("region").and_then(Value::as_str) == Some("content"))
            .collect();
        let jumps: Vec<(&Value, &Value)> = content
            .windows(2)
            .filter(|w| (level_of(w[1]).unwrap_or(0) - level_of(w[0]).unwrap_or(0)).abs() > 1)
            .map(|w| (w[0], w[1]))
            .collect();
        if !jumps.is_empty() {
            ctx.add(
                "HEADING_SKIP",
                Some(&page.url),
                details([
                    ("count", Value::from(jumps.len())),
                    (
                        "first_jumps",
                        Value::Array(
                            jumps
                                .iter()
                                .take(HEADING_EVIDENCE_TEXTS)
                                .map(|(from, to)| {
                                    Value::Object(details([
                                        ("from_level", from["level"].clone()),
                                        ("from_text", text_of(from)),
                                        ("to_level", to["level"].clone()),
                                        ("to_text", text_of(to)),
                                    ]))
                                })
                                .collect(),
                        ),
                    ),
                ]),
            );
        }
        // A blank region is a heading the parser could not place.
        if outline
            .iter()
            .any(|h| h.get("region").is_none_or(|r| !py_truthy(Some(r))))
        {
            unplaced += 1;
        }
    }
    for (_, urls, detail) in chrome {
        let map = detail.as_object().cloned().unwrap_or_default();
        ctx.add_site_wide("HEADING_IN_PAGE_CHROME", &urls, map);
    }
    if unplaced > 0 {
        ctx.skip(
            "HEADING_IN_PAGE_CHROME",
            &format!(
                "{unplaced} page(s) with no content landmark and no matching position rule \
                 -- no heading region could be determined"
            ),
        );
    }
}

/// `check_schema`: the JSON-LD parse-error row (found blocks exceed parsed blocks).
pub fn check_schema(ctx: &mut AuditContext) {
    let mut found_any = false;
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        // SF-export validation counts; a native scan never carries the column.
        if let Some(errors) = page.rec.get("validation_errors").filter(|v| !v.is_null()) {
            found_any = true;
            if errors.as_f64().is_some_and(|n| n > 0.0) {
                ctx.add(
                    "SCHEMA_VALIDATION_ERROR",
                    Some(&page.url),
                    details([("validation_errors", errors.clone())]),
                );
            }
        }
        let found = page.num("jsonld_blocks_found").unwrap_or(0.0);
        let parsed = page.num("jsonld_blocks_parsed").unwrap_or(0.0);
        if found > parsed {
            ctx.add(
                "STRUCTURED_DATA_PARSE_ERROR",
                Some(&page.url),
                details([("blocks_found", num(found)), ("blocks_parsed", num(parsed))]),
            );
        }
    }
    if !found_any {
        ctx.skip(
            "SCHEMA_VALIDATION_ERROR",
            "no Structured Data validation columns in Internal:All",
        );
    }
}

/// `check_og`: indexable HTML pages missing ``og:title`` get an OG_MISSING row
/// naming every absent preview tag; a run with no Open Graph evidence at all
/// is a stated skip, not a sitewide finding.
pub fn check_og(ctx: &mut AuditContext) {
    const OG_FIELDS: [&str; 4] = ["og_title", "og_description", "og_image", "og_url"];
    const MISSING_FIELDS: [&str; 3] = ["og_title", "og_image", "og_url"];
    let pages = ctx.indexable_html_pages();
    let has_og = pages.iter().any(|&i| {
        OG_FIELDS
            .iter()
            .any(|f| py_truthy(ctx.pages[i].rec.get(*f)))
    });
    if !has_og {
        ctx.skip(
            "OG_MISSING",
            "no Open Graph columns in Internal:All (enable OG extraction in SF)",
        );
        return;
    }
    for &i in &pages {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        if py_truthy(page.rec.get("og_title")) {
            continue;
        }
        let missing: Vec<Value> = MISSING_FIELDS
            .iter()
            .filter(|f| !py_truthy(page.rec.get(**f)))
            .map(|f| Value::from(f.replace('_', ":")))
            .collect();
        ctx.add(
            "OG_MISSING",
            Some(&page.url),
            details([("missing_tags", Value::Array(missing))]),
        );
    }
}
