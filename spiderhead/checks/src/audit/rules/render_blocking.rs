//! Render-blocking check: `<head>` scripts/stylesheets that hold
//! first paint. A stored native scan re-reads the retained static documents; export
//! input parses the `render_blocking` column, and an export without the column
//! skips instead of reporting every page clean.

use rusqlite::Connection;
use serde_json::{Map, Value};
use spiderhead_parse::asset_evidence::{find_render_blocking_resources, RenderBlockingResource};
use spiderhead_parse::dom;
use spiderhead_parse::parser::tokenize::{document_base_url, DocumentSource};
use spiderhead_parse::py_json::float_repr;
use spiderhead_store::bodies::read_document;

use super::asset_checks::ascii_dump_len;
use super::{details, py_json_loads};
use crate::audit::AuditContext;

const CHECK: &str = "RENDER_BLOCKING";
/// `metrics["reason"]` when an export audit ends unmeasured or partial.
const EXPORT_PARTIAL_REASON: &str =
    "retained render-blocking markup is absent, unreadable or resource samples were bounded";
/// `metrics["reason"]` for the retained-scan path.
const RETAINED_PARTIAL_REASON: &str =
    "not fully measured by this native crawl: static HTML is absent, incomplete or exceeds the 2 MiB per-document head-analysis budget, or resource samples were bounded";
/// `metrics["reason"]` when the export lacks the column entirely.
const NO_COLUMN_REASON: &str = "no Render Blocking Resources column (needs a native seohead crawl)";
const NO_ISSUE_EXPORT_REASON: &str =
    "head markup declares render-blocking scripts or stylesheets; runtime blocking time was not measured";
const NO_ISSUE_RETAINED_REASON: &str =
    "retained source head declares blocking scripts or stylesheets; runtime blocking time was not measured";
/// `read_document(..., max_decoded_bytes)` bound for head markup (2 MiB).
const MAX_DOCUMENT_BYTES: i64 = 2 * 1024 * 1024;
/// Per-page `resources` sample budget (`sample` and the 64 KiB JSON size cap).
const SAMPLE_LIMIT: usize = 200;
const SAMPLE_BYTES: usize = 64 * 1024;

/// The `metrics` dict literal (`asset_measurements[CHECK]`): insertion order is the
/// serialized order.
fn write_metrics(
    ctx: &mut AuditContext,
    eligible: i64,
    measured: i64,
    partial: i64,
    unavailable: i64,
    state: &str,
    reason: &str,
) {
    let mut metrics = Map::new();
    metrics.insert("unit".into(), Value::from("pages"));
    metrics.insert("eligible".into(), Value::from(eligible));
    metrics.insert("measured".into(), Value::from(measured));
    metrics.insert("partial".into(), Value::from(partial));
    metrics.insert("unavailable".into(), Value::from(unavailable));
    metrics.insert("state".into(), Value::from(state));
    metrics.insert("reason".into(), Value::from(reason));
    ctx.asset_measurements
        .insert(CHECK.into(), Value::Object(metrics));
}

/// The `sample`/`sample_bytes` accumulation shared by both paths: up to 200 URLs
/// whose `json.dumps(ensure_ascii=True)` total stays inside 64 KiB (`[`, then a
/// comma+space per extra entry, `]`).
fn budgeted_sample<'a>(urls: impl Iterator<Item = &'a str>) -> Vec<Value> {
    let mut sample = Vec::new();
    let mut sample_bytes = 2usize;
    for url in urls {
        let size = ascii_dump_len(&Value::from(url)) + if sample.is_empty() { 0 } else { 2 };
        if sample.len() < SAMPLE_LIMIT && sample_bytes + size <= SAMPLE_BYTES {
            sample.push(Value::from(url));
            sample_bytes += size;
        }
    }
    sample
}

/// Text form of the scalar `render_blocking` values stringified before
/// splitting: `42`/`4.5`/`1e+20`/`True` (`json.loads` on a list or dict raises
/// `TypeError`, so containers never reach `str`).
fn scalar_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n
            .as_i64()
            .map(|i| i.to_string())
            .or_else(|| n.as_u64().map(|u| u.to_string()))
            .unwrap_or_else(|| float_repr(n.as_f64().unwrap_or(0.0))),
        Value::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        _ => String::new(),
    }
}

