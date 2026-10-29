//! Asset findings from retained native evidence (reference the reference suite):
//! rendered image sizing and coverage, page weight, declared-resource checks,
//! whole-page network payload and supplied Lighthouse lab reports.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::types::Value as Sql;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use spiderhead_store::resources::media_matches;
use spiderhead_store::scan_artifact;

use super::details;
use super::py_truthy;
use crate::audit::AuditContext;

const RENDER_CHECKS: [&str; 3] = ["IMG_INTRINSIC_OVERSIZED", "IMG_UPSCALED", "UNUSED_CSS_JS"];
const RESOURCE_CODES: [&str; 6] = [
    "RESOURCE_OVER_2MB",
    "ASSET_SHORT_CACHE",
    "FONT_DISPLAY_MISSING",
    "UNMINIFIED_CSS_JS",
    "DUPLICATED_JS",
    "LEGACY_JS",
];
const UNUSED_RATIO_THRESHOLD: f64 = 0.5;
const UNUSED_MIN_CHARS: f64 = 20_000.0;
const MIB: f64 = 1024.0 * 1024.0;

const HTML_WHERE: &str =
    "p.status_code BETWEEN 200 AND 299 AND lower(p.content_type) LIKE '%html%'";
const RAW_DOCUMENT_JOIN: &str =
    " LEFT JOIN (SELECT d.url_id,MAX(d.document_id) AS document_id FROM documents d \
    JOIN context_items c ON c.kind='resource_inventory' AND c.item_key='document:'||d.document_id \
    WHERE d.representation='static' GROUP BY d.url_id) raw \
    ON raw.url_id=p.url_id AND p.representation!='static' ";
const ACTIVE_DOCUMENT_WHERE: &str =
    "(d.document_id=p.document_id OR d.document_id=raw.document_id)";
const INCOMPLETE_COVERAGE: &str = "(selected.document_id IS NULL OR c.completeness IS NULL OR c.completeness!='complete' OR \
    CASE WHEN json_valid(c.payload_json) THEN COALESCE(json_extract(c.payload_json,'$.state') IN ('complete','empty') \
    AND json_extract(c.payload_json,'$.omitted')=0,0) ELSE 0 END=0)";

/// `_read_json`: an object, given directly or as JSON text.
fn read_json(value: Option<&Value>) -> Option<Map<String, Value>> {
    match value? {
        Value::String(text) => match serde_json::from_str::<Value>(text).ok()? {
            Value::Object(map) => Some(map),
            _ => None,
        },
        Value::Object(map) => Some(map.clone()),
        _ => None,
    }
}

/// A non-negative JSON integer (reference `type(x) is int and x >= 0`).
fn nonneg_int(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64).filter(|n| *n >= 0)
}

/// One eligible page: url, captured HTML bytes, renderer settings blob.
struct AssetPage {
    url: String,
    html_size: Option<i64>,
    representation: Option<String>,
    renderer_json: Option<String>,
}

fn asset_pages(ctx: &AuditContext) -> Vec<AssetPage> {
    let Some(con) = ctx.scan.as_ref() else {
        return ctx
            .html_pages()
            .into_iter()
            .map(|i| AssetPage {
                url: ctx.pages[i].url.clone(),
                html_size: None,
                representation: None,
                renderer_json: None,
            })
            .collect();
    };
    let sql = format!(
        "SELECT u.url,CASE WHEN p.representation='static' THEN p.size_bytes ELSE b.decoded_bytes END,\
         d.representation,d.renderer_json FROM pages p JOIN urls u ON u.url_id=p.url_id \
         LEFT JOIN documents d ON d.document_id=p.document_id {RAW_DOCUMENT_JOIN}\
         LEFT JOIN documents sd ON sd.document_id=raw.document_id LEFT JOIN bodies b ON b.sha256=sd.body_sha256 \
         WHERE {HTML_WHERE}"
    );
    let Ok(mut stmt) = con.prepare(&sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok(AssetPage {
            url: r.get(0)?,
            html_size: match r.get::<_, Sql>(1)? {
                Sql::Integer(n) => Some(n),
                _ => None,
            },
            representation: r.get(2)?,
            renderer_json: r.get(3)?,
        })
    }) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

pub(crate) fn eligible_pages(ctx: &AuditContext) -> i64 {
    match ctx.scan.as_ref() {
        Some(con) => con
            .query_row(
                &format!("SELECT COUNT(*) FROM pages p WHERE {HTML_WHERE}"),
                [],
                |r| r.get(0),
            )
            .unwrap_or(0),
        None => ctx.html_pages().len() as i64,
    }
}

