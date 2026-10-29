//! Checks over evidence only a native crawl records (reference the reference suite).

use std::collections::{HashMap, HashSet};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{Map, Value};
use spiderhead_parse::dom::{
    self, all_elements, attr, find_all, has_ancestor_named, ElementRef, Soup,
};
use spiderhead_parse::parser::shared::{collapse_whitespace, has_ancestor, INERT_LINK_CONTAINERS};
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};
use spiderhead_parse::py_str::{is_py_space, py_strip};
use spiderhead_parse::py_url::{urljoin, urlparse, urlsplit};
use spiderhead_parse::urlutil::hostname;
use spiderhead_store::bodies::read_document;

use super::details;
use crate::audit::AuditContext;

/// `_HTML_WHERE` (the reference suite): crawled HTML pages that answered 2xx.
const HTML_WHERE: &str =
    "p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%'";
/// `_ACTIVE_DOCUMENT_WHERE` (the reference suite): the selected or its static shadow document.
const ACTIVE_DOCUMENT_WHERE: &str =
    "(d.document_id=p.document_id OR d.document_id=raw.document_id)";
/// `_NATIVE_NO_RESOURCES` (the reference suite): the scan carries no resource-graph tables.
const NATIVE_NO_RESOURCES: &str = "no resource evidence (native crawl with resource capture only)";
/// `INERT_TAGS` (the reference suite): subtrees that never render.
const INERT_TAGS: [&str; 4] = ["template", "script", "style", "noscript"];
/// Bound of retained image declarations analyzed per document (`max_images=2000`).
const MAX_RETAINED_IMAGES: usize = 2000;
/// Decoded-body bound for the retained markup pass (2 MiB).
const MAX_DOCUMENT_BYTES: i64 = 2 * 1024 * 1024;

/// `check_native_page_evidence`: lorem ipsum and plugin elements (per-image rows read the stored scan).
pub fn check_native_page_evidence(ctx: &mut AuditContext) {
    let has_lorem = ctx
        .pages
        .iter()
        .any(|p| p.rec.contains_key("lorem_ipsum_count"));
    let has_plugin = ctx
        .pages
        .iter()
        .any(|p| p.rec.contains_key("plugin_elements"));
    for i in ctx.html_pages() {
        let page = ctx.pages[i].clone();
        if page.body_unavailable() {
            continue;
        }
        let occurrences = page.num("lorem_ipsum_count").unwrap_or(0.0);
        if has_lorem && occurrences > 0.0 {
            ctx.add(
                "LOREM_IPSUM_PLACEHOLDER",
                Some(&page.url),
                details([("occurrences", page.opt("lorem_ipsum_count"))]),
            );
        }
        let count = page.num("plugin_elements").unwrap_or(0.0);
        if has_plugin && count > 0.0 {
            ctx.add(
                "UNSUPPORTED_PLUGIN",
                Some(&page.url),
                details([("count", page.opt("plugin_elements"))]),
            );
        }
    }
    if !has_lorem {
        ctx.skip(
            "LOREM_IPSUM_PLACEHOLDER",
            "no Lorem Ipsum evidence (native crawl only)",
        );
    }
    if !has_plugin {
        ctx.skip(
            "UNSUPPORTED_PLUGIN",
            "no plugin-element evidence (native crawl only)",
        );
    }
}