/// `type(v) is int` on a JSON value (bool is a separate type, floats are not int).
fn strict_int(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_u64().and_then(|u| i64::try_from(u).ok())),
        _ => None,
    }
}

/// The column payload -> (urls, observed, omitted), or `None` for every failure
/// `except (ValueError, TypeError, KeyError)` swallows.
fn parse_column(value: &Value) -> Option<(Vec<String>, i64, i64)> {
    let payload = match value {
        // `json.loads` on a non-str raises TypeError.
        Value::Array(_) | Value::Object(_) | Value::Null => return None,
        Value::String(s) => {
            if s.starts_with('[') || s.starts_with('{') {
                py_json_loads(s)?
            } else {
                Value::Array(s.split('\n').map(Value::from).collect())
            }
        }
        other => {
            let text = scalar_str(other);
            if text.starts_with('[') || text.starts_with('{') {
                return None;
            }
            Value::Array(text.split('\n').map(Value::from).collect())
        }
    };
    let (items, observed, omitted) = match payload {
        Value::Object(map) => {
            let urls = map.get("resources")?;
            let observed = strict_int(map.get("observed"))?;
            let omitted = strict_int(map.get("omitted"))?;
            if omitted < 0 {
                return None;
            }
            // `len(urls)` raises TypeError on non-sized values; strings and dicts
            // pass len() but fail `isinstance(urls, list)` right after.
            let Value::Array(items) = urls else {
                return None;
            };
            if observed != items.len() as i64 + omitted {
                return None;
            }
            (items.clone(), observed, omitted)
        }
        Value::Array(items) => {
            let observed = items.len() as i64;
            (items, observed, 0)
        }
        _ => return None,
    };
    let urls: Option<Vec<String>> = items
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect();
    urls.map(|urls| (urls, observed, omitted))
}

/// `check_render_blocking` (the reference suite).
pub fn check_render_blocking(ctx: &mut AuditContext) {
    super::install_render_checks();
    if ctx.scan.is_some() {
        check_retained_render_blocking(ctx);
        return;
    }
    if !ctx.has_column("render_blocking") {
        let count = ctx.html_pages().len() as i64;
        write_metrics(ctx, count, 0, 0, count, "unavailable", NO_COLUMN_REASON);
        ctx.skip(CHECK, NO_COLUMN_REASON);
        return;
    }
    let mut eligible = 0i64;
    let mut measured = 0i64;
    let mut partial = 0i64;
    let mut unavailable = 0i64;
    let mut issues: Vec<(String, Map<String, Value>, Option<i64>)> = Vec::new();
    for i in ctx.html_pages() {
        eligible += 1;
        let page = &ctx.pages[i];
        let value = page.rec.get("render_blocking");
        if page.body_unavailable() || matches!(value, None | Some(Value::Null)) {
            unavailable += 1;
            continue;
        }
        let Some(value) = value else {
            unavailable += 1;
            continue;
        };
        let Some((urls, observed, mut omitted)) = parse_column(value) else {
            unavailable += 1;
            continue;
        };
        measured += 1;
        let sample = budgeted_sample(urls.iter().map(String::as_str));
        omitted += (urls.len() - sample.len()) as i64;
        if omitted != 0 {
            partial += 1;
        }
        if observed != 0 {
            issues.push((
                page.url.clone(),
                details([
                    ("resources", Value::Array(sample)),
                    ("resources_omitted", Value::from(omitted)),
                    ("resources_observed", Value::from(observed)),
                    ("source", Value::from("native-head-markup")),
                    ("source_page", Value::from(page.url.as_str())),
                    ("reason", Value::from(NO_ISSUE_EXPORT_REASON)),
                ]),
                page.status_code,
            ));
        }
    }
    let (state, reason) =
        render_blocking_state(measured, partial, unavailable, EXPORT_PARTIAL_REASON);
    write_metrics(ctx, eligible, measured, partial, unavailable, state, reason);
    for (url, d, status) in issues {
        ctx.add_with(CHECK, Some(&url), d, status, 1, None);
    }
    if state != "complete" {
        ctx.skip(CHECK, reason);
    }
}

/// `("unavailable" if not measured else "partial" if unavailable or partial else
/// "complete", reason)` shared tail of both paths.
fn render_blocking_state(
    measured: i64,
    partial: i64,
    unavailable: i64,
    partial_reason: &'static str,
) -> (&'static str, &'static str) {
    if measured == 0 {
        ("unavailable", partial_reason)
    } else if unavailable > 0 || partial > 0 {
        ("partial", partial_reason)
    } else {
        ("complete", "")
    }
}