/// `_asset_evidence`: the render-time asset evidence of a rendered document.
fn asset_evidence(representation: Option<&str>, renderer_json: Option<&str>) -> Map<String, Value> {
    if representation != Some("rendered") {
        return Map::new();
    }
    let renderer = renderer_json.map(|t| Value::String(t.to_owned()));
    let settings = read_json(
        read_json(renderer.as_ref())
            .and_then(|r| r.get("settings").cloned())
            .as_ref(),
    )
    .unwrap_or_default();
    read_json(settings.get("asset_evidence")).unwrap_or_default()
}

fn get_f64(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// `TRACE_CHECKS`: the six lab checks and their Lighthouse audit ids, in order.
const TRACE_CHECKS: [(&str, &str); 6] = [
    ("LCP_DISCOVERY", "lcp-discovery-insight"),
    ("NETWORK_DEPENDENCY_TREE", "network-dependency-tree-insight"),
    ("LAYOUT_SHIFT_CULPRITS", "cls-culprits-insight"),
    ("IMAGE_DELIVERY", "image-delivery-insight"),
    ("FORCED_REFLOW", "forced-reflow-insight"),
    ("JS_EXECUTION_TIME", "bootup-time"),
];
const MAX_LIGHTHOUSE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LIGHTHOUSE_REPORTS: usize = 2000;
const MAX_LIGHTHOUSE_DETAILS: usize = 64 * 1024;
const LIGHTHOUSE_UNMEASURED: &str = "not fully measured by this native crawl: supply \
     input.lighthouse_reports with matching per-URL Lighthouse JSON for every \
     eligible HTML page; no trace was collected for missing reports";

/// Length of the `json.dumps(ensure_ascii=True)` text for a value: `serde_json`
/// emits non-ASCII raw while the reference serializer escapes it (`\uXXXX`, a
/// surrogate pair above the BMP). The check's 64 KiB budget counts the escaped
/// length, so the encoding has to be reproduced byte-for-byte.
pub(crate) fn ascii_dump_len(value: &Value) -> usize {
    serde_json::to_string(value)
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c == '\u{7f}' {
                // ensure_ascii escapes DEL (\u007f); serde_json emits it raw.
                6
            } else if c.is_ascii() {
                1
            } else if (c as u32) <= 0xFFFF {
                6
            } else {
                12
            }
        })
        .sum()
}

/// BOM first, then a guess from the NUL byte pattern, as JSON decoders do
/// on BOM-free UTF-16/32 text.
pub(crate) fn decode_json_bytes(raw: &[u8]) -> Option<String> {
    if raw.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return decode_utf32(&raw[4..], true);
    }
    if raw.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return decode_utf32(&raw[4..], false);
    }
    if raw.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(&raw[2..], true);
    }
    if raw.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(&raw[2..], false);
    }
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(raw[3..].to_vec()).ok();
    }
    if raw.len() >= 4 {
        if raw[0] == 0 {
            return if raw[1] == 0 {
                decode_utf32(raw, true)
            } else {
                decode_utf16(raw, true)
            };
        }
        if raw[1] == 0 {
            return if raw[2] == 0 && raw[3] == 0 {
                decode_utf32(raw, false)
            } else {
                decode_utf16(raw, false)
            };
        }
        return String::from_utf8(raw.to_vec()).ok();
    }
    if raw.len() == 2 {
        if raw[0] == 0 {
            return decode_utf16(raw, true);
        }
        if raw[1] == 0 {
            return decode_utf16(raw, false);
        }
    }
    String::from_utf8(raw.to_vec()).ok()
}