/// `con.execute("SELECT name FROM sqlite_master WHERE type='table'")`.
fn table_names(con: &Connection) -> HashSet<String> {
    let mut stmt = match con.prepare("SELECT name FROM sqlite_master WHERE type='table'") {
        Ok(stmt) => stmt,
        Err(_) => return HashSet::new(),
    };
    stmt.query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// `_image_graph_scope`: restrict a resource graph query to eligible pages and active documents.
fn image_graph_scope(tables: &HashSet<String>) -> (&'static str, String) {
    if ["pages", "documents", "context_items"]
        .iter()
        .all(|t| tables.contains(*t))
    {
        (
            concat!(
                " JOIN pages p ON p.url_id=o.page_url_id ",
                "JOIN documents d ON d.document_id=o.source_document_id ",
                " LEFT JOIN (SELECT d.url_id,MAX(d.document_id) AS document_id FROM documents d ",
                "JOIN context_items c ON c.kind='resource_inventory' AND c.item_key='document:'||d.document_id ",
                "WHERE d.representation='static' GROUP BY d.url_id) raw ",
                "ON raw.url_id=p.url_id AND p.representation!='static' "
            ),
            format!(" AND {HTML_WHERE} AND {ACTIVE_DOCUMENT_WHERE}"),
        )
    } else {
        ("", String::new())
    }
}

/// `check_native_image_resources`: an `<img>` whose fetched target answered 4xx/5xx.
pub fn check_native_image_resources(ctx: &mut AuditContext) {
    let no_evidence = "no resource evidence (native crawl with resource capture only)";
    let Some(con) = ctx.scan.as_ref() else {
        ctx.skip("IMG_BROKEN", no_evidence);
        return;
    };
    let tables = table_names(con);
    if !["resource_graph_occurrences", "resource_graph_fetches"]
        .iter()
        .all(|t| tables.contains(*t))
    {
        ctx.skip("IMG_BROKEN", no_evidence);
        return;
    }
    let measured = con
        .query_row(
            "SELECT 1 FROM resource_graph_occurrences o JOIN resource_graph_fetches f \
             ON f.resolved_url=o.resolved_url WHERE o.kind='image' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten();
    if measured.is_none() {
        ctx.skip("IMG_BROKEN", no_evidence);
        return;
    }
    let (joins, where_) = image_graph_scope(&tables);
    let sql = format!(
        "SELECT DISTINCT u.url, o.resolved_url, f.status_code FROM resource_graph_occurrences o \
         JOIN resource_graph_fetches f ON f.resolved_url=o.resolved_url \
         JOIN urls u ON u.url_id=o.page_url_id{joins}\
         WHERE o.kind='image' AND f.status_code>=400{where_} ORDER BY u.url, o.resolved_url"
    );
    let rows = con.prepare(&sql).ok().map_or_else(Vec::new, |mut stmt| {
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map(|rows| rows.flatten().collect::<Vec<_>>())
        .unwrap_or_default()
    });
    for (page_url, image_url, status) in rows {
        let d = details([
            ("source_page", Value::from(page_url.clone())),
            ("resource_url", Value::from(image_url.clone())),
            ("target_scope", Value::from("image_source_page")),
            ("status_code", Value::from(status)),
            ("status_scope", Value::from("resource")),
            ("reason", Value::from(format!("HTTP {status}"))),
        ]);
        ctx.add_source(
            "IMG_BROKEN",
            Some(&image_url),
            Some(status),
            1,
            (
                "native-resource-graph",
                vec![serde_json::json!({"source_url": page_url})],
            ),
            d,
        );
    }
}

/// `check_native_image_size`: an image whose fetched body is over `thresholds.img_max_kb`.
pub fn check_native_image_size(ctx: &mut AuditContext) {
    let Some(con) = ctx.scan.as_ref() else {
        return;
    };
    let tables = table_names(con);
    if !["resource_graph_occurrences", "resource_graph_fetches"]
        .iter()
        .all(|t| tables.contains(*t))
    {
        ctx.skip("IMG_OVER_KB", NATIVE_NO_RESOURCES);
        return;
    }
    let measured = con
        .query_row(
            "SELECT 1 FROM resource_graph_occurrences o JOIN resource_graph_fetches f \
             ON f.resolved_url=o.resolved_url WHERE o.kind='image' \
             AND f.state IN ('complete','partial') LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten();
    if measured.is_none() {
        ctx.skip(
            "IMG_OVER_KB",
            "not measured by this native crawl: no image body was fetched",
        );
        return;
    }
    let limit_kb = ctx.thresholds.img_max_kb;
    let (joins, where_) = image_graph_scope(&tables);
    let sql = format!(
        "SELECT DISTINCT o.resolved_url,u.url,f.bytes_received,f.status_code,f.state \
         FROM resource_graph_occurrences o \
         JOIN resource_graph_fetches f ON f.resolved_url=o.resolved_url \
         JOIN urls u ON u.url_id=o.page_url_id{joins}\
         WHERE o.kind='image' AND f.state IN ('complete','partial') \
         AND f.bytes_received > ?1{where_} ORDER BY o.resolved_url,u.url"
    );
    let bound = (limit_kb * 1024.0) as i64;
    let rows = con.prepare(&sql).ok().map_or_else(Vec::new, |mut stmt| {
        stmt.query_map([bound], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .map(|rows| rows.flatten().collect::<Vec<_>>())
        .unwrap_or_default()
    });
    for (image_url, source_page, size, status, state) in rows {
        let d = details([
            ("bytes", Value::from(size)),
            ("limit_kb", Value::from(limit_kb)),
            ("source_page", Value::from(source_page.clone())),
            ("pages", Value::from(1)),
            ("resource_url", Value::from(image_url.clone())),
            ("target_scope", Value::from("image_source_page")),
            ("status_code", Value::from(status)),
            ("status_scope", Value::from("resource")),
            (
                "size_state",
                Value::from(if state == "complete" {
                    "complete"
                } else {
                    "lower_bound"
                }),
            ),
            (
                "reason",
                Value::from("captured image response bytes exceed the configured image threshold"),
            ),
        ]);
        ctx.add_source(
            "IMG_OVER_KB",
            Some(&image_url),
            Some(status),
            1,
            (
                "native-resource-graph",
                vec![serde_json::json!({"source_url": source_page})],
            ),
            d,
        );
    }
}

// ── Retained image markup (`_measure_retained_image_declarations`) ─────────────

/// One `<img>` declaration as `extract_images` reports it for the retained pass.
struct RetainedImage {
    /// Collapsed alt length in characters (0 for a missing attribute).
    alt_length: usize,
    /// Index among every `<img>` of the document, template-skipped ones included.
    ordinal: usize,
    /// `src` resolved against the document base; `None` when absent/unusable.
    resource_url: Option<String>,
    missing_alt_attribute: bool,
    missing_alt_text: bool,
    missing_dimensions: Vec<&'static str>,
}

fn self_and_ancestors<'a>(tag: &ElementRef<'a>) -> impl Iterator<Item = ElementRef<'a>> {
    std::iter::once(*tag).chain(tag.ancestors().filter_map(ElementRef::wrap))
}

/// `_excluded` (the reference suite): hidden/inert subtree, kept here because parse's copy is private.
fn excluded(tag: &ElementRef<'_>) -> bool {
    if tag.value().name() == "input" && attr(tag, "type").unwrap_or("").to_lowercase() == "hidden" {
        return true;
    }
    self_and_ancestors(tag).any(|node| {
        let el = node.value();
        INERT_TAGS.contains(&el.name())
            || el.attr("hidden").is_some()
            || el.attr("inert").is_some()
            || attr(&node, "aria-hidden").unwrap_or("").to_lowercase() == "true"
            || attr(&node, "style")
                .unwrap_or("")
                .to_lowercase()
                .split(';')
                .any(|item| {
                    let squeezed: String = item.chars().filter(|c| !c.is_whitespace()).collect();
                    let squeezed = squeezed.strip_suffix("!important").unwrap_or(&squeezed);
                    matches!(
                        squeezed,
                        "display:none" | "visibility:hidden" | "visibility:collapse"
                    )
                })
    })
}

/// `_name_text` (the reference suite): label/reference text without mistaking control values.
fn name_text(tag: &ElementRef<'_>) -> String {
    let label = py_strip(attr(tag, "aria-label").unwrap_or(""));
    if !label.is_empty() {
        return label.to_owned();
    }
    let mut parts: Vec<String> = Vec::new();
    for node in tag.descendants() {
        let text: String = match node.value() {
            scraper::Node::Text(text) => text.to_string(),
            scraper::Node::Doctype(doctype) => doctype.name().to_string(),
            _ => continue,
        };
        let skipped = node.ancestors().filter_map(ElementRef::wrap).any(|n| {
            n != *tag
                && (INERT_TAGS.contains(&n.value().name())
                    || matches!(n.value().name(), "input" | "select" | "textarea"))
        });
        if !skipped {
            parts.push(text);
        }
    }
    for img in tag
        .descendants()
        .skip(1)
        .filter_map(ElementRef::wrap)
        .filter(|e| e.value().name() == "img")
    {
        parts.push(attr(&img, "alt").unwrap_or("").to_owned());
    }
    collapse_whitespace(&parts.join(" "))
}

/// `_aria_named` (the reference suite): an accessible name via `aria-label`/`aria-labelledby`.
fn aria_named<'a>(tag: &ElementRef<'a>, ids: &HashMap<String, Vec<ElementRef<'a>>>) -> bool {
    !py_strip(attr(tag, "aria-label").unwrap_or("")).is_empty()
        || attr(tag, "aria-labelledby")
            .unwrap_or("")
            .split(is_py_space)
            .filter(|s| !s.is_empty())
            .any(|reference| {
                ids.get(reference)
                    .is_some_and(|found| found.len() == 1 && !name_text(&found[0]).is_empty())
            })
}

/// `extract_images` (the reference suite) with `base_url` given: the full per-element facts the
/// retained pass evaluates. The parse crate only carries the ordinary has_alt/alt_length
/// projection, so the URL and layout facts are computed here against the same tree.
fn extract_images_retained(soup: &Soup, base_url: &str, max_images: usize) -> Vec<RetainedImage> {
    let mut ids: HashMap<String, Vec<ElementRef<'_>>> = HashMap::new();
    for node in all_elements(soup) {
        if let Some(id) = node.value().attr("id") {
            if !INERT_TAGS.contains(&node.value().name()) && !has_ancestor_named(&node, &INERT_TAGS)
            {
                ids.entry(id.to_owned()).or_default().push(node);
            }
        }
    }
    let mut images = Vec::new();
    for (ordinal, tag) in find_all(soup, "img").enumerate() {
        if has_ancestor(&tag, &INERT_LINK_CONTAINERS) {
            continue;
        }
        let has_alt = tag.value().attr("alt").is_some();
        let alt_value = if has_alt {
            collapse_whitespace(attr(&tag, "alt").unwrap_or(""))
        } else {
            String::new()
        };
        let roles: Vec<String> = attr(&tag, "role")
            .unwrap_or("")
            .to_lowercase()
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let named = aria_named(&tag, &ids);
        let interactive = tag
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|p| matches!(p.value().name(), "a" | "button"));
        let unnamed_interactive = interactive
            .is_some_and(|parent| name_text(&parent).is_empty() && !aria_named(&parent, &ids));
        let presentational = roles.iter().any(|r| r == "none" || r == "presentation");
        let informative_empty = alt_value.is_empty()
            && !named
            && (roles.iter().any(|r| r == "img") || unnamed_interactive);
        let malformed_empty =
            has_alt && !attr(&tag, "alt").unwrap_or("").is_empty() && alt_value.is_empty();
        let layout_hidden = self_and_ancestors(&tag).any(|node| {
            node.value().attr("hidden").is_some()
                || attr(&node, "style")
                    .unwrap_or("")
                    .split(';')
                    .any(|declaration| {
                        let squeezed: String = declaration
                            .to_lowercase()
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect();
                        squeezed.strip_suffix("!important").unwrap_or(&squeezed) == "display:none"
                    })
        });
        let raw = py_strip(attr(&tag, "src").unwrap_or(""));
        let resource_url = if raw.is_empty() {
            None
        } else {
            urljoin(base_url, raw).ok().and_then(|resolved| {
                let parts = urlparse(&resolved).ok();
                parts.and_then(|parts| {
                    (resolved.chars().count() <= 2048
                        && matches!(parts.scheme.as_str(), "http" | "https")
                        && hostname(&resolved).ok().flatten().is_some())
                    .then_some(resolved)
                })
            })
        };
        let mut missing_dimensions = Vec::new();
        for name in ["width", "height"] {
            let value = py_strip(attr(&tag, name).unwrap_or(""));
            let positive = value.len() <= 16
                && !value.is_empty()
                && value.bytes().all(|b| b.is_ascii_digit())
                && value.parse::<u64>().is_ok_and(|n| n > 0);
            if !positive {
                missing_dimensions.push(name);
            }
        }
        images.push(RetainedImage {
            alt_length: alt_value.chars().count(),
            ordinal,
            resource_url,
            missing_alt_attribute: !has_alt,
            missing_alt_text: has_alt
                && (informative_empty || malformed_empty)
                && !named
                && !presentational
                && !excluded(&tag),
            missing_dimensions: if layout_hidden {
                Vec::new()
            } else {
                missing_dimensions
            },
        });
        if images.len() > max_images {
            break; // One sentinel makes the retained pass declare partial coverage.
        }
    }
    images
}