/// The page-row SQL `_check_retained_render_blocking` runs verbatim.
const RETAINED_SQL: &str = "WITH active_static AS (\
    SELECT d.url_id,MAX(d.document_id) AS document_id FROM documents d \
    JOIN context_items c ON c.kind='resource_inventory' AND c.item_key='document:'||d.document_id \
    WHERE d.representation='static' GROUP BY d.url_id) \
    SELECT u.url,d.document_id,d.body_state,COALESCE(final.url,u.url),p.status_code \
    FROM pages p JOIN urls u USING(url_id) \
    LEFT JOIN active_static a ON a.url_id=p.url_id \
    LEFT JOIN documents d ON d.document_id=CASE WHEN p.representation='static' \
    THEN p.document_id ELSE a.document_id END AND d.representation='static' \
    LEFT JOIN responses r ON r.response_id=d.source_response_id \
    LEFT JOIN urls final ON final.url_id=r.effective_url_id \
    WHERE lower(p.content_type) LIKE '%html%' AND p.status_code BETWEEN 200 AND 299 \
    ORDER BY p.page_ordinal";

type RetainedRow = (String, Option<i64>, Option<String>, String, Option<i64>);

/// `_check_retained_render_blocking`: read frozen scan bodies instead of trusting
/// a captured column.
fn check_retained_render_blocking(ctx: &mut AuditContext) {
    let Some(con) = ctx.scan.take() else {
        return;
    };
    let mut eligible = 0i64;
    let mut measured = 0i64;
    let mut partial = 0i64;
    let mut unavailable = 0i64;
    let mut issues: Vec<(String, Map<String, Value>, Option<i64>)> = Vec::new();
    if let Ok(rows) = retained_rows(&con) {
        for (url, document_id, state, final_url, status) in rows {
            eligible += 1;
            if document_id.is_none() || state.as_deref() != Some("complete") {
                unavailable += 1;
                continue;
            }
            let resources = read_document(&con, document_id.unwrap_or(0), MAX_DOCUMENT_BYTES)
                .ok()
                .and_then(|html| retained_resources(&html, &final_url).ok());
            let Some(resources) = resources else {
                unavailable += 1;
                continue;
            };
            measured += 1;
            if !resources.is_empty() {
                let sample = budgeted_sample(resources.iter().map(|r| r.url.as_str()));
                let observed = resources.len() as i64;
                let omitted = observed - sample.len() as i64;
                if omitted > 0 {
                    partial += 1;
                }
                issues.push((
                    url.clone(),
                    details([
                        ("resources", Value::Array(sample)),
                        ("resources_observed", Value::from(observed)),
                        ("resources_omitted", Value::from(omitted)),
                        ("source", Value::from("native-retained-static-head")),
                        ("source_page", Value::from(url.as_str())),
                        ("reason", Value::from(NO_ISSUE_RETAINED_REASON)),
                    ]),
                    status,
                ));
            }
        }
    }
    ctx.scan = Some(con);
    let (state, reason) =
        render_blocking_state(measured, partial, unavailable, RETAINED_PARTIAL_REASON);
    write_metrics(ctx, eligible, measured, partial, unavailable, state, reason);
    for (url, d, status) in issues {
        ctx.add_with(CHECK, Some(&url), d, status, 1, None);
    }
    if state != "complete" {
        ctx.skip(CHECK, reason);
    }
}

/// Run [`RETAINED_SQL`]; a failed statement is an absent-evidence scan, not a
/// checked page (a failing `execute` is a crash path that
/// generated fixtures never reach because the retained DDL always exists).
fn retained_rows(con: &Connection) -> rusqlite::Result<Vec<RetainedRow>> {
    let mut stmt = con.prepare(RETAINED_SQL)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<RetainedRow>>>()?;
    Ok(rows)
}

/// `BeautifulSoup(html) -> find_render_blocking_resources(soup, document_base_url
/// (soup, final_url))`: parse the retained head and resolve the blocking tags.
fn retained_resources(
    html: &str,
    final_url: &str,
) -> spiderhead_parse::Result<Vec<RenderBlockingResource>> {
    let soup = dom::parse_document(html);
    let base = document_base_url(DocumentSource::Tree(&soup), final_url);
    find_render_blocking_resources(&soup, &base)
}