fn decode_utf16(raw: &[u8], big_endian: bool) -> Option<String> {
    if !raw.len().is_multiple_of(2) {
        return None;
    }
    let (chunks, _) = raw.as_chunks::<2>();
    let units: Vec<u16> = chunks
        .iter()
        .map(|c| {
            if big_endian {
                u16::from_be_bytes(*c)
            } else {
                u16::from_le_bytes(*c)
            }
        })
        .collect();
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

fn decode_utf32(raw: &[u8], big_endian: bool) -> Option<String> {
    if !raw.len().is_multiple_of(4) {
        return None;
    }
    let (chunks, _) = raw.as_chunks::<4>();
    chunks
        .iter()
        .map(|c| {
            let unit = if big_endian {
                u32::from_be_bytes(*c)
            } else {
                u32::from_le_bytes(*c)
            };
            char::from_u32(unit)
        })
        .collect()
}

/// `check_lighthouse_reports`: per-URL supplied Lighthouse JSON drives the six
/// lab checks. Each report is byte-budgeted, must answer for its crawl URL and
/// carry a measured `[0, 1]` score per audit before it counts toward coverage.
pub fn check_lighthouse_reports(ctx: &mut AuditContext) {
    let eligible = eligible_pages(ctx);
    let reports_value = ctx
        .config
        .get("input")
        .and_then(|input| input.get("lighthouse_reports"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut measured: BTreeMap<&str, i64> = TRACE_CHECKS.iter().map(|(c, _)| (*c, 0)).collect();
    let mut reasons: BTreeSet<String> = BTreeSet::new();
    let reports: Map<String, Value> = match reports_value.as_object() {
        Some(map) if map.len() <= MAX_LIGHTHOUSE_REPORTS => map.clone(),
        _ => {
            reasons.insert(
                "per-URL Lighthouse input exceeds the 2000-report bound or is not a URL/path object"
                    .to_owned(),
            );
            Map::new()
        }
    };
    for (url, filename) in &reports {
        let page = ctx.page_index(url).map(|i| &ctx.pages[i]);
        let Some(page) = page.filter(|p| p.is_html() && p.is_2xx()) else {
            reasons.insert(
                "Lighthouse input names a URL absent from the eligible crawl pages".to_owned(),
            );
            continue;
        };
        let status_code = page.status_code;
        let report = filename.as_str().and_then(|name| {
            use std::io::Read;
            let file = std::fs::File::open(name).ok()?;
            let mut raw = Vec::new();
            file.take(MAX_LIGHTHOUSE_BYTES + 1)
                .read_to_end(&mut raw)
                .ok()?;
            (raw.len() as u64 <= MAX_LIGHTHOUSE_BYTES).then_some(raw)
        });
        let Some(raw) = report else {
            reasons.insert(
                "supplied Lighthouse report is unreadable, oversized, malformed or belongs to another URL".to_owned(),
            );
            continue;
        };
        let text = decode_json_bytes(&raw);
        let report = text
            .as_deref()
            .and_then(|t| scan_artifact::loads(t, "supplied Lighthouse report").ok())
            .filter(|v| {
                v.is_object()
                    && v.get("finalUrl").and_then(Value::as_str) == Some(url.as_str())
                    && !py_truthy(v.get("runtimeError"))
            });
        let audits = report
            .as_ref()
            .and_then(|r| r.get("audits"))
            .and_then(Value::as_object)
            .cloned();
        let (Some(report), Some(audits)) = (report, audits) else {
            reasons.insert(
                "supplied Lighthouse report is unreadable, oversized, malformed or belongs to another URL".to_owned(),
            );
            continue;
        };
        for (check, ident) in TRACE_CHECKS {
            let Some(audit) = audits.get(ident).and_then(Value::as_object) else {
                reasons.insert(format!("supplied Lighthouse report has no {ident} result"));
                continue;
            };
            let score = audit.get("score").and_then(Value::as_f64);
            let measured_ok = score.is_some_and(|s| (0.0..=1.0).contains(&s))
                && !py_truthy(audit.get("errorMessage"));
            if !measured_ok {
                reasons.insert(format!("Lighthouse {ident} was not measured"));
                continue;
            }
            let score = score.unwrap_or(0.0);
            if score < 1.0 {
                let details = audit.get("details").cloned().unwrap_or(Value::Null);
                let encoded = ascii_dump_len(&details);
                let lighthouse_version = report
                    .get("lighthouseVersion")
                    .and_then(Value::as_str)
                    .filter(|v| v.chars().count() <= 128);
                let fetch_time = report
                    .get("fetchTime")
                    .and_then(Value::as_str)
                    .filter(|v| v.chars().count() <= 128);
                ctx.add_with(
                    check,
                    Some(url),
                    Map::from_iter([
                        ("source".into(), Value::from("supplied-lighthouse")),
                        ("audit_id".into(), Value::from(ident)),
                        ("score".into(), Value::from(score)),
                        (
                            "lighthouse_version".into(),
                            lighthouse_version.map(Value::from).unwrap_or(Value::Null),
                        ),
                        (
                            "fetch_time".into(),
                            fetch_time.map(Value::from).unwrap_or(Value::Null),
                        ),
                        (
                            "input_sha256".into(),
                            Value::from(format!("{:x}", Sha256::digest(&raw))),
                        ),
                        (
                            "details".into(),
                            if encoded <= MAX_LIGHTHOUSE_DETAILS {
                                details
                            } else {
                                Value::Null
                            },
                        ),
                        (
                            "details_omitted".into(),
                            Value::from(encoded > MAX_LIGHTHOUSE_DETAILS),
                        ),
                        (
                            "reason".into(),
                            Value::from(
                                "the supplied Lighthouse lab audit reported an optimization opportunity",
                            ),
                        ),
                    ]),
                    status_code,
                    1,
                    None,
                );
            }
            if let Some(count) = measured.get_mut(check) {
                *count += 1;
            }
        }
    }
    let reason = if reasons.is_empty() {
        LIGHTHOUSE_UNMEASURED.to_owned()
    } else {
        reasons.into_iter().collect::<Vec<_>>().join("; ")
    };
    for (check, _) in TRACE_CHECKS {
        ctx.measure(
            check,
            "pages",
            Some(eligible),
            measured.get(check).copied().unwrap_or(0),
            0,
            &reason,
        );
    }
}

/// `check_render_assets`: image sizing and unused-source findings from render layout/coverage.
pub fn check_render_assets(ctx: &mut AuditContext) {
    let mut measured = [0i64; 3];
    let mut partial = [0i64; 3];
    let mut eligible = 0i64;
    for page in asset_pages(ctx) {
        eligible += 1;
        let evidence = asset_evidence(
            page.representation.as_deref(),
            page.renderer_json.as_deref(),
        );
        let layout = read_json(evidence.get("layout")).unwrap_or_default();
        let layout_state = layout.get("state").and_then(Value::as_str).unwrap_or("");
        if matches!(layout_state, "complete" | "partial") {
            for slot in 0..2 {
                measured[slot] += 1;
                partial[slot] += i64::from(layout_state == "partial");
            }
            let sizing = layout.get("image_sizing").filter(|v| !v.is_null());
            for (key, code) in [
                ("oversized", "IMG_INTRINSIC_OVERSIZED"),
                ("upscaled", "IMG_UPSCALED"),
            ] {
                let items = sizing.and_then(|s| s.get(key)).and_then(Value::as_array);
                for item in items.into_iter().flatten() {
                    let Some(map) = item.as_object() else {
                        continue;
                    };
                    let target = map.get("src").and_then(Value::as_str);
                    let mut d = map.clone();
                    d.insert("source_page".into(), json!(page.url));
                    d.insert("source".into(), json!("native-render-layout"));
                    d.insert(
                        "min_encoded_bytes".into(),
                        layout
                            .get("min_encoded_bytes")
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                    d.insert(
                        "oversized_ratio".into(),
                        layout
                            .get("oversized_ratio")
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                    d.insert("coverage".into(), json!(layout_state));
                    d.insert(
                        "reason".into(),
                        json!("intrinsic width disagrees with the displayed device-pixel width"),
                    );
                    ctx.add(code, target, d);
                }
            }
        }
        let coverage = read_json(evidence.get("coverage")).unwrap_or_default();
        let coverage_state = coverage.get("state").and_then(Value::as_str).unwrap_or("");
        if !matches!(coverage_state, "complete" | "partial") {
            continue;
        }
        measured[2] += 1;
        let summaries: Vec<Map<String, Value>> = ["js", "css"]
            .iter()
            .map(|kind| read_json_lenient(coverage.get(*kind)))
            .collect();
        let any_skipped = summaries
            .iter()
            .any(|s| crate::audit::rules::py_truthy(s.get("skipped")));
        partial[2] += i64::from(coverage_state == "partial" || any_skipped);
        for (kind, summary) in ["js", "css"].iter().zip(&summaries) {
            let items = summary.get("resources").and_then(Value::as_array);
            for item in items.into_iter().flatten() {
                let Some(map) = item.as_object() else {
                    continue;
                };
                if get_f64(item, "unused_ratio") >= UNUSED_RATIO_THRESHOLD
                    && get_f64(item, "unused_chars") >= UNUSED_MIN_CHARS
                {
                    let mut d = map.clone();
                    d.insert("kind".into(), json!(kind));
                    d.insert("source".into(), json!("native-render-coverage"));
                    d.insert(
                        "unit".into(),
                        coverage.get("unit").cloned().unwrap_or(Value::Null),
                    );
                    d.insert("coverage".into(), json!(coverage_state));
                    d.insert(
                        "reason".into(),
                        json!("source was unused in the recorded navigation window; later interactions were not tested"),
                    );
                    ctx.add("UNUSED_CSS_JS", Some(&page.url), d);
                }
            }
        }
    }
    for (slot, code) in RENDER_CHECKS.iter().enumerate() {
        ctx.measure(
            code,
            "pages",
            Some(eligible),
            measured[slot],
            partial[slot],
            "not fully measured by this native crawl: retained render layout or Chromium coverage is absent, unavailable or bounded for eligible HTML pages",
        );
    }
}

/// `coverage.get(kind) or {}` where a non-object value reads as an empty summary.
fn read_json_lenient(value: Option<&Value>) -> Map<String, Value> {
    match value {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn check_page_weight(ctx: &mut AuditContext) {
    let (mut eligible, mut measured, mut partial) = (0i64, 0i64, 0i64);
    let limit = ctx
        .thresholds
        .get("page_weight_max_bytes")
        .unwrap_or(2.0 * MIB);
    for page in asset_pages(ctx) {
        eligible += 1;
        let evidence = asset_evidence(
            page.representation.as_deref(),
            page.renderer_json.as_deref(),
        );
        let weight = read_json(evidence.get("weight")).unwrap_or_default();
        let state = weight.get("state").and_then(Value::as_str).unwrap_or("");
        let (size, d, native) = if let (true, Some(size)) = (
            matches!(state, "complete" | "partial"),
            nonneg_int(weight.get("bytes")),
        ) {
            measured += 1;
            partial += i64::from(state == "partial");
            let mut d = weight.clone();
            d.insert("source".into(), json!("native-render-responses"));
            d.insert(
                "size_state".into(),
                json!(if state == "complete" {
                    "complete"
                } else {
                    "lower_bound"
                }),
            );
            (size, d, true)
        } else if let Some(size) = page.html_size.filter(|n| *n >= 0) {
            measured += 1;
            partial += 1;
            let d = details([
                ("source", json!("native-html-response")),
                ("bytes", json!(size)),
                ("html_bytes", json!(size)),
                ("image_bytes", Value::Null),
                ("font_bytes", Value::Null),
                ("third_party_bytes", Value::Null),
                ("unit", json!("captured HTML bytes")),
                (
                    "scope",
                    json!("HTML response only; runtime assets unavailable"),
                ),
                ("size_state", json!("lower_bound")),
            ]);
            (size, d, false)
        } else {
            continue;
        };
        if size as f64 > limit {
            let mut d = d;
            d.insert("threshold_bytes".into(), super::num(limit));
            d.insert(
                "reason".into(),
                json!(if native {
                    "observed navigation bytes exceed the page budget"
                } else {
                    "captured HTML alone exceeds the page budget; runtime asset weight was not measured"
                }),
            );
            ctx.add("HEAVY_PAGE_WEIGHT", Some(&page.url), d);
        }
    }
    ctx.measure(
        "HEAVY_PAGE_WEIGHT",
        "pages",
        Some(eligible),
        measured,
        partial,
        "not fully measured by this native crawl: runtime response weight is absent or bounded; captured HTML alone is a lower bound",
    );
}

/// Whether `table` has a column called `column`.
fn has_table_column(con: &rusqlite::Connection, table: &str, column: &str) -> bool {
    con.prepare(&format!("PRAGMA table_info({table})"))
        .ok()
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(1))
                .ok()
                .map(|rows| rows.flatten().any(|n| n == column))
        })
        .unwrap_or(false)
}

fn table_names(con: &rusqlite::Connection) -> BTreeSet<String> {
    con.prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .ok()
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))
                .ok()
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default()
}

/// A declared resource joined to its retained fetch.
struct ResourceRow {
    resource: String,
    source: String,
    kind: String,
    status: Option<i64>,
    size: Option<i64>,
    state: Option<String>,
    media: Option<String>,
    delivery_json: Option<String>,
}

fn is_bool(value: Option<&Value>) -> Option<bool> {
    value.and_then(Value::as_bool)
}

/// `check_resource_assets`: page weight plus per-resource budget, cache and source checks.
pub fn check_resource_assets(ctx: &mut AuditContext) {
    check_page_weight(ctx);
    let con = ctx.scan.take();
    let mut eligible: BTreeMap<&str, i64> = RESOURCE_CODES.iter().map(|c| (*c, 0)).collect();
    let mut measured = eligible.clone();
    let mut partial = eligible.clone();
    let mut range_response = false;
    let mut unknown = true;
    if let Some(con) = con.as_ref() {
        let tables = table_names(con);
        if ["resource_graph_occurrences", "resource_graph_fetches"]
            .iter()
            .all(|t| tables.contains(*t))
        {
            let count: i64 = con
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pages p LEFT JOIN documents selected ON selected.document_id=p.document_id \
                         {RAW_DOCUMENT_JOIN}LEFT JOIN documents d ON d.document_id=p.document_id OR d.document_id=raw.document_id \
                         LEFT JOIN context_items c ON c.kind='resource_graph_coverage' \
                         AND c.item_key='document:'||d.document_id WHERE {HTML_WHERE} AND {INCOMPLETE_COVERAGE}"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(1);
            unknown = count != 0;
            let has_delivery = has_table_column(con, "resource_graph_fetches", "delivery_json");
            let delivery = if has_delivery {
                "f.delivery_json"
            } else {
                "NULL"
            };
            let sql = format!(
                "SELECT o.resolved_url,o.source,o.kind,f.status_code,f.bytes_received,f.state,f.content_type,{delivery} FROM (\
                 SELECT o.resolved_url,MIN(u.url) AS source,\
                 CASE o.kind WHEN 'module' THEN 'script' WHEN 'css_import' THEN 'stylesheet' ELSE o.kind END AS kind \
                 FROM resource_graph_occurrences o JOIN pages p ON p.url_id=o.page_url_id \
                 JOIN urls u ON u.url_id=p.url_id JOIN documents d ON d.document_id=o.source_document_id \
                 {RAW_DOCUMENT_JOIN}WHERE {HTML_WHERE} AND {ACTIVE_DOCUMENT_WHERE} \
                 GROUP BY o.resolved_url,CASE o.kind WHEN 'module' THEN 'script' WHEN 'css_import' THEN 'stylesheet' ELSE o.kind END\
                 ) o LEFT JOIN resource_graph_fetches f ON f.resolved_url=o.resolved_url"
            );
            let rows: Vec<ResourceRow> = con
                .prepare(&sql)
                .ok()
                .and_then(|mut s| {
                    s.query_map([], |r| {
                        let int = |i: usize| -> rusqlite::Result<Option<i64>> {
                            Ok(match r.get::<_, Sql>(i)? {
                                Sql::Integer(n) => Some(n),
                                _ => None,
                            })
                        };
                        Ok(ResourceRow {
                            resource: r.get(0)?,
                            source: r.get(1)?,
                            kind: r.get(2)?,
                            status: int(3)?,
                            size: int(4)?,
                            state: r.get(5)?,
                            media: r.get(6)?,
                            delivery_json: r.get(7)?,
                        })
                    })
                    .ok()
                    .map(|rows| rows.flatten().collect())
                })
                .unwrap_or_default();
            for row in rows {
                resource_row(
                    ctx,
                    row,
                    &mut eligible,
                    &mut measured,
                    &mut partial,
                    &mut range_response,
                );
            }
            if has_delivery {
                duplicated_scripts(ctx, con);
            }
        }
    }
    ctx.scan = con;
    for code in RESOURCE_CODES {
        let reason = format!(
            "not fully measured by this native crawl: declared-resource evidence is absent, unavailable, partial or omits declarations; enable scan.v2 resources.fetch within explicit graph budgets{}",
            if range_response {
                "; HTTP 206 responses contain only byte ranges; whole-resource source was not measured"
            } else {
                ""
            }
        );
        ctx.measure(
            code,
            "resources",
            if unknown { None } else { Some(eligible[code]) },
            measured[code],
            partial[code],
            &reason,
        );
    }
}

fn resource_row(
    ctx: &mut AuditContext,
    row: ResourceRow,
    eligible: &mut BTreeMap<&str, i64>,
    measured: &mut BTreeMap<&str, i64>,
    partial: &mut BTreeMap<&str, i64>,
    range_response: &mut bool,
) {
    let ResourceRow {
        resource,
        source,
        kind,
        status,
        size,
        media,
        delivery_json,
        ..
    } = row;
    let mut state = row.state;
    // A 206 fragment is never whole-resource evidence.
    if status == Some(206) && matches!(state.as_deref(), Some("complete" | "partial")) {
        *range_response = true;
        state = Some("partial".into());
    }
    let is_source = matches!(kind.as_str(), "script" | "stylesheet");
    let mut applicable = vec!["RESOURCE_OVER_2MB", "ASSET_SHORT_CACHE"];
    if is_source {
        applicable.push("UNMINIFIED_CSS_JS");
    }
    if kind == "stylesheet" {
        applicable.push("FONT_DISPLAY_MISSING");
    }
    if kind == "script" {
        applicable.extend(["LEGACY_JS", "DUPLICATED_JS"]);
    }
    for code in applicable {
        *eligible.entry(code).or_default() += 1;
    }
    let state = state.unwrap_or_default();
    if !matches!(state.as_str(), "complete" | "partial") {
        return;
    }
    let is_partial = i64::from(state == "partial");
    let base = details([
        ("source", json!("native-resource-graph")),
        ("source_page", json!(source)),
        ("resource_url", json!(resource)),
        ("kind", json!(kind)),
        ("status_code", json!(status)),
        ("bytes", json!(size)),
        (
            "size_state",
            json!(if state == "complete" {
                "complete"
            } else {
                "lower_bound"
            }),
        ),
    ]);
    if let Some(bytes) = size.filter(|n| *n >= 0) {
        *measured.entry("RESOURCE_OVER_2MB").or_default() += 1;
        *partial.entry("RESOURCE_OVER_2MB").or_default() += is_partial;
        let limit = ctx
            .thresholds
            .get("resource_max_bytes")
            .unwrap_or(2.0 * MIB);
        if bytes as f64 > limit {
            let mut d = base.clone();
            d.insert("threshold_bytes".into(), super::num(limit));
            d.insert(
                "reason".into(),
                json!("captured resource bytes exceed the configured resource budget"),
            );
            ctx.add("RESOURCE_OVER_2MB", Some(&resource), d);
        }
    }
    let info = read_json(
        delivery_json
            .as_ref()
            .map(|t| Value::String(t.clone()))
            .as_ref(),
    )
    .unwrap_or_default();
    let cache = read_json(info.get("cache")).unwrap_or_default();
    if let Some(ok) = is_bool(cache.get("ok")) {
        *measured.entry("ASSET_SHORT_CACHE").or_default() += 1;
        *partial.entry("ASSET_SHORT_CACHE").or_default() += is_partial;
        if !ok {
            let mut d = base.clone();
            d.extend(cache.clone());
            ctx.add_with("ASSET_SHORT_CACHE", Some(&resource), d, status, 1, None);
        }
    }
    if state != "complete"
        || info.get("source_state").and_then(Value::as_str) != Some("complete")
        || !media_matches(&kind, media.as_deref().unwrap_or(""))
    {
        return;
    }
    if kind == "stylesheet" {
        if let Some(faces) = info.get("font_display").and_then(Value::as_array) {
            *measured.entry("FONT_DISPLAY_MISSING").or_default() += 1;
            if !faces.is_empty() {
                let mut d = base.clone();
                d.insert("font_faces".into(), Value::Array(faces.clone()));
                d.insert(
                    "font_faces_count".into(),
                    info.get("font_display_count")
                        .cloned()
                        .unwrap_or(json!(faces.len())),
                );
                d.insert(
                    "font_faces_omitted".into(),
                    info.get("font_display_omitted")
                        .cloned()
                        .unwrap_or(json!(0)),
                );
                d.insert(
                    "reason".into(),
                    json!("observed @font-face lacks swap, fallback or optional font-display"),
                );
                ctx.add("FONT_DISPLAY_MISSING", Some(&resource), d);
            }
        }
    }
    if is_source {
        if let Some(flag) = is_bool(info.get("unminified")) {
            *measured.entry("UNMINIFIED_CSS_JS").or_default() += 1;
            if flag {
                let mut d = base.clone();
                d.insert(
                    "reason".into(),
                    json!(
                        "retained source does not look minified (whitespace/line-length heuristic)"
                    ),
                );
                ctx.add("UNMINIFIED_CSS_JS", Some(&resource), d);
            }
        }
    }
    if kind == "script" {
        if let Some(flag) = is_bool(info.get("legacy_js")) {
            *measured.entry("LEGACY_JS").or_default() += 1;
            if flag {
                let mut d = base.clone();
                d.insert(
                    "reason".into(),
                    json!("retained script contains a legacy/polyfill marker (heuristic)"),
                );
                ctx.add("LEGACY_JS", Some(&resource), d);
            }
        }
        if info.get("sha256").is_some_and(Value::is_string) {
            *measured.entry("DUPLICATED_JS").or_default() += 1;
        }
    }
}

/// Distinct script URLs on one page whose captured bytes are identical.
fn duplicated_scripts(ctx: &mut AuditContext, con: &rusqlite::Connection) {
    let sql = format!(
        "SELECT u.url,json_extract(f.delivery_json,'$.sha256'),MIN(o.resolved_url),COUNT(DISTINCT o.resolved_url) \
         FROM resource_graph_occurrences o JOIN resource_graph_fetches f ON f.resolved_url=o.resolved_url \
         JOIN pages p ON p.url_id=o.page_url_id JOIN urls u ON u.url_id=p.url_id \
         JOIN documents d ON d.document_id=o.source_document_id {RAW_DOCUMENT_JOIN}\
         WHERE {HTML_WHERE} AND {ACTIVE_DOCUMENT_WHERE} AND o.kind IN ('script','module') AND f.state='complete' \
         AND f.status_code!=206 AND json_valid(f.delivery_json) \
         AND json_extract(f.delivery_json,'$.source_state')='complete' AND json_extract(f.delivery_json,'$.sha256') IS NOT NULL \
         AND lower(trim(substr(f.content_type,1,instr(f.content_type||';',';')-1))) IN \
         ('text/javascript','application/javascript','text/ecmascript','application/ecmascript','application/x-javascript') \
         GROUP BY o.page_url_id,json_extract(f.delivery_json,'$.sha256') HAVING COUNT(DISTINCT o.resolved_url)>1"
    );
    let rows: Vec<(String, String, String, i64)> = con
        .prepare(&sql)
        .ok()
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .ok()
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();
    for (url, digest, resource, count) in rows {
        ctx.add(
            "DUPLICATED_JS",
            Some(&url),
            details([
                ("source", json!("native-resource-graph")),
                ("sha256", json!(digest)),
                ("resource_sample", json!(resource)),
                ("distinct_resources", json!(count)),
                (
                    "reason",
                    json!("different script URLs on this page contain identical captured source bytes"),
                ),
            ]),
        );
    }
}

/// `check_network_payload`: document plus captured subresource bytes per page.
pub fn check_network_payload(ctx: &mut AuditContext) {
    let Some(con) = ctx.scan.take() else { return };
    let tables = table_names(&con);
    let needed = [
        "resource_graph_occurrences",
        "resource_graph_fetches",
        "pages",
        "urls",
        "documents",
        "context_items",
        "bodies",
    ];
    if !needed.iter().all(|t| tables.contains(*t)) {
        ctx.scan = Some(con);
        ctx.skip(
            "HEAVY_NETWORK_PAYLOAD",
            "no retained resource graph; captured HTML alone is the only payload evidence",
        );
        return;
    }
    let declarations = format!(
        "SELECT o.page_url_id, o.resolved_url FROM resource_graph_occurrences o JOIN pages p ON p.url_id=o.page_url_id \
         JOIN documents d ON d.document_id=o.source_document_id {RAW_DOCUMENT_JOIN}WHERE {HTML_WHERE} AND {ACTIVE_DOCUMENT_WHERE} \
         GROUP BY o.page_url_id, o.resolved_url"
    );
    let mut sums: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let sum_sql = format!(
        "SELECT u.url,COALESCE(SUM(f.bytes_received),0),\
         COALESCE(SUM(CASE WHEN f.state IN ('complete','partial') THEN 0 ELSE 1 END),0) \
         FROM ({declarations}) decl JOIN urls u ON u.url_id=decl.page_url_id \
         LEFT JOIN resource_graph_fetches f ON f.resolved_url=decl.resolved_url GROUP BY decl.page_url_id"
    );
    if let Ok(mut stmt) = con.prepare(&sum_sql) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        }) {
            for (url, bytes, unmeasured) in rows.flatten() {
                sums.insert(url, (bytes, unmeasured));
            }
        }
    }
    let incomplete_sql = format!(
        "SELECT u.url FROM pages p JOIN urls u ON u.url_id=p.url_id \
         LEFT JOIN documents selected ON selected.document_id=p.document_id {RAW_DOCUMENT_JOIN}\
         LEFT JOIN documents d ON d.document_id=p.document_id OR d.document_id=raw.document_id \
         LEFT JOIN context_items c ON c.kind='resource_graph_coverage' AND c.item_key='document:'||d.document_id \
         WHERE {HTML_WHERE} AND {INCOMPLETE_COVERAGE}"
    );
    let incomplete: BTreeSet<String> = con
        .prepare(&incomplete_sql)
        .ok()
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))
                .ok()
                .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default();
    ctx.scan = Some(con);
    let limit = ctx
        .thresholds
        .get("network_payload_max_bytes")
        .unwrap_or(4.0 * MIB);
    let eligible = eligible_pages(ctx);
    let (mut measured, mut partial) = (0i64, 0i64);
    for page in asset_pages(ctx) {
        let (resource_bytes, unmeasured) = sums.get(&page.url).copied().unwrap_or((0, 0));
        if page.html_size.is_none() && !sums.contains_key(&page.url) {
            continue;
        }
        measured += 1;
        let lower_bound =
            incomplete.contains(&page.url) || unmeasured > 0 || page.html_size.is_none();
        partial += i64::from(lower_bound);
        let total = resource_bytes + page.html_size.unwrap_or(0);
        if total as f64 > limit {
            ctx.add(
                "HEAVY_NETWORK_PAYLOAD",
                Some(&page.url),
                details([
                    ("source", json!("native-resource-graph")),
                    ("document_bytes", json!(page.html_size)),
                    ("resource_bytes", json!(resource_bytes)),
                    ("resources_unmeasured", json!(unmeasured)),
                    ("unit", json!("content-decoded bytes")),
                    (
                        "scope",
                        json!("captured declared subresources fetched individually; cached transfers and navigations beyond the recorded window are not modelled"),
                    ),
                    (
                        "size_state",
                        json!(if lower_bound { "lower_bound" } else { "complete" }),
                    ),
                    ("threshold_bytes", super::num(limit)),
                    (
                        "reason",
                        json!("document plus captured subresource bytes exceed the page payload budget"),
                    ),
                ]),
            );
        }
    }
    ctx.measure(
        "HEAVY_NETWORK_PAYLOAD",
        "pages",
        Some(eligible),
        measured,
        partial,
        "not fully measured by this native crawl: declared-resource evidence is absent, incomplete or omits fetches; document bytes alone are a lower bound",
    );
}