/// `dict(row)` for a `SELECT ... d.*` query: column name -> JSON value.
fn row_map(
    names: &[String],
    row: &rusqlite::Row<'_>,
) -> Result<Map<String, Value>, rusqlite::Error> {
    let mut map = Map::new();
    for (i, name) in names.iter().enumerate() {
        let value = match row.get_ref(i)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(v) => Value::from(v),
            ValueRef::Real(v) => Value::from(v),
            ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => Value::from(b.to_vec()),
        };
        map.insert(name.clone(), value);
    }
    Ok(map)
}

/// `_document_final_url` (the reference suite): the URL the document was served at.
fn document_final_url(con: &Connection, document: &Map<String, Value>, fallback: &str) -> String {
    let resolved = if document["representation"].as_str() == Some("static") {
        document["source_response_id"].as_i64().and_then(|id| {
            con.query_row(
                "SELECT u.url FROM responses r JOIN urls u ON u.url_id=r.effective_url_id \
                 WHERE r.response_id=?",
                [id],
                |r| r.get::<_, String>(0),
            )
            .ok()
        })
    } else {
        let renderer: Map<String, Value> = document["renderer_json"]
            .as_str()
            .and_then(|t| serde_json::from_str::<Value>(t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let final_url_id = renderer["final_url_id"]
            .as_i64()
            .or_else(|| renderer["navigation_url_id"].as_i64());
        final_url_id.and_then(|id| {
            con.query_row("SELECT url FROM urls WHERE url_id=?", [id], |r| {
                r.get::<_, String>(0)
            })
            .ok()
        })
    };
    resolved
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

fn opt_int(document: &Map<String, Value>, key: &str) -> Option<i64> {
    document.get(key).and_then(Value::as_i64)
}

/// `_retained_image_count_fallback`: keep page-count positives whose element locations were
/// not fully retained or analyzed.
fn retained_image_count_fallback(
    ctx: &mut AuditContext,
    document: &Map<String, Value>,
    sampled_missing: i64,
    sampled_long: bool,
) {
    let url = document["url"].as_str().unwrap_or("").to_owned();
    let status = opt_int(document, "status_code");
    let mut base = details([
        ("source_page", Value::from(url.clone())),
        (
            "source_status_code",
            status.map_or(Value::Null, Value::from),
        ),
        ("resource_url", Value::Null),
        ("resource_status_code", Value::Null),
        ("resource_evidence_state", Value::from("unavailable")),
        ("target_scope", Value::from("page_aggregate")),
        ("status_scope", Value::from("source_page")),
    ]);
    let missing = opt_int(document, "images_missing_alt_attr").unwrap_or(0);
    if missing > sampled_missing {
        let remaining = missing - sampled_missing;
        let mut d = base.clone();
        d.insert(
            "images_missing_alt_attribute".into(),
            Value::from(remaining),
        );
        d.insert(
            "images_total".into(),
            document.get("images_total").cloned().unwrap_or(Value::Null),
        );
        d.insert(
            "reason".into(),
            Value::from(
                "stored page count records missing alt attributes; per-image locations were not fully retained or analyzed",
            ),
        );
        ctx.add_source(
            "IMG_MISSING_ALT_ATTRIBUTE",
            Some(&url),
            status,
            remaining,
            ("native-page-image-count", Vec::new()),
            d,
        );
    }
    let maximum = opt_int(document, "images_max_alt_length").unwrap_or(0);
    let limit = ctx.thresholds.alt_max_chars as i64;
    if maximum > limit && !sampled_long {
        base.insert("max_alt_length".into(), Value::from(maximum));
        base.insert("max_chars".into(), Value::from(limit));
        base.insert(
            "reason".into(),
            Value::from(
                "stored page maximum records overlong alt text; its image location was not fully retained or analyzed",
            ),
        );
        ctx.add_source(
            "IMG_ALT_TOO_LONG",
            Some(&url),
            status,
            1,
            ("native-page-image-count", Vec::new()),
            base,
        );
    }
}

/// One emission the retained pass defers until the scan borrow is released.
enum Op {
    /// `_retained_image_count_fallback` for a document that could not be measured.
    Fallback {
        document: Map<String, Value>,
        sampled_missing: i64,
        sampled_long: bool,
    },
    /// One `native-retained-image-markup` finding.
    Finding {
        code: &'static str,
        target: String,
        status: Option<i64>,
        source_url: String,
        link_path: String,
        details: Map<String, Value>,
    },
}

/// `_measure_retained_image_declarations`: measure bounded active image markup without a fetch.
fn measure_retained_image_declarations(ctx: &mut AuditContext) {
    const CODES: [&str; 4] = [
        "IMG_MISSING_ALT_ATTRIBUTE",
        "IMG_MISSING_ALT",
        "IMG_MISSING_DIMENSIONS",
        "IMG_ALT_TOO_LONG",
    ];
    if ctx.scan.is_none() {
        return;
    }
    let limit = ctx.thresholds.alt_max_chars as i64;
    let mut eligible = 0i64;
    let mut measured = 0i64;
    let mut partial = 0i64;
    let mut ops: Vec<Op> = Vec::new();
    let available;
    {
        let con = ctx.scan.as_ref().expect("checked above");
        let tables = table_names(con);
        available = ["pages", "urls", "documents", "responses", "bodies"]
            .iter()
            .all(|t| tables.contains(*t));
        if available {
            let sql = format!(
                "SELECT u.url,p.status_code,p.images_total,p.images_missing_alt_attr,\
                 p.images_max_alt_length,d.* FROM pages p JOIN urls u ON u.url_id=p.url_id \
                 LEFT JOIN documents d ON d.document_id=p.document_id WHERE {HTML_WHERE} \
                 ORDER BY p.page_ordinal"
            );
            let mut rows_out = Vec::new();
            if let Ok(mut stmt) = con.prepare(&sql) {
                let names: Vec<String> = stmt
                    .column_names()
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect();
                if let Ok(mut rows) = stmt.query([]) {
                    while let Ok(Some(row)) = rows.next() {
                        if let Ok(document) = row_map(&names, row) {
                            rows_out.push(document);
                        }
                    }
                }
            }
            let has_fetches = tables.contains("resource_graph_fetches");
            for document in rows_out {
                eligible += 1;
                let url = document["url"].as_str().unwrap_or("").to_owned();
                let doc_status = opt_int(&document, "status_code");
                let document_id = opt_int(&document, "document_id");
                if document_id.is_none() || document["body_state"].as_str() != Some("complete") {
                    ops.push(Op::Fallback {
                        document,
                        sampled_missing: 0,
                        sampled_long: false,
                    });
                    continue;
                }
                let document_id = document_id.unwrap_or(0);
                let images = (|| -> Result<Vec<RetainedImage>, String> {
                    let html = read_document(con, document_id, MAX_DOCUMENT_BYTES)
                        .map_err(|e| e.to_string())?;
                    let soup = dom::parse_document(&html);
                    let base = document_base_url(
                        DocumentSource::Tree(&soup),
                        &document_final_url(con, &document, &url),
                    );
                    Ok(extract_images_retained(&soup, &base, MAX_RETAINED_IMAGES))
                })();
                let images = match images {
                    Ok(images) => images,
                    Err(_) => {
                        ops.push(Op::Fallback {
                            document,
                            sampled_missing: 0,
                            sampled_long: false,
                        });
                        continue;
                    }
                };
                measured += 1;
                if images.len() > MAX_RETAINED_IMAGES {
                    partial += 1;
                }
                for image in images.iter().take(MAX_RETAINED_IMAGES) {
                    let resource_status = image
                        .resource_url
                        .as_ref()
                        .filter(|_| has_fetches)
                        .and_then(|resource| {
                            con.query_row(
                                "SELECT status_code FROM resource_graph_fetches WHERE resolved_url=?",
                                [resource],
                                |r| r.get::<_, i64>(0),
                            )
                            .ok()
                        });
                    let status = if image.resource_url.is_some() {
                        resource_status
                    } else {
                        doc_status
                    };
                    let d = details([
                        ("source_page", Value::from(url.clone())),
                        (
                            "source_status_code",
                            doc_status.map_or(Value::Null, Value::from),
                        ),
                        (
                            "resource_url",
                            image.resource_url.clone().map_or(Value::Null, Value::from),
                        ),
                        (
                            "resource_status_code",
                            resource_status.map_or(Value::Null, Value::from),
                        ),
                        (
                            "resource_evidence_state",
                            Value::from(if resource_status.is_some() {
                                "measured"
                            } else {
                                "unavailable"
                            }),
                        ),
                        ("status_code", status.map_or(Value::Null, Value::from)),
                        (
                            "status_scope",
                            Value::from(if image.resource_url.is_some() {
                                "resource"
                            } else {
                                "source_document"
                            }),
                        ),
                        ("target_scope", Value::from("image_element")),
                        ("document_id", Value::from(document_id)),
                        ("representation", document["representation"].clone()),
                        ("image_ordinal", Value::from(image.ordinal as i64)),
                    ]);
                    let mut findings: Vec<(&str, &str, Map<String, Value>)> = Vec::new();
                    if image.missing_alt_attribute {
                        findings.push((CODES[0], "image element has no alt attribute", Map::new()));
                    }
                    if image.missing_alt_text {
                        findings.push((
                            CODES[1],
                            "image alt is whitespace-only or an informative/unnamed interactive image has empty alt text",
                            Map::new(),
                        ));
                    }
                    if !image.missing_dimensions.is_empty() {
                        let mut facts = Map::new();
                        facts.insert(
                            "missing_attributes".into(),
                            Value::from(image.missing_dimensions.clone()),
                        );
                        findings.push((
                            CODES[2],
                            "visible image does not declare positive width and height attributes",
                            facts,
                        ));
                    }
                    if image.alt_length as i64 > limit {
                        let mut facts = Map::new();
                        facts.insert(
                            "max_alt_length".into(),
                            Value::from(image.alt_length as i64),
                        );
                        facts.insert("max_chars".into(), Value::from(limit));
                        findings.push((
                            CODES[3],
                            "image alt text exceeds the configured character threshold",
                            facts,
                        ));
                    }
                    for (code, reason, facts) in findings {
                        let mut finding = d.clone();
                        finding.extend(facts);
                        finding.insert("reason".into(), Value::from(reason));
                        ops.push(Op::Finding {
                            code,
                            target: image.resource_url.clone().unwrap_or_else(|| url.clone()),
                            status,
                            source_url: url.clone(),
                            link_path: format!("img[{}]", image.ordinal),
                            details: finding,
                        });
                    }
                }
                if images.len() > MAX_RETAINED_IMAGES {
                    let sampled_missing = images
                        .iter()
                        .take(MAX_RETAINED_IMAGES)
                        .filter(|i| i.missing_alt_attribute)
                        .count() as i64;
                    let sampled_long = images
                        .iter()
                        .take(MAX_RETAINED_IMAGES)
                        .any(|i| i.alt_length as i64 > limit);
                    ops.push(Op::Fallback {
                        document,
                        sampled_missing,
                        sampled_long,
                    });
                }
            }
        }
    }
    for op in ops {
        match op {
            Op::Fallback {
                document,
                sampled_missing,
                sampled_long,
            } => retained_image_count_fallback(ctx, &document, sampled_missing, sampled_long),
            Op::Finding {
                code,
                target,
                status,
                source_url,
                link_path,
                details: d,
            } => ctx.add_source(
                code,
                Some(&target),
                status,
                1,
                (
                    "native-retained-image-markup",
                    vec![serde_json::json!({
                        "source_url": source_url,
                        "link_path": link_path,
                    })],
                ),
                d,
            ),
        }
    }
    let reason = "not fully measured by this native crawl: active HTML/DOM is absent, incomplete, exceeds 2 MiB, or more than 2000 visible images were declared";
    for code in CODES {
        ctx.measure(
            code,
            "pages",
            if available { Some(eligible) } else { None },
            measured,
            partial,
            reason,
        );
    }
}

/// `JS_REDIRECT` reads the retained
/// navigation evidence of each HTML page's selected document — a `script_navigation`
/// event that moved the main frame is a script-initiated redirect.
pub fn check_native_script_redirects(ctx: &mut AuditContext) {
    super::install_render_checks();
    let Some(con) = ctx.scan.take() else {
        return;
    };
    let sql = format!(
        "SELECT p.document_id,u.url FROM pages p JOIN urls u ON u.url_id=p.url_id \
         WHERE p.document_id IS NOT NULL AND {HTML_WHERE}"
    );
    let rows: Vec<(i64, String)> = con
        .prepare(&sql)
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .ok()
                .map(|q| q.flatten().collect())
        })
        .unwrap_or_default();
    let eligible = super::asset_checks::eligible_pages(ctx);
    let mut measured = 0i64;
    let mut partial = 0i64;
    let mut unreadable = 0i64;
    let mut issues: Vec<(String, Map<String, Value>)> = Vec::new();
    for (document_id, page_url) in rows {
        let navigation = spiderhead_store::bodies::read_document_navigation(&con, document_id)
            .ok()
            .and_then(|doc| doc.get("navigation").and_then(Value::as_object).cloned());
        let Some(navigation) = navigation else {
            unreadable += 1;
            continue;
        };
        let state = navigation
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("");
        if state != "complete" && state != "partial" {
            continue;
        }
        measured += 1;
        partial +=
            i64::from(state == "partial" || super::py_truthy(navigation.get("events_omitted")));
        let hops: Vec<Value> = navigation
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|event| {
                event.get("kind").and_then(Value::as_str) == Some("script_navigation")
                    && navigation_moved(event.get("source"), event.get("destination"))
            })
            .take(10)
            .map(|event| {
                let mut hop = Map::new();
                for key in ["source", "destination", "elapsed_ms"] {
                    hop.insert(key.into(), event.get(key).cloned().unwrap_or(Value::Null));
                }
                Value::Object(hop)
            })
            .collect();
        if !hops.is_empty() {
            issues.push((
                page_url,
                Map::from_iter([
                    ("hops".into(), Value::Array(hops.clone())),
                    ("hops_shown".into(), Value::from(hops.len() as i64)),
                    ("observation".into(), Value::from("script_navigation")),
                    ("coverage".into(), Value::from(state)),
                    (
                        "reason".into(),
                        Value::from(
                            "the recorded render navigation moved the main frame to a new \
                             location because a script asked for it, not because a server \
                             redirected",
                        ),
                    ),
                ]),
            ));
        }
    }
    ctx.scan = Some(con);
    for (url, details) in issues {
        ctx.add("JS_REDIRECT", Some(&url), details);
    }
    let reason = format!(
        "not fully measured by this native crawl: retained navigation evidence is absent, \
         bounded or unreadable{}; raw-only scans and SF exports keep none",
        if unreadable > 0 {
            format!(" ({unreadable} document(s) unreadable)")
        } else {
            String::new()
        }
    );
    ctx.measure(
        "JS_REDIRECT",
        "pages",
        Some(eligible),
        measured,
        partial,
        &reason,
    );
}

/// Whether the event changed the document, not just its fragment.
fn navigation_moved(source: Option<&Value>, destination: Option<&Value>) -> bool {
    let (Some(a), Some(b)) = (
        source.and_then(Value::as_str),
        destination.and_then(Value::as_str),
    ) else {
        return false;
    };
    let (Ok(a), Ok(b)) = (urlsplit(a), urlsplit(b)) else {
        return false;
    };
    nav_parts(&a) != nav_parts(&b)
}

fn nav_parts(u: &spiderhead_parse::py_url::SplitUrl) -> (&str, &str, &str, &str) {
    (
        u.scheme.as_str(),
        u.netloc.as_str(),
        if u.path.is_empty() {
            "/"
        } else {
            u.path.as_str()
        },
        u.query.as_str(),
    )
}

/// `check_native_export_aggregates`: native image declarations and the unmeasured aggregate
/// HREFLANG_ERROR umbrella.
pub fn check_native_export_aggregates(ctx: &mut AuditContext) {
    if ctx.scan.is_none() {
        return;
    }
    measure_retained_image_declarations(ctx);
    ctx.skip(
        "HREFLANG_ERROR",
        "not measured by this native crawl as one aggregate; the native hreflang checks \
         report each error kind on its own",
    );
}

/// Judge each page's declared Content-Type
/// against the retained selected-document body signature.
pub fn check_body_mime(ctx: &mut AuditContext) {
    super::install_render_checks();
    if ctx.scan.is_none() {
        ctx.skip(
            super::body_mime::CHECK_ID,
            "export-only analysis retains no per-URL response bodies; a native retained scan is required",
        );
        return;
    }
    let Some(con) = ctx.scan.take() else {
        return;
    };
    let evidence = super::body_mime::evaluate(&con);
    ctx.scan = Some(con);
    let check = super::body_mime::CHECK_ID;
    for (target_url, item) in &evidence.findings {
        ctx.add(check, Some(target_url), item.clone());
    }
    let eligible = evidence.eligible;
    let measured = evidence.measured;
    let unavailable = evidence.unavailable;
    let measurement_reason = if eligible == 0 {
        "no eligible page response with a supported Content-Type"
    } else if unavailable > 0 {
        "some supported response bodies were unavailable or ambiguous"
    } else {
        ""
    };
    let state = if eligible == 0 || measured == 0 {
        "unavailable"
    } else if unavailable > 0 {
        "partial"
    } else {
        "measured"
    };
    let reason_counts: String = {
        let mut pairs: Vec<(&String, &serde_json::Value)> =
            evidence.unavailable_reasons.iter().collect();
        pairs.sort_by(|a, b| a.0.cmp(b.0));
        pairs
            .iter()
            .map(|(name, count)| format!("{}={}", name, count.as_i64().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut metrics = Map::new();
    metrics.insert("unit".into(), Value::from("pages"));
    metrics.insert("eligible".into(), Value::from(eligible));
    metrics.insert("measured".into(), Value::from(measured));
    metrics.insert("partial".into(), Value::from(0));
    metrics.insert("unavailable".into(), Value::from(unavailable));
    metrics.insert("state".into(), Value::from(state));
    metrics.insert("reason".into(), Value::from(measurement_reason));
    metrics.insert(
        "unavailable_reasons".into(),
        Value::Object(evidence.unavailable_reasons),
    );
    metrics.insert(
        "body_retention_states".into(),
        Value::Object(evidence.body_retention_states),
    );
    ctx.asset_measurements
        .insert(check.into(), Value::Object(metrics));
    if evidence.findings_count == 0 {
        if unavailable > 0 {
            ctx.skip(
                check,
                &format!("page body evidence was incomplete: {reason_counts}"),
            );
        } else if measured == 0 {
            ctx.skip(
                check,
                "no page response had a supported Content-Type and recognizable body signature",
            );
        }
    }
}
